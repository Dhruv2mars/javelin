use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use javelin::{
    Actor, Capture, CheckpointId, Error, LayerHead, LayerId, LayerState, LayerUpdate, ObjectId,
    OperationId, Origin, Provenance, PublicationReceipt, PublishOutcome, Radar, RepoPath,
    Repository, Result, VersionNumber,
};
use serde_json::{Value, json};

pub const HELP: &str = "Javelin native repository foundation\n\nUsage:\n  javelin <init|history|check> [project-directory] [--json]\n  javelin layer <command> ... [--repo DIR] [--json]\n  javelin checkpoint ID [--path DIR] [PROVENANCE] [--repo DIR] [--json]\n  javelin blame PATH [--world N] [--lines] [--repo DIR] [--json]\n  javelin workspace <command> ... [--repo DIR] [--json]\n\nRepository:\n  init                     Create empty World 0; existing files stay untouched\n  history                  List accepted World versions\n  check                    Verify repository integrity\n  blame PATH [--world N] [--lines]\n                           Which World version, Layer checkpoint and session last\n                           changed PATH; --lines attributes each line\n\nCapture:\n  checkpoint ID [--path DIR]\n                           Capture the Layer directory (default: its workspace, else\n                           the current directory) when it differs from the head;\n                           agent lifecycle hook. Layer and workspace commands for an\n                           ID capture its workspace directory first.\n\nProvenance (recorded in every checkpoint a capture or stack writes):\n  --actor human|agent --agent NAME --session ID --reference REF\n                           Flags on checkpoint and layer stack; otherwise from\n                           JAVELIN_ACTOR, JAVELIN_AGENT, JAVELIN_SESSION and\n                           JAVELIN_REFERENCE. An agent name implies --actor agent.\n\nLayer:\n  create ID [--world N]     Create from World N, or current World head\n  state ID                 Read current native state\n  history ID               List retained revisions and checkpoint IDs\n  publication --operation ID\n                           Inspect an original durable Publish receipt\n  diff ID [--revision N]    Show checkpointed path changes against the base World\n  radar ID                 Paths that would make Publish conflict: World advances\n                           since the base, and each other live Layer if it\n                           published first\n  clone ID --checkpoint HASH\n                           Create from an immutable checkpoint\n  stack ID --revision N --source HASH --operation ID\n                           Stack a frozen source checkpoint into exact target revision N\n  discard ID --revision N --operation ID\n                           Terminal discard using revision N as the CAS token\n\nWorkspace (a plain Layer directory cloned from its World base; no mount or daemon):\n  create ID [--world N]     Create the Layer if needed and open its directory\n  open ID                  Open the directory of an existing active Layer\n  path ID                  Print the Layer directory and its base\n  discard ID --operation ID Discard the Layer; its directory goes to trash\n  publish ID --operation ID [--actor human|agent]\n             [--context HASH | --trace FILE] [--allow-missing-trace]\n                           Capture, then Publish the whole Layer onto the World\n  recover                  Finish interrupted steps and reclaim all trash\n\nIDs for create/clone and operation IDs for mutations must be reused on retry.\nDiff reads immutable checkpoints, not live files. Checkpoint before Publish.\nSee docs/cli.md for setup, retry, and cleanup details.";

pub type Output = (Value, String, u8);

fn success(value: Value, text: String) -> Output {
    (value, text, 0)
}

pub(super) fn update(command: &str, operation: &OperationId, update: LayerUpdate) -> Output {
    let applied = matches!(update, LayerUpdate::Applied { .. });
    let text = format!(
        "Operation {}: {}\n{}",
        operation.as_str(),
        if applied {
            "applied"
        } else {
            "stale; nothing applied"
        },
        match &update {
            LayerUpdate::Applied { state } | LayerUpdate::Stale { state } => describe(state),
        }
    );
    (
        json!({"ok": applied, "command": command, "operation": operation, "update": update}),
        text,
        if applied { 0 } else { 3 },
    )
}

