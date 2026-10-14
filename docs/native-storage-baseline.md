# Native storage baseline measurement

## Outcome, sources and dependency

Done means a committed, rerunnable release tool at `tools/native-benchmarks` drives
the real `javelin` public Repository and Layer APIs with reproducible fixed-seed
content, writes raw per-operation samples plus host/source provenance, and a
`verify` command re-checks the recorded outcomes. Actual runs on macOS and hosted
Linux are archived with raw files. Results are a baseline, not a requirement.

Source contract: `/Users/dhruv2mars/dev/github/javelin/docs/build-contract.md`,
read 2026-10-14 (Gate 6: record fixtures, machines, backend, limits, errors, cost
assumptions; set requirements later from measured behavior; do not equate API
throughput with filesystem throughput).

Source under measurement is the **unmerged dependency**
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1` (PR #8 native Layer checkpoint state,
hub-accepted as a dependency only). This PR is draft, stacked on
`feat/native-layer-checkpoints`, and cannot claim independent acceptance. It edits no
native source: only `tools/native-benchmarks/**`, this document, its workflow and
its own TASKS section. PR #10 owns concurrent shared native source edits; results
describe `e56fc86` only and must be rerun after native changes.

## Grounding (public API only)

`Repository::{init,open,put_blob,put_manifest,read_manifest,read_blob,history,
compare_and_advance,check}` and `layer_base/create_layer/clone_layer/capture_layer/
layer_diff/read_checkpoint/layer_state`. Connections use a 5 s SQLite busy timeout,
DELETE journal, `synchronous=EXTRA`, `fullfsync=ON` (`src/repository.rs`), so latency
is expected to include durable syncs; the tool records that setting and does not
assume a cause for any cost. Core object hashing and manifest decoding are private
and are not copied; fixtures use `put_blob`, `Manifest::new`, `put_manifest`.
The existing debug test of one 1 MiB blob with 24 clones is expensive; a release
baseline can attribute wall time per API call without asserting why.

## Two structurally distinct designs

1. **Criterion/`cargo bench` harness.** Statistical in-process timing. Rejected:
   multi-process contention needs separate connections and a start barrier; samples
   and outcomes are hidden from our schema; provenance and typed error capture are
   awkward; extra heavy dependency tree.
2. **Standalone release driver crate (selected).** One binary `native-bench` with
   `run`, hidden `worker` (self re-exec for real multi-process contention),
   and `verify`. Caller API is a command line:

   ```
   native-bench run --profile ci --seed 1 --out DIR [--budget-secs N] [--source-commit SHA]
   native-bench verify DIR
   ```

   Raw JSONL records, `fixture-inventory.json`, `host.json`, `summary.json`.
   Rejected alternative within this shape: drive the `javelin` CLI via shell; it
   lacks Layer commands, adds process-spawn cost and hides typed outcomes.

## Scenarios (bounded, fixed seed)

All sizes are profile constants with hard caps (reject larger): files ≤ 5000, blob
bytes ≤ 64 MiB total per repo, history ≤ 200, clones ≤ 64, processes ≤ 8.

- **ingest/dedup**: `put_blob` of seeded text-like and incompressible binary files,
  then an identical re-put to measure shared-object cost and physical growth.
- **history**: build H versions of F files, each changing k files, via
  `compare_and_advance`; time each advance, then `open`, `history`, `check` as H grows
  (OS cache not dropped; labeled warm).
- **layers**: create, capture a sparse change set, `layer_diff`, `read_checkpoint`,
  `clone_layer` ×N from one checkpoint holding a 1 MiB blob; per-clone physical and
  logical growth.
- **contention**: P real processes, each its own `Repository` handle, a start barrier,
  K `compare_and_advance` attempts from the observed head and `put_blob` (shared and
  unique bytes). Every attempt records operation ID, expected version, outcome
  (accepted/stale/error text), elapsed ns, and retries with the same operation ID and
  input after uncertain errors. Busy timeouts are reported as measured errors, never
  hidden or counted as success.
- **verify**: exactly one accepted result per version ordinal and contiguous ordered
  history; every acknowledged accept present once with that operation ID; stale never
  creates a version; same-ID retry returns the identical outcome; blobs read back
  byte-equal; `check()` passes. Regression tests include tampered-record negatives so
  removing a verifier check fails a test.

## Recorded metadata

Per run: OS/kernel, CPU model, logical CPUs, memory, filesystem type of the work
directory, `rustc -Vv`, `cargo -V`, SHA-256 of the executable and of the tool
`Cargo.lock`, exact command and budget, `--source-commit` supplied by the caller (the
tool never invokes Git), peak RSS from `getrusage` on Unix (unmeasured elsewhere),
wall, p50/p95 by nearest rank with `n` (p95 below n=20 flagged), physical (allocated
blocks) and logical (file sizes plus payload bytes) disk separately, truncation flag
when a budget stopped work. Windows is unmeasured.

## Limits retained

No filesystem-throughput, hosted-provider, free-tier, simulation-equivalence, scale
or power-loss claim; no GA threshold from small fixtures; no CI timing gates. Gate 6
hot-repository and many-repository workloads at full scale remain future work.

## Evidence

Pending. Tested-source and artifact proofs are filled in only after real runs.
