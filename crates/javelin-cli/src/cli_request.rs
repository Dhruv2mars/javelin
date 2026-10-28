//! `javelin request` runs the Publish Request loop on the attached node. See
//! `docs/online-design.md` sections 4.4 and 4.8, and `docs/cli.md`.
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

use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use javelin::{
    Attachment, Error, LayerId, ObjectId, OperationId, RemoteHead, Repository, VersionNumber,
    outbox::{Fault, Kind as Record, Outbox},
};
use javelin_protocol::{
    Kind, Message,
    codec::{
        PublishOutcome, RequestId, RequestStatus, ReviewDetail, ReviewOutcome, RevisionDetail,
        code, decode, encode,
    },
};
use serde_json::{Value, json};

use crate::{
    CliError,
    cli::Output,
    cli_remote::{Link, NodeTransport, RemoteError, call, parse_target, stale_appends, unexpected},
    cli_upload::{
        create_layer, derived, local_changes, node_checkpoint, node_head, upload, uploads,
    },
};

/// Where this clone keeps what it last saw of the requests it authored.
const REQUESTS: &str = "requests.json";

pub(crate) type Outcome = std::result::Result<Output, CliError>;

/// Parsed `request` options. Each may appear once.
pub(crate) struct Options {
    pub(crate) positionals: Vec<String>,
    values: std::collections::BTreeMap<String, String>,
}

impl Options {
    pub(crate) fn parse(args: Vec<OsString>, allowed: &[&str], usage: &str) -> Result<Self, Error> {
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

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
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

    pub(crate) fn root(&self) -> Result<PathBuf, Error> {
        match self.get("--repo") {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(std::env::current_dir()?),
        }
    }
}

/// The attached repository: its store, attachment and root.
pub(crate) struct Attached {
    pub(crate) root: PathBuf,
    pub(crate) repo: Repository,
    pub(crate) attachment: Attachment,
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
    pub(crate) fn reading(&self) -> Result<RefCell<Link>, CliError> {
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

    pub(crate) fn outbox(&self) -> Result<Outbox, CliError> {
        Ok(Outbox::open(&self.root, &self.attachment.repository)?)
    }

    pub(crate) fn transport(&self) -> Result<NodeTransport, CliError> {
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
        // The node's head for the Layer, read now, or as this clone last saw it. Earlier
        // queued records go first: until they are delivered, the node's head is not the one
        // this revision follows.
        if outbox.pending().count() > 0 {
            outbox.replay(&mut attached.transport()?)?;
        }
        let state = match outbox.pending().count() {
            0 => create_layer(&link, &layer, &base).and_then(|()| node_head(&link, &layer)),
            _ => Err(RemoteError::new(
                code::NODE_UNAVAILABLE,
                "earlier queued records are still waiting for the node",
            )),
        };
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
            let revision = match review_outcome(reply) {
                // The append before it was answered as stale: another device took the Layer.
                Err(CliError::Remote(error)) if error.code == code::MISSING_OBJECT => {
                    return Err(CliError::Remote(
                        taken_elsewhere(&outbox, &attached.repo, &operation).unwrap_or(error),
                    ));
                }
                revision => revision?,
            };
            let mut value = base_value;
            value["ok"] = json!(true);
            value["queued"] = json!(false);
            value["revision"] = json!(revision);
            let text = format!("Request {} is at revision {revision}.", request.as_str());
            Ok((value, text, 0))
        }
    }
}

/// `layer_stale` when the node answered revise `operation`'s append as stale: the Layer moved
/// on the node, usually by `javelin layer take` on another device. The append stays in the
/// outbox, and its local head is the work to keep.
fn taken_elsewhere(
    outbox: &Outbox,
    repo: &Repository,
    operation: &OperationId,
) -> Option<RemoteError> {
    let append = derived("append", operation);
    let stale = stale_appends(outbox, repo)
        .into_iter()
        .find(|record| record["operation"] == append.as_str())?;
    let layer = stale["layer"].as_str().unwrap_or_default();
    let keep = stale["local_head"].as_str().map_or(String::new(), |head| {
        format!("; keep this clone's work with `javelin layer clone NEW --checkpoint {head}`")
    });
    Some(RemoteError::new(
        code::LAYER_STALE,
        format!(
            "Layer {layer} moved on the node, perhaps by `javelin layer take` on another \
             device, so the revision was not recorded{keep}"
        ),
    ))
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
                 author runs `javelin sync`, redoes the change in a Layer over the new head \
                 (`javelin workspace create NEW`), and runs `javelin request revise {} \
                 --revision {revision} --layer NEW --operation OP`.",
                names.join(", "),
                request.as_str()
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
        // A checkpoint this token may not read lists no paths.
        Some(
            revision @ RevisionDetail {
                checkpoint: None, ..
            },
        ) => json!({"revision": revision.index, "paths": null}),
        Some(
            revision @ RevisionDetail {
                checkpoint: Some(checkpoint),
                ..
            },
        ) => {
            let bytes = node_checkpoint(&link, *checkpoint)?;
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
        let checkpoint = r.checkpoint.map_or_else(
            || "a checkpoint this token may not read".to_string(),
            |id| format!("checkpoint {id}"),
        );
        lines.push(format!(
            "  revision {} over remote World {}: {checkpoint}, {approvals}",
            r.index, r.base
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

pub(crate) fn remote_error(error: javelin_protocol::ProtocolError) -> CliError {
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
