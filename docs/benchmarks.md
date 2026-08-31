# Benchmarks

These results were measured on 31 August 2026 against the packaged `javelin-1.0.0-aarch64-apple-darwin` binary. They are evidence for this build, not universal performance guarantees.

## Host

| Item | Value |
| --- | --- |
| Machine | Mac mini (Macmini9,1) |
| CPU | Apple M1, 8 cores (4 performance, 4 efficiency) |
| Memory | 16 GB |
| OS | macOS 26.6.2 (25G83), arm64 |
| Filesystem | APFS on internal solid-state storage |
| Materialization | APFS copy-on-write where available; streamed copy fallback remains covered by tests |

Peak resident memory was not captured for these runs, so no peak-memory claim is made.

## Project-size fixtures

Each fixture generated deterministic binary files, initialized World `v1`, ran unchanged `status`, created one Layer from World, and completed `fsck`. Times below are single observations, not percentiles.

| Fixture | Init | Unchanged status | Layer create | `fsck` | Objects checked | Integrity |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1,000 files / 50 MiB | 0.839 s | 10.2 ms | 0.388 s | 0.332 s | 1,003 | valid |
| 25,000 files / 1 GiB | 12.366 s | 110.6 ms | 9.130 s | 11.827 s | 25,003 | valid |
| 100,000 files / 5 GiB | 61.243 s | 444.7 ms | 44.701 s | 68.554 s | 100,003 | valid |

Commands:

```sh
JAVELIN_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  bun run scripts/benchmark-fixtures.ts small
JAVELIN_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  bun run scripts/benchmark-fixtures.ts medium
JAVELIN_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  bun run scripts/benchmark-fixtures.ts large
```

The durable view observation makes unchanged `status` proportional to metadata inspection instead of content hashing. A changed metadata stamp still forces full reconciliation, which is covered by a fault-injection regression test.

### Durable-ingest correction

The original packaged build flushed each compressed temporary object before checking whether its content ID already existed, then synchronized each new object's shard directory separately. The scanner now hashes first, skips existing and same-scan duplicate blobs, stages only new objects, synchronizes staged files concurrently, installs the batch, and synchronizes each touched shard once. New shard creation also synchronizes the object root.

SQLite was not committing once per scanned file: normal scan and Checkpoint paths register one root Tree object, while Checkpoint state is already written in one transaction. Replacing SQLite would not have addressed the measured scaling fault.

| Fixture | Original init | Corrected init | Speedup |
| --- | ---: | ---: | ---: |
| 1,000 files / 50 MiB | 10.484 s | 0.839 s | 12.5x |
| 25,000 files / 1 GiB | 273.027 s | 12.366 s | 22.1x |
| 100,000 files / 5 GiB | 1,254.604 s | 61.243 s | 20.5x |

The supplied `/tmp/jvl-perf/loop.sh` also improved from 10.29 seconds to a 0.63-second median across three final-build runs for 1,000 random 4 KiB files. Touching one file and explicitly checkpointing that 1,000-file World completed in 80 ms.

## 100 concurrent Layers

The final packaged-binary stress fixture created 100 Layers, changed four files in each Layer, checkpointed and refreshed concurrently, then Published all work. It completed in 17.83 seconds with World `v101`, no lost accepted changes, and no integrity failure.

| Operation | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: |
| Layer create | 18 ms | 33 ms | 44 ms | 135 ms |
| Concurrent checkpoint | 1,385 ms | 2,575 ms | 2,649 ms | 2,682 ms |
| Concurrent refresh | 91 ms | 131 ms | 146 ms | 161 ms |
| Concurrent Publish | 3,336 ms | 8,981 ms | 9,589 ms | 9,737 ms |

Command:

```sh
JAVELIN_STRESS_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  cargo test --locked --test stress_100_layers -- --nocapture
```

Publish is the bottleneck because final admission is intentionally serialized per target. These measurements do not justify replacing SQLite or the filesystem object store.

## Event and provenance fixtures

| Fixture | Result |
| --- | --- |
| 10,000 filesystem writes | 0.741 s to write; 15.212 s to reconcile/checkpoint; 4 coalesced Javelin events |
| 1 GiB provenance attachment | 999.6 ms; stored object size 1,073,741,824 bytes |

Commands:

```sh
JAVELIN_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  bun run scripts/benchmark-fixtures.ts events
JAVELIN_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  bun run scripts/benchmark-fixtures.ts traces
```

The event fixture intentionally coalesces a write storm into a small number of stable Checkpoints rather than creating one event per write. The provenance attachment path stores one immutable object and records its metadata; this test does not measure search or redaction throughput.

## Interpretation

Javelin does not claim constant-time initialization, reconciliation, `fsck`, or Layer creation. First-time work scales with tracked path count and bytes. APFS clone-on-write reduces copied physical data, but directory traversal still scales with path count. The measured unchanged-status path is fast because the persisted observation proves the view metadata stamp and Layer head have not changed.
