use crate::cli::*;
use crate::config::{Config, DEFAULT_CONFIG, DEFAULT_IGNORE, IgnorePolicy, WorldRule};
use crate::error::{Context, JavelinError, Result};
use crate::model::{EntryKind, Layer, Tree, TreeEntry, ViewMarker};
use crate::objects::ObjectKind;
use crate::paths::{ProjectContext, discover};
use crate::store::{ConflictInput, NewLayer, Store, ValidationRecord, now};
use crate::view::{
    apply_entries, diff_trees, invalidate_root_cache, materialize_tree_from_cache, scan_view,
    scan_view_with_policy, view_stamp,
};
use clap::CommandFactory;
use fs2::FileExt;
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

pub fn execute(cli: Cli) -> Result<()> {
    let json_output = cli.json;
    match cli.command {
        Command::Init { path } => init(path.or(cli.project), json_output),
        Command::Version => emit(
            json_output,
            &json!({"version": crate::VERSION}),
            format!("javelin {}", crate::VERSION),
        ),
        Command::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "javelin", &mut std::io::stdout());
            Ok(())
        }
        Command::Status { ignored } => with_store(cli.project, |context, mut store| {
            status(context, &mut store, ignored, json_output)
        }),
        Command::Checkpoint { reason } => with_store(cli.project, |context, mut store| {
            checkpoint(
                context,
                &mut store,
                reason.as_deref().unwrap_or("explicit"),
                json_output,
            )
        }),
        Command::Diff { from, to, path } => with_store(cli.project, |context, mut store| {
            diff(
                context,
                &mut store,
                from.as_deref(),
                to.as_deref(),
                path.first().map(String::as_str),
                json_output,
            )
        }),
        Command::History { layer, path } => with_store(cli.project, |_context, store| {
            history(&store, layer.as_deref(), path.as_deref(), json_output)
        }),
        Command::Show { reference } => with_store(cli.project, |_context, store| {
            show(&store, &reference, json_output)
        }),
        Command::World(args) => with_store(cli.project, |context, mut store| {
            world(context, &mut store, args.command, json_output)
        }),
        Command::Layer(args) => with_store(cli.project, |context, mut store| {
            layer(context, &mut store, args.command, json_output)
        }),
        Command::Fsck => with_store(cli.project, |_context, mut store| {
            fsck(&mut store, json_output)
        }),
        Command::Repair { view } => {
            with_store_without_monitor(cli.project, |_context, mut store| {
                repair(&mut store, view.as_deref(), json_output)
            })
        }
        Command::Doctor => with_store(cli.project, |_context, store| doctor(&store, json_output)),
        Command::Refresh { layer } => with_store(cli.project, |context, mut store| {
            crate::commands::refresh(context, &mut store, layer.as_deref(), json_output)
        }),
        Command::Verify { layer } => with_store(cli.project, |context, mut store| {
            crate::commands::verify(context, &mut store, layer.as_deref(), json_output)
        }),
        Command::Publish {
            layer,
            idempotency_key,
        } => with_store(cli.project, |context, mut store| {
            crate::commands::publish(
                context,
                &mut store,
                layer.as_deref(),
                idempotency_key.as_deref(),
                json_output,
            )
        }),
        Command::Conflict(args) => with_store(cli.project, |context, mut store| {
            crate::commands::conflict(context, &mut store, args.command, json_output)
        }),
        Command::Discard {
            layer,
            cascade,
            reparent,
            purge,
        } => with_store(cli.project, |context, mut store| {
            crate::commands::discard(
                context,
                &mut store,
                layer.as_deref(),
                cascade,
                reparent.as_deref(),
                purge,
                json_output,
            )
        }),
        Command::Discarded(args) => with_store(cli.project, |_context, mut store| {
            crate::commands::discarded(&mut store, args.command, json_output)
        }),
        Command::Events {
            since,
            follow,
            jsonl,
        } => with_store(cli.project, |_context, store| {
            events(&store, since, follow, jsonl || json_output)
        }),
        Command::Provenance(args) => with_store(cli.project, |context, mut store| {
            crate::commands::provenance(context, &mut store, args.command, json_output)
        }),
        Command::Explain { path } => with_store(cli.project, |_context, store| {
            explain(&store, &path, json_output)
        }),
        Command::Claim(args) => with_store(cli.project, |_context, mut store| {
            crate::commands::claim(&mut store, args.command, json_output)
        }),
        Command::Hook(args) => with_store(cli.project, |context, mut store| {
            hook(context, &mut store, args.command, json_output)
        }),
        Command::Gc { dry_run } => with_store(cli.project, |_context, mut store| {
            gc(&mut store, dry_run, json_output)
        }),
        Command::Monitor => with_store(cli.project, |_context, mut store| monitor(&mut store)),
    }
}

fn with_store(
    project: Option<PathBuf>,
    action: impl FnOnce(&ProjectContext, Store) -> Result<()>,
) -> Result<()> {
    let context = discover(project.as_deref())?;
    let store = Store::open(&context.root)?;
    start_monitor(&store)?;
    action(&context, store)
}

fn with_store_without_monitor(
    project: Option<PathBuf>,
    action: impl FnOnce(&ProjectContext, Store) -> Result<()>,
) -> Result<()> {
    let context = discover(project.as_deref())?;
    let store = Store::open(&context.root)?;
    action(&context, store)
}

fn emit(json_output: bool, value: &impl Serialize, human: String) -> Result<()> {
    if json_output {
        let value = serde_json::to_value(value)
            .map_err(|error| JavelinError::corruption(format!("cannot encode output: {error}")))?;
        println!(
            "{}",
            json!({"schema_version": 1, "ok": true, "result": value})
        );
    } else {
        println!("{human}");
    }
    Ok(())
}

