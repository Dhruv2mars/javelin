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
    Error, FileEntry, IgnoreRules, LayerHead, LayerId, LayerUpdate, Manifest, ObjectId,
    OperationId, RepoPath, Repository, Result, SparseChanges,
    domain::ObjectKind,
    ignore::{CONFIG, LIMIT},
};

/// Reads of one entry before it is reported uncertain.
pub const ATTEMPTS: usize = 3;
/// Entries modified this close to (or after) the scan start are racy.
pub const RACY_MARGIN_NS: i128 = 2_000_000_000;
/// Version 2: Windows `ctime` became ChangeTime (v1 stored creation time), so v1 caches are rebuilt.
const MAGIC: &[u8; 8] = b"JVSTAT\x002";

/// The fields compared to decide whether an entry may have changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stat {
    size: u64,
    mtime: i128,
    /// Inode change time on Unix; ChangeTime on Windows, which NTFS and ReFS advance on every write.
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
        write_atomic(path, &encode(&self.entries))?;
        self.dirty = false;
        Ok(())
    }
}

/// Atomic replace: write a temporary sibling, sync it, rename it into place and sync the
/// directory. A reader sees the old bytes or the new bytes, never a mix.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
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
        std::io::Write::write_all(&mut file, bytes)?;
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
    Ok(result?)
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
/// that root ignore rules (`.javelinignore`) never hide. Blobs go to `store`. Empty directories are not
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
    let rules = ignore_rules(root)?;
    let racy = now() - RACY_MARGIN_NS;
    let mut old = std::mem::take(&mut cache.entries);
    let mut fresh = HashMap::with_capacity(old.len());
    let mut scan = Scan {
        entries: BTreeMap::new(),
        hashed: 0,
        uncertain: Vec::new(),
    };
    for Found {
        relative,
        path,
        stat,
    } in walk(root, &rules, prior)?
    {
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
                    // Both fingerprint the entry the same way, so a mismatch means the
                    // entry changed after the walk.
                    Hashed::Stable(read, entry) => {
                        scan.hashed += 1;
                        let old = |s: Stat| s.mtime < racy && s.ctime < racy;
                        Cached {
                            stat,
                            entry,
                            trusted: old(stat) && old(read) && stat == read,
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
    cache.dirty |= !old.is_empty();
    cache.entries = fresh;
    scan.uncertain.sort();
    Ok(scan)
}

struct Found {
    relative: String,
    path: RepoPath,
    stat: Stat,
}

/// Lists every capturable file and symlink with its `lstat` fingerprint.
///
/// Directories are listed by a small thread pool: the walk is bound by per-entry
/// syscall latency, not CPU, so parallel listing cuts a 100k-file rescan severalfold.
fn walk(root: &Path, rules: &IgnoreRules, prior: &Manifest) -> Result<Vec<Found>> {
    use std::sync::{Condvar, Mutex};
    struct Queue {
        pending: Vec<String>,
        busy: usize,
        error: Option<Error>,
    }
    let queue = Mutex::new(Queue {
        pending: vec![String::new()],
        busy: 0,
        error: None,
    });
    let ready = Condvar::new();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    let found = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let directory = {
                            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                if q.error.is_some() {
                                    return found;
                                }
                                if let Some(directory) = q.pending.pop() {
                                    q.busy += 1;
                                    break directory;
                                }
                                if q.busy == 0 {
                                    return found;
                                }
                                q = ready.wait(q).unwrap_or_else(|e| e.into_inner());
                            }
                        };
                        let mut subdirectories = Vec::new();
                        let result = list(
                            root,
                            &directory,
                            rules,
                            prior,
                            &mut found,
                            &mut subdirectories,
                        );
                        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                        q.busy -= 1;
                        match result {
                            Ok(()) => q.pending.append(&mut subdirectories),
                            Err(e) => {
                                q.error.get_or_insert(e);
                            }
                        }
                        ready.notify_all();
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("walk worker panicked"))
            .collect()
    });
    match queue.into_inner().unwrap_or_else(|e| e.into_inner()).error {
        Some(error) => Err(error),
        None => Ok(found),
    }
}

