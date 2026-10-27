//! `javelin request` runs the Publish Request loop on the attached node, and `javelin sync`
//! keeps the local World current with it. See `docs/online-design.md` sections 4.1, 4.4 and
//! 4.8, and `docs/cli.md`.
//!
//! The node holds requests, revisions, approvals and comments; a revision is a checkpoint of
//! the author's Layer on the node. `open` and `revise` therefore upload the local Layer's
//! changes first: `LayerCreate` over the remote World the Layer's base holds, the blobs and
//! changes object, then `LayerAppend`. Every command takes an operation ID that the node
//! records, so a retry returns the original outcome.
//!
//! What queues follows D7. `revise` and `publish` go through the outbox: the records are
//! journaled, then delivered, and a node outage leaves them queued for `javelin sync` or
//! `javelin outbox replay`. A queued revision names the node's next checkpoint before the
//! node is reachable ([`Repository::foreign_capture`]), from the node head this clone last
//! saw for the Layer, kept in `.javelin/requests.json`. `open`, `approve` and `comment` need a
//! live answer and refuse with `node_unavailable`.
//!
//! `sync` replays the outbox, then fetches the node's head verified from the attached head
//! (the trust anchor), stores what is missing, advances the local World and the rolling base,
//! and rewrites `attach.json`. A local World that moved away from the attachment is refused
//! with `local_world_diverged`: sync never replaces local history.

