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

## Workstream: native storage baseline measurement

Owner thread: independent implementation worker for hub e659350e. Owns only
`tools/native-benchmarks/**`, `docs/native-storage-baseline.md`,
`.github/workflows/native-storage-baseline.yml` and this section. Draft PR #16 targets
`feat/native-layer-checkpoints`. Measured source is the unmerged dependency
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`; this PR cannot claim independent
acceptance. Done: committed rerunnable release tool, raw macOS and hosted Linux runs
with fixture inventory and host/source provenance, `verify` re-checking recorded
outcomes, defect-detecting regressions, honest recommendations. Windows unmeasured.

- [x] Read instructions, contract, TASKS and decisions at `e56fc86`.
- [x] Compare designs and select the standalone driver (see doc).
- [x] Open and register exactly one draft PR (#16); report planning SHA `11fc70c` to hub.
- [x] Independent read-only plan review r1 (claude-opus-5-5): found quadratic
  validation cost, pooled-percentile and retry gaps; all applied.
- [x] Build tool, fixtures, four scenarios, `verify` (30 checks), 24 tests.
- [x] Independent read-only code review r1 (claude-opus-5-5): 15 findings fixed
  (lost raw samples, vacuous checks, debug evidence, workflow evidence upload,
  Linux RSS, overflow); each has a regression.
- [x] Real macOS runs (3 x ci, 1 x local) with raw artifacts, tested source `4149da9`.
- [x] Hosted Linux and macOS runs and Windows compile-only via own workflow, run
  38507388389; downloaded artifacts re-verified locally.
- [x] Write results, limits and recommendations in `docs/native-storage-baseline.md`.
- [x] Final doc review and hub handoff, followed by Codex r2 corrections below.

Windows performance, mounted-Layer cost, hosted-service cost, power-loss durability,
scale and GA thresholds are NOT VERIFIED and not claimed.

### Codex r2 acceptance corrections

Done means the unchanged-tool audit failures are reproduced; each worker retry binds
its initial error, input, attempt order and successful reopen; disk probes fail on
incomplete traversal and shrinking clone evidence fails; original raw data and source
labels remain immutable; debug/release tests, all retained verifiers and one fresh
release `ci` run pass; fresh Codex review and the owned hosted workflow are recorded.
Gate 6 and GA remain open. Same draft PR #16 and owned paths only.

- [x] Read hub Codex r2 audit read-only; reproduce four accepted tampers and release test failure on unchanged `4149da9` tool source.
- [x] Fix worker retry chains and bind final evidence, with focused tamper regressions.
- [x] Propagate disk traversal errors and reject missing, zeroed or shrinking clone evidence.
- [x] Correct actual build-profile test and six doc values; document rounding and precise validation scope.
- [x] Run formatting, clippy, debug/release tests, all 18 old evidence verifiers and one fresh release `ci` run at `ebd3083`.
- [x] Get fresh Codex read-only review of exact `ebd3083` source; no actionable findings. Review task and limits are retained in `evidence/codex-r2-validation/review.json`.
- [x] Run owned hosted workflow at `3acc660`, run 38569138378; both OSes pass debug/release 29 tests each and 3 x `ci` runs. Download and independently verify all 6 artifacts, then all 17 retained runs. Use one T3 watcher, stop it, and report exact source/evidence to hub.

Codex r2 tool validation is VERIFIED within the bounded public-API scope.
Hosted-tested head is `3acc6608199267ba97c950ec1793b1f20609d17f`; reviewed/local-tested
tool source is `ebd30833974b08968271d3466eab0717773a5d00`. Later evidence/docs-only
head preserves native `e56fc86`, fixtures and old raw/source labels. All 17 committed
raw directories and 8 prior downloaded/audit copies verify, 25 proof directories.
The four original P2 tamper accepts now fail by the named retry/reopen and disk
checks. Windows measurement, stress, actual retry exhaustion, growth profiling,
mounted filesystem, power loss, hot/many-repository workloads, Gate 6 and GA remain
NOT VERIFIED. Same one linked draft PR16 remains unmerged; T3 watcher stopped.

## Workstream: native validation profiling

Owns only `tools/native-profiles/**`, `docs/native-validation-profile.md`,
`.github/workflows/native-validation-profile.yml` and this appended section.
Branch `perf/native-validation-profile` targets `perf/native-storage-baseline`.
Starting dependency `100395b8d5bdd58efc82b7ffcca6414926a09188` preserves accepted,
unmerged native `e56fc86`. The user confirms hub r3 acceptance of the corrected
benchmark tool at `ebd3083` and hosted `3acc660`, now recorded by hub
commit `432bbe6`. No independent native acceptance or Gate 6 credit here.

Done means a rerunnable public-API profiling tool, precommitted hypotheses and
matched controls, honest bounded local Mac and disposable Ubuntu evidence,
source/host/artifact identity and raw failures, a tamper-detecting verifier,
independent Codex design and final review, and exact-source fmt/Clippy/tests/CLI
and hosted checks. No core optimization, schema or trust change. Attribution
is INCONCLUSIVE wherever profiling is unavailable. GA requires all three OSes.

- [x] Verify exact dependency, local instructions and current hub contract/trail.
- [x] Open and T3-link exactly one draft PR22 before implementation; report a7cf694 to hub.
- [x] Compare two designs, get completed Codex design-r1 feedback and commit fixture/hypothesis/budget plan before measuring. See docs/native-validation-profile.md.
- [ ] Implement bounded public-API producer and independent evidence verifier.
- [ ] Re-read integrity guarantees and retain exact outcome/history checks.
- [ ] Capture local optimized control and available CPU/heap profiling with overhead labels.
- [ ] Independent Codex final read-only review; fix reproduced findings.
- [ ] Verify fmt, Clippy, tests and real CLI at named source.
- [ ] Run exact-source owned Ubuntu workflow, watch once, retain and verify artifacts.
- [ ] Release watcher and report bounded conclusions and next experiment to hub.
