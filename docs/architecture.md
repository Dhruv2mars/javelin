# Javelin Architecture

Current-state reference for the Javelin code. Claims were checked against `src/` and `crates/*/src`. Javelin is Git-free: the runtime, storage, and tests never invoke or interpret Git.

## Overview

- **World**: the ordered, immutable sequence of repository versions. Each version's primary record is a changeset: the paths it changed relative to the previous version. Each version also records the root of its derived Merkle tree, computed from the parent's tree plus the changeset. Advancing the World is a compare-and-advance on the current version number.
- **Layer**: a durable, sparse head over a frozen World base. It records only the paths it changes. Layers are created, captured into, diffed, cloned, stacked, discarded, and published.
- **Checkpoint**: an immutable sparse delta, stored as canonical JSON, recorded against a Layer.
- **Stack**: imports a source checkpoint into a target Layer through a three-way merge against the common base.
- **Publish**: composes a whole Layer onto the current World in one atomic step, or reports a conflict or staleness.

Code: src/lib.rs, src/layer.rs, src/stack.rs, src/publish.rs, src/tree.rs

Concurrency, crash recovery, retries and corruption guarantees, and the tests that prove them: `docs/robustness.md`.

## Storage

- The repository is `.javelin/repository.sqlite3`. Format is `8`; the SQLite application id is `0x4a41564c`. Earlier formats are refused with `unsupported_format`; there is no migration.
- Schema (`SCHEMA` in `src/repository.rs`): tables `objects`, `attempts`, `versions`, `changes`, index `attempts_result`, and trigger `versions_order`. Immutable-row triggers stop stored rows from being rewritten. The derived radar index `layer_paths` (`src/radar.rs`) is the one mutable table.

World versions (changesets, not snapshots):

