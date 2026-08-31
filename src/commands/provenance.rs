use super::*;

pub(super) fn provenance(
    context: &ProjectContext,
    store: &mut Store,
    command: ProvenanceCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        ProvenanceCommand::Begin {
            layer,
            actor,
            kind,
            model,
        } => {
            let layer = layer
                .map(|name| store.layer(&name))
                .transpose()?
                .unwrap_or(context_layer(context, store)?);
            let actor = json!({
                "kind": kind.unwrap_or_else(|| "agent".to_string()),
                "name": actor,
                "model": model,
            });
            let session = store.begin_provenance(Some(&layer.id), &actor)?;
            emit(
                json_output,
                &json!({"session_id": session, "layer_id": layer.id, "actor": actor}),
                session,
            )
        }
        ProvenanceCommand::Event {
            session,
            event_type,
            payload,
        } => {
            let payload = payload
                .as_deref()
                .map(serde_json::from_str::<serde_json::Value>)
                .transpose()
                .map_err(|error| {
                    JavelinError::invalid(format!("invalid event payload JSON: {error}"))
                })?
                .unwrap_or_else(|| json!({}));
            let config = Config::load(&store.root)?;
            let payload = redact_value(payload, &config.provenance.redact);
            let event_id = store.append_provenance_event(&session, &event_type, &payload)?;
            emit(
                json_output,
                &json!({"event_id": event_id, "session_id": session, "type": event_type}),
                event_id,
            )
        }
        ProvenanceCommand::Attach {
            session,
            path,
            media_type,
        } => {
            let absolute = path.canonicalize().jctx(
                "PROVENANCE_IO",
                format!("cannot resolve {}", path.display()),
            )?;
            if !absolute.is_file() {
                return Err(JavelinError::invalid(
                    "provenance attachment must be a file",
                ));
            }
            let name = absolute
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| JavelinError::unsupported("attachment name must be UTF-8"))?;
            let object_id = store.objects.put_blob_file(&absolute)?;
            let size = fs::metadata(&absolute)
                .jctx("PROVENANCE_IO", "cannot stat provenance attachment")?
                .len();
            store.register_object(&object_id, ObjectKind::Blob, size)?;
            let attachment =
                store.attach_provenance(&session, name, media_type.as_deref(), &object_id)?;
            emit(
                json_output,
                &json!({"attachment_id": attachment, "object_id": object_id, "name": name, "size": size}),
                attachment,
            )
        }
        ProvenanceCommand::End { session } => {
            store.end_provenance(&session)?;
            emit(
                json_output,
                &json!({"session_id": session, "status": "ended"}),
                format!("Ended provenance session {session}"),
            )
        }
        ProvenanceCommand::Show { session, raw } => {
            let mut value = store.provenance_session(&session)?;
            if !raw
                && let Some(events) = value
                    .get_mut("events")
                    .and_then(serde_json::Value::as_array_mut)
            {
                for event in events {
                    if let Some(object) = event.as_object_mut() {
                        object.insert("payload".into(), json!({"hidden": true}));
                    }
                }
            }
            emit(json_output, &value, format!("Provenance session {session}"))
        }
        ProvenanceCommand::Search { query } => {
            let sessions = store.search_provenance(&query)?;
            let human = sessions
                .iter()
                .map(|session| session["id"].as_str().unwrap_or("unknown").to_string())
                .collect::<Vec<_>>()
                .join("\n");
            emit(json_output, &json!({"sessions": sessions}), human)
        }
        ProvenanceCommand::Purge { session } => {
            store.purge_provenance(&session)?;
            emit(
                json_output,
                &json!({"session_id": session, "raw_payloads_purged": true}),
                format!("Purged raw provenance payloads for {session}"),
            )
        }
    }
}

fn redact_value(value: serde_json::Value, configured: &[String]) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let sensitive_key = ["token", "password", "secret", "authorization", "api_key"]
                        .iter()
                        .any(|needle| key.to_lowercase().contains(needle));
                    let value = if sensitive_key {
                        json!("[REDACTED]")
                    } else {
                        redact_value(value, configured)
                    };
                    (key, value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| redact_value(value, configured))
                .collect(),
        ),
        serde_json::Value::String(mut value) => {
            for secret in configured {
                if !secret.is_empty() {
                    value = value.replace(secret, "[REDACTED]");
                }
            }
            serde_json::Value::String(value)
        }
        value => value,
    }
}

pub(super) fn explain(store: &Store, path: &str, json_output: bool) -> Result<()> {
    crate::paths::validate_relative(path)?;
    let mut accepted = Vec::new();
    for version in store.world_history()? {
        let tree = store.objects.read_tree(&version.root_tree)?;
        let changed = if let Some(parent) = &version.parent_version {
            let parent = store.world_version(parent)?;
            diff_trees(&store.objects.read_tree(&parent.root_tree)?, &tree)
                .iter()
                .any(|change| change.path == path)
        } else {
            tree.entries.iter().any(|entry| entry.path == path)
        };
        if changed {
            let sessions = if let Some(contribution) = &version.accepted_contribution {
                provenance_for_contribution(store, contribution)?
            } else {
                Vec::new()
            };
            accepted.push(json!({
                "world_version": version.id,
                "contribution_id": version.accepted_contribution,
                "root_tree": version.root_tree,
                "sessions": sessions,
            }));
        }
    }
    let mut private = Vec::new();
    for layer in store.layers(true)? {
        let history = store.checkpoint_history(&layer.id)?;
        let mut previous: Option<Tree> = None;
        for checkpoint in history {
            let tree = store.objects.read_tree(&checkpoint.root_tree)?;
            let changed = previous.as_ref().map_or_else(
                || tree.entries.iter().any(|entry| entry.path == path),
                |previous| {
                    diff_trees(previous, &tree)
                        .iter()
                        .any(|change| change.path == path)
                },
            );
            if changed {
                let sessions = provenance_for_checkpoint(store, &checkpoint.id)?;
                private.push(json!({
                    "layer_id": layer.id,
                    "layer_name": layer.name,
                    "checkpoint_id": checkpoint.id,
                    "reason": checkpoint.reason,
                    "sessions": sessions,
                }));
            }
            previous = Some(tree);
        }
    }
    emit(
        json_output,
        &json!({"path": path, "accepted": accepted, "private": private}),
        format!(
            "{} accepted changes, {} private changes for {path}",
            accepted.len(),
            private.len()
        ),
    )
}

fn provenance_for_contribution(store: &Store, contribution: &str) -> Result<Vec<String>> {
    let mut statement = store
        .conn
        .prepare(
            "SELECT session_id FROM contribution_provenance WHERE contribution_id = ?1 ORDER BY session_id",
        )
        .jctx("STORE_QUERY", "cannot prepare Contribution provenance")?;
    let rows = statement
        .query_map([contribution], |row| row.get(0))
        .jctx("STORE_QUERY", "cannot read Contribution provenance")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .jctx("STORE_QUERY", "cannot decode Contribution provenance")
}

fn provenance_for_checkpoint(store: &Store, checkpoint: &str) -> Result<Vec<String>> {
    let mut statement = store
        .conn
        .prepare(
            "SELECT session_id FROM checkpoint_provenance WHERE checkpoint_id = ?1 ORDER BY session_id",
        )
        .jctx("STORE_QUERY", "cannot prepare Checkpoint provenance")?;
    let rows = statement
        .query_map([checkpoint], |row| row.get(0))
        .jctx("STORE_QUERY", "cannot read Checkpoint provenance")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .jctx("STORE_QUERY", "cannot decode Checkpoint provenance")
}
