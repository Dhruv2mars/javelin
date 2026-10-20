//! Clone-backed Layer workspaces: each Layer is a plain directory cloned from an
//! immutable, materialized World base on the repository's own volume.
//!
//! Store layout under `.javelin/`:
//! - `workspace.lock` serializes workspace changes between processes.
//! - `bases/<N>` holds World N, written once and sealed. Unix seals it with root mode
//!   000, so ordinary tools cannot open it, and briefly reopens it, under the lock,
//!   to clone it. `base(M)` is `clone(base N)` plus the N→M tree diff, O(change).
//! - `layers/<id>` is the Layer's writable directory.
//! - `tmp/` holds staging. Anything still there at recovery is abandoned work.
//! - `trash/` holds renamed-away Layers, bases and staging, reclaimed in bounded steps.
//!
//! The database decides first and the directories follow: discard records the
//! terminal event, then renames the directory into trash. Recovery finishes any step
//! a crash interrupted. Only trash for terminal Layers, evicted bases and abandoned
//! staging is ever deleted.
mod clone;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

pub use clone::{CloneReport, CloneStrategy};

use crate::{
    CheckpointId, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, OperationId,
    RepoPath, Repository, Result, VersionNumber, domain::WorldIndex, repository::Index,
};

/// Materialized bases kept, besides any pinned for the current command.
const BASE_CACHE: usize = 2;
/// Filesystem entries each ordinary command may delete from trash.
const RECLAIM_BUDGET: usize = 4096;

/// Where a Layer's directory and its base live. Paths exist only once opened.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayerWorkspace {
    pub layer: LayerId,
    /// The Layer's writable directory, `.javelin/layers/<id>`.
    pub root: PathBuf,
    pub exists: bool,
    pub base_version: VersionNumber,
    /// The sealed base directory, `.javelin/bases/<N>`. It may be evicted at any time
    /// and is not readable on Unix while sealed; capture should compare the Layer with
    /// its checkpoint and stat cache instead.
    pub base_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseSource {
    /// Already materialized.
    Cached,
    /// Cloned from another materialized base, plus the changeset between them.
    Derived,
    /// Written from stored objects (cold).
    Materialized,
}

/// How a base was obtained. `elapsed_ms` is the cold or derive cost alone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BaseReport {
    pub version: VersionNumber,
    pub source: BaseSource,
    pub derived_from: Option<VersionNumber>,
    /// Paths written or removed on top of the clone, or all paths when materialized.
    pub changed_paths: usize,
    pub written_bytes: u64,
    pub clone: Option<CloneReport>,
    pub elapsed_ms: u64,
}

/// The result of opening a Layer's directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceReport {
    pub layer: LayerId,
    pub root: PathBuf,
    /// False when the directory already existed; nothing else was done.
    pub created: bool,
    pub checkpoint: CheckpointId,
    pub base: Option<BaseReport>,
    /// How the Layer was cloned from its base: strategy, fallback reason, copied bytes.
    pub clone: Option<CloneReport>,
    /// Checkpoint changes applied on top of the base clone.
    pub applied_changes: usize,
    /// Layer clone and apply time, excluding base materialization.
    pub elapsed_ms: u64,
    pub recovery: WorkspaceRecovery,
}

/// Bounded cleanup done by a command.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct WorkspaceRecovery {
    /// Directories renamed into trash: discarded Layers and abandoned staging.
    pub trashed: usize,
    /// Filesystem entries deleted from trash.
    pub reclaimed: usize,
    /// Trash entries left for later commands.
    pub remaining: usize,
    /// Trash entries kept because their Layer is not terminal.
    pub retained: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceDiscard {
    pub update: LayerUpdate,
    /// Whether this call moved the Layer's directory into trash.
    pub trashed: bool,
    pub recovery: WorkspaceRecovery,
}

