use super::*;

pub(super) fn fsck(store: &mut Store, json_output: bool) -> Result<()> {
    let integrity: String = store
        .conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .jctx(7, "STORE_QUERY", "cannot run SQLite integrity check")?;
    if integrity != "ok" {
        return Err(JavelinError::corruption(format!(
            "SQLite integrity check failed: {integrity}"
        )));
    }
    let foreign_key_failure: Option<(String, i64)> = store
        .conn
        .query_row("PRAGMA foreign_key_check", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
        .jctx(7, "STORE_QUERY", "cannot run SQLite foreign-key check")?;
    if let Some((table, rowid)) = foreign_key_failure {
        return Err(JavelinError::corruption(format!(
            "foreign-key violation in {table} row {rowid}"
        )));
    }
    let reachability = collect_reachability(store)?;
    for root in &reachability.roots {
        let (kind, _) = store.objects.validate(root)?;
        if kind != ObjectKind::Tree {
            return Err(JavelinError::corruption(format!(
                "root reference {root} does not identify a tree"
            )));
        }
    }
    for id in &reachability.blobs {
        let (kind, _) = store.objects.validate(id)?;
        if kind != ObjectKind::Blob {
            return Err(JavelinError::corruption(format!(
                "blob reference {id} identifies a tree"
            )));
        }
    }
    let all_objects = store.objects.all_ids()?;
    for id in &all_objects {
        let _ = store.objects.validate(id)?;
    }
    let mut statement = store
        .conn
        .prepare("SELECT id, kind, uncompressed_size FROM object_metadata")
        .jctx(7, "STORE_QUERY", "cannot prepare object metadata check")?;
    let metadata = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .jctx(7, "STORE_QUERY", "cannot read object metadata")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .jctx(7, "STORE_QUERY", "cannot decode object metadata")?;
    let metadata = metadata
        .into_iter()
        .map(|(id, kind, size)| (id, (kind, size)))
        .collect::<BTreeMap<_, _>>();
    let referenced = reachability
        .roots
        .iter()
        .chain(&reachability.blobs)
        .collect::<BTreeSet<_>>();
    for id in referenced {
        if !metadata.contains_key(id) {
            return Err(JavelinError::corruption(format!(
                "object metadata missing for {id}"
            )));
        }
    }
    for (id, (expected_kind, expected_size)) in metadata {
        let (kind, size) = store.objects.validate(&id)?;
        if format!("{kind:?}").to_lowercase() != expected_kind || size as i64 != expected_size {
            return Err(JavelinError::corruption(format!(
                "object metadata mismatch for {id}"
            )));
        }
    }
    let checked = all_objects.len();
    emit(
        json_output,
        &json!({
            "valid": true,
            "objects_checked": checked,
            "root_references": reachability.roots.len(),
            "blob_references": reachability.blobs.len(),
            "sqlite_integrity": "ok",
            "foreign_keys": "ok",
        }),
        format!("Store valid: {checked} object references checked"),
    )
}

pub(super) fn repair(store: &mut Store, requested: Option<&str>, json_output: bool) -> Result<()> {
    let layers = if let Some(layer) = requested {
        vec![store.layer(layer)?]
    } else {
        store.layers(false)?
    };
    let mut repaired = Vec::new();
    for layer in layers {
        let reconcile_lock = open_reconcile_lock(store)?;
        acquire_publish_lock(&reconcile_lock)?;
        let head = store.layer_head(&layer)?;
        let tree = store.objects.read_tree(&head.root_tree)?;
        store.mark_view(&layer.id, &head.id, true, "materializing")?;
        invalidate_root_cache(&store.metadata, &head.root_tree)?;
        let marker = (layer.id != "local").then(|| ViewMarker {
            format: 1,
            project: store.root.to_string_lossy().into_owned(),
            layer_id: layer.id.clone(),
        });
        let backend = match materialize_tree_from_cache(
            &tree,
            &head.root_tree,
            &store.metadata,
            Path::new(&layer.view_path),
            &store.objects,
            marker.as_ref(),
        ) {
            Ok(backend) => backend,
            Err(error) => {
                store.mark_view(&layer.id, &head.id, true, "repair_required")?;
                return Err(error);
            }
        };
        store.mark_view(&layer.id, &head.id, false, backend)?;
        FileExt::unlock(&reconcile_lock).jctx(8, "RECONCILE_LOCK", "cannot release repair lock")?;
        repaired.push(json!({"layer": layer.name, "checkpoint": head.id, "backend": backend}));
    }
    store.append_event(
        "repair.completed",
        Some("world"),
        None,
        &json!({"views": repaired}),
    )?;
    emit(
        json_output,
        &json!({"repaired": repaired}),
        format!("Repaired {} Managed views", repaired.len()),
    )
}

pub(super) fn doctor(store: &Store, json_output: bool) -> Result<()> {
    let world = store.current_world()?;
    let backend = if cfg!(target_os = "macos") {
        "copy_on_write_or_copy"
    } else {
        "copy"
    };
    emit(
        json_output,
        &json!({
            "version": crate::VERSION,
            "world": world,
            "database": store.metadata.join("store.sqlite3"),
            "materialization": backend,
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "monitor_ready": store.metadata.join("monitor/ready").exists(),
        }),
        format!(
            "Javelin {}: World {}, store readable, backend {backend}",
            crate::VERSION,
            world.id
        ),
    )
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0xf) as usize] as char);
    }
    output
}
