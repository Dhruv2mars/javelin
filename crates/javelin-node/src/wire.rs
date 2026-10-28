//! Protocol version 2 without its transport: one request body in, one reply body out. The
//! self-hosted node's loopback HTTP server (`server.rs`) and the Cloudflare Durable Object
//! (`crates/javelin-node-cloudflare`) both frame requests and call `handle`, so every node
//! answers the same message the same way.
//!
//! Every message passes the node's gate (`auth.rs`) with the request's bearer token, or as an
//! anonymous caller without one, so an anonymous caller can only read a public repository.
//! Refusals are `Error` replies. A caller outside the repository reads at the public frontier
//! (`disclosure.rs`), so it holds no Layer or Publish Request it could see. Replies to the
//! read messages (`Resolve`, `Get`, `Changeset`, `Attribution`, `ReadDisclosure`) may be read
//! by any browser origin; any other message sent with an `Origin`
//! is refused with `forbidden` before it is served, so a web page can never write through a
//! visitor's browser.

use javelin::{LayerId, LayerState};
use javelin_collaboration::{PrincipalId, Role};
use javelin_identity::{Need, Scope};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{PublishOutcome, code, decode, encode},
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
        // Every version is stamped, and every decided embargo tied to its version, before
        // anything is read, and again after a Publish, so the version's time is this request.
        crate::disclosure::stamp(log.records(), log.repo(), now)?;
        let reply = serve_one(log, gate, token, now, message);
        // A failure here leaves the stamp to the next request, which only delays disclosure.
        let _ = crate::disclosure::stamp(log.records(), log.repo(), now);
        reply
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
            | Message::ReadDisclosure
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
    refuse_outsider_work(gate, token, now, &message)?;
    if let Message::PublishLayer { layer, .. } = &message
        && let Some(refused) = refuse_direct_publish(gate, token, now, layer)?
    {
        return Ok(Message::PublishOutcome(PublishOutcome::Refused {
            code: refused.into(),
        }));
    }
    match &message {
        Message::OpenRequest { .. }
        | Message::Revise { .. }
        | Message::Approve { .. }
        | Message::Comment { .. }
        | Message::PublishRequest { .. }
        | Message::ReadRequest { .. }
        | Message::ListRequests { .. }
        | Message::ReadReview { .. } => return review::handle(log, gate, token, now, &message),
        Message::CheckReport(report) => {
            return checks::report(log, gate, token, now, report).map(Message::CheckOutcome);
        }
        Message::ReadDisclosure
        | Message::SetDelay { .. }
        | Message::Embargo { .. }
        | Message::Release { .. } => return disclose(log, gate, token, now, &message),
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
    // Outside the repository, the World ends at the public frontier.
    let reader = crate::disclosure::hold(log.records(), log.repo(), now, reader)?;
    if grant.is_some() {
        check_writes(log, &reader, &message)?;
    }
    log.traces()
        .refuse_wire_fetch(log.repo(), &reader, &message)?;
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
        Message::LayerCreate { layer, base } => Ok(Message::LayerOutcome(crate::layer::create(
            log.parts().0,
            &layer,
            base,
        )?)),
        Message::Get(items) if crate::stack::all_checkpoints(&items) => {
            crate::stack::get_checkpoints(log.repo(), items, &reader)
        }
        message => handle_request_as(log, message, &reader),
    }
}

/// Refuses a caller without a role in the repository any message about Layers, Publish
/// Requests or check results, before anything stored is looked up. A Layer is work above
/// every version: its state, base and checkpoints, a request's state, or a check's subject
/// could each name a version above the public frontier, and whether one exists must not
/// show. A public repository grants every signed-in caller Read, which carries the write
/// scopes, so this holds whatever the token carries. The token is checked first, with no
/// Layer named, so that refusal does not depend on stored state either.
fn refuse_outsider_work(
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: &Message,
) -> Result<(), ProtocolError> {
    let scope = match message {
        Message::LayerHead { .. }
        | Message::ReadRequest { .. }
        | Message::ListRequests { .. }
        | Message::ReadReview { .. } => Scope::Read,
        Message::LayerAppend { .. }
        | Message::LayerStack { .. }
        | Message::LayerTake { .. }
        | Message::LayerCreate { .. } => Scope::LayerWrite,
        Message::OpenRequest { .. } | Message::Revise { .. } | Message::Comment { .. } => {
            Scope::ReviewWrite
        }
        Message::Approve { .. } => Scope::ReviewApprove,
        Message::PublishRequest { .. } => Scope::Publish,
        // A member's direct Publish is answered by `refuse_direct_publish`.
        Message::PublishLayer { .. } => Scope::LayerWrite,
        Message::CheckReport(_) => Scope::CheckReport,
        Message::Get(items) if crate::stack::all_checkpoints(items) => Scope::Read,
        _ => return Ok(()),
    };
    let grant = gate.check(token, now, Need { scope, layer: None })?;
    if gate.reader(&grant).is_member() {
        return Ok(());
    }
    Err(ProtocolError::new(
        code::FORBIDDEN,
        "Layers, Publish Requests and check results need a role in the repository",
    ))
}

