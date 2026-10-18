# Blob storage benchmark

Should Javelin move blobs from SQLite rows to content-addressed files, allowing
bases to clone directly from the store?

## Go/no-go criterion (set before collecting results)

Blob files win if materializing 100,000 files is at least **5× faster on APFS or
btrfs**, physical disk for **store + base is at most approximately 1.2× corpus
logical bytes**, and ingest is **no more than 2× slower at matched durability**.
The comparison is A-tuned durable versus B durable. A “no” is valid. All three
conditions must hold; cheap Layer creation is context, not the comparison target.

## Results

Pending local APFS measurement. Linux: pending CI run.

## Method

The standalone [`tools/storage-bench`](../tools/storage-bench/README.md) has its
own lockfile and workspace and depends on the real Javelin crate. It changes no
product code. Tables and all individual samples are saved as JSON and Markdown.

Corpus generation makes the same RNG calls as `layer-bench::fixture`, with the
same seed (file count), paths, log-normal size distribution (median about 3 KB,
2 MB cap) and random bytes. Only its sink changes: the corpus stays in RAM, so
neither generation nor source-file reads are timed. Each size reports logical
bytes and a SHA-256 fingerprint over path, NUL, little-endian length and content.
There are no symlinks or executable files. Random data prevents useful deduplication.

Stores:

- **A-real** calls `Repository::init`, `put_blob` and `read_blob`. Production
  transactions are per-blob IMMEDIATE, DELETE journal, EXTRA, fullfsync ON, with
  read-back hashing. It has stronger Apple durability and integrity checks than
  the other paths, and is context rather than the decision baseline. Ingest has
  five reps at 1k/10k. At 100k it gets one attempt only if the 10k projection fits
  15 minutes; otherwise ingest is explicitly extrapolated. Read/materialization
  context then uses an untimed batched INSERT into a real initialized repository,
  retaining its production schema and DELETE journal. The timed reads use actual
  `Repository::read_blob` and verify canonical hashes. The 100k footprint of this
  setup is not claimed to come from per-blob production ingest.
- **A-tuned** retains `objects(id TEXT PRIMARY KEY CHECK(length(id)=64), kind
  INTEGER NOT NULL CHECK(kind IN (1,2,3)), bytes BLOB NOT NULL) STRICT`, plus
  immutable UPDATE/DELETE triggers. It uses WAL, batches of 1,000, cached prepared
  statements, 64 MiB cache, 2 GiB mmap limit and auto_vacuum=INCREMENTAL. A five-rep
  pilot chooses 4 KiB or 16 KiB pages and SELECT or incremental blob reads. Only
  the DELETE trigger is temporarily dropped, inside the eviction transaction;
  it is recreated before commit. This is a benchmark-only eviction experiment.
- **B** uses real `ObjectId::for_blob` (SHA-256 over `javelin-object-v1\0`, byte
  `1`, content), `objects/ab/cdef…`, a `tmp/` directory, 256 precreated fanouts,
  and read-only `0444` objects. Each single-writer insert hashes, checks for an
  existing path, writes a new temp, chmods it and renames it into place.

Matched durability policies:

| Policy | A-tuned | B |
| --- | --- | --- |
| durable (criterion) | WAL synchronous=FULL, fullfsync=OFF; commit/1,000, final WAL checkpoint included | fsync each temp before rename; fsync each touched fanout once/1,000 plus tmp directory; initialize and fsync store topology |
| none | WAL synchronous=OFF, fullfsync=OFF | no file or directory fsync |

On macOS both matched durable cases use ordinary **fsync, not F_FULLFSYNC**.
Neither claims power-loss persistence through the device's volatile write cache.
A-real requests F_FULLFSYNC and is deliberately not relabeled as symmetric.
SQLite NORMAL is not used for the decision: FULL and OFF provide the explicit
matched policies. Ingest includes fresh-store setup, hashing and final checkpoint,
with MB/s and files/s derived per sample.

### Machines and locations

