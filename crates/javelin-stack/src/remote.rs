//! Stack on a node, and refresh a local Layer from the head a node holds. The node runs
//! the Stack (see `javelin-node`); the client only reads the result back. See
//! `docs/online-design.md` section 4.3 (U6).

use std::collections::BTreeSet;
use std::io::{Read, Write};

use javelin::{LayerId, LayerState, LayerUpdate, ObjectId, OperationId, Repository, SparseChanges};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{Kind, LayerOutcome, LayerStatus, code, read_frame, write_frame},
    limits::MAX_OBJECTS,
    receiver::refusal,
    sender,
};

/// Refusal for a refresh whose local head holds work the node's head may not: the local
/// Layer is not behind the node's. Nothing is written.
pub const LAYER_DIVERGED: &str = "layer_diverged";

/// Stacks `source` onto `target` on the node, if the node's head for `target` is still
/// `target_head`. A `Stale` reply names the node's head; refresh and retry with the same
/// operation ID. A conflict is refused with `conflict`, and a merge over a resource limit
/// with `merge_limit`; neither writes anything.
pub fn stack(
    link: &mut (impl Read + Write),
    operation: &OperationId,
    target: &LayerId,
    target_head: ObjectId,
    source: ObjectId,
) -> Result<LayerOutcome, ProtocolError> {
    match request(
        link,
        &Message::LayerStack {
            operation: operation.clone(),
            target: target.clone(),
            target_head,
            source,
        },
    )? {
        Message::LayerOutcome(outcome) => Ok(outcome),
        _ => Err(unexpected()),
    }
}

/// What a refresh did. `seq` and `head` are the node's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refresh {
    /// The local head already holds the node's head.
    Current { seq: u64, head: ObjectId },
    /// The local head moved to the node's head.
    Imported {
        seq: u64,
        head: ObjectId,
        update: LayerUpdate,
    },
}

/// Brings the local Layer `layer` to the head the node holds for it.
///
/// The node's head checkpoint is read and checked against its ID, then its changes are
/// compared with the local head's. Equal changes are current. Otherwise the node must be
/// ahead (its `seq` past the local revision) on the same base tree, or the refresh is
/// refused with `layer_diverged`: the local head may hold captures the node lacks. The
/// missing blobs are fetched, and the head is imported as a Stack result
/// ([`Repository::import_stacked_head`]), so capture will not run over a directory that
/// lacks it. This moves the head only: the caller refreshes the Layer's directory.
pub fn refresh(
    link: &mut (impl Read + Write),
    repo: &mut Repository,
    layer: &LayerId,
) -> Result<Refresh, ProtocolError> {
    let node = sender::layer_head(link, layer)?;
    match node.status {
        LayerStatus::Active => {}
        LayerStatus::Published => {
            return Err(ProtocolError::new(
                code::LAYER_PUBLISHED,
                "the node's Layer is published",
            ));
        }
        LayerStatus::Discarded => {
            return Err(ProtocolError::new(
                refusal::LAYER_DISCARDED,
                "the node's Layer is discarded",
            ));
        }
    }
    let local = match repo.layer_state(layer)? {
        LayerState::Active { head } => head,
        _ => {
            return Err(ProtocolError::new(
                code::INVALID_INPUT,
                "the local Layer is not active",
            ));
        }
    };
    let changes = Repository::foreign_checkpoint_changes(&fetch_checkpoint(link, node.head)?)?;
    let ours = repo.read_checkpoint(local.checkpoint())?;
    if ours.changes() == &changes {
        return Ok(Refresh::Current {
            seq: node.seq,
            head: node.head,
        });
    }
    if node.seq <= local.revision() {
        return Err(ProtocolError::new(
            LAYER_DIVERGED,
            format!(
                "the local Layer is at revision {} and the node's at {}, with different \
                 contents",
                local.revision(),
                node.seq
            ),
        ));
    }
    let tree = match request(
        link,
        &Message::Resolve {
            version: Some(node.base),
        },
    )? {
        Message::Resolved { tree, .. } => tree,
        _ => return Err(unexpected()),
    };
    if repo.version(ours.base().version())?.tree != tree {
        return Err(ProtocolError::new(
            LAYER_DIVERGED,
            "the local Layer and the node's have different base trees",
        ));
    }
    fetch_blobs(link, repo, &changes)?;
    let operation = OperationId::parse(&format!("refresh-{}-{}", local.revision(), node.head))?;
    match repo.import_stacked_head(&operation, &local, &changes)? {
        update @ LayerUpdate::Applied {
            state: LayerState::Active { .. },
        } => Ok(Refresh::Imported {
            seq: node.seq,
            head: node.head,
            update,
        }),
        _ => Err(ProtocolError::new(
            code::LAYER_STALE,
            "the local Layer moved during refresh; retry",
        )),
    }
}

/// The node's checkpoint `id`, checked against its ID.
fn fetch_checkpoint(
    link: &mut (impl Read + Write),
    id: ObjectId,
) -> Result<Vec<u8>, ProtocolError> {
    match request(link, &Message::Get(vec![(Kind::Checkpoint, id)]))? {
        Message::Objects(mut objects) if objects.len() == 1 => {
            let (kind, claimed, bytes) = objects.remove(0);
            if kind != Kind::Checkpoint || claimed != id {
                return Err(unexpected());
            }
            let actual = kind.id(&bytes);
            if actual != id {
                return Err(ProtocolError::new(
                    code::ID_MISMATCH,
                    format!("checkpoint {id} has ID {actual}"),
                ));
            }
            Ok(bytes)
        }
        _ => Err(unexpected()),
    }
}

/// Fetches the blobs `changes` names that `repo` lacks. Each `Get` reply is a prefix of
/// its request, so the rest is asked for again.
fn fetch_blobs(
    link: &mut (impl Read + Write),
    repo: &mut Repository,
    changes: &SparseChanges,
) -> Result<(), ProtocolError> {
    let mut wanted = Vec::new();
    for id in changes
        .entries()
        .values()
        .flatten()
        .map(|entry| entry.content())
        .collect::<BTreeSet<_>>()
    {
        if !repo.has_blob(id)? {
            wanted.push((Kind::Blob, id));
        }
    }
    let mut rest = wanted.as_slice();
    while !rest.is_empty() {
        let batch = &rest[..rest.len().min(MAX_OBJECTS)];
        let objects = match request(link, &Message::Get(batch.to_vec()))? {
            Message::Objects(objects) if !objects.is_empty() && objects.len() <= batch.len() => {
                objects
            }
            _ => return Err(unexpected()),
        };
        for ((kind, id), (got_kind, got_id, bytes)) in batch.iter().zip(&objects) {
            if kind != got_kind || id != got_id {
                return Err(unexpected());
            }
            let actual = kind.id(bytes);
            if actual != *id {
                return Err(ProtocolError::new(
                    code::ID_MISMATCH,
                    format!("blob {id} has ID {actual}"),
                ));
            }
            repo.put_blob(bytes)?;
        }
        rest = &rest[objects.len()..];
    }
    Ok(())
}

fn request(link: &mut (impl Read + Write), message: &Message) -> Result<Message, ProtocolError> {
    write_frame(&mut *link, message)?;
    match read_frame(&mut *link)? {
        None => Err(ProtocolError::new(
            code::TRUNCATED,
            "the node closed the session before replying",
        )),
        Some(Message::Error { code, detail }) => Err(ProtocolError { code, detail }),
        Some(reply) => Ok(reply),
    }
}

fn unexpected() -> ProtocolError {
    ProtocolError::new("unexpected_message", "unexpected reply from the node")
}