pub(super) const BEHIND: &str = "the Layer head came from layer stack, which does not update its \
     directory; capturing the directory would revert the stacked work, so nothing was captured";

fn capture_detail(capture: &Capture) -> Value {
    json!({
        "operation": capture.operation,
        "update": capture.update,
        "hashed": capture.hashed,
        "uncertain": capture.uncertain,
        "rebuilt": capture.rebuilt,
        "behind": capture.behind,
    })
}

fn uncertain_text(capture: &Capture) -> Option<String> {
    (!capture.uncertain.is_empty()).then(|| {
        let paths: Vec<_> = capture.uncertain.iter().map(|p| p.as_str()).collect();
        format!(
            "Still changing, kept at their prior state: {}",
            paths.join(", ")
        )
    })
}

/// Adds the capture that ran before a command, if any, to its output.
pub(super) fn captured(capture: Option<Capture>, mut output: Output) -> Output {
    let Some(capture) = capture else {
        return output;
    };
    let mut lines = Vec::new();
    match (&capture.update, &capture.operation) {
        (Some(LayerUpdate::Applied { state }), Some(operation)) => lines.push(format!(
            "Captured workspace edits (operation {}): {}",
            operation.as_str(),
            describe(state)
        )),
        (Some(LayerUpdate::Stale { .. }), _) => {
            lines.push("Workspace capture was stale; edits stay in the directory".into())
        }
        _ if capture.behind => lines.push(format!("Workspace not captured: {BEHIND}")),
        _ => {}
    }
    lines.extend(uncertain_text(&capture));
    output.0["capture"] = capture_detail(&capture);
    if !lines.is_empty() {
        lines.push(std::mem::take(&mut output.1));
        output.1 = lines.join("\n");
    }
    output
}

pub(super) struct Args {
    positionals: Vec<OsString>,
    options: BTreeMap<String, OsString>,
}

impl Args {
    fn parse(args: Vec<OsString>, allowed: &[&str], count: usize) -> Result<Self> {
        let mut parsed = Self {
            positionals: Vec::new(),
            options: BTreeMap::new(),
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg.to_string_lossy().starts_with('-') {
                let key = text(&arg)?;
                if !allowed.contains(&key) || parsed.options.contains_key(key) {
                    return Err(Error::InvalidInput(format!(
                        "unknown or repeated option {key}"
                    )));
                }
                if key == "--allow-missing-trace" || key == "--lines" {
                    parsed.options.insert(key.into(), "true".into());
                    continue;
                }
                let value = args
                    .next()
                    .filter(|a| !a.to_string_lossy().starts_with('-'))
                    .ok_or_else(|| Error::InvalidInput(format!("{key} needs a value")))?;
                parsed.options.insert(key.into(), value);
            } else {
                parsed.positionals.push(arg);
            }
        }
        if parsed.positionals.len() != count {
            return Err(Error::InvalidInput(format!(
                "expected {count} positional arguments; see javelin --help"
            )));
        }
        Ok(parsed)
    }

    fn required(&self, key: &str) -> Result<&OsString> {
        self.options
            .get(key)
            .ok_or_else(|| Error::InvalidInput(format!("missing {key}")))
    }

    fn number(&self, key: &str) -> Result<Option<u64>> {
        self.options
            .get(key)
            .map(|value| {
                let value = text(value)?;
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::InvalidInput(format!(
                        "{key} needs a nonnegative integer"
                    )));
                }
                value
                    .parse()
                    .map_err(|_| Error::InvalidInput(format!("{key} is too large")))
            })
            .transpose()
    }

    fn root(&self) -> Result<PathBuf> {
        match self.options.get("--repo") {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(std::env::current_dir()?),
        }
    }

    fn layer(&self) -> Result<LayerId> {
        LayerId::parse(text(&self.positionals[0])?)
    }

    fn operation(&self) -> Result<OperationId> {
        OperationId::parse(text(self.required("--operation")?)?)
    }
}

