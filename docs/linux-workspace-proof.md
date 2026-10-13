# Linux sparse Layer proof

This is a bounded development proof, dependent on PR #5's ordinary-path probe.
It implements actual Linux OverlayFS mounts for a small owned fixture. It does
not integrate the native database or complete product Gate 2 or GA.

The bounded sparse filesystem proof is **VERIFIED** in a real disposable hosted
run at `89d76df34bf85daee9852ffc9350109bd13b2daf`. All eight backend phases passed.
The common probe verified ten ordinary-path checks and kept its caller-path clone
check INCONCLUSIVE; independent actual-backend evidence verified cloning.
No privileged operation ran on the laptop. Product Gate 2 and GA remain open.

## Observed cloud result

[Run 38430385798](https://github.com/Dhruv2mars/javelin/actions/runs/38430385798)
used Ubuntu 24.04.5, image `20261004.327.1`, Linux `6.17.0-1022-azure`, x64,
Bun 1.3.13 and ordinary UID 1001. The exact checked-out source was
`89d76df34bf85daee9852ffc9350109bd13b2daf`, including PR #5's final common probe
source `bce352ec4e5f78eb98c71facba68b872d3e11118`.
[Artifact 11830438463](https://github.com/Dhruv2mars/javelin/actions/runs/38430385798/artifacts/11830438463)
has reported archive SHA-256
`a7c1fa21d8040aeb33a0e3bf62f963bf7e92798f843680448db5bfc0a1f624dc`.
Its 19 files are preserved unchanged under
[`tools/overlayfs-proof/evidence/linux-x64`](../tools/overlayfs-proof/evidence/linux-x64/)
with a separate cloud metadata manifest. This preserves evidence after the cloud
artifact expires on 2026-10-27.

The base's 8,339 regular-file bytes, hashes, metadata and inodes remained unchanged.
A's frozen sparse snapshot stored 8,244 changed/new file bytes, including the full
8-KiB file changed at one byte. Untouched `main.c` stayed absent from all uppers.
The source continuation added 24 bytes and the clone added 17 bytes in fresh
private uppers. The other two uppers held zero regular bytes. These byte counts
exclude directory blocks and xattrs; raw inventories also retain allocated bytes.
There is no storage-efficiency claim from this tiny fixture.

Actual 0/0 whiteouts for `delete.txt` and `dir` and a
`trusted.overlay.opaque=y` renamed directory survived the shared snapshot clone.
The new source/clone uppers began empty. External compilers read the shared source
through the mounted paths and produced private values 9 and 11. Both SIGKILL
boundaries survived unmount/remount. Cleanup removed every owned mount and root.
The observed absolute-symlink sibling access remains a documented sandbox limit.

Verify the retained evidence and compare executable source hashes locally:

```sh
rtk proxy bun tools/overlayfs-proof/verify-evidence.mjs
```

This offline audit reads JSON and source files; it never mounts or invokes Git.
The executable mount/probe sources still match the recorded cloud hashes. Later
evidence/documentation commits do not change the exact SHA that was cloud-tested.

## Reproduce in disposable CI

Push changes to this draft PR. The `Linux sparse Layer proof` pull-request
workflow checks out the exact head commit, records its SHA in a separate
development metadata step, installs pinned Bun 1.3.13 and executes:

```sh
timeout --signal=TERM --kill-after=45s 240s bash tools/overlayfs-proof/run.sh
```

Do not run the mount proof on a workstation or self-hosted runner. It refuses
non-Linux, non-hosted, root and caller-argument invocations before creating paths
or invoking sudo. It accepts no caller-supplied source tree. The existing common
probe seeds its exact six-file fixture. Three views, two immutable lower trees
after cloning and five distinct writable upper/work pairs bound the experiment.
Each upper stores files only when changed; an in-place byte write copies that
whole file, not a byte range. This is sparse at file granularity.

The workflow owns only `.github/workflows/overlayfs-proof.yml`; PR #6 owns
`native.yml`. It grants `contents: read`, persists no checkout credentials,
uses full commit pins for official checkout/upload-artifact v7 and setup-bun v2,
and uploads text/JSON logs on success or failure for 14 days. No package,
privileged component, kernel module or security policy is installed or changed.

## Prerequisites and design

[Official kernel documentation](https://docs.kernel.org/filesystems/overlayfs.html)
requires suitable upper filesystem xattrs and directory types, and upper/work on
the same filesystem. Lower trees can be shared; upper/work must not overlap
between active mounts. Copy-up preserves ordinary writable paths. Deletion uses
whiteouts, represented by a 0/0 character device or an xattr-marked empty file.
Directory redirects, metadata-only copy-up and inode indexes have separate
semantics. This fixture disables those features with `redirect_dir=nofollow`,
`metacopy=off` and `index=off`. Lower directory rename can require a tool's copy
fallback; the probe also checks native rename of the resulting upper directory.

[GitHub's hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
documents disposable Linux VMs and passwordless sudo. The workflow selects
`ubuntu-24.04`, not the unprivileged `ubuntu-slim` container. Documentation does
not guarantee this precise mount/option combination. The real run must establish
kernel support, backing filesystem suitability and successful mounting.

The ordinary runner owns the fixture files and visible paths. A root-owned,
mode-700 backing parent hides writable aliases. The exposed base is a read-only
bind mount; a runner write must fail with EROFS and direct backing traversal must
fail with EACCES. This protects the fixture from the ordinary writer, not from a
privileged host administrator. Before/after byte hashes, metadata and inode
inventories independently check immutable content.

Cloning happens after the probe and all its writers exit. The proof unmounts A,
freezes its upper through a read-only bind mount, then gives A and the clone fresh
empty uppers over `snapshot:base`. It never copies the merged view or upper.
This design uses a shared sparse immutable snapshot with native whiteouts. It
tests snapshot equivalence, deletion/rename inheritance, independent writes and
compiler values 9 and 11. It does not implement cloning a live mutating Layer.

## Evidence and verdicts

Every phase must pass; a missing phase prevents a VERIFIED summary. Artifacts
include `initial.json` with exact source SHA, kernel, runner image, UID and source
hashes; the unchanged `common-probe.json`; raw bounded `storage-*.json` inventories
including inodes, allocated bytes, whiteouts and xattrs; mount options; clone and
recovery snapshots; commands and compiler output; cleanup and final summary.

The common caller-path probe must verify ten behavior checks and retain its one
INCONCLUSIVE clone-provisioning check. Its contract verdicts remain untouched.
The separate backend checks supply clone, immutable storage and remount evidence.
Initially every upper has zero regular bytes. After mutation, unchanged `main.c`
must be absent from every upper, each upper's regular-file bytes must be below
the base's 8,339 bytes, and the deletion record must be a real kernel whiteout.
At the clone boundary the new A/clone uppers must be empty. Final lower and frozen
snapshot inventories must be unchanged.

SIGKILL before rename must leave old bytes and a fsynced temporary file across
unmount/remount. SIGKILL after rename and directory fsync must leave committed
bytes across another unmount/remount. This proves writer process interruption
and clean remount of the same disk state. Machine power loss is **NOT VERIFIED**.
Automatic checkpoints, concurrent checkpoint publication and backend restart
transactions are follow-on work.

Every privileged command has a 15-second timeout and an exact owned-root marker
guard. Per-stage assertions have 25-second limits, the common probe 70 seconds,
the overall process 240 seconds and the job eight minutes. EXIT/TERM/INT cleanup
unmounts every known view before removing only the owned mkdtemp root. It never
uses lazy unmount; an unmount failure preserves the root and fails the proof.
A forced SIGKILL of the supervisor cannot guarantee trap cleanup; disposal of
the hosted VM remains the final boundary. Cleanup evidence must confirm no
remaining mounts under the root and its removal.

## Open limits

Absolute symlinks can intentionally reach sibling views. Ordinary Layer paths
provide private copy-up, not a security sandbox. Production permissions, mount
namespace confinement and untrusted-tree handling remain open. File watching
covers fresh top-level watchers; recursive watching and event order are unproven.
Stable inode identity, hardlink corner cases, special files, large trees, long
snapshot chains and performance are outside this fixture. Native DB integration,
automatic checkpoints and production mount lifecycle remain follow-on work.

Research checked current primary kernel documentation, GitHub's runner reference,
and the official [checkout](https://github.com/actions/checkout),
[upload-artifact](https://github.com/actions/upload-artifact) and
[setup-bun](https://github.com/oven-sh/setup-bun) repositories on 2026-10-13.
Action pins came from each official repository's tag-ref API. Bun's version came
from its [official release](https://github.com/oven-sh/bun/releases/tag/bun-v1.3.13).
The observed runner supports this exact fixture and mount-option combination.
Other kernels, runner images and production hosts remain unconfirmed.
