# Blob storage benchmark

Should Javelin move blobs from SQLite rows to content-addressed files, allowing
bases to clone directly from the store?

## Go/no-go criteria

Blob files win if materializing 100,000 files is at least **5× faster on APFS or
btrfs**, physical disk for **store + base is at most approximately 1.2× corpus
logical bytes**, and ingest is **no more than 2× slower at matched durability**.
The comparison is A-tuned durable versus B durable. A “no” is valid. All three
conditions must hold; cheap Layer creation is context, not the comparison target.

Criterion 2, quoted verbatim from the decision brief for PR #45:

> Go if, on the 100k-file `tools/layer-bench` fixture, on APFS and on one Linux reflink filesystem (btrfs or XFS), materializing a cold base from blob files (1) copies 0 file bytes, (2) is no slower than materializing from SQLite, and (3) uses at most 1.1× the tree's bytes for blob store plus base. Also, (4) a 10-edit checkpoint commit is at most 10% slower than with SQLite, and (5) a writer killed mid-write leaves no visible partial blob. Any miss is a no-go on that platform, and blobs stay in SQLite there.

The two criteria receive separate verdicts. Criterion 2 requires an APFS go and a
go on at least one Linux reflink filesystem; ext4 is measured as a copy control.
Both disk bounds use the fixture's logical file bytes as the denominator, as in
the measurement brief's disk-accounting requirement. Block-rounded working-tree
size is additional context, not a replacement denominator.
For its store comparisons, the matched durable A-tuned baseline is reported
alongside A-real, whose extra Apple device flushing can otherwise obscure a
store regression. The checkpoint table separates sparse metadata from the
additional full-manifest proxy; neither includes shared capture work.

## APFS results (2026-10-18)

All timing cells below are median / p95, except read cells (p50 / p99).
Materialization and read cases use the isolated image; measured ingest uses native
APFS. Raw samples and full tables: [JSON](../tools/storage-bench/results/apfs-2026-10-18.json),
[Markdown](../tools/storage-bench/results/apfs-2026-10-18.md).

Machine: Apple M1, 16 GiB RAM, macOS 26.6.2 (25G83), `rustc 1.93.0 (254b59607 2026-01-19)`.
Materialization/allocation use APFS in a dedicated 30 GiB sparse image backed by
the internal SSD; ingest uses native internal-SSD APFS. Exact commands, wall
times and selected source cohorts are in [run provenance](../tools/storage-bench/results/README.md).

| Files | Logical corpus bytes | Decimal MB |
| --- | --- | --- |
| 1000 | 7001918 | 7.002 |
| 10000 | 70521647 | 70.522 |
| 100000 | 697364836 | 697.365 |

### Full materialization

Seconds, durable-store cases. Destination files are not fsynced in any case.

| Files | Path | Warm s | Cold volume s |
| --- | --- | --- | --- |
| 1,000 | A-real | 0.466 / 0.582 | 0.601 / 1.938 |
| 1,000 | A-tuned | 0.505 / 1.016 | 0.177 / 0.609 |
| 1,000 | B-clone | 0.119 / 0.122 | 0.150 / 0.151 |
| 1,000 | B-copy | 0.123 / 0.130 | 0.204 / 0.212 |
| 10,000 | A-real | 2.158 / 2.516 | 1.864 / 2.449 |
| 10,000 | A-tuned | 1.815 / 1.881 | 1.510 / 2.131 |
| 10,000 | B-clone | 1.184 / 1.218 | 1.563 / 1.579 |
| 10,000 | B-copy | 1.702 / 1.980 | 2.509 / 2.616 |
| 100,000 | A-real | 23.002 / 28.333 | 21.946 / 31.903 |
| 100,000 | A-tuned | 15.661 / 16.294 | 18.984 / 21.218 |
| 100,000 | B-clone | 12.540 / 13.046 | 16.973 / 17.042 |
| 100,000 | B-copy | 26.954 / 30.504 | 27.639 / 28.652 |

### Native APFS ingest

Fresh-store setup, hashing and final checkpoint included. A-real requests stronger
Apple fullfsync; use A-tuned durable versus B durable for the criterion.
NONE ingest was corrected in a separate cohort; policy rows do not isolate fsync overhead.

