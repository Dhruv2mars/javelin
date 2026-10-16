//! Sparse immutable checkpoints and repository-local durable Layer heads.
use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    Error, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, Result, VersionNumber,
    domain::ObjectKind,
    repository::{decode_id, history_in, insert_object, manifest_in, read_object, validate_entry},
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE repository_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    id TEXT NOT NULL CHECK(length(id)=64)
) STRICT;
INSERT INTO repository_identity VALUES(1,lower(hex(randomblob(32))));
CREATE TABLE layer_heads (
    layer TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision>=0),
    checkpoint TEXT NOT NULL REFERENCES objects(id),
    discarded INTEGER NOT NULL CHECK(discarded IN (0,1)),
    operation TEXT UNIQUE REFERENCES layer_attempts(id) DEFERRABLE INITIALLY DEFERRED,
    PRIMARY KEY(layer,revision),
    CHECK((revision=0 AND operation IS NULL AND discarded=0) OR (revision>0 AND operation IS NOT NULL))
) STRICT;
CREATE TABLE layer_attempts (
    id TEXT PRIMARY KEY,
    layer TEXT NOT NULL,
    expected INTEGER NOT NULL,
    checkpoint TEXT NOT NULL REFERENCES objects(id),
    discard INTEGER NOT NULL CHECK(discard IN (0,1)),
    accepted INTEGER NOT NULL CHECK(accepted IN (0,1)),
    result INTEGER NOT NULL,
    FOREIGN KEY(layer,expected) REFERENCES layer_heads(layer,revision),
    FOREIGN KEY(layer,result) REFERENCES layer_heads(layer,revision) DEFERRABLE INITIALLY DEFERRED,
    CHECK((accepted=1 AND result=expected+1) OR (accepted=0 AND result>expected))
) STRICT;
";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct LayerId(String);

