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
//! The lease lives in `node.db` and the Layer's checkpoints in the repository's database,
//! so one transaction cannot hold both. Ownership is therefore written first: before a
//! Layer's first write, the writer and its operation are recorded as a claim. Once that
//! operation's attempt is recorded as accepted, the claim becomes the lease; if it was not,
//! the claim is void. Every lease read settles a claim first, so a crash between the first
//! append and its lease leaves the Layer to the principal of its first checkpoint operation,
//! and no one else can take it.
//!
//! `LayerTake` moves the lease to the caller's device. Only the owner may take it; another
//! principal is `forbidden`. Each take is recorded under its operation ID, so a retry
//! replays its outcome and never moves the lease twice. A Layer-limited token (D26) reaches
//! only its own Layer, which the gate checks before this module runs.

use javelin::{Error, LayerId, LayerState, OperationId};
use javelin_identity::Grant;
use javelin_protocol::{
    Message, ProtocolError,
    codec::{LayerOutcome, LeaseHolder, code, decode, encode},
    receiver::{WorldLog, refusal},
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    FileWorldLog,
    layer::{refused, stale_or_refused, storage_detail},
    review::is_review_layer,
};

/// Each Layer's lease, the claim a Layer's first write makes before it runs, and each
/// decided `LayerTake` with its outcome.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS layer_leases (
    layer TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    device TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS lease_claims (
    layer TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    device TEXT NOT NULL,
    operation TEXT NOT NULL
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

/// The lease on `layer`, if any, after settling a claim its first write left.
pub(crate) fn holder(log: &FileWorldLog, layer: &LayerId) -> Result<Option<Writer>, ProtocolError> {
    settle(log, layer)?;
    log.records()
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

/// Resolves a claim on `layer`: the lease when its operation's attempt was accepted, and
/// nothing otherwise. The node serves one request at a time, so a claim seen here is never
/// one whose write is still running.
fn settle(log: &FileWorldLog, layer: &LayerId) -> Result<(), ProtocolError> {
    let claim: Option<String> = log
        .records()
        .query_row(
            "SELECT operation FROM lease_claims WHERE layer = ?1",
            [layer.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let Some(operation) = claim else {
        return Ok(());
    };
    let operation = OperationId::parse(&operation)
        .map_err(|_| storage_detail("a lease claim names an invalid operation"))?;
    let accepted = log.repo().layer_attempt(&operation, layer)? == Some(true);
    resolve_claim(log.records(), layer, accepted)
}

/// Turns `layer`'s claim into its lease when `accepted`, and drops it either way.
fn resolve_claim(
    records: &Connection,
    layer: &LayerId,
    accepted: bool,
) -> Result<(), ProtocolError> {
    let tx = records.unchecked_transaction().map_err(storage)?;
    if accepted {
        tx.execute(
            "INSERT OR IGNORE INTO layer_leases (layer, principal, device)
             SELECT layer, principal, device FROM lease_claims WHERE layer = ?1",
            [layer.as_str()],
        )
        .map_err(storage)?;
    }
    tx.execute(
        "DELETE FROM lease_claims WHERE layer = ?1",
        [layer.as_str()],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

/// A write to `layer` by `writer`, through its lease. `write` performs it. A free lease is
/// claimed for the writer before the write runs, and becomes its lease once the write is
/// accepted. When another device holds the lease, the write runs only to replay an
/// operation already recorded on the Layer; otherwise the reply is the node's head as
/// stale, and nothing is written.
pub(crate) fn write(
    log: &mut FileWorldLog,
    writer: &Writer,
    operation: &OperationId,
    layer: &LayerId,
    write: impl FnOnce(&mut FileWorldLog) -> Result<LayerOutcome, ProtocolError>,
) -> Result<LayerOutcome, ProtocolError> {
    match holder(log, layer)? {
        Some(holder) if holder == *writer => write(log),
        Some(_) => {
            let state = match log.repo().layer_state(layer) {
                Err(Error::InvalidInput(_)) => return Ok(refused(refusal::UNKNOWN_LAYER)),
                state => state?,
            };
            if log.repo().layer_attempt(operation, layer)?.is_some() {
                write(log)
            } else {
                Ok(stale_or_refused(state))
            }
        }
        None => {
            log.records()
                .execute(
                    "INSERT INTO lease_claims (layer, principal, device, operation)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        layer.as_str(),
                        writer.principal,
                        writer.device,
                        operation.as_str()
                    ],
                )
                .map_err(storage)?;
            // A failed write leaves the claim for the next lease read to settle.
            let outcome = write(log)?;
            let accepted = matches!(outcome, LayerOutcome::Accepted { .. });
            resolve_claim(log.records(), layer, accepted)?;
            Ok(outcome)
        }
    }
}

/// `LayerHead`, with whether `grant`'s device holds the Layer's writer lease.
pub(crate) fn layer_head(
    log: &mut FileWorldLog,
    grant: Option<&Grant>,
    layer: &LayerId,
) -> Result<Message, ProtocolError> {
    let mut reply = log.layer_head(layer)?;
    let held = holder(log, layer)?;
    if let Message::LayerState { lease, .. } = &mut reply {
        let caller = grant.and_then(|grant| Writer::of(grant).ok());
        *lease = match held {
            None => LeaseHolder::Free,
            Some(holder) if Some(&holder) == caller.as_ref() => LeaseHolder::Caller,
            Some(_) => LeaseHolder::Other,
        };
    }
    Ok(reply)
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
    if let Some(holder) = holder(log, layer)?
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
