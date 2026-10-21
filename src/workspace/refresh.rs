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
    CheckpointId, Choice, Error, FileEntry, LayerHead, LayerId, LayerState, LayerUpdate, Manifest,
    RepoPath, Repository, Result, SparseChanges,
    capture::{Capture, Source, StatCache, changes_from, entry_at, scan, store_blob, write_atomic},
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
    /// Paths among `paths` that alias another one on this volume, for Stack, which has no
    /// workspace to probe. Both members of an alias pair are returned, so the owner may
    /// choose either spelling.
    pub fn aliased_paths(&self, paths: &[RepoPath]) -> Result<Vec<RepoPath>> {
        let store = Store::lock(self)?;
        Ok(probe_volume(&store, paths)?.into_iter().cloned().collect())
    }

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
        let capture = self.capture_at(head, root, Source::Layer)?;
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
        self.refresh_in(layer, &BTreeMap::new(), true)
    }

    /// As [`Repository::refresh_workspace`], with the owner's choice for each conflicting
    /// path. A choice names a path that conflicts; any other path is an invalid input.
    /// Re-running after an interruption converges, as an unresolved path refuses again.
    pub fn refresh_workspace_resolved(
        &mut self,
        layer: &LayerId,
        choices: &BTreeMap<RepoPath, Choice>,
    ) -> Result<WorkspaceRefresh> {
        self.refresh_in(layer, choices, true)
    }

    /// Plans the refresh without writing. Returns the same conflicts a refresh would refuse
    /// with, and changes nothing but the store's recovery of its own trash.
    pub fn refresh_preview(&mut self, layer: &LayerId) -> Result<WorkspaceRefresh> {
        self.refresh_in(layer, &BTreeMap::new(), false)
    }

    fn refresh_in(
        &mut self,
        layer: &LayerId,
        choices: &BTreeMap<RepoPath, Choice>,
        apply: bool,
    ) -> Result<WorkspaceRefresh> {
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
            if let Some(path) = choices.keys().next() {
                return Err(not_a_conflict(path));
            }
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
        // A path both the local edits and the head renamed, to different destinations, is a
        // rename conflict. Both destinations need the owner's choice, and the head's write
        // to its destination waits for that choice.
        let local_manifest = Manifest::new(scanned.entries.clone())?;
        let (local_renames, head_renames) =
            (base.renames_to(&local_manifest), base.renames_to(&target));
        for (from, to) in &local_renames {
            if let Some(other) = head_renames.get(from).filter(|other| *other != to) {
                plan.conflicts.insert(to.as_str().to_owned());
                plan.conflicts.insert(other.as_str().to_owned());
            }
        }
        plan.apply
            .retain(|(path, _)| !plan.conflicts.contains(path.as_str()));
        // Structural conflicts depend on what the head applies, so they are found again
        // after the choices change the apply list. An occupied path and the spellings holding
        // it are all conflicts, so the owner may choose either side.
        let holders = |path: &RepoPath| alias_holders(&store, &root, path);
        let structural = |apply: &[(RepoPath, Option<FileEntry>)]| -> Result<StructuralConflicts> {
            let gone: BTreeSet<&str> = apply
                .iter()
                .filter(|(_, change)| change.is_none())
                .map(|(path, _)| path.as_str())
                .collect();
            let occupants = occupied(&root, apply, &scanned.entries, &gone, &holders)?;
            let mut found = BTreeSet::new();
            found.extend(blocked(&root, apply, &gone)?);
            for (path, spellings) in &occupants {
                found.insert(path.clone());
                found.extend(spellings.iter().cloned());
            }
            found.extend(aliased_dirs(&root, apply, &gone)?);
            found.extend(aliased(&store, apply)?);
            Ok((found, occupants))
        };
        let (known, occupants) = structural(&plan.apply)?;
        // Each spelling an occupant holds, mapped to the head write it blocks. Choosing that
        // spelling keeps it and cancels the write.
        let writer_of: BTreeMap<&str, &str> = occupants
            .iter()
            .flat_map(|(path, spellings)| {
                spellings
                    .iter()
                    .map(move |spelling| (spelling.as_str(), path.as_str()))
            })
            .collect();
        for path in choices.keys() {
            if !plan.conflicts.contains(path.as_str()) && !known.contains(path.as_str()) {
                return Err(not_a_conflict(path));
            }
        }
        for (path, choice) in choices {
            plan.conflicts.remove(path.as_str());
            // The choice replaces any head write the plan already made for the path, so the
            // path gets exactly one change.
            plan.apply.retain(|(applied, _)| applied != path);
            let change = match choice {
                // The directory keeps its own entry, so nothing is written for the path. Head
                // writes below the path would need it to be a directory, so they are cancelled
                // too, and so is the head write for the other spelling the path holds.
                Choice::Ours => {
                    let blocked_write = writer_of.get(path.as_str()).copied();
                    plan.apply.retain(|(applied, _)| {
                        !under(applied.as_str(), path.as_str())
                            && Some(applied.as_str()) != blocked_write
                    });
                    continue;
                }
                Choice::Theirs => target.entries().get(path).cloned(),
                Choice::Content { bytes, executable } => {
                    let mode = |entry: Option<&FileEntry>| match entry {
                        Some(FileEntry::Regular { executable, .. }) => Some(*executable),
                        _ => None,
                    };
                    let executable = executable
                        .or_else(|| mode(scanned.entries.get(path)))
                        .or_else(|| mode(target.entries().get(path)))
                        .unwrap_or(false);
                    Some(FileEntry::Regular {
                        content: self.put_blob(bytes)?,
                        executable,
                    })
                }
            };
            plan.apply.push((path.clone(), change));
        }
        let mut conflicts = plan.conflicts.clone();
        conflicts.extend(structural(&plan.apply)?.0);
        if !conflicts.is_empty() {
            return Err(Error::RefreshConflict(conflicts.into_iter().collect()));
        }
        if !apply {
            report.applied = plan.apply.len();
            report.kept = plan.kept;
            return Ok(report);
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

fn not_a_conflict(path: &RepoPath) -> Error {
    Error::InvalidInput(format!(
        "resolution for {:?}, which is not a conflict",
        path.as_str()
    ))
}

/// Conflicts a head's changes raise against the directory on disk, with the occupied paths and
/// the spellings holding each.
type StructuralConflicts = (BTreeSet<String>, Vec<(String, Vec<String>)>);

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
    // A file and a directory cannot share a path: a head file blocks local paths below it.
    // A local file blocking head paths below it is found on disk by refresh, which names both
    // sides so either may be chosen.
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
    }
    plan
}

