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
- [x] Independently verify committed filesystem-probe controls and negative cases
  at `0f15db2`. This verifies the probe only, not a sparse Layer backend.
- [x] Rerun final probe source `bce352e`, including termination of a stalled external
  command group and rejection of hardlinked or FIFO report destinations.
- [ ] Implement Layer creation, cloning, checkpointing, discard, status, and diff.
- [ ] Adopt existing non-Git project files through a Layer without accepting edits outside it.
- [ ] Implement Stack, refresh, structural conflicts, and explicit resolution.
- [ ] Bind supplied agent traces to immutable work, with missing-trace override.
- [ ] Prove process concurrency, crash recovery, retries, and corruption detection.
- [ ] Build a real sample application using external agents and Javelin exclusively.
- [ ] Verify the complete workflow on each supported operating system.
- [x] Run native formatting, lint, test, and release-build checks on development
  GitHub Actions macOS, Linux, and Windows runners. Verify that the tested source
  matches the exact native PR #4 commit accepted by the hub at Gate 1. This does not verify product CI.
- [x] Verify the development CI scaffold setup on all three hosted OSes. This is
  infrastructure evidence only; the greeting-only test does not verify native VCS behavior.

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
Bounded foundation and probe units have been verified. Follow-on workstreams run
from reviewed, explicit dependencies; complete product workflows remain open.
PRs are never merged without explicit user instruction.

### Native repository foundation

