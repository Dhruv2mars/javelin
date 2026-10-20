//! Merkle directory trees: the derived, content-addressed snapshot of a World version.
//!
//! Changesets are the source of truth; a version's tree is computed from its
//! parent's tree plus its changeset, rewriting only the directories on the path of
//! a changed entry. Unchanged subtrees are shared by hash.
//!
//! Each directory is a prolly tree over its entries sorted by name. Items are cut
//! into chunks at content-defined boundaries, and every level above the leaves
//! lists its child chunks by their last name. A boundary follows an item when a
//! hash of its level and name falls below a threshold that rises with the chunk's
//! size (mean `TARGET` bytes), or when the chunk reaches `FORCE` bytes. Boundaries
//! depend only on names, so:
//!
//! - the same entries give the same chunks whatever the edit order (history
//!   independence), so a root hash identifies a snapshot;
//! - a content edit rewrites exactly one chunk per level;
//! - an insert or delete re-cuts only the chunks around it.
//!
//! A small directory is a single chunk. Chunks are `ObjectKind::Tree` objects.
//! Encoding: `level u8`, then for each item `tag u8`, `name length u16` (big
//! endian), the name and a 32-byte ID, with names strictly ascending. Leaf tags are
//! 1 regular file, 2 executable file, 3 symlink and 4 directory (the ID is the
//! child directory's root chunk). Items above the leaves have tag 0 and name their
//! child chunk's last name. The empty World is the empty leaf chunk `[0]`; no other
//! chunk is empty.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{
    Error, FileEntry, ObjectId, RepoPath, Result, SparseChanges,
    domain::{MAX_DEPTH, MAX_PATH, ObjectKind, valid_name},
};

/// Expected encoded chunk size in bytes.
pub const TARGET: usize = 2048;
/// A chunk is cut once its items reach this many encoded bytes.
pub const FORCE: usize = 64 * 1024;
/// Highest chunk level. Each level needs names that are boundaries at every level
/// below, so real trees stay a few levels high.
pub const MAX_LEVEL: u8 = 32;
/// Bytes of an item besides its name: tag, name length and ID.
const ITEM: usize = 35;
/// Upper bound on one encoded chunk, enforced by the decoder.
pub const MAX_CHUNK: usize = 1 + FORCE + ITEM + MAX_PATH;

const CHILD: u8 = 0;
const REGULAR: u8 = 1;
const EXECUTABLE: u8 = 2;
const SYMLINK: u8 = 3;
const DIR: u8 = 4;

/// What a directory entry names: a file, or a subdirectory by its root chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Node {
    File(FileEntry),
    Dir(ObjectId),
}

