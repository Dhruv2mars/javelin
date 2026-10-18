# Run provenance

Round-3 matched timings supersede sequential timing cohorts for the decision.
The native 100k timing cohort, image accounting/remount controls and final Linux
run are listed below. Historical image cold-volume timing and checkpoint proxy
rows remain context. Both decision criteria are stated before results in the
[decision document](../../../docs/storage-benchmark.md).

Committed JSON files are compact summaries (median/p95/p99, sample count, notes,
raw SHA-256 and location). Raw local outputs live outside Git at
`/Users/dhruv2mars/.local/share/javelin-storage-bench/round3/`; Linux raws live in
the corresponding Actions artifact. Earlier raw files were removed by a normal
deletion commit; branch history was not rewritten. Historical consolidated rows
retain `source_run` provenance. Markdown tables retain every case statistic.
Machine specs and argv are embedded in each summary. Use
`python3 tools/storage-bench/summarize.py RAW.json --location LOCATION` to regenerate.

## Round 3

Final benchmark code: `8d787684765ce087324e433ac56401b433d7834c`.
Complete Linux run: [39130520311](https://github.com/Dhruv2mars/javelin/actions/runs/39130520311).
Its three filesystem artifacts include raw JSON, compact summaries, complete
Markdown and `machine.txt`. The previous
[run 39121284581](https://github.com/Dhruv2mars/javelin/actions/runs/39121284581)
is superseded for Linux decisions and accounting.

The native decision run uses:

```sh
rtk proxy /usr/bin/time -p tools/storage-bench/target/release/storage-bench --dir /tmp/jbench-r3-native-100k --files 100000 --reps 5 --decision-only --label 'APFS native internal SSD; round3 rotated 100k decision cohort; no cache reset available' --json /Users/dhruv2mars/.local/share/javelin-storage-bench/round3/apfs-native-round3.json
```

Native cold was unavailable: `/usr/sbin/purge` returned `Unable to purge disk
buffers: Operation not permitted`; `sudo -n true` required a password. The
strict free-space plateau initially failed on the active system volume after
60 seconds, before timing began. Native timing now uses sync plus two seconds;
its space deltas are diagnostic, and all authoritative footprint numbers use
the isolated image. This is explicit in code and raw notes.

The image control uses a new 30GiB sparse APFS image at
`/tmp/jbench-r3.sparseimage`, mounted as `/Volumes/jbench-r3`. The hook detaches
that volume and reattaches the same image, exactly as in the benchmark README.

Native elapsed wall time (`/usr/bin/time -p`) was 1220.47s, with no paused
operations. Native warm medians/p95s: A-tuned 18.406/20.348s, B-clone
21.551/23.880s, B-copy 25.486/30.008s. Durable ingest was
7.768/9.604s for A and 32.233/33.827s for B. Exact samples/order:
[summary](apfs-native-round3.summary.json), [tables](apfs-native-round3.md).

## Historical round-1 image/context cohorts


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
| [apfs-2026-10-18-sqlite.json](apfs-2026-10-18-sqlite.summary.json) | 4b348cb | 4209.10 | 4704.47 | SQLite core timings and pilots; incomplete primary, invalid B setup excluded |
| [apfs-2026-10-18-files.json](apfs-2026-10-18-files.summary.json) | 9db759e | 1540.17 | 6430.46 | B durable/none full matrix, five independent allocations |
| [apfs-2026-10-18-sqlite-disk.json](apfs-2026-10-18-sqlite-disk.summary.json) | aa26561 | 1612.43 | 1612.45 | A-tuned durable/none five independent allocations |
| [apfs-2026-10-18-real-disk.json](apfs-2026-10-18-real-disk.summary.json) | aa26561 | 433.60 | 433.62 | A-real 1k/10k five independent allocations |
| [apfs-2026-10-18-real-context.json](apfs-2026-10-18-real-context.summary.json) | aa26561 | 1418.64 | 1418.65 | A-real 100k real-API read context and five allocations; 20-rep checkpoint variants and 40 randomized kills |
| [apfs-2026-10-18-none-ingest.json](apfs-2026-10-18-none-ingest.summary.json) | ff6968c | 111.15 | 111.17 | Corrected SQLite NONE schema initialization: ingest-only repeats; other SQLite rows retain identical data/schema workloads |

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
Round-3 Linux measurements supersede these cohorts. No production code was changed.
