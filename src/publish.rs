//! Whole captured-Layer publication in the repository's single SQLite boundary.
use std::collections::BTreeSet;

use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

use crate::{
    Error, LayerHead, LayerId, LayerState, Manifest, ObjectId, OperationId, RepoPath, Repository,
    Result, VersionNumber, WorldVersion,
    domain::ObjectKind,
    layer::{checkpoint_in, event_state_in, head_in, latest_event_state_in, verify_head},
    repository::{
        decode_id, history_in, insert_object, manifest_in, read_object, validate_state_in,
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
    /// advances compose without changing its immutable base. The guard sees the
    /// locked current World and exact composed candidate before any new writes.
    /// Refusal reserves no operation; durable retries never rerun the guard.
    pub fn publish_layer<F>(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
        attribution: &PublishAttribution,
        guard: F,
    ) -> Result<PublicationReceipt>
    where
        F: FnOnce(&WorldVersion, &Manifest) -> Result<()>,
    {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let history = validate_state_in(&tx)?;
        let rows = rows_in(&tx)?;
        if let Some(row) = rows.iter().find(|r| r.id == operation.as_str()) {
            let receipt = receipt_in(&tx, row, &rows, &history)?;
            if receipt.expected != *expected || receipt.attribution != *attribution {
                return Err(Error::OperationIdReused(operation.clone()));
            }
            tx.commit()?;
            return Ok(receipt);
        }
        if tx
            .prepare("SELECT 1 FROM attempts WHERE id=?1")?
            .exists([operation.as_str()])?
        {
            return Err(Error::OperationIdReused(operation.clone()));
        }
        verify_head(&tx, expected)?;
        let warnings = warnings_in(&tx, attribution)?;
        let against = history
            .last()
            .ok_or_else(|| corrupt("missing initial World"))?
            .clone();
        let actual = published_state_in(&tx, expected.layer())?
            .unwrap_or(latest_event_state_in(&tx, expected.layer())?);
        let outcome = if actual
            != (LayerState::Active {
                head: expected.clone(),
            }) {
            PublishOutcome::LayerStale {
                actual: actual.clone(),
            }
        } else {
            match compose_in(&tx, expected, &against)? {
                Err(paths) => PublishOutcome::Conflict { paths },
                Ok(candidate) => {
                    guard(&against, &candidate)?;
                    let manifest =
                        insert_object(&tx, ObjectKind::Manifest, &candidate.canonical_bytes())?;
                    let number =
                        VersionNumber::new(against.number.get().checked_add(1).ok_or_else(
                            || Error::InvalidInput("World version overflow".into()),
                        )?)?;
                    tx.execute("INSERT INTO attempts(id,expected,manifest,accepted,result) VALUES(?1,?2,?3,1,?4)",
                        params![operation.as_str(), against.number.sql(), manifest.to_string(), number.sql()])?;
                    tx.execute(
                        "INSERT INTO versions(number,manifest,operation) VALUES(?1,?2,?3)",
                        params![number.sql(), manifest.to_string(), operation.as_str()],
                    )?;
                    PublishOutcome::Published {
                        version: WorldVersion {
                            number,
                            manifest,
                            operation: Some(operation.clone()),
                        },
                    }
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
        crate::repository::test_boundary("publish_after_commit");
        Ok(receipt)
    }

    /// Recovers a lost acknowledgment. Every referenced graph and receipt binding
    /// is validated in one read snapshot; later World advances do not change it.
    pub fn publication(&self, operation: &OperationId) -> Result<Option<PublicationReceipt>> {
        let tx = self.conn.unchecked_transaction()?;
        let history = validate_state_in(&tx)?;
        let rows = rows_in(&tx)?;
        let receipt = rows
            .iter()
            .find(|r| r.id == operation.as_str())
            .map(|row| receipt_in(&tx, row, &rows, &history))
            .transpose()?;
        tx.commit()?;
        Ok(receipt)
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

/// Uses end-to-end base/head differences, not intervening World attempts.
fn compose_in(
    conn: &Connection,
    head: &LayerHead,
    world: &WorldVersion,
) -> Result<std::result::Result<Manifest, Vec<RepoPath>>> {
    let checkpoint = checkpoint_in(conn, head.checkpoint().object())?;
    if checkpoint.base().version() > world.number {
        return Err(corrupt("publication World precedes Layer base"));
    }
    let base = manifest_in(conn, checkpoint.base().manifest())?;
    let current = manifest_in(conn, world.manifest)?;
    let world_changes: BTreeSet<_> = base
        .entries()
        .keys()
        .chain(current.entries().keys())
        .filter(|p| base.entries().get(*p) != current.entries().get(*p))
        .collect();
    let mut intersections = BTreeSet::new();
    for path in checkpoint.changes().entries().keys() {
        for changed in &world_changes {
            if path == *changed || ancestor(path, changed) || ancestor(changed, path) {
                intersections.insert(path.clone());
                intersections.insert((*changed).clone());
            }
        }
    }
    if !intersections.is_empty() {
        return Ok(Err(intersections.into_iter().collect()));
    }
    let mut entries = current.entries().clone();
    for (path, after) in checkpoint.changes().entries() {
        match after {
            Some(entry) => {
                entries.insert(path.clone(), entry.clone());
            }
            None => {
                entries.remove(path);
            }
        }
    }
    Ok(Ok(Manifest::new(entries)?))
}

fn ancestor(parent: &RepoPath, child: &RepoPath) -> bool {
    child
        .as_str()
        .strip_prefix(parent.as_str())
        .is_some_and(|tail| tail.starts_with('/'))
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

fn rows_in(conn: &Connection) -> Result<Vec<PublicationRow>> {
    Ok(conn.prepare("SELECT id,layer,expected,checkpoint,against,observed,observed_publication,attribution,context,allow_missing_trace,outcome,result,world_operation,receipt FROM publications ORDER BY id")?
        .query_map([], |r| Ok(PublicationRow {
            id: r.get(0)?, layer: r.get(1)?, expected: r.get(2)?, checkpoint: r.get(3)?,
            against: r.get(4)?, observed: r.get(5)?, observed_publication: r.get(6)?,
            attribution: r.get(7)?, context: r.get(8)?, allow_missing_trace: r.get(9)?,
            outcome: r.get(10)?, result: r.get(11)?, world_operation: r.get(12)?, receipt: r.get(13)?,
        }))?.collect::<std::result::Result<_, _>>()?)
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

fn world_at(history: &[WorldVersion], number: i64) -> Result<&WorldVersion> {
    let number = VersionNumber::decode(number)?;
    history
        .iter()
        .find(|w| w.number == number)
        .ok_or_else(|| corrupt("missing publication World"))
}

fn success_state(
    conn: &Connection,
    row: &PublicationRow,
    history: &[WorldVersion],
) -> Result<LayerState> {
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
            history,
            row.result
                .ok_or_else(|| corrupt("missing publication result"))?,
        )?
        .clone(),
    })
}

/// Terminal projection only. Callers validate the complete state first; historical
/// capture/discard events and their receipts never call this helper.
pub(crate) fn published_state_in(conn: &Connection, layer: &LayerId) -> Result<Option<LayerState>> {
    let rows = rows_in(conn)?;
    rows.iter()
        .find(|r| r.layer == layer.as_str() && r.outcome == 1)
        .map(|r| success_state(conn, r, &history_in(conn)?))
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

pub(crate) fn check_publications(conn: &Connection, history: &[WorldVersion]) -> Result<usize> {
    let rows = rows_in(conn)?;
    let mut successful_layers = BTreeSet::new();
    for row in &rows {
        if row.outcome == 1 && !successful_layers.insert(&row.layer) {
            return Err(corrupt("Layer published more than once"));
        }
        receipt_in(conn, row, &rows, history)?;
    }
    Ok(rows.len())
}

fn receipt_in(
    conn: &Connection,
    row: &PublicationRow,
    rows: &[PublicationRow],
    history: &[WorldVersion],
) -> Result<PublicationReceipt> {
    let operation = stored_operation(&row.id)?;
    let layer = stored_layer(&row.layer)?;
    let expected = head_in(conn, &layer, VersionNumber::decode(row.expected)?.get())?;
    if expected.checkpoint().object() != decode_id(&row.checkpoint)? || row.observed < row.expected
    {
        return Err(corrupt("publication checkpoint or observation disagrees"));
    }
    let against = world_at(history, row.against)?.clone();
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
        observed = success_state(conn, prior, history)?;
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
            let version = world_at(history, result)?.clone();
            if version.operation.as_ref() != Some(&operation)
                || version.manifest != ObjectId::for_manifest(&candidate)
                || manifest_in(conn, version.manifest)? != candidate
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
