# APFS run provenance

The consolidated JSON/Markdown retains exact selected raw rows and names each
row's `source_run`. Individual JSON/Markdown files are unedited harness outputs.
Machine specs and argv are embedded in every JSON file.

SQLite NONE schema setup initially used SQLite's default FULL before ingest
switched to OFF. The ingest-only correction applies OFF before schema creation;
its three ingest metrics replace the old NONE ingest rows at every corpus size.
The blob payload, schema, read/materialization and eviction operations are unchanged.

The primary SQLite timing pass was stopped after completing the SQLite cases:
its B setup had copied the file store in hash-directory order. All its B rows
are excluded. Its single-allocation physical rows and Layer context are also
superseded by five-allocation repeats. The file-store rerun uses actual ordered
insertion on the measured image. A-real's 100k ingest is an explicit linear
extrapolation from its five-sample 10k median; actual API reads/materialization
use untimed bulk seeding into the real schema. No API ingest timing is claimed
for that setup.

The file-store command includes pauses between completed operations while the added code was built and smoke-tested on native scratch.
No timed operation was paused; total command wall time includes these pauses.

All ingest is native APFS when `--ingest-dir` is supplied. Materialization,
allocation, eviction, checkpoint and randomized crash cases use the dedicated
30 GiB APFS image. Cold cases detach/re-attach that image using the hook in
[the benchmark README](../README.md). The host image-backing cache can stay warm.
The image and all generated scratch data are removed after measurement.

Command `time -p real` reports and log-file timestamp spans are both retained.
They diverged for the long signalled/paused source runs; the cause is unconfirmed.
Timestamp spans are auxiliary and can include preexisting log lifetime. Per-op
latencies use Rust Instant, and no timed operation was paused. Whole-command
counters are not used for the performance comparisons.

| Raw run | Source commit | Reported time-p real s | Log timestamp span s | Selected contribution |
| --- | --- | ---: | ---: | --- |
| [apfs-2026-10-18-sqlite.json](apfs-2026-10-18-sqlite.json) | 4b348cb | 4209.10 | 4704.47 | SQLite core timings and pilots; incomplete primary, invalid B setup excluded |
| [apfs-2026-10-18-files.json](apfs-2026-10-18-files.json) | 9db759e | 1540.17 | 6430.46 | B durable/none full matrix, five independent allocations |
| [apfs-2026-10-18-sqlite-disk.json](apfs-2026-10-18-sqlite-disk.json) | aa26561 | 1612.43 | 1612.45 | A-tuned durable/none five independent allocations |
| [apfs-2026-10-18-real-disk.json](apfs-2026-10-18-real-disk.json) | aa26561 | 433.60 | 433.62 | A-real 1k/10k five independent allocations |
| [apfs-2026-10-18-real-context.json](apfs-2026-10-18-real-context.json) | aa26561 | 1418.64 | 1418.65 | A-real 100k real-API read context and five allocations; 20-rep checkpoint variants and 40 randomized kills |
| [apfs-2026-10-18-none-ingest.json](apfs-2026-10-18-none-ingest.json) | ff6968c | 111.15 | 111.17 | Corrected SQLite NONE schema initialization: ingest-only repeats; other SQLite rows retain identical data/schema workloads |

Commands ran from the checkout root. `/usr/bin/time -p` writes the reported real-time counter.
The primary output was subsequently renamed from `apfs-2026-10-18.json` to
`apfs-2026-10-18-sqlite.json`; the consolidated file is derived, not a seventh run.

## apfs-2026-10-18-sqlite.json

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /Volumes/jbench-storage/run --ingest-dir /tmp/jbench-native-ingest --files 1000,10000,100000 --reps 5 --label 'APFS (30 GB sparse disk image, internal SSD backing; native APFS ingest)' --drop-caches-cmd /tmp/storage-cache-reset.sh --json tools/storage-bench/results/apfs-2026-10-18.json
```

## apfs-2026-10-18-files.json

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /Volumes/jbench-storage/files-run --ingest-dir /tmp/jbench-files-ingest --files 1000,10000,100000 --reps 5 --page-size 4096 --stores B/durable,B/none --label 'APFS (30 GiB image; actual ordered file-store setup; native ingest)' --drop-caches-cmd /tmp/storage-cache-reset.sh --json tools/storage-bench/results/apfs-2026-10-18-files.json
```

## apfs-2026-10-18-sqlite-disk.json

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /Volumes/jbench-storage/sqlite-disk-run --ingest-dir /tmp/jbench-sqlite-disk-ingest --reps 5 --page-size 4096 --label 'APFS (30 GiB image; sqlite-disk supplement; native ingest)' --drop-caches-cmd /tmp/storage-cache-reset.sh --json tools/storage-bench/results/apfs-2026-10-18-sqlite-disk.json --files 1000,10000,100000 --stores A-tuned/durable,A-tuned/none --disk-only
```

## apfs-2026-10-18-real-disk.json

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /Volumes/jbench-storage/real-disk-run --ingest-dir /tmp/jbench-real-disk-ingest --reps 5 --page-size 4096 --label 'APFS (30 GiB image; real-disk supplement; native ingest)' --drop-caches-cmd /tmp/storage-cache-reset.sh --json tools/storage-bench/results/apfs-2026-10-18-real-disk.json --files 1000,10000 --stores A-real --disk-only
```

## apfs-2026-10-18-real-context.json

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /Volumes/jbench-storage/real-context-run --ingest-dir /tmp/jbench-real-context-ingest --reps 5 --page-size 4096 --label 'APFS (30 GiB image; real-context supplement; native ingest)' --drop-caches-cmd /tmp/storage-cache-reset.sh --json tools/storage-bench/results/apfs-2026-10-18-real-context.json --files 100000 --stores A-real --real10k-ms 218169.449291
```

## apfs-2026-10-18-none-ingest.json

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /tmp/jbench-none-corrected --files 1000,10000,100000 --reps 5 --page-size 4096 --stores A-tuned/none --ingest-only --label 'APFS (native internal SSD; corrected NONE schema initialization)' --json tools/storage-bench/results/apfs-2026-10-18-none-ingest.json
```

Checks also included locked release builds, native and x86_64-linux Clippy with
`-D warnings`, rustfmt, actionlint, a 100-file five-allocation smoke (38.94 s),
an initial 1k checkpoint/crash smoke (18.89 s), and the updated cloned-Layer /
2 MiB killed-writer smoke (20.52 s). Smoke outputs are not performance
evidence for the 100k decision and are not included in the consolidated samples.
Linux runtime measurements remain pending CI. No production code was changed.
