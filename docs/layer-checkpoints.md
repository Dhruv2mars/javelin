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
The hub accepted PR6's unchanged-core compiled checks at
`0c8582ddcb175d7bd4d2752e73f795651f810a6f`, run
https://github.com/Dhruv2mars/javelin/actions/runs/38463522128. Logs prove 26 tests
on Linux/macOS and 24 on Windows, with one ignored actor each. Windows symlink and
deleted-cwd behavior, non-Unix directory syncing, power loss and performance remain
unverified. PR7's Linux filesystem proof does not supply database binding or
automatic capture. Our Layer changes require their own platform run.

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

```rust
use javelin::{Error, FileEntry, LayerId, LayerState, LayerUpdate, OperationId,
    RepoPath, Repository, SparseChanges, VersionNumber};

fn capture_example(project: &std::path::Path) -> javelin::Result<()> {
    let mut repo = Repository::init(project)?;
    let base = repo.layer_base(VersionNumber::new(0)?)?;
    let layer_id = LayerId::parse("work")?;
    let head = repo.create_layer(&layer_id, &base)?;
    let content = repo.put_blob(&[0, 255, 42])?;
    let changes = SparseChanges::new([(RepoPath::parse("data")?,
        Some(FileEntry::Regular { content, executable: false }))])?;
    let outcome = repo.capture_layer(&OperationId::parse("capture-1")?, &head, &changes)?;
    let next = match outcome {
        LayerUpdate::Applied { state: LayerState::Active { head } } => head,
        _ => return Err(Error::InvalidInput("head changed; reconcile current state".into())),
    };
    let frozen = repo.read_checkpoint(next.checkpoint())?;
    let child = repo.clone_layer(&LayerId::parse("child")?, frozen.id())?;
    let diff = repo.layer_diff(&child)?;
    assert_eq!(diff.len(), 1);
    assert_eq!(repo.read_blob(diff[0].after.as_ref().unwrap().content())?, [0, 255, 42]);
    repo.discard_layer(&OperationId::parse("discard-child")?, &child)?;
    assert_eq!(repo.layer_history(&layer_id)?.len(), 2);
    assert_eq!(repo.history()?.len(), 1);
    Ok(())
}
```

Native types distinguish validated Layer IDs, frozen bases, checkpoint IDs,
active head tokens and discarded states. SQL remains private. Repository identity
and exact base manifest prevent foreign or stale references from silently selecting
another truth. Frozen checkpoint reads must not consult a moving Layer head.

## Recovery and format plan

Use one versioned embedded format. Format 2 adds explicit symlink
entries and Layer object kinds/tables. Unshipped format 1 is rejected, never silently
rewritten or flattened. Original World objects in a format 2 repository remain
immutable. All foundational tests are rerun against the new format. No large
prototype compatibility layer is planned. Retain all checkpoint/head history after
discard; collection is outside this unit.

## Implemented storage and correctness boundary

One random 256-bit repository identity binds bases and checkpoints to this store.
`LayerBase` binds identity, World ordinal and exact manifest. `CheckpointId` points
to a kind-tagged content-addressed canonical object with that base and a sorted
complete sparse delta. The resolved full manifest is derived in memory, never
stored per Layer or checkpoint. Regular entries carry content and executable mode;
symlink entries carry a blob of exact nonempty, NUL-free target bytes. Readers do
not follow those targets. Platform materializers must preserve the entry kind or
report an unsupported target. Empty directories and special files remain outside
the current representation.

`repository_identity`, `layer_heads`, and `layer_attempts` join the existing store.
An initial Layer adds one head row pointing to an empty sparse checkpoint; creating
another Layer on the same exact base shares that checkpoint. Cloning adds only one
head reference to an existing checkpoint. Head events have contiguous per-Layer
ordinals. The latest event derives the current head. Active head tokens have private
fields and bind the repository, Layer, ordinal and checkpoint. Discard is a terminal
event that retains the last checkpoint. `layer_history` enumerates old heads after
restart or discard, so recovery does not depend on caller-held tokens.

Capture takes a complete delta relative to the frozen World, not edits relative to
the preceding checkpoint. It removes unchanged entries and nonexistent deletions,
validates content kinds and the whole merged path tree, then writes the checkpoint,
head event and retry outcome in one immediate transaction. Stored candidates from
stale operations remain immutable too. The operation ID binds Layer, expected head
ordinal, normalized checkpoint and capture/discard action. Layer operation IDs have
their own namespace, separate from World operations. Applied and stale outcomes
return their exact original head state after later edits or discard. Invalid input
and failed transactions create no terminal outcome; retry the same input after
repairing a missing object. Reuse with other bound input reports `operation_id_reused`.

