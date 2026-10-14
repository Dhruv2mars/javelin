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
| `build` | `cargo build --locked --release --bin javelin` with a controlled environment; writes `build-record.json` |
| `package` | Archive, `.sha256`, and `PACKAGE.json` for one built binary |
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
- Entry count and expanded size are capped.
- The extracted `PACKAGE.json` is development/pre-GA, names `bin/javelin[.exe]`, and
  its binary hash matches the extracted binary; `LICENSE` is present.
- The final step is a rename from the owned temp directory (same filesystem). The
  existence check and rename are not atomic against a concurrent writer to the same
  destination path; this is a single-user developer tool.

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

Pending hosted run. Results, exact tested source, artifact names, and retained limits
are recorded here after the three-OS jobs finish.
