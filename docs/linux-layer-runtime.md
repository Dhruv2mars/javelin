# Linux Layer runtime binding

## Bounded outcome

The original binding and its historical evidence are recorded first. The
whole-Layer publication extension and its limits are described below. The later
foreground watch extension is documented at the end; earlier evidence below does
not claim to verify it.

This unit binds native repository Layer state to ordinary Linux paths through
sparse OverlayFS views. It is a production-library foundation, not a complete
Layer product flow.

Done means a real disposable hosted Linux run proves all of the following through
the production library and a bounded root mount helper:

- Real developer tools edit isolated mounted Layer views at ordinary paths.
- A safely frozen capture enters the native immutable `capture_layer` API.
- Capture derives the COMPLETE delta relative to the exact base World. Directory
  deletion expands to per-entry tombstones. Symlink targets are preserved and never
  followed. Kind, path and mode are validated. Live content is the input.
- An immutable checkpoint opens as a separate sparse clone with a fresh private
  upper/work over shared read-only caches.
- Native status/diff/check reflect exact bytes, kinds, modes and deletions.
- World, the base cache and sibling Layers stay intact, including after discard.
- Busy writers make capture refuse safely and retain edits, with no partial state.
- Abrupt process loss and restart preserve the graph and private state. Retries
  converge, interrupted preparation never selects an incomplete cache, and cleanup
  is bounded.
- Raw inventories prove one shared immutable base, per-view private upper/work,
  native reference sharing and sparse cache/storage usage.

Not claimed: autosave or a stable-change controller, continuous editing during
capture, Stack/refresh/Publish/provenance, macOS/Windows bindings, production
installation or privilege model, hostile-code confinement, power loss, network
filesystems, scale or Gate 2/GA. No manual save/commit CLI is added.

## Grounding

