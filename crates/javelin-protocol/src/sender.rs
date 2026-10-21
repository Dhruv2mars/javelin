//! Reference sender: negotiates, finds what the receiver lacks from the root down,
//! sends blobs and then tree chunks children first, then publishes the root with a
//! caller-chosen operation ID.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};

use javelin::{
    LayerId, ObjectId, OperationId, Repository, SparseChanges, VersionNumber,
    tree::{Chunk, Node},
};

use crate::{
    Kind, Message, Outcome, ProtocolError,
    codec::{LayerOutcome, LayerStatus, OBJECT_OVERHEAD, VERSION, read_frame, write_frame},
    limits::*,
};

/// Retrying after an uncertain failure must reuse the same operation, expected
/// version and tree; the receiver then returns the original durable outcome.
/// Discovery asks about one tree level at a time and never descends below a
/// chunk the receiver has, so an unchanged subtree costs one probe.
pub fn push(
    mut r: impl Read,
    mut w: impl Write,
    source: &Repository,
    tree: ObjectId,
    operation: &OperationId,
    expected: VersionNumber,
) -> Result<Outcome, ProtocolError> {
    let mut call = |message: Message| exchange(&mut r, &mut w, &message);
    match call(Message::Hello {
        min: VERSION,
        max: VERSION,
    })? {
        Message::HelloOk { version } if version == VERSION => {}
        _ => return Err(unexpected()),
    }

    let mut trees: HashMap<ObjectId, (Vec<u8>, Vec<ObjectId>)> = HashMap::new();
    let mut blobs = Vec::new();
    let mut seen = HashSet::from([(Kind::Tree, tree)]);
    let mut frontier = vec![(Kind::Tree, tree)];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for chunk in frontier.chunks(MAX_WANT) {
            let offered: BTreeSet<_> = chunk.iter().collect();
            let missing = match call(Message::Want(chunk.to_vec()))? {
                // Only offered objects, each at most once: a receiver cannot amplify transfer.
                Message::Missing(items)
                    if items.iter().all(|item| offered.contains(item))
                        && items.iter().collect::<BTreeSet<_>>().len() == items.len() =>
                {
                    items
                }
                _ => return Err(unexpected()),
            };
            for (kind, id) in missing {
                if kind == Kind::Blob {
                    blobs.push(id);
                    continue;
                }
                let bytes = source.tree_bytes_bounded(id, MAX_TREE)?;
                let refs = references(&Chunk::decode(&bytes)?);
                let children = refs.iter().filter(|(k, _)| *k == Kind::Tree).map(|r| r.1);
                trees.insert(id, (bytes.clone(), children.collect()));
                next.extend(refs.into_iter().filter(|r| seen.insert(*r)));
            }
        }
        frontier = next;
    }

    let mut batch = Batch::default();
    for id in blobs {
        let bytes = source.read_blob_bounded(id, MAX_BLOB)?;
        batch.push(&mut call, (Kind::Blob, id, bytes))?;
    }
    // Children before parents, so the receiver can check each chunk's closure.
    let mut order = Vec::new();
    postorder(tree, &trees, &mut HashSet::new(), &mut order);
    for id in order {
        let bytes = trees.remove(&id).expect("discovered").0;
        batch.push(&mut call, (Kind::Tree, id, bytes))?;
    }
    batch.flush(&mut call)?;

    match call(Message::Publish {
        operation: operation.clone(),
        expected,
        tree,
    })? {
        Message::Outcome(outcome) if consistent(&outcome, operation, expected, tree) => Ok(outcome),
        _ => Err(unexpected()),
    }
}

/// Everything a chunk names, by kind.
pub(crate) fn references(chunk: &Chunk) -> Vec<(Kind, ObjectId)> {
    chunk
        .children()
        .map(|id| (Kind::Tree, id))
        .chain(chunk.entries().map(|(_, node)| match node {
            Node::File(entry) => (Kind::Blob, entry.content()),
            Node::Dir(id) => (Kind::Tree, id),
        }))
        .collect()
}

