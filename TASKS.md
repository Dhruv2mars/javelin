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
- [x] Verify the bounded native Layer state API, immutable sparse checkpoints,
  reference clones, retained history and exact retries. Ordinary-path binding and
  automatic capture remain separate unchecked requirements.
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
  paths on a sparse backend. PR #9 executed the unchanged caller-path probe on
  Windows; required checks failed, so Windows backend acceptance remains open.
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
  Final artifacts are Linux `11847496789`, macOS `11847466814` and Windows
  `11848185219`, expiring 2026-10-27 UTC. PR #6 remains draft and unmerged; its
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
  and rejects unshipped format 1. This is verified at the bounded native API scope below.
- Exact submitted source `bd16d5224fa6dfce01509ebf6c28fb8b92dde1a3` is based on
  accepted CI head `0c8582d`. The hub's independent export on Darwin arm64,
  Rust 1.93.0, passed 43 tests with one ignored process actor, formatting,
  warnings-denied Clippy and a release build. The unchanged documented caller
  compiled against the production library and its CLI check returned two Layers,
  four objects, one World and zero World attempts. Test hooks are absent from the
  release binary. This historical source verifies local API execution; the corrected
  source accepted below supersedes it for dependency use.
- Hub export: `/var/folders/7g/c47xgk2552jdp_3frx78bkq40000gn/T/javelin-hub-layer-bd16d52-pfz39_ae`.
  Independent review `javelin-layer-state-review-hub-r1-bd16d52` completed with
  no material code defect and a medium canonical-checkpoint test coverage gap.
  The owner repaired that gap in the same PR. Corrected-source acceptance is below.
