use super::*;

impl Store {
    pub fn record_conflicts(
        &mut self,
        layer_id: &str,
        target_ref: &str,
        conflicts: &[ConflictInput],
    ) -> Result<Vec<String>> {
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin conflict recording")?;
        tx.execute(
            "UPDATE layers SET status = 'conflicted' WHERE id = ?1",
            [layer_id],
        )
        .jctx(7, "STORE_WRITE", "cannot mark Layer conflicted")?;
        let mut ids = Vec::new();
        for conflict in conflicts {
            let id = ulid::Ulid::new().to_string();
            tx.execute(
                "INSERT INTO conflicts(id, layer_id, path, conflict_type, base_entry, target_entry,
                 private_entry, target_ref, status, resolution, created_at) VALUES
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', NULL, ?9)",
                params![
                    id,
                    layer_id,
                    conflict.path,
                    conflict.conflict_type,
                    encode_optional(&conflict.base)?,
                    encode_optional(&conflict.target)?,
                    encode_optional(&conflict.private)?,
                    target_ref,
                    now()
                ],
            )
            .jctx(7, "STORE_WRITE", "cannot store Conflict")?;
            append_event_tx(
                &tx,
                "conflict.created",
                Some("conflict"),
                Some(&id),
                &json!({"layer_id": layer_id, "path": conflict.path, "type": conflict.conflict_type}),
            )?;
            ids.push(id);
        }
        tx.commit().jctx(7, "STORE_TX", "cannot commit Conflicts")?;
        Ok(ids)
    }

    pub fn conflicts(
        &self,
        layer: Option<&str>,
        include_resolved: bool,
    ) -> Result<Vec<ConflictRecord>> {
        let mut query = String::from(
            "SELECT id, layer_id, path, conflict_type, base_entry, target_entry, private_entry,
             target_ref, status, resolution, created_at FROM conflicts WHERE 1=1",
        );
        if layer.is_some() {
            query.push_str(" AND layer_id = ?1");
        }
        if !include_resolved {
            query.push_str(" AND status = 'open'");
        }
        query.push_str(" ORDER BY created_at, path");
        let mut statement =
            self.conn
                .prepare(&query)
                .jctx(7, "STORE_QUERY", "cannot prepare Conflict list")?;
        let map = |row: &rusqlite::Row<'_>| conflict_from_row(row);
        let records = if let Some(layer) = layer {
            statement
                .query_map([layer], map)
                .jctx(7, "STORE_QUERY", "cannot read Conflicts")?
                .collect::<rusqlite::Result<Vec<_>>>()
        } else {
            statement
                .query_map([], map)
                .jctx(7, "STORE_QUERY", "cannot read Conflicts")?
                .collect::<rusqlite::Result<Vec<_>>>()
        };
        records.jctx(7, "STORE_QUERY", "cannot decode Conflicts")
    }

    pub fn conflict(&self, id: &str) -> Result<ConflictRecord> {
        self.conn
            .query_row(
                "SELECT id, layer_id, path, conflict_type, base_entry, target_entry, private_entry,
                 target_ref, status, resolution, created_at FROM conflicts WHERE id = ?1",
                [id],
                conflict_from_row,
            )
            .optional()
            .jctx(7, "STORE_QUERY", "cannot read Conflict")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown Conflict {id}")))
    }

    pub fn resolve_conflict(&mut self, id: &str, resolution: &str) -> Result<()> {
        let conflict = self.conflict(id)?;
        if conflict.status != "open" {
            return Err(JavelinError::invalid(format!(
                "Conflict {id} is already resolved"
            )));
        }
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin Conflict resolution")?;
        tx.execute(
            "UPDATE conflicts SET status = 'resolved', resolution = ?1 WHERE id = ?2",
            params![resolution, id],
        )
        .jctx(7, "STORE_WRITE", "cannot resolve Conflict")?;
        let remaining: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM conflicts WHERE layer_id = ?1 AND status = 'open'",
                [&conflict.layer_id],
                |row| row.get(0),
            )
            .jctx(7, "STORE_QUERY", "cannot count open Conflicts")?;
        if remaining == 0 {
            tx.execute(
                "UPDATE layers SET status = 'active' WHERE id = ?1",
                [&conflict.layer_id],
            )
            .jctx(7, "STORE_WRITE", "cannot reactivate Layer")?;
        }
        append_event_tx(
            &tx,
            "conflict.resolved",
            Some("conflict"),
            Some(id),
            &json!({"resolution": resolution, "remaining": remaining}),
        )?;
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit Conflict resolution")
    }
}
