//! `javelin check report`: posts one completed check result to a self-hosted node
//! (`docs/ci-execution.md` section 4; unit U17). The node is the one the control plane
//! provisions under `<control dir>/nodes/<repository>`, and its trusted keys come from the
//! control plane. The reporter's token is read from `JAVELIN_TOKEN`, so it stays out of argv.
//! The command opens the node's directory in process, so it needs the node not to be
//! serving: the loopback transport cannot carry the reporter's token yet (U7 left wire
//! carriage open), and a serving node holds the directory, which makes this `busy`.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use javelin::{Error, ObjectId, OperationId, Repository, Result};
use javelin_collaboration::RepositoryId;
use javelin_control::{ControlPlane, LocalNode};
use javelin_node::{FileWorldLog, checks};
use javelin_protocol::codec::{CheckOutcome, CheckReport, Conclusion};
use serde_json::json;

use crate::cli::Output;

const USAGE: &str = "usage: javelin check report --repository ID --operation ID --tree HASH --name NAME \
--conclusion success|failure|neutral|cancelled|timed_out|skipped --completed UNIX \
[--checkpoint HASH] [--details-url URL] [--summary TEXT] [--started UNIX] [--control DIR]";

const OPTIONS: &[&str] = &[
    "--repository",
    "--operation",
    "--tree",
    "--checkpoint",
    "--name",
    "--conclusion",
    "--details-url",
    "--summary",
    "--started",
    "--completed",
    "--control",
];

/// Parses the arguments after `check report`, then posts the result. A refusal by the node
/// or the control plane is an outcome with exit code 1. Bad arguments are usage errors.
pub fn report(args: Vec<OsString>) -> Result<Output> {
    let request = Request::parse(args)?;
    let control = match &request.control {
        Some(dir) => dir.clone(),
        None => control_dir()?,
    };
    let token = env::var("JAVELIN_TOKEN").ok();
    Ok(respond(&request, &control, token.as_deref()))
}

/// Posts `request` and renders the node's answer, or the failure that stopped it.
fn respond(request: &Request, control: &Path, token: Option<&str>) -> Output {
    match post(control, request, token) {
        Ok(CheckOutcome::Accepted { id, seq }) => accepted(request, id, seq),
        Ok(CheckOutcome::Refused { code }) => refused(request, &code, ""),
        Err((code, detail)) => refused(request, &code, &detail),
    }
}

/// The node's answer, or the (code, detail) of a failure before it could answer.
fn post(
    control: &Path,
    request: &Request,
    token: Option<&str>,
) -> std::result::Result<CheckOutcome, (String, String)> {
    let failure = |error: &dyn std::fmt::Display, code: &str| (code.to_string(), error.to_string());
    let repository = RepositoryId::parse(&request.repository)
        .map_err(|error| failure(&error, "invalid_input"))?;
    let keys = ControlPlane::open(control)
        .and_then(|plane| plane.node_keys())
        .map_err(|error| (error.code().to_string(), error.to_string()))?;
    let node = LocalNode::new(control.join("nodes"));
    let dir = node.repository_dir(&repository);
    Repository::open(&dir).map_err(|_| {
        (
            "unknown_repository".to_string(),
            format!("no node for repository {}", request.repository),
        )
    })?;
    let gate = node
        .gate(&repository, keys)
        .map_err(|error| (error.code().to_string(), error.to_string()))?;
    let mut log = FileWorldLog::open(&dir).map_err(|error| failure(&error, error.code()))?;
    checks::report(&mut log, &gate, token, now(), &request.message())
        .map_err(|error| (error.code, error.detail))
}

fn accepted(request: &Request, id: ObjectId, seq: u64) -> Output {
    (
        json!({
            "ok": true,
            "command": "check report",
            "outcome": "accepted",
            "repository": request.repository,
            "tree": request.tree.to_string(),
            "name": request.name,
            "conclusion": request.conclusion.as_str(),
            "id": id.to_string(),
            "seq": seq,
        }),
        format!(
            "Accepted {} {} for tree {} as result {seq} ({id}).",
            request.name,
            request.conclusion.as_str(),
            request.tree
        ),
        0,
    )
}

fn refused(request: &Request, code: &str, detail: &str) -> Output {
    let text = if detail.is_empty() {
        format!("Refused {}: {code}.", request.name)
    } else {
        format!("Refused {}: {code}: {detail}", request.name)
    };
    (
        json!({
            "ok": false,
            "command": "check report",
            "outcome": "refused",
            "code": code,
            "detail": detail,
            "name": request.name,
        }),
        text,
        1,
    )
}

