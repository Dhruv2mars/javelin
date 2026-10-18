# Javelin build checklist

This file is the central hub's master checklist. A checked item requires evidence.
Implementation threads own one PR each and keep their own checklist in their worktree.

## Current working rules, updated 2026-10-16

These rules supersede the older draft-first and multi-round review instructions
in the historical workstream sections below.

- Implement in one owning thread and isolated worktree, run relevant checks,
  open and link a regular PR when ready, report briefly, then finish.
- No draft or planning PR requirement, routine extensive review/audit rounds,
  evidence-only finishing commits or formal handoffs.
- Keep implementation and coordination simple. Use parallel help only where it
  materially advances the task. Do not add abstractions or process without need.
- Keep T3 organized, reuse existing owners, and settle completed threads.
- Keep meaningful behavior checks and honest limits. Never merge without the
  user's explicit instruction.
- The user resumed development on 2026-10-16 and explicitly authorized merging
  ready PRs so progress is visible in the repository. The hub integrates changes
  in dependency order; owners finish their existing tasks with relevant checks.

## Resumed development and integration on 2026-10-16

Done for this integration pass means completed changes are merged in dependency
order, unfinished owners are active in their existing worktrees, and the default
branch contains checked working components. This does not establish GA.

- [x] Resume the six existing unfinished owners without duplicate threads.
- [ ] Merge the build plan and completed native dependency chain.
- [ ] Integrate other finished components with relevant checks.
- [ ] Complete whole-Layer Publish, then connect the local developer workflow.
  - [x] Make Publish incremental: snapshot prepare, short compare-and-swap write
    transaction, full validation only in `check` (write transaction 521 VM
    instructions at 1 and 1,000 prior Worlds; main took 16 ms → 611 ms under the lock).
  - [ ] Store Worlds as changesets (`base(V+1)` = V + changeset) instead of full
    manifests. Deferred: it changes the version schema and the protocol.

## Historical user pause for recalibration on 2026-10-15

The user explicitly requested stopping all work to review progress and the
development plan. This pause supersedes the continuation instructions and the
historical next actions below. Do not resume development, reviews, automatic
continuation or PR watchers without an explicit user instruction to resume.

Done for this pause means the scheduler is disabled, PR watchers are released,
workers and child reviews have stopped after necessary cleanup, and existing
code, evidence and draft PRs remain intact.

- [x] Disable the existing continuation schedule without deleting it.
- [x] Release all active implementation PR watchers and confirm the hub watcher is off.
- [x] Interrupt live implementation and review turns; notify the six unfinished
  owners to perform cleanup only and cancel their own resumable child tasks.
- [x] Confirm cleanup turns have ended and the remaining cloud run is stopped.
  All six unfinished owners acknowledged the pause with no active or pending
  turns. The project active-thread query contains only this hub, and GitHub
  reports no in-progress run after cancellation of `38739503224`.
- [x] Recalibrate with the user. The 2026-10-16 instructions simplify the PR flow
  and explicitly resume work with ready PR merges.

Verified components remain accepted only within their recorded scope. Twenty-one
marathon PRs, #3 through #23, are open drafts and unmerged at this pause.
The current product still lacks a complete local developer workflow and hosted
collaboration. Proposed next steps are discussion material, not authorization to
start more work.

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
- [x] Correct the platform decision's provenance: the review proposed the three-OS
  gate; the user explicitly chose "All three platforms for initial GA" in this
  hub on 2026-10-14, superseding the earlier Q61 macOS-first direction.

## Prove the local VCS

- [ ] Define native objects, World versions, Layers, checkpoints, and atomic Publish.
- [ ] Implement initialization, durable immutable snapshots, history, and restore.
- [x] Choose the Layer workspace approach with fresh measurements on all three
  OSes: plain directories cloned from an immutable World base, daemonless capture
  (`docs/workspace-approach.md`, gate 2 amended 2026-10-17).
- [ ] Implement clone-backed Layers: APFS directory clone, btrfs snapshot, per-file clone
  (XFS, ReFS) and a reported copy fallback, with deferred discard. See
  "Clone-backed Layers" under Current work.
- [ ] Make per-call state validation incremental. `manifest_in` hashes every blob on
  each validated call, about 5 s at 100k files and 661 MB, and that dominates
  `workspace create`. The clone itself takes about 1.2 s.
