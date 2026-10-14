# macOS sparse Layer backend attempt

Status: bounded hosted attempt complete. Results below come only from downloaded run archives.
This document does not claim CLI, native database binding, autosave, Gate 2 or GA.

## Source and dependencies

- Starts from hub commit `c5c0ebec93cb80308043192628ed452ff6f5d66d` on `chore/ga-build-plan`.
- PR #5 (`tools/workspace-probe/`) verified only directory and clonefile controls on
  macOS. Those controls validate a probe and never count as sparse proof.
- The hosted workflow checks out PR #5's probe at
  `0e7ed045774b18819ac12f62f44ba0fdfdc641a4` as developer setup and verifies
  `probe.mjs` SHA-256 `617f009e…0b49`. PR #5 and PR #7 remain unmerged dependencies;
  using the probe is not acceptance of either PR. No product command invokes Git.
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
Whether FSEvents delivers events for NFS volumes was UNCONFIRMED before the run.

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
  whose server holds its lock. Hard links are unsupported by `nfsserve`.

On the hosted runner, the base is owned by `root:wheel`, so the server user cannot
write it. Layers mount with `vers=3,tcp,locallocks,noac,nonegnamecache,soft,rdirplus`.
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

Tested executable source is `39a065b075f2a55688628eb7b3fd64f75b05334e`. Every run
used `macos-26` image `20260907.0351.1`, macOS 26.6.2 (25G83) on arm64, Xcode 26.6
(17F113), MacOSX 26.5 SDK, Rust 1.99.0 and Bun 1.3.13. Archives expire 2026-10-28.

| Run | Source | Artifact (digest) | Result |
| --- | --- | --- | --- |
| [38505212596](https://github.com/Dhruv2mars/javelin/actions/runs/38505212596) | `25033c0` | `11867613085` (`eb1e50b1…39a1`) | NOT VERIFIED: stale handle after file rename |
| [38505779476](https://github.com/Dhruv2mars/javelin/actions/runs/38505779476) | `4847a18` | `11866888720` (`53d01202…6c40`) | VERIFIED, before review fixes |
| [38506206609](https://github.com/Dhruv2mars/javelin/actions/runs/38506206609) | `1c503a2` | `11867809729` (`3a22fa6a…632b`) | VERIFIED after review r1 fixes |
| [38507420049](https://github.com/Dhruv2mars/javelin/actions/runs/38507420049) | `39a065b` | `11868946498` (`f456a51d…f0cf`) | VERIFIED on the tested source |

The first run's stale handles came from a rename bookkeeping bug that stored the
renamed file as `README.txt/`. `4847a18` fixed it with a no-reopen regression.
The first run's watch failure did not recur; its cause is UNCONFIRMED.

### Loopback NFS sparse overlay: VERIFIED for the bounded fixture

Run 38507420049 passed all 19 required checks with zero cleanup errors, and 25 local tests
passed on the runner. Run 38506206609 on `1c503a2` produced the same check results:

- Real `nfs` mounts of three Layers over one `root:wheel` base the server user cannot write.
- The pinned probe's complete report: reads, in-place writes, create, atomic-save
  rename, delete, directory rename, traversal, relative symlink, Bun `fs.watch`,
  `cc` build and run, independent process writes, writer SIGKILL interruption,
  and final base and peer integrity. Its own clone check is INCONCLUSIVE by design
  in paths mode; it exited 1 only for that reason.
- kqueue `NOTE_WRITE` on a mounted directory.
- Busy-Layer clone refusal; quiescent clone after unmount, identical remount and
  independent divergence, including a compiler run in the clone.
- Server SIGKILL with the mount active, forced unmount, restart and remount keep fsynced state.
- Storage: the base digest is unchanged. Layers B and C hold no private files.
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

Independent read-only review used `gpt-6.1-sol` at high effort over six rounds.
Every fix has a regression that fails when the fix is reverted.

- r1 on `25033c0`: four P1 and seven P2 issues. Nine were fixed in `1c503a2`:
  rename ordering, symlink-handle writes, CREATE truncation, probe completeness,
  whiteout publication, durability syncs, cleanup tracking, nested clones and mode
  bits. Plain READDIR pagination and case-insensitive storage remain limits.
- r2 to r5: stale or misdirected handles and whiteout memory when a sync or
  persistence step failed after a rename or delete, plus relative clone paths and
  partial clone rollback. Fixed in `84f3402`, `96a28bc`, `1a6b774` and `39a065b`.
- r6 on `39a065b`: no blocking issue. It swept every NFS operation for errors
  that leave handles or whiteouts inconsistent with disk. Mounted I/O failure
  injection was not exercised.

## Limits

- The hosted work volume is case-insensitive. Different spellings of one name can
  bypass a whiteout or receive separate handles. Production needs case-sensitive
  backing storage or consistent name resolution.
- `nfsserve` 0.11.0 drops the plain READDIR cookie; large directories need `rdirplus`.
- Copy-up is per file. A small write to a large base file copies the whole file.
- An interruption after a rename moves private bytes, before whiteouts persist,
  can re-expose the old base name as a duplicate. Data is not lost.
- One process serves one Layer under a global lock. Throughput was not measured.
- Product integration, automatic checkpoints, native storage and the CLI are absent.
- The NFS client and server are on one host. Remote NFS, sleep and network loss are untested.
- Directory rename materializes its subtree. Hard links, xattrs and ACLs are unsupported.
- Whiteouts and copy-ups survive process kill. Power loss is untested.
