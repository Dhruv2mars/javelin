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
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{Store, WorkspaceRecovery, native, remove_entry, representable, sync_dir};
use crate::{
    CheckpointId, Error, FileEntry, LayerId, LayerState, LayerUpdate, RepoPath, Repository, Result,
    SparseChanges,
    capture::{Capture, StatCache, changes_from, scan, store_blob, write_atomic},
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
        if self.stacked(&head)? && self.synced(layer)? != Some(head.checkpoint()) {
            return Capture::behind(head).map(Some);
        }
        let capture = self.capture_at(head, &root)?;
        // The directory now matches the captured head. A stale update leaves the record as
        // it was, because the directory still matches the checkpoint it was synced with.
        let synced = match &capture.update {
            None => Some(capture.head.checkpoint()),
            Some(LayerUpdate::Applied {
                state: LayerState::Active { head },
            }) => Some(head.checkpoint()),
            _ => None,
        };
        if let Some(checkpoint) = synced {
            self.write_sync(layer, checkpoint)?;
        }
        Ok(Some(capture))
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
        plan.conflicts.extend(blocked(&root, &plan.apply)?);
        if !plan.conflicts.is_empty() {
            return Err(Error::RefreshConflict(plan.conflicts.into_iter().collect()));
        }

        for (path, change) in &plan.apply {
            match change {
                None => remove(&root, path)?,
                Some(entry) => place(&store, &root, path, entry, self)?,
            }
        }
        let mut parents = BTreeSet::new();
        for (path, change) in &plan.apply {
            if change.is_some()
                && let Some(parent) = native(&root, path.as_str()).parent()
            {
                parents.insert(parent.to_path_buf());
            }
        }
        for parent in parents {
            sync_dir(&parent)?;
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

/// Paths a file or directory on disk already blocks: a non-directory where the head needs
/// a parent, or a directory where it needs a file. Symlinked parents also count, because
/// writes never go through links.
fn blocked(root: &Path, apply: &[(RepoPath, Option<FileEntry>)]) -> Result<Vec<String>> {
    let mut blocked = Vec::new();
    for (path, change) in apply {
        if change.is_none() {
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
}
