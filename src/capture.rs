//! Daemonless change capture for plain-directory Layers.
//!
//! A per-Layer stat cache maps each path to the stat fingerprint it had when its
//! content was last hashed. A scan walks the directory and hashes only entries whose
//! fingerprint differs or whose cache entry is untrusted.
//!
//! Capture boundary:
//! - **Stable read.** An entry is hashed between two `lstat` calls (plus an `fstat` of
//!   the opened file on Unix). If the fingerprints differ, the read is retried; after
//!   [`ATTEMPTS`] tries the path is reported uncertain, keeps its prior checkpointed
//!   state, and is left out of the cache so the next scan hashes it again.
//! - **Timestamp semantics.** The reference time is the system clock read once before
//!   the walk starts, minus [`RACY_MARGIN_NS`] to cover coarse kernel clocks and
//!   2-second FAT timestamps. It is per scan, not the cache file's write time.
//! - **Racy entries** (Git's rule, <https://git-scm.com/docs/racy-git>). An entry whose
//!   mtime or ctime is not older than the reference time could change again within the
//!   same timestamp tick without changing its fingerprint. It is stored with
//!   `trusted = false` and is re-hashed on every later scan until a scan sees it with
//!   old timestamps. Writing a newer cache never makes an entry trusted; only a
//!   non-racy re-hash does. This replaces Git's index-size smudging.
//! - **Crash safety.** The cache is written to a temporary file, synced and renamed
//!   over the old one. A checksum guards the contents; a missing, torn or corrupt
//!   cache is discarded and every entry is hashed again.
//!
//! The cache is only an accelerator: every claim in it is "this fingerprint had this
//! content", which stays true whenever it is written, so concurrent writers are safe.
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, Metadata},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{
    Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest, ObjectId, OperationId,
    RepoPath, Repository, Result, SparseChanges, domain::ObjectKind,
};

/// Reads of one entry before it is reported uncertain.
pub const ATTEMPTS: usize = 3;
/// Entries modified this close to (or after) the scan start are racy.
pub const RACY_MARGIN_NS: i128 = 2_000_000_000;
const MAGIC: &[u8; 8] = b"JVSTAT\x001";

/// The fields compared to decide whether an entry may have changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stat {
    size: u64,
    mtime: i128,
    /// Inode change time on Unix; creation time on Windows.
    ctime: i128,
    /// Zero on Windows, where std exposes no stable file index.
    ino: u64,
    /// Full `st_mode` on Unix; file attributes on Windows.
    mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Cached {
    stat: Stat,
    entry: FileEntry,
    trusted: bool,
}

/// A per-Layer stat cache. Load it, pass it to [`scan`], and save it once the blobs
/// it references are durable.
#[derive(Debug, Default)]
pub struct StatCache {
    entries: HashMap<String, Cached>,
    rebuilt: bool,
    dirty: bool,
}

impl StatCache {
    /// A missing, unreadable or corrupt cache loads as empty and marks a rebuild.
    pub fn load(path: &Path) -> Self {
        let entries = fs::read(path).ok().and_then(|bytes| decode(&bytes));
        Self {
            rebuilt: entries.is_none(),
            dirty: entries.is_none(),
            entries: entries.unwrap_or_default(),
        }
    }