/// Head additions that reach an entry an earlier addition already made on this volume: a
/// name that differs only by case or Unicode normalization. The volume decides, so no
/// folding rule is guessed. The names are created in a scratch tree in the store's staging
/// area, never in the workspace, and the tree is removed afterwards.
fn aliased(store: &Store, apply: &[(RepoPath, Option<FileEntry>)]) -> Result<Vec<String>> {
    let added = apply
        .iter()
        .filter(|(_, change)| change.is_some())
        .map(|(path, _)| path);
    Ok(probe_volume(store, added)?
        .into_iter()
        .map(|path| path.as_str().to_owned())
        .collect())
}

/// Paths in `paths` that name an entry another path already made on this volume, in sorted
/// order. Both members of each alias pair are returned: probing forward flags the later
/// spelling, and probing in reverse flags the earlier one. Each probe runs in its own scratch
/// area in the store, which is removed afterwards.
pub(crate) fn probe_volume<'a>(
    store: &Store,
    paths: impl IntoIterator<Item = &'a RepoPath>,
) -> Result<Vec<&'a RepoPath>> {
    let paths: Vec<&RepoPath> = paths.into_iter().collect();
    let mut found = BTreeSet::new();
    found.extend(probe_once(store, paths.iter().copied())?);
    found.extend(probe_once(store, paths.iter().rev().copied())?);
    Ok(found.into_iter().collect())
}

fn probe_once<'a>(
    store: &Store,
    paths: impl IntoIterator<Item = &'a RepoPath>,
) -> Result<Vec<&'a RepoPath>> {
    let scratch = store.staging("alias");
    fs::create_dir_all(&scratch)?;
    let found = probe(&scratch, paths);
    // Our own scratch tree: a failed removal only leaves files the store's recovery trashes.
    let _ = fs::remove_dir_all(&scratch);
    Ok(found)
}