impl Repository {
    /// The Layer's directory and base paths, for capture and scripts. Takes no lock.
    pub fn layer_workspace(&self, layer: &LayerId) -> Result<LayerWorkspace> {
        let store = self.store_dir()?;
        let checkpoint = match self.layer_state(layer)? {
            LayerState::Active { head } | LayerState::Published { head, .. } => head.checkpoint(),
            LayerState::Discarded { checkpoint, .. } => checkpoint,
        };
        let base_version = self.read_checkpoint(checkpoint)?.base().version();
        let root = store.join("layers").join(layer.as_str());
        Ok(LayerWorkspace {
            layer: layer.clone(),
            exists: root.try_exists()?,
            root,
            base_version,
            base_root: store.join("bases").join(base_version.to_string()),
        })
    }

    /// Materializes an active Layer's directory by cloning its base and applying its
    /// checkpoint. Idempotent: an existing directory is returned untouched.
    pub fn open_workspace(&mut self, layer: &LayerId) -> Result<WorkspaceReport> {
        let store = Store::lock(self)?;
        let recovery = store.recover(self, Some(RECLAIM_BUDGET))?;
        let head = match self.layer_state(layer)? {
            LayerState::Active { head } => head,
            LayerState::Published { operation, .. } => {
                return Err(Error::LayerPublished {
                    layer: layer.clone(),
                    operation,
                });
            }
            LayerState::Discarded { .. } => {
                return Err(Error::InvalidInput(format!(
                    "Layer {:?} is discarded",
                    layer.as_str()
                )));
            }
        };
        let root = store.layers().join(layer.as_str());
        let mut report = WorkspaceReport {
            layer: layer.clone(),
            root: root.clone(),
            created: false,
            checkpoint: head.checkpoint(),
            base: None,
            clone: None,
            applied_changes: 0,
            elapsed_ms: 0,
            recovery,
        };
        if root.try_exists()? {
            return Ok(report);
        }
        let checkpoint = self.read_checkpoint(head.checkpoint())?;
        let version = checkpoint.base().version();
        let (base_root, base) = self.ensure_base(&store, version)?;
        let started = Instant::now();
        let staging = store.staging(&format!("layer~{}", layer.as_str()));
        let built = (|| {
            let clone = unsealed(&base_root, || clone::clone_tree(&base_root, &staging))?;
            open_root(&staging)?;
            let changes = checkpoint.changes().entries();
            let (applied, _) = if changes.is_empty() {
                (0, 0)
            } else {
                let before = At(Index::new(&self.conn), checkpoint.base().version());
                let changes = changes.iter().map(|(p, e)| (p, e.as_ref()));
                self.apply(&staging, &before, changes)?
            };
            clone::barrier(&staging)?;
            fs::rename(&staging, &root)?;
            sync_dir(&store.layers())?;
            Ok::<_, Error>((clone, applied))
        })();
        let (clone, applied) = built.inspect_err(|_| store.abandon(&staging))?;
        let head_version = self.world_head()?;
        store.evict_bases(&[version, head_version])?;
        report.created = true;
        report.base = Some(base);
        report.clone = Some(clone);
        report.applied_changes = applied;
        report.elapsed_ms = millis(started);
        Ok(report)
    }

    /// Records the terminal discard, then renames the directory into trash for later
    /// reclamation. Retrying with the same operation and head is safe after any crash.
    pub fn discard_workspace(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
    ) -> Result<WorkspaceDiscard> {
        let store = Store::lock(self)?;
        let update = self.discard_layer(operation, expected)?;
        let layer = expected.layer();
        let mut trashed = false;
        if matches!(self.layer_state(layer)?, LayerState::Discarded { .. }) {
            let root = store.layers().join(layer.as_str());
            if root.try_exists()? {
                store.trash(&root, &format!("layer~{}", layer.as_str()))?;
                trashed = true;
            }
        }
        let recovery = store.recover(self, Some(RECLAIM_BUDGET))?;
        Ok(WorkspaceDiscard {
            update,
            trashed,
            recovery,
        })
    }

    /// Finishes interrupted steps, reseals bases and reclaims all eligible trash.
    pub fn recover_workspaces(&mut self) -> Result<WorkspaceRecovery> {
        let store = Store::lock(self)?;
        store.recover(self, None)
    }

