//! The node's Layer partition: `LayerAppend` and `LayerHead`, and the changes objects an
//! append names. A changes object is staged in `node.db` until an append references it.
//! An accepted append is one native `append_checkpoint` transaction, so the Layer head,
//! its `live_paths` index and the attempt record commit together.

use javelin::{
    Error, LayerId, LayerState, LayerUpdate, ObjectId, OperationId, Repository, SparseChanges,
};
use javelin_protocol::{
    Message, ProtocolError,
    codec::{LayerOutcome, LayerStatus, code},
    receiver::refusal,
};
use rusqlite::{Connection, OptionalExtension, params};

/// Changes objects, keyed by their ID. Each is a canonical changeset.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS changes (
    id TEXT PRIMARY KEY,
    bytes BLOB NOT NULL
);";

pub(crate) fn has_changes(records: &Connection, id: ObjectId) -> Result<bool, ProtocolError> {
    records
        .query_row(
            "SELECT 1 FROM changes WHERE id = ?1",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
        .map_err(storage)
}

/// Stages a verified changes object. Bytes that are not a canonical changeset are
/// refused before anything is written.
pub(crate) fn store_changes(
    records: &Connection,
    id: ObjectId,
    bytes: &[u8],
) -> Result<(), ProtocolError> {
    SparseChanges::from_canonical_bytes(bytes)
        .map_err(|e| ProtocolError::new("malformed", e.to_string()))?;
    records
        .execute(
            "INSERT OR IGNORE INTO changes (id, bytes) VALUES (?1, ?2)",
            params![id.to_string(), bytes],
        )
        .map_err(storage)?;
    Ok(())
}

/// One `LayerAppend`. The parent must be a head the Layer has held, so a retry whose
/// parent has since been superseded replays its own attempt. Any other parent is stale.
pub(crate) fn append(
    repo: &mut Repository,
    records: &Connection,
    operation: &OperationId,
    layer: &LayerId,
    parent: Option<ObjectId>,
    changes: ObjectId,
) -> Result<LayerOutcome, ProtocolError> {
    let changes = load_changes(records, changes)?.ok_or_else(|| {
        ProtocolError::new(
            "missing_object",
            format!("changes {changes} are not uploaded"),
        )
    })?;
    let state = match repo.layer_state(layer) {
        Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
        state => state?,
    };
    let expected = match parent {
        Some(parent) => repo
            .layer_history(layer)?
            .into_iter()
            .find_map(|state| match state {
                LayerState::Active { head } if head.checkpoint().object() == parent => Some(head),
                _ => None,
            }),
        None => None,
    };
    let Some(expected) = expected else {
        return Ok(stale_or_refused(state));
    };
    match repo.append_checkpoint(operation, &expected, &changes) {
        Ok(LayerUpdate::Applied {
            state: LayerState::Active { head },
        }) => Ok(LayerOutcome::Accepted {
            seq: head.revision(),
            checkpoint: head.checkpoint().object(),
        }),
        Ok(LayerUpdate::Stale { state }) => Ok(stale_or_refused(state)),
        Ok(LayerUpdate::Applied { .. }) => Err(storage_detail(
            "append applied without an active Layer head",
        )),
        Err(error) => match error.code() {
            "operation_id_reused"
            | "missing_agent_trace"
            | "layer_published"
            | "missing_object"
            | "invalid_input" => Ok(refused(error.code())),
            _ => Err(error.into()),
        },
    }
}

/// `LayerHead`: the Layer's base, head sequence, head and status.
pub(crate) fn layer_head(repo: &Repository, layer: &LayerId) -> Result<Message, ProtocolError> {
    let state = match repo.layer_state(layer) {
        Err(Error::InvalidInput(_)) => {
            return Err(ProtocolError::new(
                refusal::UNKNOWN_LAYER,
                format!("Layer {} does not exist", layer.as_str()),
            ));
        }
        state => state?,
    };
    let (seq, head, status) = match &state {
        LayerState::Active { head } => (
            head.revision(),
            head.checkpoint().object(),
            LayerStatus::Active,
        ),
        LayerState::Published { head, .. } => (
            head.revision(),
            head.checkpoint().object(),
            LayerStatus::Published,
        ),
        LayerState::Discarded {
            revision,
            checkpoint,
            ..
        } => (*revision, checkpoint.object(), LayerStatus::Discarded),
    };
    let base = match &state {
        LayerState::Active { head } | LayerState::Published { head, .. } => {
            repo.read_checkpoint(head.checkpoint())?
        }
        LayerState::Discarded { checkpoint, .. } => repo.read_checkpoint(*checkpoint)?,
    }
    .base()
    .version();
    Ok(Message::LayerState {
        layer: layer.clone(),
        base,
        seq,
        head,
        state: status,
    })
}

/// The reply for a parent that is not current. An active Layer is stale against its
/// head; a Layer that is no longer active has no head to be stale against.
fn stale_or_refused(state: LayerState) -> LayerOutcome {
    match state {
        LayerState::Active { head } => LayerOutcome::Stale {
            head: head.checkpoint().object(),
        },
        LayerState::Published { .. } => refused(code::LAYER_PUBLISHED),
        LayerState::Discarded { .. } => refused(refusal::LAYER_DISCARDED),
    }
}

fn load_changes(
    records: &Connection,
    id: ObjectId,
) -> Result<Option<SparseChanges>, ProtocolError> {
    let bytes: Option<Vec<u8>> = records
        .query_row(
            "SELECT bytes FROM changes WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    bytes
        .map(|bytes| {
            SparseChanges::from_canonical_bytes(&bytes).map_err(|e| storage_detail(&e.to_string()))
        })
        .transpose()
}

fn refused(code: &str) -> LayerOutcome {
    LayerOutcome::Refused { code: code.into() }
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    storage_detail(&error.to_string())
}

fn storage_detail(detail: &str) -> ProtocolError {
    ProtocolError::new("storage", detail)
}
