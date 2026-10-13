use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use javelin::{Error, Repository, Result, VersionNumber};
use serde_json::{Value, json};

const HELP: &str = "Javelin native repository foundation\n\nUsage: javelin <init|history|check> [project-directory] [--json]\n\ninit     Create an empty immutable World 0; existing files stay untouched\nhistory  Read accepted World versions in ascending order\ncheck    Verify stored objects, manifests, history, and operation outcomes\n\nLayers and the complete Layer Publish workflow are not implemented yet.";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let structured = args.iter().any(|a| a == "--json");
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    match run(args) {
        Ok((value, text)) => {
            if structured {
                println!("{value}");
            } else {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if structured {
                eprintln!(
                    "{}",
                    json!({"ok": false, "error": {"code": error.code(), "message": error.to_string()}})
                );
            } else {
                eprintln!("javelin: {error}");
            }
            ExitCode::from(if matches!(error, Error::InvalidInput(_)) {
                2
            } else {
                1
            })
        }
    }
}

fn run(args: Vec<OsString>) -> Result<(Value, String)> {
    let mut args = args.into_iter().filter(|a| a != "--json");
    let command = args
        .next()
        .ok_or_else(|| Error::InvalidInput(HELP.into()))?;
    if command != "init" && command != "history" && command != "check" {
        return Err(Error::InvalidInput(format!(
            "unknown command {:?}; expected init, history, or check",
            command
        )));
    }
    let root = args
        .next()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    if root.as_os_str().to_string_lossy().starts_with('-') || args.next().is_some() {
        return Err(Error::InvalidInput(
            "expected one optional project directory and --json".into(),
        ));
    }
    if command == "init" {
        let repo = Repository::init(&root)?;
        let initial = repo.version(VersionNumber::new(0)?)?;
        return Ok((
            json!({"ok": true, "command": "init", "initial": initial, "tracked_files": 0}),
            format!(
                "Native repository ready at {}. Initial World 0. Existing files were not ingested.",
                root.display()
            ),
        ));
    }
    let repo = Repository::open(&root)?;
    if command == "history" {
        let versions = repo.history()?;
        let text = versions
            .iter()
            .map(|v| format!("World {}  {}", v.number, v.manifest))
            .collect::<Vec<_>>()
            .join("\n");
        Ok((
            json!({"ok": true, "command": "history", "versions": versions}),
            text,
        ))
    } else {
        let report = repo.check()?;
        let text = format!(
            "Integrity OK: {} World versions, {} objects, {} operation outcomes.",
            report.versions, report.objects, report.attempts
        );
        Ok((
            json!({"ok": true, "command": "check", "integrity": report}),
            text,
        ))
    }
}
