//! Keeps a Layer's directory in step with its head when the head moves under it, which
//! Stack does.
//!
//! The directory is synced with a checkpoint `S`, recorded beside its stat cache as
//! `.javelin/capture/<hash>.sync`. Everything else in the directory is a local edit
//! against `S`. Refresh merges the local edits (`S` to directory) with the head's changes
//! (`S` to head): a path that both sides changed differently, or a file and a directory
//! that would share a path, refuses the whole refresh before anything is written.
//!
//! Crash safety. Each file is replaced by renaming a complete staged copy over it, so a
//! path holds either its old or its new content. A changed path already holding the head's
//! content matches the head, so re-running after an interruption applies only what
//! remains and converges. The sync record is written last.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{Store, WorkspaceRecovery, clone, native, remove_entry, representable, sync_dir};
use crate::{
    CheckpointId, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, RepoPath,
    Repository, Result, SparseChanges,
    capture::{Capture, StatCache, changes_from, entry_at, scan, store_blob, write_atomic},
};

/// What a refresh did. `refreshed` is false when the directory already matched the head.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceRefresh {
    pub layer: LayerId,
    pub root: PathBuf,
    pub refreshed: bool,
    /// The checkpoint the directory was synced with before the refresh.
    pub from: CheckpointId,
    /// The head the directory is synced with afterwards.
    pub to: CheckpointId,
    /// Paths written or removed to match the head.
    pub applied: usize,
    /// Local edits left as they are because the head does not change those paths.
    pub kept: usize,
    pub recovery: WorkspaceRecovery,
}

/// The three-way decision for one refresh. Nothing is written until it has no conflicts.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Paths that cannot be merged, or that a file or directory on disk blocks.
    conflicts: BTreeSet<String>,
    /// Head changes for paths the directory holds at `S`, in path order.
    apply: Vec<(RepoPath, Option<FileEntry>)>,
    /// Local edits the head does not touch.
    kept: usize,
}

impl Repository {
    /// Captures the Layer's clone-backed directory (`.javelin/layers/<id>`) before a
    /// command reads Layer state. `None` when there is no directory or the Layer is
    /// no longer active, so there is nothing to capture.
    ///
    /// A directory whose head came from Stack and that is not synced with it is behind:
    /// it is not scanned, because capturing it would revert the stacked work.
    pub fn capture_workspace(&mut self, layer: &LayerId) -> Result<Option<Capture>> {
        let root = self.workspace_root(layer)?;
        if !root.try_exists()? {
            return Ok(None);
        }
        let _store = Store::lock(self)?;
        let head = match self.layer_state(layer)? {
            LayerState::Active { head } => head,
            _ => return Ok(None),
        };
        self.capture_synced(layer, head, &root, true).map(Some)
    }

    /// Captures the Layer's directory at `root` into a checkpoint when it differs from the
    /// head. A stacked head is captured only once the directory is synced with it. Only
    /// the Layer's own directory moves its sync record.
    pub fn capture_directory(&mut self, layer: &LayerId, root: &Path) -> Result<Capture> {
        let _store = Store::lock(self)?;
        let head = match self.layer_state(layer)? {
            LayerState::Active { head } => head,
            _ => return Err(Error::InvalidInput("Layer is not active".into())),
        };
        let workspace = self.workspace_root(layer)?;
        let same = matches!(
            (fs::canonicalize(root), fs::canonicalize(&workspace)),
            (Ok(a), Ok(b)) if a == b
        );
        self.capture_synced(layer, head, root, same)
    }

    fn capture_synced(
        &mut self,
        layer: &LayerId,
        head: LayerHead,
        root: &Path,
        workspace: bool,
    ) -> Result<Capture> {
        if self.stacked(&head)? && self.synced(layer)? != Some(head.checkpoint()) {
            return Capture::behind(head);
        }
        let capture = self.capture_at(head, root)?;
        // The directory now matches the captured head. A stale update leaves the record as
        // it was, because the directory still matches the checkpoint it was synced with.
        let synced = match &capture.update {
            None => Some(capture.head.checkpoint()),
            Some(LayerUpdate::Applied {
                state: LayerState::Active { head },
            }) => Some(head.checkpoint()),
            _ => None,
        };
        if let Some(checkpoint) = synced
            && workspace
        {
            self.write_sync(layer, checkpoint)?;
        }
        Ok(capture)
    }

