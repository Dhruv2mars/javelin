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
the preceding checkpoint. It removes unchanged entries and nonexistent exact-path deletions,
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

Original source was verified on Darwin arm64 with rustc/cargo 1.93.0. The inherited baseline passed
26 tests with one ignored process actor before changes. Format 2 passes all of
those checks plus the original regressions, totaling 43 tests with one ignored actor.
These results predate the review repair and corrected checks below.
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

Exact source `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1` is hub-accepted as
native Layer state after the evidence repair below, local checks and the
corrected three-OS run. Acceptance covers native state only. Filesystem paths, backend capture reconciliation,
stable-change detection, automatic capture and interrupted materialization must
be integrated and verified in a follow-on unit. Windows symlink materialization,
platform naming limits, non-Unix parent-directory sync, hardware power loss,
network filesystems and scale/performance remain unverified. No Gate2 or GA claim
follows from this PR.

## Hub review and same-PR evidence repair

The hub reviewed `bd16d52` and all 18 artifact logs/full job log. Its independent
Claude Opus 5.5 review reported no material code defect, but dependency acceptance
required an evidence repair in this same PR. The original malformed-object
test used `serde_json::json!`, which sorted outer fields alphabetically. Redundant
unchanged entries and nonexistent tombstones therefore had both a noncanonical
delta and noncanonical bytes. Removing either canonicality comparison separately
still passed that test. The hub independently reproduced both exit-0 results in
`/var/folders/7g/c47xgk2552jdp_3frx78bkq40000gn/T/javelin-layer-guard-audit-yghl6v22`.
The original run remains compiled-platform evidence; it did not independently
protect the two canonicality checks.

The corrected fixtures serialize in `repository/version/manifest/changes` field
order. They compare empty and regular-entry control bytes with actual API-produced
checkpoint bytes, including the nested `kind/content/executable` order. Separate
tests inject a redundant unchanged entry and a nonexistent exact-path tombstone
with otherwise canonical bytes. A normalized empty delta has a successful
canonical control, followed by semantically identical reordered and trailing-newline
variants. Those variants isolate byte canonicality. Every injected object has a
correct native kind-tagged hash. No production encoder or guard is called to decide
whether malformed external bytes should be accepted.

Small SQL tamper cases replace a stale candidate with a valid checkpoint from
another World base, replace a stale discard candidate with a different valid
checkpoint on the same base, and switch a head plus its accepted candidate to a
valid checkpoint on another base. They exercise public state/history/init/check
and subsequent capture rejection, while the checkpoint graphs themselves remain
readable. Independent guard probes produced these confirmed results, with all
changes restored byte-for-byte before final actual-source checks:

- Removing only `normalized != changes` makes each canonically encoded redundant
  entry/tombstone test fail with exit 101. The byte comparison remains enabled.
- Removing only the byte re-encoding comparison makes the normalized empty-delta
  byte-encoding test fail with exit 101. Delta normalization remains enabled.
- Removing only candidate-base binding makes the valid stale foreign-base
  candidate test fail with exit 101.
- Removing only discard-candidate equality makes the valid same-base stale
  discard replacement test fail with exit 101.
- Removing only the per-Layer base scan still rejects the switched-base head,
  so that behavioral test passes with exit 0. Accepted-attempt binding supplies
  overlapping protection: each accepted event must be the next ordinal, match
  its candidate, and bind that candidate to the preceding head's base. Removing
  both base protections makes this test fail with exit 101. We do not claim an
  independent single-guard failure for the redundant scan. This overlap is
  confirmed by the probes, beyond the reviewer's initial coverage finding.

Mutation logs are at `/tmp/javelin-pr8-checkpoint-guard-repair`, named
`normalization-redundant.log`, `normalization-tombstone.log`, `byte-encoding.log`,
`candidate-base.log`, `discard-candidate.log`, `base-scan-overlap.log` and
`both-base-guards.log`. They are temporary development artifacts.

Corrected code/test commit `0a75b1a26e4ca3fbdf7f93a5ae7162c2e0f73b61` passes
52 tests in both debug and release on Darwin arm64 Rust 1.93.0, with one ignored
actor per suite. This includes all 14 inherited/new process tests, 18 Layer tests,
16 core tests and four CLI tests. Actual-source formatting, Clippy with warnings
denied, release build, whitespace check and the extracted caller compiled against
the production release library pass. Release CLI integrity again reports two
Layers, four objects, one World and zero World attempts. The release binary has
no test-boundary string. Runtime/tests still contain no Git invocation or parsing.
The original hosted run is not claimed as verification of these repaired tests
or the prefix-tombstone clarification; corrected run 38468818530 below covers them.

The hub resolved these semantics within the authorized scope:

- Any valid immutable checkpoint bound to this repository can seed a Layer,
  including one retained only as a stale operation candidate. The recovery test
  reads that retained candidate ID, reopens the repository and clones it without
  adding object bytes or changing the original head or World.
- A tombstone deletes an exact file/symlink entry. It does not delete a subtree.
  Ambiguous implied-directory prefixes now return `invalid_input`, including a
  prefix supplied alongside its child tombstones or private child additions.
  Future capture expands directory deletes into exact per-entry tombstones. No
  directory entries, directory conventions or materialization are added here.
  Removing an actual base file to replace it with child entries stays valid.
- `LayerId` is an opaque, case-sensitive metadata identity. Native state accepts
  names such as `.`, `..`, `CON`, `con`, `A` and `a` without using them as host
  paths. These are unsafe as raw host components. A future binder must map/encode
  identities and guard containment, platform reserved names and case collisions.

