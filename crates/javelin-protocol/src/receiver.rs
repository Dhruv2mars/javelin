//! Reference receiver. Objects are stored only through the public native API and
//! accepted only when their computed ID equals the claimed ID. A tree chunk is
//! stored only after everything it names, so having a tree means having all of it.
//! World references are written only by `advance_to_tree`, which verifies the
//! changed part of the candidate tree.
//!
//! Reads run as a `Reader`, whose audiences decide what it receives. Version IDs,
//! committed trees and changesets are the same for everyone; a hidden entry in them is
//! a commitment. An object inside hidden entries is served only to their audiences, and
//! `Want` and `Get` answer as if it were absent for anyone else, so a guessed ID is never
//! confirmed. `Open` returns only the openings the reader may read.

use std::collections::BTreeSet;
use std::io::{Read, Write};

use javelin::{
    LayerId, ObjectId, OperationId, PublishAttribution, Reader, Repository, VersionNumber,
};

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

    /// Whether the staged changes object `id` names only paths visible to `reader`. A
    /// changes object is plain, so a probe for one that names a hidden path is answered
    /// as for an object that was never stored. Stores without hidden entries keep the
    /// default.
    fn changes_visible(&mut self, id: ObjectId, reader: &Reader) -> Result<bool, ProtocolError> {
        let _ = (id, reader);
        Ok(true)
    }

    /// Stores a changes object whose ID the caller has verified. Bytes that are not a
    /// canonical changeset are refused as `malformed`.
    fn store_changes(&mut self, id: ObjectId, bytes: &[u8]) -> Result<(), ProtocolError>;

    /// Appends the staged changes `changes` to `layer` after `parent`, for `writer`.
    /// Refusals and staleness are outcomes. A repeated operation replays its recorded
    /// outcome. Staged changes, and objects their entries name, that the writer may not
    /// read are treated exactly as absent.
    fn append_checkpoint(
        &mut self,
        operation: &OperationId,
        layer: &LayerId,
        parent: Option<ObjectId>,
        changes: ObjectId,
        writer: &Reader,
    ) -> Result<LayerOutcome, ProtocolError>;

    /// The `LayerState` reply for `layer`: its base, head sequence, head and status.
    fn layer_head(&mut self, layer: &LayerId) -> Result<Message, ProtocolError>;
}

/// Serves one message for an outsider: a caller with no role reads public content only.
pub fn handle_request<L: WorldLog>(
    log: &mut L,
    message: Message,
) -> Result<Message, ProtocolError> {
    handle_request_as(log, message, &Reader::outsider())
}

