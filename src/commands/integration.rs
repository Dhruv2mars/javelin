use super::*;

#[derive(Debug)]
pub(super) struct RefreshResult {
    pub(super) layer: Layer,
    pub(super) checkpoint: crate::model::Checkpoint,
    pub(super) target_ref: String,
    pub(super) target_root: String,
    pub(super) changed: bool,
}

pub(super) fn refresh(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let layer = selected_layer(context, store, requested)?;
    let result = refresh_layer(store, &layer)?;
    emit(
        json_output,
        &json!({
            "layer": result.layer,
            "checkpoint": result.checkpoint,
            "target_ref": result.target_ref,
            "target_root": result.target_root,
            "changed": result.changed,
        }),
        format!(
            "Refreshed {} to {} through {}",
            result.layer.name, result.target_ref, result.checkpoint.id
        ),
    )
}

pub(super) fn refresh_layer(store: &mut Store, original: &Layer) -> Result<RefreshResult> {
    if original.status == LayerStatus::Discarded {
        return Err(JavelinError::policy(format!(
            "Private Layer {} is discarded",
            original.name
        )));
    }
    let open_conflicts = store.conflicts(Some(&original.id), false)?;
    if !open_conflicts.is_empty() {
        return Err(JavelinError::conflict(format!(
            "Private Layer {} has {} unresolved Conflicts",
            original.name,
            open_conflicts.len()
        ))
        .details(json!({"conflict_ids": open_conflicts.iter().map(|item| &item.id).collect::<Vec<_>>() }))
        .recovery([format!("javelin conflict list {}", original.name)]));
    }
    let source_checkpoint = reconcile(store, original, "refresh-reconcile")?;
    let layer = store.layer(&original.id)?;
    let (_, _, base_tree) = store.tree_for_ref(&source_checkpoint.synchronized_ref)?;
    let private_tree = store.objects.read_tree(&source_checkpoint.root_tree)?;
    let (target_ref, target_root, target_tree) = target_state(store, &layer)?;
    if source_checkpoint.synchronized_ref == target_ref {
        return Ok(RefreshResult {
            layer,
            checkpoint: source_checkpoint,
            target_ref,
            target_root,
            changed: false,
        });
    }

    let (integrated, conflicts) = integrate_trees(store, &base_tree, &target_tree, &private_tree)?;
    if !conflicts.is_empty() {
        let ids = store.record_conflicts(&layer.id, &target_ref, &conflicts)?;
        return Err(JavelinError::conflict(format!(
            "Refresh found {} incompatible path states",
            ids.len()
        ))
        .details(json!({"layer_id": layer.id, "conflict_ids": ids}))
        .recovery([format!("javelin conflict list {}", layer.name)]));
    }
    let root_tree = store.objects.put_tree(&integrated)?;
    store.register_object(
        &root_tree,
        ObjectKind::Tree,
        crate::objects::encode_tree(&integrated)?.len() as u64,
    )?;
    let checkpoint = store.append_checkpoint(&layer.id, &root_tree, &target_ref, "refresh")?;
    store.mark_view(&layer.id, &checkpoint.id, true, "materializing")?;
    let marker = (layer.id != "local").then(|| ViewMarker {
        format: 1,
        project: store.root.to_string_lossy().into_owned(),
        layer_id: layer.id.clone(),
    });
    match materialize_tree_from_cache(
        &integrated,
        &root_tree,
        &store.metadata,
        Path::new(&layer.view_path),
        &store.objects,
        marker.as_ref(),
    ) {
        Ok(backend) => store.mark_view(&layer.id, &checkpoint.id, false, backend)?,
        Err(error) => {
            store.mark_view(&layer.id, &checkpoint.id, true, "repair_required")?;
            return Err(error.recovery([format!("javelin repair --view {}", layer.name)]));
        }
    }
    store.append_event(
        "refresh.completed",
        Some("layer"),
        Some(&layer.id),
        &json!({"checkpoint_id": checkpoint.id, "target_ref": target_ref, "root_tree": root_tree}),
    )?;
    Ok(RefreshResult {
        layer: store.layer(&layer.id)?,
        checkpoint,
        target_ref,
        target_root,
        changed: true,
    })
}