    fn store_dir(&self) -> Result<PathBuf> {
        self.conn
            .path()
            .and_then(|db| Path::new(db).parent())
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::CorruptRepository("repository database has no path".into()))
    }

    // Direct reads: the command already validated full state once through layer_state.
    fn world_head(&self) -> Result<VersionNumber> {
        let head: i64 = self
            .conn
            .query_row("SELECT max(number) FROM versions", [], |r| r.get(0))?;
        VersionNumber::decode(head)
    }

    /// Layers with a terminal discard event, in one query.
    fn discarded_layers(&self) -> Result<HashSet<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT layer FROM layer_heads WHERE discarded=1")?;
        let layers = statement
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(layers)
    }

    fn ensure_base(&self, store: &Store, version: VersionNumber) -> Result<(PathBuf, BaseReport)> {
        let target = store.bases().join(version.to_string());
        let mut report = BaseReport {
            version,
            source: BaseSource::Cached,
            derived_from: None,
            changed_paths: 0,
            written_bytes: 0,
            clone: None,
            elapsed_ms: 0,
        };
        if target.try_exists()? {
            return Ok((target, report));
        }
        let started = Instant::now();
        let nearest = store
            .bases_cached()?
            .into_iter()
            .min_by_key(|(v, _)| v.get().abs_diff(version.get()));
        let staging = store.staging(&format!("base~{version}"));
        let built = (|| {
            let (changed, written) = match &nearest {
                Some((from, path)) => {
                    // The tree diff skips shared subtrees: O(change), not O(repo).
                    let index = Index::new(&self.conn);
                    let changes = index.trees.diff(index.root(*from)?, index.root(version)?)?;
                    report.clone = Some(unsealed(path, || clone::clone_tree(path, &staging))?);
                    open_root(&staging)?;
                    report.source = BaseSource::Derived;
                    report.derived_from = Some(*from);
                    let changes = changes.entries().iter().map(|(p, e)| (p, e.as_ref()));
                    self.apply(&staging, &At(index, *from), changes)?
                }
                None => {
                    let manifest = self.manifest_at(version)?;
                    clone::create_base_dir(&staging)?;
                    report.source = BaseSource::Materialized;
                    let entries = manifest.entries().iter();
                    self.apply(&staging, &Nothing, entries.map(|(p, e)| (p, Some(e))))?
                }
            };
            clone::barrier(&staging)?;
            // Publish first: a sealed directory cannot be renamed to a new parent.
            fs::rename(&staging, &target)?;
            seal(&target)?;
            sync_dir(&store.bases())?;
            Ok::<_, Error>((changed, written))
        })();
        let (changed, written) = built.inspect_err(|_| store.abandon(&staging))?;
        report.changed_paths = changed;
        report.written_bytes = written;
        report.elapsed_ms = millis(started);
        Ok((target, report))
    }

    /// Applies exact path changes relative to `before` under `root`. Removals go first;
    /// every new name must be created fresh, so a volume that folds case or Unicode
    /// normalization reports the collision instead of silently merging two paths.
    fn apply<'a>(
        &self,
        root: &Path,
        before: &impl Before,
        changes: impl Iterator<Item = (&'a RepoPath, Option<&'a FileEntry>)>,
    ) -> Result<(usize, u64)> {
        let changes: Vec<_> = changes.collect();
        let mut removed = HashSet::new();
        for (path, _) in &changes {
            if !before.file(path)? {
                continue;
            }
            remove_entry(&native(root, path.as_str()))?;
            let mut dir = path.as_str();
            while let Some((parent, _)) = dir.rsplit_once('/') {
                match fs::remove_dir(native(root, parent)) {
                    Ok(()) => {
                        removed.insert(parent.to_owned());
                    }
                    Err(_) => break,
                }
                dir = parent;
            }
        }
        let mut dirs = HashSet::new();
        let mut written = 0;
        for (path, entry) in &changes {
            let Some(entry) = entry else { continue };
            representable(path)?;
            for (index, _) in path.as_str().match_indices('/') {
                let dir = &path.as_str()[..index];
                if dirs.contains(dir) || (!removed.contains(dir) && before.dir(dir)?) {
                    continue;
                }
                match fs::create_dir(native(root, dir)) {
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        return Err(collision(dir));
                    }
                    other => other?,
                }
                dirs.insert(dir.to_owned());
            }
            written += self.write_entry(&native(root, path.as_str()), entry, path)?;
        }
        Ok((changes.len(), written))
    }

    fn write_entry(&self, path: &Path, entry: &FileEntry, name: &RepoPath) -> Result<u64> {
        let created = |e: io::Error| match e.kind() {
            io::ErrorKind::AlreadyExists => collision(name.as_str()),
            _ => e.into(),
        };
        match entry {
            FileEntry::Regular {
                content,
                executable,
            } => {
                use std::io::Write;
                let bytes = self.read_blob(*content)?;
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(
                    &mut options,
                    if *executable { 0o777 } else { 0o666 },
                );
                #[cfg(not(unix))]
                let _ = executable;
                let mut file = options.open(path).map_err(created)?;
                file.write_all(&bytes)?;
                if clone::SYNC_EACH_FILE {
                    flush(&file)?;
                }
                Ok(bytes.len() as u64)
            }
            FileEntry::Symlink { target } => {
                let target = self.read_blob(*target)?;
                #[cfg(unix)]
                let target = PathBuf::from(
                    <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&target),
                );
                #[cfg(not(unix))]
                let target = PathBuf::from(String::from_utf8(target).map_err(|_| {
                    Error::Unsupported(format!(
                        "symlink {:?} has a non-UTF-8 target, which Windows cannot store",
                        name.as_str()
                    ))
                })?);
                symlink(&target, path).map_err(|e| match e.kind() {
                    io::ErrorKind::AlreadyExists => collision(name.as_str()),
                    io::ErrorKind::PermissionDenied if cfg!(windows) => {
                        Error::Unsupported(format!(
                            "symlink {:?} needs Windows Developer Mode or the symlink privilege",
                            name.as_str()
                        ))
                    }
                    _ => e.into(),
                })?;
                Ok(0)
            }
        }
    }
}

