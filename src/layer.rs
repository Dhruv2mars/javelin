//! Sparse immutable checkpoints and repository-local durable Layer heads.
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    Error, FileEntry, Manifest, ObjectId, OperationId, RepoPath, Repository, Result, SparseChanges,
    VersionNumber,
    domain::{ObjectKind, WorldIndex},
    repository::{Index, decode_id, insert_object, read_object, validate_format, version_in},
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
    stack INTEGER NOT NULL DEFAULT 0 CHECK(stack IN (0,1)),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
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
/// `world` is the base version's hash-chain identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayerBase {
    repository: ObjectId,
    version: VersionNumber,
    world: ObjectId,
}

impl LayerBase {
    pub fn version(&self) -> VersionNumber {
        self.version
    }
    pub fn world(&self) -> ObjectId {
        self.world
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
    Published {
        head: LayerHead,
        operation: OperationId,
        version: crate::WorldVersion,
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
/// Changes are relative to the base; `Repository::checkpoint_manifest` derives the
/// full snapshot when a caller needs one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerCheckpoint {
    id: CheckpointId,
    base: LayerBase,
    changes: SparseChanges,
    sources: Vec<CheckpointId>,
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
    /// Frozen direct Stack sources, retained by later captures and clones.
    pub fn stack_sources(&self) -> &[CheckpointId] {
        &self.sources
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
    world: ObjectId,
    changes: BTreeMap<String, Option<FileEntry>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sources: Vec<CheckpointId>,
}

impl Repository {
    pub fn layer_base(&self, version: VersionNumber) -> Result<LayerBase> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let base = LayerBase {
            repository: identity_in(&tx)?,
            version,
            world: version_in(&tx, version)?.id,
        };
        tx.commit()?;
        Ok(base)
    }

    /// Retry with the same ID/base returns the initial frozen head, even after updates.
    pub fn create_layer(&mut self, layer: &LayerId, base: &LayerBase) -> Result<LayerHead> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_format(&tx)?;
        let (checkpoint, changes) = prepare_checkpoint(&tx, base, &SparseChanges::new([])?, &[])?;
        let head = create_in(&tx, layer, checkpoint, &changes)?;
        tx.commit()?;
        Ok(head)
    }

    /// Adds references only. The source is an immutable checkpoint, never a live path.
    pub fn clone_layer(&mut self, layer: &LayerId, checkpoint: CheckpointId) -> Result<LayerHead> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_format(&tx)?;
        let frozen = checkpoint_in(&tx, checkpoint.0)?;
        let head = create_in(&tx, layer, checkpoint, &frozen.changes)?;
        tx.commit()?;
        Ok(head)
    }

