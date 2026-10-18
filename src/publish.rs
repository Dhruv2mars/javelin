//! Whole captured-Layer publication: prepare from a read snapshot, then advance
//! the World with a short compare-and-swap write transaction.
use std::collections::BTreeSet;

use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

use crate::{
    Error, LayerHead, LayerId, LayerState, ObjectId, OperationId, RepoPath, Repository, Result,
    SparseChanges, VersionNumber, WorldVersion,
    domain::{ObjectKind, intersections, version_id},
    layer::{checkpoint_in, event_state_in, head_in, latest_event_state_in, verify_head},
    repository::{
        Index, append_version_in, changeset_in, decode_id, head_in as world_head_in,
        insert_object, read_object, validate_format, version_in,
    },
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE publications (
    id TEXT PRIMARY KEY,
    layer TEXT NOT NULL,
    expected INTEGER NOT NULL CHECK(expected>=0),
    checkpoint TEXT NOT NULL REFERENCES objects(id),
    against INTEGER NOT NULL REFERENCES versions(number),
    observed INTEGER NOT NULL CHECK(observed>=expected),
    observed_publication TEXT REFERENCES publications(id),
    attribution INTEGER NOT NULL CHECK(attribution IN (0,1)),
    context TEXT REFERENCES objects(id),
    allow_missing_trace INTEGER NOT NULL CHECK(allow_missing_trace IN (0,1)),
    outcome INTEGER NOT NULL CHECK(outcome IN (1,2,3)),
    result INTEGER REFERENCES versions(number),
    world_operation TEXT REFERENCES attempts(id),
    receipt TEXT NOT NULL REFERENCES objects(id),
    FOREIGN KEY(layer,expected) REFERENCES layer_heads(layer,revision),
    FOREIGN KEY(layer,observed) REFERENCES layer_heads(layer,revision),
    CHECK((attribution=0 AND context IS NULL AND allow_missing_trace=0)
       OR (attribution=1 AND (context IS NOT NULL OR allow_missing_trace=1))),
    CHECK((outcome=1 AND result IS NOT NULL AND world_operation IS NOT NULL
              AND result=against+1 AND world_operation=id)
       OR (outcome IN (2,3) AND result IS NULL AND world_operation IS NULL)),
    CHECK(outcome=3 OR (observed=expected AND observed_publication IS NULL)),
    CHECK(observed_publication IS NULL OR outcome=3)
) STRICT;
CREATE UNIQUE INDEX publications_one_success_per_layer
    ON publications(layer) WHERE outcome=1;
CREATE INDEX publications_layer ON publications(layer);
";

/// Caller-declared attribution, not authenticated identity. Context is one opaque Blob.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublishAttribution {
    Human,
    Agent {
        context: Option<ObjectId>,
        allow_missing_trace: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishWarning {
    MissingAgentTrace,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PublishOutcome {
    Published {
        version: WorldVersion,
    },
    /// Sorted unique endpoints of all exact or ancestor/descendant intersections.
    Conflict {
        paths: Vec<RepoPath>,
    },
    LayerStale {
        actual: LayerState,
    },
}

/// The exact original input and durable result, including the World seen under lock.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicationReceipt {
    pub operation: OperationId,
    pub expected: LayerHead,
    pub attribution: PublishAttribution,
    pub against: WorldVersion,
    pub warnings: Vec<PublishWarning>,
    pub outcome: PublishOutcome,
}

impl PublicationReceipt {
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("publication receipt encoding")
    }
}

impl Repository {
    /// Accepts the whole exact latest Active captured checkpoint. Compatible World
    /// advances compose without changing its immutable base. Work is O(change),
    /// independent of repository size and history length.
    ///
    /// Preparation reads one snapshot without the write lock: it validates only the
    /// records this advance depends on and composes the candidate. A short write
    /// transaction then rechecks the operation ID, the World head and the Layer
    /// state, and advances atomically. A moved World or Layer re-prepares, so
    /// compatible concurrent Publishes rebase. The guard runs inside that short
    /// transaction and sees the locked current World and the exact changeset the
    /// new version applies to it, before any new writes; it must stay fast. Refusal reserves no operation;
    /// durable retries never rerun the guard. Full history checks live in `check`.
    pub fn publish_layer<F>(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
        attribution: &PublishAttribution,
        guard: F,
    ) -> Result<PublicationReceipt>
    where
        F: FnOnce(&WorldVersion, &SparseChanges) -> Result<()>,
    {
        let mut guard = Some(guard);
        #[cfg(test)]
        let mut first = true;
        loop {
            let snapshot = self.conn.unchecked_transaction()?;
            let plan = prepare_in(&snapshot, operation, expected, attribution)?;
            snapshot.commit()?;
            #[cfg(test)]
            if std::mem::take(&mut first) {
                crate::repository::test_boundary("publish_prepared");
            }
            let plan = match plan {
                Prepared::Replay(receipt) => return Ok(receipt),
                Prepared::Advance(plan) => plan,
            };
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            #[cfg(test)]
            lock_probe::start(&tx)?;
            if operation_exists_in(&tx, operation)?
                || tx
                    .prepare("SELECT 1 FROM attempts WHERE id=?1")?
                    .exists([operation.as_str()])?
                || world_head_in(&tx)? != plan.against
                || layer_state_in(&tx, expected.layer())? != plan.actual
            {
                continue;
            }
            let Plan {
                against,
                actual,
                warnings,
                composed,
            } = plan;
            let outcome = match composed {
                None => PublishOutcome::LayerStale {
                    actual: actual.clone(),
                },
                Some(Err(paths)) => PublishOutcome::Conflict { paths },
                Some(Ok(candidate)) => {
                    let guard = guard.take().expect("guard runs at most once");
                    guard(&against, &candidate)?;
                    let number = crate::repository::next_number(against.number)?;
                    tx.execute("INSERT INTO attempts(id,expected,accepted,result) VALUES(?1,?2,1,?3)",
                        params![operation.as_str(), against.number.sql(), number.sql()])?;
                    PublishOutcome::Published {
                        version: append_version_in(&tx, &against, operation, &candidate)?,
                    }
                }
            };
            let receipt = PublicationReceipt {
                operation: operation.clone(),
                expected: expected.clone(),
                attribution: attribution.clone(),
                against,
                warnings,
                outcome,
            };
            let blob = insert_object(&tx, ObjectKind::Blob, &receipt.bytes())?;
            let (actor, context, allow_missing) = attribution_columns(attribution);
            let (tag, result) = match &receipt.outcome {
                PublishOutcome::Published { version } => (1, Some(version.number.sql())),
                PublishOutcome::Conflict { .. } => (2, None),
                PublishOutcome::LayerStale { .. } => (3, None),
            };
            let observed_publication = match &actual {
                LayerState::Published { operation, .. } => Some(operation.as_str()),
                _ => None,
            };
            tx.execute("INSERT INTO publications(id,layer,expected,checkpoint,against,observed,observed_publication,attribution,context,allow_missing_trace,outcome,result,world_operation,receipt) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![operation.as_str(), expected.layer().as_str(), expected.revision() as i64,
                    expected.checkpoint().object().to_string(), receipt.against.number.sql(),
                    revision(&actual) as i64, observed_publication, actor, context, allow_missing,
                    tag, result, result.map(|_| operation.as_str()), blob.to_string()])?;
            #[cfg(test)]
            crate::repository::test_boundary("publish_before_commit");
            tx.commit()?;
            #[cfg(test)]
            lock_probe::finish(&self.conn)?;
            #[cfg(test)]
            crate::repository::test_boundary("publish_after_commit");
            return Ok(receipt);
        }
    }

    /// Recovers a lost acknowledgment. The receipt's referenced graph and bindings
    /// are validated in one read snapshot; later World advances do not change it.
    pub fn publication(&self, operation: &OperationId) -> Result<Option<PublicationReceipt>> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let receipt = row_in(&tx, operation)?
            .map(|row| receipt_in(&tx, &row))
            .transpose()?;
        tx.commit()?;
        Ok(receipt)
    }
}

