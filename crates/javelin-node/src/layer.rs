//! The node's Layer partition: `LayerAppend` and `LayerHead`, and the changes objects an
//! append names. A changes object is staged in `node.db` until an append references it.
//! An accepted append is one native `capture_layer` transaction, so the Layer head,
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

/// Whether every path a staged changes object names is visible to `reader` under every
/// policy the World has had. A staged object has no base yet, so all of them apply.
pub(crate) fn changes_visible(
    repo: &Repository,
    records: &Connection,
    id: ObjectId,
    reader: &javelin::Reader,
) -> Result<bool, ProtocolError> {
    let Some(changes) = load_changes(records, id)? else {
        return Ok(true);
    };
    let from = javelin::VersionNumber::new(0)?;
    Ok(repo.visible(changes.entries().keys(), from, reader)?)
}

/// Whether `reader` may know of staged changes `id`: they name only paths visible to it, or
/// it uploaded them.
pub(crate) fn accessible_changes(
    repo: &Repository,
    records: &Connection,
    id: ObjectId,
    reader: &javelin::Reader,
) -> Result<bool, ProtocolError> {
    Ok(changes_visible(repo, records, id, reader)? || repo.uploaded(id, reader)?)
}

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
    writer: Option<&javelin::Reader>,
) -> Result<LayerOutcome, ProtocolError> {
    let id = changes;
    let not_uploaded =
        || ProtocolError::new("missing_object", format!("changes {id} are not uploaded"));
    let changes = load_changes(records, changes)?.ok_or_else(not_uploaded)?;
    // Changes the writer may not read are absent to it.
    if let Some(writer) = writer
        && !accessible_changes(repo, records, id, writer)?
    {
        return Err(not_uploaded());
    }
    let state = match repo.layer_state(layer) {
        Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
        state => state?,
    };
    // Contents can revert, so a checkpoint can recur in the history. The latest match is
    // the parent of a new append; an earlier one can only be the parent of a replay, which
    // the latest refuses as a reused operation.
    let mut update = None;
    let heads = repo.layer_history(layer)?.into_iter().rev();
    for head in heads.filter_map(|state| match state {
        LayerState::Active { head } if Some(head.checkpoint().object()) == parent => Some(head),
        _ => None,
    }) {
        // An append replaces the Layer's whole delta. A head the writer may not read is not
        // a parent it can name: the append is stale, as for any head it does not hold.
        if let Some(writer) = writer
            && !matches!(repo.checkpoint_visible(head.checkpoint(), writer), Ok(true))
        {
            continue;
        }
        update = Some(repo.capture_layer_as(operation, &head, &changes, writer));
        if !matches!(update, Some(Err(Error::OperationIdReused(_)))) {
            break;
        }
    }
    let Some(update) = update else {
        return Ok(stale_or_refused(state));
    };
    match update {
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
/// The refusal for a Layer this node does not hold.
pub(crate) fn unknown_layer(layer: &LayerId) -> ProtocolError {
    ProtocolError::new(
        refusal::UNKNOWN_LAYER,
        format!("Layer {} does not exist", layer.as_str()),
    )
}

pub(crate) fn layer_head(repo: &Repository, layer: &LayerId) -> Result<Message, ProtocolError> {
    let state = match repo.layer_state(layer) {
        Err(Error::InvalidInput(_)) => return Err(unknown_layer(layer)),
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
pub(crate) fn stale_or_refused(state: LayerState) -> LayerOutcome {
    match state {
        LayerState::Active { head } => LayerOutcome::Stale {
            head: head.checkpoint().object(),
        },
        LayerState::Published { .. } => refused(code::LAYER_PUBLISHED),
        LayerState::Discarded { .. } => refused(refusal::LAYER_DISCARDED),
    }
}

pub(crate) fn load_changes(
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

pub(crate) fn refused(code: &str) -> LayerOutcome {
    LayerOutcome::Refused { code: code.into() }
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    storage_detail(&error.to_string())
}

pub(crate) fn storage_detail(detail: &str) -> ProtocolError {
    ProtocolError::new("storage", detail)
}
