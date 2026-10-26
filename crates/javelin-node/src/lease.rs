//! Writer leases: a Layer follows its owner, and one device writes to it at a time
//! (`docs/direction.md`, Layers follow their owner; `docs/online-design.md` D21).
//!
//! The lease names the owner (the token's `sub`) and the writing device (its `dev`). The
//! first accepted write, a `LayerAppend` or `LayerStack`, takes a free lease for its caller,
//! so the first writer owns the Layer. A write from any other device is answered as
//! `layer_stale` with the node's head, and changes nothing, so the old device's queued
//! appends stay recoverable with `javelin layer clone`. A retry of an operation the node
//! already recorded on the Layer replays its outcome whoever holds the lease now.
//!
//! `LayerTake` moves the lease to the caller's device. Only the owner may take it; another
//! principal is `forbidden`. Each take is recorded under its operation ID, so a retry
//! replays its outcome and never moves the lease twice. A Layer-limited token (D26) reaches
//! only its own Layer, which the gate checks before this module runs.

use javelin::{Error, LayerId, LayerState, OperationId};
use javelin_identity::Grant;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{LayerOutcome, code, decode, encode},
    receiver::refusal,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    FileWorldLog,
    layer::{refused, stale_or_refused, storage_detail},
    review::is_review_layer,
};

/// Each Layer's lease, and each decided `LayerTake` with its outcome.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS layer_leases (
    layer TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    device TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS lease_takes (
    operation TEXT PRIMARY KEY,
    request BLOB NOT NULL,
    outcome BLOB NOT NULL
);";

/// Who is writing: the token's principal and device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Writer {
    principal: String,
    device: String,
}

impl Writer {
    /// The writer a grant names. Writes always carry a token, so an anonymous grant is
    /// refused.
    pub(crate) fn of(grant: &Grant) -> Result<Self, ProtocolError> {
        match (&grant.principal, &grant.device) {
            (Some(principal), Some(device)) => Ok(Self {
                principal: principal.clone(),
                device: device.clone(),
            }),
            _ => Err(ProtocolError::new(
                code::UNAUTHORIZED,
                "a Layer write needs a token",
            )),
        }
    }
}

/// The lease on `layer`, if any.
pub(crate) fn holder(
    records: &Connection,
    layer: &LayerId,
) -> Result<Option<Writer>, ProtocolError> {
    records
        .query_row(
            "SELECT principal, device FROM layer_leases WHERE layer = ?1",
            [layer.as_str()],
            |row| {
                Ok(Writer {
                    principal: row.get(0)?,
                    device: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(storage)
}

/// A write to `layer` by `writer`, through its lease. `write` performs it. A free lease
/// is taken once the write is accepted. When another device holds the lease, the write
/// runs only to replay an operation already recorded on the Layer; otherwise the reply is
/// the node's head as stale, and nothing is written.
pub(crate) fn write(
    log: &mut FileWorldLog,
    writer: &Writer,
    operation: &OperationId,
    layer: &LayerId,
    write: impl FnOnce(&mut FileWorldLog) -> Result<LayerOutcome, ProtocolError>,
) -> Result<LayerOutcome, ProtocolError> {
    match holder(log.records(), layer)? {
        Some(holder) if holder == *writer => write(log),
        Some(_) => {
            let state = match log.repo().layer_state(layer) {
                Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
                state => state?,
            };
            if log.repo().has_layer_attempt(operation, layer)? {
                write(log)
            } else {
                Ok(stale_or_refused(state))
            }
        }
        None => {
            let outcome = write(log)?;
            if matches!(outcome, LayerOutcome::Accepted { .. }) {
                log.records()
                    .execute(
                        "INSERT OR IGNORE INTO layer_leases (layer, principal, device)
                         VALUES (?1, ?2, ?3)",
                        params![layer.as_str(), writer.principal, writer.device],
                    )
                    .map_err(storage)?;
            }
            Ok(outcome)
        }
    }
}

/// One `LayerTake`: moves `layer`'s lease to `writer`'s device and replies with the node's
/// head. A free lease is taken, so the caller becomes the owner. The lease moves and the
/// take is recorded in one transaction.
pub(crate) fn take(
    log: &mut FileWorldLog,
    writer: &Writer,
    operation: &OperationId,
    layer: &LayerId,
) -> Result<LayerOutcome, ProtocolError> {
    // The caller is part of the request: the same operation from another device is a
    // different request.
    let mut request = encode(&Message::LayerTake {
        operation: operation.clone(),
        layer: layer.clone(),
    })?;
    for part in [&writer.principal, &writer.device] {
        request.push(0);
        request.extend_from_slice(part.as_bytes());
    }
    let decided: Option<(Vec<u8>, Vec<u8>)> = log
        .records()
        .query_row(
            "SELECT request, outcome FROM lease_takes WHERE operation = ?1",
            [operation.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((recorded, outcome)) = decided {
        if recorded != request {
            return Ok(refused(code::OPERATION_ID_REUSED));
        }
        return match decode(&outcome)? {
            Message::LayerOutcome(outcome) => Ok(outcome),
            _ => Err(storage_detail(
                "recorded take outcome is not a Layer outcome",
            )),
        };
    }
    if is_review_layer(layer) {
        return Ok(refused(refusal::LAYER_RESERVED));
    }
    let head = match log.repo().layer_state(layer) {
        Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
        Ok(LayerState::Active { head }) => head,
        Ok(state) => return Ok(stale_or_refused(state)),
        Err(error) => return Err(error.into()),
    };
    if let Some(holder) = holder(log.records(), layer)?
        && holder.principal != writer.principal
    {
        return Err(ProtocolError::new(
            code::FORBIDDEN,
            "only the Layer's owner can take its writer lease",
        ));
    }
    let outcome = LayerOutcome::Accepted {
        seq: head.revision(),
        checkpoint: head.checkpoint().object(),
    };
    let bytes = encode(&Message::LayerOutcome(outcome.clone()))?;
    let (_, records) = log.parts();
    let tx = records.transaction().map_err(storage)?;
    tx.execute(
        "INSERT INTO layer_leases (layer, principal, device) VALUES (?1, ?2, ?3)
         ON CONFLICT(layer) DO UPDATE SET device = excluded.device",
        params![layer.as_str(), writer.principal, writer.device],
    )
    .map_err(storage)?;
    tx.execute(
        "INSERT INTO lease_takes (operation, request, outcome) VALUES (?1, ?2, ?3)",
        params![operation.as_str(), request, bytes],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(outcome)
}

fn storage(error: rusqlite::Error) -> ProtocolError {
    storage_detail(&error.to_string())
}
