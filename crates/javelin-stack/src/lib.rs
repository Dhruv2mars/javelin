//! Bounded durable Stack from an immutable source checkpoint to an exact target.
//! [`remote`] runs a Stack on a node and refreshes a local Layer from a node's head.
use javelin::{
    CheckpointId, Choice, FileEntry, LayerHead, LayerUpdate, Manifest, ObjectId, OperationId,
    RepoPath, Repository, SparseChanges,
};
use javelin_merge::{Conflict, MergeError, Merged, Resource};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub mod remote;

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
    stack_layer_resolved(repo, operation, target, source, &BTreeMap::new())
}

/// As [`stack_layer`], applying the owner's choice for each conflicting path. Choices are
/// recorded as one immutable resolution object that the Stack binding names, so a retry
/// with the same choices replays the original outcome and different choices are refused as
/// a reused operation ID. A choice for a path that does not conflict is an invalid input.
pub fn stack_layer_resolved(
    repo: &mut Repository,
    operation: &OperationId,
    target: &LayerHead,
    source: CheckpointId,
    choices: &BTreeMap<RepoPath, Choice>,
) -> Result<StackOutcome, StackError> {
    let resolution = match choices.is_empty() {
        true => None,
        false => Some(record_resolution(repo, choices)?),
    };
    if let Some(outcome) = repo.stack_operation_resolved(operation, target, source, resolution)? {
        return Ok(StackOutcome::Updated(outcome));
    }
    let ours = repo.read_checkpoint(target.checkpoint())?;
    let theirs = repo.read_checkpoint(source)?;
    let base = repo.manifest_at(theirs.base().version())?;
    let ours_manifest = repo.checkpoint_manifest(ours.id())?;
    let theirs_manifest = repo.checkpoint_manifest(theirs.id())?;
    let merged = javelin_merge::merge_resolved(
        &base,
        &ours_manifest,
        &theirs_manifest,
        choices,
        |paths| repo.aliased_paths(paths),
        |id| repo.read_blob_bounded(id, Resource::BlobBytes.limit() as usize),
    )
    .map_err(StackError::Merge)?;
    if let Some(path) = merged.unused.first() {
        return Err(javelin::Error::InvalidInput(format!(
            "resolution for {:?}, which is not a conflict",
            path.as_str()
        ))
        .into());
    }
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
    Ok(StackOutcome::Updated(repo.commit_stack_resolved(
        operation, target, source, &changes, resolution,
    )?))
}

/// Stores the choices as one canonical JSON blob. Content is stored by its own blob ID, so
/// the resolution names exactly the bytes the owner supplied.
fn record_resolution(
    repo: &mut Repository,
    choices: &BTreeMap<RepoPath, Choice>,
) -> Result<ObjectId, StackError> {
    let mut resolutions = Vec::new();
    for (path, choice) in choices {
        resolutions.push(match choice {
            Choice::Ours => json!({ "path": path, "choice": "ours" }),
            Choice::Theirs => json!({ "path": path, "choice": "theirs" }),
            Choice::Content { bytes, executable } => {
                let content = repo.put_blob(bytes)?;
                json!({
                    "path": path,
                    "choice": "content",
                    "content": content,
                    "executable": executable,
                })
            }
        });
    }
    let bytes =
        serde_json::to_vec(&json!({ "resolutions": resolutions })).expect("resolution encoding");
    Ok(repo.put_blob(&bytes)?)
}