/// Creates each addition's components under `scratch`. A component whose exact name is new
/// but whose entry already exists is an alias of an earlier one. A failure for any other
/// reason is left to the write, which reports it with the path's own error.
fn probe<'a>(scratch: &Path, paths: impl IntoIterator<Item = &'a RepoPath>) -> Vec<&'a RepoPath> {
    let mut dirs = BTreeMap::<String, PathBuf>::new();
    let mut found = Vec::new();
    for path in paths {
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
                    found.push(path);
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

/// Whether `path` lies strictly below `dir`.
fn under(path: &str, dir: &str) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

/// Whether some path in `gone` lies under `dir`.
fn removed_under(gone: &BTreeSet<&str>, dir: &str) -> bool {
    gone.range(dir..)
        .take_while(|path| path.starts_with(dir))
        .any(|path| path.len() > dir.len() && path.as_bytes()[dir.len()] == b'/')
}

/// Whether the removals leave the directory `dir` with nothing in it: every entry under it is
/// removed, or is a directory that the removals empty in turn.
fn emptied(root: &Path, dir: &str, gone: &BTreeSet<&str>) -> Result<bool> {
    for entry in fs::read_dir(native(root, dir))? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(false);
        };
        let path = format!("{dir}/{name}");
        let gone_entry = if entry.file_type()?.is_dir() {
            removed_under(gone, &path) && emptied(root, &path, gone)?
        } else {
            gone.contains(path.as_str())
        };
        if !gone_entry {
            return Ok(false);
        }
    }
    Ok(true)
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
                Ok(meta) if !meta.is_dir() => {
                    blocked.push(parent.to_owned());
                    // The head write below the file is blocked by it, so either may be chosen.
                    blocked.push(path.as_str().to_owned());
                }
                Ok(_) => {}
                // A file above `parent` blocks it too, and the loop reaches that file next.
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
                Err(e) => return Err(e.into()),
            }
            dir = parent;
        }
        // Reaching the path through a file fails with NotADirectory; the ancestor is already
        // reported above, so the path itself counts as free. A directory the removals empty
        // is free too: the removals delete it before the file is written.
        match fs::symlink_metadata(native(root, path.as_str())) {
            Ok(meta)
                if meta.is_dir()
                    && !(removed_under(gone, path.as_str())
                        && emptied(root, path.as_str(), gone)?) =>
            {
                blocked.push(path.as_str().to_owned())
            }
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
/// Each occupied path comes with the spellings that hold it and stay, which the owner may also
/// choose. A spelling the head removes gives the path up, so it does not occupy it.
fn occupied(
    root: &Path,
    apply: &[(RepoPath, Option<FileEntry>)],
    scanned: &BTreeMap<RepoPath, FileEntry>,
    gone: &BTreeSet<&str>,
    holders: &impl Fn(&RepoPath) -> Result<Vec<String>>,
) -> Result<Vec<(String, Vec<String>)>> {
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
                // Only the exact name can be an interrupted write. Matching bytes under another
                // spelling are an alias of a tracked file, which the head must not claim.
                let interrupted = listed_exactly(root, path.as_str())?
                    && entry_at(&full, executable)?.as_ref() == Some(entry);
                if interrupted {
                    continue;
                }
                let spellings = holders(path)?;
                let kept: Vec<String> = spellings
                    .iter()
                    .filter(|name| !gone.contains(name.as_str()))
                    .cloned()
                    .collect();
                // An occupant no spelling accounts for is an ignored file, which stays occupied.
                if spellings.is_empty() || !kept.is_empty() {
                    occupied.push((path.as_str().to_owned(), kept));
                }
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(occupied)
}

/// Names in `path`'s parent, other than its own, that hold the same entry on this volume, as
/// [`probe_volume`] decides. Each is returned by its full path.
fn alias_holders(store: &Store, root: &Path, path: &RepoPath) -> Result<Vec<String>> {
    let text = path.as_str();
    let (parent, leaf) = text.rsplit_once('/').unwrap_or(("", text));
    let listing = match fs::read_dir(native(root, parent)) {
        Ok(entries) => entries.collect::<std::io::Result<Vec<_>>>()?,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.into()),
    };
    let mut holders = Vec::new();
    for entry in listing {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| *name != leaf) else {
            continue;
        };
        let other = if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}/{name}")
        };
        let Ok(other_path) = RepoPath::parse(&other) else {
            continue;
        };
        if !probe_volume(store, [&other_path, path])?.is_empty() {
            holders.push(other);
        }
    }
    Ok(holders)
}

/// Whether the parent's listing names `path`'s leaf exactly. A name that only resolves on this
/// volume is not listed, so it is an alias of an entry the listing names differently.
fn listed_exactly(root: &Path, path: &str) -> Result<bool> {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    match fs::read_dir(native(root, parent)) {
        Ok(entries) => {
            for entry in entries {
                if entry?.file_name() == OsStr::new(name) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(false),
        Err(e) => Err(e.into()),
    }
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
    fn divergent_paths_conflict_while_unrelated_edits_are_kept() {
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
        assert_eq!(names(&plan.conflicts), ["shared"]);
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
        let no_holders = |_: &RepoPath| Ok(Vec::new());
        assert!(
            occupied(root.path(), &apply, &BTreeMap::new(), &gone, &no_holders)
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
        let no_holders = |_: &RepoPath| Ok(Vec::new());
        assert!(
            occupied(root.path(), &same, &BTreeMap::new(), &none, &no_holders)
                .unwrap()
                .is_empty()
        );
        let differs = [(path("new.txt"), file(b"mine"))];
        let found = occupied(root.path(), &differs, &BTreeMap::new(), &none, &no_holders).unwrap();
        assert_eq!(found, [("new.txt".to_owned(), Vec::new())]);
    }
}
