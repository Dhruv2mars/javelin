# Javelin Architecture

Current-state reference for the Javelin code. Claims were checked against `src/` and `crates/*/src`. Javelin is Git-free: the runtime, storage, and tests never invoke or interpret Git.

## Overview

- **World**: the ordered, immutable sequence of repository versions. Each version's primary record is a changeset: the paths it changed relative to the previous version. Full snapshots are derived on demand. Advancing the World is a compare-and-advance on the current version number.
- **Layer**: a durable, sparse head over a frozen World base. It records only the paths it changes. Layers are created, captured into, diffed, cloned, stacked, discarded, and published.
- **Checkpoint**: an immutable sparse delta, stored as canonical JSON, recorded against a Layer.
- **Stack**: imports a source checkpoint into a target Layer through a three-way merge against the common base.
- **Publish**: composes a whole Layer onto the current World in one atomic step, or reports a conflict or staleness.

Code: src/lib.rs, src/layer.rs, src/stack.rs, src/publish.rs

## Storage

- The repository is `.javelin/repository.sqlite3`. Format is `6`; the SQLite application id is `0x4a41564c`. Earlier formats are refused with `unsupported_format`; there is no migration.
- Schema (`SCHEMA` in `src/repository.rs`): tables `objects`, `attempts`, `versions`, `changes`, indexes `changes_by_version` and `attempts_result`, and trigger `versions_order`. Immutable-row triggers stop stored rows from being rewritten.

World versions (changesets, not snapshots):

