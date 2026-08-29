use crate::error::{Context, JavelinError, Result};
use crate::model::{Checkpoint, Layer, Tree, TreeEntry, WorldVersion};
use crate::objects::{ObjectKind, ObjectStore};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

pub struct NewLayer<'a> {
    pub name: &'a str,
    pub origin_ref: &'a str,
    pub synchronized_ref: &'a str,
    pub root_tree: &'a str,
    pub target_kind: &'a str,
    pub target_id: Option<&'a str>,
    pub view_path: &'a Path,
}

pub type ConflictInput = (
    String,
    String,
    Option<TreeEntry>,
    Option<TreeEntry>,
    Option<TreeEntry>,
);

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
            std::fs::create_dir_all(metadata.join(directory))
                .jctx("STORE_IO", format!("cannot create .javelin/{directory}"))?;
        }
        set_private_permissions(&metadata)?;
        Self::open(root)
    }

    pub fn open(root: &Path) -> Result<Self> {
        let metadata = root.join(".javelin");
        let database = metadata.join("store.sqlite3");
        let conn = Connection::open(&database)
            .jctx("STORE_OPEN", format!("cannot open {}", database.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .jctx("STORE_PRAGMA", "cannot enable WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .jctx("STORE_PRAGMA", "cannot enable foreign keys")?;
        conn.pragma_update(None, "busy_timeout", 10_000)
            .jctx("STORE_PRAGMA", "cannot set busy timeout")?;
        conn.pragma_update(None, "synchronous", "FULL")
            .jctx("STORE_PRAGMA", "cannot set durable synchronization")?;
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
        let timestamp = now();
        self.conn
            .execute(
                "UPDATE claims SET released_at = ?1
                 WHERE released_at IS NULL AND expires_at <= ?1",
                [&timestamp],
            )
            .jctx("STARTUP_RECOVERY", "cannot expire Claims")?;
        self.conn
            .execute(
                "UPDATE layers SET status = 'active' WHERE status = 'publishing'",
                [],
            )
            .jctx("STARTUP_RECOVERY", "cannot recover publishing Layers")?;
        self.conn
            .execute(
                "UPDATE publish_attempts SET status = 'interrupted', updated_at = ?1
                 WHERE status IN ('queued', 'running', 'publishing')",
                [&timestamp],
            )
            .jctx("STARTUP_RECOVERY", "cannot recover Publish attempts")?;

        let abandoned = {
            let mut statement = self
                .conn
                .prepare("SELECT request_id, pid FROM publish_queue")
                .jctx("STARTUP_RECOVERY", "cannot inspect Publish queue")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })
                .jctx("STARTUP_RECOVERY", "cannot read Publish queue")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .jctx("STARTUP_RECOVERY", "cannot decode Publish queue")?
                .into_iter()
                .filter_map(|(request_id, pid)| (!startup_process_alive(pid)).then_some(request_id))
                .collect::<Vec<_>>()
        };
        for request_id in abandoned {
            self.conn
                .execute(
                    "DELETE FROM publish_queue WHERE request_id = ?1",
                    [&request_id],
                )
                .jctx(
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
                .jctx("STARTUP_RECOVERY", "cannot inspect Managed views")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .jctx("STARTUP_RECOVERY", "cannot read Managed views")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .jctx("STARTUP_RECOVERY", "cannot decode Managed views")?
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
                    .jctx("STARTUP_RECOVERY", "cannot mark stale Managed view")?;
            }
        }

        let grace_seconds = std::env::var("JAVELIN_STARTUP_TEMP_GRACE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(3600);
        let temp = self.metadata.join("temp");
        if temp.exists() {
            for entry in fs::read_dir(&temp).jctx("STARTUP_RECOVERY", "cannot inspect temp data")? {
                let entry = entry.jctx("STARTUP_RECOVERY", "cannot read temp entry")?;
                let metadata = fs::symlink_metadata(entry.path())
                    .jctx("STARTUP_RECOVERY", "cannot inspect temp entry")?;
                let old = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age >= Duration::from_secs(grace_seconds));
                if old {
                    if metadata.file_type().is_dir() {
                        fs::remove_dir_all(entry.path())
                            .jctx("STARTUP_RECOVERY", "cannot remove abandoned temp directory")?;
                    } else {
                        fs::remove_file(entry.path())
                            .jctx("STARTUP_RECOVERY", "cannot remove abandoned temp file")?;
                    }
                }
            }
        }

        let ready = self.metadata.join("monitor/ready");
        let pid_file = self.metadata.join("monitor/pid");
        let monitor_alive = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .is_some_and(startup_process_alive);
        if !monitor_alive {
            let _ = fs::remove_file(ready);
            let _ = fs::remove_file(pid_file);
        }
        Ok(())
    }

    pub fn initialized(&self) -> Result<bool> {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM world)", [], |row| row.get(0))
            .jctx("STORE_QUERY", "cannot inspect World")
    }

    pub fn initialize_world(&mut self, root_tree: &str) -> Result<(WorldVersion, Layer)> {
        if self.initialized()? {
            return Ok((self.current_world()?, self.layer("local")?));
        }
        let now = now();
        let project_id = ulid::Ulid::new().to_string();
        let checkpoint_id = ulid::Ulid::new().to_string();
        let tx = self
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin World initialization")?;
        tx.execute(
            "INSERT INTO world(id, current_version, created_at) VALUES (?1, 'v1', ?2)",
            params![project_id, now],
        )
        .jctx("STORE_WRITE", "cannot create World")?;
        tx.execute(
            "INSERT INTO versions(id, sequence, parent_version, root_tree, accepted_contribution, created_at)
             VALUES ('v1', 1, NULL, ?1, NULL, ?2)",
            params![root_tree, now],
        )
        .jctx("STORE_WRITE", "cannot create World Version v1")?;
        tx.execute(
            "INSERT INTO layers(id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
             target_id, status, view_path, created_at) VALUES
             ('local', 'local', 'v1', 'v1', ?1, 'world', NULL, 'active', ?2, ?3)",
            params![checkpoint_id, self.root.to_string_lossy(), now],
        )
        .jctx("STORE_WRITE", "cannot create Local Layer")?;
        tx.execute(
            "INSERT INTO layer_checkpoints(id, layer_id, sequence, previous_checkpoint, root_tree,
             synchronized_ref, reason, created_at) VALUES (?1, 'local', 1, NULL, ?2, 'v1', 'init', ?3)",
            params![checkpoint_id, root_tree, now],
        )
        .jctx("STORE_WRITE", "cannot create Local Layer Checkpoint")?;
        tx.execute(
            "INSERT INTO views(layer_id, path, materialized_ref, stale, backend, updated_at)
             VALUES ('local', ?1, ?2, 0, 'existing', ?3)",
            params![self.root.to_string_lossy(), checkpoint_id, now],
        )
        .jctx("STORE_WRITE", "cannot register Local Layer view")?;
        append_event_tx(
            &tx,
            "world.initialized",
            Some("world"),
            Some("v1"),
            &json!({"root_tree": root_tree}),
        )?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit World initialization")?;
        Ok((self.current_world()?, self.layer("local")?))
    }

    pub fn project_id(&self) -> Result<String> {
        self.conn
            .query_row("SELECT id FROM world LIMIT 1", [], |row| row.get(0))
            .jctx("STORE_QUERY", "cannot read World ID")
    }

    pub fn current_world(&self) -> Result<WorldVersion> {
        self.conn
            .query_row(
                "SELECT v.id, v.sequence, v.parent_version, v.root_tree, v.accepted_contribution, v.created_at
                 FROM world w JOIN versions v ON v.id = w.current_version LIMIT 1",
                [],
                world_from_row,
            )
            .jctx("STORE_QUERY", "cannot read Current World")
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
            .jctx("STORE_QUERY", "cannot read World Version")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown World Version {id}")))
    }

    pub fn world_history(&self) -> Result<Vec<WorldVersion>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, sequence, parent_version, root_tree, accepted_contribution, created_at
                 FROM versions ORDER BY sequence",
            )
            .jctx("STORE_QUERY", "cannot prepare World history")?;
        let rows = statement
            .query_map([], world_from_row)
            .jctx("STORE_QUERY", "cannot read World history")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode World history")
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
            .jctx("STORE_QUERY", "cannot read Private Layer")?
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
        let mut statement = self
            .conn
            .prepare(query)
            .jctx("STORE_QUERY", "cannot prepare Layer list")?;
        let rows = statement
            .query_map([], layer_from_row)
            .jctx("STORE_QUERY", "cannot read Layers")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode Layers")
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
            .jctx("STORE_QUERY", "cannot prepare Monitor Layer list")?;
        let rows = statement
            .query_map([], layer_from_row)
            .jctx("STORE_QUERY", "cannot read Monitor Layers")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode Monitor Layers")
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
            .jctx("STORE_QUERY", "cannot read Layer Checkpoint")?
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
            .jctx("STORE_QUERY", "cannot prepare Checkpoint history")?;
        let rows = statement
            .query_map([layer_id], checkpoint_from_row)
            .jctx("STORE_QUERY", "cannot read Checkpoint history")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode Checkpoint history")
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
            .jctx("STORE_QUERY", "cannot check Layer name")?
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
            .jctx("STORE_TX", "cannot begin Layer creation")?;
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
        .jctx("STORE_WRITE", "cannot create initial Layer Checkpoint")?;
        tx.execute(
            "INSERT INTO views(layer_id, path, materialized_ref, stale, backend, updated_at)
             VALUES (?1, ?2, ?3, 0, 'pending', ?4)",
            params![id, view_path.to_string_lossy(), checkpoint, now],
        )
        .jctx("STORE_WRITE", "cannot register Layer view")?;
        append_event_tx(
            &tx,
            "layer.created",
            Some("layer"),
            Some(&id),
            &json!({"name": name, "origin_ref": origin_ref, "target_kind": target_kind, "target_id": target_id}),
        )?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit Layer creation")?;
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
            .jctx("STORE_TX", "cannot begin Checkpoint append")?;
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
        .jctx("STORE_WRITE", "cannot append Layer Checkpoint")?;
        tx.execute(
            "UPDATE layers SET head_checkpoint = ?1, synchronized_ref = ?2 WHERE id = ?3",
            params![id, synchronized_ref, layer.id],
        )
        .jctx("STORE_WRITE", "cannot advance Layer head")?;
        tx.execute(
            "UPDATE views SET materialized_ref = ?1, stale = 0, updated_at = ?2 WHERE layer_id = ?3",
            params![id, now, layer.id],
        )
        .jctx("STORE_WRITE", "cannot update view reference")?;
        tx.execute(
            "INSERT OR IGNORE INTO checkpoint_provenance(checkpoint_id, session_id)
             SELECT ?1, id FROM provenance_sessions WHERE layer_id = ?2 AND status = 'active'",
            params![id, layer.id],
        )
        .jctx("STORE_WRITE", "cannot link Checkpoint provenance")?;
        append_event_tx(
            &tx,
            "checkpoint.created",
            Some("layer"),
            Some(&layer.id),
            &json!({"checkpoint_id": id, "root_tree": root_tree, "reason": reason}),
        )?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit Layer Checkpoint")?;
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
            .jctx("STORE_TX", "cannot begin World acceptance")?;
        tx.execute(
            "INSERT INTO versions(id, sequence, parent_version, root_tree, accepted_contribution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, sequence, current.id, root_tree, contribution_id, now],
        )
        .jctx("STORE_WRITE", "cannot append World Version")?;
        let changed = tx
            .execute(
                "UPDATE world SET current_version = ?1 WHERE current_version = ?2",
                params![id, current.id],
            )
            .jctx("STORE_WRITE", "cannot advance Current World")?;
        if changed != 1 {
            return Err(JavelinError::stale(
                "Current World changed during acceptance; retry Publish",
            ));
        }
        tx.execute(
            "UPDATE views SET stale = 1, updated_at = ?1 WHERE layer_id != 'local'",
            [now.clone()],
        )
        .jctx("STORE_WRITE", "cannot mark views stale")?;
        append_event_tx(&tx, event_type, Some("world"), Some(&id), event_payload)?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit World acceptance")?;
        self.world_version(&id)
    }

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

    pub fn contribution_details_by_key(
        &self,
        key: &str,
    ) -> Result<Option<(String, String, String, String)>> {
        self.conn
            .query_row(
                "SELECT id, resulting_target_ref, source_layer, source_checkpoint
                 FROM contributions WHERE idempotency_key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
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
            .transaction()
            .jctx("STORE_TX", "cannot begin Publish acceptance")?;
        let previous_target_ref = if layer.target_kind == "world" {
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

        let resulting_target_ref = if layer.target_kind == "world" {
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
                layer.target_kind,
                layer.target_id,
                previous_target_ref,
                resulting_target_ref,
                summary.to_string(),
                now
            ],
        )
        .map_err(|error| {
            if error.to_string().contains("idempotency_key") {
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
        if layer.target_kind == "world" {
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
            Some(&layer.target_kind),
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

    #[allow(clippy::too_many_arguments)]
    pub fn insert_contribution(
        &mut self,
        id: &str,
        key: Option<&str>,
        layer: &Layer,
        source_checkpoint: &str,
        previous_target_ref: &str,
        resulting_target_ref: &str,
        summary: &serde_json::Value,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO contributions(id, idempotency_key, source_layer, source_checkpoint,
                 target_kind, target_id, previous_target_ref, resulting_target_ref, summary_json,
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
                    key,
                    layer.id,
                    source_checkpoint,
                    layer.target_kind,
                    layer.target_id,
                    previous_target_ref,
                    resulting_target_ref,
                    summary.to_string(),
                    now()
                ],
            )
            .map_err(|error| {
                if error.to_string().contains("idempotency_key") {
                    JavelinError::stale("duplicate Publish idempotency key")
                } else {
                    JavelinError::corruption(format!("cannot append Contribution: {error}"))
                }
            })?;
        Ok(())
    }

    pub fn append_parent_checkpoint(
        &mut self,
        parent_id: &str,
        root_tree: &str,
        contribution_id: &str,
    ) -> Result<Checkpoint> {
        let parent = self.layer(parent_id)?;
        let synchronized_ref = parent.synchronized_ref.clone();
        let checkpoint = self.append_checkpoint(
            &parent.id,
            root_tree,
            &synchronized_ref,
            &format!("Publish Contribution {contribution_id}"),
        )?;
        self.append_event(
            "publish.accepted",
            Some("layer"),
            Some(&parent.id),
            &json!({"contribution_id": contribution_id, "checkpoint_id": checkpoint.id}),
        )?;
        Ok(checkpoint)
    }

    pub fn record_conflicts(
        &mut self,
        layer_id: &str,
        target_ref: &str,
        conflicts: &[ConflictInput],
    ) -> Result<Vec<String>> {
        let tx = self
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin conflict recording")?;
        tx.execute(
            "UPDATE layers SET status = 'conflicted' WHERE id = ?1",
            [layer_id],
        )
        .jctx("STORE_WRITE", "cannot mark Layer conflicted")?;
        let mut ids = Vec::new();
        for (path, conflict_type, base, target, private) in conflicts {
            let id = ulid::Ulid::new().to_string();
            tx.execute(
                "INSERT INTO conflicts(id, layer_id, path, conflict_type, base_entry, target_entry,
                 private_entry, target_ref, status, resolution, created_at) VALUES
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', NULL, ?9)",
                params![
                    id,
                    layer_id,
                    path,
                    conflict_type,
                    encode_optional(base)?,
                    encode_optional(target)?,
                    encode_optional(private)?,
                    target_ref,
                    now()
                ],
            )
            .jctx("STORE_WRITE", "cannot store Conflict")?;
            append_event_tx(
                &tx,
                "conflict.created",
                Some("conflict"),
                Some(&id),
                &json!({"layer_id": layer_id, "path": path, "type": conflict_type}),
            )?;
            ids.push(id);
        }
        tx.commit().jctx("STORE_TX", "cannot commit Conflicts")?;
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
        let mut statement = self
            .conn
            .prepare(&query)
            .jctx("STORE_QUERY", "cannot prepare Conflict list")?;
        let map = |row: &rusqlite::Row<'_>| conflict_from_row(row);
        let records = if let Some(layer) = layer {
            statement
                .query_map([layer], map)
                .jctx("STORE_QUERY", "cannot read Conflicts")?
                .collect::<rusqlite::Result<Vec<_>>>()
        } else {
            statement
                .query_map([], map)
                .jctx("STORE_QUERY", "cannot read Conflicts")?
                .collect::<rusqlite::Result<Vec<_>>>()
        };
        records.jctx("STORE_QUERY", "cannot decode Conflicts")
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
            .jctx("STORE_QUERY", "cannot read Conflict")?
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
            .jctx("STORE_TX", "cannot begin Conflict resolution")?;
        tx.execute(
            "UPDATE conflicts SET status = 'resolved', resolution = ?1 WHERE id = ?2",
            params![resolution, id],
        )
        .jctx("STORE_WRITE", "cannot resolve Conflict")?;
        let remaining: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM conflicts WHERE layer_id = ?1 AND status = 'open'",
                [&conflict.layer_id],
                |row| row.get(0),
            )
            .jctx("STORE_QUERY", "cannot count open Conflicts")?;
        if remaining == 0 {
            tx.execute(
                "UPDATE layers SET status = 'active' WHERE id = ?1",
                [&conflict.layer_id],
            )
            .jctx("STORE_WRITE", "cannot reactivate Layer")?;
        }
        append_event_tx(
            &tx,
            "conflict.resolved",
            Some("conflict"),
            Some(id),
            &json!({"resolution": resolution, "remaining": remaining}),
        )?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit Conflict resolution")
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
            .jctx("STORE_WRITE", "cannot record validation")?;
        Ok(())
    }

    pub fn link_version_validations(
        &mut self,
        version_id: &str,
        validation_ids: &[String],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin Version validation linking")?;
        for validation_id in validation_ids {
            tx.execute(
                "INSERT INTO version_validations(id, version_id, validation_run_id)
                 VALUES (?1, ?2, ?3)",
                params![ulid::Ulid::new().to_string(), version_id, validation_id],
            )
            .jctx("STORE_WRITE", "cannot link World Version validation")?;
        }
        tx.commit()
            .jctx("STORE_TX", "cannot commit Version validation links")
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
            .jctx("STORE_QUERY", "cannot prepare event stream")?;
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
            .jctx("STORE_QUERY", "cannot read events")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode events")
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
            .jctx("STORE_WRITE", "cannot update Managed view")?;
        Ok(())
    }

    pub fn view_stale(&self, layer_id: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT stale FROM views WHERE layer_id = ?1",
                [layer_id],
                |row| row.get(0),
            )
            .jctx("STORE_QUERY", "cannot read Managed view state")
    }

    pub fn set_layer_status(&mut self, layer_id: &str, status: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE layers SET status = ?1 WHERE id = ?2",
                params![status, layer_id],
            )
            .jctx("STORE_WRITE", "cannot change Layer status")?;
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
            .jctx("STORE_QUERY", "cannot prepare child Layer query")?;
        let rows = statement
            .query_map([layer_id], layer_from_row)
            .jctx("STORE_QUERY", "cannot read child Layers")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode child Layers")
    }

    pub fn all_children(&self, layer_id: &str) -> Result<Vec<Layer>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, name, origin_ref, synchronized_ref, head_checkpoint, target_kind,
                 target_id, status, view_path, created_at FROM layers WHERE target_kind = 'layer'
                 AND target_id = ?1 ORDER BY created_at",
            )
            .jctx("STORE_QUERY", "cannot prepare child Layer query")?;
        let rows = statement
            .query_map([layer_id], layer_from_row)
            .jctx("STORE_QUERY", "cannot read child Layers")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode child Layers")
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
            .jctx("STORE_WRITE", "cannot reparent Private Layer")?;
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
            .jctx("STORE_TX", "cannot begin Discard")?;
        tx.execute(
            "UPDATE layers SET status = 'discarded' WHERE id = ?1",
            [layer_id],
        )
        .jctx("STORE_WRITE", "cannot Discard Layer")?;
        tx.execute(
            "INSERT INTO discard_records(layer_id, discarded_at, purge_after) VALUES (?1, ?2, ?3)
             ON CONFLICT(layer_id) DO UPDATE SET discarded_at = excluded.discarded_at,
             purge_after = excluded.purge_after",
            params![layer_id, now, purge_after],
        )
        .jctx("STORE_WRITE", "cannot record Discard retention")?;
        append_event_tx(
            &tx,
            "layer.discarded",
            Some("layer"),
            Some(layer_id),
            &json!({"purge_after": purge_after}),
        )?;
        tx.commit().jctx("STORE_TX", "cannot commit Discard")
    }

    pub fn recover_discard(&mut self, layer_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin Discard recovery")?;
        tx.execute(
            "UPDATE layers SET status = 'active' WHERE id = ?1 AND status = 'discarded'",
            [layer_id],
        )
        .jctx("STORE_WRITE", "cannot recover Layer")?;
        tx.execute(
            "DELETE FROM discard_records WHERE layer_id = ?1",
            [layer_id],
        )
        .jctx("STORE_WRITE", "cannot clear Discard record")?;
        append_event_tx(
            &tx,
            "layer.recovered",
            Some("layer"),
            Some(layer_id),
            &json!({}),
        )?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit Discard recovery")
    }

    pub fn purge_layer(&mut self, layer_id: &str) -> Result<()> {
        let layer = self.layer(layer_id)?;
        if layer.id == "local" || layer.status != "discarded" {
            return Err(JavelinError::policy(
                "only an exact discarded named Layer can be purged",
            ));
        }
        if !self.all_children(&layer.id)?.is_empty() {
            return Err(JavelinError::policy(
                "cannot purge a Layer with retained child Layers",
            ));
        }
        self.conn
            .execute("DELETE FROM layers WHERE id = ?1", [&layer.id])
            .jctx("STORE_WRITE", "cannot purge Layer")?;
        Ok(())
    }

    pub fn register_object(&mut self, id: &str, kind: ObjectKind, size: u64) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO object_metadata(id, kind, uncompressed_size, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, format!("{kind:?}").to_lowercase(), size as i64, now()],
            )
            .jctx("STORE_WRITE", "cannot register object")?;
        Ok(())
    }

    pub fn begin_provenance(
        &mut self,
        layer_id: Option<&str>,
        actor: &serde_json::Value,
    ) -> Result<String> {
        if let Some(layer_id) = layer_id {
            let _ = self.layer(layer_id)?;
        }
        let id = ulid::Ulid::new().to_string();
        self.conn
            .execute(
                "INSERT INTO provenance_sessions(id, layer_id, actor_json, status, started_at)
                 VALUES (?1, ?2, ?3, 'active', ?4)",
                params![id, layer_id, actor.to_string(), now()],
            )
            .jctx("STORE_WRITE", "cannot begin provenance session")?;
        self.append_event(
            "provenance.started",
            Some("provenance"),
            Some(&id),
            &json!({"layer_id": layer_id, "actor": actor}),
        )?;
        Ok(id)
    }

    pub fn append_provenance_event(
        &mut self,
        session_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<String> {
        let (layer_id, actor_json, status): (Option<String>, String, String) = self
            .conn
            .query_row(
                "SELECT layer_id, actor_json, status FROM provenance_sessions WHERE id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read provenance session")?
            .ok_or_else(|| {
                JavelinError::invalid(format!("unknown provenance session {session_id}"))
            })?;
        if status != "active" {
            return Err(JavelinError::policy("provenance session has ended"));
        }
        let event_id = ulid::Ulid::new().to_string();
        self.conn
            .execute(
                "INSERT INTO provenance_events(event_id, session_id, layer_id, timestamp, actor_json,
                 event_type, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![event_id, session_id, layer_id, now(), actor_json, event_type, payload.to_string()],
            )
            .jctx("STORE_WRITE", "cannot append provenance event")?;
        Ok(event_id)
    }

    pub fn attach_provenance(
        &mut self,
        session_id: &str,
        name: &str,
        media_type: Option<&str>,
        object_id: &str,
    ) -> Result<String> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provenance_sessions WHERE id = ?1)",
                [session_id],
                |row| row.get(0),
            )
            .jctx("STORE_QUERY", "cannot inspect provenance session")?;
        if !exists {
            return Err(JavelinError::invalid(format!(
                "unknown provenance session {session_id}"
            )));
        }
        let id = ulid::Ulid::new().to_string();
        self.conn
            .execute(
                "INSERT INTO provenance_attachments(id, session_id, object_id, name, media_type,
                 purged, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                params![id, session_id, object_id, name, media_type, now()],
            )
            .jctx("STORE_WRITE", "cannot attach provenance payload")?;
        Ok(id)
    }

    pub fn end_provenance(&mut self, session_id: &str) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE provenance_sessions SET status = 'ended', ended_at = ?1
                 WHERE id = ?2 AND status = 'active'",
                params![now(), session_id],
            )
            .jctx("STORE_WRITE", "cannot end provenance session")?;
        if changed != 1 {
            return Err(JavelinError::invalid(
                "provenance session is unknown or ended",
            ));
        }
        self.append_event(
            "provenance.ended",
            Some("provenance"),
            Some(session_id),
            &json!({}),
        )?;
        Ok(())
    }

    pub fn provenance_session(&self, session_id: &str) -> Result<serde_json::Value> {
        let session = self
            .conn
            .query_row(
                "SELECT id, layer_id, actor_json, status, started_at, ended_at
                 FROM provenance_sessions WHERE id = ?1",
                [session_id],
                |row| {
                    let actor: String = row.get(2)?;
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "layer_id": row.get::<_, Option<String>>(1)?,
                        "actor": serde_json::from_str::<serde_json::Value>(&actor).unwrap_or(json!({})),
                        "status": row.get::<_, String>(3)?,
                        "started_at": row.get::<_, String>(4)?,
                        "ended_at": row.get::<_, Option<String>>(5)?,
                    }))
                },
            )
            .optional()
            .jctx("STORE_QUERY", "cannot read provenance session")?
            .ok_or_else(|| JavelinError::invalid(format!("unknown provenance session {session_id}")))?;
        let mut statement = self
            .conn
            .prepare(
                "SELECT event_id, layer_id, timestamp, actor_json, event_type, payload_json FROM provenance_events
                 WHERE session_id = ?1 ORDER BY timestamp, event_id",
            )
            .jctx("STORE_QUERY", "cannot prepare provenance event query")?;
        let events = statement
            .query_map([session_id], |row| {
                let actor: String = row.get(3)?;
                let payload: String = row.get(5)?;
                Ok(json!({
                    "schema": "javelin.provenance.v1",
                    "event_id": row.get::<_, String>(0)?,
                    "session_id": session_id,
                    "layer_id": row.get::<_, Option<String>>(1)?,
                    "timestamp": row.get::<_, String>(2)?,
                    "actor": serde_json::from_str::<serde_json::Value>(&actor).unwrap_or(json!({})),
                    "type": row.get::<_, String>(4)?,
                    "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(json!({})),
                }))
            })
            .jctx("STORE_QUERY", "cannot read provenance events")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode provenance events")?;
        Ok(json!({"session": session, "events": events}))
    }

    pub fn search_provenance(&self, query: &str) -> Result<Vec<serde_json::Value>> {
        let pattern = format!("%{query}%");
        let mut statement = self
            .conn
            .prepare(
                "SELECT DISTINCT s.id, s.layer_id, s.actor_json, s.status, s.started_at
                 FROM provenance_sessions s LEFT JOIN provenance_events e ON e.session_id = s.id
                 WHERE s.id LIKE ?1 OR s.actor_json LIKE ?1 OR e.payload_json LIKE ?1 OR e.event_type LIKE ?1
                 ORDER BY s.started_at",
            )
            .jctx("STORE_QUERY", "cannot prepare provenance search")?;
        let rows = statement
            .query_map([pattern], |row| {
                let actor: String = row.get(2)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "layer_id": row.get::<_, Option<String>>(1)?,
                    "actor": serde_json::from_str::<serde_json::Value>(&actor).unwrap_or(json!({})),
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                }))
            })
            .jctx("STORE_QUERY", "cannot search provenance")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode provenance search")
    }

    pub fn purge_provenance(&mut self, session_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .jctx("STORE_TX", "cannot begin provenance purge")?;
        tx.execute(
            "UPDATE provenance_events SET payload_json = '{\"purged\":true}' WHERE session_id = ?1",
            [session_id],
        )
        .jctx("STORE_WRITE", "cannot purge provenance events")?;
        tx.execute(
            "UPDATE provenance_attachments SET object_id = NULL, purged = 1 WHERE session_id = ?1",
            [session_id],
        )
        .jctx("STORE_WRITE", "cannot purge provenance attachments")?;
        tx.commit()
            .jctx("STORE_TX", "cannot commit provenance purge")?;
        self.append_event(
            "provenance.purged",
            Some("provenance"),
            Some(session_id),
            &json!({}),
        )?;
        Ok(())
    }

    pub fn expired_provenance_sessions(&self, cutoff: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id FROM provenance_sessions WHERE started_at <= ?1 AND
                 (EXISTS(SELECT 1 FROM provenance_events e WHERE e.session_id = provenance_sessions.id
                  AND e.payload_json != '{\"purged\":true}') OR
                  EXISTS(SELECT 1 FROM provenance_attachments a WHERE a.session_id = provenance_sessions.id
                  AND a.purged = 0)) ORDER BY started_at",
            )
            .jctx("STORE_QUERY", "cannot prepare expired provenance query")?;
        let rows = statement
            .query_map([cutoff], |row| row.get(0))
            .jctx("STORE_QUERY", "cannot read expired provenance sessions")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode expired provenance sessions")
    }

    pub fn expired_discarded_layers(&self, cutoff: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT layer_id FROM discard_records WHERE purge_after <= ?1 ORDER BY purge_after",
            )
            .jctx("STORE_QUERY", "cannot prepare expired Discard query")?;
        let rows = statement
            .query_map([cutoff], |row| row.get(0))
            .jctx("STORE_QUERY", "cannot read expired discarded Layers")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode expired discarded Layers")
    }

    pub fn expire_claims(&mut self, cutoff: &str) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE claims SET released_at = ?1 WHERE released_at IS NULL AND expires_at <= ?1",
                [cutoff],
            )
            .jctx("STORE_WRITE", "cannot expire Claims")
    }

    pub fn create_claim(&mut self, layer_id: &str, resource: &str, seconds: u64) -> Result<String> {
        let layer = self.layer(layer_id)?;
        let id = ulid::Ulid::new().to_string();
        let expires = (Utc::now() + chrono::Duration::seconds(seconds as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn
            .execute(
                "INSERT INTO claims(id, layer_id, resource, kind, expires_at, released_at, created_at)
                 VALUES (?1, ?2, ?3, 'path', ?4, NULL, ?5)",
                params![id, layer.id, resource, expires, now()],
            )
            .jctx("STORE_WRITE", "cannot create Claim")?;
        self.append_event(
            "claim.created",
            Some("claim"),
            Some(&id),
            &json!({"layer_id": layer.id, "resource": resource, "expires_at": expires}),
        )?;
        Ok(id)
    }

    pub fn claims(&self) -> Result<Vec<serde_json::Value>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT c.id, c.layer_id, l.name, c.resource, c.kind, c.expires_at, c.released_at,
                 c.created_at FROM claims c JOIN layers l ON l.id = c.layer_id
                 WHERE c.released_at IS NULL AND c.expires_at > ?1 ORDER BY c.created_at",
            )
            .jctx("STORE_QUERY", "cannot prepare Claim list")?;
        let rows = statement
            .query_map([now()], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "layer_id": row.get::<_, String>(1)?,
                    "layer_name": row.get::<_, String>(2)?,
                    "resource": row.get::<_, String>(3)?,
                    "kind": row.get::<_, String>(4)?,
                    "expires_at": row.get::<_, String>(5)?,
                    "released_at": row.get::<_, Option<String>>(6)?,
                    "created_at": row.get::<_, String>(7)?,
                }))
            })
            .jctx("STORE_QUERY", "cannot read Claims")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .jctx("STORE_QUERY", "cannot decode Claims")
    }

    pub fn renew_claim(&mut self, id: &str, seconds: u64) -> Result<String> {
        let expires = (Utc::now() + chrono::Duration::seconds(seconds as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let changed = self
            .conn
            .execute(
                "UPDATE claims SET expires_at = ?1 WHERE id = ?2 AND released_at IS NULL",
                params![expires, id],
            )
            .jctx("STORE_WRITE", "cannot renew Claim")?;
        if changed != 1 {
            return Err(JavelinError::invalid("Claim is unknown or released"));
        }
        self.append_event(
            "claim.renewed",
            Some("claim"),
            Some(id),
            &json!({"expires_at": expires}),
        )?;
        Ok(expires)
    }

    pub fn release_claim(&mut self, id: &str) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE claims SET released_at = ?1 WHERE id = ?2 AND released_at IS NULL",
                params![now(), id],
            )
            .jctx("STORE_WRITE", "cannot release Claim")?;
        if changed != 1 {
            return Err(JavelinError::invalid("Claim is unknown or released"));
        }
        self.append_event("claim.released", Some("claim"), Some(id), &json!({}))?;
        Ok(())
    }
}