fn init(path: Option<PathBuf>, json_output: bool) -> Result<()> {
    let requested = path.unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&requested)
        .jctx("INIT_IO", format!("cannot create {}", requested.display()))?;
    let root = requested
        .canonicalize()
        .jctx("INIT_IO", format!("cannot resolve {}", requested.display()))?;
    if root.join(".javelin/store.sqlite3").exists() {
        let store = Store::open(&root)?;
        let world = store.current_world()?;
        return emit(
            json_output,
            &json!({"world_version": world.id, "root_tree": world.root_tree, "path": root}),
            format!(
                "World already initialized at {} ({})",
                root.display(),
                world.id
            ),
        );
    }
    create_policy_file(&root.join("javelin.toml"), DEFAULT_CONFIG)?;
    create_policy_file(&root.join(".javelinignore"), DEFAULT_IGNORE)?;
    let mut store = Store::create(&root)?;
    let scan = scan_view(&root, &store.objects)?;
    let root_tree = store.objects.put_tree(&scan.tree)?;
    store.register_object(
        &root_tree,
        ObjectKind::Tree,
        crate::objects::encode_tree(&scan.tree)?.len() as u64,
    )?;
    let (world, local) = store.initialize_world(&root_tree)?;
    let stamp = view_stamp(&root)?;
    let head = store.layer_head(&local)?;
    write_view_observation(&store, &local, &head, &stamp)?;
    start_monitor(&store)?;
    emit(
        json_output,
        &json!({"world_version": world.id, "root_tree": root_tree, "path": root}),
        format!("Initialized World {} at {}", world.id, root.display()),
    )
}

fn create_policy_file(path: &Path, contents: &str) -> Result<()> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => file
            .write_all(contents.as_bytes())
            .and_then(|_| file.sync_all())
            .jctx("INIT_IO", format!("cannot write {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(
                JavelinError::new(7, "INIT_IO", format!("cannot create {}", path.display()))
                    .details(json!({"cause": error.to_string()})),
            )
        }
    }
}

fn reconcile(store: &mut Store, layer: &Layer, reason: &str) -> Result<crate::model::Checkpoint> {
    if layer.status == "discarded" {
        return Err(JavelinError::policy(format!(
            "Private Layer {} is discarded",
            layer.name
        )));
    }
    let view = Path::new(&layer.view_path);
    let stamp = view_stamp(view)?;
    if let Some(checkpoint) = observed_checkpoint(store, layer, &stamp)? {
        return Ok(checkpoint);
    }

    let reconcile_lock = open_reconcile_lock(store)?;
    acquire_publish_lock(&reconcile_lock)?;
    let result = (|| {
        let current = store.layer(&layer.id)?;
        let stamp = view_stamp(view)?;
        if let Some(checkpoint) = observed_checkpoint(store, &current, &stamp)? {
            return Ok(checkpoint);
        }
        let policy = tracking_policy(store, &current)?;
        let scan = scan_view_with_policy(view, &store.objects, &policy)?;
        let stable_stamp = view_stamp(view)?;
        if stable_stamp != stamp {
            return Err(JavelinError::busy(
                "Managed view changed during reconciliation; retry the command",
            ));
        }
        let root_tree = store.objects.put_tree(&scan.tree)?;
        store.register_object(
            &root_tree,
            ObjectKind::Tree,
            crate::objects::encode_tree(&scan.tree)?.len() as u64,
        )?;
        let checkpoint =
            store.append_checkpoint(&current.id, &root_tree, &current.synchronized_ref, reason)?;
        write_view_observation(store, &current, &checkpoint, &stable_stamp)?;
        Ok(checkpoint)
    })();
    let unlock = FileExt::unlock(&reconcile_lock)
        .jctx("RECONCILE_LOCK", "cannot release reconciliation lock");
    match (result, unlock) {
        (Ok(checkpoint), Ok(())) => Ok(checkpoint),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn observed_checkpoint(
    store: &Store,
    layer: &Layer,
    stamp: &str,
) -> Result<Option<crate::model::Checkpoint>> {
    let path = view_observation_path(store, layer);
    let Ok(bytes) = fs::read(path) else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
    if value.get("schema").and_then(serde_json::Value::as_u64) != Some(1)
        || value.get("layer_id").and_then(serde_json::Value::as_str) != Some(layer.id.as_str())
        || value.get("stamp").and_then(serde_json::Value::as_str) != Some(stamp)
    {
        return Ok(None);
    }
    let head = store.layer_head(layer)?;
    Ok((value
        .get("checkpoint_id")
        .and_then(serde_json::Value::as_str)
        == Some(head.id.as_str()))
    .then_some(head))
}

fn write_view_observation(
    store: &Store,
    layer: &Layer,
    checkpoint: &crate::model::Checkpoint,
    stamp: &str,
) -> Result<()> {
    write_monitor_state(
        &view_observation_path(store, layer),
        &json!({
            "schema": 1,
            "layer_id": layer.id,
            "checkpoint_id": checkpoint.id,
            "stamp": stamp,
        })
        .to_string(),
    )
}

fn view_observation_path(store: &Store, layer: &Layer) -> PathBuf {
    store
        .metadata
        .join("monitor")
        .join(format!("view-{}.json", layer.id))
}

fn tracking_policy(store: &Store, layer: &Layer) -> Result<IgnorePolicy> {
    let (_, _, synchronized) = store.tree_for_ref(&layer.synchronized_ref)?;
    let text = if let Some(entry) = synchronized
        .entries
        .iter()
        .find(|entry| entry.path == ".javelinignore" && entry.kind == EntryKind::File)
    {
        let bytes = store
            .objects
            .read_blob(entry.object_id.as_deref().unwrap_or(""))?;
        String::from_utf8(bytes).map_err(|_| JavelinError::policy(".javelinignore is not UTF-8"))?
    } else {
        DEFAULT_IGNORE.to_string()
    };
    IgnorePolicy::parse(&text)
}

fn open_reconcile_lock(store: &Store) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.metadata.join("locks/reconcile.lock"))
        .jctx("RECONCILE_LOCK", "cannot open reconciliation lock")
}

fn context_layer(context: &ProjectContext, store: &Store) -> Result<Layer> {
    store.layer(&context.layer_id)
}

fn checkpoint(
    context: &ProjectContext,
    store: &mut Store,
    reason: &str,
    json_output: bool,
) -> Result<()> {
    let layer = context_layer(context, store)?;
    let before = layer.head_checkpoint.clone();
    let checkpoint = reconcile(store, &layer, reason)?;
    emit(
        json_output,
        &json!({"checkpoint": checkpoint, "created": checkpoint.id != before}),
        format!("Layer Checkpoint {} ({})", checkpoint.id, checkpoint.reason),
    )
}