    pub fn layer_state(&self, layer: &LayerId) -> Result<LayerState> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let state = crate::publish::published_state_in(&tx, layer)?
            .unwrap_or(latest_in(&tx, layer)?.state(identity_in(&tx)?));
        tx.commit()?;
        Ok(state)
    }

    /// Verifies the checkpoint and its complete Stack ancestry.
    pub fn read_checkpoint(&self, checkpoint: CheckpointId) -> Result<LayerCheckpoint> {
        let tx = self.conn.unchecked_transaction()?;
        let frozen = (*CheckpointReader::new(&tx).read(checkpoint.0)?).clone();
        tx.commit()?;
        Ok(frozen)
    }

    /// Derives the checkpoint's complete snapshot: its base World plus its changes.
    pub fn checkpoint_manifest(&self, checkpoint: CheckpointId) -> Result<Manifest> {
        let tx = self.conn.unchecked_transaction()?;
        let frozen = checkpoint_in(&tx, checkpoint.0)?;
        let index = Index::new(&tx);
        let mut entries = index.trees.entries(index.root(frozen.base.version)?)?;
        frozen.changes.apply(&mut entries);
        tx.commit()?;
        Manifest::new(entries)
    }

    /// Enumerates immutable head events, followed by terminal publication when present.
    /// Verifies this Layer's complete event and attempt history; other Layers and
    /// the World are `check()`'s.
    pub fn layer_history(&self, layer: &LayerId) -> Result<Vec<LayerState>> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        check_layer(&mut CheckpointReader::shallow(&tx), layer)?;
        let latest = latest_in(&tx, layer)?;
        let repository = identity_in(&tx)?;
        let mut history = (0..=latest.revision)
            .map(|revision| Ok(event_in(&tx, layer, revision)?.state(repository)))
            .collect::<Result<Vec<_>>>()?;
        if let Some(published) = crate::publish::published_state_in(&tx, layer)? {
            history.push(published);
        }
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
        validate_format(&tx)?;
        verify_head(&tx, expected)?;
        crate::publish::refuse_published_in(&tx, operation, expected.layer())?;
        let before = checkpoint_in(&tx, expected.checkpoint.0)?;
        let candidate = prepare_checkpoint(&tx, &before.base, changes, &before.sources)?;
        let outcome = update_in(&tx, operation, expected, candidate, false, None)?;
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
        validate_format(&tx)?;
        verify_head(&tx, expected)?;
        crate::publish::refuse_published_in(&tx, operation, expected.layer())?;
        // A discard untracks the Layer, so its changes are not needed.
        let candidate = (expected.checkpoint, SparseChanges::default());
        let outcome = update_in(&tx, operation, expected, candidate, true, None)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Diff this exact frozen head, including an older head after further edits/discard.
    pub fn layer_diff(&self, head: &LayerHead) -> Result<Vec<PathDiff>> {
        let tx = self.conn.unchecked_transaction()?;
        verify_head(&tx, head)?;
        let frozen = checkpoint_in(&tx, head.checkpoint.0)?;
        let index = Index::new(&tx);
        let diff = frozen
            .changes
            .entries()
            .iter()
            .map(|(path, after)| {
                Ok(PathDiff {
                    path: path.clone(),
                    before: index.entry_at(path, frozen.base.version)?,
                    after: after.clone(),
                })
            })
            .collect::<Result<_>>()?;
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

fn validate_base(conn: &Connection, base: &LayerBase) -> Result<()> {
    if identity_in(conn)? != base.repository {
        return Err(Error::InvalidInput("foreign Layer base".into()));
    }
    if version_in(conn, base.version)?.id != base.world {
        return Err(Error::CorruptRepository(
            "Layer base disagrees with exact World".into(),
        ));
    }
    Ok(())
}

fn checkpoint_bytes(
    base: &LayerBase,
    changes: &SparseChanges,
    sources: &[CheckpointId],
) -> Vec<u8> {
    serde_json::to_vec(&EncodedCheckpoint {
        repository: base.repository,
        version: base.version.get(),
        world: base.world,
        sources: sources.to_vec(),
        changes: changes.encoded(),
    })
    .expect("checkpoint encoding")
}

/// Stores the normalized checkpoint and returns it with its exact changes.
pub(crate) fn prepare_checkpoint(
    conn: &Connection,
    base: &LayerBase,
    changes: &SparseChanges,
    sources: &[CheckpointId],
) -> Result<(CheckpointId, SparseChanges)> {
    validate_base(conn, base)?;
    let changes = crate::domain::normalize(&Index::new(conn), base.version, changes)?;
    let id = insert_object(
        conn,
        ObjectKind::Checkpoint,
        &checkpoint_bytes(base, &changes, sources),
    )?;
    Ok((CheckpointId(id), changes))
}

/// Scoped to one validation transaction; never stored on a Repository handle.
/// Only fully verified graphs enter this cache. Rc avoids copying sparse trees
/// every time an event/attempt refers to the same immutable checkpoint. A shallow
/// reader verifies only the checkpoints it is asked for, not their Stack sources;
/// commands use it so cost follows what they touch, and `check()` reads deeply.
pub(crate) struct CheckpointReader<'a> {
    pub(crate) conn: &'a Connection,
    checked: BTreeMap<ObjectId, Rc<LayerCheckpoint>>,
    deep: bool,
}

