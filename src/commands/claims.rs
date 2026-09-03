use super::*;

pub(super) fn claim(store: &mut Store, command: ClaimCommand, json_output: bool) -> Result<()> {
    match command {
        ClaimCommand::List => {
            let claims = store.claims()?;
            let mut overlaps = Vec::new();
            for (index, left) in claims.iter().enumerate() {
                for right in claims.iter().skip(index + 1) {
                    let left_resource = left["resource"].as_str().unwrap_or("");
                    let right_resource = right["resource"].as_str().unwrap_or("");
                    if claim_overlap(left_resource, right_resource) {
                        overlaps.push(json!({"left": left["id"], "right": right["id"], "resource_left": left_resource, "resource_right": right_resource}));
                    }
                }
            }
            let human = claims
                .iter()
                .map(|claim| {
                    format!(
                        "{}\t{}\t{}",
                        claim["id"].as_str().unwrap_or("unknown"),
                        claim["layer_name"].as_str().unwrap_or("unknown"),
                        claim["resource"].as_str().unwrap_or("unknown")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            emit(
                json_output,
                &json!({"claims": claims, "overlaps": overlaps}),
                human,
            )
        }
        ClaimCommand::Renew { id, seconds } => {
            let expires_at = store.renew_claim(&id, seconds)?;
            emit(
                json_output,
                &json!({"claim_id": id, "expires_at": expires_at}),
                format!("Renewed Claim {id} to {expires_at}"),
            )
        }
        ClaimCommand::Release { id } => {
            store.release_claim(&id)?;
            emit(
                json_output,
                &json!({"claim_id": id, "released": true}),
                format!("Released Claim {id}"),
            )
        }
    }
}

pub(super) fn validate_claim_resource(resource: &str) -> Result<()> {
    if resource == "**" {
        return Ok(());
    }
    let path = resource.strip_suffix("/**").unwrap_or(resource);
    if path.contains(['*', '?', '[', ']']) {
        return Err(JavelinError::invalid(
            "Claim must be an exact path, **, or an exact path ending in /**",
        ));
    }
    crate::paths::validate_relative(path)
}

fn claim_overlap(left: &str, right: &str) -> bool {
    left == right
        || left == "**"
        || right == "**"
        || left
            .strip_suffix("/**")
            .is_some_and(|prefix| right == prefix || right.starts_with(&format!("{prefix}/")))
        || right
            .strip_suffix("/**")
            .is_some_and(|prefix| left == prefix || left.starts_with(&format!("{prefix}/")))
}

pub(super) fn hook(
    context: &ProjectContext,
    store: &mut Store,
    command: HookCommand,
    json_output: bool,
) -> Result<()> {
    let layer = context_layer(context, store)?;
    let (event_type, safe_refresh, session) = match command {
        HookCommand::OperationStart { session } => ("hook.operation-start", false, session),
        HookCommand::OperationEnd { session } => ("hook.operation-end", true, session),
        HookCommand::SessionStart { session } => ("hook.session-start", false, session),
        HookCommand::SessionEnd { session } => ("hook.session-end", false, session),
    };
    let checkpoint = reconcile(store, &layer, event_type)?;
    let refreshed = if safe_refresh {
        Some(refresh_layer(store, &store.layer(&layer.id)?)?)
    } else {
        None
    };
    store.append_event(
        event_type,
        Some("layer"),
        Some(&layer.id),
        &json!({"session_id": session, "checkpoint_id": checkpoint.id, "safe_refresh": safe_refresh}),
    )?;
    emit(
        json_output,
        &json!({"event": event_type, "checkpoint": checkpoint, "refresh_checkpoint": refreshed.map(|value| value.checkpoint.id)}),
        event_type.to_string(),
    )
}