    /// Brings the Layer's directory to its current head, keeping local edits that the
    /// head does not change. Refuses with [`Error::RefreshConflict`] and changes nothing
    /// when a local edit and the head disagree on a path. The directory is not captured
    /// first: local edits stay local until the next capture.
    pub fn refresh_workspace(&mut self, layer: &LayerId) -> Result<WorkspaceRefresh> {
        let store = Store::lock(self)?;
        let recovery = store.recover(self, Some(super::RECLAIM_BUDGET))?;
        let head = match self.layer_state(layer)? {
            LayerState::Active { head } => head,
            _ => {
                return Err(Error::InvalidInput(format!(
                    "Layer {:?} is not active",
                    layer.as_str()
                )));
            }
        };
        let root = store.layers().join(layer.as_str());
        if !root.try_exists()? {
            return Err(Error::InvalidInput(format!(
                "Layer {:?} has no workspace; open it first",
                layer.as_str()
            )));
        }
        let Some(from) = self.synced(layer)? else {
            return Err(Error::InvalidInput(format!(
                "workspace of Layer {:?} has no sync record, so it cannot be refreshed",
                layer.as_str()
            )));
        };
        let mut report = WorkspaceRefresh {
            layer: layer.clone(),
            root: root.clone(),
            refreshed: false,
            from,
            to: head.checkpoint(),
            applied: 0,
            kept: 0,
            recovery,
        };
        if from == head.checkpoint() {
            return Ok(report);
        }
        let before = self.read_checkpoint(from)?;
        let after = self.read_checkpoint(head.checkpoint())?;
        if before.base().version() != after.base().version() {
            return Err(Error::Unsupported(format!(
                "Layer {:?} changed its base World; recreate the workspace",
                layer.as_str()
            )));
        }
        let base = self.checkpoint_manifest(from)?;
        let target = self.checkpoint_manifest(head.checkpoint())?;

        let cache_path = self.capture_cache(layer)?;
        let mut cache = StatCache::load(&cache_path);
        let tx = self.conn.unchecked_transaction()?;
        let scanned = scan(&root, &mut cache, &base, &mut |bytes| {
            store_blob(&tx, bytes)
        })?;
        tx.commit()?;
        // The cache only accelerates later scans; failing to save it loses no edits.
        let _ = cache.save(&cache_path);

        let local = changes_from(&base, &scanned.entries)?;
        let theirs = changes_from(&base, target.entries())?;
        let mut plan = plan(&local, &theirs);
        // A path still changing during the scan keeps its prior state, so the head must not
        // overwrite it either.
        for path in &scanned.uncertain {
            if theirs.entries().contains_key(path) {
                plan.conflicts.insert(path.as_str().to_owned());
            }
        }
        let gone: BTreeSet<&str> = plan
            .apply
            .iter()
            .filter(|(_, change)| change.is_none())
            .map(|(path, _)| path.as_str())
            .collect();
        plan.conflicts.extend(blocked(&root, &plan.apply, &gone)?);
        plan.conflicts
            .extend(occupied(&root, &plan.apply, &scanned.entries, &gone)?);
        plan.conflicts
            .extend(aliased_dirs(&root, &plan.apply, &gone)?);
        plan.conflicts.extend(aliased(&store, &plan.apply)?);
        if !plan.conflicts.is_empty() {
            return Err(Error::RefreshConflict(plan.conflicts.into_iter().collect()));
        }

        // Removals come first, so every path a removal frees is free before an addition
        // writes to it.
        for (path, change) in &plan.apply {
            if change.is_none() {
                remove(&root, path)?;
            }
        }
        for (path, change) in &plan.apply {
            if let Some(entry) = change {
                place(&store, &root, path, entry, self)?;
            }
        }
        // The sync record names the head only once every write it depends on is durable.
        // Each changed directory entry lives in its parent, so sync every directory on the
        // way to each head change. Changes an interrupted run already applied are head
        // changes too, so they are walked as well. A directory a removal emptied and deleted
        // is already gone.
        clone::barrier(&root)?;
        let mut dirs = BTreeSet::from([root.clone()]);
        for path in theirs.entries().keys() {
            let mut dir = path.as_str();
            while let Some((parent, _)) = dir.rsplit_once('/') {
                dirs.insert(native(&root, parent));
                dir = parent;
            }
        }
        for dir in dirs {
            // An ancestor a removal or the head replaced with a file is no longer a directory.
            match sync_dir(&dir) {
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
                other => other?,
            }
        }
        self.write_sync(layer, head.checkpoint())?;
        report.refreshed = true;
        report.applied = plan.apply.len();
        report.kept = plan.kept;
        Ok(report)
    }