    /// True when the loaded cache was missing or corrupt.
    pub fn rebuilt(&self) -> bool {
        self.rebuilt
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Atomic replace: write a temporary sibling, sync it, rename it into place and
    /// sync the directory. Skips the write when the scan changed nothing.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidInput("cache path needs a parent".into()))?;
        fs::create_dir_all(parent)?;
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        static SAVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        name.push(format!(".{}-{n}.tmp", std::process::id()));
        let temporary = parent.join(name);
        let result: std::io::Result<_> = (|| {
            let mut file = fs::File::create(&temporary)?;
            std::io::Write::write_all(&mut file, &encode(&self.entries))?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        self.dirty = false;
        Ok(())
    }
}

/// The Layer directory's complete current entries.
#[derive(Debug)]
pub struct Scan {
    pub entries: BTreeMap<RepoPath, FileEntry>,
    /// Entries whose content was read and hashed in this scan.
    pub hashed: usize,
    /// Entries that kept changing during [`ATTEMPTS`] reads. Each keeps its prior state.
    pub uncertain: Vec<RepoPath>,
}

/// Walks `root` and returns its entries, hashing only what the cache cannot vouch for.
///
/// `prior` is the Layer's current checkpointed manifest. It supplies the state of
/// uncertain paths, the executable bit where the OS has none, and the tracked set
/// that ignore rules never hide. Blobs go to `store`. Empty directories are not
/// represented, matching the native model. `.javelin` entries are never captured.
pub fn scan(
    root: &Path,
    cache: &mut StatCache,
    prior: &Manifest,
    store: &mut dyn FnMut(&[u8]) -> Result<ObjectId>,
) -> Result<Scan> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(Error::InvalidInput("Layer root must be a directory".into()));
    }
    let racy = now() - RACY_MARGIN_NS;
    let mut old = std::mem::take(&mut cache.entries);
    let mut fresh = HashMap::with_capacity(old.len());
    let mut scan = Scan {
        entries: BTreeMap::new(),
        hashed: 0,
        uncertain: Vec::new(),
    };
    let mut pending = vec![String::new()];
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(root.join(&directory))? {
            let item = item?;
            let name = item.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| Error::Unsupported(format!("non-UTF-8 name in {directory:?}")))?;
            if name.eq_ignore_ascii_case(".javelin") {
                continue;
            }
            let relative = if directory.is_empty() {
                name.to_owned()
            } else {
                format!("{directory}/{name}")
            };
            let path = RepoPath::parse(&relative).map_err(|_| {
                Error::Unsupported(format!("name outside the native path model: {relative:?}"))
            })?;
            let meta = match item.metadata() {
                Ok(meta) => meta,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let kind = meta.file_type();
            if kind.is_dir() {
                if !untracked_ignored(prior, &path, true) {
                    pending.push(relative);
                }
                continue;
            }
            if !kind.is_file() && !kind.is_symlink() {
                return Err(Error::Unsupported(format!("special file at {relative:?}")));
            }
            if untracked_ignored(prior, &path, false) {
                continue;
            }
            let stat = stat(&meta);
            let reused = old
                .remove(&relative)
                .filter(|c| c.trusted && c.stat == stat);
            let cached = match reused {
                Some(cached) => cached,
                None => {
                    cache.dirty = true;
                    let executable = matches!(
                        prior.entries().get(&path),
                        Some(FileEntry::Regular {
                            executable: true,
                            ..
                        })
                    );
                    match read_stable(&root.join(&relative), executable, store)? {
                        // The walk's fingerprint is what the next walk compares against;
                        // the stable read's own fingerprint must also be old to trust it.
                        Hashed::Stable(read, entry) => {
                            scan.hashed += 1;
                            let old = |s: Stat| s.mtime < racy && s.ctime < racy;
                            Cached {
                                stat,
                                entry,
                                trusted: old(stat) && old(read),
                            }
                        }
                        Hashed::Gone => continue,
                        Hashed::Unstable => {
                            if let Some(entry) = prior.entries().get(&path) {
                                scan.entries.insert(path.clone(), entry.clone());
                            }
                            scan.uncertain.push(path);
                            continue;
                        }
                    }
                }
            };
            scan.entries.insert(path, cached.entry.clone());
            fresh.insert(relative, cached);
        }
    }
    cache.dirty |= !old.is_empty();
    cache.entries = fresh;
    scan.uncertain.sort();
    Ok(scan)
}

/// Seam for root ignore rules (PR #36). Tracked paths are never ignored.
fn untracked_ignored(prior: &Manifest, path: &RepoPath, directory: bool) -> bool {
    let ignored = |_: &RepoPath| false; // Replace with the shared path -> ignored? rule.
    if !ignored(path) {
        return false;
    }
    let tracked = if directory {
        let prefix = format!("{}/", path.as_str());
        prior
            .entries()
            .keys()
            .any(|p| p.as_str().starts_with(&prefix))
    } else {
        prior.entries().contains_key(path)
    };
    !tracked
}

enum Hashed {
    Stable(Stat, FileEntry),
    Gone,
    Unstable,
}

