//! Clone-backed Layer workspaces: each Layer is a plain directory cloned from an
//! immutable, materialized World base on the repository's own volume.
//!
//! Store layout under `.javelin/`:
//! - `workspace.lock` serializes workspace changes between processes.
//! - `bases/rolling` is the repository's one base, sealed. It holds the World recorded
//!   in `base.json` and moves forward in place, under the lock, by applying changesets.
//!   A Layer at a newer World advances it first. A Layer at an older World clones it and
//!   applies the inverse changeset to the clone, O(distance). Unix seals it with root
//!   mode 000, so ordinary tools cannot open it, and briefly reopens it under the lock.
//! - `base.json` records the rolling base's World. An advance writes `advancing` first.
//!   A crash or failure mid-advance drops the base, and the next Layer rebuilds it.
//! - `layers/<id>` is the Layer's writable directory.
//! - `tmp/` holds staging. Anything still there at recovery is abandoned work.
//! - `trash/` holds renamed-away Layers, bases and staging, reclaimed in bounded steps.
//!
//! The database decides first and the directories follow: discard and Publish record the
//! terminal event, then rename the directory into trash. Recovery finishes any step a
//! crash interrupted. Only trash for terminal Layers, dropped bases and abandoned staging
//! is ever deleted.
mod clone;
mod refresh;

use std::{
    collections::HashSet,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub use clone::{CloneReport, CloneStrategy};
pub use refresh::WorkspaceRefresh;

use crate::{
    CheckpointId, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, OperationId,
    PublicationReceipt, PublishAttribution, PublishOutcome, RepoPath, Repository, Result,
    SparseChanges, VersionNumber, WorldVersion, domain::WorldIndex, repository::Index,
};

/// Filesystem entries each ordinary command may delete from trash.
const RECLAIM_BUDGET: usize = 4096;
/// The repository's one materialized base, under `bases/`.
const ROLLING: &str = "rolling";

/// Where a Layer's directory and its base live. Paths exist only once opened.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayerWorkspace {
    pub layer: LayerId,
    /// The Layer's writable directory, `.javelin/layers/<id>`.
    pub root: PathBuf,
    pub exists: bool,
    pub base_version: VersionNumber,
    /// The repository's rolling base, `.javelin/bases/rolling`. It holds the World that
    /// `base.json` records, which is not necessarily this Layer's World. It is sealed on
    /// Unix, so capture should compare the Layer with its checkpoint and stat cache.
    pub base_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseSource {
    /// The rolling base already held this World.
    Cached,
    /// The rolling base was ahead of this World: the Layer's clone was rewound to it by
    /// the changeset between the two, and the rolling base was left alone.
    Derived,
    /// The rolling base was behind this World and moved forward in place, by the
    /// changeset between the two.
    Advanced,
    /// Written from stored objects (cold): the rolling base was absent or partial.
    Materialized,
}

/// How the rolling base was brought to a World. `elapsed_ms` is that cost alone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BaseReport {
    /// The World the Layer was opened at.
    pub version: VersionNumber,
    pub source: BaseSource,
    /// The World the rolling base held before an advance or rewind.
    pub derived_from: Option<VersionNumber>,
    /// Paths written or removed on top of the base, or all paths when materialized.
    pub changed_paths: usize,
    pub written_bytes: u64,
    /// The clone of the rolling base, when a Layer's clone was rewound.
    pub clone: Option<CloneReport>,
    pub elapsed_ms: u64,
}