enum Prepared {
    Replay(PublicationReceipt),
    Advance(Plan),
}

/// Everything the advance decided from one snapshot. `composed` is None when the
/// Layer is no longer the exact Active head.
struct Plan {
    against: WorldVersion,
    actual: LayerState,
    warnings: Vec<PublishWarning>,
    composed: Option<std::result::Result<SparseChanges, Vec<RepoPath>>>,
}

/// Cost scales with the Layer's own records and its changeset, not with history.
fn prepare_in(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    attribution: &PublishAttribution,
) -> Result<Prepared> {
    validate_format(conn)?;
    if let Some(row) = row_in(conn, operation)? {
        let receipt = receipt_in(conn, &row)?;
        if receipt.expected != *expected || receipt.attribution != *attribution {
            return Err(Error::OperationIdReused(operation.clone()));
        }
        return Ok(Prepared::Replay(receipt));
    }
    if conn
        .prepare("SELECT 1 FROM attempts WHERE id=?1")?
        .exists([operation.as_str()])?
    {
        return Err(Error::OperationIdReused(operation.clone()));
    }
    verify_head(conn, expected)?;
    let warnings = warnings_in(conn, attribution)?;
    let against = world_head_in(conn)?;
    // The head World's own receipt and this Layer's earlier receipts are what the
    // new outcome builds on; unrelated history is left to `check`.
    if let Some(op) = &against.operation
        && let Some(row) = row_in(conn, op)?
    {
        receipt_in(conn, &row)?;
    }
    for row in rows_in(conn, "WHERE layer=?1", [expected.layer().as_str()])? {
        receipt_in(conn, &row)?;
    }
    let actual = layer_state_in(conn, expected.layer())?;
    let composed = if actual
        == (LayerState::Active {
            head: expected.clone(),
        }) {
        Some(compose_in(conn, expected, &against)?)
    } else {
        None
    };
    Ok(Prepared::Advance(Plan {
        against,
        actual,
        warnings,
        composed,
    }))
}

