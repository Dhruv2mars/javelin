//! Reference receiver. Objects are stored only through the public native API and
//! accepted only when their computed ID equals the claimed ID. A tree chunk is
//! stored only after everything it names, so having a tree means having all of it.
//! World references are written only by `advance_to_tree`, which verifies the
//! changed part of the candidate tree. Fetch reads any stored object by ID.

use std::collections::BTreeSet;
use std::io::{Read, Write};

use javelin::{LayerId, ObjectId, OperationId, PublishAttribution, Repository, VersionNumber};

use crate::{
    Kind, Message, Outcome, ProtocolError,
    codec::{LayerOutcome, OBJECT_OVERHEAD, PublishOutcome, VERSION, read_frame, write_frame},
    limits::*,
};

/// Refusal codes that hosted writes return as outcomes, not as protocol errors.
pub mod refusal {
    pub const UNKNOWN_LAYER: &str = "unknown_layer";
    pub const LAYER_DISCARDED: &str = "layer_discarded";
    pub const LAYER_RESERVED: &str = "layer_reserved";
}

/// The coordinator's ordering point for hosted writes. A node implements it over its
/// storage. A repeated operation replays its recorded outcome, and the World advances
/// at most once per operation.
pub trait WorldLog {
    /// The repository that holds this log's objects and World versions.
    fn repository(&mut self) -> &mut Repository;

    /// Publishes `layer` as the next World version, provided its head is `expected` and
    /// its base is `base`. Refusals and conflicts are returned as outcomes. `Err` means
    /// the node could not decide: the caller retries the same operation.
    fn publish_layer(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        expected: ObjectId,
        base: VersionNumber,
        attribution: &PublishAttribution,
    ) -> Result<PublishOutcome, ProtocolError>;

    /// Whether the changes object `id` is stored. Only checkpoint objects are staged.
    fn has_changes(&mut self, id: ObjectId) -> Result<bool, ProtocolError>;

    /// Stores a changes object whose ID the caller has verified. Bytes that are not a
    /// canonical changeset are refused as `malformed`.
    fn store_changes(&mut self, id: ObjectId, bytes: &[u8]) -> Result<(), ProtocolError>;

    /// Appends the staged changes `changes` to `layer` after `parent`. Refusals and
    /// staleness are outcomes. A repeated operation replays its recorded outcome.
    fn append_checkpoint(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        parent: Option<ObjectId>,
        changes: ObjectId,
    ) -> Result<LayerOutcome, ProtocolError>;

    /// The `LayerState` reply for `layer`: its base, head sequence, head and status.
    fn layer_head(&mut self, layer: &LayerId) -> Result<Message, ProtocolError>;
}

/// Serves one message: a hosted write goes to the log, anything else to the repository.
/// The legacy `Publish` is not served here: it would skip the log's operation records,
/// so one operation ID could mean two different things.
pub fn handle_request<L: WorldLog>(
    log: &mut L,
    message: Message,
) -> Result<Message, ProtocolError> {
    match message {
        Message::Publish { .. } => Err(unexpected()),
        Message::PublishLayer {
            operation,
            layer,
            expected,
            base,
            attribution,
        } => Ok(Message::PublishOutcome(log.publish_layer(
            &operation,
            &layer,
            expected,
            base,
            &attribution,
        )?)),
        Message::LayerAppend {
            operation,
            layer,
            parent,
            checkpoint,
        } => Ok(Message::LayerOutcome(
            log.append_checkpoint(&operation, &layer, parent, checkpoint)?,
        )),
        Message::LayerHead { layer } => log.layer_head(&layer),
        // A batch of only changes objects goes to the log. A mixed batch falls through
        // to `handle`, which refuses checkpoint kinds.
        Message::Want(items) if all_changes(&items) => {
            unique(&items)?;
            let mut missing = Vec::new();
            for (kind, id) in items {
                if !log.has_changes(id)? {
                    missing.push((kind, id));
                }
            }
            Ok(Message::Missing(missing))
        }
        Message::Objects(objects) if all_changes_objects(&objects) => {
            // Checked before anything is stored: a wrong ID rejects the whole batch.
            for (kind, claimed, bytes) in &objects {
                let actual = kind.id(bytes);
                if actual != *claimed {
                    return Err(mismatch(*claimed, actual));
                }
            }
            for (_, id, bytes) in &objects {
                log.store_changes(*id, bytes)?;
            }
            Ok(Message::Stored {
                count: objects.len() as u32,
            })
        }
        other => handle(log.repository(), other),
    }
}