| Files | Store / policy | Seconds median / p95 | Median MB/s | Median files/s |
| --- | --- | --- | --- | --- |
| 1,000 | A-real | 20.309 / 21.411 | 0.345 | 49 |
| 1,000 | A-tuned/durable | 0.053 / 0.205 | 132.856 | 18,974 |
| 1,000 | A-tuned/none | 0.151 / 0.190 | 46.258 | 6,606 |
| 1,000 | B/durable | 0.219 / 0.228 | 32.008 | 4,571 |
| 1,000 | B/none | 0.192 / 0.290 | 36.541 | 5,219 |
| 10,000 | A-real | 218.169 / 238.913 | 0.323 | 46 |
| 10,000 | A-tuned/durable | 0.332 / 0.336 | 212.236 | 30,095 |
| 10,000 | A-tuned/none | 0.974 / 1.061 | 72.415 | 10,269 |
| 10,000 | B/durable | 1.790 / 1.906 | 39.392 | 5,586 |
| 10,000 | B/none | 1.710 / 1.747 | 41.245 | 5,849 |
| 100,000 | A-real | 2,181.694 / — (extrapolated) | unmeasured | unmeasured |
| 100,000 | A-tuned/durable | 4.895 / 5.336 | 142.461 | 20,428 |
| 100,000 | A-tuned/none | 9.739 / 11.249 | 71.609 | 10,268 |
| 100,000 | B/durable | 24.356 / 24.840 | 28.632 | 4,106 |
| 100,000 | B/none | 21.280 / 21.916 | 32.771 | 4,699 |

### Physical allocation

Multiples of corpus logical bytes; median / p95 of five independent synced, settled allocations.
SQLite columns include its DB and checkpointed WAL. B uses store-to-base clones.

| Files | Backend | Store alone | + base | + 1 Layer | + 5 Layers |
| --- | --- | --- | --- | --- | --- |
| 1,000 | A-real | 1.126 / 1.126 | 2.539 / 2.539 | 2.589 / 2.589 | 2.787 / 2.787 |
| 1,000 | A-tuned/durable | 1.117 / 1.117 | 2.529 / 2.529 | 2.579 / 2.579 | 2.777 / 2.777 |
| 1,000 | B/durable | 1.429 / 1.429 | 1.482 / 1.482 | 1.531 / 1.531 | 1.729 / 1.729 |
| 10,000 | A-real | 1.110 / 1.190 | 2.520 / 2.600 | 2.572 / 2.653 | 2.783 / 2.863 |
| 10,000 | A-tuned/durable | 1.111 / 1.111 | 2.519 / 2.519 | 2.572 / 2.573 | 2.783 / 2.783 |
| 10,000 | B/durable | 1.419 / 1.420 | 1.471 / 1.471 | 1.523 / 1.523 | 1.733 / 1.734 |
| 100,000 | A-real | 1.131 / 1.134 | 2.546 / 2.549 | 2.600 / 2.603 | 2.817 / 2.820 |
| 100,000 | A-tuned/durable | 1.128 / 1.131 | 2.543 / 2.546 | 2.597 / 2.600 | 2.814 / 2.817 |
| 100,000 | B/durable | 1.425 / 1.426 | 1.477 / 1.477 | 1.531 / 1.531 | 1.745 / 1.746 |

At 100k, apparent lengths count shared contents repeatedly. The SQLite value is the closed database plus any WAL/SHM; clone apparent lengths do not measure exclusive blocks.

| Store | Store apparent MB | Base + 1 Layer apparent MB | Base + 5 Layers apparent MB |
| --- | --- | --- | --- |
| A-real | 774.451 | 1,394.730 | 4,184.189 |
| A-tuned/durable | 775.365 | 1,394.730 | 4,184.189 |
| B/durable | 697.365 | 1,394.730 | 4,184.189 |

### 100k incremental materialization

Milliseconds, warm sources; new blobs already stored, base setup excluded.

| Path | Changed entries | ms median / p95 |
| --- | --- | --- |
| A-tuned | 1 | 0.165 / 0.267 |
| A-tuned | 100 | 10.518 / 19.360 |
| B-clone | 1 | 1.963 / 3.239 |
| B-clone | 100 | 13.969 / 28.564 |
| B-copy | 1 | 0.155 / 0.248 |
| B-copy | 100 | 10.746 / 12.039 |

### Random full-blob reads

Microseconds, p50 / p99 pooled over 5,000 reads. Cold-start means one reset per
1,000 reads, then warming; not independent cold reads.

