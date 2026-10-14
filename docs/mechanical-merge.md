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
    pub with: Option<RepoPath>, // the colliding path for PathPrefix
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
byte is `Binary`. Otherwise `diffy::merge_bytes` performs a line merge. A clean
merge yields `Merged::Content`. A conflicted merge is `Content`; diffy's marker
rendering is discarded and never offered as content.

Lines are exact byte sequences ending in `\n`. CRLF and LF lines differ, so
rewriting line endings conflicts with any concurrent edit. A missing final
newline is part of the last line. Edits to the same or adjacent lines conflict.
Identical edits converge. Different insertions at the same point conflict.
Non-UTF-8 text without NUL bytes still merges by line.

Structural pass: every resolved path, and every conflicted path that either
side still has, is a candidate. A resolved path that is a directory prefix of,
or lies under, another candidate becomes a `PathPrefix` conflict naming the first
collision. Already conflicted paths keep their original kind.

## Design choice

Two shapes were compared, plus a zero-dependency variant:

- A: per-path rules above, `diffy` 0.5.2 for line merges, per-file conflicts.
- B: the same rules, `similar` for line diffs plus bespoke diff3 chunking that
  returns structured hunk ranges.
- C: bespoke Myers diff and diff3 with no dependencies.

A was selected as the smallest correct shape. Primary source reviewed: diffy
0.5.2 `src/merge/mod.rs` and `src/utils.rs` from the crates.io package. Findings:
both-side insertions at one point form a `Conflict` (`create_merge_range` with no
ancestor and both sides); any change regions not separated by a common line
merge into one region; `cleanup_conflicts` only resolves regions where both sides
are equal or one side equals the ancestor. The classifier assigns sequential IDs
through a hash map that is never iterated, so output is deterministic. Default
features add one dependency, `hashbrown`. License: MIT OR Apache-2.0.

B would give hunk-level records, but it means owning diff3 chunking code.
C also means owning a diff algorithm. Owners already receive all three exact
entries, which is enough to resolve a file. Hunk ranges can be added later
without changing the per-path rules.

## Evidence

Integration tests (`tests/repository_merge.rs`) create a native repository,
publish World 1, capture each side in a real Layer, and merge the checkpoint
manifests. Clean results are stored and read back through `put_blob` and
`put_manifest`. Sixteen tests cover disjoint path and line edits, overlapping and
adjacent edits, insert/insert, convergence, add/delete/modify, add/add, CRLF,
missing final newline, binary refusal, non-UTF-8 text, executable bits, symlink
target and type changes, prefix collisions including unresolved paths,
determinism, symmetry, and loader errors.

`scripts/mutation-check.ts` removes nine guards one at a time and requires
`cargo test` to fail each time. It requires the unmutated tests to pass first.

`.github/workflows/mechanical-merge.yml` runs formatting, Clippy, tests, the
release example, and the mutation check on Linux, macOS, and Windows, retaining
logs as artifacts.

## Limits

- Conflict records are per file. They do not include hunk ranges.
- Binary detection is a NUL-byte heuristic. A NUL-free binary format is line-merged.
- Line ending rewrites are not normalized. Their conflicts are intentional.
- Rename detection is out of scope. A rename is a delete plus an add.
- Large-file cost is unbounded. diffy holds three full versions in memory.
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
