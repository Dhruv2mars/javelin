//! Pure three-way merge of native manifests for Stack and owner-controlled refresh.
//! The caller loads content, stores merged bytes, and resolves every conflict.

use std::collections::{BTreeMap, BTreeSet};

use javelin::{FileEntry, Manifest, ObjectId, RepoPath};
use similar::{Algorithm, DiffOp, capture_diff_slices};

/// A resolved path. New bytes have no object ID until the caller stores them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Merged {
    Retained(FileEntry),
    Content { bytes: Vec<u8>, executable: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictKind {
    /// Both sides changed one region of a regular file that no unchanged line separates.
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
    /// The first path in sorted order that this path would be a directory of, or a
    /// file under, if both were kept. Always set for PathPrefix.
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
            Path::Unresolved(kind) => outcome
                .conflicts
                .push(conflict(kind, collisions.get(&path).cloned())),
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
        return Ok(match merge_lines(&b, &o, &t) {
            Some(bytes) => Path::Resolved(Merged::Content { bytes, executable }),
            None => Path::Unresolved(ConflictKind::Content),
        });
    };
    Ok(Path::Resolved(Merged::Retained(FileEntry::Regular {
        content,
        executable,
    })))
}

fn merge_lines(base: &[u8], ours: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    diff3(&lines(base), &lines(ours), &lines(theirs)).map(|merged| merged.concat())
}

/// diff3 without convergence: base lines matched unchanged on both sides are
/// anchors. Each region between anchors is taken whole from the only side that
/// changed it. A region both sides changed conflicts, even if identically, because
/// with repeated lines an apparent shared edit can be an artifact of the matching.
/// A clean result therefore applies one complete edit script from each side to
/// disjoint base lines separated by unchanged lines.
fn diff3<'a>(base: &[&'a [u8]], ours: &[&'a [u8]], theirs: &[&'a [u8]]) -> Option<Vec<&'a [u8]>> {
    let (in_ours, in_theirs) = (matches(base, ours), matches(base, theirs));
    let (mut b, mut o, mut t) = (0, 0, 0);
    let mut out = Vec::new();
    loop {
        let anchor = (b..base.len()).find_map(|i| Some((i, in_ours[i]?, in_theirs[i]?)));
        let (nb, no, nt) = anchor.unwrap_or((base.len(), ours.len(), theirs.len()));
        let (region, mine, other) = (&base[b..nb], &ours[o..no], &theirs[t..nt]);
        let taken = if mine == region {
            other
        } else if other == region {
            mine
        } else {
            return None;
        };
        out.extend_from_slice(taken);
        let Some((nb, no, nt)) = anchor else {
            return Some(out);
        };
        debug_assert!(base[nb] == ours[no] && base[nb] == theirs[nt]);
        out.push(base[nb]);
        (b, o, t) = (nb + 1, no + 1, nt + 1);
    }
}

/// Lines keep their exact terminator; a final line may have none.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|byte| *byte == b'\n').collect()
}

/// For each base line, the index of the line it is matched to as unchanged.
/// No deadline is passed, so the result never depends on time.
fn matches(base: &[&[u8]], other: &[&[u8]]) -> Vec<Option<usize>> {
    let mut matched = vec![None; base.len()];
    for op in capture_diff_slices(Algorithm::Myers, base, other) {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for i in 0..len {
                matched[old_index + i] = Some(new_index + i);
            }
        }
    }
    matched
}