use std::{
    cell::RefCell,
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use javelin::{
    AttachedWorld, Attachment, Error, LayerId, LayerState, ObjectId, OperationId, RemoteHead,
    Repository, SparseChanges, VersionNumber,
    outbox::{Fault, Kind as Record, Outbox},
    tree::{self, MemoryStore},
};
use javelin_protocol::{
    Kind, Message,
    codec::{
        LayerOutcome, LayerStatus, PublishOutcome, RequestId, RequestStatus, ReviewDetail,
        ReviewOutcome, code, decode, encode,
    },
    fetch::{Anchor, fetch},
    limits::{MAX_BLOB, MAX_FRAME, MAX_OBJECTS, MAX_TREE, MAX_WANT},
    sender,
};
use serde_json::{Value, json};

use crate::{
    CliError,
    cli::Output,
    cli_remote::{
        Down, Link, NodeTransport, RemoteError, RepoBlobs, Session, Up, call, parse_target, remote,
        unexpected,
    },
};

/// Local refusal: the local World moved away from the attached remote World.
pub const LOCAL_WORLD_DIVERGED: &str = "local_world_diverged";
/// Where this clone keeps what it last saw of the requests it authored.
const REQUESTS: &str = "requests.json";

type Outcome = std::result::Result<Output, CliError>;
/// One outbox record to enqueue: its kind, scope, operation and wire message.
type Queued = (Record, String, OperationId, Vec<u8>);

/// Parsed `request` options. Each may appear once.
struct Options {
    positionals: Vec<String>,
    values: std::collections::BTreeMap<String, String>,
}

impl Options {
    fn parse(args: Vec<OsString>, allowed: &[&str], usage: &str) -> Result<Self, Error> {
        let invalid = || Error::InvalidInput(format!("expected: {usage}"));
        let mut parsed = Self {
            positionals: Vec::new(),
            values: Default::default(),
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let arg = arg
                .into_string()
                .map_err(|_| Error::InvalidInput("arguments must be UTF-8".into()))?;
            if arg.starts_with("--") {
                if !allowed.contains(&arg.as_str()) || parsed.values.contains_key(&arg) {
                    return Err(Error::InvalidInput(format!(
                        "unknown or repeated option {arg}; expected: {usage}"
                    )));
                }
                let value = args
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| Error::InvalidInput(format!("{arg} needs a value")))?;
                parsed.values.insert(arg, value);
            } else if arg.starts_with('-') {
                return Err(invalid());
            } else {
                parsed.positionals.push(arg);
            }
        }
        Ok(parsed)
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn required(&self, key: &str) -> Result<&str, Error> {
        self.get(key)
            .ok_or_else(|| Error::InvalidInput(format!("missing {key}")))
    }

    fn number<T: std::str::FromStr>(&self, key: &str) -> Result<Option<T>, Error> {
        self.get(key)
            .map(|value| {
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

    fn operation(&self) -> Result<OperationId, Error> {
        OperationId::parse(self.required("--operation")?)
    }

    fn root(&self) -> Result<PathBuf, Error> {
        match self.get("--repo") {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(std::env::current_dir()?),
        }
    }
}

/// The attached repository: its store, attachment and root.
struct Attached {
    root: PathBuf,
    repo: Repository,
    attachment: Attachment,
}

impl Attached {
    fn open(root: PathBuf) -> Result<Self, CliError> {
        let repo = Repository::open(&root)?;
        let attachment = repo.attachment()?.ok_or_else(|| {
            Error::InvalidInput("this repository is not attached to a node; attach first".into())
        })?;
        Ok(Self {
            root,
            repo,
            attachment,
        })
    }

    /// A session for reads, which go out anonymously when no token can be had.
    fn reading(&self) -> Result<RefCell<Link>, CliError> {
        Ok(Link::new(self.target()?))
    }

    /// A session for writes, which never go out without a token.
    fn writing(&self) -> Result<RefCell<Link>, CliError> {
        Ok(Link::writing(self.target()?))
    }

    fn target(&self) -> Result<crate::cli_remote::Target, CliError> {
        Ok(parse_target(&format!(
            "{}/{}",
            self.attachment.node, self.attachment.repository
        ))?)
    }

    fn outbox(&self) -> Result<Outbox, CliError> {
        Ok(Outbox::open(&self.root, &self.attachment.repository)?)
    }

    fn transport(&self) -> Result<NodeTransport, CliError> {
        Ok(NodeTransport::new(
            &self.attachment.node,
            &self.attachment.repository,
        )?)
    }
}

/// `javelin request ACTION ...`
pub fn request(args: Vec<OsString>) -> Outcome {
    let mut args = args.into_iter();
    let action = args
        .next()
        .and_then(|action| action.into_string().ok())
        .unwrap_or_default();
    let args: Vec<_> = args.collect();
    match action.as_str() {
        "open" => open(args),
        "revise" => revise(args),
        "comment" => comment(args),
        "approve" => approve(args),
        "show" => show(args),
        "list" => list(args),
        "publish" => publish(args),
        _ => Err(Error::InvalidInput(
            "expected: javelin request open|revise|comment|approve|show|list|publish; see javelin --help"
                .into(),
        )
        .into()),
    }
}

fn request_id(options: &Options, usage: &str) -> Result<RequestId, CliError> {
    match options.positionals.as_slice() {
        [id] => RequestId::parse(id).map_err(|_| {
            Error::InvalidInput(
                "a request ID has 1 to 128 ASCII letters, digits, `-`, `_`, `.` or `:`".into(),
            )
            .into()
        }),
        _ => Err(Error::InvalidInput(format!("expected: {usage}")).into()),
    }
}

/// `request open ID --layer LAYER --operation OP`: uploads the Layer's changes to the node
/// and opens a request whose revision 1 is that checkpoint.
fn open(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request open ID --layer LAYER --operation OP [--repo DIR] [--json]";
    let options = Options::parse(args, &["--layer", "--operation", "--repo"], usage)?;
    let request = request_id(&options, usage)?;
    let layer = LayerId::parse(options.required("--layer")?)?;
    let operation = options.operation()?;
    let mut attached = Attached::open(options.root()?)?;
    let (capture, changes, base) = local_changes(&mut attached.repo, &layer)?;
    let link = attached.writing()?;
    let node = upload(&link, &attached.repo, &layer, &base, &changes, &operation)?;
    let reply = call(
        &link,
        &Message::OpenRequest {
            operation: operation.clone(),
            request: request.clone(),
            layer: layer.clone(),
            checkpoint: node.checkpoint,
            base: VersionNumber::new(base.number)?,
        },
    )
    .map_err(CliError::Remote)?;
    let revision = review_outcome(reply)?;
    save_record(&attached.root, &request, &layer, &base, &node.bytes)?;
    let value = json!({
        "ok": true,
        "command": "request open",
        "request": request.as_str(),
        "layer": layer,
        "operation": operation,
        "revision": revision,
        "checkpoint": node.checkpoint,
        "base": base,
    });
    let text = format!(
        "Opened request {} from Layer {} as revision {revision} (checkpoint {}, over remote World {}).",
        request.as_str(),
        layer.as_str(),
        node.checkpoint,
        base.number
    );
    Ok(crate::cli::captured(capture, (value, text, 0)))
}

/// `request revise ID --revision N --operation OP [--layer LAYER]`: submits the Layer's
/// current changes as revision N + 1. Queued while the node is unavailable.
fn revise(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request revise ID --revision N --operation OP [--layer LAYER] [--repo DIR] [--json]";
    let options = Options::parse(
        args,
        &["--revision", "--operation", "--layer", "--repo"],
        usage,
    )?;
    let request = request_id(&options, usage)?;
    let expected: u32 = options
        .number("--revision")?
        .ok_or_else(|| Error::InvalidInput("missing --revision".into()))?;
    let operation = options.operation()?;
    let mut attached = Attached::open(options.root()?)?;
    let mut outbox = attached.outbox()?;
    // A retry delivers what the first attempt queued, unchanged.
    if !queued(&outbox, Record::Revise, &operation) {
        let record = load_record(&attached.root, &request)?;
        let link = attached.writing()?;
        let layer = match (options.get("--layer"), &record) {
            (Some(layer), _) => LayerId::parse(layer)?,
            (None, Some(record)) => record.layer.clone(),
            (None, None) => read_review(&link, &request, 0)?.summary.layer,
        };
        let (_, changes, base) = local_changes(&mut attached.repo, &layer)?;
        // The node's head for the Layer, read now, or as this clone last saw it.
        let state = create_layer(&link, &layer, &base).and_then(|()| node_head(&link, &layer));
        let (head, bytes, online) = match state {
            Ok((head, bytes)) => (head, bytes, true),
            Err(error) if error.code == code::NODE_UNAVAILABLE => match &record {
                Some(record) if record.layer == layer => (
                    ObjectId::for_checkpoint(&record.head),
                    record.head.clone(),
                    false,
                ),
                _ => {
                    return Err(CliError::Remote(RemoteError::new(
                        code::NODE_UNAVAILABLE,
                        format!(
                            "{}; this clone has not seen Layer {} on the node, so the revision \
                             cannot be queued. Retry when the node is back",
                            error.detail,
                            layer.as_str()
                        ),
                    )));
                }
            },
            Err(error) => return Err(CliError::Remote(error)),
        };
        let mut records = Vec::new();
        let (checkpoint, next) = if Repository::foreign_checkpoint_changes(&bytes)? == changes {
            (head, bytes)
        } else {
            let next = Repository::foreign_capture(&bytes, &changes)?;
            let link = online.then_some(&link);
            records.extend(uploads(link, &attached.repo, &changes, &layer, &operation)?);
            records.push((
                Record::Append,
                layer.as_str().to_owned(),
                derived("append", &operation),
                encode(&Message::LayerAppend {
                    operation: derived("append", &operation),
                    layer: layer.clone(),
                    parent: Some(head),
                    checkpoint: Kind::Checkpoint.id(&changes.canonical_bytes()),
                })
                .map_err(remote_error)?,
            ));
            (ObjectId::for_checkpoint(&next), next)
        };
        records.push((
            Record::Revise,
            request.as_str().to_owned(),
            operation.clone(),
            encode(&Message::Revise {
                operation: operation.clone(),
                request: request.clone(),
                expected_revision: expected,
                checkpoint,
                base: VersionNumber::new(base.number)?,
            })
            .map_err(remote_error)?,
        ));
        for (kind, scope, operation, payload) in &records {
            outbox.enqueue(*kind, scope, operation, payload)?;
        }
        save_record(&attached.root, &request, &layer, &base, &next)?;
    }
    let delivered = deliver(&attached, &mut outbox, Record::Revise, &operation)?;
    let base_value = json!({
        "command": "request revise",
        "request": request.as_str(),
        "operation": operation,
    });
    match delivered {
        Delivery::Queued { fault, pending } => Ok(queued_output(
            base_value,
            &fault,
            pending,
            format!("Revision of request {}", request.as_str()),
        )),
        Delivery::Done(reply) => {
            let revision = review_outcome(reply)?;
            let mut value = base_value;
            value["ok"] = json!(true);
            value["queued"] = json!(false);
            value["revision"] = json!(revision);
            let text = format!("Request {} is at revision {revision}.", request.as_str());
            Ok((value, text, 0))
        }
    }
}

/// `request comment ID --body TEXT --operation OP [--revision N]`
fn comment(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request comment ID --body TEXT --operation OP [--revision N] [--repo DIR] [--json]";
    let options = Options::parse(
        args,
        &["--body", "--operation", "--revision", "--repo"],
        usage,
    )?;
    let request = request_id(&options, usage)?;
    let body = options.required("--body")?.to_owned();
    let revision: Option<u32> = options.number("--revision")?;
    let operation = options.operation()?;
    let attached = Attached::open(options.root()?)?;
    let reply = call(
        &attached.writing()?,
        &Message::Comment {
            operation: operation.clone(),
            request: request.clone(),
            revision,
            body,
        },
    )
    .map_err(CliError::Remote)?;
    review_outcome(reply)?;
    let value = json!({
        "ok": true,
        "command": "request comment",
        "request": request.as_str(),
        "revision": revision,
        "operation": operation,
    });
    let on = revision.map_or(String::new(), |n| format!(" on revision {n}"));
    let text = format!("Commented on request {}{on}.", request.as_str());
    Ok((value, text, 0))
}

/// `request approve ID --revision N --operation OP`: approves exactly revision N.
fn approve(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request approve ID --revision N --operation OP [--repo DIR] [--json]";
    let options = Options::parse(args, &["--revision", "--operation", "--repo"], usage)?;
    let request = request_id(&options, usage)?;
    let revision: u32 = options
        .number("--revision")?
        .ok_or_else(|| Error::InvalidInput("missing --revision".into()))?;
    let operation = options.operation()?;
    let attached = Attached::open(options.root()?)?;
    let reply = call(
        &attached.writing()?,
        &Message::Approve {
            operation: operation.clone(),
            request: request.clone(),
            revision,
        },
    )
    .map_err(CliError::Remote)?;
    review_outcome(reply)?;
    let value = json!({
        "ok": true,
        "command": "request approve",
        "request": request.as_str(),
        "revision": revision,
        "operation": operation,
    });
    let text = format!(
        "Approved revision {revision} of request {}.",
        request.as_str()
    );
    Ok((value, text, 0))
}

/// `request publish ID --revision N --operation OP [--base WORLD]`: publishes exactly
/// revision N's checkpoint. Queued while the node is unavailable.
fn publish(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request publish ID --revision N --operation OP [--base WORLD] [--repo DIR] [--json]";
    let options = Options::parse(
        args,
        &["--revision", "--operation", "--base", "--repo"],
        usage,
    )?;
    let request = request_id(&options, usage)?;
    let revision: u32 = options
        .number("--revision")?
        .ok_or_else(|| Error::InvalidInput("missing --revision".into()))?;
    let operation = options.operation()?;
    let attached = Attached::open(options.root()?)?;
    let mut outbox = attached.outbox()?;
    if !queued(&outbox, Record::Publish, &operation) {
        // Every revision of a request shares its Layer's base.
        let base = match options.number::<u64>("--base")? {
            Some(base) => base,
            None => match load_record(&attached.root, &request)? {
                Some(record) => record.base.number,
                None => {
                    let detail = read_review(&attached.reading()?, &request, 0).map_err(|e| {
                        if e.code == code::NODE_UNAVAILABLE {
                            RemoteError::new(
                                &e.code,
                                format!(
                                    "{}; pass --base WORLD (from `javelin request show`) to \
                                     queue the Publish",
                                    e.detail
                                ),
                            )
                        } else {
                            e
                        }
                    })?;
                    detail
                        .revisions
                        .iter()
                        .find(|r| r.index == revision)
                        .map(|r| r.base.get())
                        .ok_or_else(|| {
                            RemoteError::new(
                                code::UNKNOWN_REVISION,
                                format!("request {} has no revision {revision}", request.as_str()),
                            )
                        })?
                }
            },
        };
        let payload = encode(&Message::PublishRequest {
            operation: operation.clone(),
            request: request.clone(),
            revision,
            base: VersionNumber::new(base)?,
        })
        .map_err(remote_error)?;
        outbox.enqueue(Record::Publish, request.as_str(), &operation, &payload)?;
    }
    let delivered = deliver(&attached, &mut outbox, Record::Publish, &operation)?;
    let mut value = json!({
        "command": "request publish",
        "request": request.as_str(),
        "revision": revision,
        "operation": operation,
    });
    let reply = match delivered {
        Delivery::Queued { fault, pending } => {
            return Ok(queued_output(
                value,
                &fault,
                pending,
                format!(
                    "Publish of request {} revision {revision}",
                    request.as_str()
                ),
            ));
        }
        Delivery::Done(reply) => reply,
    };
    value["queued"] = json!(false);
    match reply {
        Message::PublishOutcome(PublishOutcome::Published { number, id, tree }) => {
            value["ok"] = json!(true);
            value["published"] = json!({"number": number, "id": id, "tree": tree});
            let text = format!(
                "Published revision {revision} of request {} as remote World {number}. Run \
                 `javelin sync` to bring it here.",
                request.as_str()
            );
            Ok((value, text, 0))
        }
        Message::PublishOutcome(PublishOutcome::Conflict { paths }) => {
            value["ok"] = json!(false);
            value["code"] = json!(code::CONFLICT);
            value["paths"] = json!(paths);
            let names: Vec<_> = paths.iter().map(|path| path.as_str()).collect();
            let text = format!(
                "Conflict: the World changed these paths since the revision's base: {}. The \
                 author revises the request over them.",
                names.join(", ")
            );
            Ok((value, text, 3))
        }
        Message::PublishOutcome(PublishOutcome::LayerStale { head }) => {
            value["ok"] = json!(false);
            value["code"] = json!(code::LAYER_STALE);
            value["head"] = json!(head);
            Ok((value, format!("Stale: the node's Layer is at {head}."), 3))
        }
        Message::PublishOutcome(PublishOutcome::Refused { code: refused }) => {
            if refused == code::STALE_REVISION {
                value["ok"] = json!(false);
                value["code"] = json!(refused);
                let text = format!(
                    "Stale: revision {revision} is not the current revision of request {}; \
                     review and approve the current one",
                    request.as_str()
                );
                return Ok((value, text, 3));
            }
            Err(CliError::Remote(refusal(&refused)))
        }
        _ => Err(CliError::Remote(unexpected())),
    }
}

/// `request show ID [--revision N]`: the request, its revisions and approvals, its
/// comments, and the paths revision N (default: the current one) changes.
fn show(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request show ID [--revision N] [--repo DIR] [--json]";
    let options = Options::parse(args, &["--revision", "--repo"], usage)?;
    let request = request_id(&options, usage)?;
    let chosen: Option<u32> = options.number("--revision")?;
    let attached = Attached::open(options.root()?)?;
    let link = attached.reading()?;
    let mut detail = read_review(&link, &request, 0).map_err(CliError::Remote)?;
    while (detail.comments.len() as u32) < detail.comments_total {
        let more =
            read_review(&link, &request, detail.comments.len() as u32).map_err(CliError::Remote)?;
        if more.comments.is_empty() {
            break;
        }
        detail.comments.extend(more.comments);
    }
    let shown = match chosen {
        Some(n) => Some(
            detail
                .revisions
                .iter()
                .find(|r| r.index == n)
                .ok_or_else(|| {
                    RemoteError::new(
                        code::UNKNOWN_REVISION,
                        format!("request {} has no revision {n}", request.as_str()),
                    )
                })?,
        ),
        None => detail.revisions.last(),
    };
    let changes = match shown {
        Some(revision) => {
            let bytes = node_checkpoint(&link, revision.checkpoint)?;
            let changes = Repository::foreign_checkpoint_changes(&bytes)?;
            let paths: Vec<Value> = changes
                .entries()
                .iter()
                .map(|(path, entry)| json!({"path": path, "deleted": entry.is_none()}))
                .collect();
            json!({"revision": revision.index, "paths": paths})
        }
        None => Value::Null,
    };
    let summary = &detail.summary;
    let revisions: Vec<Value> = detail
        .revisions
        .iter()
        .map(|r| {
            json!({
                "index": r.index,
                "base": r.base,
                "checkpoint": r.checkpoint,
                "tree": r.tree,
                "approvals": r.approvals,
            })
        })
        .collect();
    let comments: Vec<Value> = detail
        .comments
        .iter()
        .map(|c| json!({"author": c.author, "revision": c.revision, "body": c.body}))
        .collect();
    let value = json!({
        "ok": true,
        "command": "request show",
        "request": summary.request.as_str(),
        "layer": summary.layer,
        "author": summary.author,
        "state": state(summary.state),
        "revision": summary.revision,
        "required_approvals": detail.required_approvals,
        "revisions": revisions,
        "comments": comments,
        "changes": changes,
    });
    let mut lines = vec![format!(
        "Request {} by {} from Layer {}: {}, revision {}; Publish needs {} approval(s)",
        summary.request.as_str(),
        summary.author,
        summary.layer.as_str(),
        state(summary.state),
        summary.revision,
        detail.required_approvals
    )];
    for r in &detail.revisions {
        let approvals = match r.approvals.as_slice() {
            [] => "no approvals".to_string(),
            names => format!("approved by {}", names.join(", ")),
        };
        lines.push(format!(
            "  revision {} over remote World {}: checkpoint {}, {approvals}",
            r.index, r.base, r.checkpoint
        ));
    }
    if let Some(paths) = changes["paths"].as_array() {
        lines.push(format!("Revision {} changes:", changes["revision"]));
        for path in paths {
            let mark = if path["deleted"] == true { "D" } else { "M" };
            lines.push(format!("  {mark} {}", path["path"]));
        }
    }
    for c in &detail.comments {
        let on = c
            .revision
            .map_or("request".to_string(), |n| format!("revision {n}"));
        lines.push(format!("{} on {on}: {}", c.author, c.body));
    }
    Ok((value, lines.join("\n"), 0))
}

/// `request list`: every request on the node, in ID order.
fn list(args: Vec<OsString>) -> Outcome {
    let usage = "javelin request list [--repo DIR] [--json]";
    let options = Options::parse(args, &["--repo"], usage)?;
    if !options.positionals.is_empty() {
        return Err(Error::InvalidInput(format!("expected: {usage}")).into());
    }
    let attached = Attached::open(options.root()?)?;
    let link = attached.reading()?;
    let mut requests = Vec::new();
    let mut after = None;
    loop {
        let (page, more) =
            match call(&link, &Message::ListRequests { after }).map_err(CliError::Remote)? {
                Message::RequestList { requests, more } => (requests, more),
                _ => return Err(CliError::Remote(unexpected())),
            };
        after = page.last().map(|last| last.request.clone());
        requests.extend(page);
        if !more || after.is_none() {
            break;
        }
    }
    let listed: Vec<Value> = requests
        .iter()
        .map(|r| {
            json!({
                "request": r.request.as_str(),
                "layer": r.layer,
                "author": r.author,
                "state": state(r.state),
                "revision": r.revision,
            })
        })
        .collect();
    let mut lines = vec![format!("{} requests", requests.len())];
    for r in &requests {
        lines.push(format!(
            "  {} {} revision {} by {} from Layer {}",
            r.request.as_str(),
            state(r.state),
            r.revision,
            r.author,
            r.layer.as_str()
        ));
    }
    Ok((
        json!({"ok": true, "command": "request list", "requests": listed}),
        lines.join("\n"),
        0,
    ))
}

/// `javelin sync [--repo DIR]`
pub fn sync(args: Vec<OsString>) -> Outcome {
    let usage = "javelin sync [--repo DIR] [--json]";
    let options = Options::parse(args, &["--repo"], usage)?;
    if !options.positionals.is_empty() {
        return Err(Error::InvalidInput(format!("expected: {usage}")).into());
    }
    let root = options.root()?;
    let mut repo = Repository::open(&root)?;
    let attachment = match repo.attached_world()? {
        None => {
            return Err(Error::InvalidInput(
                "this repository is not attached to a node; attach first".into(),
            )
            .into());
        }
        Some(AttachedWorld::Diverged { attachment, local }) => {
            let recover = local
                .operation
                .as_ref()
                .map(|operation| repo.publication(operation))
                .transpose()?
                .flatten()
                .map_or(String::new(), |receipt| {
                    format!(
                        " Its Layer's last checkpoint is {0}: `javelin layer clone NEW \
                         --checkpoint {0}` then `javelin request open` proposes it to the node.",
                        receipt.expected.checkpoint().object()
                    )
                });
            return Err(CliError::Remote(RemoteError::new(
                LOCAL_WORLD_DIVERGED,
                format!(
                    "local World {} is not the attached remote World {}: it was published in \
                     this repository, not on the node, and sync never replaces it.{recover} To \
                     follow the node, attach a fresh directory: `javelin attach {}/{} DIR`",
                    local.number, attachment.head.number, attachment.node, attachment.repository
                ),
            )));
        }
        Some(AttachedWorld::Current(attachment)) => attachment,
    };
    let attached = Attached {
        root: root.clone(),
        repo: Repository::open(&root)?,
        attachment: attachment.clone(),
    };

    // Queued work goes first, so a queued Publish is in the World this sync fetches.
    let mut outbox = attached.outbox()?;
    let replay = match outbox.pending().count() {
        0 => None,
        _ => Some(outbox.replay(&mut attached.transport()?)?),
    };
    if let Some(stall) = replay.as_ref().and_then(|replay| replay.stalled.as_ref())
        && matches!(stall.fault, Fault::NodeUnavailable(_))
    {
        return Err(CliError::Remote(RemoteError::new(
            code::NODE_UNAVAILABLE,
            stall.fault.detail(),
        )));
    }
    let delivered: Vec<Value> = replay
        .iter()
        .flat_map(|replay| &replay.delivered)
        .map(|delivered| {
            let entry = outbox.entry(delivered.seq);
            json!({
                "seq": delivered.seq,
                "kind": entry.map(|entry| entry.kind.as_str()),
                "operation": entry.map(|entry| entry.operation.as_str()),
            })
        })
        .collect();
    let stalled = replay.as_ref().and_then(|replay| replay.stalled.as_ref());
    let pending = outbox.pending().count();
    drop(outbox);

    // Only the versions after the attached head are verified, from its ID.
    let link = attached.reading()?;
    let anchor = Anchor {
        number: VersionNumber::new(attachment.head.number)?,
        id: attachment.head.id,
        tree: attachment.head.tree,
    };
    let trees = Trees {
        repo: &attached.repo,
        staged: MemoryStore::default(),
    };
    let fetched = {
        let mut blobs = RepoBlobs(&mut repo);
        fetch(
            Down(&link),
            Up(&link),
            &trees,
            &mut blobs,
            Some(&anchor),
            None,
            None,
        )
        .map_err(|e| CliError::Remote(remote(&link, e)))?
    };
    let head = RemoteHead {
        number: fetched.number.get(),
        id: fetched.id,
        tree: fetched.tree,
    };
    let advanced = head.number != attachment.head.number;
    let base = if advanced {
        repo.import_tree(&trees, head.tree)?;
        repo.sync_head(&attachment, &head)?;
        Some(repo.warm_base()?)
    } else {
        None
    };
    let local = repo.head()?;
    let mut text = if advanced {
        format!(
            "Synced {} from remote World {} to {} (local World {}).",
            attachment.repository, attachment.head.number, head.number, local.number
        )
    } else {
        format!(
            "{} is current at remote World {} (local World {}).",
            attachment.repository, head.number, local.number
        )
    };
    if replay.is_some() {
        text.push_str(&format!(
            "\nDelivered {} queued records; {pending} pending.",
            delivered.len()
        ));
    }
    let (stall, stall_code, exit) = match stalled {
        Some(stall) => {
            text.push_str(&format!(
                "\nRecord {} waits: {}: {}",
                stall.seq,
                stall.fault.code(),
                stall.fault.detail()
            ));
            (
                json!({"seq": stall.seq, "code": stall.fault.code(), "detail": stall.fault.detail()}),
                json!(stall.fault.code()),
                1,
            )
        }
        None => (Value::Null, Value::Null, 0),
    };
    let value = json!({
        "ok": exit == 0,
        "command": "sync",
        "node": attachment.node,
        "repository": attachment.repository,
        "from": attachment.head,
        "head": head,
        "advanced": advanced,
        "local_head": local,
        "base": base,
        "outbox": {"delivered": delivered, "stalled": stall, "pending": pending},
        "code": stall_code,
    });
    Ok((value, text, exit))
}

/// Tree chunks for a fetch: the local store's, then the ones this fetch received. A chunk
/// the store holds has its whole closure, so an unchanged subtree is never requested again.
struct Trees<'a> {
    repo: &'a Repository,
    staged: MemoryStore,
}

impl tree::Store for Trees<'_> {
    fn load(&self, id: ObjectId) -> javelin::Result<Vec<u8>> {
        if self.repo.has_tree(id)? {
            return self.repo.tree_bytes_bounded(id, MAX_TREE);
        }
        self.staged.load(id)
    }
    fn save(&self, id: ObjectId, bytes: &[u8]) -> javelin::Result<()> {
        self.staged.save(id, bytes)
    }
}

/// Captures the Layer's workspace, then returns its complete changes and the remote World
/// its base holds.
fn local_changes(
    repo: &mut Repository,
    layer: &LayerId,
) -> Result<(Option<javelin::Capture>, SparseChanges, RemoteHead), CliError> {
    repo.set_provenance(crate::cli::provenance(None)?);
    let capture = repo.capture_workspace(layer)?;
    let head = match repo.layer_state(layer)? {
        LayerState::Active { head } => head,
        _ => {
            return Err(Error::InvalidInput(format!(
                "Layer {} is not active; propose work from an active Layer",
                layer.as_str()
            ))
            .into());
        }
    };
    let checkpoint = repo.read_checkpoint(head.checkpoint())?;
    if checkpoint.changes().entries().is_empty() {
        return Err(Error::InvalidInput(format!(
            "Layer {} has no changes to propose",
            layer.as_str()
        ))
        .into());
    }
    let local = checkpoint.base().version();
    let base = repo.remote_world(local)?.ok_or_else(|| {
        Error::InvalidInput(format!(
            "Layer {}'s base, local World {local}, was not fetched from the node; create the \
             Layer from a World that attach or sync brought here",
            layer.as_str()
        ))
    })?;
    Ok((capture, checkpoint.changes().clone(), base))
}

/// The node's checkpoint after an upload.
struct NodeHead {
    checkpoint: ObjectId,
    bytes: Vec<u8>,
}

/// Brings the Layer on the node to `changes`: creates it over remote World `base` if it is
/// missing, then uploads the blobs and changes object it lacks and appends them. A Layer the
/// node already holds at these changes is left as it is, so a retry appends nothing.
fn upload(
    link: &RefCell<Link>,
    repo: &Repository,
    layer: &LayerId,
    base: &RemoteHead,
    changes: &SparseChanges,
    operation: &OperationId,
) -> Result<NodeHead, CliError> {
    create_layer(link, layer, base).map_err(CliError::Remote)?;
    let (head, bytes) = node_head(link, layer).map_err(CliError::Remote)?;
    if Repository::foreign_checkpoint_changes(&bytes)? == *changes {
        return Ok(NodeHead {
            checkpoint: head,
            bytes,
        });
    }
    let session = &mut Session(link);
    sender::upload_blobs(session, repo, changes).map_err(|e| CliError::Remote(remote(link, e)))?;
    let id =
        sender::upload_changes(session, changes).map_err(|e| CliError::Remote(remote(link, e)))?;
    let append = derived("append", operation);
    match sender::append(session, &append, layer, Some(head), id)
        .map_err(|e| CliError::Remote(remote(link, e)))?
    {
        LayerOutcome::Accepted { checkpoint, .. } => Ok(NodeHead {
            checkpoint,
            bytes: node_checkpoint(link, checkpoint)?,
        }),
        LayerOutcome::Stale { head } => Err(CliError::Remote(RemoteError::new(
            code::LAYER_STALE,
            format!(
                "Layer {} moved on the node to {head}, perhaps from another device; run \
                 `javelin layer refresh {}` and retry",
                layer.as_str(),
                layer.as_str()
            ),
        ))),
        LayerOutcome::Refused { code: refused } => Err(CliError::Remote(RemoteError::new(
            &refused,
            format!("the node refused the upload to Layer {}", layer.as_str()),
        ))),
    }
}

/// Creates the Layer on the node over remote World `base`, unless it is there already.
fn create_layer(
    link: &RefCell<Link>,
    layer: &LayerId,
    base: &RemoteHead,
) -> std::result::Result<(), RemoteError> {
    let base_number =
        VersionNumber::new(base.number).map_err(|e| RemoteError::new(e.code(), e.to_string()))?;
    match call(
        link,
        &Message::LayerCreate {
            layer: layer.clone(),
            base: base_number,
        },
    )? {
        Message::LayerOutcome(LayerOutcome::Accepted { .. }) => Ok(()),
        Message::LayerOutcome(LayerOutcome::Refused { code: refused }) => Err(RemoteError::new(
            &refused,
            format!(
                "the node refused Layer {} over remote World {}: it may exist there over \
                 another World; use another Layer ID",
                layer.as_str(),
                base.number
            ),
        )),
        _ => Err(unexpected()),
    }
}

/// The node's head checkpoint for an active Layer, and its bytes.
fn node_head(
    link: &RefCell<Link>,
    layer: &LayerId,
) -> std::result::Result<(ObjectId, Vec<u8>), RemoteError> {
    let node = sender::layer_head(&mut Session(link), layer).map_err(|e| remote(link, e))?;
    match node.status {
        LayerStatus::Active => {}
        LayerStatus::Published => {
            return Err(RemoteError::new(
                code::LAYER_PUBLISHED,
                format!("Layer {} is published on the node", layer.as_str()),
            ));
        }
        LayerStatus::Discarded => {
            return Err(RemoteError::new(
                "layer_discarded",
                format!("Layer {} is discarded on the node", layer.as_str()),
            ));
        }
    }
    let bytes = node_checkpoint(link, node.head).map_err(|error| match error {
        CliError::Remote(error) => error,
        other => RemoteError::new(other.code(), other.to_string()),
    })?;
    Ok((node.head, bytes))
}

/// The node's checkpoint `id`, checked against its ID.
fn node_checkpoint(link: &RefCell<Link>, id: ObjectId) -> Result<Vec<u8>, CliError> {
    match call(link, &Message::Get(vec![(Kind::Checkpoint, id)])).map_err(CliError::Remote)? {
        Message::Objects(mut objects)
            if objects.len() == 1 && objects[0].0 == Kind::Checkpoint && objects[0].1 == id =>
        {
            let bytes = objects.remove(0).2;
            if Kind::Checkpoint.id(&bytes) != id {
                return Err(CliError::Remote(RemoteError::new(
                    code::ID_MISMATCH,
                    format!("checkpoint {id} does not hash to its ID"),
                )));
            }
            Ok(bytes)
        }
        _ => Err(CliError::Remote(unexpected())),
    }
}

/// The outbox records that upload `changes`: blob batches, then the changes object. With a
/// live `link`, only what the node lacks; without one, everything, since the node may lack
/// any of it and stores each object once.
fn uploads(
    link: Option<&RefCell<Link>>,
    repo: &Repository,
    changes: &SparseChanges,
    layer: &LayerId,
    operation: &OperationId,
) -> Result<Vec<Queued>, CliError> {
    let blobs: Vec<ObjectId> = changes
        .entries()
        .values()
        .flatten()
        .map(|entry| entry.content())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let bytes = changes.canonical_bytes();
    let changes_id = Kind::Checkpoint.id(&bytes);
    let (blobs, send_changes) = match link {
        Some(link) => (
            missing(link, Kind::Blob, &blobs)?,
            !missing(link, Kind::Checkpoint, &[changes_id])?.is_empty(),
        ),
        None => (blobs, true),
    };
    let mut batches: Vec<Vec<(Kind, ObjectId, Vec<u8>)>> = Vec::new();
    let mut size = 0;
    for id in blobs {
        let blob = repo.read_blob_bounded(id, MAX_BLOB)?;
        let len = javelin_protocol::codec::OBJECT_OVERHEAD + blob.len();
        let full = batches
            .last()
            .is_none_or(|batch| batch.len() == MAX_OBJECTS || size + len > MAX_FRAME / 2);
        if full {
            batches.push(Vec::new());
            size = 0;
        }
        size += len;
        batches
            .last_mut()
            .expect("a batch")
            .push((Kind::Blob, id, blob));
    }
    if send_changes {
        batches.push(vec![(Kind::Checkpoint, changes_id, bytes)]);
    }
    batches
        .into_iter()
        .enumerate()
        .map(|(n, objects)| {
            let operation = derived(&format!("upload{n}"), operation);
            let payload = encode(&Message::Objects(objects)).map_err(remote_error)?;
            Ok((
                Record::Upload,
                layer.as_str().to_owned(),
                operation,
                payload,
            ))
        })
        .collect()
}

/// Which of `ids` the node lacks.
fn missing(link: &RefCell<Link>, kind: Kind, ids: &[ObjectId]) -> Result<Vec<ObjectId>, CliError> {
    let mut lacking = Vec::new();
    for chunk in ids.chunks(MAX_WANT) {
        let items: Vec<_> = chunk.iter().map(|id| (kind, *id)).collect();
        match call(link, &Message::Want(items.clone())).map_err(CliError::Remote)? {
            Message::Missing(found) if found.iter().all(|item| items.contains(item)) => {
                lacking.extend(found.into_iter().map(|(_, id)| id));
            }
            _ => return Err(CliError::Remote(unexpected())),
        }
    }
    Ok(lacking)
}

/// An operation ID for one step of `operation`, the same on every retry.
fn derived(step: &str, operation: &OperationId) -> OperationId {
    let seed = format!("javelin-request\0{step}\0{}", operation.as_str());
    OperationId::parse(&format!("{step}-{}", ObjectId::for_blob(seed.as_bytes())))
        .expect("a derived operation ID is valid")
}

fn queued(outbox: &Outbox, kind: Record, operation: &OperationId) -> bool {
    outbox
        .entries()
        .any(|entry| entry.kind == kind && entry.operation == *operation)
}

enum Delivery {
    Done(Message),
    Queued { fault: Fault, pending: usize },
}

/// Replays the outbox, then reads the outcome the node recorded for `operation`.
fn deliver(
    attached: &Attached,
    outbox: &mut Outbox,
    kind: Record,
    operation: &OperationId,
) -> Result<Delivery, CliError> {
    let replay = outbox.replay(&mut attached.transport()?)?;
    let entry = outbox
        .entries()
        .find(|entry| entry.kind == kind && entry.operation == *operation)
        .ok_or_else(|| CliError::Remote(unexpected()))?;
    match &entry.outcome {
        Some(outcome) => Ok(Delivery::Done(
            decode(outcome).map_err(|e| CliError::Remote(e.into()))?,
        )),
        None => Ok(Delivery::Queued {
            fault: replay.stalled.map_or_else(
                || Fault::NodeUnavailable("the record was not delivered".into()),
                |stall| stall.fault,
            ),
            pending: outbox.pending().count(),
        }),
    }
}

fn queued_output(mut value: Value, fault: &Fault, pending: usize, what: String) -> Output {
    value["ok"] = json!(true);
    value["queued"] = json!(true);
    value["code"] = json!(fault.code());
    value["detail"] = json!(fault.detail());
    value["pending"] = json!(pending);
    let text = format!(
        "{what} is queued ({}: {}). `javelin sync` delivers it when the node is back; {pending} \
         records pending.",
        fault.code(),
        fault.detail()
    );
    (value, text, 0)
}

/// The revision a `ReviewOutcome` accepted, or its refusal as the error.
fn review_outcome(reply: Message) -> Result<u32, CliError> {
    match reply {
        Message::ReviewOutcome(ReviewOutcome::Accepted { revision }) => Ok(revision),
        Message::ReviewOutcome(ReviewOutcome::Refused { code }) => {
            Err(CliError::Remote(refusal(&code)))
        }
        _ => Err(CliError::Remote(unexpected())),
    }
}

/// A node refusal with a message that says what to do about it.
fn refusal(refused: &str) -> RemoteError {
    let detail = match refused {
        code::NOT_AUTHOR => "only the request's author submits revisions",
        code::SELF_APPROVAL => "the author cannot approve their own request",
        code::STALE_REVISION => {
            "that revision is not the current one; read `javelin request show` and retry"
        }
        code::UNKNOWN_REVISION => "the request has no such revision",
        code::NOT_ENOUGH_APPROVALS => {
            "the revision lacks the approvals Publish needs; approvals of earlier revisions do not count"
        }
        code::NOT_OPEN => "the request is no longer open",
        code::MISSING_OBJECT => {
            "the revision's checkpoint is not on the node: its Layer moved there, perhaps from \
             another device; run `javelin layer refresh` and revise again"
        }
        "already_open" => "a request with this ID is already open",
        "already_approved" => "you already approved this revision",
        "unchanged_revision" => "the Layer has not changed since the current revision",
        "insufficient_role" => "your role on the repository does not allow this",
        "no_access" => "you have no role on this repository",
        _ => "the node refused the request",
    };
    RemoteError::new(refused, detail)
}

fn read_review(
    link: &RefCell<Link>,
    request: &RequestId,
    comments_from: u32,
) -> std::result::Result<ReviewDetail, RemoteError> {
    match call(
        link,
        &Message::ReadReview {
            request: request.clone(),
            comments_from,
        },
    )? {
        Message::ReviewDetail(detail) => Ok(detail),
        _ => Err(unexpected()),
    }
}

fn state(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Open => "open",
        RequestStatus::Published => "published",
        RequestStatus::Closed => "closed",
    }
}

fn remote_error(error: javelin_protocol::ProtocolError) -> CliError {
    CliError::Remote(error.into())
}

/// What this clone last saw of a request it authored: its Layer, the remote World the
/// Layer's base holds, and the bytes of the node's head checkpoint for that Layer, which name
/// the node's next checkpoint for a queued revision.
struct RequestRecord {
    layer: LayerId,
    base: RemoteHead,
    head: Vec<u8>,
}

fn records_path(root: &Path) -> PathBuf {
    root.join(".javelin").join(REQUESTS)
}

fn read_records(root: &Path) -> Result<serde_json::Map<String, Value>, CliError> {
    match fs::read(records_path(root)) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(Error::CorruptRepository(format!("{REQUESTS} is not a JSON object")).into()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(Error::from(error).into()),
    }
}

fn load_record(root: &Path, request: &RequestId) -> Result<Option<RequestRecord>, CliError> {
    let records = read_records(root)?;
    let Some(record) = records.get(request.as_str()) else {
        return Ok(None);
    };
    let corrupt = || Error::CorruptRepository(format!("{REQUESTS}: bad record"));
    let layer = LayerId::parse(record["layer"].as_str().ok_or_else(corrupt)?)?;
    let base = serde_json::from_value(record["base"].clone()).map_err(|_| corrupt())?;
    let head = record["head"]
        .as_str()
        .ok_or_else(corrupt)?
        .as_bytes()
        .to_vec();
    Ok(Some(RequestRecord { layer, base, head }))
}

/// Records the request atomically: a temporary file, synced, then renamed.
fn save_record(
    root: &Path,
    request: &RequestId,
    layer: &LayerId,
    base: &RemoteHead,
    head: &[u8],
) -> Result<(), CliError> {
    let mut records = read_records(root)?;
    let head = std::str::from_utf8(head)
        .map_err(|_| Error::CorruptRepository("a node checkpoint is not UTF-8 JSON".into()))?;
    records.insert(
        request.as_str().into(),
        json!({"layer": layer, "base": base, "head": head}),
    );
    let path = records_path(root);
    let temporary = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(
            &serde_json::to_vec_pretty(&Value::Object(records)).map_err(std::io::Error::other)?,
        )?;
        file.sync_all()?;
        fs::rename(&temporary, &path)
    };
    write().map_err(|error| CliError::from(Error::from(error)))
}