- [ ] Capture automatically at every Javelin command and supplied agent lifecycle
  event, with generated operation IDs and the stat-cache racy-entry rule.
  - [x] Daemonless capture of plain Layer directories (`src/capture.rs`): stat cache,
    racy rule with per-entry trust, stable reads, atomic checksummed cache, generated
    idempotent operation IDs, and the `javelin checkpoint` lifecycle hook.
  - [x] Measure the real capture path at 100k files (`examples/capture_bench.rs`).
  - [x] Capture the clone-backed workspace first in every `layer` and `workspace`
    command that names a Layer; never capture over a Stack-produced head.
  - [ ] Refresh a clone-backed directory from a Stack-produced head (today capture
    refuses with `behind` until this exists).
  - [x] Apply root `.javelinignore` rules (PR #36's `IgnoreRules`) to untracked paths.
  - [ ] Make repository validation incremental: every call re-reads and re-hashes
    every blob of every World and checkpoint, so a 100k-file checkpoint commit costs
    seconds, not the scan (see docs/workspace-approach.md, "Capture latency").
- [ ] Prove ordinary filesystem behavior for clone-backed Layers (gate 2).
  The proof (`tools/workspace-probe/gate2.mjs`, `.github/workflows/gate2.yml`, PR #46)
  passes 34/34 with the real binary on APFS, btrfs, XFS and ext4 (copy fallback) in run
  `39113744899` at `d455ff2`. NTFS and ReFS pass 33/34: Windows capture misses a
  same-size rewrite that restores mtime (no ChangeTime in the stat cache), listed in
  `gate2-known-failures.json`. Check this item when that list is empty. Worlds are seeded
  through the library because clone-backed workspaces have no Publish yet.
- [x] Retire the macOS and Windows sparse-backend proof tracks (2026-10-17).
- [ ] Retire the OverlayFS helper once clone-backed Layers reach parity.
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
- [ ] Verify security and correctness with appropriate checks and fix material defects.
- [ ] Set measured release limits and performance requirements before claiming scale.
- [ ] Commit fixed workload shapes and numeric pass/fail thresholds before the
  release acceptance measurements. Baselines alone cannot pass Gate 6.
- [ ] Run the full local and hosted acceptance suite against release artifacts.
- [ ] Publish versioned packages and release artifacts with checksums and provenance.
- [ ] Verify the public release as a new developer and record GA evidence.

## Current work

### Clone-backed Layers (feat/clone-layers)

Done means a Layer is a plain directory cloned from a sealed World base, with
the strategy found by trial and reported; discard is deferred and crash-safe;
collisions are rejected; and CI exercises APFS, btrfs, XFS, ext4, ReFS and NTFS.

- [x] `src/workspace.rs` and `src/workspace/clone.rs`: bases, the bounded cache,
  the lock, recovery, bounded trash, collision checks and per-OS clone primitives.
- [x] Measure the read-only base → writable Layer path on APFS. Choose directory
  `clonefile` with a root-only seal (`docs/workspace-approach.md`).
- [x] ReFS rule: always clone where copy-on-write exists, through `CopyFile`
  verified by shared clusters. Docs aligned with the contract.
- [x] CLI `workspace create|open|path|discard|recover` without `--helper`.
- [x] Tests: tree equality, isolation, derived bases, checkpoint apply, discard and
  recovery, collisions, and exact data sharing (APFS private size, FIEMAP).
- [x] Local APFS, 100k files: warm clone 1.15–1.21 s, 0 bytes copied.
- [ ] CI green on all six filesystems.
- [x] Wire capture into every workspace and layer command, whichever of this PR
  and daemonless capture merges second (done in daemonless capture).

Detailed per-workstream logs were removed on 2026-10-17; merged PRs hold that
history. Current state is in `docs/architecture.md` and `docs/workspace-approach.md`.

### Workspace ignore rules (PR #36)

Done means the rebased PR exposes a pure matcher usable by clone-backed capture,
preserves tracked entries and private outputs, passes workspace checks and required
CI, and is merged.

- [x] Rebase onto main's structure and consolidate the nested checklist here.
- [x] Preserve bounded literal rules, current-checkpoint tracking and private bytes;
  retain capture/watch, Publish/rebind and recovery regressions.
- [x] Expose the platform-independent matcher and remove duplicated runtime/CLI test helpers.
- [x] Repair references to deleted evidence and documentation.
- [x] Pass local workspace formatting, warnings-denied Clippy and tests (212 passed,
  one ignored actor), with a validated failing Git shim and no invocation marker.

PR #36 tracks required CI, review handling and the final merge through T3 monitoring.
