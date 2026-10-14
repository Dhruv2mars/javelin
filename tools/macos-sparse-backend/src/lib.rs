//! Sparse Layer overlay served as a loopback NFSv3 filesystem.
//!
//! A Layer reads unchanged entries from an immutable base directory. Every change
//! lives in a private upper directory: `tree/` holds created or copied-up entries,
//! `whiteouts` lists base paths hidden by deletes and renames, and `tmp/` stages
//! copy-ups so an interrupted copy never publishes a partial file.
//! The base is never opened for writing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nfsserve::fs_util::metadata_to_fattr3;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, nfspath3, nfsstat3, nfstime3, sattr3, set_atime, set_mode3,
    set_mtime, set_size3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

const ROOT: fileid3 = 1;

/// Test-only fault injection: abort the process after a copy-up is staged but
/// before it is published, to prove interrupted copy-ups leave base bytes visible.
pub const CRASH_AFTER_STAGE_ENV: &str = "JAVELIN_OVERLAY_CRASH_AFTER_STAGE";

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
    if bytes.is_empty() || bytes.contains(&b'/') || bytes.contains(&0) || bytes.contains(&b'\n') {
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

impl Overlay {
    /// Opens a Layer. `upper` is created if missing; stale staging files from an
    /// interrupted copy-up are discarded.
    pub fn open(base: &Path, upper: &Path) -> io::Result<Overlay> {
        let base = base.canonicalize()?;
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
        a.fsid = 0x4a41_5645;
        Ok(a)
    }

    /// Persists the whiteout set atomically. Whiteouts are written before an
    /// operation hides base content, and an upper entry always wins over a
    /// whiteout, so an interruption never exposes a half-applied change.
    fn add_whiteouts(&self, st: &mut State, rels: &[&Path]) -> Result<(), nfsstat3> {
        let mut changed = false;
        for rel in rels {
            if self.base.join(rel).symlink_metadata().is_ok() && Self::base_visible(st, rel) {
                changed |= st.whiteouts.insert(rel.to_path_buf());
            }
        }
        if !changed {
            return Ok(());
        }
        let mut body = Vec::new();
        for w in &st.whiteouts {
            body.extend_from_slice(w.as_os_str().as_bytes());
            body.push(b'\n');
        }
        let tmp = self.upper.join("tmp").join("whiteouts");
        let mut f = File::create(&tmp).map_err(io_err)?;
        f.write_all(&body).map_err(io_err)?;
        f.sync_all().map_err(io_err)?;
        fs::rename(&tmp, self.upper.join("whiteouts")).map_err(io_err)?;
        sync_dir(&self.upper).map_err(io_err)
    }

    /// Creates upper directories for every ancestor, copying base modes.
    fn ensure_upper_dir(&self, st: &State, rel: &Path) -> Result<(), nfsstat3> {
        let mut prefix = PathBuf::new();
        for c in rel.components() {
            prefix.push(c);
            let up = self.tree(&prefix);
            match fs::symlink_metadata(&up) {
                Ok(m) if m.is_dir() => continue,
                Ok(_) => return Err(nfsstat3::NFS3ERR_NOTDIR),
                Err(_) => {}
            }
            let mode = match self.locate(st, &prefix) {
                Some(Loc::Base(b)) => {
                    let m = fs::metadata(&b).map_err(io_err)?;
                    if !m.is_dir() {
                        return Err(nfsstat3::NFS3ERR_NOTDIR);
                    }
                    m.mode() & 0o7777
                }
                Some(Loc::Upper(_)) => unreachable!(),
                None => return Err(nfsstat3::NFS3ERR_NOENT),
            };
            fs::create_dir(&up).map_err(io_err)?;
            fs::set_permissions(&up, fs::Permissions::from_mode(mode)).map_err(io_err)?;
        }
        Ok(())
    }

    /// Copies one visible base entry into the upper tree through staging.
    fn copy_up(&self, st: &State, rel: &Path) -> Result<PathBuf, nfsstat3> {
        let src = match self.locate(st, rel).ok_or(nfsstat3::NFS3ERR_NOENT)? {
            Loc::Upper(p) => return Ok(p),
            Loc::Base(p) => p,
        };
        if let Some(parent) = rel.parent() {
            self.ensure_upper_dir(st, parent)?;
        }
        let meta = fs::symlink_metadata(&src).map_err(io_err)?;
        let dst = self.tree(rel);
        if meta.is_dir() {
            fs::create_dir(&dst).map_err(io_err)?;
            fs::set_permissions(&dst, fs::Permissions::from_mode(meta.mode() & 0o7777))
                .map_err(io_err)?;
            return Ok(dst);
        }
        let stage = self.upper.join("tmp").join(format!(
            "copy-{}",
            self.staged.fetch_add(1, Ordering::SeqCst)
        ));
        if meta.file_type().is_symlink() {
            symlink(fs::read_link(&src).map_err(io_err)?, &stage).map_err(io_err)?;
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
            to.sync_all().map_err(io_err)?;
        }
        if std::env::var_os(CRASH_AFTER_STAGE_ENV).is_some() {
            std::process::abort();
        }
        fs::rename(&stage, &dst).map_err(io_err)?;
        sync_dir(dst.parent().unwrap()).map_err(io_err)?;
        Ok(dst)
    }

    /// Copies a whole visible subtree into the upper tree.
    fn copy_up_tree(&self, st: &State, rel: &Path) -> Result<(), nfsstat3> {
        let up = self.copy_up(st, rel)?;
        if fs::symlink_metadata(&up).map_err(io_err)?.is_dir() {
            for child in self.children(st, rel)?.into_keys() {
                self.copy_up_tree(st, &rel.join(child))?;
            }
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
    if let set_mode3::mode(m) = a.mode {
        fs::set_permissions(path, fs::Permissions::from_mode(m & 0o7777))?;
    }
    if let set_size3::size(n) = a.size {
        let f = OpenOptions::new().write(true).open(path)?;
        f.set_len(n)?;
        f.sync_all()?;
    }
    let mut times = FileTimes::new();
    let mut touched = false;
    match a.atime {
        set_atime::SET_TO_CLIENT_TIME(t) => {
            times = times.set_accessed(to_time(t));
            touched = true
        }
        set_atime::SET_TO_SERVER_TIME => {
            times = times.set_accessed(SystemTime::now());
            touched = true
        }
        set_atime::DONT_CHANGE => {}
    }
    match a.mtime {
        set_mtime::SET_TO_CLIENT_TIME(t) => {
            times = times.set_modified(to_time(t));
            touched = true
        }
        set_mtime::SET_TO_SERVER_TIME => {
            times = times.set_modified(SystemTime::now());
            touched = true
        }
        set_mtime::DONT_CHANGE => {}
    }
    if touched {
        File::open(path)?.set_times(times)?;
    }
    Ok(())
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
    Ok(body
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(std::ffi::OsStr::from_bytes(l)))
        .collect())
}

/// Copies a quiescent Layer's private state into a new upper directory.
/// Only private changes are copied; the base stays shared.
pub fn clone_upper(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    copy_tree(&from.join("tree"), &to.join("tree"))?;
    if from.join("whiteouts").exists() {
        fs::copy(from.join("whiteouts"), to.join("whiteouts"))?;
    }
    File::open(to)?.sync_all()
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        fs::set_permissions(to, meta.permissions())
    } else {
        fs::copy(from, to)?;
        File::open(to)?.sync_all()
    }
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
        self.ensure_upper_dir(&st, rel.parent().unwrap())?;
        let up = self.tree(&rel);
        if up.is_symlink() {
            fs::remove_file(&up).map_err(io_err)?;
        }
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&up)
            .map_err(io_err)?;
        f.sync_all().map_err(io_err)?;
        sync_dir(up.parent().unwrap()).map_err(io_err)?;
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
        Self::forget(&mut st, &rel);
        Ok(())
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
        self.add_whiteouts(&mut st, &[&from, &to])?;
        let dst = self.tree(&to);
        if src_dir && dst.is_dir() {
            fs::remove_dir(&dst).map_err(io_err)?;
        }
        fs::rename(self.tree(&from), &dst).map_err(io_err)?;
        sync_dir(dst.parent().unwrap()).map_err(io_err)?;
        sync_dir(self.tree(from.parent().unwrap()).as_path()).map_err(io_err)?;
        Self::rekey(&mut st, &from, &to);
        Ok(())
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
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let mut st = self.lock();
        let rel = self.dir_rel(&st, dirid)?.join(name(linkname)?);
        if self.locate(&st, &rel).is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        self.ensure_upper_dir(&st, rel.parent().unwrap())?;
        let up = self.tree(&rel);
        symlink(std::ffi::OsStr::from_bytes(&target.0), &up).map_err(io_err)?;
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
