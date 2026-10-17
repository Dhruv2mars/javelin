//! Pure three-way merge of native manifests for Stack and owner-controlled refresh.
//! The caller loads content, stores merged bytes, and resolves every conflict.

use std::collections::{BTreeMap, BTreeSet};

use javelin::{FileEntry, Manifest, ObjectId, RepoPath};

mod matching;
mod resources;
use matching::matches;
pub use resources::{MergeError, Resource, ResourceLimit, Usage};

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
    pub usage: Usage,
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
) -> Result<MergeOutcome, MergeError<E>> {
    let (b, o, t) = (base.entries(), ours.entries(), theirs.entries());
    Resource::Entries.check((b.len() + o.len() + t.len()) as u64)?;
    let mut usage = Usage::default();
    let mut path_bytes = 0;
    let mut metadata_work = 0;
    for path in b.keys().chain(o.keys()).chain(t.keys()) {
        let len = path.as_str().len() as u64;
        Resource::PathBytes.check(len)?;
        path_bytes += len;
        Resource::TotalPathBytes.check(path_bytes)?;
        // Covers path comparisons/clones and scanning/parsing every slash prefix
        // in the structural pass, including bounded BTree lookups.
        metadata_work += 64 * (len + 1) * (len + 1);
    }
    usage.reserve(metadata_work)?;
    let paths: BTreeSet<&RepoPath> = b.keys().chain(o.keys()).chain(t.keys()).collect();
    let mut results = BTreeMap::new();
    for path in paths {
        let sides = (b.get(path), o.get(path), t.get(path));
        let result = merge_path(sides, &mut load, &mut usage)?;
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

    let mut outcome = MergeOutcome {
        usage,
        ..MergeOutcome::default()
    };
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
    usage: &mut Usage,
) -> Result<Path, MergeError<E>> {
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
        let mut bounded_load = |id| {
            let bytes = load(id).map_err(MergeError::Load)?;
            usage.loaded(bytes.len())?;
            Ok::<_, MergeError<E>>(bytes)
        };
        // Check each returned allocation before invoking the next loader.
        let (b, o, t) = (bounded_load(*b)?, bounded_load(*o)?, bounded_load(*t)?);
        if [&b, &o, &t].iter().any(|bytes| bytes.contains(&0)) {
            return Ok(Path::Unresolved(ConflictKind::Binary));
        }
        return Ok(match merge_lines(&b, &o, &t, usage)? {
            Some(bytes) => Path::Resolved(Merged::Content { bytes, executable }),
            None => Path::Unresolved(ConflictKind::Content),
        });
    };
    Ok(Path::Resolved(Merged::Retained(FileEntry::Regular {
        content,
        executable,
    })))
}

fn merge_lines(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    usage: &mut Usage,
) -> Result<Option<Vec<u8>>, ResourceLimit> {
    let bytes = [base, ours, theirs];
    let mut counts = [0u64; 3];
    for (count, bytes) in counts.iter_mut().zip(bytes) {
        *count = bytes.iter().filter(|b| **b == b'\n').count() as u64
            + u64::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
        Resource::Lines.check(*count)?;
    }
    let [n, m, k] = counts;
    let [b, o, t] = bytes.map(|b| b.len() as u64);
    let mut work = 0;
    for (side_lines, side_bytes) in [(m, o), (k, t)] {
        let cells = (n + 1) * (side_lines + 1);
        Resource::TableCells.check(cells)?;
        // Prefix, suffix, omission scan, zeroing, traceback; byte equality is
        // bounded by all-pair shorter-line lengths, conservatively summed here.
        work += 5 * cells + 3 * (n * side_bytes).min(side_lines * b) + 4 * (n + side_lines + 1);
    }
    // At most n uniqueness scans of all lines/bytes. Regions and output are
    // linear in the admitted text, including vector growth and byte copies.
    work += n * (n + m + k + b + o + t) + 16 * (n + m + k + b + o + t + 1);
    usage.reserve(work)?;
    Ok(diff3(&lines(base), &lines(ours), &lines(theirs)).map(|merged| merged.concat()))
}

/// A region's changing side (true is ours), its merged lines, and the anchor after it.
type Step<'a, 'r> = (Option<bool>, &'r [&'a [u8]], Option<usize>);

/// diff3 without convergence: base lines matched unchanged on both sides are
/// anchors. Each region between anchors is taken whole from the only side that
/// changed it; a region both sides changed conflicts, even if identically.
///
/// A change can often be matched at several places, so only an anchor that every
/// minimal diff of both sides keeps unchanged, and that occurs exactly once in base,
/// ours, and theirs, separates the two sides' changes. Across any other anchor one
/// side's edit could equally sit on the other side, through repeated lines or moved
/// blocks, and merging could drop, duplicate, or reorder an edit.
fn diff3<'a>(base: &[&'a [u8]], ours: &[&'a [u8]], theirs: &[&'a [u8]]) -> Option<Vec<&'a [u8]>> {
    let (in_ours, ours_kept) = matches(base, ours);
    let (in_theirs, theirs_kept) = matches(base, theirs);
    let mut steps: Vec<Step> = Vec::new();
    let (mut b, mut o, mut t) = (0, 0, 0);
    loop {
        let anchor = (b..base.len()).find_map(|i| Some((i, in_ours[i]?, in_theirs[i]?)));
        let (nb, no, nt) = anchor.unwrap_or((base.len(), ours.len(), theirs.len()));
        let (region, mine, other) = (&base[b..nb], &ours[o..no], &theirs[t..nt]);
        let (side, taken) = match (mine == region, other == region) {
            (true, true) => (None, region),
            (false, true) => (Some(true), mine),
            (true, false) => (Some(false), other),
            (false, false) => return None,
        };
        let line = anchor.map(|(nb, no, nt)| {
            debug_assert!(base[nb] == ours[no] && base[nb] == theirs[nt]);
            nb
        });
        steps.push((side, taken, line));
        let Some((nb, no, nt)) = anchor else { break };
        (b, o, t) = (nb + 1, no + 1, nt + 1);
    }

    let unique = |line: &[u8]| {
        [base, ours, theirs]
            .iter()
            .all(|lines| lines.iter().filter(|l| **l == line).count() == 1)
    };
    let kept = |i: usize| ours_kept[i] && theirs_kept[i];
    let (mut last, mut separated) = (None, false);
    let mut out = Vec::new();
    for (side, taken, line) in steps {
        if side.is_some() {
            if last.is_some() && last != side && !separated {
                return None;
            }
            (last, separated) = (side, false);
        }
        out.extend_from_slice(taken);
        if let Some(i) = line {
            separated = separated || (unique(base[i]) && kept(i));
            out.push(base[i]);
        }
    }
    Some(out)
}

/// Lines keep their exact terminator; a final line may have none.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|byte| *byte == b'\n').collect()
}
