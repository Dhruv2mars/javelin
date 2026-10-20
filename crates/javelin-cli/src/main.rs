use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use javelin::{Error, Repository, Result, VersionNumber};
use serde_json::{Value, json};

mod cli;

const HELP: &str = cli::HELP;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let structured = args.iter().any(|a| a == "--json");
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && (args[0] == "--version" || args[0] == "-V") {
        println!("javelin {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run(args) {
        Ok((value, text, code)) => {
            if structured {
                println!("{value}");
            } else {
                println!("{text}");
            }
            ExitCode::from(code)
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
            ExitCode::from(
                if matches!(error, CliError::Native(Error::InvalidInput(_))) {
                    2
                } else {
                    1
                },
            )
        }
    }
}

#[derive(Debug)]
enum CliError {
    Native(Error),
    Stack(javelin_stack::StackError),
}

impl From<Error> for CliError {
    fn from(error: Error) -> Self {
        Self::Native(error)
    }
}
impl From<javelin_stack::StackError> for CliError {
    fn from(error: javelin_stack::StackError) -> Self {
        match error {
            javelin_stack::StackError::Storage(error) => Self::Native(error),
            error => Self::Stack(error),
        }
    }
}
impl CliError {
    fn code(&self) -> &str {
        match self {
            Self::Native(error) => error.code(),
            Self::Stack(_) => "stack_merge_error",
        }
    }
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(error) => error.fmt(f),
            Self::Stack(error) => error.fmt(f),
        }
    }
}

fn run(args: Vec<OsString>) -> std::result::Result<cli::Output, CliError> {
    let args: Vec<_> = args.into_iter().filter(|a| a != "--json").collect();
    if args.first().is_some_and(|a| a == "layer") && args.get(1).is_some_and(|a| a == "stack") {
        return cli::stack(args.into_iter().skip(2).collect());
    }
    if args.first().is_some_and(|a| a == "blame") {
        return cli::blame(args.into_iter().skip(1).collect()).map_err(Into::into);
    }
    if args.first().is_some_and(|a| a == "checkpoint") {
        return cli::checkpoint(args.into_iter().skip(1).collect()).map_err(Into::into);
    }
    if args
        .first()
        .is_some_and(|a| a == "layer" || a == "workspace")
    {
        return cli::run(args).map_err(Into::into);
    }
    run_repository(args)
        .map(|(value, text)| (value, text, 0))
        .map_err(Into::into)
}

fn run_repository(args: Vec<OsString>) -> Result<(Value, String)> {
    let mut args = args.into_iter();
    let command = args
        .next()
        .ok_or_else(|| Error::InvalidInput(HELP.into()))?;
    if command != "init" && command != "history" && command != "check" {
        return Err(Error::InvalidInput(format!(
            "unknown command {:?}; expected init, history, check, blame, checkpoint, layer, or workspace",
            command
        )));
    }
    let root = match args.next() {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
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
            .map(|v| format!("World {}  {}", v.number, v.id))
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