Native state (`src/layer.rs`, PR #8 at `e56fc86`) provides repository-bound
`layer_base`, `create_layer`, `clone_layer`, `capture_layer(op, expected, complete
sparse delta)`, `read_checkpoint`, `layer_state/history`, derived `layer_diff` and
terminal `discard_layer`. Tombstones delete exact file/symlink paths; ambiguous
directory prefixes are rejected. `LayerId` is opaque and case-sensitive, so it is
never used as a host path component.

The read-only Linux proof (PR #7, final `19224ef`, executable `89d76df`) verified
on hosted Ubuntu 24.04 that OverlayFS with `index=off,metacopy=off,redirect_dir=nofollow`
gives ordinary paths, file-granular copy-up, kernel whiteouts, private uppers over
a shared read-only lower, and frozen uppers reused as lowers for clones. It froze
by unmounting after all writers exited. It had no native database and no capture.

## Compared shapes

### A. Layered immutable caches plus per-superblock read-only freeze (chosen)

- Base cache: one read-only directory per World manifest, materialized once.
- Checkpoint cache: one sparse directory per non-empty checkpoint containing only
  its changed entries and kernel whiteouts, layered over its base cache.
- A view mounts `lowerdir=checkpoint:base` with a fresh private upper/work.
- Caches are built by writing through a private staging overlay, freezing it,
  verifying exact content, then non-lazy unmounting and renaming the staged upper
  into the cache. Unmount success proves no descriptor outlives sealing.
- Capture remounts the view's own overlay superblock read-only. The kernel refuses
  with `EBUSY` while any file is open for writing. Readers and shells stay valid.
  A root helper lists the frozen upper; content is read through the frozen view.
- Cost: changed bytes are stored once per checkpoint cache; capture hashes only
  upper entries plus one metadata walk of the view.

### B. Flattened per-checkpoint trees plus unmount freeze

- Each opened checkpoint is materialized as one complete flat lower tree.
- Capture unmounts the view, then scans its upper including whiteouts and opaque
  xattrs as root.
- Simpler single-lower mounts and no merged-view walk, but every opened checkpoint
  copies the whole repository, unmount refuses whenever any shell or reader is in
  the view, the path disappears during capture, and root must interpret user names
  and xattrs.

### C. Base cache only, with delta replayed into each clone's upper

- No checkpoint caches; each clone replays the checkpoint delta into its own upper.
- Fewer cache kinds, but clone bytes are copied per Layer instead of shared, the
  upper is not fresh, and replay creates a partially materialized window.

### Synthesis

Choose A. It keeps the proof's verified mount options and frozen-lower direction,
shares base and checkpoint bytes, keeps the path mounted during capture, and needs
no xattr parsing. Read-only remount is an owned quiescent boundary for exactly one
view, not host-wide `fsfreeze`, lazy detach or forced unmount.

## Caller types and ownership

```rust
// Linux only. The helper is a root-owned executable invoked as `sudo -n <helper>`.
let helper = LayerMountHelper::sudo("/usr/libexec/javelin-ci/javelin-linux-layer-helper")?;
let mut runtime = LinuxLayerRuntime::open(&repo, runtime_root, helper)?;
let path = runtime.mount(&mut repo, &layer)?;          // ordinary path for tools
match runtime.capture(&mut repo, &layer, &operation)? { // owned frozen boundary
    CaptureOutcome { update: LayerUpdate::Applied { .. }, .. } => {}
    CaptureOutcome { update: LayerUpdate::Stale { .. }, .. } => {} // edits retained
}
runtime.discard(&mut repo, &layer, &discard_op)?;     // native terminal discard first
```

- The ordinary-user library owns native graph calls, blob hashing, capture/verify
  walks, binding records and the runtime lock. It never writes hidden backing.
- The root helper owns only `mkdir/chown/chmod/rename/remove`, `mount` and non-lazy
  `umount` under one validated runtime root. It never reads the database, file
  contents or caller-supplied paths. Arguments are fixed verbs and hex tokens.
- Layer IDs map to `sha256("javelin-linux-view-v1\0" || id)` slots. Records store the
  exact ID; a mismatching record refuses. Raw IDs never become host paths.

## Runtime layout and recovery boundaries

```
<root>/            root 0755, every ancestor root-owned and not group/other writable
  owner            root marker containing the canonical root path and user ID
  hidden/          root 0700: empty/, cache/<b|c><hex>/, stage/<key>/, views/<slot>/{upper,work},
                   .lock (helper verb lock), epoch (advanced by every mutating verb)
  mnt/stage/<key>  root 0555 staging mountpoints
  mnt/ro/<hex>     root 0555 read-only checkpoint mountpoints
  views/<slot>     root 0555 live view mountpoints
  state/           user 0700: runtime.json, lock, <slot>.json binding records
```

- Unmounted mountpoints are root-owned `0555`, so writers cannot bypass a view into
  an empty underlying directory. Parents are root-owned, so mountpoints cannot be
  replaced.
- Cache selection is only `hidden/cache/<key>`, renamed there after verification.
  Staging leftovers are discarded and rebuilt; they are never selected.
- Binding records are written atomically before the upper is created and before
  each native capture/discard call, with the pending operation ID. Immediately
  before the native capture call the pending record also stores a candidate: the
  expected head revision and a digest of the exact complete delta.
- Native attempt rows decide whether a pending operation committed. On retry, a
  durable attempt is adopted only if it matches the recorded candidate exactly;
  otherwise it belongs to another caller and the pending work is abandoned
  (cleared and thawed) with `operation_id_reused`. Without a durable attempt,
  nothing committed, so any failure before the native call thaws. Only uncertain
  errors from the native call itself keep the view frozen for a later retry. A
  pending discard retries after a non-lazy unmount.
- Reopen reuses the recorded lowers and existing upper, so uncheckpointed edits
  survive and are never overwritten by a newer head.

## Verification plan

Local preflight on macOS covers formatting, Clippy, all native tests and the
platform-independent capture/mapping/validation units. No mount, sudo or install
runs on the laptop. Real proof runs only in `.github/workflows/linux-layer-runtime.yml`
on disposable `ubuntu-24.04`. It installs the helper root-owned, runs the gated
mount tests as the ordinary runner user, runs the unchanged common probe pinned to
`19224ef` from a separate developer checkout against base/A/B/clone paths, and
uploads raw inventories, mount tables, commands and source hashes.

Results are reported as VERIFIED, NOT VERIFIED or INCONCLUSIVE per bounded scope.

## Implementation

- `src/bin/javelin-linux-layer-helper.rs` is the bounded root helper. It requires
  real and effective UID 0 plus a non-root `SUDO_UID/SUDO_GID`, validates every
  ancestor as root-owned and not group/other writable, checks the exact layout,
  owners and modes against a root-owned owner marker, and accepts only
  `b|c<64 hex>` cache keys and 64-hex slots. Mounts use `mount(2)` with
  `MS_NOSUID|MS_NODEV` and the proof's options; freeze/thaw use `MS_REMOUNT` with
  or without `MS_RDONLY`; unmount uses `umount2(UMOUNT_NOFOLLOW)` without
  `MNT_DETACH` or `MNT_FORCE`. `EBUSY` exits 3. Upper listing is metadata-only and
  refused unless the overlay is frozen read-only. Removal only targets hidden
  stage/view directories with no mounts beneath them.
- `src/linux_layer.rs` is the ordinary-user runtime: `LayerMountHelper::sudo`
  accepts only a canonical root-owned helper and `/usr/bin/sudo`;
  `LinuxLayerRuntime::{init, open, mount, unmount, capture, discard,
  mount_checkpoint, unmount_checkpoint, status, recover}`. One `flock` per runtime
  serializes operations and returns `busy` to concurrent callers. `Error::Busy`
  and `Error::Unsupported` are the only public API additions. Two crate-private
  accessors were added: the repository identity, which binds a runtime to one
  repository, and `layer_operation`, which reads a durable Layer attempt row and
  its exact outcome so recovery never guesses whether an operation committed.
- Capture reads the frozen merged view: entries named by the upper listing are
  hashed from the view (`O_NOFOLLOW`, inode re-check, `readlink` for symlinks);
  all other entries must equal the opened checkpoint exactly or capture fails as
  corruption. Absent base paths become exact tombstones. Unsupported entries
  refuse capture with no native change: special files, setuid/setgid/sticky modes,
  non-UTF-8 names and names outside the native path model such as `:` or
  `.javelin`. Only the owner execute bit is represented. Empty directories are
  reported as `unrepresented_directories` and are not restored in clones.
- Freeze ownership through the reads uses the helper epoch: the upper listing
  returns it and capture re-reads it after the last content read. A thaw, remount
  or unmount in between (even a direct helper call) makes capture `busy` with no
  native change. Cache sealing passes the listed epoch the same way.
- Retrying any completed capture operation ID of the Layer returns its durable
  native outcome with `replayed: true`. Only this view's own pending capture can
  move the head the view is bound to; replaying another caller's operation never
  rebinds it. Replays report `unrepresented_directories` as empty.
- `recover` aborts staging leftovers, resolves pending work per view (continuing
  past failures, which it reports), removes views of discarded Layers and thaws
  frozen views that have no pending work.
- Runtime names match `[A-Za-z0-9][A-Za-z0-9_-]*` and parents must be listed in
  the root-owned `/etc/javelin/linux-layer-parents`.

## Trust model and limits

- The helper trusts `sudo` for caller identity and an administrator-written,
  root-owned `/etc/javelin/linux-layer-parents` for permitted runtime parents.
  Every helper verb takes a root-only per-runtime lock, so a view cannot be
  thawed while its frozen upper is listed.
- Cache content is verified by the ordinary-user library before sealing. The
  root helper does not re-verify it, so the invoking user's own processes can
  deliberately seal arbitrary content under a cache key by calling the helper
  directly. Sealed caches are unreachable for writes afterwards, and other users
  cannot read views or caches because uppers and cache roots are `0700`. This is
  protection against accidental tool writes, not against the same user.
- Ordinary paths provide private copy-up, not a sandbox. Absolute symlinks can
  reach sibling views, as the common probe records.
- Copy-up is whole-file: a one-byte edit stores the whole file in the upper and in
  later checkpoint caches. Capture performs one metadata walk of the view.
- Directory renames of lower directories fall back to copy because
  `redirect_dir=nofollow` is kept from the verified proof.
- Writers that open files during the short frozen window get `EROFS`. Continuous
  editing during capture, stable-change detection and autosave remain follow-on.
- If another caller advances a Layer outside the runtime, the view stays bound to
  its old head: captures return `Stale` and keep edits, and runtime `discard` also
  returns `Stale`. Clean stale views can explicitly rebind as described below.
  Dirty views retain their bytes and require owner resolution. A native discard
  at the current head followed by `recover` removes the view and loses uncaptured
  edits, so it is a destructive terminal action rather than a rebind mechanism.
- Direct helper calls by the same user are trusted, like cache sealing. Thawing a
  pending view directly while a writer keeps a file open makes later freezes
  `busy` until the writer closes it.
- When resolving another operation's pending work fails or is abandoned, the
  unrelated `capture`/`mount` call returns that error; retrying succeeds.
- A retry of a completed discard reports that the view no longer exists; the
  native `discard_layer` retry returns the original outcome.
- The helper lists at most 200,000 upper entries; larger uppers cannot be
  captured. Capture also hashes upper files and walks the view once, and the
  native graph check runs on each lookup. Scale is unmeasured.
- Caches and retained checkpoint mounts have no collection policy yet.
- Production installation, a non-sudo privilege model, mount namespaces, other
  kernels, power loss, network filesystems and scale remain unverified.

## Independent review and hub hold

All reviews below are read-only Claude Opus 5.5 (medium effort) passes. Every
finding after the first round was reproduced on hosted Linux against unchanged
production source before it was fixed (the round-one race repro adds only a
`#[cfg(test)]` boundary). The linked Actions runs below record those repros.
The committed evidence copies were removed in PR #37.

- Round 1 (`34f6312`): six findings, fixed or documented in `4c9b894..93f9eaa`.
  The hub held `93f9eaa`; its "fixed" claims for the list race and frozen
  recovery were incomplete, as the hub review and later repros showed.
- Hub hold (`93f9eaa`, repro [run 38505477176](https://github.com/Dhruv2mars/javelin/actions/runs/38505477176)):
  a stale-bound view with an unreadable file stayed frozen forever; a direct
  thaw/edit/refreeze between listing and reading was captured with the old blob;
  replay left the view frozen and older operations were not replayable; `init`
  root-deleted a runtime named `.rt-y.javelin-init`. Fixed in `e2a8000`.
- Round 2 (repro [run 38506377158](https://github.com/Dhruv2mars/javelin/actions/runs/38506377158)):
  replaying a foreign operation rebound the view and the next capture dropped
  foreign work; a pending ID reused on another Layer froze the view. Fixed in
  `a279a84`.
- Round 3 (repro [run 38506956046](https://github.com/Dhruv2mars/javelin/actions/runs/38506956046)):
  a pending capture adopted a same-Layer foreign commit under its ID; recovery
  ignored frozen views without pending work. Fixed in `654970f`.
- Round 4 (repro [run 38507609759](https://github.com/Dhruv2mars/javelin/actions/runs/38507609759)):
  a transient database lock during re-verification abandoned the view's own
  commit. Fixed in `d61648e`.
- Round 5 (repros [run 38508346755](https://github.com/Dhruv2mars/javelin/actions/runs/38508346755)
  and [run 38508564838](https://github.com/Dhruv2mars/javelin/actions/runs/38508564838)):
  a helper refusal and a direct thaw both abandoned the view's own commit, and a
  foreign same-ID commit plus an over-limit upper froze the view. The first
  misconfiguration repro was INCONCLUSIVE because the test failed earlier in
  `recover`; the corrected test reproduced it. Rounds 2-5 all came from deciding
  ownership by re-reading the view, so `d802d9b` replaced that with the recorded
  candidate design above.
- Round 6 (`d802d9b`): no source defects. It found the transient test had become
  vacuous; `8137111` makes it block reads, and it now fails the first recovery,
  keeps the view frozen and pending, then adopts the commit.

## Hosted evidence

Executable source `8137111cb00a227f0f225ef4debe7ca1bf891e98`. Later commits
change only this document and `TASKS.md`.

- [Linux Layer runtime run 38509261390](https://github.com/Dhruv2mars/javelin/actions/runs/38509261390)
  (job 115673748698, artifact `11868883074`, `sha256:80fa249a…`) and dispatched
  [run 38509461412](https://github.com/Dhruv2mars/javelin/actions/runs/38509461412)
  (job 115674389087, artifact `11868493900`, `sha256:14f85c84…`), expiring
  2026-10-28. Each passed 16 of 16 gated tests (15 mount tests plus the actor
  entry), all 15 summary checks VERIFIED, the shared-mapping freeze refusal
  VERIFIED, and all mounts removed with the runtime parent deleted.
- Ubuntu 24.04 image `20261004.327.1`, kernel `6.17.0-1022-azure`, x86_64, Rust
  1.99.0, Bun 1.3.13, ordinary UID 1001. Installed helper SHA-256
  `08edd4663aec1ba94c332c910643c4ed7e35fbeaff6513f81e4a62d70c4b15bb`.
- [Native run 38509261520](https://github.com/Dhruv2mars/javelin/actions/runs/38509261520):
  Linux and macOS passed 65 tests, Windows 50. Linux ignores 17 gated or actor
  entries; macOS and Windows ignore one actor each. Local Darwin debug and release
  suites pass 65 tests with one ignored actor, plus fmt, Clippy, release build and
  `git diff --check`. Release binaries contain no test-boundary string.
  `native.yml`, the build contract and the existing probes are unchanged.
- File watching: the unchanged common probe's `watch-notifications` check failed
  in 2 of 23 hosted probe executions on this branch (`93f9eaa` run 38503434985
  attempt 1 and `83751fb` run 38506552843 attempt 1, each "no watch event for
  watch-create" within 2 seconds). It passed in the other 21. It is INCONCLUSIVE,
  not reliable.

What the runs prove: shared base and checkpoint caches stay byte- and
inode-identical; fresh uppers start empty; real tools' edits become exact
complete native deltas; clones of older checkpoints match exactly; held writers
and shared writable mappings make capture `busy`; bypass writes, read-only views
and hidden backing are refused; kills before and after native commit, during cache
staging and during discard converge to exactly one native outcome; stale bindings,
foreign or reused operation IDs, direct helper thaws, helper refusals, transient
database locks, unreadable files and over-limit uppers never freeze a view
forever, never adopt foreign work and never abandon the view's own commit.

Verdicts: bounded Linux runtime binding VERIFIED at `8137111` on this runner image
and kernel. File-watch reliability INCONCLUSIVE. NOT VERIFIED: other kernels and
distributions, the production privilege model, autosave, re-binding stale views,
Stack and Publish, macOS/Windows bindings, Gate 2 and GA.

## Whole-Layer workspace publication (October 17)

Dependency: native Publish PR #23 merged into main at `dd4c915`.
The implementation began on its exact source `f6ee9b94c462723fef9a26b4021f4cd8c962c37b`
while it was unmerged and native CI was pending. This branch now includes merged
main; it does not independently re-accept that native core.

Done for this scoped extension means real disposable hosted Linux tests prove
that safe workspace publication freezes before acceptance, refuses held writers
and uncaptured edits, preserves the original native receipt across retries and
process loss, and retains a read-only published view plus a writable clone of its
final checkpoint. Root `TASKS.md` belongs to the hub and is unchanged.

- [x] Read instructions, runtime/helper behavior and native publication boundary.
- [x] Implement `LinuxLayerRuntime::publish` and terminal publication reconciliation.
- [x] Compile the public caller signature and notify the hub and CLI owner.
- [x] Add and inspect focused mounted/process publication regressions.
- [x] Run formatting, lint, all 97 local native tests and the release build.
- [x] Run and inspect disposable hosted Linux publication proof (run 38958858572).
- [x] Open and T3-link regular PR #25 against main.
- [x] Reproduce and fix published observation recreating explicitly unmounted views;
      keep existing mounts read-only and permit pending-operation recovery.

```rust
let receipt = runtime.publish(&mut repo, &layer, &operation,
    &PublishAttribution::Human, |_current_world, _candidate| Ok(()))?;
```

The guard has the native `FnOnce(&WorldVersion, &Manifest) -> Result<()>` signature.
No extra policy is imposed. Before native acceptance, the runtime freezes the
view's own superblock, reads it using the existing helper listing/epoch protocol,
and requires exact agreement of the effective capture tree with its bound
immutable checkpoint. Uncaptured tracked/nonignored changes refuse with
`invalid_input` and a checkpoint-required message. Nonignored empty directories
must be removed because native checkpoints cannot represent them. Ignored
untracked artifacts remain private, as described under ignore-aware discovery.
Refusal retains the private bytes and restores a writable Active view. Successful
publication leaves the view read-only and keeps its upper and binding; it never
replaces private bytes with the composed World or removes recoverable state.

The native `publication` receipt is the acceptance authority. A pending runtime
intent stores a digest of the exact expected head and attribution. Lost replies
and process kills after native acceptance recover that receipt without advancing
again or rerunning the guard. With no native receipt, recovery releases the intent
and preserves edits; publishing requires an explicit retry so caller policy can
run again. A completed operation retry with the same Layer and attribution returns
its original receipt, including its original World observation, even after later
World advancement. A changed attribution or Layer refuses operation reuse.

`LayerPublished` is a definite terminal capture/discard refusal. Pending state is
cleared and the view is read-only, rather than stranded waiting for a commit that
cannot occur. Historical native capture/discard outcomes remain historical; a
capture replay must never thaw a currently Published Layer. Mount can also expose
a final published checkpoint read-only when no earlier view binding exists.

Open, status and recovery freeze existing mounts when they observe native
publication; they preserve explicitly unmounted views. Explicit mount requests
and recovery of a pending native receipt may recreate the view read-only.
Damaged view records are reported per view by recovery rather than blocking open;
healthy published views are still frozen during open.
If a writer prevents that freeze, the operation reports `busy` and preserves the
view/upper for retry after the writer closes. **There is no continuous reconciler:**
a direct low-level native publication in another process is not instantly enforced
on an existing writable mount. Until a runtime call observes it, that mount can
remain writable. Such private edits remain recoverable and are never silently
adopted into the already accepted checkpoint. Use this safe workspace API for the
ordinary publication path. Production privilege, other kernels and autosave remain
outside this bounded extension.


## Explicit workspace rebind after native Stack (October 17)

```rust
runtime.rebind(&mut repo, &old_bound_head, &desired_active_head)?;
```

This action adopts an already committed native checkpoint; it never runs Stack,
refreshes a World, changes native history, or composes content. Both tokens must
belong to this repository and the same Layer. The old token must exactly match
this view's bound revision and checkpoint. The desired token must be newer and
exactly match the current Active head. Foreign, discarded and published inputs
refuse. The ordinary path remains at its old checkpoint until explicitly rebound.

The runtime lock excludes other runtime operations. Existing verified staging
builds and seals the desired immutable cache before touching the view. The old
view is frozen with the existing helper and epoch protocol, then compared byte,
kind and mode exactly against its bound checkpoint. Uncaptured files, deletions,
mode changes and unrepresented empty directories refuse; bytes remain private and
recoverable. A held writable descriptor or shared writable mapping refuses with
`busy` on the verified kernel. The refusal restores the prior mounted/unmounted
state. Validation may temporarily mount an explicitly unmounted view read-only;
success and recovery preserve that explicit unmount.

The binding record carries one small rebind intent and its original mount state:

- **Validating:** old selection and upper remain authoritative. Recovery abandons
  validation and restores the old mount state; a new explicit call is required.
  Interrupted cache staging is aborted by the existing recovery path and never
  selected.
- **Prepared:** the complete old view was verified and non-lazily unmounted, and
  the intent durably selects the exact desired cache. Recovery may remove the
  clean old upper repeatedly. The old immutable checkpoint remains inspectable.
- **Selected:** the new opened checkpoint, lowers and bound head are durable.
  Recovery only recreates the new view read-only if originally mounted; it never
  deletes the new upper.
- **Complete:** the exact old/new pair is retained for no-op retries. Only after
  this record is durable is an Active view thawed. A kill between completion and
  thaw is handled by existing frozen-view recovery. Later tool edits are retained
  by exact retries and recovery. Further captures use the new checkpoint as their
  lower reference and continue through ordinary whole-Layer Publish.

Native Stack commits before rebind. SQLite and filesystem selection are separate
boundaries. The desired Active head is checked again after frozen validation,
immediately before detach and durable intent. A native advance observed there
refuses and keeps the old selection. A low-level native caller can still advance
between that observation and intent persistence or at any later time: the runtime
lock is not a native database lock. Once Prepared is durable, recovery finishes
that requested immutable selection, even if native history advanced again. It
never silently selects the later head. A subsequent capture is stale, and another
explicit rebind is required. This is not an atomic native-head/filesystem CAS.
The same user invoking helper verbs directly remains inside the documented trust
boundary; an observed helper epoch change before detach refuses.

An exact completed retry is accepted only while that desired token is still the
current Active head and the view remains bound to it. A later capture or native
advance requires new exact tokens. `mount`, capture, Publish, discard and recover
resolve interrupted intents under the runtime lock. `status` reports a pending
`rebind` without choosing a checkpoint. `unmount` during an unfinished intent
returns `busy`; recover first so a successful explicit unmount is not undone.

Focused tests use public native `Repository::commit_stack` with a known disjoint
prepared fixture; the root crate gains no Stack adapter dependency or merge code.
The existing disposable hosted Linux runtime workflow includes the end-to-end
Stack → rebind → compiler/read/write → checkpoint → Publish proof, refusals,
native/helper races and process kills at cache, validation, removal, selection and
completion boundaries. The proof summary requires all four rebind evidence files
and complete mount cleanup. No mounts run on the development laptop.


Bounded hosted result: **VERIFIED** at executable/test source
`a6f6d95a57917a29e4b25ce81508a74e538cc9e8` in
[run 39011891335](https://github.com/Dhruv2mars/javelin/actions/runs/39011891335).
The retained artifact reports 26/26 gated tests, 25/25 required summary checks,
14 rebind process-kill cases, held-writer and shared-mapping refusal, and cleanup
with exit code zero and every mount plus runtime parent removed. Ubuntu runner
image `20261012.334.1`, kernel `6.17.0-1022-azure`, x86_64, ordinary UID 1001,
Rust 1.99.0. Local Darwin checks passed fmt, Clippy with warnings denied, 117
workspace tests (one ignored process actor), and the workspace release build.
The first hosted run `39011644791` was NOT VERIFIED: a test-only mapper barrier
waited for a newline while its release sent one byte. The one-line correction
was included before the successful run; the earlier run also removed all mounts.
No helper, native core, Cargo, CLI, adapter or probe changes are part of this
extension. Root `TASKS.md` is unchanged. Other kernels, distributions, production
privileges, automatic refresh, power loss and GA remain outside this proof.


## Opt-in foreground stable checkpoints

`workspace watch ID [--quiet-ms N] [--duration-ms N]` runs the focused runtime
`watch` method. See [the command reference](cli.md#foreground-automatic-checkpoints-on-linux)
for setup, timing, retry, terminal-stop and cleanup behavior. Existing commands do
not start a controller or daemon.

The controller hashes each discovered/tracked file's actual bytes and symlink target without storing
observation objects. Samples are separated by a 250ms sleep after each scan; equal
samples must span the quiet window (1000ms default, minimum 250ms). A view equal to
its bound immutable checkpoint is a no-op. It acquires the existing runtime lock
for each sample and releases it before sleeping. It validates the active binding,
freezes through the unchanged helper, reads the frozen candidate and verifies its
exact entries and empty-directory observation plus the helper epoch. Only then is
a fresh capture operation written to the existing pending record and submitted to
the unchanged freeze/capture/receipt flow. Capture validates its own epoch again.

A quiet observation does not delimit an application transaction. Writers/mappings
must close before freeze succeeds; boundary changes invalidate the attempt and
restart the quiet window. Capture IO/corruption errors are surfaced, and uncertain
native outcomes retain the pending operation for restart/recovery. No separate
watch metadata, schema, native capture semantics, ignore algorithm or event
framework is added. A killed process before the pending write may leave a frozen
active view; restart uses existing resume behavior before observation. A process
killed after the pending write resolves that same capture and receipt first.

Published/Discarded stop; a stale external binding requires explicit rebind.
Watch does not change World or recover interrupted Publish/discard/rebind on the
user's behalf. Performance and large-tree bounds beyond existing limits remain
unverified. Discovery uses the root ignore rules described below.

## Ignore-aware discovery

The shared `javelin::IgnoreRules` API parses rule text and answers `ignored(path)`
without filesystem access or a platform backend. Clone-backed Layers and
daemonless stat-cache scans can use the same matcher, preserving tracked paths
and traversing their ancestors even when a rule matches. File loading and
OverlayFS validation remain in the Linux runtime adapter.

Linux workspace checkpoint, foreground watch and Publish read the root
`.javelinignore` from the workspace view. Each nonempty line is a literal,
case-sensitive root-relative native path, for example `build/`, `packages/ui/dist/`
or `private.env`. A path matches itself and all descendants; the optional trailing
slash has the same meaning. Blank lines and lines beginning with `#` are ignored.
Use LF or CRLF. Leading/trailing whitespace, absolute paths, `.`/`..` components,
globs (`*`, `?`, brackets), negation (`!`) and backslash escapes are errors.
There are no nested rule files, automatic imports or external VCS semantics.
The file must be regular UTF-8, at most 64 KiB with at most 256 rules; missing
means no rules. Symlinked rule files are refused, including dangling symlinks.
Malformed rules and read failures abort the operation visibly and preserve bytes.
`.javelinignore` itself is always eligible for capture, even if named in a rule.

Rules exclude only paths absent from the **current bound checkpoint**, including
files added since the original mount. Tracked contents, executable bits, symlink
targets and deletions still enter the next checkpoint. Editing rules cannot
silently untrack an entry. Removing a rule discovers the still-present private
files. Watch compares the effective capture tree and revalidates it after freeze;
ignored-only edits do not create checkpoints. Checkpoint and clone snapshots
contain tracked state only.

Publish allows ignored untracked private artifacts (including empty ignored
directories), retains their bytes in the private workspace and leaves the whole
view read-only on success. Uncaptured tracked and nonignored changes still refuse.
Rebind deliberately checks the full tree and refuses ignored private artifacts
rather than deleting them. Capture, refusal and recovery never remove ignored
bytes. Discard retains its explicit destructive behavior. Existing helper listing
and filesystem limits still apply; ignore rules do not expand them.
