//! Linux binding of native Layer state to ordinary paths through sparse OverlayFS views.
//!
//! The ordinary-user library owns graph calls, hashing, binding records and the
//! runtime lock. A root-owned helper, run through `sudo -n`, performs only bounded
//! layout and mount operations. Capture freezes one view's overlay superblock
//! read-only; the kernel refuses with `EBUSY` while any file is open for writing.
//! Other platforms compile this module but every runtime entry point refuses.
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    fs,
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    CheckpointId, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest,
    ObjectId, OperationId, RepoPath, Repository, Result, SparseChanges, domain::ObjectKind,
};

const RECORD_FORMAT: u32 = 1;

fn ensure_linux() -> Result<()> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "Layer views require Linux OverlayFS".into(),
        ))
    }
}

fn trusted(path: &Path, executable: bool) -> Result<()> {
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        return Err(Error::InvalidInput(format!(
            "{} must be an absolute canonical path",
            path.display()
        )));
    }
    for ancestor in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(ancestor)?;
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(Error::InvalidInput(format!(
                "{} must be root-owned and not group/other writable",
                ancestor.display()
            )));
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(Error::InvalidInput(format!(
            "{} must be a root-owned file not writable by group or others",
            path.display()
        )));
    }
    if executable && meta.mode() & 0o111 == 0 {
        return Err(Error::InvalidInput(format!(
            "{} is not executable",
            path.display()
        )));
    }
    Ok(())
}

/// A root-owned helper executable, invoked non-interactively through `/usr/bin/sudo`.
/// The production installation and privilege model remain follow-on work.
#[derive(Clone, Debug)]
pub struct LayerMountHelper {
    sudo: PathBuf,
    helper: PathBuf,
}

impl LayerMountHelper {
    pub fn sudo(helper: impl AsRef<Path>) -> Result<Self> {
        let sudo = PathBuf::from("/usr/bin/sudo");
        trusted(&sudo, true)?;
        trusted(helper.as_ref(), true)?;
        Ok(Self {
            sudo,
            helper: helper.as_ref().into(),
        })
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.sudo)
            .arg("-n")
            .arg(&self.helper)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        match output.status.code() {
            Some(0) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
            Some(3) => Err(Error::Busy(message)),
            Some(2) => Err(Error::InvalidInput(message)),
            _ => Err(Error::Io(std::io::Error::other(format!(
                "mount helper {} failed: {message}",
                args.first().unwrap_or(&"")
            )))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Capture,
    Discard,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    operation: String,
    action: Action,
}

/// Durable binding of one Layer to its view. `opened` and `lowers` stay fixed while
/// the private upper exists, so reopening never overwrites uncheckpointed edits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewRecord {
    format: u32,
    layer: String,
    opened: String,
    lowers: Vec<String>,
    revision: u64,
    head: String,
    pending: Option<Pending>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeRecord {
    format: u32,
    repository: ObjectId,
}

#[derive(Debug, Serialize)]
pub struct CaptureOutcome {
    pub update: LayerUpdate,
    /// Paths in the complete delta relative to the exact base World.
    pub changed_paths: usize,
    /// Empty directories exist only on disk; native state cannot represent them.
    pub unrepresented_directories: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct RecoveryReport {
    pub stages_aborted: usize,
    pub pending_resolved: usize,
    pub views_removed: usize,
}

#[derive(Debug, Serialize)]
pub struct ViewStatus {
    pub path: PathBuf,
    pub mounted: bool,
    pub frozen: bool,
    pub bound_revision: u64,
    pub bound_checkpoint: String,
    pub pending: Option<String>,
    pub current: LayerState,
}

/// Opaque, case-sensitive Layer IDs never become host path components.
pub(crate) fn slot(layer: &LayerId) -> String {
    let mut hash = Sha256::new();
    hash.update(b"javelin-linux-view-v1\0");
    hash.update(layer.as_str().as_bytes());
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn mount_rows() -> Result<Vec<(PathBuf, bool)>> {
    Ok(fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let target = fields.nth(4)?;
            let readonly = fields.next()?.split(',').any(|o| o == "ro");
            // Runtime paths are restricted to bytes that mountinfo never escapes.
            Some((PathBuf::from(target), readonly))
        })
        .collect())
}

/// `Some(readonly)` for the topmost mount exactly at `target`.
fn mount_state(target: &Path) -> Result<Option<bool>> {
    Ok(mount_rows()?
        .into_iter()
        .rev()
        .find(|(path, _)| path == target)
        .map(|(_, readonly)| readonly))
}

pub(crate) struct Snapshot {
    pub(crate) entries: BTreeMap<RepoPath, FileEntry>,
    pub(crate) unrepresented: Vec<String>,
}

fn unsupported(relative: &Path, what: &str) -> Error {
    Error::Unsupported(format!("{what} at {:?}", relative))
}

/// Reads a frozen view. Paths from the private upper are read through the view;
/// every other path must be the opened checkpoint's exact entry. Symlinks are read
/// with `readlink` and never followed. Absent base paths become complete deletions.
pub(crate) fn snapshot(
    root: &Path,
    upper: &HashSet<Vec<u8>>,
    lower: &Manifest,
    store: &mut dyn FnMut(&[u8]) -> Result<ObjectId>,
) -> Result<Snapshot> {
    let mut entries = BTreeMap::new();
    let mut unrepresented = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(directory) = pending.pop() {
        let mut names = fs::read_dir(root.join(&directory))?
            .map(|e| Ok(e?.file_name()))
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        if names.is_empty() && !directory.as_os_str().is_empty() {
            unrepresented.push(directory.to_string_lossy().into_owned());
        }
        for name in names {
            let relative = directory.join(&name);
            let text = relative
                .to_str()
                .ok_or_else(|| unsupported(&relative, "non-UTF-8 name"))?;
            let path = RepoPath::parse(text)
                .map_err(|_| unsupported(&relative, "name outside the native path model"))?;
            let full = root.join(&relative);
            let meta = fs::symlink_metadata(&full)?;
            let kind = meta.file_type();
            if kind.is_dir() {
                pending.push(relative);
                continue;
            }
            if !kind.is_file() && !kind.is_symlink() {
                return Err(unsupported(&relative, "special file"));
            }
            let entry = if upper.contains(relative.as_os_str().as_bytes()) {
                if kind.is_symlink() {
                    let target = fs::read_link(&full)?;
                    FileEntry::Symlink {
                        target: store(target.as_os_str().as_bytes())?,
                    }
                } else {
                    let mut file = fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&full)?;
                    let opened = file.metadata()?;
                    if !opened.is_file() || opened.ino() != meta.ino() || opened.dev() != meta.dev()
                    {
                        return Err(unsupported(&relative, "file changed during frozen read"));
                    }
                    if opened.mode() & 0o7000 != 0 {
                        return Err(unsupported(&relative, "setuid, setgid or sticky mode"));
                    }
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes)?;
                    FileEntry::Regular {
                        content: store(&bytes)?,
                        executable: opened.mode() & 0o100 != 0,
                    }
                }
            } else {
                let entry = lower.entries().get(&path).cloned();
                let agrees = match &entry {
                    Some(FileEntry::Regular { executable, .. }) => {
                        kind.is_file() && (meta.mode() & 0o100 != 0) == *executable
                    }
                    Some(FileEntry::Symlink { .. }) => kind.is_symlink(),
                    None => false,
                };
                if !agrees {
                    return Err(Error::CorruptRepository(format!(
                        "view lower disagrees with its checkpoint at {text:?}"
                    )));
                }
                entry.unwrap()
            };
            entries.insert(path, entry);
        }
    }
    unrepresented.sort();
    Ok(Snapshot {
        entries,
        unrepresented,
    })
}

