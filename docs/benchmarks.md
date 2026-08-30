# Benchmarks

These results were measured on 30 August 2026 against the packaged `javelin-1.0.0-aarch64-apple-darwin` binary. They are evidence for this build, not universal performance guarantees.

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
| 1,000 files / 50 MiB | 10.484 s | 17.3 ms | 0.411 s | 0.350 s | 1,003 | valid |
| 25,000 files / 1 GiB | 273.027 s | 128.7 ms | 10.943 s | 33.244 s | 25,003 | valid |
| 100,000 files / 5 GiB | 1,254.604 s | 451.3 ms | 45.179 s | 199.190 s | 100,003 | valid |

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

## 100 concurrent Layers

The final packaged-binary stress fixture created 100 Layers, changed four files in each Layer, checkpointed and refreshed concurrently, then Published all work. It completed in 24.29 seconds with World `v101`, no lost accepted changes, and no integrity failure.

| Operation | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: |
| Layer create | 17 ms | 32 ms | 33 ms | 36 ms |
| Concurrent checkpoint | 3,881 ms | 8,501 ms | 8,763 ms | 8,763 ms |
| Concurrent refresh | 100 ms | 138 ms | 140 ms | 141 ms |
| Concurrent Publish | 3,462 ms | 9,411 ms | 10,063 ms | 10,239 ms |

Command:

```sh
JAVELIN_STRESS_BIN=dist/javelin-1.0.0-aarch64-apple-darwin/javelin \
  cargo test --locked --test stress_100_layers -- --nocapture
```

Publish is the bottleneck because final admission is intentionally serialized per target. These measurements do not justify replacing SQLite or the filesystem object store.

## Event and provenance fixtures

| Fixture | Result |
| --- | --- |
| 10,000 filesystem writes | 17.075 s to write; 106.568 s to reconcile/checkpoint; 5 coalesced Javelin events |
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
