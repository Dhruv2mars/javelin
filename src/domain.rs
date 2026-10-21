use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    /// Computes the canonical kind-tagged blob ID without storing content.
    pub fn for_blob(bytes: &[u8]) -> Self {
        Self::hash(ObjectKind::Blob, bytes)
    }

    /// Hashes canonical manifest bytes without storing or validating references.
    pub fn for_manifest(manifest: &Manifest) -> Self {
        Self::hash(ObjectKind::Manifest, &manifest.canonical_bytes())
    }

    /// Hashes tree chunk bytes without decoding or storing them.
    pub fn for_tree(bytes: &[u8]) -> Self {
        Self::hash(ObjectKind::Tree, bytes)
    }

    /// Hashes checkpoint bytes without decoding or storing them.
    pub fn for_checkpoint(bytes: &[u8]) -> Self {
        Self::hash(ObjectKind::Checkpoint, bytes)
    }

    /// Constructs a digest. This does not prove an object exists or is valid.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidInput(
                "object ID must be 64 lowercase hex digits".into(),
            ));
        }
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
                .map_err(|_| Error::InvalidInput("invalid object ID".into()))?;
        }
        Ok(Self(bytes))
    }

    pub(crate) fn hash(kind: ObjectKind, bytes: &[u8]) -> Self {
        let mut hash = Self::hasher(kind);
        hash.update(bytes);
        Self(hash.finalize().into())
    }

    pub(crate) fn hasher(kind: ObjectKind) -> Sha256 {
        let mut hash = Sha256::new();
        hash.update(b"javelin-object-v1\0");
        hash.update([kind as u8]);
        hash
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectKind {
    Blob = 1,
    Manifest = 2,
    Checkpoint = 3,
    Tree = 4,
}

impl ObjectKind {
    pub(crate) fn decode(kind: i64) -> Result<Self> {
        match kind {
            1 => Ok(Self::Blob),
            2 => Ok(Self::Manifest),
            3 => Ok(Self::Checkpoint),
            4 => Ok(Self::Tree),
            _ => Err(Error::CorruptRepository("unknown object kind".into())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct VersionNumber(u64);

impl VersionNumber {
    pub fn new(value: u64) -> Result<Self> {
        if value > i64::MAX as u64 {
            return Err(Error::InvalidInput(
                "World version exceeds storage range".into(),
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
    pub(crate) fn sql(self) -> i64 {
        self.0 as i64
    }
    pub(crate) fn decode(value: i64) -> Result<Self> {
        if value < 0 {
            return Err(Error::CorruptRepository("negative World version".into()));
        }
        Ok(Self(value as u64))
    }
}

impl fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Longest repository path in bytes, and most components in one path. Trees
/// encode a name's length as `u16` and recurse once per component. Both equal the
/// protocol's wire limits.
pub const MAX_PATH: usize = 4096;
pub const MAX_DEPTH: usize = 256;

/// One path component: a file or directory name inside its parent.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.eq_ignore_ascii_case(".javelin")
        && !name.contains(['\\', ':', '\0', '/'])
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RepoPath(pub(crate) String);

impl RepoPath {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > MAX_PATH
            || value.split('/').count() > MAX_DEPTH
            || !value.split('/').all(valid_name)
        {
            return Err(Error::InvalidInput(format!(
                "invalid repository-relative path {value:?}"
            )));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Proper ancestor directories, nearest last. Prefixes of a valid path are valid.
    pub(crate) fn ancestors(&self) -> impl Iterator<Item = RepoPath> + '_ {
        self.0
            .match_indices('/')
            .map(|(index, _)| RepoPath(self.0[..index].into()))
    }

    /// Bounds of every path strictly below this one: `[self + "/", self + "0")`.
    pub(crate) fn descendant_range(&self) -> (String, String) {
        (format!("{}/", self.0), format!("{}0", self.0))
    }
}

/// Ordering matches `str`, so manifests can be range-searched by path prefix.
impl std::borrow::Borrow<str> for RepoPath {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(Error::InvalidInput(
                "operation ID must contain 1..128 ASCII letters, digits, '-', '_' or '.'".into(),
            ));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileEntry {
    Regular {
        content: ObjectId,
        executable: bool,
    },
    /// Exact link-target bytes live in a blob; readers never follow the target.
    Symlink {
        target: ObjectId,
    },
}

/// How an owner resolves one conflicting path. Javelin never picks a side itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Choice {
    /// The Layer's own entry at the path; absent means the path is deleted.
    Ours,
    /// The source's entry at the path (the head for refresh); absent means deleted.
    Theirs,
    /// Exact bytes as a regular file. `executable: None` keeps the mode of the Layer's
    /// regular file at the path, else the source's, else false.
    Content {
        bytes: Vec<u8>,
        executable: Option<bool>,
    },
}

impl FileEntry {
    pub fn content(&self) -> ObjectId {
        match self {
            Self::Regular { content, .. } => *content,
            Self::Symlink { target } => *target,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest(BTreeMap<RepoPath, FileEntry>);

impl Manifest {
    /// Exact-entry renames from `self` to `later`: a path `later` removes whose entry `later`
    /// adds at exactly one other path, and which is the only path `self` removes with that
    /// entry. Content is never compared by similarity, so an edited rename is reported as a
    /// delete and an add. Ambiguous matches on either side are not reported.
    pub fn renames_to(&self, later: &Manifest) -> BTreeMap<RepoPath, RepoPath> {
        let mut removed = BTreeMap::<&FileEntry, Vec<&RepoPath>>::new();
        let mut added = BTreeMap::<&FileEntry, Vec<&RepoPath>>::new();
        for (path, entry) in &self.0 {
            if !later.0.contains_key(path) {
                removed.entry(entry).or_default().push(path);
            }
        }
        for (path, entry) in &later.0 {
            if !self.0.contains_key(path) {
                added.entry(entry).or_default().push(path);
            }
        }
        let mut renames = BTreeMap::new();
        for (entry, origins) in &removed {
            if let ([from], Some([to])) = (origins.as_slice(), added.get(entry).map(Vec::as_slice))
            {
                renames.insert((*from).clone(), (*to).clone());
            }
        }
        renames
    }

    pub fn new(entries: impl IntoIterator<Item = (RepoPath, FileEntry)>) -> Result<Self> {
        let mut map = BTreeMap::new();
        for (path, entry) in entries {
            if map.insert(path, entry).is_some() {
                return Err(Error::InvalidInput("duplicate manifest path".into()));
            }
        }
        for path in map.keys() {
            for (index, _) in path.as_str().match_indices('/') {
                if map.contains_key(&RepoPath(path.as_str()[..index].into())) {
                    return Err(Error::InvalidInput(
                        "file and descendant paths collide".into(),
                    ));
                }
            }
        }
        Ok(Self(map))
    }
    pub fn entries(&self) -> &BTreeMap<RepoPath, FileEntry> {
        &self.0
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let entries: BTreeMap<_, _> = self
            .0
            .iter()
            .map(|(path, entry)| (path.as_str(), entry))
            .collect();
        // Serializing a string-keyed map with string and bool fields cannot fail.
        serde_json::to_vec(&entries).expect("manifest encoding")
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let entries: BTreeMap<String, FileEntry> = serde_json::from_slice(bytes)
            .map_err(|e| Error::CorruptRepository(format!("invalid manifest encoding: {e}")))?;
        let entries = entries
            .into_iter()
            .map(|(path, entry)| Ok((RepoPath::parse(&path)?, entry)))
            .collect::<Result<Vec<_>>>()?;
        let manifest = Self::new(entries)?;
        if manifest.canonical_bytes() != bytes {
            return Err(Error::CorruptRepository(
                "noncanonical manifest encoding".into(),
            ));
        }
        Ok(manifest)
    }
}

/// A changeset: exact paths changed relative to a parent, each with its new entry
/// or None for deletion. It is a World version's primary record and a Layer
/// checkpoint's delta against its frozen base. Snapshots are derived from these.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SparseChanges(BTreeMap<RepoPath, Option<FileEntry>>);

impl SparseChanges {
    pub fn new(entries: impl IntoIterator<Item = (RepoPath, Option<FileEntry>)>) -> Result<Self> {
        let mut changes = BTreeMap::new();
        for (path, entry) in entries {
            if changes.insert(path, entry).is_some() {
                return Err(Error::InvalidInput("duplicate changed path".into()));
            }
        }
        Ok(Self(changes))
    }
    pub fn entries(&self) -> &BTreeMap<RepoPath, Option<FileEntry>> {
        &self.0
    }

    pub(crate) fn encoded(&self) -> BTreeMap<String, Option<FileEntry>> {
        self.0
            .iter()
            .map(|(p, e)| (p.as_str().into(), e.clone()))
            .collect()
    }

    /// The canonical encoding: what a checkpoint upload carries. `from_canonical_bytes`
    /// reads it back.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.encoded()).expect("changeset encoding")
    }

    /// Decodes `canonical_bytes` output. Any other encoding is refused, so one changeset
    /// has one byte form and one object ID.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let encoded: BTreeMap<String, Option<FileEntry>> = serde_json::from_slice(bytes)
            .map_err(|e| Error::InvalidInput(format!("invalid changes encoding: {e}")))?;
        let changes = Self::new(
            encoded
                .into_iter()
                .map(|(path, entry)| Ok((RepoPath::parse(&path)?, entry)))
                .collect::<Result<Vec<_>>>()?,
        )?;
        if changes.canonical_bytes() != bytes {
            return Err(Error::InvalidInput("noncanonical changes encoding".into()));
        }
        Ok(changes)
    }

    /// Applies this changeset to a snapshot, producing the child snapshot.
    pub(crate) fn apply(&self, entries: &mut BTreeMap<RepoPath, FileEntry>) {
        for (path, after) in &self.0 {
            match after {
                Some(entry) => entries.insert(path.clone(), entry.clone()),
                None => entries.remove(path),
            };
        }
    }
}

/// World version identity is a hash chain over derived roots and changesets,
/// computed in O(change): `id(V) = H(id(V-1) || root(V) || canonical changeset(V))`,
/// with 32 zero bytes before World 0. One comparison verifies a version's content.
pub(crate) fn version_id(
    parent: Option<ObjectId>,
    tree: ObjectId,
    changes: &SparseChanges,
) -> ObjectId {
    let mut hash = Sha256::new();
    hash.update(b"javelin-version-v2\0");
    hash.update(parent.map_or([0; 32], |p| p.0));
    hash.update(tree.0);
    hash.update(changes.canonical_bytes());
    ObjectId(hash.finalize().into())
}

/// Point reads over one immutable World snapshot. The storage engine supplies it
/// (derived trees); normalization below stays engine-neutral.
pub(crate) trait WorldIndex {
    /// The file entry at `path` in the snapshot of `version`.
    fn entry_at(&self, path: &RepoPath, version: VersionNumber) -> Result<Option<FileEntry>>;
    /// Whether `path` is a directory, that is, has live descendants, at `version`.
    fn is_dir(&self, path: &RepoPath, version: VersionNumber) -> Result<bool>;
    /// Every file strictly below `dir` at `version`.
    fn files_under(&self, dir: &RepoPath, version: VersionNumber) -> Result<Vec<RepoPath>>;
    /// Validates a new entry's referenced content.
    fn check_entry(&self, entry: &FileEntry) -> Result<()>;
}

/// Validates a delta against the snapshot of `base` and drops no-op entries. The
/// result is a canonical changeset whose application keeps file paths and their
/// descendants disjoint. Work is O(changed paths x depth) tree lookups.
pub(crate) fn normalize(
    index: &impl WorldIndex,
    base: VersionNumber,
    changes: &SparseChanges,
) -> Result<SparseChanges> {
    let mut delta = BTreeMap::new();
    for (path, after) in &changes.0 {
        if let Some(entry) = after {
            index.check_entry(entry)?;
        }
        let before = index.entry_at(path, base)?;
        if after.is_none() && before.is_none() {
            let (start, end) = path.descendant_range();
            let added_child = changes
                .0
                .range::<str, _>((
                    std::ops::Bound::Included(start.as_str()),
                    std::ops::Bound::Excluded(end.as_str()),
                ))
                .any(|(_, e)| e.is_some());
            if added_child || index.is_dir(path, base)? {
                return Err(Error::InvalidInput(
                    "directory-prefix tombstone is ambiguous; delete exact entries".into(),
                ));
            }
        }
        if before == *after {
            continue;
        }
        delta.insert(path.clone(), after.clone());
    }
    let collide = || Error::InvalidInput("file and descendant paths collide".into());
    let live_after = |path: &RepoPath| -> Result<bool> {
        match delta.get(path) {
            Some(after) => Ok(after.is_some()),
            None => Ok(index.entry_at(path, base)?.is_some()),
        }
    };
    for (path, after) in &delta {
        if after.is_none() {
            continue;
        }
        for ancestor in path.ancestors() {
            if live_after(&ancestor)? {
                return Err(collide());
            }
        }
        let (start, end) = path.descendant_range();
        if delta
            .range::<str, _>((
                std::ops::Bound::Included(start.as_str()),
                std::ops::Bound::Excluded(end.as_str()),
            ))
            .any(|(_, e)| e.is_some())
        {
            return Err(collide());
        }
        // A file replacing a directory must delete every file below it.
        if index.is_dir(path, base)? {
            for child in index.files_under(path, base)? {
                if !delta.contains_key(&child) {
                    return Err(collide());
                }
            }
        }
    }
    Ok(SparseChanges(delta))
}

/// One live, unpublished Layer's changed path and its entry there (None: deleted).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LivePath {
    pub(crate) layer: crate::LayerId,
    pub(crate) path: RepoPath,
    pub(crate) entry: Option<FileEntry>,
}

/// Every live Layer's changed paths, indexed by path. The storage engine supplies
/// it, locally from the Layer index and on a server from streamed checkpoints.
pub(crate) trait LiveIndex {
    /// Live changes at exactly `path`, or strictly below it when `below` is set.
    fn live(&self, path: &RepoPath, below: bool) -> Result<Vec<LivePath>>;
}

/// Path pairs (ours, theirs) on which `changes`, over World `base`, would conflict
/// if each other live Layer published first, grouped by that Layer. It applies
/// compose's rule: a pair is the same path or an ancestor/descendant, and their
/// entry differs from ours at `base`. Work is O(changed paths x depth) indexed
/// lookups plus matches, independent of the number of live Layers.
pub(crate) fn collisions(
    world: &impl WorldIndex,
    live: &impl LiveIndex,
    layer: &crate::LayerId,
    changes: &SparseChanges,
    base: VersionNumber,
) -> Result<BTreeMap<crate::LayerId, BTreeSet<(RepoPath, RepoPath)>>> {
    let mut found = BTreeMap::<_, BTreeSet<_>>::new();
    let mut at_base = BTreeMap::<RepoPath, Option<FileEntry>>::new();
    for path in changes.0.keys() {
        let mut hits = live.live(path, false)?;
        hits.extend(live.live(path, true)?);
        for ancestor in path.ancestors() {
            hits.extend(live.live(&ancestor, false)?);
        }
        for hit in hits {
            if &hit.layer == layer {
                continue;
            }
            let before = match at_base.get(&hit.path) {
                Some(entry) => entry.clone(),
                None => {
                    let entry = world.entry_at(&hit.path, base)?;
                    at_base.insert(hit.path.clone(), entry.clone());
                    entry
                }
            };
            if before != hit.entry {
                found
                    .entry(hit.layer)
                    .or_default()
                    .insert((path.clone(), hit.path));
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(bytes: &[u8]) -> FileEntry {
        FileEntry::Regular {
            content: ObjectId::for_blob(bytes),
            executable: false,
        }
    }

    fn manifest(entries: &[(&str, &[u8])]) -> Manifest {
        Manifest::new(
            entries
                .iter()
                .map(|(p, bytes)| (RepoPath::parse(p).unwrap(), entry(bytes))),
        )
        .unwrap()
    }

    #[test]
    fn renames_need_one_origin_and_one_destination_for_the_entry() {
        let base = manifest(&[("a", b"x"), ("b", b"x"), ("c", b"y")]);
        let later = manifest(&[("d", b"x"), ("c", b"y")]);
        // Two deleted paths hold the same entry, so neither is a rename of `d`.
        assert!(base.renames_to(&later).is_empty());

        let base = manifest(&[("a", b"x"), ("c", b"y")]);
        let renamed = base.renames_to(&manifest(&[("d", b"x"), ("c", b"y")]));
        assert_eq!(
            renamed.get(&RepoPath::parse("a").unwrap()),
            Some(&RepoPath::parse("d").unwrap())
        );
    }
}
