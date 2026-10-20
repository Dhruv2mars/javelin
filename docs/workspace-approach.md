# Layer workspace approach

Status: accepted on 2026-10-17. This supersedes the sparse-kernel-backend
direction (FSKit, OverlayFS, ProjFS) and amends gate 2 of `build-contract.md`.
Amended on 2026-10-18 by `git3-direction.md`: Javelin is online-first, one rolling
base replaces per-version bases, and blobs are intended to become plain files
that Layers clone from, pending the recorded benchmark.

## Decision

A Layer is a plain directory on the same volume as the repository store. It is
created by **cloning the materialized World base** with the kernel's
copy-on-write primitive. Javelin **captures changes without a daemon** whenever
it runs: on every `javelin` command, and on lifecycle events that agent tools
already emit. Checkpoints and Publish record **changesets**: the changed paths
relative to the parent, plus pointers to content-addressed blobs.

| Filesystem | Layer creation | Unchanged data duplicated |
| --- | --- | --- |
| APFS (macOS) | one `clonefile(2)` of the base directory | no |
| btrfs (Linux) | one subvolume snapshot of the base | no |
| XFS, bcachefs (Linux) | per-file `FICLONE` reflink | no |
| ReFS / Dev Drive (Windows) | per-file block clone through `CopyFile` | no |
| ext4, NTFS, others | per-file byte copy, reported as `copy` | yes |

No mount, FUSE, file provider, kernel extension, system extension, root helper,
`sudo` or background process is involved on any platform. Editors, compilers,
watchers, atomic-save renames and symlinks behave normally because a Layer is an
ordinary directory, with no projection layer in between.

## Why the earlier direction was wrong

Gate 2 required a sparse backend that projects shared content into normal
paths, and it banned clone controls. Every candidate that satisfies "sparse and
projected" needs something users do not want:

- **macOS:** FSKit needs a per-user extension enablement, and NFS loopback needs
  a server process. Neither passed the macOS proof track.
- **Windows:** ProjFS needs an elevated feature enablement and a running provider.
  The Windows proof track spent eight attempts on harness defects.
- **Linux:** OverlayFS needs a root-owned helper, `sudo -n` and an
  administrator-managed parent list.
- **Elsewhere:** EdenFS, CitC and VFS for Git all depend on a long-lived daemon
  or driver. VFS for Git was superseded by Scalar and sparse-checkout for new
  deployments, largely because Apple deprecated the kernel features its macOS
  port needed. EdenFS on macOS runs over NFS loopback.

The result was that roughly three quarters of the added lines were proof and
evidence code, the product shipped as Linux-only, and macOS and Windows had no
writable Layer. The goal behind the ban was "do not duplicate the repository per
Layer". Kernel clones meet that goal for file data without a projection layer.

## What we take from Scion

From Anthropic's [Scion write-up](https://www.anthropic.com/engineering/redesigning-source-control):

- **Changesets, not snapshots.** A checkpoint or Publish stores the changed paths
  relative to its parent, with content-addressed blob pointers. Cost scales with
  the size of the change, not the size of the repository.
- **Derived data.** Diffs, history, blame and bases are computed from changesets,
  eagerly or lazily, and never stored as primary truth.
  The rolling base advances in place by `apply(changeset V→V+1)`, so keeping it
  current costs only the change (`git3-direction.md`).
- **One compare-and-swap row for World advance.** No global refs and no global
  lock. Preparation stays concurrent.
- **An optional working copy.** Jobs that only read or query use the API without
  a Layer. A Layer can be sparse (`--paths`) to bound its file count.
- **Want-based, immutable, resumable transfer** for hosting. This is noted for the
  protocol work and does not affect Layers.
- **Saying no to over-engineering.** Scion's authors reported that adversarial
  review drifted into excessive validation. Javelin's proof tracks did the same.

We do not take Scion's per-node daemon: capture stays daemonless. Javelin was
local-first until 2026-10-18, when `git3-direction.md` adopted Scion's online
client: assume online for speed, never for correctness. Layers stay plain cloned
directories, and Layer work continues during an outage.

## Change capture without a daemon