/// What a directory held before changes are applied to it.
trait Before {
    fn file(&self, path: &RepoPath) -> Result<bool>;
    fn dir(&self, dir: &str) -> Result<bool>;
}

/// A new, empty directory.
struct Nothing;

impl Before for Nothing {
    fn file(&self, _: &RepoPath) -> Result<bool> {
        Ok(false)
    }
    fn dir(&self, _: &str) -> Result<bool> {
        Ok(false)
    }
}

/// A World snapshot, read through its tree: O(depth) per lookup.
struct At<'a>(Index<'a>, VersionNumber);

impl Before for At<'_> {
    fn file(&self, path: &RepoPath) -> Result<bool> {
        Ok(self.0.entry_at(path, self.1)?.is_some())
    }
    fn dir(&self, dir: &str) -> Result<bool> {
        self.0.is_dir(&RepoPath(dir.into()), self.1)
    }
}

fn collision(path: &str) -> Error {
    Error::Unsupported(format!(
        "path {path:?} collides with another World path on this volume, which folds case \
         or Unicode normalization; this World cannot be materialized here"
    ))
}

/// Windows cannot create reserved device names, trailing dots or spaces, or `<>:"|?*`.
fn representable(path: &RepoPath) -> Result<()> {
    if cfg!(windows)
        && let Some(name) = windows_unrepresentable(path.as_str())
    {
        return Err(Error::Unsupported(format!(
            "path {:?} has the component {name:?}, which Windows cannot represent; \
             this World cannot be materialized here",
            path.as_str()
        )));
    }
    Ok(())
}

