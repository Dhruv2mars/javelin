# Linux sparse Layer proof workstream

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

### Current PR CI repair

Done means the current PR head passes the hosted Linux proof and PR #7 is regular.
The previous `89d76df` proof remains historical. Keep the existing PR #5 base
until its integration merges; no core/probe changes or merges belong here.

- [x] Inspect failed run `38461947427` at `19224ef`: missing watch notification; other behavior checks and cleanup passed. Pin stable Bun 1.4.3, which includes the upstream Linux watcher rewrite, without changing the common probe or assertions.
- [x] Pass the unchanged mounted-path probe and all eight backend phases at b0d7e220766ab2fa5555de020e5e7572f7a99d9f in hosted run 38870501111 with Bun 1.4.3.
- [x] Mark existing PR #7 ready and report the inherited native Cargo.lock blocker to hub.

## Main integration

- [x] Merge current origin/main after PR #5 merged; preserve imported code unchanged and keep root TASKS.md byte-identical to main.
- [x] Validate imported lockfile/build with `rtk cargo check --locked --all-targets`; owned workflow passes actionlint and diff checks. OverlayFS executable sources, common probe and Bun 1.4.3 workflow are unchanged from the passing `b0d7e22` proof.