/// The control plane's directory: `JAVELIN_CONTROL_DIR`, else `$JAVELIN_HOME/control`.
fn control_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("JAVELIN_CONTROL_DIR") {
        return Ok(dir.into());
    }
    let home = match env::var_os("JAVELIN_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .ok_or_else(|| Error::InvalidInput("set JAVELIN_HOME or HOME".into()))?,
        )
        .join(".javelin"),
    };
    Ok(home.join("control"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The parsed command line. `completed` is required and `started` defaults to it, so the
/// same arguments always make the same message and a retry replays the original result.
struct Request {
    repository: String,
    operation: OperationId,
    tree: ObjectId,
    checkpoint: Option<ObjectId>,
    name: String,
    conclusion: Conclusion,
    details_url: Option<String>,
    summary: Option<String>,
    started: u64,
    completed: u64,
    control: Option<PathBuf>,
}

impl Request {
    fn parse(args: Vec<OsString>) -> Result<Self> {
        let mut options = std::collections::BTreeMap::new();
        let mut args = args.into_iter();
        while let Some(key) = args.next() {
            let key = text(&key)?.to_owned();
            if !OPTIONS.contains(&key.as_str()) || options.contains_key(&key) {
                return Err(usage(&format!("unknown or repeated option {key}")));
            }
            let value = args
                .next()
                .ok_or_else(|| usage(&format!("{key} needs a value")))?;
            options.insert(key, text(&value)?.to_owned());
        }
        let required = |key: &str| {
            options
                .get(key)
                .cloned()
                .ok_or_else(|| usage(&format!("missing {key}")))
        };
        let conclusion = required("--conclusion")?;
        let conclusion = Conclusion::parse(&conclusion)
            .ok_or_else(|| usage(&format!("unknown conclusion {conclusion}")))?;
        let number = |key: &str| {
            options
                .get(key)
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| usage(&format!("{key} needs a Unix time in seconds")))
                })
                .transpose()
        };
        let object = |key: &str| -> Result<Option<ObjectId>> {
            options
                .get(key)
                .map(|value| ObjectId::parse(value).map_err(|error| usage(&error.to_string())))
                .transpose()
        };
        let completed = number("--completed")?.ok_or_else(|| usage("missing --completed"))?;
        Ok(Self {
            repository: required("--repository")?,
            operation: OperationId::parse(&required("--operation")?)?,
            tree: object("--tree")?.ok_or_else(|| usage("missing --tree"))?,
            checkpoint: object("--checkpoint")?,
            name: required("--name")?,
            conclusion,
            details_url: options.get("--details-url").cloned(),
            summary: options.get("--summary").cloned(),
            started: number("--started")?.unwrap_or(completed),
            completed,
            control: options.get("--control").map(PathBuf::from),
        })
    }

    fn message(&self) -> CheckReport {
        CheckReport {
            operation: self.operation.clone(),
            repository: self.repository.clone(),
            subject_tree: self.tree,
            subject_checkpoint: self.checkpoint,
            name: self.name.clone(),
            conclusion: self.conclusion,
            details_url: self.details_url.clone(),
            summary: self.summary.clone(),
            started_at: self.started,
            completed_at: self.completed,
        }
    }
}

fn usage(detail: &str) -> Error {
    Error::InvalidInput(format!("{detail}\n{USAGE}"))
}

fn text(value: &OsString) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| Error::InvalidInput("arguments must be UTF-8".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    const TREE: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn parses_a_complete_report() {
        let request = Request::parse(args(&[
            "--repository",
            "repo-1",
            "--operation",
            "report-1",
            "--tree",
            TREE,
            "--name",
            "ci/unit",
            "--conclusion",
            "timed_out",
            "--started",
            "10",
            "--completed",
            "20",
        ]))
        .unwrap();
        assert_eq!(request.conclusion, Conclusion::TimedOut);
        assert_eq!((request.started, request.completed), (10, 20));
        assert_eq!(request.message().name, "ci/unit");
    }

    #[test]
    fn rejects_bad_arguments_as_usage_errors() {
        let base = [
            "--repository",
            "repo-1",
            "--operation",
            "report-1",
            "--tree",
            TREE,
            "--name",
            "ci/unit",
        ];
        let with = |extra: &[&str]| {
            let mut all = base.to_vec();
            all.extend_from_slice(extra);
            Request::parse(args(&all))
        };
        assert!(matches!(
            with(&["--conclusion", "pending"]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(with(&[]), Err(Error::InvalidInput(_))));
        assert!(matches!(
            with(&["--conclusion", "success", "--bogus", "x"]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            with(&[
                "--conclusion",
                "success",
                "--completed",
                "20",
                "--started",
                "soon"
            ]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            with(&["--conclusion", "success"]),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn same_arguments_make_the_same_message_for_retry() {
        let parse = || {
            Request::parse(args(&[
                "--repository",
                "repo-1",
                "--operation",
                "report-1",
                "--tree",
                TREE,
                "--name",
                "ci/unit",
                "--conclusion",
                "success",
                "--completed",
                "20",
            ]))
            .unwrap()
        };
        let first = parse();
        assert_eq!((first.started, first.completed), (20, 20));
        assert_eq!(first.message(), parse().message());
    }

    #[test]
    fn a_node_refusal_is_an_outcome_with_exit_code_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let request = Request::parse(args(&[
            "--repository",
            "repo-1",
            "--operation",
            "report-1",
            "--tree",
            TREE,
            "--name",
            "ci/unit",
            "--conclusion",
            "success",
            "--completed",
            "20",
            "--control",
            dir.path().to_str().unwrap(),
        ]))
        .unwrap();
        let (value, _, code) = respond(&request, dir.path(), Some("token"));
        assert_eq!(code, 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["outcome"], "refused");
        // No control plane is initialized here, so the keys cannot be read.
        assert_eq!(value["code"], "not_initialized");
    }
}
