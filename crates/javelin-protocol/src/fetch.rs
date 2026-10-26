//! Reference fetch: resolves a version, verifies its tree root against the version ID
//! chain (from the caller's trust anchor, or from World 0 without one), then reads the tree from the root down, one level per round
//! trip, and the blobs last. With a path prefix it reads only the chunks on the way to
//! the prefix and the subtree below it. Objects already held locally are not requested,
//! every received object is checked against its ID before it is used, and the walk
//! refuses to name more than `MAX_FETCH_PATHS` paths.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};

use javelin::{
    FileEntry, MAX_DEPTH, MAX_PATH, ObjectId, RepoPath, VersionHasher, VersionNumber,
    genesis_version_id,
    tree::{Chunk, Node, Store, empty},
};

use crate::{
    Kind, Message, ProtocolError,
    codec::VERSION,
    limits::*,
    sender::{exchange, unexpected},
};

/// A tree root that the version ID chain does not commit to: the server named a root,
/// a changeset or an ID that does not hash to the chain.
pub const CHAIN_MISMATCH: &str = "chain_mismatch";

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

/// A version the caller verified earlier and kept: its number, chain ID and root. A
/// fetch given an anchor verifies only the versions between the anchor and its target,
/// so its cost follows that distance, not the length of the history. A server whose
/// chain does not pass through the anchor is refused. Every fetched version is
/// verified, so [`Fetched::anchor`] is a valid anchor for the next fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub number: VersionNumber,
    pub id: ObjectId,
    pub tree: ObjectId,
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

