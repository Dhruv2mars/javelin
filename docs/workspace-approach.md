# Layer workspace approach

Status: accepted on 2026-10-17. This supersedes the sparse-kernel-backend
direction (FSKit, OverlayFS, ProjFS) and amends gate 2 of `build-contract.md`.

## Decision

A Layer is a plain directory on the same volume as the repository store. It is
created by **cloning an immutable, materialized World base** with the kernel's
copy-on-write primitive. Javelin **captures changes without a daemon** whenever
it runs: on every `javelin` command, and on lifecycle events that agent tools
already emit. Checkpoints and Publish record **changesets**: the changed paths
relative to the parent, plus pointers to content-addressed blobs.

| Filesystem | Layer creation | Unchanged data duplicated |
| --- | --- | --- |
| APFS (macOS) | one `clonefile(2)` of the base directory | no |
| btrfs (Linux) | one subvolume snapshot of the base | no |
| XFS, bcachefs (Linux) | per-file `FICLONE` reflink | no |
| ReFS / Dev Drive (Windows) | per-file block clone, or copy when create speed matters more | no (clone) |
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
  or driver. VFS for Git is in maintenance mode.

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
  `base(V+1) = clone(base(V)) + apply(changeset V→V+1)`, so keeping bases current
  costs one clone plus the change.
- **One compare-and-swap row for World advance.** No global refs and no global
  lock. Preparation stays concurrent.
- **An optional working copy.** Jobs that only read or query use the API without
  a Layer. A Layer can be sparse (`--paths`) to bound its file count.
- **Want-based, immutable, resumable transfer** for hosting. This is noted for the
  protocol work and does not affect Layers.
- **Saying no to over-engineering.** Scion's authors reported that adversarial
  review drifted into excessive validation. Javelin's proof tracks did the same.

We do not take Scion's per-node daemon or its always-online client. Javelin stays
local-first, with no daemon.

## Change capture without a daemon

Javelin keeps a per-Layer stat cache recording path, size, mtime, ctime, inode and
mode. Before any command reads Layer state, Javelin walks the Layer and compares
each entry with the cache, hashing only entries that differ. It uses Git's
racy-entry rule: an entry whose mtime is not older than the cache write is
re-hashed, so same-second edits are never missed. Changed files become blobs, and
the changeset becomes a checkpoint. If the cache is missing or corrupt, Javelin
hashes the whole Layer and rebuilds the cache.

Capture runs at these triggers:

- At the start of every `javelin` command in the Layer.
- On agent lifecycle events (turn end, tool-call end). The contract already says
  Javelin accepts these from agent tools; the hook is simply `javelin checkpoint`.
- In the optional foreground `javelin workspace watch`, for humans who want capture while
  they edit. It is a command the user runs, not a service.

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

What the numbers say:

- **Directory clone and snapshot are the only near-free creates.** APFS and btrfs
  share all unchanged data. XFS reflink shares it too, but costs per file, a
  little more than copying.
- **The stat walk is fast enough without a daemon.** Rescanning 100k files takes
  0.25–0.4 s on Linux, Windows and this laptop, and finds exactly the 10 edits
  every time. The hosted macOS runner is slower, at 1.2 s.
- **Windows block clone saves disk but is 3–5 times slower than copying.** The
  clone crate issues about eight calls per file; a leaner implementation might
  narrow the gap (unconfirmed). Windows runner times varied about 2× between the
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
  administrator setup; using it does not. The existing OverlayFS helper is
  retired once the clone backend reaches parity. Fast ext4 support is not worth a
  root helper.
- **Discard is deferred.** Removing 100k files takes about 4 s. Discard renames
  the Layer into the store's trash, and later commands delete it.
- **The base must be immutable.** A directory clone briefly blocks writers to its
  source, which is harmless because bases are read-only. Bases are created
  read-only and replaced, never edited in place.
- **Never hardlink.** A hardlink shares one inode across Layers, so an in-place
  write would leak between them. Clones are copy-on-write per file.
- **Isolation is per directory.** A process can still write into another Layer's
  path if it chooses to. That matches every surveyed tool and is not enforced.
- **Unconfirmed:**
  - Spotlight and Time Machine behavior with many clones. Excluding the store from
    indexing is untested.
  - Whether a standard (non-admin) Windows user can call the block-clone FSCTL on
    a Dev Drive. Hosted runners are admin.
  - Whether a dedicated Windows block-clone path can approach copy speed.

## Sources

- **Scion:** primary write-up, read in full.
- **APFS:** `clonefile(2)` man page; Apple DTS forum answers on directory clones.
- **Linux:** `ioctl_ficlone(2)`, btrfs subvolume docs, inotify and fanotify man pages.
- **Windows:** Microsoft Learn on block cloning, Dev Drive,
  `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, ProjFS, and the USN journal.
- **Other systems:** EdenFS overview, Scalar docs, pnpm store docs.
- **Partially read:** the CitC/Piper material (CACM 2016) and the Mononoke/Bonsai
  schema were seen only through excerpts. They are marked unconfirmed and were
  not relied on.
