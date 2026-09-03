use crate::error::{Context, JavelinError, Result};
use crate::model::{Checkpoint, Layer, LayerStatus, TargetKind, Tree, TreeEntry, WorldVersion};
use crate::objects::{ObjectKind, ObjectStore};
use crate::process::process_alive;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod claims;
mod conflicts;
mod provenance;
mod publish;
mod records;
mod schema;
use records::*;
use schema::migrate;

pub struct Store {
    pub root: PathBuf,
    pub metadata: PathBuf,
    pub conn: Connection,
    pub objects: ObjectStore,
}

#[derive(Debug, Clone)]
pub struct ConflictRecord {
    pub id: String,
    pub layer_id: String,
    pub path: String,
    pub conflict_type: String,
    pub base_entry: Option<TreeEntry>,
    pub target_entry: Option<TreeEntry>,
    pub private_entry: Option<TreeEntry>,
    pub target_ref: String,
    pub status: String,
    pub resolution: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ValidationRecord {
    pub id: String,
    pub rule_name: String,
    pub command_json: String,
    pub required: bool,
    pub exit_code: i32,
    pub duration_ms: i64,
    pub environment_json: String,
    pub stdout_object: Option<String>,
    pub stderr_object: Option<String>,
    pub candidate_root: String,
    pub policy_hash: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ExistingContribution {
    pub id: String,
    pub resulting_target_ref: String,
    pub source_layer: Option<String>,
    pub source_checkpoint: Option<String>,
}

pub struct NewLayer<'a> {
    pub name: &'a str,
    pub origin_ref: &'a str,
    pub synchronized_ref: &'a str,
    pub root_tree: &'a str,
    pub target_kind: &'a str,
    pub target_id: Option<&'a str>,
    pub view_path: &'a Path,
}

#[derive(Debug, Clone)]
pub struct ConflictInput {
    pub path: String,
    pub conflict_type: String,
    pub base: Option<TreeEntry>,
    pub target: Option<TreeEntry>,
    pub private: Option<TreeEntry>,
}

impl Store {
    pub fn create(root: &Path) -> Result<Self> {
        let metadata = root.join(".javelin");
        for directory in [
            "objects",
            "materialized",
            "views",
            "conflicts",
            "temp",
            "locks",
            "monitor",
            "trash",
        ] {
            std::fs::create_dir_all(metadata.join(directory)).jctx(
                7,
                "STORE_IO",
                format!("cannot create .javelin/{directory}"),
            )?;
        }
        set_private_permissions(&metadata)?;
        Self::open(root)
    }

    pub fn open(root: &Path) -> Result<Self> {
        let metadata = root.join(".javelin");
        let database = metadata.join("store.sqlite3");
        let conn = Connection::open(&database).jctx(
            7,
            "STORE_OPEN",
            format!("cannot open {}", database.display()),
        )?;
        conn.pragma_update(None, "journal_mode", "WAL").jctx(
            7,
            "STORE_PRAGMA",
            "cannot enable WAL",
        )?;
        conn.pragma_update(None, "foreign_keys", "ON").jctx(
            7,
            "STORE_PRAGMA",
            "cannot enable foreign keys",
        )?;
        conn.pragma_update(None, "busy_timeout", 10_000).jctx(
            7,
            "STORE_PRAGMA",
            "cannot set busy timeout",
        )?;
        conn.pragma_update(None, "synchronous", "FULL").jctx(
            7,
            "STORE_PRAGMA",
            "cannot set durable synchronization",
        )?;
        migrate(&conn)?;
        let objects = ObjectStore::new(&metadata)?;
        let mut store = Self {
            root: root.to_path_buf(),
            metadata,
            conn,
            objects,
        };
        store.recover_startup()?;
        Ok(store)
    }

