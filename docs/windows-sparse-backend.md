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
closed (untested). Snapshot blobs do not preserve original timestamps. `capture` resolves
identities against the base only, so it refuses a root served from a snapshot; capturing a clone
(a snapshot of a snapshot) is not supported. A case-only rename is not distinguished. Quiescence
during capture is the caller's responsibility (the provider is stopped first), not enforced.
The administrator owns the scratch root's children; the proof user may add entries there but has
no delete-child right, so it cannot rename or replace the frozen container.

## Attempts

Every attempt ran as a `pull_request` job on `windows-2022`. Original archives and cloud
run/job/artifact records were retained and are now only in Git history. The
planned bound was six attempts; it was extended to eight because every failure was a harness
defect, not a platform refusal. Attempt 8 applies the independent review findings below.

| Attempt | Source | Run | Outcome |
| --- | --- | --- | --- |
| 1 | `bd00005` | `38505914226` | cmd did not start the compile batch |
| 2 | `6e849b5` | `38506216303` | Same; no batch line echoed |
| 3 | `cf8a8c1` | `38506397776` | Even `cmd /c echo` failed via a `System32/cmd.exe` path |
| 4 | `60fdf62` | `38506542313` | Backslash path fixed cmd; `CF_SIZE_OF_OP_PARAM` undefined |
| 5 | `e738c3f` | `38506681907` | Workflow concluded `failure`: both compiled; recursive inheritance grants left files with empty DACLs |
| 6 | `ee23f17` | `38506890542` | All phases ran; traversal hydrated the slow file; renamed unchanged file copied |
| 7 | `4588385` | `38507282143` | All native phases passed; reviewed as `fdd0f4b` |
| **8** | **`8712866`** | **`38508454070`** | **Final; review fixes; all native phases VERIFIED** |

The final tested source is exactly `8712866f0272f6cb12907fc0352002163fa61a1d`, job
`115671233319`. Artifact `windows-sparse-backend-38508454070-1` (id `11869270547`, expires
2026-10-28) has digest
`sha256:c7b9b68d9f677946edf068f5ba0a4d08b1b271ec58e7cd4196533e411f95d999`; its 96 raw files were
extracted into Git history. Owned files are checked out without line-ending conversion
(`.gitattributes`), so their runner hashes equal the committed bytes. The workflow and unchanged
common probe received Windows CRLF conversion; the verifier applies that transform explicitly.
Host: Windows Server 2022 Datacenter build 20348, image `20261004.326.1`. The Cloud Files
filter `cldflt` was present, running and automatic before setup, so no component was started.
Client-ProjFS was already enabled. Cost: eight jobs totalling about 408 seconds of runner time
(final job 80 seconds) on a public repository's hosted runner. The Unix tools on the proof user's
PATH (`cat`, `cp`, `mv` and others used by the common probe) are MSYS binaries shipped in the
runner's Git for Windows directory; Git itself is never run.

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
  (set by `/setowner`; no owner readback) with read/execute for the user. The user's
  `attrib -R`, `icacls /grant runneradmin:F`, `takeown`, write, delete, rename, create,
  container rename and rename of the whole frozen root were all refused, and the attribute
  stayed set. The grant probe named the administrator, not the proof user
  `jvs127c2b16d4c6`: `userInfo().username` in `checks.mjs` returned the parent's inherited
  `USERNAME` under `Start-Process -Credential`, so the attempt 8 "target the proof user" fix
  did not take effect (raw argv in `native-protect-base.json` and
  `native-protect-snapshot.json`). The refusal still shows the user lacks WRITE_DAC, which any
  grant needs; a self-grant was not exercised. Known limitation, not rerun.
- Two clones of one frozen snapshot each showed the captured tree exactly, private writes stayed
  isolated between clones and from the source, and the snapshot was unchanged at the end. Clone
  provider logs show changed files served from `frozen\snap\blobs\<sha256>` and unchanged
  files from the frozen base.
