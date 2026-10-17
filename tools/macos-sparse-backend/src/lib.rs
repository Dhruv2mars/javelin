//! Sparse Layer overlay served as a loopback NFSv3 filesystem.
//!
//! A Layer reads unchanged entries from an immutable base directory. Every change
//! lives in a private upper directory: `tree/` holds created or copied-up entries,
//! `whiteouts` lists base paths hidden by deletes and renames, and `tmp/` stages
//! copy-ups so an interrupted copy never publishes a partial file.
//! The base is never opened for writing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use filetime::FileTime;
use nfsserve::fs_util::metadata_to_fattr3;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, nfspath3, nfsstat3, nfstime3, sattr3, set_atime, set_mode3,
    set_mtime, set_size3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

const ROOT: fileid3 = 1;
const WHITEOUTS_HEADER: &[u8] = b"JAVELIN_WHITEOUTS_V1\0";
static NEXT_CLONE: AtomicU64 = AtomicU64::new(0);

/// Test-only fault injection: abort the process at a named point. `stage`
/// aborts after a copy-up is staged but before it is published; `rename`
/// aborts after a rename moved private bytes but before whiteouts persist.
pub const CRASH_AT_ENV: &str = "JAVELIN_OVERLAY_CRASH_AT";

fn crash_at(point: &str) {
    if std::env::var_os(CRASH_AT_ENV).is_some_and(|v| v == point) {
        std::process::abort();
    }
}

pub struct Overlay {
    base: PathBuf,
    upper: PathBuf,
    state: Mutex<State>,
    staged: AtomicU64,
}

struct State {
    ids: HashMap<fileid3, PathBuf>,
    paths: HashMap<PathBuf, fileid3>,
    next: fileid3,
    whiteouts: BTreeSet<PathBuf>,
}

enum Loc {
    Upper(PathBuf),
    Base(PathBuf),
}

impl Loc {
    fn path(&self) -> &Path {
        match self {
            Loc::Upper(p) | Loc::Base(p) => p,
        }
    }
}

fn io_err(e: io::Error) -> nfsstat3 {
    match e.kind() {
        io::ErrorKind::NotFound => nfsstat3::NFS3ERR_NOENT,
        io::ErrorKind::PermissionDenied => nfsstat3::NFS3ERR_ACCES,
        io::ErrorKind::AlreadyExists => nfsstat3::NFS3ERR_EXIST,
        io::ErrorKind::DirectoryNotEmpty => nfsstat3::NFS3ERR_NOTEMPTY,
        io::ErrorKind::NotADirectory => nfsstat3::NFS3ERR_NOTDIR,
        io::ErrorKind::IsADirectory => nfsstat3::NFS3ERR_ISDIR,
        _ => nfsstat3::NFS3ERR_IO,
    }
}

fn name(n: &filename3) -> Result<PathBuf, nfsstat3> {
    let bytes = &n.0;
    if bytes.is_empty() || bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(nfsstat3::NFS3ERR_INVAL);
    }
    let p = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    match p.components().next() {
        Some(Component::Normal(_)) => Ok(p),
        _ => Err(nfsstat3::NFS3ERR_INVAL),
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn check_readable_base(dir: &Path) -> io::Result<()> {
    let entries = fs::read_dir(dir).map_err(|e| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "unsupported base layout: cannot enumerate {}: {e}",
                dir.display()
            ),
        )
    })?;
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            check_readable_base(&entry.path())?;
        } else if kind.is_file() {
            File::open(entry.path()).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "unsupported base layout: file {} is not readable by the server: {e}",
                        entry.path().display()
                    ),
                )
            })?;
        }
    }
    Ok(())
}