impl LayerId {
    pub fn parse(value: &str) -> Result<Self> {
        OperationId::parse(value)?;
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CheckpointId(ObjectId);

impl CheckpointId {
    pub fn parse(value: &str) -> Result<Self> {
        Ok(Self(ObjectId::parse(value)?))
    }
    pub fn object(self) -> ObjectId {
        self.0
    }
}

/// Exact immutable base, issued by a repository rather than assembled by callers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayerBase {
    repository: ObjectId,
    version: VersionNumber,
    manifest: ObjectId,
}

impl LayerBase {
    pub fn version(&self) -> VersionNumber {
        self.version
    }
    pub fn manifest(&self) -> ObjectId {
        self.manifest
    }
}

/// Complete private changes relative to the exact base. None deletes an exact
/// file/symlink path; capture must expand directory deletion into entry tombstones.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SparseChanges(BTreeMap<RepoPath, Option<FileEntry>>);

impl SparseChanges {
    pub fn new(entries: impl IntoIterator<Item = (RepoPath, Option<FileEntry>)>) -> Result<Self> {
        let mut changes = BTreeMap::new();
        for (path, entry) in entries {
            if changes.insert(path, entry).is_some() {
                return Err(Error::InvalidInput("duplicate changed path".into()));
            }
        }
        Ok(Self(changes))
    }
    pub fn entries(&self) -> &BTreeMap<RepoPath, Option<FileEntry>> {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayerHead {
    repository: ObjectId,
    layer: LayerId,
    revision: u64,
    checkpoint: CheckpointId,
}

impl LayerHead {
    pub fn layer(&self) -> &LayerId {
        &self.layer
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn checkpoint(&self) -> CheckpointId {
        self.checkpoint
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LayerState {
    Active {
        head: LayerHead,
    },
    Discarded {
        layer: LayerId,
        revision: u64,
        checkpoint: CheckpointId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LayerUpdate {
    Applied { state: LayerState },
    Stale { state: LayerState },
}

/// A frozen graph read. Later head updates do not alter these values or objects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerCheckpoint {
    id: CheckpointId,
    base: LayerBase,
    changes: SparseChanges,
    manifest: Manifest,
}

impl LayerCheckpoint {
    pub fn id(&self) -> CheckpointId {
        self.id
    }
    pub fn base(&self) -> &LayerBase {
        &self.base
    }
    pub fn changes(&self) -> &SparseChanges {
        &self.changes
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// Status and diff share exact before/after entries, preserving type and mode changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PathDiff {
    pub path: RepoPath,
    pub before: Option<FileEntry>,
    pub after: Option<FileEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedCheckpoint {
    repository: ObjectId,
    version: u64,
    manifest: ObjectId,
    changes: BTreeMap<String, Option<FileEntry>>,
}

impl Repository {
    pub fn layer_base(&self, version: VersionNumber) -> Result<LayerBase> {
        let tx = self.conn.unchecked_transaction()?;
        let world = history_in(&tx)?
            .into_iter()
            .find(|v| v.number == version)
            .ok_or(Error::MissingVersion(version))?;
        let base = LayerBase {
            repository: identity_in(&tx)?,
            version,
            manifest: world.manifest,
        };
        tx.commit()?;
        Ok(base)
    }

    /// Retry with the same ID/base returns the initial frozen head, even after updates.
    pub fn create_layer(&mut self, layer: &LayerId, base: &LayerBase) -> Result<LayerHead> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_layers(&tx)?;
        let checkpoint = prepare_checkpoint(&tx, base, &SparseChanges::new([])?)?;
        let head = create_in(&tx, layer, checkpoint)?;
        tx.commit()?;
        Ok(head)
    }

    /// Adds references only. The source is an immutable checkpoint, never a live path.
    pub fn clone_layer(&mut self, layer: &LayerId, checkpoint: CheckpointId) -> Result<LayerHead> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_layers(&tx)?;
        checkpoint_in(&tx, checkpoint.0)?;
        let head = create_in(&tx, layer, checkpoint)?;
        tx.commit()?;
        Ok(head)
    }

    pub fn layer_state(&self, layer: &LayerId) -> Result<LayerState> {
        let tx = self.conn.unchecked_transaction()?;
        check_layers(&tx)?;
        let state = latest_in(&tx, layer)?.state(identity_in(&tx)?);
        tx.commit()?;
        Ok(state)
    }

    pub fn read_checkpoint(&self, checkpoint: CheckpointId) -> Result<LayerCheckpoint> {
        let tx = self.conn.unchecked_transaction()?;
        let frozen = checkpoint_in(&tx, checkpoint.0)?;
        tx.commit()?;
        Ok(frozen)
    }

    /// Enumerates retained immutable heads, including history before terminal discard.
    pub fn layer_history(&self, layer: &LayerId) -> Result<Vec<LayerState>> {
        let tx = self.conn.unchecked_transaction()?;
        check_layers(&tx)?;
        let latest = latest_in(&tx, layer)?;
        let repository = identity_in(&tx)?;
        let history = (0..=latest.revision)
            .map(|revision| Ok(event_in(&tx, layer, revision)?.state(repository)))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(history)
    }

    /// Future capture supplies a complete delta relative to the Layer's frozen World.
    /// On uncertain storage errors, retry the same operation, expected head and changes.
    pub fn capture_layer(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
        changes: &SparseChanges,
    ) -> Result<LayerUpdate> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_layers(&tx)?;
        verify_head(&tx, expected)?;
        let base = checkpoint_in(&tx, expected.checkpoint.0)?.base;
        let checkpoint = prepare_checkpoint(&tx, &base, changes)?;
        let outcome = update_in(&tx, operation, expected, checkpoint, false)?;
        #[cfg(test)]
        crate::repository::test_boundary("layer_before_commit");
        tx.commit()?;
        #[cfg(test)]
        crate::repository::test_boundary("layer_after_commit");
        Ok(outcome)
    }

    /// Discard is a terminal event; checkpoint and head history remain recoverable.
    pub fn discard_layer(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
    ) -> Result<LayerUpdate> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_layers(&tx)?;
        verify_head(&tx, expected)?;
        let outcome = update_in(&tx, operation, expected, expected.checkpoint, true)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Diff this exact frozen head, including an older head after further edits/discard.
    pub fn layer_diff(&self, head: &LayerHead) -> Result<Vec<PathDiff>> {
        let tx = self.conn.unchecked_transaction()?;
        verify_head(&tx, head)?;
        let frozen = checkpoint_in(&tx, head.checkpoint.0)?;
        let base = manifest_in(&tx, frozen.base.manifest)?;
        let diff = frozen
            .changes
            .0
            .into_iter()
            .map(|(path, after)| {
                let before = base.entries().get(&path).cloned();
                PathDiff {
                    path,
                    before,
                    after,
                }
            })
            .collect();
        tx.commit()?;
        Ok(diff)
    }
}

fn identity_in(conn: &Connection) -> Result<ObjectId> {
    let ids = conn
        .prepare("SELECT singleton,id FROM repository_identity")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match ids.as_slice() {
        [(1, id)] => decode_id(id),
        _ => Err(Error::CorruptRepository(
            "invalid repository identity".into(),
        )),
    }
}

fn validate_base(conn: &Connection, base: &LayerBase) -> Result<Manifest> {
    if identity_in(conn)? != base.repository {
        return Err(Error::InvalidInput("foreign Layer base".into()));
    }
    let world = history_in(conn)?
        .into_iter()
        .find(|v| v.number == base.version)
        .ok_or(Error::MissingVersion(base.version))?;
    if world.manifest != base.manifest {
        return Err(Error::CorruptRepository(
            "Layer base disagrees with exact World".into(),
        ));
    }
    manifest_in(conn, base.manifest)
}

fn normalize(
    conn: &Connection,
    base: &Manifest,
    changes: &SparseChanges,
) -> Result<(SparseChanges, Manifest)> {
    let mut merged = base.entries().clone();
    let mut delta = BTreeMap::new();
    for (path, after) in &changes.0 {
        if let Some(entry) = after {
            validate_entry(conn, entry)?;
        }
        if after.is_none() && !base.entries().contains_key(path) {
            let prefix = format!("{}/", path.as_str());
            let has_children = base
                .entries()
                .keys()
                .chain(
                    changes
                        .0
                        .iter()
                        .filter_map(|(p, entry)| entry.as_ref().map(|_| p)),
                )
                .any(|p| p.as_str().starts_with(&prefix));
            if has_children {
                return Err(Error::InvalidInput(
                    "directory-prefix tombstone is ambiguous; delete exact entries".into(),
                ));
            }
        }
        if base.entries().get(path) == after.as_ref() {
            continue;
        }
        match after {
            Some(entry) => {
                merged.insert(path.clone(), entry.clone());
            }
            None => {
                merged.remove(path);
            }
        }
        delta.insert(path.clone(), after.clone());
    }
    Ok((SparseChanges(delta), Manifest::new(merged)?))
}

fn checkpoint_bytes(base: &LayerBase, changes: &SparseChanges) -> Vec<u8> {
    serde_json::to_vec(&EncodedCheckpoint {
        repository: base.repository,
        version: base.version.get(),
        manifest: base.manifest,
        changes: changes
            .0
            .iter()
            .map(|(p, e)| (p.as_str().into(), e.clone()))
            .collect(),
    })
    .expect("checkpoint encoding")
}

fn prepare_checkpoint(
    conn: &Connection,
    base: &LayerBase,
    changes: &SparseChanges,
) -> Result<CheckpointId> {
    let base_manifest = validate_base(conn, base)?;
    let (changes, _) = normalize(conn, &base_manifest, changes)?;
    Ok(CheckpointId(insert_object(
        conn,
        ObjectKind::Checkpoint,
        &checkpoint_bytes(base, &changes),
    )?))
}

pub(crate) fn checkpoint_in(conn: &Connection, id: ObjectId) -> Result<LayerCheckpoint> {
    let bytes = read_object(conn, id, ObjectKind::Checkpoint)?;
    let encoded: EncodedCheckpoint =
        serde_json::from_slice(&bytes).map_err(|_| Error::CorruptObject(id))?;
    let base = LayerBase {
        repository: encoded.repository,
        version: VersionNumber::new(encoded.version).map_err(|_| Error::CorruptObject(id))?,
        manifest: encoded.manifest,
    };
    let changes = SparseChanges::new(
        encoded
            .changes
            .into_iter()
            .map(|(p, e)| Ok((RepoPath::parse(&p)?, e)))
            .collect::<Result<Vec<_>>>()
            .map_err(|_| Error::CorruptObject(id))?,
    )
    .map_err(|_| Error::CorruptObject(id))?;
    let base_manifest = validate_base(conn, &base)
        .map_err(|e| Error::CorruptRepository(format!("invalid checkpoint base: {e}")))?;
    let (normalized, manifest) = normalize(conn, &base_manifest, &changes)
        .map_err(|e| Error::CorruptRepository(format!("invalid checkpoint graph: {e}")))?;
    if normalized != changes || checkpoint_bytes(&base, &changes) != bytes {
        return Err(Error::CorruptObject(id));
    }
    Ok(LayerCheckpoint {
        id: CheckpointId(id),
        base,
        changes,
        manifest,
    })
}

struct Event {
    layer: LayerId,
    revision: i64,
    checkpoint: CheckpointId,
    discarded: bool,
    operation: Option<OperationId>,
}

impl Event {
    fn state(&self, repository: ObjectId) -> LayerState {
        if self.discarded {
            LayerState::Discarded {
                layer: self.layer.clone(),
                revision: self.revision as u64,
                checkpoint: self.checkpoint,
            }
        } else {
            LayerState::Active {
                head: LayerHead {
                    repository,
                    layer: self.layer.clone(),
                    revision: self.revision as u64,
                    checkpoint: self.checkpoint,
                },
            }
        }
    }
}

fn event_in(conn: &Connection, layer: &LayerId, revision: i64) -> Result<Event> {
    let (checkpoint, discarded, operation): (String, i64, Option<String>) = conn
        .query_row(
            "SELECT checkpoint,discarded,operation FROM layer_heads WHERE layer=?1 AND revision=?2",
            params![layer.as_str(), revision],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| Error::CorruptRepository("missing Layer head event".into()))?;
    if revision < 0 || ![0, 1].contains(&discarded) {
        return Err(Error::CorruptRepository("invalid Layer event".into()));
    }
    Ok(Event {
        layer: layer.clone(),
        revision,
        checkpoint: CheckpointId(decode_id(&checkpoint)?),
        discarded: discarded == 1,
        operation: operation
            .map(|s| {
                OperationId::parse(&s)
                    .map_err(|_| Error::CorruptRepository("invalid Layer operation".into()))
            })
            .transpose()?,
    })
}

fn latest_in(conn: &Connection, layer: &LayerId) -> Result<Event> {
    let revision: Option<i64> = conn.query_row(
        "SELECT max(revision) FROM layer_heads WHERE layer=?1",
        [layer.as_str()],
        |r| r.get(0),
    )?;
    event_in(
        conn,
        layer,
        revision.ok_or_else(|| Error::InvalidInput("unknown Layer".into()))?,
    )
}

fn verify_head(conn: &Connection, head: &LayerHead) -> Result<()> {
    if identity_in(conn)? != head.repository {
        return Err(Error::InvalidInput("foreign Layer head".into()));
    }
    let revision = i64::try_from(head.revision)
        .map_err(|_| Error::InvalidInput("Layer revision overflow".into()))?;
    let event = event_in(conn, &head.layer, revision)?;
    if event.discarded || event.checkpoint != head.checkpoint {
        return Err(Error::InvalidInput("invalid active head token".into()));
    }
    Ok(())
}

fn create_in(conn: &Connection, layer: &LayerId, checkpoint: CheckpointId) -> Result<LayerHead> {
    let exists = conn
        .prepare("SELECT 1 FROM layer_heads WHERE layer=?1")?
        .exists([layer.as_str()])?;
    if exists {
        if event_in(conn, layer, 0)?.checkpoint != checkpoint {
            return Err(Error::InvalidInput(
                "Layer ID reused with different snapshot".into(),
            ));
        }
    } else {
        conn.execute(
            "INSERT INTO layer_heads(layer,revision,checkpoint,discarded) VALUES(?1,0,?2,0)",
            params![layer.as_str(), checkpoint.0.to_string()],
        )?;
    }
    Ok(LayerHead {
        repository: identity_in(conn)?,
        layer: layer.clone(),
        revision: 0,
        checkpoint,
    })
}

fn update_in(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    checkpoint: CheckpointId,
    discard: bool,
) -> Result<LayerUpdate> {
    let prior = conn
        .query_row(
            "SELECT layer,expected,checkpoint,discard FROM layer_attempts WHERE id=?1",
            [operation.as_str()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((layer, revision, candidate, was_discard)) = prior {
        if layer != expected.layer.as_str()
            || revision != expected.revision as i64
            || candidate != checkpoint.0.to_string()
            || was_discard != discard
        {
            return Err(Error::OperationIdReused(operation.clone()));
        }
        return attempt_outcome(conn, operation);
    }
    let current = latest_in(conn, &expected.layer)?;
    let accepted = current.revision == expected.revision as i64 && !current.discarded;
    let result = if accepted {
        current
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("Layer revision overflow".into()))?
    } else {
        current.revision
    };
    conn.execute("INSERT INTO layer_attempts(id,layer,expected,checkpoint,discard,accepted,result) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![operation.as_str(),expected.layer.as_str(),expected.revision as i64,checkpoint.0.to_string(),discard,accepted,result])?;
    if accepted {
        conn.execute("INSERT INTO layer_heads(layer,revision,checkpoint,discarded,operation) VALUES(?1,?2,?3,?4,?5)",
            params![expected.layer.as_str(),result,checkpoint.0.to_string(),discard,operation.as_str()])?;
    }
    attempt_outcome(conn, operation)
}

fn attempt_outcome(conn: &Connection, operation: &OperationId) -> Result<LayerUpdate> {
    let (layer, expected, checkpoint, discard, accepted, result): (
        String,
        i64,
        String,
        i64,
        i64,
        i64,
    ) = conn.query_row(
        "SELECT layer,expected,checkpoint,discard,accepted,result FROM layer_attempts WHERE id=?1",
        [operation.as_str()],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let layer = LayerId::parse(&layer)
        .map_err(|_| Error::CorruptRepository("invalid stored Layer ID".into()))?;
    let before = event_in(conn, &layer, expected)?;
    let after = event_in(conn, &layer, result)?;
    let candidate = checkpoint_in(conn, decode_id(&checkpoint)?)?;
    if before.discarded
        || ![0, 1].contains(&discard)
        || ![0, 1].contains(&accepted)
        || checkpoint_in(conn, before.checkpoint.0)?.base != candidate.base
        || (discard == 1 && candidate.id != before.checkpoint)
    {
        return Err(Error::CorruptRepository(
            "invalid Layer attempt binding".into(),
        ));
    }
    let state = after.state(identity_in(conn)?);
    if accepted == 1 {
        if expected.checked_add(1) != Some(result)
            || after.checkpoint != candidate.id
            || after.discarded != (discard == 1)
            || after.operation.as_ref() != Some(operation)
        {
            return Err(Error::CorruptRepository(
                "accepted Layer attempt disagrees with head".into(),
            ));
        }
        Ok(LayerUpdate::Applied { state })
    } else {
        if result <= expected {
            return Err(Error::CorruptRepository(
                "invalid stale Layer outcome".into(),
            ));
        }
        Ok(LayerUpdate::Stale { state })
    }
}

pub(crate) fn check_layers(conn: &Connection) -> Result<usize> {
    identity_in(conn)?;
    let layers = conn
        .prepare("SELECT DISTINCT layer FROM layer_heads ORDER BY layer")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for layer in &layers {
        let layer = LayerId::parse(layer)
            .map_err(|_| Error::CorruptRepository("invalid stored Layer ID".into()))?;
        let revisions = conn
            .prepare("SELECT revision FROM layer_heads WHERE layer=?1 ORDER BY revision")?
            .query_map([layer.as_str()], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut previous: Option<Event> = None;
        let mut base = None;
        for (index, revision) in revisions.into_iter().enumerate() {
            if revision != index as i64 {
                return Err(Error::CorruptRepository(
                    "Layer head history is not contiguous".into(),
                ));
            }
            let event = event_in(conn, &layer, revision)?;
            let snapshot = checkpoint_in(conn, event.checkpoint.0)?;
            if let Some(base) = &base {
                if base != &snapshot.base {
                    return Err(Error::CorruptRepository("Layer base changed".into()));
                }
            } else {
                base = Some(snapshot.base);
            }
            if let Some(prev) = previous {
                if prev.discarded || (event.discarded && prev.checkpoint != event.checkpoint) {
                    return Err(Error::CorruptRepository("invalid discard history".into()));
                }
                let op = event.operation.as_ref().ok_or_else(|| {
                    Error::CorruptRepository("missing Layer head operation".into())
                })?;
                match attempt_outcome(conn, op)? {
                    LayerUpdate::Applied { state } if state == event.state(identity_in(conn)?) => {}
                    _ => {
                        return Err(Error::CorruptRepository(
                            "Layer head operation disagrees".into(),
                        ));
                    }
                }
            } else if event.operation.is_some() || event.discarded {
                return Err(Error::CorruptRepository("invalid initial Layer".into()));
            }
            previous = Some(event);
        }
    }
    let operations = conn
        .prepare("SELECT id FROM layer_attempts")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for operation in operations {
        let op = OperationId::parse(&operation)
            .map_err(|_| Error::CorruptRepository("invalid stored Layer operation".into()))?;
        attempt_outcome(conn, &op)?;
    }
    Ok(layers.len())
}