fn world_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorldVersion> {
    Ok(WorldVersion {
        id: row.get(0)?,
        sequence: row.get(1)?,
        parent_version: row.get(2)?,
        root_tree: row.get(3)?,
        accepted_contribution: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn layer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Layer> {
    Ok(Layer {
        id: row.get(0)?,
        name: row.get(1)?,
        origin_ref: row.get(2)?,
        synchronized_ref: row.get(3)?,
        head_checkpoint: row.get(4)?,
        target_kind: row.get(5)?,
        target_id: row.get(6)?,
        status: row.get(7)?,
        view_path: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn checkpoint_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        id: row.get(0)?,
        layer_id: row.get(1)?,
        sequence: row.get(2)?,
        previous_checkpoint: row.get(3)?,
        root_tree: row.get(4)?,
        synchronized_ref: row.get(5)?,
        reason: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn conflict_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConflictRecord> {
    let base: Option<String> = row.get(4)?;
    let target: Option<String> = row.get(5)?;
    let private: Option<String> = row.get(6)?;
    Ok(ConflictRecord {
        id: row.get(0)?,
        layer_id: row.get(1)?,
        path: row.get(2)?,
        conflict_type: row.get(3)?,
        base_entry: decode_optional(base),
        target_entry: decode_optional(target),
        private_entry: decode_optional(private),
        target_ref: row.get(7)?,
        status: row.get(8)?,
        resolution: row.get(9)?,
        created_at: row.get(10)?,
    })
}

fn encode_optional(entry: &Option<TreeEntry>) -> Result<Option<String>> {
    entry
        .as_ref()
        .map(|entry| {
            serde_json::to_string(entry).map_err(|error| {
                JavelinError::corruption(format!("cannot encode path state: {error}"))
            })
        })
        .transpose()
}

fn decode_optional(value: Option<String>) -> Option<TreeEntry> {
    value.and_then(|value| serde_json::from_str(&value).ok())
}

fn append_event_tx(
    tx: &Transaction<'_>,
    event_type: &str,
    subject_kind: Option<&str>,
    subject_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO events(event_type, subject_kind, subject_id, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event_type,
            subject_kind,
            subject_id,
            payload.to_string(),
            now()
        ],
    )
    .jctx("STORE_WRITE", "cannot append event")?;
    Ok(tx.last_insert_rowid())
}

fn append_event_conn(
    conn: &Connection,
    event_type: &str,
    subject_kind: Option<&str>,
    subject_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO events(event_type, subject_kind, subject_id, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event_type,
            subject_kind,
            subject_id,
            payload.to_string(),
            now()
        ],
    )
    .jctx("STORE_WRITE", "cannot append event")?;
    Ok(conn.last_insert_rowid())
}

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(unix)]
fn startup_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn startup_process_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .jctx("STORE_IO", "cannot protect Javelin metadata")
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
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
            source_layer TEXT NOT NULL REFERENCES layers(id),
            source_checkpoint TEXT NOT NULL REFERENCES layer_checkpoints(id),
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
    .jctx("MIGRATION_FAILED", "cannot migrate Javelin Store")?;
    let has_environment = {
        let mut statement = conn
            .prepare("PRAGMA table_info(validation_runs)")
            .jctx("MIGRATION_FAILED", "cannot inspect validation schema")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .jctx("MIGRATION_FAILED", "cannot read validation schema")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .jctx("MIGRATION_FAILED", "cannot decode validation schema")?;
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
            "MIGRATION_FAILED",
            "cannot migrate validation environment schema",
        )?;
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
             VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )
        .jctx("MIGRATION_FAILED", "cannot record schema migration 2")?;
    }
    Ok(())
}
