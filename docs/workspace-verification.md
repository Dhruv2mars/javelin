# Layer filesystem verification

PR [#5](https://github.com/Dhruv2mars/javelin/pull/5) delivers a VERIFIED
ordinary-path probe on this macOS host. The real sparse-backend result is
INCONCLUSIVE. No production backend is accepted, and filesystem Gate 2 remains
open. This follows the hub's clarified Gate 2 at planning commit `8db2a5b`.
The final executable probe source is commit `bce352ec4e5f78eb98c71facba68b872d3e11118`.

The contract requires immutable shared base content, private writes, deletions
and renames, sparse Layer clones, ordinary external-tool paths, prevention of
accidental cross-Layer leakage, and interruption recovery. All those requirements
must pass on a real sparse backend. Directory and clonefile controls establish
that the probe can detect behavior; they do not establish that storage model.

## Reproducible evidence

The [probe instructions](../tools/workspace-probe/README.md) describe arguments,
budgets, caller-provided paths, cleanup, and exit codes. The implementation is
[probe.mjs](../tools/workspace-probe/probe.mjs), with a
[writer process](../tools/workspace-probe/atomic-writer.mjs), a direct
[clonefile helper](../tools/workspace-probe/clonefile.c), and
[FSClient discovery](../tools/workspace-probe/fskit-discovery.m).

The exact parent verification commands were:

```sh
rtk proxy bun tools/workspace-probe/verify.mjs /tmp/javelin-probe-evidence
rtk proxy bun tools/workspace-probe/verify.mjs
rtk proxy bun tools/workspace-probe/discover.mjs /tmp/javelin-host.json
rtk proxy /usr/sbin/diskutil info /System/Volumes/Data
```

The [committed evidence](../tools/workspace-probe/evidence/macos-arm64/)
contains command outputs and fixture hashes.
The final omitted-output run created the unique directory
`javelin-probe-evidence-6sxr25` under the host temporary directory and exited 0.
Its JSON files are the committed behavior evidence. The hub also independently
exported and verified the prior source commit `0f15db2`, including safety cases.

The observed results are:

| Capability | Directory control | Clonefile control | Real sparse backend |
| --- | --- | --- | --- |
| Text and binary reads and private in-place writes | VERIFIED | VERIFIED | INCONCLUSIVE |
| Create, direct atomic-save rename, delete, directory rename and traversal | VERIFIED | VERIFIED | INCONCLUSIVE |
| Relative symlink reads and writes within a view | VERIFIED | VERIFIED | INCONCLUSIVE |
| Top-level create, replacement, delete watch notifications | VERIFIED | VERIFIED | INCONCLUSIVE |
| C compiler and generated executable access through ordinary paths | VERIFIED | VERIFIED | INCONCLUSIVE |
| Concurrent separate writer processes and base and peer integrity | VERIFIED | VERIFIED | INCONCLUSIVE |
| Clone of a modified snapshot, subsequent independent writes | VERIFIED | VERIFIED | INCONCLUSIVE |
| Writer SIGKILL before and after atomic-save rename | VERIFIED | VERIFIED | INCONCLUSIVE |
| Immutable shared base and sparse namespace changes and clones | NOT VERIFIED | NOT VERIFIED | INCONCLUSIVE |
| Backend termination, restart, remount and durable checkpoint recovery | INCONCLUSIVE | INCONCLUSIVE | INCONCLUSIVE |

Both controls pass eleven checks and exit 0. A deliberate hard-link between A
and B makes six checks fail and exits 1, including in-place writes and final peer
integrity. This proves that ordinary-path leakage can fail the probe.

Caller-prepared directory paths pass ten checks. Cloning a modified Layer is
INCONCLUSIVE because that mode has no backend lifecycle adapter. Unsafe output
paths are rejected without modifying fixture bytes, including symlink aliases.
Hard-linked destinations and FIFOs are rejected too. A 2-second wall abort with
a deliberately stalled shell tool proves that both that tool and its subprocess
stop. The incomplete report exits 124. Report creation uses exclusive open after
approved previous evidence is removed, so existing aliases cannot be overwritten.
An overlapping-root configuration exits 2. A deliberate 300-ms wall timeout
exits 124 and records INCONCLUSIVE. The verification runner exits 0 after checking
all these expected results. None of its caller-path evidence represents a mount.

Watch checks use fresh watchers for each operation and observe only top-level
notifications. They do not prove recursive watches, event ordering, or delivery
under load. Snapshot hashes exclude timestamps and access-time effects.

The interruption test kills the writer after its ready message. Before rename,
the old target survives with an unpublished temporary file. After rename and
file and directory synchronization, the new target survives process death.
This does not prove survival of backend death, power loss, or checkpoint recovery.

An absolute symlink from A can read B's bytes on these controls. Path confinement
is therefore NOT VERIFIED. This is explicit same-user access through a link;
it is separate from accidental leakage during private writes. The experiment
only targets disposable paths. Host filesystem semantics are not an agent sandbox.

## Actual macOS host and FSKit prerequisites

Observed on 2026-10-13:

| Fact | Observation |
| --- | --- |
| OS and architecture | macOS 26.6.2, build 25G83, Darwin 25.6.0, arm64 |
| Resources | 16 GiB memory, eight CPUs; no stress or mount experiments |
| Fixture filesystem | APFS, 4096-byte blocks, about 105 GB available at discovery |
| Development tool | Bun 1.3.13 |
| Selected developer directory | `/Library/Developer/CommandLineTools` |
| Active SDK | macOS 27.0; FSKit headers present |
| Installed Xcode | Xcode 27.0, build 27A266a, through per-command `DEVELOPER_DIR` |
| Code-signing identities | `security find-identity -v -p codesigning` reports zero valid identities |
| FSKit executable discovery | Framework links and executes; FSClient lists enabled Apple exfat, msdos and ftp modules only |
| Directory resource | `FSPathURLResource` class exists on the runtime |
| New mount-client API | SDK requires macOS 27.0; selector is absent on this 26.6.2 runtime |

Apple describes FSKit as a user-space app extension connected to ordinary
filesystem tools. It is not a ready-made Layer union implementation.
[FSKit overview](https://developer.apple.com/documentation/fskit/)

The installed headers mark the original APIs available in macOS 15.4 and path
and generic URL resources in 26.0. Apple documents a directory-backed passthrough
sample using an app, an extension, the FSKit module entitlement, and user enablement
under General, Login Items & Extensions, File System Extensions. Its documented
mount path uses `mount -t passthrough source target`.
[Apple passthrough sample](https://developer.apple.com/documentation/fskit/building-a-passthrough-file-system),
[path resource](https://developer.apple.com/documentation/fskit/fspathurlresource),
[module entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.fskit.fsmodule)

The parent independently downloaded Apple's
[official sample archive](https://docs-assets.developer.apple.com/published/0b4283600908/BuildingAPassthroughFileSystem.zip).
Its SHA-256 was `81c63039eea41ec539bbe3b7486efa426bce295b8a2c6099104fdf3b7925ed46`.
Only sample source and project files were extracted into a temporary directory.
The project specifies automatic `Mac Developer` signing, an empty development
team, and an extension entitlement file containing the FSKit module entitlement.
This independently confirms the subordinate research's prerequisite observations.

The exact bounded build attempt was:

```sh
rtk proxy env DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -project /tmp/javelin-official-passthrough/Passthrough.xcodeproj -scheme Passthrough -configuration Debug -derivedDataPath /tmp/javelin-passthrough-build -jobs 1 build
```

It exited 69 before compilation and requested agreement to the Xcode license.
The license was not accepted. Discovery's `xcodebuild -checkFirstLaunchStatus`
also exits 69. Using a per-command developer directory avoids changing the
host's selected toolchain. [Apple command-line tool reference](https://developer.apple.com/documentation/xcode/xcode-command-line-tool-reference)

No custom extension was built, signed, installed, enabled, or mounted. No privileged
component or security setting changed. The host meets the observed framework and
directory-resource minimums, but executable extension deployment is INCONCLUSIVE.
Ad-hoc signing acceptance, entitlement provisioning, backing-directory sandbox
access, mount permissions, and extension recovery remain unconfirmed. We looked
at Apple's sample, entitlement and resource documentation, installed SDK headers,
FSClient enumeration, signing identity output, and the actual build failure.
Zero identities alone does not prove that all possible development signing paths
are unavailable.

## Platform choices and untested requirements

The first macOS implementation experiment remains provisionally FSKit. Its
documented directory-resource mechanism fits an ordinary-path experiment on
this OS. Custom immutable lookup, private changes, deletion markers, rename
handling, snapshot cloning, and recovery are still implementation and verification
work. No performance or production suitability claim follows from these controls.

Linux OverlayFS documents shared lower trees, private writable upper and work
directories, copy-up, and deletion markers. The work directory must share the
upper filesystem; writable uppers require supported extended attributes and
directory-entry types. Lower directory rename can fail with `EXDEV` unless
redirect handling is enabled. Default copy-up synchronization is not a general
metadata durability promise; `fsync=strict` strengthens it. Sharing upper or
work directories between mounts is unsupported.
[Linux kernel OverlayFS documentation](https://docs.kernel.org/filesystems/overlayfs.html)

OverlayFS is a plausible contract fit, not a verified Linux choice. A Linux run
still needs mount privileges, namespace and external-tool visibility evidence,
chosen mount options, frozen lower content, sparse clone storage evidence,
whiteout and directory-rename checks, and interrupted mount and checkpoint recovery.
No Linux kernel, mount, or compiler was exercised by this PR.

Windows ProjFS documents projected entries, hydration into local storage, full
local files after application writes, and tombstones after deletion. A provider
could share immutable source content across virtualization roots. That is an
implementation inference; hydration can also cache unchanged bytes.
[Microsoft ProjFS cache states](https://learn.microsoft.com/en-us/windows/win32/projfs/cache-state)

ProjFS requires enabling the optional `Client-ProjFS` component with elevation,
and restart if requested. No Windows component was inspected or enabled here.
[Microsoft ProjFS enablement](https://learn.microsoft.com/en-us/windows/win32/projfs/enabling-windows-projected-file-system)

Windows still needs a normal Windows-tool runner, provider startup and restart,
rename and hydration identity checks, local edit preservation, clone storage
evidence, watch behavior, and recovery verification. The current shell-tool
runner is POSIX-oriented. Windows support is INCONCLUSIVE, as is Linux support.

## Smallest next implementation step

Resolve Xcode's license and demonstrate an accepted signing and entitlement path,
then build and enable Apple's unchanged passthrough extension. Mount one
disposable directory and test ordinary reads, writes, renames, and deletes from
another process. This establishes deployment and resource plumbing only.

After that, implement one minimal FSKit sparse Layer over immutable base bytes
with a private change store, then two isolated views and one snapshot clone.
Run this probe against their actual mounted paths. Add backend restart and clone
provisioning evidence outside the path-only probe, and verify shared base content
and sparse storage directly. Gate 2 can close only after those checks pass.
The production backend recommendation remains unconfirmed until then.