/// The first path component Win32 cannot create, if any.
pub(crate) fn windows_unrepresentable(path: &str) -> Option<&str> {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    path.split('/').find(|name| {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ');
        let device = DEVICES.iter().any(|d| stem.eq_ignore_ascii_case(d))
            || ["COM", "LPT"].iter().any(|d| {
                stem.len() > 3
                    && stem.is_char_boundary(3)
                    && stem[..3].eq_ignore_ascii_case(d)
                    && [
                        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³",
                    ]
                    .contains(&&stem[3..])
            });
        device
            || name.ends_with(['.', ' '])
            || name.contains(['<', '>', '"', '|', '?', '*'])
            || name.chars().any(|c| (c as u32) < 0x20)
    })
}

fn native(root: &Path, path: &str) -> PathBuf {
    path.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

#[cfg(unix)]
pub(crate) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
pub(crate) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        // A Windows directory symlink is removed as a directory.
        Err(e) if cfg!(windows) && path.is_symlink() => fs::remove_dir(path).map_err(|_| e),
        other => other,
    }
}

/// Plain `fsync` on Apple platforms: `sync_all` there issues the much slower `F_FULLFSYNC`.
#[cfg(target_vendor = "apple")]
fn flush(file: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_vendor = "apple"))]
fn flush(file: &fs::File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// Directory durability needs platform verification off Unix, as in `repository.rs`.
#[cfg(not(unix))]
fn sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Seals a base root so ordinary tools cannot open or change anything inside it.
fn seal(root: &Path) -> io::Result<()> {
    set_mode(root, 0o000)
}

/// Opens a cloned or new root for its owner and everyone's reads.
fn open_root(root: &Path) -> io::Result<()> {
    set_mode(root, 0o755)
}

/// Runs `f` with the base root readable by its owner, resealing it afterwards.
fn unsealed<T>(root: &Path, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    set_mode(root, 0o700)?;
    let result = f();
    seal(root)?;
    result
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Windows has no directory mode to seal; base protection there is by convention only.
#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// The locked store directory. Every mutation happens while this value lives.
struct Store {
    dir: PathBuf,
    _lock: fs::File,
}

impl Store {
    fn lock(repo: &Repository) -> Result<Self> {
        let dir = repo.store_dir()?;
        for sub in ["bases", "layers", "tmp", "trash"] {
            match fs::create_dir(dir.join(sub)) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                other => other?,
            }
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("workspace.lock"))?;
        lock.lock()?;
        Ok(Self { dir, _lock: lock })
    }

    fn bases(&self) -> PathBuf {
        self.dir.join("bases")
    }
    fn layers(&self) -> PathBuf {
        self.dir.join("layers")
    }

    fn staging(&self, name: &str) -> PathBuf {
        self.dir.join("tmp").join(format!("{name}~{}", nonce()))
    }

    /// Atomically renames `path` into trash under a name recording what it was.
    fn trash(&self, path: &Path, kind: &str) -> io::Result<()> {
        // Moving a directory to a new parent needs write permission on it.
        set_mode(path, 0o700)?;
        fs::rename(
            path,
            self.dir.join("trash").join(format!("{kind}~{}", nonce())),
        )?;
        sync_dir(&self.dir.join("trash"))
    }

    /// Best effort after a failed build; recovery finishes it otherwise.
    fn abandon(&self, staging: &Path) {
        if staging.exists() {
            let _ = self.trash(staging, "tmp");
        }
    }

    fn bases_cached(&self) -> io::Result<Vec<(VersionNumber, PathBuf)>> {
        let mut bases = Vec::new();
        for entry in fs::read_dir(self.bases())? {
            let entry = entry?;
            let version = entry.file_name().to_str().and_then(|n| n.parse().ok());
            if let Some(version) = version.and_then(|v| VersionNumber::new(v).ok()) {
                bases.push((version, entry.path()));
            }
        }
        bases.sort_by_key(|(v, _)| std::cmp::Reverse(*v));
        Ok(bases)
    }

    /// Keeps pinned bases and the newest others, up to the cache bound.
    fn evict_bases(&self, pinned: &[VersionNumber]) -> io::Result<()> {
        let pinned: std::collections::BTreeSet<_> = pinned.iter().copied().collect();
        let mut room = BASE_CACHE.saturating_sub(pinned.len());
        for (version, path) in self.bases_cached()? {
            if pinned.contains(&version) {
                continue;
            }
            if room > 0 {
                room -= 1;
                continue;
            }
            self.trash(&path, &format!("base~{version}"))?;
        }
        Ok(())
    }

    /// Finishes interrupted steps and reclaims up to `budget` trash entries (or all).
    fn recover(&self, repo: &Repository, budget: Option<usize>) -> Result<WorkspaceRecovery> {
        let mut report = WorkspaceRecovery::default();
        let discarded = repo.discarded_layers()?;
        let terminal = |layer: &LayerId| discarded.contains(layer.as_str());
        for entry in fs::read_dir(self.dir.join("tmp"))? {
            self.trash(&entry?.path(), "tmp")?;
            report.trashed += 1;
        }
        for entry in fs::read_dir(self.layers())? {
            let entry = entry?;
            if let Some(layer) = layer_named(&entry.file_name())
                && terminal(&layer)
            {
                self.trash(&entry.path(), &format!("layer~{}", layer.as_str()))?;
                report.trashed += 1;
            }
        }
        for entry in fs::read_dir(self.bases())? {
            let entry = entry?;
            let name = entry.file_name();
            match name.to_str().and_then(|n| n.parse::<u64>().ok()) {
                // A crash between publishing and sealing leaves a base open.
                Some(_) => seal(&entry.path())?,
                None => {
                    self.trash(&entry.path(), "tmp")?;
                    report.trashed += 1;
                }
            }
        }
        let mut budget = budget.unwrap_or(usize::MAX);
        let mut entries: Vec<_> =
            fs::read_dir(self.dir.join("trash"))?.collect::<io::Result<_>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let held = name
                .strip_prefix("layer~")
                .and_then(|rest| rest.rsplit_once('~'))
                .and_then(|(id, _)| LayerId::parse(id).ok())
                .is_some_and(|layer| !terminal(&layer));
            if held {
                report.retained.push(name);
                continue;
            }
            let before = budget;
            let done = reclaim(&entry.path(), &mut budget)?;
            report.reclaimed += before - budget;
            if !done {
                report.remaining += 1;
            }
        }
        Ok(report)
    }
}

