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

Production registration, antivirus/indexer interaction, logoff,
shell behavior, provider-only termination, arbitrary concurrent editing, and
power-loss durability remain unconfirmed.
