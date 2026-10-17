# macOS mounted backend suitability

This unit retains measurements of historical backend `39a065b`, originally inherited
through docs-only `61aae6f`. PR #20 now targets main after PR #11 merged. It is ready
as verification tooling only. Main includes later permission and whiteout repairs;
the old measurements do not establish their behavior or accept a product gate.

## Fixture design comparison

1. A Bun supervisor launches the inherited CLI and isolated Python ordinary-path
   workers. A small C helper calls FSEvents and kqueue directly. Python calls clonefile and
   handles held descriptors and bounded raw RPC calls under a disposable account.
   Each worker has a process group, deadline and raw output. This reuses the exact
   locked backend and needs no additional Rust dependency graph.
2. A separate Rust consumer invokes the public overlay crate and NFS listener,
   with Bun orchestration. It could expose counters directly but introduces a new
   server executable and an additional lockfile. Its instrumentation could differ
   from the inherited CLI and conceal lifecycle defects.

Choose approach 1. Use the inherited server executable unchanged. Measure upper
logical and allocated bytes and operation elapsed times.
Observed allocated bytes do not by themselves measure transient copied bytes.
A bounded raw RPC observer records READDIRPLUS cookies separately from ordinary
mounted traversal. Neither replaces the other.

The hosted job has a 20-minute limit. Build and probes run within a 14-minute
supervisor budget. Commands have individual deadlines and process-group TERM/KILL
termination. Cleanup runs before sealing evidence and is retried by a workflow trap.
No mount, account or host security operation runs on the developer laptop.

Fixtures start with 32 then 512 and 2,048 tiny C source files. The configured ceiling
is 50,000 entries and 1 GiB file contents per active fixture. The 1 GiB truncate
fixture uses a separate mount after the small cases. Controls and staged copies
consume additional scratch space; the 1 GiB bound describes fixture payload. Larger tiers stop when time or byte budgets expire and remain INCONCLUSIVE.
Ordinary APFS and direct clonefile controls are controls only. They are not Layer proof.

## Historical observed suitability

For measured backend `39a065b`, native binding should wait for a hardened or
replacement NFS layer. The hosted
collector succeeds when it preserves valid evidence, including backend failures.
It does not accept the backend, macOS product support, Gate 2 or GA.

