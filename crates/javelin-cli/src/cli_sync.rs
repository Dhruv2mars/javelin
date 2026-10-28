//! `javelin sync`: delivers queued records, then brings the local World to the attached
//! node's head. The fetch is verified from the attached head, the trust anchor, so its cost
//! follows the number of new versions. It stores what is missing, advances the local World
//! and the rolling base, and rewrites `attach.json`. A local World that moved away from the
//! attachment is refused with `local_world_diverged`: sync never replaces local history. See
//! `docs/online-design.md` sections 4.1 and 4.4, and `docs/cli.md`.

use std::{cell::RefCell, ffi::OsString, path::Path};

use javelin::{
    Attachment, Error, ObjectId, RemoteHead, Repository, VersionNumber, WorldVersion,
    outbox::Fault,
    tree::{self, MemoryStore},
};
use javelin_protocol::{
    Message,
    codec::{LayerOutcome, PublishOutcome, ReviewOutcome, code, decode},
    fetch::{Anchor, fetch_opened},
    limits::MAX_TREE,
};
use serde_json::{Value, json};

use crate::{
    CliError,
    cli_remote::{
        Down, LOCAL_WORLD_DIVERGED, Link, RemoteError, RepoBlobs, Up, parse_target,
        recover_attachment, remote, summary,
    },
    cli_request::{Attached, Options, Outcome},
};

/// `javelin sync [--repo DIR]`
pub fn sync(args: Vec<OsString>) -> Outcome {
    let usage = "javelin sync [--repo DIR] [--json]";
    let options = Options::parse(args, &["--repo"], usage)?;
    if !options.positionals.is_empty() {
        return Err(Error::InvalidInput(format!("expected: {usage}")).into());
    }
    let root = options.root()?;
    let mut repo = Repository::open(&root)?;
    let attachment = repo.attachment()?.ok_or_else(|| {
        Error::InvalidInput("this repository is not attached to a node; attach first".into())
    })?;
    let link = Link::new(parse_target(&format!(
        "{}/{}",
        attachment.node, attachment.repository
    ))?);
    let attachment = recover_attachment(&link, &mut repo, attachment)?;
    let checked = repo.head()?;
    if !repo.follows_attachment(&attachment)? {
        let recover = checked
            .operation
            .as_ref()
            .map(|operation| repo.publication(operation))
            .transpose()?
            .flatten()
            .map_or(String::new(), |receipt| {
                format!(
                    " Its Layer's last checkpoint is {0}: `javelin layer clone NEW \
                     --checkpoint {0}` then `javelin request open` proposes it to the node.",
                    receipt.expected.checkpoint().object()
                )
            });
        return Err(CliError::Remote(RemoteError::new(
            LOCAL_WORLD_DIVERGED,
            format!(
                "local World {} is not the attached remote World {}: it was published in \
                 this repository, not on the node, and sync never replaces it.{recover} To \
                 follow the node, attach a fresh directory: `javelin attach {}/{} DIR`",
                checked.number, attachment.head.number, attachment.node, attachment.repository
            ),
        )));
    }
    let attached = Attached {
        root: root.clone(),
        repo: Repository::open(&root)?,
        attachment: attachment.clone(),
    };

    // Queued work goes first, so a queued Publish is in the World this sync fetches.
    let mut outbox = attached.outbox()?;
    let replay = match outbox.pending().count() {
        0 => None,
        _ => Some(outbox.replay(&mut attached.transport()?)?),
    };
    if let Some(stall) = replay.as_ref().and_then(|replay| replay.stalled.as_ref())
        && matches!(stall.fault, Fault::NodeUnavailable(_))
    {
        return Err(CliError::Remote(RemoteError::new(
            code::NODE_UNAVAILABLE,
            stall.fault.detail(),
        )));
    }
    // A queued record the node refused was delivered, but did not do what it asked.
    let mut refused = None;
    // Direct Publishes the node accepted: their Layers close here once the World follows.
    let published: Vec<_> = replay
        .iter()
        .flat_map(|replay| &replay.delivered)
        .filter_map(|delivered| {
            let entry = outbox.entry(delivered.seq)?;
            let (layer, node) =
                crate::cli_publish::published_layer(&entry.payload, &delivered.outcome)?;
            Some((layer, entry.operation.clone(), node))
        })
        .collect();
    let delivered: Vec<Value> = replay
        .iter()
        .flat_map(|replay| &replay.delivered)
        .map(|delivered| {
            let entry = outbox.entry(delivered.seq);
            let refusal = refusal_of(&delivered.outcome);
            if let (Some(code), None) = (&refusal, &refused) {
                refused = Some((delivered.seq, code.clone()));
            }
            json!({
                "seq": delivered.seq,
                "kind": entry.map(|entry| entry.kind.as_str()),
                "operation": entry.map(|entry| entry.operation.as_str()),
                "outcome": summary(&delivered.outcome),
                "refused": refusal,
            })
        })
        .collect();
    let stalled = replay.as_ref().and_then(|replay| replay.stalled.as_ref());
    let pending = outbox.pending().count();
    drop(outbox);

    let head = follow(&link, &root, &mut repo, &attachment, &checked, None)?;
    let advanced = head.number != attachment.head.number;
    let base = advanced.then(|| repo.warm_base()).transpose()?;
    let local = repo.head()?;
    let mut closed = Vec::new();
    for (layer, operation, node) in &published {
        if crate::cli_publish::close(&link, &mut repo, layer, operation, *node).unwrap_or(false) {
            closed.push(layer.clone());
        }
    }
    let mut text = if advanced {
        format!(
            "Synced {} from World {} to World {}.",
            attachment.repository, attachment.head.number, head.number
        )
    } else {
        format!(
            "{} is current at World {}.",
            attachment.repository, head.number
        )
    };
    for layer in &closed {
        text.push_str(&format!(
            "\nLayer {} was published on the node; its directory was reclaimed.",
            layer.as_str()
        ));
    }
    if replay.is_some() {
        text.push_str(&format!(
            "\nDelivered {} queued records; {pending} pending.",
            delivered.len()
        ));
    }
    let (stall, mut code, mut exit) = match stalled {
        Some(stall) => {
            text.push_str(&format!(
                "\nRecord {} waits: {}: {}",
                stall.seq,
                stall.fault.code(),
                stall.fault.detail()
            ));
            (
                json!({"seq": stall.seq, "code": stall.fault.code(), "detail": stall.fault.detail()}),
                json!(stall.fault.code()),
                1,
            )
        }
        None => (Value::Null, Value::Null, 0),
    };
    if let Some((seq, refusal)) = &refused {
        text.push_str(&format!(
            "\nRecord {seq} was refused by the node: {refusal}; see `outbox` in --json"
        ));
        if exit == 0 {
            (code, exit) = (json!(refusal), 1);
        }
    }
    let value = json!({
        "ok": exit == 0,
        "command": "sync",
        "node": attachment.node,
        "repository": attachment.repository,
        "from": attachment.head,
        "head": head,
        "advanced": advanced,
        "local_head": local,
        "base": base,
        "outbox": {"delivered": delivered, "stalled": stall, "pending": pending},
        "closed": closed,
        "code": code,
    });
    Ok((value, text, exit))
}