/// The rolling base's record in `base.json`. `advancing` names the target of an advance
/// that has not finished, so a base found with it set is partial and must be dropped.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct RollingState {
    version: u64,
    advancing: Option<u64>,
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
            base_root: store.join("bases").join(ROLLING),
        })
    }

    /// Materializes an active Layer's directory by cloning the rolling base and applying
    /// its checkpoint. Idempotent: an existing directory is returned untouched.
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
        let started = Instant::now();
        let (rolling_at, mut base) = self.ensure_rolling(&store, version)?;
        let rolling = store.rolling();
        let staging = store.staging(&format!("layer~{}", layer.as_str()));
        let built = (|| {
            let clone = unsealed(&rolling, || clone::clone_tree(&rolling, &staging))?;
            open_root(&staging)?;
            if rolling_at > version {
                // The rolling base is ahead of this Layer: rewind the clone to its World.
                let index = Index::new(&self.conn);
                let changes = index
                    .trees
                    .diff(index.root(rolling_at)?, index.root(version)?)?;
                let changes = changes.entries().iter().map(|(p, e)| (p, e.as_ref()));
                let (changed, written) = self.apply(&staging, &At(index, rolling_at), changes)?;
                base.source = BaseSource::Derived;
                base.derived_from = Some(rolling_at);
                base.changed_paths = changed;
                base.written_bytes = written;
                base.clone = Some(clone.clone());
            }
            let changes = checkpoint.changes().entries();
            let (applied, _) = if changes.is_empty() {
                (0, 0)
            } else {
                let before = At(Index::new(&self.conn), version);
                let changes = changes.iter().map(|(p, e)| (p, e.as_ref()));
                self.apply(&staging, &before, changes)?
            };
            clone::barrier(&staging)?;
            fs::rename(&staging, &root)?;
            sync_dir(&store.layers())?;
            self.write_sync(layer, head.checkpoint())?;
            Ok::<_, Error>((clone, applied))
        })();
        let (clone, applied) = built.inspect_err(|_| store.abandon(&staging))?;
        report.created = true;
        report.base = Some(base);
        report.clone = Some(clone);
        report.applied_changes = applied;
        report.elapsed_ms = millis(started);
        Ok(report)
    }

    /// Publishes the Layer and reclaims its directory under the same lock. A crash after
    /// the publish commits leaves the directory for the next command, because recovery
    /// treats a published Layer as terminal. A retry replays the receipt and finishes it.
    pub fn publish_workspace<F>(
        &mut self,
        operation: &OperationId,
        expected: &LayerHead,
        attribution: &PublishAttribution,
        guard: F,
    ) -> Result<PublicationReceipt>
    where
        F: FnOnce(&WorldVersion, &SparseChanges) -> Result<()>,
    {
        let store = Store::lock(self)?;
        let receipt = self.publish_layer(operation, expected, attribution, guard)?;
        if matches!(receipt.outcome, PublishOutcome::Published { .. }) {
            let layer = expected.layer();
            let root = store.layers().join(layer.as_str());
            if root.try_exists()? {
                store.trash(&root, &format!("layer~{}", layer.as_str()))?;
            }
            store.recover(self, Some(RECLAIM_BUDGET))?;
        }
        Ok(receipt)
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

    /// Layers whose directory is reclaimable: a terminal discard, or a successful Publish.
    fn terminal_layers(&self) -> Result<HashSet<String>> {
        let mut statement = self.conn.prepare(
            "SELECT layer FROM layer_heads WHERE discarded=1 \
             UNION SELECT layer FROM publications WHERE outcome=1",
        )?;
        let layers = statement
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(layers)
    }

    /// Brings the rolling base to `version` under the lock and returns the World it holds
    /// afterwards. It advances in place when behind, is rebuilt cold when absent or partial,
    /// and is left alone when ahead; the caller then rewinds the Layer's own clone.
    fn ensure_rolling(
        &self,
        store: &Store,
        version: VersionNumber,
    ) -> Result<(VersionNumber, BaseReport)> {
        let mut report = BaseReport {
            version,
            source: BaseSource::Cached,
            derived_from: None,
            changed_paths: 0,
            written_bytes: 0,
            clone: None,
            elapsed_ms: 0,
        };
        let started = Instant::now();
        let held = match store.rolling_state()? {
            Some(RollingState {
                version: held,
                advancing: None,
            }) if store.rolling().try_exists()? => Some(VersionNumber::new(held)?),
            _ => None,
        };
        let at = match held {
            Some(at) if at >= version => at,
            Some(at) => {
                let (changed, written) = self.advance_rolling(store, at, version)?;
                report.source = BaseSource::Advanced;
                report.derived_from = Some(at);
                report.changed_paths = changed;
                report.written_bytes = written;
                version
            }
            None => {
                let (changed, written) = self.materialize_rolling(store, version)?;
                report.source = BaseSource::Materialized;
                report.changed_paths = changed;
                report.written_bytes = written;
                version
            }
        };
        report.elapsed_ms = millis(started);
        Ok((at, report))
    }

    /// Moves the rolling base forward from `from` to `to`, in place. The record names the
    /// target before the first write, so a crash leaves a partial base that is dropped.
    fn advance_rolling(
        &self,
        store: &Store,
        from: VersionNumber,
        to: VersionNumber,
    ) -> Result<(usize, u64)> {
        let target = store.rolling();
        store.write_rolling_state(RollingState {
            version: from.get(),
            advancing: Some(to.get()),
        })?;
        let applied = unsealed(&target, || -> Result<(usize, u64)> {
            // The tree diff skips shared subtrees: O(change), not O(repo).
            let index = Index::new(&self.conn);
            let changes = index.trees.diff(index.root(from)?, index.root(to)?)?;
            let changes = changes.entries().iter().map(|(p, e)| (p, e.as_ref()));
            let applied = self.apply(&target, &At(index, from), changes)?;
            clone::barrier(&target)?;
            Ok(applied)
        });
        match applied {
            Ok(applied) => {
                store.write_rolling_state(RollingState {
                    version: to.get(),
                    advancing: None,
                })?;
                Ok(applied)
            }
            Err(error) => {
                // Best effort: if this also fails, the partial record makes the next command drop it.
                let _ = store.drop_rolling();
                Err(error)
            }
        }
    }

    /// Replaces the rolling base with a cold materialization of `version`.
    fn materialize_rolling(&self, store: &Store, version: VersionNumber) -> Result<(usize, u64)> {
        store.drop_rolling()?;
        let target = store.rolling();
        let staging = store.staging("rolling");
        let built = (|| {
            let manifest = self.manifest_at(version)?;
            clone::create_base_dir(&staging)?;
            let entries = manifest.entries().iter();
            let counts = self.apply(&staging, &Nothing, entries.map(|(p, e)| (p, Some(e))))?;
            clone::barrier(&staging)?;
            // Publish first: a sealed directory cannot be renamed to a new parent.
            fs::rename(&staging, &target)?;
            seal(&target)?;
            store.write_rolling_state(RollingState {
                version: version.get(),
                advancing: None,
            })?;
            sync_dir(&store.bases())?;
            Ok::<_, Error>(counts)
        })();
        built.inspect_err(|_| store.abandon(&staging))
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
fn unsealed<T, E: From<io::Error>>(
    root: &Path,
    f: impl FnOnce() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
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
        // Moving a directory to a new parent needs write permission on it. A symlink is moved
        // as a link, and chmod would change its target outside the store.
        if !path.is_symlink() {
            set_mode(path, 0o700)?;
        }
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

    fn rolling(&self) -> PathBuf {
        self.bases().join(ROLLING)
    }

    /// The rolling base's record. A missing or unreadable record means no usable base.
    fn rolling_state(&self) -> io::Result<Option<RollingState>> {
        match fs::read(self.dir.join("base.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Replaces the record atomically and makes it durable before returning.
    fn write_rolling_state(&self, state: RollingState) -> io::Result<()> {
        let tmp = self.dir.join("base.json.tmp");
        let mut file = fs::File::create(&tmp)?;
        let bytes = serde_json::to_vec(&state).map_err(io::Error::other)?;
        file.write_all(&bytes)?;
        flush(&file)?;
        fs::rename(&tmp, self.dir.join("base.json"))?;
        sync_dir(&self.dir)
    }

    /// Trashes the rolling base and then its record. A crash between the two leaves a
    /// record without a directory, which reads as no usable base.
    fn drop_rolling(&self) -> io::Result<()> {
        let rolling = self.rolling();
        if rolling.try_exists()? {
            self.trash(&rolling, "base~rolling")?;
        }
        match fs::remove_file(self.dir.join("base.json")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Finishes interrupted steps and reclaims up to `budget` trash entries (or all).
    fn recover(&self, repo: &Repository, budget: Option<usize>) -> Result<WorkspaceRecovery> {
        let mut report = WorkspaceRecovery::default();
        let terminal_layers = repo.terminal_layers()?;
        let terminal = |layer: &LayerId| terminal_layers.contains(layer.as_str());
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
        // A crash mid-advance leaves the rolling base partial: drop it, and the next
        // create rebuilds it cold.
        if let Some(RollingState {
            advancing: Some(_), ..
        }) = self.rolling_state()?
        {
            self.drop_rolling()?;
            report.trashed += 1;
        }
        for entry in fs::read_dir(self.bases())? {
            let entry = entry?;
            if entry.file_name() == ROLLING {
                // A crash between publishing and sealing leaves the base open.
                seal(&entry.path())?;
            } else {
                // Per-version bases from before the rolling base.
                self.trash(&entry.path(), "base~legacy")?;
                report.trashed += 1;
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
    use std::fs;

    use super::windows_unrepresentable as bad;

    #[cfg(unix)]
    #[test]
    fn abandoning_a_staged_symlink_leaves_its_target_alone() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::TempDir::new().unwrap();
        let dir = scratch.path().join("store");
        fs::create_dir_all(dir.join("tmp")).unwrap();
        fs::create_dir_all(dir.join("trash")).unwrap();
        let outside = scratch.path().join("outside");
        fs::write(&outside, b"x").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.join("tmp").join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let store = super::Store {
            dir: dir.clone(),
            _lock: fs::File::create(dir.join("lock")).unwrap(),
        };

        store.abandon(&link);

        let mode = fs::metadata(&outside).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the link's target must keep its mode");
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "the link must be trashed"
        );
    }

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
