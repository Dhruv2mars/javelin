use super::*;

pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS schema_migrations(
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS world(
            id TEXT PRIMARY KEY,
            current_version TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS versions(
            id TEXT PRIMARY KEY,
            sequence INTEGER NOT NULL UNIQUE,
            parent_version TEXT REFERENCES versions(id),
            root_tree TEXT NOT NULL,
            accepted_contribution TEXT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS layers(
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            origin_ref TEXT NOT NULL,
            synchronized_ref TEXT NOT NULL,
            head_checkpoint TEXT NOT NULL,
            target_kind TEXT NOT NULL CHECK(target_kind IN ('world', 'layer')),
            target_id TEXT REFERENCES layers(id),
            status TEXT NOT NULL CHECK(status IN ('active', 'conflicted', 'publishing', 'discarded')),
            view_path TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            CHECK((target_kind = 'world' AND target_id IS NULL) OR
                  (target_kind = 'layer' AND target_id IS NOT NULL))
        );
        CREATE TABLE IF NOT EXISTS layer_checkpoints(
            id TEXT PRIMARY KEY,
            layer_id TEXT NOT NULL REFERENCES layers(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            previous_checkpoint TEXT REFERENCES layer_checkpoints(id),
            root_tree TEXT NOT NULL,
            synchronized_ref TEXT NOT NULL,
            reason TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(layer_id, sequence)
        );
        CREATE TABLE IF NOT EXISTS contributions(
            id TEXT PRIMARY KEY,
            idempotency_key TEXT UNIQUE,
            source_layer TEXT REFERENCES layers(id) ON DELETE SET NULL,
            source_checkpoint TEXT REFERENCES layer_checkpoints(id) ON DELETE SET NULL,
            target_kind TEXT NOT NULL,
            target_id TEXT,
            previous_target_ref TEXT NOT NULL,
            resulting_target_ref TEXT NOT NULL,
            summary_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS publish_attempts(
            id TEXT PRIMARY KEY,
            idempotency_key TEXT,
            layer_id TEXT NOT NULL,
            status TEXT NOT NULL,
            candidate_root TEXT,
            error_json TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS publish_queue(
            ticket INTEGER PRIMARY KEY AUTOINCREMENT,
            request_id TEXT NOT NULL UNIQUE,
            target TEXT NOT NULL,
            pid INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS publish_queue_target_ticket
            ON publish_queue(target, ticket);
        CREATE TABLE IF NOT EXISTS conflicts(
            id TEXT PRIMARY KEY,
            layer_id TEXT NOT NULL REFERENCES layers(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            conflict_type TEXT NOT NULL,
            base_entry TEXT,
            target_entry TEXT,
            private_entry TEXT,
            target_ref TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('open', 'resolved')),
            resolution TEXT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS conflict_resolutions(
            id TEXT PRIMARY KEY,
            conflict_id TEXT NOT NULL REFERENCES conflicts(id) ON DELETE CASCADE,
            checkpoint_id TEXT REFERENCES layer_checkpoints(id),
            resolution TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS validations(
            id TEXT PRIMARY KEY,
            contribution_id TEXT REFERENCES contributions(id),
            validation_run_id TEXT NOT NULL REFERENCES validation_runs(id)
        );
        CREATE TABLE IF NOT EXISTS version_validations(
            id TEXT PRIMARY KEY,
            version_id TEXT NOT NULL REFERENCES versions(id),
            validation_run_id TEXT NOT NULL REFERENCES validation_runs(id)
        );
        CREATE TABLE IF NOT EXISTS validation_runs(
            id TEXT PRIMARY KEY,
            rule_name TEXT NOT NULL,
            command_json TEXT NOT NULL,
            required INTEGER NOT NULL,
            exit_code INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL,
            environment_json TEXT NOT NULL,
            stdout_object TEXT,
            stderr_object TEXT,
            candidate_root TEXT NOT NULL,
            policy_hash TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS provenance_sessions(
            id TEXT PRIMARY KEY,
            layer_id TEXT REFERENCES layers(id),
            actor_json TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            ended_at TEXT
        );
        CREATE TABLE IF NOT EXISTS provenance_events(
            event_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES provenance_sessions(id) ON DELETE CASCADE,
            layer_id TEXT REFERENCES layers(id),
            timestamp TEXT NOT NULL,
            actor_json TEXT NOT NULL,
            event_type TEXT NOT NULL,
            payload_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS provenance_attachments(
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES provenance_sessions(id) ON DELETE CASCADE,
            object_id TEXT,
            name TEXT NOT NULL,
            media_type TEXT,
            purged INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS checkpoint_provenance(
            checkpoint_id TEXT NOT NULL REFERENCES layer_checkpoints(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL REFERENCES provenance_sessions(id),
            PRIMARY KEY(checkpoint_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS contribution_provenance(
            contribution_id TEXT NOT NULL REFERENCES contributions(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL REFERENCES provenance_sessions(id),
            PRIMARY KEY(contribution_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS claims(
            id TEXT PRIMARY KEY,
            layer_id TEXT NOT NULL REFERENCES layers(id) ON DELETE CASCADE,
            resource TEXT NOT NULL,
            kind TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            released_at TEXT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS events(
            cursor INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type TEXT NOT NULL,
            subject_kind TEXT,
            subject_id TEXT,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS views(
            layer_id TEXT PRIMARY KEY REFERENCES layers(id) ON DELETE CASCADE,
            path TEXT NOT NULL UNIQUE,
            materialized_ref TEXT NOT NULL,
            stale INTEGER NOT NULL,
            backend TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS discard_records(
            layer_id TEXT PRIMARY KEY REFERENCES layers(id) ON DELETE CASCADE,
            discarded_at TEXT NOT NULL,
            purge_after TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS object_metadata(
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            uncompressed_size INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );
        INSERT OR IGNORE INTO schema_migrations(version, applied_at)
        VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
        COMMIT;
        "#,
    )
    .jctx(7, "MIGRATION_FAILED", "cannot migrate Javelin Store")?;
    let has_environment = {
        let mut statement = conn.prepare("PRAGMA table_info(validation_runs)").jctx(
            7,
            "MIGRATION_FAILED",
            "cannot inspect validation schema",
        )?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .jctx(7, "MIGRATION_FAILED", "cannot read validation schema")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .jctx(7, "MIGRATION_FAILED", "cannot decode validation schema")?;
        columns.iter().any(|column| column == "environment_json")
    };
    if !has_environment {
        conn.execute_batch(
            r#"
            BEGIN IMMEDIATE;
            ALTER TABLE validation_runs ADD COLUMN environment_json TEXT NOT NULL DEFAULT '{}';
            INSERT OR IGNORE INTO schema_migrations(version, applied_at)
            VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
            COMMIT;
            "#,
        )
        .jctx(
            7,
            "MIGRATION_FAILED",
            "cannot migrate validation environment schema",
        )?;
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
             VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )
        .jctx(7, "MIGRATION_FAILED", "cannot record schema migration 2")?;
    }
    migrate_nullable_contribution_sources(conn)?;
    Ok(())
}

fn migrate_nullable_contribution_sources(conn: &Connection) -> Result<()> {
    let source_layer_required = {
        let mut statement = conn.prepare("PRAGMA table_info(contributions)").jctx(
            7,
            "MIGRATION_FAILED",
            "cannot inspect Contribution schema",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?))
            })
            .jctx(7, "MIGRATION_FAILED", "cannot read Contribution schema")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .jctx(7, "MIGRATION_FAILED", "cannot decode Contribution schema")?
            .into_iter()
            .find(|(name, _)| name == "source_layer")
            .is_some_and(|(_, not_null)| not_null != 0)
    };
    if !source_layer_required {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
             VALUES (3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )
        .jctx(7, "MIGRATION_FAILED", "cannot record schema migration 3")?;
        return Ok(());
    }

    conn.pragma_update(None, "foreign_keys", "OFF").jctx(
        7,
        "MIGRATION_FAILED",
        "cannot pause foreign keys for migration",
    )?;
    let migrated = conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        CREATE TABLE contributions_new(
            id TEXT PRIMARY KEY,
            idempotency_key TEXT UNIQUE,
            source_layer TEXT REFERENCES layers(id) ON DELETE SET NULL,
            source_checkpoint TEXT REFERENCES layer_checkpoints(id) ON DELETE SET NULL,
            target_kind TEXT NOT NULL,
            target_id TEXT,
            previous_target_ref TEXT NOT NULL,
            resulting_target_ref TEXT NOT NULL,
            summary_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        INSERT INTO contributions_new(
            id, idempotency_key, source_layer, source_checkpoint, target_kind, target_id,
            previous_target_ref, resulting_target_ref, summary_json, created_at
        )
        SELECT id, idempotency_key, source_layer, source_checkpoint, target_kind, target_id,
               previous_target_ref, resulting_target_ref, summary_json, created_at
        FROM contributions;
        DROP TABLE contributions;
        ALTER TABLE contributions_new RENAME TO contributions;
        INSERT OR IGNORE INTO schema_migrations(version, applied_at)
        VALUES (3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
        COMMIT;
        "#,
    );
    conn.pragma_update(None, "foreign_keys", "ON").jctx(
        7,
        "MIGRATION_FAILED",
        "cannot restore foreign keys after migration",
    )?;
    migrated.jctx(
        7,
        "MIGRATION_FAILED",
        "cannot make Contribution source references purge-safe",
    )?;
    let violation: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .optional()
        .jctx(7, "MIGRATION_FAILED", "cannot verify migrated foreign keys")?;
    if let Some(table) = violation {
        return Err(JavelinError::corruption(format!(
            "foreign-key violation after schema migration in {table}"
        )));
    }
    Ok(())
}