impl Overlay {
    /// Opens a Layer. `upper` is created if missing; stale staging files from an
    /// interrupted copy-up are discarded.
    pub fn open(base: &Path, upper: &Path) -> io::Result<Overlay> {
        let base = base.canonicalize()?;
        check_readable_base(&base)?;
        fs::create_dir_all(upper)?;
        let upper = upper.canonicalize()?;
        if upper.starts_with(&base) || base.starts_with(&upper) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "base and upper must not nest",
            ));
        }
        fs::create_dir_all(upper.join("tree"))?;
        let tmp = upper.join("tmp");
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        fs::create_dir(&tmp)?;
        let whiteouts = read_whiteouts(&upper)?;
        let mut state = State {
            ids: HashMap::new(),
            paths: HashMap::new(),
            next: ROOT + 1,
            whiteouts,
        };
        state.ids.insert(ROOT, PathBuf::new());
        state.paths.insert(PathBuf::new(), ROOT);
        Ok(Overlay {
            base,
            upper,
            state: Mutex::new(state),
            staged: AtomicU64::new(0),
        })
    }

    fn tree(&self, rel: &Path) -> PathBuf {
        self.upper.join("tree").join(rel)
    }

    fn base_visible(st: &State, rel: &Path) -> bool {
        let mut prefix = PathBuf::new();
        for c in rel.components() {
            prefix.push(c);
            if st.whiteouts.contains(&prefix) {
                return false;
            }
        }
        true
    }

    fn locate(&self, st: &State, rel: &Path) -> Option<Loc> {
        let up = self.tree(rel);
        if fs::symlink_metadata(&up).is_ok() {
            return Some(Loc::Upper(up));
        }
        let b = self.base.join(rel);
        if Self::base_visible(st, rel) && fs::symlink_metadata(&b).is_ok() {
            return Some(Loc::Base(b));
        }
        None
    }

    fn path_of(st: &State, id: fileid3) -> Result<PathBuf, nfsstat3> {
        st.ids.get(&id).cloned().ok_or(nfsstat3::NFS3ERR_STALE)
    }

    fn id_of(st: &mut State, rel: &Path) -> fileid3 {
        if let Some(id) = st.paths.get(rel) {
            return *id;
        }
        let id = st.next;
        st.next += 1;
        st.ids.insert(id, rel.to_path_buf());
        st.paths.insert(rel.to_path_buf(), id);
        id
    }

    fn forget(st: &mut State, rel: &Path) {
        let gone: Vec<PathBuf> = st
            .paths
            .keys()
            .filter(|p| p.starts_with(rel))
            .cloned()
            .collect();
        for p in gone {
            if let Some(id) = st.paths.remove(&p) {
                st.ids.remove(&id);
            }
        }
    }

    fn rekey(st: &mut State, from: &Path, to: &Path) {
        Self::forget(st, to);
        let moved: Vec<(PathBuf, fileid3)> = st
            .paths
            .iter()
            .filter(|(p, _)| p.starts_with(from))
            .map(|(p, i)| (p.clone(), *i))
            .collect();
        for (p, id) in moved {
            st.paths.remove(&p);
            // Joining an empty suffix would add a trailing slash and break file lookups.
            let rest = p.strip_prefix(from).unwrap();
            let np = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            st.ids.insert(id, np.clone());
            st.paths.insert(np, id);
        }
    }

    fn attr(&self, st: &mut State, rel: &Path) -> Result<fattr3, nfsstat3> {
        let loc = self.locate(st, rel).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let meta = fs::symlink_metadata(loc.path()).map_err(io_err)?;
        let id = Self::id_of(st, rel);
        let mut a = metadata_to_fattr3(id, &meta);
        // The library helper forces owner-write and drops special bits.
        a.mode = meta.mode() & 0o7777;
        a.fsid = 0x4a41_5645;
        Ok(a)
    }

    /// Persists the whiteout set atomically before publishing it in memory.
    /// An upper entry always wins over a whiteout, so a whiteout written ahead
    /// of an interrupted delete or rename never hides surviving private bytes.
    fn add_whiteouts(&self, st: &mut State, rels: &[&Path]) -> Result<(), nfsstat3> {
        let mut next = st.whiteouts.clone();
        for rel in rels {
            if self.base.join(rel).symlink_metadata().is_ok() && Self::base_visible(st, rel) {
                next.insert(rel.to_path_buf());
            }
        }
        if next.len() == st.whiteouts.len() {
            return Ok(());
        }
        let mut body = WHITEOUTS_HEADER.to_vec();
        for w in &next {
            body.extend_from_slice(w.as_os_str().as_bytes());
            body.push(0);
        }
        let tmp = self.upper.join("tmp").join("whiteouts");
        let mut f = File::create(&tmp).map_err(io_err)?;
        f.write_all(&body).map_err(io_err)?;
        f.sync_all().map_err(io_err)?;
        fs::rename(&tmp, self.upper.join("whiteouts")).map_err(io_err)?;
        // The manifest is replaced; memory must match it even if the sync fails.
        st.whiteouts = next;
        sync_dir(&self.upper).map_err(io_err)
    }

    /// Creates upper directories for every ancestor, copying base modes.
    fn ensure_upper_dir(&self, st: &State, rel: &Path) -> Result<(), nfsstat3> {
        self.with_upper_dir(st, rel, || Ok(()))
    }

    /// Copy-up may need to populate a private parent that is logically read-only.
    /// The state mutex hides temporary access; restore modes on success and error.
    fn with_upper_dir<T>(
        &self,
        st: &State,
        rel: &Path,
        copy: impl FnOnce() -> Result<T, nfsstat3>,
    ) -> Result<T, nfsstat3> {
        let mut restore = Vec::new();
        let result = (|| {
            for prefix in rel.ancestors().collect::<Vec<_>>().into_iter().rev() {
                let up = self.tree(prefix);
                let (mode, created) = match fs::symlink_metadata(&up) {
                    Ok(m) if m.is_dir() => (m.mode() & 0o7777, false),
                    Ok(_) => return Err(nfsstat3::NFS3ERR_NOTDIR),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        let loc = self.locate(st, prefix).ok_or(nfsstat3::NFS3ERR_NOENT)?;
                        let m = fs::symlink_metadata(loc.path()).map_err(io_err)?;
                        if !m.is_dir() {
                            return Err(nfsstat3::NFS3ERR_NOTDIR);
                        }
                        fs::create_dir(&up).map_err(io_err)?;
                        (m.mode() & 0o7777, true)
                    }
                    Err(e) => return Err(io_err(e)),
                };
                if mode & 0o700 != 0o700 {
                    restore.push((up.clone(), fs::Permissions::from_mode(mode)));
                }
                if created || mode & 0o700 != 0o700 {
                    fs::set_permissions(&up, fs::Permissions::from_mode(mode | 0o700))
                        .map_err(io_err)?;
                }
                if created {
                    sync_dir(up.parent().unwrap()).map_err(io_err)?;
                }
            }
            copy()
        })();
        let mut restore_error = None;
        for (path, mode) in restore.into_iter().rev() {
            if let Err(e) = fs::set_permissions(path, mode) {
                restore_error = Some(io_err(e));
            }
        }
        match restore_error {
            Some(e) => Err(e),
            None => result,
        }
    }

    /// Copies one visible base entry into the upper tree through staging.
    fn copy_up(&self, st: &State, rel: &Path) -> Result<PathBuf, nfsstat3> {
        let src = match self.locate(st, rel).ok_or(nfsstat3::NFS3ERR_NOENT)? {
            Loc::Upper(p) => return Ok(p),
            Loc::Base(p) => p,
        };
        self.with_upper_dir(st, rel.parent().unwrap_or(Path::new("")), || {
            let meta = fs::symlink_metadata(&src).map_err(io_err)?;
            let dst = self.tree(rel);
            if meta.is_dir() {
                fs::create_dir(&dst).map_err(io_err)?;
                fs::set_permissions(&dst, fs::Permissions::from_mode(meta.mode() & 0o7777))
                    .map_err(io_err)?;
                preserve_times(&dst, &meta).map_err(io_err)?;
                sync_dir(dst.parent().unwrap()).map_err(io_err)?;
                return Ok(dst);
            }
            let stage = self.upper.join("tmp").join(format!(
                "copy-{}",
                self.staged.fetch_add(1, Ordering::SeqCst)
            ));
            if meta.file_type().is_symlink() {
                symlink(fs::read_link(&src).map_err(io_err)?, &stage).map_err(io_err)?;
                preserve_times(&stage, &meta).map_err(io_err)?;
            } else {
                let mut from = File::open(&src).map_err(io_err)?;
                let mut to = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stage)
                    .map_err(io_err)?;
                io::copy(&mut from, &mut to).map_err(io_err)?;
                to.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))
                    .map_err(io_err)?;
                preserve_times(&stage, &meta).map_err(io_err)?;
                to.sync_all().map_err(io_err)?;
            }
            crash_at("stage");
            fs::rename(&stage, &dst).map_err(io_err)?;
            sync_dir(dst.parent().unwrap()).map_err(io_err)?;
            Ok(dst)
        })
    }

    /// Copies a whole visible subtree into the upper tree.
    fn copy_up_tree(&self, st: &State, rel: &Path) -> Result<(), nfsstat3> {
        let meta =
            fs::symlink_metadata(self.locate(st, rel).ok_or(nfsstat3::NFS3ERR_NOENT)?.path())
                .map_err(io_err)?;
        let up = self.copy_up(st, rel)?;
        if fs::symlink_metadata(&up).map_err(io_err)?.is_dir() {
            for child in self.children(st, rel)?.into_keys() {
                self.copy_up_tree(st, &rel.join(child))?;
            }
            preserve_times(&up, &meta).map_err(io_err)?;
        }
        Ok(())
    }

    fn children(&self, st: &State, rel: &Path) -> Result<BTreeMap<PathBuf, ()>, nfsstat3> {
        let mut out = BTreeMap::new();
        let up = self.tree(rel);
        if up.is_dir() && !up.is_symlink() {
            for e in fs::read_dir(&up).map_err(io_err)? {
                out.insert(PathBuf::from(e.map_err(io_err)?.file_name()), ());
            }
        }
        let b = self.base.join(rel);
        if Self::base_visible(st, rel) && b.is_dir() && !b.is_symlink() {
            for e in fs::read_dir(&b).map_err(io_err)? {
                let n = PathBuf::from(e.map_err(io_err)?.file_name());
                if Self::base_visible(st, &rel.join(&n)) {
                    out.insert(n, ());
                }
            }
        }
        Ok(out)
    }

    fn dir_rel(&self, st: &State, id: fileid3) -> Result<PathBuf, nfsstat3> {
        let rel = Self::path_of(st, id)?;
        match self.locate(st, &rel) {
            Some(l) if l.path().is_dir() && !l.path().is_symlink() => Ok(rel),
            Some(_) => Err(nfsstat3::NFS3ERR_NOTDIR),
            None => Err(nfsstat3::NFS3ERR_STALE),
        }
    }

    /// Data operations act only on regular files, never through symlinks.
    fn regular_file(&self, st: &State, rel: &Path) -> Result<(), nfsstat3> {
        let loc = self.locate(st, rel).ok_or(nfsstat3::NFS3ERR_STALE)?;
        let meta = fs::symlink_metadata(loc.path()).map_err(io_err)?;
        if meta.is_dir() {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        if !meta.is_file() {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn to_time(t: nfstime3) -> SystemTime {
    UNIX_EPOCH + Duration::new(t.seconds as u64, t.nseconds)
}

/// Applies NFS mode, size and time changes to an upper entry. Ownership changes
/// are ignored because every Layer entry belongs to the serving user.
fn apply_sattr(path: &Path, a: &sattr3) -> io::Result<()> {
    let original = fs::symlink_metadata(path)?.permissions();
    let grant_write = matches!(a.size, set_size3::size(_))
        && matches!(a.mode, set_mode3::mode(m) if m & 0o200 != 0)
        && original.mode() & 0o200 == 0;
    if grant_write {
        fs::set_permissions(path, fs::Permissions::from_mode(original.mode() | 0o200))?;
    }
    let result = (|| {
        if let set_size3::size(n) = a.size {
            let f = OpenOptions::new().write(true).open(path)?;
            f.set_len(n)?;
            f.sync_all()?;
        }
        apply_times(path, a)?;
        if let set_mode3::mode(m) = a.mode {
            fs::set_permissions(path, fs::Permissions::from_mode(m & 0o7777))?;
        }
        Ok(())
    })();
    if result.is_err() && grant_write {
        fs::set_permissions(path, original)?;
    }
    result
}

fn apply_times(path: &Path, a: &sattr3) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    let mut atime = FileTime::from_last_access_time(&meta);
    let mut mtime = FileTime::from_last_modification_time(&meta);
    let mut touched = false;
    match a.atime {
        set_atime::SET_TO_CLIENT_TIME(t) => {
            atime = FileTime::from_system_time(to_time(t));
            touched = true
        }
        set_atime::SET_TO_SERVER_TIME => {
            atime = FileTime::now();
            touched = true
        }
        set_atime::DONT_CHANGE => {}
    }
    match a.mtime {
        set_mtime::SET_TO_CLIENT_TIME(t) => {
            mtime = FileTime::from_system_time(to_time(t));
            touched = true
        }
        set_mtime::SET_TO_SERVER_TIME => {
            mtime = FileTime::now();
            touched = true
        }
        set_mtime::DONT_CHANGE => {}
    }
    if touched {
        filetime::set_symlink_file_times(path, atime, mtime)?;
    }
    Ok(())
}

fn preserve_times(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    filetime::set_symlink_file_times(
        path,
        FileTime::from_last_access_time(meta),
        FileTime::from_last_modification_time(meta),
    )
}

fn read_whiteouts(upper: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let mut body = Vec::new();
    match File::open(upper.join("whiteouts")) {
        Ok(mut f) => {
            f.read_to_end(&mut body)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(e),
    }
    let (paths, separator) = if let Some(paths) = body.strip_prefix(WHITEOUTS_HEADER) {
        if !paths.is_empty() && !paths.ends_with(&[0]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated whiteouts",
            ));
        }
        (paths, 0)
    } else {
        if body.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported whiteout encoding",
            ));
        }
        (body.as_slice(), b'\n')
    };
    Ok(paths
        .split(|b| *b == separator)
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(std::ffi::OsStr::from_bytes(l)))
        .collect())
}

