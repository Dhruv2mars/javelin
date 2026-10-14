# Native development packages

Status: development tooling. These archives are **development / pre-GA** builds of the
native CLI at Cargo version `0.1.0`. They are not a public release, not signed, not
published, and not Gate 5 (installation, upgrade, and recovery on every platform)
evidence. The CLI inside supports only `init`, `history`, and `check`; Layers and the
complete Layer Publish workflow do not exist yet, and the CLI has no `--version` flag.

Base source: accepted native Layer state `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`
(unmerged dependency; PR #17 claims no independent acceptance). Nothing in this change
edits root Cargo files, `src/`, or `tests/`.

## What it does

`tools/native-packaging` is a small Rust crate (own `Cargo.lock`, empty `[workspace]`,
no dependency on the `javelin` crate) that builds the `javelin-pack` developer tool:

| Command | Purpose |
| --- | --- |
| `build` | `cargo build --locked --release --bin javelin` with a controlled environment; writes `build-record.json` including a fingerprint of the sources built |
| `package` | Archive, `.sha256`, and `PACKAGE.json` for one built binary; refuses a `--source` that differs from what was built |
| `install` | Verify checksum, extract into an owned temp directory, rename into a **fresh** path |
| `smoke` | Resolve `javelin` through `PATH` and exercise it in a new non-Git project |
| `repro` | Build the same source from two directories and report whether the bytes match |

It never runs Git, publishes, tags, reads secrets, installs globally, or edits `PATH`.
`--source-sha` is supplied by the caller; `SOURCE_DATE_EPOCH` is supplied by the caller
(CI reads the commit time during checkout setup).

## Archive shape

```text
javelin-development-0.1.0-<sha12>-<target-triple>.tar.gz   (Linux, macOS)
javelin-development-0.1.0-<sha12>-<target-triple>.zip      (Windows)
<archive>.sha256                                            `<hex>  <file name>`
```

Inside, one top directory named like the archive:

```text
bin/javelin[.exe]
LICENSE                    the repository's actual MIT license, byte for byte
THIRD-PARTY-NOTICES.md     crate, version, license expression, Cargo.lock checksum
licenses/<crate>-<ver>/    each dependency's own license files
README.txt                 says this is a development build
PACKAGE.json               machine-readable provenance (below)
```

Layouts compared: **A** this Rust crate (selected), **B** per-OS shell and PowerShell
scripts around `tar`, `Compress-Archive`, `shasum`, `Get-FileHash`, **C** Bun scripts.
A wins on one implementation of traversal-safe extraction, deterministic archive
metadata, and checksum handling across three operating systems, using a toolchain the
build already requires. B needs three implementations with weaker extraction checks.
C adds a runtime the CLI build does not otherwise need on runners. Plain `tar`/`zip`
archives were chosen over installers, MSI, or pkg formats because they need no
elevation, registry, or signing and leave the choice of `PATH` to the user.

## PACKAGE.json

- `channel: "development"`, `pre_ga: true`, `cargo_version: "0.1.0"`, a notice.
- `source`: caller-supplied commit SHA; SHA-256 of `Cargo.lock`; SHA-256 over
  `Cargo.toml`, `Cargo.lock`, and `src/**` (path and content hashes, Git-free).
- `toolchain`: full `rustc -vV`, cargo version, exact build command, rustflags
  (path remaps), `SOURCE_DATE_EPOCH`.
- `binary`: path, SHA-256, size, and the test-hook markers confirmed absent.
- `dependencies`: normal-dependency closure for the target with `Cargo.lock` checksums.
- `platform.known`: OS, architecture, target triple of the build host.
- `platform.minimum_os_version`: `"unconfirmed"`; other architectures: unconfirmed.

Known versus unconfirmed:

| Fact | Status |
| --- | --- |
| OS, architecture, and target triple of each archive | Known: the build host |
| Hosted runner image that built and ran it | Known per run (see Evidence) |
| Binary-declared floor (macOS `minos`, Linux glibc symbol version) | Inspected as informational output only |
| Minimum supported OS version | **Unconfirmed**; no older OS was tested |
| Other architectures (Linux arm64, macOS x86_64, Windows arm64) | **Unconfirmed**; not built or run |

## Safe install

`install` refuses to proceed unless all of these hold, and on any failure removes only
its own temp directory and leaves the destination absent:

- The checksum file is exactly `<64 hex>  <archive name>` and matches the archive bytes.
- The destination does not exist (file, directory, or symlink); an existing install,
  and any binary in it, is never modified. There is no in-place update.
- Every entry is a regular file or directory with a portable, relative name: no `..`,
  absolute, backslash, drive/ADS `:`, Windows device names, trailing dot or space,
  control characters, case-insensitive duplicates, file-versus-directory conflicts, a
  second top-level directory, symlinks, hardlinks, or devices.
- Entry count, file payload bytes, and the whole decompressed tar stream (including PAX/GNU metadata) are capped; a directory entry with a payload is refused.
- The extracted `PACKAGE.json` is development/pre-GA, names `bin/javelin[.exe]`, and
  its binary hash matches the extracted binary; `LICENSE` is present.
- The final step claims the destination and renames the verified tree into it. On Unix
  an exclusive `create_dir` claims the path atomically and the rename replaces only that
  empty claim, so a destination created by someone else is never replaced. On Windows a
  directory rename cannot replace an existing one and fails closed, but the existence
  check and the rename are not atomic against a concurrent writer; this is a
  single-user developer tool.

Manual install without the tool: verify with `shasum -a 256 -c` / `sha256sum -c` (Unix)
or `Get-FileHash` (Windows), then `tar -xzf` / `Expand-Archive` into an empty directory.
The download job exercises exactly that path on every OS.

## Smoke

`javelin-pack smoke --bin-dir <dir>` puts the unpacked `bin/` on `PATH`, ahead of a
preexisting `javelin`, in a temporary project outside any Git worktree, and checks:
`javelin` resolves to the unpacked binary and its hash matches `PACKAGE.json`; `init
--json`, `history --json`, `check --json`, and plain `check` behave; repeated `init` is
idempotent and leaves an existing project file untouched; storage damage makes `check
--json` exit 1 with a structured stderr error and no success output, and restoring the
bytes makes it pass; with the preexisting binary first on `PATH` that binary (not the
package) runs and its bytes are unchanged; and a `git` shim placed first on `PATH` was
never invoked (the shim is validated first so the check cannot pass vacuously).

Scope limits: the corruption check damages the SQLite file's data pages, so it proves
the packaged CLI's failure path and exit code, not content-hash detection of a single
modified object (that belongs to the core suite). The Git shim is the packager itself
copied to `git[.exe]`; its Windows effectiveness is recorded under Evidence.

## Upgrade and rollback

Only side-by-side installs of the minimal development binary are supported and tested:
install a newer package into a new fresh path, select it by `PATH`, and roll back by
selecting the previous path. A corrupt newer package fails without touching the older
install, whose bytes are asserted unchanged. There is no updater, no in-place upgrade,
no data migration, and no claim about the future release upgrade path. The test uses
two real builds of the same source (one with `-Copt-level=2`) labelled with different
source SHAs; they are different binaries, not different Javelin versions.

## Test hooks

The release binary is built with no features and is refused by `package` if it contains
`JAVELIN_TEST_` or `process_tests`, strings that exist in the library's test-only
process-hook code. A regression appends a marker to the real binary and expects refusal.

## Licenses

Javelin is MIT licensed (`LICENSE` shipped unchanged). The root `Cargo.toml` declares no
`license` field. Dependencies are MIT, Apache-2.0, MIT/Apache-2.0, Zlib, or
Unlicense OR MIT, plus the bundled SQLite amalgamation (public-domain notice in its own
source header, version 3.53.2) built by `libsqlite3-sys`. The notices list the
normal-dependency closure for the target, a conservative superset that includes
compile-time proc-macro crates; packaging fails if a listed crate has no license file.
This is a developer's inspection of `Cargo.lock` metadata and crate sources, not legal
advice or a complete audit of transitive build tools.

## Reproducibility

`build` sets `SOURCE_DATE_EPOCH`, `CARGO_INCREMENTAL=0`, `TZ=UTC`, `LC_ALL=C`, path
remaps for the source, build directory, Cargo home, and Rust sysroot
(`--remap-path-prefix`, plus `-ffile-prefix-map` for the bundled C on non-MSVC), and
removes ambient `RUSTFLAGS`, `CARGO_PROFILE_*`, and `CARGO_BUILD_*`. It does not edit
root Cargo profiles. `repro` builds the same source copied into two differently named
directories and compares the bytes. The archive layer is separately deterministic for
identical input (sorted entries, fixed owner, modes, and times, no gzip timestamp),
which a test asserts; that is not binary reproducibility.

## Developer commands

```sh
P="cargo run --locked --quiet --manifest-path tools/native-packaging/Cargo.toml --"
export SOURCE_DATE_EPOCH=<commit time>
$P build   --source . --out work/build
$P package --build work/build --source . --source-sha <40-hex> --out work/dist
$P install --archive work/dist/<archive> --checksum work/dist/<archive>.sha256 --dest work/install/javelin
$P smoke   --bin-dir work/install/javelin/bin
cargo test --locked --manifest-path tools/native-packaging/Cargo.toml
```

## Evidence

Tested source: `b0383ff52658618ed840d9b5449eea892fa4f337` on PR #17, based on accepted
`e56fc86`. Relative to `e56fc86` the branch changes only this document, `TASKS.md`,
`.github/workflows/native-packaging.yml`, and `tools/native-packaging/**`; root
`Cargo.toml`, `Cargo.lock`, `src/`, `tests/`, and `LICENSE` are byte-identical (checked
with a file-level diff). Later commits on the PR are documentation or `TASKS.md` only.

Hosted run: https://github.com/Dhruv2mars/javelin/actions/runs/38507762765, all six jobs
succeeded (three `Package`, three `Download smoke`). Artifacts expire 2026-10-28:
`native-package-<os>` (archive and `.sha256`), `native-packaging-logs-<os>-1`,
`native-download-smoke-logs-<os>-1`. An earlier run failed on Windows twice for test
assumptions (a gzip-only archive-metadata test, then zip unix-mode bits that do not
exist for Windows-written zips); both were test fixes with no product change.

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Runner image | `ubuntu24` 20261004.327.1 | `macos15` 20260907.0337.1 | `win25-vs2026` 20260925.250.1 |
| Architecture, target | X64, `x86_64-unknown-linux-gnu` | ARM64, `aarch64-apple-darwin` | X64, `x86_64-pc-windows-msvc` |
| Toolchain | rustc/cargo 1.99.0 | same | same |
| Archive | `.tar.gz` | `.tar.gz` | `.zip` |
| Packager tests (real CLI) | 16 passed | 16 passed | 15 passed, 1 Unix-only test not compiled |
| Package job `install` + `smoke` | 18 checks passed | 18 passed | 18 passed |
| Download job: native checksum then native extract (`tar`/`Expand-Archive`), smoke | 18 passed | 18 passed | 18 passed |
| Download job: packager `install` of the same download, smoke | 18 passed | 18 passed | 18 passed |
| Binary-declared floor (informational) | glibc symbol version up to 2.34 | `minos 11.0` | not inspected |
| Two-directory rebuild identical | **yes** (`8f1832...`) | **yes** (`fdfb14...`) | **no** |

Independent hash cross-check, done outside CI on the downloaded artifacts: all three
`.sha256` files verify with `shasum -a 256 -c`; the Linux archive's `PACKAGE.json`,
extracted binary hash, and shipped `LICENSE` match (`LICENSE` is byte-identical to the
repository file); the Windows zip's `PACKAGE.json` binary hash equals the hash of the
extracted `javelin.exe` and the download job's smoke hash.

### Verdicts

| Scope | Verdict |
| --- | --- |
| Packager builds the locked release CLI on Linux x86_64, macOS arm64, Windows x86_64 and packages archive, checksum, manifest, licenses | VERIFIED (run above) |
| Clean-temp install into a fresh path, with checksum verification and rejection of tampered, traversing, linked, duplicate, oversized archives | VERIFIED on all three OS by the packager tests; hostile-archive fixtures use crafted tar and zip, not external tools |
| Downloaded artifact verified with OS-native checksum and extraction tools, then smoked through `PATH` in a new non-Git project | VERIFIED on all three OS |
| `init`/`history`/`check` JSON success, damaged-storage exit 1, preexisting binary bytes preserved | VERIFIED on all three OS for the packaged binary |
| Git never invoked by the packaged CLI during smoke | VERIFIED on all three OS with a validated `git` shim; the negative fixture (a binary that does call Git) runs on Unix only, so Windows shim sensitivity is shown only by its positive probe |
| Side-by-side install, upgrade, rollback of the minimal development binary | VERIFIED on all three OS (test), development binary only |
| Release binary excludes test hooks | VERIFIED by marker scan of all three release binaries plus refusal regression; marker strings are a proxy for the compiled-out hook code |
| Reproducible binary | VERIFIED identical on Linux and macOS for two directories on one runner each; **NOT reproducible on Windows** (two rebuilds differ in 24 bytes starting at offset 264 of 2,245,632, in the PE header region; the packaged Windows binary differs from both). Determinism is not claimed for Windows, across runners, or across toolchains |
| Deterministic archive layer | VERIFIED by test for identical input; does not imply a reproducible binary |
| Minimum OS versions, other architectures (Linux arm64, macOS x86_64, Windows arm64) | NOT VERIFIED / unconfirmed |
| Public release, signing, provenance attestation, npm or GitHub release, upgrade across real versions, Gate 5 | NOT VERIFIED; not attempted |
| Content-level corruption detection by the packaged CLI | NOT VERIFIED here; the smoke damages SQLite pages only |

### Retained limits

- Development distribution proof only. No signed or published artifact exists.
- The CLI has no `--version`; the binary is identified by hash and `PACKAGE.json`.
- Each OS was tested on one hosted image; older and newer OS releases are untested.
- The Windows destination claim-and-rename is not atomic against a concurrent writer.
- The Git shim is the packager copied to `git[.exe]`; it detects a spawned `git`
  found through the child's `PATH`, not Git linked as a library.
- Third-party notices come from `Cargo.lock` metadata and crate sources and are not a
  legal audit; the notices list is a conservative superset including proc-macro crates.
