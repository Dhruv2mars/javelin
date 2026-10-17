# macOS sparse Layer backend attempt

Status: bounded hosted feasibility evidence collected. The hub has not promoted
this implementation as the macOS backend or accepted Gate 2.
Results below come from downloaded run archives. Source findings and untested
capabilities are listed separately. CLI, native database binding, autosave and GA remain open.

## Source and dependencies

- Starts from hub commit `c5c0ebec93cb80308043192628ed452ff6f5d66d` on `chore/ga-build-plan`.
- PR #5 (`tools/workspace-probe/`) verified only directory and clonefile controls on
  macOS. Those controls validate a probe and never count as sparse proof.
- The hosted workflow checks out PR #5's probe at
  `0e7ed045774b18819ac12f62f44ba0fdfdc641a4` as developer setup and verifies
  `probe.mjs` SHA-256 `617f009e…0b49`. The original attempts used unmerged PR #5
  and PR #7 dependencies; using the probe was not acceptance of either PR.
  No product command invokes Git.
- The root crate at the starting commit is a scaffold. No native Layer API is used.
- PR #5 laptop record (macOS 26.6.2, SDK 27): the FSKit sample build exited 69
  because the Xcode license was unaccepted, and zero signing identities existed.
  This unit did not repeat laptop setup, accept licenses, mount or change the laptop.

## Done

A reproducible hosted macOS attempt with committed source, recorded SDK, host,
license, signing and extension-activation prerequisites, raw job archive and cleanup
evidence, plus a source-backed comparison of at least two sparse ordinary-path
approaches. VERIFIED only if a real sparse mount passes ordinary-path checks.

## Compared approaches

Research sources were read on 2026-10-14. UNCONFIRMED marks claims not checked
against a primary source or a run.

| Approach | Signing | Activation | Hosted CI | Verdict before run |
| --- | --- | --- | --- | --- |
| FSKit module (`FSUnaryFileSystem`) | Restricted entitlement, paid-team profile | Per-user System Settings toggle | No team, no GUI | Expected blocked; probe records it |
| Loopback NFSv3 server (`nfsserve` 0.11.0, BSD-3-Clause) | None | `sudo mount_nfs` | Passwordless sudo on disposable runner | Selected |
| macFUSE kext | Signed kext | Reduced Security and reboot on Apple Silicon | Forbidden here | Rejected |
| macFUSE FSKit backend | FSKit | Same toggle; mounts only under `/Volumes` | No | Rejected |
| FUSE-T (NFS/SMB) | Signed package | Install | Possible | Redundant with NFS; restrictive commercial license |
| `mount -o union`, tmpfs, disk images, clonefile | — | — | — | Not sparse copy-on-write; rejected |

FSKit sources:

- [FSKit](https://developer.apple.com/documentation/fskit): "The current version
  of FSKit supports only `FSUnaryFileSystem`." `FSPathURLResource` is
  `FSKIT_API_AVAILABILITY_V2` in the macOS 26 SDK `FSResource.h`.
- [Building a passthrough file system](https://developer.apple.com/documentation/fskit/building-a-passthrough-file-system)
  requires Xcode 26 and macOS 26, enables the module through Settings > General >
  Login Items & Extensions > File System Extensions, and mounts with `mount -t`.
- Apple DTS, [forum thread 817501](https://developer.apple.com/forums/thread/817501):
  "FSKit modules must be signed with the `com.apple.developer.fskit.fsmodule`
  entitlement. That's a *restricted* entitlement … you'll need to be a member of a paid team."
- Apple DTS, [thread 831843](https://developer.apple.com/forums/thread/831843), on CI
  automation: pluginkit provides "some visibility … but that's the extent of what's
  currently available." Whether `pluginkit -e use` enables an FSKit module is
  UNCONFIRMED; the hosted probe records its actual result.
- [Thread 819160](https://developer.apple.com/forums/thread/819160) reports an
  `fskitd` deadlock cleared only by restart, so each FSKit command has a timeout.
- [runner-images](https://github.com/actions/runner-images) accepts the Xcode
  license during image build (`Xcode.Installer.psm1`); the probe records
  `xcodebuild -license check` rather than relying on that.

NFS sources: the [`nfsserve`](https://github.com/huggingface/nfsserve) README and
`src/vfs.rs` (trait and macOS `mount_nfs` example), and local `man mount_nfs`.
FSEvents delivery on NFS remains UNCONFIRMED. The attempt exercised Bun `fs.watch`
and an explicit kqueue check, without an FSEvents API test.

## Selected design

`macos-sparse-backend serve --base DIR --upper DIR --port N` serves one Layer as a
loopback NFSv3 export on `127.0.0.1`. A second structurally distinct caller shape,
one server exporting every Layer by export name, was rejected: one process per
Layer gives independent kill and remount, and a lock file per Layer.

- Unchanged entries are read from the shared base. The server never opens it for writing.
- `upper/tree/` holds created and copied-up entries. A write copies up only that file.
- `upper/whiteouts` lists hidden base paths and is replaced atomically. A whiteout
  also hides the base subtree beneath it, so a recreated directory starts empty.
- Copy-ups are staged in `upper/tmp/` and renamed into place; staging is cleared on open.
- A directory rename copies the visible subtree into `upper/tree/`. That subtree
  is no longer sparse; this matches OverlayFS-style limits and is recorded.
- `clone --from UPPER --to UPPER` copies only private state and refuses a Layer
  whose server holds its lock. It copies into a sibling staging directory, holds
  that directory's Layer lock through publication, and renames the completed
  directory into place. A competing server's destination is not removed on copy
  failure. Hard links are unsupported by `nfsserve`.

On the hosted runner, the base is owned by `root:wheel`, so the server user cannot
write it. Mounting requires `sudo -n /sbin/mount_nfs`; this is not a non-admin setup proof.
Layers mount with `vers=3,tcp,locallocks,noac,nonegnamecache,soft,rdirplus`.
`rdirplus` is required: `nfsserve` 0.11.0 ignores the cookie for plain READDIR.

## Run

```sh
cd tools/macos-sparse-backend
rtk cargo test --locked
```

The tests exercise the overlay through the NFS trait without mounting. One test
aborts a child process during copy-up and reopens the Layer. Another serves a
Layer and checks that clone refuses it. Mounting runs only in
`.github/workflows/macos-sparse-backend.yml` on a disposable `macos-26` runner:
`fskit-probe.mjs` records FSKit prerequisites, then `attempt.mjs` mounts four Layers
and runs the pinned probe plus clone, kill-recovery and storage checks.

## Results

The retained hosted runs below tested old executable source through
`39a065b075f2a55688628eb7b3fd64f75b05334e`. They do not validate the subsequent
PR comment fixes. PR #20 also measures old `39a065b`, not the revised source.
Every retained run
used `macos-26` image `20260907.0351.1`, macOS 26.6.2 (25G83) on arm64, Xcode 26.6
(17F113), MacOSX 26.5 SDK, Rust 1.99.0 and Bun 1.3.13. Archives expire 2026-10-28.

| Run | Source | Artifact (digest) | Result |
| --- | --- | --- | --- |
| [38505212596](https://github.com/Dhruv2mars/javelin/actions/runs/38505212596) | `25033c0` | `11867613085` (`eb1e50b1…39a1`) | NOT VERIFIED: stale handle after file rename |
| [38505779476](https://github.com/Dhruv2mars/javelin/actions/runs/38505779476) | `4847a18` | `11866888720` (`53d01202…6c40`) | VERIFIED, before review fixes |
| [38506206609](https://github.com/Dhruv2mars/javelin/actions/runs/38506206609) | `1c503a2` | `11867809729` (`3a22fa6a…632b`) | VERIFIED after review r1 fixes |
| [38507420049](https://github.com/Dhruv2mars/javelin/actions/runs/38507420049) | `39a065b` | `11868946498` (`f456a51d…f0cf`) | VERIFIED on the tested source |

### PR comment fixes on 2026-10-16

Comments `4292227991`, `4292228003` and `4292228021` identified attribute ordering,
clone publication and timestamp defects. Five regressions failed on the previous
implementation: read-only CREATE, mode-000 time updates, mode-only copy-up mtime,
clone entry mtimes, and exposure of an incomplete clone destination. The FIFO
clone fixture also checks that failed-copy rollback preserves a competing Layer.

The fixes apply size and time changes before restrictive chmod, use an
owner-permitted path operation for timestamps, and publish clones from locked
sibling staging directories. Copy-up and clone retain source access and
modification times, including directory times after copying children and symlink
times without changing the target.

For those fixes, local Darwin arm64 checks passed on Rust 1.93.0: format, warnings-denied
Clippy, 31 tests and release build. Two ignored child entry points run through
their parent crash tests. Added coverage includes directory and symlink copy-up
times. These are unmounted trait and process regressions, not a new hosted mount,
FSKit, performance or Gate 2 result. Known limits below remain open.

### Read-only parent fix on 2026-10-17

Comment `4298776857` identified failed copy-up of a writable base file beneath
read-only directories. Copy-up now temporarily grants owner access to its private
parents while holding the state mutex, then restores their intended modes before
returning, including on error. Ordinary new-file creation still observes the
read-only parent mode.

The focused regression failed on `76952fe` and passes with this fix. It covers
nested `0555` parents, a successful edit, an unreadable-source error, mode reporting
and reopen, unchanged base bytes, and rejection of new-file creation. The prior
timestamp fix from comment `4292228021` remains in the copy-up and clone paths.
No hosted or mounted result is claimed for this change.

The first run's stale handles came from a rename bookkeeping bug that stored the
renamed file as `README.txt/`. `4847a18` fixed it with a no-reopen regression.
The first run's watch failure did not recur; its cause is UNCONFIRMED.

### Loopback NFS sparse overlay: bounded fixture evidence

Run 38507420049's `nfs/summary.json` contains 19 entries: 18 required checks are
VERIFIED and one common-probe clone check is INCONCLUSIVE. `attempt.mjs` excludes
that clone check by design when computing its aggregate VERIFIED verdict.
Cleanup reports no remaining mounts, servers or errors. The runner's trait-level
suite reports 25 passed tests and two ignored child-test entry points.
These results establish bounded feasibility, not backend correctness or Gate 2 acceptance.

The raw mounted fixture records the following results:

- Real `nfs` mounts of three Layers over one `root:wheel` base the server user cannot write.
- Ten behavior checks in the pinned common probe are VERIFIED: reads, in-place writes, create, atomic-save
  rename, delete, directory rename, traversal, relative symlink, Bun `fs.watch`,
  `cc` build and run, independent process writes, writer SIGKILL interruption,
  and final base and peer integrity. Its eleventh check, clone, is INCONCLUSIVE
  in paths mode. The complete report has `behaviorVerdict: INCONCLUSIVE` and exit 1.
  Its separate `pathBoundary` is NOT VERIFIED: an absolute symlink read a sibling
  view. Its four contract checks are INCONCLUSIVE. Report completeness does not
  mean the entire probe passed.
- Bun `fs.watch` with a fresh top-level watcher per operation and kqueue
  `NOTE_WRITE` on a mounted directory. FSEvents, recursive watching and event
  ordering remain untested.
- Busy-Layer clone refusal; quiescent clone after unmount, identical remount and
  independent divergence, including a compiler run in the clone.
- Server SIGKILL with the mount active, followed by forced unmount, restart and
  remount, keeps the fixture's fsynced state. The attempt does not test continued
  I/O through the same mount or existing open handles after restart.
- Storage: the base digest is unchanged. B wrote a compiler output and a concurrent
  file, then removed them, so it has zero private file bytes at the end. C was
  untouched and has zero private file bytes. This is an end-state measurement,
  not proof that B never created private state.
  A holds 8,263 private bytes against an 8,339-byte base, and the clone holds 8,245.
  Unchanged `main.c` was never copied. A 1-byte write copied the whole 8,192-byte
  `bytes.bin`, and the directory rename materialized `renamed-dir/nested.txt`.

### FSKit: INCONCLUSIVE, missing per-user module enablement

Recorded on every run: `xcodebuild -license check` exit 0, 0 valid code-signing
identities, FSKit headers including `FSPathURLResource`, and three system FSKit
modules. Apple's sample downloaded with the expected SHA-256 and built ad-hoc.
The extension carries `com.apple.developer.fskit.fsmodule` with an ad-hoc
signature and no provisioning profile. `pluginkit -a` registered it, and
`pluginkit -e use` exited 0 and marked it elected. `mount -F -t passthrough` then
exited 69: `Module com.example.apple-samplecode.Passthrough.AppEx is disabled!`

The runner image reports SIP disabled, and AMFI logs `developer mode is force
enabled` and `Enforcement disabled`. This unit did not change those settings and
does not rely on them. Even so, FSKit refused the module. The missing prerequisite
is the per-user File System Extensions enablement. Apple documents only the System
Settings toggle, plus a paid-team profile for the restricted entitlement. The
passthrough sample is not sparse, so a future FSKit mount would need its own overlay.

## Review

Earlier read-only review used `gpt-6.1-sol` at high effort over six rounds.
Those rounds and their regressions do not close the gaps found by the hub audit below.

- r1 on `25033c0`: four P1 and seven P2 issues. Nine were fixed in `1c503a2`:
  rename ordering, symlink-handle writes, CREATE truncation, probe completeness,
  whiteout publication, durability syncs, cleanup tracking, nested clones and mode
  bits. Plain READDIR pagination and case-insensitive storage remain limits.
- r2 to r5: stale or misdirected handles and whiteout memory when a sync or
  persistence step failed after a rename or delete, plus relative clone paths and
  partial clone rollback. Fixed in `84f3402`, `96a28bc`, `1a6b774` and `39a065b`.
- r6 on `39a065b` reported no blocking issue within its review scope. It swept every NFS operation for errors
  that leave handles or whiteouts inconsistent with disk. Mounted I/O failure
  injection was not exercised.

The hub's independent audit of `39a065b`, staged at
`/tmp/javelin-hub-audit-pr11`, found evidence overclaims and correctness/setup gaps.
This docs-only correction checks the raw `nfs/summary.json`, `nfs/probe-paths.json`,
`nfs/commands.jsonl`, `environment.log` and `cargo.log` in artifact `11868946498`
against the source. At the time of the October 14 docs-only correction, all seven
component hashes in `environment.log` matched `39a065b`. The component has since
changed; those hashes are historical provenance. The audit recommends bounded feasibility after
these corrections; the hub still owns acceptance.

## Limits and next units

These gaps remain open in the revised implementation. The October 14 audit
correction changed documentation only; the October 16 fixes above do not close
these gaps. Source findings below come from
`tools/macos-sparse-backend/src/lib.rs`, `src/main.rs`, `attempt.mjs`, and the
locked `nfsserve` 0.11.0 source. They are not new hosted runtime proofs.

- Restart changes `nfsserve`'s handle generation and resets the overlay's in-memory
  file IDs. Existing handles on the same mount become stale and I/O can return
  ESTALE. `vfs.rs` derives the generation from startup time; `Overlay::open`
  recreates the ID maps. The recorded kill proof forcibly unmounts and remounts.
  Same-mount recovery is an unresolved correctness unit.
- The fixture requires sudo for mounting, unmounting and base ownership. Normal
  user setup remains an unresolved unit. Passwordless sudo on this runner does
  not establish that setup on a developer Mac.
- Loopback binding does not isolate local users. `nfsserve` parses AUTH_UNIX but
  does not enforce its identity in the NFS operations. Its ACCESS handler returns
  requested rights for this read-write export. Any local user who connects to
  the port can access the Layer under the server process's OS permissions.
  Authentication and permission enforcement remain unresolved correctness work.
- The hosted work volume is case-insensitive. `base_visible` compares exact path
  spellings while APFS accepts case and Unicode-normalization aliases. An alias
  can bypass a whiteout and re-expose a deleted base entry or receive a separate
  handle. Consistent name resolution is an unresolved correctness unit. The raw
  host check measures case sensitivity only; normalization behavior was not
  exercised by the mounted fixture.
- `nfsserve` drops the plain READDIR cookie, so the fixture uses `rdirplus`.
  The overlay's paginated listing also returns `NFS3ERR_BAD_COOKIE` if the entry
  identified by the resume cookie has been removed. Listing during mutation
  remains unresolved even with `rdirplus`.
- Overlay filesystem operations hold one global mutex across filesystem I/O. Each WRITE calls
  `sync_data`, and copy-up calls `sync_all`. Writes copy the whole base file,
  including before a truncate through `setattr` or CREATE. Throughput, latency
  and large-file cost are unmeasured.
- `add_whiteouts` clones, serializes and replaces the entire whiteout set for
  each new deletion. Mass deletion of N base entries has O(N²) cumulative work
  in the number of entries, before path-length and tree-operation costs.
- Delete persists a whiteout before removing an upper entry, and `locate` prefers
  the upper entry over that whiteout. A crash between these steps can leave the
  file visible after restart and lose the intended delete. Rename interruption
  before whiteout persistence can also re-expose the old base name. Crash-safe
  mutation recovery remains unresolved; the current kill fixture does not
  exercise either window.
- `nfsserve` returns RPC procedure-unavailable for LINK and COMMIT. Directory
  rename copies the visible subtree. Hard links are unsupported; xattrs, ACLs
  and FSEvents compatibility are unverified.
- Killing a clone can leave a `.javelin-clone-*` sibling staging directory.
  Ordinary copy failures remove staging; automatic collection after process kill
  is unimplemented. An incomplete staged clone is not published as its destination.
- Killing copy-up while private parent permissions are temporarily widened can
  leave those host modes widened. Recovery of that permission window remains
  open and is not covered by the retained kill fixture.
- Product integration, automatic checkpoints, native storage and the CLI are absent.
- The NFS client and server are on one host. Remote NFS, sleep and network loss are untested.
- The selected process-kill and trait-level crash fixtures cover only their
  recorded operations. Power loss and mounted I/O failure injection are untested.

These correctness and setup gaps need separate scoped work and verification.