/// Copies a quiescent Layer's private state into a new upper directory.
/// Only private changes are copied; the base stays shared.
/// The upper root is owner-only from staging through publication.
pub fn clone_upper(from: &Path, to: &Path) -> io::Result<()> {
    let from = from.canonicalize()?;
    let parent = match to.parent().ok_or(io::ErrorKind::InvalidInput)? {
        p if p.as_os_str().is_empty() => Path::new("."),
        p => p,
    }
    .canonicalize()?;
    let to = parent.join(to.file_name().ok_or(io::ErrorKind::InvalidInput)?);
    if to.starts_with(&from) || from.starts_with(&to) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone destination must not nest with its source",
        ));
    }
    if to.symlink_metadata().is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let stage = loop {
        let path = parent.join(format!(
            ".javelin-clone-{}-{}",
            std::process::id(),
            NEXT_CLONE.fetch_add(1, Ordering::Relaxed)
        ));
        // Restrict access atomically: copied entries can have public modes even
        // when the source upper root is private.
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let mut held = None;
    let mut published = false;
    let copied = (|| {
        let lock = File::create(stage.join("lock"))?;
        lock.try_lock().map_err(io::Error::from)?;
        held = Some(lock);
        copy_tree(&from.join("tree"), &stage.join("tree"))?;
        if from.join("whiteouts").exists() {
            fs::copy(from.join("whiteouts"), stage.join("whiteouts"))?;
            File::open(stage.join("whiteouts"))?.sync_all()?;
        }
        File::open(&stage)?.sync_all()?;
        fs::rename(&stage, &to)?;
        published = true;
        File::open(&parent)?.sync_all()
    })();
    if let Err(e) = copied {
        return Err(match force_remove(if published { &to } else { &stage }) {
            Ok(()) => e,
            Err(c) => io::Error::other(format!("{e}; partial clone not removed: {c}")),
        });
    }
    drop(held);
    Ok(())
}

