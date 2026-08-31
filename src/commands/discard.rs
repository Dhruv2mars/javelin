use super::*;

pub(super) fn discard(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    cascade: bool,
    reparent: Option<&str>,
    purge: bool,
    json_output: bool,
) -> Result<()> {
    let layer = selected_layer(context, store, requested)?;
    if layer.id == "local" {
        let world = store.current_world()?;
        let tree = store.objects.read_tree(&world.root_tree)?;
        let checkpoint =
            store.append_checkpoint(&layer.id, &world.root_tree, &world.id, "discard")?;
        store.mark_view(&layer.id, &checkpoint.id, true, "materializing")?;
        let backend = materialize_tree_from_cache(
            &tree,
            &world.root_tree,
            &store.metadata,
            &store.root,
            &store.objects,
            None,
        )?;
        store.mark_view(&layer.id, &checkpoint.id, false, backend)?;
        return emit(
            json_output,
            &json!({"layer": "local", "checkpoint": checkpoint, "world_version": world.id}),
            format!("Discarded Local Layer changes through {}", checkpoint.id),
        );
    }
    let children = store.active_children(&layer.id)?;
    if !children.is_empty() && !cascade && reparent.is_none() {
        return Err(JavelinError::policy(format!(
            "Private Layer {} has {} active children; use --cascade or --reparent",
            layer.name,
            children.len()
        )));
    }
    if let Some(target) = reparent {
        let (target_kind, target_id) = if target == "world" {
            ("world", None)
        } else if let Some(target_layer) = target.strip_prefix("layer:") {
            let target_layer = store.layer(target_layer)?;
            if target_layer.id == layer.id {
                return Err(JavelinError::policy(
                    "child Layers cannot be reparented to the Layer being discarded",
                ));
            }
            ("layer", Some(target_layer.id))
        } else {
            return Err(JavelinError::invalid(
                "--reparent must be world or layer:LAYER",
            ));
        };
        if let Some(target_id) = target_id.as_deref() {
            for child in &children {
                if reparent_would_cycle(store, &child.id, target_id)? {
                    return Err(JavelinError::policy(format!(
                        "reparenting {} to {} would create a Layer cycle",
                        child.name, target
                    )));
                }
            }
        }
        for child in &children {
            store.reparent_layer(&child.id, target_kind, target_id.as_deref())?;
        }
    }
    if cascade {
        cascade_discard_children(store, &layer.id, purge)?;
    }
    discard_named(store, &layer)?;
    if purge {
        store.purge_layer(&layer.id)?;
    }
    emit(
        json_output,
        &json!({"layer_id": layer.id, "name": layer.name, "purged": purge}),
        format!("Discarded Private Layer {}", layer.name),
    )
}

fn cascade_discard_children(store: &mut Store, parent_id: &str, purge: bool) -> Result<()> {
    let children = if purge {
        store.all_children(parent_id)?
    } else {
        store.active_children(parent_id)?
    };
    for child in children {
        cascade_discard_children(store, &child.id, purge)?;
        if child.status != LayerStatus::Discarded {
            discard_named(store, &child)?;
        }
        if purge {
            let trash = store.metadata.join("trash").join(&child.id);
            store.purge_layer(&child.id)?;
            if trash.exists() {
                fs::remove_dir_all(&trash)
                    .jctx("DISCARD_IO", "cannot purge retained child view")?;
            }
        }
    }
    Ok(())
}

fn reparent_would_cycle(store: &Store, child_id: &str, target_id: &str) -> Result<bool> {
    let mut current = Some(target_id.to_string());
    let mut seen = BTreeSet::new();
    while let Some(id) = current {
        if id == child_id {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            return Err(JavelinError::corruption(
                "existing Private Layer target cycle",
            ));
        }
        let layer = store.layer(&id)?;
        current = if layer.target_kind == TargetKind::Layer {
            layer.target_id
        } else {
            None
        };
    }
    Ok(false)
}

fn discard_named(store: &mut Store, layer: &Layer) -> Result<()> {
    let config = Config::load(&store.root)?;
    let purge_after = (chrono::Utc::now()
        + chrono::Duration::days(config.retention.discarded_days as i64))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let view = PathBuf::from(&layer.view_path);
    let trash = store.metadata.join("trash").join(&layer.id);
    if view.exists() {
        if trash.exists() {
            fs::remove_dir_all(&trash).jctx("DISCARD_IO", "cannot replace retained trash view")?;
        }
        fs::rename(&view, &trash).jctx("DISCARD_IO", "cannot retain discarded Layer view")?;
        sync_dir(
            trash
                .parent()
                .ok_or_else(|| JavelinError::corruption("trash path has no parent"))?,
        )?;
        if view.parent() != trash.parent() {
            sync_dir(
                view.parent()
                    .ok_or_else(|| JavelinError::corruption("view path has no parent"))?,
            )?;
        }
    }
    store.record_discard(&layer.id, &purge_after)
}

pub(super) fn discarded(
    store: &mut Store,
    command: DiscardedCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        DiscardedCommand::List => {
            let layers = store
                .layers(true)?
                .into_iter()
                .filter(|layer| layer.status == LayerStatus::Discarded)
                .collect::<Vec<_>>();
            let human = layers
                .iter()
                .map(|layer| format!("{}\t{}", layer.name, layer.id))
                .collect::<Vec<_>>()
                .join("\n");
            emit(json_output, &json!({"discarded": layers}), human)
        }
        DiscardedCommand::Recover { layer } => {
            let layer = store.layer(&layer)?;
            if layer.status != LayerStatus::Discarded {
                return Err(JavelinError::invalid("Private Layer is not discarded"));
            }
            let trash = store.metadata.join("trash").join(&layer.id);
            let view = PathBuf::from(&layer.view_path);
            if trash.exists() {
                if let Some(parent) = view.parent() {
                    fs::create_dir_all(parent).jctx("DISCARD_IO", "cannot create view parent")?;
                }
                fs::rename(&trash, &view).jctx("DISCARD_IO", "cannot recover retained view")?;
                sync_dir(
                    view.parent()
                        .ok_or_else(|| JavelinError::corruption("view path has no parent"))?,
                )?;
                if trash.parent() != view.parent() {
                    sync_dir(
                        trash
                            .parent()
                            .ok_or_else(|| JavelinError::corruption("trash path has no parent"))?,
                    )?;
                }
            } else {
                let head = store.layer_head(&layer)?;
                let tree = store.objects.read_tree(&head.root_tree)?;
                let marker = ViewMarker {
                    format: 1,
                    project: store.root.to_string_lossy().into_owned(),
                    layer_id: layer.id.clone(),
                };
                materialize_tree_from_cache(
                    &tree,
                    &head.root_tree,
                    &store.metadata,
                    &view,
                    &store.objects,
                    Some(&marker),
                )?;
            }
            store.recover_discard(&layer.id)?;
            emit(
                json_output,
                &json!({"layer": store.layer(&layer.id)?}),
                format!("Recovered Private Layer {}", layer.name),
            )
        }
        DiscardedCommand::Purge { layer } => {
            let layer = store.layer(&layer)?;
            let trash = store.metadata.join("trash").join(&layer.id);
            store.purge_layer(&layer.id)?;
            if trash.exists() {
                fs::remove_dir_all(&trash).jctx("DISCARD_IO", "cannot purge retained view")?;
            }
            emit(
                json_output,
                &json!({"purged_layer_id": layer.id}),
                format!("Purged discarded Private Layer {}", layer.name),
            )
        }
    }
}
