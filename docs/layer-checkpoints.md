# Native Layer checkpoint foundation

## Bounded outcome and sources

Done means real native API and process tests prove exact immutable World bases,
sparse immutable checkpoints, durable independent Layer heads, snapshot clones,
derived status/diff, discard without World/sibling mutation, restart, corruption
rejection, and concurrent/retried updates. Object counts and byte counts must prove
that unchanged content and cloned checkpoints are shared.

Source contract: `/Users/dhruv2mars/dev/github/javelin/docs/build-contract.md`,
read on 2026-10-14. Native core dependency is hub-accepted
`c78ea7a5d3eb9555625391e2f7346859d70f4c2a`. This PR targets
`chore/native-cross-platform-ci` and inherits its workflow without editing it.
PR6's actual-core platform results are not yet accepted. PR7's Linux filesystem
proof does not supply database binding or automatic capture.

This unit supplies state for a later ordinary-path backend and autosave controller.
It does not prove filesystem isolation, automatic checkpoints, Gate2, Stack,
refresh, whole-Layer Publish, traces, hosting, or hardware power-loss durability.
No manual save/commit command is the intended user workflow. No CLI is added.

## Grounding

`src/domain.rs` validates paths, IDs, canonical manifests and kind-tagged objects.
`src/repository.rs` owns the embedded SQLite store. Object preparation commits
independently; World compare-and-advance uses an immediate transaction and verifies
all accepted history and candidates before recording a durable outcome. Graph
reads use a read transaction. Immutable rows have update/delete rejection triggers.
The accepted base is an exact ordinal plus manifest, not a moving World pointer.
Layers must use this same object store and publication authority.

## Two designs before stateful implementation

Both designs take the same caller input: a frozen base obtained from the repository,
a Layer ID, an expected active head, and changed/deleted paths from future capture.
Both represent regular files and symlinks explicitly and share immutable content.
The designs were compared sequentially in this thread under the user's no-spawn
instruction; no independent model review is claimed.

### Complete sparse delta per checkpoint

Each immutable checkpoint contains repository identity, exact World ordinal and
manifest, and the complete sorted delta relative to that World. Entries are file
replacements or tombstones. Unchanged paths are absent. A clone adds a Layer and
initial head reference to the existing checkpoint. Reads merge base plus delta in
one read transaction. Head updates use an immediate transaction, validate the base
and all referenced objects, and append an immutable head event and retry outcome.
Recovery needs SQLite recovery and graph validation, with no replay algorithm.
Cost is repeated metadata for currently changed paths, never base file bytes.

### Incremental checkpoint chain

Each checkpoint contains a parent checkpoint plus new edits relative to that parent.
Caller capture can submit only edits since the head. Clone still shares one frozen
reference. Reads walk the chain before merging the base. SQL validates parent/base
identity and acyclicity before committing head and retry outcome. Recovery must
validate every ancestor. Long histories need replay limits or compaction, and
retention must preserve all ancestors. This reduces repeated delta metadata but
adds read/recovery rules that this bounded unit does not need.

### Selection and caller sketch

Choose complete sparse deltas. This hides validation and SQL behind the repository
API without a backend framework. The delta is canonicalized against the base so
redundant unchanged entries and nonexistent tombstones do not persist. Merging
must reject file/descendant collisions before a head can advance.

```rust,ignore
let base = repo.layer_base(version)?; // private exact repository/base identity
let head = repo.create_layer(&layer_id, &base)?;
let outcome = repo.capture_layer(&operation_id, &head, &changes)?;
let frozen = repo.read_checkpoint(outcome.head().checkpoint())?;
let child = repo.clone_layer(&child_id, frozen.id())?;
let diff = repo.layer_diff(&child)?; // exact before/after entries, including modes
repo.discard_layer(&child)?;
```

Native types will distinguish validated Layer IDs, frozen bases, checkpoint IDs,
active head tokens and discarded states. SQL remains private. Repository identity
and exact base manifest prevent foreign or stale references from silently selecting
another truth. Frozen checkpoint reads must not consult a moving Layer head.

## Recovery and format plan

Use one versioned embedded format. A format bump is required for explicit symlink
entries and Layer object kinds/tables. Unshipped format 1 is rejected, never silently
rewritten or flattened. Original World objects in a format 2 repository remain
immutable. All foundational tests will be rerun against the new format. No large
prototype compatibility layer is planned. Retain all checkpoint/head history after
discard; collection is outside this unit.

## Verification

Implementation and verification are pending. Platform and durability claims will
be recorded only after inspecting actual results.