fn read_stable(
    full: &Path,
    executable: bool,
    store: &mut dyn FnMut(&[u8]) -> Result<ObjectId>,
) -> Result<Hashed> {
    let lstat = || match fs::symlink_metadata(full) {
        Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => Ok(Some(stat(&meta))),
        Ok(_) => Ok(None),
        Err(e) => Err(e),
    };
    for _ in 0..ATTEMPTS {
        let result: std::io::Result<_> = (|| {
            let Some(before) = lstat()? else {
                return Ok(None);
            };
            let content = read_entry(full, before)?;
            Ok(content
                .filter(|_| lstat().ok().flatten() == Some(before))
                .map(|c| (before, c)))
        })();
        let (after, (symlink, bytes)) = match result {
            Ok(Some(read)) => read,
            Ok(None) => continue,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Hashed::Gone),
            Err(e) if raced(&e) => continue,
            Err(e) => return Err(e.into()),
        };
        let content = store(&bytes)?;
        let entry = if symlink {
            FileEntry::Symlink { target: content }
        } else {
            FileEntry::Regular {
                content,
                executable: executable_bit(after, executable),
            }
        };
        return Ok(Hashed::Stable(after, entry));
    }
    Ok(Hashed::Unstable)
}

/// Reads a symlink target or file bytes. `None` means the entry changed underneath.
fn read_entry(full: &Path, before: Stat) -> std::io::Result<Option<(bool, Vec<u8>)>> {
    let meta = fs::symlink_metadata(full)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(full)?;
        #[cfg(unix)]
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(target.as_os_str()).to_vec();
        #[cfg(not(unix))]
        let bytes = target
            .to_str()
            .ok_or_else(|| std::io::Error::other("non-UTF-8 symlink target"))?
            .as_bytes()
            .to_vec();
        return Ok(Some((true, bytes)));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let mut file = options.open(full)?;
    let opened = file.metadata()?;
    if !opened.is_file() || stat(&opened) != before {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.read_to_end(&mut bytes)?;
    Ok(Some((false, bytes)))
}

/// A path swapped for a symlink between `lstat` and an `O_NOFOLLOW` open.
fn raced(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    return error.raw_os_error() == Some(libc::ELOOP);
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

#[cfg(unix)]
fn stat(meta: &Metadata) -> Stat {
    use std::os::unix::fs::MetadataExt;
    Stat {
        size: meta.len(),
        mtime: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
        ctime: meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128,
        ino: meta.ino(),
        mode: meta.mode(),
    }
}

#[cfg(windows)]
fn stat(meta: &Metadata) -> Stat {
    use std::os::windows::fs::MetadataExt;
    // FILETIME counts 100 ns intervals from 1601-01-01.
    let ns = |t: u64| (t as i128 - 116_444_736_000_000_000) * 100;
    Stat {
        size: meta.file_size(),
        mtime: ns(meta.last_write_time()),
        ctime: ns(meta.creation_time()),
        ino: 0,
        mode: meta.file_attributes(),
    }
}

#[cfg(unix)]
fn executable_bit(stat: Stat, _prior: bool) -> bool {
    stat.mode & 0o100 != 0
}

/// Windows has no executable bit; keep the checkpointed one.
#[cfg(not(unix))]
fn executable_bit(_stat: Stat, prior: bool) -> bool {
    prior
}

fn now() -> i128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

fn encode(entries: &HashMap<String, Cached>) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + entries.len() * 128);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (path, c) in entries {
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&c.stat.size.to_le_bytes());
        out.extend_from_slice(&c.stat.mtime.to_le_bytes());
        out.extend_from_slice(&c.stat.ctime.to_le_bytes());
        out.extend_from_slice(&c.stat.ino.to_le_bytes());
        out.extend_from_slice(&c.stat.mode.to_le_bytes());
        let kind = match c.entry {
            FileEntry::Regular {
                executable: false, ..
            } => 0u8,
            FileEntry::Regular {
                executable: true, ..
            } => 1,
            FileEntry::Symlink { .. } => 2,
        };
        out.extend_from_slice(&[kind, c.trusted as u8]);
        out.extend_from_slice(c.entry.content().as_bytes());
    }
    let sum = Sha256::digest(&out);
    out.extend_from_slice(&sum);
    out
}

