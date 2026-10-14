# Windows sparse backend

Draft [PR #14](https://github.com/Dhruv2mars/javelin/pull/14) owns this bounded unit. Its base
is unmerged `feat/windows-projfs-proof` at exact `cc94e22b8a316b4308305218aab003c172c96e42`
(PR #9). The hub accepted only PR #9's bounded collection; its ProjFS backend stays
INCONCLUSIVE. This unit depends on that evidence and claims no acceptance from it. No product
commands, native database binding, autosave, Gate 2 or GA result is produced here.

## Bounded outcome

Done means one disposable GitHub-hosted Windows attempt with exact tested source, job,
artifact digest, raw records and cleanup, giving per-scope verdicts for directory rename root
cause, ordinary file operations, permission-enforced base and snapshot protection, two clones of
one immutable snapshot, mid-operation kills and private storage. There is no laptop
installation, privilege, reboot, mount or security change. A platform refusal would have ended
the attempt with that verdict.

## Root cause of the PR #9 rename failure

Microsoft documents that a directory created on disk as a ProjFS placeholder "never becomes a
full directory" ([cache state](https://learn.microsoft.com/en-us/windows/win32/projfs/cache-state),
updated 2025-03-13). Microsoft's VFS for Git states it "does not support rename or move of
partial directories", reported as `0x80070032` "The request is not supported"
([troubleshooting](https://github.com/microsoft/VFSForGit/blob/master/docs/troubleshooting.md)).
No Microsoft source we found offers a provider API that turns a placeholder directory into a
full one. Universal impossibility across every provider design is not confirmed.

The final hosted run reproduced this with PR #9's provider copied unchanged
(`projfs-provider.cpp`, blob `966ea5b`, identical to `cc94e22`). Every base directory rename
failed with Win32 error 50 (`ERROR_NOT_SUPPORTED`): untouched, enumerated and hydrated, and
dirty (a new child added). The provider received `PRE_RENAME` (32) for each rename that
succeeded and none for the refused ones, so the filter refuses before the provider is asked.
Renaming a file placeholder, a locally created (full) directory, and a local directory holding a
moved base file all succeeded.

## Architecture before implementation

| Shape | Decision |
| --- | --- |
| ProjFS virtualization root (PR #9) | Kept only as a rename control. Base directories cannot be renamed. |
| Cloud Files sync root per Layer, provider serving an immutable base plus snapshot | Chosen. Documented Win32 API, Server 2016 minimum, no stated admin requirement, rename callbacks for placeholders. |
| Container filesystem layers (wcifs/bindflt) | Rejected: no public third-party API, needs admin. |
| ReFS block-clone copies | Rejected: full namespace copy, excluded by the contract like clonefile. |

Two read-only reviews compared these shapes independently: Codex `gpt-6.1-sol` (high) and
Claude Opus 5.5 (medium). Both recommended Cloud Files with the ProjFS rename matrix as a
control. Neither executed code. They differed on population: Codex suggested eager
`ALWAYS_FULL` namespace creation; this unit uses on-demand `FULL` population so untouched
subtrees create no local entries.

Sources: [cloud sync engine](https://learn.microsoft.com/en-us/windows/win32/cfapi/build-a-cloud-file-sync-engine),
[CfRegisterSyncRoot](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/nf-cfapi-cfregistersyncroot),
[CF_CALLBACK_TYPE](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ne-cfapi-cf_callback_type),
[CF_OPERATION_PARAMETERS](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ns-cfapi-cf_operation_parameters).
The runner's actual SDK header (10.0.26100.0 `cfapi.h`) declarations are retained in
`sdk-cfapi-declarations.txt`. It does not define the `CF_SIZE_OF_OP_PARAM` helper, so the
provider computes the documented size directly.

## Design

One native executable, `sparse.exe`, guarded to GitHub-hosted runners:

```text
sparse serve <base> <snapshot|-> <root> <stop> <ready>
sparse capture <base> <root> <snapshot-out>   # quiescent: provider stopped
sparse audit <root>                           # placeholder state and allocation, no hydration
sparse move|hold|flush-directory|unregister|bounded ...
```

- `serve` registers the root once with hydration `FULL`, population `FULL` and in-sync tracking
  of all attributes, then connects. Only the root level is populated eagerly (metadata
  placeholders, plus real symlinks because Cloud Files has no symlink placeholder). Other
  directories are populated on first access. Each placeholder's identity is its view path.
- After population, local NTFS state is authoritative, so deletes and renames need no
  tombstones. Data callbacks read the immutable base or snapshot blob for that identity.
- `capture` walks the stopped root, never enumerating unpopulated directories. In-sync
  placeholders and dehydrated or byte-identical renamed placeholders become base references.
  Changed or new files become content-addressed blobs. The manifest records deletions, new
  directories, references, blobs and symlinks.
- An administrator copies the capture into an Administrators-owned container. The proof user
  gets read/execute only, and the read-only attribute is set. Two clone roots serve the same
  frozen snapshot.

Bounds: fixtures at most 128 entries and 256 KiB per file. Symlinks below the root level fail
closed (untested). Snapshot blobs do not preserve original timestamps.

## Attempts

Every attempt ran as a `pull_request` job on `windows-2022`. Original archives and cloud
run/job/artifact records are under `tools/windows-sparse-backend/evidence/<attempt>/`. The
planned bound was six attempts; it was extended to seven because every failure was a harness
defect, not a platform refusal.

| Attempt | Source | Run | Outcome |
| --- | --- | --- | --- |
| 1 | `bd00005` | `38505914226` | cmd did not start the compile batch |
| 2 | `6e849b5` | `38506216303` | Same; no batch line echoed |
| 3 | `cf8a8c1` | `38506397776` | Even `cmd /c echo` failed via a `System32/cmd.exe` path |
| 4 | `60fdf62` | `38506542313` | Backslash path fixed cmd; `CF_SIZE_OF_OP_PARAM` undefined |
| 5 | `e738c3f` | `38506681907` | Both compiled; recursive inheritance grants left files with empty DACLs |
| 6 | `ee23f17` | `38506890542` | All phases ran; traversal hydrated the slow file; renamed unchanged file copied |
| **7** | **`4588385`** | **`38507282143`** | **Final; all native phases VERIFIED** |

The final tested source is exactly `45883857262afb0f47520a8de8f862fece5bc1a0`. Artifact
`windows-sparse-backend-38507282143-1` has digest
`sha256:ae50ed33d004526d9deb26a573931b2810eb064406faf20ef15d762548827692`; its 96 raw files are
extracted in `evidence/7-final/raw/`. Owned files are checked out without line-ending conversion
(`.gitattributes`), so their runner hashes equal the committed bytes. The workflow and unchanged
common probe received Windows CRLF conversion; the verifier applies that transform explicitly.
Host: Windows Server 2022 Datacenter build 20348, image `20261004.326.1`. The Cloud Files
filter `cldflt` was present, running and automatic before setup, so no component was started.
Client-ProjFS was already enabled. Cost: seven jobs totalling 328 seconds of runner time
(final job `115667565200`, 75 seconds) on a public repository's hosted runner.

## Verdicts (final run)

All phases ran as a disposable non-administrator account inside a Windows job wrapper. Every
wrapped command ended with `activeAfterTermination=0`. Providers stopped, sync roots were
unregistered, the account was removed and the owned scratch tree was deleted.

**VERIFIED (bounded fixture, this runner):**

- ProjFS root cause: base directories refuse rename (error 50) before the provider is notified;
  local/full directories rename.
- Cloud Files directory rename: untouched (unpopulated), enumerated and dirty base directories,
  local directories and mixed cases all renamed, and their contents read correctly afterward.
- Native ordinary operations: text and binary read, in-place write, create, delete, atomic save
  by rename, directory and file rename, base symlink read, new relative symlink, traversal,
  external `cc` compile and run, file `fsync`, and peer isolation. Compiler output was CRLF
  (`39 0d 0a`); the native check normalises it explicitly. A Win32 directory flush succeeded.
- Permission-enforced protection: the base and the frozen snapshot are Administrators-owned
  with read/execute for the user. The user's `attrib -R`, `icacls /grant`, `takeown`, write,
  delete, rename, create and container rename were all refused, and the attribute stayed set.
- Two clones of one frozen snapshot each showed the captured tree exactly, private writes stayed
  isolated between clones and from the source, and the snapshot was unchanged at the end.
- Mid-hydration provider kill: a reader blocked on hydration failed when the provider was killed;
  the placeholder stayed dehydrated (state 57, 0 allocated bytes); after restart it hydrated to
  the base bytes.
- Mid-write kill vs closed handle: writers holding open handles and the provider were killed
  together. The in-place write left a torn file (new 13-byte prefix, original tail), the
  atomic-save target kept its previous content and the temporary held the partial bytes. A
  write completed and closed before the kill survived intact.
- Sparse private state: untouched peer placeholders kept 0 allocated bytes. The snapshot holds
  5 private blobs (the files actually changed), references for unchanged and renamed files, 4
  deletions and 3 new directories.

**NOT VERIFIED (unchanged common probe on its own fixture):** external compiler (expects
`9\n`, got CRLF), independent writer and writer interruption (Bun `fsync` on a directory
returns EPERM), final base/peer integrity (writer leftovers) and the path boundary (an absolute
symlink can read a sibling view). Create/atomic-save/rename/delete and traversal now pass,
unlike PR #9. These are probe portability gaps, not changed here.

**INCONCLUSIVE:** storage cost at scale (allocation is cluster-rounded, placeholder header
overhead not measured, clone verification hashed every file and hydrated it), metadata
preservation, symlinks below the root, antivirus/indexer hydration, power loss, concurrent
writers, separate provider-only and writer-only kills, production registration and upgrade,
native database binding, autosave, Windows product acceptance, Gate 2 and GA.

## Rerun and audit

The workflow runs on pull requests that change the owned executable files or workflow, or by
`workflow_dispatch`. It is limited to 12 minutes; each wrapped command has a 30 to 190 second
bound. The scripts refuse local execution. Checkout is developer setup; nothing under test
invokes or reads Git.

```sh
rtk proxy bun tools/windows-sparse-backend/verify-evidence.mjs
```

The audit checks all seven archive digests against GitHub's recorded digests, every final raw
file against the archive, tested source hashes, identity, cleanup and the recorded verdicts
above. A one-byte change to a raw file makes it exit 1. It never projects a root or runs setup.