| Files | Store | Warm us p50 / p99 | Cold-start us p50 / p99 |
| --- | --- | --- | --- |
| 1,000 | A-real | 11.291 / 98.417 | 15.833 / 3,433.041 |
| 1,000 | A-tuned/durable | 3.334 / 8.333 | 4.583 / 335.917 |
| 1,000 | B/durable | 14.250 / 21.709 | 129.708 / 425.125 |
| 10,000 | A-real | 11.042 / 54.541 | 154.375 / 954.041 |
| 10,000 | A-tuned/durable | 4.375 / 11.709 | 6.875 / 611.417 |
| 10,000 | B/durable | 15.708 / 25.250 | 211.542 / 637.792 |
| 100,000 | A-real | 13.833 / 69.958 | 305.042 / 3,653.500 |
| 100,000 | A-tuned/durable | 6.792 / 20.625 | 395.792 / 1,900.834 |
| 100,000 | B/durable | 124.958 / 199.291 | 320.000 / 958.166 |

### 100k eviction

10% of objects by recorded LRU order. Times are median / p95; freed values are
median MiB, measured physically after sync (not logical victim bytes).

| Store | Retention | DELETE/unlink ms | Freed MiB | Vacuum ms | Extra freed MiB |
| --- | --- | --- | --- | --- | --- |
| A-tuned/durable | no-base | 742.281 / 3,059.308 | 0.000 | 2,910.070 / 5,253.177 | 73.246 |
| A-tuned/durable | live-base | 990.240 / 1,087.666 | 0.000 | 1,741.035 / 3,642.945 | 73.246 |
| A-tuned/none | no-base | 724.735 / 2,945.058 | 0.000 | 375.596 / 2,569.996 | 73.246 |
| A-tuned/none | live-base | 687.509 / 693.964 | 0.000 | 404.435 / 448.168 | 73.246 |
| B/durable | no-base | 4,067.497 / 7,776.238 | 93.254 | n/a | n/a |
| B/durable | live-base | 3,607.735 / 3,610.249 | 2.289 | n/a | n/a |
| B/none | no-base | 4,570.662 / 8,790.905 | 93.266 | n/a | n/a |
| B/none | live-base | 4,813.450 / 6,751.546 | 2.293 | n/a | n/a |

### 10-edit checkpoint store cost

Milliseconds, 20 repetitions; store-side work only. The sparse metadata case
models checkpoint storage; the full-manifest case is a conservative common SQLite
metadata proxy using real canonical manifest bytes. Metadata-only lstat walks are
reported alongside, outside the commit timer.

The real 100k-entry canonical manifest proxy is 14,233,271 bytes (14.233 MB); both designs write it to SQLite.

| Store | Metadata variant | Store commit ms median / p95 | Stat walk ms median / p95 |
| --- | --- | --- | --- |
| A-real | blobs-only | 324.821 / 416.755 | 311.226 / 399.451 |
| A-real | with-checkpoint | 337.584 / 396.173 | 325.068 / 392.017 |
| A-real | with-manifest | 444.098 / 638.292 | 330.397 / 384.546 |
| A-tuned/durable | blobs-only | 5.333 / 12.608 | 301.412 / 344.729 |
| A-tuned/durable | with-checkpoint | 5.455 / 7.010 | 300.376 / 329.651 |
| A-tuned/durable | with-manifest | 73.918 / 104.817 | 305.146 / 363.406 |
| B/durable | blobs-only | 3.812 / 24.906 | 306.305 / 469.779 |
| B/durable | with-checkpoint | 4.746 / 29.899 | 308.870 / 382.238 |
| B/durable | with-manifest | 77.938 / 110.838 | 317.116 / 434.847 |

### Randomized killed writers

20 SIGKILL trials per durable store, every final hash verified after every kill;
SQLite also passes integrity_check. Temp counts are swept and verified absent.

| Store | Passed trials | Final objects hash-verified (sum) | Temps swept (sum) |
| --- | --- | --- | --- |
| A-tuned/durable | 20/20 | 11191 | 0 |
| B/durable | 20/20 | 10047 | 9 |

### Clone proof and Layer context

Context creates a sixth Layer while five snapshot Layers remain live. It uses the
image and is not directly comparable to native first-Layer timings elsewhere.

| Files | Clone base allocation MB | Copy base allocation MB | Directory-clone Layer s median / p95 |
| --- | --- | --- | --- |
| 1,000 | 0.369 | 9.884 | 0.018 / 0.023 |
| 10,000 | 3.625 | 99.373 | 0.435 / 0.449 |
| 100,000 | 35.836 | 988.078 | 7.382 / 7.595 |

