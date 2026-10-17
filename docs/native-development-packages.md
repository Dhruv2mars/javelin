# Native development packages

Status: development tooling. These archives are **development / pre-GA** builds of the
native CLI at Cargo version `0.1.0`. They are not a public release, not signed, not
published, and not Gate 5 (installation, upgrade, and recovery on every platform)
evidence. The CLI inside supports only `init`, `history`, and `check`; Layers and the
complete Layer Publish workflow do not exist yet, and the CLI has no `--version` flag.

The original packager used native Layer state
`e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`. The CLI workspace refactor starts from
merged main `c48209f`, adds `crates/javelin-cli`, and updates packaging build and
provenance checks for the moved executable.

## What it does

`tools/native-packaging` is a small Rust crate (own `Cargo.lock`, empty `[workspace]`,
no dependency on the `javelin` crate) that builds the `javelin-pack` developer tool:

| Command | Purpose |
| --- | --- |
| `build` | `cargo build --locked --release --package javelin-cli --bin javelin` with a controlled environment; writes `build-record.json` including a fingerprint of the sources built |
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
  `Cargo.toml`, `Cargo.lock`, `LICENSE`, any `.cargo/config*`, root `src/**`,
  `crates/javelin-cli/Cargo.toml`, and `crates/javelin-cli/src/**`
  (path and content hashes, Git-free).
- `toolchain`: full `rustc -vV`, cargo version, exact build command, rustflags
  (path remaps), `SOURCE_DATE_EPOCH`.
- `binary`: path, SHA-256, size, and the test-hook markers confirmed absent.
- `dependencies`: CLI normal-dependency closure for the target (including the root library) with `Cargo.lock` checksums.
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

`install` checks bounded archive input and its checksum before writing. It refuses
existing destinations without modifying them. Archive-format refusals create no
destination parent or scratch tree. Later manifest-validation failures remove
only the owned temporary tree and leave the new destination absent:

- The checksum file is exactly `<64 hex>  <archive name>` and matches the archive bytes.
- The destination does not exist (file, directory, or symlink); an existing install,
  and any binary in it, is never modified. There is no in-place update.
- Every entry is a regular file or directory with a portable, relative name: no `..`,
  absolute, backslash, drive/ADS `:`, Windows device names, trailing dot or space,
  control characters, case-insensitive duplicates, file-versus-directory conflicts, a
  second top-level directory, symlinks, hardlinks, or devices.
- Compressed input is capped at 272 MiB. File payloads total at most 256 MiB. The
  decompressed tar stream, including headers, extensions and trailing padding,
  is capped at 272 MiB; an individual PAX record is capped at 64 MiB.
- At most 10,000 raw tar headers or ZIP central-directory records are allowed,
  including extension and directory records. ZIP records are checked before the
  extraction library deduplicates names, and their actual count must agree with
  the footer. A directory's declared nonzero payload is refused before decompression.
- GNU sparse headers and `GNU.sparse.*`, `SCHILY.realsize` or
  `SCHILY.filetype=sparse` PAX extensions are unsupported and refused in raw
  headers. Sparse data is never silently treated as a regular file.
- ZIP Unix types permit only regular files and directories. FIFO, character/block
  devices, sockets and symlinks are refused. When the writing host supplies no
  Unix type, including DOS archives, the name and directory attributes determine
  the file/directory shape; absent type bits do not imply a verified Unix type.
- The bounded gzip decoder is drained through CRC/ISIZE trailer validation before
  any destination writes. Truncated trailers, bad CRC, trailing data and multiple
  gzip members are refused. These are format restrictions of this development installer.
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
`license` field. Dependencies are MIT, Apache-2.0, MIT/Apache-2.0, Zlib,
Unlicense OR MIT, or `(MIT OR Apache-2.0) AND Unicode-3.0` (`unicode-ident`), plus the bundled SQLite amalgamation (public-domain notice in its own
source header, version 3.53.2) built by `libsqlite3-sys`. The notices list the
CLI normal-dependency closure for the target (including the root library), a conservative superset that includes
compile-time proc-macro crates; packaging fails if a listed crate has no license file.
This is a developer's inspection of `Cargo.lock` metadata and crate sources, not legal
advice or a complete audit of transitive build tools.

The source fingerprint is taken before and after Cargo runs. If the covered inputs
change during the build, no build record is retained. This detects ordinary saves
during a build; an edit reverted between samples is outside this check.

## Reproducibility

