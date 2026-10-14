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
Implementation began with prerequisite collection only. Run `38465691765` then
confirmed the feature, compiler and extended APIs, allowing native implementation.
The quiescent snapshot also records aliases to existing immutable bytes for renamed
unchanged files; those bytes do not become copied private content. The prototype
is bounded to this small fixture, 128 entries and 256 KiB per backing file.

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

Every hosted attempt ran as a `pull_request` job on `windows-2022`. Original
archives, cloud run/job/artifact records and the exact executable bytes are kept
under `tools/windows-workspace-proof/evidence/<attempt>/`. Earlier failures stay.

| Attempt | Source | Run | Job | Recorded outcome |
| --- | --- | --- | --- | --- |
| initial | `4d817c5` | `38465330661` | success | SDK probe failed to compile: missing `<cwchar>` |
| prerequisite | `648474c` | `38465691765` | success | Feature enabled without restart, compiler and required exports present |
| native-compile | `1311f81` | `38467591404` | success | Native provider compile failed |
| runtime | `fcf9ac8` | `38467953801` | failure | Native `base` phase exit 3 |
| compiler-temp | `3ce299d` | `38468282388` | success | Native `build` phase exit 3 |
| behavior | `c10f95e` | `38468765691` | success | Native `mutate` phase exit 3 |
| clone-state | `ffcc90e` | `38469282514` | success | `directory-rename` and `clone-snapshot` exit 3 |
| **final** | **`21b2dda`** | **`38469723137`** | **success** | **Backend INCONCLUSIVE** |

The final executable is exactly `21b2ddaa9798d0b1bf81cd1b00467c29125d52f5`.
Artifact `11851736345` has digest
`sha256:62cdd12848000a277b6e3a82a7c9f98cf5daa93438701a9f7fbf69ee8c636af9`.
Its 99 raw files match the hub's separate download byte for byte.
Workflow success only means collection finished. The final summary itself says
backend INCONCLUSIVE, and every summary scope is INCONCLUSIVE.

**Source hashes.** The runner hashed sources after a Windows checkout, which
converted LF to CRLF. Every retained source file is the `21b2dda` blob with that
conversion applied, and `source-transform.json` records the canonical LF hash and
the cloud hash for each file. Audit limit: the owned verifier checks retained
source bytes against the cloud hashes, and checks the current tree directly or
after CRLF conversion. It does not read `source-transform.json` and does not bind
canonical bytes to a Git blob. Runtime and audit tools deliberately avoid Git. The
hub separately confirmed all 10 tested blobs and the CRLF conversion. Clone with
`core.autocrlf=false` so the retained bytes stay exact.

**Final run records (raw files in `final-attempt/raw/`).**

- Prerequisites: `feature.json` shows Client-ProjFS already `Enabled` before
  setup, so no enable or restart happened. Compiler and API checks were ready.
  Setup ran as elevated `runneradmin` (`summary.json` `ordinaryUserVerified: false`).
- Non-admin execution: `runtime-summary.json` records disposable user
  `jvpabfa80caf2ab` with `administrator: false`. Providers started on four roots
  and all stopped; the account was then removed (`backend.json`). These phases
  were executed as non-admin, but the summary scope `non-admin-provider` stays
  INCONCLUSIVE because the backend gate failed.
- Native phases: 14 of 15 exited 0 with phase verdict VERIFIED (job-termination,
  base, metadata, hydrate, build, build-clean, mutate, clone-snapshot,
  clone-verify, four recovery phases, final). Each is only as strong as its
  assertions, limited below. The `metadata` phase checks file size only; metadata
  edits were not tested.
- Directory rename: `directory-rename` exited 3. It renamed only base directory
  `a/dir` while that directory was a projected placeholder (state 1 in
  `state-placeholder-a.json`), and got `ENOTSUP`. The provider's
  `PRJ_NOTIFY_PRE_RENAME` handler returns `S_OK`, so the provider itself did not
  deny the rename. Renaming a locally created or full directory was not tested.
  The finding is: renaming a projected base placeholder directory fails with
  ENOTSUP here. It is not a claim about all directories.
- Common probe: unchanged `probe.mjs --backend paths` exited 1. Four checks were
  VERIFIED: text/binary read, in-place writes, relative symlink and watch.
  NOT VERIFIED:
  - create/atomic-save/rename/delete: `mv dir renamed-dir` reported "Function
    not implemented", the same base-directory rename case.
  - directory traversal: it still saw `delete.txt` and `dir/nested.txt`.
  - external compiler: `'9\r\n' !== '9\n'`, a POSIX newline assumption. The check
    stopped there, so the peer compile never ran.
  - independent writer and writer interruption: the writer exited before
    readiness with `EPERM` from `fsync`. The hub review traces this to the
    directory fsync after rename (`atomic-writer.mjs:15`); the captured stack
    does not name the line.
  - final base/peer integrity: leftover files such as `concurrent.txt` differed
    from expectations.
  Clone is INCONCLUSIVE by design. Leftover probe files (`concurrent.txt`,
  `recovery.txt`, `probe-app.exe`) also ended up in the native clone fixture.