- Thread: `t3-thread://v1/mcp:0b3d92f7-a705-4ba5-9bb6-b5ca62612768`.
- Branch: `feat/native-repository-foundation`, based on `chore/ga-build-plan`.
- PR: [#4](https://github.com/Dhruv2mars/javelin/pull/4), draft.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-native-repository-foundation`.
- Owns `src/`, root Cargo manifests, native tests, and local-core documentation.
- Done: native initialization, exact immutable objects, ordered history, integrity checks,
  and proven publication metadata behavior across retries, concurrency, and restart.
- VERIFIED bounded Darwin foundation: reviewed commit `c78ea7a5d3eb9555625391e2f7346859d70f4c2a`.
  The hub ran 26 tests, formatting, warnings-denied Clippy, release build, and real
  CLI smoke in an isolated export. PR #6 verifies matching compiled source on Linux
  and macOS, plus the Windows suite with two Unix-only tests excluded.
  Full Layer workflows remain open.
- The original correction turn is terminal with T3 status `failed`; it left a clean
  committed correction, which the hub verified independently. It is not a live writer.
- Local baseline `7ec2e75` was initially unpublished when CI started. The native
  owner pushed it unchanged, and the hub confirmed the remote branch now names that SHA.
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
- Host: macOS 26.6.2 build 25G83, arm64. PR #7 subsequently verified Linux caller
  paths on a sparse backend. Windows execution remains unverified.
- The hub independently exported `0f15db2` and reran directory, clonefile, hardlink
  negative, and safety regression checks. Both controls pass all eleven checks;
  the hardlink negative fails actual isolation assertions with exit 1. Timeout
  exit 124 remains INCONCLUSIVE, and unsafe report aliases leave caller bytes intact.
- Hub evidence: `/tmp/javelin-hub-probe-0f15db2`, including `safety/` reports.
- Final executable source `bce352e` was exported and independently rerun by the hub
  at `/tmp/javelin-hub-probe-bce352e/evidence`; all verifier assertions pass, including
  stalled-child termination, alias, hardlink and FIFO report protections.
- The verifier now creates a unique default output folder. Final docs and checklist
  were handed off at `0e7ed04`. The probe is VERIFIED; production sparse backend is not.
- The official FSKit sample build exited 69 because Xcode's license is unaccepted.
  Zero valid signing identities were found. These are prerequisites, not an accepted
  sparse backend, and no license or host security change has been made.
- Handoff owner: the hub will commission real sparse backend implementation and
  filesystem-probe execution for each OS after inspecting PR #5's evidence. PR #6
  verifies native storage development CI only and does not own the sparse backend gate.

### Cross-platform verification CI

- Thread: `t3-thread://v1/mcp:fa456317-56e0-44c1-afc9-91ecbbaccbe0`.
- Branch: `chore/native-cross-platform-ci`, based on `feat/native-repository-foundation`.
- PR: [#6](https://github.com/Dhruv2mars/javelin/pull/6), draft, stacked on PR #4.
- Owns `.github/workflows/native.yml` and `docs/cross-platform-verification.md`.
- Done: real formatting, lint, test, and release-build runs on hosted macOS,
  Linux, and Windows, with logs and explicit dependency status.
- Baseline source: `7ec2e75`, `tests/scaffold.rs`. The single process assertion
  captures the original greeting and proves no VCS behavior.
- VERIFIED setup evidence: [run 38427608862](https://github.com/Dhruv2mars/javelin/actions/runs/38427608862),
  checked-out commit `fca6f758abec55f92f104d0fb0d9a42fd7b0278f`.
- The hub inspected all three completed job results and downloaded all three log
  artifacts to `/tmp/javelin-hub-ci-38427608862`. Each has six logs, matching SHA,
  and one passing scaffold integration test. Artifacts expire 2026-10-27.
- Actual failure-path log upload is unconfirmed. Complete product workflows remain open.
- The implementation rerun requires hub verification of PR #4 at a named commit,
  followed by `native-core-complete <SHA>` and a rebase of this same PR onto that source.
- GitHub Actions here is Javelin development infrastructure. Javelin's product CI
  workflow syntax and execution design remain a separate, unchecked hosting item.
- `native-core-complete c78ea7a5d3eb9555625391e2f7346859d70f4c2a` was sent after
  independent corrected-source acceptance. The same PR #6 completed the actual-core run.
- VERIFIED compiled core suite at final head `0c8582ddcb175d7bd4d2752e73f795651f810a6f`,
  [run 38463522128](https://github.com/Dhruv2mars/javelin/actions/runs/38463522128).
  The hub inspected all three jobs and downloaded all eighteen logs to
  `/tmp/javelin-hub-ci-38463522128`. Linux and macOS each passed 26 tests; Windows
  passed 24. Each has one ignored child-process entry point exercised by parent tests.
  Formatting, Clippy, release builds, all eight process tests and the missing-tail
  retained-anchor regression passed on every OS with Rust 1.99.0.
- The hub also inspected earlier actual-core run `38462957668` at `bfd2a51d` and
  its eighteen logs in `/tmp/javelin-hub-ci-38462957668`. Host API comparisons from
  reviewed `c78ea7a` to both tested heads show only workflow, CI doc and TASKS changes.
- Windows excludes Unix-only storage-symlink and deleted-cwd tests. Those behaviors,
  non-Unix directory syncing, hardware power loss and performance remain unverified.
  Native checks do not establish sparse Layers, installation, product CI or GA.
  Retained artifacts expire 2026-10-27 UTC. PR #6 remains draft and unmerged; its
  owner stopped the watcher after the final handoff.

### Linux sparse Layer backend proof

- Thread: `t3-thread://v1/mcp:7a0a409d-e0f3-4321-84f1-5ebbcc357056`.
- Branch: `feat/linux-sparse-layer-proof`, based on `test/layer-filesystem-contract`.
- PR: [#7](https://github.com/Dhruv2mars/javelin/pull/7), draft, stacked on PR #5.
- Owns `tools/overlayfs-proof/`, `.github/workflows/overlayfs-proof.yml`, and
  `docs/linux-workspace-proof.md`. Native CI owns only `native.yml`.
- Done: bounded real shared-base OverlayFS mounts, sparse private changes and clones,
  ordinary-tool verification, recovery and cleanup evidence on disposable Linux CI.
- Mounts or privilege changes on the macOS developer host are outside this task.
- The proof is not complete product filesystem acceptance. Native storage integration,
  automatic checkpointing, platform coverage, and production lifecycle remain gates.
- Actual Linux run [38430385798](https://github.com/Dhruv2mars/javelin/actions/runs/38430385798)
  completed on `89d76df34bf85daee9852ffc9350109bd13b2daf`, after an earlier watcher
  wake had no Actions run. The owner rejected that earlier false-positive status.
- The hub inspected the completed OverlayFS job and downloaded its artifacts to
  `/tmp/javelin-hub-overlayfs-38430385798`. Shared-base mounts, separate clone proof,
  remount results and cleanup are evidenced. The hub ran the offline source-hash
  audit and compared all nineteen downloaded files with the committed evidence;
  all are byte-identical. This bounded Linux primitive is VERIFIED; native Layer
  integration, production lifecycle and product Gate 2 remain open.

### Native Layer and checkpoint foundation

- Thread: `t3-thread://v1/mcp:4888671a-e20b-422b-b5d7-efcf07e19974`.
- Branch: `feat/native-layer-checkpoints`, based on `chore/native-cross-platform-ci`.
- PR: [#8](https://github.com/Dhruv2mars/javelin/pull/8), draft, stacked on PR #6.
- Owns native Layer/domain/storage additions, relevant tests and Cargo changes,
  and `docs/layer-checkpoints.md`. Workflow and filesystem-proof sources are excluded.
- Done: exact-base Layer metadata, immutable sparse changed/deleted-path checkpoints,
  durable private heads, cheap immutable-snapshot clones, derived status/diff and discard
  that preserves World and siblings, with restart/concurrency/corruption proofs.
- Ordinary-path binding, automatic capture, Stack/refresh/full Publish and trace policy
  are required follow-on work. API tests cannot verify a complete Layer user workflow.
- This unit also owns the two inherited low validator-coverage regressions noted in review.
- Selected representation: a complete sparse delta per immutable checkpoint, with
  transactional head history and retry outcomes. Format 2 distinguishes symlinks
  and rejects unshipped format 1. This is a reported design choice, not verified behavior.

### Windows projected Layer filesystem proof

- Thread: `t3-thread://v1/mcp:ab206657-bb50-427f-bc5a-2e93ef9843d1`.
- Branch: `feat/windows-projfs-proof`, based on verified `feat/linux-sparse-layer-proof`
  at `19224ef`. Draft PR registration is pending.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-windows-projfs-proof`.
- Owns `tools/windows-workspace-proof/`, `docs/windows-workspace-proof.md`, and
  `.github/workflows/windows-workspace-proof.yml`. Native code and existing probes
  and workflows are excluded. This work does not overlap active PR #8.
- Done: bounded real Windows CI prerequisite discovery, and projected ordinary-path
  behavior, isolation, sparse clone and provider restart proof when available.
  Missing capabilities remain INCONCLUSIVE with exact commands and source evidence.
- ProjFS is provisional. Official Microsoft docs describe the optional feature and
  read hydration, not its availability on the hosted runner or production acceptance.
  The proof must distinguish read caches from private changes and measure both.
- The unchanged caller-path probe needs Unix utilities on Windows. The owner will
  record their exact paths and versions; missing utilities leave affected checks
  INCONCLUSIVE. Unix-only verifier safety tests cannot count as Windows evidence.
- Feature setup is limited to disposable CI, with no reboot or macOS host changes.
  Native database binding, autosave and product Gate 2 are outside this unit.

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
- Historical native review started at `6d44167`. The hub independently exported it and
  observed `cargo test --locked`: 21 passed, one ignored process-test entry point.
  Its release build also passed. Owner added a rollback regression at `8ae7fba`
  and final evidence at `b9decc6`, reporting 22 tests passed.
- Earlier hub acceptance was withheld for a reproduced medium correctness gap: after an
  acknowledged advance, deleting the newest World row while retaining its accepted
  attempt makes `history` and repeated `init` silently return the older head.
  `check` detected the broken foreign key. The owner fixed this in the same PR #4.
- Reproduction used exported `6d44167` and `/tmp/javelin-hub-tail-corruption-6d44167`.
  No runtime or test invoked Git. Tests must reject missing-tail history and stale
  results beyond head; the explicit-directory CLI's eager cwd lookup is also returned.
- Corrected source `c78ea7a` passes the hub's 26-test suite, release smoke, original
  corruption reproduction, formatting and Clippy. Follow-up Claude Opus5.5 review
  reports no blocking findings. The core-complete signal was sent to PR #6's owner.
- Follow-on low test coverage: preserve regressions for stale-only manifest graph
  corruption and invalid stale-result ordering. Code was directly probed and rejects
  both, but the current suite does not detect removal of those individual checks.
- Cross-model platform trail review by `claude-opus-5-5` confirmed exact-source
  comparisons and downloaded test evidence. Wording now distinguishes matching source
  from the tested CI head and names Windows exclusions. Its Windows probe portability
  gap is resolved in the launch brief through explicit utility discovery and
  INCONCLUSIVE checks when utilities are absent. Read-hydration cost remains open.

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
