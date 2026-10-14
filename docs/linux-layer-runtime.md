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
let helper = LayerMountHelper::sudo("/usr/local/libexec/javelin/javelin-linux-layer-helper")?;
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