Local: Apple M1, 16 GiB RAM, macOS 26.6.2 (25G83), rustc 1.93.0, internal SSD
APFS. A dedicated 30 GiB sparse APFS image (about 32.0 GB usable volume) backed by that SSD holds all disk,
materialization, read and eviction measurements. All stores' measured ingest runs
on native internal-SSD APFS (`--ingest-dir`); closed, checkpointed stores are
then seeded on the image outside timings. SQLite files are copied only after
checkpoint and close, preserving logical page layout. B is inserted through its
actual temp/write/fsync/rename path in corpus order and with the same policy,
avoiding the inode/extent-order bias of copying a CAS in directory-enumeration
order. The file store remains 0444.
Thus each comparison uses the same location for both designs. This separation
keeps the real API's stronger per-blob fullfsync timings on the native SSD while
isolating allocation and allowing mounted-volume cache resets. A preliminary
run using Rust File::sync_all for B was discarded: on Apple it requests
F_FULLFSYNC, which would mismatch SQLite's fullfsync=OFF. B now calls
libc::fsync explicitly, including for directories; no preliminary numbers are
used in the results. See [Rust's platform implementation](https://raw.githubusercontent.com/rust-lang/rust/1.93.0/library/std/src/sys/fs/unix.rs).
The resulting mixed-location matrix must not be read as an end-to-end native
SSD latency claim. The exact commands and machine probes accompany the results.

### Cache, timing, verification and allocation

Every warm case explicitly reads the whole store first. Passwordless `sudo -n
purge` failed (`sudo: a password is required`). Cold cases close all store handles
and detach/re-attach the image before each materialization or 1,000-read sequence.
This discards the mounted volume's cache; the host image-backing cache and drive
cache can remain warm. Cold random-read rows are **cold-start sequences**, not
independently cold reads: the sequence warms as it proceeds. Linux CI drops
caches using the explicit sudo hook, outside timings.

Every timed operation has at least five reps, except the budgeted A-real 100k
case. Tables report median and nearest-rank p95; with five samples p95 is the
maximum. Read p50/p99 pool 1,000 random full-Vec reads per rep (5,000 total).
The deterministic random sequence and individual latency samples are retained.

Full materialization includes directories, destination file creation and writable
permissions. A reads then writes; B-copy explicitly loops over read/write with a
64 KiB buffer, never `fs::copy`, `io::copy` or `copy_file_range`. B-clone uses
clonefile on APFS or FICLONE on Linux and chmods to 0644. Unsupported clones are
reported and never replaced by copies in that timing case. Timing excludes sync,
store-handle opening, verification, cleanup and cache setup. Consequently this is materialization cost,
not a durable whole-tree commit time. A-tuned SELECT/B-copy do not rehash on the
hot path; A-real does. B-clone trusts immutable object contents. Verification is
outside timing: all files at 1k, 100 regularly spaced files at larger sizes, and
every random read are compared to expected bytes; checked materializations are
also hashed. Every incremental changed path is checked, including deletion.

Incremental cases prepare an untimed base and prestored new blobs, then restore
changed paths outside timing between repetitions: one
modified file, or 34 modified + 33 deleted + 33 added paths. Writes/clones use
sibling temps and rename; no destination fsync is timed. B reports both copy and
clone incremental paths. The changed entries are the first 100 fixture paths, a localized deterministic
path set. This tests applying a changeset, not generating one.

Physical allocation is the dedicated volume's available-space delta after sync:
statfs `f_bavail * f_bsize` on APFS, statvfs `f_bavail * f_frsize` on Linux. It
waits for consecutive equal readings 200 ms apart, failing after 20 s. Disk states
(store, +base, +1 Layer, +5 Layers) have five independent allocations, with
median/p95 reported. Each repetition rebuilds the store and working trees; SQLite
uses a closed seed outside the measured directory and B uses its actual insertion
path. The seed allocation is constant and excluded by the before/after delta.
Every repeated materialization additionally records its base allocation delta. SQLite is
checkpointed, and database/WAL/SHM apparent sizes are reported. Apparent regular-file lengths (excluding directory lengths)
count clone contents repeatedly, just as a logical `du` view does; ordinary block
`du` can also double-count shared extents, and is not exclusive physical usage.

A Layer is an APFS directory clone or a Linux per-file reflink of the base; ext4
uses an explicitly labeled byte copy. Context base-to-Layer timings are separate
from store-to-base comparisons. APFS sharing must allocate less than half the
explicit-copy control for the same corpus; Linux samples of files at least 4 KiB require FIEMAP shared
extents; this check does not certify tiny/inline-file metadata sharing. A sharing failure terminates the run. Every original remains live for
the check. On this APFS volume, small B objects still consume 4 KiB minimum allocations,
plus directory and inode metadata. Linux layout and rounding remain unmeasured. Reflinks share those blocks but cannot remove that rounding.

Eviction orders blobs by deterministic last-touch index (LCG seed 42, 2× file-count
touches, ties by original index), records victim indices and removes the least
recent 10%. Five reps measure DELETE in one transaction plus checkpoint, or unlink
plus policy directory fsync, and actual freed bytes after sync. SQLite additionally
drains incremental_vacuum's freelist and checkpoints, reporting time and further
reclamation. It cannot compact partially occupied pages like a full VACUUM. Full
VACUUM is unmeasured. Repeat setup restores victims without fsync, outside timing;
the next sync flushes this restored state. Live-base reps recreate a current base
before eviction, so reinserted B objects share with that base in every rep. Layers
are absent in this case; their additional retention effect is unmeasured.

## Crash safety

The small SIGKILL check stores 100 blobs, pauses with a partial next blob in
`tmp/`, kills the child, hashes every final object and checks that exactly 100
final paths exist. The partial tmp is detectable and swept. This exercises one
known pre-rename process-crash boundary; it is not randomized crash injection,
post-rename coverage or a power-loss proof.

SQLite WAL makes a batch transaction atomic: recovery ignores incomplete WAL
commits. OFF weakens crash guarantees. B publishes only complete temp files by
rename, but an interrupted batch may publish a subset of its objects; referencing
metadata needs its own ordered durable commit. Ordinary fsync and fullfsync have
different device-flush guarantees on Apple platforms. Future file-store reads
must verify canonical object hashes to detect torn/corrupt bytes; existence alone
is not integrity. Temp sweeps need a single-writer/locking rule so they do not
remove an active writer's temp. Atomic rename does not replace fanout-directory
fsync or a durable reachability/GC protocol. None of those protocols is implemented
in production by this measurement PR.

## Linux measurements — pending CI run

The workflow runs btrfs, ext4 and XFS as three parallel ubuntu-24.04 jobs using
16 GiB sparse loop images, Rust 1.99.0, the full 1k/10k/100k matrix and five reps.
XFS is formatted with `-m reflink=1`, and xfs_info records proof. The summary
records kernel/OS, CPU, RAM, core count and formatter versions. Runs are
unprivileged after format/mount/chown; only cold-cache dropping uses sudo. JSON
and Markdown are uploaded even after a partial failure. There are no timing gates.

| FS | 100k A-tuned durable materialize warm/cold | B-clone warm/cold | B-copy warm/cold | durable ingest ratio B/A | store+base | +1/+5 Layers | eviction/read |
| --- | --- | --- | --- | --- | --- | --- | --- |
| btrfs | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run |
| ext4 | pending CI run | expected unsupported; pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run |
| XFS reflink=1 | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run |

Linux timing, shared-extent proof and physical accounting are unmeasured locally;
no Linux outcome is inferred from APFS. The CI budget estimate is 45–120 minutes per filesystem, with all three jobs
running in parallel. This is unconfirmed: estimates use the operation count
(hundreds of thousands of durable commits/fsyncs and millions of file creations)
and the APFS run, not Linux data. Slow hosted storage can exceed that range;
the 180-minute timeout keeps the full matrix, and ordinary step failures still
run artifact upload. Results are saved incrementally.

## Implications to revisit with the completed data

A rolling base pays for changed entries instead of a fresh full materialization.
Its one/100-entry updates are the relevant local baseline; full 100k rebuild speed
matters mainly for first checkout, cache misses and recovery. Directory clone
remains available for creating Layers from either storage backend. Applying a
changeset safely to a rolling base also requires versioning/publication ordering;
these timings cover file operations only.

Future directory-level tree objects could compare directory hashes, skip whole
unchanged subtrees and clone a cached materialized directory. A flat blob-file
store alone cannot clone a whole directory from a path-independent fanout. Tree
objects would need a materialized-tree cache or equivalent layout to support that
operation. They can reduce metadata traversal for either blob backend. Their
performance and disk footprint are unmeasured; this benchmark does not establish
that moving individual blobs is a prerequisite.

## Measurement limits

The local case order is fixed rather than randomized. The shared host, image
backing cache, background allocation and thermal state are not controlled. Warm
and cold-volume times can invert, and five samples give no narrow confidence
interval; retain the p95/max and raw samples when interpreting small differences.
Cold volume is not cold drive hardware. The native-ingest/image-operations split
also limits end-to-end conclusions. Physical footprints are isolated-volume allocation deltas and include
filesystem metadata. The original single-snapshot SQLite pass is superseded by
the independent-allocation supplement. The original B directory-copy setup is
excluded; the repeated file-store pass uses actual insertion-order setup. Content is random
and incompressible; other size distributions and compressible trees may change
rounding or filesystem-specific inline-storage behavior. Windows, networking,
concurrent writers, process RSS, file-descriptor pressure and object-store scale
beyond 100k are unmeasured. Hash verification is outside the tuned/copy hot paths;
including it would change latency, especially compared with zero-copy cloning.
