# macOS sparse Layer backend attempt

Status: bounded hosted attempt. Results are recorded below only from actual runs.
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
write it. Layers mount with `vers=3,tcp,locallocks,noac,nonegnamecache,soft`.

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

Pending the first hosted run.

## Limits

- Product integration, automatic checkpoints, native storage and the CLI are absent.
- The NFS client and server are on one host. Remote NFS, sleep and network loss are untested.
- Directory rename materializes its subtree. Hard links, xattrs and ACLs are unsupported.
- Whiteouts and copy-ups survive process kill. Power loss is untested.