`build` sets `SOURCE_DATE_EPOCH`, `CARGO_INCREMENTAL=0`, `TZ=UTC`, `LC_ALL=C`, path
remaps for the source, build directory, Cargo home, and Rust sysroot
(`--remap-path-prefix`, plus `-ffile-prefix-map` for the bundled C on non-MSVC), and
removes ambient `RUSTFLAGS`, `RUSTC`, `CC`/`CXX`, `CFLAGS*`, `CARGO_PROFILE_*`, and
`CARGO_BUILD_*`. It does not edit root Cargo profiles and does not neutralize a user's
`~/.cargo/config.toml` (a documented limit). `repro` builds the same source copied into two differently named
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

Historical tested source: `b9243f54750a1f3ba1b2c6196b07d8fda811a472`, based on
accepted `e56fc86`. Relative to `e56fc86` the branch changes only this document,
`TASKS.md`, `.github/workflows/native-packaging.yml`, and `tools/native-packaging/**`;
root `Cargo.toml`, `Cargo.lock`, `src/`, `tests/`, `LICENSE`, and `native.yml` are
byte-identical at that source. The Codex audit corrections below supersede its
archive-rejection claims; original build and downloaded smoke proofs remain valid.

Hosted run: https://github.com/Dhruv2mars/javelin/actions/runs/38509714612, six jobs
succeeded (three `Package`, three `Download smoke`). Artifacts expire 2026-10-28:
`native-package-<os>` (archive and `.sha256`), `native-packaging-logs-<os>-1`,
`native-download-smoke-logs-<os>-1`. Three earlier runs failed and were fixed in place:
two for Windows test assumptions (gzip-only metadata test; zip unix-mode bits that
Windows-written zips do not carry) and one because the reproducibility step did not copy
the newly fingerprinted `LICENSE`; none changed the packaged binary.

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Runner image | `ubuntu24` 20261004.327.1 | `macos15` 20260907.0337.1 | `win25-vs2026` 20260925.250.1 |
| Architecture, target | X64, `x86_64-unknown-linux-gnu` | ARM64, `aarch64-apple-darwin` | X64, `x86_64-pc-windows-msvc` |
| Toolchain | rustc/cargo 1.99.0 | same | same |
| Archive | `.tar.gz` | `.tar.gz` | `.zip` |
| Packager tests | 18 integration + 1 unit passed | 18 integration + 1 unit passed | 17 integration + 1 unit passed, 1 Unix-only integration test not compiled |
| Package job `install` + `smoke` | 18 checks passed | 18 passed | 18 passed |
| Download job: native checksum, native extract (`tar` / `Expand-Archive`), smoke | 18 passed | 18 passed | 18 passed |
| Download job: packager `install` of the same download, smoke | 18 passed | 18 passed | 18 passed |
| Binary-declared floor (informational) | glibc symbol version up to 2.34 | `minos 11.0` | not inspected |
| Packaged binary SHA-256 | `8f1832...` | `fdfb14...` | `b2e64c...` |
| Two-directory rebuild identical | **yes** | **yes** | **no** |

Cross-checks done outside CI on the downloaded artifacts: all three `.sha256` files verify
with `shasum -a 256 -c`; all three `PACKAGE.json` files carry the same `source.sha`,
`Cargo.lock` hash (`a3ff94c6...`, equal to the repository file), and source fingerprint
(`015d41cb...`, 8 files); every shipped `LICENSE` is byte-identical to the repository
file; the dependency lists agree except macOS also lists `libc` (31 versus 32 crates).
The `core.autocrlf false` step matters: the previous run's Windows checkout had CRLF
sources, a different `Cargo.lock` hash, and a CRLF `LICENSE`, which review round 2 caught.

Windows rebuilds differ at 20 byte positions of 2,245,632: offset 264, three isolated
offsets near 1,986,9xx, and 16 consecutive bytes at 1,987,324 to 1,987,339. The hub Codex r2 audit confirmed the differing bytes are PE timestamp fields
and the PDB GUID. `/Brepro` is a candidate only and was not tested or applied here. The packaged Windows binary differs from both rebuilds.

### Verdicts

