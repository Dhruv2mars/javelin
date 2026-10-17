# Source-local Cargo configuration

Done: Cargo builds from the canonical source directory; a real Cargo regression
proves source-local configuration wins over unrelated caller-local configuration.

- [x] Read repository instructions and build contract; confirm the missing cwd.
- [x] Add a focused regression and confirm it fails before the fix (binary prints
  `caller` instead of `source`).
- [x] Set Cargo's cwd to the canonical source and run focused checks: the real
  Cargo regression and existing source-mutation/stale-record test pass; Cargo fmt
  and Clippy (`--all-targets -- -D warnings`) pass on the macOS host.