impl<'a> CheckpointReader<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self {
            conn,
            checked: BTreeMap::new(),
            deep: true,
        }
    }

    fn shallow(conn: &'a Connection) -> Self {
        Self {
            deep: false,
            ..Self::new(conn)
        }
    }

    pub(crate) fn read(&mut self, id: ObjectId) -> Result<Rc<LayerCheckpoint>> {
        let mut pending = vec![(id, None)];
        let mut visiting = BTreeSet::new();
        while let Some((id, leaving)) = pending.pop() {
            if let Some(node) = leaving {
                visiting.remove(&id);
                self.checked.insert(id, node);
            } else if !self.checked.contains_key(&id) {
                if !visiting.insert(id) {
                    return Err(Error::CorruptRepository("cyclic Stack ancestry".into()));
                }
                let node = Rc::new(checkpoint_node(self.conn, id)?);
                let sources = if self.deep {
                    node.sources.clone()
                } else {
                    vec![]
                };
                pending.push((id, Some(node)));
                pending.extend(sources.into_iter().rev().map(|source| (source.0, None)));
            }
        }
        Ok(Rc::clone(&self.checked[&id]))
    }
}

/// Reads and verifies one checkpoint without walking its Stack ancestry.
pub(crate) fn checkpoint_in(conn: &Connection, id: ObjectId) -> Result<LayerCheckpoint> {
    Ok((*CheckpointReader::shallow(conn).read(id)?).clone())
}