fn layer_state_in(conn: &Connection, layer: &LayerId) -> Result<LayerState> {
    match published_state_in(conn, layer)? {
        Some(state) => Ok(state),
        None => latest_event_state_in(conn, layer),
    }
}

fn attribution_columns(attribution: &PublishAttribution) -> (i64, Option<String>, bool) {
    match attribution {
        PublishAttribution::Human => (0, None, false),
        PublishAttribution::Agent {
            context,
            allow_missing_trace,
        } => (1, context.map(|id| id.to_string()), *allow_missing_trace),
    }
}

fn warnings_in(conn: &Connection, attribution: &PublishAttribution) -> Result<Vec<PublishWarning>> {
    match attribution {
        PublishAttribution::Human => Ok(vec![]),
        PublishAttribution::Agent {
            context: Some(id), ..
        } => {
            read_object(conn, *id, ObjectKind::Blob)?;
            Ok(vec![])
        }
        PublishAttribution::Agent {
            context: None,
            allow_missing_trace: true,
        } => Ok(vec![PublishWarning::MissingAgentTrace]),
        PublishAttribution::Agent { .. } => Err(Error::MissingAgentTrace),
    }
}

fn revision(state: &LayerState) -> u64 {
    match state {
        LayerState::Active { head } | LayerState::Published { head, .. } => head.revision(),
        LayerState::Discarded { revision, .. } => *revision,
    }
}

/// Uses end-to-end base/head differences, not intervening World attempts. The
/// candidate is the Layer's changeset, which is canonical relative to `world`
/// whenever nothing intersects. Indexed lookups only; no snapshot is loaded.
fn compose_in(
    conn: &Connection,
    head: &LayerHead,
    world: &WorldVersion,
) -> Result<std::result::Result<SparseChanges, Vec<RepoPath>>> {
    let checkpoint = checkpoint_in(conn, head.checkpoint().object())?;
    if checkpoint.base().version() > world.number {
        return Err(corrupt("publication World precedes Layer base"));
    }
    let found = intersections(
        &Index(conn),
        checkpoint.changes(),
        checkpoint.base().version(),
        world.number,
    )?;
    if !found.is_empty() {
        return Ok(Err(found.into_iter().collect()));
    }
    Ok(Ok(checkpoint.changes().clone()))
}

struct PublicationRow {
    id: String,
    layer: String,
    expected: i64,
    checkpoint: String,
    against: i64,
    observed: i64,
    observed_publication: Option<String>,
    attribution: i64,
    context: Option<String>,
    allow_missing_trace: i64,
    outcome: i64,
    result: Option<i64>,
    world_operation: Option<String>,
    receipt: String,
}