| Scope | Verdict |
| --- | --- |
| Packager builds the locked release CLI on Linux x86_64, macOS arm64, Windows x86_64 and packages archive, checksum, manifest, licenses | VERIFIED (run above) |
| Original archive rejection guarantees | HELD by the hub Codex r2 audit; valid-checksum ZIP duplicates, raw count overflow, directory payload, sparse PAX, special ZIP modes and gzip trailer damage were accepted. Corrected-source verification is tracked below |
| Downloaded artifact verified with OS-native checksum and extraction tools, then smoked through `PATH` in a new non-Git project | VERIFIED on all three OS |
| `init`/`history`/`check` JSON success, damaged-storage exit 1, preexisting binary bytes preserved | VERIFIED on all three OS for the packaged binary |
| Git never invoked by the packaged CLI during smoke | VERIFIED on all three OS with a validated `git` shim; the negative fixture (a binary that does call Git) runs on Unix only, so Windows shim sensitivity is shown only by its positive probe |
| Side-by-side install, upgrade, rollback of the minimal development binary | VERIFIED on all three OS (test), development binary only |
| Release binary excludes test hooks | VERIFIED by marker scan of all three release binaries plus refusal regression; marker strings are a proxy for the compiled-out hook code |
| Reproducible binary | VERIFIED identical on Linux and macOS for two directories on one runner each; **NOT reproducible on Windows**. Determinism is not claimed for Windows, across runners, or across toolchains |
| Deterministic archive layer | VERIFIED by test for identical input; does not imply a reproducible binary |
| Minimum OS versions, other architectures (Linux arm64, macOS x86_64, Windows arm64) | NOT VERIFIED / unconfirmed |
| Public release, signing, provenance attestation, npm or GitHub release, upgrade across real versions, Gate 5 | NOT VERIFIED; not attempted |
| Content-level corruption detection by the packaged CLI | NOT VERIFIED here; the smoke damages SQLite pages only |

### Review

Independent read-only reviews, used instead of self-assessment:

- Round 1, `gpt-6.1-sol` at high reasoning on the code at `f7a7f7e`: six findings (decompression
  limit bypass, Windows test, provenance not bound to the built source, unquoted `CFLAGS`
  paths, a vacuous cleanup test, a Unix destination race), all fixed with regressions.
- Round 2 on `gpt-6.1-sol` failed immediately on a provider usage limit and produced no
  review. It was rerun with Claude Opus 5.5 at medium effort on `bf14bf7`: two medium findings
  (a tar stream cut exactly on a header boundary was accepted; Windows CRLF checkout), plus
  lows (path filter, doc license list, fingerprint and environment gaps, an unsupported
  "PE header" wording). All were fixed; the boundary and `repro` paths have regressions.
- Mutation checks confirmed the new tests fail when the guarded code is removed. One
  mutation (removing the `..` component check alone) survives because another check
  rejects the same names.
- No separate architect subagent was used. The shape comparison above is the owner's
  own, and round 1 found no materially simpler design. The final head `b9243f5` was not
  independently reviewed after the round-2 fixes beyond the hosted run and these checks.

### Retained limits

- Development distribution proof only. No signed or published artifact exists.
- The CLI has no `--version`; the binary is identified by hash and `PACKAGE.json`.
- Each OS was tested on one hosted image; older and newer OS releases are untested.
- The Windows destination claim-and-rename is not atomic against a concurrent writer.
- The Git shim is the packager copied to `git[.exe]`; it detects a spawned `git` found
  through the child's `PATH`, not Git linked as a library.
- A user's `~/.cargo/config.toml` is not neutralized by `build`.
- Third-party notices come from `Cargo.lock` metadata and crate sources and are not a
  legal audit; the list is a conservative superset including proc-macro crates.

### Codex audit corrections

Hub acceptance remains HELD pending fresh exact-source review and hosted evidence.
The owner copied the reviewer fixtures into `/tmp/javelin-pr17-owner-r3/fixtures`
and exported unchanged `b9243f5` into owner scratch. Reviewer scratch was read only.
All ten fixture installs returned success before fixes, including the real macOS
archive with its eight-byte gzip trailer removed and its CRC damaged. Results
are retained in `prior-results.json`; corrected refusals are in `fixed-results.json`.

ZIP choices inspected were the existing library's configuration, a second
metadata reader, and custom format parsing. `zip` 9.0.3 has no strict duplicate
configuration and deduplicates central records before exposing them. The selected
[rawzip 0.5.2](https://docs.rs/rawzip/0.5.2/rawzip/) preflight iterates raw central
records with no internal name deduplication or new transitive dependencies.
`rc-zip-sync` was inspected and rejected because metadata-only use would add 30
locked dependencies. The original ZIP writer and decompression remain unchanged,
so the valid development-package format is preserved.

The new portable regression fixtures name the host's actual binary path. They
check malformed archives with correct checksums, bad-checksum precedence over
parsing and destination checks, existing binary/scratch byte preservation, and
absence of new directories or owned temporary residue. Clean tar and DOS ZIP
controls still install. Regressions are run against the unchanged prior installer
as well as the corrected one, without invoking Git under test.

Local and hosted corrected-source counts and review results will be recorded here
when complete. Minimum OS versions, other architectures, Windows concurrent
installation atomicity, Windows binary reproducibility, signing and GA remain
unverified. No root sources, Cargo files or native workflow are changed.
