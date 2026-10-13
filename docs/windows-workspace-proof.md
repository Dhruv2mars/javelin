# Windows projected filesystem proof

This bounded proof owns one disposable GitHub-hosted Windows job and draft
[PR #9](https://github.com/Dhruv2mars/javelin/pull/9), based on unmerged
[PR #7](https://github.com/Dhruv2mars/javelin/pull/7). The unchanged common probe
comes from accepted PR #5 source `bce352e`; Linux source `89d76df` and evidence
`19224ef` are dependencies, not Windows evidence. No product commands, metadata
binding, autosave or GA gate are implemented here.

## Bounded outcome

Done requires a rerunnable hosted attempt with raw prerequisite results and exact
source, job and artifact provenance. A missing prerequisite or required restart
ends this attempt with the backend INCONCLUSIVE. A successful CI collection job
is not a filesystem pass. There is no laptop projection, Windows installation,
reboot, license acceptance or host security change.

## Architecture before implementation

Two read-only architecture candidates and a cross-judge compared these shapes.
All used the same model family, so review diversity was limited.

| Shape | State ownership | Cost and decision |
| --- | --- | --- |
| Direct immutable backing tree | One shared read-only tree; separate ProjFS/NTFS roots retain cache, full files and tombstones | Chosen. One native SDK C++ provider owns callback and lifecycle state. Smallest way to test actual Windows semantics. |
| Immutable path-to-blob index plus per-root manifest | Provider resolves deleted paths, changed blob references, then base entries; reconciles notifications and NTFS with serialized state | Rejected for the initial proof. Adds indexing, serialization and a second write/recovery protocol before availability is known. |

The caller's intended interface, only after prerequisites pass, is:

```text
provider serve <shared-immutable-tree> <owned-root>
audit quiescent-clone <source-root> <immutable-sparse-snapshot> <fresh-root>
provider serve <shared-immutable-tree> <same-surviving-root>
```

The native sketch is one `Provider` owner with a virtualization context, stable
instance identity and enumeration sessions. Its required callbacks enumerate
sorted base entries, provide placeholder metadata, and supply requested byte
ranges. Sessions retain search expression and cursor and handle restart scans;
ordering uses `PrjFileNameCompare`. `start` marks a fresh root once and starts
virtualization; `stop` waits for callbacks to end. Reopening preserves the existing
root and identity. These are proposed behaviors, not executed evidence.

The useful part of the manifest candidate is a quiescent snapshot containing
only changed content references and deleted path records. A fresh clone would
resolve deleted, changed, then base paths. It must omit read hydration, preserve
metadata changes explicitly, measure physical storage and prove isolation. Copying
the entire root would copy cache and cannot prove a sparse Layer.

Interface screening favored one owner over public SDK wrappers or a second storage
subsystem. We accept NTFS state dependence to test the smallest provider first.
Same-root kill/restart and fresh-root clone remain separate proof obligations.
Implementation begins with prerequisite collection only; a blocked gate means no
provider is written or started.

## Source grounding

Microsoft documents ProjFS as an optional component enabled in an elevated shell
using `Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart`.
If it reports `RestartNeeded: True`, the guide requires a reboot. This proof stops
there. The guide does not establish availability on a particular hosted image.
[Enablement guide](https://learn.microsoft.com/en-us/windows/win32/projfs/enabling-windows-projected-file-system).

Virtual files have no local bytes. Placeholders cache metadata, hydrated placeholders
cache read content, dirty placeholders record metadata edits, full files contain
local content changes or new files, and tombstones suppress deleted base paths.
Local entries take precedence over projection. Hydration therefore cannot count
as private changed content. Actual counts and allocation costs require a running
provider. [Cache states](https://learn.microsoft.com/en-us/windows/win32/projfs/cache-state).

The lifecycle guide documents `PrjMarkDirectoryAsPlaceholder`, the required
enumeration/placeholder/data callbacks, `PrjStartVirtualizing`, reuse of an existing
root, and `PrjStopVirtualizing`. The enumeration guide requires restart handling,
search matching and filename ordering. Data delivery uses `PrjWritePlaceholderInfo`
and `PrjWriteFileData`; symlinks need extended placeholder and directory-entry APIs.
State inspection must run outside callbacks because `PrjGetOnDiskFileState` can
trigger callbacks and deadlock. These docs define the API contract, not observed
atomic-save, clone or recovery behavior.
[Programming guide](https://learn.microsoft.com/en-us/windows/win32/projfs/projfs-programming-guide),
[lifecycle](https://learn.microsoft.com/en-us/windows/win32/projfs/virtualization-instance-lifecycle),
[enumeration](https://learn.microsoft.com/en-us/windows/win32/projfs/enumerating-files-and-directories),
[file data](https://learn.microsoft.com/en-us/windows/win32/projfs/providing-file-data),
[state inspection](https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/nf-projectedfslib-prjgetondiskfilestate).

Microsoft's native RegFS sample is a read-only registry projection and prevents
modification, deletion and rename. It is a lifecycle reference, not evidence for
writable Layers. [Sample README](https://raw.githubusercontent.com/microsoft/Windows-classic-samples/main/Samples/ProjectedFileSystem/README.md).

## Portability and verification limits

If a provider becomes available, run unchanged `tools/workspace-probe/probe.mjs
--backend paths` with the runner's Unix tools. Record actual `cat`, `cp`, `mv`,
`rm`, `cmp` and shell paths/versions first. Missing tools leave affected checks
INCONCLUSIVE. The common caller-path clone verdict stays INCONCLUSIVE; a separate
actual sparse clone audit must supply that evidence. POSIX FIFO and process-group
negative checks in `verify.mjs` are not Windows evidence. Windows children need
independent bounded tree termination. No sandbox or hardware-powerloss claim is
made. Feature setup identity and actual non-admin provider identity must be distinct
in evidence; elevated runner access is not an ordinary-user pass.

## Attempt and verdicts

Hosted source, job, artifact and exact raw results will be recorded after the attempt.
Availability, backend ordinary-path behavior, sparse clone, storage cost, restart
recovery, ordinary-user operation, product Gate 2 and GA are currently INCONCLUSIVE.