Foreign-reference error classification remains a nonblocking limitation. A foreign
base/head token returns `invalid_input`; a foreign checkpoint object inserted into
this store is rejected as `corrupt_repository` when its graph is read. An unknown
checkpoint ID without local bytes is indistinguishable from another missing local
object. None can silently adopt another repository's base.

## Hub acceptance of the corrected source

The hub accepted `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1` as native Layer state.
Its independent parent-side run passed the 52-test suite, the release-library
caller and the targeted release test-name-prefix run. A second fresh Claude Opus
5.5 review (R2) confirmed the medium canonicality-guard evidence gap is closed and
found no material new bug. The hub inspected all 18 artifact logs plus the
complete job log of the corrected run. The accepted PR6 base remains
`0c8582ddcb175d7bd4d2752e73f795651f810a6f`. The next Linux binding unit starts
from this exact commit. Docs-only handoff commits after it change no source,
tests, schema, Cargo files, CLI, workflow or probes.

[Native run 38468818530](https://github.com/Dhruv2mars/javelin/actions/runs/38468818530)
compiled exact commit `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1` with Rust
1.99.0. All three native jobs succeeded on the same runner images listed below.

- [Linux job 115542288749](https://github.com/Dhruv2mars/javelin/actions/runs/38468818530/job/115542288749)
  ran on Ubuntu 24.04 x86_64, image `20261004.327.1`, and passed 52 tests.
- [macOS job 115542289331](https://github.com/Dhruv2mars/javelin/actions/runs/38468818530/job/115542289331)
  ran on macOS 15 arm64, image `20260907.0337.1`, and passed 52 tests.
- [Windows job 115542289153](https://github.com/Dhruv2mars/javelin/actions/runs/38468818530/job/115542289153)
  ran on Windows 2025 x86_64, image `20260925.250.1`, and passed 50 tests.

Each runner ignored only the process actor entry point and passed all 14 process
checks and all 18 Layer checks. Formatting logs are empty, Clippy ends in a
successful dev build, and the release build succeeds. Windows again omits the
two Unix-only inherited tests; equivalent Windows behavior is not proven.
Artifacts `native-logs-ubuntu-24.04-1` / `11850772498`,
`native-logs-macos-15-1` / `11850931356` and
`native-logs-windows-2025-1` / `11850917029` are unexpired until 2026-10-27
between 21:14:16 and 21:17:45 UTC. Hub raw proof is at the temporary directory
`/tmp/javelin-hub-layer-ci-38468818530`.

Known limits after acceptance: ordinary-path binding, capture reconciliation,
autosave, Windows symlink materialization, platform naming limits, non-Unix
directory sync, power loss, performance, full Layer Publish, Gate2 and GA remain
unverified. Foreign-reference error classification stays a nonblocking limitation.

## Original hosted verification at `bd16d52`

[Native run 38465628999](https://github.com/Dhruv2mars/javelin/actions/runs/38465628999)
compiled exact commit `bd16d5224fa6dfce01509ebf6c28fb8b92dde1a3` with Rust
1.99.0. All three native jobs succeeded. This evidence update changes only this
document and the bounded TASKS section; the verified source and tests stay exact.

- [Linux job 115531579426](https://github.com/Dhruv2mars/javelin/actions/runs/38465628999/job/115531579426)
  ran on Ubuntu 24.04 x86_64, image `20261004.327.1`, and passed 43 tests.
- [macOS job 115531579792](https://github.com/Dhruv2mars/javelin/actions/runs/38465628999/job/115531579792)
  ran on macOS 15 arm64, image `20260907.0337.1`, and passed 43 tests.
- [Windows job 115531579184](https://github.com/Dhruv2mars/javelin/actions/runs/38465628999/job/115531579184)
  ran on Windows 2025 x86_64, image `20260925.250.1`, and passed 41 tests.

Each runner ignored only the process actor entry point. All 14 actual process
tests, all nine Layer integration tests, both new inherited-validator regressions
and missing-tail World protection passed. Windows omits the two inherited Unix
tests for storage symlinks and deleted working directories. That omission does
not prove equivalent Windows behavior. Symlink entries are preserved in native
state on every runner; ordinary filesystem symlink materialization is untested.

Downloaded and inspected each six-file artifact. Environment logs identify the
exact commit, runner/architecture and compiler. Test logs identify all executed
tests and passing suite totals. Formatting logs are empty, Clippy logs end in a
successful dev build, and release logs end in a successful optimized build. The
complete job log confirms the formatting, lint, test and release commands ran.
GitHub's jobs API reported successful jobs but omitted step arrays; artifacts and
the full job log supply the command evidence. The unrelated Macroscope check was
skipped; no independent review is claimed from it.

Artifact names and IDs are `native-logs-ubuntu-24.04-1` / `11849415012`,
`native-logs-macos-15-1` / `11847574889`, and
`native-logs-windows-2025-1` / `11848383630`. GitHub marks them unexpired with
expiry on 2026-10-27 between 20:50:19 and 20:52:44 UTC. Downloaded copies and
the full job log are at `/tmp/javelin-pr8-run38465628999`; that local directory
is temporary. The workflow is inherited unchanged from accepted PR6 and checks
native state only. Platform workflow isolation, autosave, hardware power loss,
performance, full Layer Publish and GA remain outside this evidence.
