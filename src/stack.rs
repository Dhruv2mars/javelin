//! Atomic storage boundary for Stack. Mechanical merging lives in javelin-stack
//! so the merge engine can continue to depend on the native storage crate.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::domain::ObjectKind;
use crate::layer::{attempt_outcome, checkpoint_in, prepare_checkpoint, update_in, verify_head};
use crate::repository::{decode_id, insert_object, read_object, validate_format};
use crate::{
    CheckpointId, Error, LayerHead, LayerUpdate, ObjectId, OperationId, Repository, Result,
    SparseChanges,
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE stacks (
    id TEXT PRIMARY KEY REFERENCES layer_attempts(id) DEFERRABLE INITIALLY DEFERRED,
    source TEXT NOT NULL REFERENCES objects(id),
    binding TEXT NOT NULL REFERENCES objects(id)
) STRICT;
";

impl Repository {
    /// Replay a committed Stack before repeating merge preparation. Binds the exact
    /// target token and frozen source, including original successful/stale outcomes.
    pub fn stack_operation(
        &self,
        operation: &OperationId,
        expected: &LayerHead,
        source: CheckpointId,
    ) -> Result<Option<LayerUpdate>> {
        self.stack_operation_resolved(operation, expected, source, None)
    }

    /// As [`Repository::stack_operation`], for a Stack whose conflicts were resolved by the
    /// stored resolution object. A retry must name the same resolution, or the ID is reused.
    pub fn stack_operation_resolved(
        &self,
        operation: &OperationId,
        expected: &LayerHead,
        source: CheckpointId,
        resolution: Option<ObjectId>,
    ) -> Result<Option<LayerUpdate>> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        let result = replay_in(&tx, operation, expected, source, resolution)?;
        tx.commit()?;
        Ok(result)
    }

    /// Storage primitive for a clean, prepared merge. `changes` is the complete
    /// delta against the target's original World base. Prefer javelin-stack for
    /// bounded mechanical merging. This never refreshes or publishes either Layer.
    /// Checkpoint, ancestry, CAS event and retry binding commit atomically.
    pub fn commit_stack(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
        source: CheckpointId,
        changes: &SparseChanges,
    ) -> Result<LayerUpdate> {
        self.commit_stack_resolved(operation, expected, source, changes, None)
    }

    /// As [`Repository::commit_stack`], recording the object that resolved the conflicts.
    /// `resolution` is an immutable blob the Stack binding names; it is verified on replay.
    pub fn commit_stack_resolved(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
        source: CheckpointId,
        changes: &SparseChanges,
        resolution: Option<ObjectId>,
    ) -> Result<LayerUpdate> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_format(&tx)?;
        let provenance = self.provenance.as_ref();
        let outcome = stack_in(
            &tx, operation, expected, source, changes, resolution, provenance,
        )?;
        #[cfg(test)]
        crate::repository::test_boundary("stack_before_commit");
        tx.commit()?;
        #[cfg(test)]
        crate::repository::test_boundary("stack_after_commit");
        Ok(outcome)
    }

    /// The stored bytes of checkpoint `id`, after verifying it. The bytes hash to `id`,
    /// so another store can check them before reading their changes.
    pub fn checkpoint_object(&self, id: CheckpointId) -> Result<Vec<u8>> {
        let tx = self.conn.unchecked_transaction()?;
        validate_format(&tx)?;
        checkpoint_in(&tx, id.object())?;
        let bytes = read_object(&tx, id.object(), ObjectKind::Checkpoint)?;
        tx.commit()?;
        Ok(bytes)
    }

    /// The changes in checkpoint bytes from another store, such as a node's Layer head.
    /// Only the encoding is checked: the base names the other store's World.
    pub fn foreign_checkpoint_changes(bytes: &[u8]) -> Result<SparseChanges> {
        #[derive(Deserialize)]
        struct Encoded {
            changes: std::collections::BTreeMap<String, Option<crate::FileEntry>>,
        }
        let id = ObjectId::for_checkpoint(bytes);
        let encoded: Encoded =
            serde_json::from_slice(bytes).map_err(|_| Error::CorruptObject(id))?;
        SparseChanges::new(
            encoded
                .changes
                .into_iter()
                .map(|(path, entry)| Ok((crate::RepoPath::parse(&path)?, entry)))
                .collect::<Result<Vec<_>>>()?,
        )
        .map_err(|_| Error::CorruptObject(id))
    }

    /// Moves `expected` to a head another store produced, such as a Stack on a node.
    /// `changes` is that head's delta against the same base. The changes are stored as a
    /// standalone checkpoint and stacked onto `expected` in one transaction, so the new head
    /// is a Stack result: capture treats a directory that does not hold it as behind, and
    /// a refresh brings the directory to it. Blobs must already be stored. A retry with the
    /// same operation replays the original outcome.
    pub fn import_stacked_head(
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
        let before = checkpoint_in(&tx, expected.checkpoint().object())?;
        let imported = prepare_checkpoint(&tx, before.base(), changes, &[], None)?;
        let outcome = stack_in(&tx, operation, expected, imported.id, changes, None, None)?;
        tx.commit()?;
        Ok(outcome)
    }
}

