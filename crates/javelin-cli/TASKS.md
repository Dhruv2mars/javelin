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
