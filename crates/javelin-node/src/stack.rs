//! The node's `LayerStack`, and the checkpoint reads a client refreshes from. A Stack
//! runs the same `javelin-stack` engine, with the same limits, as a local Stack, so the
//! same inputs give the same changes. A conflict or a merge limit writes nothing: the
//! reply is a refusal and the target head does not move. See `docs/online-design.md`
//! section 4.3 (U6).

use std::collections::BTreeSet;

use javelin::{CheckpointId, Error, LayerId, LayerState, ObjectId, OperationId, Repository};
use javelin_merge::MergeError;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{Kind, LayerOutcome, OBJECT_OVERHEAD, code},
    limits::{MAX_CHECKPOINT, MAX_FRAME, MAX_OBJECTS},
    receiver::refusal,
};
use javelin_stack::{StackError, StackOutcome};

use crate::layer::{refused, stale_or_refused, storage_detail};
use crate::review::is_review_layer;

/// Refusal for a Stack whose merge exceeds a `javelin-merge` resource limit, or reads a
/// blob over the merge's blob bound. Nothing is written, so the operation ID stays free.
pub const MERGE_LIMIT: &str = "merge_limit";

/// One `LayerStack`. `target_head` must be a head the Layer has held: the current head
/// stacks, and an earlier one only replays the operation it already recorded, so a retry
/// after a lost reply gets the same `seq`. Any other head is stale, and nothing is written,
/// so the client can read the head and retry with the same operation ID.
pub(crate) fn stack(
    repo: &mut Repository,
    operation: &OperationId,
    target: &LayerId,
    target_head: ObjectId,
    source: ObjectId,
) -> Result<LayerOutcome, ProtocolError> {
    if is_review_layer(target) {
        return Ok(refused(refusal::LAYER_RESERVED));
    }
    let state = match repo.layer_state(target) {
        Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
        state => state?,
    };
    let source = CheckpointId::parse(&source.to_string())?;
    let current = match &state {
        LayerState::Active { head } => Some(head.clone()),
        _ => None,
    };
    // Contents can revert, so a head checkpoint can recur. The latest match decides; an
    // earlier one can only replay, which the latest refuses as a reused operation.
    let mut result = None;
    let heads = repo.layer_history(target)?.into_iter().rev();
    for head in heads.filter_map(|state| match state {
        LayerState::Active { head } if head.checkpoint().object() == target_head => Some(head),
        _ => None,
    }) {
        let attempt = if current.as_ref() == Some(&head) {
            javelin_stack::stack_layer(repo, operation, &head, source).map(Some)
        } else {
            repo.stack_operation(operation, &head, source)
                .map(|replay| replay.map(StackOutcome::Updated))
                .map_err(StackError::Storage)
        };
        match attempt {
            Ok(None) => {}
            Err(StackError::Storage(Error::OperationIdReused(_))) => result = Some(attempt),
            attempt => {
                result = Some(attempt);
                break;
            }
        }
    }
    let Some(result) = result else {
        return Ok(stale_or_refused(state));
    };
    match result {
        Ok(Some(StackOutcome::Updated(javelin::LayerUpdate::Applied {
            state: LayerState::Active { head },
        }))) => Ok(LayerOutcome::Accepted {
            seq: head.revision(),
            checkpoint: head.checkpoint().object(),
        }),
        Ok(Some(StackOutcome::Updated(javelin::LayerUpdate::Stale { state }))) => {
            Ok(stale_or_refused(state))
        }
        Ok(Some(StackOutcome::Updated(javelin::LayerUpdate::Applied { .. }))) => {
            Err(storage_detail("Stack applied without an active Layer head"))
        }
        Ok(Some(StackOutcome::Conflicts(_))) => Ok(refused(code::CONFLICT)),
        Ok(None) => Ok(stale_or_refused(state)),
        Err(StackError::Merge(
            MergeError::Resource(_) | MergeError::Load(Error::ObjectTooLarge { .. }),
        )) => Ok(refused(MERGE_LIMIT)),
        Err(StackError::Merge(MergeError::Load(error)) | StackError::Storage(error)) => {
            match error.code() {
                "operation_id_reused" | "layer_published" | "missing_object" | "invalid_input" => {
                    Ok(refused(error.code()))
                }
                _ => Err(error.into()),
            }
        }
    }
}

/// A `Get` of checkpoints: each one's stored bytes, which hash to its ID. A client reads
/// a Layer head this way to refresh from it. As for other kinds, the reply is the longest
/// prefix of the request that fits one frame.
pub(crate) fn get_checkpoints(
    repo: &Repository,
    items: Vec<(Kind, ObjectId)>,
) -> Result<Message, ProtocolError> {
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err(ProtocolError::new(
            code::MALFORMED,
            "duplicate object request",
        ));
    }
    let mut objects = Vec::new();
    let mut size = 5;
    for (kind, id) in items.into_iter().take(MAX_OBJECTS) {
        let bytes = repo.checkpoint_object(CheckpointId::parse(&id.to_string())?)?;
        if bytes.len() > MAX_CHECKPOINT {
            return Err(ProtocolError::new(
                code::LIMIT_EXCEEDED,
                format!("checkpoint {id} is larger than one object"),
            ));
        }
        size += OBJECT_OVERHEAD + bytes.len();
        if !objects.is_empty() && size > MAX_FRAME {
            break;
        }
        objects.push((kind, id, bytes));
    }
    Ok(Message::Objects(objects))
}

/// Whether `items` names only checkpoints.
pub(crate) fn all_checkpoints(items: &[(Kind, ObjectId)]) -> bool {
    !items.is_empty() && items.iter().all(|(kind, _)| *kind == Kind::Checkpoint)
}
