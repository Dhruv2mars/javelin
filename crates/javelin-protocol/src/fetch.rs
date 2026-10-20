//! Reference fetch: resolves a version, then reads its tree from the root down,
//! one level per round trip, and the blobs last. With a path prefix it reads only
//! the chunks on the way to the prefix and the subtree below it. Objects already
//! held locally are not requested, and every received object is checked against
//! its ID before it is used.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};

use javelin::{
    FileEntry, MAX_DEPTH, MAX_PATH, ObjectId, RepoPath, VersionNumber,
    tree::{Chunk, Node, Store},
};

use crate::{
    Kind, Message, ProtocolError,
    codec::VERSION,
    limits::*,
    sender::{exchange, unexpected},
};

/// Where fetched blobs go.
pub trait Blobs {
    fn has(&self, id: ObjectId) -> bool;
    fn put(&mut self, id: ObjectId, bytes: Vec<u8>) -> Result<(), ProtocolError>;
}

impl Blobs for BTreeMap<ObjectId, Vec<u8>> {
    fn has(&self, id: ObjectId) -> bool {
        self.contains_key(&id)
    }
    fn put(&mut self, id: ObjectId, bytes: Vec<u8>) -> Result<(), ProtocolError> {
        self.insert(id, bytes);
        Ok(())
    }
}

/// The fetched version and the files at or below the prefix.
#[derive(Debug)]
pub struct Fetched {
    pub number: VersionNumber,
    pub id: ObjectId,
    pub tree: ObjectId,
    pub files: BTreeMap<RepoPath, FileEntry>,
    /// Objects transferred, not counting those already held.
    pub trees_received: usize,
    pub blobs_received: usize,
}

/// What a pending chunk contributes: everything below it, or only the entries on
/// the way to the prefix. `dir` is the path of the directory it belongs to.
#[derive(Clone)]
enum Want {
    All { dir: String },
    Path { dir: String, rest: Vec<String> },
}

/// Fetches `version` (the head when None), or only `prefix` within it. Tree chunks
/// go to `trees`, which also serves chunks held from earlier fetches.
pub fn fetch(
    mut r: impl Read,
    mut w: impl Write,
    trees: &impl Store,
    blobs: &mut impl Blobs,
    version: Option<VersionNumber>,
    prefix: Option<&RepoPath>,
) -> Result<Fetched, ProtocolError> {
    let mut call = |message: Message| exchange(&mut r, &mut w, &message);
    match call(Message::Hello {
        min: VERSION,
        max: VERSION,
    })? {
        Message::HelloOk { version } if version == VERSION => {}
        _ => return Err(unexpected()),
    }
    let (number, id, root) = match call(Message::Resolve { version })? {
        Message::Resolved { number, id, tree } if version.is_none_or(|v| v == number) => {
            (number, id, tree)
        }
        _ => return Err(unexpected()),
    };
    let start = match prefix {
        None => Want::All { dir: String::new() },
        Some(prefix) => Want::Path {
            dir: String::new(),
            rest: prefix.as_str().split('/').map(str::to_owned).collect(),
        },
    };
    let mut fetched = Fetched {
        number,
        id,
        tree: root,
        files: BTreeMap::new(),
        trees_received: 0,
        blobs_received: 0,
    };
    let mut wanted_blobs = Vec::new();
    let mut pending = vec![(root, start)];
    while !pending.is_empty() {
        let missing: Vec<ObjectId> = {
            let mut seen = HashSet::new();
            pending
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| seen.insert(*id) && trees.load(*id).is_err())
                .collect()
        };
        fetched.trees_received += missing.len();
        for (id, bytes) in get(&mut call, Kind::Tree, &missing)? {
            trees.save(id, &bytes)?;
        }
        let mut next = Vec::new();
        for (id, want) in pending {
            let chunk = Chunk::decode(&trees.load(id)?)?;
            walk(
                &chunk,
                want,
                &mut next,
                &mut fetched.files,
                &mut wanted_blobs,
            )?;
        }
        pending = next;
    }
    let mut seen = HashSet::new();
    wanted_blobs.retain(|id| seen.insert(*id) && !blobs.has(*id));
    fetched.blobs_received = wanted_blobs.len();
    for (id, bytes) in get(&mut call, Kind::Blob, &wanted_blobs)? {
        blobs.put(id, bytes)?;
    }
    Ok(fetched)
}

/// Follows one chunk's references according to `want`.
fn walk(
    chunk: &Chunk,
    want: Want,
    next: &mut Vec<(ObjectId, Want)>,
    files: &mut BTreeMap<RepoPath, FileEntry>,
    blobs: &mut Vec<ObjectId>,
) -> Result<(), ProtocolError> {
    if chunk.level() > 0 {
        match &want {
            Want::All { .. } => next.extend(chunk.children().map(|id| (id, want.clone()))),
            Want::Path { rest, .. } => next.extend(chunk.child_for(&rest[0]).map(|id| (id, want))),
        }
        return Ok(());
    }
    for (name, node) in chunk.entries() {
        let (dir, below) = match &want {
            Want::All { dir } => (dir, None),
            Want::Path { dir, rest } if rest[0] == name => (dir, Some(&rest[1..])),
            Want::Path { .. } => continue,
        };
        let path = format!("{dir}{name}");
        if path.len() > MAX_PATH || path.matches('/').count() >= MAX_DEPTH {
            return Err(ProtocolError::new(
                "limit_exceeded",
                "tree path exceeds path limits",
            ));
        }
        match (node, below) {
            (Node::File(entry), None | Some([])) => {
                blobs.push(entry.content());
                files.insert(RepoPath::parse(&path)?, entry);
            }
            (Node::File(_), Some(_)) => {}
            (Node::Dir(id), None | Some([])) => next.push((id, Want::All { dir: path + "/" })),
            (Node::Dir(id), Some(rest)) => next.push((
                id,
                Want::Path {
                    dir: path + "/",
                    rest: rest.to_vec(),
                },
            )),
        }
    }
    Ok(())
}

/// Requests `ids` and returns each verified object, re-requesting the rest when a
/// reply answers only a prefix.
fn get(
    call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    kind: Kind,
    ids: &[ObjectId],
) -> Result<Vec<(ObjectId, Vec<u8>)>, ProtocolError> {
    let mut out = Vec::with_capacity(ids.len());
    let mut rest = ids;
    while !rest.is_empty() {
        let request: Vec<_> = rest.iter().take(MAX_WANT).map(|id| (kind, *id)).collect();
        let Message::Objects(objects) = call(Message::Get(request.clone()))? else {
            return Err(unexpected());
        };
        if objects.is_empty() || objects.len() > request.len() {
            return Err(unexpected());
        }
        for ((k, id, bytes), wanted) in objects.into_iter().zip(&request) {
            if (k, id) != *wanted {
                return Err(unexpected());
            }
            if kind.id(&bytes) != id {
                return Err(ProtocolError::new(
                    "id_mismatch",
                    format!("received bytes do not match {id}"),
                ));
            }
            out.push((id, bytes));
        }
        rest = &ids[out.len()..];
    }
    Ok(out)
}
