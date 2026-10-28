//! Publish in an attached repository goes to the node, because the node's World is the World.
//! `workspace publish` and `adopt PROJECT --repo DIR` upload the Layer as `request open` does
//! (created on the node over the remote World its base holds, then the blobs and changes it
//! lacks), then send `PublishLayer` through the outbox, so it queues while the node is down
//! (D7). The node publishes directly for Maintain when the repository requires no approvals;
//! otherwise it answers `insufficient_role` or `review_required`, and the message names
//! `javelin request open`.
//!
//! The local World never diverges: after the node publishes, the clone follows it to its head
//! as `javelin sync` does. The Layer is then closed here: its directory is reclaimed and its
//! local state reads discarded, since the World that published it is the node's.

use std::{
    cell::RefCell,
    ffi::OsString,
    path::{Path, PathBuf},
};

use javelin::{
    Error, LayerHead, LayerId, LayerState, ObjectId, OperationId, PublishAttribution, RemoteHead,
    Repository, VersionNumber, outbox::Kind as Record,
};
use javelin_protocol::{
    Kind, Message,
    codec::{PublishOutcome, code, decode, encode},
};
use serde_json::{Value, json};

use crate::{
    CliError,
    cli_remote::{LOCAL_WORLD_DIVERGED, Link, RemoteError, recover_attachment},
    cli_request::{
        Attached, Delivery, Options, Outcome, deliver, known_layer_head, queued, queued_output,
        refusal, remote_error,
    },
    cli_upload::{derived, missing, node_checkpoint, upload, uploads},
};

/// `javelin workspace publish ID --operation OP` in an attached repository. The workspace was
/// captured by the caller, which refuses a directory it could not capture whole.
pub(crate) fn workspace(
    root: &Path,
    layer: &LayerId,
    operation: &OperationId,
    attribution: &PublishAttribution,
) -> Outcome {
    let attached = Attached::open(root.to_path_buf())?;
    let published = publish(attached, layer, operation, attribution, "workspace publish")?;
    Ok(published.output(format!("Layer {}", layer.as_str())))
}

/// `javelin adopt PROJECT --repo DIR --operation OP`: imports the project directory into the
/// attached repository DIR as one Publish on its node, through the reserved Layer that
/// `adopt` uses. A retry with the same operation delivers or replays the first attempt.
pub(crate) fn adopt(args: Vec<OsString>) -> Outcome {
    let usage = "javelin adopt PROJECT --repo DIR --operation OP [--json]";
    let options = Options::parse(args, &["--repo", "--operation"], usage)?;
    let [project] = options.positionals.as_slice() else {
        return Err(Error::InvalidInput(format!("expected: {usage}")).into());
    };
    let operation = OperationId::parse(options.required("--operation")?)?;
    let mut attached = Attached::open(options.root()?).map_err(|error| match error {
        CliError::Native(Error::InvalidInput(_)) => Error::InvalidInput(
            "adopt --repo imports into an attached repository; `javelin adopt DIR` without \
             --repo makes a new local one"
                .into(),
        )
        .into(),
        error => error,
    })?;
    let layer = javelin::adoption_layer(&operation)?;
    if !queued(&attached.outbox()?, Record::Publish, &operation) {
        attached.repo.set_provenance(crate::cli::provenance(None)?);
        attached.repo.stage_import(project, &operation)?;
    }
    let files = attached
        .repo
        .layer_history(&layer)?
        .into_iter()
        .rev()
        .find_map(|state| match state {
            LayerState::Active { head } => Some(head),
            _ => None,
        })
        .map(|head| attached.repo.read_checkpoint(head.checkpoint()))
        .transpose()?
        .map(|checkpoint| checkpoint.changes().entries().len());
    let published = publish(
        attached,
        &layer,
        &operation,
        &PublishAttribution::Human,
        "adopt",
    )?;
    let mut output = published.output(format!(
        "Imported {} ({} files)",
        PathBuf::from(project).display(),
        files.unwrap_or(0)
    ));
    output.0["project"] = json!(project);
    output.0["files"] = json!(files);
    Ok(output)
}