/// Removes a partial clone, first restoring owner access to copied read-only directories.
fn force_remove(path: &Path) -> io::Result<()> {
    fn unlock(p: &Path) -> io::Result<()> {
        let meta = fs::symlink_metadata(p)?;
        if meta.is_dir() {
            fs::set_permissions(p, fs::Permissions::from_mode(meta.mode() & 0o7777 | 0o700))?;
            for e in fs::read_dir(p)? {
                unlock(&e?.path())?;
            }
        }
        Ok(())
    }
    unlock(path)?;
    fs::remove_dir_all(path)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        symlink(fs::read_link(from)?, to)?;
        return preserve_times(to, &meta);
    }
    let mode = meta.mode() & 0o7777;
    let readable = mode | if meta.is_dir() { 0o500 } else { 0o400 };
    if readable != mode {
        fs::set_permissions(from, fs::Permissions::from_mode(readable))?;
    }
    let copied = (|| {
        if meta.is_dir() {
            fs::create_dir(to)?;
            for e in fs::read_dir(from)? {
                let e = e?;
                copy_tree(&e.path(), &to.join(e.file_name()))?;
            }
        } else {
            fs::copy(from, to)?;
        }
        let handle = File::open(to)?;
        preserve_times(to, &meta)?;
        fs::set_permissions(to, meta.permissions())?;
        handle.sync_all()
    })();
    if readable != mode {
        fs::set_permissions(from, meta.permissions())?;
    }
    copied
}