[Run 38716401553](https://github.com/Dhruv2mars/javelin/actions/runs/38716401553)
executed source `bc0bb7a` against unchanged backend `39a065b` on macOS 26.6.2
build 25G83, arm64, image `20260907.0351.1`. The job completed in under four minutes.
Its raw phase results show:

- Deleting `CaseName.txt` hid that spelling, but `casename.txt` still returned its
  base contents. Separately, deleting NFC `café.txt` left NFD `cafe\u0301.txt`
  readable. APFS controls established same-inode equivalence for each pair.
  Both bypasses persisted after remount.
- SIGKILL and restart on the same port changed raw handles. The unread base-file
  offset, post-restart write, held directory and new opens returned errno 70,
  stale NFS file handle, before any unmount. A cached read still returned bytes.
  The completed fsynced write survived eventual remount. Generation and handle
  IDs are not persisted, so remount recovery does not establish same-mount recovery.
- A 640-entry directory produced successive READDIRPLUS pages of seven entries.
  Deleting the resume entry returned NFS status 10003, BAD_COOKIE. Renaming it
  returned status 0. Concurrent ordinary `find -delete` completed with no remaining
  entries in this attempt; that success does not erase the raw cookie failure.
- An ordinary disposable process at UID 1500 received EACCES reading the owner's
  mode-0600 sentinel directly. Its credentialed raw NFS READ returned the secret.
  Loopback binding therefore does not isolate the Layer owner from local users.
- The producer labelled the 32, 512 and 2,048-file tiers VERIFIED and recorded zero
  command errors. Those raw labels remain unchanged. The stronger verifier narrows
  all three tiers to INCONCLUSIVE because visible count and content inventories
  were not captured. These timings describe requested workloads, not independently
  established file counts. At the requested 2,048 tier, NFS read/syntax-check/delete took 12.75/44.14/24.26
  seconds, versus 0.21/16.34/0.09 on ordinary APFS and 0.23/21.52/0.24 on the
  direct clonefile control. One source also produced a real object through each
  measured path. These are one ordered fixture, not stable benchmark estimates.
- Truncating a populated 1 GiB base file through NFS took 1.92 seconds. The separate
  inherited stage-abort diagnostic retained a full 1 GiB copy with the source hash,
  confirming whole-file copy-up before truncation. Reads did not grow upper bytes
  in the small tiers. The 50,000-entry ceiling was not attempted and is INCONCLUSIVE.
- Actual FSEvents and kqueue APIs both delivered events for the bounded mounted
  mutations, with APFS positive controls. Broader application watching is unproven.

The raw cleanup records show no remaining owned mount, server/process group,
listening port, disposable account or temporary root.

Two fixture defects in that attempt require a new exact-source run. The artifact
upload excluded the archived `.github` workflow file, so the downloaded archive's
manifest check correctly fails. The hosted pre-upload audit passed before that file
was omitted. Preserve the archive unchanged rather than restoring the missing file.
Also, unavailable Python xattr bindings aborted the optional capability phase, which
is INCONCLUSIVE. The runner now uses the ordinary macOS `xattr` tool and uploads
hidden source files. The corrected run was inspected before tooling readiness.

## Completed corrected run and integration

[Run 38869992611](https://github.com/Dhruv2mars/javelin/actions/runs/38869992611)
tested tooling source `1a1f315` and the same historical backend `39a065b`. Its
[raw artifact](https://github.com/Dhruv2mars/javelin/actions/runs/38869992611/artifacts/12033082355)
contains the previously omitted workflow source. The strengthened offline verifier
confirms archive integrity and cleanup, the same alias/restart/cookie/access
failures, and suitability NOT VERIFIED. Raw records and producer verdicts have not
been rewritten. Only the derived cost-tier verdicts narrow to INCONCLUSIVE because
those old records lack visible-file inventories. This run also records hard-link
errno 76 and successful xattr read/write through `._capability` AppleDouble.

The old producer recorded read/syntax-check/delete times of 7.81/44.98/32.63 seconds
for its requested 2,048-file NFS tier, compared with 0.30/26.34/0.12 on ordinary APFS.
These are observations with unconfirmed fixture completeness, not scale acceptance.
The populated 1 GiB truncate took 1.61 seconds and the stage diagnostic retained
a source-hash-matching 1 GiB copy. No new benchmark or mounted run was performed
for the integration and verifier corrections.

The workflow now obtains `39a065b` through a separate pinned developer checkout.
Hash checks, source snapshots and the Cargo manifest all refer to that checkout,
not main's repaired backend. PR events run syntax and offline regression checks;
only an explicit workflow dispatch can reproduce the disposable hosted measurement.
For local offline self-tests, point `SUITABILITY_BACKEND_ROOT` to a developer checkout
of `39a065b`. To apply the stronger auditor to an old archive, pass its independently
obtained producer tool directory as the fourth argument after archive, source SHA
and run ID. This compares the archived old producer bytes while using the stronger
current verdict rules. It does not pretend that the current runner produced old data.

Pagination verification now requires successful delete and rename setup, successful
traversal and an empty successful final scan. Local-access verification requires
explicit permission denial, not unrelated NFS failures; any returned protected bytes
fail. Future cost checks require matching visible/base inventories, the deterministic
requested fixture, full read contents and complete compiler inputs. Every command
must retain both referenced logs even when a checksum inventory is regenerated.
Focused offline regressions cover these cases. No backend repair is included here.

The unit checklist is in [the scoped TASKS file](../tools/macos-backend-validation/TASKS.md).
Root `TASKS.md` remains byte-identical to the integrated main base.

## Next smallest engineering step

The historical result recommends enforcing the serving owner's authorization at the NFS boundary, with an actual
second-UID denial regression. Then resolve case and Unicode identity, restart-safe
handles and deletion-stable directory cookies before native binding. Reconsider the
NFS approach if those guarantees cannot be provided simply. Whole-file copy-up and
serialized directory operations also need a cost budget. Do not fix the inherited
backend in this verification unit.