fn decode(bytes: &[u8]) -> Option<HashMap<String, Cached>> {
    let (body, sum) = bytes.split_at_checked(bytes.len().checked_sub(32)?)?;
    if Sha256::digest(body).as_slice() != sum {
        return None;
    }
    let mut input = body.strip_prefix(MAGIC.as_slice())?;
    let mut take = |n: usize| {
        let (head, tail) = input.split_at_checked(n)?;
        input = tail;
        Some(head)
    };
    let count = u64::from_le_bytes(take(8)?.try_into().ok()?);
    let mut entries = HashMap::with_capacity(count.min(1 << 20) as usize);
    for _ in 0..count {
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let path = std::str::from_utf8(take(len)?).ok()?.to_owned();
        RepoPath::parse(&path).ok()?;
        let stat = Stat {
            size: u64::from_le_bytes(take(8)?.try_into().ok()?),
            mtime: i128::from_le_bytes(take(16)?.try_into().ok()?),
            ctime: i128::from_le_bytes(take(16)?.try_into().ok()?),
            ino: u64::from_le_bytes(take(8)?.try_into().ok()?),
            mode: u32::from_le_bytes(take(4)?.try_into().ok()?),
        };
        let flags = take(2)?;
        let id = ObjectId::from_bytes(take(32)?.try_into().ok()?);
        let entry = match flags[0] {
            0 | 1 => FileEntry::Regular {
                content: id,
                executable: flags[0] == 1,
            },
            2 => FileEntry::Symlink { target: id },
            _ => return None,
        };
        let trusted = match flags[1] {
            0 | 1 => flags[1] == 1,
            _ => return None,
        };
        entries.insert(
            path,
            Cached {
                stat,
                entry,
                trusted,
            },
        );
    }
    (input.is_empty() && entries.len() as u64 == count).then_some(entries)
}

/// The complete delta from `base` to `after`, with per-entry deletions.
pub fn changes_from(
    base: &Manifest,
    after: &BTreeMap<RepoPath, FileEntry>,
) -> Result<SparseChanges> {
    let changed = after
        .iter()
        .filter(|(path, entry)| base.entries().get(*path) != Some(*entry))
        .map(|(path, entry)| (path.clone(), Some(entry.clone())));
    let deleted = base
        .entries()
        .keys()
        .filter(|path| !after.contains_key(*path))
        .map(|path| (path.clone(), None));
    SparseChanges::new(changed.chain(deleted))
}

/// The result of capturing a Layer directory.
#[derive(Debug)]
pub struct Capture {
    /// `None` when the directory already matches the Layer head.
    pub update: Option<LayerUpdate>,
    /// The generated operation ID, reused verbatim by an identical retry.
    pub operation: Option<OperationId>,
    pub head: LayerHead,
    pub changes: SparseChanges,
    pub hashed: usize,
    pub uncertain: Vec<RepoPath>,
    pub rebuilt: bool,
}

impl Repository {
    /// Captures the Layer's directory into a checkpoint when it differs from the head.
    ///
    /// The operation ID is derived from the Layer, the expected head and the exact
    /// changes, so a retry after a lost reply or crash replays the original outcome,
    /// and concurrent identical captures agree on one revision. Blobs are committed
    /// before the cache is saved, so the cache never references missing content.
    pub fn capture_directory(&mut self, layer: &LayerId, root: &Path) -> Result<Capture> {
        let head = match self.layer_state(layer)? {
            LayerState::Active { head } => head,
            _ => return Err(Error::InvalidInput("Layer is not active".into())),
        };
        let checkpoint = self.read_checkpoint(head.checkpoint())?;
        let base = self.read_manifest(checkpoint.base().manifest())?;
        let cache_path = self.capture_cache(layer)?;
        let mut cache = StatCache::load(&cache_path);
        let rebuilt = cache.rebuilt();
        let tx = self.conn.unchecked_transaction()?;
        let scan = scan(root, &mut cache, checkpoint.manifest(), &mut |bytes| {
            store_blob(&tx, bytes)
        })?;
        tx.commit()?;
        // The cache only accelerates later scans; failing to save it loses no edits.
        let _ = cache.save(&cache_path);
        let changes = changes_from(&base, &scan.entries)?;
        let (update, operation) = if &changes == checkpoint.changes() {
            (None, None)
        } else {
            let operation = operation_id(&head, &changes);
            let update = self.capture_layer(&operation, &head, &changes)?;
            (Some(update), Some(operation))
        };
        Ok(Capture {
            update,
            operation,
            head,
            changes,
            hashed: scan.hashed,
            uncertain: scan.uncertain,
            rebuilt,
        })
    }