/// Commits a prepared Stack inside the caller's transaction.
fn stack_in(
    tx: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
    changes: &SparseChanges,
    resolution: Option<ObjectId>,
    provenance: Option<&crate::Provenance>,
) -> Result<LayerUpdate> {
    verify_head(tx, expected)?;
    crate::publish::refuse_published_in(tx, operation, expected.layer())?;
    checkpoint_in(tx, source.object())?;
    let before = checkpoint_in(tx, expected.checkpoint().object())?;
    let mut sources = before.stack_sources().to_vec();
    sources.push(source);
    sources.sort();
    sources.dedup();
    let candidate = prepare_checkpoint(tx, before.base(), changes, &sources, provenance)?;
    update_in(
        tx,
        operation,
        expected,
        candidate,
        false,
        Some(source),
        resolution,
    )
}

fn replay_in(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
    resolution: Option<ObjectId>,
) -> Result<Option<LayerUpdate>> {
    let prior: Option<(String, i64)> = conn
        .query_row(
            "SELECT layer,expected FROM layer_attempts WHERE id=?1",
            [operation.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((layer, revision)) = prior else {
        return Ok(None);
    };
    if layer != expected.layer().as_str() || revision != expected.revision() as i64 {
        return Err(Error::OperationIdReused(operation.clone()));
    }
    // Verifies the attempt's Stack binding before comparing its source.
    let outcome = attempt_outcome(conn, operation)?;
    if source_in(conn, operation)? != Some(source) || resolution_in(conn, operation)? != resolution
    {
        return Err(Error::OperationIdReused(operation.clone()));
    }
    verify_head(conn, expected)?;
    Ok(Some(outcome))
}

pub(crate) fn source_in(
    conn: &Connection,
    operation: &OperationId,
) -> Result<Option<CheckpointId>> {
    let source: Option<String> = conn
        .query_row(
            "SELECT source FROM stacks WHERE id=?1",
            [operation.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    source
        .map(|s| {
            CheckpointId::parse(&s)
                .map_err(|_| Error::CorruptRepository("invalid Stack source ID".into()))
        })
        .transpose()
}

/// The resolution object a committed Stack names, if any.
pub(crate) fn resolution_in(
    conn: &Connection,
    operation: &OperationId,
) -> Result<Option<ObjectId>> {
    let binding: Option<String> = conn
        .query_row(
            "SELECT binding FROM stacks WHERE id=?1",
            [operation.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(binding) = binding else {
        return Ok(None);
    };
    let bytes = read_object(conn, decode_id(&binding)?, ObjectKind::Blob)?;
    Ok(stored_binding(&bytes)?.resolution)
}

fn stored_binding(bytes: &[u8]) -> Result<StoredBinding> {
    serde_json::from_slice(bytes)
        .map_err(|_| Error::CorruptRepository("invalid Stack input binding".into()))
}

pub(crate) fn check_stacks(conn: &Connection) -> Result<()> {
    if conn
        .prepare(
            "SELECT 1 FROM stacks s LEFT JOIN layer_attempts a ON a.id=s.id WHERE a.id IS NULL",
        )?
        .exists([])?
    {
        return Err(Error::CorruptRepository("orphan Stack attempt".into()));
    }
    let mut bindings = conn.prepare("SELECT binding FROM stacks")?;
    for binding in bindings.query_map([], |r| r.get::<_, String>(0))? {
        let bytes = read_object(conn, decode_id(&binding?)?, ObjectKind::Blob)?;
        if let Some(resolution) = stored_binding(&bytes)?.resolution {
            read_object(conn, resolution, ObjectKind::Blob)?;
        }
    }
    Ok(())
}

// The hash-bound binding protects exact source input even when the source was
// already present in the target ancestry (set membership alone cannot bind it).
// `resolution` is absent for a Stack that needed no owner choice, which keeps those
// bindings byte-identical to earlier ones.
#[derive(Serialize)]
struct StackBinding<'a> {
    operation: &'a OperationId,
    expected: &'a LayerHead,
    source: CheckpointId,
    candidate: CheckpointId,
    outcome: &'a LayerUpdate,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<ObjectId>,
}

#[derive(Deserialize)]
struct StoredBinding {
    #[serde(default)]
    resolution: Option<ObjectId>,
}

fn binding_bytes(
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
    candidate: CheckpointId,
    outcome: &LayerUpdate,
    resolution: Option<ObjectId>,
) -> Vec<u8> {
    serde_json::to_vec(&StackBinding {
        operation,
        expected,
        source,
        candidate,
        outcome,
        resolution,
    })
    .expect("Stack binding encoding")
}

pub(crate) fn record_in(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
    candidate: CheckpointId,
    outcome: &LayerUpdate,
    resolution: Option<ObjectId>,
) -> Result<()> {
    let binding = insert_object(
        conn,
        ObjectKind::Blob,
        &binding_bytes(operation, expected, source, candidate, outcome, resolution),
    )?;
    conn.execute(
        "INSERT INTO stacks(id,source,binding) VALUES(?1,?2,?3)",
        params![
            operation.as_str(),
            source.object().to_string(),
            binding.to_string()
        ],
    )?;
    Ok(())
}

pub(crate) fn verify_binding(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
    candidate: CheckpointId,
    outcome: &LayerUpdate,
) -> Result<()> {
    let binding: String = conn.query_row(
        "SELECT binding FROM stacks WHERE id=?1",
        [operation.as_str()],
        |r| r.get(0),
    )?;
    let stored = read_object(conn, decode_id(&binding)?, ObjectKind::Blob)?;
    let resolution = stored_binding(&stored)?.resolution;
    if stored != binding_bytes(operation, expected, source, candidate, outcome, resolution) {
        return Err(Error::CorruptRepository(
            "Stack input binding disagrees".into(),
        ));
    }
    if let Some(resolution) = resolution {
        read_object(conn, resolution, ObjectKind::Blob)?;
    }
    Ok(())
}
