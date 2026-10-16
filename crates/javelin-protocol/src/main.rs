//! `javelin-protocol serve <project>` speaks version 1 on stdin/stdout.
//! `javelin-protocol push <project> <manifest> <operation> <expected>` speaks it
//! on stdin/stdout and reports the outcome on stderr. Transport is the caller's job.

use std::{io, process::ExitCode};

use javelin::{ObjectId, OperationId, Repository, VersionNumber};
use javelin_protocol::{Outcome, ProtocolError, push, serve};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve", project] => Repository::open(project)
            .map_err(ProtocolError::from)
            .and_then(|mut repo| serve(&mut repo, io::stdin().lock(), io::stdout().lock())),
        ["push", project, manifest, operation, expected] => {
            run_push(project, manifest, operation, expected)
        }
        _ => Err(ProtocolError::new(
            "usage",
            "javelin-protocol serve <project> | push <project> <manifest> <operation> <expected>",
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error {} {}", e.code, e.detail);
            ExitCode::FAILURE
        }
    }
}

fn run_push(
    project: &str,
    manifest: &str,
    operation: &str,
    expected: &str,
) -> Result<(), ProtocolError> {
    let repo = Repository::open(project)?;
    let expected = expected
        .parse::<u64>()
        .map_err(|_| ProtocolError::new("invalid_input", "expected version must be an integer"))?;
    let outcome = push(
        io::stdin().lock(),
        io::stdout().lock(),
        &repo,
        ObjectId::parse(manifest)?,
        &OperationId::parse(operation)?,
        VersionNumber::new(expected)?,
    )?;
    match outcome {
        Outcome::Accepted {
            number,
            manifest,
            operation,
        } => {
            eprintln!("accepted {number} {manifest} {operation}")
        }
        Outcome::Stale { actual } => eprintln!("stale {actual}"),
    }
    Ok(())
}