### Separate decision verdicts (APFS only)

| Criterion / condition | Measured 100k evidence | APFS result |
| --- | --- | --- |
| 1: materialize ≥5× | 1.249× warm; 1.119× cold-volume speedup | no-go |
| 1: store+base ≤1.2× | 1.477× median / 1.477× p95 | no-go |
| 1: ingest ≤2× time | 4.976× B/A-tuned durable | no-go |
| 2(1): zero file-byte copies | clonefile-only path; cold base allocates 35.836 / 35.836 MB median/p95 vs 987.308 MB copy; metadata allocation is not zero | go: sharing evidence |
| 2(2): cold base no slower | 1.119× speedup; B/A time 0.894× | go |
| 2(3): store+base ≤1.1× | 1.477× corpus logical bytes | no-go |
| 2(4): 10-edit commit ≤1.1× time | 0.870× B/A-tuned with sparse metadata; 1.054× with full-manifest proxy; 0.014× B/A-real sparse | go |
| 2(5): killed writer has no partial final | 20/20 B kills passed; every final hash checked | go |

**Provisional APFS verdict: no-go under criterion 1 and criterion 2.** The criteria agree: per-file reflinks share data, but the measured layout exceeds both logical-byte disk bounds. Criterion 1 additionally fails its speed/ingest gates as shown above. Criterion 2 uses cold-volume materialization and the sparse-metadata store-cost model for the 10-edit comparison; the full-manifest proxy is context. Current-configuration A-real is shown separately because its stronger Apple durability and per-blob API transactions are not a matched baseline.

These are APFS-image materialization/allocation and native-APFS ingest measurements on one M1 host. Criterion 1's cross-platform decision remains pending btrfs CI. Criterion 2 already misses its required APFS half; btrfs/XFS results remain pending and cannot make this APFS measurement pass. Under the stated criteria, retain SQLite blobs on APFS. No Linux policy is inferred from this host.

### Disk denominators (100k, same store+base allocation)

The shared allocated-tree denominator is 987,373,568 bytes: the median physical delta of a standalone A-tuned read/write checkout after a cold-volume start. The logical denominator is 697,364,836 bytes. Store+base numerators are medians of five independent allocations.

| Store, durable | Store+base allocated MB (approx) | / logical content bytes (official) | C1 ≤1.2× / C2 ≤1.1×, logical | / allocated standalone tree (context) | C1 ≤1.2× / C2 ≤1.1×, allocated |
| --- | ---: | ---: | --- | ---: | --- |
| A-real | 1,775.825 | 2.546× | no-go / no-go | 1.799× | no-go / no-go |
| A-tuned/durable | 1,773.740 | 2.543× | no-go / no-go | 1.796× | no-go / no-go |
| B/durable | 1,029.804 | 1.477× | no-go / no-go | 1.043× | go / go |

**Denominator note:** roughly 4 KiB minimum per-file allocation rounding is paid by any plain checkout, regardless of blob-store design. B store+base is roughly 1.04× an allocated standalone tree and **would pass both the 1.2× and 1.1× disk bounds under that denominator**. It fails both bounds under logical content bytes. Both official verdicts retain the literal pre-registered logical denominator; the allocated-tree comparison does not change them. This table makes the consequence of either interpretation explicit.

The standalone tree alone allocates 1.416× logical bytes, already above both absolute disk bounds. On this APFS layout and fixture, a plain checkout therefore exceeds either bound before adding any blob store.

**Context only:** substituting allocated-tree bytes would make the measured APFS median conditions of criterion 2 pass, subject to the cold-volume and store-cost model limits below. Criterion 1 would still fail its materialization-speed and ingest gates. Neither counterfactual changes the official logical-byte verdicts.

B nevertheless uses 41.9% less store+base space than A-tuned SQLite in this measurement. Relative improvement and the absolute logical-byte bounds answer different parts of the decision.

The sparse-checkpoint median passes criterion 2(4), but its p95 is 4.27× the SQLite p95. That tail regression remains relevant even though the decision table compares medians.

What could change the answer: a larger-file corpus, a filesystem layout that avoids small-file rounding, or a file-store design that reduces per-file metadata and durable ingestion cost. Those possibilities are unmeasured. The current plain-file design does not meet either disk bound on the required fixture even before adding common manifest history. The allocated-tree denominator passes both disk bounds for B as shown above; the official gates use corpus logical bytes.

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
  If an attempted 100k ingest hits the budget despite its projection, the harness
  records the aborted duration and extrapolation, then seeds a complete corpus
  outside timing so the actual API's read/materialization context is retained.