/// The complete sparse delta relative to the exact base, with per-entry deletions.
pub(crate) fn complete_changes(
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

/// Writes `target` through a staging view over `lower`: deletions, then pruning of
/// directories no longer implied by `target`, then additions with exact modes.
pub(crate) fn materialize(
    repo: &Repository,
    root: &Path,
    lower: &Manifest,
    target: &Manifest,
) -> Result<()> {
    let mut parents = Vec::new();
    for (path, entry) in lower.entries() {
        if target.entries().get(path) != Some(entry) {
            fs::remove_file(root.join(path.as_str()))?;
            let mut parent = Path::new(path.as_str()).parent();
            while let Some(directory) = parent.filter(|p| !p.as_os_str().is_empty()) {
                parents.push(directory.to_path_buf());
                parent = directory.parent();
            }
        }
    }
    parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    parents.dedup();
    for directory in parents {
        let prefix = format!("{}/", directory.display());
        let implied = target
            .entries()
            .keys()
            .any(|p| p.as_str().starts_with(&prefix));
        let full = root.join(&directory);
        if !implied && fs::read_dir(&full)?.next().is_none() {
            fs::remove_dir(&full)?;
        }
    }
    for (path, entry) in target.entries() {
        if lower.entries().get(path) == Some(entry) {
            continue;
        }
        let full = root.join(path.as_str());
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = repo.read_blob(entry.content())?;
        match entry {
            FileEntry::Regular { executable, .. } => {
                let mode = if *executable { 0o755 } else { 0o644 };
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&full)?;
                file.write_all(&bytes)?;
                file.set_permissions(fs::Permissions::from_mode(mode))?;
                file.sync_all()?;
            }
            FileEntry::Symlink { .. } => {
                std::os::unix::fs::symlink(OsStr::from_bytes(&bytes), &full)?;
            }
        }
    }
    Ok(())
}