- `changes(path, version, kind, content)` holds one row per path a version changed. `kind` is 1 regular, 2 executable, 3 symlink; NULL `kind` and `content` mean deletion. It is a `WITHOUT ROWID` table with primary key `(path, version)`.
- The primary key serves per-path point reads at any version (`entry_at`: the latest row with `version <= V`) and descendant range scans (`path >= 'p/' AND path < 'p0'`). `changes_by_version` reads one version's changeset.
- `versions(number, id, operation)`. `id` is a hash chain: `SHA-256("javelin-version-v1\0" || id(V-1) || canonical changeset bytes)`, with 32 zero bytes before World 0. Computing it is O(change). `WorldVersion` is `{ number, id, operation }`.
- The changeset type is `SparseChanges` (`src/domain.rs`), shared with Layer checkpoint deltas. Its canonical bytes are the JSON map of path to entry or `null`.
- Snapshots are derived data: `Repository::manifest_at(version)` and `Repository::checkpoint_manifest(checkpoint)` scan change rows (O(rows up to that version)). Publish never derives one. `Repository::changeset(version)` returns the stored changeset; `Repository::head()` reads the head row.
- `attempts.manifest` is set for `compare_and_advance` (the protocol's full-manifest input) and NULL for Layer publications, which bind their candidate through the `publications` row. `compare_and_advance` derives the head snapshot to compute its changeset, so it is O(repo).
- Composition and normalization are engine-neutral (`intersections`, `normalize` over the `WorldIndex` trait in `src/domain.rs`). The SQLite implementation is `Index` in `src/repository.rs`.
- `connect()` sets `foreign_keys=ON`, `synchronous=EXTRA`, `fullfsync=ON`, and a 5 second `busy_timeout`. Writes run in `BEGIN IMMEDIATE` transactions.
- `fullfsync` is set unconditionally. It only has effect on Apple platforms.
- `sync_directory` is a no-op off Unix.

Object kinds and IDs:

- Kinds: `Blob` = 1, `Manifest` = 2, `Checkpoint` = 3. Manifest objects exist only for protocol and `put_manifest` input; World versions do not store them.
- `ObjectId` is SHA-256 over `b"javelin-object-v1\0"`, the kind byte, and the bytes. Constructors: `ObjectId::for_blob`, `for_manifest`, `from_bytes`, `parse`.
- `Manifest::new` builds the entry set; `canonical_bytes` encodes it; `Manifest::decode` re-encodes the parsed value and rejects input that is not in canonical form.
- `FileEntry` is `Regular` or `Symlink`.
- `RepoPath::parse` validates repository-relative paths (rules in `src/domain.rs`).
- `VersionNumber` is bounded by `i64::MAX`.
- `OperationId::parse` accepts 1 to 128 characters from ASCII alphanumerics and `-_.`.

Integrity and bounded reads:

- `verify_object_streamed` hashes objects through a 64 KiB stack buffer (a literal `64 * 1024`, not a named constant).
- `read_object_bounded` checks the stored size before allocating.
- Bounded accessors used by callers: `read_blob_bounded`, `manifest_bytes_bounded`.
- `Repository::check` is the integrity-check entry point.

Validation scope:

- Commands validate only what they read or write: the head row, the Layer rows and checkpoints they touch (without walking Stack ancestry), the Layer's successful publication receipt, and new objects. Their cost does not grow with repository size or history length.
- Full-graph validation runs in `check()`, `history()`, `init()` on an existing store, `read_checkpoint` (with its Stack ancestry), and `layer_history` (that Layer's events and attempts). It verifies the hash chain, that every changeset is canonical against its parent snapshot, that accepted manifests equal derived snapshots, and all Layer, Stack and publication bindings.

Code: src/domain.rs, src/repository.rs, src/lib.rs

## Layers and checkpoints

- Layer state lives in tables `repository_identity`, `layer_heads`, and `layer_attempts` (`SCHEMA` in `src/layer.rs`).
- `LayerId` uses the `OperationId` rules.
- `checkpoint_bytes` produces the canonical JSON for a checkpoint.
- `normalize()` (`src/domain.rs`) drops no-op entries, rejects directory-prefix tombstones with `invalid_input`, and rejects file/descendant collisions, using indexed lookups against the base version.
- `LayerBase` is `{ repository, version, world }`, where `world` is the base version's `id`. `LayerCheckpoint` holds the base and changes only; it no longer carries a full manifest.
- Operations: `create_layer`, `capture_layer`, `discard_layer`, `clone_layer`, `layer_diff`, `layer_history`, `check_layers`.
- Each mutating call goes through `update_in`, which records its operation id in `layer_attempts`. `attempt_outcome_in` returns the recorded outcome when an id is replayed.
- Recorded outcomes include accepted, stale, conflict, and `LayerStale`. ID reuse surfaces as the error code `operation_id_reused`.
- CLI: `javelin layer` has subcommands create, state, history, publication, diff, clone, stack, and discard (help text in `crates/javelin-cli/src/cli.rs`). `stack` calls `javelin_stack::stack_layer`.
- Linux-only workspace commands (mount, path, unmount, checkpoint, watch, publish, discard, rebind, recover) are compiled only on Linux (`src/linux_layer.rs`, `crates/javelin-cli/src/cli_linux.rs`).

- Daemonless capture (`src/capture.rs`): `scan` walks a plain directory against a `StatCache` stored at `.javelin/capture/<sha256(layer id)>` and hashes only changed or untrusted entries. `changes_from` turns the result into `SparseChanges` against the base World. `Repository::capture_directory` stores blobs, saves the cache and calls `capture_layer` with a generated `capture-<hash>` operation ID. `capture_workspace` captures `.javelin/layers/<id>` and runs first in every `layer` and clone-backed `workspace` command that names a Layer. A head produced by Stack is never captured over (`Capture::behind`), because the directory lacks the stacked work. The CLI hook is `javelin checkpoint ID [--path DIR]`.

Layer workspaces: see [workspace-approach.md](workspace-approach.md). Clone-backed
directories (`src/workspace.rs`, `src/workspace/clone.rs`, CLI `crates/javelin-cli/src/cli_workspace.rs`):

- `open_workspace` clones `.javelin/bases/<N>` into `.javelin/layers/<id>` and applies the checkpoint changes. `layer_workspace` returns the root and base paths.
- `CloneStrategy` is the strategy that worked: `DirectoryClone`, `Snapshot`, `FileClone` or `Copy`. `CloneReport` adds the fallback reason and copied bytes.
- Bases derive from the nearest cached base plus the changeset between derived snapshots. The cache keeps two bases and pins the World head.
- `discard_workspace` writes the terminal event, then renames the directory into `trash/`. `recover_workspaces` and every command's bounded recovery finish interrupted steps.

Code: src/layer.rs, src/capture.rs, src/linux_layer.rs, crates/javelin-cli/src/cli.rs, crates/javelin-cli/src/cli_linux.rs

## Stack and mechanical merge

Stack (`src/stack.rs`, `crates/javelin-stack/src/lib.rs`):

- Table `stacks` records stack operations. `stack_operation` replays a recorded operation id.
- `commit_stack` advances the target Layer with a compare-and-swap and checks source ancestry.
- Binding blobs are recorded and verified by `record_in` and `verify_binding`.
- `stack_layer` loads blobs with `read_blob_bounded`, bounded by `Resource::BlobBytes.limit()`.
- Outcomes: `StackOutcome::Updated` or `StackOutcome::Conflicts`. Errors: `StackError::Storage` or `StackError::Merge`.

Merge engine (`crates/javelin-merge`):

- Entry point: `merge(base, ours, theirs, load)` returns `Result<MergeOutcome, MergeError<E>>`. `load` is supplied by the caller to fetch blob bytes.
- Per-path decisions are made by `merge_path`. Text is merged line by line by `merge_lines`, which uses diff3.
- diff3 anchors are lines unique in all three versions and kept by every minimal diff. Matching is an exact LCS in `matching.rs` (`matches()`, with mandatory-row computation). The crate does not use the `similar` crate.
- Conflict kinds (`ConflictKind`): Content, Binary, ModifyDelete, AddAdd, EntryType, PathPrefix.
- Admission limits are constants returned by `Resource::limit()`:
  - Entries 4096
  - PathBytes 1024
  - TotalPathBytes 128 * 1024
  - BlobBytes 1024 * 1024
  - LoadedBytes 8 * 1024 * 1024
  - Lines 8192
  - TableCells 4 * 1024 * 1024
  - Work 512 * 1024 * 1024
- `Usage::loaded` charges `4 * bytes + 32` against `LoadedBytes`.

Code: src/stack.rs, crates/javelin-stack/src/lib.rs, crates/javelin-merge/src/lib.rs, crates/javelin-merge/src/matching.rs, crates/javelin-merge/src/resources.rs

## Publish

- Table `publications` records each attempt. Outcome codes: 1 Published, 2 Conflict, 3 LayerStale.
- A partial unique index allows at most one successful publication per Layer.
- `publish_layer(operation, expected, attribution, guard)` is the entry point. `operation` is the idempotency key. `expected` is a `LayerHead`: the exact latest Active checkpoint of the Layer being published, not a World version.
- Prepare (`prepare_in`) runs in a read snapshot without the write lock. It replays an existing receipt, refuses operation-id reuse, verifies the head and trace, validates only the records the advance builds on (the head World's receipt and this Layer's earlier receipts), and composes the candidate.
- `compose_in` applies the whole Layer onto the current World with indexed lookups only. For each Layer-changed path it compares the entry at the Layer's base and at the head for the path itself, each ancestor prefix, and each descendant with a change row in `(base, head]`. A difference is an exact-path or ancestor/descendant collision. Differences are end-to-end, so a World change that was reverted is not a collision. Work is O(changed paths × depth) plus descendant ranges; no snapshot is loaded.
- Without collisions, the new version's changeset is the Layer's changeset unchanged (it is canonical relative to the head). The guard receives `(&WorldVersion, &SparseChanges)`.
- Advance is a short `BEGIN IMMEDIATE` transaction in `publish_layer` itself, not `Repository::compare_and_advance`. It rechecks that the operation id is unused, that the World head is still the prepared `against`, and that the Layer state is unchanged. On a mismatch it re-prepares, so compatible concurrent Publishes rebase and intersecting ones record a conflict. It then runs the guard, inserts the `attempts`, `versions`, `changes` and `publications` rows, and commits.
- Every lookup inside that transaction uses a key or index (`attempts_result`, `publications_layer`, the one-success index), so its cost does not grow with history. The cost test in `tests/support/publish_cost.rs` measured 521 SQLite VM instructions at both 1 and 1,000 prior Worlds.
- Full history, Layer, Stack and publication validation stays in `Repository::check` (see Validation scope).
- `publication()` returns the stored receipt when an operation id is replayed, validating that receipt's own bindings.
- Attribution: `PublishAttribution::Human`, or `PublishAttribution::Agent { context, allow_missing_trace }`. Agent publishes without a trace are refused with `missing_agent_trace` unless `allow_missing_trace` is set.

Code: src/publish.rs, src/repository.rs, src/lib.rs

## Repository protocol crate (`crates/javelin-protocol`)

Framing and messages (`src/codec.rs`):

- Magic `JVLP`, `VERSION` 1, frame length as `u32` big-endian, then a tag byte. Tags are in the range 0x01 to 0x7f.
- Tags: Hello 0x01, HelloOk 0x02, Want 0x10, Missing 0x11, Blobs 0x20, Manifests 0x21, Stored 0x22, Publish 0x30, Outcome 0x31, Error 0x7f.
- `Outcome::from_native(outcome, manifest)` maps repository results to wire outcomes; the accepted outcome reports the requested manifest.
- `read_frame` and `write_frame` enforce the frame limits below.

Receiver (`src/receiver.rs`, `serve()`):

- `negotiate()` requires Hello as the first message.
- `Want` checks presence with `has_blob` and `has_manifest` and rejects duplicate ids. Only ids the receiver lacks are reported as Missing.
- `Blobs` and `Manifests` verify each payload's id before storing it. A mismatch is reported as `id_mismatch`.
- `Publish` calls `compare_and_advance` and returns the outcome.

Sender (`src/sender.rs`, `push()`):

- Reads the manifest with `manifest_bytes_bounded(MAX_FRAME)` and blobs with `read_blob_bounded(MAX_BLOB)`. To push a World version, derive its snapshot with `manifest_at` and store it with `put_manifest`.
- Sends only items the receiver marked Missing.
- `consistent()` checks that returned outcomes match what was sent.

Limits (`limits` module in `src/lib.rs`):

- `MAX_FRAME` 16 MiB (`16 << 20`)
- `MAX_BLOB` 8 MiB
- `MAX_BLOBS` 1024
- `MAX_WANT` 4096
- `MAX_MANIFESTS` 16
- `MAX_ENTRIES` 65536
- `MAX_PATH` 4096
- `MAX_DEPTH` 256
- `MAX_DETAIL` 1024

CLI binary (`src/main.rs`):

- `javelin-protocol serve <project>` runs the receiver.
- `javelin-protocol push <project> <manifest> <operation> <expected>` runs the sender.
- Results go to stderr as `accepted ...`, `stale ...`, or `error <code> <detail>`.

Code: crates/javelin-protocol/src/lib.rs, src/codec.rs, src/receiver.rs, src/sender.rs, src/main.rs

## Review and permission model crate (`crates/javelin-collaboration`)

- `RevisionRef { base, manifest, context }` identifies a reviewed revision.
- Opaque ids are 1 to 128 characters from `[A-Za-z0-9-_.:]`.
- `CommentBody::MAX_BYTES` is 65536.
- `Role` is `Read`, `Review`, or `Maintain`. `Visibility` is a separate enum.
- `AppliedPermissions { repository, epoch, visibility, roles, required_approvals }` is the permission snapshot each action is checked against.
- `Action` is `Open`, `Revise`, `Approve`, or `Comment`. `Review::apply` is the state machine.
- Check order: operation replay first, then the epoch check (`stale_permissions`), then role and authorship checks.
- Refusal codes: repository_mismatch, stale_permissions, operation_id_reused, not_open, already_open, no_access, insufficient_role, not_author, self_approval, stale_revision, unknown_revision, revision_mismatch, unchanged_revision, already_approved, not_enough_approvals.
- `check_publishable` checks approvals against `required_approvals` and returns `not_enough_approvals` when they are short.
- The crate is a library. Neither `src/` nor the CLI depends on it yet.

Code: crates/javelin-collaboration/src/lib.rs, crates/javelin-collaboration/examples/review_flow.rs

## Known limits

- OverlayFS workspace commands (mount, checkpoint, watch, and related) are compiled only on Linux. Clone-backed workspace commands run on every platform.
- Capture and base materialization derive full snapshots (`manifest_at`, `checkpoint_manifest`), which scan every change row up to the base. They already walk the whole tree, so this is not their dominant cost. Commands no longer re-validate full state.
- Directory fsync is a no-op off Unix. `fullfsync` has effect only on Apple platforms.
- Merge refuses inputs that exceed the `Resource` limits. Only line-level diff3 is implemented, so binary and structural changes surface as conflicts.
- Protocol frames and item counts are capped by the `limits` constants. Only protocol version 1 exists.
- Publish does not enforce review state yet; the collaboration crate is not wired in.

Code: src/linux_layer.rs, crates/javelin-merge/src/resources.rs, crates/javelin-protocol/src/lib.rs, crates/javelin-collaboration/src/lib.rs

## Unverified

Claims from the removed narrative docs (linked to their pre-removal versions below)
that were not traced in code:

- [docs/review-permission-model.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/review-permission-model.md): a `(repository, request)` store. Not traced in code.
- [docs/layer-checkpoints.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/layer-checkpoints.md): checkpoints stored under the `Checkpoint` object kind. Not traced end to end.
- [docs/native-repository.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/native-repository.md): `fullfsync` behavior on Apple platforms. Platform behavior, not verified in code or by running it.
- [docs/layer-publish.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/layer-publish.md): that the World advance and receipt write share one transaction. Not traced.
- [docs/bounded-mechanical-merge.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/bounded-mechanical-merge.md): exact failure outcome when a merge limit is exceeded. Not traced.
- [docs/layer-publish.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/layer-publish.md): the exact trace check behind `missing_agent_trace`. Not traced.
- [docs/layer-stack.md](https://github.com/Dhruv2mars/javelin/blob/476b92d/docs/layer-stack.md): the blob load order inside `stack_layer`. Not traced.