    fn recover_startup(&mut self) -> Result<()> {
        let monitor_pid = self.metadata.join("monitor/pid");
        if fs::read_to_string(&monitor_pid)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .is_some_and(process_alive)
        {
            return Ok(());
        }
        let timestamp = now();
        self.conn
            .execute(
                "UPDATE claims SET released_at = ?1
                 WHERE released_at IS NULL AND expires_at <= ?1",
                [&timestamp],
            )
            .jctx(7, "STARTUP_RECOVERY", "cannot expire Claims")?;
        self.conn
            .execute(
                "UPDATE layers SET status = 'active' WHERE status = 'publishing'",
                [],
            )
            .jctx(7, "STARTUP_RECOVERY", "cannot recover publishing Layers")?;
        self.conn
            .execute(
                "UPDATE publish_attempts SET status = 'interrupted', updated_at = ?1
                 WHERE status IN ('queued', 'running', 'publishing')",
                [&timestamp],
            )
            .jctx(7, "STARTUP_RECOVERY", "cannot recover Publish attempts")?;

        let abandoned = {
            let mut statement = self
                .conn
                .prepare("SELECT request_id, pid FROM publish_queue")
                .jctx(7, "STARTUP_RECOVERY", "cannot inspect Publish queue")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })
                .jctx(7, "STARTUP_RECOVERY", "cannot read Publish queue")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .jctx(7, "STARTUP_RECOVERY", "cannot decode Publish queue")?
                .into_iter()
                .filter_map(|(request_id, pid)| (!process_alive(pid)).then_some(request_id))
                .collect::<Vec<_>>()
        };
        for request_id in abandoned {
            self.conn
                .execute(
                    "DELETE FROM publish_queue WHERE request_id = ?1",
                    [&request_id],
                )
                .jctx(
                    7,
                    "STARTUP_RECOVERY",
                    "cannot remove abandoned Publish request",
                )?;
        }