fn parse_upper(output: &str) -> Result<HashSet<Vec<u8>>> {
    let bad = || Error::Io(std::io::Error::other("malformed helper listing"));
    let value: Value = serde_json::from_str(output).map_err(|_| bad())?;
    let mut paths = HashSet::new();
    for entry in value["entries"].as_array().ok_or_else(bad)? {
        let kind = entry["kind"].as_str().ok_or_else(bad)?;
        let hex = entry["path"].as_str().ok_or_else(bad)?;
        if hex.len() % 2 != 0 {
            return Err(bad());
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad()))
            .collect::<Result<Vec<_>>>()?;
        if kind == "file" || kind == "symlink" {
            paths.insert(bytes);
        }
    }
    Ok(paths)
}

fn hash_only(bytes: &[u8]) -> Result<ObjectId> {
    Ok(ObjectId::hash(ObjectKind::Blob, bytes))
}

/// Errors returned before native storage could commit; safe to clear a pending
/// operation and thaw. Others may be uncertain and keep the view frozen.
fn certain(error: &Error) -> bool {
    matches!(
        error,
        Error::InvalidInput(_) | Error::Unsupported(_) | Error::OperationIdReused(_)
    )
}

pub struct LinuxLayerRuntime {
    root: PathBuf,
    root_text: String,
    helper: LayerMountHelper,
    repository: ObjectId,
}

impl LinuxLayerRuntime {
    /// Creates the root-owned runtime layout through the helper, then opens it.
    pub fn init(
        repo: &Repository,
        root: impl AsRef<Path>,
        helper: LayerMountHelper,
    ) -> Result<Self> {
        ensure_linux()?;
        let text = root
            .as_ref()
            .to_str()
            .ok_or_else(|| Error::InvalidInput("runtime root must be UTF-8".into()))?;
        helper.run(&["init", text])?;
        Self::open(repo, root, helper)
    }

