# Javelin build checklist

This file is the central hub's master checklist. A checked item requires evidence.
Implementation threads own one PR each and keep their own checklist in their worktree.

## Direction of 2026-10-26

`docs/direction.md` records the user's answers: two products (CLI and the javelin.codes
forge), no managed hosting, superposition visibility, Layers that follow their owner, and
CI left open. Work from it, in roughly this order:

- [x] Clean up Git: merge #92 and #93, close #2, archive the Plan A branches as
  `archive/*` tags, delete every merged branch.
- [x] Record the direction and amend the contract, the online design, the Git 3.0
  direction, README and AGENTS.md.
- [x] Token transport: bearer tokens on the wire, outbox refresh through the control
  plane, network control-plane client, `node_unavailable` for 5xx.
- [x] History verification proportional to new versions (trust anchor).
- [x] Forge read-only pages: repo home, file, history, version diff (`apps/forge/`).
- [x] First real deploy of the Cloudflare node on the owner's account, free tier only;
  settle every unverified free-tier limit.
- [x] Superposition storage format: salted commitments for hidden tree entries, identical
  IDs for every reader. Must land before any real repository is stored. Format 10,
  `.javelinpolicy`, `Open`/`Opened`; tests in `tests/superposition.rs`,
  `crates/javelin-protocol/tests/superposition.rs`, `crates/javelin-node/tests/fork_hidden.rs`.
  - [x] Wire the caller's role into the node's reads and `check_writer` into its writes
    (`crates/javelin-node/tests/wire_hidden.rs`).
  - [x] Authorize Layer object reads, so an unpublished hidden edit cannot be confirmed by ID
    (uploads are recorded per principal; `wire_hidden.rs`).
  - [ ] Apply named groups from the control plane.
- [ ] Path audiences, disclosure delay, embargo and the public frontier on the node, CLI
  and forge.
  - [x] Disclosure delay, embargo and the public frontier on the node (`disclosure.rs`,
    format 11 `object_access.since`), CLI (`javelin frontier`, `delay`, `embargo`) and forge
    (frontier as the head, "delayed" notice). Tests: `crates/javelin-node/tests/wire_frontier.rs`,
    `apps/forge/test/frontier.test.ts`. macOS only; Linux and Windows unverified.
  - [x] Trail 3's `ListRequests`, `ReadReview` and `LayerCreate` refuse callers without a
    role before any lookup; signed-out `attach` and `sync` stop at the frontier
    (`wire_frontier.rs`, `crates/javelin-cli/tests/frontier.rs`).
  - [ ] Outside contributors (signed in, no role) are refused every Layer, Publish Request and
    check message so none can name a held version. Let them reach their own Layers, based
    at or below the frontier, once a Layer records who created it.
  - [ ] `--embargo` on `javelin request publish`; until then `javelin embargo --operation ID`
    precedes the Publish.
- [x] Node budget: refuse with `quota_exceeded` before a write crosses a free-tier limit
  (feat/node-budget). `budget.json` (storage bytes, rows written per UTC day, largest object;
  Free-plan defaults), enforced in `wire::handle` before any state changes on both nodes;
  `ReadUsage`/`Usage` and numbers in the refusal for Maintain only; `PUT /admin/r/{repo}/budget`
  on Cloudflare; `javelin node usage`. Evidence: parity budget scenario on `Directory` and
  `PageStorage`, conformance `run_budget` on loopback and `wrangler dev`, `crates/javelin-cli/
  tests/budget.rs`.
  - [ ] Budget rows across repositories on one Cloudflare account (the daily quota is per
    account; each repository counts only its own), and admin pushes.
  - [ ] A CLI for the owner to change the budget (today: the admin route, or `budget.json`
    and a restart on a self-hosted node).
- [x] Layer writer lease and `javelin layer take` (replaces D21).
- [x] The pull request loop from the CLI: `javelin request open|revise|comment|approve|show|
  list|publish` over the wire with operation IDs, and D7 queueing (revise and publish queue,
  open, approve and comment refuse). New wire messages `LayerCreate`, `ListRequests` and
  `ReadReview`; `open` and `revise` upload the Layer's changes to the node. Evidence:
  `crates/javelin-cli/tests/requests_sync.rs` against the node's server with real tokens.
- [x] `javelin sync`: replays the outbox, fetches from the attached head as trust anchor,
  advances the local World and rolling base, rewrites `attach.json`; refuses
  `local_world_diverged` and a chain that misses the anchor (`chain_mismatch`).