impl Node {
    fn file(&self) -> Option<FileEntry> {
        match self {
            Self::File(entry) => Some(entry.clone()),
            Self::Dir(_) => None,
        }
    }
    fn dir(&self) -> Option<ObjectId> {
        match self {
            Self::Dir(id) => Some(*id),
            Self::File(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Item {
    name: String,
    tag: u8,
    id: ObjectId,
}

impl Item {
    fn new(name: &str, node: &Node) -> Self {
        let (tag, id) = match node {
            Node::File(FileEntry::Regular {
                content,
                executable,
            }) => (if *executable { EXECUTABLE } else { REGULAR }, *content),
            Node::File(FileEntry::Symlink { target }) => (SYMLINK, *target),
            Node::Dir(id) => (DIR, *id),
        };
        Self {
            name: name.into(),
            tag,
            id,
        }
    }
    fn node(&self) -> Node {
        match self.tag {
            REGULAR | EXECUTABLE => Node::File(FileEntry::Regular {
                content: self.id,
                executable: self.tag == EXECUTABLE,
            }),
            SYMLINK => Node::File(FileEntry::Symlink { target: self.id }),
            _ => Node::Dir(self.id),
        }
    }
    fn len(&self) -> usize {
        ITEM + self.name.len()
    }
}

/// One decoded tree object.
#[derive(Debug, Eq, PartialEq)]
pub struct Chunk {
    level: u8,
    items: Vec<Item>,
}

impl Chunk {
    pub fn level(&self) -> u8 {
        self.level
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.items.iter().map(Item::len).sum::<usize>());
        out.push(self.level);
        for item in &self.items {
            out.push(item.tag);
            out.extend_from_slice(&(item.name.len() as u16).to_be_bytes());
            out.extend_from_slice(item.name.as_bytes());
            out.extend_from_slice(item.id.as_bytes());
        }
        out
    }

    /// Parses one chunk, rejecting anything but the canonical encoding: bounded
    /// size and level, valid ascending names, tags valid for the level, and no
    /// trailing bytes. Chunk boundaries are checked where trees are derived.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let bad = |why: &str| Error::InvalidInput(format!("malformed tree chunk: {why}"));
        if bytes.len() > MAX_CHUNK {
            return Err(bad("too large"));
        }
        let (&level, mut rest) = bytes.split_first().ok_or_else(|| bad("empty"))?;
        if level > MAX_LEVEL {
            return Err(bad("level too high"));
        }
        let mut items: Vec<Item> = Vec::new();
        while let [tag, a, b, tail @ ..] = rest {
            let len = u16::from_be_bytes([*a, *b]) as usize;
            if tail.len() < len + 32 {
                return Err(bad("truncated item"));
            }
            let name = std::str::from_utf8(&tail[..len]).map_err(|_| bad("name is not UTF-8"))?;
            if !valid_name(name) {
                return Err(bad("invalid name"));
            }
            if items.last().is_some_and(|last| last.name.as_str() >= name) {
                return Err(bad("names are not strictly ascending"));
            }
            if (level == 0) == (*tag == CHILD) || *tag > DIR {
                return Err(bad("tag invalid for level"));
            }
            items.push(Item {
                name: name.into(),
                tag: *tag,
                id: ObjectId::from_bytes(tail[len..len + 32].try_into().unwrap()),
            });
            rest = &tail[len + 32..];
        }
        if !rest.is_empty() {
            return Err(bad("trailing bytes"));
        }
        if level > 0 && items.is_empty() {
            return Err(bad("empty internal chunk"));
        }
        Ok(Self { level, items })
    }

    /// Child chunk IDs, for a chunk above the leaves.
    pub fn children(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.items
            .iter()
            .filter(|item| item.tag == CHILD)
            .map(|item| item.id)
    }

    /// Directory entries, for a leaf chunk.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Node)> + '_ {
        self.items
            .iter()
            .filter(|item| item.tag != CHILD)
            .map(|item| (item.name.as_str(), item.node()))
    }

    /// The child chunk that holds `name` if anything does, for a chunk above the
    /// leaves: the first whose last name is at least `name`.
    pub fn child_for(&self, name: &str) -> Option<ObjectId> {
        let index = self.items.partition_point(|item| item.name.as_str() < name);
        self.items
            .get(index)
            .filter(|i| i.tag == CHILD)
            .map(|i| i.id)
    }

    fn last(&self) -> &str {
        self.items.last().map_or("", |item| item.name.as_str())
    }
}

/// `prefix + name`, refused beyond the native path limits so a deep stored or
/// received tree cannot exhaust the stack.
fn joined(prefix: &str, name: &str) -> Result<String> {
    let path = format!("{prefix}{name}");
    if path.len() > MAX_PATH || path.matches('/').count() >= MAX_DEPTH {
        return Err(Error::InvalidInput("tree path exceeds path limits".into()));
    }
    Ok(path)
}

/// The root of the empty World.
pub fn empty() -> ObjectId {
    ObjectId::hash(ObjectKind::Tree, &[0])
}

/// Raw tree object storage.
pub trait Store {
    /// The bytes of a stored tree object, verified against `id`.
    fn load(&self, id: ObjectId) -> Result<Vec<u8>>;
    /// Records a new chunk whose hash is `id`.
    fn save(&self, id: ObjectId, bytes: &[u8]) -> Result<()>;
}

/// In-memory store for tests, benchmarks and transfer staging.
#[derive(Default)]
pub struct MemoryStore {
    objects: RefCell<HashMap<ObjectId, Vec<u8>>>,
    written: Cell<usize>,
}

impl MemoryStore {
    /// Total bytes of distinct chunks stored.
    pub fn bytes(&self) -> usize {
        self.written.get()
    }
    pub fn len(&self) -> usize {
        self.objects.borrow().len()
    }
    pub fn is_empty(&self) -> bool {
        self.objects.borrow().is_empty()
    }
}

impl Store for MemoryStore {
    fn load(&self, id: ObjectId) -> Result<Vec<u8>> {
        self.objects
            .borrow()
            .get(&id)
            .cloned()
            .ok_or(Error::MissingObject(id))
    }
    fn save(&self, id: ObjectId, bytes: &[u8]) -> Result<()> {
        if self
            .objects
            .borrow_mut()
            .insert(id, bytes.to_vec())
            .is_none()
        {
            self.written.set(self.written.get() + bytes.len());
        }
        Ok(())
    }
}

/// Tree operations over a store, with a cache of decoded chunks.
pub struct Trees<S> {
    store: S,
    cache: RefCell<HashMap<ObjectId, Arc<Chunk>>>,
}

type Change<'a> = (&'a str, Option<&'a FileEntry>);
/// A name and its node before and after.
type ItemDiff = (String, Option<Node>, Option<Node>);

impl<S: Store> Trees<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            cache: RefCell::default(),
        }
    }
    pub fn store(&self) -> &S {
        &self.store
    }
    /// Drops the decoded-chunk cache. Callers that walk a long history call this
    /// per version so memory stays bounded by one version's chunks.
    pub fn forget(&self) {
        self.cache.borrow_mut().clear();
    }
    pub fn into_store(self) -> S {
        self.store
    }

    fn chunk(&self, id: ObjectId, cache: bool) -> Result<Arc<Chunk>> {
        if let Some(chunk) = self.cache.borrow().get(&id) {
            return Ok(chunk.clone());
        }
        // The empty World needs no stored object.
        let bytes = if id == empty() {
            vec![0]
        } else {
            self.store.load(id)?
        };
        if ObjectId::hash(ObjectKind::Tree, &bytes) != id {
            return Err(Error::CorruptObject(id));
        }
        let chunk = Arc::new(Chunk::decode(&bytes).map_err(|_| Error::CorruptObject(id))?);
        if cache {
            self.cache.borrow_mut().insert(id, chunk.clone());
        }
        Ok(chunk)
    }

    fn get(&self, id: ObjectId) -> Result<Arc<Chunk>> {
        self.chunk(id, true)
    }

    /// The child chunk of an internal item, checked against its parent.
    fn child(&self, parent: &Chunk, item: &Item, cache: bool) -> Result<Arc<Chunk>> {
        let child = self.chunk(item.id, cache)?;
        if child.level + 1 != parent.level || child.items.is_empty() || child.last() != item.name {
            return Err(Error::CorruptObject(item.id));
        }
        Ok(child)
    }

    fn put(&self, chunk: Chunk) -> Result<ObjectId> {
        let bytes = chunk.encode();
        let id = ObjectId::hash(ObjectKind::Tree, &bytes);
        if !self.cache.borrow().contains_key(&id) {
            self.store.save(id, &bytes)?;
            self.cache.borrow_mut().insert(id, Arc::new(chunk));
        }
        Ok(id)
    }

    /// Builds the tree of a complete snapshot. Equal to applying it to `empty()`.
    pub fn build<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a RepoPath, &'a FileEntry)>,
    ) -> Result<ObjectId> {
        let changes: Vec<Change<'a>> = entries
            .into_iter()
            .map(|(p, e)| (p.as_str(), Some(e)))
            .collect();
        self.root(self.apply_dir(None, &changes)?)
    }

    /// The root after applying `changes` to the snapshot at `root`. Work is
    /// O(changed paths x depth) chunk reads and writes. Deleting a missing file is
    /// a no-op; a file and a descendant, or deleting a directory by name, is refused.
    pub fn apply(&self, root: ObjectId, changes: &SparseChanges) -> Result<ObjectId> {
        let changes: Vec<Change<'_>> = changes
            .entries()
            .iter()
            .map(|(p, e)| (p.as_str(), e.as_ref()))
            .collect();
        self.root(self.apply_dir(Some(root), &changes)?)
    }

    fn root(&self, dir: Option<ObjectId>) -> Result<ObjectId> {
        match dir {
            Some(root) => Ok(root),
            None => self.put(Chunk {
                level: 0,
                items: Vec::new(),
            }),
        }
    }

    fn apply_dir(&self, dir: Option<ObjectId>, changes: &[Change<'_>]) -> Result<Option<ObjectId>> {
        type Group<'a> = (Option<Option<&'a FileEntry>>, Vec<Change<'a>>);
        let mut groups: BTreeMap<&str, Group<'_>> = BTreeMap::new();
        for &(path, entry) in changes {
            match path.split_once('/') {
                None => groups.entry(path).or_default().0 = Some(entry),
                Some((name, rest)) => groups.entry(name).or_default().1.push((rest, entry)),
            }
        }
        let mut edits = Vec::new();
        for (name, (exact, deeper)) in groups {
            let current = match dir {
                Some(dir) => self.find(dir, name)?,
                None => None,
            };
            let file = current.as_ref().and_then(Node::file);
            let sub = current.as_ref().and_then(Node::dir);
            if exact == Some(None) && file.is_none() && (sub.is_some() || !deeper.is_empty()) {
                return Err(Error::InvalidInput(
                    "directory-prefix tombstone is ambiguous; delete exact entries".into(),
                ));
            }
            let file = exact.map_or(file, |e| e.cloned());
            let sub = if deeper.is_empty() {
                sub
            } else {
                self.apply_dir(sub, &deeper)?
            };
            let next = match (file, sub) {
                (Some(_), Some(_)) => {
                    return Err(Error::InvalidInput(
                        "file and descendant paths collide".into(),
                    ));
                }
                (Some(file), None) => Some(Node::File(file)),
                (None, Some(dir)) => Some(Node::Dir(dir)),
                (None, None) => None,
            };
            if next != current {
                edits.push((name.to_owned(), next.map(|n| Item::new(name, &n))));
            }
        }
        if edits.is_empty() {
            return Ok(dir);
        }
        self.update(dir, edits)
    }

    /// Applies sorted name edits to one directory's prolly tree, re-cutting only
    /// the chunks around each edit. None means the directory is now empty.
    fn update(
        &self,
        dir: Option<ObjectId>,
        edits: Vec<(String, Option<Item>)>,
    ) -> Result<Option<ObjectId>> {
        let root = match dir {
            Some(root) if !self.get(root)?.items.is_empty() => root,
            _ => return self.build_level(0, edits.into_iter().filter_map(|(_, i)| i)),
        };
        let top = self.get(root)?.level;
        let mut edits = edits;
        for level in 0..top {
            edits = self
                .update_level(root, level, &edits)?
                .into_iter()
                .collect();
        }
        // The root is the only chunk at the top level: re-cut it whole, then grow
        // or shrink the tree to the first level with one chunk.
        let chunk = self.get(root)?;
        let mut items = Vec::with_capacity(chunk.items.len() + edits.len());
        let mut edits = edits.into_iter().peekable();
        for item in &chunk.items {
            while let Some((_, edit)) = edits.next_if(|(name, _)| *name < item.name) {
                items.extend(edit);
            }
            match edits.next_if(|(name, _)| *name == item.name) {
                Some((_, edit)) => items.extend(edit),
                None => items.push(item.clone()),
            }
        }
        items.extend(edits.filter_map(|(_, edit)| edit));
        self.build_level(top, items)
    }

    /// Re-cuts the chunks at `level` that `edits` touch, continuing past each edit
    /// until a new boundary meets an old one. Returns the matching edits one level up.
    fn update_level(
        &self,
        root: ObjectId,
        level: u8,
        edits: &[(String, Option<Item>)],
    ) -> Result<BTreeMap<String, Option<Item>>> {
        let corrupt = || Error::CorruptObject(root);
        let mut out = BTreeMap::new();
        let mut next = 0;
        while next < edits.len() {
            let (mut chunk, mut rightmost) = self
                .seek(root, level, &edits[next].0, false)?
                .ok_or_else(corrupt)?;
            let mut cut = Chunker::new(level);
            let mut added = Vec::new();
            loop {
                out.insert(chunk.last().to_owned(), None);
                let end = (!rightmost).then(|| chunk.last());
                let mut items = chunk.items.iter().peekable();
                loop {
                    let edit = edits
                        .get(next)
                        .filter(|(name, _)| end.is_none_or(|end| name.as_str() <= end));
                    let item = match (items.peek(), edit) {
                        (None, None) => break,
                        (Some(item), Some((name, edit))) if *name <= item.name => {
                            if *name == item.name {
                                items.next();
                            }
                            next += 1;
                            edit.clone()
                        }
                        (Some(_), _) => items.next().cloned(),
                        (None, Some((_, edit))) => {
                            next += 1;
                            edit.clone()
                        }
                    };
                    if let Some(done) = item.and_then(|item| cut.push(item)) {
                        added.push(self.summary(done)?);
                    }
                }
                if rightmost || cut.is_empty() {
                    break;
                }
                let following = self.seek(root, level, chunk.last(), true)?;
                (chunk, rightmost) = following.ok_or_else(corrupt)?;
            }
            if let Some(done) = cut.finish() {
                added.push(self.summary(done)?);
            }
            for item in added {
                out.insert(item.name.clone(), Some(item));
            }
        }
        Ok(out)
    }

    /// The chunk at `level` holding `name`: the first whose last name is at least
    /// `name` (greater when `after`); otherwise, unless `after`, the rightmost.
    /// Also reports whether it is the rightmost chunk at its level.
    fn seek(
        &self,
        root: ObjectId,
        level: u8,
        name: &str,
        after: bool,
    ) -> Result<Option<(Arc<Chunk>, bool)>> {
        let mut chunk = self.get(root)?;
        let mut rightmost = true;
        while chunk.level > level {
            let index = chunk.items.partition_point(|item| {
                if after {
                    item.name.as_str() <= name
                } else {
                    item.name.as_str() < name
                }
            });
            let index = match index {
                i if i < chunk.items.len() => i,
                _ if after => return Ok(None),
                _ => chunk.items.len() - 1,
            };
            rightmost &= index + 1 == chunk.items.len();
            chunk = self.child(&chunk, &chunk.items[index], true)?;
        }
        if chunk.level != level || (after && chunk.last() <= name) {
            return Ok(None);
        }
        Ok(Some((chunk, rightmost)))
    }

    fn summary(&self, chunk: Chunk) -> Result<Item> {
        let name = chunk.last().to_owned();
        Ok(Item {
            name,
            tag: CHILD,
            id: self.put(chunk)?,
        })
    }

    /// Cuts a complete level and builds upward until one chunk remains.
    fn build_level(
        &self,
        level: u8,
        items: impl IntoIterator<Item = Item>,
    ) -> Result<Option<ObjectId>> {
        if level > MAX_LEVEL {
            return Err(Error::InvalidInput("directory tree too high".into()));
        }
        let mut cut = Chunker::new(level);
        let mut chunks: Vec<Chunk> = items.into_iter().filter_map(|i| cut.push(i)).collect();
        chunks.extend(cut.finish());
        if chunks.len() > 1 {
            let summaries = chunks
                .into_iter()
                .map(|chunk| self.summary(chunk))
                .collect::<Result<Vec<_>>>()?;
            return self.build_level(level + 1, summaries);
        }
        let Some(chunk) = chunks.pop() else {
            return Ok(None);
        };
        // A shrunken top level can hold one child; the first level with more than
        // one item, or the leaves, is the canonical root.
        let mut root = if chunk.level > 0 && chunk.items.len() == 1 {
            chunk.items[0].id
        } else {
            return self.put(chunk).map(Some);
        };
        loop {
            let chunk = self.get(root)?;
            if chunk.level == 0 || chunk.items.len() > 1 {
                return Ok(Some(root));
            }
            root = chunk.items[0].id;
        }
    }

    /// The entry named `name` directly inside directory `dir`.
    fn find(&self, dir: ObjectId, name: &str) -> Result<Option<Node>> {
        let Some((leaf, _)) = self.seek(dir, 0, name, false)? else {
            return Ok(None);
        };
        Ok(leaf
            .items
            .binary_search_by(|item| item.name.as_str().cmp(name))
            .ok()
            .map(|index| leaf.items[index].node()))
    }

    /// The node at `path`, if any. O(depth) chunk reads.
    pub fn lookup(&self, root: ObjectId, path: &RepoPath) -> Result<Option<Node>> {
        let mut dir = root;
        let mut names = path.as_str().split('/').peekable();
        while let Some(name) = names.next() {
            match self.find(dir, name)? {
                Some(node) if names.peek().is_none() => return Ok(Some(node)),
                Some(Node::Dir(id)) => dir = id,
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The complete snapshot at `root`. O(snapshot), independent of history.
    pub fn entries(&self, root: ObjectId) -> Result<BTreeMap<RepoPath, FileEntry>> {
        let mut out = BTreeMap::new();
        self.walk(root, "", &mut |path, entry| {
            out.insert(path, entry);
        })?;
        Ok(out)
    }

    /// Every file strictly below `dir` in the snapshot at `root`.
    pub fn files_under(&self, root: ObjectId, dir: &RepoPath) -> Result<Vec<RepoPath>> {
        let mut out = Vec::new();
        if let Some(Node::Dir(id)) = self.lookup(root, dir)? {
            self.walk(id, &format!("{}/", dir.as_str()), &mut |path, _| {
                out.push(path)
            })?;
        }
        Ok(out)
    }

    fn walk(
        &self,
        dir: ObjectId,
        prefix: &str,
        f: &mut impl FnMut(RepoPath, FileEntry),
    ) -> Result<()> {
        let mut items = Vec::new();
        self.leaves(&*self.chunk(dir, false)?, &mut items)?;
        for item in items {
            let path = joined(prefix, &item.name)?;
            match item.node() {
                Node::File(entry) => f(RepoPath(path), entry),
                Node::Dir(id) => self.walk(id, &format!("{path}/"), f)?,
            }
        }
        Ok(())
    }

    /// All items of one directory in order. Walks are not cached.
    fn leaves(&self, chunk: &Chunk, out: &mut Vec<Item>) -> Result<()> {
        if chunk.level == 0 {
            out.extend(chunk.items.iter().cloned());
            return Ok(());
        }
        for item in &chunk.items {
            self.leaves(&*self.child(chunk, item, false)?, out)?;
        }
        Ok(())
    }

    /// The changeset from snapshot `from` to snapshot `to`. Equal subtrees and
    /// equal chunks are skipped by hash, so work follows the difference.
    pub fn diff(&self, from: ObjectId, to: ObjectId) -> Result<SparseChanges> {
        let mut out = BTreeMap::new();
        self.diff_dir(Some(from), Some(to), "", &mut out)?;
        SparseChanges::new(out)
    }

    fn diff_dir(
        &self,
        from: Option<ObjectId>,
        to: Option<ObjectId>,
        prefix: &str,
        out: &mut BTreeMap<RepoPath, Option<FileEntry>>,
    ) -> Result<()> {
        if from == to {
            return Ok(());
        }
        for (name, before, after) in self.diff_items(from, to)? {
            let path = joined(prefix, &name)?;
            let file = after.as_ref().and_then(Node::file);
            if before.as_ref().and_then(Node::file) != file {
                out.insert(RepoPath(path.clone()), file);
            }
            let (from, to) = (
                before.as_ref().and_then(Node::dir),
                after.as_ref().and_then(Node::dir),
            );
            if from != to {
                self.diff_dir(from, to, &format!("{path}/"), out)?;
            }
        }
        Ok(())
    }

    /// Items that differ between two directories. Chunks present on both sides
    /// cover identical runs of items, so they are dropped before expanding the
    /// highest remaining level.
    fn diff_items(&self, from: Option<ObjectId>, to: Option<ObjectId>) -> Result<Vec<ItemDiff>> {
        type Side = Vec<(ObjectId, Arc<Chunk>)>;
        let load = |id: Option<ObjectId>| -> Result<Side> {
            id.map(|id| Ok((id, self.get(id)?))).into_iter().collect()
        };
        let (mut a, mut b) = (load(from)?, load(to)?);
        loop {
            let ids: HashSet<ObjectId> = a.iter().map(|(id, _)| *id).collect();
            let shared: HashSet<ObjectId> = b
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| ids.contains(id))
                .collect();
            a.retain(|(id, _)| !shared.contains(id));
            b.retain(|(id, _)| !shared.contains(id));
            let level = a.iter().chain(&b).map(|(_, c)| c.level).max().unwrap_or(0);
            if level == 0 {
                break;
            }
            let expand = |side: Side| -> Result<Side> {
                let mut out = Vec::new();
                for (id, chunk) in side {
                    if chunk.level < level {
                        out.push((id, chunk));
                        continue;
                    }
                    for item in &chunk.items {
                        out.push((item.id, self.child(&chunk, item, true)?));
                    }
                }
                Ok(out)
            };
            (a, b) = (expand(a)?, expand(b)?);
        }
        let mut left = a.iter().flat_map(|(_, c)| c.items.iter()).peekable();
        let mut right = b.iter().flat_map(|(_, c)| c.items.iter()).peekable();
        let mut out = Vec::new();
        loop {
            let (l, r) = match (left.peek(), right.peek()) {
                (None, None) => break,
                (Some(l), Some(r)) if l.name == r.name => (left.next(), right.next()),
                (Some(l), Some(r)) if l.name < r.name => (left.next(), None),
                (Some(_), None) => (left.next(), None),
                _ => (None, right.next()),
            };
            if l != r {
                let name = l.or(r).map(|i| i.name.clone()).unwrap_or_default();
                out.push((name, l.map(Item::node), r.map(Item::node)));
            }
        }
        Ok(out)
    }

    /// Sorted endpoints of every exact or ancestor/descendant intersection between
    /// the changed paths and the files that differ between snapshots `base` and
    /// `head`. A directory whose subtree hash is equal on both sides is skipped
    /// with every changed path below it.
    pub fn intersections(
        &self,
        base: ObjectId,
        head: ObjectId,
        changes: &SparseChanges,
    ) -> Result<BTreeSet<RepoPath>> {
        let paths: Vec<&str> = changes.entries().keys().map(RepoPath::as_str).collect();
        let mut found = BTreeSet::new();
        self.intersect(Some(base), Some(head), "", &paths, &mut found)?;
        Ok(found)
    }

    fn intersect(
        &self,
        base: Option<ObjectId>,
        head: Option<ObjectId>,
        prefix: &str,
        paths: &[&str],
        found: &mut BTreeSet<RepoPath>,
    ) -> Result<()> {
        if base == head {
            return Ok(());
        }
        let mut groups: BTreeMap<&str, (bool, Vec<&str>)> = BTreeMap::new();
        for path in paths {
            match path.split_once('/') {
                None => groups.entry(path).or_default().0 = true,
                Some((name, rest)) => groups.entry(name).or_default().1.push(rest),
            }
        }
        let find = |dir: Option<ObjectId>, name| dir.map_or(Ok(None), |d| self.find(d, name));
        for (name, (exact, deeper)) in groups {
            let (before, after) = (find(base, name)?, find(head, name)?);
            if before == after {
                continue;
            }
            let path = format!("{prefix}{name}");
            let files_differ =
                before.as_ref().and_then(Node::file) != after.as_ref().and_then(Node::file);
            let (from, to) = (
                before.as_ref().and_then(Node::dir),
                after.as_ref().and_then(Node::dir),
            );
            if exact {
                if files_differ {
                    found.insert(RepoPath(path.clone()));
                }
                let mut below = BTreeMap::new();
                self.diff_dir(from, to, &format!("{path}/"), &mut below)?;
                if !below.is_empty() {
                    found.insert(RepoPath(path.clone()));
                    found.extend(below.into_keys());
                }
            }
            if !deeper.is_empty() {
                if files_differ {
                    found.insert(RepoPath(path.clone()));
                    found.extend(deeper.iter().map(|rest| RepoPath(format!("{path}/{rest}"))));
                }
                self.intersect(from, to, &format!("{path}/"), &deeper, found)?;
            }
        }
        Ok(())
    }
}

/// Cuts one level's items into chunks.
struct Chunker {
    level: u8,
    items: Vec<Item>,
    size: usize,
}

impl Chunker {
    fn new(level: u8) -> Self {
        Self {
            level,
            items: Vec::new(),
            size: 0,
        }
    }
    fn push(&mut self, item: Item) -> Option<Chunk> {
        let len = item.len();
        self.size += len;
        // A chunk holds at least two items, so every level with two or more items
        // shrinks and the tree converges, even when each item is past TARGET.
        let cut = !self.items.is_empty()
            && (self.size >= FORCE || boundary(self.level, &item.name, len, self.size));
        self.items.push(item);
        cut.then(|| self.take())
    }
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    fn take(&mut self) -> Chunk {
        self.size = 0;
        Chunk {
            level: self.level,
            items: std::mem::take(&mut self.items),
        }
    }
    fn finish(mut self) -> Option<Chunk> {
        (!self.items.is_empty()).then(|| self.take())
    }
}

/// `TARGET² · 2/π`: with a boundary hazard of `size / SPREAD` per byte, chunk
/// sizes follow a Rayleigh distribution with mean `TARGET`.
const SPREAD: u64 = (TARGET * TARGET) as u64 * 7 / 11;

/// A content-defined boundary follows an item with probability `len · size /
/// SPREAD`, decided by a hash of its level and name, where `size` is the chunk's
/// bytes including the item. The rising hazard keeps chunk sizes close
/// to `TARGET`, so an edit rarely lands in an outsized chunk. Item lengths depend
/// only on names, so boundaries do too.
fn boundary(level: u8, name: &str, len: usize, size: usize) -> bool {
    let hash = Sha256::new()
        .chain_update(b"javelin-tree-v1\0")
        .chain_update([level])
        .chain_update(name.as_bytes())
        .finalize();
    let h = u32::from_be_bytes(hash[..4].try_into().unwrap());
    u128::from(h) * u128::from(SPREAD) < ((len * size) as u128) << 32
}
