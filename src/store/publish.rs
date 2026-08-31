use super::*;

impl Store {
    pub fn contribution_by_key(&self, key: &str) -> Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT id, resulting_target_ref FROM contributions WHERE idempotency_key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read idempotent Contribution")
    }

    pub fn contribution_details_by_key(&self, key: &str) -> Result<Option<ExistingContribution>> {
        self.conn
            .query_row(
                "SELECT id, resulting_target_ref, source_layer, source_checkpoint
                 FROM contributions WHERE idempotency_key = ?1",
                [key],
                |row| {
                    Ok(ExistingContribution {
                        id: row.get(0)?,
                        resulting_target_ref: row.get(1)?,
                        source_layer: row.get(2)?,
                        source_checkpoint: row.get(3)?,
                    })
                },
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read idempotent Contribution details")
    }

    pub fn accept_publish(
        &mut self,
        layer: &Layer,
        source_checkpoint: &Checkpoint,
        candidate_root: &str,
        idempotency_key: Option<&str>,
        validation_ids: &[String],
        summary: &serde_json::Value,
    ) -> Result<(String, String)> {
        if let Some(key) = idempotency_key
            && let Some(existing) = self.contribution_by_key(key)?
        {
            return Ok(existing);
        }
        let contribution_id = ulid::Ulid::new().to_string();
        let now = now();
        crate::fault::hit("before_db_transaction");
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .jctx("STORE_TX", "cannot begin Publish acceptance")?;
        let previous_target_ref = if layer.target_kind == TargetKind::World {
            tx.query_row("SELECT current_version FROM world LIMIT 1", [], |row| {
                row.get::<_, String>(0)
            })
            .jctx("STORE_QUERY", "cannot read Publish target")?
        } else {
            let target_id = layer
                .target_id
                .as_deref()
                .ok_or_else(|| JavelinError::corruption("Layer target has no ID"))?;
            tx.query_row(
                "SELECT head_checkpoint FROM layers WHERE id = ?1 AND status != 'discarded'",
                [target_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read parent Layer target")?
            .ok_or_else(|| JavelinError::stale("parent Layer is unavailable"))?
        };
        if previous_target_ref != source_checkpoint.synchronized_ref {
            return Err(JavelinError::stale(
                "Publish target advanced after final Refresh; retry Publish",
            ));
        }

        let resulting_target_ref = if layer.target_kind == TargetKind::World {
            let sequence: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(sequence), 0) + 1 FROM versions",
                    [],
                    |row| row.get(0),
                )
                .jctx("STORE_QUERY", "cannot allocate World Version")?;
            let version_id = format!("v{sequence}");
            tx.execute(
                "INSERT INTO versions(id, sequence, parent_version, root_tree, accepted_contribution, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![version_id, sequence, previous_target_ref, candidate_root, contribution_id, now],
            )
            .jctx("STORE_WRITE", "cannot append accepted World Version")?;
            crate::fault::hit("inside_transaction_before_current_pointer_update");
            let changed = tx
                .execute(
                    "UPDATE world SET current_version = ?1 WHERE current_version = ?2",
                    params![version_id, previous_target_ref],
                )
                .jctx("STORE_WRITE", "cannot advance Current World")?;
            if changed != 1 {
                return Err(JavelinError::stale("Current World changed during Publish"));
            }
            crate::fault::hit("after_current_pointer_update_before_commit");
            version_id
        } else {
            let target_id = layer.target_id.as_deref().unwrap();
            let previous: (i64, String) = tx
                .query_row(
                    "SELECT sequence, synchronized_ref FROM layer_checkpoints WHERE id = ?1",
                    [&previous_target_ref],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .jctx("STORE_QUERY", "cannot read parent Layer head")?;
            let checkpoint_id = ulid::Ulid::new().to_string();
            tx.execute(
                "INSERT INTO layer_checkpoints(id, layer_id, sequence, previous_checkpoint, root_tree,
                 synchronized_ref, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    checkpoint_id,
                    target_id,
                    previous.0 + 1,
                    previous_target_ref,
                    candidate_root,
                    previous.1,
                    format!("Publish Contribution {contribution_id}"),
                    now
                ],
            )
            .jctx("STORE_WRITE", "cannot append parent Layer Checkpoint")?;
            let changed = tx
                .execute(
                    "UPDATE layers SET head_checkpoint = ?1 WHERE id = ?2 AND head_checkpoint = ?3",
                    params![checkpoint_id, target_id, previous_target_ref],
                )
                .jctx("STORE_WRITE", "cannot advance parent Layer")?;
            if changed != 1 {
                return Err(JavelinError::stale("parent Layer changed during Publish"));
            }
            checkpoint_id
        };

        tx.execute(
            "INSERT INTO contributions(id, idempotency_key, source_layer, source_checkpoint,
             target_kind, target_id, previous_target_ref, resulting_target_ref, summary_json,
             created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                contribution_id,
                idempotency_key,
                layer.id,
                source_checkpoint.id,
                layer.target_kind.as_str(),
                layer.target_id,
                previous_target_ref,
                resulting_target_ref,
                summary.to_string(),
                now
            ],
        )
        .map_err(|error| {
            if matches!(
                error,
                rusqlite::Error::SqliteFailure(ref failure, _)
                    if failure.code == rusqlite::ErrorCode::ConstraintViolation
            ) {
                JavelinError::stale("duplicate Publish idempotency key")
            } else {
                JavelinError::corruption(format!("cannot append Contribution: {error}"))
            }
        })?;
        for validation_id in validation_ids {
            tx.execute(
                "INSERT INTO validations(id, contribution_id, validation_run_id)
                 VALUES (?1, ?2, ?3)",
                params![
                    ulid::Ulid::new().to_string(),
                    contribution_id,
                    validation_id
                ],
            )
            .jctx("STORE_WRITE", "cannot link Publish validation")?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO contribution_provenance(contribution_id, session_id)
             SELECT ?1, id FROM provenance_sessions WHERE layer_id = ?2",
            params![contribution_id, layer.id],
        )
        .jctx("STORE_WRITE", "cannot link Contribution provenance")?;
        if layer.target_kind == TargetKind::World {
            tx.execute(
                "UPDATE views SET stale = 1, updated_at = ?1 WHERE layer_id != ?2",
                params![now, layer.id],
            )
            .jctx("STORE_WRITE", "cannot mark World views stale")?;
        } else if let Some(target_id) = &layer.target_id {
            tx.execute(
                "UPDATE views SET stale = 1, updated_at = ?1 WHERE layer_id = ?2",
                params![now, target_id],
            )
            .jctx("STORE_WRITE", "cannot mark parent view stale")?;
        }
        append_event_tx(
            &tx,
            "publish.accepted",
            Some(layer.target_kind.as_str()),
            Some(&resulting_target_ref),
            &json!({
                "contribution_id": contribution_id,
                "source_layer": layer.id,
                "previous_target_ref": previous_target_ref,
                "resulting_target_ref": resulting_target_ref,
                "candidate_root": candidate_root,
            }),
        )?;
        crate::fault::hit("before_event_delivery");
        tx.commit()
            .jctx("STORE_TX", "cannot commit Publish acceptance")?;
        crate::fault::hit("after_db_commit_before_view_update");
        Ok((contribution_id, resulting_target_ref))
    }
}