- The hub downloaded all eighteen artifact logs and the full job log from
  [run 38465628999](https://github.com/Dhruv2mars/javelin/actions/runs/38465628999)
  to `/tmp/javelin-hub-layer-ci-38465628999`. Exact `bd16d52` passed formatting,
  Clippy, release build and 43 tests on Linux/macOS, 41 on Windows, with one ignored
  actor each. All fourteen process tests and nine Layer integration tests passed.
  Windows omits storage-symlink and deleted-cwd tests. Artifacts expire 2026-10-27.
  Evidence head `45fe5e7` changes only TASKS and docs; it does not change tested code.
- Review corrections stay in PR #8. Canonical-delta and byte-encoding fixtures must
  each detect removal of their own guard. Add valid-graph tamper coverage for base
  binding and discard where practical. Clones may use any valid immutable checkpoint,
  including stale-only candidates. Directory deletions must name each tracked entry;
  an implied directory-prefix tombstone must not silently stand in for its children.
- The hub reproduced the coverage gap by removing each guard separately in an
  isolated scratch copy. The existing valid-hash test still passed both times.
  Original source was restored. Mutation logs are in
  `/var/folders/7g/c47xgk2552jdp_3frx78bkq40000gn/T/javelin-layer-guard-audit-yghl6v22`.
- Layer IDs are opaque metadata, not host path components. The future filesystem
  binder must map identities safely and prove containment and collision handling.
  Ordinary-path binding, autosave, production naming limits and Gate 2 remain open.
- Corrected source head `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, code/test commit
  `0a75b1a`, adds independent canonical-byte/delta fixtures and valid-graph binding
  tamper cases. The only runtime change rejects ambiguous directory-prefix deletions.
  The hub's exact export `/tmp/javelin-hub-layer-e56fc86-1xjnu2h0` passes 52 tests,
  formatting, Clippy and release build on Darwin arm64 Rust 1.93.0. The production
  library caller and release CLI integrity pass; the release prefix-deletion
  regression also passes. No runtime or test Git invocation was found.
- The hub inspected all seven owner mutation logs. Each canonicality and candidate
  guard removal fails its intended regression. The single per-Layer base scan
  removal remains rejected through accepted-attempt binding; removing both base
  protections fails. No independent failure claim is made for the redundant scan.
  Fresh review `javelin-layer-state-review-hub-r2-e56fc86` closed the medium finding.
  It independently ran the guard mutations and verified file/directory replacement
  still works. No material new bug or required user decision was found.
- VERIFIED bounded native state dependency at exact head
  `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, code/test commit `0a75b1a`.
  The hub inspected all eighteen logs and the complete job log for
  [corrected run 38468818530](https://github.com/Dhruv2mars/javelin/actions/runs/38468818530)
  in `/tmp/javelin-hub-layer-ci-38468818530`. Rust 1.99.0 compiled this exact source.
  Linux/macOS each passed 52 tests; Windows passed 50, excluding two Unix-only tests.
  All fourteen process checks and eighteen Layer integration tests passed.
  Formatting, Clippy and release builds ran and passed on all three OSes.
  Artifacts Linux `11850772498`, macOS `11850931356`, Windows `11850917029` expire
  2026-10-27 UTC. The old `bd16d52` run is not proof for this correction.
- The Codex owner turn ended with a usage-limit error. The same thread and PR were
  resumed on available Claude Opus for an evidence-only handoff. Committed source
  and downloaded proof are intact; no native source work was reopened.
  Acceptance covers state APIs, not filesystem isolation, autosave, full Publish,
  Stack/refresh, hosting, hardware power loss or measured scale. PR #8 stays draft
  and unmerged. The next filesystem unit must preserve these validated semantics.
- Final handoff head `5fc51a5bd055a3c08dec978032258cc5fd43ef32` changes only
  TASKS and `docs/layer-checkpoints.md` from accepted `e56fc86`. The hub checked
  the exact diff. Tested native source is unchanged, and the owner is stopped
  with its watcher disabled. PR #10 uses the accepted source commit.

### Windows projected Layer filesystem proof

- Thread: `t3-thread://v1/mcp:ab206657-bb50-427f-bc5a-2e93ef9843d1`.
- Branch: `feat/windows-projfs-proof`, based on verified `feat/linux-sparse-layer-proof`
  at `19224ef`.
- PR: [#9](https://github.com/Dhruv2mars/javelin/pull/9), draft and linked to owner
  and hub. Planning commit `387cbaa98c75e6b35c70c8358bfacf60e0b83604`.
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
  Sources: [feature enablement](https://learn.microsoft.com/en-us/windows/win32/projfs/enabling-windows-projected-file-system)
  and [cache states](https://learn.microsoft.com/en-us/windows/win32/projfs/cache-state),
  inspected by the hub on 2026-10-14 local time.
- The unchanged caller-path probe needs Unix utilities on Windows. The owner will
  record their exact paths and versions; missing utilities leave affected checks
  INCONCLUSIVE. Unix-only verifier safety tests cannot count as Windows evidence.
- Feature setup is limited to disposable CI, with no reboot or macOS host changes.
  Native database binding, autosave and product Gate 2 are outside this unit.
- The hub inspected [prerequisite run 38465330661](https://github.com/Dhruv2mars/javelin/actions/runs/38465330661)
  at `4d817c50f23c849635f07c1e33c552bbf69ea758` and downloaded artifact `11847982295`
  to `/tmp/javelin-hub-windows-prereq-38465330661`. Windows Server 2022 build 20348
  reports Client-ProjFS already enabled, administrator identity, Unix utilities,
  and MSVC compiler discovery. The API probe failed with C2039 for `std::wprintf`.
  This is a source error, not proof of an unavailable compiler. No projected root
  or provider existed. The owner added `<cwchar>` at `648474c`; the corrected
  prerequisite result follows. This initial attempt did not verify API availability.
- The corrected [run 38465691765](https://github.com/Dhruv2mars/javelin/actions/runs/38465691765)
  at `648474ce69800c2d77ed51c806a7b88fcb21b098` passed actual SDK compilation and
  export inspection. The hub downloaded raw compiler and API records to
  `/tmp/javelin-hub-windows-prereq-38465691765`, confirming both exited zero.
  Feature and API prerequisites are VERIFIED on this admin Server 2022 runner.
  No provider or projected root existed in that attempt; it did not verify a backend.
  The same owner subsequently completed the bounded runtime attempt below.
- Codex usage limits ended the owner turn with uncommitted evidence/audit files.
  The same thread/worktree/PR was resumed on available `claude-work` Opus for its
  bounded handoff. No replacement workstream or second PR was created.
- Actual runtime run `38469723137` at `21b2ddaa9798d0b1bf81cd1b00467c29125d52f5`
  has been downloaded to `/tmp/javelin-hub-windows-runtime-38469723137`.
  The hub independently confirmed the completed run and artifact `11851736345`
  through GitHub. Its digest is
  `62cdd12848000a277b6e3a82a7c9f98cf5daa93438701a9f7fbf69ee8c636af9`;
  retained cloud artifacts expire 2026-10-27 UTC.
- Final evidence/docs head `3ad82410b147850488118512c6f09bc93758c819` preserves
  the tested executable. Its diff adds only owned evidence, the offline verifier,
  docs and TASKS. The hub ran the committed verifier on an isolated export;
  all eight archives passed their digest, raw-file and source checks. All 99 final
  raw files match the hub's independent download byte for byte. All ten retained
  source files match the tested commit with the recorded Windows CRLF conversion.
- Raw evidence and executable assertions support bounded non-admin fixture
  execution, separate source/clone writes, compiler execution, shared-base write
  refusal, and provider kill/reopen before and after a file rename. The snapshot
  copied eight private files totaling 64,088 bytes, retained two deletions, omitted
  hydrated unchanged `main.c`, and reused the unchanged base symlink. The hung
  parent/child negative exited 124 with zero remaining Windows job processes.
  Provider stops, owned scratch removal and temporary account removal are recorded.
- Native directory rename failed with `ENOTSUP`, exit 3. The unchanged common
  probe exited 1, including CRLF output and POSIX writer assumptions. Cache queries
  include errors; metadata preservation and production storage cost remain
  INCONCLUSIVE. These fixture results do not verify a complete ordinary-user
  backend, native database binding, autosave, product Gate 2 or GA.
- Bounded runtime implementation is stopped; PR #9 remains draft/unmerged and unwatched.
  Independent review `javelin-windows-handoff-hub-review-r1-3ad8241` confirmed
  archive/source provenance and the INCONCLUSIVE backend verdict. It found medium
  documentation and checkbox overclaims, returned to this same owner for a
  docs/TASKS-only correction. No new runtime implementation was commissioned.
- Scope corrections: peer `b` has its own full `concurrent.txt`, and projected
  symlinks also report full state. Directory rename failed only for the tested
  base placeholder directory. Restart checks preserve completed writes across
  provider termination, with no kill during an open write. Snapshot write refusal
  tests a clearable Windows read-only attribute, not permission-enforced immutable
  storage; base ACL protection is separate. Multiple snapshot clones, metadata
  edits and mid-write recovery remain unverified. These limits preserve the
  bounded fixture evidence without accepting the Windows Layer backend.
- Review wording/checklist corrections are closed at final head
  `cc94e22b8a316b4308305218aab003c172c96e42`. The hub inspected the exact diff:
  only owned docs and TASKS changed from `3ad8241`; executable and eight archives
  are unchanged. The offline audit still passes. Failed ordinary-path behavior,
  broader restart recovery and clone/storage guarantees are explicitly unchecked.
  The owner is stopped again. This accepts the bounded collection handoff, while
  all Windows backend limitations above remain open.

### Native Linux Layer filesystem binding

- Thread: `t3-thread://v1/mcp:bc4ffe10-9bdc-40bf-b78d-c22654bf4f4a`.
- Branch: `feat/linux-layer-workspaces`, based on exact accepted native Layer
  state `e56fc86` through `feat/native-layer-checkpoints`.
- Provider: configured `claude-work`, model `claude-opus-5-5`, while Codex workers
  are usage-limited. Shared root native source ownership remains with this unit;
  the parallel standalone components below own separate directories.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-linux-layer-workspaces`.
- PR: [#10](https://github.com/Dhruv2mars/javelin/pull/10), draft and linked to
  owner and hub. Planning commit `e25819c3d72cb518ea89e4a99da196b6d83ca193`.
- Owns the native Linux binding, necessary exports/Cargo additions, targeted tests,
  `tools/linux-layer-runtime/`, `docs/linux-layer-runtime.md`, and
  `.github/workflows/linux-layer-runtime.yml`. Existing proofs and workflows remain
  unchanged. A pinned developer checkout may supply PR #7's verified common probe.
- Done: ordinary Linux paths bound to actual repository state, safe quiescent
  native capture, immutable sparse clone, exact diff/discard, interrupted
  materialization recovery, isolation and raw real-run storage/cleanup evidence.
- Automatic capture while tools keep working, production privilege setup,
  complete Layer workflows and other platforms remain required follow-on work.
  Mounts and privilege tests are limited to disposable hosted Linux CI.
- The design compares three shapes and selects shared read-only base/checkpoint
  caches, separate private uppers, and a per-view read-only remount for capture
  with busy-writer refusal. Exact-source runtime evidence and acceptance limits follow.
- Submitted executable `93f9eaa00421f3c5efbaac79449bc22699c464b1` has actual
  hosted evidence inspected by the hub. Run `38503434985`, attempt 2, and
  dispatched run `38503753472` each passed four gated tests. The hub downloaded
  both 21-file artifacts to `/tmp/javelin-hub-linux-binding-38503434985-attempt2`
  and `/tmp/javelin-hub-linux-binding-38503753472`, matched all fourteen source
  hashes against an exact export, and checked raw inventories, recovery and cleanup.
  Base-cache inodes/hashes are identical across four boundaries, fresh uppers are
  empty, and the checkpoint cache omits unchanged `main.c` while retaining changed
  bytes and whiteouts. Shared writable mappings refuse capture with `EBUSY` on
  this kernel. These results remain subject to the independent source review.
- The hub inspected all eighteen native logs and actual job steps from run
  `38503434805` at the same source. Linux/macOS each passed 63 tests, Windows 50.
  Linux has five gated/actor entries ignored in this ordinary suite; macOS/Windows
  have one actor each. Formatting, Clippy and release build passed on all three OSes.
- Attempt 1 of `38503434985` failed the watch-notifications check. The hub retained
  its failed-job log at `/tmp/javelin-hub-linux-binding-watch-attempt1.log`.
  File-watch reliability remains INCONCLUSIVE despite later passing runs.
  The hub's exact local export passes 63 tests with one ignored actor on Darwin
  arm64, Rust 1.93.0. This does not exercise Linux mounts.
- Independent review `javelin-linux-binding-hub-review-r1-93f9eaa` found a material
  recovery defect: after an external Layer head advance, a persistent pre-native
  snapshot error can leave the stale-bound view frozen with pending capture,
  blocking mount/capture/discard. The hub confirmed the faulty history-length
  condition in source; an actual hosted regression is required. Acceptance is held
  for correction in the same PR #10. Reserved staging-name deletion, replay thaw
  handling and helper list/snapshot trust scope were also returned to that owner.
  No binding acceptance, product Gate 2 or GA is claimed from green tests.

### Expanded parallel workstreams

The user authorized broader parallel execution on 2026-10-14. Independent,
dependency-ready units stay active while the hub verifies completed artifacts.
The existing hourly hub scheduler is unchanged. No second scheduler is created.

- [x] Review parallel boundaries and launch seven isolated, app-bound single-PR threads.
- [x] Confirm each new worktree and register all seven draft PRs with this hub.
- [ ] Independently inspect and accept each bounded artifact before integration.

#### Javelin mechanical merge engine

- Thread: [Javelin mechanical merge engine](t3-thread://v1/mcp:c7d7eb0c-cdc6-4861-a5f3-69968a664085).
- Branch: `feat/mechanical-merge-engine`; base source `e56fc86`, PR target `feat/native-layer-checkpoints`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-mechanical-merge-engine`.
- Draft PR: [#12](https://github.com/Dhruv2mars/javelin/pull/12), linked to owner and hub; unmerged.
- Owns: crates/javelin-merge/**; docs/mechanical-merge.md; mechanical-merge.yml and its own appended TASKS section.
- Done: Pure deterministic three-way merge with explicit structural/content conflicts and actual native-manifest caller tests.
- Implementation/evidence acceptance is pending. No product gate is checked by launch.

#### Javelin repository protocol and verified transfers

- Thread: [Javelin repository protocol and verified transfers](t3-thread://v1/mcp:8bc9054b-2120-4a21-be7d-bd3a9a132feb).
- Branch: `feat/repository-protocol`; base source `e56fc86`, PR target `feat/native-layer-checkpoints`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-repository-protocol`.
- Draft PR: [#15](https://github.com/Dhruv2mars/javelin/pull/15), linked to owner and hub; unmerged.
- Owns: crates/javelin-protocol/**; docs/repository-protocol.md; repository-protocol.yml and its own appended TASKS section.
- Done: Bounded versioned codec and verified object transfer/reference receiver between real native repositories.
- Implementation/evidence acceptance is pending. No product gate is checked by launch.
- Submitted source `58fadbf1896160590ac8aed66d1d0b596a5c410a` independently
  passes fourteen tests in the hub's exact isolated export on Darwin arm64,
  Rust 1.93.0. Tests launch the actual sender and receiver across two repositories,
  drop a committed reply, retry in fresh processes and verify incremental transfer.
  The receiver stores through public native APIs and compares claimed IDs before
  World references advance. Refused input can leave unreferenced objects.
  Transport security, authentication, whole-Layer Publish, traces and hosting
  remain outside this unit. Current-head hosted results and owner review are pending;
  this local check does not accept the protocol dependency.

#### Javelin review and applied-permission state machine

- Thread: [Javelin review and applied-permission state machine](t3-thread://v1/mcp:08bd84e4-6d16-4282-9e65-01e13e2d614d).
- Branch: `feat/review-permission-model`; base source `e56fc86`, PR target `feat/native-layer-checkpoints`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/feat-review-permission-model`.
- Draft PR: [#13](https://github.com/Dhruv2mars/javelin/pull/13), linked to owner and hub; unmerged.
- Owns: crates/javelin-collaboration/**; docs/review-permission-model.md; review-permission-model.yml and its own appended TASKS section.
- Done: Pure author-owned revision/exact-approval/applied-permission model; no durable store or publication ticket.
- Implementation/evidence acceptance is pending. No product gate is checked by launch.

#### Javelin Windows sparse backend repair proof

- Thread: [Javelin Windows sparse backend repair proof](t3-thread://v1/mcp:dac1385c-b28b-43a5-ae66-ff6ec16c75b3).
- Branch: `test/windows-sparse-backend`; base source `cc94e22`, PR target `feat/windows-projfs-proof`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/test-windows-sparse-backend`.
- Draft PR: [#14](https://github.com/Dhruv2mars/javelin/pull/14), linked to owner and hub; unmerged.
- Owns: tools/windows-sparse-backend/**; docs/windows-sparse-backend.md; windows-sparse-backend.yml and its own appended TASKS section.
- Done: One bounded real sparse-backend attempt addressing rename and permission-enforced snapshot gaps, with honest refusal evidence.
- Hub audit `javelin-hub-audit-pr14-8712866-r1` on tested `8712866`, head `2565598`:
  offline verifier passes on 8 archives and 96 raw files and fails on a flipped
  byte; the hub re-downloaded run `38508454070` artifact and matched digest
  `c7b9b68d…d999` and all 96 raw files byte for byte; tested executable differs
  from head only in the verifier. ProjFS base-directory rename refusal (error 50,
  three states), Cloud Files seven rename cases, non-admin ordinary operations,
  Administrators-owned base/snapshot refusals, two-clone isolation with unchanged
  snapshot, kill-mid-download recovery, torn in-place write after writer kill,
  zero allocated bytes for untouched placeholders, and five-file capture are
  VERIFIED from raw files. The four common-probe failures are honestly reported.
- Returned for docs/TASKS-only correction in the same PR: the doc says the ACL
  probe ran `icacls /grant <self>:F` as the proof user, but the raw argv in both
  protection probes is `runneradmin:F` because `userInfo().username` returned the
  parent's name under `Start-Process -Credential` (hub confirmed in
  `native-protect-base.json` and `native-protect-snapshot.json`; proof user is
  `jvs127c2b16d4c6`). The denial still holds since any grant needs WRITE_DAC.
  Attempt 5 `e738c3f` concluded `failure`, which the attempts table omits.
- Recorded limits for the next Windows unit, not fixed here: capture writes
  whatever `bytes()` returned with no PARTIAL guard (`sparse.cpp:77-81, 321-327`),
  so a renamed partial placeholder could be recorded truncated; capture and
  audit walk directory junctions as plain directories (`sparse.cpp:304, 326,
  377-378`) with no entry or size bound; manifest blob names and link targets
  are trusted and blob bytes are never checked against their digest name; the
  snapshot-served-root marker is owner-editable. Production Cloud Files facts
  confirmed from code and Microsoft docs: per-user `CfRegisterSyncRoot` needs no
  admin, Windows 10 1709 / Server 2016 minimum, NTFS only, dehydrated placeholders
  unreadable without a provider. Unconfirmed: reboot/logoff persistence without
  `StorageProviderSyncRootManager`, antivirus or indexer hydration, OneDrive
  coexistence, packaging expectations.
- Bounded verdict: acceptable as Windows feasibility evidence once the wording
  correction lands. Not Gate 2, not Windows product acceptance, not GA. Next
  bounded Windows unit: capture hardening (PARTIAL guard, junction refusal,
  manifest path and digest validation, bounds) with a rerun, then a
  Windows-portable common probe (CRLF-tolerant compiler check, file-only fsync).

#### Javelin macOS sparse filesystem feasibility

- Thread: [Javelin macOS sparse filesystem feasibility](t3-thread://v1/mcp:98fd57f4-9ac4-412f-8277-3d1ed5098d1d).
- Branch: `test/macos-sparse-backend`; base source `c5c0ebe`, PR target `chore/ga-build-plan`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/test-macos-sparse-backend`.
- Draft PR: [#11](https://github.com/Dhruv2mars/javelin/pull/11), linked to owner and hub; unmerged.
- Owns: tools/macos-sparse-backend/**; docs/macos-sparse-backend.md; macos-sparse-backend.yml and its own appended TASKS section.
- Done: One hosted macOS prerequisite and feasible mounted-backend attempt, without laptop or host-security changes.
- Implementation/evidence acceptance is pending. No product gate is checked by launch.

#### Javelin native development packaging and installation

- Thread: [Javelin native development packaging and installation](t3-thread://v1/mcp:6ebbed40-380a-4bc8-8330-96e51b8c89e4).
- Branch: `build/native-development-packages`; base source `e56fc86`, PR target `feat/native-layer-checkpoints`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/build-native-development-packages`.
- Draft PR: [#17](https://github.com/Dhruv2mars/javelin/pull/17), linked to owner and hub; unmerged.
- Owns: tools/native-packaging/**; docs/native-development-packages.md; native-packaging.yml and its own appended TASKS section.
- Done: Three-OS development CLI archives, checksums and clean-temp installation smoke; no public release.
- Implementation/evidence acceptance is pending. No product gate is checked by launch.

#### Javelin native storage cost and contention measurements

- Thread: [Javelin native storage cost and contention measurements](t3-thread://v1/mcp:0247ee42-1e8c-4241-afdf-07c41c64cf4b).
- Branch: `perf/native-storage-baseline`; base source `e56fc86`, PR target `feat/native-layer-checkpoints`.
- Worktree: `/Users/dhruv2mars/.t3/worktrees/javelin/perf-native-storage-baseline`.
- Draft PR: [#16](https://github.com/Dhruv2mars/javelin/pull/16), linked to owner and hub; unmerged.
- Owns: tools/native-benchmarks/**; docs/native-storage-baseline.md; native-storage-baseline.yml and its own appended TASKS section.
- Done: Reproducible release-native API cost and multiprocess contention measurements; no scale or filesystem-throughput claim.
- Implementation/evidence acceptance is pending. No product gate is checked by launch.

Standalone Rust crates own an empty `[workspace]`, lockfile and target ignore.
They use accepted public native types and APIs; they do not copy private hashing
or decoding. Root Cargo/src/native tests remain outside these units' ownership.
Integration, core exports, Stack adaptation, durable collaboration and actual
publication authorization require later bounded native work. The reviewed plan
and dependency rules are in `docs/parallel-build-plan.md`.
- Read-only design task `javelin-whole-layer-publish-grounding-r1-20261014`
  completed. It wrote no code and created no PR or scheduler. Its design and the
  hub's rulings are recorded under "Next native unit" below.

#### Hub consolidation on 2026-10-14

- All seven expanded workstreams and the Linux binding correction handed off.
  Each owner thread is stopped with its watcher released; the merge-engine thread
  ended cancelled during its own round-4 review, so the hub audit is that round.
- The hub fetched every branch and confirmed from file-level diffs that each PR
  head differs from its tested commit only in TASKS, its doc, committed evidence,
  or the offline verifier: PR #10 `8137111`→`a4a95c1`, #11 `39a065b`→`651835d`,
  #13 `87f2acd`→`e0f0a19`, #14 `8712866`→`2565598`, #15 `02c7427`→`29457a8`,
  #16 `4149da9`→`688cc0c`, #17 `b9243f5`→`f31c4cb`. PR #12 is tested at head `2ca73dd`.
- Owner reports are not acceptance. The hub launched eight independent read-only
  audits on `claude-work` `claude-fable-5-1` at high effort, one per PR, each
  exporting the exact tested commit, downloading the hosted artifacts, rerunning
  tests and verifiers, and reviewing source adversarially:
  `javelin-hub-audit-pr10-8137111-r1`, `-pr11-39a065b-r1`, `-pr12-2ca73dd-r1`,
  `-pr13-87f2acd-r1`, `-pr14-8712866-r1`, `-pr15-02c7427-r1`, `-pr16-4149da9-r1`,
  `-pr17-b9243f5-r1`. Scratch lives under `/tmp/javelin-hub-audit-pr*`.
- [ ] Inspect each audit against its cited evidence, then accept or return each PR.
- Owner-reported native API requests awaiting the next core unit: public pre-store
  object ID computation, `ObjectId` byte conversion, a cheap existence check or
  size-limited read (PR #15); a cheap public head read and readable effective
  pragmas (PR #16). The owner of PR #13 and PR #15 each force exact-base Publish,
  which conflicts with nonintersecting advances; both must adopt the Layer-bound
  Publish outcome once the core unit exists.

#### Next native unit: whole-Layer Publish

- Gated on hub acceptance of PR #10 at `8137111`, because PR #10 is the root
  `src/` writer. Branch from that accepted head; not stacked on PR #12, #13 or #15.
- Design selected: one `publish_layer` call composing `head World + Layer delta`
  inside one IMMEDIATE SQLite transaction, with durable `Published`, `Conflict`
  and `LayerStale` outcomes, operation-ID idempotency across later advances,
  `OperationIdReused` for changed input or raw-advance collisions, a guard hook run
  under the lock for the future hosted permission check, and a format 3 append-only
  `publications` table. Prepare-then-seal and an external consumer crate were
  rejected for livelock and TOCTOU reasons.
- Hub rulings from the contract and verified grilling answers: a published Layer
  is read-only with clone allowed and no auto-discard or auto-refresh; an
  intersecting advance is a conflict even when the results are identical;
  attribution is declared in the Publish input and recorded in the receipt; agent
  context is one opaque Blob reference for now; format 2 stores are rejected
  without migration. The contract now records the read-only rule.
- Acceptance tests (a) through (k) in the design cover base-head publish,
  nonintersecting advance without refresh, exact/prefix/delete-modify conflicts,
  stale/discarded/second publish, retry identity, kill-point atomicity, concurrent
  disjoint and overlapping Layers, attribution and override warnings, guard
  refusal writing zero rows, tamper detection, and format rejection, plus fmt,
  Clippy and the three-OS native workflow.


### Hub continuation and review

- Hourly scheduler: `scheduled-task:command:mcp:3981ee7f-8fb4-4683-a0d3-78e3e74e0371:schedule-task:javelin-ga-hub-continuation-20261013`.
- Bound hub: `e659350e-23d8-4473-98e8-d3609b784340`.
- First scheduled continuation: 2026-10-13 16:54:44 UTC, 22:24:44 Asia/Kolkata.
- Continuation reads this checklist, checks live work before delegating, and keeps
  unchanged status quiet. Disable the scheduler when GA is verified or the user cancels.
- Provider routing from 2026-10-14: Codex is usage-limited. The user directed that
  worker threads and delegated tasks use `claude-work` `claude-fable-5-1` at high
  effort until Codex limits reset. The hub itself runs on the same model. Reviews
  by the same model family are recorded as a limitation, not independence.
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