/// Moves `repo` from `local`, a World that follows `attached`
/// ([`Repository::follows_attachment`]), to remote World `version`, or to the node's head
/// when `None`. `javelin sync` and `javelin layer take` both follow the node this way. Only
/// the versions after the attached head are fetched, verified from its ID. Hidden entries
/// the node opens for this caller arrive with their openings, stored before the World that
/// uses them, as attach stores them; the rest stay commitments. The advance is a
/// compare-and-swap on `local` and records the new head in `attach.json`
/// ([`Repository::sync_head`]). Returns that head; the caller warms the rolling base.
pub(crate) fn follow(
    link: &RefCell<Link>,
    root: &Path,
    repo: &mut Repository,
    attached: &Attachment,
    local: &WorldVersion,
    version: Option<VersionNumber>,
) -> Result<RemoteHead, CliError> {
    let anchor = Anchor {
        number: VersionNumber::new(attached.head.number)?,
        id: attached.head.id,
        tree: attached.head.tree,
    };
    let trees = Trees {
        repo: Repository::open(root)?,
        staged: MemoryStore::default(),
    };
    let fetched = {
        let mut blobs = RepoBlobs(repo);
        fetch_opened(
            Down(link),
            Up(link),
            &trees,
            &mut blobs,
            Some(&anchor),
            version,
            None,
        )
        .map_err(|e| CliError::Remote(remote(link, e)))?
    };
    for (opening, audience) in fetched.openings.values() {
        if opening.file().is_none() {
            repo.import_tree(&trees, opening.content)?;
        }
        repo.put_opening(opening, audience)?;
    }
    let head = RemoteHead {
        number: fetched.number.get(),
        id: fetched.id,
        tree: fetched.tree,
    };
    if head.number != attached.head.number {
        repo.import_tree(&trees, head.tree)?;
        repo.sync_head(attached, local, &head)?;
    }
    Ok(head)
}

/// Tree chunks for a fetch: the local store's, then the ones this fetch received. A chunk
/// the store holds has its whole closure, so an unchanged subtree is never requested again.
struct Trees {
    repo: Repository,
    staged: MemoryStore,
}

impl tree::Store for Trees {
    fn load(&self, id: ObjectId) -> javelin::Result<Vec<u8>> {
        if self.repo.has_tree(id)? {
            return self.repo.tree_bytes_bounded(id, MAX_TREE);
        }
        self.staged.load(id)
    }
    fn save(&self, id: ObjectId, bytes: &[u8]) -> javelin::Result<()> {
        self.staged.save(id, bytes)
    }
}

/// The code of a recorded outcome that refused what its record asked, if it did.
fn refusal_of(outcome: &[u8]) -> Option<String> {
    match decode(outcome).ok()? {
        Message::ReviewOutcome(ReviewOutcome::Refused { code })
        | Message::PublishOutcome(PublishOutcome::Refused { code })
        | Message::LayerOutcome(LayerOutcome::Refused { code }) => Some(code),
        Message::PublishOutcome(PublishOutcome::Conflict { .. }) => Some(code::CONFLICT.into()),
        Message::PublishOutcome(PublishOutcome::LayerStale { .. })
        | Message::LayerOutcome(LayerOutcome::Stale { .. }) => Some(code::LAYER_STALE.into()),
        _ => None,
    }
}
