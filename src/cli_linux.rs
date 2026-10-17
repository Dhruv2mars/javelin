use std::path::PathBuf;

use javelin::{
    Error, LayerId, LayerMountHelper, LayerUpdate, LinuxLayerRuntime, OperationId, Repository,
    Result,
};
use serde_json::json;

use super::{Output, success, update};

pub fn run(
    action: &str,
    repo: PathBuf,
    root: PathBuf,
    helper: PathBuf,
    layer: Option<LayerId>,
    operation: Option<OperationId>,
) -> Result<Output> {
    let mut repo = Repository::open(repo)?;
    let helper = LayerMountHelper::sudo(helper)?;
    let mut runtime = if action == "init" {
        LinuxLayerRuntime::init(&repo, &root, helper)?
    } else {
        LinuxLayerRuntime::open(&repo, &root, helper)?
    };
    let command = format!("workspace {action}");
    match action {
        "init" => Ok(success(
            json!({"ok": true, "command": command, "root": root}),
            format!("Linux workspace ready at {}", root.display()),
        )),
        "recover" => {
            let report = runtime.recover(&mut repo)?;
            let ok = report.failures.is_empty();
            let text = format!(
                "Recovery: {} pending operations resolved, {} views removed, {} views thawed, {} stages aborted, {} failures.{}",
                report.pending_resolved,
                report.views_removed,
                report.views_thawed,
                report.stages_aborted,
                report.failures.len(),
                if ok {
                    String::new()
                } else {
                    format!("\n{}", report.failures.join("\n"))
                }
            );
            Ok((
                json!({"ok": ok, "command": command, "recovery": report}),
                text,
                if ok { 0 } else { 1 },
            ))
        }
        _ => {
            let layer = layer.expect("validated Layer argument");
            match action {
                "mount" => {
                    let path = runtime.mount(&mut repo, &layer)?;
                    Ok(success(
                        json!({"ok": true, "command": command, "path": path}),
                        path.display().to_string(),
                    ))
                }
                "path" => {
                    let status = runtime.status(&repo, &layer)?;
                    if !status.mounted
                        || status.frozen
                        || status.pending.is_some()
                        || !matches!(status.current, javelin::LayerState::Active { .. })
                    {
                        return Err(Error::InvalidInput("Layer view is not mounted and writable; use workspace mount or recover".into()));
                    }
                    Ok(success(
                        json!({"ok": true, "command": command, "path": status.path, "view": status}),
                        status.path.display().to_string(),
                    ))
                }
                "unmount" => {
                    runtime.unmount(&layer)?;
                    Ok(success(
                        json!({"ok": true, "command": command, "layer": layer}),
                        "Unmounted; private edits retained.".into(),
                    ))
                }
                "checkpoint" => {
                    let operation = operation.expect("validated operation");
                    let capture = runtime.capture(&mut repo, &layer, &operation)?;
                    let applied = matches!(capture.update, LayerUpdate::Applied { .. });
                    let mut reply = update(&command, &operation, capture.update.clone());
                    reply.1.push_str(&format!("\n{} checkpointed paths; {} empty directories are not represented. Replay: {}.", capture.changed_paths, capture.unrepresented_directories.len(), capture.replayed));
                    reply.0 = json!({"ok": applied, "command": command, "operation": operation, "capture": capture});
                    Ok(reply)
                }
                "discard" => {
                    let operation = operation.expect("validated operation");
                    let outcome = runtime.discard(&mut repo, &layer, &operation)?;
                    Ok(update(&command, &operation, outcome))
                }
                _ => unreachable!(),
            }
        }
    }
}
