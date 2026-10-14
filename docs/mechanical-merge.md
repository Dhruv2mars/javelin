# Mechanical merge engine

`crates/javelin-merge` is a pure, deterministic three-way merge of native
manifests. It is the mechanical core for a future Stack and owner-controlled
refresh. It does not write files, persist Stack state, mint object IDs, or
resolve conflicts on anyone's behalf.

Source baseline: `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, the hub-accepted
bounded Layer state dependency. That dependency is not merged. This component
depends on it and claims no independent acceptance.

## Caller API

```rust
pub fn merge<E>(
    base: &Manifest,
    ours: &Manifest,
    theirs: &Manifest,
    load: impl FnMut(ObjectId) -> Result<Vec<u8>, E>,
) -> Result<MergeOutcome, E>;

pub struct MergeOutcome {
    pub merged: BTreeMap<RepoPath, Merged>, // absent paths are deleted
    pub conflicts: Vec<Conflict>,           // sorted by path
}
pub enum Merged {
    Retained(FileEntry),                        // an existing object, unchanged
    Content { bytes: Vec<u8>, executable: bool }, // caller stores via put_blob
}
pub struct Conflict {
    pub path: RepoPath,
    pub kind: ConflictKind,
    pub base: Option<FileEntry>,
    pub ours: Option<FileEntry>,
    pub theirs: Option<FileEntry>,
    pub with: Option<RepoPath>, // first path this one would collide with
}
pub enum ConflictKind { Content, Binary, ModifyDelete, AddAdd, EntryType, PathPrefix }
```

The library uses only the public root API: `Manifest::entries`, `FileEntry`,
`RepoPath`, and `ObjectId`. It never hashes, decodes manifests, or reads storage.
A caller passes `|id| repo.read_blob(id)` as the loader. The loader runs only when
both sides changed a regular file's content, in sorted path order. Its error is
returned unchanged.

A clean result becomes a manifest only through the caller:
`put_blob` for each `Merged::Content`, then `Manifest::new` and `put_manifest`.
`examples/stack_preview.rs` does exactly that with two real Layer checkpoints.
A result with conflicts has no manifest. The owner decides each conflicted path
from the exact base, ours, and theirs entries.

## Rules

Per path, compared by exact `FileEntry` equality:

| Case | Result |
| --- | --- |
| ours equals theirs, or theirs equals base | ours (retained, or deleted) |
| ours equals base | theirs (retained, or deleted) |
| both changed, absent from base | `AddAdd` |
| both changed, one side deleted | `ModifyDelete` |
| both changed, any symlink involved | `EntryType` (targets are never line-merged) |
| both regular | executable bit and content merge separately |

Executable bit: the side that changed it wins. If both changed it, both have
the same value. Content: if only one side changed it, that object is retained.
If both changed it, the three versions are loaded. Any version containing a NUL
byte is `Binary`. Otherwise a diff3 line merge without convergence runs:

1. `similar` (`RawMyers`, exact, no deadline) diffs base lines against each side.
2. Base lines kept unchanged on both sides are anchors.
3. Each region between anchors is taken whole from the only side that changed
   it. A region both sides changed is a `Content` conflict, even when both made
   it identical, and no bytes are offered.
4. Changes from different sides must be separated by at least one anchor line
   that occurs exactly once in base, ours, and theirs, and that every minimal diff
   of each side keeps unchanged (removing it from base shortens the longest common
   subsequence with that side). Otherwise the file is a `Content` conflict.

Lines are exact byte sequences ending in `\n`. CRLF and LF lines differ, so
rewriting line endings conflicts with any concurrent edit. A missing final
newline is part of the last line. Edits from the two sides conflict unless a unique
unchanged line lies between them. This covers adjacent lines, different insertions
at one point, and edits within runs of repeated lines such as blanks or braces.
Non-UTF-8 text without NUL bytes still merges by line.

Identical changes converge only per path: two sides with the same content object
are not loaded or merged. Inside a file, a shared edit next to a one-sided edit is
left to the owner.

Why unique separators: with repeated lines an edit can be matched at several
places. In review, converging an apparent shared edit dropped a real edit. Merging
across a repeated anchor applied a shared fix twice, removing two braces where each
side removed one. In a fourth round a block swap left a unique line unmatched in
one of two equally minimal diffs, so the result depended on which block Myers kept
in place. A line that occurs once in every version and that every minimal diff
keeps is matched the same way by all of them. Any such diff reads the changes on
each side of it as staying on that side.

Guarantee, as implemented: each clean region is copied from base or from exactly
one side. Regions are read from `similar`'s exact `RawMyers` diffs. Where a
region's content equals base it counts as unchanged. Two sides' changes are always
separated by an unchanged line that is unique in all three versions and kept by
every minimal diff of both sides. This is checked empirically, not proven. A property test requires every clean result to equal the outcome of
every pair of minimal diffs of the two sides (see Evidence). The rule is
conservative and trades clean merges near repeated lines for explicit conflicts.

Structural pass: every resolved path, and every conflicted path that either
side still has, is a candidate. A resolved path that is a directory prefix of,
or lies under, another candidate becomes a `PathPrefix` conflict. Conflicted
paths keep their original kind. Both record the first colliding path in `with`.

## Design choice

Three shapes were compared:

- A: `diffy` 0.5.2 `merge_bytes` for line merges.
- B: `similar` 3.2.0 line diffs plus classic diff3 anchor chunking.
- C: a bespoke Myers diff plus diff3 with no dependencies.

A was implemented first and rejected after an independent review proved it
unsound. Base `a a a`, ours `a x a`, and theirs `x a a` merged cleanly to
`x a x a`, an invented line. Base `b b b b b`, ours `b b z b b`, and theirs with
one `b` deleted merged cleanly to ours, dropping the deletion. The cause is how
diffy forms regions. It merges per aligned sub-range, so after its diff
compaction slides edits among identical lines, one side's deletion can pair with
the other side's and count once (source: `diffy-0.5.2/src/merge/mod.rs`,
`merge_solutions` and `cleanup_conflicts`, crates.io package). Compaction is not
unique to diffy: `similar`'s `capture_diff_slices` also compacts and trims
(`similar-3.2.0/src/common.rs`). Both cases are now regressions in
`tests/repeated_lines.rs`.

B was selected. A second review round showed that B with whole-region convergence
could still drop an edit. Base `a`, blank, `}`, `b`: ours replaced the blank with
`}` and theirs added a `}`. The diff read both as inserting `}` at one point, so
it was kept once. Convergence inside files was removed. A third round showed a
shared fix inside a run of braces applied twice across a repeated anchor. That led
to the unique-separator rule. A fourth round added the kept-by-every-minimal-diff
condition and switched to exact `RawMyers`. The line merge is about 70 lines.
Its separator check costs one extra exact diff per side per candidate anchor. `similar` has no default features and adds no dependencies. `RawMyers` computes a
shortest edit script without heuristics; its source warns it can be slow on
pathological inputs. Without a deadline the output does not depend on time (source:
`similar-3.2.0/src/algorithms/mod.rs`, `src/common.rs`, `src/types.rs`).
License: Apache-2.0. C would add a diff algorithm to own for no correctness gain.

Conflict records are per file. Hunk ranges can be added later from the same
regions without changing any rule.

## Evidence

Integration tests (`tests/repository_merge.rs`) create a native repository,
publish World 1, capture each side in a real Layer, and merge the checkpoint
manifests. Clean results are stored and read back through `put_blob` and
`put_manifest`. Sixteen tests cover disjoint path and line edits, overlapping and
adjacent edits, insert/insert, per-path convergence, partial convergence left as
a conflict, add/delete/modify, add/add, CRLF,
missing final newline, binary refusal, non-UTF-8 text, executable bits, symlink
target and type changes, prefix collisions including unresolved paths,
determinism, symmetry, and loader errors.

`tests/repeated_lines.rs` runs in memory with fixed generators and no Git. It
holds the review regressions (F1, N1, R3-1, R4-1) and two property tests:

- 20,000 cases over repetitive `a`, `b`, and `}` bases, with each side making one
  edit with a unique token. A clean result must have the implied line count and
  each surviving token once.
- 20,000 cases with bases over `a`, blank, and `}`, and edits over those plus `x`.
  Each side makes one or two edits. An independent oracle lists every minimal
  diff of each side from its own LCS table and does not use `similar`. A clean
  result requires that every minimal diff of ours, paired with every minimal diff
  of theirs, deletes no common line, inserts at no common point, and produces
  exactly the result. This checks content and order, and that the result does
  not depend on the matching. Over 3,000 cases must be clean.

A one-off local run of the same oracle used 200,000 cases over a six-symbol
alphabet, bases up to seven lines, up to two edits per side, and a one-in-four
block rotation per side, in a /tmp copy. It found no violation in 103,017 clean
results. That run is not part of CI.

`scripts/mutation-check.ts` applies sixteen guard mutations one at a time. They
include region convergence, region reordering, and weakened separator rules. Each
mutant must compile, and `cargo test` must then fail. The unmutated tests must
pass first.

`.github/workflows/mechanical-merge.yml` runs formatting, Clippy, tests, the
release example, and the mutation check on Linux, macOS, and Windows, retaining
logs as artifacts.

## Limits

- Conflict records are per file. They do not include hunk ranges.
- Identical edits inside a file next to other edits conflict. A cherry-picked fix
  plus other work therefore needs owner resolution.
- Files whose two sides' edits have no unique line between them conflict, even
  when a human would merge them. This is common in files of repeated lines.
- The oracle covers small generated inputs. It is evidence, not a proof for all
  inputs.
- Binary detection is a NUL-byte heuristic. A NUL-free binary format is line-merged.
- Line ending rewrites are not normalized. Their conflicts are intentional.
- Rename detection is out of scope. A rename is a delete plus an add.
- Large files are held fully in memory, three versions at once. Exact `RawMyers`
  has no cost bound on pathological inputs, and each candidate separator costs two
  more exact diffs. Measured once on an Apple Silicon laptop, release build,
  2,000-line files: far-apart edits 0.3 ms; one side reversed 176 ms; both sides
  rewriting 20% of lines scattered throughout 2.75 s. The scattered case is the
  separator cost. A forward/backward LCS table would make it one pass, which is
  future work if real repositories hit it.
- No semantic language-aware merging exists, by design.

## Future gated adapter

These need hub-approved core work and are not implemented here.

- Stack into a Layer: with base = the Layer's frozen World manifest, ours = the
  target Layer checkpoint, theirs = the source checkpoint, the caller stores new
  bytes and converts `merged` into `SparseChanges` relative to the target's base.
  Both Layers must share the same frozen World; otherwise refresh one first.
  Deletions must be explicit tombstones for every removed file path. Capture uses
  the target head as the expected head, so a concurrent edit returns `Stale`.
- Owner-controlled refresh: base = old World, ours = Layer checkpoint, theirs =
  new World. Javelin reports conflicts. Only the owner applies a resolution.
  Nonintersecting World advances still need no refresh.
- Whole-Layer Publish: merge output is never published directly. A resolved Layer
  is published whole through the World compare-and-advance path.
- Stack persistence, conflict-state storage, and partial resolution records are
  separate core work. They must not reuse textual conflict markers.