fn layer_named(name: &std::ffi::OsStr) -> Option<LayerId> {
    LayerId::parse(name.to_str()?).ok()
}

/// Deletes `path` depth-first, one entry per budget unit. Returns false when the
/// budget ran out first; the next call continues where this one stopped.
fn reclaim(path: &Path, budget: &mut usize) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        other => other?,
    };
    if *budget == 0 {
        return Ok(false);
    }
    if meta.is_dir() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o700 != 0o700 {
                set_mode(path, meta.permissions().mode() | 0o700)?;
            }
        }
        for entry in fs::read_dir(path)? {
            if !reclaim(&entry?.path(), budget)? {
                return Ok(false);
            }
        }
        if *budget == 0 {
            return Ok(false);
        }
        fs::remove_dir(path)?;
    } else {
        match remove_entry(path) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && cfg!(windows) => {
                // Windows refuses to delete read-only files.
                let mut permissions = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                fs::set_permissions(path, permissions)?;
                remove_entry(path)?;
            }
            other => other?,
        }
    }
    *budget -= 1;
    Ok(true)
}

fn nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{time:x}-{:x}-{:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::windows_unrepresentable as bad;

    #[test]
    fn windows_names() {
        for path in [
            "CON",
            "a/con.txt",
            "nul",
            "Aux.tar.gz",
            "COM1",
            "lpt9.x",
            "COM¹",
            "x/a.",
            "b ",
            "q?",
            "a<b",
            "x\u{1}",
        ] {
            assert!(bad(path).is_some(), "{path}");
        }
        for path in ["CONSOLE", "com", "COM10", "a.b", "src/main.rs", "Foo", "é"] {
            assert_eq!(bad(path), None, "{path}");
        }
    }
}