#[cfg(test)]
thread_local! {
    static CHECKPOINT_NODE_READS: std::cell::RefCell<BTreeMap<ObjectId, usize>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

fn checkpoint_node(conn: &Connection, id: ObjectId) -> Result<LayerCheckpoint> {
    #[cfg(test)]
    CHECKPOINT_NODE_READS.with(|reads| *reads.borrow_mut().entry(id).or_default() += 1);
    let bytes = read_object(conn, id, ObjectKind::Checkpoint)?;
    let encoded: EncodedCheckpoint =
        serde_json::from_slice(&bytes).map_err(|_| Error::CorruptObject(id))?;
    let base = LayerBase {
        repository: encoded.repository,
        version: VersionNumber::new(encoded.version).map_err(|_| Error::CorruptObject(id))?,
        world: encoded.world,
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
    validate_base(conn, &base)
        .map_err(|e| Error::CorruptRepository(format!("invalid checkpoint base: {e}")))?;
    let normalized = crate::domain::normalize(&Index::new(conn), base.version, &changes)
        .map_err(|e| Error::CorruptRepository(format!("invalid checkpoint graph: {e}")))?;
    if normalized != changes
        || encoded.sources.windows(2).any(|pair| pair[0] >= pair[1])
        || checkpoint_bytes(&base, &changes, &encoded.sources) != bytes
    {
        return Err(Error::CorruptObject(id));
    }
    Ok(LayerCheckpoint {
        id: CheckpointId(id),
        base,
        changes,
        sources: encoded.sources,
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

pub(crate) fn verify_head(conn: &Connection, head: &LayerHead) -> Result<()> {
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

fn create_in(
    conn: &Connection,
    layer: &LayerId,
    checkpoint: CheckpointId,
    changes: &SparseChanges,
) -> Result<LayerHead> {
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
        crate::radar::track_in(conn, layer, changes)?;
    }
    Ok(LayerHead {
        repository: identity_in(conn)?,
        layer: layer.clone(),
        revision: 0,
        checkpoint,
    })
}

/// `candidate` is the new head checkpoint and its changes. An applied update also
/// moves the Layer's entries in the radar index, in the same transaction.
pub(crate) fn update_in(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    candidate: (CheckpointId, SparseChanges),
    discard: bool,
    source: Option<CheckpointId>,
) -> Result<LayerUpdate> {
    let (checkpoint, changes) = candidate;
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
            || crate::stack::source_in(conn, operation)? != source
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
    conn.execute("INSERT INTO layer_attempts(id,layer,expected,checkpoint,discard,accepted,result,stack) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![operation.as_str(),expected.layer.as_str(),expected.revision as i64,checkpoint.0.to_string(),discard,accepted,result,source.is_some()])?;
    if accepted {
        conn.execute("INSERT INTO layer_heads(layer,revision,checkpoint,discarded,operation) VALUES(?1,?2,?3,?4,?5)",
            params![expected.layer.as_str(),result,checkpoint.0.to_string(),discard,operation.as_str()])?;
        if discard {
            crate::radar::untrack_in(conn, &expected.layer)?;
        } else {
            crate::radar::track_in(conn, &expected.layer, &changes)?;
        }
    }
    if let Some(source) = source {
        let state = event_in(conn, &expected.layer, result)?.state(identity_in(conn)?);
        let outcome = if accepted {
            LayerUpdate::Applied { state }
        } else {
            LayerUpdate::Stale { state }
        };
        crate::stack::record_in(conn, operation, expected, source, checkpoint, &outcome)?;
    }
    attempt_outcome(conn, operation)
}

pub(crate) fn attempt_outcome(conn: &Connection, operation: &OperationId) -> Result<LayerUpdate> {
    attempt_outcome_in(&mut CheckpointReader::shallow(conn), operation)
}

fn attempt_outcome_in(
    checkpoints: &mut CheckpointReader<'_>,
    operation: &OperationId,
) -> Result<LayerUpdate> {
    let conn = checkpoints.conn;
    let (layer, expected, checkpoint, discard, accepted, result, stack): (
        String,
        i64,
        String,
        i64,
        i64,
        i64,
        i64,
    ) = conn.query_row(
        "SELECT layer,expected,checkpoint,discard,accepted,result,stack FROM layer_attempts WHERE id=?1",
        [operation.as_str()],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    )?;
    let layer = LayerId::parse(&layer)
        .map_err(|_| Error::CorruptRepository("invalid stored Layer ID".into()))?;
    let before = event_in(conn, &layer, expected)?;
    let after = event_in(conn, &layer, result)?;
    let candidate = checkpoints.read(decode_id(&checkpoint)?)?;
    let before_checkpoint = checkpoints.read(before.checkpoint.0)?;
    if before.discarded
        || ![0, 1].contains(&discard)
        || ![0, 1].contains(&accepted)
        || before_checkpoint.base != candidate.base
        || (discard == 1 && candidate.id != before.checkpoint)
    {
        return Err(Error::CorruptRepository(
            "invalid Layer attempt binding".into(),
        ));
    }
    let source = crate::stack::source_in(conn, operation)?;
    if stack != i64::from(source.is_some()) {
        return Err(Error::CorruptRepository(
            "missing or unexpected Stack binding".into(),
        ));
    }
    let mut sources = before_checkpoint.sources.clone();
    if let Some(source) = source {
        if discard != 0 {
            return Err(Error::CorruptRepository("Stack cannot discard".into()));
        }
        checkpoints.read(source.0)?;
        sources.push(source);
        sources.sort();
        sources.dedup();
    }
    if candidate.sources != sources {
        return Err(Error::CorruptRepository(
            "Layer attempt lost or changed Stack ancestry".into(),
        ));
    }
    let state = after.state(identity_in(conn)?);
    let outcome = if accepted == 1 {
        if expected.checked_add(1) != Some(result)
            || after.checkpoint != candidate.id
            || after.discarded != (discard == 1)
            || after.operation.as_ref() != Some(operation)
        {
            return Err(Error::CorruptRepository(
                "accepted Layer attempt disagrees with head".into(),
            ));
        }
        LayerUpdate::Applied { state }
    } else {
        if result <= expected {
            return Err(Error::CorruptRepository(
                "invalid stale Layer outcome".into(),
            ));
        }
        LayerUpdate::Stale { state }
    };
    if let Some(source) = source {
        crate::stack::verify_binding(
            conn,
            operation,
            &head_in(conn, &layer, expected as u64)?,
            source,
            candidate.id,
            &outcome,
        )?;
    }
    Ok(outcome)
}

pub(crate) fn check_layers(checkpoints: &mut CheckpointReader<'_>) -> Result<usize> {
    let conn = checkpoints.conn;
    identity_in(conn)?;
    let layers = conn
        .prepare("SELECT DISTINCT layer FROM layer_heads ORDER BY layer")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for layer in &layers {
        let layer = LayerId::parse(layer)
            .map_err(|_| Error::CorruptRepository("invalid stored Layer ID".into()))?;
        check_events(checkpoints, &layer)?;
    }
    let operations = conn
        .prepare("SELECT id FROM layer_attempts")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    check_attempts(checkpoints, operations)?;
    Ok(layers.len())
}

/// One Layer's complete event and attempt history, for reads of that history.
fn check_layer(checkpoints: &mut CheckpointReader<'_>, layer: &LayerId) -> Result<()> {
    check_events(checkpoints, layer)?;
    let operations = checkpoints
        .conn
        .prepare("SELECT id FROM layer_attempts WHERE layer=?1")?
        .query_map([layer.as_str()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    check_attempts(checkpoints, operations)
}

fn check_attempts(checkpoints: &mut CheckpointReader<'_>, operations: Vec<String>) -> Result<()> {
    for operation in operations {
        let op = OperationId::parse(&operation)
            .map_err(|_| Error::CorruptRepository("invalid stored Layer operation".into()))?;
        attempt_outcome_in(checkpoints, &op)?;
    }
    Ok(())
}

fn check_events(checkpoints: &mut CheckpointReader<'_>, layer: &LayerId) -> Result<()> {
    let conn = checkpoints.conn;
    let layer = layer.clone();
    {
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
            let snapshot = checkpoints.read(event.checkpoint.0)?;
            if let Some(base) = &base {
                if base != &snapshot.base {
                    return Err(Error::CorruptRepository("Layer base changed".into()));
                }
            } else {
                base = Some(snapshot.base.clone());
            }
            if let Some(prev) = previous {
                if prev.discarded || (event.discarded && prev.checkpoint != event.checkpoint) {
                    return Err(Error::CorruptRepository("invalid discard history".into()));
                }
                let op = event.operation.as_ref().ok_or_else(|| {
                    Error::CorruptRepository("missing Layer head operation".into())
                })?;
                match attempt_outcome_in(checkpoints, op)? {
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
    Ok(())
}

/// Raw historical accessors. These never project today's publication status.
pub(crate) fn event_state_in(
    conn: &Connection,
    layer: &LayerId,
    revision: u64,
) -> Result<LayerState> {
    let revision = i64::try_from(revision)
        .map_err(|_| Error::CorruptRepository("invalid Layer event revision".into()))?;
    Ok(event_in(conn, layer, revision)?.state(identity_in(conn)?))
}

pub(crate) fn head_in(conn: &Connection, layer: &LayerId, revision: u64) -> Result<LayerHead> {
    match event_state_in(conn, layer, revision)? {
        LayerState::Active { head } => Ok(head),
        _ => Err(Error::CorruptRepository(
            "expected an Active Layer event".into(),
        )),
    }
}

pub(crate) fn latest_event_state_in(conn: &Connection, layer: &LayerId) -> Result<LayerState> {
    Ok(latest_in(conn, layer)?.state(identity_in(conn)?))
}

#[cfg(test)]
#[path = "../tests/support/stack_validation.rs"]
mod stack_validation_tests;