fn rows_in(
    conn: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<PublicationRow>> {
    Ok(conn.prepare(&format!("SELECT id,layer,expected,checkpoint,against,observed,observed_publication,attribution,context,allow_missing_trace,outcome,result,world_operation,receipt FROM publications {filter} ORDER BY id"))?
        .query_map(params, |r| Ok(PublicationRow {
            id: r.get(0)?, layer: r.get(1)?, expected: r.get(2)?, checkpoint: r.get(3)?,
            against: r.get(4)?, observed: r.get(5)?, observed_publication: r.get(6)?,
            attribution: r.get(7)?, context: r.get(8)?, allow_missing_trace: r.get(9)?,
            outcome: r.get(10)?, result: r.get(11)?, world_operation: r.get(12)?, receipt: r.get(13)?,
        }))?.collect::<std::result::Result<_, _>>()?)
}

fn row_in(conn: &Connection, operation: &OperationId) -> Result<Option<PublicationRow>> {
    Ok(rows_in(conn, "WHERE id=?1", [operation.as_str()])?
        .into_iter()
        .next())
}

fn corrupt(message: &str) -> Error {
    Error::CorruptRepository(message.into())
}

fn stored_operation(id: &str) -> Result<OperationId> {
    OperationId::parse(id).map_err(|_| corrupt("invalid publication operation"))
}

fn stored_layer(id: &str) -> Result<LayerId> {
    LayerId::parse(id).map_err(|_| corrupt("invalid publication Layer"))
}

fn world_at(conn: &Connection, number: i64) -> Result<WorldVersion> {
    version_in(conn, VersionNumber::decode(number)?)
        .map_err(|_| corrupt("missing publication World"))
}

fn success_state(conn: &Connection, row: &PublicationRow) -> Result<LayerState> {
    if row.outcome != 1 {
        return Err(corrupt("observation is not a successful publication"));
    }
    Ok(LayerState::Published {
        head: head_in(
            conn,
            &stored_layer(&row.layer)?,
            VersionNumber::decode(row.expected)?.get(),
        )?,
        operation: stored_operation(&row.id)?,
        version: world_at(
            conn,
            row.result
                .ok_or_else(|| corrupt("missing publication result"))?,
        )?,
    })
}

/// Terminal projection only, found through the one-success-per-Layer index and
/// verified through its receipt bindings (O(that change)). Historical
/// capture/discard events and their receipts never call this helper.
pub(crate) fn published_state_in(conn: &Connection, layer: &LayerId) -> Result<Option<LayerState>> {
    rows_in(conn, "WHERE layer=?1 AND outcome=1", [layer.as_str()])?
        .first()
        .map(|r| {
            receipt_in(conn, r)?;
            success_state(conn, r)
        })
        .transpose()
}

pub(crate) fn refuse_published_in(
    conn: &Connection,
    operation: &OperationId,
    layer: &LayerId,
) -> Result<()> {
    // A previously committed capture/discard must keep its original retry outcome.
    if !conn
        .prepare("SELECT 1 FROM layer_attempts WHERE id=?1")?
        .exists([operation.as_str()])?
        && let Some(LayerState::Published { operation, .. }) = published_state_in(conn, layer)?
    {
        return Err(Error::LayerPublished {
            layer: layer.clone(),
            operation,
        });
    }
    Ok(())
}

pub(crate) fn operation_exists_in(conn: &Connection, operation: &OperationId) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM publications WHERE id=?1")?
        .exists([operation.as_str()])?)
}

pub(crate) fn check_publications(conn: &Connection) -> Result<usize> {
    let rows = rows_in(conn, "", [])?;
    let mut successful_layers = BTreeSet::new();
    for row in &rows {
        if row.outcome == 1 && !successful_layers.insert(&row.layer) {
            return Err(corrupt("Layer published more than once"));
        }
        receipt_in(conn, row)?;
    }
    Ok(rows.len())
}

