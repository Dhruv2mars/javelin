use crate::cli::*;
use crate::config::{Config, DEFAULT_CONFIG, DEFAULT_IGNORE, IgnorePolicy, WorldRule};
use crate::durability::sync_dir;
use crate::error::{Context, JavelinError, Result};
use crate::model::{EntryKind, Layer, LayerStatus, TargetKind, Tree, TreeEntry, ViewMarker};
use crate::objects::ObjectKind;
use crate::paths::{ProjectContext, discover};
use crate::process::process_alive;
use crate::store::{ConflictInput, NewLayer, Store, ValidationRecord, now};
use crate::view::{
    apply_entries, diff_trees, invalidate_root_cache, materialize_tree_from_cache, scan_view,
    scan_view_with_policy, view_stamp,
};
use clap::CommandFactory;
use fs2::FileExt;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const TEXT_DIFF_LIMIT: u64 = 1024 * 1024;

mod claims;
mod conflicts;
mod discard;
mod integration;
mod maintenance;
mod monitor;
mod provenance;
mod publish;
mod reachability;
mod retention;
mod validation;
use claims::{claim, hook};
use conflicts::conflict;
use discard::{discard, discarded};
#[cfg(test)]
use integration::integrate_trees;
use integration::{refresh, refresh_layer};
use maintenance::{doctor, fsck, hex, repair};
use monitor::{monitor, start_monitor, write_monitor_state};
use provenance::{explain, provenance};
use publish::{acquire_publish_lock, open_publish_lock, publish};
use reachability::collect_reachability;
use retention::{events, gc};
use validation::{run_validations, verify};

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
                &path,
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
        } => with_store_read(cli.project, |_context, store| {
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
        Command::Gc { dry_run } => with_store_gc(cli.project, |_context, mut store| {
            gc(&mut store, dry_run, json_output)
        }),
        Command::Monitor => {
            let context = discover(cli.project.as_deref())?;
            let mut store = Store::open(&context.root)?;
            monitor(&mut store)
        }
    }
}

fn with_store(
    project: Option<PathBuf>,
    action: impl FnOnce(&ProjectContext, Store) -> Result<()>,
) -> Result<()> {
    let context = discover(project.as_deref())?;
    let store = Store::open(&context.root)?;
    start_monitor(&store)?;
    let _object_lease = acquire_object_reference_lease(&store)?;
    action(&context, store)
}

fn with_store_read(
    project: Option<PathBuf>,
    action: impl FnOnce(&ProjectContext, Store) -> Result<()>,
) -> Result<()> {
    let context = discover(project.as_deref())?;
    let store = Store::open(&context.root)?;
    start_monitor(&store)?;
    action(&context, store)
}

fn with_store_gc(
    project: Option<PathBuf>,
    action: impl FnOnce(&ProjectContext, Store) -> Result<()>,
) -> Result<()> {
    let context = discover(project.as_deref())?;
    let store = Store::open(&context.root)?;
    start_monitor(&store)?;
    let _object_lease = acquire_object_gc_lease(&store)?;
    action(&context, store)
}