- [x] Requests and sync under #99's audiences, and #101's guard shared: `javelin sync` and
  `layer take` follow the node through one path (`cli_sync::follow`: `fetch_opened` from the
  attached head, openings stored as attach stores them, `follows_attachment` widened to any
  attach or sync World with the attached tree and no Publish receipt, node-confirmed
  recovery of an interrupted sync). `ReviewDetail` drops the plain snapshot tree and sends a
  revision's checkpoint only when the caller may read it. Evidence: `requests_sync.rs`
  `sync_and_request_show_follow_the_callers_audiences`,
  `a_sync_stopped_before_attach_json_is_finished_by_the_next_sync`, `layer_lease.rs`.
- [x] Checkpoint upload on capture (U5's CLI call site): every capture on an attached Layer
  queues its checkpoint in the outbox and delivers it within the 2 s budget; outages queue and
  replay on the next capture or `javelin sync`; only the lease holder uploads (`layer_stale`
  otherwise); `forbidden` uploads are decided, not queued forever; `javelin status` reports
  each Layer. Evidence: `crates/javelin-cli/tests/capture_upload.rs` against the node's server
  with real tokens.
  - [ ] The bytes of a path outside the writer's audiences reach the node before the append is
    refused (served to nobody outside the audience). A client-side check would need the
    caller's audiences.
- [x] One Rust node core on Cloudflare through wasm, replacing the TypeScript Worker
  (`feat/node-core-wasm`). `javelin` without `fs` and `javelin-node` without `native` build
  for wasm32; `Storage`/`Coordination` in `crates/javelin-node/src/storage.rs`; the Durable
  Object runs the core with SQLite on a page-row VFS (`crates/javelin-node-cloudflare`).
  Conformance (admin + protocol v2) passes on the self-hosted node, `wrangler dev` and the
  deployed Worker; parity test in `crates/javelin-node-cloudflare/tests/parity.rs`; Free-plan
  measurements in `tools/node-cloudflare/README.md`. TypeScript node and `tools/spike-do` deleted.
  - [x] Requests and sync (trail 3) and the public frontier (trail 19) folded into the core:
    request reads, delay, embargo and release, frontier filtering and `LayerCreate` run in
    the Durable Object; parity and the wire conformance cover a request loop and a held
    version.
  - [ ] Rerun the hosted acceptance `target_*` now that `LayerCreate` reaches a remote node.
  - [ ] Fork on the Cloudflare node (`NodeLink::fork` across two objects).
- [ ] Forge connects a provider account through OAuth and provisions the node there.
- [ ] Forge on javelin.codes once the domain is bought.
- [x] Forge live on Vercel at https://javelin-codes.vercel.app (Hobby, CLI deploys with
  `bun run deploy` in `apps/forge` from `main`): landing page at `/` with install status
  (installers coming) and open-by-node-URL, SPA fallback and security headers. Proved
  against a tunneled `javelin-node serve`.
- [x] CI for building Javelin: Buildkite (free `linux-small` queue) runs fmt, clippy and tests
  on every push and reports to the trail as a gate. Decided 2026-10-28.
