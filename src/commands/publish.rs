use super::*;

pub(super) fn publish(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    key: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let layer = selected_layer(context, store, requested)?;
    crate::fault::hit("before_publish_lease");
    let lock_name = if layer.target_kind == TargetKind::World {
        "world".to_string()
    } else {
        format!("layer-{}", layer.target_id.as_deref().unwrap_or("missing"))
    };
    let lock_file = open_publish_lock(store, &lock_name)?;
    acquire_queued_publish_lock(store, &lock_name, &lock_file)?;
    if let Some(key) = key
        && let Some(existing) = store.contribution_details_by_key(key)?
    {
        let source_layer = existing.source_layer.as_deref().ok_or_else(|| {
            JavelinError::stale("Publish idempotency key belongs to a purged Private Layer")
        })?;
        if layer.id != source_layer {
            return Err(JavelinError::stale(
                "Publish idempotency key belongs to a different Private Layer",
            ));
        }
        let recovered = if let Some(source_checkpoint) = &existing.source_checkpoint {
            let layer = store.layer(source_layer)?;
            let head = store.layer_head(&layer)?;
            let checkpoint = if head.synchronized_ref == existing.resulting_target_ref {
                Some(head.id)
            } else if head.id == *source_checkpoint {
                Some(
                    store
                        .append_checkpoint(
                            &layer.id,
                            &head.root_tree,
                            &existing.resulting_target_ref,
                            &format!("recover Publish Contribution {}", existing.id),
                        )?
                        .id,
                )
            } else {
                None
            };
            repair_layer_target_view_under_lease(store, &layer)?;
            checkpoint
        } else {
            None
        };
        return emit(
            json_output,
            &json!({
                "idempotent": true,
                "contribution_id": existing.id,
                "resulting_target_ref": existing.resulting_target_ref,
                "source_checkpoint": recovered,
            }),
            format!(
                "Contribution {} already accepted as {}",
                existing.id, existing.resulting_target_ref
            ),
        );
    }
    let refreshed = refresh_layer(store, &layer)?;
    let candidate = store.objects.read_tree(&refreshed.checkpoint.root_tree)?;
    crate::fault::hit("after_candidate_construction");
    let changes = {
        let (_, _, base) = store.tree_for_ref(&refreshed.checkpoint.synchronized_ref)?;
        diff_trees(&base, &candidate)
    };
    let validations = run_validations(store, &candidate, &refreshed.checkpoint.root_tree)?;
    if validations
        .iter()
        .any(|validation| validation.required && validation.exit_code != 0)
    {
        let _ = FileExt::unlock(&lock_file);
        return Err(
            JavelinError::verification("required World Rule rejected Publish")
                .details(json!({"validations": validations})),
        );
    }
    let validation_ids = validations
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    let (contribution_id, resulting_ref) = store.accept_publish(
        &refreshed.layer,
        &refreshed.checkpoint,
        &refreshed.checkpoint.root_tree,
        key,
        &validation_ids,
        &json!({"changes": changes}),
    )?;
    let source_checkpoint = store.append_checkpoint(
        &refreshed.layer.id,
        &refreshed.checkpoint.root_tree,
        &resulting_ref,
        &format!("Publish Contribution {contribution_id}"),
    )?;
    crate::fault::hit("during_view_update");
    if refreshed.layer.target_kind == TargetKind::Layer
        && let Some(parent_id) = &refreshed.layer.target_id
    {
        let parent = store.layer(parent_id)?;
        let parent_head = store.layer_head(&parent)?;
        let marker = ViewMarker {
            format: 1,
            project: store.root.to_string_lossy().into_owned(),
            layer_id: parent.id.clone(),
        };
        match materialize_tree_from_cache(
            &candidate,
            &refreshed.checkpoint.root_tree,
            &store.metadata,
            Path::new(&parent.view_path),
            &store.objects,
            Some(&marker),
        ) {
            Ok(backend) => store.mark_view(&parent.id, &parent_head.id, false, backend)?,
            Err(_) => store.mark_view(&parent.id, &parent_head.id, true, "repair_required")?,
        }
    }
    FileExt::unlock(&lock_file).jctx(8, "PUBLISH_LOCK", "cannot release Publish lease")?;
    emit(
        json_output,
        &json!({
            "contribution_id": contribution_id,
            "resulting_target_ref": resulting_ref,
            "source_checkpoint": source_checkpoint,
            "candidate_root": refreshed.checkpoint.root_tree,
            "changes": changes,
            "validations": validations,
        }),
        format!("Published Contribution {contribution_id} as {resulting_ref}"),
    )
}