fn with_store_without_monitor(
    project: Option<PathBuf>,
    action: impl FnOnce(&ProjectContext, Store) -> Result<()>,
) -> Result<()> {
    let context = discover(project.as_deref())?;
    let store = Store::open(&context.root)?;
    let _object_lease = acquire_object_reference_lease(&store)?;
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
    fs::create_dir_all(&requested).jctx(
        7,
        "INIT_IO",
        format!("cannot create {}", requested.display()),
    )?;
    let root = requested.canonicalize().jctx(
        7,
        "INIT_IO",
        format!("cannot resolve {}", requested.display()),
    )?;
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
    let _object_lease = acquire_object_reference_lease(&store)?;
    let scan = scan_view(&root, &store.objects)?;
    store.register_objects(&scan.objects)?;
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
            .jctx(7, "INIT_IO", format!("cannot write {}", path.display())),
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
    if layer.status == LayerStatus::Discarded {
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
        store.register_objects(&scan.objects)?;
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
    let unlock = FileExt::unlock(&reconcile_lock).jctx(
        8,
        "RECONCILE_LOCK",
        "cannot release reconciliation lock",
    );
    match (result, unlock) {
        (Ok(checkpoint), Ok(())) => Ok(checkpoint),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ViewObservation {
    schema: u8,
    layer_id: String,
    checkpoint_id: String,
    stamp: String,
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
    let Ok(observation) = serde_json::from_slice::<ViewObservation>(&bytes) else {
        return Ok(None);
    };
    if observation.schema != 1 || observation.layer_id != layer.id || observation.stamp != stamp {
        return Ok(None);
    }
    let head = store.layer_head(layer)?;
    Ok((observation.checkpoint_id == head.id).then_some(head))
}

fn write_view_observation(
    store: &Store,
    layer: &Layer,
    checkpoint: &crate::model::Checkpoint,
    stamp: &str,
) -> Result<()> {
    write_monitor_state(
        &view_observation_path(store, layer),
        &serde_json::to_string(&ViewObservation {
            schema: 1,
            layer_id: layer.id.clone(),
            checkpoint_id: checkpoint.id.clone(),
            stamp: stamp.to_string(),
        })
        .map_err(|error| {
            JavelinError::corruption(format!("cannot encode view observation: {error}"))
        })?,
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
        .jctx(8, "RECONCILE_LOCK", "cannot open reconciliation lock")
}

fn open_object_lifecycle_lock(store: &Store) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.metadata.join("locks/object-lifecycle.lock"))
        .jctx(8, "GC_LOCK", "cannot open object lifecycle lock")
}

fn acquire_object_reference_lease(store: &Store) -> Result<File> {
    let lock = open_object_lifecycle_lock(store)?;
    FileExt::lock_shared(&lock).jctx(8, "GC_LOCK", "cannot protect object references from GC")?;
    Ok(lock)
}

fn acquire_object_gc_lease(store: &Store) -> Result<File> {
    let lock = open_object_lifecycle_lock(store)?;
    lock.lock_exclusive()
        .jctx(8, "GC_LOCK", "cannot isolate object garbage collection")?;
    Ok(lock)
}

fn context_layer(context: &ProjectContext, store: &Store) -> Result<Layer> {
    store.layer(&context.layer_id)
}

fn selected_layer(
    context: &ProjectContext,
    store: &Store,
    requested: Option<&str>,
) -> Result<Layer> {
    match requested {
        Some(name) => store.layer(name),
        None => context_layer(context, store),
    }
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
    path_filters: &[String],
    json_output: bool,
) -> Result<()> {
    let layer = context_layer(context, store)?;
    let head = reconcile(store, &layer, "command-reconcile")?;
    let from = from.unwrap_or(&head.synchronized_ref);
    let to = to.unwrap_or(&head.id);
    let (_, from_root, from_tree) = store.tree_for_ref(from)?;
    let (_, to_root, to_tree) = store.tree_for_ref(to)?;
    let mut changes = diff_trees(&from_tree, &to_tree);
    if !path_filters.is_empty() {
        changes.retain(|change| {
            path_filters.iter().any(|filter| {
                change.path == *filter || change.path.starts_with(&format!("{filter}/"))
            })
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
    if kind != ObjectKind::Blob || length > TEXT_DIFF_LIMIT {
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
            let mut previous: Option<Tree> = None;
            for version in &history {
                let current = store.objects.read_tree(&version.root_tree)?;
                let changed = previous.as_ref().map_or_else(
                    || current.entries.iter().any(|entry| entry.path == path),
                    |previous| {
                        diff_trees(previous, &current)
                            .iter()
                            .any(|change| change.path == path)
                    },
                );
                if changed {
                    filtered.push(version.clone());
                }
                previous = Some(current);
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
        let bytes_hex = if size <= TEXT_DIFF_LIMIT {
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
        lock.flush().jctx(7, "OUTPUT_IO", "cannot flush path bytes")
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
                claims::validate_claim_resource(&resource)?;
                store.create_claim(&created.id, &resource, 3600)?;
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
    let lock_file = open_publish_lock(store, "world")?;
    acquire_publish_lock(&lock_file)?;
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
    FileExt::unlock(&lock_file).jctx(8, "PUBLISH_LOCK", "cannot release Publish lease")?;
    emit(
        json_output,
        &json!({"world_version": restored, "validations": validations, "policy_override": failed && accept_failing}),
        format!("Restored {} as {}", version, restored.id),
    )
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
    fn gc_waits_for_active_object_reference_writer() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("world");
        fs::create_dir_all(&root).unwrap();
        let store = Store::create(&root).unwrap();
        let writer = acquire_object_reference_lease(&store).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let handle = std::thread::spawn(move || {
            let store = Store::open(&root).unwrap();
            ready_tx.send(()).unwrap();
            let _gc = acquire_object_gc_lease(&store).unwrap();
            done_tx.send(()).unwrap();
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(writer);
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
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
