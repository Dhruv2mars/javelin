# Storage bench (benchmark code only)

Native APFS decision timings, from the checkout root (fresh scratch path):

```sh
rtk proxy cargo run --release --locked --manifest-path tools/storage-bench/Cargo.toml -- \
  --dir /tmp/jbench-native-decision --files 100000 --reps 5 --decision-only \
  --label 'native APFS internal SSD, warm only' --json /tmp/apfs-native.json
```

This runs four matched ingest cases (A-tuned FULL/OFF, B fsync/none), rotating
case order every repetition, then rotates A-tuned read/write, B-clone and B-copy
materializations. All matched decision cases use 4KiB SQLite pages and SELECT,
without a pilot-dependent choice. `decision-order` rows record every repetition.
`decision/*` rows are the timing evidence for the decision. The broader matrix's
sequential cohorts remain context and must not replace the matched cohort.
Warm setup reads the entire source. Each case has a common sync plus two-second
boundary. On an isolated filesystem this also requires a sustained allocation
plateau; native system-volume statfs deltas are diagnostic because unrelated
writers prevent exclusive accounting. Cold timing is explicitly unmeasured
without a working cache-reset hook. No fallback is hidden in B-clone.

For the complete matrix, prepare and mount a **dedicated** scratch filesystem
first. On Linux, run the measurement unprivileged with sudo available only for
cache drop and the read-only btrfs accounting command:

```sh
rtk proxy cargo run --release --locked --manifest-path tools/storage-bench/Cargo.toml -- \
  --dir /mnt/storage-bench/run --files 1000,10000,100000 --reps 5 \
  --label 'btrfs dedicated loop image' \
  --drop-caches-cmd "sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'" \
  --json /tmp/btrfs.json
rtk proxy python3 tools/storage-bench/summarize.py /tmp/btrfs.json
```

The full run includes matched decision cohorts, the real Repository API context,
five independent allocations of store/base/one/five Layers, incremental changes,
reads, LRU eviction, all three checkpoint store-cost proxies (20 repetitions each)
and killed writers. The Linux workflow builds and runs the same command on btrfs,
XFS (`reflink=1`) and ext4, with 16GiB loop images. It records runner/kernel/CPU,
distro, formatter versions and machine.txt in each artifact. The workflow does
not cancel an active run on push and has no performance acceptance gate.

Output JSON retains raw samples, exact machine/argv, order rows and accounting
readings. Markdown retains per-case sample counts, median/p95/p99 and notes.
Both update after every row, preserving partial results. `summarize.py` creates a
summary JSON of all case statistics, sample counts and SHA-256/location of the raw
file. Commit summaries and Markdown; retain raw JSON in CI artifacts or the
recorded local archive. Git history is not rewritten to remove earlier raw files.
Nearest-rank quantiles are used; p95 is the maximum for five repetitions.

Other modes/options:

| Option | Meaning |
| --- | --- |
| `--decision-only` | Rotated matched ingest/materialization cohort; ignores `--stores` and uses `--dir`, not `--ingest-dir` |
| `--kill-only` | 20 randomized kills per durable store, plus five SQLite mid-transaction, five B mid-payload and five B pre-rename kills |
| `--extras-only` | Checkpoint store-cost proxies plus killed writers; requires at least 1,000 files |
| `--disk-only` | Standalone allocation/deletion controls and independent disk states, without timing the broad operation matrix |
| `--ingest-only` | Legacy sequential ingest context; use `--decision-only` for matched comparisons |
| `--stores A-tuned/durable,B/durable` | Select broad matrix/disk cases; does not restrict matched decision or extras cohorts |
| `--page-size 4096` | Fix broad matrix page size; otherwise a 4/16KiB pilot chooses it |
| `--ingest-dir NEW_PATH` | Broad matrix only: ingest all stores on a separate native volume, then seed measured stores outside timings |
| `--real10k-ms MS` | Recorded real-API 10k baseline for a 100k-only context run; projected ingest over 15 minutes is explicitly extrapolated |
| `--remount-cmd COMMAND` | One 100k store+base remount control per selected store, at the first disk repetition |

At least five repetitions and 100 files are required. Existing scratch paths are
refused. Modes should be used separately. Successful runs delete only generated
scratch data. On failure inspect the fresh scratch path and any
`temp_dir/javelin-storage-seed-<pid>` before removing them. A-real's 100k ingest is
budgeted separately; an aborted or extrapolated case never becomes a measured
throughput result. Untimed real-schema SQL seeding provides actual API read
context, not production ingestion evidence.

