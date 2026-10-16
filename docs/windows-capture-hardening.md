# Windows capture hardening

Draft [PR #19](https://github.com/Dhruv2mars/javelin/pull/19) targets unmerged
`test/windows-sparse-backend` at `8a77d4222a0324e1090dd1a03b09251fc76a67d4`.
PR #14 was accepted only as bounded feasibility at tested `8712866`, with docs
corrected at `8a77d42`. This unit does not independently accept that base, Gate 2,
or GA. The existing evidence directories remain immutable.

## Verification plan

First run the inherited `sparse.cpp` unchanged with separately named hosted
controls. Record actual placeholder states, read outcomes, capture manifests,
blob lengths, junction behavior, malformed manifest loading and marker edits.
Unexpected results remain unconfirmed until raw evidence explains them.
Then implement the smallest fixes and rerun the same cases as refusal checks,
alongside the prior ordinary operations, seven Cloud Files rename cases,
two-clone isolation and restart checks. The common POSIX probe retains its known
failures; its portability repair belongs to another unit.

No provider is installed or registered on the developer laptop. Only the existing
disposable GitHub-hosted Windows 2022 workflow may run these fixtures. New
fixture helpers operate on owned disposable paths. Raw command records include
arguments, identity, timeouts and results. Capture during active user editing is
unsupported, and this work must not claim an atomic live snapshot.

## Design comparison before implementation

Independent Codex `gpt-6.1-sol` high task `windows-capture-pr19-design-r1`
read the actual inherited source and compared two shapes. It ran no provider.

1. Bind mode and source digests into Cloud Files sync-root identity and compare
   that identity with protected provider configuration on every serve and capture.
   This adds a consistency check, but maintains the same binding twice.
2. Use one administrator-owned configuration record outside the writable Layer,
   binding the opened root's file identity, canonical path, mode and exact sources.
   Both serve and capture validate this record. This is the preferred bounded
   shape, subject to implementation and hosted proof. It needs no database,
   daemon or control plane.

An editable `.registered` marker cannot authorize capture or identify its source.
Owner checks alone are insufficient: the configuration's DACL and parent must
prevent replacement, writes, deletion, ACL changes and ownership changes by the
proof user. Validate opened objects rather than trusting pathname strings.

Microsoft exposes the root file ID and identity bytes in
[CF_SYNC_ROOT_STANDARD_INFO](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ns-cfapi-cf_sync_root_standard_info).
The registration identity is provider-supplied data, and
[CfRegisterSyncRoot](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/nf-cfapi-cfregistersyncroot)
allows callers with the documented root rights to register and update it.
Neither shape authenticates a hostile provider running as the same user.
Protected configuration and frozen artifacts have a narrower claim.

The design also requires bounded traversal that pins directories and checks
reparse tags before descending, strict checked reads, validated manifests and
content digests before serving, and fresh staging with no final manifest after
refusal. The recorded fully dehydrated renamed file must remain a safe base
reference; uncertain partial content must refuse before reading.

Windows [file sharing rules](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
support refusing open writers and holding files stable during reads. Directory
handles alone do not freeze a namespace. The hosted fixture must separately
establish writer/provider termination before supported capture.

## Current status

Run `38570489714` at `bd42718` stopped at helper compilation, exit 2 with SDK
`CompletionStatus` type errors because `NTSTATUS` was undeclared. No ordinary
proof account was created; cleanup completed. Its successful collection job
does not establish runtime reproduction. The raw attempt is retained separately
under `hardening-evidence/1-control-bd42718`. A focused compile-only control
precedes further runtime assertions. No hardening result is verified.

The compile-only run `38695475455` at `809e71d` failed with the same missing
`NTSTATUS` type. Adding `ntstatus.h` supplied constants but did not declare the
type. The helper must also match the inherited provider's `bcrypt.h` include
before `cfapi.h`. This failed attempt is retained under
`hardening-evidence/2-compile-809e71d`; no proof account was created and cleanup
completed. Microsoft distinguishes the type from the status-code constants in
its [NTSTATUS documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/using-ntstatus-values).

Production registration, antivirus/indexer interaction, logoff,
shell behavior, provider-only termination, arbitrary concurrent editing, and
power-loss durability remain unconfirmed.

Compile-only run `38695682464` at `5683058` built all three native executables
with exit 0, no warnings and no timeouts. The summary records no blockers,
no proof account and complete cleanup. Archive digest is
`c4dedfddde6000deaffe2421d5958fc63c19befc83a0af4efcf12445f3a2f6aa`.
This establishes compiler compatibility only. The next control enables runtime
fixtures against the unchanged inherited capture source and records Win32 read
errors separately from the inherited stream-reader result.

Run `38696161679` at `80f03d0` reached the ordinary-user fixture. Its archive
digest matches `a9fc5caf705c54c4bf28a2860c27e0dedfeeb1bb0c31baf82f018809fbe7208d`.
The actual token was nonadministrator SID
`S-1-5-21-1000861503-3182281758-1041194900-1003`. Cleanup completed; the base and
frozen snapshot remained unchanged. The previous rename, ordinary operations,
clone and restart phases returned zero. The common probe returned its known
failure code 1.

Both partial fixture creations returned `8007018b`, with zero entries processed.
Rename and read then failed because the file did not exist. Their successful
captures recorded no private blobs. These cases are NOT ESTABLISHED, and prove
neither zero-read nor short-read capture. No hardening implementation may be
accepted on that evidence. The next fixture records the runner's system error
message and stops each case when a required setup step fails. Missing
preconditions must fail the collection workflow after cleanup.

Other bounded observations in this same unchanged-source run are separate:

- The walker listed the owned outside sentinel through a directory junction.
  Capture accepted its 32 bytes as a private blob with the sentinel's digest;
  the outside fixture stayed unchanged. The junction was explicitly removed
  without traversal before recursive cleanup.
- A stopped snapshot-served root accepted capture after its editable marker was
  changed to `snapshot=-`. The manifest contained base references to names absent
  from the base, including `created.txt` and `completed.txt`.
- Providers started from manifests containing a blob name outside `blobs`, a
  digest/content mismatch, duplicate records, file-prefix conflict, and an
  outside relative link. These observations establish loading acceptance only;
  this fixture did not read the malformed views through ordinary filesystem paths.

The raw attempt remains immutable under `hardening-evidence/4-runtime-80f03d0`.

Independent Codex high review `windows-pr19-sdk-preconditions-r1` confirmed the
missing setup and compared helper creation to the inherited successful path.
The next fixture creates while registered but disconnected, supplies the same
timestamps and in-sync flag, then reconnects for rename/hydration. These are
fixture corrections, not a demonstrated cause of the earlier HRESULT. Opened
handle metadata must establish identity and logical length before proceeding.
The runner's system error message is retained on API failure. A case succeeds
as a reproduction only when the existing moved partial file has the intended
failed zero/short read and capture accepts that prefix; command success alone
cannot establish it.

## Implemented hardening, hosted verification pending

The retained failed run `38716055510` at `0d63827` establishes the zero-read
defect. The real renamed partial file had logical length 4096. Win32 read opened
it and failed with error 362, while the inherited stream reader opened it,
returned zero bytes with no stream error, and capture accepted an empty private
blob. The 16384-byte case had 12288 bytes resident, but its read also returned
zero bytes before failure. A returned nonzero short prefix is not confirmed.

Capture now refuses uncertain partial files before content reads. Fully
dehydrated renamed files may still reference the base when identity, logical
length, on-disk bytes and modified bytes match the safe case. Materialized reads
use a pinned handle, deny concurrent write/delete opens, check Win32 read status
and exact length, and enforce the 256 KiB file bound.

The shared walker pins directory and entry handles, rejects junctions and unknown
reparses before descending, and caps entries at 128, depth at 16 components,
relative paths at 512 characters and elapsed traversal at 10 seconds. Symlink
records must name declared files within the root; capture never reads their
targets. Manifests reject malformed or ambiguous records and Windows path
aliases. Blob names are lowercase SHA-256 digests. Content is verified before
exposure and again immediately before hydration.

An administrator-owned per-root configuration under the frozen fixture binds
root volume/file identity, root path, base and snapshot mode. Both serve and
capture validate it; the editable marker is diagnostic only. Supported capture
requires administrator-protected root and local objects with no proof-user write
or ACL rights, plus a free provider/capture lease. The fixture applies protection
without enumerating unpopulated cloud directories or following reparses, then
restores writable access. An existing writer blocks this transition. This is a
bounded offline capture requiring trusted preparation, not production live
snapshot support or authentication against an arbitrary same-user provider.

All reads and validation finish before staging begins. A fresh pending directory
is validated before rename to the selected output; refusal cannot replace an
existing output. Process death may leave an unselected `.pending` directory,
which is refused on retry until explicitly removed. Power-loss durability is
not claimed.

The focused Windows checks include partial-file refusal, junctions, relative
link escape, marker edits, actual token SID self-grants, active provider/writer
refusal and retry, limits, malformed manifests, missing/tampered/truncated blobs,
and mutation before hydration. Existing ordinary operations, seven Cloud Files
rename cases, clone isolation and restart checks still run. Verify a downloaded
artifact against current source with `bun tools/windows-sparse-backend/verify-hardening.mjs <attempt-directory>`.
