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

Owns only `.github/workflows/**`, `docs/cross-platform-verification.md`, and
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
- [ ] Check repository Actions availability and push the committed baseline gate.
- [ ] Inspect real hosted runner results and record exact scope and proof links.
- [ ] Report PR, results, dependencies, and blockers to the hub.
- [ ] After hub confirmation, rebase onto native core and verify all three OSes.

Draft PR: https://github.com/Dhruv2mars/javelin/pull/6, registered with T3 Code.
Actions API reads confirm public repository, enabled Actions, and read-only token
defaults. Local Rust `1.99.0` formatting, Clippy, scaffold test, and release build
passed. Hosted execution is still INCONCLUSIVE until the three jobs finish.