/// Lists one directory: files and symlinks into `found`, directories to visit next.
fn list(
    root: &Path,
    directory: &str,
    rules: &IgnoreRules,
    prior: &Manifest,
    found: &mut Vec<Found>,
    subdirectories: &mut Vec<String>,
) -> Result<()> {
    let items = match fs::read_dir(root.join(directory)) {
        Ok(items) => items,
        // A directory removed after its parent was listed holds nothing to capture.
        Err(e) if e.kind() == ErrorKind::NotFound && !directory.is_empty() => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for item in items {
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
            if !untracked_ignored(rules, prior, &path, true) {
                subdirectories.push(relative);
            }
            continue;
        }
        if !kind.is_file() && !kind.is_symlink() {
            return Err(Error::Unsupported(format!("special file at {relative:?}")));
        }
        if !untracked_ignored(rules, prior, &path, false) {
            let stat = match fingerprint(&root.join(&relative), &meta) {
                Ok(stat) => stat,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            found.push(Found {
                relative,
                path,
                stat,
            });
        }
    }
    Ok(())
}

/// Ignore rules hide only untracked paths; tracked paths and their ancestors stay visible.
fn untracked_ignored(
    rules: &IgnoreRules,
    prior: &Manifest,
    path: &RepoPath,
    directory: bool,
) -> bool {
    if !rules.ignored(path.as_str()) {
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

/// Reads `.javelinignore` at the Layer root: a bounded UTF-8 regular file, never a link.
fn ignore_rules(root: &Path) -> Result<IgnoreRules> {
    let invalid = |why: &str| Error::InvalidInput(format!("{CONFIG} {why}"));
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let path = root.join(CONFIG);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(invalid("must be a regular file")),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(IgnoreRules::default()),
        Err(e) => return Err(e.into()),
    }
    let file = options
        .open(&path)
        .map_err(|_| invalid("must be a regular file"))?;
    if !file.metadata()?.is_file() {
        return Err(invalid("must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(invalid("exceeds 64 KiB"));
    }
    IgnoreRules::parse(&String::from_utf8(bytes).map_err(|_| invalid("must be UTF-8"))?)
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
        Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
            fingerprint(full, &meta).map(Some)
        }
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
    if !opened.is_file() || open_stat(&file)? != before {
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

/// The fingerprint of `full`, whose directory entry metadata is `meta`.
#[cfg(unix)]
fn fingerprint(_full: &Path, meta: &Metadata) -> std::io::Result<Stat> {
    Ok(stat(meta))
}

/// Directory listings on Windows omit ChangeTime, so each entry is opened for attributes
/// only. That takes no data access, so it does not conflict with a writer's share mode.
#[cfg(windows)]
fn fingerprint(full: &Path, _meta: &Metadata) -> std::io::Result<Stat> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .access_mode(windows::FILE_READ_ATTRIBUTES)
        .custom_flags(windows::FILE_FLAG_BACKUP_SEMANTICS | windows::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(full)?;
    open_stat(&file)
}

#[cfg(unix)]
fn open_stat(file: &fs::File) -> std::io::Result<Stat> {
    Ok(stat(&file.metadata()?))
}

/// The fingerprint of an open file. Windows reads ChangeTime from the handle: creation time,
/// which the old fingerprint used, never changes on a rewrite.
#[cfg(windows)]
fn open_stat(file: &fs::File) -> std::io::Result<Stat> {
    use std::os::windows::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(Stat {
        size: meta.file_size(),
        mtime: filetime_ns(meta.last_write_time()),
        ctime: filetime_ns(windows::change_time(file)?),
        ino: 0,
        mode: meta.file_attributes(),
    })
}

/// FILETIME counts 100 ns intervals from 1601-01-01.
#[cfg(windows)]
fn filetime_ns(ticks: u64) -> i128 {
    (ticks as i128 - 116_444_736_000_000_000) * 100
}

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, fs, io, os::windows::io::AsRawHandle};

    pub(super) const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    pub(super) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_BASIC_INFO: i32 = 0;

    /// `FILE_BASIC_INFO`: four FILETIMEs, then the attributes.
    #[repr(C)]
    #[derive(Default)]
    struct BasicInfo {
        creation: i64,
        access: i64,
        write: i64,
        change: i64,
        attributes: u32,
    }

    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            file: *mut c_void,
            class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
    }

    /// The ChangeTime FILETIME of an open file.
    pub(super) fn change_time(file: &fs::File) -> io::Result<u64> {
        let mut info = BasicInfo::default();
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FILE_BASIC_INFO,
                (&mut info as *mut BasicInfo).cast(),
                size_of::<BasicInfo>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.change as u64)
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
    /// Stack produced the head, so the directory lacks the stacked work and nothing was
    /// scanned: capturing it would revert that work.
    pub behind: bool,
}

impl Capture {
    /// The result for a directory that lacks the head's work: nothing was scanned.
    pub(crate) fn behind(head: LayerHead) -> Result<Self> {
        Ok(Self {
            update: None,
            operation: None,
            head,
            changes: SparseChanges::new([])?,
            hashed: 0,
            uncertain: Vec::new(),
            rebuilt: false,
            behind: true,
        })
    }
}

impl Repository {
    /// Whether Stack produced the head. Stack changes only native state, so a Layer's
    /// directory lacks the stacked work until it is refreshed to the head.
    pub(crate) fn stacked(&self, head: &LayerHead) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT a.stack FROM layer_heads h JOIN layer_attempts a ON a.id=h.operation \
                 WHERE h.layer=?1 AND h.revision=?2",
                rusqlite::params![head.layer().as_str(), head.revision() as i64],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            == Some(1))
    }

    /// The Layer's clone-backed directory, whether or not it exists yet.
    pub fn workspace_root(&self, layer: &LayerId) -> Result<PathBuf> {
        Ok(self.store_path()?.join("layers").join(layer.as_str()))
    }

    /// Scans `root` against the head and commits any difference. The caller decides
    /// whether the directory may be captured at all (see `capture_workspace`).
    pub(crate) fn capture_at(&mut self, head: LayerHead, root: &Path) -> Result<Capture> {
        let checkpoint = self.read_checkpoint(head.checkpoint())?;
        let base = self.manifest_at(checkpoint.base().version())?;
        let cache_path = self.capture_cache(head.layer())?;
        let mut cache = StatCache::load(&cache_path);
        let rebuilt = cache.rebuilt();
        let manifest = self.checkpoint_manifest(checkpoint.id())?;
        let tx = self.conn.unchecked_transaction()?;
        let scan = scan(root, &mut cache, &manifest, &mut |bytes| {
            store_blob(&tx, bytes)
        })?;
        tx.commit()?;
        // The cache only accelerates later scans; failing to save it loses no edits.
        let _ = cache.save(&cache_path);
        let changes = changes_from(&base, &scan.entries)?;
        let (update, operation) = if &changes == checkpoint.changes() {
            (None, None)
        } else {
            let operation = operation_id(&head, &changes, self.provenance.as_ref());
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
            behind: false,
        })
    }

    /// `.javelin/capture/<sha256(layer)>`: fixed-width and case-insensitive safe.
    pub(crate) fn capture_cache(&self, layer: &LayerId) -> Result<PathBuf> {
        let name = ObjectId::from_bytes(Sha256::digest(layer.as_str().as_bytes()).into());
        Ok(self.store_path()?.join("capture").join(name.to_string()))
    }

    fn store_path(&self) -> Result<PathBuf> {
        self.conn
            .path()
            .and_then(|db| Path::new(db).parent())
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::CorruptRepository("repository has no directory".into()))
    }
}

/// Stores new content; existing blobs are not re-read (`check` verifies them).
pub(crate) fn store_blob(conn: &Connection, bytes: &[u8]) -> Result<ObjectId> {
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

/// Provenance is hashed too: the same edit captured for another session is a
/// different checkpoint, so it must not replay this operation.
fn operation_id(
    head: &LayerHead,
    changes: &SparseChanges,
    provenance: Option<&crate::Provenance>,
) -> OperationId {
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
    if let Some(provenance) = provenance {
        hash.update(b"provenance\0");
        hash.update(provenance.bytes());
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
