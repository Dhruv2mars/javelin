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

## Native development packages workstream

- [x] Read `AGENTS.md`, `RTK.md`, root `TASKS.md`, `docs/build-contract.md`, and `decisions.tsv`.
- [x] Open the draft PR, link it to T3 Code, and report PR, planning SHA, and owned paths to the hub.
- [x] Compare archive and installer shapes; select the simplest per-OS caller API (see `docs/native-development-packages.md`).
- [x] Inspect dependencies and license obligations; ship the actual `LICENSE` plus generated third-party notices.
- [x] Build the release CLI from the accepted source with `--locked --release` and no test hooks.
- [x] Package `tar.gz` (Linux, macOS) and `zip` (Windows) with `SHA256` and a manifest naming source SHA, toolchain, dependency hashes, target triple, and development/pre-GA channel.
- [x] Install with checksum verification, owned temporary extraction, and a rename into a fresh path; reject tampered, traversing, absolute, symlink, and duplicate entries.
- [x] Smoke the unpacked binary through `PATH` in a new non-Git project: `init`, `history`, `check` JSON, corruption exit code, preexisting binary bytes preserved.
- [x] Test side-by-side install, upgrade, and rollback of the minimal development binary only.
- [x] Attempt a reproducible build twice with `--remap-path-prefix` and `SOURCE_DATE_EPOCH`; record the outcome without claiming determinism it does not show.
- [x] Run all three OS jobs; download the artifacts on a second job and smoke them with native tools; inspect raw logs.
- [x] Obtain independent read-only review; add defect-detecting regressions; record limits.
- [ ] Hand off the exact tested source, doc-only head, proofs, VERIFIED/NOT VERIFIED/INCONCLUSIVE scopes, and limits to the hub.

Owner: thread `t3-thread://v1/mcp:6ebbed40-380a-4bc8-8330-96e51b8c89e4`.
Branch `build/native-development-packages`, based on exact accepted native Layer
state `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`. That source is accepted by the hub
but unmerged; this PR targets `feat/native-layer-checkpoints` and claims no
independent acceptance. Owned paths: `tools/native-packaging/**`,
`docs/native-development-packages.md`, `.github/workflows/native-packaging.yml`,
and this section. No root Cargo, source, test, or workflow edits.

Done: for one named source commit, hosted Linux, macOS, and Windows jobs build the
release CLI, produce archive plus checksum plus manifest, and a separate job
downloads each artifact, verifies the checksum, extracts into a fresh path, and
runs the `PATH` smoke above Git-free. The result is a development distribution
and clean-temp installation proof, not a public release or Gate 5.

Evidence (see `docs/native-development-packages.md`): tested source
`b0383ff52658618ed840d9b5449eea892fa4f337`, hosted run
https://github.com/Dhruv2mars/javelin/actions/runs/38507762765, six jobs green.
Archives, checksums, native-tool download smoke, and packager install smoke are
VERIFIED on Linux x86_64, macOS arm64, and Windows x86_64. Binary reproducibility is
VERIFIED identical on Linux and macOS and NOT reproducible on Windows. Minimum OS
versions, other architectures, signing, publishing, and Gate 5 remain unverified.
Independent review r1 (gpt-6.1-sol) found six issues, all fixed with regressions; a
follow-up review is recorded in the doc when complete.
