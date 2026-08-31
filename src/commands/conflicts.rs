use super::*;

pub(super) fn conflict(
    _context: &ProjectContext,
    store: &mut Store,
    command: ConflictCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        ConflictCommand::List { layer } => {
            let layer_id = layer
                .map(|name| store.layer(&name).map(|layer| layer.id))
                .transpose()?;
            let conflicts = store.conflicts(layer_id.as_deref(), false)?;
            let human = conflicts
                .iter()
                .map(|conflict| {
                    format!(
                        "{}\t{}\t{}",
                        conflict.id, conflict.conflict_type, conflict.path
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            emit(
                json_output,
                &json!({"conflicts": conflicts_to_json(&conflicts)}),
                human,
            )
        }
        ConflictCommand::Show { id } => {
            let conflict = store.conflict(&id)?;
            emit(
                json_output,
                &conflict_to_json(&conflict),
                format!(
                    "{}\t{}\t{}",
                    conflict.id, conflict.conflict_type, conflict.path
                ),
            )
        }
        ConflictCommand::Resolve { id, r#use } => {
            let record = store.conflict(&id)?;
            if record.status != "open" {
                return Err(JavelinError::invalid(format!(
                    "Conflict {id} is already resolved"
                )));
            }
            let layer = store.layer(&record.layer_id)?;
            let head = store.layer_head(&layer)?;
            let head_tree = store.objects.read_tree(&head.root_tree)?;
            let remaining_before = store.conflicts(Some(&layer.id), false)?.len();
            let synchronize = remaining_before == 1;
            let resolved_tree = if r#use == "edited" {
                let policy = tracking_policy(store, &layer)?;
                scan_view_with_policy(Path::new(&layer.view_path), &store.objects, &policy)?.tree
            } else {
                let selected = match r#use.as_str() {
                    "base" => record.base_entry.clone(),
                    "target" => record.target_entry.clone(),
                    "private" => record.private_entry.clone(),
                    _ => unreachable!(),
                };
                apply_path_resolution(&head_tree, &record.path, selected)
            };
            let root_tree = store.objects.put_tree(&resolved_tree)?;
            let synchronized_ref = if synchronize {
                record.target_ref.as_str()
            } else {
                layer.synchronized_ref.as_str()
            };
            let checkpoint = store.append_checkpoint(
                &layer.id,
                &root_tree,
                synchronized_ref,
                &format!("resolve Conflict {id} using {use_value}", use_value = r#use),
            )?;
            store.mark_view(&layer.id, &checkpoint.id, true, "materializing")?;
            store.resolve_conflict(&id, &r#use)?;
            let marker = (layer.id != "local").then(|| ViewMarker {
                format: 1,
                project: store.root.to_string_lossy().into_owned(),
                layer_id: layer.id.clone(),
            });
            let backend = materialize_tree_from_cache(
                &resolved_tree,
                &root_tree,
                &store.metadata,
                Path::new(&layer.view_path),
                &store.objects,
                marker.as_ref(),
            )?;
            store.mark_view(&layer.id, &checkpoint.id, false, backend)?;
            emit(
                json_output,
                &json!({"conflict_id": id, "resolution": r#use, "checkpoint": checkpoint}),
                format!(
                    "Resolved Conflict {id} using {use_value}",
                    use_value = r#use
                ),
            )
        }
    }
}

fn apply_path_resolution(tree: &Tree, path: &str, selected: Option<TreeEntry>) -> Tree {
    let mut updates = BTreeMap::new();
    for entry in &tree.entries {
        if entry.path == path || entry.path.starts_with(&format!("{path}/")) {
            updates.insert(entry.path.clone(), None);
        }
    }
    if let Some(entry) = selected {
        updates.insert(path.to_string(), Some(entry));
    }
    apply_entries(tree, updates)
}

fn conflict_to_json(record: &crate::store::ConflictRecord) -> serde_json::Value {
    json!({
        "id": record.id,
        "layer_id": record.layer_id,
        "path": record.path,
        "type": record.conflict_type,
        "base": record.base_entry,
        "target": record.target_entry,
        "private": record.private_entry,
        "target_ref": record.target_ref,
        "status": record.status,
        "resolution": record.resolution,
        "created_at": record.created_at,
    })
}

fn conflicts_to_json(records: &[crate::store::ConflictRecord]) -> Vec<serde_json::Value> {
    records.iter().map(conflict_to_json).collect()
}