- [ ] CI inside Javelin for its users: not decided yet (the user's call). No work planned.
- [x] Gate 2 failures of 2026-10-26 (#96 xfs discard, #98 apfs concurrent capture):
  the first was a product gap. A Layer discarded or published, then killed before the
  trash move, kept a live directory until a reclaiming command ran. The next command
  naming the Layer now finishes the move. The second was a probe timing assumption. The
  concurrent-rewrite check now waits for 4 distinct checkpoints, not a fixed 3 s window.
- [x] Gate 2 `crash/interrupted-create-recovers` assumes timing too. Its kill points came
  from the fastest of 3 timed creates. One of 6 local APFS runs timed a 2.8 s create (usual
  ~50 ms), so all 7 kills landed after exit: "only 0 kills landed". Now it kills at observed
  states: held at the store lock with its Layer recorded (`gate2_seed --hold-store`), and
  after its directory appears but before exit. Trail https://entire.com/et/dhruv2mars/-/trails/14.
  APFS: 9 of 9 runs VERIFIED, including slowed creates; the old check reproduced "only 0
  kills landed" with slow timing creates. Linux: Buildkite runs the crash clause. Windows:
  not run.

## Move to Entire (2026-10-27)

GitHub suspended the Dhruv2mars account on 2026-10-27. The repository moved to Entire
with every branch intact; trails replace PRs, Sol is the only reviewer, and checks run
locally on macOS. Since 2026-10-27 Buildkite runs fmt, clippy and tests on Linux for
every push (`.buildkite/pipeline.yml`).

- [x] Point `AGENTS.md` and the contract at trails and local checks.
- [x] Land the open work as trails. All merged on 2026-10-28 after a green Buildkite build,
  local macOS checks on the merge result and a Sol approval: #99 superposition (trail 7),
  #101 Layer lease (trail 5), forge on Vercel (trail 6), Gate 2 create probe (trail 14), CLI
  requests and sync (trail 3), public frontier (trail 19), node core on wasm (trail 18).
- [ ] End-to-end daily-workflow acceptance: two clones, Layers, request, review, publish and
  sync against a self-hosted node (`test/daily-workflow-acceptance`, in progress).
- [ ] `tests/native_object_access.rs::concurrent_row_replacement_never_mixes_metadata_and_payload`
  flaked once on Linux (Buildkite build 25 on main `686a470`: `repo.has_blob(id).is_ok()`
  failed); the rebuild (32) and the same commit's branch build (22) passed. Find what
  `has_object` returns there.
- [x] Forge deploys to Vercel from the CLI (`bun run deploy` in `apps/forge` from `main`),
  since Vercel's Git integration cannot follow Entire. The landing page says installers are
  coming instead of pointing at GitHub releases. Production redeployed from `c675585`.
- [ ] Release binaries on npm (decided 2026-10-28; GitHub Releases is gone and Entire hosts no
  release files): wrapper `@dhruv2mars/javelin` plus one package per platform, `npm i -g` or
  `bun add -g`, and a curl installer that reads the same tarballs from `registry.npmjs.org`
  (trail 23, `feat/release-install`). The first real publish waits for the user's go-ahead
  and their npm login.
- [x] GA platforms are macOS and Linux; Windows ships as experimental and untested until a
  runner exists. Decided 2026-10-28.
- [ ] Windows has no runner. Buildkite runs Gate 2's crash clause on Linux (trail 14); the rest
  of Gate 2 and the workspace probes run nowhere automatically. The Windows `database is locked` flake under
  concurrent Layer creates is parked: its traced run `40442668503` is unreadable, and
  the cause is unconfirmed (lead: slow journal create/delete on a loaded Windows disk).
- [ ] Delete the temporary `diag/windows-layer-busy` branch from origin once the parked
  Windows flake no longer needs its tracing. Merged branches were deleted on 2026-10-28.
- [ ] `.github/workflows/` no longer runs. Buildkite covers the Linux Rust job; port or
  drop the rest (forge, node, workspace probes, cargo deny) and then delete it.

## Merge rule for flaky tests (2026-10-28)

A trail may merge when its Buildkite build is green and every local macOS failure also
reproduces on main. Known flakes (`fix/flaky-tests`): node server connection resets under load
(`loopback.rs`, `wire_auth.rs`, `wire_hidden.rs`), duplicate sends in `outbox_replay.rs`, and
`attach.rs`'s fake node.

## Daily workflow acceptance (2026-10-28)

- [x] GA's promise in one script: `crates/javelin-cli/tests/daily_workflow.rs` drives the real
  `javelin` and `javelin-node serve` processes with real tokens (owner signed in through
  `javelin control serve`, collaborator on a key the node trusts): import to World 1, request,
  comment, approve, revise (old approval stops counting), node restart with a queued Publish,
  conflict and recovery, `layer take` to a second device, every clone at the node's World with
  identical files. `tools/acceptance/daily-workflow.sh` is the same loop for any node URL;
  the test runs it too (Unix). Linked from `docs/cli.md`. Windows: not run.

Rough edges hit on the way. Fixed in this trail:

- [x] `javelin attach URL DIR` into a missing DIR failed with a bare `filesystem error`; it now
  makes the directory once the node has answered.
- [x] A revise after `layer take` moved the Layer away reported `missing_object` and told you to
  `layer refresh`; it now reports `layer_stale` with the `layer clone --checkpoint` that keeps
  the work.
- [x] A conflicting `request publish` said "the author revises the request over them" without
  saying how; it now names `sync`, a new Layer, and `request revise ID --revision N --layer NEW --operation OP`.
- [x] `history` and `check` took only a positional directory; they accept `--repo DIR` too.
- [x] `repo create` did not say how to serve the new self-hosted repository; it prints the
  `javelin-node serve` line and the attach URL.

Open:

- [ ] No membership: the control plane serves repository tokens only to the creator and admin,
  and nothing grants a second person a role. The test invites by writing `permissions.json`
  and `keys.json` into the stopped node's directory and signing the collaborator's tokens with
  the test key. Belongs with the forge's accounts and members.
- [ ] A serving `javelin-node` reads permissions and trusted keys only at start, so a role
  change needs a node restart.
- [ ] `javelin login` approved with the admin token signs the device in as principal `admin`; a
  second person needs the browser passkey page, and then still has no role (above).
- [ ] No direct Publish to the node from the CLI. Importing a project means opening a request
  of your own and publishing it, which on a repository that requires approvals needs someone
  else's approval first. `workspace publish` in an attached clone publishes locally and then
  `sync` refuses `local_world_diverged`.
- [ ] No import into an attached repository: `adopt` makes a new local repository only.
- [ ] Local World numbers and IDs differ between clones of the same repository (one remote World
  5 was local World 4, 5 and 2 in three clones). `history` and `workspace create` ("Base World
  N") print local numbers; only `sync`, `attach` and request output show the node's.
- [ ] `request show` marks every changed path `M`, added files included: the node's sparse
  changes do not say whether the base had the path.
- [ ] A Layer whose request was published on the node stays active in the clone that opened it,
  with its directory. `layer radar` keeps listing it as a live Layer that would conflict.
- [ ] No way to move a Layer onto a newer World after a conflict. `layer refresh` follows only a
  node-side Stack; the author redoes the change in a new Layer by hand.
- [ ] Queuing a Publish of someone else's request while the node is down needs `--base WORLD`,
  read from `request show` before the outage.
- [ ] `layer take` works only for a Layer the node has seen, which today means one a request was
  opened from (see "Checkpoint upload on capture" above).
- [ ] The attach URL names the repository by its opaque ID (`repo_…`), not `acme/app`.
- [ ] A stale append stays listed under `outbox status` `stale` after its work was kept with
  `layer clone`; nothing clears it.

## Working rules

Working rules live in `AGENTS.md` (`CLAUDE.md` imports it). They supersede the older
draft-first and multi-round review instructions in the historical sections below.

- [x] Make `AGENTS.md` the single repo rule file for Codex and Claude Code, keeping only
  Javelin-specific rules that the global instruction files do not already cover.

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
  - [x] Store Worlds as changesets (`base(V+1)` = V + changeset) instead of full
    manifests (PR #47, format 6).

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
- [x] Make per-call state validation incremental. `manifest_in` hashes every blob on
  each validated call, about 5 s at 100k files and 661 MB, and that dominates
  `workspace create`. The clone itself takes about 1.2 s. Done: commands validate only
  what they touch (docs/architecture.md, Validation scope); `workspace create` was the
  last command reaching full validation (`history()` for the head number) and now reads
  the head row.
- [ ] Capture automatically at every Javelin command and supplied agent lifecycle
  event, with generated operation IDs and the stat-cache racy-entry rule.
  - [x] Daemonless capture of plain Layer directories (`src/capture.rs`): stat cache,
    racy rule with per-entry trust, stable reads, atomic checksummed cache, generated
    idempotent operation IDs, and the `javelin checkpoint` lifecycle hook.
  - [x] Measure the real capture path at 100k files (`examples/capture_bench.rs`).
  - [x] Capture the clone-backed workspace first in every `layer` and `workspace`
    command that names a Layer; never capture over a Stack-produced head.
  - [x] Refresh a clone-backed directory from a Stack-produced head (`workspace refresh`
    three-way merges local edits; conflicts change nothing).
  - [x] Refresh refuses head paths that an ignored or case-aliased file occupies, flushes
    before the sync record moves, and lets `checkpoint` capture a synced stacked head.
  - [x] Refresh refuses head paths that alias each other on the volume (case or Unicode
    normalization, probed on the volume), applies head deletions before additions so a file
    can become a directory, syncs only directories that still exist, and a capture through a
    relative path to the workspace moves its sync record.
  - [x] Refresh discovers collisions caused by conflicting head writes before validating
    choices, so a local file blocking a head write resolves through either side, a tracked
    alias resolves through both spellings, and an explicit overwrite replaces an ignored file.
  - [x] Apply root `.javelinignore` rules (PR #36's `IgnoreRules`) to untracked paths.
  - [x] Make repository validation incremental: every call re-reads and re-hashes
    every blob of every World and checkpoint, so a 100k-file checkpoint commit costs
    seconds, not the scan (see docs/workspace-approach.md, "Capture latency"). Done: full
    validation runs only in `check`, `history` and `init` on an existing store.
- [x] Prove ordinary filesystem behavior for clone-backed Layers (gate 2).
  The proof (`tools/workspace-probe/gate2.mjs`, `.github/workflows/gate2.yml`, PRs #46 and #57)
  passes 34/34 with the real binary on APFS, btrfs, XFS, ext4 (copy fallback), NTFS and ReFS
  in run `39438052570` at `39653a6`. Windows capture now reads ChangeTime (PR #57), which
  closed the last failure, so `gate2-known-failures.json` is `{}`. Worlds are seeded
  through the library; `workspace publish` now exists, so the probe may switch to it.
- [x] Retire the macOS and Windows sparse-backend proof tracks (2026-10-17).
- [x] Retire the OverlayFS helper once clone-backed Layers reach parity (2026-10-18,
  PR `refactor/retire-overlayfs`). Clone-backed Layers are the only runtime.
- [x] Clone-backed `workspace publish`: capture, then Publish the whole Layer, with
  exact retries; refuses busy, uncertain or `behind` captures.
- [x] Publish from a workspace whose head came from Stack (after `workspace refresh`).
- [x] Reclaim a published Layer's directory; a successful Publish reclaims it, and recovery finishes interrupted reclaims.
- [x] Independently verify committed filesystem-probe controls and negative cases
  at `0f15db2`. This verifies the probe only, not a sparse Layer backend.
- [x] Rerun final probe source `bce352e`, including termination of a stalled external
  command group and rejection of hardlinked or FIFO report destinations.
- [ ] Implement Layer creation, cloning, checkpointing, discard, status, and diff.
- [x] Verify the bounded native Layer state API, immutable sparse checkpoints,
  reference clones, retained history and exact retries. Ordinary-path binding and
  automatic capture remain separate unchecked requirements.
- [x] Adopt existing non-Git project files through a Layer without accepting edits outside it. (PR #61)
- [x] Implement Stack, refresh, structural conflicts, and explicit resolution.
- [x] Bind supplied agent traces to immutable work, with missing-trace override.
- [x] Prove process concurrency, crash recovery, retries, and corruption detection.
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
- [ ] Implement attach and independent forks with permitted inherited history.
- [ ] Verify public upstream contribution without requiring a hosted fork.
- [ ] Verify control-plane outage behavior and provider-outage local work.
- [ ] Resolve CI execution design and verify the accepted workflow syntax.
  - [x] Design written: `docs/ci-execution.md` (PR on `docs/ci-execution`), with examples in `docs/examples/ci/`.
  - [x] Hub confirms the dialect: GitHub Actions YAML (CI1, confirmed 2026-10-21). The contract accepts "CI workflow syntax compatibility" without naming one.
  - [x] Hub adds U17 (check ingestion and required-check gate) and U18 (validator) to the unit table.
  - [x] U17 check ingestion and required-check gate (`docs/ci-execution.md` §11, tests 1–15): `CheckReport` 0x60 and `CheckOutcome` 0x61, name-pattern tokens (`checks:report`), tree binding, required-check gate in `PublishLayer` and `PublishRequest`, `javelin check report`.
  - [x] `javelin check report --node URL` posts to a serving node over the wire with the
    reporter token (`JAVELIN_TOKEN`) as `Authorization: Bearer`; the node's gate checks it
    (`crates/javelin-cli/tests/wire_credentials.rs`, `crates/javelin-node/tests/wire_auth.rs`).
  - [x] U18 validator (`tools/ci-validate/`) passes examples 01–07, rejects 08–09 with the expected codes and rejects duplicate-key fixture 10. `validator_fixtures_match_readme` runs in the CI Rust job on all three OSes.
- [ ] Measure hot-repository contention, many repositories, transfer costs, and quotas.

## Deliver OSS GA

- [ ] Verify installation and upgrades from clean machines on supported platforms.
  - [x] Release workflow builds 5 targets, attests, and dry-runs on the branch (`release.yml`).
  - [x] Install scripts verify SHA-256 before writing; corrupted archive is refused.
  - [x] Clean-machine job per target: install previous build, seed repo, upgrade, reopen, smoke, no Git invoked.
  - [x] Windows `install.ps1` and PowerShell path verified on `windows-2025`.
  - [x] Tag-triggered pre-release publish: `v0.1.0-alpha.1` published 2026-10-21 with the user's go-ahead (5 targets, attested, clean-machine checked; fixes #83, #85).
- [ ] Publish tutorials, command reference, hosting setup, and recovery documentation.
- [x] Add contribution guidance, support policy, security reporting, and license checks
  (PR #56; `licenses.yml` run `39434261246` passed at `51fd153`). Private vulnerability
  reporting still needs enabling in repo settings.
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
- [x] CLI `workspace create|open|path|discard|publish|recover`.
- [x] Tests: tree equality, isolation, derived bases, checkpoint apply, discard and
  recovery, collisions, and exact data sharing (APFS private size, FIEMAP).
- [x] Local APFS, 100k files: warm clone 1.15–1.21 s, 0 bytes copied.
- [x] CI green on all six filesystems (run `39438057484` at `39653a6`, Workspace jobs apfs, ext4, xfs, btrfs, ntfs, refs).
- [x] Make the settled-scan capture test independent of wall-clock time: a slow Windows runner
  (run `39436475593`) passed the racy window before the second scan (PR #57).
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

### Tree objects as derived data (feat/tree-objects)

Done means: World versions carry a derived Merkle root computed eagerly at
Publish in O(change x depth); directories are prolly trees with name-determined
chunk boundaries; snapshots, base derivation and conflict checks read trees, not
history; `check()` recomputes every root from changesets; a randomized test
proves history independence; differential tests match the flat-manifest
reference; the bench reports the required numbers; fmt, clippy (host and Linux
zig cross-check) and all tests pass; CI is green; the protocol moves to trees in
a stacked PR without the 65,536-entry cap.

- [x] `src/tree.rs`: canonical chunk encoding (`ObjectKind::Tree`), prolly
  chunking with a size-aware boundary rule (mean 2 KiB), incremental apply,
  lookup, walk, diff, tree intersections.
- [x] Format 7: `versions.tree`, version id covers the root, eager derivation in
  Publish (computed in prepare, written in the short transaction), `attempts.tree`.
- [x] Snapshots, Layer normalize/diff, compose and workspace bases read trees.
- [x] `check()`/`history()` recompute every root and verify chunk presence.
- [x] Tests (`tests/tree_objects.rs`): history independence over shuffled edits
  and random batches in 6,000-entry directories, differential snapshot, diff,
  lookup and intersection checks against flat snapshots, subtree skipping,
  refusals, corruption detection.
- [x] Bench at 100k and synthetic 1M files (`docs/architecture.md`, Measurements):
  1-file change 0.03 ms / 5.6 KB (100k) and 0.035 ms / 7.0 KB (1M); one edit in a
  50k-entry directory 6.5 KB; snapshot flat at 76–86 ms over 10–300 versions x
  1,000-file churn where format 6 grew to 176 ms; Publish unchanged at 14–15 ms.
- [x] Docs (`architecture.md`).
- [x] Stacked PR: protocol v2 on trees. Have/want by hash, pruned at any
  chunk the receiver holds (a stored chunk implies its closure); root-down fetch
  with sparse prefix fetch; per-object bounds (`MAX_TREE`, `MAX_BLOB`,
  `MAX_OBJECTS`) replace `MAX_ENTRIES`; Publish of a tree root verifies the
  changed part and canonical form (`advance_to_tree`). A 70,000-entry snapshot
  transfers and publishes.

## Git 3.0 roadmap, accepted 2026-10-18

The user accepted the Git 3.0 direction (`docs/git3-direction.md`): online-first
and outage-tolerant, Bonsai changesets with derived trees, storage split by
primitive, one rolling base, and no local kernel VFS. These items are in
dependency order. Each is done when its PR is merged with the checks it names.

- [x] Changeset World (shipped as format 6): O(change) Publish and shallow hot-path
  validation (PR #47).
- [x] Record the Git 3.0 decision and amend the contract and workspace docs (PR #45).
- [x] Storage benchmark, blob files vs SQLite (PR #50; APFS no-go): apply the go/no-go
  criterion recorded in `docs/git3-direction.md`.
- [x] Tree objects as derived data: Merkle trees with prolly chunking, then the
  protocol on trees, which drops the 65,536-entry `MAX_ENTRIES` cap (PRs #51 and #52;
  measurements in `docs/architecture.md`).
- [x] Rolling base: one base per repository per machine, advanced in place under
  a short lock; Layer create is lock, clone, unlock.
  - [ ] Measure rewinding a Layer far behind the base (reverse changeset, O(distance)) at 100k files; add a bound or a cold fallback if it is slow.
  - [ ] Process-kill tests for an advance and a reclaim; the current tests seed partial state instead of killing a process.
- [x] Collision radar (PR #48, format 8 after #51).
- [x] Trace blame (PR #49, format 8 on main after #48).
  - Evidence: checkpoint provenance
    from `JAVELIN_*`/flags, per-path attribution through Publish into `blame`,
    `javelin blame PATH [--lines]`, rebuilt and compared by `check()`. Evidence:
    multi-Layer, multi-session, rename, conflict-then-resolve, clone and Stack tests;
    path blame 0.015 ms flat, line blame 63 ms at 1,000 versions of a 1,000-line file.
  - [ ] Follow-up: a cross-platform `layer publish` CLI for clone-backed Layers
    (Publish is library-only off Linux, so CLI blame needs it end to end).
- [ ] Online pieces: attach, streaming checkpoints, server-side compose, outage
  queue and replay, and the World log (one ordering point per repository since
  `docs/direction.md`, 2026-10-26).
  Note 2026-10-20: decisions D1, D2, D5 and D23 are recorded in `docs/online-design.md`
  section 7; Free-plan limits stay unverified until U13.
- [ ] Later, not GA: optional `javelin mount` (FUSE, lazy fetch) for cloud agent
  runners and Linux CI.

## GA hub plan, started 2026-10-20

The user asked for every remaining phase at full power. The GA hub thread runs
owner threads on `claude-haiku-5-5` (high effort) on Claude, one PR per thread and worktree, and
merges green PRs in dependency order. Delegates and computer use are Haiku.
A 30-minute continuation runs in the hub.

Wave 1, done:
- [x] Finish the storage benchmark with the Linux verdicts (#50).
- [x] Rebase and land tree objects (#51), then protocol v2 on trees (#52); fetch
  validation (#69).
- [x] Windows ChangeTime in the stat cache: gate 2 34/34 on all six filesystems (#57).
- [x] OSS hygiene: CONTRIBUTING, SECURITY, SUPPORT, cargo-deny license check (#56);
  private vulnerability reporting enabled.
- [x] `docs/online-design.md`: online and hosting design, PR-sized units, open decisions
  (#58, decisions #60; CI execution design #59).

Wave 2, commissioned 2026-10-20 after #51 merged (radar and blame; rolling base and
reclaim; Stack-head refresh and Publish; robustness proofs):
- [x] Rebase collision radar (#48) and trace blame (#49) onto main as format 8.
- [x] Rolling base: one base per repository per machine; Layer create is lock, clone, unlock
  (#66).
- [x] Refresh a clone-backed directory from a Stack-produced head, then Publish from it.
- [x] Reclaim a published Layer's directory (#66).
- [x] Process concurrency, crash recovery, retries and corruption proofs.

Wave 3, after wave 2:
- [x] Stack, refresh, structural conflicts and explicit resolution end to end.
- [x] Adopt existing non-Git project files through a Layer. (PR #61)
- [x] Bind agent traces to immutable work, with a missing-trace override (#68).
- [ ] Online units from `docs/online-design.md`, in its order.
  - [x] U1 protocol v2 hosting messages (#67)
  - [x] U2 node core: file-backed World log, loopback transport, idempotent PublishLayer.
  - [x] U4 attach: `.javelin/attach.json` written last, full fetch into the rolling base, refusal on local World history.
  - [x] U5 streaming checkpoints: captures upload changes and blobs, `LayerAppend` appends them with idempotent replay and stale refusal, `LayerHead` reads the node head (`crates/javelin-node/tests/checkpoints.rs`).
  - [x] U6 server-side Stack and refresh from head: `LayerStack` on the node with the local engine and limits (golden test pinned), conflicts and `merge_limit` refusals that write nothing, stale `target_head` then retry, checkpoint `Get`, `javelin layer refresh` (`crates/javelin-node/tests/stack.rs`, `crates/javelin-cli/tests/layer_refresh.rs`).
  - [x] U3 client outbox: checksummed append-only journal, replay in journal order, fault-injection proxy against the loopback node.
  - [x] U7 scoped tokens, applied permissions, epochs and revocation on the node
    (`javelin-identity`, `javelin-node` auth gate, test keys). Authenticated control-plane
    pushes remain open.
  - [x] Tokens on the wire: every `POST /v2/r/{repo}` carries `Authorization: Bearer`, and the
    node's server decides every message at its gate, loaded from the node directory
    (`keys.json`, `permissions.json`); anonymous callers read public repositories only (D24).
    The CLI caches repository tokens and refreshes them with the device refresh token through
    the control plane; `javelin outbox replay` never sends a write without one. The control
    plane serves an HTTP API (`javelin control serve`) that `JAVELIN_CONTROL_URL` selects.
  - [x] U12 World log in one Durable Object: commit transaction on workerd (50 publishers, kill inside and after commit); deployed-object checks and replication re-check moved to U13.
  - [x] U9 author-owned revisions and exact approvals: `PublishRequest`, the node review
    partition (`review.rs`), tree IDs in collaboration.
  - [x] U13 Cloudflare managed node: Free-plan limits cited (unverified items marked), TypeScript Worker and Durable Object (wasm fallback, D23), `repo create --provider cloudflare` on workerd; deployed-only checks pending (`tools/node-cloudflare/README.md`).
  - [x] U13 deployed: `javelin-node` runs on the Workers Free plan at
    `https://javelin-node.dhruv-sharma10102005.workers.dev` with `NODE_ADMIN_TOKEN` as a secret;
    conformance (2 passed), `repo create --provider cloudflare`, and the acceptance `target_*`
    tests (5 verified, 7 not verified, 0 failed) against it; the U12 spike on a deployed object
    (14 of 14); every unverified Free-plan row settled by docs or `bun run probe`
    (`tools/node-cloudflare/README.md`).
  - [ ] Move the World-log commit path, objects, Layers and review into the Cloudflare node
    (today it serves provision, permissions and state only).
  - [ ] Run the Rust node core on Cloudflare via wasm: split `javelin-core` and a store trait out
    of the root crate (sketch in `tools/node-cloudflare/README.md`).
  - [x] U13 S1 follow-up: a `Provisioning` retry with another `--provider` or `--node-url` is refused as `operation_id_reused`; the reserved record stores both (`javelin-control` `NodeTarget`).
  - [x] U10 restricted agent traces (`javelin-node` trace store: D25 visibility, Maintain-only
    restriction, wire fetch guard, `missing_agent_trace` node tests).
  - [x] U8 control plane minimum: admin-token bootstrap, device-code API, CLI login, repo create
    (`javelin-control`, in-process and file-backed; applied-permission push to the self-hosted
    node; no passkeys, which are U8b).
  - [x] U11 forks with inherited history (`javelin-node` fork: log copy to N with source IDs,
    object copy into the fork's store, `fork_of` provenance, traces omitted with a marker unless
    the forker is Maintain; `javelin-control` fork op, in-process node; wire pull deferred).
  - [x] U8b passkey sign-in for device approval: WebAuthn enrollment and sign-in (webauthn-rs,
    software-authenticator tests), browser approval page over loopback HTTP, no email (D5).
  - [x] U14 hosted acceptance suite (`tests/hosted_acceptance/`, `tools/acceptance/run.ts`):
    contract gate 4 and the outage matrix against the self-hosted node and control plane, with
    browser device approval over HTTP; the `target_*` tests against a node URL (the Worker on
    workerd by default, a deployed node later); a failing `git` stand-in with a positive control
    and a marker check. Assertions only a deployed node or an unbuilt piece can prove report
    NOT VERIFIED with the reason. Product gaps found, each with an ignored failing `gap_*` test:
    - [x] Control plane unreachable is `control_plane_unavailable`, in process (an unopenable
      control directory) and over HTTP (`RemoteControl`, `JAVELIN_CONTROL_URL`); `gap_*` test
      un-ignored.
    - [x] A node answering HTTP 5xx, or not answering, is `node_unavailable`; `gap_*` test
      un-ignored.
    - [x] The client outbox transport (`NodeTransport`, `javelin outbox replay`) carries the
      credential and refreshes it at the control plane; gate 4's distinct outcomes are VERIFIED
      end to end with the CLI (`tests/hosted_acceptance/outages.rs`).
    - [ ] `cargo deny` checks only the root package (not `--workspace`), so the member crates'
      licenses are unchecked: BSD-3-Clause (`ed25519-dalek`), ISC (`ring`, `rustls-webpki`) and
      MPL-2.0 (`crypto-glue`, via `webauthn-rs`) are linked into `javelin-control` today.
  - [x] Forge first read-only pages (`apps/forge/`): repo home with README, file view (text
    highlighted, binary notice), history with attribution and changed-path counts, version
    diff against its parent. The browser reads the node directly over protocol v2 through one
    shared TypeScript codec (`packages/protocol/`); new read message `Attribution` 0x44; CORS on
    the self-hosted node for read messages only, writes from a browser origin refused. `bun test`
    runs against a real seeded `javelin-node serve`; CI job `Forge / self-hosted node`.
    - [ ] Control-plane lookup of `owner/repo` → node (today `?node=&repository=` or
      `forge.config.json`), and the Worker that serves `dist/`.
- [x] Release packaging and clean-machine installs on three OSes (#62).
- [ ] Docs.
- [ ] Sample application built with external agents and Javelin only; GA acceptance.

## Post-hoc review of #48 and #52 on main

- [x] Review #48 (radar) and #52 (protocol v2 trees) on main: Opus 5.5 fallback review, after Sol (Codex, Codex-swayu) was unavailable. No radar defects.
- [x] #52 tree work bound: a few KB of shared-subtree chunks can name 2^k files. Accepted by decision; documented in docs/architecture.md Known limits.
- [x] Follow-up: fetch verifies the server's tree root against the version ID chain, and caps named paths for an untrusted server.
  - [x] Trust anchor: let the caller store a verified version and verify only later versions incrementally.
    `fetch` takes an `Anchor`; cost is flat in history length (`tests/anchor.rs`). No
    Merkle mountain range: reasoning in docs/architecture.md, Fetch.
