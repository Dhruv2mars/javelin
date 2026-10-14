# Linux Layer runtime binding

## Bounded outcome

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
  hidden/          root 0700: empty/, cache/<b|c><hex>/, stage/<key>/, views/<slot>/{upper,work}
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
  each native capture/discard call, with the pending operation ID.
- A pending capture keeps the view frozen. Recovery remounts it read-only if needed,
  recomputes the identical delta and retries the same operation, so the native
  outcome converges. A pending discard retries after a non-lazy unmount.
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
  and `Error::Unsupported` are the only parent API additions, plus a crate-private
  repository identity accessor used to bind a runtime to one repository.
- Capture reads the frozen merged view: entries named by the upper listing are
  hashed from the view (`O_NOFOLLOW`, inode re-check, `readlink` for symlinks);
  all other entries must equal the opened checkpoint exactly or capture fails as
  corruption. Absent base paths become exact tombstones. Unsupported entries
  refuse capture with no native change: special files, setuid/setgid/sticky modes,
  non-UTF-8 names and names outside the native path model such as `:` or
  `.javelin`. Only the owner execute bit is represented. Empty directories are
  reported as `unrepresented_directories` and are not restored in clones.
- Pre-native errors that are certain (`invalid_input`, `unsupported`,
  `operation_id_reused`) clear the pending record and thaw. Other errors keep the
  view frozen with the pending operation so `capture`, `mount` or `recover` retry
  the identical frozen delta under the same operation ID.

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
- If another caller advances a Layer outside the runtime, its view's captures stay
  stale until it is discarded; re-binding a view to a newer head is follow-on.
- A retry of a completed discard reports that the view no longer exists; the
  native `discard_layer` retry returns the original outcome.
- Caches and retained checkpoint mounts have no collection policy yet.
- Production installation, a non-sudo privilege model, mount namespaces, other
  kernels, power loss, network filesystems and scale remain unverified.

## Independent review

A read-only Claude Opus 5.5 (medium effort) review of `34f6312` reported six
defects and one unconfirmed kernel risk. Fixed in `4c9b894..93f9eaa`:

1. A failed read during capture (for example a `chmod 000` file) kept the view
   frozen forever and stopped `recover`. Pre-native failures now thaw unless the
   native head advanced, and `recover` continues past a failing view, reporting it.
2. A concurrent direct `thaw` could change the upper while root listed it. Every
   helper verb now holds a per-runtime lock and the listing rechecks the freeze.
3. Cache sealing trusts the invoking user; documented above.
4. Views and caches were world-readable. Uppers and cache roots are now `0700`.
5. `init` accepted any root-owned parent. Parents now come from a root-owned list.
6. Retrying a completed capture after a lost reply returned `operation_id_reused`.
   The binding now returns the original outcome.
7. Shared writable mappings might bypass the freeze on newer kernels. Hosted runs
   now test this directly: a `MAP_SHARED|PROT_WRITE` mapping whose descriptor was
   closed made the freeze fail with `EBUSY` on kernel `6.17.0-1022-azure`, so the
   capture refused. Other kernels remain unverified.

## Hosted evidence

Executable source `93f9eaa00421f3c5efbaac79449bc22699c464b1` (later commits change
only this document and `TASKS.md`):

- [Linux Layer runtime run 38503434985](https://github.com/Dhruv2mars/javelin/actions/runs/38503434985),
  attempt 1 job failed: the unchanged common probe's `watch-notifications` check
  saw no event for `watch-create` within its 2-second window. All other checks
  passed. Attempt 2, [job 115655951380](https://github.com/Dhruv2mars/javelin/actions/runs/38503434985/job/115655951380),
  and a dispatched [run 38503753472](https://github.com/Dhruv2mars/javelin/actions/runs/38503753472),
  job 115656408546, both passed everything. Watch delivery is therefore
  VERIFIED in 2 of 3 runs, with 1 flake; it is not proven reliable. The previous
  source `34f6312` also passed in [run 38502015959](https://github.com/Dhruv2mars/javelin/actions/runs/38502015959).
- Ubuntu 24.04 image `20261004.327.1`, kernel `6.17.0-1022-azure`, x86_64, Rust
  1.99.0, Bun 1.3.13, ordinary UID 1001. Installed helper SHA-256
  `6083a828d63956d94e53479f418b699b99c98b8a43e3e24882f31a485b5b3de4`, root:root
  0755 in a root-owned directory. Common probe source pinned to `19224ef` and its
  hash checked against the proof's recorded `617f009e…`.
- Artifacts `11867140370` (`sha256:207b4ace…`) and `11867140559`
  (`sha256:b5a08b0f…`), expiring 2026-10-28. Each `summary.json` is VERIFIED:
  4 of 4 gated tests (3 mount tests plus the actor entry), flow, recovery and probe
  VERIFIED, all mounts removed and the runtime parent deleted. Downloaded copies
  are in `/tmp/javelin-pr10-run38503434985-final` and
  `/tmp/javelin-pr10-run38503753472-final` (temporary).
- [Native run 38503434805](https://github.com/Dhruv2mars/javelin/actions/runs/38503434805):
  Linux and macOS passed 63 tests and Windows passed 50. Linux ignores five
  gated or actor entries; macOS and Windows ignore one actor each. Windows does not
  compile the Unix-only binding.

What the runs prove, against `34f6312` inventories (sizes unchanged in `93f9eaa`):

- Fresh A/B/clone uppers have zero entries. A shared 8,344-byte base cache keeps
  identical inodes and hashes before capture, after capture, after clone and at
  the end.
- The checkpoint cache holds only the 7 changed entries (24,180 bytes, mostly the
  compiled `app` and the whole-file `bytes.bin` copy-up), plus kernel whiteouts for
  `delete.txt` and `dir`; untouched `main.c` is absent.
- Real `sh/mv/dd/rm/chmod/ln/cc/find` edits become an exact native diff: changed
  bytes, an execute-bit change, a new symlink target, a compiled binary, and
  per-entry tombstones for a removed directory. An empty directory is reported as
  unrepresented.
- A held writer and a shared writable mapping each make capture `busy` with no
  pending record. Unfrozen listing, unlisted parents, write bypass of an unmounted
  view (`EACCES`), writes to checkpoint and base views (`EROFS`) and hidden
  backing traversal (`EACCES`) are all refused.
- Clones of an older checkpoint after later edits match it exactly and compile 7
  and 11 independently. Stale external head advances keep edits. Discard removes
  only the clone's view and storage and keeps World and sibling Layers.
- Kills before native capture, after native commit, while staging a cache and
  during discard all recover to exactly one native outcome. Incomplete caches are
  never selected. A lock-holding capture makes a concurrent caller `busy`.
- The unchanged common probe reports 10 VERIFIED checks and keeps its caller-path
  clone check INCONCLUSIVE. Captured probe edits then clone into a separate view
  (values 9 and 11).

Verdicts: bounded Linux runtime binding VERIFIED on this runner image and kernel.
File-watch reliability is INCONCLUSIVE (one flake in three runs). The following
are NOT VERIFIED: other kernels and distributions, the production privilege
model, autosave, Stack and Publish, macOS/Windows bindings, Gate 2 and GA.