Creation/clone retries use the Layer ID and exact initial snapshot. They return the
initial frozen token, even after subsequent edits or discard. Read `layer_state`
for the current state. Discarded IDs cannot be revived. A new Layer may clone any
valid frozen checkpoint, including one retained from a discarded Layer. All object,
head, identity and outcome rows reject update/delete through immutable triggers.

Exact checkpoint/diff reads hold one read transaction and do not consult a moving
Layer head. Full inspection and Layer mutations validate retained event and outcome
graphs before writing. There is no collection policy in this PR. As in the core,
an owner who removes all metadata anchors consistently can erase evidence of a
previous head; this store is not tamper-proof. Complete metadata history scans and
whole-object reads favor a small correctness boundary. Scale remains unmeasured.

This API never advances World. Future whole-Layer Publish must revalidate the exact
selected Layer checkpoint and accepted World inside the final publication boundary,
along with refresh/conflict and permission/policy checks owned by later units.
Calling inherited metadata compare-and-advance is not full Layer Publish.

## Local verification and remaining work

Verified on Darwin arm64 with rustc/cargo 1.93.0. The inherited baseline passed
26 tests with one ignored process actor before changes. Format 2 passes all of
those checks plus the new regressions, totaling 43 tests with one ignored actor.
Commands from this worktree:

```sh
rtk proxy cargo test --locked --all-targets --all-features
rtk proxy cargo test --release --locked --all-targets --all-features
rtk proxy cargo fmt --all -- --check
rtk proxy cargo clippy --locked --all-targets --all-features -- -D warnings
rtk proxy cargo build --locked --release --all-features
rtk git diff --check
```

`tests/layer_checkpoints.rs` drives the production library API. A 1 MiB accepted
base stays shared. Capture stores one new 3-byte blob and one sparse checkpoint,
adding fewer than 1 KiB of object bytes. Unchanged files and nonexistent deletions
are omitted. Twenty-four clones add zero objects and zero object bytes, each
with one head row. Their combined Layer ID and checkpoint reference strings use
at most 1,920 logical bytes; this excludes SQLite row/page overhead. The changed
checkpoint itself is under 1 KiB. Clone does not add retry outcome rows.

Tests read exact additions/deletions, mode and symlink changes, frozen snapshots
after later head/World advances, sibling edits, retained history after discard,
post-discard recovery clones, foreign bases/heads/imported checkpoints, structural
collisions, missing/corrupt graphs, stale-only candidate corruption, missing head
anchors, stale outcome ordering, and write-failure rollback. Malformed checkpoint
objects with valid hashes cannot bypass base/path/canonical-delta validation.
Format 1 returns `unsupported_format` without rewriting the database.

`tests/support/process.rs` retains all eight inherited process checks and adds six
Layer checks. Independent processes behind pipe barriers prove one winner and
seven durable stale outcomes, eight identical retries appending once, independent
Layer heads sharing one checkpoint, capture racing discard, kill before commit,
and lost/received acknowledgement recovery after a later head. A fresh process
reads the older checkpoint's exact binary entry after later capture. No sleeps or
polling daemon are used. Test boundary hooks are absent from production builds.

`tests/native_repository.rs` adds stale-only manifest/content corruption and
invalid stale outcome ordering regressions. History, exact version reads,
repeated init, retry, new World advancement and inspection fail without adding
Worlds or outcomes. The inherited validators already handled these cases; no
parent-branch correctness rewrite was needed.
Temporarily removing each inherited guard made its new regression fail at the
history rejection assertion, with exit 101. The exact source was restored before
final checks. These probes confirm the tests detect loss of those validations.

The Rust caller example above was extracted, compiled with `rustc --edition=2024`
against `target/release/libjavelin.rlib`, and run in a fresh temporary project.
The release binary's `check --json` then returned two Layers, four objects, one
World and zero World attempts. Its bytes contain no `JAVELIN_TEST_BOUNDARY` string.
Source inspection for process calls, sleeps and Git references found only launches
of the native CLI/current test executable in tests. Runtime and tests are
Git-independent. These API/process checks do not verify ordinary-path binding.

The local bounded state result is VERIFIED. Our three-OS CI is pending until its
actual job logs are inspected. Filesystem paths, backend capture reconciliation,
stable-change detection, automatic capture and interrupted materialization must
be integrated and verified in a follow-on unit. Windows symlink materialization,
platform naming limits, non-Unix parent-directory sync, hardware power loss,
network filesystems and scale/performance remain unverified. No Gate2 or GA claim
follows from this PR.