fn status(
    context: &ProjectContext,
    store: &mut Store,
    include_ignored: bool,
    json_output: bool,
) -> Result<()> {
    let layer = context_layer(context, store)?;
    let checkpoint = reconcile(store, &layer, "command-reconcile")?;
    let (_, base_root, base) = store.tree_for_ref(&checkpoint.synchronized_ref)?;
    let current = store.objects.read_tree(&checkpoint.root_tree)?;
    let changes = diff_trees(&base, &current);
    let ignored = if include_ignored {
        let policy = tracking_policy(store, &layer)?;
        scan_view_with_policy(Path::new(&layer.view_path), &store.objects, &policy)?.ignored
    } else {
        Vec::new()
    };
    let human = if changes.is_empty() {
        format!("{} clean at {}", layer.name, checkpoint.id)
    } else {
        changes
            .iter()
            .map(|change| format!("{:?}\t{}", change.change, change.path))
            .collect::<Vec<_>>()
            .join("\n")
    };
    emit(
        json_output,
        &json!({
            "layer": layer,
            "checkpoint": checkpoint.id,
            "synchronized_root": base_root,
            "private_root": checkpoint.root_tree,
            "changes": changes,
            "ignored": ignored,
        }),
        human,
    )
}

fn diff(
    context: &ProjectContext,
    store: &mut Store,
    from: Option<&str>,
    to: Option<&str>,
    path_filter: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let layer = context_layer(context, store)?;
    let head = reconcile(store, &layer, "command-reconcile")?;
    let from = from.unwrap_or(&head.synchronized_ref);
    let to = to.unwrap_or(&head.id);
    let (_, from_root, from_tree) = store.tree_for_ref(from)?;
    let (_, to_root, to_tree) = store.tree_for_ref(to)?;
    let mut changes = diff_trees(&from_tree, &to_tree);
    if let Some(filter) = path_filter {
        changes.retain(|change| {
            change.path == filter || change.path.starts_with(&format!("{filter}/"))
        });
    }
    let renames = detected_renames(&changes);
    let details = changes
        .iter()
        .map(|change| render_change(store, change, &renames))
        .collect::<Result<Vec<_>>>()?;
    let human = details
        .iter()
        .map(|detail| {
            detail["patch"].as_str().map_or_else(
                || {
                    format!(
                        "{}\t{}",
                        detail["change"].as_str().unwrap_or("change"),
                        detail["path"].as_str().unwrap_or("unknown")
                    )
                },
                |patch| {
                    format!(
                        "{}\n{}",
                        detail["path"].as_str().unwrap_or("unknown"),
                        patch
                    )
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    emit(
        json_output,
        &json!({"from": from, "from_root": from_root, "to": to, "to_root": to_root, "changes": details}),
        human,
    )
}

fn detected_renames(changes: &[crate::model::Change]) -> BTreeMap<String, String> {
    let mut deleted = BTreeMap::new();
    for change in changes {
        if change.new.is_none()
            && let Some(object_id) = change
                .old
                .as_ref()
                .and_then(|entry| entry.object_id.as_ref())
        {
            deleted.insert(object_id.clone(), change.path.clone());
        }
    }
    let mut renames = BTreeMap::new();
    for change in changes {
        if change.old.is_none()
            && let Some(object_id) = change
                .new
                .as_ref()
                .and_then(|entry| entry.object_id.as_ref())
            && let Some(previous) = deleted.get(object_id)
        {
            renames.insert(change.path.clone(), previous.clone());
        }
    }
    renames
}

fn render_change(
    store: &Store,
    change: &crate::model::Change,
    renames: &BTreeMap<String, String>,
) -> Result<serde_json::Value> {
    let old = entry_text(store, change.old.as_ref())?;
    let new = entry_text(store, change.new.as_ref())?;
    let binary = matches!((&old, &new), (Some(Err(_)), _) | (_, Some(Err(_))));
    let patch = match (old, new) {
        (Some(Ok(old)), Some(Ok(new))) => Some(diffy::create_patch(&old, &new).to_string()),
        (None, Some(Ok(new))) => Some(diffy::create_patch("", &new).to_string()),
        (Some(Ok(old)), None) => Some(diffy::create_patch(&old, "").to_string()),
        _ => None,
    };
    Ok(json!({
        "path": change.path,
        "change": format!("{:?}", change.change).to_lowercase(),
        "old": change.old,
        "new": change.new,
        "binary": binary,
        "patch": patch,
        "rename_from": renames.get(&change.path),
    }))
}

fn entry_text(
    store: &Store,
    entry: Option<&TreeEntry>,
) -> Result<Option<std::result::Result<String, ()>>> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.kind != EntryKind::File {
        return Ok(Some(Err(())));
    }
    let object_id = entry.object_id.as_deref().unwrap_or("");
    let (kind, length) = store.objects.info(object_id)?;
    if kind != ObjectKind::Blob || length > 1024 * 1024 {
        return Ok(Some(Err(())));
    }
    let bytes = store.objects.read_blob(object_id)?;
    if bytes.contains(&0) {
        return Ok(Some(Err(())));
    }
    Ok(Some(String::from_utf8(bytes).map_err(|_| ())))
}

fn history(
    store: &Store,
    layer: Option<&str>,
    path: Option<&str>,
    json_output: bool,
) -> Result<()> {
    if let Some(layer) = layer {
        let layer = store.layer(layer)?;
        let mut history = store.checkpoint_history(&layer.id)?;
        if let Some(path) = path {
            history = filter_checkpoint_history(store, history, path)?;
        }
        let human = history
            .iter()
            .map(|checkpoint| {
                format!(
                    "{}\t{}\t{}",
                    checkpoint.id, checkpoint.reason, checkpoint.root_tree
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        emit(
            json_output,
            &json!({"layer": layer, "checkpoints": history}),
            human,
        )
    } else {
        let mut history = store.world_history()?;
        if let Some(path) = path {
            let mut filtered = Vec::new();
            for version in &history {
                let current = store.objects.read_tree(&version.root_tree)?;
                let changed = if let Some(parent) = &version.parent_version {
                    let parent = store.world_version(parent)?;
                    diff_trees(&store.objects.read_tree(&parent.root_tree)?, &current)
                        .iter()
                        .any(|change| change.path == path)
                } else {
                    current.entries.iter().any(|entry| entry.path == path)
                };
                if changed {
                    filtered.push(version.clone());
                }
            }
            history = filtered;
        }
        let human = history
            .iter()
            .map(|version| format!("{}\t{}", version.id, version.root_tree))
            .collect::<Vec<_>>()
            .join("\n");
        emit(json_output, &json!({"versions": history}), human)
    }
}

fn filter_checkpoint_history(
    store: &Store,
    history: Vec<crate::model::Checkpoint>,
    path: &str,
) -> Result<Vec<crate::model::Checkpoint>> {
    let mut filtered = Vec::new();
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
            filtered.push(checkpoint.clone());
        }
        previous = Some(tree);
    }
    Ok(filtered)
}

fn show(store: &Store, reference: &str, json_output: bool) -> Result<()> {
    let (state_ref, path) = reference.split_once(':').unwrap_or((reference, ""));
    let (resolved, root, tree) = store.tree_for_ref(state_ref)?;
    if path.is_empty() {
        let human = tree
            .entries
            .iter()
            .map(|entry| format!("{:?}\t{}", entry.kind, entry.path))
            .collect::<Vec<_>>()
            .join("\n");
        return emit(
            json_output,
            &json!({"reference": resolved, "root_tree": root, "tree": tree}),
            human,
        );
    }
    let entry = tree
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| JavelinError::invalid(format!("path {path} not found in {state_ref}")))?;
    if entry.kind == EntryKind::Directory {
        return emit(json_output, entry, format!("directory\t{path}"));
    }
    let object_id = entry
        .object_id
        .as_deref()
        .ok_or_else(|| JavelinError::corruption("path state has no object"))?;
    if json_output {
        let (_, size) = store.objects.info(object_id)?;
        let bytes_hex = if size <= 1024 * 1024 {
            Some(hex(&store.objects.read_blob(object_id)?))
        } else {
            None
        };
        emit(
            true,
            &json!({
                "reference": resolved,
                "path": path,
                "entry": entry,
                "size": size,
                "bytes_hex": bytes_hex,
                "content_omitted": bytes_hex.is_none(),
            }),
            String::new(),
        )
    } else {
        let _ = store.objects.validate(object_id)?;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        store.objects.write_blob_to_writer(object_id, &mut lock)?;
        lock.flush().jctx("OUTPUT_IO", "cannot flush path bytes")
    }
}

fn world(
    context: &ProjectContext,
    store: &mut Store,
    command: WorldCommand,
    json_output: bool,
) -> Result<()> {
    let layer = context_layer(context, store)?;
    let _ = reconcile(store, &layer, "command-reconcile")?;
    match command {
        WorldCommand::Current => {
            let world = store.current_world()?;
            emit(
                json_output,
                &world,
                format!("{}\t{}", world.id, world.root_tree),
            )
        }
        WorldCommand::History => history(store, None, None, json_output),
        WorldCommand::Restore {
            version,
            accept_failing,
            reason,
        } => restore_world(
            store,
            &version,
            accept_failing,
            reason.as_deref(),
            json_output,
        ),
    }
}

fn layer(
    context: &ProjectContext,
    store: &mut Store,
    command: LayerCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        LayerCommand::Create {
            name,
            from,
            target,
            claim,
        } => {
            let context_layer = context_layer(context, store)?;
            let current_head = reconcile(store, &context_layer, "command-reconcile")?;
            let (target_kind, target_id, default_ref) = if target == "world" {
                let world = store.current_world()?;
                ("world", None, world.id)
            } else if let Some(parent_name) = target.strip_prefix("layer:") {
                let parent = store.layer(parent_name)?;
                let head = store.layer_head(&parent)?;
                ("layer", Some(parent.id), head.id)
            } else {
                return Err(JavelinError::invalid(
                    "Layer target must be world or layer:LAYER",
                ));
            };
            let explicit_from = from.is_some();
            let origin_ref = from.unwrap_or_else(|| {
                if context_layer.id == "local" && target_kind == "world" {
                    current_head.id.clone()
                } else {
                    default_ref.clone()
                }
            });
            let (resolved, root_tree) = store.resolve_ref(&origin_ref)?;
            let synchronized_ref = if explicit_from {
                resolved.clone()
            } else {
                default_ref.clone()
            };
            let tree = store.objects.read_tree(&root_tree)?;
            let view_path = store.metadata.join("views").join(&name);
            let created = store.create_layer(NewLayer {
                name: &name,
                origin_ref: &resolved,
                synchronized_ref: &synchronized_ref,
                root_tree: &root_tree,
                target_kind,
                target_id: target_id.as_deref(),
                view_path: &view_path,
            })?;
            let marker = ViewMarker {
                format: 1,
                project: store.root.to_string_lossy().into_owned(),
                layer_id: created.id.clone(),
            };
            let backend = materialize_tree_from_cache(
                &tree,
                &root_tree,
                &store.metadata,
                &view_path,
                &store.objects,
                Some(&marker),
            )?;
            store.mark_view(&created.id, &created.head_checkpoint, false, backend)?;
            for resource in claim {
                create_claim(store, &created.id, &resource, 3600)?;
            }
            emit(
                json_output,
                &json!({"layer": store.layer(&created.id)?, "path": view_path, "backend": backend}),
                view_path.to_string_lossy().into_owned(),
            )
        }
        LayerCommand::List => {
            let layers = store.layers(false)?;
            let human = layers
                .iter()
                .map(|layer| format!("{}\t{}\t{}", layer.name, layer.status, layer.view_path))
                .collect::<Vec<_>>()
                .join("\n");
            emit(json_output, &json!({"layers": layers}), human)
        }
        LayerCommand::Show { layer } => {
            let layer = store.layer(&layer)?;
            let head = store.layer_head(&layer)?;
            emit(
                json_output,
                &json!({"layer": layer, "head": head}),
                format!("{}\t{}", layer.name, head.id),
            )
        }
        LayerCommand::Path { layer } => {
            let layer = store.layer(&layer)?;
            emit(
                json_output,
                &json!({"layer_id": layer.id, "path": layer.view_path}),
                layer.view_path,
            )
        }
        LayerCommand::Restore { checkpoint, layer } => {
            let selected = store.checkpoint(&checkpoint)?;
            let layer = if let Some(layer) = layer {
                store.layer(&layer)?
            } else {
                context_layer(context, store)?
            };
            let tree = store.objects.read_tree(&selected.root_tree)?;
            let appended = store.append_checkpoint(
                &layer.id,
                &selected.root_tree,
                &layer.synchronized_ref,
                "restore",
            )?;
            store.mark_view(&layer.id, &appended.id, true, "materializing")?;
            let marker = (layer.id != "local").then(|| ViewMarker {
                format: 1,
                project: store.root.to_string_lossy().into_owned(),
                layer_id: layer.id.clone(),
            });
            let backend = materialize_tree_from_cache(
                &tree,
                &selected.root_tree,
                &store.metadata,
                Path::new(&layer.view_path),
                &store.objects,
                marker.as_ref(),
            )?;
            store.mark_view(&layer.id, &appended.id, false, backend)?;
            emit(
                json_output,
                &appended,
                format!("Restored Layer {} through {}", layer.name, appended.id),
            )
        }
    }
}

fn restore_world(
    store: &mut Store,
    version: &str,
    accept_failing: bool,
    reason: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let selected = store.world_version(version)?;
    if accept_failing && reason.is_none() {
        return Err(JavelinError::invalid("--accept-failing requires --reason"));
    }
    let candidate = store.objects.read_tree(&selected.root_tree)?;
    let validations = run_validations(store, &candidate, &selected.root_tree)?;
    let failed = validations
        .iter()
        .any(|validation| validation.required && validation.exit_code != 0);
    if failed && !accept_failing {
        return Err(
            JavelinError::verification("required World Rule rejected restore")
                .details(json!({"validations": validations}))
                .recovery([format!(
                    "javelin world restore {version} --accept-failing --reason TEXT"
                )]),
        );
    }
    let current = store.current_world()?;
    let restored = store.append_world_version(
        &selected.root_tree,
        None,
        "world.restored",
        &json!({"restored_from": version, "previous": current.id, "accept_failing": accept_failing, "reason": reason}),
    )?;
    let validation_ids = validations
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    store.link_version_validations(&restored.id, &validation_ids)?;
    emit(
        json_output,
        &json!({"world_version": restored, "validations": validations, "policy_override": failed && accept_failing}),
        format!("Restored {} as {}", version, restored.id),
    )
}

fn fsck(store: &mut Store, json_output: bool) -> Result<()> {
    let integrity: String = store
        .conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .jctx("STORE_QUERY", "cannot run SQLite integrity check")?;
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
        .jctx("STORE_QUERY", "cannot run SQLite foreign-key check")?;
    if let Some((table, rowid)) = foreign_key_failure {
        return Err(JavelinError::corruption(format!(
            "foreign-key violation in {table} row {rowid}"
        )));
    }
    let mut roots = BTreeSet::new();
    let mut blob_references = BTreeSet::new();
    for version in store.world_history()? {
        roots.insert(version.root_tree);
    }
    for layer in store.layers(true)? {
        for checkpoint in store.checkpoint_history(&layer.id)? {
            roots.insert(checkpoint.root_tree);
        }
    }
    for root in &roots {
        let (kind, _) = store.objects.validate(root)?;
        if kind != ObjectKind::Tree {
            return Err(JavelinError::corruption(format!(
                "root reference {root} does not identify a tree"
            )));
        }
        let tree = store.objects.read_tree(root)?;
        for entry in tree.entries {
            match entry.kind {
                EntryKind::Directory if entry.object_id.is_some() || entry.executable => {
                    return Err(JavelinError::corruption(format!(
                        "directory {} has invalid portable metadata",
                        entry.path
                    )));
                }
                EntryKind::File | EntryKind::Symlink => {
                    let object_id = entry.object_id.ok_or_else(|| {
                        JavelinError::corruption(format!(
                            "tracked path {} has no blob reference",
                            entry.path
                        ))
                    })?;
                    blob_references.insert(object_id);
                }
                EntryKind::Directory => {}
            }
        }
    }
    for query in [
        "SELECT stdout_object FROM validation_runs WHERE stdout_object IS NOT NULL",
        "SELECT stderr_object FROM validation_runs WHERE stderr_object IS NOT NULL",
        "SELECT object_id FROM provenance_attachments WHERE object_id IS NOT NULL",
    ] {
        let mut statement = store
            .conn
            .prepare(query)
            .jctx("STORE_QUERY", "cannot prepare referenced-object check")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .jctx("STORE_QUERY", "cannot read referenced objects")?;
        for row in rows {
            blob_references.insert(row.jctx("STORE_QUERY", "cannot decode object reference")?);
        }
    }
    for conflict in store.conflicts(None, true)? {
        for entry in [
            conflict.base_entry,
            conflict.target_entry,
            conflict.private_entry,
        ]
        .into_iter()
        .flatten()
        {
            if let Some(object_id) = entry.object_id {
                blob_references.insert(object_id);
            }
        }
    }
    for id in &blob_references {
        let (kind, size) = store.objects.validate(id)?;
        if kind != ObjectKind::Blob {
            return Err(JavelinError::corruption(format!(
                "blob reference {id} identifies a tree"
            )));
        }
        store.register_object(id, kind, size)?;
    }
    let all_objects = store.objects.all_ids()?;
    for id in &all_objects {
        let _ = store.objects.validate(id)?;
    }
    let mut statement = store
        .conn
        .prepare("SELECT id, kind, uncompressed_size FROM object_metadata")
        .jctx("STORE_QUERY", "cannot prepare object metadata check")?;
    let metadata = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .jctx("STORE_QUERY", "cannot read object metadata")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .jctx("STORE_QUERY", "cannot decode object metadata")?;
    for (id, expected_kind, expected_size) in metadata {
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
            "root_references": roots.len(),
            "blob_references": blob_references.len(),
            "sqlite_integrity": "ok",
            "foreign_keys": "ok",
        }),
        format!("Store valid: {checked} object references checked"),
    )
}