pub(super) fn integrate_trees(
    store: &mut Store,
    base: &Tree,
    target: &Tree,
    private: &Tree,
) -> Result<(Tree, Vec<ConflictInput>)> {
    let base_map = base.map();
    let target_map = target.map();
    let private_map = private.map();
    let mut paths = BTreeSet::new();
    paths.extend(base_map.keys().cloned());
    paths.extend(target_map.keys().cloned());
    paths.extend(private_map.keys().cloned());
    let mut result = BTreeMap::new();
    let mut conflicts = Vec::new();
    for path in paths {
        let base_entry = base_map.get(&path).cloned();
        let target_entry = target_map.get(&path).cloned();
        let private_entry = private_map.get(&path).cloned();
        let selected = if target_entry == base_entry {
            private_entry.clone()
        } else if private_entry == base_entry || private_entry == target_entry {
            target_entry.clone()
        } else if let Some(merged) = try_text_integration(
            store,
            base_entry.as_ref(),
            target_entry.as_ref(),
            private_entry.as_ref(),
        )? {
            Some(merged)
        } else {
            let conflict_type = ConflictKind::classify(
                base_entry.as_ref(),
                target_entry.as_ref(),
                private_entry.as_ref(),
            )
            .as_str()
            .to_string();
            conflicts.push(ConflictInput {
                path: path.clone(),
                conflict_type,
                base: base_entry,
                target: target_entry,
                private: private_entry,
            });
            continue;
        };
        if let Some(entry) = selected {
            result.insert(path, entry);
        }
    }
    let mut folded = BTreeMap::<String, String>::new();
    let mut case_paths = BTreeSet::new();
    for path in result.keys() {
        let key = path.to_lowercase();
        if let Some(previous) = folded.insert(key, path.clone())
            && previous != *path
        {
            case_paths.insert(previous);
            case_paths.insert(path.clone());
        }
    }
    for path in case_paths {
        conflicts.push(ConflictInput {
            path: path.clone(),
            conflict_type: ConflictKind::Case.as_str().to_string(),
            base: base_map.get(&path).cloned(),
            target: target_map.get(&path).cloned(),
            private: private_map.get(&path).cloned(),
        });
    }
    Ok((Tree::from_map(result), conflicts))
}

fn try_text_integration(
    store: &mut Store,
    base: Option<&TreeEntry>,
    target: Option<&TreeEntry>,
    private: Option<&TreeEntry>,
) -> Result<Option<TreeEntry>> {
    let (Some(base), Some(target), Some(private)) = (base, target, private) else {
        return Ok(None);
    };
    if base.kind != EntryKind::File
        || target.kind != EntryKind::File
        || private.kind != EntryKind::File
        || base.executable != target.executable
        || base.executable != private.executable
    {
        return Ok(None);
    }
    let ids = [
        base.object_id.as_deref().unwrap_or(""),
        target.object_id.as_deref().unwrap_or(""),
        private.object_id.as_deref().unwrap_or(""),
    ];
    for id in ids {
        let (kind, length) = store.objects.info(id)?;
        if kind != ObjectKind::Blob || length > TEXT_DIFF_LIMIT {
            return Ok(None);
        }
    }
    let base_bytes = store.objects.read_blob(ids[0])?;
    let target_bytes = store.objects.read_blob(ids[1])?;
    let private_bytes = store.objects.read_blob(ids[2])?;
    if base_bytes.contains(&0) || target_bytes.contains(&0) || private_bytes.contains(&0) {
        return Ok(None);
    }
    let (Ok(base_text), Ok(private_text), Ok(target_text)) = (
        std::str::from_utf8(&base_bytes),
        std::str::from_utf8(&private_bytes),
        std::str::from_utf8(&target_bytes),
    ) else {
        return Ok(None);
    };
    let Ok(merged) = diffy::merge(base_text, private_text, target_text) else {
        return Ok(None);
    };
    let object_id = store.objects.put_blob(merged.as_bytes())?;
    store.register_object(&object_id, ObjectKind::Blob, merged.len() as u64)?;
    Ok(Some(TreeEntry {
        path: private.path.clone(),
        kind: EntryKind::File,
        object_id: Some(object_id),
        executable: private.executable,
    }))
}

enum ConflictKind {
    Case,
    CreateCreate,
    DeleteModify,
    ModifyDelete,
    Type,
    Mode,
    ModifyModify,
    PathState,
}

impl ConflictKind {
    fn classify(
        base: Option<&TreeEntry>,
        target: Option<&TreeEntry>,
        private: Option<&TreeEntry>,
    ) -> Self {
        match (base, target, private) {
            (None, Some(_), Some(_)) => Self::CreateCreate,
            (Some(_), None, Some(_)) => Self::DeleteModify,
            (Some(_), Some(_), None) => Self::ModifyDelete,
            (Some(base), Some(target), Some(private))
                if base.kind != target.kind || base.kind != private.kind =>
            {
                Self::Type
            }
            (Some(_), Some(target), Some(private)) if target.executable != private.executable => {
                Self::Mode
            }
            (Some(_), Some(_), Some(_)) => Self::ModifyModify,
            _ => Self::PathState,
        }
    }

    const fn as_str(&self) -> &'static str {
        match self {
            Self::Case => "case",
            Self::CreateCreate => "create_create",
            Self::DeleteModify => "delete_modify",
            Self::ModifyDelete => "modify_delete",
            Self::Type => "type",
            Self::Mode => "mode",
            Self::ModifyModify => "modify_modify",
            Self::PathState => "path_state",
        }
    }
}

fn target_state(store: &Store, layer: &Layer) -> Result<(String, String, Tree)> {
    if layer.target_kind == TargetKind::World {
        let version = store.current_world()?;
        let tree = store.objects.read_tree(&version.root_tree)?;
        Ok((version.id, version.root_tree, tree))
    } else {
        let parent = store.layer(
            layer
                .target_id
                .as_deref()
                .ok_or_else(|| JavelinError::corruption("Layer target has no ID"))?,
        )?;
        let head = store.layer_head(&parent)?;
        let tree = store.objects.read_tree(&head.root_tree)?;
        Ok((head.id, head.root_tree, tree))
    }
}