/// Serves one message for `reader`: a hosted write goes to the log, anything else to
/// the repository. The legacy `Publish` is not served here: it would skip the log's
/// operation records, so one operation ID could mean two different things.
pub fn handle_request_as<L: WorldLog>(
    log: &mut L,
    message: Message,
    reader: &Reader,
) -> Result<Message, ProtocolError> {
    match message {
        Message::Publish { .. } => Err(unexpected()),
        Message::PublishLayer {
            operation,
            layer,
            expected,
            base,
            attribution,
        } => {
            let outcome = log.publish_layer(&operation, &layer, expected, base, &attribution)?;
            Ok(Message::PublishOutcome(match outcome {
                // A conflict with a change the caller cannot see is reported without its
                // path.
                PublishOutcome::Conflict { paths } if !reader.reads_all() => {
                    let mut visible = Vec::new();
                    for path in paths {
                        if log.repository().visible([&path], base, reader)? {
                            visible.push(path);
                        }
                    }
                    PublishOutcome::Conflict { paths: visible }
                }
                outcome => outcome,
            }))
        }
        Message::LayerAppend {
            operation,
            layer,
            parent,
            checkpoint,
        } => Ok(Message::LayerOutcome(log.append_checkpoint(
            &operation, &layer, parent, checkpoint, reader,
        )?)),
        Message::LayerHead { layer } => log.layer_head(&layer),
        // A batch of only changes objects goes to the log. A mixed batch falls through
        // to `handle`, which refuses checkpoint kinds.
        Message::Want(items) if all_changes(&items) => {
            unique(&items)?;
            let mut missing = Vec::new();
            for (kind, id) in items {
                // Present only if the caller may read it: it names no path hidden from the
                // caller, or the caller uploaded it.
                if !log.has_changes(id)?
                    || !(log.changes_visible(id, reader)?
                        || log.repository().uploaded(id, reader)?)
                {
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
                if let Some(principal) = reader.principal() {
                    log.repository().record_upload(*id, principal)?;
                }
            }
            Ok(Message::Stored {
                count: objects.len() as u32,
            })
        }
        other => handle(log.repository(), other, reader),
    }
}

/// Serves one session to an outsider. Any refusal is reported to the peer and ends the
/// session.
pub fn serve(repo: &mut Repository, r: impl Read, w: impl Write) -> Result<(), ProtocolError> {
    serve_as(repo, r, w, &Reader::outsider())
}

/// Serves one session to `reader`. A reader with no principal is identified by the session,
/// so it may read back what it uploads during the session and nothing else it uploads.
pub fn serve_as(
    repo: &mut Repository,
    mut r: impl Read,
    mut w: impl Write,
    reader: &Reader,
) -> Result<(), ProtocolError> {
    let session;
    let reader = match reader.principal() {
        Some(_) => reader,
        None => {
            session = reader.clone().with_principal(session_principal());
            &session
        }
    };
    let mut negotiated = false;
    loop {
        let result = match read_frame(&mut r) {
            Ok(None) => return Ok(()),
            Ok(Some(message)) if !negotiated => negotiate(message).inspect(|_| negotiated = true),
            Ok(Some(message)) => handle(repo, message, reader),
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

fn handle(
    repo: &mut Repository,
    message: Message,
    reader: &Reader,
) -> Result<Message, ProtocolError> {
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
                // Hidden from this caller: reported missing, as if never stored.
                if !found || !repo.readable(id, reader)? {
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
                let id = match kind {
                    Kind::Blob => repo.put_blob(bytes)?,
                    // A reference the uploader may not read is refused as absent.
                    Kind::Tree => repo.put_tree_as(bytes, Some(reader))?,
                    Kind::Checkpoint => return Err(checkpoint_unserved()),
                };
                if let Some(principal) = reader.principal() {
                    repo.record_upload(id, principal)?;
                }
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
        Message::Changeset { version, offset } => {
            let bytes = repo.changeset(version)?.canonical_bytes();
            let total = bytes.len();
            let start = usize::try_from(offset)
                .ok()
                .filter(|start| *start <= total)
                .ok_or_else(|| {
                    ProtocolError::new("malformed", "changeset offset is past its end")
                })?;
            let end = total.min(start + MAX_CHANGESET_PART);
            Ok(Message::ChangesetPart {
                total: total as u64,
                bytes: bytes[start..end].to_vec(),
            })
        }
        Message::Open(commitments) => {
            let openings = repo.openings(&commitments, reader)?;
            Ok(Message::Opened(
                openings
                    .into_iter()
                    .map(|(commitment, opening, _)| (commitment, opening))
                    .collect(),
            ))
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
        Message::Attribution { version } => Ok(Message::Attributed {
            number: version,
            publication: repo.version_publication(version)?,
        }),
        // The reply is the longest prefix of the request that fits one frame.
        Message::Get(items) => {
            unique(&items)?;
            let mut objects = Vec::new();
            let mut size = 5;
            for (kind, id) in items.into_iter().take(MAX_OBJECTS) {
                if !repo.readable(id, reader)? {
                    // The same refusal as an object that was never stored.
                    return Err(javelin::Error::MissingObject(id).into());
                }
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

/// A principal unique to one anonymous session of this process.
fn session_principal() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seed = format!(
        "{}:{nanos}:{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    format!("session:{}", ObjectId::for_blob(seed.as_bytes()))
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
