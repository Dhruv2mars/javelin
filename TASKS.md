# Javelin build checklist

This file is the central hub's master checklist. A checked item requires evidence.
Implementation threads own one PR each and keep their own checklist in their worktree.

## Definition of done

Javelin reaches GA when a developer can install a released build, create a project,
use ordinary tools in isolated Layers, recover history, and collaborate through
hosted Publish Requests. The supported platforms, recovery behavior, concurrency,
and installation paths must have reproducible verification. Public documentation
must describe the actual product and its limits. No runtime, storage, test, or user
workflow may invoke or interpret Git.

## Bootstrap the build

- [x] Recover the accepted design from the final and earlier grilling threads.
- [x] Confirm that earlier code may be replaced rather than preserved.
- [x] Open and link the hub's draft planning PR: [#3](https://github.com/Dhruv2mars/javelin/pull/3).
- [x] Record the accepted product contract and milestone verification gates in `docs/build-contract.md`.
- [x] Persist the latest working instructions in `AGENTS.md` and `RTK.md`.
- [ ] Launch one local-core thread and one filesystem-verification thread.
- [ ] Record their thread, branch, PR, and evidence links here.
- [ ] Arrange persistent hub continuation without duplicating active work.
- [ ] Independently review this plan against the accepted design.

## Prove the local VCS

- [ ] Define native objects, World versions, Layers, checkpoints, and atomic Publish.
- [ ] Implement initialization, durable immutable snapshots, history, and restore.
- [ ] Prove ordinary filesystem behavior for sparse isolated Layers.
- [ ] Implement Layer creation, cloning, checkpointing, discard, status, and diff.
- [ ] Implement Stack, refresh, structural conflicts, and explicit resolution.
- [ ] Bind supplied agent traces to immutable work, with missing-trace override.
- [ ] Prove process concurrency, crash recovery, retries, and corruption detection.
- [ ] Build a real sample application using external agents and Javelin exclusively.
- [ ] Verify the complete workflow on each supported operating system.

## Prove hosting and collaboration

- [ ] Define the versioned provider-independent repository protocol.
- [ ] Implement repository authority, immutable transfers, and idempotent Publish.
- [ ] Provision one repository on the first provider with a simple creation flow.
- [ ] Implement identity, scoped credentials, applied permissions, and revocation.
- [ ] Implement author-owned immutable Publish Request revisions and exact approvals.
- [ ] Implement browser review, comments, checks, direct Publish, and reviewed Publish.
- [ ] Implement clone and independent forks with permitted inherited history.
- [ ] Verify public upstream contribution without requiring a hosted fork.
- [ ] Verify control-plane outage behavior and provider-outage local work.
- [ ] Resolve CI execution design and verify the accepted workflow syntax.
- [ ] Measure hot-repository contention, many repositories, transfer costs, and quotas.

## Deliver OSS GA

- [ ] Verify installation and upgrades from clean machines on supported platforms.
- [ ] Publish tutorials, command reference, hosting setup, and recovery documentation.
- [ ] Add contribution guidance, support policy, security reporting, and license checks.
- [ ] Complete independent security and correctness review, fixing material findings.
- [ ] Set measured release limits and performance requirements before claiming scale.
- [ ] Run the full local and hosted acceptance suite against release artifacts.
- [ ] Publish versioned packages and release artifacts with checksums and provenance.
- [ ] Verify the public release as a new developer and record GA evidence.

## Current work