fn postorder(
    id: ObjectId,
    trees: &HashMap<ObjectId, (Vec<u8>, Vec<ObjectId>)>,
    done: &mut HashSet<ObjectId>,
    out: &mut Vec<ObjectId>,
) {
    let Some((_, children)) = trees.get(&id) else {
        return;
    };
    if !done.insert(id) {
        return;
    }
    for child in children {
        postorder(*child, trees, done, out);
    }
    out.push(id);
}

/// Accumulates objects into frames within the count and size limits.
#[derive(Default)]
struct Batch {
    objects: Vec<(Kind, ObjectId, Vec<u8>)>,
    size: usize,
}

impl Batch {
    fn push(
        &mut self,
        call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
        object: (Kind, ObjectId, Vec<u8>),
    ) -> Result<(), ProtocolError> {
        let cost = OBJECT_OVERHEAD + object.2.len();
        if !self.objects.is_empty()
            && (self.objects.len() == MAX_OBJECTS || self.size + cost > MAX_FRAME - 5)
        {
            self.flush(call)?;
        }
        self.size += cost;
        self.objects.push(object);
        Ok(())
    }

    fn flush(
        &mut self,
        call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    ) -> Result<(), ProtocolError> {
        if self.objects.is_empty() {
            return Ok(());
        }
        let sent = self.objects.len();
        self.size = 0;
        match call(Message::Objects(std::mem::take(&mut self.objects)))? {
            Message::Stored { count } if count as usize == sent => Ok(()),
            _ => Err(unexpected()),
        }
    }
}

/// An outcome must answer this exact Publish: acceptance is the next version with the
/// requested tree and operation; staleness reports a head beyond `expected`.
fn consistent(
    outcome: &Outcome,
    operation: &OperationId,
    expected: VersionNumber,
    tree: ObjectId,
) -> bool {
    match outcome {
        Outcome::Accepted {
            number,
            tree: accepted_tree,
            operation: accepted,
        } => {
            Some(number.get()) == expected.get().checked_add(1)
                && *accepted_tree == tree
                && accepted == operation
        }
        Outcome::Stale { actual } => *actual > expected,
    }
}

/// One message on a link that carries one request per exchange, such as a node's
/// `/v2` POST. The caller answers `Hello` before any other message.
fn request(link: &mut (impl Read + Write), message: &Message) -> Result<Message, ProtocolError> {
    write_frame(&mut *link, message)?;
    read_reply(&mut *link)
}