/// Serves one session. Any refusal is reported to the peer and ends the session.
pub fn serve(
    repo: &mut Repository,
    mut r: impl Read,
    mut w: impl Write,
) -> Result<(), ProtocolError> {
    let mut negotiated = false;
    loop {
        let result = match read_frame(&mut r) {
            Ok(None) => return Ok(()),
            Ok(Some(message)) if !negotiated => negotiate(message).inspect(|_| negotiated = true),
            Ok(Some(message)) => handle(repo, message),
            Err(e) => Err(e),
        };
        match result {
            Ok(reply) => write_frame(&mut w, &reply)?,
            Err(e) => {
                // Best effort: the peer may already be gone.
                let _ = write_frame(
                    &mut w,
                    &Message::Error {
                        code: e.code.clone(),
                        detail: e.detail.clone(),
                    },
                );
                return Err(e);
            }
        }
    }
}

fn negotiate(message: Message) -> Result<Message, ProtocolError> {
    match message {
        Message::Hello { min, max } if (min..=max).contains(&VERSION) => {
            Ok(Message::HelloOk { version: VERSION })
        }
        Message::Hello { .. } => Err(ProtocolError::new(
            "unsupported_version",
            format!("supported versions: {VERSION}..={VERSION}"),
        )),
        _ => Err(unexpected()),
    }
}

fn handle(repo: &mut Repository, message: Message) -> Result<Message, ProtocolError> {
    match message {
        Message::Want(items) => {
            // Streamed probes still hash stored bytes, so repeats amplify work.
            unique(&items)?;
            let mut missing = Vec::new();
            for (kind, id) in items {
                let found = match kind {
                    Kind::Blob => repo.has_blob(id),
                    Kind::Tree => repo.has_tree(id),
                    Kind::Checkpoint => return Err(checkpoint_unserved()),
                }?;
                if !found {
                    missing.push((kind, id));
                }
            }
            Ok(Message::Missing(missing))
        }
        Message::Objects(objects) => {
            // Refused before anything is stored, so a mixed batch is all or nothing.
            if objects.iter().any(|(kind, _, _)| *kind == Kind::Checkpoint) {
                return Err(checkpoint_unserved());
            }
            for (kind, claimed, bytes) in &objects {
                let actual = kind.id(bytes);
                if actual != *claimed {
                    return Err(mismatch(*claimed, actual));
                }
                match kind {
                    Kind::Blob => repo.put_blob(bytes)?,
                    Kind::Tree => repo.put_tree(bytes)?,
                    Kind::Checkpoint => return Err(checkpoint_unserved()),
                };
            }
            Ok(Message::Stored {
                count: objects.len() as u32,
            })
        }
        Message::Publish {
            operation,
            expected,
            tree,
        } => {
            let outcome = repo.advance_to_tree(&operation, expected, tree)?;
            Ok(Message::Outcome(Outcome::from_native(&outcome)?))
        }
        Message::Resolve { version } => {
            let world = match version {
                Some(number) => repo.version(number)?,
                None => repo.head()?,
            };
            Ok(Message::Resolved {
                number: world.number,
                id: world.id,
                tree: world.tree,
            })
        }
        // The reply is the longest prefix of the request that fits one frame.
        Message::Get(items) => {
            unique(&items)?;
            let mut objects = Vec::new();
            let mut size = 5;
            for (kind, id) in items.into_iter().take(MAX_OBJECTS) {
                let bytes = match kind {
                    Kind::Blob => repo.read_blob_bounded(id, MAX_BLOB)?,
                    Kind::Tree => repo.tree_bytes_bounded(id, MAX_TREE)?,
                    Kind::Checkpoint => return Err(checkpoint_unserved()),
                };
                size += OBJECT_OVERHEAD + bytes.len();
                if !objects.is_empty() && size > MAX_FRAME {
                    break;
                }
                objects.push((kind, id, bytes));
            }
            Ok(Message::Objects(objects))
        }
        _ => Err(unexpected()),
    }
}

fn all_changes(items: &[(Kind, ObjectId)]) -> bool {
    !items.is_empty() && items.iter().all(|(kind, _)| *kind == Kind::Checkpoint)
}

fn all_changes_objects(objects: &[(Kind, ObjectId, Vec<u8>)]) -> bool {
    !objects.is_empty() && objects.iter().all(|(kind, _, _)| *kind == Kind::Checkpoint)
}

fn unique(items: &[(Kind, javelin::ObjectId)]) -> Result<(), ProtocolError> {
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err(ProtocolError::new("malformed", "duplicate object request"));
    }
    Ok(())
}

fn mismatch(claimed: javelin::ObjectId, stored: javelin::ObjectId) -> ProtocolError {
    ProtocolError::new(
        "id_mismatch",
        format!("claimed {claimed} but content has ID {stored}"),
    )
}

/// Checkpoint objects decode but have no store on this path yet. Refused as the
/// version 2 receiver always refused kind 3: `malformed`.
fn checkpoint_unserved() -> ProtocolError {
    ProtocolError::new(
        "malformed",
        "checkpoint objects are not served on this path",
    )
}

fn unexpected() -> ProtocolError {
    ProtocolError::new(
        "unexpected_message",
        "message not valid in this session state",
    )
}