fn text(value: &OsString) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| Error::InvalidInput("command, IDs, and option names must be UTF-8".into()))
}

pub fn run(mut args: Vec<OsString>) -> Result<Output> {
    let group = args.remove(0);
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        return Ok(success(json!({"ok": true, "help": HELP}), HELP.into()));
    }
    if args.is_empty() {
        return Err(Error::InvalidInput(
            "expected a subcommand; see javelin --help".into(),
        ));
    }
    let action = args.remove(0);
    let action = text(&action)?;
    if group == "workspace" {
        return cli_workspace::run(action, args);
    }
    layer(action, args)
}

fn describe(state: &LayerState) -> String {
    match state {
        LayerState::Active { head } => format!(
            "Layer {} active revision {} checkpoint {}",
            head.layer().as_str(),
            head.revision(),
            head.checkpoint().object()
        ),
        LayerState::Published {
            head,
            operation,
            version,
        } => format!(
            "Layer {} published as World {} operation {} revision {} checkpoint {}",
            head.layer().as_str(),
            version.number,
            operation,
            head.revision(),
            head.checkpoint().object()
        ),
        LayerState::Discarded {
            layer,
            revision,
            checkpoint,
        } => format!(
            "Layer {} discarded revision {} checkpoint {}",
            layer.as_str(),
            revision,
            checkpoint.object()
        ),
    }
}

pub(super) fn historical_head(
    repo: &Repository,
    layer: &LayerId,
    revision: u64,
) -> Result<LayerHead> {
    repo.layer_history(layer)?
        .into_iter()
        .find_map(|state| match state {
            LayerState::Active { head } if head.revision() == revision => Some(head),
            _ => None,
        })
        .ok_or_else(|| Error::InvalidInput(format!("no active Layer revision {revision}")))
}

