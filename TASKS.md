# Javelin build checklist

This file is the central hub's master checklist. A checked item requires evidence.
Implementation threads own one PR each and keep their own checklist in their worktree.

## Definition of done

Javelin reaches GA when a developer can install a released build, create a project,
use ordinary tools in isolated Layers, recover history, and collaborate through
hosted Publish Requests. The supported platforms, recovery behavior, concurrency,
and installation paths must have reproducible verification on macOS, Linux, and
Windows. Public documentation
must describe the actual product and its limits. No runtime, storage, test, or user
workflow may invoke or interpret Git.

## Bootstrap the build

- [x] Recover the accepted design from the final and earlier grilling threads.
- [x] Confirm that earlier code may be replaced rather than preserved.
- [x] Open and link the hub's draft planning PR: [#3](https://github.com/Dhruv2mars/javelin/pull/3).
- [x] Record the accepted product contract and milestone verification gates in `docs/build-contract.md`.
- [x] Persist the latest working instructions in `AGENTS.md` and `RTK.md`.
- [x] Launch one local-core thread and one filesystem-verification thread.
- [x] Record their thread, branch, PR, and evidence links here.
- [x] Arrange hourly persistent hub continuation without duplicating active work.
- [x] Independently review this plan against the accepted design with `claude-opus-5-5`.
- [x] Strengthen the sparse-backend gate and record platform requirements from review.

## Prove the local VCS

- [ ] Define native objects, World versions, Layers, checkpoints, and atomic Publish.
- [ ] Implement initialization, durable immutable snapshots, history, and restore.
- [ ] Prove ordinary filesystem behavior for sparse isolated Layers.
- [ ] Implement Layer creation, cloning, checkpointing, discard, status, and diff.
- [ ] Adopt existing non-Git project files through a Layer without accepting edits outside it.
- [ ] Implement Stack, refresh, structural conflicts, and explicit resolution.
- [ ] Bind supplied agent traces to immutable work, with missing-trace override.
- [ ] Prove process concurrency, crash recovery, retries, and corruption detection.
- [ ] Build a real sample application using external agents and Javelin exclusively.
- [ ] Verify the complete workflow on each supported operating system.
- [ ] Run native formatting, lint, test, and release-build checks on hosted macOS,
  Linux, and Windows runners, then repeat against the completed native implementation.

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
The initial implementation workstreams are running from this branch's recorded contract.
PRs are never merged without explicit user instruction.

### Native repository foundation

- Thread: `t3-thread://v1/mcp:0b3d92f7-a705-4ba5-9bb6-b5ca62612768`.
- Branch: `feat/native-repository-foundation`, based on `chore/ga-build-plan`.
- PR: [#4](https://github.com/Dhruv2mars/javelin/pull/4), draft.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-native-repository-foundation`.
- Owns `src/`, root Cargo manifests, native tests, and local-core documentation.
- Done: native initialization, exact immutable objects, ordered history, integrity checks,
  and proven publication metadata behavior across retries, concurrency, and restart.
- Evidence so far: planning commit `3d4144b`; implementation and product verification pending.
- Bounded foundation decision: init creates empty immutable World 0 and preserves
  existing files. Existing-project adoption through a Layer is a follow-on gate.

### Layer filesystem verification

- Thread: `t3-thread://v1/mcp:24542472-902b-4329-927b-f4e65dfe7531`.
- Branch: `test/layer-filesystem-contract`, based on `chore/ga-build-plan`.
- PR: [#5](https://github.com/Dhruv2mars/javelin/pull/5), draft.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/test-layer-filesystem-contract`.
- Owns `tools/workspace-probe/` and `docs/workspace-verification.md`.
- Done: reproducible ordinary-path probe, honest backend capability verdicts,
  host prerequisites, and a bounded evidence-backed recommendation.
- Evidence so far: planning commit `c6b68e4`; actual sparse-backend verification pending.
- Host: macOS 26.6.2 build 25G83, arm64. Linux and Windows execution remains unverified.

### Cross-platform verification CI

- Thread: `t3-thread://v1/mcp:fa456317-56e0-44c1-afc9-91ecbbaccbe0`.
- Branch: `chore/native-cross-platform-ci`, based on `feat/native-repository-foundation`.
- PR: [#6](https://github.com/Dhruv2mars/javelin/pull/6), draft, stacked on PR #4.
- Owns `.github/workflows/` and `docs/cross-platform-verification.md`.
- Done: real formatting, lint, test, and release-build runs on hosted macOS,
  Linux, and Windows, with logs and explicit dependency status.
- Baseline source: `7ec2e75`, `tests/scaffold.rs`. The single process assertion
  captures the original greeting and proves no VCS behavior.
- Complete implementation checks remain pending until the native-core unit is ready.

### Hub continuation and review

- Hourly scheduler: `scheduled-task:command:mcp:3981ee7f-8fb4-4683-a0d3-78e3e74e0371:schedule-task:javelin-ga-hub-continuation-20261013`.
- Bound hub: `e659350e-23d8-4473-98e8-d3609b784340`.
- First scheduled continuation: 2026-10-13 16:54:44 UTC, 22:24:44 Asia/Kolkata.
- Continuation reads this checklist, checks live work before delegating, and keeps
  unchanged status quiet. Disable the scheduler when GA is verified or the user cancels.
- Contract review round 1 failed before review with `Claude API rate limit reached. Try again later.`
- Contract review round 2 uses provider `claude-work`, model `claude-opus-5-5`.
- Review task: `node:delegated-task:command%3Amcp%3A3981ee7f-8fb4-4683-a0d3-78e3e74e0371%3Adelegate-task%3Ajavelin-ga-contract-review-r2-20261013`.
- Review completed. No high-severity contract contradictions were found. Accepted
  corrections cover sparse-backend proof, mandatory filesystem checks, platform
  requirements, traceable decision sources, and subagent Layer sharing.
- Live workthread and PR ownership records were updated while review was running.
- PR #4's checklist heading placement was returned to its owner for correction.
- Product implementation, sparse backend behavior, and GA readiness remain unverified.

### Superseded work

PR #1 and PR #2 are historical work, not dependencies of the GA build. Their open
state does not authorize merging them. Untracked `dist/` and `packages/` contain
earlier artifacts and are excluded from this build's evidence and releases.

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
