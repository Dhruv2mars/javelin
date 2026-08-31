use super::*;

pub(super) fn verify(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let layer = selected_layer(context, store, requested)?;
    let refreshed = refresh_layer(store, &layer)?;
    let tree = store.objects.read_tree(&refreshed.checkpoint.root_tree)?;
    let validations = run_validations(store, &tree, &refreshed.checkpoint.root_tree)?;
    let failed = validations
        .iter()
        .any(|validation| validation.required && validation.exit_code != 0);
    if failed {
        return Err(JavelinError::verification("required World Rule failed")
            .details(json!({"validations": validations})));
    }
    emit(
        json_output,
        &json!({"candidate_root": refreshed.checkpoint.root_tree, "validations": validations}),
        format!("Verified {} World Rules", validations.len()),
    )
}

pub(super) fn run_validations(
    store: &mut Store,
    candidate: &Tree,
    candidate_root: &str,
) -> Result<Vec<ValidationRecord>> {
    let (_, _, accepted_tree) = if let Ok(world) = store.current_world() {
        let tree = store.objects.read_tree(&world.root_tree)?;
        (world.id, world.root_tree, tree)
    } else {
        return Err(JavelinError::corruption("Current World unavailable"));
    };
    let (accepted_config, _) = config_from_tree(store, &accepted_tree)?;
    let (candidate_config, candidate_policy) = config_from_tree(store, candidate)?;
    let policy_hash = blake3::hash(candidate_policy.as_bytes())
        .to_hex()
        .to_string();
    let mut rules = accepted_config.verification.rules;
    for rule in candidate_config.verification.rules {
        if !rules.iter().any(|existing| {
            existing.name == rule.name
                && existing.command == rule.command
                && existing.required == rule.required
                && existing.timeout_seconds == rule.timeout_seconds
        }) {
            rules.push(rule);
        }
    }
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let candidate_dir = tempfile::Builder::new()
        .prefix("candidate-")
        .tempdir_in(store.metadata.join("temp"))
        .jctx("VERIFY_IO", "cannot create isolated candidate view")?;
    materialize_tree_from_cache(
        candidate,
        candidate_root,
        &store.metadata,
        candidate_dir.path(),
        &store.objects,
        None,
    )?;
    let mut records = Vec::new();
    for rule in rules {
        let record = run_rule(
            store,
            candidate_dir.path(),
            candidate_root,
            &policy_hash,
            &rule,
        )?;
        store.record_validation(&record)?;
        records.push(record);
    }
    Ok(records)
}

fn config_from_tree(store: &Store, tree: &Tree) -> Result<(Config, String)> {
    let entry = tree
        .entries
        .iter()
        .find(|entry| entry.path == "javelin.toml" && entry.kind == EntryKind::File)
        .ok_or_else(|| JavelinError::policy("candidate has no javelin.toml"))?;
    let bytes = store
        .objects
        .read_blob(entry.object_id.as_deref().unwrap_or(""))?;
    let text =
        String::from_utf8(bytes).map_err(|_| JavelinError::policy("javelin.toml is not UTF-8"))?;
    Ok((Config::parse(&text)?, text))
}

fn run_rule(
    store: &mut Store,
    candidate_dir: &Path,
    candidate_root: &str,
    policy_hash: &str,
    rule: &WorldRule,
) -> Result<ValidationRecord> {
    crate::fault::hit("during_verification");
    let stdout_path = store
        .metadata
        .join("temp")
        .join(format!("validation-{}-stdout", ulid::Ulid::new()));
    let stderr_path = store
        .metadata
        .join("temp")
        .join(format!("validation-{}-stderr", ulid::Ulid::new()));
    let stdout_file =
        File::create(&stdout_path).jctx("VERIFY_IO", "cannot create validation stdout")?;
    let stderr_file =
        File::create(&stderr_path).jctx("VERIFY_IO", "cannot create validation stderr")?;
    let start = Instant::now();
    let mut child = ProcessCommand::new(&rule.command[0])
        .args(&rule.command[1..])
        .current_dir(candidate_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|error| {
            JavelinError::verification(format!("cannot start World Rule {}: {error}", rule.name))
        })?;
    let timeout = Duration::from_secs(rule.timeout_seconds);
    let status = match child
        .wait_timeout(timeout)
        .jctx("VERIFY_IO", "cannot wait for World Rule")?
    {
        Some(status) => status,
        None => {
            child
                .kill()
                .jctx("VERIFY_IO", "cannot stop timed-out World Rule")?;
            let _ = child.wait();
            let stdout = fs::read(&stdout_path).unwrap_or_default();
            let stderr = fs::read(&stderr_path).unwrap_or_default();
            let stdout_object = (!stdout.is_empty())
                .then(|| store.objects.put_blob(&stdout))
                .transpose()?;
            let stderr_object = (!stderr.is_empty())
                .then(|| store.objects.put_blob(&stderr))
                .transpose()?;
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Ok(ValidationRecord {
                id: ulid::Ulid::new().to_string(),
                rule_name: rule.name.clone(),
                command_json: serde_json::to_string(&rule.command).unwrap(),
                required: rule.required,
                exit_code: 124,
                duration_ms: start.elapsed().as_millis() as i64,
                environment_json: validation_environment(candidate_dir),
                stdout_object,
                stderr_object,
                candidate_root: candidate_root.to_string(),
                policy_hash: policy_hash.to_string(),
                created_at: now(),
            });
        }
    };
    let stdout = fs::read(&stdout_path).unwrap_or_default();
    let stderr = fs::read(&stderr_path).unwrap_or_default();
    let stdout_object = (!stdout.is_empty())
        .then(|| store.objects.put_blob(&stdout))
        .transpose()?;
    let stderr_object = (!stderr.is_empty())
        .then(|| store.objects.put_blob(&stderr))
        .transpose()?;
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    Ok(ValidationRecord {
        id: ulid::Ulid::new().to_string(),
        rule_name: rule.name.clone(),
        command_json: serde_json::to_string(&rule.command).unwrap(),
        required: rule.required,
        exit_code: status.code().unwrap_or(128),
        duration_ms: start.elapsed().as_millis() as i64,
        environment_json: validation_environment(candidate_dir),
        stdout_object,
        stderr_object,
        candidate_root: candidate_root.to_string(),
        policy_hash: policy_hash.to_string(),
        created_at: now(),
    })
}

fn validation_environment(candidate_dir: &Path) -> String {
    json!({
        "os": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "candidate_cwd": candidate_dir,
        "path_configured": std::env::var_os("PATH").is_some(),
    })
    .to_string()
}
