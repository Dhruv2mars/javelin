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
byte is `Binary`. Otherwise a classic diff3 line merge runs:

1. `similar` (Myers, no deadline) matches base lines to each side.
2. Base lines matched unchanged on both sides are anchors.
3. Each region between anchors is taken whole from the only side that changed
   it, or once when both sides made it byte-identical. Any other region makes
   the file a `Content` conflict, and no bytes are offered.

Lines are exact byte sequences ending in `\n`. CRLF and LF lines differ, so
rewriting line endings conflicts with any concurrent edit. A missing final
newline is part of the last line. Edits not separated by an unchanged common line,
including edits to adjacent lines, conflict. Identical edits converge only when
the whole region is identical. Different insertions at the same point conflict.
Non-UTF-8 text without NUL bytes still merges by line.

Soundness does not depend on a minimal diff. Any valid matching yields regions
that each come wholly from one side, so a non-minimal diff can only enlarge a
conflict. It never invents or drops an edit.

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
unsound. diffy compacts each side's diff, and its private compaction moves a
deletion away from the matching insertion. Its merge then splits change regions at
common sub-ranges. With repeated lines this gives clean results that are wrong.
Base `a a a`, ours `a x a`, and theirs `x a a` merged cleanly to `x a x a`, an
invented line. Base `b b b b b`, ours `b b z b b`, and theirs with one `b` deleted
merged cleanly to ours, dropping the deletion. diffy exposes no option to
disable compaction (source: `diffy-0.5.2/src/merge/mod.rs` and
`src/diff/mod.rs`, crates.io package). Both cases are now regressions in
`tests/repeated_lines.rs`.

B was selected. The anchor rule is about 30 lines and is sound for any matching.
`similar` supplies a mature diff with no default features and no dependencies.
Its source says `Algorithm::Myers` may use bounded non-minimal splits, which here
only affect conflict size. With no deadline the output does not depend on time
(source: `similar-3.2.0/src/algorithms/mod.rs`, `src/common.rs`, `src/types.rs`).
License: Apache-2.0. C would add a diff algorithm to own for no correctness gain.

Conflict records are per file. Hunk ranges can be added later from the same
regions without changing any rule.

## Evidence

Integration tests (`tests/repository_merge.rs`) create a native repository,
publish World 1, capture each side in a real Layer, and merge the checkpoint
manifests. Clean results are stored and read back through `put_blob` and
`put_manifest`. Sixteen tests cover disjoint path and line edits, overlapping and
adjacent edits, insert/insert, convergence, add/delete/modify, add/add, CRLF,
missing final newline, binary refusal, non-UTF-8 text, executable bits, symlink
target and type changes, prefix collisions including unresolved paths,
determinism, symmetry, and loader errors.

`tests/repeated_lines.rs` runs in memory with a fixed generator. It has the two
review regressions and 20,000 generated cases over repetitive bases of `a`, `b`,
and `}` lines. In each case each side makes one replacement, insertion, or
deletion with a unique token. Every clean result must have the exact implied
line count and each surviving token exactly once.

`scripts/mutation-check.ts` removes twelve guards one at a time and requires
`cargo test` to fail each time. It requires the unmutated tests to pass first.

`.github/workflows/mechanical-merge.yml` runs formatting, Clippy, tests, the
release example, and the mutation check on Linux, macOS, and Windows, retaining
logs as artifacts.

## Limits

- Conflict records are per file. They do not include hunk ranges.
- Binary detection is a NUL-byte heuristic. A NUL-free binary format is line-merged.
- Line ending rewrites are not normalized. Their conflicts are intentional.
- Rename detection is out of scope. A rename is a delete plus an add.
- Large files are held fully in memory, three versions at once. Diff cost on
  pathological inputs is bounded only by `similar`'s Myers heuristics.
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