APFS disk/remount control setup (image outside the repository):

```sh
rtk proxy hdiutil create -size 30g -fs APFS -type SPARSE -volname jbench-image /tmp/jbench-image.sparseimage
rtk proxy hdiutil attach -nobrowse /tmp/jbench-image.sparseimage
rtk proxy sh -c 'cat > /tmp/jbench-remount.sh <<"SH"
#!/bin/sh
set -eu
hdiutil detach /Volumes/jbench-image >/dev/null
hdiutil attach -nobrowse /tmp/jbench-image.sparseimage >/dev/null
SH'
rtk proxy chmod +x /tmp/jbench-remount.sh
rtk proxy cargo run --release --locked --manifest-path tools/storage-bench/Cargo.toml -- \
  --dir /Volumes/jbench-image/run --files 100000 --reps 5 --disk-only \
  --stores A-tuned/durable,B/durable --page-size 4096 \
  --remount-cmd /tmp/jbench-remount.sh --label 'APFS isolated image, disk control' \
  --json /tmp/apfs-image.json
rtk proxy hdiutil detach /Volumes/jbench-image
```

The same hook can supply `--drop-caches-cmd` for cold-volume context in a full run.
Close other handles to the image first. Detach/re-attach resets volume cache;
host image-backing and SSD caches can remain warm. Image timings are not native
SSD timings. Native cold timing needs a separate working reset mechanism.

A-tuned preserves production object columns, constraints and immutable triggers,
but uses WAL, 64MiB cache, a 2GiB mmap limit and 1,000-object batches. Matched durable
means SQLite FULL/fullfsync OFF versus ordinary fsync of every B temp before
rename, then touched fanouts and tmp directory once per batch. NONE means SQLite
OFF versus no fsync. Setup and final WAL checkpoint are included in ingest.
A-real retains its actual, stronger Apple fullfsync behavior as context. There is
one writer. No power-loss durability claim follows from process-kill trials.

B uses canonical Javelin blob IDs, 256 fanouts and 0444 objects, publishing through
temp/write/chmod/fsync/rename. Materialization includes directories and writable
permissions, excluding sync, store-handle opening, verification and cleanup.
B-copy loops over read/write; it never uses a copy API that might silently reflink.
Per-file APFS clone does not stat its source to test whether it is a directory.
Directory cloning of an already materialized base is a separate Layer context.

Disk snapshots sync and require five equal readings at 500ms intervals (at least
two seconds), with a 60-second timeout; raw readings are saved. On btrfs, the
primary number sums `btrfs filesystem usage --raw` **used** Data, Metadata and
System bytes, multiplying DUP/RAID1 by two. GlobalReserve is already metadata and
is not counted twice. Unsupported profiles fail explicitly. Statfs is retained as
a diagnostic; APFS/XFS/ext4 use statfs allocation deltas. Standalone explicit-write
trees and deletion residuals control accounting and supply the separately labelled
allocated-tree denominator. Official bounds always divide by logical corpus bytes.
FIEMAP inspects **every file**, including tiny ones, reports inline/shared/unshared/
unmapped payload bytes and file counts, and paginates all extents. Inline data is
counted as copied payload, so an inline btrfs clone cannot satisfy zero file bytes.

Kill injection is inside the benchmark's actual ingestion path and is enabled
only for child fault trials. B pauses after half a 2MiB payload and after complete
file fsync before rename; SQLite pauses after ten inserts before commit. The parent
waits for each exact stage, SIGKILLs, records observed stage and temp lengths, checks
all visible hashes and SQLite integrity, and sweeps temps. Randomized trials are
retained as additional coverage. Timing paths do not enable those hooks.

Checkpoint rows are **store-cost proxies**: ten file reads/hashes and blob commits,
optionally plus a sparse-checkpoint-shaped or full-manifest SQLite row. Tuned blobs
and metadata share one transaction; real capture commits blobs, saves the cache,
then commits checkpoint/head separately. Scan, serialization, cache persistence,
graph validation and head publication are excluded. These rows cannot verify the
full C2 checkpoint criterion. Both medians and tails remain available.

See [decision and results](../../docs/storage-benchmark.md) and
[run provenance](results/README.md).