- **A-tuned** retains `objects(id TEXT PRIMARY KEY CHECK(length(id)=64), kind
  INTEGER NOT NULL CHECK(kind IN (1,2,3)), bytes BLOB NOT NULL) STRICT`, plus
  immutable UPDATE/DELETE triggers. It uses WAL, batches of 1,000, cached prepared
  statements, 64 MiB cache, 2 GiB mmap limit and auto_vacuum=INCREMENTAL. A five-rep
  pilot compares 4 KiB and 16 KiB pages and SELECT versus incremental blob reads.
  The APFS run selects **4,096-byte pages and plain SELECT**: pilot ingest medians
  are 79.873 versus 94.526 ms, and 1,000 warm reads take 4.713 versus 6.548 ms
  for SELECT versus incremental I/O (five reps each). Only
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
case. Tables use nearest-rank p50 (reported as median), p95 and p99; p50 with an
even sample count selects the lower middle observation. Decisions compare p50s,
with tail quantiles retained for variability. With five samples p95 is the
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
These core footprints contain corpus blobs and working trees; the tuned/file
cases omit shared manifest/checkpoint history. A-real retains its initialized
repository metadata. Adding a metadata database to a production split would
increase B's footprint; the checkpoint supplement measures that write cost
separately, without treating its accumulated history as corpus data.

A Layer is an APFS directory clone or a Linux per-file reflink of the base; ext4
uses an explicitly labeled byte copy. Context base-to-Layer timings are separate
from store-to-base comparisons. APFS sharing must allocate less than half the
explicit-copy control for the same corpus. Context times create a sixth Layer
while the five disk-snapshot Layers remain live; the image and cache state also
differ from the native first-Layer results in `workspace-approach.md`. Linux
samples of files at least 4 KiB require FIEMAP shared
extents; this check does not certify tiny/inline-file metadata sharing. A sharing failure terminates the run. Every original remains live for
the check. On this APFS volume, small B objects still consume 4 KiB minimum allocations,
plus directory and inode metadata. Any plain checkout on this layout pays the same
small-file allocation rounding, regardless of blob-store design. Linux layout and
rounding remain unmeasured. Reflinks share those blocks but cannot remove that rounding.
For this exact 100k corpus, 697,364,836 logical bytes round to 932,982,784
payload bytes at 4 KiB per-file granularity: **1.338× before metadata**. This
APFS allocation floor already exceeds both decision bounds. It is a deterministic
size calculation, separately reported from measured statfs deltas; it is not an
assumption about btrfs inline-file accounting.

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

## Ten-edit checkpoint store cost

At 100k, 20 repetitions per variant apply five single-byte appends and five
same-size first-byte XOR rewrites, matching `examples/capture_bench.rs`. The
100k-file base is materialized and a Layer is cloned from it before timing
(APFS directory clone / Linux per-file reflink; explicit copy on ext4). The base
remains live during the edits. Timings read and hash the ten
files and commit their blobs; connections are already open. A-real uses actual
per-blob `Repository::put_blob`; A-tuned commits the ten in one WAL FULL
transaction; B writes/fsyncs/renames ten temps and fsyncs touched directories.

The harness reports blobs alone, blobs plus a sparse-checkpoint-shaped SQLite
row, and blobs plus real `Manifest::canonical_bytes()` for 100k entries. Metadata
stays in SQLite for B, using the same WAL FULL policy as A-tuned. The tuned case
places blobs and metadata in one transaction; B publishes blobs before committing
metadata. A-real uses its real EXTRA/fullfsync blob policy and an EXTRA/fullfsync
metadata transaction. Final WAL checkpoints are timed for the tuned/split cases.
Distinct edits prevent accidental dedupe between trials; each store begins with
the same corpus. The full-manifest row is the requested conservative proxy:
`capture_at` batches new blobs, and `prepare_checkpoint` writes sparse checkpoint
bytes rather than a new full manifest. A-real's requested per-blob baseline is
therefore context, not an exact measurement of today's entire capture transaction.

Serialization, filesystem edits, the stat walk, cache persistence, graph
validation and Layer-head publication are outside this store comparison. The
sparse row has the production encoding shape and kind tag but is not a complete
valid Layer transaction. A metadata-only lstat walk over the 100k-file Layer is
reported alongside each trial for scale; it is not a timed full `capture::scan`.
These measurements do not establish end-to-end capture latency.

