# Native validation profiling

This diagnostic measures the unchanged, accepted but unmerged native source
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`. Its starting dependency is
`100395b8d5bdd58efc82b7ffcca6414926a09188`, the evidence-only head of the
corrected benchmark unit. Hub acceptance is recorded at `432bbe6`, after
independent r3 review of tool `ebd3083` and hosted `3acc660`.
PR #22 targets `perf/native-storage-baseline`. It accepts no new native source.
PR #18 remains the sole root writer; whole-Layer Publish depends on its accepted
head. No optimization, schema change, trust change, Gate 6 or GA claim belongs here.
Initial GA still requires macOS, Linux and Windows. Windows is unmeasured here.

## Preregistration

This section is committed before measurements. Later results must retain these
criteria and report incomplete runs instead of dropping slow or failed samples.

Two designs were compared. A standalone public-API Rust driver with an external
profiler leaves native code and the primary optimized executable unchanged.
A counting allocator or tracing build could expose more events, but changes
instrumentation and cannot by itself isolate causes. Choose the standalone
driver. Use separate profiler runs with the same executable and fixture; compare
their operation timings with an unprofiled loop. No instrumentation build may
be presented as production timing. No cumulative Rust allocation counter is
planned. OS CPU and resident memory include Rust, SQLite/C and OS effects.

Independent Codex design feedback, task `native-validation-design-r1-pr22`,
model `gpt-6-sol`, recommends A and the qualifications below. The child completed
without edits or measurements and has no pending work. Its exact report is
retained under `tools/native-profiles/reviews/design-r1.json`.

### Matched fixtures

Fixed seed 1. Every nonempty World references 262,144 payload bytes. The baseline
has depth 16, 32 entries, shared blob IDs and repeated manifests. Cases:

| Case | Depth | Entries | Blob references | Historical manifests |
| --- | ---: | ---: | --- | --- |
| base | 16 | 32 | shared | repeated |
| depth4 | 4 | 32 | shared | repeated |
| depth64 | 64 | 32 | shared | repeated |
| entries8 | 16 | 8 | shared | repeated |
| entries128 | 16 | 128 | shared | repeated |
| distinct-blobs | 16 | 32 | distinct | repeated |
| distinct-manifests | 16 | 32 | shared | distinct |

For each entry count, prepopulate the complete blob pool in index order. Its
unique payload totals 262,144 bytes even when a World references only the first
blob repeatedly. Prepopulate both shared and distinct manifest pools, 64 variants
each, in the same order on every fixture with that entry count. Executable bits
of the first six paths encode the variant. Thus the shared-ID pair has identical
stored object pools and only its referenced blob IDs differ. Prepopulate all
variants on both sides of the manifest-reuse pair and end on variant zero.
Always derive native object IDs through public put APIs.

Depth necessarily changes historical reference count and attempt rows. It keeps
head contents and stored object pool fixed. Entry count at fixed aggregate
payload also changes bytes per entry, from 32 KiB to 2 KiB. Manifest flag variants
have slightly different canonical lengths. Record these facts and exact IDs and
lengths; these are controlled comparisons with these stated limits.

Each repetition uses a fresh fixture per action. Three repetitions rotate case
order deterministically, with one timed observation per case/action/repetition.
Report raw paired ratios and the median across three observations, never an
invented within-repetition median. Actions are public `history` for head
discovery, accepted advance, stale advance and Layer clone. Time initialization,
each blob/manifest put, seeding, and Layer preparation separately. A post-seed
identical `put_blob` is a negative control for history-depth growth.
An advance timer covers validation, lock acquisition and commit together; public
APIs cannot separate those internals. Head discovery is outside that timer.

First run an uncontended accepted advance. Then use a separate equivalent
baseline fixture for two processes, independent handles and distinct operation
IDs. Both read the same expected head before a parent-controlled barrier.
Exactly one accepted and one stale result are required. Retain all errors,
reopens and exact-input retries, at most three attempts. Never turn a stale
outcome into a new-input retry. Replay accepted/stale operations after reopening
and reject operation-ID reuse with changed input. History and check counts must
match actual results, with no unexplained World or attempt.

### Hypotheses and falsifiers

H1: head discovery, accepted/stale advance and clone cost grows with depth while
the post-seed identical blob put does not. Full history/attempt validation is
source-supported; timing alone cannot apportion that work. No consistent positive
depth direction, invalid fixture controls, or comparable blob-put growth
falsifies this diagnostic interpretation.

H2: more entries at fixed aggregate payload raises per-reference overhead in
history/advance. No consistent positive direction falsifies the observed trend;
blob size, SQLite locality and canonical manifest size remain alternate costs.

H3: shared IDs retain substantial per-reference work despite fewer referenced
unique blobs. H4: repeated manifests retain substantial work despite fewer
referenced unique manifests. The source has no deduplication within history
validation. A repeatable greater-than-twofold reuse benefit falsifies the
specific expectation of little timing benefit; locality remains a possible cause.

H5: sampled CPU stacks, when observable, identify validation/hash/SQLite costs
in repeated history and stale replay. Unsupported tools, insufficient symbols,
too few samples or disagreement with operation CPU/wall trends make attribution
INCONCLUSIVE. Heap snapshots or heap profiles must state whether they show
live allocations, cumulative allocations or peak heap; none equals peak RSS.

A median ratio at least 2 and the same direction in all three repetitions is
an exploratory diagnostic signal only. Report smaller effects and disagreements
without a binary performance verdict. These are not GA pass thresholds.
Timeouts or budget stops are INCONCLUSIVE and remain in raw evidence.

### Budgets and environment

No scaling beyond the matrix. Per native child: wall 90 seconds, CPU 60 seconds,
RSS 256 MiB; profile children may use RSS 512 MiB and CPU 60 seconds. Enforce CPU
with unprivileged rlimit, wall with parent timeout, and RSS with child observations
at phase boundaries plus parent polling where available. Polling is not a strict
instantaneous RSS guarantee; report overshoot and polling availability. Linux
also limits address space for ordinary children. Active fixture files cap at
128 MiB; retained fixture/evidence total caps at 1 GiB. Check disk during runs
and stop rather than scale. No more than two native workers concurrently.
Complete measurement run wall cap 10 minutes; hosted job cap 20 minutes.
Record timeout/failure output and do not summarize it as a successful sample.

Use ordinary local Mac temporary files and disposable Ubuntu 24.04 files,
without mounts or privileged laptop changes. Do not wait indefinitely for load.
Record CPU/model, filesystem, load before/after, compiler, complete build flags,
working set, seed, concurrent work, checked-out SHA supplied by developer,
source/binary fingerprints, workflow run/job/artifact IDs and digests.
Prefer installed sample/heap or perf/heaptrack/valgrind. Development binaries
may only be installed in owned scratch or ephemeral runner paths. Unavailable
profilers and blocked hardware counters remain INCONCLUSIVE with raw probe logs.

## Integrity that a future redesign must retain

`read_object` checks the native hash and kind against actual bytes;
`manifest_in` checks canonical encoding, valid paths and every referenced blob.
`history_in` requires contiguous Worlds from an empty World zero, an exact
accepted-attempt anchor for each later version, and valid candidate graphs and
result anchors for every retained attempt, including stale-only candidates.
Missing acknowledged tails fail closed even when only a stale attempt retains
the anchor. Immutable tables reject updates/deletes. Same-ID identical retries
return the original durable outcome; changed input is rejected. Publication
validation happens under one write transaction before any new durable reference.
Layer checkpoint/base graphs and retained immutable head history also remain
validated. These are guarantees to preserve, not candidates for removal.

Source read: `src/repository.rs` read_object/manifest_in/history_in/
compare_and_advance and `src/layer.rs` check_layers/clone_layer.
Regression evidence read: `tests/native_repository.rs`
missing_corrupt_objects_are_rejected_without_advancing,
missing_tail_world_is_corruption_when_any_outcome_retains_its_anchor,
stale_only_candidate_graph_corruption_is_rejected_by_every_history_validator;
`tests/layer_checkpoints.rs`
malformed_or_missing_checkpoint_graphs_and_head_history_never_fall_back.
Kind/existence-only checks, stale caches and undocumented trust changes would
not preserve this contract.

## Evidence status

No measurements yet. CPU/heap attribution, controls, hosted execution and verifier
validation remain unverified. The eventual verifier checks recorded consistency,
digests, coverage, controls and recomputed summaries without modifying input
bytes. It does not authenticate arbitrary fabricated datasets.
