# CLI workspace package

Done means the existing CLI and root library API work with default root Cargo
commands, `target/release/javelin` and installed `javelin` remain available, and
development packaging fingerprints the CLI inputs and rejects wrong binaries.

- [x] Read repository guidance and verify merged main base.
- [x] Move CLI source and actual-binary tests into a small workspace member.
- [x] Update packaging provenance, dependency closure, workflow paths and docs.
- [x] Run default workspace, native, actual-binary and package/install checks.
  macOS / Rust 1.93.0: 103 workspace tests passed, one existing test ignored;
  all 10 actual-binary CLI tests passed. Packager: 29 tests passed, including
  concurrent-save refusal, moved-input fingerprints, wrong-binary rejection,
  archive install/smoke and two-directory repro. Workspace and packager fmt and
  warnings-denied Clippy passed. Default release build and `cargo install` plus
  Git-blocking smoke passed; standalone protocol `cargo check --locked` passed.
  No laptop mounts or cross-platform execution claimed.
- [x] Prepare checked implementation for one regular PR; leave integration to the hub.

- [x] Fix PR #27 Linux CI compilation: add Linux-only dev `libc` for the moved
  ignored mount test's `getuid` call. Confirmed raw Native run 38993027564 error.
  Linux x86_64 cross-target Clippy with all CLI targets/features and warnings
  denied passes (Zig C compiler, `CRATE_CC_NO_DEFAULTS=1`); this compiles the
  ignored Linux test without executing mounts. All 10 macOS CLI tests, CLI test
  check, formatting and locked resolution pass. Root API and TASKS unchanged.

- [x] Merge main `24fb973` into the same PR to include benchmark/profile/runtime
  dependencies missing from the previous checked head. Raw baseline run
  38993862206 confirmed the absent benchmark working directory. All 10 CLI tests,
  Linux cross-target warnings-denied Clippy and formatting pass after the merge.
  All six standalone lockfiles pass locked offline metadata; refreshed only the
  profile lockfile's missing root `libc` entry. Native source/tests (with the CLI
  moves mapped) and root TASKS match main byte for byte. No manual benchmarks.


## Whole-Layer Publish CLI

Done means actual CLI commands delegate safe Linux Publish, retain exact receipts
and opaque attribution, and prove refusal, conflict/retry, read-only success and
continuation in a disposable hosted Linux run. Root TASKS remains main's file.

- [x] Read rules, build contract and merged CLI/runtime dependencies.
- [x] Wire workspace Publish and platform-independent receipt inspection.
- [x] Add grammar/output tests, hosted actual-binary workflow and focused docs.
- [x] Run fmt, warnings-denied Clippy, workspace/binary tests and release build.
  macOS: 103 workspace tests passed, one inherited ignored test; 10 actual-binary
  CLI tests passed. Linux cross-target CLI all-target/all-feature Clippy passed.
- [x] Verify disposable hosted Linux smoke with a failing Git stand-in.
  Run 38994939678 passed both actual-binary mount tests and cleanup (`exit=0`).
  Raw logs downloaded and inspected. PR CI repeats this smoke and the existing
  native/package checks. Limits: Linux helper must be configured separately;
  macOS/Windows ordinary paths, autosave and refresh remain unsupported. Raw trace
  import reads the file into memory. Unmounted physical enforcement is only
  observed on remount; no out-of-band backing-path guarantee is claimed.

Delivery: one regular T3-linked PR against main; merge belongs to the hub.


## Native Stack CLI and explicit Linux workspace rebind

Done: separate actual CLI actions Stack a frozen source into an exact target and
rebind its clean view through the public adapters, then tools/checkpoint/Publish
work in disposable Linux. Exact retries preserve original inputs and outcomes.
Root TASKS stays identical to main; no native or adapter algorithm/API changes.

