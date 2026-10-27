//! Protocol version 2 without its transport: one request body in, one reply body out. The
//! self-hosted node's loopback HTTP server (`server.rs`) and the Cloudflare Durable Object
//! (`crates/javelin-node-cloudflare`) both frame requests and call `handle`, so every node
//! answers the same message the same way.
//!
//! Every message passes the node's gate (`auth.rs`) with the request's bearer token, or as an
//! anonymous caller without one, so an anonymous caller can only read a public repository.
//! Refusals are `Error` replies. Replies to the read messages (`Resolve`, `Get`, `Changeset`,
//! `Attribution`) may be read by any browser origin; any other message sent with an `Origin`
//! is refused with `forbidden` before it is served, so a web page can never write through a
//! visitor's browser.

use javelin::LayerState;
use javelin_identity::{Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{code, decode, encode},
    handle_request_as,
    receiver::WorldLog,
};

use crate::{
    FileWorldLog,
    auth::{self, Gate},
    checks, lease, review,
};

/// One reply: an encoded message, which a browser may read when `cors` is set.
pub struct Answer {
    pub body: Vec<u8>,
    pub cors: bool,
}

/// The token of one `Authorization` header value. Only the Bearer scheme carries one; any
/// other value, and an empty token, is `None`, a framing error.
pub fn bearer(value: &str) -> Option<String> {
    value
        .trim()
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Answers one request body at Unix time `now`. `origin` says whether the request came with
/// an `Origin` header. Only a reply that cannot be encoded, even as an `Error`, is `Err`.
pub fn handle(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    origin: bool,
    now: u64,
    body: &[u8],
) -> Result<Answer, ProtocolError> {
    let mut cors = false;
    let reply = match decode(body).and_then(|message| {
        cors = is_read(&message);
        // Refused before the gate or the handler sees it: a page in a visitor's browser
        // never writes, whatever credential the browser holds.
        if origin && !cors {
            return Err(ProtocolError::new(
                code::FORBIDDEN,
                "a browser origin may only send read messages",
            ));
        }
        serve_one(log, gate, token, now, message)
    }) {
        Ok(reply) => reply,
        Err(ProtocolError { code, detail }) => Message::Error { code, detail },
    };
    let body = match encode(&reply) {
        Ok(bytes) => bytes,
        Err(ProtocolError { code, detail }) => encode(&Message::Error { code, detail })?,
    };
    Ok(Answer { body, cors })
}

/// Messages that only read the repository, which a browser may send.
fn is_read(message: &Message) -> bool {
    matches!(
        message,
        Message::Resolve { .. }
            | Message::Get(_)
            | Message::Changeset { .. }
            | Message::Attribution { .. }
    )
}

/// One message through the gate and the trace guard, then its handler. The review
/// partition and check reports authorize inside their handlers, because what they need
/// depends on stored state, such as the Layer a request belongs to.
fn serve_one(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: Message,
) -> Result<Message, ProtocolError> {
    match &message {
        Message::OpenRequest { .. }
        | Message::Revise { .. }
        | Message::Approve { .. }
        | Message::Comment { .. }
        | Message::PublishRequest { .. }
        | Message::ReadRequest { .. } => return review::handle(log, gate, token, now, &message),
        Message::CheckReport(report) => {
            return checks::report(log, gate, token, now, report).map(Message::CheckOutcome);
        }
        _ => {}
    }
    // Version negotiation reveals nothing about the repository, so it needs no grant.
    let grant = match &message {
        Message::Hello { .. } => None,
        other => Some(gate.check(token, now, need(other))?),
    };
    // Hidden entries follow the caller's role: anonymous is an outsider.
    let reader = grant
        .as_ref()
        .map_or_else(javelin::Reader::outsider, |grant| gate.reader(grant));
    if grant.is_some() {
        check_writes(log, &reader, &message)?;
    }
    log.traces().refuse_wire_fetch(&message)?;
    // Writes to a Layer go through its writer lease, which names the grant's device.
    let writer = || {
        grant
            .as_ref()
            .ok_or_else(|| ProtocolError::new(code::UNAUTHORIZED, "a token is required"))
            .and_then(lease::Writer::of)
    };
    match message {
        Message::LayerAppend {
            operation,
            layer,
            parent,
            checkpoint,
        } => Ok(Message::LayerOutcome(lease::write(
            log,
            &writer()?,
            &operation,
            &layer,
            |log| log.append_checkpoint(&operation, &layer, parent, checkpoint, &reader),
        )?)),
        Message::LayerStack {
            operation,
            target,
            target_head,
            source,
        } => Ok(Message::LayerOutcome(lease::write(
            log,
            &writer()?,
            &operation,
            &target,
            |log| {
                crate::stack::stack(
                    log.parts().0,
                    &operation,
                    &target,
                    target_head,
                    source,
                    &reader,
                )
            },
        )?)),
        Message::LayerHead { layer } => lease::layer_head(log, grant.as_ref(), &layer),
        Message::LayerTake { operation, layer } => Ok(Message::LayerOutcome(lease::take(
            log,
            &writer()?,
            &reader,
            &operation,
            &layer,
        )?)),
        Message::Get(items) if crate::stack::all_checkpoints(&items) => {
            crate::stack::get_checkpoints(log.repo(), items, &reader)
        }
        message => handle_request_as(log, message, &reader),
    }
}

/// Refuses a write that would change a path outside the caller's audiences, before it
/// reaches its handler: an append's staged changes over the Layer's base, the source of a
/// Stack over the target's base, and the whole Layer for a Publish.
fn check_writes(
    log: &mut FileWorldLog,
    writer: &javelin::Reader,
    message: &Message,
) -> Result<(), ProtocolError> {
    match message {
        Message::LayerAppend {
            layer,
            parent,
            checkpoint,
            ..
        } => {
            let Some(changes) = crate::layer::load_changes(log.records(), *checkpoint)? else {
                return Ok(());
            };
            // Unreadable staged changes are absent to the writer; the append says so.
            if !crate::layer::accessible_changes(log.repo(), log.records(), *checkpoint, writer)? {
                return Ok(());
            }
            let Ok(LayerState::Active { head }) = log.repo().layer_state(layer) else {
                return Ok(());
            };
            // An unreadable parent is left to the append, which treats it as stale.
            let previous = parent
                .and_then(|parent| javelin::CheckpointId::parse(&parent.to_string()).ok())
                .filter(|parent| matches!(log.repo().checkpoint_visible(*parent, writer), Ok(true)))
                .and_then(|parent| log.repo().read_checkpoint(parent).ok());
            let Some(previous) = previous else {
                return Ok(());
            };
            // The append replaces the whole delta, so the writer must be able to make every
            // path the transition changes: new or edited entries and dropped ones.
            let mut transition = changes.entries().clone();
            for (path, entry) in previous.changes().entries() {
                if changes.entries().get(path) != Some(entry) {
                    transition.entry(path.clone()).or_insert(None);
                }
            }
            let transition = javelin::SparseChanges::new(transition)?;
            let base = log
                .repo()
                .read_checkpoint(head.checkpoint())?
                .base()
                .version();
            auth::check_write(log.repo(), writer, &transition, base)
        }
        Message::LayerStack {
            target_head,
            source,
            ..
        } => {
            let base = javelin::CheckpointId::parse(&target_head.to_string())
                .ok()
                .and_then(|id| log.repo().read_checkpoint(id).ok())
                .map(|target| target.base().version());
            auth::check_checkpoint_write(log.repo(), writer, *source, base)
        }
        Message::PublishLayer { expected, .. } => {
            auth::check_checkpoint_write(log.repo(), writer, *expected, None)
        }
        _ => Ok(()),
    }
}

/// What one message needs from the caller's token. Messages that name a Layer carry it, so a
/// Layer-limited token reaches only its own Layer there. Object uploads (`Want`, `Objects`)
/// name none: they need `layer:write`, and change no reference, since objects are addressed
/// by content. Everything else, replies sent as requests included, needs `read`.
fn need(message: &Message) -> Need<'_> {
    let (scope, layer) = match message {
        Message::LayerAppend { layer, .. } => (Scope::LayerWrite, Some(layer)),
        Message::LayerStack { target, .. } => (Scope::LayerWrite, Some(target)),
        Message::LayerTake { layer, .. } => (Scope::LayerWrite, Some(layer)),
        Message::PublishLayer { layer, .. } => (Scope::Publish, Some(layer)),
        Message::Publish { .. } => (Scope::Publish, None),
        Message::Want(_) | Message::Objects(_) => (Scope::LayerWrite, None),
        Message::LayerHead { layer } => (Scope::Read, Some(layer)),
        _ => (Scope::Read, None),
    };
    Need {
        scope,
        layer: layer.map(|layer| layer.as_str()),
    }
}

