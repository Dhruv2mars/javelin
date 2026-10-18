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
repetitions, except the explicitly limited real-store 100k ingest. Disk allocation
snapshots use one sample; every materialization also records its allocation delta.
`--reps` must be at least five. Omit the cache hook to mark cold cases unmeasured.
The hook runs outside timings and must fail if it cannot reset the cache. The
benchmark refuses an existing scratch directory and removes its generated data on
success. On failure, inspect and remove that scratch directory yourself.

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

`--ingest-dir` is optional. On this Mac, image fsync is about 100× slower than
native APFS fsync. The committed run therefore measures **all** stores' ingest
on native internal-SSD APFS, then byte-copies their closed, checkpointed stores
onto the dedicated image outside timings. Materialization, disk and eviction are
measured on the image. Ingest comparisons are native-to-native; materialization
comparisons are image-to-image. The image and native results are not interchangeable.
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
but exclude sync, correctness checks and cleanup. B-copy uses explicit read/write;
B-clone never falls back. Linux requires FIEMAP shared extents on sampled files;
APFS requires clone allocation to be less than half the explicit-copy control.
The SIGKILL test interrupts one deterministic pre-rename boundary; it is not a
power-loss test. LRU eviction uses a recorded deterministic touch sequence and
reports both DELETE/unlink and actual incremental-vacuum reclamation.

See [the decision and results](../../docs/storage-benchmark.md). Linux CI uses
three parallel jobs, each with a dedicated 16 GiB loop image, including ext4.