- [x] Read contracts and verify merged main includes PRs 26–29 and format 4.
- [x] Implement separate Stack/rebind commands, explicit tokens and outcomes.
- [x] Add actual-binary merge/conflict/retry/frozen-source/error coverage.
- [x] Extend existing disposable Linux smoke through rebind/tools/Publish.
- [x] Document checkpoint/unmount safety and explicit retry boundaries.
- [x] Run fmt, Clippy, tests/release and inspect hosted Linux smoke.
- [x] Prepare checked implementation for one regular T3-linked PR against main;
  delivery URL is reported in this thread and integration belongs to the hub.

- [x] Authorized integration: make merge/Stack default root members, remove
  unused local locks, hash root lock in mechanical CI, and fingerprint both
  adapters in the existing packager list and regression.
- [x] Linux cross-target all-target/all-feature Clippy passes with Zig; no mounts
  were attempted on the laptop. Mutation tests run in an isolated source copy.

- [x] Copy root MIT LICENSE byte for byte into both compiled adapters; include
  these inputs in the packager fingerprint and regression without collector changes.

Verification: macOS Rust 1.93.0 default workspace tests passed 160 tests with one
inherited ignore (including 13 actual-binary CLI tests). Workspace/package fmt,
warnings-denied Clippy, Linux cross-target CLI Clippy, release build and CLI install
passed. The 29 real packager tests passed, including archive install, changed-input
refusal and reproducibility. An isolated mechanical run killed 29/29 compiling
mutations and restored all source hashes. An initial overlapping mutation/check
run was invalidated; these results are from stable restored source and isolated
mutations.

Disposable Linux run [39013214105](https://github.com/Dhruv2mars/javelin/actions/runs/39013214105)
at CLI source `e2556be` passed three Stack binary cases and all three mount cases,
including Stack/rebind/tools/checkpoint/Publish. Raw job/artifact logs were inspected:
Git positive control/absence checks passed, no mounts remained, cleanup `exit=0`.
Final integration adds only license files/fingerprint regression and this checklist
after that CLI source. No laptop mounts, daemon, autosave, refresh, restore or
project adoption were added. Linux helper setup and macOS/Windows workspace support
remain outside this unit.


## Foreground automatic stable checkpoints

Done: explicit workspace watch captures content-stable edits with existing durable
receipts, frozen validation and no redundant history; hosted disposable Linux
proves tools, rename/delete/modes/symlinks, writers/mappings, boundary races,
competing callers, killed capture recovery and terminal stops with full cleanup.
Root TASKS stays identical to merged main.

- [x] Read current rules/contracts and verify PR30 merged base b234dd6.
- [x] Implement bounded foreground polling and exact frozen validation.
- [x] Add deterministic tests and real-process Linux/CLI coverage.
- [x] Document sampling, opt-in/retry behavior and unsupported ignore/scale limits.
- [x] Run proportional fmt/Clippy/workspace/release and hosted smoke checks.
- [x] Prepare checked implementation for one regular T3-linked PR against main;
  delivery URL is reported in this thread and integration belongs to the hub.

Verification: local macOS workspace tests passed 162 tests with one inherited
ignore; workspace fmt, warnings-denied Clippy and release build passed. Linux
x86_64 cross-target all-target/all-feature Clippy also passed using Zig, without
mounting on the laptop. The final stale-recovery change additionally passed the
focused quiet-window test, both Clippy targets and release rebuild.

Hosted implementation source `9aa7221` passed runtime run
[39034554393](https://github.com/Dhruv2mars/javelin/actions/runs/39034554393)
(29 mount tests, VERIFIED summary) and actual CLI run
[39034556726](https://github.com/Dhruv2mars/javelin/actions/runs/39034556726)
(four mount cases and three Stack binary cases). Raw logs/artifacts were downloaded
and inspected; all 16 runtime evidence source hashes match delivered implementation
files. Both runs proved failing-Git positive controls and absence during product
execution, with complete cleanup (runtime allUnmountedAndRemoved=true; CLI exit=0).
The final commit amend includes this scoped checklist; implementation/test/workflow
files retain their tested hashes. Ignore support, large-tree performance, other
kernels/platforms, production privilege setup and complete GA acceptance remain
unverified. No native graph/schema/adapter/helper/probe changes were made.
