//! Atomic storage boundary for Stack. Mechanical merging lives in javelin-stack
//! so the merge engine can continue to depend on the native storage crate.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::domain::ObjectKind;
use crate::layer::{attempt_outcome, checkpoint_in, prepare_checkpoint, update_in, verify_head};
use crate::repository::{decode_id, insert_object, read_object, validate_state_in};
use crate::{
    CheckpointId, Error, LayerHead, LayerUpdate, OperationId, Repository, Result, SparseChanges,
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
        let tx = self.conn.unchecked_transaction()?;
        validate_state_in(&tx)?;
        let result = replay_in(&tx, operation, expected, source)?;
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
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_state_in(&tx)?;
        verify_head(&tx, expected)?;
        crate::publish::refuse_published_in(&tx, operation, expected.layer())?;
        checkpoint_in(&tx, source.object())?;
        let before = checkpoint_in(&tx, expected.checkpoint().object())?;
        let mut sources = before.stack_sources().to_vec();
        sources.push(source);
        sources.sort();
        sources.dedup();
        let checkpoint = prepare_checkpoint(&tx, before.base(), changes, &sources)?;
        let outcome = update_in(&tx, operation, expected, checkpoint, false, Some(source))?;
        #[cfg(test)]
        crate::repository::test_boundary("stack_before_commit");
        tx.commit()?;
        #[cfg(test)]
        crate::repository::test_boundary("stack_after_commit");
        Ok(outcome)
    }
}

fn replay_in(
    conn: &Connection,
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
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
    if layer != expected.layer().as_str()
        || revision != expected.revision() as i64
        || source_in(conn, operation)? != Some(source)
    {
        return Err(Error::OperationIdReused(operation.clone()));
    }
    verify_head(conn, expected)?;
    Ok(Some(attempt_outcome(conn, operation)?))
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

pub(crate) fn check_stacks(conn: &Connection) -> Result<()> {
    if conn
        .prepare(
            "SELECT 1 FROM stacks s LEFT JOIN layer_attempts a ON a.id=s.id WHERE a.id IS NULL",
        )?
        .exists([])?
    {
        return Err(Error::CorruptRepository("orphan Stack attempt".into()));
    }
    Ok(())
}

// The hash-bound binding protects exact source input even when the source was
// already present in the target ancestry (set membership alone cannot bind it).
#[derive(Serialize)]
struct StackBinding<'a> {
    operation: &'a OperationId,
    expected: &'a LayerHead,
    source: CheckpointId,
    candidate: CheckpointId,
    outcome: &'a LayerUpdate,
}

fn binding_bytes(
    operation: &OperationId,
    expected: &LayerHead,
    source: CheckpointId,
    candidate: CheckpointId,
    outcome: &LayerUpdate,
) -> Vec<u8> {
    serde_json::to_vec(&StackBinding {
        operation,
        expected,
        source,
        candidate,
        outcome,
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
) -> Result<()> {
    let binding = insert_object(
        conn,
        ObjectKind::Blob,
        &binding_bytes(operation, expected, source, candidate, outcome),
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
    if read_object(conn, decode_id(&binding)?, ObjectKind::Blob)?
        != binding_bytes(operation, expected, source, candidate, outcome)
    {
        return Err(Error::CorruptRepository(
            "Stack input binding disagrees".into(),
        ));
    }
    Ok(())
}