/// Uploads the blobs `changes` reference that the node does not hold. Blobs are read
/// from `source`, the repository that captured them.
pub fn upload_blobs(
    link: &mut (impl Read + Write),
    source: &Repository,
    changes: &SparseChanges,
) -> Result<(), ProtocolError> {
    let wanted: Vec<(Kind, ObjectId)> = changes
        .entries()
        .values()
        .flatten()
        .map(|entry| (Kind::Blob, entry.content()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut missing = Vec::new();
    for chunk in wanted.chunks(MAX_WANT) {
        match request(link, &Message::Want(chunk.to_vec()))? {
            Message::Missing(items) if items.iter().all(|item| chunk.contains(item)) => {
                missing.extend(items)
            }
            _ => return Err(unexpected()),
        }
    }
    let mut batch = Batch::default();
    let mut send = |message: Message| request(link, &message);
    for (kind, id) in missing {
        let bytes = source.read_blob_bounded(id, MAX_BLOB)?;
        batch.push(&mut send, (kind, id, bytes))?;
    }
    batch.flush(&mut send)
}

/// Uploads `changes` as a checkpoint object and returns its ID. An object the node
/// already holds is not sent again.
pub fn upload_changes(
    link: &mut (impl Read + Write),
    changes: &SparseChanges,
) -> Result<ObjectId, ProtocolError> {
    let bytes = changes.canonical_bytes();
    let id = Kind::Checkpoint.id(&bytes);
    match request(link, &Message::Want(vec![(Kind::Checkpoint, id)]))? {
        Message::Missing(items) if items.is_empty() => return Ok(id),
        Message::Missing(items) if items == [(Kind::Checkpoint, id)] => {}
        _ => return Err(unexpected()),
    }
    match request(link, &Message::Objects(vec![(Kind::Checkpoint, id, bytes)]))? {
        Message::Stored { count: 1 } => Ok(id),
        _ => Err(unexpected()),
    }
}

/// Appends the uploaded changes `changes` to `layer` after `parent`. `parent` must be
/// the node's head for the outcome to be `Accepted`; otherwise it is `Stale`.
pub fn append(
    link: &mut (impl Read + Write),
    operation: &OperationId,
    layer: &LayerId,
    parent: Option<ObjectId>,
    changes: ObjectId,
) -> Result<LayerOutcome, ProtocolError> {
    let message = Message::LayerAppend {
        operation: operation.clone(),
        layer: layer.clone(),
        parent,
        checkpoint: changes,
    };
    match request(link, &message)? {
        Message::LayerOutcome(outcome) => Ok(outcome),
        _ => Err(unexpected()),
    }
}

/// A node's view of one Layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeLayer {
    pub base: VersionNumber,
    pub seq: u64,
    pub head: ObjectId,
    pub status: LayerStatus,
}

pub fn layer_head(
    link: &mut (impl Read + Write),
    layer: &LayerId,
) -> Result<NodeLayer, ProtocolError> {
    match request(
        link,
        &Message::LayerHead {
            layer: layer.clone(),
        },
    )? {
        Message::LayerState {
            layer: reported,
            base,
            seq,
            head,
            state,
        } if reported == *layer => Ok(NodeLayer {
            base,
            seq,
            head,
            status: state,
        }),
        _ => Err(unexpected()),
    }
}

/// Uploads captures to one Layer on a node. Each append names the head the node last
/// accepted from this uploader, so a retry after a lost reply names the same parent and
/// replays the same operation.
pub struct Uploader {
    layer: LayerId,
    parent: Option<ObjectId>,
}

impl Uploader {
    /// `parent` is the node's head for `layer` when uploading starts.
    pub fn new(layer: LayerId, parent: ObjectId) -> Self {
        Self {
            layer,
            parent: Some(parent),
        }
    }

    /// Uploads the blobs and changes, then appends them under `operation`. Only an
    /// accepted append moves the parent on.
    pub fn upload(
        &mut self,
        link: &mut (impl Read + Write),
        source: &Repository,
        operation: &OperationId,
        changes: &SparseChanges,
    ) -> Result<LayerOutcome, ProtocolError> {
        upload_blobs(link, source, changes)?;
        let id = upload_changes(link, changes)?;
        let outcome = append(link, operation, &self.layer, self.parent, id)?;
        if let LayerOutcome::Accepted { checkpoint, .. } = &outcome {
            self.parent = Some(*checkpoint);
        }
        Ok(outcome)
    }
}

pub(crate) fn exchange(
    r: &mut impl Read,
    w: &mut impl Write,
    message: &Message,
) -> Result<Message, ProtocolError> {
    write_frame(w, message)?;
    read_reply(r)
}

fn read_reply(r: &mut impl Read) -> Result<Message, ProtocolError> {
    match read_frame(r)? {
        None => Err(ProtocolError::new(
            "truncated",
            "receiver closed the session before replying",
        )),
        Some(Message::Error { code, detail }) => Err(ProtocolError { code, detail }),
        Some(reply) => Ok(reply),
    }
}

pub(crate) fn unexpected() -> ProtocolError {
    ProtocolError::new("unexpected_message", "unexpected reply from receiver")
}