- `changes(version, path, kind, content)` holds one row per path a version changed. `kind` is 1 regular, 2 executable, 3 symlink; NULL `kind` and `content` mean deletion. It is a `WITHOUT ROWID` table with primary key `(version, path)`, which reads one version's changeset. Point reads at a version go through trees, not this table.
- `versions(number, id, tree, operation)`. `tree` is the root of the version's derived tree. `id` is a hash chain: `SHA-256("javelin-version-v2\0" || id(V-1) || tree(V) || canonical changeset bytes)`, with 32 zero bytes before World 0. Computing it is O(change), and one comparison of `id` or `tree` verifies a version's full content. `WorldVersion` is `{ number, id, tree, operation }`.
- The changeset type is `SparseChanges` (`src/domain.rs`), shared with Layer checkpoint deltas. Its canonical bytes are the JSON map of path to entry or `null`.
- Snapshots are derived data: `Repository::manifest_at(version)` walks the version's tree and `Repository::checkpoint_manifest(checkpoint)` walks its base tree and applies the checkpoint's changes. Both are O(snapshot) and independent of history length. Publish never derives one. `Repository::changeset(version)` returns the stored changeset; `Repository::head()` reads the head row.
- `attempts.tree` is the candidate root for `compare_and_advance` (the protocol's full-manifest input) and NULL for Layer publications, which bind their candidate through the `publications` row. `compare_and_advance` builds the manifest's tree (O(manifest)) and records the tree diff against the head as the changeset, which skips shared subtrees.
- Normalization is engine-neutral (`normalize` over the `WorldIndex` trait in `src/domain.rs`). The implementation is `Index` in `src/repository.rs`, which reads trees.
- `connect()` sets `foreign_keys=ON`, `synchronous=EXTRA`, `fullfsync=ON`, and a 5 second `busy_timeout`. Writes run in `BEGIN IMMEDIATE` transactions.
- `fullfsync` is set unconditionally. It only has effect on Apple platforms.
- `sync_directory` is a no-op off Unix.

Object kinds and IDs:

- Kinds: `Blob` = 1, `Manifest` = 2, `Checkpoint` = 3, `Tree` = 4. Manifest objects exist only for protocol and `put_manifest` input; World versions do not store them.
- `ObjectId` is SHA-256 over `b"javelin-object-v1\0"`, the kind byte, and the bytes. Constructors: `ObjectId::for_blob`, `for_manifest`, `from_bytes`, `parse`.
- `Manifest::new` builds the entry set; `canonical_bytes` encodes it; `Manifest::decode` re-encodes the parsed value and rejects input that is not in canonical form.
- `FileEntry` is `Regular` or `Symlink`.
- `RepoPath::parse` validates repository-relative paths (rules in `src/domain.rs`): at most `MAX_PATH` (4,096) bytes and `MAX_DEPTH` (256) components, equal to the protocol's wire limits.
- `VersionNumber` is bounded by `i64::MAX`.
- `OperationId::parse` accepts 1 to 128 characters from ASCII alphanumerics and `-_.`.

Integrity and bounded reads:

- `verify_object_streamed` hashes objects through a 64 KiB stack buffer (a literal `64 * 1024`, not a named constant).
- `read_object_bounded` checks the stored size before allocating.
- Bounded accessors used by callers: `read_blob_bounded`, `manifest_bytes_bounded`.
- `Repository::check` is the integrity-check entry point.

Validation scope:

- Commands validate only what they read or write: the head row, the Layer rows and checkpoints they touch (without walking Stack ancestry), the Layer's successful publication receipt, and new objects. Their cost does not grow with repository size or history length.
- Full-graph validation runs in `check()`, `history()`, `init()` on an existing store, `read_checkpoint` (with its Stack ancestry), and `layer_history` (that Layer's events and attempts). It verifies the hash chain, that every changeset is canonical against its parent tree, that every stored root equals the root recomputed from the parent root plus the changeset (each derived chunk must be stored with the same bytes), that accepted candidates equal derived roots, and all Layer, Stack and publication bindings. Work is O(total change x depth); no full snapshot is held. `check()` also decodes every stored tree chunk.

Code: src/domain.rs, src/repository.rs, src/lib.rs

## Trees (derived data)

`src/tree.rs`. A version's tree is derived from its parent's tree plus its changeset; changesets stay the source of truth, and `check()` rebuilds every root from them.

- Each directory is a prolly tree over its entries sorted by name. Items are cut into chunks at content-defined boundaries; each level above the leaves lists its child chunks by last name, up to one root chunk per directory. A directory entry names a file (regular, executable, symlink) or a subdirectory by its root chunk, so unchanged subtrees are shared by hash.
- Boundary rule (`boundary`): after an item, cut when the first 4 bytes of `SHA-256("javelin-tree-v1\0" || level || name)` fall below `len · size / SPREAD`, where `len` is the item's encoded bytes and `size` the chunk's bytes so far. The rising hazard gives Rayleigh-distributed chunk sizes with mean `TARGET` (2 KiB; 4 KiB wrote about 40% more per edit in `tree_bench`). A chunk is also cut at `FORCE` (64 KiB). Every input depends only on names, so:
  - the same entries give the same root whatever the edit order (history independence);
  - a content edit rewrites exactly one chunk per level of each directory on its path;
  - an insert or delete re-cuts only the chunks around it, until a new boundary meets an old one (`update_level`).
- Encoding (`Chunk`): `level u8`, then per item `tag u8, name length u16 BE, name, 32-byte ID`, names strictly ascending. Leaf tags: 1 regular, 2 executable, 3 symlink, 4 directory. Internal items have tag 0. The empty World is the empty leaf `[0]`; no other chunk is empty. `Chunk::decode` refuses anything else, including chunks over `MAX_CHUNK` bytes or above `MAX_LEVEL` (32). IDs are `ObjectId` over kind `Tree`.
- Operations on `Trees<S: Store>`, with a decoded-chunk cache: `apply` (O(changed paths x depth)), `build`, `lookup`, `entries`, `files_under`, `diff` (drops chunks present on both sides before expanding, so work follows the difference), and `intersections`.
- Derivation is eager. Publish computes the new root in its prepare phase, outside the lock, buffering new chunks (`Sql::buffer`); the short write transaction inserts them with the version row. The cost is O(change x depth) and small (see Measurements), and eager roots let the version ID cover the content, give every reader an O(1) root, and avoid a catch-up path that would write on reads.
- Storage: chunks are rows in `objects`. `Sql` writes them (`write`), buffers them for a later transaction (`buffer`), or, for `check()`, verifies each recomputed chunk is stored with identical bytes (`read`).

Code: src/tree.rs, src/repository.rs

## Layers and checkpoints

- Layer state lives in tables `repository_identity`, `layer_heads`, and `layer_attempts` (`SCHEMA` in `src/layer.rs`).
- `LayerId` uses the `OperationId` rules.
- `checkpoint_bytes` produces the canonical JSON for a checkpoint.
- `normalize()` (`src/domain.rs`) drops no-op entries, rejects directory-prefix tombstones with `invalid_input`, and rejects file/descendant collisions, using indexed lookups against the base version.
- `LayerBase` is `{ repository, version, world }`, where `world` is the base version's `id`. `LayerCheckpoint` holds the base, changes, Stack sources and an optional `provenance` Blob ID; it no longer carries a full manifest.
- Provenance (`Provenance` in `src/blame.rs`) is `{ actor: human|agent, agent?, session?, reference? }`, each field 1 to 256 bytes without control characters, stored as one canonical-JSON Blob. `Repository::set_provenance` declares it for a handle; `capture_layer` and `commit_stack` record it in the checkpoints they write, and capture's generated operation ID hashes it. Created Layers' empty checkpoints and handles without provenance stay unattributed. It is caller-declared, not authenticated.
- Operations: `create_layer`, `capture_layer`, `discard_layer`, `clone_layer`, `layer_diff`, `layer_history`, `check_layers`.
- Each mutating call goes through `update_in`, which records its operation id in `layer_attempts`. `attempt_outcome_in` returns the recorded outcome when an id is replayed.
- Recorded outcomes include accepted, stale, conflict, and `LayerStale`. ID reuse surfaces as the error code `operation_id_reused`.
- CLI: `javelin layer` has subcommands create, state, history, publication, diff, radar, clone, stack, and discard (help text in `crates/javelin-cli/src/cli.rs`). `stack` calls `javelin_stack::stack_layer`.
- There is one Layer runtime on every platform: a plain directory cloned from the Layer's World base, captured without a daemon. No mount, helper or background process is involved.

- Daemonless capture (`src/capture.rs`): `scan` walks a plain directory against a `StatCache` stored at `.javelin/capture/<sha256(layer id)>` and hashes only changed or untrusted entries. `changes_from` turns the result into `SparseChanges` against the base World. `Repository::capture_directory` stores blobs, saves the cache and calls `capture_layer` with a generated `capture-<hash>` operation ID. `capture_workspace` captures `.javelin/layers/<id>` and runs first in every `layer` and `workspace` command that names a Layer, except `workspace refresh`. The directory's sync record (`.javelin/capture/<sha256(layer id)>.sync`) names the checkpoint it last matched. A stacked head that differs from that record is never captured over (`Capture::behind`), because the directory lacks the stacked work. The CLI hook is `javelin checkpoint ID [--path DIR]`.

Layer workspaces: see [workspace-approach.md](workspace-approach.md). Clone-backed
directories (`src/workspace.rs`, `src/workspace/clone.rs`, CLI `crates/javelin-cli/src/cli_workspace.rs`):

- `open_workspace` brings the rolling base `.javelin/bases/rolling` to the Layer's World under the lock, clones it into `.javelin/layers/<id>` and applies the checkpoint changes, checking existing files and directories with tree lookups. `layer_workspace` returns the root and base paths.
- `CloneStrategy` is the strategy that worked: `DirectoryClone`, `Snapshot`, `FileClone` or `Copy`. `CloneReport` adds the fallback reason and copied bytes.
- One rolling base per repository, recorded in `.javelin/base.json`. A Layer at a newer World advances it in place by the tree diff between the two versions, applied with tree lookups: O(change), not O(repo). A Layer at an older World clones the base and rewinds the clone by the reverse diff. An absent or partial base is materialized from the full snapshot. Per-version bases are gone.
- `discard_workspace` writes the terminal event, then renames the directory into `trash/`. `recover_workspaces` and every command's bounded recovery finish interrupted steps.
- `refresh_workspace` (`src/workspace/refresh.rs`) brings a directory to a stacked head. It plans from three states: the sync record `S`, the directory, and the head. Each path is planned before any write, so a conflict writes nothing and fails with `RefreshConflict`. Writes are per-path atomic renames, and the sync record moves last.
- CLI `workspace` has subcommands create, open, path, refresh, discard, publish and recover. `workspace publish` captures the directory, then calls `publish_workspace` with the latest Active head; it refuses a Stack-produced head (`Capture::behind`) and files still changing during capture. `publish_workspace` runs `publish_layer` under the lock and, on success, renames the Layer's directory into trash and reclaims it, with the same budget as discard.
- Root `.javelinignore` rules (`IgnoreRules`, `src/ignore.rs`) hide untracked paths from capture; tracked paths are always visited.

Code: src/layer.rs, src/capture.rs, src/ignore.rs, src/workspace.rs, crates/javelin-cli/src/cli.rs, crates/javelin-cli/src/cli_workspace.rs

## Collision radar

- `Repository::radar(layer)` (`src/radar.rs`) reports what would make an active Layer's Publish conflict, in one read snapshot: `world` is `intersections()` against the current head, exactly the paths Publish would report now; `layers` lists every other live Layer that would make it conflict by publishing first, with `(ours, theirs)` path pairs.
- The Layer rule is compose's: a pair is the same path or an ancestor/descendant, and the other Layer's entry differs from this Layer's entry at its base. It lives in `collisions()` in `src/domain.rs` over the engine-neutral `LiveIndex` trait, next to `WorldIndex`, so a hosted World fed by streamed checkpoints can serve the same report.
- Table `layer_paths(path, layer, kind, content)`, primary key `(path, layer)`, `WITHOUT ROWID`, plus index `layer_paths_by_layer`, holds each live Layer's head changes. Exact and ancestor lookups are point reads; descendants are one range scan. Cost is O(changed paths × depth × log) plus matches, independent of the number of live Layers.
- It is derived data, written in the same transaction as the head change: `create_in` and `update_in` (`src/layer.rs`) set it for create, clone, capture and Stack, writing only differing rows; an applied discard and a successful Publish delete the Layer's rows. A crash on either side of a commit leaves it consistent with the Layer heads. `check()` (and `history()`) rebuild it from every live Layer's head checkpoint and refuse a mismatch with `corrupt_repository`.
- CLI: `javelin layer radar ID`, and a `radar` object in `checkpoint` output.

Code: src/radar.rs, src/domain.rs, src/layer.rs, src/publish.rs

## Trace blame

- Attribution is per path: each `layer_paths` row also names the `checkpoint` (and its `provenance`) that last set the path's entry. `track_in` attributes a new or changed entry to the new head checkpoint and keeps an unchanged one's. A clone's paths are attributed to its source checkpoint; Stack's imported changes to the Stack checkpoint, so to whoever ran Stack.
- Publish copies the published Layer's rows into table `blame(path, version, checkpoint, provenance)`, primary key `(path, version)`, `WITHOUT ROWID`, one row per World change, in its write transaction (O(change)), before clearing the Layer's radar rows. Versions advanced by `compare_and_advance` have no rows: their origin has no checkpoint.
- `Repository::blame(path, at)` returns the last change at or before `at` (default head) and its `Origin { version, layer, checkpoint, provenance }`: one primary-key read of `changes`, then point reads. O(log), independent of history.
- `Repository::blame_lines(path, at)` walks only this path's `changes` rows since it was last created, reading each version's blob and matching its lines to the previous version's with Myers' O(ND) diff (`matching()`, common prefix and suffix trimmed, lines interned). Unmatched lines take that version's origin. Past 2,048 edits in the trimmed middle, the middle counts as rewritten. Origins are resolved only for versions that still own a line. No cache: at 1,000 versions of a 1,000-line file it took 63 ms (`tests/blame_bench.rs`).
- A rename is a delete plus an add, so a renamed file's lines start at the renaming version. A deleted path's line history restarts when it is recreated.
- Derived and verified: `attribution_in` (`src/layer.rs`) rebuilds a Layer's per-path attribution from its revision history. `check()` compares it with `layer_paths` for live Layers and with `blame` for every successful publication, and refuses extra rows.
- CLI: `javelin blame PATH [--world N] [--lines]`. Provenance comes from `--actor`, `--agent`, `--session`, `--reference` on `checkpoint` and `layer stack`, else from `JAVELIN_ACTOR`, `JAVELIN_AGENT`, `JAVELIN_SESSION`, `JAVELIN_REFERENCE` for every command that captures.

Code: src/blame.rs, src/layer.rs, src/radar.rs, src/publish.rs, crates/javelin-cli/src/cli.rs

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
- Conflict kinds (`ConflictKind`): Content, Binary, ModifyDelete, AddAdd, EntryType, PathPrefix, RenameRename, Alias.
- Renames are exact-entry matches only (same entry bytes, no similarity). A base path whose two sides move it to different destinations is RenameRename, and `with` names the other destination.
- Alias conflicts come from a volume probe over the present paths (case and Unicode aliasing). Both members of an alias pair are listed, so either may be chosen, and `with` is None. Refresh lists a local file's head descendants and a local spelling's head write the same way; keeping the local side cancels the head writes it blocks.
- `merge_resolved` applies owner choices (`Choice`: Ours, Theirs, or Content) to conflicting paths and collision/alias members. Javelin never keeps two colliding paths. A choice for a path that does not conflict is invalid input.
- Stack stores the choices as one immutable resolution blob (`{"resolutions":[...]}`) that the Stack binding names. Retries with the same blob replay the original outcome. A different blob under the same operation ID is `operation_id_reused`.
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
- `compose_in` applies the whole Layer onto the current World by walking the base and head trees along the Layer's changed paths (`Trees::intersections`). Any directory whose subtree hash is equal at base and head is skipped with every changed path below it. Otherwise it compares the file at the path itself and at each ancestor, and diffs the subtrees below a changed path. A difference is an exact-path or ancestor/descendant collision. Differences are end-to-end, so a World change that was reverted is not a collision. Work is O(changed paths × depth) chunk reads plus the differing subtrees below changed paths; no snapshot is loaded.
- Without collisions, the new version's changeset is the Layer's changeset unchanged (it is canonical relative to the head), and its tree is the head tree plus that changeset. The guard receives `(&WorldVersion, &SparseChanges)`.
- Advance is a short `BEGIN IMMEDIATE` transaction in `publish_layer` itself, not `Repository::compare_and_advance`. It rechecks that the operation id is unused, that the World head is still the prepared `against`, and that the Layer state is unchanged. On a mismatch it re-prepares, so compatible concurrent Publishes rebase and intersecting ones record a conflict. It then runs the guard, inserts the new tree chunks and the `attempts`, `versions`, `changes` and `publications` rows, and commits.
- Every lookup inside that transaction uses a key or index (`attempts_result`, `publications_layer`, the one-success index), so its cost does not grow with history. The cost test in `tests/support/publish_cost.rs` measured 521 SQLite VM instructions at both 1 and 1,000 prior Worlds.
- Full history, Layer, Stack and publication validation stays in `Repository::check` (see Validation scope).
- `publication()` returns the stored receipt when an operation id is replayed, validating that receipt's own bindings.
- Attribution: `PublishAttribution::Human`, or `PublishAttribution::Agent { context, allow_missing_trace }`. Agent publishes without a trace are refused with `missing_agent_trace` unless `allow_missing_trace` is set.

Code: src/publish.rs, src/repository.rs, src/lib.rs

## Adoption

`Repository::adopt` (`javelin adopt`) turns a project directory into a repository whose
World 1 holds its files. World 0 stays empty, as in `init`. The reserved Layer is named
`adoption-<hash>`, where `<hash>` is 16 bytes of the operation ID's SHA-256 (`adoption_layer`).
Fresh stores get that Layer in the same transaction as World 0, so no store exists with World 0
but without it. The name binds the operation: only the same operation can resume or replay the
adoption. Capture reads the directory once with Source::Project, which skips `.git` at any depth
and honors root `.javelinignore`. Publish then accepts the checkpoint whole under that operation.
A store with any other Layer or version is refused without change.

Code: src/adopt.rs, src/capture.rs (`Source`), src/layer.rs (`create_on_version_in`)

## Repository protocol crate (`crates/javelin-protocol`)

Version 2 moves snapshots as derived trees: tree chunks and blobs, content-addressed and verified by hash, with no packfiles and no snapshot-wide size limit.

Framing and messages (`src/codec.rs`):

- Magic `JVLP`, `VERSION` 2, frame length as `u32` big-endian, then a tag byte. Tags are in the range 0x01 to 0x7f. Version 1 peers are refused with `unsupported_version`.
- Tags: Hello 0x01, HelloOk 0x02, Want 0x10, Missing 0x11, Get 0x12, Objects 0x20, Stored 0x22, Publish 0x30, Outcome 0x31, Resolve 0x40, Resolved 0x41, Error 0x7f.
- Object kinds on the wire: Blob 1, Tree 4. `Objects` items are `(kind, id, bytes)`; the decoder checks each length against its kind's limit and decodes every tree chunk with the native canonical decoder before accepting the frame.
- `Outcome::from_native(outcome)` maps repository results to wire outcomes; the accepted outcome reports the version's tree root.
- `read_frame` and `write_frame` enforce the frame limits below.

Receiver (`src/receiver.rs`, `serve()`):

- `negotiate()` requires Hello as the first message.
- `Want` checks presence with `has_blob` and `has_tree` and rejects duplicate ids. Only ids the receiver lacks are reported as Missing.
- `Objects` verifies each payload's id before storing it (`id_mismatch` otherwise). Blobs go to `put_blob`, chunks to `Repository::put_tree`, which refuses a chunk until every chunk, subdirectory and blob it names is stored. Having a tree therefore means having all of it, so a sender can stop probing at any chunk the receiver has.
- `Publish` calls `Repository::advance_to_tree`. It diffs the candidate against the expected version's tree (unchanged subtrees are never read), validates the changed contents, and requires the root derived from that changeset to equal the candidate, which rejects any non-canonical tree. Work is O(named paths × depth), where named paths are the paths added, modified or deleted between the expected version's tree and the candidate. Shared subtrees are skipped, but a tree can name more paths than its bytes (see Known limits).
- `Resolve` returns a version's (or the head's) number, ID and tree root. `Changeset` (tag 0x42) returns one part of a version's canonical changeset: `ChangesetPart` (tag 0x43) carries the total length and at most `MAX_CHANGESET_PART` bytes from the requested offset. `Get` returns the longest prefix of the requested objects that fits one frame; the client asks again for the rest.

Sender (`src/sender.rs`, `push()`):

- Probes from the root down, one level per `Want`, and never descends below a chunk the receiver has. An unchanged snapshot costs one probe; a one-file change sends the new blob and the chunks on its path.
- Sends blobs first, then missing chunks children before parents, batched by `MAX_OBJECTS` and `MAX_FRAME`, then publishes the root.
- `consistent()` checks that returned outcomes match what was sent. `Repository::put_snapshot` turns a full snapshot into a tree to push.

Fetch (`src/fetch.rs`, `fetch()`):

- Resolves the version, then verifies its root against the version ID chain before reading anything else. The chain starts at World 0's fixed ID (`genesis_version_id`). For each version from 1 to the target, the client fetches its changeset in parts and hashes `VersionHasher::new(parent, root)` over them as they arrive, never holding a whole changeset; the result must equal the claimed ID. A mismatch is refused with `chain_mismatch` (`CHAIN_MISMATCH`), and nothing is stored first. A changeset larger than `MAX_CHANGESET` is refused with `limit_exceeded`.
- Then reads the tree from the root down, one level per round trip, and the blobs last. Chunks already in the caller's `tree::Store` and blobs the caller has are not requested. Every object is checked against its id.
- Every named path, file or directory the walk follows, and every chunk reference, counts once against `MAX_FETCH_PATHS`. Exceeding it is refused with `limit_exceeded`, so a lying server cannot make the walk expand a shared subtree without bound.
- With a path prefix it follows only the chunks on the way to the prefix, using each internal chunk's last names (`Chunk::child_for`), then everything below it. The result lists the files at or below the prefix.
- Replies that answer another version, carry unrequested objects or bytes that do not match their ids, or make no progress are refused.

Limits (`limits` module in `src/lib.rs`):

- `MAX_FRAME` 16 MiB (`16 << 20`)
- `MAX_BLOB` 8 MiB
- `MAX_TREE` `javelin::tree::MAX_CHUNK` (about 69 KiB): one tree chunk
- `MAX_OBJECTS` 1024 objects per `Objects` frame
- `MAX_WANT` 4096 ids per `Want`, `Missing` or `Get`
- `MAX_CHANGESET_PART` `MAX_BLOB` (8 MiB): bytes of one `ChangesetPart`
- `MAX_CHANGESET` 1 GiB (`1 << 30`): the largest changeset a fetch will hash
- `MAX_FETCH_PATHS` 1,048,576 (`1 << 20`): named paths and chunk references one fetch expands
- `MAX_DETAIL` 1024
- Version 1's `MAX_MANIFESTS` and repository-wide `MAX_ENTRIES` (65,536) are gone: a snapshot spans as many chunks and frames as it needs. Paths are bounded natively (`MAX_PATH`, `MAX_DEPTH`); trees that would exceed them are refused when read.

CLI binary (`src/main.rs`):

- `javelin-protocol serve <project>` runs the receiver.
- `javelin-protocol push <project> <tree> <operation> <expected>` runs the sender.
- Results go to stderr as `accepted ...`, `stale ...`, or `error <code> <detail>`.

Code: crates/javelin-protocol/src/lib.rs, src/codec.rs, src/receiver.rs, src/sender.rs, src/fetch.rs, src/main.rs

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

## Measurements

Release builds on an Apple M1 (8 cores, APFS), 2026-10-18, with nothing else running.

`tests/tree_bench.rs` (chunks in memory, cold decoded cache per operation; `src/m<k>/` holds 1,000 files):

| Files | Build | Chunks / bytes | 1-file change | 100-file change | Add + delete | Full walk |
| --- | --- | --- | --- | --- | --- | --- |
| 100k | 0.05 s | 2,340 / 4.5 MB | 0.03 ms, 5.6 KB | 1.7 ms, 347 KB | 0.04 ms, 7.4 KB | 35 ms |
| 1M | 0.57 s | 24,017 / 46 MB | 0.035 ms, 7.0 KB | 2.0 ms, 388 KB | 0.04 ms, 8.4 KB | 382 ms |

One entry in a 50,000-entry directory (2.5 MB listing): edit 0.029 ms and 6.5 KB written, insert 0.031 ms and 6.8 KB, delete 0.033 ms and 6.7 KB. A 4 KiB `TARGET` wrote 11–12 KB for the same edits.

`tests/publish_bench.rs` (SQLite end to end, 100k files, median of 20 one-file Publishes; snapshot is the median of three `manifest_at` reads of the head). Churn is files changed per history step:

| History x churn | Publish, trees | Publish, format 6 | DB growth, trees / format 6 | Snapshot, trees | Snapshot, format 6 |
| --- | --- | --- | --- | --- | --- |
| 10 x 1 | 14.5 ms | 13.2 ms | 5.3 KB / 1.0 KB | 76 ms | 169 ms (noisy) |
| 1,000 x 1 | 15.2 ms | 13.8 ms | 5.5 KB / 1.2 KB | 78 ms | 82 ms |
| 10 x 1,000 | 14.3 ms | 16.7 ms | 5.5 KB / 1.6 KB | 84 ms | 85 ms |
| 300 x 1,000 | 14.4 ms | 16.3 ms | 4.9 KB / 0.6 KB | 86 ms | 176 ms |

Snapshots no longer depend on history: format 6 scanned every change row up to the version (about 400,000 rows in the last case); trees read only the snapshot. Publish latency is unchanged; each Publish now also stores about 4 KB of chunks for the directories on the changed path.

## Known limits

- A workspace whose sync record is missing (created before refresh existed) is refused by `workspace refresh` with `invalid_input`, and its stacked head stays `behind`. Recreate that workspace to use Stack.
- A crash between a capture's head commit and its sync write can make the next refresh report a false `refresh_conflict`. Nothing is lost; resolve it by capturing and refreshing again.
- Capture and first-time base materialization read full snapshots (`manifest_at`, `checkpoint_manifest`) by walking a tree: O(snapshot), independent of history length. They already walk the whole directory, so this is not their dominant cost. Commands no longer re-validate full state.
- Directory fsync is a no-op off Unix. `fullfsync` has effect only on Apple platforms.
- Merge refuses inputs that exceed the `Resource` limits. Only line-level diff3 is implemented, so binary and structural changes surface as conflicts.
- Protocol frames, objects and item counts are capped by the `limits` constants; snapshots are not. Only protocol version 2 exists. `Get` serves any stored object by id; authorization is outside the crate.
- Fetch returns files and stores chunks in a caller-supplied store; attaching a local repository to fetched versions is roadmap item 7.
- Publish does not enforce review state yet; the collaboration crate is not wired in.
- Tree work follows named paths, not tree bytes. A subtree can be named under several parents, so a few KB of canonical chunks can name 2^k files. `advance_to_tree` expands every named path with no cap, because snapshots are uncapped by design. Work and memory for a received tree are proportional to the paths it names. Accepted for Publish: the reference receiver accepts Objects and Publish from any caller, with no authentication in this crate, so the bound is safe only when callers authenticate and authorize outside it. `fetch` is bounded for an untrusted server: it refuses past `MAX_FETCH_PATHS` named paths and checks the root against the version ID chain. Its costs:
  - The chain walk starts at World 0 and costs two round trips per version (`Resolve`, then `Changeset` parts), so fetching version N costs O(N) round trips and transfers every changeset. A trust anchor stored by the caller would allow verifying only the new versions; that is not built yet.
  - The server re-encodes the canonical changeset for each part, so a changeset larger than one part costs O(parts × size).
  - Protocol version stays 2 with the new tags 0x42 and 0x43, so a new client against an old server fails at the first `Changeset` request, not at negotiation.
  - Snapshots above `MAX_FETCH_PATHS` (about one million paths) cannot be fetched until the bound is raised against a measured memory budget.

Code: src/capture.rs, crates/javelin-merge/src/resources.rs, crates/javelin-protocol/src/lib.rs, crates/javelin-collaboration/src/lib.rs

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
