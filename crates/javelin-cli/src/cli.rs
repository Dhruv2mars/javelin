use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use javelin::{
    CheckpointId, Error, LayerHead, LayerId, LayerState, LayerUpdate, ObjectId, OperationId,
    PublicationReceipt, PublishOutcome, Repository, Result, VersionNumber,
};
use serde_json::{Value, json};

pub const HELP: &str = "Javelin native repository foundation\n\nUsage:\n  javelin <init|history|check> [project-directory] [--json]\n  javelin layer <command> ... [--repo DIR] [--json]\n  javelin workspace <command> ... --root ABS --helper ABS [--repo DIR] [--json]\n\nRepository:\n  init                     Create empty World 0; existing files stay untouched\n  history                  List accepted World versions\n  check                    Verify repository integrity\n\nLayer:\n  create ID [--world N]     Create from World N, or current World head\n  state ID                 Read current native state\n  history ID               List retained revisions and checkpoint IDs\n  publication --operation ID\n                           Inspect an original durable Publish receipt\n  diff ID [--revision N]    Show checkpointed path changes against the base World\n  clone ID --checkpoint HASH\n                           Create from an immutable checkpoint\n  stack ID --revision N --source HASH --operation ID\n                           Stack a frozen source checkpoint into exact target revision N\n  discard ID --revision N --operation ID\n                           Terminal discard using revision N as the CAS token\n\nLinux workspace (requires configured root-owned mount helper and sudo -n):\n  init                     Initialize runtime layout for this repository\n  mount ID                 Open or resume a writable Layer path\n  path ID                  Print an existing mounted writable path\n  unmount ID               Unmount, retaining private edits\n  checkpoint ID --operation ID\n                           Explicitly freeze, capture, and thaw the view\n  watch ID [--quiet-ms N] [--duration-ms N]\n                           Foreground stable checkpoints (250ms samples; 1000ms quiet)\n  publish ID --operation ID [--actor human|agent]\n             [--context HASH | --trace FILE] [--allow-missing-trace]\n                           Publish the whole explicit checkpoint; keep view read-only\n  discard ID --operation ID Remove a view through the runtime's bound head\n  rebind ID --revision OLD --to-revision NEW\n                           Explicitly select the newer immutable native head\n  recover                  Resolve pending operations and remove discarded views\n\nIDs for create/clone and operation IDs for mutations must be reused on retry.\nDiff reads immutable checkpoints, not live files. Checkpoint before Publish.\nWatch is opt-in; ignore rules are not implemented. Ordinary workspace paths require Linux; macOS and Windows are unsupported.\nSee docs/cli.md for setup, retry, and cleanup details.";

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