/// What a Publish to the node came to.
struct Published {
    value: Value,
    outcome: Option<PublishOutcome>,
    /// The node's head this clone followed to afterwards, or why it could not.
    head: Option<Result<RemoteHead, CliError>>,
    queued: Option<String>,
}

impl Published {
    fn output(self, what: String) -> crate::cli::Output {
        let mut value = self.value;
        if let Some(queued) = self.queued {
            return (value, format!("{what}: {queued}"), 0);
        }
        let (text, exit) = match self.outcome {
            Some(PublishOutcome::Published { number, .. }) => {
                let followed = match &self.head {
                    Some(Ok(head)) => format!(" This clone is at World {}.", head.number),
                    Some(Err(error)) => format!(
                        " This clone did not follow it ({}: {error}); run `javelin sync`.",
                        error.code()
                    ),
                    None => String::new(),
                };
                (format!("{what} published as World {number}.{followed}"), 0)
            }
            Some(PublishOutcome::Conflict { paths }) => {
                let names: Vec<_> = paths.iter().map(|path| path.as_str()).collect();
                value["ok"] = json!(false);
                value["code"] = json!(code::CONFLICT);
                value["paths"] = json!(paths);
                (
                    format!(
                        "Conflict: the node's World changed these paths since the Layer's base: \
                         {}. Nothing was published. Run `javelin sync`, redo the change in a \
                         new Layer over the new head (`javelin workspace create NEW`), and \
                         publish that.",
                        names.join(", ")
                    ),
                    3,
                )
            }
            Some(PublishOutcome::LayerStale { head }) => {
                value["ok"] = json!(false);
                value["code"] = json!(code::LAYER_STALE);
                value["head"] = json!(head);
                (
                    format!(
                        "Stale: the node's Layer is at {head}, perhaps moved by another device; \
                         nothing was published. Run `javelin layer take` or `javelin layer \
                         refresh`, then publish again"
                    ),
                    3,
                )
            }
            _ => (String::new(), 1),
        };
        (value, text, exit)
    }
}

/// Uploads `layer` to the attached node unless a first attempt queued it, sends its
/// `PublishLayer`, and, once the node published, follows the node and closes the Layer.
fn publish(
    mut attached: Attached,
    layer: &LayerId,
    operation: &OperationId,
    attribution: &PublishAttribution,
    command: &str,
) -> Result<Published, CliError> {
    let link = attached.writing()?;
    // A sync that stopped before `attach.json` is finished first; a node that cannot answer
    // leaves that to a later sync.
    attached.attachment =
        match recover_attachment(&link, &mut attached.repo, attached.attachment.clone()) {
            Err(CliError::Remote(error)) if error.code == code::NODE_UNAVAILABLE => {
                attached.attachment.clone()
            }
            recovered => recovered?,
        };
    if !attached.repo.follows_attachment(&attached.attachment)? {
        return Err(diverged(&attached));
    }
    let mut outbox = attached.outbox()?;
    if !queued(&outbox, Record::Publish, operation) {
        let records = prepare(&link, &attached, layer, operation, attribution)?;
        for (kind, scope, operation, payload) in &records {
            outbox.enqueue(*kind, scope, operation, payload)?;
        }
    }
    let mut value = json!({
        "command": command,
        "layer": layer,
        "operation": operation,
    });
    let delivered = deliver(&attached, &mut outbox, Record::Publish, operation)?;
    let node = outbox
        .entries()
        .filter(|entry| entry.kind == Record::Publish && entry.operation == *operation)
        .find_map(|entry| match decode(&entry.payload) {
            Ok(Message::PublishLayer { expected, .. }) => Some(expected),
            _ => None,
        });
    let reply = match delivered {
        Delivery::Queued { fault, pending } => {
            let (value, text, _) = queued_output(value, &fault, pending, "the Publish".into());
            return Ok(Published {
                value,
                outcome: None,
                head: None,
                queued: Some(text),
            });
        }
        Delivery::Done(reply) => reply,
    };
    drop(outbox);
    value["queued"] = json!(false);
    let outcome = match reply {
        Message::PublishOutcome(PublishOutcome::Refused { code: refused }) => {
            return Err(CliError::Remote(direct_refusal(&refused, layer)));
        }
        Message::PublishOutcome(outcome) => outcome,
        _ => return Err(CliError::Remote(crate::cli_remote::unexpected())),
    };
    let mut head = None;
    if let PublishOutcome::Published { number, id, tree } = &outcome {
        value["ok"] = json!(true);
        value["published"] = json!({"number": number, "id": id, "tree": tree});
        // The Publish stands whatever happens here; `javelin sync` closes the Layer later.
        let closed = node.is_some_and(|node| {
            close(&link, &mut attached.repo, layer, operation, node).unwrap_or(false)
        });
        value["closed"] = json!(closed);
        let followed = follow(&link, &mut attached);
        value["head"] = followed.as_ref().map_or(Value::Null, |head| json!(head));
        head = Some(followed);
    }
    Ok(Published {
        value,
        outcome: Some(outcome),
        head,
        queued: None,
    })
}

