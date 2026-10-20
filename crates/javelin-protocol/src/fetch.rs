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
#[derive(Clone, PartialEq, Eq, Hash)]
enum Want {
    All { dir: String },
    Path { dir: String, rest: Vec<String> },
}

/// What a parent says about a child chunk: its level, and the last name it is
/// filed under. The child must match both, as native trees require.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Link {
    level: u8,
    last: String,
}

/// A chunk to read, with its want and the parent's claim (none for a directory root).
type Pending = (ObjectId, Want, Option<Link>);

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
    let mut pending: Vec<Pending> = vec![(root, start, None)];
    while !pending.is_empty() {
        // The same chunk under the same want, reached by two parents, is read once.
        let mut seen = HashSet::new();
        pending.retain(|entry| seen.insert(entry.clone()));
        let missing: Vec<ObjectId> = {
            let mut seen = HashSet::new();
            pending
                .iter()
                .map(|(id, _, _)| *id)
                .filter(|id| seen.insert(*id) && trees.load(*id).is_err())
                .collect()
        };
        fetched.trees_received += missing.len();
        get(&mut call, Kind::Tree, &missing, |id, bytes| {
            Ok(trees.save(id, &bytes)?)
        })?;
        let mut next = Vec::new();
        for (id, want, link) in pending {
            let chunk = Chunk::decode(&trees.load(id)?)?;
            if let Some(Link { level, last }) = &link
                && (chunk.level() != *level || chunk.last() != last)
            {
                return Err(ProtocolError::new(
                    "malformed",
                    format!("chunk {id} does not match the parent that names it"),
                ));
            }
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
    get(&mut call, Kind::Blob, &wanted_blobs, |id, bytes| {
        blobs.put(id, bytes)
    })?;
    Ok(fetched)
}

/// Follows one chunk's references according to `want`.
fn walk(
    chunk: &Chunk,
    want: Want,
    next: &mut Vec<Pending>,
    files: &mut BTreeMap<RepoPath, FileEntry>,
    blobs: &mut Vec<ObjectId>,
) -> Result<(), ProtocolError> {
    if chunk.level() > 0 {
        let level = chunk.level() - 1;
        let claim = |(last, id): (&str, ObjectId)| {
            let link = Link {
                level,
                last: last.to_owned(),
            };
            (id, want.clone(), Some(link))
        };
        match &want {
            Want::All { .. } => next.extend(chunk.child_links().map(claim)),
            Want::Path { rest, .. } => next.extend(chunk.child_for(&rest[0]).map(claim)),
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
            (Node::Dir(id), None | Some([])) => {
                next.push((id, Want::All { dir: path + "/" }, None))
            }
            (Node::Dir(id), Some(rest)) => next.push((
                id,
                Want::Path {
                    dir: path + "/",
                    rest: rest.to_vec(),
                },
                None,
            )),
        }
    }
    Ok(())
}

/// Requests `ids` and hands each verified object to `store` as its reply arrives,
/// re-requesting the rest when a reply answers only a prefix.
fn get(
    call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    kind: Kind,
    ids: &[ObjectId],
    mut store: impl FnMut(ObjectId, Vec<u8>) -> Result<(), ProtocolError>,
) -> Result<(), ProtocolError> {
    let mut done = 0;
    while done < ids.len() {
        let request: Vec<_> = ids[done..]
            .iter()
            .take(MAX_WANT)
            .map(|id| (kind, *id))
            .collect();
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
            store(id, bytes)?;
            done += 1;
        }
    }
    Ok(())
}
