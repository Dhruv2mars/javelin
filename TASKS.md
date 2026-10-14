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

## Filesystem verification workstream

Owned by `test/layer-filesystem-contract`, based on `chore/ga-build-plan`.
Done means a committed bounded probe that uses ordinary paths and external tools,
reports per-capability verdicts, and records host prerequisites and a provisional
next-step recommendation. A production backend recommendation remains unconfirmed
until a real sparse Layer sharing immutable base content passes Gate 2. Directory
and clonefile controls verify the probe only. This PR may finish with a VERIFIED
probe and an INCONCLUSIVE sparse-backend result.
Only this workstream's checklist is updated here. Global gates remain unverified.

- [x] Read build instructions and define the self-verifiable outcome.
- [x] Open exactly one draft PR and register it with T3 Code: [#5](https://github.com/Dhruv2mars/javelin/pull/5).
- [x] Discover host filesystem, FSKit, SDK, signing, and installation prerequisites. FSClient executes; Xcode build exits 69 for unaccepted license; zero valid signing identities. No Layer mounted.
- [x] Build a backend-agnostic behavior probe and explicit directory/clonefile controls.
- [x] Exercise bytes, mutations, traversal, symlinks, watches, compiler access, isolation, and process interruption. Both controls pass eleven checks on this host.
- [x] Verify the probe catches a broken isolation control and records unsupported capabilities. Hardlink leakage fails; caller-prepared paths report clone provisioning INCONCLUSIVE.
- [x] Verify review repairs for canonical output paths, fresh watchers, clone write readback, and wall-time bounds. Hub independently reproduced controls and regressions at `0f15db2`.
- [x] Use a unique verifier output directory by default, per hub concurrency review, and rerun the final verifier. Final executable source `bce352e`; omitted-output run exits 0 into unique `javelin-probe-evidence-6sxr25`.
- [x] Repair final review findings and exercise hard-link and FIFO report rejection plus actual external-tool subprocess termination on wall expiry. Directory and clonefile still pass eleven checks.
- [x] Commit reproducible host evidence, official platform sources, gaps, and the smallest next step. `docs/workspace-verification.md` and `tools/workspace-probe/evidence/macos-arm64/` preserve actual results. Production recommendation and real sparse Gate 2 remain INCONCLUSIVE.
- [x] Report final evidence and verdict to the hub, leaving PR #5 draft and unmerged. Final source `bce352ec4e5f78eb98c71facba68b872d3e11118`; probe VERIFIED, sparse backend and Gate 2 INCONCLUSIVE. Stable completion message `filesystem-pr5-completion-evidence-v1`.

## Linux sparse Layer proof workstream

Owned by `feat/linux-sparse-layer-proof`, based on `test/layer-filesystem-contract`.
Done means a bounded disposable GitHub-hosted Linux run uses native OverlayFS,
one immutable lower tree and separate sparse upper/work directories, passes the
committed ordinary-path probe and external compiler, and records clone semantics,
process interruption, remount, cleanup, storage evidence and exact source SHA.
This proof does not complete product Gate 2 or GA. Only this appended checklist
is owned here. Privileged mounts are restricted to the ephemeral Linux CI job.

- [x] Read instructions and latest hub contract; define verifiable scope.
- [x] Open and immediately link one draft PR against `test/layer-filesystem-contract`: [#7](https://github.com/Dhruv2mars/javelin/pull/7).
- [x] Confirm official kernel/runner prerequisites and record unresolved hosted-mount assumptions in `docs/linux-workspace-proof.md`.
- [x] Implement guarded small fixtures, actual mount commands and immutable sparse snapshot clones. Hosted run `38430385798` verified all eight backend phases.
- [x] Drive common probe as ordinary runner UID 1001; ten checks VERIFIED, caller-path clone remains INCONCLUSIVE, independent sparse snapshot clone and compiler checks VERIFIED.
- [x] Add owned workflow with pinned actions, bounded execution and retained artifacts. Local shell/Bun/Python checks pass; laptop refusal exits 2 before sudo.
- [x] Observe actual cloud run and record exact SHA `89d76df34bf85daee9852ffc9350109bd13b2daf`, Linux `6.17.0-1022-azure`, sparse uppers, clone, SIGKILL/remount and cleanup evidence in `tools/overlayfs-proof/evidence/linux-x64/`. Product Gate2/GA and power-loss durability remain unverified.
- [x] Report evidence and blockers to hub via stable message `overlayfs-pr7-completion-evidence-v1`; leave PR #7 draft and unmerged for hub review.
- [x] Resolve base TASKS conflict by preserving PR #5's completed handoff and this appended workstream. Import base repairs unchanged.
- [x] Establish real hosted run after conflict repair. The `bf9fbc4` notification referred only to a skipped Macroscope check; real OverlayFS run `38430385798` passed at `89d76df`.
- [x] Audit retained artifact against source hashes and raw storage/recovery records with `rtk proxy bun tools/overlayfs-proof/verify-evidence.mjs`; all 19 downloaded artifact files match the committed copies. Final evidence reported to hub.

## Windows projected filesystem proof workstream

Owned by `feat/windows-projfs-proof`, based on `feat/linux-sparse-layer-proof`.
Done means one rerunnable bounded disposable GitHub-hosted Windows attempt retains
raw prerequisite results, exact executable source SHA, job and artifact provenance,
and per-scope verdicts. If ProjFS can run without reboot, implement a minimal native
provider and measure actual ordinary-path behavior and sparse private state.
If feature, API, compiler or other required prerequisites are missing, finish with
availability evidence and an INCONCLUSIVE backend verdict. Product Gate 2 and GA
remain unverified. No Windows installation or projection runs on the laptop.
Only owned Windows tools, documentation, workflow and this section may change.

- [x] Read instructions and latest hub contract; define bounded self-verifiable done.
- [x] Open exactly one draft PR against `feat/linux-sparse-layer-proof`, link it immediately, and report planning SHA to the hub. PR #9; planning `387cbaa`; stable hub message `windows-pr9-planning-v1`.
- [x] Apply architect and compare two source-backed native provider shapes before coding. Direct-tree/native root owner chosen over blob-index/per-root write manifest; retain only immutable changed/deleted snapshot model for clone. Same-model reviews have limited diversity.
- [x] Implement bounded prerequisite capture for feature, API, compiler, identity and Unix utility availability. No provider or projection is present in this first unit.
- [x] Run actual hosted Windows prerequisite jobs and inspect raw archives. Initial `4d817c5` run `38465330661` exposed a missing `<cwchar>` include, not an unavailable compiler. Corrected source `648474c` run `38465691765` confirms enabled feature, native compiler and all required exports. Retain both original archives.
- [x] Execute native provider and ordinary-path checks as a disposable non-admin CI account in run `38469723137` at `21b2dda`, with Windows job child-termination records (17 `activeAfterTermination=0`, 20 children exit 0, owned root removed). Execution and evidence only; the gate below failed.
- [ ] Ordinary-path behavior gate: NOT VERIFIED. Projected base placeholder directory rename ENOTSUP; common probe exit 1 (rename, traversal, CRLF compiler output, writer `fsync` EPERM, final integrity).
- [ ] Restart recovery: only completed, closed-handle state survived provider kill and reopen. Mid-write kill and atomic-save recovery INCONCLUSIVE.
- [ ] Sparse clone and storage cost: one clone copied only full files plus tombstones. Multi-clone sharing, ACL-enforced snapshot immutability (only the read-only attribute was proven), metadata edits, aliases and storage cost INCONCLUSIVE.
- [x] Preserve original artifact, audit source hashes offline, and document exact evidence and gaps. Eight attempts retained byte-identically; CRLF checkout transform recorded; `rtk proxy bun tools/windows-workspace-proof/verify-evidence.mjs` passes and rejects a tampered raw file.
- [x] Report VERIFIED/NOT VERIFIED/INCONCLUSIVE scopes, dependencies and blockers to the hub; leave the single PR draft and unmerged. Backend, Gate 2 and GA INCONCLUSIVE. Hub review r1-3ad8241 corrections applied to docs/TASKS only.
- [ ] Later unit, not this one: directory rename (base placeholder, local and full), common-probe Windows CRLF/fsync portability, probe/native fixture leftover isolation, ACL-enforced snapshots and mid-write kill recovery.

## Windows sparse backend workstream

Owned by `test/windows-sparse-backend`, based on unmerged `feat/windows-projfs-proof`
at exact `cc94e22b8a316b4308305218aab003c172c96e42`. That base is a dependency, not
acceptance; PR #9 accepted only its bounded collection, and its backend stays INCONCLUSIVE.
Owns only `tools/windows-sparse-backend/`, `docs/windows-sparse-backend.md`,
`.github/workflows/windows-sparse-backend.yml` and this section.
Done means one bounded disposable hosted Windows attempt, with exact tested source,
job, artifact digest, raw records, cost and cleanup, that gives an honest per-scope
verdict for: the directory-rename root cause (projected, local and full directories);
binary/read/write/save/delete/symlink/compiler on separate native fixtures;
ACL-enforced base and frozen-snapshot protection against an ordinary user clearing
attributes; two clones sharing one immutable snapshot with private writes; actual
mid-write kill versus completed closed-handle preservation; hydration/full-copy cost
recorded without overclaim. Windows CRLF/fsync checks stay separate from the unchanged
common probe. A real platform refusal ends the attempt with a documented verdict.
No laptop install, privilege, reboot or security change. No Windows product acceptance
unless the entire minimum backend passes.

- [x] Read instructions, hub contract, PR #9 evidence and review corrections; define done.
- [x] Open exactly one draft PR against `feat/windows-projfs-proof`, link it, report planning SHA to hub. PR #14, planning `34b0aee`, message `windows-sparse-backend-pr14-planning-v1`.
- [x] Ground rename/ACL/cache behavior in primary Microsoft docs. ProjFS placeholder directories never become full (cache-state doc); VFS for Git documents partial-directory rename as unsupported (0x80070032); PR #9 log has no PRE_RENAME for `dir`. Actual SDK `cfapi.h` declarations are captured by the hosted run.
- [x] Compare at least two filesystem shapes with independent review; record limits. Cloud Files sync root per Layer chosen; ProjFS rename matrix kept as control; container layers rejected. Two read-only reviews (Codex gpt-6.1-sol high, Claude Opus 5.5 medium) agreed; neither executed code.
- [x] Implement the bounded native backend and separate checks in the owned directory. `sparse.cpp` Cloud Files provider/capture/audit, pinned unchanged PR #9 `projfs-provider.cpp`, `checks.mjs`, `run.ps1`, `user-run.ps1`.
- [x] Run one bounded hosted attempt; retain raw archive and provenance; audit offline. Eight attempts (bound extended from six for harness defects); final `8712866` run `38508454070`, digest `c7b9b68d…d999`. `rtk proxy bun tools/windows-sparse-backend/verify-evidence.mjs` passes and rejects a tampered raw file.
- [x] Independent read-only review; fix material findings; report verdicts to hub. Codex round hit its usage limit; Claude Opus 5.5 high reviewed `fdd0f4b` (same family). One high and three medium findings fixed and rerun in attempt 8; remaining lows documented.
- [x] Bounded fixture VERIFIED on Cloud Files: rename matrix, native ordinary ops, ACL base and frozen snapshot (including the frozen root itself), two clones of one snapshot, mid-hydration and mid-write kills vs closed-handle preservation, dehydrated untouched peers. ProjFS base-directory rename refused (error 50, no PRE_RENAME).
- [ ] Unchanged common probe: CRLF compiler, Bun directory fsync EPERM, final integrity and path boundary NOT VERIFIED (probe portability; not owned here).
- [ ] INCONCLUSIVE: storage cost at scale, metadata preservation, nested symlinks, AV hydration, power loss, concurrency, provider-only kill during a write, capture of clones, production registration, native binding, autosave, Windows acceptance, Gate 2, GA.