fn repair_layer_target_view_under_lease(store: &mut Store, source: &Layer) -> Result<()> {
    if source.target_kind != TargetKind::Layer {
        return Ok(());
    }
    let parent_id = source
        .target_id
        .as_deref()
        .ok_or_else(|| JavelinError::corruption("Layer target has no ID"))?;
    let parent = store.layer(parent_id)?;
    let parent_head = store.layer_head(&parent)?;
    let tree = store.objects.read_tree(&parent_head.root_tree)?;
    let marker = ViewMarker {
        format: 1,
        project: store.root.to_string_lossy().into_owned(),
        layer_id: parent.id.clone(),
    };
    match materialize_tree_from_cache(
        &tree,
        &parent_head.root_tree,
        &store.metadata,
        Path::new(&parent.view_path),
        &store.objects,
        Some(&marker),
    ) {
        Ok(backend) => store.mark_view(&parent.id, &parent_head.id, false, backend),
        Err(_) => store.mark_view(&parent.id, &parent_head.id, true, "repair_required"),
    }
}

pub(super) fn open_publish_lock(store: &Store, target: &str) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(
            store
                .metadata
                .join("locks")
                .join(format!("{target}.publish.lock")),
        )
        .jctx(8, "PUBLISH_LOCK", "cannot open Publish lease")
}

pub(super) fn acquire_publish_lock(file: &File) -> Result<()> {
    let start = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if start.elapsed() < Duration::from_secs(30) => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                return Err(JavelinError::busy(format!(
                    "Publish lease timed out: {error}"
                )));
            }
        }
    }
}

fn acquire_queued_publish_lock(store: &mut Store, target: &str, file: &File) -> Result<()> {
    let request_id = ulid::Ulid::new().to_string();
    let pid = std::process::id();
    store
        .conn
        .execute(
            "INSERT INTO publish_queue(request_id, target, pid, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![request_id, target, pid, now()],
        )
        .jctx(8, "PUBLISH_QUEUE", "cannot enter Publish queue")?;
    let started = Instant::now();
    loop {
        let head: Option<(String, u32, String)> = store
            .conn
            .query_row(
                "SELECT request_id, pid, created_at FROM publish_queue
                 WHERE target = ?1 ORDER BY ticket LIMIT 1",
                [target],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .jctx(8, "PUBLISH_QUEUE", "cannot inspect Publish queue")?;
        match head {
            Some((head_id, _head_pid, _created_at)) if head_id == request_id => {
                if file.try_lock_exclusive().is_ok() {
                    store
                        .conn
                        .execute(
                            "DELETE FROM publish_queue WHERE request_id = ?1",
                            [&request_id],
                        )
                        .jctx(8, "PUBLISH_QUEUE", "cannot leave Publish queue")?;
                    return Ok(());
                }
            }
            Some((head_id, head_pid, created_at))
                if !process_alive(head_pid) || publish_request_expired(&created_at) =>
            {
                store
                    .conn
                    .execute(
                        "DELETE FROM publish_queue WHERE request_id = ?1",
                        [&head_id],
                    )
                    .jctx(
                        8,
                        "PUBLISH_QUEUE",
                        "cannot remove abandoned Publish request",
                    )?;
            }
            _ => {}
        }
        if started.elapsed() >= Duration::from_secs(300) {
            let _ = store.conn.execute(
                "DELETE FROM publish_queue WHERE request_id = ?1",
                [&request_id],
            );
            return Err(JavelinError::busy(
                "Publish queue wait exceeded 300 seconds",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn publish_request_expired(created_at: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(created_at).map_or(true, |created| {
        chrono::Utc::now().signed_duration_since(created.with_timezone(&chrono::Utc))
            > chrono::Duration::seconds(300)
    })
}
