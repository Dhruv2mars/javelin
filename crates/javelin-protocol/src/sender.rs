//! Reference sender: negotiates, finds what the receiver lacks from the root down,
//! sends blobs and then tree chunks children first, then publishes the root with a
//! caller-chosen operation ID.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};

use javelin::{
    ObjectId, OperationId, Repository, VersionNumber,
    tree::{Chunk, Node},
};

use crate::{
    Kind, Message, Outcome, ProtocolError,
    codec::{OBJECT_OVERHEAD, VERSION, read_frame, write_frame},
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

pub(crate) fn exchange(
    r: &mut impl Read,
    w: &mut impl Write,
    message: &Message,
) -> Result<Message, ProtocolError> {
    write_frame(w, message)?;
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
