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
    let Ok(pid) = u32::try_from(pid) else {
        return false;
    };
    process_alive(pid)
}

pub(super) fn write_monitor_state(path: &Path, value: &str) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", ulid::Ulid::new()));
    let mut file = File::create(&temp).jctx(8, "MONITOR_IO", "cannot create Monitor state")?;
    file.write_all(value.as_bytes())
        .and_then(|_| file.sync_all())
        .jctx(8, "MONITOR_IO", "cannot write Monitor state")?;
    fs::rename(&temp, path).jctx(8, "MONITOR_IO", "cannot install Monitor state")?;
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
        .jctx(8, "MONITOR_IO", "cannot open Monitor lock")?;
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
    let mut query_failures = 0_u8;
    let result = loop {
        if !store.metadata.exists() {
            break Ok(());
        }
        let layers = match store.monitor_layers() {
            Ok(layers) => {
                query_failures = 0;
                layers
            }
            Err(error) => {
                query_failures += 1;
                if query_failures >= 3 {
                    record_monitor_error(store, None, "layer-query", &error);
                    break Err(error);
                }
                thread::sleep(Duration::from_millis(debounce));
                continue;
            }
        };
        for layer in layers {
            if !Path::new(&layer.view_path).exists() {
                continue;
            }
            let _object_lease = match acquire_object_reference_lease(store) {
                Ok(lease) => lease,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "object-lease", &error);
                    continue;
                }
            };
            let view = Path::new(&layer.view_path);
            let stamp = match view_stamp(view) {
                Ok(stamp) => stamp,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "view-stamp", &error);
                    continue;
                }
            };
            let observed = match observed_checkpoint(store, &layer, &stamp) {
                Ok(observed) => observed,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "observation", &error);
                    continue;
                }
            };
            if observed.is_some() {
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
            let policy = match tracking_policy(store, &layer) {
                Ok(policy) => policy,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "tracking-policy", &error);
                    continue;
                }
            };
            let scan = match scan_view_with_policy(view, &store.objects, &policy) {
                Ok(scan) => scan,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "scan", &error);
                    continue;
                }
            };
            let stable_stamp = match view_stamp(view) {
                Ok(stamp) => stamp,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "stable-stamp", &error);
                    continue;
                }
            };
            if stable_stamp != pending_stamps[&layer.id] {
                pending_stamps.insert(layer.id.clone(), stable_stamp);
                continue;
            }
            if let Err(error) = store.register_objects(&scan.objects) {
                record_monitor_error(store, Some(&layer.id), "object-metadata", &error);
                continue;
            }
            let root_tree = match store.objects.put_tree(&scan.tree) {
                Ok(root_tree) => root_tree,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "tree-write", &error);
                    continue;
                }
            };
            let reconcile_lock = match open_reconcile_lock(store) {
                Ok(lock) => lock,
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "reconcile-lock", &error);
                    continue;
                }
            };
            if reconcile_lock.try_lock_exclusive().is_err() {
                continue;
            }
            let checkpoint = (|| -> Result<Option<(Layer, crate::model::Checkpoint)>> {
                let current = store.layer(&layer.id)?;
                if current.synchronized_ref != layer.synchronized_ref {
                    return Ok(None);
                }
                let head = store.layer_head(&current)?;
                if head.root_tree == root_tree {
                    return Ok(Some((current, head)));
                }
                store.register_object(
                    &root_tree,
                    ObjectKind::Tree,
                    crate::objects::encode_tree(&scan.tree)?.len() as u64,
                )?;
                let checkpoint = store.append_checkpoint(
                    &current.id,
                    &root_tree,
                    &current.synchronized_ref,
                    "automatic",
                )?;
                Ok(Some((current, checkpoint)))
            })();
            let _ = FileExt::unlock(&reconcile_lock);
            match checkpoint {
                Ok(Some((current, checkpoint))) => {
                    match write_view_observation(store, &current, &checkpoint, &stable_stamp) {
                        Ok(()) => {
                            captured_stamps.insert(layer.id.clone(), stable_stamp);
                            pending_stamps.remove(&layer.id);
                        }
                        Err(error) => record_monitor_error(
                            store,
                            Some(&layer.id),
                            "observation-write",
                            &error,
                        ),
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    record_monitor_error(store, Some(&layer.id), "checkpoint", &error);
                }
            }
        }
        thread::sleep(Duration::from_millis(debounce));
    };
    let _ = fs::remove_file(&ready_path);
    let _ = fs::remove_file(&pid_path);
    let _ = FileExt::unlock(&lock);
    result
}

fn record_monitor_error(
    store: &mut Store,
    layer_id: Option<&str>,
    stage: &str,
    error: &JavelinError,
) {
    let _ = store.append_event(
        "monitor.error",
        layer_id.map(|_| "layer"),
        layer_id,
        &json!({"stage": stage, "code": error.code, "message": error.message}),
    );
}

pub(super) fn start_monitor(store: &Store) -> Result<()> {
    if std::env::var_os("JAVELIN_MONITOR_CHILD").is_some() {
        return Ok(());
    }
    let ready_path = store.metadata.join("monitor/ready");
    let startup_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.metadata.join("monitor/start.lock"))
        .jctx(8, "MONITOR_LOCK", "cannot open Monitor startup lock")?;
    startup_lock
        .lock_exclusive()
        .jctx(8, "MONITOR_LOCK", "cannot acquire Monitor startup lock")?;
    if monitor_ready(&ready_path) {
        return Ok(());
    }
    let _ = fs::remove_file(&ready_path);
    let executable =
        std::env::current_exe().jctx(8, "MONITOR_IO", "cannot locate Javelin binary")?;
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
    {
        // Windows inherits every inheritable handle, even when the child's
        // standard streams are NUL. Do not keep a caller's output pipe alive.
        #[cfg(windows)]
        let _stdio = MonitorStdioInheritance::suspend()?;
        command
            .spawn()
            .jctx(8, "MONITOR_IO", "cannot start Monitor")?;
    }
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
struct MonitorStdioInheritance(Vec<(windows_sys::Win32::Foundation::HANDLE, u32)>);

#[cfg(windows)]
impl MonitorStdioInheritance {
    fn suspend() -> Result<Self> {
        use windows_sys::Win32::Foundation::{
            GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
        };
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        let mut guard = Self(Vec::new());
        for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // Standard handles remain owned by this process throughout spawn.
            let handle = unsafe { GetStdHandle(stream) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags = 0;
            if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
                return Err(JavelinError::new(
                    8,
                    "MONITOR_IO",
                    "cannot read stdio inheritance",
                ));
            }
            if flags & HANDLE_FLAG_INHERIT != 0 {
                if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                    return Err(JavelinError::new(
                        8,
                        "MONITOR_IO",
                        "cannot suspend stdio inheritance",
                    ));
                }
                guard.0.push((handle, flags));
            }
        }
        Ok(guard)
    }
}

#[cfg(windows)]
impl Drop for MonitorStdioInheritance {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        for &(handle, flags) in &self.0 {
            // Restore the caller's flags before validation or any later spawn.
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags);
            }
        }
    }
}

#[cfg(windows)]
fn configure_monitor_process(command: &mut ProcessCommand) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    Ok(())
}

#[cfg(not(windows))]
fn configure_monitor_process(_command: &mut ProcessCommand) -> Result<()> {
    Ok(())
}
