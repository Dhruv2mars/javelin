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
    ObjectId, OperationId, PublicationReceipt, PublishAttribution, RepoPath, Repository, Result,
    SparseChanges, WorldVersion, domain::ObjectKind,
};

mod ignore;
mod watch;

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
    Publish,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    operation: String,
    action: Action,
    /// Written immediately before the native call: the exact head and delta this
    /// view submits. Recovery adopts a durable native commit under this operation
    /// ID only if it matches, so ownership never depends on re-reading the view.
    #[serde(default)]
    candidate: Option<Candidate>,
    /// Exact native publication input digest; absent before frozen-view validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    expected: u64,
    changes: String,
}

impl Pending {
    fn is(&self, operation: &OperationId, action: Action) -> bool {
        self.operation == operation.as_str() && self.action == action
    }
}

/// Deterministic digest of a complete sparse delta (sorted paths, exact entries).
fn changes_digest(changes: &SparseChanges) -> String {
    let entries: BTreeMap<&str, &Option<FileEntry>> = changes
        .entries()
        .iter()
        .map(|(path, entry)| (path.as_str(), entry))
        .collect();
    Sha256::digest(serde_json::to_vec(&entries).expect("delta encoding"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn publication_input(head: &LayerHead, attribution: &PublishAttribution) -> String {
    Sha256::digest(serde_json::to_vec(&(head, attribution)).expect("publication input encoding"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rebind: Option<Rebind>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rebind {
    expected: BoundHead,
    desired: BoundHead,
    lowers: Vec<String>,
    mounted: bool,
    phase: RebindPhase,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundHead {
    revision: u64,
    checkpoint: String,
}

impl From<&LayerHead> for BoundHead {
    fn from(head: &LayerHead) -> Self {
        Self {
            revision: head.revision(),
            checkpoint: head.checkpoint().object().to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RebindPhase {
    Validating,
    Prepared,
    Selected,
    Complete,
}

impl ViewRecord {
    fn rebinding(&self) -> bool {
        self.rebind
            .as_ref()
            .is_some_and(|r| r.phase != RebindPhase::Complete)
    }
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
    /// Replays report only native data, so this is empty for them.
    pub unrepresented_directories: Vec<String>,
    /// The outcome came from the operation's durable native record.
    pub replayed: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RecoveryReport {
    pub stages_aborted: usize,
    pub pending_resolved: usize,
    pub views_removed: usize,
    pub views_thawed: usize,
    pub views_published: usize,
    /// Views that could not be recovered; others are still processed.
    pub failures: Vec<String>,
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

#[derive(Debug, PartialEq, Eq)]
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
    read_snapshot(root, Some(upper), lower, None, store)
}

fn read_snapshot(
    root: &Path,
    upper: Option<&HashSet<Vec<u8>>>,
    lower: &Manifest,
    tracked: Option<&Manifest>,
    store: &mut dyn FnMut(&[u8]) -> Result<ObjectId>,
) -> Result<Snapshot> {
    let rules = tracked.map(|_| ignore::Rules::load(root)).transpose()?;
    let mut entries = BTreeMap::new();
    let mut unrepresented = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(directory) = pending.pop() {
        let mut names = fs::read_dir(root.join(&directory))?
            .map(|e| Ok(e?.file_name()))
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        if names.is_empty()
            && !directory.as_os_str().is_empty()
            && !rules
                .as_ref()
                .is_some_and(|r| r.ignored(&directory.to_string_lossy()))
        {
            unrepresented.push(directory.to_string_lossy().into_owned());
        }
        for name in names {
            let relative = directory.join(&name);
            let text = relative
                .to_str()
                .ok_or_else(|| unsupported(&relative, "non-UTF-8 name"))?;
            // Ignore discovery only. A tracked descendant keeps its ancestors traversable.
            if let (Some(rules), Some(tracked)) = (&rules, tracked)
                && rules.ignored(text)
                && !tracked.entries().keys().any(|p| {
                    p.as_str() == text
                        || p.as_str()
                            .strip_prefix(text)
                            .is_some_and(|s| s.starts_with('/'))
                })
            {
                continue;
            }
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
            let entry = if upper.is_none_or(|upper| upper.contains(relative.as_os_str().as_bytes()))
            {
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
                        return Err(Error::Busy(format!(
                            "file changed during read at {relative:?}"
                        )));
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

/// Parses a frozen upper listing and the helper epoch it was taken in.
fn parse_upper(output: &str) -> Result<(u64, HashSet<Vec<u8>>)> {
    let bad = || Error::Io(std::io::Error::other("malformed helper listing"));
    let value: Value = serde_json::from_str(output).map_err(|_| bad())?;
    let epoch = value["epoch"].as_u64().ok_or_else(bad)?;
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
    Ok((epoch, paths))
}

fn hash_only(bytes: &[u8]) -> Result<ObjectId> {
    Ok(ObjectId::hash(ObjectKind::Blob, bytes))
}

/// Errors returned before native storage could commit; safe to clear a pending
/// operation and thaw. Others may be uncertain and keep the view frozen.
fn certain(error: &Error) -> bool {
    matches!(
        error,
        Error::InvalidInput(_)
            | Error::Unsupported(_)
            | Error::OperationIdReused(_)
            | Error::LayerPublished { .. }
            | Error::MissingAgentTrace
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
        for entry in fs::read_dir(runtime.root.join("state"))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let Some(slot) = name.strip_suffix(".json").filter(|s| *s != "runtime") else {
                continue;
            };
            // A damaged view must not prevent opening the runtime for recover(),
            // which reports per-view failures and still processes healthy views.
            let Ok(bytes) = fs::read(runtime.record_path(slot)) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<ViewRecord>(&bytes) else {
                continue;
            };
            let Ok(layer) = LayerId::parse(&record.layer) else {
                continue;
            };
            if self::slot(&layer) != slot || record.format != RECORD_FORMAT {
                continue;
            }
            if matches!(repo.layer_state(&layer)?, LayerState::Published { .. }) {
                runtime.freeze_view(slot)?;
            }
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
            let (epoch, upper) = parse_upper(&self.helper("list", &["stage", key])?)?;
            let snap = snapshot(&stage, &upper, lower, &mut hash_only)?;
            if &snap.entries != target.entries() || !snap.unrepresented.is_empty() {
                return Err(Error::CorruptRepository(
                    "staged cache differs from its immutable source".into(),
                ));
            }
            // The helper refuses if any verb ran since the verified listing.
            self.helper("seal", &[key, &epoch.to_string()]).map(|_| ())
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

    /// Opens or resumes a path; a published Layer is mounted read-only.
    pub fn mount(&mut self, repo: &mut Repository, layer: &LayerId) -> Result<PathBuf> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        let path = self.root.join("views").join(&slot);
        let record = match self.read_record(&slot, layer)? {
            Some(mut record) => {
                if record.pending.is_some() || record.rebinding() {
                    self.resolve(repo, layer, &slot, &mut record)?;
                    if self.read_record(&slot, layer)?.is_none() {
                        return Err(Error::InvalidInput("Layer was discarded".into()));
                    }
                }
                record
            }
            None => {
                let head = match repo.layer_state(layer)? {
                    LayerState::Active { head } | LayerState::Published { head, .. } => head,
                    LayerState::Discarded { .. } => {
                        return Err(Error::InvalidInput("Layer is discarded".into()));
                    }
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
                    rebind: None,
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
        self.resume_view(repo, layer, &slot, &record, true)?;
        Ok(path)
    }

    /// Explicitly selects the current Active checkpoint after a native head advance.
    /// Both heads must be exact tokens of the same Layer. Uncaptured edits and
    /// held writers refuse without replacing the old view. An unmounted view is
    /// mounted read-only only for validation, then remains unmounted.
    ///
    /// The durable local intent is the selection boundary; subsequent native
    /// advances do not retarget it. Recovery finishes that immutable selection.
    /// A completed exact retry is a no-op, including after new private edits.
    pub fn rebind(
        &mut self,
        repo: &mut Repository,
        expected: &LayerHead,
        desired: &LayerHead,
    ) -> Result<()> {
        let _lock = self.lock()?;
        if repo.repository_identity()? != self.repository {
            return Err(Error::InvalidInput(
                "runtime is bound to another repository".into(),
            ));
        }
        let layer = expected.layer();
        if desired.layer() != layer {
            return Err(Error::InvalidInput(
                "rebind heads belong to different Layers".into(),
            ));
        }
        let slot = slot(layer);
        let mut record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        if record.pending.is_some() || record.rebinding() {
            self.resolve(repo, layer, &slot, &mut record)?;
            record = self
                .read_record(&slot, layer)?
                .ok_or_else(|| Error::InvalidInput("Layer was discarded".into()))?;
        }
        self.require_active(repo, desired)?;
        let mut old = record.clone();
        old.revision = expected.revision();
        old.head = expected.checkpoint().object().to_string();
        if self.head(repo, layer, &old)? != *expected {
            return Err(Error::InvalidInput("invalid old rebind head".into()));
        }
        if record.rebind.as_ref().is_some_and(|r| {
            r.phase == RebindPhase::Complete
                && r.expected == BoundHead::from(expected)
                && r.desired == BoundHead::from(desired)
        }) && self.head(repo, layer, &record)? == *desired
        {
            return Ok(());
        }
        if self.head(repo, layer, &record)? != *expected
            || desired.revision() <= expected.revision()
        {
            return Err(Error::InvalidInput(
                "rebind requires the exact old view and a newer head".into(),
            ));
        }
        let lowers = self.ensure_caches(repo, desired.checkpoint())?;
        let mounted = mount_state(&self.view_path(layer))?.is_some();
        record.rebind = Some(Rebind {
            expected: expected.into(),
            desired: desired.into(),
            lowers,
            mounted,
            phase: RebindPhase::Validating,
        });
        self.write_record(&slot, &record)?;
        let prepared = (|| {
            self.mount_view(&slot, &record, true)?;
            self.freeze_view(&slot)?;
            #[cfg(test)]
            crate::repository::test_boundary("rebind_frozen");
            let (epoch, upper) = parse_upper(&self.helper("list", &["view", &slot])?)?;
            let opened = repo.read_checkpoint(CheckpointId::parse(&record.opened)?)?;
            let snap = snapshot(
                &self.view_path(layer),
                &upper,
                opened.manifest(),
                &mut hash_only,
            )?;
            let checkpoint = repo.read_checkpoint(expected.checkpoint())?;
            if snap.entries != *checkpoint.manifest().entries() || !snap.unrepresented.is_empty() {
                return Err(Error::InvalidInput(
                    "workspace has uncaptured changes; checkpoint before rebinding".into(),
                ));
            }
            if self.epoch()? != epoch {
                return Err(Error::Busy("view changed during rebind validation".into()));
            }
            #[cfg(test)]
            crate::repository::test_boundary("rebind_validated");
            self.require_active(repo, desired)?;
            if self.epoch()? != epoch {
                return Err(Error::Busy("view changed before rebind selection".into()));
            }
            self.helper("unmount-view", &[&slot])?;
            Ok(())
        })();
        match prepared {
            Ok(()) => (),
            Err(error) => {
                self.finish_rebind(repo, layer, &slot, &mut record)?;
                return Err(error);
            }
        };
        record.rebind.as_mut().unwrap().phase = RebindPhase::Prepared;
        self.write_record(&slot, &record)?;
        #[cfg(test)]
        crate::repository::test_boundary("rebind_prepared");
        self.finish_rebind(repo, layer, &slot, &mut record)
    }

    fn require_active(&self, repo: &Repository, desired: &LayerHead) -> Result<()> {
        match repo.layer_state(desired.layer())? {
            LayerState::Active { head } if head == *desired => Ok(()),
            _ => Err(Error::InvalidInput(
                "desired rebind head is not the Layer's current Active head".into(),
            )),
        }
    }

    fn finish_rebind(
        &self,
        repo: &Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
    ) -> Result<()> {
        let mut intent = record.rebind.clone().expect("rebind intent");
        if intent.phase == RebindPhase::Validating {
            if intent.mounted {
                self.resume_view(repo, layer, slot, record, true)?;
            } else {
                self.helper("unmount-view", &[slot])?;
            }
            record.rebind = None;
            return self.write_record(slot, record);
        }
        if intent.phase == RebindPhase::Prepared {
            // The old upper was verified clean and detached before the intent.
            // Until Selected is durable, no new writable view can be exposed.
            self.helper("unmount-view", &[slot])?;
            self.helper("remove-view", &[slot])?;
            #[cfg(test)]
            crate::repository::test_boundary("rebind_removed");
            record.opened = intent.desired.checkpoint.clone();
            record.head = record.opened.clone();
            record.revision = intent.desired.revision;
            record.lowers = intent.lowers.clone();
            intent.phase = RebindPhase::Selected;
            record.rebind = Some(intent.clone());
            self.write_record(slot, record)?;
        }
        #[cfg(test)]
        crate::repository::test_boundary("rebind_selected");
        if intent.mounted {
            self.mount_view(slot, record, true)?;
        }
        intent.phase = RebindPhase::Complete;
        record.rebind = Some(intent);
        self.write_record(slot, record)?;
        #[cfg(test)]
        crate::repository::test_boundary("rebind_complete");
        self.resume_view(repo, layer, slot, record, false)
    }

    /// Unmounts without discarding; the private upper and binding remain.
    pub fn unmount(&mut self, layer: &LayerId) -> Result<()> {
        let _lock = self.lock()?;
        let slot = slot(layer);
        let record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        if record.rebinding() {
            return Err(Error::Busy(
                "recover interrupted rebind before unmounting".into(),
            ));
        }
        self.helper("unmount-view", &[&slot]).map(|_| ())
    }

    fn ensure_thawed(&self, slot: &str) -> Result<()> {
        if mount_state(&self.root.join("views").join(slot))? == Some(true) {
            self.helper("thaw", &["view", slot])?;
        }
        Ok(())
    }

    /// Observation freezes existing mounts without undoing an explicit unmount.
    fn freeze_view(&self, slot: &str) -> Result<bool> {
        match mount_state(&self.root.join("views").join(slot))? {
            None => Ok(false),
            Some(readonly) => {
                if !readonly {
                    self.helper("freeze", &["view", slot])?;
                }
                Ok(true)
            }
        }
    }

    /// A terminal native publication is never thawed. There is no continuous
    /// reconciler: a direct native caller is enforced when next observed here.
    fn resume_view(
        &self,
        repo: &Repository,
        layer: &LayerId,
        slot: &str,
        record: &ViewRecord,
        mount: bool,
    ) -> Result<()> {
        if matches!(repo.layer_state(layer)?, LayerState::Published { .. }) {
            if mount {
                self.mount_view(slot, record, true)?;
            }
            self.freeze_view(slot).map(|_| ())
        } else {
            if mount {
                self.mount_view(slot, record, false)?;
            }
            self.ensure_thawed(slot)
        }
    }

    fn epoch(&self) -> Result<u64> {
        let value: Value = serde_json::from_str(&self.helper("epoch", &[])?)
            .map_err(|_| Error::Io(std::io::Error::other("malformed epoch reply")))?;
        value["epoch"]
            .as_u64()
            .ok_or_else(|| Error::Io(std::io::Error::other("malformed epoch reply")))
    }

    /// The durable native outcome already recorded for this operation, if any.
    /// Native attempt rows are the authority for whether an operation committed.
    fn durable(
        &self,
        repo: &Repository,
        layer: &LayerId,
        operation: &OperationId,
        discard: bool,
    ) -> Result<Option<crate::layer::LayerAttempt>> {
        match repo.layer_operation(operation)? {
            Some(attempt) if &attempt.layer != layer || attempt.discard != discard => {
                Err(Error::OperationIdReused(operation.clone()))
            }
            found => Ok(found),
        }
    }

    fn abandon(
        &self,
        repo: &Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
    ) -> Result<()> {
        record.pending = None;
        self.write_record(slot, record)?;
        self.resume_view(repo, layer, slot, record, false)
    }

    /// Records an outcome and thaws. Only an operation this view froze for (its
    /// pending capture) may move the bound head, and never backwards; replaying
    /// another caller's operation must not adopt that caller's head.
    fn settle_capture(
        &self,
        repo: &Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
        owned: bool,
        outcome: CaptureOutcome,
    ) -> Result<CaptureOutcome> {
        if let LayerUpdate::Applied {
            state: LayerState::Active { head },
        } = &outcome.update
            && owned
            && head.revision() > record.revision
        {
            record.revision = head.revision();
            record.head = head.checkpoint().object().to_string();
        }
        record.pending = None;
        self.write_record(slot, record)?;
        self.resume_view(repo, layer, slot, record, false)?;
        Ok(outcome)
    }

    /// Freezes the view, captures its complete delta through the native API, and
    /// thaws. `busy` means a writer held a file open or the freeze was not owned
    /// for the whole read; nothing changed. Retrying any completed operation ID of
    /// this Layer returns its durable native outcome; only this view's own pending
    /// capture can move the head it is bound to.
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
            candidate: None,
            publication: None,
        };
        if record.rebinding()
            || record
                .pending
                .as_ref()
                .is_some_and(|p| !p.is(operation, Action::Capture))
        {
            self.resolve(repo, layer, &slot, &mut record)?;
            record = self
                .read_record(&slot, layer)?
                .ok_or_else(|| Error::InvalidInput("Layer was discarded".into()))?;
        }
        if record.pending.is_none() && self.durable(repo, layer, operation, false)?.is_none() {
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
        let owned = record
            .pending
            .as_ref()
            .is_some_and(|p| p.is(operation, Action::Capture));
        let durable = match self.durable(repo, layer, operation, false) {
            Ok(durable) => durable,
            Err(error) => {
                // The ID was used natively elsewhere; this capture can never commit.
                if certain(&error) && owned {
                    self.abandon(repo, layer, slot, record)?;
                }
                return Err(error);
            }
        };
        if let Some(attempt) = &durable
            && !owned
        {
            // A plain retry of a completed operation: report it, never rebind.
            let changed = repo
                .read_checkpoint(attempt.checkpoint)?
                .changes()
                .entries()
                .len();
            return self.settle_capture(
                repo,
                layer,
                slot,
                record,
                false,
                CaptureOutcome {
                    update: attempt.outcome.clone(),
                    changed_paths: changed,
                    unrepresented_directories: Vec::new(),
                    replayed: true,
                },
            );
        }
        if let Some(attempt) = durable {
            // Decide ownership from the recorded candidate alone. Without one, this
            // view never reached its native call, so the commit is another caller's.
            let candidate = record.pending.as_ref().and_then(|p| p.candidate.clone());
            let changes = repo.read_checkpoint(attempt.checkpoint)?.changes().clone();
            let ours = candidate.is_some_and(|c| {
                c.expected == attempt.expected && c.changes == changes_digest(&changes)
            });
            if !ours {
                self.abandon(repo, layer, slot, record)?;
                return Err(Error::OperationIdReused(operation.clone()));
            }
            return self.settle_capture(
                repo,
                layer,
                slot,
                record,
                true,
                CaptureOutcome {
                    update: attempt.outcome,
                    changed_paths: changes.entries().len(),
                    unrepresented_directories: Vec::new(),
                    replayed: true,
                },
            );
        }
        if let LayerState::Published { operation, .. } = repo.layer_state(layer)? {
            self.abandon(repo, layer, slot, record)?;
            return Err(Error::LayerPublished {
                layer: layer.clone(),
                operation,
            });
        }
        match mount_state(&self.root.join("views").join(slot))? {
            None => self.mount_view(slot, record, true)?,
            Some(false) => {
                self.helper("freeze", &["view", slot])?;
            }
            Some(true) => {}
        }
        let attempt = (|| {
            let (epoch, upper) = parse_upper(&self.helper("list", &["view", slot])?)?;
            #[cfg(test)]
            crate::repository::test_boundary("capture_listed");
            let opened = repo.read_checkpoint(CheckpointId::parse(&record.opened)?)?;
            let base = repo.read_manifest(opened.base().manifest())?;
            let view = self.root.join("views").join(slot);
            let expected = self.head(repo, layer, record)?;
            let checkpoint = repo.read_checkpoint(expected.checkpoint())?;
            let snap = read_snapshot(
                &view,
                Some(&upper),
                opened.manifest(),
                Some(checkpoint.manifest()),
                &mut |b| repo.put_blob(b),
            )?;
            // Every mutating helper verb advances the epoch, so equality proves no
            // thaw, remount or unmount happened between the listing and the last read.
            if self.epoch()? != epoch {
                return Err(Error::Busy(
                    "view was thawed or remounted during capture".into(),
                ));
            }
            let changes = complete_changes(&base, &snap.entries)?;
            let expected = self.head(repo, layer, record)?;
            Ok((changes, snap.unrepresented, expected))
        })();
        let (changes, unrepresented, expected) = match attempt {
            Ok(values) => values,
            Err(error) => {
                // No native attempt exists for this operation, so nothing committed:
                // thaw and keep the edits.
                self.abandon(repo, layer, slot, record)?;
                return Err(error);
            }
        };
        let candidate = Candidate {
            expected: expected.revision(),
            changes: changes_digest(&changes),
        };
        if record
            .pending
            .as_ref()
            .and_then(|p| p.candidate.as_ref())
            .is_some_and(|recorded| recorded != &candidate)
        {
            // A watch observation (or an interrupted prepared capture) is bound
            // to exact input. Never substitute a later frozen tree under its ID.
            self.abandon(repo, layer, slot, record)?;
            return Err(Error::Busy(
                "frozen candidate differs from recorded capture input".into(),
            ));
        }
        if let Some(pending) = record.pending.as_mut() {
            pending.candidate = Some(candidate);
        }
        self.write_record(slot, record)?;
        let update = match repo.capture_layer(operation, &expected, &changes) {
            Ok(update) => update,
            Err(error) => {
                // Uncertain storage errors stay frozen; the retry consults the
                // durable native record and verifies it before adopting anything.
                if certain(&error) {
                    self.abandon(repo, layer, slot, record)?;
                }
                return Err(error);
            }
        };
        #[cfg(test)]
        crate::repository::test_boundary("capture_committed");
        let changed = changes.entries().len();
        self.settle_capture(
            repo,
            layer,
            slot,
            record,
            true,
            CaptureOutcome {
                update,
                changed_paths: changed,
                unrepresented_directories: unrepresented,
                replayed: false,
            },
        )
    }

    /// Publishes the whole bound checkpoint after freezing the ordinary path.
    /// Uncaptured tracked/nonignored file changes or empty directories require a
    /// checkpoint or cleanup first. Ignored untracked bytes remain private. A successful view stays
    /// read-only. Completed retries return the original native receipt and never
    /// rerun the guard. Recovery with no native receipt releases the intent without
    /// publishing: callers must explicitly retry with their guard/policy again.
    pub fn publish<F>(
        &mut self,
        repo: &mut Repository,
        layer: &LayerId,
        operation: &OperationId,
        attribution: &PublishAttribution,
        guard: F,
    ) -> Result<PublicationReceipt>
    where
        F: FnOnce(&WorldVersion, &Manifest) -> Result<()>,
    {
        let _lock = self.lock()?;
        let slot = slot(layer);
        let mut record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        if record.pending.is_some() || record.rebinding() {
            self.resolve(repo, layer, &slot, &mut record)?;
            record = self
                .read_record(&slot, layer)?
                .ok_or_else(|| Error::InvalidInput("Layer was discarded".into()))?;
        }
        if let Some(receipt) = repo.publication(operation)? {
            self.resume_view(repo, layer, &slot, &record, false)?;
            if receipt.expected.layer() != layer || receipt.attribution != *attribution {
                return Err(Error::OperationIdReused(operation.clone()));
            }
            return Ok(receipt);
        }
        if repo.layer_operation(operation)?.is_some() {
            return Err(Error::OperationIdReused(operation.clone()));
        }
        let view = self.root.join("views").join(&slot);
        if mount_state(&view)?.is_none() {
            return Err(Error::InvalidInput("Layer view is not mounted".into()));
        }
        self.helper("freeze", &["view", &slot])?;
        record.pending = Some(Pending {
            operation: operation.as_str().into(),
            action: Action::Publish,
            candidate: None,
            publication: None,
        });
        self.write_record(&slot, &record)?;
        #[cfg(test)]
        crate::repository::test_boundary("workspace_publish_frozen");
        let prepared = (|| {
            let expected = self.head(repo, layer, &record)?;
            let (epoch, upper) = parse_upper(&self.helper("list", &["view", &slot])?)?;
            let opened = repo.read_checkpoint(CheckpointId::parse(&record.opened)?)?;
            let checkpoint = repo.read_checkpoint(expected.checkpoint())?;
            let snap = read_snapshot(
                &view,
                Some(&upper),
                opened.manifest(),
                Some(checkpoint.manifest()),
                &mut hash_only,
            )?;
            if snap.entries != *checkpoint.manifest().entries() || !snap.unrepresented.is_empty() {
                return Err(Error::InvalidInput(
                    "workspace has uncaptured changes; checkpoint first (remove empty directories, which checkpoints cannot represent)".into(),
                ));
            }
            if self.epoch()? != epoch {
                return Err(Error::Busy(
                    "view changed during publication preparation".into(),
                ));
            }
            Ok((expected, epoch))
        })();
        let (expected, epoch) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.abandon(repo, layer, &slot, &mut record)?;
                return Err(error);
            }
        };
        record.pending.as_mut().unwrap().publication =
            Some(publication_input(&expected, attribution));
        self.write_record(&slot, &record)?;
        #[cfg(test)]
        crate::repository::test_boundary("workspace_publish_prepared");
        let mut guard_refused = false;
        let result = repo.publish_layer(operation, &expected, attribution, |world, candidate| {
            let checked = (|| {
                if self.epoch()? != epoch {
                    return Err(Error::Busy("view changed before publication".into()));
                }
                guard(world, candidate)?;
                if self.epoch()? != epoch {
                    return Err(Error::Busy("view changed in publication guard".into()));
                }
                Ok(())
            })();
            guard_refused = checked.is_err();
            checked
        });
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(error) => {
                // A guard refusal cannot have committed. Uncertain storage errors
                // retain the frozen intent until publication() resolves certainty.
                if guard_refused || certain(&error) {
                    self.abandon(repo, layer, &slot, &mut record)?;
                }
                return Err(error);
            }
        };
        #[cfg(test)]
        crate::repository::test_boundary("workspace_publish_committed");
        self.abandon(repo, layer, &slot, &mut record)?;
        Ok(receipt)
    }

    fn resolve_publication(
        &self,
        repo: &Repository,
        layer: &LayerId,
        slot: &str,
        record: &mut ViewRecord,
    ) -> Result<()> {
        let pending = record.pending.as_ref().unwrap();
        let operation = OperationId::parse(&pending.operation)?;
        let receipt = repo.publication(&operation)?;
        let agrees = receipt.as_ref().is_none_or(|receipt| {
            receipt.expected.layer() == layer
                && pending.publication.as_ref()
                    == Some(&publication_input(&receipt.expected, &receipt.attribution))
        });
        // No receipt means no native acceptance. Never invent or replay optional
        // policy on recovery; release for an explicit caller retry instead.
        if agrees && receipt.is_some() {
            // A pending native receipt may need its view restored to complete
            // recovery. Plain observation never creates a missing mount.
            self.resume_view(repo, layer, slot, record, true)?;
        }
        self.abandon(repo, layer, slot, record)?;
        if !agrees {
            return Err(Error::OperationIdReused(operation));
        }
        Ok(())
    }

    /// Terminal native discard, then bounded removal of this view's private storage.
    /// A busy view refuses before any state changes. World and siblings are untouched.
    /// A view bound to a stale head gets `Stale` back and keeps its edits.
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
            candidate: None,
            publication: None,
        };
        if record.rebinding()
            || record
                .pending
                .as_ref()
                .is_some_and(|p| !p.is(operation, Action::Discard))
        {
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
        let update = match self.durable(repo, layer, operation, true) {
            Ok(Some(attempt)) => Ok(attempt.outcome),
            Ok(None) => self
                .head(repo, layer, record)
                .and_then(|expected| repo.discard_layer(operation, &expected)),
            Err(error) => Err(error),
        };
        let update = match update {
            Ok(update) => update,
            Err(error) => {
                if certain(&error) {
                    record.pending = None;
                    self.write_record(slot, record)?;
                    self.resume_view(repo, layer, slot, record, true)?;
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
                self.resume_view(repo, layer, slot, record, true)?;
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
        if record.rebinding() {
            self.finish_rebind(repo, layer, slot, record)?;
        }
        let Some(pending) = record.pending.clone() else {
            return Ok(());
        };
        let operation = OperationId::parse(&pending.operation)
            .map_err(|_| Error::CorruptRepository("invalid pending operation".into()))?;
        match pending.action {
            Action::Publish => self.resolve_publication(repo, layer, slot, record),
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
        let _lock = self.lock()?;
        let slot = slot(layer);
        let record = self
            .read_record(&slot, layer)?
            .ok_or_else(|| Error::InvalidInput("Layer has no view".into()))?;
        let path = self.root.join("views").join(&slot);
        let current = repo.layer_state(layer)?;
        if matches!(current, LayerState::Published { .. }) {
            self.freeze_view(&slot)?;
        }
        let state = mount_state(&path)?;
        Ok(ViewStatus {
            path,
            mounted: state.is_some(),
            frozen: state == Some(true),
            bound_revision: record.revision,
            bound_checkpoint: record.head.clone(),
            pending: if record.rebinding() {
                Some("rebind".into())
            } else {
                record.pending.map(|p| p.operation)
            },
            current,
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
            if let Err(error) = self.recover_view(repo, slot, &name, &mut report) {
                report.failures.push(format!("{slot}: {error}"));
            }
        }
        Ok(report)
    }

    fn recover_view(
        &self,
        repo: &mut Repository,
        slot: &str,
        name: &str,
        report: &mut RecoveryReport,
    ) -> Result<()> {
        let bytes = fs::read(self.root.join("state").join(name))?;
        let mut record: ViewRecord = serde_json::from_slice(&bytes)
            .map_err(|_| Error::CorruptRepository("invalid view record".into()))?;
        let layer = LayerId::parse(&record.layer)
            .map_err(|_| Error::CorruptRepository("invalid view record".into()))?;
        if self::slot(&layer) != slot || record.format != RECORD_FORMAT {
            return Err(Error::CorruptRepository(
                "view record slot/format mismatch".into(),
            ));
        }
        if record.pending.is_some() || record.rebinding() {
            if let Err(error) = self.resolve(repo, &layer, slot, &mut record)
                && (!matches!(error, Error::LayerPublished { .. }) || record.pending.is_some())
            {
                return Err(error);
            }
            report.pending_resolved += 1;
        } else if matches!(repo.layer_state(&layer)?, LayerState::Published { .. }) {
            if self.freeze_view(slot)? {
                report.views_published += 1;
            }
        } else if matches!(repo.layer_state(&layer)?, LayerState::Discarded { .. }) {
            self.helper("unmount-view", &[slot])?;
            self.helper("remove-view", &[slot])?;
            self.remove_record(slot)?;
            report.views_removed += 1;
        } else if mount_state(&self.root.join("views").join(slot))? == Some(true) {
            // A crash between freeze and the pending write leaves nothing to resolve.
            self.helper("thaw", &["view", slot])?;
            report.views_thawed += 1;
        }
        Ok(())
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
    fn ignore_discovery_preserves_current_checkpoint_entries_and_deletions() {
        let (_d, mut repo) = repo();
        let tree = base_tree();
        let lower = import(&mut repo, tree.path());
        let root = tree.path();
        fs::create_dir_all(root.join("build/nested")).unwrap();
        fs::write(root.join("build/added"), b"tracked after opening").unwrap();
        symlink("README.txt", root.join("added-link")).unwrap();
        let current = import(&mut repo, root);
        fs::write(
            root.join(".javelinignore"),
            "build/\nadded-link\nREADME.txt\nbytes.bin\ndir/\n",
        )
        .unwrap();
        fs::write(root.join("build/nested/private"), b"never stored").unwrap();
        fs::create_dir(root.join("build/empty")).unwrap();
        fs::write(root.join("build/added"), b"tracked edit").unwrap();
        fs::set_permissions(root.join("bytes.bin"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(root.join("added-link")).unwrap();
        symlink("new-target", root.join("added-link")).unwrap();
        fs::remove_file(root.join("README.txt")).unwrap();
        fs::remove_dir_all(root.join("dir")).unwrap();
        let before = fs::read(root.join("build/nested/private")).unwrap();
        let snap = read_snapshot(
            root,
            Some(&all_paths(root)),
            &lower,
            Some(&current),
            &mut |b| repo.put_blob(b),
        )
        .unwrap();
        let observed = read_snapshot(root, None, &current, Some(&current), &mut hash_only).unwrap();
        assert_eq!(snap, observed);
        assert!(!snap.entries.contains_key(&path("build/nested/private")));
        assert!(snap.unrepresented.is_empty());
        assert_eq!(fs::read(root.join("build/nested/private")).unwrap(), before);
        assert!(snap.entries.contains_key(&path(".javelinignore")));
        assert!(matches!(
            snap.entries[&path("bytes.bin")],
            FileEntry::Regular {
                executable: true,
                ..
            }
        ));
        assert_eq!(
            repo.read_blob(snap.entries[&path("build/added")].content())
                .unwrap(),
            b"tracked edit"
        );
        assert_eq!(
            repo.read_blob(snap.entries[&path("added-link")].content())
                .unwrap(),
            b"new-target"
        );
        let changes = complete_changes(&current, &snap.entries).unwrap();
        for name in ["README.txt", "dir/a.txt", "dir/sub/b.txt"] {
            assert_eq!(changes.entries()[&path(name)], None);
        }
        // Removing rules discovers the still-present bytes; no ignore operation deleted them.
        fs::write(root.join(".javelinignore"), "").unwrap();
        let exposed = read_snapshot(root, None, &current, Some(&current), &mut hash_only).unwrap();
        assert!(exposed.entries.contains_key(&path("build/nested/private")));
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
    fn native_attempt_rows_decide_whether_operations_committed() {
        let (_d, mut repo) = repo();
        let base = repo.layer_base(VersionNumber::new(0).unwrap()).unwrap();
        let layer = LayerId::parse("work").unwrap();
        let head = repo.create_layer(&layer, &base).unwrap();
        let capture = OperationId::parse("capture").unwrap();
        assert!(repo.layer_operation(&capture).unwrap().is_none());
        let changes = SparseChanges::new([(
            path("f"),
            Some(FileEntry::Regular {
                content: repo.put_blob(b"x").unwrap(),
                executable: false,
            }),
        )])
        .unwrap();
        let applied = repo.capture_layer(&capture, &head, &changes).unwrap();
        let found = repo.layer_operation(&capture).unwrap().unwrap();
        assert_eq!(
            (found.layer, found.discard, found.expected, found.outcome),
            (layer.clone(), false, 0, applied.clone())
        );
        assert_eq!(
            repo.read_checkpoint(found.checkpoint).unwrap().changes(),
            &changes
        );
        // A stale attempt is durable too and keeps its exact original outcome.
        let stale_op = OperationId::parse("stale").unwrap();
        let stale = repo.capture_layer(&stale_op, &head, &changes).unwrap();
        assert!(matches!(stale, LayerUpdate::Stale { .. }));
        assert_eq!(
            repo.layer_operation(&stale_op).unwrap().unwrap().outcome,
            stale
        );
        let LayerUpdate::Applied {
            state: LayerState::Active { head: next },
        } = applied
        else {
            panic!("capture not applied")
        };
        let discard_op = OperationId::parse("discard").unwrap();
        let discarded = repo.discard_layer(&discard_op, &next).unwrap();
        let found = repo.layer_operation(&discard_op).unwrap().unwrap();
        assert!(found.discard && found.expected == 1);
        assert_eq!(found.outcome, discarded);
    }

    #[test]
    fn candidate_digest_is_exact_and_order_independent() {
        let (_d, mut repo) = repo();
        let a = FileEntry::Regular {
            content: repo.put_blob(b"a").unwrap(),
            executable: false,
        };
        let one = SparseChanges::new([(path("x"), Some(a.clone())), (path("y"), None)]).unwrap();
        let two = SparseChanges::new([(path("y"), None), (path("x"), Some(a.clone()))]).unwrap();
        assert_eq!(changes_digest(&one), changes_digest(&two));
        let exec = SparseChanges::new([
            (
                path("x"),
                Some(FileEntry::Regular {
                    content: a.content(),
                    executable: true,
                }),
            ),
            (path("y"), None),
        ])
        .unwrap();
        assert_ne!(changes_digest(&one), changes_digest(&exec));
        let pending: Pending =
            serde_json::from_str(r#"{"operation":"op","action":"capture"}"#).unwrap();
        assert!(pending.candidate.is_none());
    }

    #[test]
    fn records_reject_unknown_fields() {
        let json = r#"{"format":1,"layer":"a","opened":"x","lowers":[],"revision":0,"head":"x","pending":null,"extra":1}"#;
        assert!(serde_json::from_str::<ViewRecord>(json).is_err());
        let parsed = parse_upper(
            r#"{"epoch":7,"entries":[{"path":"612f62","kind":"file"},{"path":"63","kind":"whiteout"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed, (7, HashSet::from([b"a/b".to_vec()])));
        assert!(parse_upper(r#"{"epoch":1,"entries":[{"path":"6","kind":"file"}]}"#).is_err());
        assert!(
            parse_upper(r#"{"entries":[]}"#).is_err(),
            "listing without epoch"
        );
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
