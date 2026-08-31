use super::*;

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

pub(super) fn write_monitor_state(path: &Path, value: &str) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    let mut file = File::create(&temp).jctx("MONITOR_IO", "cannot create Monitor state")?;
    file.write_all(value.as_bytes())
        .and_then(|_| file.sync_all())
        .jctx("MONITOR_IO", "cannot write Monitor state")?;
    fs::rename(&temp, path).jctx("MONITOR_IO", "cannot install Monitor state")?;
    sync_dir(
        path.parent()
            .ok_or_else(|| JavelinError::corruption("Monitor state path has no parent"))?,
    )
}

pub(super) fn monitor(store: &mut Store) -> Result<()> {
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
            let Ok(_object_lease) = acquire_object_reference_lease(store) else {
                continue;
            };
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

pub(super) fn start_monitor(store: &Store) -> Result<()> {
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