fn layer(action: &str, args: Vec<OsString>) -> Result<Output> {
    let allowed: &[&str] = match action {
        "publication" => &["--repo", "--operation"],
        "create" => &["--repo", "--world"],
        "clone" => &["--repo", "--checkpoint"],
        "state" | "history" | "radar" => &["--repo"],
        "diff" => &["--repo", "--revision"],
        "discard" => &["--repo", "--revision", "--operation"],
        _ => {
            return Err(Error::InvalidInput(format!(
                "unknown layer command {action}; see javelin --help"
            )));
        }
    };
    if action == "publication" {
        let args = Args::parse(args, allowed, 0)?;
        let operation = args.operation()?;
        let repo = Repository::open(args.root()?)?;
        return match repo.publication(&operation)? {
            Some(receipt) => Ok(publication("layer publication", receipt)),
            None => Ok(success(
                json!({"ok": true, "command": "layer publication", "operation": operation, "receipt": null}),
                format!("No publication receipt for {}", operation.as_str()),
            )),
        };
    }
    let args = Args::parse(args, allowed, 1)?;
    let id = args.layer()?;
    let world = args
        .number("--world")?
        .map(VersionNumber::new)
        .transpose()?;
    let revision = args.number("--revision")?;
    let checkpoint = if action == "clone" {
        Some(CheckpointId::parse(text(args.required("--checkpoint")?)?)?)
    } else {
        None
    };
    let operation = if action == "discard" {
        args.required("--revision")?;
        Some(args.operation()?)
    } else {
        None
    };
    let mut repo = open(&args)?;
    let capture = repo.capture_workspace(&id)?;
    let command = format!("layer {action}");
    let output = match action {
        "create" | "clone" => {
            let head = if let Some(checkpoint) = checkpoint {
                repo.clone_layer(&id, checkpoint)?
            } else {
                let version = match world {
                    Some(version) => version,
                    None => repo.head()?.number,
                };
                let base = repo.layer_base(version)?;
                repo.create_layer(&id, &base)?
            };
            let state = LayerState::Active { head };
            success(
                json!({"ok": true, "command": command, "state": state}),
                describe(&state),
            )
        }
        "state" => {
            let state = repo.layer_state(&id)?;
            success(
                json!({"ok": true, "command": command, "state": state}),
                describe(&state),
            )
        }
        "radar" => {
            let radar = repo.radar(&id)?;
            success(
                json!({"ok": true, "command": command, "clear": radar.clear(), "radar": radar}),
                radar_text(&radar),
            )
        }
        "history" => {
            let history = repo.layer_history(&id)?;
            let text = history.iter().map(describe).collect::<Vec<_>>().join("\n");
            success(
                json!({"ok": true, "command": command, "history": history}),
                text,
            )
        }
        "diff" => {
            let head = match revision {
                Some(revision) => historical_head(&repo, &id, revision)?,
                None => repo
                    .layer_history(&id)?
                    .into_iter()
                    .rev()
                    .find_map(|state| match state {
                        LayerState::Active { head } => Some(head),
                        _ => None,
                    })
                    .ok_or_else(|| Error::InvalidInput("Layer has no active history".into()))?,
            };
            let changes = repo.layer_diff(&head)?;
            let text = if changes.is_empty() {
                "No checkpointed changes.".into()
            } else {
                changes
                    .iter()
                    .map(|change| {
                        format!(
                            "{} {}",
                            match (&change.before, &change.after) {
                                (None, _) => "A",
                                (_, None) => "D",
                                _ => "M",
                            },
                            serde_json::to_string(change.path.as_str()).expect("path encoding")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            success(
                json!({"ok": true, "command": command, "head": head, "changes": changes}),
                text,
            )
        }
        "discard" => {
            let expected = historical_head(&repo, &id, revision.expect("required revision"))?;
            let operation = operation.expect("required operation");
            let outcome = repo.discard_layer(&operation, &expected)?;
            update(&command, &operation, outcome)
        }
        _ => unreachable!(),
    };
    Ok(captured(capture, output))
}

#[path = "cli_workspace.rs"]
mod cli_workspace;

/// Publish attribution as typed; `store` saves a trace file as a context Blob.
struct AttributionInput {
    agent: bool,
    context: Option<ObjectId>,
    trace: Option<PathBuf>,
    allow_missing_trace: bool,
}

impl AttributionInput {
    fn parse(args: &Args) -> Result<Self> {
        let agent = match args.options.get("--actor").map(text).transpose()? {
            None | Some("human") => false,
            Some("agent") => true,
            _ => return Err(Error::InvalidInput("--actor must be human or agent".into())),
        };
        let context = args
            .options
            .get("--context")
            .map(|v| ObjectId::parse(text(v)?))
            .transpose()?;
        let trace = args.options.get("--trace").map(PathBuf::from);
        let allow_missing_trace = args.options.contains_key("--allow-missing-trace");
        if (!agent && (context.is_some() || trace.is_some() || allow_missing_trace))
            || (context.is_some() && trace.is_some())
        {
            return Err(Error::InvalidInput(
                "context/trace/override require --actor agent; choose either --context or --trace"
                    .into(),
            ));
        }
        if allow_missing_trace && (context.is_some() || trace.is_some()) {
            return Err(Error::InvalidInput(
                "--allow-missing-trace applies only without --context or --trace".into(),
            ));
        }
        Ok(Self {
            agent,
            context,
            trace,
            allow_missing_trace,
        })
    }

    fn store(self, repo: &mut Repository) -> Result<javelin::PublishAttribution> {
        if !self.agent {
            return Ok(javelin::PublishAttribution::Human);
        }
        let context = match self.trace {
            Some(path) => Some(repo.put_blob(&std::fs::read(path)?)?),
            None => self.context,
        };
        Ok(javelin::PublishAttribution::Agent {
            context,
            allow_missing_trace: self.allow_missing_trace,
        })
    }
}

fn publication(command: &str, receipt: PublicationReceipt) -> Output {
    let (ok, code, outcome) = match &receipt.outcome {
        PublishOutcome::Published { version } => {
            (true, 0, format!("published as World {}", version.number))
        }
        PublishOutcome::Conflict { paths } => (
            false,
            3,
            format!(
                "conflict; nothing published\n{}",
                paths
                    .iter()
                    .map(|p| serde_json::to_string(p.as_str()).expect("path encoding"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        ),
        PublishOutcome::LayerStale { .. } => (false, 3, "stale; nothing published".into()),
    };
    let mut text = format!(
        "Operation {}: {}\nLayer {} revision {} checkpoint {}",
        receipt.operation.as_str(),
        outcome,
        receipt.expected.layer().as_str(),
        receipt.expected.revision(),
        receipt.expected.checkpoint().object()
    );
    for warning in &receipt.warnings {
        text.push_str(&format!(
            "\nWarning: {}",
            serde_json::to_string(warning).expect("warning encoding")
        ));
    }
    (
        json!({"ok": ok, "command": command, "receipt": receipt}),
        text,
        code,
    )
}

/// Daemonless capture: scan the Layer directory and checkpoint any difference.
pub fn checkpoint(args: Vec<OsString>) -> Result<Output> {
    let allowed = [&["--repo", "--path"][..], PROVENANCE].concat();
    let args = Args::parse(args, &allowed, 1)?;
    let id = args.layer()?;
    let mut repo = open(&args)?;
    // Never default to a subdirectory of the Layer: everything outside it would read
    // as deleted. A clone-backed Layer captures its own directory.
    let workspace = repo.workspace_root(&id)?;
    let path = match args.options.get("--path") {
        Some(path) => PathBuf::from(path),
        None if workspace.try_exists()? => workspace,
        None => std::env::current_dir()?,
    };
    let capture = repo.capture_directory(&id, &path)?;
    if capture.behind {
        return Err(Error::InvalidInput(BEHIND.into()));
    }
    let detail = capture_detail(&capture);
    let mut output = match (capture.update.clone(), capture.operation.clone()) {
        (Some(outcome), Some(operation)) => update("checkpoint", &operation, outcome),
        _ => {
            let state = LayerState::Active {
                head: capture.head.clone(),
            };
            success(
                json!({"ok": true, "command": "checkpoint", "unchanged": true, "state": state}),
                format!("Unchanged; {}", describe(&state)),
            )
        }
    };
    output.0["capture"] = detail;
    if let Some(text) = uncertain_text(&capture) {
        output.1 = format!("{}\n{text}", output.1);
    }
    // The lifecycle hook is the earliest point to warn about a future conflict.
    match repo.radar(&id) {
        Ok(radar) => {
            if !radar.clear() {
                output.1 = format!("{}\n{}", output.1, radar_text(&radar));
            }
            output.0["radar"] = json!(radar);
        }
        // Published or discarded since the capture: nothing can conflict.
        Err(Error::InvalidInput(_)) => {}
        Err(error) => return Err(error),
    }
    Ok(output)
}

fn quoted(path: &javelin::RepoPath) -> String {
    serde_json::to_string(path.as_str()).expect("path encoding")
}

fn radar_text(radar: &Radar) -> String {
    let layer = radar.head.layer().as_str();
    if radar.clear() {
        return format!(
            "Radar clear: Layer {layer} has no World or Layer collisions at World {}",
            radar.world.version
        );
    }
    let mut lines = vec![format!(
        "Radar: Layer {layer} revision {} would conflict",
        radar.head.revision()
    )];
    if !radar.world.paths.is_empty() {
        let paths: Vec<_> = radar.world.paths.iter().map(quoted).collect();
        lines.push(format!(
            "  World {} since base World {}: {}",
            radar.world.version,
            radar.base,
            paths.join(", ")
        ));
    }
    for collision in &radar.layers {
        let pairs: Vec<_> = collision
            .paths
            .iter()
            .map(|pair| {
                if pair.ours == pair.theirs {
                    quoted(&pair.ours)
                } else {
                    format!("{} with {}", quoted(&pair.ours), quoted(&pair.theirs))
                }
            })
            .collect();
        lines.push(format!(
            "  Layer {} if it publishes first: {}",
            collision.layer.as_str(),
            pairs.join(", ")
        ));
    }
    lines.join("\n")
}

/// `adopt [project-directory] --operation ID`: one command from an existing project
/// directory to a repository whose World 1 holds its files. Retry with the same operation.
pub fn adopt(args: Vec<OsString>) -> Result<Output> {
    let mut root = None;
    let mut operation = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--operation" {
            let value = args
                .next()
                .ok_or_else(|| Error::InvalidInput("--operation needs a value".into()))?;
            if operation.replace(value).is_some() {
                return Err(Error::InvalidInput("repeated option --operation".into()));
            }
        } else if arg.to_string_lossy().starts_with('-') || root.replace(arg).is_some() {
            return Err(Error::InvalidInput(
                "expected adopt [project-directory] --operation ID".into(),
            ));
        }
    }
    let operation = operation.ok_or_else(|| Error::InvalidInput("missing --operation".into()))?;
    let operation = OperationId::parse(text(&operation)?)?;
    let root = match root {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
    let adoption = Repository::adopt(&root, &operation)?;
    let files = adoption.files;
    let (mut value, mut text, code) = publication("adopt", adoption.receipt);
    value["files"] = json!(files);
    if let (Some(files), 0) = (files, code) {
        text = format!(
            "Adopted {files} files through Layer {}; the project directory was not changed.\n{text}",
            javelin::ADOPTION_LAYER
        );
    }
    Ok((value, text, code))
}

pub fn stack(args: Vec<OsString>) -> std::result::Result<Output, super::CliError> {
    let allowed = [
        &["--repo", "--revision", "--source", "--operation"][..],
        PROVENANCE,
    ]
    .concat();
    let args = Args::parse(args, &allowed, 1)?;
    let id = args.layer()?;
    args.required("--revision")?;
    let revision = args.number("--revision")?.unwrap();
    let source = CheckpointId::parse(text(args.required("--source")?)?)?;
    let operation = args.operation()?;
    let mut repo = open(&args)?;
    let capture = repo.capture_workspace(&id)?;
    let target = historical_head(&repo, &id, revision)?;
    let outcome = javelin_stack::stack_layer(&mut repo, &operation, &target, source)?;
    let (mut reply, text, code) = match outcome {
        javelin_stack::StackOutcome::Updated(outcome) => update("layer stack", &operation, outcome),
        javelin_stack::StackOutcome::Conflicts(conflicts) => {
            let conflicts: Vec<_> = conflicts
                .into_iter()
                .map(|c| {
                    json!({
                        "path": c.path, "kind": format!("{:?}", c.kind),
                        "base": c.base, "ours": c.ours, "theirs": c.theirs, "with": c.with,
                    })
                })
                .collect();
            let text = format!(
                "Operation {}: conflicts; nothing applied\n{}",
                operation.as_str(),
                conflicts
                    .iter()
                    .map(|c| format!("{} {}", c["kind"], c["path"]))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            (
                json!({"ok": false, "command": "layer stack", "operation": operation, "conflicts": conflicts}),
                text,
                3,
            )
        }
    };
    reply["target"] = json!(target);
    reply["source"] = json!(source);
    let text = format!(
        "{text}\nTarget {} revision {} checkpoint {}\nSource checkpoint {}",
        id.as_str(),
        target.revision(),
        target.checkpoint().object(),
        source.object()
    );
    Ok(captured(capture, (reply, text, code)))
}

const PROVENANCE: &[&str] = &["--actor", "--agent", "--session", "--reference"];

/// Opens the repository acting for the declared provenance, so every capture and
/// Stack the command runs records it.
pub(super) fn open(args: &Args) -> Result<Repository> {
    let mut repo = Repository::open(args.root()?)?;
    repo.set_provenance(provenance(Some(args))?);
    Ok(repo)
}

/// Each field comes from its flag, else its `JAVELIN_*` variable. Nothing declared
/// leaves checkpoints unattributed.
pub(super) fn provenance(args: Option<&Args>) -> Result<Option<Provenance>> {
    let field = |flag: &str, variable: &str| -> Result<Option<String>> {
        match args.and_then(|a| a.options.get(flag)) {
            Some(value) => Ok(Some(text(value)?.to_owned())),
            None => match std::env::var(variable) {
                Ok(value) if !value.is_empty() => Ok(Some(value)),
                Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
                Err(_) => Err(Error::InvalidInput(format!("{variable} must be UTF-8"))),
            },
        }
    };
    let actor = field("--actor", "JAVELIN_ACTOR")?;
    let agent = field("--agent", "JAVELIN_AGENT")?;
    let session = field("--session", "JAVELIN_SESSION")?;
    let reference = field("--reference", "JAVELIN_REFERENCE")?;
    let actor = match actor.as_deref() {
        Some("human") => Actor::Human,
        Some("agent") => Actor::Agent,
        Some(_) => return Err(Error::InvalidInput("actor must be human or agent".into())),
        None if agent.is_some() => Actor::Agent,
        None if session.is_some() || reference.is_some() => Actor::Human,
        None => return Ok(None),
    };
    Provenance::new(actor, agent, session, reference).map(Some)
}

fn who(origin: &Origin) -> String {
    match (&origin.checkpoint, &origin.provenance) {
        (None, _) => "no Layer".into(),
        (Some(_), None) => "unattributed".into(),
        (Some(_), Some(p)) => {
            let mut parts = vec![match p.actor {
                Actor::Human => "human".to_string(),
                Actor::Agent => p.agent.clone().unwrap_or_else(|| "agent".into()),
            }];
            parts.extend(p.session.as_ref().map(|s| format!("session {s}")));
            parts.extend(p.reference.as_ref().map(|r| format!("ref {r}")));
            parts.join(" ")
        }
    }
}

fn origin_text(origin: &Origin) -> String {
    let mut text = format!("World {}", origin.version.number);
    if let Some(layer) = &origin.layer {
        text.push_str(&format!(" Layer {}", layer.as_str()));
    }
    if let Some(checkpoint) = &origin.checkpoint {
        text.push_str(&format!(" checkpoint {}", checkpoint.object()));
    }
    format!("{text}: {}", who(origin))
}

/// `javelin blame PATH [--world N] [--lines]`: World provenance, read-only.
pub fn blame(args: Vec<OsString>) -> Result<Output> {
    let args = Args::parse(args, &["--repo", "--world", "--lines"], 1)?;
    let path = RepoPath::parse(text(&args.positionals[0])?)?;
    let world = args
        .number("--world")?
        .map(VersionNumber::new)
        .transpose()?;
    let repo = Repository::open(args.root()?)?;
    let quoted = quoted(&path);
    if args.options.contains_key("--lines") {
        let blame = repo.blame_lines(&path, world)?;
        let text = match &blame {
            None => format!("No file at {quoted}"),
            Some(blame) => blame
                .lines
                .iter()
                .map(|line| {
                    let origin = &blame.origins[line.origin];
                    format!(
                        "{:>5} {:<32} {:>5}| {}",
                        origin.version.number,
                        who(origin),
                        line.number,
                        line.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        return Ok(success(
            json!({"ok": true, "command": "blame", "lines": blame}),
            text,
        ));
    }
    let blame = repo.blame(&path, world)?;
    let text = match &blame {
        None => format!("No history for {quoted}"),
        Some(blame) => format!(
            "{quoted} {} in {}",
            if blame.entry.is_some() {
                "last changed"
            } else {
                "deleted"
            },
            origin_text(&blame.origin)
        ),
    };
    Ok(success(
        json!({"ok": true, "command": "blame", "blame": blame}),
        text,
    ))
}