    /// `.javelin/capture/<hash>.sync`: the checkpoint the directory was last synced with.
    /// An unreadable or invalid record counts as absent.
    pub(super) fn synced(&self, layer: &LayerId) -> Result<Option<CheckpointId>> {
        let path = self.sync_path(layer)?;
        match fs::read_to_string(path) {
            Ok(text) => Ok(CheckpointId::parse(text.trim()).ok()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn write_sync(&self, layer: &LayerId, checkpoint: CheckpointId) -> Result<()> {
        let line = format!("{}\n", checkpoint.object());
        write_atomic(&self.sync_path(layer)?, line.as_bytes())
    }

    fn sync_path(&self, layer: &LayerId) -> Result<PathBuf> {
        let mut path = self.capture_cache(layer)?.into_os_string();
        path.push(".sync");
        Ok(path.into())
    }
}

/// Three-way decision over local edits (`S` to directory) and head changes (`S` to head).
fn plan(local: &SparseChanges, theirs: &SparseChanges) -> Plan {
    let (local, theirs) = (local.entries(), theirs.entries());
    let mut plan = Plan {
        kept: local
            .keys()
            .filter(|path| !theirs.contains_key(*path))
            .count(),
        ..Plan::default()
    };
    for (path, change) in theirs {
        match local.get(path) {
            // Both sides made the same change, so the directory already holds it.
            Some(mine) if mine == change => {}
            Some(_) => {
                plan.conflicts.insert(path.as_str().to_owned());
            }
            None => plan.apply.push((path.clone(), change.clone())),
        }
    }
    // A file and a directory cannot share a path: a local file blocks head paths below it,
    // and a head file blocks local paths below it.
    for (path, mine) in local {
        if mine.is_none() {
            continue;
        }
        let mut dir = path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if matches!(theirs.get(&RepoPath(parent.into())), Some(Some(_))) {
                plan.conflicts.insert(path.as_str().to_owned());
            }
            dir = parent;
        }
        let prefix = format!("{}/", path.as_str());
        let below_head_file = theirs
            .range(RepoPath(prefix.clone())..)
            .take_while(|(p, _)| p.as_str().starts_with(&prefix))
            .any(|(_, change)| change.is_some());
        if below_head_file {
            plan.conflicts.insert(path.as_str().to_owned());
        }
    }
    plan
}

/// Head additions that reach an entry an earlier addition already made on this volume: a
/// name that differs only by case or Unicode normalization. The volume decides, so no
/// folding rule is guessed. The names are created in a scratch tree in the store's staging
/// area, never in the workspace, and the tree is removed afterwards.
fn aliased(store: &Store, apply: &[(RepoPath, Option<FileEntry>)]) -> Result<Vec<String>> {
    let scratch = store.staging("alias");
    fs::create_dir_all(&scratch)?;
    let found = probe(&scratch, apply);
    // Our own scratch tree: a failed removal only leaves files the store's recovery trashes.
    let _ = fs::remove_dir_all(&scratch);
    Ok(found)
}

/// Creates each addition's components under `scratch`. A component whose exact name is new
/// but whose entry already exists is an alias of an earlier one. A failure for any other
/// reason is left to the write, which reports it with the path's own error.
fn probe(scratch: &Path, apply: &[(RepoPath, Option<FileEntry>)]) -> Vec<String> {
    let mut dirs = BTreeMap::<String, PathBuf>::new();
    let mut found = Vec::new();
    for (path, change) in apply {
        if change.is_none() {
            continue;
        }
        let names: Vec<&str> = path.as_str().split('/').collect();
        let mut parent = scratch.to_path_buf();
        for (i, name) in names.iter().enumerate() {
            let key = names[..=i].join("/");
            if let Some(dir) = dirs.get(&key) {
                parent = dir.clone();
                continue;
            }
            let last = i + 1 == names.len();
            let child = parent.join(name);
            let made = if last {
                fs::File::create_new(&child).map(drop)
            } else {
                fs::create_dir(&child)
            };
            match made {
                Ok(()) if last => {}
                Ok(()) => {
                    dirs.insert(key, child.clone());
                    parent = child;
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    found.push(path.as_str().to_owned());
                    break;
                }
                Err(_) => break,
            }
        }
    }
    found
}

/// Head additions whose directory components name an existing directory under another spelling
/// on this volume: `D/new` when the workspace holds `d`. A component that resolves on disk but
/// is missing from its parent's listing is such an alias, because the listing names entries as
/// stored. A component the head deletes is removed first, so it is not an alias.
fn aliased_dirs(
    root: &Path,
    apply: &[(RepoPath, Option<FileEntry>)],
    gone: &BTreeSet<&str>,
) -> Result<Vec<String>> {
    let mut listings = BTreeMap::<String, BTreeSet<OsString>>::new();
    let mut found = Vec::new();
    for (path, change) in apply {
        if change.is_none() {
            continue;
        }
        let names: Vec<&str> = path.as_str().split('/').collect();
        for i in 1..names.len() {
            let dir = names[..i].join("/");
            if gone.contains(dir.as_str()) {
                break;
            }
            match fs::symlink_metadata(native(root, &dir)) {
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            let parent = names[..i - 1].join("/");
            if !listings.contains_key(&parent) {
                let listing = match fs::read_dir(native(root, &parent)) {
                    Ok(entries) => entries
                        .map(|entry| entry.map(|entry| entry.file_name()))
                        .collect::<std::io::Result<BTreeSet<OsString>>>()?,
                    Err(e) if e.kind() == ErrorKind::NotFound => BTreeSet::new(),
                    Err(e) => return Err(e.into()),
                };
                listings.insert(parent.clone(), listing);
            }
            if !listings[&parent].contains(OsStr::new(names[i - 1])) {
                found.push(path.as_str().to_owned());
                break;
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Whether a head removal of an ancestor frees `path`. The removed ancestor is a file or a
/// symlink, so nothing under it is reached through it any more.
fn freed(path: &str, gone: &BTreeSet<&str>) -> bool {
    let mut dir = path;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        if gone.contains(parent) {
            return true;
        }
        dir = parent;
    }
    false
}

/// Paths a file or directory on disk already blocks: a non-directory where the head needs
/// a parent, or a directory where it needs a file. Symlinked parents also count, because
/// writes never go through links. A path under a file or symlink the head deletes is free.
fn blocked(
    root: &Path,
    apply: &[(RepoPath, Option<FileEntry>)],
    gone: &BTreeSet<&str>,
) -> Result<Vec<String>> {
    let mut blocked = Vec::new();
    for (path, change) in apply {
        if change.is_none() || freed(path.as_str(), gone) {
            continue;
        }
        let mut dir = path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            match fs::symlink_metadata(native(root, parent)) {
                Ok(meta) if !meta.is_dir() => blocked.push(parent.to_owned()),
                Ok(_) => {}
                // A file above `parent` blocks it too, and the loop reaches that file next.
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
                Err(e) => return Err(e.into()),
            }
            dir = parent;
        }
        // Reaching the path through a file fails with NotADirectory; the ancestor is already
        // reported above, so the path itself counts as free.
        match fs::symlink_metadata(native(root, path.as_str())) {
            Ok(meta) if meta.is_dir() => blocked.push(path.as_str().to_owned()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(e) => return Err(e.into()),
        }
    }
    blocked.sort();
    blocked.dedup();
    Ok(blocked)
}

/// Head paths that a file on disk already occupies but the scan did not see: an ignored
/// untracked file, or a name that matches a local one only by case or normalization on this
/// volume. Writing over them would lose their bytes, so the refresh refuses. An ignored file
/// that already holds the head's entry is the write of an interrupted refresh, so it is applied.
fn occupied(
    root: &Path,
    apply: &[(RepoPath, Option<FileEntry>)],
    scanned: &BTreeMap<RepoPath, FileEntry>,
    gone: &BTreeSet<&str>,
) -> Result<Vec<String>> {
    let mut occupied = Vec::new();
    for (path, change) in apply {
        let Some(entry) = change else { continue };
        if scanned.contains_key(path) || gone.contains(path.as_str()) || freed(path.as_str(), gone)
        {
            continue;
        }
        let full = native(root, path.as_str());
        match fs::symlink_metadata(&full) {
            Ok(meta) if !meta.is_dir() => {
                let executable = matches!(
                    entry,
                    FileEntry::Regular {
                        executable: true,
                        ..
                    }
                );
                if entry_at(&full, executable)?.as_ref() != Some(entry) {
                    occupied.push(path.as_str().to_owned());
                }
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(occupied)
}

/// Removes a path the head deletes, then the parents it leaves empty. A path already gone
/// (an interrupted refresh got this far) is not an error.
fn remove(root: &Path, path: &RepoPath) -> Result<()> {
    let full = native(root, path.as_str());
    match fs::symlink_metadata(&full) {
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        other => {
            other?;
            remove_entry(&full)?;
        }
    }
    let mut dir = path.as_str();
    while let Some((parent, _)) = dir.rsplit_once('/') {
        if fs::remove_dir(native(root, parent)).is_err() {
            break;
        }
        dir = parent;
    }
    Ok(())
}

/// Writes a head entry by renaming a complete staged copy over the path, so the path never
/// holds partial content. A failed rename leaves the old content and removes the staged copy.
fn place(
    store: &Store,
    root: &Path,
    path: &RepoPath,
    entry: &FileEntry,
    repo: &Repository,
) -> Result<()> {
    representable(path)?;
    let target = native(root, path.as_str());
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let staged = store.staging("refresh");
    repo.write_entry(&staged, entry, path)?;
    fs::rename(&staged, &target).inspect_err(|_| store.abandon(&staged))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectId;

    fn path(p: &str) -> RepoPath {
        RepoPath::parse(p).unwrap()
    }
    fn file(bytes: &[u8]) -> Option<FileEntry> {
        Some(FileEntry::Regular {
            content: ObjectId::for_blob(bytes),
            executable: false,
        })
    }
    fn changes(items: &[(&str, Option<FileEntry>)]) -> SparseChanges {
        SparseChanges::new(items.iter().map(|(p, e)| (path(p), e.clone()))).unwrap()
    }
    fn names(conflicts: &BTreeSet<String>) -> Vec<&str> {
        conflicts.iter().map(String::as_str).collect()
    }

    #[test]
    fn same_change_on_both_sides_is_not_a_conflict() {
        let local = changes(&[("a", file(b"x")), ("gone", None)]);
        let theirs = changes(&[("a", file(b"x")), ("gone", None), ("b", file(b"y"))]);
        let plan = plan(&local, &theirs);
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.apply, vec![(path("b"), file(b"y"))]);
        assert_eq!(plan.kept, 0);
    }

    #[test]
    fn divergent_paths_and_structure_conflict_while_unrelated_edits_are_kept() {
        let local = changes(&[
            ("shared", file(b"mine")),
            ("x", file(b"local")),
            ("own", file(b"o")),
        ]);
        let theirs = changes(&[
            ("shared", file(b"theirs")),
            ("x/y", file(b"t")),
            ("new", file(b"n")),
        ]);
        let plan = plan(&local, &theirs);
        assert_eq!(names(&plan.conflicts), ["shared", "x"]);
        assert_eq!(
            plan.apply,
            vec![(path("new"), file(b"n")), (path("x/y"), file(b"t"))]
        );
        assert_eq!(plan.kept, 2);
    }

    #[test]
    fn a_head_file_above_a_local_path_conflicts() {
        let local = changes(&[("d/child", file(b"c"))]);
        let theirs = changes(&[("d", file(b"f"))]);
        assert_eq!(names(&plan(&local, &theirs).conflicts), ["d/child"]);
    }

    #[test]
    fn an_untouched_layer_has_nothing_to_merge() {
        let none = changes(&[]);
        assert_eq!(plan(&none, &none), Plan::default());
    }

    #[test]
    fn a_head_addition_under_an_existing_directory_spelling_is_an_alias_only_on_its_volume() {
        let root = tempfile::TempDir::new().unwrap();
        fs::create_dir(root.path().join("d")).unwrap();
        fs::write(root.path().join("d/keep"), b"k").unwrap();
        let none = BTreeSet::new();
        let exact = [(path("d/new"), file(b"n"))];
        assert!(aliased_dirs(root.path(), &exact, &none).unwrap().is_empty());
        let alias = [(path("D/new"), file(b"n"))];
        let on_this_volume = root.path().join("D").exists();
        let found = aliased_dirs(root.path(), &alias, &none).unwrap();
        assert_eq!(
            found,
            if on_this_volume {
                vec!["D/new".to_owned()]
            } else {
                vec![]
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_under_a_head_removed_symlink_is_free() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        fs::write(outside.path().join("y"), b"external").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("x")).unwrap();
        let apply = [(path("x"), None), (path("x/y"), file(b"n"))];
        let gone = BTreeSet::from(["x"]);
        assert!(blocked(root.path(), &apply, &gone).unwrap().is_empty());
        assert!(
            occupied(root.path(), &apply, &BTreeMap::new(), &gone)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_ignored_file_holding_the_head_entry_is_applied_not_occupied() {
        let root = tempfile::TempDir::new().unwrap();
        fs::write(root.path().join("new.txt"), b"theirs").unwrap();
        let none = BTreeSet::new();
        let same = [(path("new.txt"), file(b"theirs"))];
        assert!(
            occupied(root.path(), &same, &BTreeMap::new(), &none)
                .unwrap()
                .is_empty()
        );
        let differs = [(path("new.txt"), file(b"mine"))];
        assert_eq!(
            occupied(root.path(), &differs, &BTreeMap::new(), &none).unwrap(),
            ["new.txt"]
        );
    }
}
