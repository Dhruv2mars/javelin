//! Moving a local Layer's work to the attached node for `javelin request`: the Layer is
//! created there over the remote World its base holds, then the blobs and changes object it
//! lacks are uploaded and appended. `request open` sends them now; `request revise` journals
//! them as outbox records, so they queue through an outage (D7).

use std::{cell::RefCell, collections::BTreeSet};

use javelin::{
    Error, LayerId, LayerState, ObjectId, OperationId, RemoteHead, Repository, SparseChanges,
    VersionNumber, outbox::Kind as Record,
};
use javelin_protocol::{
    Kind, Message,
    codec::{LayerOutcome, LayerStatus, OBJECT_OVERHEAD, code, encode},
    limits::{MAX_BLOB, MAX_FRAME, MAX_OBJECTS, MAX_WANT},
    sender,
};

use crate::{
    CliError,
    cli_remote::{Link, RemoteError, Session, call, remote, unexpected},
    cli_request::remote_error,
};

/// One outbox record to enqueue: its kind, scope, operation and wire message.
pub(crate) type Queued = (Record, String, OperationId, Vec<u8>);

/// Captures the Layer's workspace, then returns its complete changes and the remote World
/// its base holds.
pub(crate) fn local_changes(
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
pub(crate) struct NodeHead {
    pub(crate) checkpoint: ObjectId,
    pub(crate) bytes: Vec<u8>,
}

/// Brings the Layer on the node to `changes`: creates it over remote World `base` if it is
/// missing, then uploads the blobs and changes object it lacks and appends them. A Layer the
/// node already holds at these changes is left as it is, so a retry appends nothing.
pub(crate) fn upload(
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
pub(crate) fn create_layer(
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
pub(crate) fn node_head(
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
pub(crate) fn node_checkpoint(link: &RefCell<Link>, id: ObjectId) -> Result<Vec<u8>, CliError> {
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
pub(crate) fn uploads(
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
        // A hidden entry names no object.
        .filter_map(|entry| entry.content())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let bytes = changes.canonical_bytes();
    let changes_id = Kind::Checkpoint.id(&bytes);
    // A node that stops answering here still gets the whole revision queued, and is not
    // asked again.
    let down = std::cell::Cell::new(false);
    let lacking = |kind, ids: &[ObjectId]| match link
        .filter(|_| !down.get())
        .map(|link| missing(link, kind, ids))
    {
        Some(Err(CliError::Remote(error))) if error.code == code::NODE_UNAVAILABLE => {
            down.set(true);
            Ok(ids.to_vec())
        }
        Some(found) => found,
        None => Ok(ids.to_vec()),
    };
    let send_changes = !lacking(Kind::Checkpoint, &[changes_id])?.is_empty();
    let blobs = lacking(Kind::Blob, &blobs)?;
    let mut batches: Vec<Vec<(Kind, ObjectId, Vec<u8>)>> = Vec::new();
    let mut size = 0;
    for id in blobs {
        let blob = repo.read_blob_bounded(id, MAX_BLOB)?;
        let len = OBJECT_OVERHEAD + blob.len();
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
pub(crate) fn missing(
    link: &RefCell<Link>,
    kind: Kind,
    ids: &[ObjectId],
) -> Result<Vec<ObjectId>, CliError> {
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
pub(crate) fn derived(step: &str, operation: &OperationId) -> OperationId {
    let seed = format!("javelin-request\0{step}\0{}", operation.as_str());
    OperationId::parse(&format!("{step}-{}", ObjectId::for_blob(seed.as_bytes())))
        .expect("a derived operation ID is valid")
}