fn repair(store: &mut Store, requested: Option<&str>, json_output: bool) -> Result<()> {
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
        let backend = materialize_tree_from_cache(
            &tree,
            &head.root_tree,
            &store.metadata,
            Path::new(&layer.view_path),
            &store.objects,
            marker.as_ref(),
        )?;
        store.mark_view(&layer.id, &head.id, false, backend)?;
        FileExt::unlock(&reconcile_lock).jctx("RECONCILE_LOCK", "cannot release repair lock")?;
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

fn doctor(store: &Store, json_output: bool) -> Result<()> {
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

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0xf) as usize] as char);
    }
    output
}

#[derive(Debug)]
struct RefreshResult {
    layer: Layer,
    checkpoint: crate::model::Checkpoint,
    target_ref: String,
    target_root: String,
    changed: bool,
}

fn refresh(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let layer = requested
        .map(|name| store.layer(name))
        .transpose()?
        .unwrap_or(context_layer(context, store)?);
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

fn refresh_layer(store: &mut Store, original: &Layer) -> Result<RefreshResult> {
    if original.status == "discarded" {
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

fn integrate_trees(
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
            let conflict_type = conflict_type(
                base_entry.as_ref(),
                target_entry.as_ref(),
                private_entry.as_ref(),
            );
            conflicts.push((
                path.clone(),
                conflict_type,
                base_entry,
                target_entry,
                private_entry,
            ));
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
        conflicts.push((
            path.clone(),
            "case".to_string(),
            base_map.get(&path).cloned(),
            target_map.get(&path).cloned(),
            private_map.get(&path).cloned(),
        ));
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
        if kind != ObjectKind::Blob || length > 1024 * 1024 {
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

fn conflict_type(
    base: Option<&TreeEntry>,
    target: Option<&TreeEntry>,
    private: Option<&TreeEntry>,
) -> String {
    match (base, target, private) {
        (None, Some(_), Some(_)) => "create_create",
        (Some(_), None, Some(_)) => "delete_modify",
        (Some(_), Some(_), None) => "modify_delete",
        (Some(base), Some(target), Some(private))
            if base.kind != target.kind || base.kind != private.kind =>
        {
            "type"
        }
        (Some(_), Some(target), Some(private)) if target.executable != private.executable => "mode",
        (Some(_), Some(_), Some(_)) => "modify_modify",
        _ => "path_state",
    }
    .to_string()
}

fn target_state(store: &Store, layer: &Layer) -> Result<(String, String, Tree)> {
    if layer.target_kind == "world" {
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

fn verify(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let layer = requested
        .map(|name| store.layer(name))
        .transpose()?
        .unwrap_or(context_layer(context, store)?);
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

fn run_validations(
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

fn publish(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    key: Option<&str>,
    json_output: bool,
) -> Result<()> {
    if let Some(key) = key
        && let Some((contribution, resulting_ref, layer_id, source_checkpoint)) =
            store.contribution_details_by_key(key)?
    {
        let layer = store.layer(&layer_id)?;
        let source = store.checkpoint(&source_checkpoint)?;
        let recovered = store.append_checkpoint(
            &layer.id,
            &source.root_tree,
            &resulting_ref,
            &format!("recover Publish Contribution {contribution}"),
        )?;
        return emit(
            json_output,
            &json!({
                "idempotent": true,
                "contribution_id": contribution,
                "resulting_target_ref": resulting_ref,
                "source_checkpoint": recovered,
            }),
            format!("Contribution {contribution} already accepted as {resulting_ref}"),
        );
    }
    let layer = requested
        .map(|name| store.layer(name))
        .transpose()?
        .unwrap_or(context_layer(context, store)?);
    crate::fault::hit("before_publish_lease");
    let lock_name = if layer.target_kind == "world" {
        "world".to_string()
    } else {
        format!("layer-{}", layer.target_id.as_deref().unwrap_or("missing"))
    };
    let lock_path = store
        .metadata
        .join("locks")
        .join(format!("{lock_name}.publish.lock"));
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .jctx("PUBLISH_LOCK", "cannot open Publish lease")?;
    acquire_queued_publish_lock(store, &lock_name, &lock_file)?;
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
    if refreshed.layer.target_kind == "layer"
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
    FileExt::unlock(&lock_file).jctx("PUBLISH_LOCK", "cannot release Publish lease")?;
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

fn acquire_publish_lock(file: &File) -> Result<()> {
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
        .jctx("PUBLISH_QUEUE", "cannot enter Publish queue")?;
    let started = Instant::now();
    loop {
        let head: Option<(String, u32)> = store
            .conn
            .query_row(
                "SELECT request_id, pid FROM publish_queue WHERE target = ?1 ORDER BY ticket LIMIT 1",
                [target],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .jctx("PUBLISH_QUEUE", "cannot inspect Publish queue")?;
        match head {
            Some((head_id, _head_pid)) if head_id == request_id => {
                if file.try_lock_exclusive().is_ok() {
                    store
                        .conn
                        .execute(
                            "DELETE FROM publish_queue WHERE request_id = ?1",
                            [&request_id],
                        )
                        .jctx("PUBLISH_QUEUE", "cannot leave Publish queue")?;
                    return Ok(());
                }
            }
            Some((head_id, head_pid)) if !process_alive(head_pid) => {
                store
                    .conn
                    .execute(
                        "DELETE FROM publish_queue WHERE request_id = ?1",
                        [&head_id],
                    )
                    .jctx("PUBLISH_QUEUE", "cannot remove abandoned Publish request")?;
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

fn conflict(
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

fn discard(
    context: &ProjectContext,
    store: &mut Store,
    requested: Option<&str>,
    cascade: bool,
    reparent: Option<&str>,
    purge: bool,
    json_output: bool,
) -> Result<()> {
    let layer = requested
        .map(|name| store.layer(name))
        .transpose()?
        .unwrap_or(context_layer(context, store)?);
    if layer.id == "local" {
        let world = store.current_world()?;
        let tree = store.objects.read_tree(&world.root_tree)?;
        let checkpoint =
            store.append_checkpoint(&layer.id, &world.root_tree, &world.id, "discard")?;
        store.mark_view(&layer.id, &checkpoint.id, true, "materializing")?;
        materialize_tree_from_cache(
            &tree,
            &world.root_tree,
            &store.metadata,
            &store.root,
            &store.objects,
            None,
        )?;
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
        if child.status != "discarded" {
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
        current = if layer.target_kind == "layer" {
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
    }
    store.record_discard(&layer.id, &purge_after)
}

fn discarded(store: &mut Store, command: DiscardedCommand, json_output: bool) -> Result<()> {
    match command {
        DiscardedCommand::List => {
            let layers = store
                .layers(true)?
                .into_iter()
                .filter(|layer| layer.status == "discarded")
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
            if layer.status != "discarded" {
                return Err(JavelinError::invalid("Private Layer is not discarded"));
            }
            let trash = store.metadata.join("trash").join(&layer.id);
            let view = PathBuf::from(&layer.view_path);
            if trash.exists() {
                if let Some(parent) = view.parent() {
                    fs::create_dir_all(parent).jctx("DISCARD_IO", "cannot create view parent")?;
                }
                fs::rename(&trash, &view).jctx("DISCARD_IO", "cannot recover retained view")?;
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
fn provenance(
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

fn explain(store: &Store, path: &str, json_output: bool) -> Result<()> {
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

fn claim(store: &mut Store, command: ClaimCommand, json_output: bool) -> Result<()> {
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
                        claim["id"], claim["layer_name"], claim["resource"]
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

fn claim_overlap(left: &str, right: &str) -> bool {
    left == right
        || left == "**"
        || right == "**"
        || left
            .strip_suffix("/**")
            .is_some_and(|prefix| right.starts_with(prefix))
        || right
            .strip_suffix("/**")
            .is_some_and(|prefix| left.starts_with(prefix))
}

fn hook(
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

fn gc(store: &mut Store, dry_run: bool, json_output: bool) -> Result<()> {
    let config = Config::load(&store.root)?;
    let current_time = now();
    let trace_cutoff = (chrono::Utc::now()
        - chrono::Duration::days(config.retention.raw_trace_days as i64))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let expired_provenance = store.expired_provenance_sessions(&trace_cutoff)?;
    let mut expired_discarded = store.expired_discarded_layers(&current_time)?;
    let mut claims_expired = 0;
    let mut discarded_purged = Vec::new();
    if !dry_run {
        claims_expired = store.expire_claims(&current_time)?;
        for session in &expired_provenance {
            store.purge_provenance(session)?;
        }
        while !expired_discarded.is_empty() {
            let current_batch = std::mem::take(&mut expired_discarded);
            let mut remaining = Vec::new();
            let mut progressed = false;
            for layer_id in current_batch {
                if store.all_children(&layer_id)?.is_empty() {
                    let trash = store.metadata.join("trash").join(&layer_id);
                    store.purge_layer(&layer_id)?;
                    if trash.exists() {
                        fs::remove_dir_all(&trash)
                            .jctx("DISCARD_IO", "cannot purge expired retained view")?;
                    }
                    discarded_purged.push(layer_id);
                    progressed = true;
                } else {
                    remaining.push(layer_id);
                }
            }
            if !progressed {
                break;
            }
            expired_discarded = remaining;
        }
    }
    let mut reachable = BTreeSet::new();
    for version in store.world_history()? {
        reachable.insert(version.root_tree);
    }
    for layer in store.layers(true)? {
        for checkpoint in store.checkpoint_history(&layer.id)? {
            reachable.insert(checkpoint.root_tree);
        }
    }
    let roots = reachable.clone();
    for root in roots {
        let tree = store.objects.read_tree(&root)?;
        for entry in tree.entries {
            if let Some(object_id) = entry.object_id {
                reachable.insert(object_id);
            }
        }
    }
    for table_query in [
        "SELECT stdout_object FROM validation_runs WHERE stdout_object IS NOT NULL",
        "SELECT stderr_object FROM validation_runs WHERE stderr_object IS NOT NULL",
        "SELECT object_id FROM provenance_attachments WHERE object_id IS NOT NULL AND purged = 0",
    ] {
        let mut statement = store
            .conn
            .prepare(table_query)
            .jctx("STORE_QUERY", "cannot prepare GC reachability query")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .jctx("STORE_QUERY", "cannot read GC reachability")?;
        for row in rows {
            reachable.insert(row.jctx("STORE_QUERY", "cannot decode GC object ID")?);
        }
    }
    for conflict in store.conflicts(None, true)? {
        for entry in [
            conflict.base_entry,
            conflict.target_entry,
            conflict.private_entry,
        ]
        .into_iter()
        .flatten()
        {
            if let Some(object_id) = entry.object_id {
                reachable.insert(object_id);
            }
        }
    }
    let all = store.objects.all_ids()?;
    let unreachable = all
        .into_iter()
        .filter(|id| !reachable.contains(id))
        .collect::<Vec<_>>();
    if !dry_run {
        for id in &unreachable {
            store.objects.remove(id)?;
            store
                .conn
                .execute("DELETE FROM object_metadata WHERE id = ?1", [id])
                .jctx("STORE_WRITE", "cannot remove GC metadata")?;
        }
        store.append_event(
            "gc.completed",
            Some("world"),
            None,
            &json!({
                "objects_removed": unreachable.len(),
                "discarded_layers_purged": discarded_purged,
                "provenance_sessions_purged": expired_provenance,
                "claims_expired": claims_expired,
            }),
        )?;
    }
    emit(
        json_output,
        &json!({
            "dry_run": dry_run,
            "reachable": reachable.len(),
            "unreachable": unreachable,
            "expired_discarded_layers": if dry_run { expired_discarded } else { discarded_purged.clone() },
            "expired_provenance_sessions": expired_provenance,
            "claims_expired": claims_expired,
        }),
        format!(
            "{} unreachable objects{}",
            unreachable.len(),
            if dry_run { " (dry run)" } else { " removed" }
        ),
    )
}
fn events(store: &Store, since: i64, follow: bool, jsonl: bool) -> Result<()> {
    let events = store.events_since(since)?;
    if jsonl {
        let mut cursor = since;
        for event in &events {
            println!("{event}");
            std::io::stdout()
                .flush()
                .jctx("OUTPUT_IO", "cannot flush event stream")?;
            cursor = event["cursor"].as_i64().unwrap_or(cursor);
        }
        if follow {
            loop {
                thread::sleep(Duration::from_millis(250));
                for event in store.events_since(cursor)? {
                    println!("{event}");
                    std::io::stdout()
                        .flush()
                        .jctx("OUTPUT_IO", "cannot flush event stream")?;
                    cursor = event["cursor"].as_i64().unwrap_or(cursor);
                }
            }
        }
        Ok(())
    } else {
        if follow {
            return Err(JavelinError::invalid("--follow requires --jsonl or --json"));
        }
        emit(
            false,
            &events,
            events
                .iter()
                .map(|event| {
                    format!(
                        "{}\t{}",
                        event["cursor"],
                        event["type"].as_str().unwrap_or("unknown")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}
fn monitor(store: &mut Store) -> Result<()> {
    let lock_path = store.metadata.join("monitor/lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .jctx("MONITOR_IO", "cannot open Monitor lock")?;
    lock.try_lock_exclusive()
        .map_err(|_| JavelinError::busy("another Monitor already owns this World"))?;
    let pid = std::process::id();
    let ready_path = store.metadata.join("monitor/ready");
    let pid_path = store.metadata.join("monitor/pid");
    write_monitor_state(&pid_path, &pid.to_string())?;
    write_monitor_state(
        &ready_path,
        &json!({"pid": pid, "ready": true, "started_at": now()}).to_string(),
    )?;
    store.append_event("monitor.ready", Some("world"), None, &json!({"pid": pid}))?;
    let debounce = Config::load(&store.root)?.checkpoint.debounce_ms.max(25);
    let mut pending_stamps = HashMap::<String, String>::new();
    let mut captured_stamps = HashMap::<String, String>::new();
    loop {
        if !store.metadata.exists() {
            break;
        }
        let layers = match store.monitor_layers() {
            Ok(layers) => layers,
            Err(_) => break,
        };
        for layer in layers {
            if !Path::new(&layer.view_path).exists() {
                continue;
            }
            let view = Path::new(&layer.view_path);
            let Ok(stamp) = view_stamp(view) else {
                continue;
            };
            if observed_checkpoint(store, &layer, &stamp)
                .ok()
                .flatten()
                .is_some()
            {
                captured_stamps.insert(layer.id.clone(), stamp);
                pending_stamps.remove(&layer.id);
                continue;
            }
            if captured_stamps.get(&layer.id) == Some(&stamp) {
                continue;
            }
            if pending_stamps.get(&layer.id) != Some(&stamp) {
                pending_stamps.insert(layer.id.clone(), stamp);
                continue;
            }
            let Ok(policy) = tracking_policy(store, &layer) else {
                continue;
            };
            let Ok(scan) = scan_view_with_policy(view, &store.objects, &policy) else {
                continue;
            };
            let Ok(stable_stamp) = view_stamp(view) else {
                continue;
            };
            if stable_stamp != pending_stamps[&layer.id] {
                pending_stamps.insert(layer.id.clone(), stable_stamp);
                continue;
            }
            let Ok(root_tree) = store.objects.put_tree(&scan.tree) else {
                continue;
            };
            let Ok(reconcile_lock) = open_reconcile_lock(store) else {
                continue;
            };
            if reconcile_lock.try_lock_exclusive().is_err() {
                continue;
            }
            let checkpoint = store.layer(&layer.id).ok().and_then(|current| {
                if current.synchronized_ref != layer.synchronized_ref {
                    return None;
                }
                let head = store.layer_head(&current).ok()?;
                if head.root_tree == root_tree {
                    return Some((current, head));
                }
                store
                    .register_object(
                        &root_tree,
                        ObjectKind::Tree,
                        crate::objects::encode_tree(&scan.tree)
                            .map(|bytes| bytes.len() as u64)
                            .unwrap_or(0),
                    )
                    .ok()?;
                let checkpoint = store
                    .append_checkpoint(
                        &current.id,
                        &root_tree,
                        &current.synchronized_ref,
                        "automatic",
                    )
                    .ok()?;
                Some((current, checkpoint))
            });
            let _ = FileExt::unlock(&reconcile_lock);
            if let Some((current, checkpoint)) = checkpoint
                && write_view_observation(store, &current, &checkpoint, &stable_stamp).is_ok()
            {
                captured_stamps.insert(layer.id.clone(), stable_stamp);
                pending_stamps.remove(&layer.id);
            }
        }
        thread::sleep(Duration::from_millis(debounce));
    }
    let _ = fs::remove_file(&ready_path);
    let _ = fs::remove_file(&pid_path);
    let _ = FileExt::unlock(&lock);
    Ok(())
}

fn start_monitor(store: &Store) -> Result<()> {
    if std::env::var_os("JAVELIN_MONITOR_CHILD").is_some() {
        return Ok(());
    }
    let ready_path = store.metadata.join("monitor/ready");
    if monitor_ready(&ready_path) {
        return Ok(());
    }
    let _ = fs::remove_file(&ready_path);
    let executable = std::env::current_exe().jctx("MONITOR_IO", "cannot locate Javelin binary")?;
    let mut command = ProcessCommand::new(executable);
    command
        .arg("--project")
        .arg(&store.root)
        .arg("__monitor")
        .env("JAVELIN_MONITOR_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_monitor_process(&mut command)?;
    command.spawn().jctx("MONITOR_IO", "cannot start Monitor")?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if monitor_ready(&ready_path) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(JavelinError::busy("Monitor did not become ready within 5s"))
}

#[cfg(windows)]
fn configure_monitor_process(command: &mut ProcessCommand) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(stream) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(JavelinError::new(
                7,
                "MONITOR_IO",
                "cannot prevent Monitor from inheriting command handles",
            )
            .details(json!({"cause": std::io::Error::last_os_error().to_string()})));
        }
    }
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    Ok(())
}

#[cfg(not(windows))]
fn configure_monitor_process(_command: &mut ProcessCommand) -> Result<()> {
    Ok(())
}

fn monitor_ready(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Some(pid) = value.get("pid").and_then(serde_json::Value::as_u64) else {
        return false;
    };
    process_alive(pid as u32)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn write_monitor_state(path: &Path, value: &str) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    let mut file = File::create(&temp).jctx("MONITOR_IO", "cannot create Monitor state")?;
    file.write_all(value.as_bytes())
        .and_then(|_| file.sync_all())
        .jctx("MONITOR_IO", "cannot write Monitor state")?;
    fs::rename(&temp, path).jctx("MONITOR_IO", "cannot install Monitor state")
}
fn create_claim(
    _store: &mut Store,
    _layer_id: &str,
    _resource: &str,
    _seconds: u64,
) -> Result<String> {
    _store.create_claim(_layer_id, _resource, _seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(identity: &str) -> Option<TreeEntry> {
        Some(TreeEntry {
            path: "path".into(),
            kind: EntryKind::Symlink,
            object_id: Some(identity.repeat(64)),
            executable: false,
        })
    }

    fn tree(entry: Option<TreeEntry>) -> Tree {
        Tree {
            entries: entry.into_iter().collect(),
        }
    }

    #[test]
    fn path_state_integration_table_is_exhaustive() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(temp.path()).unwrap();
        let a = state("a");
        let b = state("b");
        let c = state("c");
        let cases = [
            (a.clone(), a.clone(), b.clone(), b.clone(), false),
            (a.clone(), b.clone(), a.clone(), b.clone(), false),
            (a.clone(), b.clone(), b.clone(), b.clone(), false),
            (a.clone(), b.clone(), c.clone(), None, true),
            (None, None, a.clone(), a.clone(), false),
            (None, a.clone(), a.clone(), a.clone(), false),
            (None, a.clone(), b.clone(), None, true),
            (a.clone(), None, None, None, false),
            (a.clone(), b.clone(), None, None, true),
            (a.clone(), None, b.clone(), None, true),
            (a.clone(), a.clone(), None, None, false),
        ];
        for (base, target, private, expected, conflict) in cases {
            let (result, conflicts) =
                integrate_trees(&mut store, &tree(base), &tree(target), &tree(private)).unwrap();
            assert_eq!(
                result.entries.into_iter().next(),
                expected,
                "wrong integrated path state"
            );
            assert_eq!(!conflicts.is_empty(), conflict, "wrong conflict decision");
        }
    }
}
