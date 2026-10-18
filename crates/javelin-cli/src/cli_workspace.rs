//! Clone-backed Layer directories: `javelin workspace <command>` without `--helper`.
use std::ffi::OsString;

use javelin::{
    CloneReport, CloneStrategy, Error, LayerState, Repository, Result, VersionNumber,
    WorkspaceReport,
};
use serde_json::json;

use super::{Args, Output, captured, success};

pub(super) fn run(action: &str, args: Vec<OsString>) -> Result<Output> {
    let (allowed, count): (&[&str], usize) = match action {
        "create" => (&["--repo", "--world"], 1),
        "open" | "path" => (&["--repo"], 1),
        "discard" => (&["--repo", "--operation"], 1),
        "recover" => (&["--repo"], 0),
        _ => {
            return Err(Error::InvalidInput(format!(
                "unknown workspace command {action}; clone-backed workspaces support \
                 create, open, path, discard and recover (OverlayFS commands need --helper)"
            )));
        }
    };
    let args = Args::parse(args, allowed, count)?;
    let world = args
        .number("--world")?
        .map(VersionNumber::new)
        .transpose()?;
    let operation = (action == "discard")
        .then(|| args.operation())
        .transpose()?;
    let mut repo = Repository::open(args.root()?)?;
    let command = format!("workspace {action}");
    if action == "recover" {
        let recovery = repo.recover_workspaces()?;
        let text = format!(
            "Moved {} directories to trash; reclaimed {} entries; {} trash entries remain",
            recovery.trashed, recovery.reclaimed, recovery.remaining
        );
        return Ok(success(
            json!({"ok": true, "command": command, "recovery": recovery}),
            text,
        ));
    }
    let layer = args.layer()?;
    let capture = repo.capture_workspace(&layer)?;
    let output = match action {
        "create" | "open" => {
            if action == "create" {
                let version = match world {
                    Some(version) => version,
                    None => repo.history()?.last().expect("World 0 exists").number,
                };
                let base = repo.layer_base(version)?;
                repo.create_layer(&layer, &base)?;
            }
            let report = repo.open_workspace(&layer)?;
            let text = report_text(&report);
            success(
                json!({"ok": true, "command": command, "workspace": report}),
                text,
            )
        }
        "path" => {
            let workspace = repo.layer_workspace(&layer)?;
            let text = workspace.root.display().to_string();
            success(
                json!({"ok": true, "command": command, "workspace": workspace}),
                text,
            )
        }
        "discard" => {
            let operation = operation.expect("required operation");
            let expected = repo
                .layer_history(&layer)?
                .into_iter()
                .rev()
                .find_map(|state| match state {
                    LayerState::Active { head } => Some(head),
                    _ => None,
                })
                .ok_or_else(|| Error::InvalidInput("Layer has no active history".into()))?;
            let discard = repo.discard_workspace(&operation, &expected)?;
            let (mut reply, text, code) =
                super::update(&command, &operation, discard.update.clone());
            reply["trashed"] = json!(discard.trashed);
            reply["recovery"] = json!(discard.recovery);
            (reply, text, code)
        }
        _ => unreachable!(),
    };
    Ok(captured(capture, output))
}

fn report_text(report: &WorkspaceReport) -> String {
    let mut lines = vec![format!(
        "Layer {} {} at {}",
        report.layer.as_str(),
        if report.created {
            "opened"
        } else {
            "already open"
        },
        report.root.display()
    )];
    if let Some(base) = &report.base {
        lines.push(format!(
            "Base World {}: {:?} in {} ms",
            base.version, base.source, base.elapsed_ms
        ));
    }
    if let Some(clone) = &report.clone {
        lines.push(clone_text(clone, report.elapsed_ms));
    }
    lines.join("\n")
}

fn clone_text(clone: &CloneReport, elapsed: u64) -> String {
    let mut line = format!(
        "Strategy {} in {elapsed} ms; copied {} bytes",
        clone.strategy.as_str(),
        clone.copied_bytes
    );
    if let Some(reason) = &clone.fallback {
        line.push_str(&format!(" ({reason})"));
    }
    if clone.strategy == CloneStrategy::Copy {
        line.push_str(
            "\nWarning: this volume cannot clone files, so unchanged data was duplicated. \
             Use APFS, btrfs, XFS or a ReFS Dev Drive to share it.",
        );
    }
    line
}
