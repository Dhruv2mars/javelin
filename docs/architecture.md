# Architecture

## Boundary

Javelin owns local version-control state. It initializes World, stores immutable versions, records Private Layer Checkpoints, integrates target changes, runs configured World Rules, accepts Contributions, exposes events and provenance, and repairs its own views.

External software owns agent launch, prompts, model selection, task assignment, scheduling, deployment, and hostile-process containment. Javelin has no command that starts an agent.

## Canonical state

`.javelin/store.sqlite3` selects Current World, Layer heads, Conflict status, validation records, events, retention state, and provenance links. `.javelin/objects/` stores immutable content. These two stores are canonical.

Project root and named Layer directories are Managed views. They may contain tentative edits, but accepted truth never comes from a view. Javelin can recreate every retained immutable state from objects and metadata.

## State graph

World history is one parent chain:

```text
v1 <- v2 <- v3 <- Current World
```

One accepted Contribution creates one World Version. Restore creates another World Version whose root equals an older state. It does not move the current pointer backward.

A Private Layer keeps:

```text
Origin Reference         fixed creation evidence
Synchronized Reference   latest target coherently incorporated
Layer head               latest append-only Checkpoint
target                   World or parent Private Layer
Managed view             ordinary filesystem directory
```

Child Publish appends a Checkpoint to its parent. Parent Publish appends a World Version. A parent may Publish while children retain pinned origins.

## Capture

The Monitor polls active, non-stale Managed views using the configured debounce. It serializes full reconciliation through a project lock. Commands also reconcile before critical reads and mutations. Monitor latency is an optimization; full scans are the correctness boundary.

Capture uses tracking policy from the Layer's Synchronized Reference. An edited `.javelinignore` is tracked but cannot hide or include files in the same Contribution. The accepted policy applies after Publish and subsequent reconciliation.

## Refresh

Refresh compares each normalized path across synchronized base `B`, latest target `T`, and private head `P`.

- `T == B`: apply private state.
- `P == B`: keep target state.
- `T == P`: keep the already-applied state.
- Otherwise, attempt bounded UTF-8 text integration for regular files with matching modes.
- Store a Conflict for every unresolved divergence.

Absent is a real path state. Kind, blob identity, executable bit, and symlink target participate in equality. Case-fold collisions become Conflicts before materialization.

A timer never refreshes a Layer. Safe boundaries are explicit `refresh`, `hook operation-end`, and the mandatory Refresh inside Publish.

## Publish

Each target uses a SQLite ticket queue plus an OS file lease. Ticket order decides admission. Dead Unix waiters are removed after liveness checks. One accepted operation owns the target lease.

The admitted publisher:

1. Reconciles and freezes source Layer.
2. Refreshes against current target.
3. Builds immutable candidate objects.
4. Materializes an isolated candidate when World Rules exist.
5. Runs accepted and candidate policy rules.
6. Opens one short SQLite transaction.
7. Compares target pointer, appends Contribution and target state, links validations and provenance, then advances pointer.
8. Marks affected views stale or updates them.

Hashing, compression, materialization, and checks stay outside the acceptance transaction. An idempotency key maps retry to one Contribution. If a client dies after database commit, retry repairs source synchronization without accepting a second state.

## Objects and materialization

Blob and Tree IDs use domain-separated BLAKE3. Objects use a versioned header and zstd payload. Writes use a temporary file, `fsync`, atomic rename, and parent-directory synchronization.

Tree encoding is deterministic and path-sorted. It records regular bytes, directories including empty directories, symlink targets, and executable bits for regular files.

Javelin builds one immutable root cache per Tree ID. On macOS it uses `clonefile` from cache into Managed views. Other systems use streamed copy. Hardlinks are forbidden because in-place writes could mutate shared inodes.

## Recovery model

Database commit selects accepted truth. A crash before commit leaves old target. A crash after commit leaves new target even if view update fails. `repair` invalidates root cache and recreates selected views from canonical objects and retained Checkpoints.

Fault injection covers object writes, Publish lease, candidate build, verification, transaction boundaries, event insertion, post-commit handling, and view update.

## Storage seam

SQL stays inside `Store` domain methods. Core state uses immutable IDs and value structs instead of passing query rows through command code. A future storage engine can implement the same domain operations; v1.0.0 does not build one.