/// The outbox records that publish `layer`: what the node lacks of it, then `PublishLayer`
/// naming the node's checkpoint for its changes. Live when the node answers; while it is
/// down, from the node head this clone last saw for the Layer, as a queued revision is.
fn prepare(
    link: &RefCell<Link>,
    attached: &Attached,
    layer: &LayerId,
    operation: &OperationId,
    attribution: &PublishAttribution,
) -> Result<Vec<crate::cli_upload::Queued>, CliError> {
    let repo = &attached.repo;
    let head = active_head(repo, layer)?;
    let checkpoint = repo.read_checkpoint(head.checkpoint())?;
    let changes = checkpoint.changes().clone();
    if changes.entries().is_empty() {
        return Err(Error::InvalidInput(format!(
            "Layer {} has no changes to publish",
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
    let mut records = Vec::new();
    // The agent trace a Publish names must be on the node too.
    if let PublishAttribution::Agent {
        context: Some(trace),
        ..
    } = attribution
    {
        let lacking = match missing(link, Kind::Blob, &[*trace]) {
            Err(CliError::Remote(error)) if error.code == code::NODE_UNAVAILABLE => vec![*trace],
            found => found?,
        };
        if !lacking.is_empty() {
            let bytes = repo.read_blob(*trace)?;
            let payload = encode(&Message::Objects(vec![(Kind::Blob, *trace, bytes)]))
                .map_err(remote_error)?;
            records.push((
                Record::Upload,
                layer.as_str().to_owned(),
                derived("trace", operation),
                payload,
            ));
        }
    }
    let expected = match upload(link, repo, layer, &base, &changes, operation) {
        Ok(node) => node.checkpoint,
        Err(CliError::Remote(error)) if error.code == code::NODE_UNAVAILABLE => {
            let Some(known) = known_layer_head(&attached.root, layer)? else {
                return Err(CliError::Remote(RemoteError::new(
                    code::NODE_UNAVAILABLE,
                    format!(
                        "{}; the node has not seen Layer {} from this clone, so its Publish \
                         cannot be queued. Retry when the node is back",
                        error.detail,
                        layer.as_str()
                    ),
                )));
            };
            if Repository::foreign_checkpoint_changes(&known)? == changes {
                ObjectId::for_checkpoint(&known)
            } else {
                records.extend(uploads(None, repo, &changes, layer, operation)?);
                let append = derived("append", operation);
                records.push((
                    Record::Append,
                    layer.as_str().to_owned(),
                    append.clone(),
                    encode(&Message::LayerAppend {
                        operation: append,
                        layer: layer.clone(),
                        parent: Some(ObjectId::for_checkpoint(&known)),
                        checkpoint: Kind::Checkpoint.id(&changes.canonical_bytes()),
                    })
                    .map_err(remote_error)?,
                ));
                ObjectId::for_checkpoint(&Repository::foreign_capture(&known, &changes)?)
            }
        }
        Err(error) => return Err(error),
    };
    records.push((
        Record::Publish,
        layer.as_str().to_owned(),
        operation.clone(),
        encode(&Message::PublishLayer {
            operation: operation.clone(),
            layer: layer.clone(),
            expected,
            base: VersionNumber::new(base.number)?,
            attribution: attribution.clone(),
        })
        .map_err(remote_error)?,
    ));
    Ok(records)
}

/// The Layer's latest Active head: what is published.
fn active_head(repo: &Repository, layer: &LayerId) -> Result<LayerHead, CliError> {
    match repo.layer_state(layer)? {
        LayerState::Active { head } => Ok(head),
        _ => Err(Error::InvalidInput(format!(
            "Layer {} is not active; publish work from an active Layer",
            layer.as_str()
        ))
        .into()),
    }
}

/// Closes the local Layer whose `PublishLayer` of node checkpoint `node` the node accepted
/// under `operation`: discards it and reclaims its directory, when its latest head holds the
/// published changes. A Layer that moved on since, or ended, is left as it is. Returns
/// whether the Layer is closed.
pub(crate) fn close(
    link: &RefCell<Link>,
    repo: &mut Repository,
    layer: &LayerId,
    operation: &OperationId,
    node: ObjectId,
) -> Result<bool, CliError> {
    let head = match repo.layer_state(layer)? {
        LayerState::Active { head } => head,
        LayerState::Discarded { .. } => return Ok(true),
        LayerState::Published { .. } => return Ok(false),
    };
    let bytes = node_checkpoint(link, node)?;
    let local = repo.read_checkpoint(head.checkpoint())?;
    if Repository::foreign_checkpoint_changes(&bytes)? != *local.changes() {
        return Ok(false);
    }
    let discard = repo.discard_workspace(&derived("close", operation), &head)?;
    Ok(matches!(
        discard.update,
        javelin::LayerUpdate::Applied { .. }
    ))
}

/// The Layer and node checkpoint of a delivered `PublishLayer` record the node published.
pub(crate) fn published_layer(payload: &[u8], outcome: &[u8]) -> Option<(LayerId, ObjectId)> {
    let Ok(Message::PublishOutcome(PublishOutcome::Published { .. })) = decode(outcome) else {
        return None;
    };
    match decode(payload) {
        Ok(Message::PublishLayer {
            layer, expected, ..
        }) => Some((layer, expected)),
        _ => None,
    }
}

/// Follows the node to its head after a Publish, as `javelin sync` does.
fn follow(link: &RefCell<Link>, attached: &mut Attached) -> Result<RemoteHead, CliError> {
    let local = attached.repo.head()?;
    let head = crate::cli_sync::follow(
        link,
        &attached.root,
        &mut attached.repo,
        &attached.attachment,
        &local,
        None,
    )?;
    if head.number != attached.attachment.head.number {
        attached.repo.warm_base()?;
    }
    Ok(head)
}

/// A clone that published here before Publish went to the node cannot follow it.
fn diverged(attached: &Attached) -> CliError {
    CliError::Remote(RemoteError::new(
        LOCAL_WORLD_DIVERGED,
        format!(
            "this clone's World was published here, not on the node, so it cannot follow the \
             node after a Publish. Attach a fresh directory (`javelin attach {}/{} DIR`) and \
             publish from there",
            attached.attachment.node, attached.attachment.repository
        ),
    ))
}

/// A direct Publish the node refused, with the way through a Publish Request.
fn direct_refusal(refused: &str, layer: &LayerId) -> RemoteError {
    let request = format!(
        "`javelin request open ID --layer {} --operation OP`",
        layer.as_str()
    );
    match refused {
        code::INSUFFICIENT_ROLE => RemoteError::new(
            refused,
            format!(
                "your role on the repository does not publish directly (Maintain does); \
                 propose the Layer with {request}"
            ),
        ),
        code::REVIEW_REQUIRED => RemoteError::new(
            refused,
            format!(
                "this repository requires approvals before Publish, so changes go through a \
                 Publish Request: {request}"
            ),
        ),
        other => refusal(other),
    }
}
