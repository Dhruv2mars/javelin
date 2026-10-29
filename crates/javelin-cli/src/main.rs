use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use javelin::{Error, Repository, Result, VersionNumber};
use serde_json::{Value, json};

mod cli;
mod cli_auth;
mod cli_changes;
mod cli_check;
mod cli_control;
mod cli_disclosure;
mod cli_member;
mod cli_node;
mod cli_publish;
mod cli_remote;
mod cli_request;
mod cli_store;
mod cli_stream;
mod cli_sync;
mod cli_update;
mod cli_upload;
mod cli_worlds;

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
    Remote(cli_remote::RemoteError),
    Control(javelin_control::ControlError),
}

impl From<Error> for CliError {
    fn from(error: Error) -> Self {
        Self::Native(error)
    }
}
impl From<cli_remote::RemoteError> for CliError {
    fn from(error: cli_remote::RemoteError) -> Self {
        Self::Remote(error)
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
            Self::Remote(error) => &error.code,
            Self::Control(error) => error.code(),
        }
    }
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(error) => error.fmt(f),
            Self::Stack(error) => error.fmt(f),
            Self::Remote(error) if error.code == javelin_protocol::codec::code::QUOTA_EXCEEDED => {
                write!(
                    f,
                    "the node's budget refused this write ({})\n{}",
                    error.detail,
                    cli_node::quota_hint(&error.detail)
                )
            }
            Self::Remote(error) => write!(f, "{}", error.detail),
            Self::Control(error) => error.fmt(f),
        }
    }
}

fn run(args: Vec<OsString>) -> std::result::Result<cli::Output, CliError> {
    let args: Vec<_> = args.into_iter().filter(|a| a != "--json").collect();
    if args.first().is_some_and(|a| a == "layer") && args.get(1).is_some_and(|a| a == "stack") {
        return cli::stack(args.into_iter().skip(2).collect());
    }
    if args.first().is_some_and(|a| a == "layer") && args.get(1).is_some_and(|a| a == "refresh") {
        return cli_remote::layer_refresh(args.into_iter().skip(2).collect());
    }
    if args.first().is_some_and(|a| a == "layer") && args.get(1).is_some_and(|a| a == "update") {
        return cli_update::layer_update(args.into_iter().skip(2).collect());
    }
    if args.first().is_some_and(|a| a == "layer") && args.get(1).is_some_and(|a| a == "take") {
        return cli_remote::layer_take(args.into_iter().skip(2).collect());
    }
    if args.first().is_some_and(|a| a == "blame") {
        return cli::blame(args.into_iter().skip(1).collect()).map_err(Into::into);
    }
    if args.first().is_some_and(|a| a == "attach") {
        return cli_remote::attach(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "frontier") {
        return cli_disclosure::frontier(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "delay") {
        return cli_disclosure::delay(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "embargo") {
        return cli_disclosure::embargo(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "node") {
        return cli_node::node(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "request") {
        return cli_request::request(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "status") {
        return cli_stream::status(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "sync") {
        return cli_sync::sync(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "outbox") && args.get(1).is_some_and(|a| a == "replay") {
        return cli_remote::outbox_replay(args.into_iter().skip(2).collect());
    }
    if args.first().is_some_and(|a| a == "outbox") && args.get(1).is_some_and(|a| a == "status") {
        return cli::outbox_status(args.into_iter().skip(2).collect()).map_err(Into::into);
    }
    // `adopt PROJECT --repo DIR` imports into an attached repository; without --repo, adopt
    // makes a new repository of the project directory.
    if args.first().is_some_and(|a| a == "adopt") && args.iter().any(|a| a == "--repo") {
        return cli_publish::adopt(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "adopt") {
        return cli::adopt(args.into_iter().skip(1).collect()).map_err(Into::into);
    }
    if args
        .first()
        .is_some_and(|a| a == "login" || a == "control" || a == "repo")
    {
        return cli_control::run(args).map_err(CliError::Control);
    }
    if args.first().is_some_and(|a| a == "member") {
        return cli_member::run(args.into_iter().skip(1).collect()).map_err(CliError::Control);
    }
    if args.first().is_some_and(|a| a == "check") && args.get(1).is_some_and(|a| a == "report") {
        return cli_check::report(args.into_iter().skip(2).collect()).map_err(Into::into);
    }
    if args.first().is_some_and(|a| a == "checkpoint") {
        return cli::checkpoint(args.into_iter().skip(1).collect()).map_err(Into::into);
    }
    if args
        .first()
        .is_some_and(|a| a == "layer" || a == "workspace")
    {
        return cli::run(args);
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
            "unknown command {:?}; expected init, adopt, attach, sync, status, request, history, check, blame, checkpoint, layer, or workspace",
            command
        )));
    }
    let usage = "expected one optional project directory, or --repo DIR, and --json";
    // `--repo DIR` names the directory as every other command does.
    let root = match args.next() {
        Some(flag) if flag == "--repo" => PathBuf::from(
            args.next()
                .ok_or_else(|| Error::InvalidInput(usage.into()))?,
        ),
        Some(path) if path.to_string_lossy().starts_with('-') => {
            return Err(Error::InvalidInput(usage.into()));
        }
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
    if args.next().is_some() {
        return Err(Error::InvalidInput(usage.into()));
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
        // An attached repository lists the node's numbers and IDs for the Worlds it holds.
        let worlds = cli_worlds::Worlds::of(&repo)?;
        let mut versions = Vec::new();
        let mut lines = Vec::new();
        for version in repo.history()? {
            let shown = worlds.version(&version)?;
            lines.push(format!(
                "{}  {}",
                worlds.label(version.number)?,
                shown["id"].as_str().unwrap_or_default()
            ));
            versions.push(shown);
        }
        let text = lines.join("\n");
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