- Sparse clone: `native-clone-snapshot.json` copied only eight full (state 8)
  files, 64,088 bytes in total. Most of that is leftover probe output, including
  the 55,820-byte `probe-app.exe`. It recorded tombstones for `delete.txt` and
  `dir`, and referenced the unchanged symlink `inside-link` from the immutable
  base. `aliases` is empty, so alias reuse of renamed unchanged files was not
  exercised. Fresh root `d` compiled and ran `main.c`. Only one clone was made;
  sharing one snapshot across several clones was NOT verified. Metadata
  preservation is INCONCLUSIVE. The fixture base is about 8 KiB, so this run cannot
  show that a sparse snapshot is smaller than a full copy at repository scale.
- Snapshot immutability: snapshot files were made read-only with `chmod 0o444`,
  which on Windows only sets the read-only attribute. The parent directory is
  owned by and fully controlled by the user, who could clear that attribute. The
  refused write in `clone-verify` proves the attribute was set; it does not prove
  an access-controlled immutable snapshot. Base-tree protection is separate and
  stronger: the shared base grants the user read/execute only, and the `base`
  phase was refused a write.
- Provider kill/restart: each kill happened after write handles were closed. The
  first kill came after `native-recovery.txt` and `.saving` were written and
  synced. On reopen both still existed, the deletion held, and the persisted
  instance identity matched (asserted on this first reopen only). The `.saving`
  rename happened before the second kill, so "temporary absent" after that reopen
  reflects the completed rename. This shows completed local state survives
  provider termination and reopen. It does not show recovery from a kill during
  a write, or atomic-save recovery.
- Child termination: there are 17 `WINDOWS_JOB` records in
  `ordinary-user.stderr.txt`, all `timedOut=0 activeAfterTermination=0`.
  `cleanup.json` lists 20 children, none timed out, all exit 0.
  The owned GUID root was removed with no error.
- Cache state and cost (`state-*.json`, `PRJ_FILE_STATE`: 1 placeholder,
  3 hydrated placeholder, 8 full, 16 tombstone). Each `state-initial-{a,b,c}`
  audit ran after that root's provider was started and then stopped. Every entry
  returned HRESULT `0x80070171` with 0 allocated bytes. The cause is unconfirmed,
  so these entries are not classified. Root `a` after mutation had 9 full files,
  2 tombstones and 135,256 allocated bytes. Peer `b` also holds full files:
  `concurrent.txt` (its own `B\n` written by the common probe) and `inside-link`.
  `inside-link` is also full in `c`. So peers are not placeholders and hydration
  only. These records show no leak of `a`'s content, but they do not prove
  isolation by state counts. Some queries still fail, and allocation counts use
  NTFS clusters, so storage cost stays INCONCLUSIVE.

**Verdicts.** The offline archive/source audit is VERIFIED, within the limit
above. The executed native phases are recorded as phase results with the scope
limits listed. NOT VERIFIED: directory rename (projected base placeholder,
ENOTSUP) and the listed common-probe checks. INCONCLUSIVE: storage cost,
metadata edits/preservation, multi-clone sharing, permission-enforced snapshot
immutability, mid-write kill recovery, ordinary-user scope, the projected
backend, product Gate 2 and GA. None of these are accepted.

**Inherited documentation.** The Windows status in inherited
`docs/workspace-verification.md` predates this run and is stale. This unit
does not own that file. For Windows ProjFS, this document and PR #9 supersede it.

Open items, deliberately left for later units: directory rename (including local
and full directories), Windows CRLF and fsync assumptions in the common probe,
leftover-file isolation between probe and native fixtures, ACL-enforced snapshot
immutability, and mid-write kill recovery. This unit changes neither the provider
nor the common probe.

## Rerun and audit

The workflow runs on pull requests that change the owned Windows executable files
or workflow. Its `workflow_dispatch` entry permits another bounded run on a selected
source ref. Setup is confined to a `windows-2022` GitHub-hosted job with an eight-minute
limit. Each external command has a 30- or 60-second bound. No native runtime Git
command or repository metadata is involved; checkout is a development action pinned
to the pull request's head SHA. The collected source hashes independently identify
the executable and unchanged probe bytes.

The script refuses local execution and exports a unique evidence directory before
setup. It keeps raw command output and a summary, then removes only its owned GUID
scratch directory after checking its sentinel and rejecting reparse entries.
Collection uses the elevated runner identity if present and records that fact.
Prerequisite command expiry requests Windows/.NET tree termination; a timeout fails
the collection cleanup verdict because descendant exit was not independently proved.
Runtime commands use a native Windows job object: create the child suspended, assign
it before resuming, enforce a time limit, terminate remaining children, and record
the active process count. The runtime audit must show that count reached zero.
[Windows job objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).

Runtime setup creates one uniquely named disposable non-admin account and grants
access only to owned fixture paths. The shared backing tree retains administrator
ownership and grants that user read/execute access. The user script records its
real SID and admin membership, starts providers, and executes ordinary operations
inside the Windows job wrapper. Setup removes its account after the wrapper exits.
Owned projected roots are removed only after every provider stop is confirmed;
cleanup never accepts an arbitrary caller location.

After retaining a completed archive and its provenance, audit it offline with:

```sh
rtk proxy bun tools/windows-workspace-proof/verify-evidence.mjs
```

The audit checks original archive digest, raw file hashes, source hashes, exact run
and artifact identity, job label, prerequisite command outcomes, identity,
scratch cleanup and explicit INCONCLUSIVE backend scopes. For the final run it also
asserts the recorded open failures, sparse clone contents, restart recovery and child exits. It never projects a root
or invokes platform setup. Its success means the bounded collection evidence is
consistent, not that Windows Layers work.