#[async_trait]
impl NFSFileSystem for Overlay {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    fn root_dir(&self) -> fileid3 {
        ROOT
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let mut st = self.lock();
        let dir = self.dir_rel(&st, dirid)?;
        match filename.0.as_slice() {
            b"." => return Ok(dirid),
            b".." => return Ok(Self::id_of(&mut st, dir.parent().unwrap_or(Path::new("")))),
            _ => {}
        }
        let rel = dir.join(name(filename)?);
        self.locate(&st, &rel).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        Ok(Self::id_of(&mut st, &rel))
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let mut st = self.lock();
        let rel = Self::path_of(&st, id)?;
        self.attr(&mut st, &rel).map_err(|e| {
            if matches!(e, nfsstat3::NFS3ERR_NOENT) {
                nfsstat3::NFS3ERR_STALE
            } else {
                e
            }
        })
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        let mut st = self.lock();
        let rel = Self::path_of(&st, id)?;
        let up = if rel.as_os_str().is_empty() {
            self.tree(&rel)
        } else {
            self.copy_up(&st, &rel)?
        };
        if fs::symlink_metadata(&up)
            .map_err(io_err)?
            .file_type()
            .is_symlink()
        {
            apply_times(&up, &setattr).map_err(io_err)?;
            return self.attr(&mut st, &rel);
        }
        apply_sattr(&up, &setattr).map_err(io_err)?;
        self.attr(&mut st, &rel)
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let st = self.lock();
        let rel = Self::path_of(&st, id)?;
        self.regular_file(&st, &rel)?;
        let loc = self.locate(&st, &rel).ok_or(nfsstat3::NFS3ERR_STALE)?;
        let mut f = File::open(loc.path()).map_err(io_err)?;
        let len = f.metadata().map_err(io_err)?.len();
        f.seek(SeekFrom::Start(offset)).map_err(io_err)?;
        let mut buf = Vec::new();
        f.take(count as u64).read_to_end(&mut buf).map_err(io_err)?;
        Ok((buf, offset + count as u64 >= len))
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        let mut st = self.lock();
        let rel = Self::path_of(&st, id)?;
        self.regular_file(&st, &rel)?;
        let up = self.copy_up(&st, &rel)?;
        let mut f = OpenOptions::new().write(true).open(&up).map_err(io_err)?;
        f.seek(SeekFrom::Start(offset)).map_err(io_err)?;
        f.write_all(data).map_err(io_err)?;
        f.sync_data().map_err(io_err)?;
        self.attr(&mut st, &rel)
    }

    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let mut st = self.lock();
        let rel = self.dir_rel(&st, dirid)?.join(name(filename)?);
        if let Some(l) = self.locate(&st, &rel)
            && l.path().is_dir()
            && !l.path().is_symlink()
        {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        // UNCHECKED CREATE of an existing name keeps its bytes unless the
        // request sets a size (RFC 1813 section 3.3.8).
        let up = if self.locate(&st, &rel).is_some() {
            self.regular_file(&st, &rel)?;
            self.copy_up(&st, &rel)?
        } else {
            self.ensure_upper_dir(&st, rel.parent().unwrap())?;
            let up = self.tree(&rel);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&up)
                .map_err(io_err)?
                .sync_all()
                .map_err(io_err)?;
            sync_dir(up.parent().unwrap()).map_err(io_err)?;
            up
        };
        apply_sattr(&up, &attr).map_err(io_err)?;
        let a = self.attr(&mut st, &rel)?;
        Ok((a.fileid, a))
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        let mut st = self.lock();
        let rel = self.dir_rel(&st, dirid)?.join(name(filename)?);
        if self.locate(&st, &rel).is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        self.ensure_upper_dir(&st, rel.parent().unwrap())?;
        let up = self.tree(&rel);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&up)
            .map_err(io_err)?
            .sync_all()
            .map_err(io_err)?;
        sync_dir(up.parent().unwrap()).map_err(io_err)?;
        Ok(Self::id_of(&mut st, &rel))
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let mut st = self.lock();
        let rel = self.dir_rel(&st, dirid)?.join(name(dirname)?);
        if self.locate(&st, &rel).is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        // A whiteout at this path also hides any older base subtree below it.
        self.ensure_upper_dir(&st, rel.parent().unwrap())?;
        fs::create_dir(self.tree(&rel)).map_err(io_err)?;
        sync_dir(self.tree(rel.parent().unwrap()).as_path()).map_err(io_err)?;
        let a = self.attr(&mut st, &rel)?;
        Ok((a.fileid, a))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        let mut st = self.lock();
        let rel = self.dir_rel(&st, dirid)?.join(name(filename)?);
        let loc = self.locate(&st, &rel).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let is_dir = loc.path().is_dir() && !loc.path().is_symlink();
        if is_dir && !self.children(&st, &rel)?.is_empty() {
            return Err(nfsstat3::NFS3ERR_NOTEMPTY);
        }
        let result = (|| {
            self.add_whiteouts(&mut st, &[&rel])?;
            let up = self.tree(&rel);
            if fs::symlink_metadata(&up).is_ok() {
                if is_dir {
                    fs::remove_dir(&up)
                } else {
                    fs::remove_file(&up)
                }
                .map_err(io_err)?;
                sync_dir(up.parent().unwrap()).map_err(io_err)?;
            }
            Ok(())
        })();
        // Whatever step failed, handles to an entry that is no longer visible go stale.
        if self.locate(&st, &rel).is_none() {
            Self::forget(&mut st, &rel);
        }
        result
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        let mut st = self.lock();
        let from = self.dir_rel(&st, from_dirid)?.join(name(from_filename)?);
        let to = self.dir_rel(&st, to_dirid)?.join(name(to_filename)?);
        if from == to {
            return Ok(());
        }
        if to.starts_with(&from) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        let src = self.locate(&st, &from).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        let src_dir = src.path().is_dir() && !src.path().is_symlink();
        if let Some(dst) = self.locate(&st, &to) {
            let dst_dir = dst.path().is_dir() && !dst.path().is_symlink();
            if src_dir != dst_dir {
                return Err(if dst_dir {
                    nfsstat3::NFS3ERR_ISDIR
                } else {
                    nfsstat3::NFS3ERR_NOTDIR
                });
            }
            if dst_dir && !self.children(&st, &to)?.is_empty() {
                return Err(nfsstat3::NFS3ERR_NOTEMPTY);
            }
        }
        // Directory renames materialize the visible subtree so the moved names
        // no longer depend on base paths; file renames copy up only the file.
        if src_dir {
            self.copy_up_tree(&st, &from)?
        } else {
            self.copy_up(&st, &from).map(|_| ())?
        };
        self.ensure_upper_dir(&st, to.parent().unwrap())?;
        // Move private bytes first; rename(2) replaces an empty directory or a
        // file atomically. Whiteouts follow, so an interruption can briefly
        // re-expose the old base name but never loses the destination.
        let dst = self.tree(&to);
        fs::rename(self.tree(&from), &dst).map_err(io_err)?;
        // Handles follow the moved bytes even if a later sync or whiteout write fails.
        Self::rekey(&mut st, &from, &to);
        sync_dir(dst.parent().unwrap()).map_err(io_err)?;
        sync_dir(self.tree(from.parent().unwrap()).as_path()).map_err(io_err)?;
        crash_at("rename");
        self.add_whiteouts(&mut st, &[&from, &to])
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let mut st = self.lock();
        let dir = self.dir_rel(&st, dirid)?;
        let names: Vec<PathBuf> = self.children(&st, &dir)?.into_keys().collect();
        let mut skipping = start_after != 0;
        if skipping
            && !names
                .iter()
                .any(|n| st.paths.get(&dir.join(n)) == Some(&start_after))
        {
            return Err(nfsstat3::NFS3ERR_BAD_COOKIE);
        }
        let mut entries = Vec::new();
        let mut end = true;
        for n in names {
            let rel = dir.join(&n);
            if skipping {
                skipping = st.paths.get(&rel) != Some(&start_after);
                continue;
            }
            if entries.len() >= max_entries {
                end = false;
                break;
            }
            let attr = self.attr(&mut st, &rel)?;
            entries.push(DirEntry {
                fileid: attr.fileid,
                name: n.as_os_str().as_bytes().into(),
                attr,
            });
        }
        Ok(ReadDirResult { entries, end })
    }

    async fn symlink(
        &self,
        dirid: fileid3,
        linkname: &filename3,
        target: &nfspath3,
        attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let mut st = self.lock();
        let rel = self.dir_rel(&st, dirid)?.join(name(linkname)?);
        if self.locate(&st, &rel).is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        self.ensure_upper_dir(&st, rel.parent().unwrap())?;
        let up = self.tree(&rel);
        symlink(std::ffi::OsStr::from_bytes(&target.0), &up).map_err(io_err)?;
        apply_times(&up, attr).map_err(io_err)?;
        sync_dir(up.parent().unwrap()).map_err(io_err)?;
        let a = self.attr(&mut st, &rel)?;
        Ok((a.fileid, a))
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let st = self.lock();
        let rel = Self::path_of(&st, id)?;
        let loc = self.locate(&st, &rel).ok_or(nfsstat3::NFS3ERR_STALE)?;
        Ok(fs::read_link(loc.path())
            .map_err(io_err)?
            .as_os_str()
            .as_bytes()
            .into())
    }
}
