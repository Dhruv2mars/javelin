# Native storage baseline measurement

A rerunnable release tool, `tools/native-benchmarks`, drives only the public
`javelin` Repository and Layer APIs with fixed-seed content and records raw samples,
fixture inventory, and host/source provenance. This document states what was run,
what it shows, and what it does not show. It sets no requirement.

## Scope, source and dependency

- Contract: `docs/build-contract.md` Gate 6 (record fixtures, machines, backend,
  limits, errors, cost assumptions; set requirements from measured behavior; do not
  equate API throughput with filesystem throughput).
- The retained measurements use native source
  `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, with
  `javelin_source_sha256 = 590fc27e71a59c0018e36a3185b57d12dbc73e28959265b218147d72ef8453e6`
  over `src/**` plus the root `Cargo.toml`. Each original run retains its source
  label and hash. PR #16 now targets `main`; main integration changed native code
  and root Cargo files. A fresh build at the current head embeds the integrated
  source hash. The retained numbers do not measure that current source; rerun the
  workload before comparing its performance.
- Cost assumptions: none are claimed. Hosting, provider and free-tier costs are
  unmeasured.

## Tool

```
cd tools/native-benchmarks
cargo build --release --locked
./target/release/native-bench run --profile ci --seed 1 --out DIR \
    [--workdir DIR] [--budget-secs N] [--source-commit SHA] [--keep-repos]
./target/release/native-bench verify DIR
```

Profiles `smoke` (tests), `ci` (hosted-runner size), `local` (laptop) and `stress`
(heavier contention; defined but never run, so unmeasured). Size
overrides exist; every value has a hard cap and a derived cap on the estimated bytes
a write may re-read (`estimated_validation_bytes` ≤ 32 GiB). `compare_and_advance`
validates history inside its IMMEDIATE transaction, and Layer mutations validate
stored Layer graphs. These growing traversals can make naive fixture caps quadratic.
`put_blob` does not validate history. The estimate is read from source, not measured.
A non-release build is
refused unless `--allow-debug-build`, and `verify` fails debug evidence the same way.

Own `Cargo.lock`, empty `[workspace]`, path dependency `../..`. `build.rs` embeds
SHA-256 of the measured sources and the tool, the target, and the compiling `rustc`.
New host records match the mountpoint reported by `df` to the mount table, decoding
its escaped paths. Older records used device-only matching and may name another
mount when one device has multiple mounts. Those recorded values are unchanged.
The tool never invokes Git (a regression test puts a failing `git` shim on `PATH`);
`--source-commit` is caller-supplied and unverified. Fixtures take ObjectIds only from
`put_blob`/`put_manifest`; no core hashing or manifest decoding is copied.

### Candidates considered

1. Criterion / `cargo bench`: rejected, because multi-process contention, typed
   outcomes and our raw-record schema do not fit it.
2. Driving the `javelin` CLI from a shell script: rejected, because the CLI exposes no
   Layer operations and spawn cost would be mixed into samples.
3. **Standalone release driver (selected).** Each scenario runs in its own process,
   so peak memory is per scenario; contention spawns real worker processes.

### Scenarios

- **ingest**: seeded text/source/binary files (size classes in
  `fixture-inventory.json`), `put_blob`, then an identical re-put.
- **history**: H versions of F files, each changing k files, through `put_manifest` and
  `compare_and_advance`; `open`/`history`/`version`/`read_manifest`/`check` at
  versions 1, H/4, H/2 and H (1, 7, 15, 30) on three repetitions (warm OS cache).
- **layers**: create, six growing sparse captures with diff/state/checkpoint reads, one
  capture holding a 1 MiB blob, 24 clones of that checkpoint, diffs, three discards.
- **contention**: P worker processes (own handles, wall-clock barrier) each doing
  K `compare_and_advance` from the head they just read plus `put_blob` of shared and
  unique bytes. A pre-barrier prefilled World and uncontended baseline advances give
  the single-process cost at the same state. An uncertain `storage_error`/`io_error`
  triggers a recorded reopen and a retry of the *same* operation ID and input (max 3);
  stale is final and the next operation uses a new ID. Operations or blob puts still
  unresolved after a worker finishes are resolved by the parent with the same input.

### What `verify` checks (from raw files alone)

Summary equals recomputation from raw samples; both ingest passes contain every
configured file index and matching successful inputs/object IDs; history contains
exactly the configured number of advances; provenance and release build;
clean scenario exits; no errors outside contention; contention errors only
BUSY/LOCKED by SQLite code; contiguous attempt numbers, identical input per attempt,
retries only after each action's uncertain error and successful reopen in the same
worker/PID and recorded order, including setup writes and head reads; worker/index
coverage and setup objects bind the final World inputs; every operation and blob
has a durable outcome; one accepted winner per contiguous version with history matching
every acknowledged accept and no unexplained entry; stale `actual` greater than
`expected`, bounded by the final head and consistent with real-time order; first
attempts used the head just read; `check()` versions/attempts/objects match the
records; replaying every operation ID returns the identical outcome and leaves
repository files byte-identical, with any digest directory/entry/file read error
terminating the scenario rather than supplying empty bytes; same ID with a different
manifest is rejected;
acknowledged blobs read back byte-equal by identity; workers completed all operations;
no budget truncation; no mismatch samples; clone heads start at the source checkpoint
and the layer count matches; failed disk traversal terminates the scenario with
`disk_probe_failed`, so its disk evidence is inconclusive; initialized repositories
must have bytes and files, and clone stored bytes/files cannot shrink; clone storage
is not payload-proportional (skipped when
the payload is too small to tell). Tests tamper with each of these and require the
named check to fail. Fixed-seed reproducibility is tested too: the ingest API
object digest `ce53b3e27112fdd4…` is identical on all three hosts below.

## Evidence

Original measured tool source: `4149da97368bca3f68d0773d40bc801bfbced4b3`, native source
`e56fc86`. The Codex r2 corrections change the verifier, disk error propagation and
retry recording, not the fixtures or native source. Original run files and source
labels are unchanged. Old setup samples identify input by worker/index/seed and the
fixed source generator, so their exact setup bytes are inferred from that source.
New runs explicitly set `record_format_version: 2` in `run.json` and require the
payload and public manifest entries in `content`. Version 1 is the original format;
its files and summaries are not converted. Parent resolution now records its reopen. Neither source nor
raw values are retroactively relabeled. Evidence is under
`tools/native-benchmarks/evidence/`; rerun `native-bench verify` on any run directory.

| Source | Machine | Runs |
| --- | --- | --- |
| `macos-4149da9/` | Apple M1 Mac mini (Macmini9,1), 8 CPUs, 16 GiB, macOS 26.6.2, APFS (`/System/Volumes/Data`), `rustc 1.93.0` | 3 × `ci`, 1 × `local` |
| `hosted-run-38507388389/ubuntu-24.04/` | hosted runner, AMD EPYC 9V45, 4 CPUs, 15.6 GiB, Linux 6.17 azure, ext4 per the mount entry (`stat -f` reports `ext2/ext3`), mounted `nobarrier,data=writeback,journal_async_commit,commit=30`, `rustc` 1.99.0 | 3 × `ci` |
| `hosted-run-38507388389/macos-15/` | hosted runner, Apple M1 (Virtual), 3 CPUs, 7 GiB, macOS 15.7.9, `rustc` 1.99.0 | 3 × `ci` (4 processes on 3 CPUs) |

Hosted run: https://github.com/Dhruv2mars/javelin/actions/runs/38507388389 (artifacts
expire 14 days after 2026-10-14; the trimmed raw files are committed). Its Linux and
macOS jobs ran format, clippy, 22 integration + 2 unit tests, a release build, and three
`ci` runs each with `verify`; all passed. The Windows job only compiled and linted the
tool; Windows is **unmeasured** (peak memory and physical disk are Unix-only).

All runs: `verify` printed "all recorded invariants hold", no budget truncation, 0
errors in the 3 × 3 `ci` runs. Counts below are in each run's `summary.json`.

### Medians and tails, in ms (raw samples in `samples.jsonl`)

Percentiles use nearest rank on raw integer nanoseconds. Table values then round to
nearest millisecond, with half milliseconds rounded up; the ingest row retains one
decimal place. Raw samples and summaries retain their original nanosecond values.

| Measure | M1 mini `ci` ×3 | hosted Linux `ci` ×3 | hosted macOS `ci` ×3 |
| --- | --- | --- | --- |
| ingest `put_blob` p50 (n=300) | 12.9, 12.9, 13.0 | 1.0, 1.0, 0.9 | 2.6, 2.1, 2.2 |
| history `compare_and_advance`, p50 of first / last quarter in time (n=30) | 65/383, 46/417, 104/323 | 33/253, 33/243, 34/256 | 55/321, 37/264, 36/342 |
| `clone_layer` p50 / max (n=24) | 714/2089, 603/1541, 581/1248 | 481/615, 494/626, 493/619 | 649/909, 580/836, 686/1062 |
| baseline uncontended advance p50 (n=8) | 28, 32, 26 | 11, 11, 11 | 15, 14, 19 |
| contention accepted p50 / p95 / max | 62/280/770, 33/191/225, 59/144/311 | 21/31/39, 16/30/92, 20/30/46 | 19/32/58, 24/48/89, 18/53/89 |
| contention stale p50 / p95 / max | 132/1049/1779, 81/1708/4799, 84/1217/2042 | 21/194/845, 20/347/746, 21/442/741 | 29/419/1346, 31/441/1094, 87/930/1298 |

Contention counts per run (accepted + stale = 100 operations): M1 47+53, 74+26, 57+43;
Linux 49+51, 54+46, 48+52; macOS 64+36, 59+41, 66+34. Barrier release skew ranged from
nanoseconds (Linux) to 6.5 ms (one M1 `ci` run); hosted macOS reached 3.1 ms (see
`summary.json` `barrier`). The
`local` M1 run (8 processes, 40 operations each, 1000 files): 115 accepted + 205 stale,
accepted p50 73.5 ms, stale p50 220 ms, p95 2335 ms, max 3704 ms; its history advance
p50 of first/last quarter was 140/1299 ms and `clone_layer` p50 1600 ms.

Peak resident memory per scenario parent process: 8.5–13.8 MiB on the M1 (incl.
`local`; `getrusage`, bytes on macOS), 8.2–11.0 MiB hosted Linux (`VmHWM`), 8.8–18.4 MiB
hosted macOS; contention worker processes 4.8–6.2 MiB everywhere.

Disk, `ci` (stored bytes under `.javelin` were identical on Linux and macOS for ingest and
history; the M1 `before_clones` figure differed by 4 KiB in one run). 300 files, 5.27 MB content, 4.63 MB unique payload (270 unique objects) stored as
4.94 MB; re-putting identical content added 0 bytes. After 30 history versions 11.6 MB
stored. 24 clones of a 1 MiB checkpoint added 0 stored bytes (SQLite page granularity:
a row can fit in an existing page, so this is not a per-clone cost). Physical
(allocated blocks) on the M1 was 5.27–5.41 MB for the ingest repository vs 4.94 MB stored;
physical equaled stored on hosted Linux. Object counts in disk rows come from `check()` and
include the 270 unique blobs plus manifest objects.

### Actual busy errors

All nine `ci` runs had zero errors. The `local` M1 run (8 processes) recorded four
real `DatabaseBusy/5` errors, each after about 5.3–5.4 s, matching the documented 5 s
busy timeout: three on `prep_put_blob` and one on `prep_put_manifest`, all in the
unsynchronized pre-barrier setup writes. Each was followed by a recorded reopen and a
retry of the same input that succeeded (retries took 43 ms to 2.6 s; the reopens before
them took up to 1.3 s). The synchronized
operations had no errors there. An earlier, unretained exploratory `local` run with the pre-fix tool build (commit
`42f8078`; its raw samples were lost to a tool bug fixed in `4149da9`; not in this repository,
so unverifiable here) reportedly also showed `disk I/O error` and
`attempt to write a readonly database` storage errors from workers; those were not
reproduced in the retained runs and are **not** evidence either way.

## Observations (measured, not causal)

- History advance latency grows with accumulated state. In every environment the history
  advance is several times slower, and the `history()`/`check()` reads tens of times
  slower, at the end than at the start of the 30-version build (e.g. advance 65→383 ms and
  `history()` 10→379 ms in the first M1 run). The
  `local` run, with about 3.3× the files, shows about 3.1–4.0× the last-quarter advance
  time (1299 ms vs 323–417 ms in the three M1 `ci` runs). `compare_and_advance` validates stored
  history inside its IMMEDIATE transaction. The tool did not profile the growth, so
  its cause remains unconfirmed. `put_blob` does not run this history validation.
  Pooled percentiles across such a series mix states; `summary.json` flags those groups
  (`pooled_across_changing_state`) and gives first/last-quarter medians.
- `clone_layer` p50 of about 0.5–0.7 s for a 1 MiB shared payload is an API-level
  cost on this build; 24 clones took about 14–20 s in total across the three M1 `ci` runs.
  No cause is assumed.
- Single `put_blob` is about 1 ms on hosted Linux but 2–13 ms on macOS. The source sets
  `synchronous=EXTRA` and `fullfsync=ON`, and `fullfsync` only takes effect on macOS,
  so macOS and Linux numbers are not durability-equivalent. The hosted Linux root is also
  mounted `nobarrier`, so fsync there does not force a device cache flush; Linux latencies
  reflect a lower durability bar than macOS. The effective pragmas at
  the measured connection are not readable through the public API.
- Stale outcomes carry a much longer tail than accepted ones under contention (p95
  0.2–2.3 s, max up to 4.8 s; medians are similar on hosted Linux), compatible with waiting
  on the write lock;
  the tool does not separate lock wait from work.

## Limits retained

- Public Repository/Layer API only. This is **not** filesystem throughput, a mounted
  Layer, hosted-service, provider or free-tier capacity, simulation equivalence, or
  scale. No hardware power-loss claim. Sizes are small and fixed (300 files, 30
  versions, 24 clones, 4–8 processes, 100–320 contended operations); these are not GA
  requirements and no timing threshold gates any job.
- Single machine per environment, three repetitions, shared noisy hosted runners; p95
  with n < 20 equals the maximum. OS caches were not dropped; samples are warm.
- Windows, Linux outside the hosted Ubuntu runner, other architectures, other
  filesystems, and corruption-detection cost were not measured.
- Contention sizes were not tuned to reach the 5 s busy timeout in synchronized
  operations; four timeouts occurred only in setup writes at the `local` size. How often
  a real client would see them is unknown.
- Gate 6 hot-repository and many-repository workloads at realistic scale remain future
  work, and the numbers describe `e56fc86` only.

## Recommendations (require later integration, none applied here)

1. Have the native owner add a cheap public head-read: the contention workers had to
   call `history()` (full validation) to learn the head, which cost 12–25 ms p50 on
   macOS and grows with history. Also export effective connection pragmas.
2. Decide whether full-history validation on every World advance is intended; if not, a later
   change must keep immutability and not cache stale history to buy speed. Rerun this
   tool before and after and compare series, not pooled percentiles.
3. Before any Layer capture or clone latency is promised to users, measure on the
   real mounted backend; these numbers cannot stand in for it.
4. Rerun this tool on every native source change and keep the `verify` checks as the
   regression guard for retry, ordering and replay behavior.

## Codex r2 tool validation

The corrected tool source is `ebd30833974b08968271d3466eab0717773a5d00`, still on
native `e56fc86`. Format, clippy, debug and release tests pass. Each test profile
passes 7 unit and 22 CLI tests. The corrected release verifier passes all 18 old
proof directories with 33 checks each; original files and labels are unchanged.
The four previously accepted audit tamper cases now fail at the retry and disk checks.

One fresh release `ci` run is retained at
`tools/native-benchmarks/evidence/codex-r2-validation/fresh-ci-ebd3083/`. It contains
2,616 raw records, 100 contended advances, 48 accepted and 52 stale, and 80 blob puts,
with zero errors. All 33 verifier checks pass. Its source and executable hashes,
M1/16 GiB/APFS host, Rust 1.93.0, exact command and 600-second budget are in `run.json`.
This validates the tool corrections; it does not revise the earlier performance table.

Fresh read-only Codex review of that exact source found no actionable issues and
confirmed the four audit findings are addressed. Review identity and limitations,
matched-summary regressions, local test logs and old-run verification results are in
`evidence/codex-r2-validation/`. Exhausted worker-to-parent resolution is tested
synthetically; actual retry exhaustion was not reproduced.

The owned hosted workflow passed on exact head
`3acc6608199267ba97c950ec1793b1f20609d17f` in
https://github.com/Dhruv2mars/javelin/actions/runs/38569138378. Ubuntu 24.04 and macOS 15
each passed formatting, clippy, debug and release tests, a release build and three
`ci` runs with 33 verifier checks. Each test profile passed 7 unit and 22 CLI tests.
All six downloaded raw runs were independently verified locally, with unchanged
file hashes and zero errors. Each has 100 contended advances and 80 blob puts;
Ubuntu accepted/stale counts were 54/46, 56/44 and 53/47, and macOS counts were
71/29, 61/39 and 62/38. Windows only compiled and linted the benchmark.

Exact source/job/artifact proofs, logs, machine provenance and trimmed raw files are
in `evidence/codex-r2-validation/hosted-run-38569138378/`. Tool and native source hashes
match the reviewed/local-tested source. All 17 retained raw run directories pass.
Later evidence and documentation commits are not new tested-source claims. The
original performance table and its raw source labels are unchanged. All earlier
scope limits, Gate 6 and GA remain open.