- Mid-hydration provider kill: a reader blocked on hydration failed when the provider was killed;
  the placeholder stayed dehydrated (state 57, 0 allocated bytes); after restart it hydrated to
  the base bytes.
- Mid-write kill vs closed handle: two writers, confirmed alive with open write handles, were
  killed in turn and then the provider was killed. The file had hydrated before the kill. The
  in-place write left a torn file (new 13-byte prefix, original tail). The atomic-save temporary
  held its partial bytes; its target was untouched, trivially, because the rename never ran. A
  write completed and closed before the kill survived intact.
- Sparse private state: untouched peer placeholders kept 0 allocated bytes. The snapshot holds
  5 private blobs (the files actually changed) and base references for unchanged files,
  including a renamed hydrated file, a renamed never-read file and a renamed never-opened
  directory subtree (both dehydrated at capture), plus 6 deletions and 5 new directories.

**Unchanged common probe on its own fixture:** VERIFIED text/binary read, private in-place
writes, create/atomic-save/rename/delete, directory traversal, relative symlink and watch
notifications. Create/atomic-save/rename/delete and traversal failed under ProjFS in PR #9. NOT VERIFIED: external compiler
(expects `9\n`, got CRLF), independent writer and writer interruption (Bun `fsync` on a
directory returns EPERM), final base/peer integrity (writer leftovers) and the path boundary
(an absolute symlink can read a sibling view). INCONCLUSIVE by design: caller-path clone
isolation. These are probe portability gaps, not changed here.

**INCONCLUSIVE:** storage cost at scale (allocation is cluster-rounded, placeholder header
overhead not measured, clone verification hashed every file and hydrated it), metadata
preservation, symlinks below the root, antivirus/indexer hydration, power loss, concurrent
writers, a provider-only kill during a write, simultaneous kills, capture of clones,
production registration and upgrade,
native database binding, autosave, Windows product acceptance, Gate 2 and GA.

## Independent review

Reviewers were read-only and executed only the offline verifier. Codex `gpt-6.1-sol` (high)
failed at its provider usage limit before reviewing, so the review round used Claude Opus 5.5
(high), the same model family as the implementer; diversity is limited. It found no blocking
evidence defects and confirmed the ProjFS, rename, identity, kill, snapshot, peer, cleanup and
source claims. Its findings were applied in attempt 8: capture refuses snapshot-served roots,
the frozen root can no longer be renamed by the user, the kill checks and verifier now assert
the reader failure and post-kill state, the partial-directory and dehydrated-rename capture
paths are exercised with a size check, providers log source paths, writer liveness is
recorded, capture checks its writes and the verifier pins the final digest, run, phases and
sources. The ACL-probe change intended to target the proof user did not take effect (see the
protection verdict above). Documented rather than changed: case-only renames, unvalidated
manifest paths (protected by ACL only) and unenforced capture quiescence.

The hub's independent audit of tested `8712866` / head `2565598` matched the re-downloaded
artifact digest and all 96 raw files and confirmed every VERIFIED claim. It recorded these
further known limits of the proof code, left unfixed in this PR:

- `capture` writes whatever `bytes()` returned with no PARTIAL guard, so a renamed partial
  placeholder that is neither in-sync nor size-matched could be captured truncated.
- `capture` and `audit` walk directory junctions as plain directories, with no entry or size
  bound on that walk.
- Manifest blob names and link targets are trusted, and blob bytes are never checked against
  their digest name.
- The `.registered` marker that makes `capture` refuse snapshot-served roots is owner-editable.

## Rerun and audit

The workflow runs on pull requests that change the owned executable files or workflow, or by
`workflow_dispatch`. It is limited to 12 minutes; each wrapped command has a 30 to 190 second
bound. The scripts refuse local execution. Checkout is developer setup; nothing under test
invokes or reads Git.

The offline evidence audit was removed with the raw evidence; it remains in Git history.