        let views = {
            let mut statement = self
                .conn
                .prepare(
                    "SELECT l.id, l.view_path FROM layers l
                     JOIN views v ON v.layer_id = l.id
                     WHERE l.status != 'discarded' AND l.id != 'local'",
                )
                .jctx(7, "STARTUP_RECOVERY", "cannot inspect Managed views")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .jctx(7, "STARTUP_RECOVERY", "cannot read Managed views")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .jctx(7, "STARTUP_RECOVERY", "cannot decode Managed views")?
        };
        for (layer_id, view_path) in views {
            let marker = Path::new(&view_path).join(".javelin-view");
            let valid = fs::read(&marker)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| {
                    value
                        .get("layer_id")
                        .and_then(|id| id.as_str())
                        .map(str::to_owned)
                })
                .is_some_and(|id| id == layer_id);
            if !valid {
                self.conn
                    .execute(
                        "UPDATE views SET stale = 1, backend = 'repair_required', updated_at = ?1
                         WHERE layer_id = ?2",
                        params![timestamp, layer_id],
                    )
                    .jctx(7, "STARTUP_RECOVERY", "cannot mark stale Managed view")?;
            }
        }

        let grace_seconds = std::env::var("JAVELIN_STARTUP_TEMP_GRACE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(3600);
        let temp = self.metadata.join("temp");
        if temp.exists() {
            for entry in
                fs::read_dir(&temp).jctx(7, "STARTUP_RECOVERY", "cannot inspect temp data")?
            {
                let entry = entry.jctx(7, "STARTUP_RECOVERY", "cannot read temp entry")?;
                let metadata = fs::symlink_metadata(entry.path()).jctx(
                    7,
                    "STARTUP_RECOVERY",
                    "cannot inspect temp entry",
                )?;
                let old = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age >= Duration::from_secs(grace_seconds));
                if old {
                    if metadata.file_type().is_dir() {
                        fs::remove_dir_all(entry.path()).jctx(
                            7,
                            "STARTUP_RECOVERY",
                            "cannot remove abandoned temp directory",
                        )?;
                    } else {
                        fs::remove_file(entry.path()).jctx(
                            7,
                            "STARTUP_RECOVERY",
                            "cannot remove abandoned temp file",
                        )?;
                    }
                }
            }
        }

        let ready = self.metadata.join("monitor/ready");
        let monitor_alive = fs::read_to_string(&monitor_pid)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .is_some_and(process_alive);
        if !monitor_alive {
            let _ = fs::remove_file(ready);
            let _ = fs::remove_file(monitor_pid);
        }
        Ok(())
    }

    pub fn initialized(&self) -> Result<bool> {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM world)", [], |row| row.get(0))
            .jctx(7, "STORE_QUERY", "cannot inspect World")
    }

    pub fn initialize_world(&mut self, root_tree: &str) -> Result<(WorldVersion, Layer)> {
        if self.initialized()? {
            return Ok((self.current_world()?, self.layer("local")?));
        }
        let now = now();
        let project_id = ulid::Ulid::new().to_string();
        let checkpoint_id = ulid::Ulid::new().to_string();
        let tx =
            self.conn
                .transaction()
                .jctx(7, "STORE_TX", "cannot begin World initialization")?;
        tx.execute(
            "INSERT INTO world(id, current_version, created_at) VALUES (?1, 'v1', ?2)",
            params![project_id, now],
        )
        .jctx(7, "STORE_WRITE", "cannot create World")?;
        tx.execute(
            "INSERT INTO versions(id, sequence, parent_version, root_tree, accepted_contribution, created_at)
             VALUES ('v1', 1, NULL, ?1, NULL, ?2)",
            params![root_tree, now],
        )
        .jctx(7, "STORE_WRITE", "cannot create World Version v1")?;
        tx.execute(
            "INSERT INTO layers(id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
             target_id, status, view_path, created_at) VALUES
             ('local', 'local', 'v1', 'v1', ?1, 'world', NULL, 'active', ?2, ?3)",
            params![checkpoint_id, self.root.to_string_lossy(), now],
        )
        .jctx(7, "STORE_WRITE", "cannot create Local Layer")?;
        tx.execute(
            "INSERT INTO layer_checkpoints(id, layer_id, sequence, previous_checkpoint, root_tree,
             synchronized_ref, reason, created_at) VALUES (?1, 'local', 1, NULL, ?2, 'v1', 'init', ?3)",
            params![checkpoint_id, root_tree, now],
        )
        .jctx(7, "STORE_WRITE", "cannot create Local Layer Checkpoint")?;
        tx.execute(
            "INSERT INTO views(layer_id, path, materialized_ref, stale, backend, updated_at)
             VALUES ('local', ?1, ?2, 0, 'existing', ?3)",
            params![self.root.to_string_lossy(), checkpoint_id, now],
        )
        .jctx(7, "STORE_WRITE", "cannot register Local Layer view")?;
        append_event_tx(
            &tx,
            "world.initialized",
            Some("world"),
            Some("v1"),
            &json!({"root_tree": root_tree}),
        )?;
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit World initialization")?;
        Ok((self.current_world()?, self.layer("local")?))
    }

    pub fn project_id(&self) -> Result<String> {
        self.conn
            .query_row("SELECT id FROM world LIMIT 1", [], |row| row.get(0))
            .jctx(7, "STORE_QUERY", "cannot read World ID")
    }

    pub fn current_world(&self) -> Result<WorldVersion> {
        self.conn
            .query_row(
                "SELECT v.id, v.sequence, v.parent_version, v.root_tree, v.accepted_contribution, v.created_at
                 FROM world w JOIN versions v ON v.id = w.current_version LIMIT 1",
                [],
                world_from_row,
            )
            .jctx(7, "STORE_QUERY", "cannot read Current World")
    }

    pub fn world_version(&self, id: &str) -> Result<WorldVersion> {
        self.conn
            .query_row(
                "SELECT id, sequence, parent_version, root_tree, accepted_contribution, created_at
                 FROM versions WHERE id = ?1",
                [id],
                world_from_row,
            )
            .optional()
            .jctx(7, "STORE_QUERY", "cannot read World Version")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown World Version {id}")))
    }

    pub fn world_history(&self) -> Result<Vec<WorldVersion>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, sequence, parent_version, root_tree, accepted_contribution, created_at
                 FROM versions ORDER BY sequence",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare World history")?;
        let rows = statement.query_map([], world_from_row).jctx(
            7,
            "STORE_QUERY",
            "cannot read World history",
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>().jctx(
            7,
            "STORE_QUERY",
            "cannot decode World history",
        )
    }

    pub fn layer(&self, name_or_id: &str) -> Result<Layer> {
        self.conn
            .query_row(
                "SELECT id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
                 target_id, status, view_path, created_at FROM layers WHERE id = ?1 OR name = ?1
                 ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END LIMIT 1",
                [name_or_id],
                layer_from_row,
            )
            .optional()
            .jctx(7, "STORE_QUERY", "cannot read Private Layer")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown Private Layer {name_or_id}")))
    }

    pub fn layers(&self, include_discarded: bool) -> Result<Vec<Layer>> {
        let query = if include_discarded {
            "SELECT id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
             target_id, status, view_path, created_at FROM layers ORDER BY created_at, name"
        } else {
            "SELECT id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
             target_id, status, view_path, created_at FROM layers WHERE status != 'discarded'
             ORDER BY created_at, name"
        };
        let mut statement =
            self.conn
                .prepare(query)
                .jctx(7, "STORE_QUERY", "cannot prepare Layer list")?;
        let rows =
            statement
                .query_map([], layer_from_row)
                .jctx(7, "STORE_QUERY", "cannot read Layers")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx(7, "STORE_QUERY", "cannot decode Layers")
    }

    pub fn monitor_layers(&self) -> Result<Vec<Layer>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT l.id, l.name, l.origin_ref, l.synchronized_ref, l.head_checkpoint,
                 l.target_kind, l.target_id, l.status, l.view_path, l.created_at
                 FROM layers l JOIN views v ON v.layer_id = l.id
                 WHERE l.status IN ('active', 'conflicted') AND v.stale = 0 AND v.backend != 'pending'
                 ORDER BY l.created_at, l.name",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare Monitor Layer list")?;
        let rows = statement.query_map([], layer_from_row).jctx(
            7,
            "STORE_QUERY",
            "cannot read Monitor Layers",
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>().jctx(
            7,
            "STORE_QUERY",
            "cannot decode Monitor Layers",
        )
    }

    pub fn checkpoint(&self, id: &str) -> Result<Checkpoint> {
        self.conn
            .query_row(
                "SELECT id, layer_id, sequence, previous_checkpoint, root_tree, synchronized_ref,
                 reason, created_at FROM layer_checkpoints WHERE id = ?1",
                [id],
                checkpoint_from_row,
            )
            .optional()
            .jctx(7, "STORE_QUERY", "cannot read Layer Checkpoint")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown Layer Checkpoint {id}")))
    }

    pub fn layer_head(&self, layer: &Layer) -> Result<Checkpoint> {
        self.checkpoint(&layer.head_checkpoint)
    }

    pub fn checkpoint_history(&self, layer_id: &str) -> Result<Vec<Checkpoint>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, layer_id, sequence, previous_checkpoint, root_tree, synchronized_ref,
                 reason, created_at FROM layer_checkpoints WHERE layer_id = ?1 ORDER BY sequence",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare Checkpoint history")?;
        let rows = statement.query_map([layer_id], checkpoint_from_row).jctx(
            7,
            "STORE_QUERY",
            "cannot read Checkpoint history",
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>().jctx(
            7,
            "STORE_QUERY",
            "cannot decode Checkpoint history",
        )
    }

    pub fn resolve_ref(&self, reference: &str) -> Result<(String, String)> {
        if reference == "world" || reference == "current" {
            let version = self.current_world()?;
            return Ok((version.id, version.root_tree));
        }
        if reference.starts_with('v') && reference[1..].bytes().all(|byte| byte.is_ascii_digit()) {
            let version = self.world_version(reference)?;
            return Ok((version.id, version.root_tree));
        }
        if let Some(layer_name) = reference.strip_prefix("layer:") {
            let layer = self.layer(layer_name)?;
            let checkpoint = self.layer_head(&layer)?;
            return Ok((checkpoint.id, checkpoint.root_tree));
        }
        if let Ok(checkpoint) = self.checkpoint(reference) {
            return Ok((checkpoint.id, checkpoint.root_tree));
        }
        if let Ok(layer) = self.layer(reference) {
            let checkpoint = self.layer_head(&layer)?;
            return Ok((checkpoint.id, checkpoint.root_tree));
        }
        Err(JavelinError::invalid(format!(
            "unknown State Reference {reference}"
        )))
    }

    pub fn tree_for_ref(&self, reference: &str) -> Result<(String, String, Tree)> {
        let (resolved, root) = self.resolve_ref(reference)?;
        let tree = self.objects.read_tree(&root)?;
        Ok((resolved, root, tree))
    }

    pub fn create_layer(&mut self, spec: NewLayer<'_>) -> Result<Layer> {
        let NewLayer {
            name,
            origin_ref,
            synchronized_ref,
            root_tree,
            target_kind,
            target_id,
            view_path,
        } = spec;
        if name == "local" || name.trim().is_empty() || name.contains(['/', '\\', '\0']) {
            return Err(JavelinError::invalid("invalid Private Layer name"));
        }
        if self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM layers WHERE name = ?1)",
                [name],
                |row| row.get::<_, bool>(0),
            )
            .jctx(7, "STORE_QUERY", "cannot check Layer name")?
        {
            return Err(JavelinError::invalid(format!(
                "Private Layer {name} already exists"
            )));
        }
        let id = ulid::Ulid::new().to_string();
        let checkpoint = ulid::Ulid::new().to_string();
        let now = now();
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin Layer creation")?;
        tx.execute(
            "INSERT INTO layers(id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
             target_id, status, view_path, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
             'active', ?8, ?9)",
            params![id, name, origin_ref, synchronized_ref, checkpoint, target_kind, target_id, view_path.to_string_lossy(), now],
        )
        .map_err(|error| JavelinError::invalid(format!("cannot create Layer: {error}")))?;
        tx.execute(
            "INSERT INTO layer_checkpoints(id, layer_id, sequence, previous_checkpoint, root_tree,
             synchronized_ref, reason, created_at) VALUES (?1, ?2, 1, NULL, ?3, ?4, 'create', ?5)",
            params![checkpoint, id, root_tree, synchronized_ref, now],
        )
        .jctx(7, "STORE_WRITE", "cannot create initial Layer Checkpoint")?;
        tx.execute(
            "INSERT INTO views(layer_id, path, materialized_ref, stale, backend, updated_at)
             VALUES (?1, ?2, ?3, 0, 'pending', ?4)",
            params![id, view_path.to_string_lossy(), checkpoint, now],
        )
        .jctx(7, "STORE_WRITE", "cannot register Layer view")?;
        append_event_tx(
            &tx,
            "layer.created",
            Some("layer"),
            Some(&id),
            &json!({"name": name, "origin_ref": origin_ref, "target_kind": target_kind, "target_id": target_id}),
        )?;
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit Layer creation")?;
        self.layer(&id)
    }

    pub fn append_checkpoint(
        &mut self,
        layer_id: &str,
        root_tree: &str,
        synchronized_ref: &str,
        reason: &str,
    ) -> Result<Checkpoint> {
        let layer = self.layer(layer_id)?;
        let head = self.layer_head(&layer)?;
        if head.root_tree == root_tree
            && head.synchronized_ref == synchronized_ref
            && reason != "restore"
        {
            return Ok(head);
        }
        let id = ulid::Ulid::new().to_string();
        let sequence = head.sequence + 1;
        let now = now();
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin Checkpoint append")?;
        tx.execute(
            "INSERT INTO layer_checkpoints(id, layer_id, sequence, previous_checkpoint, root_tree,
             synchronized_ref, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                layer.id,
                sequence,
                head.id,
                root_tree,
                synchronized_ref,
                reason,
                now
            ],
        )
        .jctx(7, "STORE_WRITE", "cannot append Layer Checkpoint")?;
        tx.execute(
            "UPDATE layers SET head_checkpoint = ?1, synchronized_ref = ?2 WHERE id = ?3",
            params![id, synchronized_ref, layer.id],
        )
        .jctx(7, "STORE_WRITE", "cannot advance Layer head")?;
        tx.execute(
            "UPDATE views SET materialized_ref = ?1, stale = 0, updated_at = ?2 WHERE layer_id = ?3",
            params![id, now, layer.id],
        )
        .jctx(7, "STORE_WRITE", "cannot update view reference")?;
        tx.execute(
            "INSERT OR IGNORE INTO checkpoint_provenance(checkpoint_id, session_id)
             SELECT ?1, id FROM provenance_sessions WHERE layer_id = ?2 AND status = 'active'",
            params![id, layer.id],
        )
        .jctx(7, "STORE_WRITE", "cannot link Checkpoint provenance")?;
        append_event_tx(
            &tx,
            "checkpoint.created",
            Some("layer"),
            Some(&layer.id),
            &json!({"checkpoint_id": id, "root_tree": root_tree, "reason": reason}),
        )?;
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit Layer Checkpoint")?;
        self.checkpoint(&id)
    }

    pub fn append_world_version(
        &mut self,
        root_tree: &str,
        contribution_id: Option<&str>,
        event_type: &str,
        event_payload: &serde_json::Value,
    ) -> Result<WorldVersion> {
        let current = self.current_world()?;
        let sequence = current.sequence + 1;
        let id = format!("v{sequence}");
        let now = now();
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin World acceptance")?;
        tx.execute(
            "INSERT INTO versions(id, sequence, parent_version, root_tree, accepted_contribution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, sequence, current.id, root_tree, contribution_id, now],
        )
        .jctx(7, "STORE_WRITE", "cannot append World Version")?;
        let changed = tx
            .execute(
                "UPDATE world SET current_version = ?1 WHERE current_version = ?2",
                params![id, current.id],
            )
            .jctx(7, "STORE_WRITE", "cannot advance Current World")?;
        if changed != 1 {
            return Err(JavelinError::stale(
                "Current World changed during acceptance; retry Publish",
            ));
        }
        tx.execute(
            "UPDATE views SET stale = 1, updated_at = ?1 WHERE layer_id != 'local'",
            [now.clone()],
        )
        .jctx(7, "STORE_WRITE", "cannot mark views stale")?;
        append_event_tx(&tx, event_type, Some("world"), Some(&id), event_payload)?;
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit World acceptance")?;
        self.world_version(&id)
    }

    pub fn record_validation(&mut self, record: &ValidationRecord) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO validation_runs(id, rule_name, command_json, required, exit_code,
                 duration_ms, environment_json, stdout_object, stderr_object, candidate_root,
                 policy_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    record.id,
                    record.rule_name,
                    record.command_json,
                    record.required,
                    record.exit_code,
                    record.duration_ms,
                    record.environment_json,
                    record.stdout_object,
                    record.stderr_object,
                    record.candidate_root,
                    record.policy_hash,
                    record.created_at
                ],
            )
            .jctx(7, "STORE_WRITE", "cannot record validation")?;
        Ok(())
    }

    pub fn link_version_validations(
        &mut self,
        version_id: &str,
        validation_ids: &[String],
    ) -> Result<()> {
        let tx = self.conn.transaction().jctx(
            7,
            "STORE_TX",
            "cannot begin Version validation linking",
        )?;
        for validation_id in validation_ids {
            tx.execute(
                "INSERT INTO version_validations(id, version_id, validation_run_id)
                 VALUES (?1, ?2, ?3)",
                params![ulid::Ulid::new().to_string(), version_id, validation_id],
            )
            .jctx(7, "STORE_WRITE", "cannot link World Version validation")?;
        }
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit Version validation links")
    }

    pub fn append_event(
        &mut self,
        event_type: &str,
        subject_kind: Option<&str>,
        subject_id: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<i64> {
        append_event_conn(&self.conn, event_type, subject_kind, subject_id, payload)
    }

    pub fn events_since(&self, cursor: i64) -> Result<Vec<serde_json::Value>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT cursor, event_type, subject_kind, subject_id, payload_json, created_at
                 FROM events WHERE cursor > ?1 ORDER BY cursor",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare event stream")?;
        let rows = statement
            .query_map([cursor], |row| {
                let payload: String = row.get(4)?;
                Ok(json!({
                    "schema": "javelin.event.v1",
                    "cursor": row.get::<_, i64>(0)?,
                    "type": row.get::<_, String>(1)?,
                    "subject_kind": row.get::<_, Option<String>>(2)?,
                    "subject_id": row.get::<_, Option<String>>(3)?,
                    "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(json!({})),
                    "created_at": row.get::<_, String>(5)?,
                }))
            })
            .jctx(7, "STORE_QUERY", "cannot read events")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx(7, "STORE_QUERY", "cannot decode events")
    }

    pub fn mark_view(
        &mut self,
        layer_id: &str,
        state_ref: &str,
        stale: bool,
        backend: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE views SET materialized_ref = ?1, stale = ?2, backend = ?3, updated_at = ?4
                 WHERE layer_id = ?5",
                params![state_ref, stale, backend, now(), layer_id],
            )
            .jctx(7, "STORE_WRITE", "cannot update Managed view")?;
        Ok(())
    }

    pub fn view_stale(&self, layer_id: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT stale FROM views WHERE layer_id = ?1",
                [layer_id],
                |row| row.get(0),
            )
            .jctx(7, "STORE_QUERY", "cannot read Managed view state")
    }

    pub fn set_layer_status(&mut self, layer_id: &str, status: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE layers SET status = ?1 WHERE id = ?2",
                params![status, layer_id],
            )
            .jctx(7, "STORE_WRITE", "cannot change Layer status")?;
        Ok(())
    }

    pub fn active_children(&self, layer_id: &str) -> Result<Vec<Layer>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
                 target_id, status, view_path, created_at FROM layers WHERE target_kind = 'layer'
                 AND target_id = ?1 AND status != 'discarded' ORDER BY created_at",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare child Layer query")?;
        let rows = statement.query_map([layer_id], layer_from_row).jctx(
            7,
            "STORE_QUERY",
            "cannot read child Layers",
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>().jctx(
            7,
            "STORE_QUERY",
            "cannot decode child Layers",
        )
    }

    pub fn all_children(&self, layer_id: &str) -> Result<Vec<Layer>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
                 target_id, status, view_path, created_at FROM layers WHERE target_kind = 'layer'
                 AND target_id = ?1 ORDER BY created_at",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare child Layer query")?;
        let rows = statement.query_map([layer_id], layer_from_row).jctx(
            7,
            "STORE_QUERY",
            "cannot read child Layers",
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>().jctx(
            7,
            "STORE_QUERY",
            "cannot decode child Layers",
        )
    }

    pub fn reparent_layer(
        &mut self,
        layer_id: &str,
        target_kind: &str,
        target_id: Option<&str>,
    ) -> Result<()> {
        if target_kind != "world" && target_kind != "layer" {
            return Err(JavelinError::invalid(
                "reparent target must be World or Private Layer",
            ));
        }
        if target_kind == "layer" && target_id == Some(layer_id) {
            return Err(JavelinError::policy("Private Layer cannot target itself"));
        }
        let changed = self
            .conn
            .execute(
                "UPDATE layers SET target_kind = ?1, target_id = ?2 WHERE id = ?3",
                params![target_kind, target_id, layer_id],
            )
            .jctx(7, "STORE_WRITE", "cannot reparent Private Layer")?;
        if changed != 1 {
            return Err(JavelinError::invalid("unknown Private Layer"));
        }
        self.append_event(
            "layer.reparented",
            Some("layer"),
            Some(layer_id),
            &json!({"target_kind": target_kind, "target_id": target_id}),
        )?;
        Ok(())
    }

    pub fn record_discard(&mut self, layer_id: &str, purge_after: &str) -> Result<()> {
        let now = now();
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin Discard")?;
        tx.execute(
            "UPDATE layers SET status = 'discarded' WHERE id = ?1",
            [layer_id],
        )
        .jctx(7, "STORE_WRITE", "cannot Discard Layer")?;
        tx.execute(
            "INSERT INTO discard_records(layer_id, discarded_at, purge_after) VALUES (?1, ?2, ?3)
             ON CONFLICT(layer_id) DO UPDATE SET discarded_at = excluded.discarded_at,
             purge_after = excluded.purge_after",
            params![layer_id, now, purge_after],
        )
        .jctx(7, "STORE_WRITE", "cannot record Discard retention")?;
        append_event_tx(
            &tx,
            "layer.discarded",
            Some("layer"),
            Some(layer_id),
            &json!({"purge_after": purge_after}),
        )?;
        tx.commit().jctx(7, "STORE_TX", "cannot commit Discard")
    }

    pub fn recover_discard(&mut self, layer_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin Discard recovery")?;
        tx.execute(
            "UPDATE layers SET status = 'active' WHERE id = ?1 AND status = 'discarded'",
            [layer_id],
        )
        .jctx(7, "STORE_WRITE", "cannot recover Layer")?;
        tx.execute(
            "DELETE FROM discard_records WHERE layer_id = ?1",
            [layer_id],
        )
        .jctx(7, "STORE_WRITE", "cannot clear Discard record")?;
        append_event_tx(
            &tx,
            "layer.recovered",
            Some("layer"),
            Some(layer_id),
            &json!({}),
        )?;
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit Discard recovery")
    }

    pub fn purge_layer(&mut self, layer_id: &str) -> Result<()> {
        let layer = self.layer(layer_id)?;
        if layer.id == "local" || layer.status != LayerStatus::Discarded {
            return Err(JavelinError::policy(
                "only an exact discarded named Layer can be purged",
            ));
        }
        if !self.all_children(&layer.id)?.is_empty() {
            return Err(JavelinError::policy(
                "cannot purge a Layer with retained child Layers",
            ));
        }
        let tx = self
            .conn
            .transaction()
            .jctx(7, "STORE_TX", "cannot begin Layer purge")?;
        tx.execute(
            "UPDATE provenance_sessions SET layer_id = NULL WHERE layer_id = ?1",
            [&layer.id],
        )
        .jctx(7, "STORE_WRITE", "cannot detach Layer provenance sessions")?;
        tx.execute(
            "UPDATE provenance_events SET layer_id = NULL WHERE layer_id = ?1",
            [&layer.id],
        )
        .jctx(7, "STORE_WRITE", "cannot detach Layer provenance events")?;
        tx.execute("DELETE FROM layers WHERE id = ?1", [&layer.id])
            .jctx(7, "STORE_WRITE", "cannot purge Layer")?;
        tx.commit().jctx(7, "STORE_TX", "cannot commit Layer purge")
    }

    pub fn register_object(&mut self, id: &str, kind: ObjectKind, size: u64) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO object_metadata(id, kind, uncompressed_size, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, format!("{kind:?}").to_lowercase(), size as i64, now()],
            )
            .jctx(7, "STORE_WRITE", "cannot register object")?;
        Ok(())
    }

    pub fn register_objects(&mut self, objects: &[(String, ObjectKind, u64)]) -> Result<()> {
        if objects.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction().jctx(
            7,
            "STORE_TX",
            "cannot begin object metadata registration",
        )?;
        {
            let mut statement = tx
                .prepare(
                    "INSERT OR IGNORE INTO object_metadata(id, kind, uncompressed_size, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .jctx(
                    7,
                    "STORE_WRITE",
                    "cannot prepare object metadata registration",
                )?;
            for (id, kind, size) in objects {
                statement
                    .execute(params![
                        id,
                        format!("{kind:?}").to_lowercase(),
                        *size as i64,
                        now()
                    ])
                    .jctx(7, "STORE_WRITE", "cannot register object metadata")?;
            }
        }
        tx.commit()
            .jctx(7, "STORE_TX", "cannot commit object metadata registration")
    }

    pub fn expired_discarded_layers(&self, cutoff: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT layer_id FROM discard_records WHERE purge_after <= ?1 ORDER BY purge_after",
            )
            .jctx(7, "STORE_QUERY", "cannot prepare expired Discard query")?;
        let rows = statement.query_map([cutoff], |row| row.get(0)).jctx(
            7,
            "STORE_QUERY",
            "cannot read expired discarded Layers",
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>().jctx(
            7,
            "STORE_QUERY",
            "cannot decode expired discarded Layers",
        )
    }
}

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).jctx(
        7,
        "STORE_IO",
        "cannot protect Javelin metadata",
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