    /// `.javelin/capture/<sha256(layer)>`: fixed-width and case-insensitive safe.
    fn capture_cache(&self, layer: &LayerId) -> Result<PathBuf> {
        let database = self
            .conn
            .path()
            .ok_or_else(|| Error::CorruptRepository("repository has no path".into()))?;
        let store = Path::new(database)
            .parent()
            .ok_or_else(|| Error::CorruptRepository("repository has no directory".into()))?;
        let name = ObjectId::from_bytes(Sha256::digest(layer.as_str().as_bytes()).into());
        Ok(store.join("capture").join(name.to_string()))
    }
}

/// Stores new content; existing blobs are not re-read (`check` verifies them).
fn store_blob(conn: &Connection, bytes: &[u8]) -> Result<ObjectId> {
    let id = ObjectId::for_blob(bytes);
    let exists = conn
        .query_row(
            "SELECT 1 FROM objects WHERE id=?1",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        return Ok(id);
    }
    crate::repository::insert_object(conn, ObjectKind::Blob, bytes)
}

fn operation_id(head: &LayerHead, changes: &SparseChanges) -> OperationId {
    let mut hash = Sha256::new();
    hash.update(b"javelin-capture-v1\0");
    hash.update(head.layer().as_str());
    hash.update(format!(
        "\0{}\0{}\0",
        head.revision(),
        head.checkpoint().object()
    ));
    for (path, entry) in changes.entries() {
        hash.update(path.as_str());
        hash.update(b"\0");
        hash.update(serde_json::to_vec(entry).expect("entry encoding"));
        hash.update(b"\0");
    }
    let id = ObjectId::from_bytes(hash.finalize().into());
    OperationId::parse(&format!("capture-{id}")).expect("generated operation ID")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, cache: &mut StatCache) -> Scan {
        let store = &mut |bytes: &[u8]| Ok(ObjectId::for_blob(bytes));
        scan(root, cache, &Manifest::new([]).unwrap(), store).unwrap()
    }

    /// Simulates an edit the fingerprint cannot see: same size, mtime and ctime
    /// within one timestamp tick. The cache now claims stale content.
    fn forge(cache: &mut StatCache, path: &str) {
        let bogus = ObjectId::for_blob(b"stale content");
        cache.entries.get_mut(path).unwrap().entry = FileEntry::Regular {
            content: bogus,
            executable: false,
        };
    }

    fn captured(scan: &Scan, path: &str) -> ObjectId {
        scan.entries[&RepoPath::parse(path).unwrap()].content()
    }

    #[test]
    fn racily_clean_entries_stay_untrusted_after_the_cache_advances() {
        let dir = tempfile::TempDir::new().unwrap();
        let (root, file) = (dir.path().join("layer"), dir.path().join("cache"));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("f"), b"real").unwrap();
        let real = ObjectId::for_blob(b"real");

        let mut cache = StatCache::load(&file);
        run(&root, &mut cache);
        assert!(!cache.entries["f"].trusted, "fresh entries are racy");
        forge(&mut cache, "f");
        // Rewriting the cache later must not launder the racy entry into trust.
        cache.dirty = true;
        cache.save(&file).unwrap();
        let mut cache = StatCache::load(&file);
        assert!(!cache.entries["f"].trusted);
        assert_eq!(captured(&run(&root, &mut cache), "f"), real);

        std::thread::sleep(std::time::Duration::from_nanos(
            RACY_MARGIN_NS as u64 + 200_000_000,
        ));
        let rehashed = run(&root, &mut cache);
        assert_eq!(rehashed.hashed, 1);
        assert!(cache.entries["f"].trusted, "an old entry becomes trusted");
        // Control: a trusted entry with an unchanged fingerprint is reused as is.
        forge(&mut cache, "f");
        let reused = run(&root, &mut cache);
        assert_eq!(reused.hashed, 0);
        assert_ne!(captured(&reused, "f"), real);
    }

    #[test]
    fn cache_round_trips_and_rejects_any_damage() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("a"), b"a").unwrap();
        let mut cache = StatCache::load(&dir.path().join("none"));
        run(dir.path(), &mut cache);
        let bytes = encode(&cache.entries);
        assert_eq!(decode(&bytes).unwrap(), cache.entries);
        for i in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[i] ^= 0x40;
            assert!(decode(&damaged).is_none(), "byte {i}");
        }
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(decode(&[]).is_none());
    }
}
