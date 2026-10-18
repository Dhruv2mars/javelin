//! Bounded durable Stack from an immutable source checkpoint to an exact target.
use javelin::{
    CheckpointId, FileEntry, LayerHead, LayerUpdate, Manifest, OperationId, Repository,
    SparseChanges,
};
use javelin_merge::{Conflict, MergeError, Merged, Resource};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StackOutcome {
    Updated(LayerUpdate),
    Conflicts(Vec<Conflict>),
}

#[derive(Debug)]
pub enum StackError {
    Storage(javelin::Error),
    Merge(MergeError<javelin::Error>),
}
impl From<javelin::Error> for StackError {
    fn from(error: javelin::Error) -> Self {
        Self::Storage(error)
    }
}
impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(f),
            Self::Merge(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for StackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Storage(error) => error,
            Self::Merge(error) => error,
        })
    }
}

/// Imports only source changes relative to its frozen base, preserving the target
/// base and unrelated private/World content. Conflicts reserve no operation ID;
/// owners resolve explicitly and use a new exact head/source. Success and stale
/// retries return their original outcome, even after later edits/discard/Publish.
pub fn stack_layer(
    repo: &mut Repository,
    operation: &OperationId,
    target: &LayerHead,
    source: CheckpointId,
) -> Result<StackOutcome, StackError> {
    if let Some(outcome) = repo.stack_operation(operation, target, source)? {
        return Ok(StackOutcome::Updated(outcome));
    }
    let ours = repo.read_checkpoint(target.checkpoint())?;
    let theirs = repo.read_checkpoint(source)?;
    let base = repo.manifest_at(theirs.base().version())?;
    let ours_manifest = repo.checkpoint_manifest(ours.id())?;
    let theirs_manifest = repo.checkpoint_manifest(theirs.id())?;
    let merged = javelin_merge::merge(&base, &ours_manifest, &theirs_manifest, |id| {
        repo.read_blob_bounded(id, Resource::BlobBytes.limit() as usize)
    })
    .map_err(StackError::Merge)?;
    if !merged.is_clean() {
        return Ok(StackOutcome::Conflicts(merged.conflicts));
    }
    // No mutation until the entire bounded merge is clean. New immutable blobs
    // may be left unreferenced if another writer wins before the target CAS.
    let mut entries = Vec::new();
    for (path, result) in merged.merged {
        let entry = match result {
            Merged::Retained(entry) => entry,
            Merged::Content { bytes, executable } => FileEntry::Regular {
                content: repo.put_blob(&bytes)?,
                executable,
            },
        };
        entries.push((path, entry));
    }
    let merged = Manifest::new(entries)?;
    let target_base = repo.manifest_at(ours.base().version())?;
    let paths: BTreeSet<_> = target_base
        .entries()
        .keys()
        .chain(merged.entries().keys())
        .collect();
    let changes = SparseChanges::new(paths.into_iter().filter_map(|path| {
        let after = merged.entries().get(path);
        (target_base.entries().get(path) != after).then(|| (path.clone(), after.cloned()))
    }))?;
    Ok(StackOutcome::Updated(
        repo.commit_stack(operation, target, source, &changes)?,
    ))
}