    /// Opens a runtime bound to exactly one repository identity.
    pub fn open(
        repo: &Repository,
        root: impl AsRef<Path>,
        helper: LayerMountHelper,
    ) -> Result<Self> {
        ensure_linux()?;
        let root = root.as_ref().to_path_buf();
        if !root.is_absolute() || fs::canonicalize(&root)? != root {
            return Err(Error::InvalidInput(
                "runtime root must be absolute and canonical".into(),
            ));
        }
        let root_text = root
            .to_str()
            .ok_or_else(|| Error::InvalidInput("runtime root must be UTF-8".into()))?
            .to_string();
        let state = fs::symlink_metadata(root.join("state"))?;
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if !state.is_dir() || state.uid() != uid || state.mode() & 0o7777 != 0o700 {
            return Err(Error::InvalidInput(
                "runtime state must be a 0700 directory owned by this user".into(),
            ));
        }
        let runtime = Self {
            root,
            root_text,
            helper,
            repository: repo.repository_identity()?,
        };
        let _lock = runtime.lock()?;
        let path = runtime.root.join("state/runtime.json");
        let expected = RuntimeRecord {
            format: RECORD_FORMAT,
            repository: runtime.repository,
        };
        match fs::read(&path) {
            Ok(bytes) => {
                let found: RuntimeRecord = serde_json::from_slice(&bytes)
                    .map_err(|_| Error::CorruptRepository("invalid runtime record".into()))?;
                if found != expected {
                    return Err(Error::InvalidInput(
                        "runtime is bound to another repository".into(),
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_atomic(&path, &serde_json::to_vec(&expected).unwrap())?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(runtime)
    }

    pub fn view_path(&self, layer: &LayerId) -> PathBuf {
        self.root.join("views").join(slot(layer))
    }

    /// Only one runtime operation runs at a time; a concurrent caller gets `busy`.
    fn lock(&self) -> Result<fs::File> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join("state/lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(fs::TryLockError::WouldBlock) => Err(Error::Busy(
                "another Layer runtime operation is in progress".into(),
            )),
            Err(fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    fn helper(&self, verb: &str, args: &[&str]) -> Result<String> {
        let mut all = vec![verb, self.root_text.as_str()];
        all.extend_from_slice(args);
        self.helper.run(&all)
    }

    fn record_path(&self, slot: &str) -> PathBuf {
        self.root.join("state").join(format!("{slot}.json"))
    }

    fn read_record(&self, slot: &str, layer: &LayerId) -> Result<Option<ViewRecord>> {
        let bytes = match fs::read(self.record_path(slot)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let record: ViewRecord = serde_json::from_slice(&bytes)
            .map_err(|_| Error::CorruptRepository("invalid view record".into()))?;
        if record.format != RECORD_FORMAT || record.layer != layer.as_str() {
            return Err(Error::CorruptRepository(
                "view record belongs to another Layer".into(),
            ));
        }
        Ok(Some(record))
    }

    fn write_record(&self, slot: &str, record: &ViewRecord) -> Result<()> {
        write_atomic(
            &self.record_path(slot),
            &serde_json::to_vec(record).unwrap(),
        )
    }

    fn remove_record(&self, slot: &str) -> Result<()> {
        fs::remove_file(self.record_path(slot))?;
        fs::File::open(self.root.join("state"))?.sync_all()?;
        Ok(())
    }

    /// Builds one read-only cache: write through a private staging view, freeze it,
    /// verify every byte/kind/mode, then seal after a non-lazy unmount.
    fn ensure_cache(
        &self,
        repo: &Repository,
        key: &str,
        lower_key: Option<&str>,
        lower: &Manifest,
        target: &Manifest,
    ) -> Result<()> {
        let mut args = vec![key];
        args.extend(lower_key);
        let staged: Value = serde_json::from_str(&self.helper("stage", &args)?)
            .map_err(|_| Error::Io(std::io::Error::other("malformed stage reply")))?;
        if staged["cached"] == Value::Bool(true) {
            return Ok(());
        }
        let stage = self.root.join("mnt/stage").join(key);
        let result = (|| {
            materialize(repo, &stage, lower, target)?;
            #[cfg(test)]
            crate::repository::test_boundary("cache_staged");
            self.helper("freeze", &["stage", key])?;
            let upper = parse_upper(&self.helper("list", &["stage", key])?)?;
            let snap = snapshot(&stage, &upper, lower, &mut hash_only)?;
            if &snap.entries != target.entries() || !snap.unrepresented.is_empty() {
                return Err(Error::CorruptRepository(
                    "staged cache differs from its immutable source".into(),
                ));
            }
            self.helper("seal", &[key]).map(|_| ())
        })();
        if result.is_err() {
            let _ = self.helper("abort", &[key]);
        }
        result
    }

    /// Returns `[c<checkpoint>, b<manifest>]`, or `[b<manifest>]` for an empty delta.
    fn ensure_caches(&self, repo: &Repository, checkpoint: CheckpointId) -> Result<Vec<String>> {
        let frozen = repo.read_checkpoint(checkpoint)?;
        let manifest = frozen.base().manifest();
        let base = repo.read_manifest(manifest)?;
        let base_key = format!("b{manifest}");
        self.ensure_cache(repo, &base_key, None, &Manifest::new([])?, &base)?;
        if frozen.changes().entries().is_empty() {
            return Ok(vec![base_key]);
        }
        let key = format!("c{}", checkpoint.object());
        self.ensure_cache(repo, &key, Some(&base_key), &base, frozen.manifest())?;
        Ok(vec![key, base_key])
    }

    fn mount_view(&self, slot: &str, record: &ViewRecord, readonly: bool) -> Result<()> {
        let mut args = vec![slot, if readonly { "ro" } else { "rw" }];
        args.extend(record.lowers.iter().map(String::as_str));
        self.helper("mount-view", &args).map(|_| ())
    }

    fn head(&self, repo: &Repository, layer: &LayerId, record: &ViewRecord) -> Result<LayerHead> {
        let history = repo.layer_history(layer)?;
        match usize::try_from(record.revision)
            .ok()
            .and_then(|r| history.into_iter().nth(r))
        {
            Some(LayerState::Active { head })
                if head.checkpoint().object().to_string() == record.head =>
            {
                Ok(head)
            }
            _ => Err(Error::CorruptRepository(
                "bound Layer head disagrees with native history".into(),
            )),
        }
    }

    /// Opens or resumes an ordinary writable path for an active Layer.
    pub fn mount(&mut self, repo: &mut Repository, layer: &LayerId) -> Result<PathBuf> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        let path = self.root.join("views").join(&slot);
        let record = match self.read_record(&slot, layer)? {
            Some(mut record) => {
                if record.pending.is_some() {
                    self.resolve(repo, layer, &slot, &mut record)?;
                    if self.read_record(&slot, layer)?.is_none() {
                        return Err(Error::InvalidInput("Layer was discarded".into()));
                    }
                }
                record
            }
            None => {
                let LayerState::Active { head } = repo.layer_state(layer)? else {
                    return Err(Error::InvalidInput("Layer is discarded".into()));
                };
                let lowers = self.ensure_caches(repo, head.checkpoint())?;
                let checkpoint = head.checkpoint().object().to_string();
                let record = ViewRecord {
                    format: RECORD_FORMAT,
                    layer: layer.as_str().into(),
                    opened: checkpoint.clone(),
                    lowers,
                    revision: head.revision(),
                    head: checkpoint,
                    pending: None,
                };
                self.write_record(&slot, &record)?;
                record
            }
        };
        if matches!(repo.layer_state(layer)?, LayerState::Discarded { .. }) {
            return Err(Error::InvalidInput(
                "Layer is discarded; recover removes its view".into(),
            ));
        }
        self.mount_view(&slot, &record, false)?;
        if mount_state(&path)? == Some(true) {
            self.helper("thaw", &["view", &slot])?;
        }
        Ok(path)
    }

    /// Unmounts without discarding; the private upper and binding remain.
    pub fn unmount(&mut self, layer: &LayerId) -> Result<()> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        self.read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        self.helper("unmount-view", &[&slot]).map(|_| ())
    }

    /// Freezes the view, captures its complete delta through the native API, and
    /// thaws. `busy` means a writer held a file open; nothing changed.
    pub fn capture(
        &mut self,
        repo: &mut Repository,
        layer: &LayerId,
        operation: &OperationId,
    ) -> Result<CaptureOutcome> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        let mut record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        let this = Pending {
            operation: operation.as_str().into(),
            action: Action::Capture,
        };
        if record.pending.as_ref().is_some_and(|p| p != &this) {
            self.resolve(repo, layer, &slot, &mut record)?;
            record = self
                .read_record(&slot, layer)?
                .ok_or_else(|| Error::InvalidInput("Layer was discarded".into()))?;
        }
        if record.pending.is_none() {
            if mount_state(&self.root.join("views").join(&slot))?.is_none() {
                return Err(Error::InvalidInput("Layer view is not mounted".into()));
            }
            self.helper("freeze", &["view", &slot])?;
            record.pending = Some(this);
            self.write_record(&slot, &record)?;
            #[cfg(test)]
            crate::repository::test_boundary("capture_frozen");
        }
        self.capture_frozen(repo, layer, &slot, &mut record, operation)
    }

    fn capture_frozen(
        &self,
        repo: &mut Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
        operation: &OperationId,
    ) -> Result<CaptureOutcome> {
        match mount_state(&self.root.join("views").join(slot))? {
            None => self.mount_view(slot, record, true)?,
            Some(false) => {
                self.helper("freeze", &["view", slot])?;
            }
            Some(true) => {}
        }
        let attempt = (|| {
            let upper = parse_upper(&self.helper("list", &["view", slot])?)?;
            let opened = repo.read_checkpoint(CheckpointId::parse(&record.opened)?)?;
            let base = repo.read_manifest(opened.base().manifest())?;
            let view = self.root.join("views").join(slot);
            let snap = snapshot(&view, &upper, opened.manifest(), &mut |b| repo.put_blob(b))?;
            let changes = complete_changes(&base, &snap.entries)?;
            let expected = self.head(repo, layer, record)?;
            Ok((changes, snap.unrepresented, expected))
        })();
        let (changes, unrepresented, expected) = match attempt {
            Ok(values) => values,
            Err(error) => {
                if certain(&error) {
                    record.pending = None;
                    self.write_record(slot, record)?;
                    self.helper("thaw", &["view", slot])?;
                }
                return Err(error);
            }
        };
        let update = match repo.capture_layer(operation, &expected, &changes) {
            Ok(update) => update,
            Err(error) => {
                if certain(&error) {
                    record.pending = None;
                    self.write_record(slot, record)?;
                    self.helper("thaw", &["view", slot])?;
                }
                return Err(error);
            }
        };
        #[cfg(test)]
        crate::repository::test_boundary("capture_committed");
        if let LayerUpdate::Applied {
            state: LayerState::Active { head },
        } = &update
        {
            record.revision = head.revision();
            record.head = head.checkpoint().object().to_string();
        }
        record.pending = None;
        self.write_record(slot, record)?;
        self.helper("thaw", &["view", slot])?;
        Ok(CaptureOutcome {
            update,
            changed_paths: changes.entries().len(),
            unrepresented_directories: unrepresented,
        })
    }

    /// Terminal native discard, then bounded removal of this view's private storage.
    /// A busy view refuses before any state changes. World and siblings are untouched.
    pub fn discard(
        &mut self,
        repo: &mut Repository,
        layer: &LayerId,
        operation: &OperationId,
    ) -> Result<LayerUpdate> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        let mut record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        let this = Pending {
            operation: operation.as_str().into(),
            action: Action::Discard,
        };
        if record.pending.as_ref().is_some_and(|p| p != &this) {
            self.resolve(repo, layer, &slot, &mut record)?;
            record = self
                .read_record(&slot, layer)?
                .ok_or_else(|| Error::InvalidInput("Layer was discarded".into()))?;
        }
        if record.pending.is_none() {
            self.helper("unmount-view", &[&slot])?;
            record.pending = Some(this);
            self.write_record(&slot, &record)?;
            #[cfg(test)]
            crate::repository::test_boundary("discard_unmounted");
        }
        self.finish_discard(repo, layer, &slot, &mut record, operation)
    }

    fn finish_discard(
        &self,
        repo: &mut Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
        operation: &OperationId,
    ) -> Result<LayerUpdate> {
        self.helper("unmount-view", &[slot])?;
        let update = self
            .head(repo, layer, record)
            .and_then(|expected| repo.discard_layer(operation, &expected));
        let update = match update {
            Ok(update) => update,
            Err(error) => {
                if certain(&error) {
                    record.pending = None;
                    self.write_record(slot, record)?;
                    self.mount_view(slot, record, false)?;
                }
                return Err(error);
            }
        };
        match &update {
            LayerUpdate::Applied { .. } => {
                self.helper("remove-view", &[slot])?;
                self.remove_record(slot)?;
            }
            LayerUpdate::Stale { .. } => {
                record.pending = None;
                self.write_record(slot, record)?;
                self.mount_view(slot, record, false)?;
            }
        }
        Ok(update)
    }

    fn resolve(
        &self,
        repo: &mut Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
    ) -> Result<()> {
        let Some(pending) = record.pending.clone() else {
            return Ok(());
        };
        let operation = OperationId::parse(&pending.operation)
            .map_err(|_| Error::CorruptRepository("invalid pending operation".into()))?;
        match pending.action {
            Action::Capture => self
                .capture_frozen(repo, layer, slot, record, &operation)
                .map(|_| ()),
            Action::Discard => self
                .finish_discard(repo, layer, slot, record, &operation)
                .map(|_| ()),
        }
    }

    /// Mounts an exact immutable checkpoint read-only, sharing the same caches.
    pub fn mount_checkpoint(
        &mut self,
        repo: &Repository,
        checkpoint: CheckpointId,
    ) -> Result<PathBuf> {
        let _lock = self.lock()?;
        let keys = self.ensure_caches(repo, checkpoint)?;
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.helper("mount-ro", &keys)?;
        Ok(self.root.join("mnt/ro").join(keys[0]))
    }

    pub fn unmount_checkpoint(
        &mut self,
        repo: &Repository,
        checkpoint: CheckpointId,
    ) -> Result<()> {
        let _lock = self.lock()?;
        let frozen = repo.read_checkpoint(checkpoint)?;
        let key = if frozen.changes().entries().is_empty() {
            format!("b{}", frozen.base().manifest())
        } else {
            format!("c{}", checkpoint.object())
        };
        self.helper("unmount-ro", &[&key]).map(|_| ())
    }

    pub fn status(&self, repo: &Repository, layer: &LayerId) -> Result<ViewStatus> {
        let slot = slot(layer);
        let record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        let path = self.root.join("views").join(&slot);
        let state = mount_state(&path)?;
        Ok(ViewStatus {
            path,
            mounted: state.is_some(),
            frozen: state == Some(true),
            bound_revision: record.revision,
            bound_checkpoint: record.head,
            pending: record.pending.map(|p| p.operation),
            current: repo.layer_state(layer)?,
        })
    }

    /// Discards interrupted cache staging, resolves pending operations with their
    /// original IDs, and removes views whose Layers are already discarded.
    pub fn recover(&mut self, repo: &mut Repository) -> Result<RecoveryReport> {
        let _lock = self.lock()?;
        let mut report = RecoveryReport::default();
        let status: Value = serde_json::from_str(&self.helper("status", &[])?)
            .map_err(|_| Error::Io(std::io::Error::other("malformed status reply")))?;
        for stage in status["stages"].as_array().into_iter().flatten() {
            if let Some(key) = stage.as_str() {
                self.helper("abort", &[key])?;
                report.stages_aborted += 1;
            }
        }
        let mut names = fs::read_dir(self.root.join("state"))?
            .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        for name in names {
            let Some(slot) = name.strip_suffix(".json") else {
                continue;
            };
            if slot == "runtime" {
                continue;
            }
            let bytes = fs::read(self.root.join("state").join(&name))?;
            let mut record: ViewRecord = serde_json::from_slice(&bytes)
                .map_err(|_| Error::CorruptRepository("invalid view record".into()))?;
            let layer = LayerId::parse(&record.layer)
                .map_err(|_| Error::CorruptRepository("invalid view record".into()))?;
            if self::slot(&layer) != slot {
                return Err(Error::CorruptRepository("view record slot mismatch".into()));
            }
            if record.pending.is_some() {
                self.resolve(repo, &layer, slot, &mut record)?;
                report.pending_resolved += 1;
            } else if matches!(repo.layer_state(&layer)?, LayerState::Discarded { .. }) {
                self.helper("unmount-view", &[slot])?;
                self.helper("remove-view", &[slot])?;
                self.remove_record(slot)?;
                report.views_removed += 1;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;
    use crate::VersionNumber;

    fn path(s: &str) -> RepoPath {
        RepoPath::parse(s).unwrap()
    }

    fn all_paths(root: &Path) -> HashSet<Vec<u8>> {
        let mut out = HashSet::new();
        let mut stack = vec![PathBuf::new()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(root.join(&dir)).unwrap() {
                let rel = dir.join(entry.unwrap().file_name());
                if fs::symlink_metadata(root.join(&rel)).unwrap().is_dir() {
                    stack.push(rel);
                } else {
                    out.insert(rel.as_os_str().as_bytes().to_vec());
                }
            }
        }
        out
    }

    fn repo() -> (TempDir, Repository) {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    fn import(repo: &mut Repository, tree: &Path) -> Manifest {
        let upper = all_paths(tree);
        let snap = snapshot(tree, &upper, &Manifest::new([]).unwrap(), &mut |b| {
            repo.put_blob(b)
        })
        .unwrap();
        Manifest::new(snap.entries).unwrap()
    }

    fn base_tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("dir/sub")).unwrap();
        fs::write(r.join("README.txt"), "base\n").unwrap();
        fs::write(r.join("bytes.bin"), [0u8, 255, 1, 2]).unwrap();
        fs::write(r.join("dir/a.txt"), "a").unwrap();
        fs::write(r.join("dir/sub/b.txt"), "b").unwrap();
        fs::write(r.join("run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(r.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("README.txt", r.join("link")).unwrap();
        dir
    }

    #[test]
    fn slots_are_unique_fixed_width_and_never_raw_ids() {
        let ids = [".", "..", "CON", "con", "A", "a", "a.b", "-"];
        let slots: HashSet<_> = ids
            .iter()
            .map(|id| slot(&LayerId::parse(id).unwrap()))
            .collect();
        assert_eq!(slots.len(), ids.len());
        for s in &slots {
            assert_eq!(s.len(), 64);
            assert!(
                s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
        assert_eq!(
            slot(&LayerId::parse("A").unwrap()),
            slot(&LayerId::parse("A").unwrap())
        );
    }

    #[test]
    fn import_preserves_kinds_modes_and_symlink_targets() {
        let (_d, mut repo) = repo();
        let tree = base_tree();
        let m = import(&mut repo, tree.path());
        assert_eq!(m.entries().len(), 6);
        assert!(matches!(
            m.entries()[&path("run.sh")],
            FileEntry::Regular {
                executable: true,
                ..
            }
        ));
        assert!(matches!(
            m.entries()[&path("README.txt")],
            FileEntry::Regular {
                executable: false,
                ..
            }
        ));
        let FileEntry::Symlink { target } = m.entries()[&path("link")] else {
            panic!("symlink kind lost")
        };
        assert_eq!(repo.read_blob(target).unwrap(), b"README.txt");
    }

    #[test]
    fn complete_delta_expands_directory_deletion_and_detects_mode_and_kind() {
        let (_d, mut repo) = repo();
        let tree = base_tree();
        let base = import(&mut repo, tree.path());
        let r = tree.path();
        fs::remove_dir_all(r.join("dir")).unwrap();
        fs::write(r.join("README.txt"), "changed\n").unwrap();
        fs::set_permissions(r.join("bytes.bin"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(r.join("link")).unwrap();
        fs::write(r.join("link"), "now a file").unwrap();
        symlink("/etc/passwd-never-followed", r.join("outside")).unwrap();
        fs::create_dir(r.join("empty")).unwrap();
        // Upper contains only what the tools touched; run.sh stays from the lower.
        let upper: HashSet<Vec<u8>> = ["README.txt", "bytes.bin", "link", "outside"]
            .iter()
            .map(|p| p.as_bytes().to_vec())
            .collect();
        let snap = snapshot(r, &upper, &base, &mut |b| repo.put_blob(b)).unwrap();
        assert_eq!(snap.unrepresented, vec!["empty".to_string()]);
        let changes = complete_changes(&base, &snap.entries).unwrap();
        let keys: Vec<_> = changes
            .entries()
            .iter()
            .map(|(p, e)| (p.as_str().to_string(), e.is_some()))
            .collect();
        assert_eq!(
            keys,
            [
                ("README.txt", true),
                ("bytes.bin", true),
                ("dir/a.txt", false),
                ("dir/sub/b.txt", false),
                ("link", true),
                ("outside", true)
            ]
            .map(|(p, e)| (p.to_string(), e))
        );
        let FileEntry::Symlink { target } = changes.entries()[&path("outside")].clone().unwrap()
        else {
            panic!("symlink followed")
        };
        assert_eq!(
            repo.read_blob(target).unwrap(),
            b"/etc/passwd-never-followed"
        );
        assert!(matches!(
            changes.entries()[&path("bytes.bin")],
            Some(FileEntry::Regular {
                executable: true,
                ..
            })
        ));
        assert!(matches!(
            changes.entries()[&path("link")],
            Some(FileEntry::Regular { .. })
        ));
        // The native API accepts the delta as complete and exact.
        let manifest = repo.put_manifest(&base).unwrap();
        let world = OperationId::parse("world").unwrap();
        repo.compare_and_advance(&world, VersionNumber::new(0).unwrap(), manifest)
            .unwrap();
        let layer = LayerId::parse("work").unwrap();
        let lb = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
        let head = repo.create_layer(&layer, &lb).unwrap();
        let op = OperationId::parse("capture").unwrap();
        let LayerUpdate::Applied {
            state: LayerState::Active { head },
        } = repo.capture_layer(&op, &head, &changes).unwrap()
        else {
            panic!("capture not applied")
        };
        let frozen = repo.read_checkpoint(head.checkpoint()).unwrap();
        assert_eq!(frozen.changes(), &changes);
        assert_eq!(frozen.manifest().entries(), &snap.entries);
    }

    #[test]
    fn unsupported_entries_refuse_capture() {
        let (_d, mut repo) = repo();
        let empty = Manifest::new([]).unwrap();
        for (name, setup) in [
            (
                "colon",
                Box::new(|r: &Path| fs::write(r.join("a:b"), "x").unwrap()) as Box<dyn Fn(&Path)>,
            ),
            (
                "reserved",
                Box::new(|r: &Path| fs::write(r.join(".javelin"), "x").unwrap()),
            ),
            (
                "setuid",
                Box::new(|r: &Path| {
                    fs::write(r.join("s"), "x").unwrap();
                    fs::set_permissions(r.join("s"), fs::Permissions::from_mode(0o4755)).unwrap();
                }),
            ),
            (
                "fifo",
                Box::new(|r: &Path| {
                    let c = std::ffi::CString::new(r.join("f").as_os_str().as_bytes()).unwrap();
                    // SAFETY: valid path string.
                    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
                }),
            ),
        ] {
            let dir = TempDir::new().unwrap();
            setup(dir.path());
            let upper = all_paths(dir.path());
            let result = snapshot(dir.path(), &upper, &empty, &mut |b| repo.put_blob(b));
            assert!(matches!(result, Err(Error::Unsupported(_))), "{name}");
        }
    }

    #[test]
    fn lower_disagreement_is_corruption_not_silent_content() {
        let (_d, mut repo) = repo();
        let tree = base_tree();
        let base = import(&mut repo, tree.path());
        fs::write(tree.path().join("unknown"), "x").unwrap();
        let result = snapshot(tree.path(), &HashSet::new(), &base, &mut |b| {
            repo.put_blob(b)
        });
        assert!(matches!(result, Err(Error::CorruptRepository(_))));
    }

    #[test]
    fn materialize_applies_exact_checkpoint_over_lower() {
        let (_d, mut repo) = repo();
        let tree = base_tree();
        let base = import(&mut repo, tree.path());
        let mut entries = base.entries().clone();
        entries.remove(&path("dir/a.txt"));
        entries.remove(&path("dir/sub/b.txt"));
        entries.remove(&path("README.txt"));
        let file = FileEntry::Regular {
            content: repo.put_blob(b"replaces a directory").unwrap(),
            executable: true,
        };
        entries.insert(path("dir"), file);
        entries.insert(
            path("README.txt/inner"),
            FileEntry::Regular {
                content: repo.put_blob(&[9, 0, 9]).unwrap(),
                executable: false,
            },
        );
        entries.insert(
            path("link"),
            FileEntry::Symlink {
                target: repo.put_blob(b"../outside").unwrap(),
            },
        );
        let target = Manifest::new(entries).unwrap();
        // Simulate the staging view: a copy of the lower that the materializer edits.
        let view = base_tree();
        materialize(&repo, view.path(), &base, &target).unwrap();
        let snap = snapshot(
            view.path(),
            &all_paths(view.path()),
            &Manifest::new([]).unwrap(),
            &mut hash_only,
        )
        .unwrap();
        assert_eq!(&snap.entries, target.entries());
        assert!(snap.unrepresented.is_empty());
        assert_eq!(
            fs::read_link(view.path().join("link")).unwrap(),
            Path::new("../outside")
        );
    }

    #[test]
    fn records_reject_unknown_fields() {
        let json = r#"{"format":1,"layer":"a","opened":"x","lowers":[],"revision":0,"head":"x","pending":null,"extra":1}"#;
        assert!(serde_json::from_str::<ViewRecord>(json).is_err());
        let parsed = parse_upper(
            r#"{"entries":[{"path":"612f62","kind":"file"},{"path":"63","kind":"whiteout"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed, HashSet::from([b"a/b".to_vec()]));
        assert!(parse_upper(r#"{"entries":[{"path":"6","kind":"file"}]}"#).is_err());
    }

    #[test]
    fn helper_and_runtime_refuse_untrusted_or_unsupported_paths() {
        let dir = TempDir::new().unwrap();
        let helper = dir.path().join("helper");
        fs::write(&helper, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let canonical = fs::canonicalize(&helper).unwrap();
        assert!(matches!(
            LayerMountHelper::sudo(&canonical),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            LayerMountHelper::sudo("relative"),
            Err(Error::InvalidInput(_)) | Err(Error::Io(_))
        ));
        if !cfg!(target_os = "linux") {
            assert!(matches!(ensure_linux(), Err(Error::Unsupported(_))));
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../tests/support/linux_layer_mounts.rs"]
mod mount_tests;
