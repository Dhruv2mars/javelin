//! Pure three-way merge of native manifests for Stack and owner-controlled refresh.
//! The caller loads content, stores merged bytes, and resolves every conflict.

use std::collections::{BTreeMap, BTreeSet};

use javelin::{FileEntry, Manifest, ObjectId, RepoPath};

/// A resolved path. New bytes have no object ID until the caller stores them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Merged {
    Retained(FileEntry),
    Content { bytes: Vec<u8>, executable: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictKind {
    /// Both sides changed overlapping or adjacent lines of a regular file.
    Content,
    /// Both sides changed a regular file whose content is not line-mergeable text.
    Binary,
    /// One side deleted the path and the other changed it.
    ModifyDelete,
    /// Both sides added different entries at a path absent from the base.
    AddAdd,
    /// Both sides changed the path differently and at least one entry is a symlink.
    EntryType,
    /// A resolved path would be a directory of, or a file under, another path.
    PathPrefix,
}

/// Unresolved path with the exact entries the owner chooses between.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub path: RepoPath,
    pub kind: ConflictKind,
    pub base: Option<FileEntry>,
    pub ours: Option<FileEntry>,
    pub theirs: Option<FileEntry>,
    /// For PathPrefix, the first colliding path in sorted order.
    pub with: Option<RepoPath>,
}

/// Paths absent from `merged` and `conflicts` are deleted in the result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MergeOutcome {
    pub merged: BTreeMap<RepoPath, Merged>,
    pub conflicts: Vec<Conflict>,
}

impl MergeOutcome {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

enum Path {
    Resolved(Merged),
    Deleted,
    Unresolved(ConflictKind),
}

/// Merges `ours` and `theirs` relative to `base`. `load` is called, in sorted path
/// order, only for regular-file content that both sides changed.
pub fn merge<E>(
    base: &Manifest,
    ours: &Manifest,
    theirs: &Manifest,
    mut load: impl FnMut(ObjectId) -> Result<Vec<u8>, E>,
) -> Result<MergeOutcome, E> {
    let (b, o, t) = (base.entries(), ours.entries(), theirs.entries());
    let paths: BTreeSet<&RepoPath> = b.keys().chain(o.keys()).chain(t.keys()).collect();
    let mut results = BTreeMap::new();
    for path in paths {
        let sides = (b.get(path), o.get(path), t.get(path));
        let result = merge_path(sides, &mut load)?;
        results.insert(path.clone(), (result, sides));
    }

    // Any path either side still has could end up in the owner's result.
    let present: BTreeSet<&RepoPath> = results
        .iter()
        .filter(|(_, (result, (_, o, t)))| match result {
            Path::Resolved(_) => true,
            Path::Deleted => false,
            Path::Unresolved(_) => o.is_some() || t.is_some(),
        })
        .map(|(path, _)| path)
        .collect();
    let mut collisions: BTreeMap<RepoPath, RepoPath> = BTreeMap::new();
    for path in &present {
        let text = path.as_str();
        for (index, _) in text.match_indices('/') {
            let parent = RepoPath::parse(&text[..index]).expect("prefix of a valid path");
            if present.contains(&parent) {
                collisions
                    .entry((*path).clone())
                    .or_insert_with(|| parent.clone());
                collisions.entry(parent).or_insert_with(|| (*path).clone());
            }
        }
    }

    let mut outcome = MergeOutcome::default();
    for (path, (result, (b, o, t))) in results {
        let conflict = |kind, with| Conflict {
            path: path.clone(),
            kind,
            base: b.cloned(),
            ours: o.cloned(),
            theirs: t.cloned(),
            with,
        };
        match result {
            Path::Deleted => {}
            Path::Unresolved(kind) => outcome.conflicts.push(conflict(kind, None)),
            Path::Resolved(merged) => match collisions.get(&path) {
                Some(other) => outcome
                    .conflicts
                    .push(conflict(ConflictKind::PathPrefix, Some(other.clone()))),
                None => {
                    outcome.merged.insert(path, merged);
                }
            },
        }
    }
    Ok(outcome)
}

type Sides<'a> = (
    Option<&'a FileEntry>,
    Option<&'a FileEntry>,
    Option<&'a FileEntry>,
);

/// Resolves one path from exact entry equality, then line content when both edit text.
fn merge_path<E>(
    (base, ours, theirs): Sides<'_>,
    load: &mut impl FnMut(ObjectId) -> Result<Vec<u8>, E>,
) -> Result<Path, E> {
    let take = |entry: Option<&FileEntry>| match entry {
        Some(entry) => Path::Resolved(Merged::Retained(entry.clone())),
        None => Path::Deleted,
    };
    if ours == theirs || theirs == base {
        return Ok(take(ours));
    }
    if ours == base {
        return Ok(take(theirs));
    }
    let (Some(base), Some(ours), Some(theirs)) = (base, ours, theirs) else {
        return Ok(Path::Unresolved(if base.is_none() {
            ConflictKind::AddAdd
        } else {
            ConflictKind::ModifyDelete
        }));
    };
    let (
        FileEntry::Regular {
            content: b,
            executable: bx,
        },
        FileEntry::Regular {
            content: o,
            executable: ox,
        },
        FileEntry::Regular {
            content: t,
            executable: tx,
        },
    ) = (base, ours, theirs)
    else {
        return Ok(Path::Unresolved(ConflictKind::EntryType));
    };
    // A bit changed by both sides has the same value, so one side always wins cleanly.
    let executable = if ox == bx { *tx } else { *ox };
    let content = if o == t || t == b {
        *o
    } else if o == b {
        *t
    } else {
        let (b, o, t) = (load(*b)?, load(*o)?, load(*t)?);
        if [&b, &o, &t].iter().any(|bytes| bytes.contains(&0)) {
            return Ok(Path::Unresolved(ConflictKind::Binary));
        }
        return Ok(match diffy::merge_bytes(&b, &o, &t) {
            Ok(bytes) => Path::Resolved(Merged::Content { bytes, executable }),
            // The marker rendering is discarded; markers are never accepted content.
            Err(_) => Path::Unresolved(ConflictKind::Content),
        });
    };
    Ok(Path::Resolved(Merged::Retained(FileEntry::Regular {
        content,
        executable,
    })))
}