## Crash safety

The small SIGKILL check stores 100 blobs, pauses with a partial next blob in
`tmp/`, kills the child, hashes every final object and checks that exactly 100
final paths exist. The partial tmp is detectable and swept. This exercises one
known pre-rename process-crash boundary. The additional decision measurement
re-execs the actual ingestion loop and SIGKILLs it after each of 20 deterministic
pseudorandom 1–200 ms delays per durable store. A ready handshake excludes process
startup. Each trial starts with 100 committed blobs, verifies every final file or
row after the kill, and checks SQLite `PRAGMA integrity_check`. B temp leftovers
are counted, swept, and checked absent. The child stream includes a 2 MiB
pseudorandom payload alongside 100 fixture blobs per batch, covering multi-page
writes without changing the timed corpus. Delays, final counts and temp counts are
saved in raw results. This samples active ingestion without prescribing a write
boundary; it is still process-crash testing, not power-loss proof.

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

The same workflow includes the 20-repetition checkpoint variants and 20 randomized
writer kills per durable store. No performance cutoff is used.

| FS | C1 speed / disk / ingest | C2 (1) base physical delta / FIEMAP shared fraction | C2 (2) cold speed | C2 (3) ≤1.1× | C2 (4) 10-edit ≤1.1× time | C2 (5) killed writer | Verdicts C1 / C2 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| btrfs | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run / pending CI run |
| XFS reflink=1 | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run / pending CI run |
| ext4 copy control | pending CI run | pending CI run (clone expected unsupported) | pending CI run | pending CI run | pending CI run | pending CI run | pending CI run / pending CI run |

Criterion 1 explicitly names APFS or btrfs; its XFS/ext4 columns are diagnostic
and cannot independently satisfy that cross-platform gate.

Linux timing, shared-extent proof and physical accounting are unmeasured locally;
no Linux outcome is inferred from APFS. The CI budget estimate is 45–120 minutes per filesystem, with all three jobs
running in parallel. This is unconfirmed: estimates use the operation count
(hundreds of thousands of durable commits/fsyncs and millions of file creations)
and the APFS run, not Linux data. Slow hosted storage can exceed that range;
the 180-minute timeout keeps the full matrix, and ordinary step failures still
run artifact upload. Results are saved incrementally.

## Implications to revisit with the completed data

A rolling base pays for changed entries instead of a fresh full materialization.
On this APFS run, a one-entry clone update takes 1.963 ms median versus 0.165 ms
for A-tuned read/write; a 100-entry changeset takes 13.969 versus 10.518 ms.
These changesets mix modifications, additions and deletions. The per-file clone
store does not improve either measured rolling-base update median.
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
This is traversal between known manifests/changesets: discovering arbitrary edits
in a working directory still needs a stat scan or another change-detection
mechanism. Smaller directory manifests would also change the common checkpoint
metadata cost, so the full-manifest proxy must not be extrapolated to that design.

## Measurement limits

The local case order is fixed rather than randomized. The shared host, image
backing cache, background allocation and thermal state are not controlled. Warm
and cold-volume times can invert, and five samples give no narrow confidence
interval; retain the p95/max and raw samples when interpreting small differences.
Cold volume is not cold drive hardware. The native-ingest/image-operations split
also limits end-to-end conclusions. Physical footprints are isolated-volume allocation deltas and include
filesystem metadata. The original single-snapshot SQLite pass is superseded by
the independent-allocation supplement. The original B directory-copy setup is
excluded; the repeated file-store pass uses actual insertion-order setup.
The SQLite timings and B rerun are separate cohorts, without paired or
randomized interleaving. The allocation supplement does not re-time SQLite
operations. SQLite NONE ingest was separately repeated after fixing schema setup
to apply OFF from the start; other NONE cases retain the same data/schema workload.
Policy rows from separate cohorts do not isolate fsync overhead. Small timing
differences should not be treated as causal proof.
The disk bounds and ingest differences do not depend on such small differences.
Content is random
and incompressible; other size distributions and compressible trees may change
rounding or filesystem-specific inline-storage behavior. Windows, networking,
concurrent writers, process RSS, file-descriptor pressure and object-store scale
beyond 100k are unmeasured. Hash verification is outside the tuned/copy hot paths;
including it would change latency, especially compared with zero-copy cloning.