impl Fetched {
    /// The fetched version, verified, to keep as the anchor for later fetches.
    pub fn anchor(&self) -> Anchor {
        Anchor {
            number: self.number,
            id: self.id,
            tree: self.tree,
        }
    }
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
/// go to `trees`, which also serves chunks held from earlier fetches. The root is
/// verified against `anchor`, or against the chain from World 0 when it is None.
pub fn fetch(
    mut r: impl Read,
    mut w: impl Write,
    trees: &impl Store,
    blobs: &mut impl Blobs,
    anchor: Option<&Anchor>,
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
    verify_chain(&mut call, anchor, version.is_none(), number, id, root)?;
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
    let mut named = 0usize;
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
            named += walk(
                &chunk,
                want,
                &mut next,
                &mut fetched.files,
                &mut wanted_blobs,
            )?;
            if named > MAX_FETCH_PATHS {
                return Err(ProtocolError::new(
                    "limit_exceeded",
                    format!("fetch names more than {MAX_FETCH_PATHS} paths"),
                ));
            }
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

/// Checks that `tree` is the root that version `number` commits to, relative to
/// `anchor`. A version ID hashes its parent's ID, its root and its changeset, so one
/// trusted ID proves every version linked to it:
///
/// - after the anchor, each version from `anchor + 1` to `number` must hash onto the
///   one below it, starting from the anchor's ID;
/// - at the anchor, the server must name exactly the anchor's ID and root;
/// - before the anchor (an explicit older version only), the walk runs down from the
///   anchor: each version's parent ID is trusted once the version hashes to its
///   trusted ID, and one more link checks the target's own root. A head behind the
///   anchor is a rollback and is refused.
///
/// Without an anchor the walk starts at World 0's fixed ID. Work is one link per
/// version between the anchor and the target. Each changeset streams in parts and is
/// hashed as it arrives, never held whole. Nothing is stored before this passes.
fn verify_chain(
    call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    anchor: Option<&Anchor>,
    head: bool,
    number: VersionNumber,
    id: ObjectId,
    tree: ObjectId,
) -> Result<(), ProtocolError> {
    let anchor = match anchor {
        Some(anchor) => *anchor,
        None => Anchor {
            number: VersionNumber::new(0)?,
            id: genesis_version_id(),
            tree: empty(),
        },
    };
    if number == anchor.number {
        return if (id, tree) == (anchor.id, anchor.tree) {
            Ok(())
        } else {
            Err(mismatch(number))
        };
    }
    if number > anchor.number {
        let mut parent = anchor.id;
        for n in anchor.number.get() + 1..=number.get() {
            let version = VersionNumber::new(n)?;
            let (claimed, root) = if version == number {
                (id, tree)
            } else {
                resolve(call, version)?
            };
            if link(call, version, parent, root)? != claimed {
                return Err(mismatch(version));
            }
            parent = claimed;
        }
        return Ok(());
    }
    if head {
        return Err(ProtocolError::new(
            CHAIN_MISMATCH,
            format!(
                "the server's head, World {number}, is behind the anchor at World {}",
                anchor.number
            ),
        ));
    }
    // `trusted` is the verified ID of the version being linked and `root` its claimed root.
    let (mut trusted, mut root) = (anchor.id, anchor.tree);
    for n in (number.get()..=anchor.number.get()).rev() {
        let version = VersionNumber::new(n)?;
        let Some(below) = n.checked_sub(1) else {
            // World 0 has no parent: its ID and root are fixed.
            return if (trusted, root) == (genesis_version_id(), empty()) {
                Ok(())
            } else {
                Err(mismatch(version))
            };
        };
        let below = VersionNumber::new(below)?;
        let (parent, parent_root) = if below == number {
            (id, tree)
        } else {
            resolve(call, below)?
        };
        if link(call, version, parent, root)? != trusted {
            return Err(mismatch(version));
        }
        (trusted, root) = (parent, parent_root);
    }
    Ok(())
}

fn mismatch(version: VersionNumber) -> ProtocolError {
    ProtocolError::new(
        CHAIN_MISMATCH,
        format!("version {version} does not commit to the chain and tree its server named"),
    )
}

/// The ID and root the server claims for `version`.
fn resolve(
    call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    version: VersionNumber,
) -> Result<(ObjectId, ObjectId), ProtocolError> {
    match call(Message::Resolve {
        version: Some(version),
    })? {
        Message::Resolved {
            number: got,
            id,
            tree,
        } if got == version => Ok((id, tree)),
        _ => Err(unexpected()),
    }
}

/// The ID that `version` has over `parent` and `root`, hashing its changeset as the
/// parts arrive.
fn link(
    call: &mut impl FnMut(Message) -> Result<Message, ProtocolError>,
    version: VersionNumber,
    parent: ObjectId,
    root: ObjectId,
) -> Result<ObjectId, ProtocolError> {
    let mut hash = VersionHasher::new(Some(parent), root);
    let mut offset = 0u64;
    let mut total = None;
    loop {
        let Message::ChangesetPart {
            total: whole,
            bytes,
        } = call(Message::Changeset { version, offset })?
        else {
            return Err(unexpected());
        };
        if whole > MAX_CHANGESET as u64 {
            return Err(ProtocolError::new(
                "limit_exceeded",
                format!("changeset of version {version} exceeds {MAX_CHANGESET} bytes"),
            ));
        }
        if *total.get_or_insert(whole) != whole || offset + bytes.len() as u64 > whole {
            return Err(ProtocolError::new(
                "malformed",
                format!("changeset part of version {version} disagrees with its length"),
            ));
        }
        if bytes.is_empty() && offset < whole {
            return Err(ProtocolError::new(
                "malformed",
                format!("empty changeset part of version {version}"),
            ));
        }
        hash.update(&bytes);
        offset += bytes.len() as u64;
        if offset == whole {
            return Ok(hash.finish());
        }
    }
}

/// Follows one chunk's references according to `want`. Returns how many named paths
/// or chunk references it followed, which the caller bounds by `MAX_FETCH_PATHS`.
fn walk(
    chunk: &Chunk,
    want: Want,
    next: &mut Vec<Pending>,
    files: &mut BTreeMap<RepoPath, FileEntry>,
    blobs: &mut Vec<ObjectId>,
) -> Result<usize, ProtocolError> {
    if chunk.level() > 0 {
        let level = chunk.level() - 1;
        let claim = |(last, id): (&str, ObjectId)| {
            let link = Link {
                level,
                last: last.to_owned(),
            };
            (id, want.clone(), Some(link))
        };
        let before = next.len();
        match &want {
            Want::All { .. } => next.extend(chunk.child_links().map(claim)),
            Want::Path { rest, .. } => next.extend(chunk.child_for(&rest[0]).map(claim)),
        }
        return Ok(next.len() - before);
    }
    let mut named = 0;
    for (name, node) in chunk.entries() {
        let (dir, below) = match &want {
            Want::All { dir } => (dir, None),
            Want::Path { dir, rest } if rest[0] == name => (dir, Some(&rest[1..])),
            Want::Path { .. } => continue,
        };
        named += 1;
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
    Ok(named)
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