Javelin keeps a per-Layer stat cache recording path, size, mtime, ctime, inode and
mode (`src/capture.rs`). Before any command reads Layer state, Javelin walks the
Layer and compares each entry with the cache, hashing only entries that differ.
Changed files become blobs, and the changeset becomes a checkpoint under a
generated operation ID, so a retry replays instead of adding history. If the cache
is missing or corrupt, Javelin hashes the whole Layer and rebuilds the cache.

The capture boundary is:

- **Stable read.** Each entry is hashed between two `lstat` calls, plus an `fstat`
  of the open file on Unix. If they differ, the read is retried. After three tries
  the path is reported uncertain, keeps its prior checkpointed state, and stays out
  of the cache, so the next scan hashes it again.
- **Timestamps.** The racy reference is the clock read once before the walk, minus
  2 seconds to cover coarse kernel clocks and FAT's 2-second timestamps. It is per
  scan, not the time the cache file was written.
- **Racy entries** ([Git's rule](https://git-scm.com/docs/racy-git)). An entry
  whose mtime or ctime is not older than the reference can change again without
  changing its fingerprint. It is cached as untrusted and re-hashed on every scan
  until a scan sees old timestamps. Writing a newer cache never makes an entry
  trusted; only a non-racy re-hash does. This replaces Git's index-size smudging.
- **Crash safety.** The cache is written to a temporary file, synced and renamed
  over the old one, and carries a checksum. A torn or damaged cache is discarded.
  Blobs are committed before the cache, and the cache before the checkpoint. A
  crash at any point loses nothing: the cache only says "this fingerprint had this
  content", which stays true.

Tests in `tests/capture.rs` cover same-size rewrites with a restored mtime,
replace-by-rename, deletes, case-only renames, concurrent identical captures,
concurrent rewrites (never a mixed version), and cache damage at every byte.

Limits: on Windows, directory enumeration can report stale size and mtime for a
file still open by its writer; the next scan after the writer closes sees the
change. A writer that changes bytes in place without changing size, within one
timestamp tick, during the read, can be captured torn; the next capture corrects
it. A network filesystem whose clock is more than 2 seconds ahead of this machine
can defeat the racy rule (unconfirmed; not tested).

Capture runs at these triggers:

- At the start of every `layer` and `workspace` command that names the Layer.
- On agent lifecycle events (turn end, tool-call end). The contract already says
  Javelin accepts these from agent tools; the hook is simply `javelin checkpoint`.
- A foreground watch command, for humans who want capture while they edit, is not
  built. If added, it is a command the user runs, not a service.

Because the Layer is the real directory, an edit is durable on disk the moment the
editor writes it. Capture only decides when that edit becomes a recoverable
checkpoint. OS change journals (FSEvents replay, the NTFS USN journal) can later
replace the walk as an accelerator. The measurements below show they are not
needed yet, so they are left out.

## Measurements

The bench is `tools/layer-bench`. It builds a deterministic, repository-shaped
tree with log-normal file sizes (median about 3 KB, long tail, about 70 MB per
10k files) and measures each strategy over 5 repetitions, reporting medians.
The steps are create, disk delta, first status scan, rescan after 10 edits, and
discard. The rescan must find exactly the 10 edits, or the run fails. All
numbers are fresh. No earlier benchmark data was used.

Hosted runners (GitHub Actions runs 39057565557 and 39058557182). Medians of 5.
The 100k-file tree holds 697 MB. Linux XFS and btrfs are 12 GB loop images; the
Windows Dev Drive is a ReFS VHDX created with `Format-Volume -DevDrive`.

| Filesystem | Strategy | Create | Disk | Scan | Rescan, 10 edits | Discard |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| APFS | directory clone | 1.6 s | 34 MB | 1.4 s | 1.2 s | 4.0 s |
| APFS | copy | 36.6 s | 979 MB | 2.2 s | 1.1 s | 4.8 s |
| btrfs | subvolume snapshot | 5.5 ms | 0 MB | 1.0 s | 0.37 s | 3.3 s |
| XFS | per-file reflink | 7.7 s | 74 MB | 0.29 s | 0.31 s | 3.3 s |
| XFS | copy | 4.3 s | 1,007 MB | 0.30 s | 0.31 s | 3.9 s |
| ext4 | copy (no clone support) | 3.3 s | 954 MB | 0.33 s | 0.34 s | 7.5 s |
| ReFS Dev Drive | per-file block clone | 37–93 s | n/a | 0.60 s | 0.33 s | 7.3 s |
| ReFS Dev Drive | copy | 11–19 s | 855 MB | 0.73 s | 0.30 s | 5.9 s |
| NTFS | copy (no clone support) | 13.6–16.5 s | 897 MB | 0.61 s | 0.25 s | 5.7 s |

At 10k files (71 MB) the same ordering holds. An APFS directory clone takes 74 ms
and a btrfs snapshot 3 ms. Per-file reflink on XFS takes 0.68 s, an ext4 copy
0.32 s, and an NTFS copy 1.5 s.

On this laptop (M1, APFS, macOS 26.6.2) a 100k-file directory clone took 1.2 s
and 37 MB; rescan after 10 edits took 0.38 s.

The "Rescan" column is a metadata baseline, not capture latency: layer-bench only
compares size, mtime and inode after single-byte appends. It does not hash, apply
the racy rule, or see same-size rewrites.

### Capture latency

`examples/capture_bench.rs` measures the real capture path on the same 100k-file,
697 MB tree: the stat cache, racy rule, stable reads, SHA-256 hashing, blob storage
and the checkpoint commit. Edits alternate single-byte appends and same-size
rewrites. This laptop, medians of 5:

| Step | Time |
| --- | ---: |
| Metadata-only walk (layer-bench's rescan, single thread) | 0.27 s |
| Scan after 10 edits (parallel stat walk, racy rule, hashes 10) | 0.18 s |
| Clean capture, no edits (scan plus repository reads) | 15 s |
| Capture after 10 edits, including the checkpoint commit | 51 s |
| Cold rebuild with no cache (hashes all 100k files) | 28 s |

The scan meets the 0.25–0.4 s budget; it lists directories on up to 8 threads.
The repository does not: each Layer read and `capture_layer` re-reads and
re-hashes every blob of every World and checkpoint, so a commit costs seconds at
this size and grows with each checkpoint. Incremental repository validation is the
next step (TASKS.md).

What the numbers say:

- **Directory clone and snapshot are the only near-free creates.** APFS and btrfs
  share all unchanged data. XFS reflink shares it too, but costs per file, a
  little more than copying.
- **The stat walk is fast enough without a daemon.** A metadata rescan of 100k
  files takes 0.25–0.4 s on Linux, Windows and this laptop, and finds exactly the
  10 edits every time. The hosted macOS runner is slower, at 1.2 s. The full
  capture scan, with hashing, takes 0.18 s here (see "Capture latency").
- **Windows block clone saves disk but was 3–5 times slower than copying** with the
  `reflink-copy` crate, which issues about eight calls per file. The product instead
  calls `CopyFileExW`, which block-clones by itself on ReFS and Dev Drive (Windows 11
  24H2, Server 2025). The contract rule wins over create speed: where copy-on-write
  exists, a Layer never duplicates unchanged data. Windows runner times varied about 2× between the
  two runs, so ranges are shown. ReFS free-space deltas were not usable (they were
  negative or below the size of the files written), so that cell is n/a.
- **A hybrid that copies small files and clones large ones is not worth it.**
  Copying files under 16 KB and cloning the rest saved only 35–50% of disk,
  because small files fill whole blocks, and its create time fell between the two
  pure strategies.
- **Unprivileged btrfs subvolume delete was refused.** Emptying the subvolume and
  calling `rmdir` worked without privileges.

FSEvents replay (macOS, this laptop, separate C probe): replaying 1, 10 and 1,000
edits from a stored event ID took 13, 13 and 33 ms in the 10k tree. In the 100k
tree it took 183 to 376 ms, because replay cost follows volume-wide activity, not
Layer size. That is no faster than the stat walk, which is why journals are left
out.

## Consequences and limits

- **Create cost scales with entries, not bytes.** APFS clones metadata per inode,
  about 12 µs and 370 bytes per file, so 100k files take about 1.2 s and 37 MB.
  The btrfs snapshot is constant time. Sparse Layers bound large monorepos.
- **ext4 and NTFS fall back to copying,** which costs O(bytes) and duplicates data.
  Javelin reports the strategy in `javelin workspace` output and recommends btrfs or
  XFS on Linux and a Dev Drive on Windows.
- **Windows creates are slow at 100k files either way,** 11–19 s for a copy. Sparse
  Layers are the main lever there. Creating a Dev Drive needs one-time
  administrator setup; using it does not. The OverlayFS helper was retired on
  2026-10-18, after the clone backend reached gate-2 parity. Fast ext4 support is
  not worth a root helper.
- **Discard is deferred.** Removing 100k files takes about 4 s. Discard renames
  the Layer into the store's trash, and later commands delete it.
- **Layers never see base edits.** A directory clone briefly blocks writers to its
  source, so Layer create takes the workspace lock, clones the base and unlocks.
  A clone does not depend on its source afterwards. Until 2026-10-18 bases were
  immutable and replaced, never edited in place. `git3-direction.md` replaced
  that rule with one rolling base advanced in place under the same lock.
- **Never hardlink.** A hardlink shares one inode across Layers, so an in-place
  write would leak between them. Clones are copy-on-write per file.
- **Isolation is per directory.** A process can still write into another Layer's
  path if it chooses to. That matches every surveyed tool and is not enforced.
- **Unconfirmed:**
  - Whether `CopyFile` clones files smaller than one cluster; the probe checks one
    256 KiB file, and the report counts no copied bytes once cloning is verified.
  - Spotlight and Time Machine behavior with many clones. Excluding the store from
    indexing is untested.
  - Whether a standard (non-admin) Windows user can call the block-clone FSCTL on
    a Dev Drive. Hosted runners are admin.
  - Whether `CopyFile` block clone approaches plain copy speed at 100k files.

## Implementation

`src/workspace.rs` and `src/workspace/clone.rs` implement this decision. The CLI
exposes it as `javelin workspace create|open|path|discard|publish|recover`. It is
the only Layer runtime.

**Store layout (current code).** Everything lives under `.javelin/`, on the
repository's volume: `bases/rolling` holds the repository's one sealed base,
`base.json` records the World it holds, `layers/<id>` holds each Layer, `tmp/` holds
staging, and `trash/` holds renamed-away directories. `workspace.lock` serializes workspace
changes between processes. `Repository::layer_workspace` returns a Layer's root and
base paths for capture.

**Strategy by trial.** Javelin tries each primitive on the real volume and never
reads the filesystem name. The order is a one-call tree clone (APFS directory
`clonefile`, btrfs subvolume snapshot), then a per-file clone (`FICLONE`, per-file
`clonefile`, `CopyFile` block clone), then a byte copy. On Windows, a 256 KiB probe
copy must share its first cluster (`FSCTL_GET_RETRIEVAL_POINTERS`) before Javelin
reports `file_clone`. Every result reports the strategy, the reason each faster
strategy failed, and the bytes copied. Javelin never hardlinks.

**Bases (current code).** One rolling base per repository, per `git3-direction.md`
§4. Layer create takes the lock, brings the base to the Layer's World, clones it,
applies the checkpoint and unlocks. Behind the Layer's World, the base advances in
place by the exact changeset between the two manifests, O(change). Ahead of it, the
Layer's own clone is rewound by the reverse changeset, and the base stays where it
is. The base is cold-written from stored objects when absent, into a btrfs
subvolume where possible, flushed, published by rename, then sealed. An advance writes
`advancing` to `base.json` first. A crash leaves that record set, and recovery trashes
the partial base, so the next create rebuilds it cold. Reapplying the changeset over
a partial base was rejected: a half-written path cannot be told apart from a
case-folding collision. Per-version bases from earlier builds are trashed by recovery.

**Sealing measured on this laptop** (M1, APFS, 100k files and 661–697 MB):

| Read-only base → writable Layer | Create | Note |
| --- | ---: | --- |
| directory `clonefile`, unsealed base | 1.3–2.0 s | Layer comes out writable |
| directory `clonefile` + `chmod` walk to unseal 0444/0555 modes | 5.6–6.3 s | the walk un-shares metadata |
| directory `clonefile` of a base with deny-write ACLs on every entry | 2.5–3.6 s | sealing walk 4.9–6.2 s; clone carries no ACLs without `CLONE_ACL` |
| `copyfile(COPYFILE_CLONE \| COPYFILE_RECURSIVE)` | 16–18 s | fails outright on a mode-sealed base |
| per-file `clonefile` walk | 15–17 s | |

The `clonefile(2)` man page discourages directory clones in favour of `copyfile(3)`,
but `copyfile` was 10 times slower here, so Javelin keeps the directory clone and
seals only the base root, with mode 000. A clone needs read access to its source
(mode 000 returns `EACCES`), so Javelin opens the root to 0700 under the lock,
clones it, and seals it again. The Layer root is then set to 0755, which is O(1)
per Layer. Recovery reseals any base a crash left open. Windows has no directory
mode to seal, so base protection there is by convention only.

**Discard and recovery.** Discard records the terminal event in the database
first, then renames the directory into `trash/`, so a crash between the two steps
is finished by the next command. Publish works the same way: under the lock, a
successful Publish renames the Layer's directory into `trash/`, then reclaims within
the same budget. Recovery treats a published Layer as terminal, so a crash between
the publish commit and the rename is finished by the next workspace command. A
retried Publish replays its receipt and finishes the reclaim too. Every workspace
command first moves abandoned staging, discarded Layers and published Layers into
trash. It then deletes at most 4,096 trash
entries, so a 100k-file discard costs each command a bounded amount of work.
`workspace recover` reclaims everything. A trash entry for a Layer that is not
discarded is never deleted. btrfs Layers are deleted by emptying them and calling
`rmdir`.

**Unrepresentable Worlds.** Base materialization creates every name fresh. If a
volume folds case (`Foo` and `foo`) or Unicode normalization (NFC and NFD `é`),
the second name already exists, and Javelin fails with `unsupported`: "this World
cannot be materialized here". On Windows, reserved device names, trailing dots or
spaces, and `<>:"|?*` are rejected before writing. Partial staging goes to trash.

**Durability.** Linux flushes the filesystem with one `syncfs` before each publishing
rename. macOS and Windows sync each file that Javelin writes. A Windows Layer created
by byte copy (`CopyFile`) is not flushed file by file.

**Product measurements, this laptop (APFS, 100k files, 661 MB):**

- Cold base materialization: 12.8–14.7 s, including a per-file `fsync`.
- Warm Layer clone: 1.15–1.21 s, with a disk delta of 10–66 MB (a noisy free-space
  reading) and 0 bytes copied.
- The exact sharing test reports APFS private size 0 for a cloned 8 MiB file.
- Deferred discard rename: instant. Reclaiming 100k files takes 5.9–6.1 s, spread over
  commands.
- **Not yet within 1–2 s end to end:** each `Repository` call re-validates full
  state, and `manifest_in` re-reads and hashes every referenced blob. At this size
  `layer_state` costs about 5 s, `read_checkpoint` 3 s and `history` 5 s, so
  `open_workspace` takes about 9.4 s, of which the clone is 1.2 s. Recording the
  Layer (`layer_base` plus `create_layer`) takes another 13 s. This is pre-existing
  repository behavior and is tracked in `TASKS.md`.

CI (`Workspace / <fs>` in `ci.yml`) runs the workspace tests on APFS, btrfs, XFS and
ext4 loop or runner volumes, and on ReFS Dev Drive and NTFS. Each job asserts its
expected strategy and records a 20k-file cold and warm measurement.

## Sources

- **Scion:** primary write-up, read in full.
- **APFS:** `clonefile(2)` man page; Apple DTS forum answers on directory clones.
- **Linux:** `ioctl_ficlone(2)`, btrfs subvolume docs, inotify and fanotify man pages.
- **Windows:** Microsoft Learn on block cloning, Dev Drive,
  `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, ProjFS, and the USN journal.
- **Other systems:** EdenFS overview, Scalar docs, pnpm store docs.
- **Partially read:** the CitC/Piper material (CACM 2016) and the Mononoke/Bonsai
  schema were seen only through excerpts. They are marked unconfirmed and were
  not relied on. `git3-direction.md` adopts Bonsai through Scion's description
  and lists what remains unconfirmed.
