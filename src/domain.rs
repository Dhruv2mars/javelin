use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
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
}

impl ObjectKind {
    pub(crate) fn decode(kind: i64) -> Result<Self> {
        match kind {
            1 => Ok(Self::Blob),
            2 => Ok(Self::Manifest),
            3 => Ok(Self::Checkpoint),
            _ => Err(Error::CorruptRepository("unknown object kind".into())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.contains(['\\', ':', '\0'])
            || value.split('/').any(|p| {
                p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".javelin")
            })
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.encoded()).expect("changeset encoding")
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

/// World version identity is a hash chain over changesets, computed in O(change):
/// `id(V) = H(id(V-1) || canonical changeset(V))`, with 32 zero bytes before World 0.
pub(crate) fn version_id(parent: Option<ObjectId>, changes: &SparseChanges) -> ObjectId {
    let mut hash = Sha256::new();
    hash.update(b"javelin-version-v1\0");
    hash.update(parent.map_or([0; 32], |p| p.0));
    hash.update(changes.canonical_bytes());
    ObjectId(hash.finalize().into())
}

/// Indexed point reads over immutable World history. The storage engine supplies
/// it; composition and normalization below stay engine-neutral.
pub(crate) trait WorldIndex {
    /// The entry at `path` in the snapshot of `version`.
    fn entry_at(&self, path: &RepoPath, version: VersionNumber) -> Result<Option<FileEntry>>;
    /// Distinct paths strictly below `dir` with any change in versions `(after, upto]`
    /// (from World 0 when `after` is None). A superset of paths live at `upto`.
    fn changed_under(
        &self,
        dir: &RepoPath,
        after: Option<VersionNumber>,
        upto: VersionNumber,
    ) -> Result<Vec<RepoPath>>;
    /// Validates a new entry's referenced content.
    fn check_entry(&self, entry: &FileEntry) -> Result<()>;
}

/// Sorted endpoints of every exact or ancestor/descendant intersection between the
/// changeset's paths and paths whose entry differs end-to-end between `base` and
/// `head`. Work is O(changed paths x depth) indexed lookups plus descendant ranges.
pub(crate) fn intersections(
    index: &impl WorldIndex,
    changes: &SparseChanges,
    base: VersionNumber,
    head: VersionNumber,
) -> Result<BTreeSet<RepoPath>> {
    let mut found = BTreeSet::new();
    if base == head {
        return Ok(found);
    }
    let mut memo = BTreeMap::<RepoPath, bool>::new();
    let mut changed = |path: RepoPath| -> Result<Option<RepoPath>> {
        if let Some(hit) = memo.get(&path) {
            return Ok(hit.then_some(path));
        }
        let hit = index.entry_at(&path, base)? != index.entry_at(&path, head)?;
        memo.insert(path.clone(), hit);
        Ok(hit.then_some(path))
    };
    for path in changes.0.keys() {
        let candidates = std::iter::once(path.clone())
            .chain(path.ancestors())
            .chain(index.changed_under(path, Some(base), head)?);
        for candidate in candidates {
            if let Some(hit) = changed(candidate)? {
                found.insert(path.clone());
                found.insert(hit);
            }
        }
    }
    Ok(found)
}

/// Validates a delta against the snapshot of `base` and drops no-op entries. The
/// result is a canonical changeset whose application keeps file paths and their
/// descendants disjoint. Work is O(changed paths x depth) plus descendant ranges.
pub(crate) fn normalize(
    index: &impl WorldIndex,
    base: VersionNumber,
    changes: &SparseChanges,
) -> Result<SparseChanges> {
    let live_under = |dir: &RepoPath| -> Result<bool> {
        for path in index.changed_under(dir, None, base)? {
            if index.entry_at(&path, base)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    };
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
            if added_child || live_under(path)? {
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
        // A file in the base snapshot has no descendants there.
        if index.entry_at(path, base)?.is_none() {
            for child in index.changed_under(path, None, base)? {
                if !delta.contains_key(&child) && index.entry_at(&child, base)?.is_some() {
                    return Err(collide());
                }
            }
        }
    }
    Ok(SparseChanges(delta))
}

/// A complete snapshot used as a WorldIndex, for derived checks over one version.
pub(crate) struct SnapshotIndex<'a, F> {
    pub(crate) entries: &'a BTreeMap<RepoPath, FileEntry>,
    pub(crate) check: F,
}

impl<F: Fn(&FileEntry) -> Result<()>> WorldIndex for SnapshotIndex<'_, F> {
    fn entry_at(&self, path: &RepoPath, _: VersionNumber) -> Result<Option<FileEntry>> {
        Ok(self.entries.get(path).cloned())
    }
    fn changed_under(
        &self,
        dir: &RepoPath,
        _: Option<VersionNumber>,
        _: VersionNumber,
    ) -> Result<Vec<RepoPath>> {
        let (start, end) = dir.descendant_range();
        Ok(self
            .entries
            .range::<str, _>((
                std::ops::Bound::Included(start.as_str()),
                std::ops::Bound::Excluded(end.as_str()),
            ))
            .map(|(p, _)| p.clone())
            .collect())
    }
    fn check_entry(&self, entry: &FileEntry) -> Result<()> {
        (self.check)(entry)
    }
}