struct Args {
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
                if key == "--allow-missing-trace" {
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
        return workspace(action, args);
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
        "state" | "history" => &["--repo"],
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
    let mut repo = Repository::open(args.root()?)?;
    let command = format!("layer {action}");
    match action {
        "create" | "clone" => {
            let head = if let Some(checkpoint) = checkpoint {
                repo.clone_layer(&id, checkpoint)?
            } else {
                let version = match world {
                    Some(version) => version,
                    None => {
                        repo.history()?
                            .last()
                            .ok_or_else(|| Error::CorruptRepository("missing World head".into()))?
                            .number
                    }
                };
                let base = repo.layer_base(version)?;
                repo.create_layer(&id, &base)?
            };
            let state = LayerState::Active { head };
            Ok(success(
                json!({"ok": true, "command": command, "state": state}),
                describe(&state),
            ))
        }
        "state" => {
            let state = repo.layer_state(&id)?;
            Ok(success(
                json!({"ok": true, "command": command, "state": state}),
                describe(&state),
            ))
        }
        "history" => {
            let history = repo.layer_history(&id)?;
            let text = history.iter().map(describe).collect::<Vec<_>>().join("\n");
            Ok(success(
                json!({"ok": true, "command": command, "history": history}),
                text,
            ))
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
            Ok(success(
                json!({"ok": true, "command": command, "head": head, "changes": changes}),
                text,
            ))
        }
        "discard" => {
            let expected = historical_head(&repo, &id, revision.expect("required revision"))?;
            let operation = operation.expect("required operation");
            let outcome = repo.discard_layer(&operation, &expected)?;
            Ok(update(&command, &operation, outcome))
        }
        _ => unreachable!(),
    }
}

fn workspace(action: &str, args: Vec<OsString>) -> Result<Output> {
    let count = match action {
        "init" | "recover" => 0,
        "mount" | "path" | "unmount" | "checkpoint" | "discard" | "publish" | "rebind"
        | "watch" => 1,
        _ => {
            return Err(Error::InvalidInput(format!(
                "unknown workspace command {action}; see javelin --help"
            )));
        }
    };
    let allowed: &[&str] = if action == "publish" {
        &[
            "--repo",
            "--root",
            "--helper",
            "--operation",
            "--actor",
            "--context",
            "--trace",
            "--allow-missing-trace",
        ]
    } else if action == "watch" {
        &[
            "--repo",
            "--root",
            "--helper",
            "--quiet-ms",
            "--duration-ms",
        ]
    } else if action == "rebind" {
        &[
            "--repo",
            "--root",
            "--helper",
            "--revision",
            "--to-revision",
        ]
    } else if action == "checkpoint" || action == "discard" {
        &["--repo", "--root", "--helper", "--operation"]
    } else {
        &["--repo", "--root", "--helper"]
    };
    let args = Args::parse(args, allowed, count)?;
    let root = PathBuf::from(args.required("--root")?);
    let helper = PathBuf::from(args.required("--helper")?);
    let layer = if count == 1 {
        Some(args.layer()?)
    } else {
        None
    };
    let operation = if ["checkpoint", "discard", "publish"].contains(&action) {
        Some(args.operation()?)
    } else {
        None
    };
    let rebind = if action == "rebind" {
        args.required("--revision")?;
        args.required("--to-revision")?;
        Some((
            args.number("--revision")?.unwrap(),
            args.number("--to-revision")?.unwrap(),
        ))
    } else {
        None
    };
    let watch = if action == "watch" {
        let quiet = args.number("--quiet-ms")?.unwrap_or(1000);
        let duration = args.number("--duration-ms")?;
        if quiet < 250 || duration == Some(0) {
            return Err(Error::InvalidInput(
                "--quiet-ms must be >=250 and --duration-ms must be positive".into(),
            ));
        }
        Some((quiet, duration))
    } else {
        None
    };
    let attribution = AttributionInput::parse(&args)?;
    if !cfg!(target_os = "linux") {
        return Err(Error::Unsupported("workspace commands require Linux OverlayFS; macOS and Windows ordinary paths are not supported".into()));
    }
    #[cfg(unix)]
    {
        cli_linux::run(
            action,
            args.root()?,
            (root, helper),
            layer,
            operation,
            attribution,
            cli_linux::Options { rebind, watch },
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (
            root,
            helper,
            layer,
            operation,
            attribution.agent,
            attribution.context,
            attribution.trace,
            attribution.allow_missing_trace,
            rebind,
            watch,
        );
        unreachable!()
    }
}

#[cfg(unix)]
#[path = "cli_linux.rs"]
mod cli_linux;

// Parsing is platform independent; only the runtime caller stores raw context.
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
        Ok(Self {
            agent,
            context,
            trace,
            allow_missing_trace,
        })
    }

    #[cfg(unix)]
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

pub fn stack(args: Vec<OsString>) -> std::result::Result<Output, super::CliError> {
    let args = Args::parse(
        args,
        &["--repo", "--revision", "--source", "--operation"],
        1,
    )?;
    let id = args.layer()?;
    args.required("--revision")?;
    let revision = args.number("--revision")?.unwrap();
    let source = CheckpointId::parse(text(args.required("--source")?)?)?;
    let operation = args.operation()?;
    let mut repo = Repository::open(args.root()?)?;
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
    Ok((reply, text, code))
}
