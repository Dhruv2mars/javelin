# Storage bench (not product code)

Run on a dedicated scratch filesystem, with a path that does **not** yet exist:

```sh
cargo run --release --locked --manifest-path tools/storage-bench/Cargo.toml -- \
  --dir /mnt/scratch/run --files 1000,10000,100000 --reps 5 --label 'btrfs loop image' \
  --drop-caches-cmd "sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'" \
  --json out.json
```

Tables print to stdout. `out.json` contains all samples, machine details, corpus
fingerprints and notes; `out.md` contains the tables. Both update after each
measurement, preserving partial output on failure. Timed operations use five
repetitions, except the explicitly limited real-store 100k ingest. The 100k run
also measures 10-edit store commits with 20 repetitions per variant, and kills
20 writers per durable store. `--extras-only` repeats those additional decision
measurements without rerunning the core matrix. Disk snapshots recreate store/base/Layer allocations for every repetition; every
materialization also records its allocation delta. `--disk-only` repeats just those
allocation cases, and `--page-size 4096` (or 16384) fixes the pilot choice for a
repeat run. SQLite uses a closed byte-identical seed in a temporary directory outside
the measured scratch directory; B is freshly ingested through its actual write
path. Seeds are removed on success.
`--reps` must be at least five. `--stores A-real` (or a comma-separated list of
exact store/policy labels) restricts core cases in a repeat run. The additional
100k decision measurements always compare all three required durable stores. `--real10k-ms MS` supplies an
already-measured real-store 10k median when repeating only 100k; this is recorded
in the invocation. If the projected 100k real ingest exceeds 15 minutes, it is
extrapolated and the real schema is batch-seeded outside timing for API read and
materialization context. This setup is explicitly distinct from production ingest. Omit the cache hook to mark cold cases unmeasured.
The hook runs outside timings and must fail if it cannot reset the cache. The
benchmark refuses an existing scratch directory and removes its generated data on
success. On failure, inspect and remove that scratch directory and any
`temp_dir/javelin-storage-seed-<pid>` left by this process.

APFS example (image is outside the repository):

```sh
hdiutil create -size 30g -fs APFS -type SPARSE -volname jbench-storage /tmp/jbench-storage.sparseimage
hdiutil attach -nobrowse /tmp/jbench-storage.sparseimage
cat > /tmp/storage-cache-reset.sh <<'SH'
#!/bin/sh
set -eu
hdiutil detach /Volumes/jbench-storage >/dev/null
hdiutil attach -nobrowse /tmp/jbench-storage.sparseimage >/dev/null
SH
chmod +x /tmp/storage-cache-reset.sh
cargo run --release --locked --manifest-path tools/storage-bench/Cargo.toml -- \
  --dir /Volumes/jbench-storage/run --files 1000,10000,100000 --reps 5 \
  --label 'APFS (30 GB sparse disk image, internal SSD backing)' --ingest-dir /tmp/jbench-native-ingest \
  --drop-caches-cmd /tmp/storage-cache-reset.sh --json out.json
hdiutil detach /Volumes/jbench-storage
rm /tmp/jbench-storage.sparseimage /tmp/storage-cache-reset.sh
```

`--ingest-dir` is optional. The committed run measures **all** stores' ingest
on native internal-SSD APFS, then seeds the image outside timings: byte-copy closed, checkpointed SQLite
files; insert B through its actual write path in corpus order with the same policy. Materialization, disk and eviction are
measured on the image. Ingest comparisons are native-to-native; materialization
comparisons are image-to-image. This keeps the real API's fullfsync timings on
native SSD while isolating disk accounting and resetting the volume cache.
The image and native results are not interchangeable.
Linux CI omits this flag and measures all operations on its loop filesystem.

Close Finder windows and other handles to the image before starting. Detach and
re-attach discards the mounted volume's filesystem cache; the host's image-backing
cache may remain warm. This is a cold **volume** measurement, not an SSD hardware
cache flush. Random-read cold-start sequences reset once per 1,000 reads and then
warm; they are not 1,000 independently cold reads. Warm cases explicitly read all
corpus objects first. The in-memory corpus itself remains allocated in both cases.

A-real is the actual `Repository` API, including its stronger Apple fullfsync and
read-back hash verification. It is context, not the matched-durability baseline.
A-tuned has the production `objects` columns, constraints and immutable triggers,
WAL, a 64 MiB cache, 2 GiB mmap limit and batches of 1,000. A pilot chooses 4/16 KiB
pages and SELECT/incremental blob reads. Durable means SQLite FULL (fullfsync OFF)
versus file fsync before rename and one fsync per touched directory per batch.
None means SQLite OFF versus no file/directory fsync. Files use real Javelin blob
IDs, 256 fanouts and read-only object files. There is one writer.

Materialization timings include directory creation and writable file permissions,
but exclude store-handle opening, sync, correctness checks and cleanup. B-copy uses explicit read/write;
B-clone never falls back. Linux requires FIEMAP shared extents on sampled files;
APFS requires clone allocation to be less than half the explicit-copy control.
The early SIGKILL test interrupts one deterministic pre-rename boundary. The
100k decision supplement also kills actual ingest loops at 20 pseudorandom delays
for each of B durable and A-tuned durable, verifies every final hash and SQLite
integrity, and counts/sweeps leftover temps. Neither is a power-loss test.
Checkpoint cases read/hash 10 edited files and commit their blobs; additional
variants include a sparse-checkpoint-shaped SQLite row and a real full-manifest
canonical-byte SQLite row. Both designs retain metadata in SQLite. These isolate
store cost and exclude scanning, serialization, graph validation, cache saving
and head publication; the full-manifest variant is a conservative proxy because
current capture writes a sparse checkpoint. A metadata-only walk is reported
alongside for scale. LRU eviction uses a recorded deterministic touch sequence and
reports both DELETE/unlink and actual incremental-vacuum reclamation.

See [the decision and results](../../docs/storage-benchmark.md). Linux CI uses
three parallel jobs, each with a dedicated 16 GiB loop image, including ext4.
