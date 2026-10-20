# Install and upgrade Javelin

Javelin is pre-GA. This page covers only what the release pipeline checks. There are
no package-manager installs (Homebrew, crates.io, npm), and this page does not
describe a stable release channel. Until GA, pin a version with `--version` or
`-Version`.

## Platforms

Each release has one archive per platform, built on a native runner of that platform.

| Platform | Archive target | Runner used for build and clean-machine check |
| --- | --- | --- |
| macOS, Apple silicon | `aarch64-apple-darwin` | macOS 15 (arm64) |
| macOS, Intel | `x86_64-apple-darwin` | macOS 15 Intel |
| Linux, x86_64 (glibc) | `x86_64-unknown-linux-gnu` | Ubuntu 24.04 |
| Linux, aarch64 (glibc) | `aarch64-unknown-linux-gnu` | Ubuntu 24.04 arm |
| Windows, x86_64 | `x86_64-pc-windows-msvc` | Windows Server 2025 |

Linux binaries are dynamically linked against glibc. They are built on Ubuntu 24.04,
so they need a glibc at least as new as that build's requirement. The exact symbol
version is recorded in the build log of each release. Other OS versions and
architectures are not tested.

## Install on macOS or Linux

Replace `TAG` with a release tag such as `v0.1.0`.

```sh
curl -fsSLO https://github.com/Dhruv2mars/javelin/releases/download/TAG/install.sh
sh install.sh --version TAG
javelin --version
```

This installs `javelin` to `~/.local/bin/javelin`. If that directory is not on your
`PATH`, the script says so; it does not edit your shell profile.

Options:

- `--bin-dir DIR` installs elsewhere (default `$HOME/.local/bin`, or `JAVELIN_BIN_DIR`).
- `--archive FILE --sums FILE` installs a local archive after checking it against a
  local `SHA256SUMS` file. Use this for files you downloaded by hand.

## Install on Windows

Replace `TAG` with a release tag such as `v0.1.0`. Run this in Windows PowerShell or
PowerShell 7.

```powershell
Invoke-WebRequest https://github.com/Dhruv2mars/javelin/releases/download/TAG/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Version TAG
javelin --version
```

This installs `javelin.exe` to `%LOCALAPPDATA%\Programs\Javelin\bin`. The script does
not change `PATH`; add that directory yourself if you want `javelin` on every shell.

## What the installer checks

- The archive's SHA-256 must match its line in the release's `SHA256SUMS` before
  anything is extracted or written. On a mismatch, the installer stops and the
  existing binary is unchanged.
- The binary is written to a temporary file in the destination directory and renamed
  into place. An interrupted install does not leave a partial `javelin`.
- Nothing runs with elevated privileges. Nothing else on the machine is changed.

To check the files yourself:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS
sha256sum -c SHA256SUMS --ignore-missing         # Linux
```

Each archive is also attested with GitHub build provenance. With the GitHub CLI:

```sh
gh attestation verify javelin-TAG-x86_64-unknown-linux-gnu.tar.gz --repo Dhruv2mars/javelin
```

## Upgrade

Run the installer again with a newer `--version`. It replaces `javelin` in the same
place, atomically. Your repositories are not touched: a repository is a `.javelin`
directory inside the project, and the binary does not rewrite it on install.

The release workflow checks one upgrade path: the immediately previous build of the
same source, and then the new build, both on a clean runner. Repositories created
by the previous build still open with the new one. Upgrades from older public
releases are not tested yet, because no public release exists.

## Uninstall

Delete the installed binary. Repositories are left alone.

```sh
rm ~/.local/bin/javelin          # macOS and Linux
```

```powershell
Remove-Item "$env:LOCALAPPDATA\Programs\Javelin" -Recurse   # Windows
```

## Known limits

- Binaries are not code-signed or notarized on macOS or Windows.
- The Javelin CLI is pre-GA. See [cli.md](cli.md) for the commands it has, and the
  [build checklist](../TASKS.md) for what is not built yet.
- Source builds work as described in the [quickstart](quickstart.md).