The hub owns branch `chore/ga-build-plan` and [PR #3](https://github.com/Dhruv2mars/javelin/pull/3).
The initial implementation workstreams will start from this branch's recorded contract.
PRs are never merged without explicit user instruction.

## Open engineering questions

- Native filesystem backends and their host prerequisites require direct verification.
- Storage layout and provider partitioning remain implementation choices.
- Numerical performance claims and adequate provider free-tier capacity are unproven.
- CI execution, release credentials, and infrastructure access remain to be established.

## Sources

- Final design: `codex://threads/01a12eae-a899-7ad3-9462-5f95a64e76cc`.
- Earlier grilling: `codex://threads/01a084f7-7b5f-79d1-b381-d686c430a7f9`.
- Hub: `t3-thread://v1/e659350e-23d8-4473-98e8-d3609b784340`.
- Build authorization and thread rules: the user's messages in this hub on 2026-10-13.

## Workstream: native Layer checkpoints

Owns one draft PR targeting `chore/native-cross-platform-ci`. Done is the bounded
API/process outcome in `docs/layer-checkpoints.md`, including falsifiable sparse
sharing, recovery, concurrency and corruption evidence. Filesystem binding and
automatic capture remain required follow-on work. Do not change probes/workflows.

- [x] Read instructions, latest hub contract, accepted core and source ownership.
- [x] Ground the store and compare complete sparse deltas with checkpoint chains.
- [x] Create planning commit and immediately register the single draft PR [#8](https://github.com/Dhruv2mars/javelin/pull/8).
- [x] Implement typed Layer bases, sparse checkpoints, clone, head updates and discard.
- [x] Add core stale-manifest/outcome regressions and Layer behavioral/process checks.
- [x] Run all foundational tests, formatting, Clippy and release build locally.
- [x] Inspect our exact source's inherited three-OS CI and retained logs.
- [x] Report exact source, artifacts, verdict and follow-on work to the hub.

Original native API/process checks passed 43 tests, one ignored actor, on Darwin arm64
Rust 1.93.0. The documented caller also ran against the production release library
and release CLI integrity inspection. Sparse sharing and limits are recorded in
`docs/layer-checkpoints.md`. Exact source `bd16d5224fa6dfce01509ebf6c28fb8b92dde1a3`
passed native CI run [38465628999](https://github.com/Dhruv2mars/javelin/actions/runs/38465628999).
Downloaded and inspected all three six-file log artifacts: Linux/macOS 43 tests,
Windows 41 tests, one ignored actor each, all 14 process checks on every runner.
The accepted PR6 base is `0c8582ddcb175d7bd4d2752e73f795651f810a6f`.
Compiled native scope is verified, but hub dependency acceptance is pending the
same-PR evidence repair below. Filesystem binding/autosave remain follow-on.

- [x] Independently prove canonical delta normalization and byte encoding guards.
- [x] Cover valid-graph binding tamper, stale-only recovery, exact-path deletion and opaque IDs.
- [ ] Restore each mutation, run corrected local/hosted checks and report exact source.

Corrected code/tests `0a75b1a26e4ca3fbdf7f93a5ae7162c2e0f73b61` pass 52 local
tests in debug/release, one ignored actor, plus fmt/Clippy/release build and the
production-library example. Independent normalization/byte/candidate-base/discard
guard removals fail the new targeted tests. The base scan overlaps accepted-attempt
binding; its single removal retains rejection, while removal of both fails the test.
Actual-source hosted evidence is pending. Format/workflow/probes remain unchanged.

## Workstream: native repository foundation

Owns one draft PR based on `chore/ga-build-plan`. Done means a fresh non-Git
project initializes an immutable World; exact object and manifest reads reject
missing/corrupt data; ordered history and compare-and-advance survive process
restart, concurrent winners/losers, retries, and interrupted publication. CLI
init/history/integrity checks must produce useful ordinary and JSON output.
This work does not claim Layers, filesystem backends, or complete Layer Publish.

- [x] Read local instructions and accepted build contract.
- [x] Open and immediately register the single draft PR: [#4](https://github.com/Dhruv2mars/javelin/pull/4).
- [x] Compare two storage/type sketches with read-only design subagents and cross-judge.
- [x] Capture smallest runnable scaffold baseline, commit `7ec2e75`.
- [x] Record chosen storage design and reconciliation in `docs/native-repository.md`.
- [x] Implement typed immutable objects, manifests, and repository initialization.
- [x] Implement ordered history and atomic idempotent compare-and-advance.
- [x] Implement minimal CLI init, history, and integrity inspection.
- [x] Prove exact binary data, path boundaries, missing/corrupt objects on macOS.
- [x] Prove process restart, contention, retries, and failure boundaries on macOS.
- [x] Independently inspect behavior and reconcile documentation.
- [x] Report commands, results, limitations, and VERIFIED status to hub, message `native-foundation-final-evidence-v1`.

Evidence is recorded in `docs/native-repository.md`. These checks apply only to
this bounded foundation on Darwin arm64. Global GA items above remain unchanged.
Linux/Windows behavior, hardware power-loss durability, Layer adoption, and the
complete Layer Publish workflow remain unconfirmed follow-on work. PR #4 stays
draft and unmerged, depending on the hub planning branch.

## Hub acceptance corrections

The hub found missing-tail metadata validation after the initial report. Done for
this correction requires regression failures before the fix, corruption errors from
history/init/advance after it, and a passing full suite at the corrected SHA.

- [x] Reject missing tail Worlds with retained accepted or stale operation anchors.
- [x] Resolve explicit CLI directories without evaluating a deleted current directory.
- [x] Preserve incidental files on valid-store init and classify empty interrupted init.
- [x] Document undetectable consistent truncation and integrity inspection schema scope.
- [x] Run final checks and report exact corrected SHA within PR #4, message `native-acceptance-corrections-final-v1`.

The corrected suite passes 26 tests, one ignored child-process entry point.
Earlier completion evidence is superseded by the hub acceptance corrections;
the corrected source and limits are documented in `docs/native-repository.md`.

## Workstream: native cross-platform CI

Owns only `.github/workflows/native.yml`, `docs/cross-platform-verification.md`, and
this progress section. The single draft PR targets `feat/native-repository-foundation`
at scaffold baseline `7ec2e75`. It cannot land independently before its base.
Done means committed formatting, Clippy, native tests, and release-build jobs
actually run on macOS, Linux, and Windows hosted runners, with retained failure
logs and linked evidence. Baseline success does not verify future native core,
sparse filesystem backends, installation, or GA. After the hub confirms native
core completion, rebase this same PR and repeat the three-OS implementation gate.

- [x] Read worktree instructions, TASKS, and the current hub contract at `8db2a5b`.
- [x] Create the planning commit and open and register exactly one draft PR.
- [x] Confirm runner labels, action versions, and Rust commands in primary docs.
- [x] Add a bounded three-OS workflow with narrow permissions and failure evidence.
- [x] Check repository Actions availability and push the committed baseline gate.
- [x] Inspect real hosted runner results and record exact scope and proof links.
- [x] Report PR, results, dependencies, and blockers to the hub.
- [x] After hub confirmation, rebase onto native core and verify all three OSes.
- [x] Rebase onto hub-reviewed core `c78ea7a5d3eb9555625391e2f7346859d70f4c2a`.
- [x] Inspect actual core job results and downloaded artifacts on all three OSes.

Draft PR: https://github.com/Dhruv2mars/javelin/pull/6, registered with T3 Code.
Actions API reads confirm public repository, enabled Actions, and read-only token
defaults. Local Rust `1.99.0` formatting, Clippy, scaffold test, and release build
passed. Hosted scaffold setup is VERIFIED at `fca6f75` by successful Linux, macOS,
and Windows jobs in https://github.com/Dhruv2mars/javelin/actions/runs/38427608862.
All check steps and artifact uploads passed. Downloaded all three artifacts and
confirmed all six logs, exact commit, and one passing scaffold test per runner.
Evidence and artifact expiry are recorded in `docs/cross-platform-verification.md`.
That historical scaffold run did not verify native core. Sparse backends, GA,
and Javelin product CI remain NOT VERIFIED.
Hub sent `native-core-complete c78ea7a5d3eb9555625391e2f7346859d70f4c2a`.
Rebased onto that exact commit. The TASKS conflict was resolved by preserving the
entire reviewed base and appending only this workstream section. Source, tests,
Cargo files, and all other native-owned files match the reviewed core exactly.
Actual compiled native suite is VERIFIED at tested PR head
`bfd2a51d75430a419272549b0e209bda6badf9d1` on exact reviewed core
`c78ea7a5d3eb9555625391e2f7346859d70f4c2a`, run
https://github.com/Dhruv2mars/javelin/actions/runs/38462957668.
All formatting, lint, tests, release and log-upload steps passed. Downloaded and
inspected six logs per runner. Linux/macOS passed 26 tests with one ignored actor;
Windows passed 24 with one ignored actor. Windows excludes two Unix-only tests,
so symlink rejection and deleted-cwd explicit-path coverage remain NOT VERIFIED
there. Non-Unix directory sync, power-loss durability, performance and GA remain
unverified. No source changes were made.

## Workstream: mechanical merge engine

Owner: T3 implementation thread commissioned by hub
`e659350e-23d8-4473-98e8-d3609b784340`. Branch `feat/mechanical-merge-engine`,
draft PR against `feat/native-layer-checkpoints`. Source baseline is exact
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, the hub-accepted bounded Layer state
dependency. That dependency is unmerged; this PR depends on it and cannot claim
independent acceptance.

Owned paths: `crates/javelin-merge/**`, `docs/mechanical-merge.md`,
`.github/workflows/mechanical-merge.yml`, and this section only.

Done means: an independently compiled `crates/javelin-merge` library (own
`Cargo.lock`, empty `[workspace]`, path dependency on the root crate) performs a
pure deterministic three-way merge of native `Manifest`s through a caller-supplied
content loader. It returns retained `FileEntry` references, new merged bytes plus
executable bit for the caller to persist with `Repository::put_blob`, and explicit
deterministic conflict records. It never writes files, mints object IDs, persists
Stack state, invents semantic resolutions, or emits textual conflict markers.
A real caller example and integration tests use actual repository-produced
manifests and blobs. Adversarial tests cover the cases below, and the new workflow
passes on hosted Linux, macOS, and Windows with downloaded logs inspected.

- [x] Planning commit, draft PR, T3 registration, hub report.
- [x] Ground the design and compare at least two candidate shapes; record the choice.
  Candidates A (diffy), B (similar plus bespoke diff3), and C (bespoke) are compared in
  `docs/mechanical-merge.md`. The delegated Codex architecture review was cancelled
  before it produced findings, so the comparison is owner-only.
- [x] Compare a mature Rust merge crate with a bespoke line merge from primary docs.
- [x] Implement manifest-level merge: retain, add, delete, modify, convergent.
- [x] Implement within-file line merge with CRLF and missing-final-newline safety.
- [x] Refuse binary content merge; report symlink, type, and executable conflicts.
- [x] Report file/directory-prefix structural conflicts.
- [x] Real caller example and integration tests over actual repository objects.
- [x] Adversarial regressions that fail when guards are removed.
- [x] Independent read-only review; record findings and limits.
- [ ] Follow-up review of the diff3 fix against prior findings.
- [x] Three-OS evidence at the fixed head `b725a69`, with downloaded logs inspected.
- [x] Three-OS workflow evidence with inspected artifacts.
- [x] Document the future gated adapter to SparseChanges, refresh, and Publish.
- [x] Owner evidence handoff with VERIFIED, NOT VERIFIED, and INCONCLUSIVE scopes.
- [ ] Hub acceptance after its fresh Codex round-5 review.

Draft PR: https://github.com/Dhruv2mars/javelin/pull/12, linked in T3. Planning SHA
`63929936581a72d71e4b53b447b6b544b54138da`.

Evidence so far, at tested head `a4275025eec11c8c3a58e60d178924b60902cae0`:
run https://github.com/Dhruv2mars/javelin/actions/runs/38505396039 (pull_request)
passed on ubuntu-24.04 (X64), macos-15 (ARM64), and windows-2025 (X64) with Rust
1.99.0. Downloaded all three artifacts to `/tmp/javelin-merge-ci-38505396039`.
Each runner logged 16 passing tests, a release example printing the same merged
manifest `f0d748da511a8540b64683e5855752dd4cb8f3b0c2a715b95523211c7e5c7d97` and one
`Content` conflict, and 9/9 guard mutations killed. Linux and macOS source hashes
equal the committed bytes. Windows hashes equal the committed bytes after CRLF
checkout conversion, verified locally for all six files.

Independent review (native Claude Opus 5.5 subagent, read-only, at `d6d3079`) found
F1 HIGH: diffy produced wrong clean merges with repeated lines, an invented
line, and a dropped deletion. It also found F2 MEDIUM, doc claims false under F1,
and F3 LOW, conflicted paths not naming collisions. All three are fixed in
`b53e41f` and the following doc commit. diffy was replaced by diff3 anchor
chunking over `similar`. Two exact regressions and a 20,000-case property test
were added, and the mutation guards went from nine to twelve. The reviewer's
`git merge-file` comparison was a /tmp review aid only. No product test invokes Git.
The `a4275025` CI evidence above predates this fix and is superseded by the
rerun at the fixed head.

Hosted evidence for the round-1 fix at `21a7dc7`: run
https://github.com/Dhruv2mars/javelin/actions/runs/38520379743 passed on all three
OSes. Artifacts in `/tmp/javelin-merge-ci-38520379743` show 19 tests, 12/12
mutations killed, the same merged manifest, and source hashes matching
the commit (Windows after CRLF conversion). It is superseded by the round-2 fix.

Round-2 review (native Claude Opus 5.5 subagent at `21a7dc7`): N1 HIGH, a clean
merge dropping one side's `}` through whole-region convergence, still occurred.
N2 MEDIUM: the doc's "sound for any matching" claim was false. N3 MEDIUM: the
property test was count-only and could not generate identical insertions.
N4 LOW: the doc wrongly blamed compaction alone. N5 LOW: a missing anchor
equality assertion. Fixed in `4dd0e6d` and the following doc commit:
- convergence inside files was removed;
- an exhaustive minimal-diff (LCS) oracle with content and order checks was added;
- the N1 regressions were added;
- a `debug_assert` now checks anchor equality;
- the doc was corrected;
- a reversal-agreement check was tried, then removed because no test detects it
  under the minimal-diff spec;
- mutations rose to 13, including convergence and region reordering.

Hosted evidence for the round-2 fix at `45dfa01`: run
https://github.com/Dhruv2mars/javelin/actions/runs/38524136786 passed on all three
OSes. It showed 21 tests, 13/13 mutations, and seven source hashes matching the
commit on each runner. It is superseded by the round-3 fix.

Round-3 review (native Claude Opus 5.5 subagent at `45dfa01`):
- R3-1 HIGH: a shared fix inside a run of identical lines was applied twice. Two
  docs lines were contradicted by this. The removed reversal check would have
  caught part of it.
- R3-2 LOW: the doc misdescribed the alphabet.
- R3-3 LOW: the guarantee wording was inexact.

Fixed in `865be13` and the following doc commit:
- changes from different sides now merge only across an anchor line unique in
  base, ours, and theirs;
- the oracle now requires every pair of minimal diffs to be independent and to
  produce the result;
- the count test no longer tolerates duplicate deletions;
- three R3-1 regressions were added;
- mutants must compile, and 15/15 are killed.

A local 200,000-case run with up to seven-line bases and three edits per side gave
29,312 clean results and no oracle violation. It ran in a /tmp copy and is not CI.

Round-4 review (native Claude Opus 5.5 subagent at `2ca73dd`): R4-1 MEDIUM, a
block swap left a unique anchor unmatched in an equally minimal diff, so clean
results could depend on the matching. R4-2 LOW, heuristic Myers might not be
minimal on large inputs, unconfirmed. It also confirmed a realistic Rust file
merges cleanly in four far-apart edit combinations. Fixed in the following
commits: a separator must also be kept by every minimal diff of both sides
(LCS shrinks without it); diffs use exact `RawMyers`; the swap regression was
added; 16/16 mutants compile and are killed. A local 200,000-case run with
block rotations and a six-symbol alphabet found no violation in 103,017 clean
results. An owner-run realistic Rust check merged three far-apart combinations.

Review limits: the delegated Codex implementation review r1 failed on a provider
usage limit. The OpenCode GPT-6.1 Sol r2 failed with insufficient account funds.
The Claude Sonnet 5.5 r3 hit the API rate limit. None produced findings.

Hosted evidence for the round-4 fix at exact tested head
`b725a6961d74e850c8f64d9f8e062d5b3b378b40`: run
https://github.com/Dhruv2mars/javelin/actions/runs/38526975921, attempt 1,
succeeded on ubuntu-24.04 X64, macos-15 ARM64, and windows-2025 X64. Jobs
115729719267, 115729718692, and 115729720210 respectively passed formatting,
Clippy, 22 tests, the release caller, and 16/16 compiling mutations killed.
Artifacts 11875802586, 11875664982, and 11875789437 respectively were downloaded
to `/tmp/javelin-merge-ci-38526975921-codex` and inspected. All seven source hashes
match the commit, accounting for Windows CRLF checkout. Each release example
stored manifest `f0d748da511a8540b64683e5855752dd4cb8f3b0c2a715b95523211c7e5c7d97`
and reported `src/main.rs` as a `Content` conflict in the conflicting case.
See `docs/mechanical-merge.md` for job links, commands, and the retained limits.

Exact component tree `bd5a374a4810570b190079a59be193f1d94274bd` is unchanged
between implementation `81cadc9225399bc70b6fe8c0bb32e2d7318e733b`, review source
`e0a5f5f5c2ba4952c606191d2b9c681137d9a2de`, and tested head `b725a69`.
This handoff only updates this section and the owned doc. No engine, test,
caller, workflow, or dependency changes. Native run 38526975843 also reports
success on all three OSes at `b725a69`; its artifacts were not inspected here.

Review status: the owner's prior round-5 failed without findings. Per user
routing reset, the hub launched a fresh independent Codex round-5 on the same
source. No replacement review was launched by this thread. Follow-up review and
hub acceptance remain unchecked. The sole linked PR is still draft #12, and
its T3 watcher is released. No merge or scheduler.

Cost-label correction: the retained `/tmp/jm-perf/examples/perf.rs` uses
`i % 10 == 3` for ours and `i % 10 == 7` for theirs. Each side changes 10% of
lines, 20% combined. The old label said 20% each. Only that label is corrected;
the measured 2.75 s and full-memory, unbounded exact-Myers and separator costs
remain. The hub was notified before the documentation correction.

VERIFIED: bounded library and native-repository caller at the tested source,
22 tests and 16 guard mutations on the three hosted OS and architecture pairs.
NOT VERIFIED: persisted Stack, refresh, or whole-Layer adapter; other
architectures; arbitrary large-file resource bounds; acceptance of unmerged
native source baseline `e56fc86`.
INCONCLUSIVE: general correctness outside the small oracle inputs and fresh
independent review, which remains hub-owned and pending. No GA claim.