/// Validates one receipt against its own bindings, its Layer's other records and
/// the Worlds it names. It never walks unrelated history.
fn receipt_in(conn: &Connection, row: &PublicationRow) -> Result<PublicationReceipt> {
    let operation = stored_operation(&row.id)?;
    let layer = stored_layer(&row.layer)?;
    let rows = rows_in(conn, "WHERE layer=?1", [layer.as_str()])?;
    let expected = head_in(conn, &layer, VersionNumber::decode(row.expected)?.get())?;
    if expected.checkpoint().object() != decode_id(&row.checkpoint)? || row.observed < row.expected
    {
        return Err(corrupt("publication checkpoint or observation disagrees"));
    }
    let against = world_at(conn, row.against)?;
    if checkpoint_in(conn, expected.checkpoint().object())?
        .base()
        .version()
        > against.number
    {
        return Err(corrupt("publication World precedes Layer base"));
    }
    let attribution = match (row.attribution, row.allow_missing_trace, &row.context) {
        (0, 0, None) => PublishAttribution::Human,
        (1, flag @ (0 | 1), context) => PublishAttribution::Agent {
            context: context.as_deref().map(decode_id).transpose()?,
            allow_missing_trace: flag == 1,
        },
        _ => return Err(corrupt("invalid publication attribution")),
    };
    let warnings = warnings_in(conn, &attribution)?;
    let mut observed = event_state_in(conn, &layer, VersionNumber::decode(row.observed)?.get())?;
    if let Some(id) = &row.observed_publication {
        let prior = rows
            .iter()
            .find(|r| &r.id == id)
            .ok_or_else(|| corrupt("missing observed publication"))?;
        if prior.id == row.id
            || prior.layer != row.layer
            || prior.expected != row.observed
            || prior.result.is_none_or(|n| n > row.against)
        {
            return Err(corrupt("invalid observed publication binding"));
        }
        observed = success_state(conn, prior)?;
    }
    // Once a success is in this receipt's against-World, its terminal status
    // cannot be omitted, even if its raw final captured revision stayed the same.
    if rows.iter().any(|r| {
        r.layer == row.layer
            && r.outcome == 1
            && r.result.is_some_and(|n| n <= row.against)
            && row.observed_publication.as_deref() != Some(r.id.as_str())
    }) {
        return Err(corrupt("publication ignored a terminal Layer"));
    }
    let exact = observed
        == (LayerState::Active {
            head: expected.clone(),
        });
    let raw_attempt = conn
        .prepare("SELECT 1 FROM attempts WHERE id=?1")?
        .exists([row.id.as_str()])?;
    let outcome = match row.outcome {
        1 => {
            let result = row
                .result
                .ok_or_else(|| corrupt("missing publication result"))?;
            if !exact
                || row.world_operation.as_deref() != Some(row.id.as_str())
                || !raw_attempt
                || row.against.checked_add(1) != Some(result)
                || latest_event_state_in(conn, &layer)? != observed
            {
                return Err(corrupt("invalid successful publication binding"));
            }
            let candidate = compose_in(conn, &expected, &against)?
                .map_err(|_| corrupt("successful publication intersects World changes"))?;
            let version = world_at(conn, result)?;
            if version.operation.as_ref() != Some(&operation)
                || version.id != version_id(Some(against.id), &candidate)
                || changeset_in(conn, version.number)? != candidate
            {
                return Err(corrupt(
                    "published World disagrees with complete Layer composition",
                ));
            }
            PublishOutcome::Published { version }
        }
        2 | 3 => {
            if raw_attempt || row.result.is_some() || row.world_operation.is_some() {
                return Err(corrupt("failed publication owns a World attempt"));
            }
            if row.outcome == 2 {
                if !exact {
                    return Err(corrupt("conflict did not observe exact Active head"));
                }
                let paths = compose_in(conn, &expected, &against)?
                    .err()
                    .ok_or_else(|| corrupt("publication conflict has no intersections"))?;
                PublishOutcome::Conflict { paths }
            } else {
                if exact {
                    return Err(corrupt("stale publication observed exact Active head"));
                }
                PublishOutcome::LayerStale { actual: observed }
            }
        }
        _ => return Err(corrupt("invalid publication outcome")),
    };
    let receipt = PublicationReceipt {
        operation,
        expected,
        attribution,
        against,
        warnings,
        outcome,
    };
    if read_object(conn, decode_id(&row.receipt)?, ObjectKind::Blob)? != receipt.bytes() {
        return Err(corrupt(
            "publication receipt disagrees with immutable bindings",
        ));
    }
    Ok(receipt)
}

/// Measures the Publish write transaction: wall time and SQLite VM instructions.
#[cfg(test)]
pub(crate) mod lock_probe {
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    use rusqlite::Connection;

    thread_local! {
        static ACTIVE: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };
        pub(crate) static LAST: Cell<Option<(Duration, u64)>> = const { Cell::new(None) };
    }

    pub(crate) fn start(conn: &Connection) -> crate::Result<()> {
        ACTIVE.set(Some((Instant::now(), 0)));
        conn.progress_handler(
            1,
            Some(|| {
                ACTIVE.with(|a| a.set(a.get().map(|(t, n)| (t, n + 1))));
                false
            }),
        )?;
        Ok(())
    }

    pub(crate) fn finish(conn: &Connection) -> crate::Result<()> {
        conn.progress_handler(0, None::<fn() -> bool>)?;
        if let Some((start, ops)) = ACTIVE.take() {
            LAST.set(Some((start.elapsed(), ops)));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/support/publish_cost.rs"]
mod publish_cost_tests;