/// Direct Publish is for Maintain on a repository whose rules require no approvals. Any
/// other member's `PublishLayer` is answered with a refusal outcome that says to open a Publish
/// Request: `insufficient_role` for a role below Maintain, `review_required` when approvals
/// are required, and `forbidden` for a token without the Publish scope. An outcome, not an error, so a queued Publish is decided rather than retried
/// forever. Nothing is recorded, so the same operation may publish once the rules allow it. A
/// token limited to another Layer is refused by the gate, as before.
fn refuse_direct_publish(
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    layer: &LayerId,
) -> Result<Option<&'static str>, ProtocolError> {
    let grant = gate.check(
        token,
        now,
        Need {
            scope: Scope::LayerWrite,
            layer: Some(layer.as_str()),
        },
    )?;
    let Some(permissions) = gate.permissions() else {
        return Ok(None);
    };
    let role = grant
        .principal
        .as_deref()
        .and_then(|principal| PrincipalId::parse(principal).ok())
        .and_then(|principal| permissions.role(&principal));
    if role < Some(Role::Maintain) {
        return Ok(Some(code::INSUFFICIENT_ROLE));
    }
    if permissions.required_approvals > 0 {
        return Ok(Some(code::REVIEW_REQUIRED));
    }
    let publish = Need {
        scope: Scope::Publish,
        layer: Some(layer.as_str()),
    };
    match gate.check(token, now, publish) {
        Err(error) if error.code == code::FORBIDDEN => Ok(Some(code::FORBIDDEN)),
        _ => Ok(None),
    }
}

/// The disclosure messages. Anyone who may read the repository reads its disclosure, as
/// far as its role allows; only Maintain changes it.
fn disclose(
    log: &mut FileWorldLog,
    gate: &Gate,
    token: Option<&str>,
    now: u64,
    message: &Message,
) -> Result<Message, ProtocolError> {
    let grant = gate.check(token, now, need(message))?;
    let reader = gate.reader(&grant);
    let records = log.records();
    let repo = log.repo();
    if !matches!(message, Message::ReadDisclosure) && !reader.reads_all() {
        return Err(ProtocolError::new(
            code::FORBIDDEN,
            "only Maintain changes the disclosure delay and embargoes",
        ));
    }
    match message {
        Message::SetDelay { delay } => crate::disclosure::set_delay(records, *delay)?,
        Message::Embargo { operation, until } => {
            crate::disclosure::embargo(records, repo, operation, *until)?
        }
        Message::Release { version } => crate::disclosure::release(records, *version, now)?,
        _ => {}
    }
    Ok(Message::Disclosure(crate::disclosure::state(
        records, repo, now, &reader,
    )?))
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
        Message::LayerAppend { layer, .. } | Message::LayerCreate { layer, .. } => {
            (Scope::LayerWrite, Some(layer))
        }
        Message::LayerStack { target, .. } => (Scope::LayerWrite, Some(target)),
        Message::LayerTake { layer, .. } => (Scope::LayerWrite, Some(layer)),
        Message::PublishLayer { layer, .. } => (Scope::Publish, Some(layer)),
        Message::Publish { .. }
        | Message::SetDelay { .. }
        | Message::Embargo { .. }
        | Message::Release { .. } => (Scope::Publish, None),
        Message::Want(_) | Message::Objects(_) => (Scope::LayerWrite, None),
        Message::LayerHead { layer } => (Scope::Read, Some(layer)),
        _ => (Scope::Read, None),
    };
    Need {
        scope,
        layer: layer.map(|layer| layer.as_str()),
    }
}
