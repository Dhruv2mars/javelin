# Install and upgrade Javelin

Javelin is pre-GA, and **no release is published yet**: the commands below work once the
first release is on the release host ([releases.md](releases.md)). Until then, build from
source as the [quickstart](quickstart.md) describes. There are no package-manager installs
(Homebrew, crates.io, npm).

## Install on macOS or Linux

```sh
curl -fsSL https://javelin-codes.vercel.app/install.sh | sh
```

This installs `javelin` and `javelin-node` into `~/.javelin/bin`. If that directory is not
on your `PATH`, the installer prints the line to add to your shell profile; it never edits
the profile itself. Then:

```sh
javelin --version
javelin init
```

Options, after `sh -s --` (or as environment variables):

- `--version v0.1.0` (`JAVELIN_VERSION`) installs that version instead of the latest.
- `--install-dir DIR` (`JAVELIN_INSTALL_DIR`) installs elsewhere.
- `--release-base URL-OR-DIR` (`JAVELIN_RELEASE_BASE`) reads releases from another host or a
  local directory with the same layout.
- `--archive FILE --sums FILE` installs an archive you downloaded, after checking it against
  its `SHA256SUMS`.

## Install on Windows

In Windows PowerShell 5.1 or PowerShell 7:

```powershell
irm https://javelin-codes.vercel.app/install.ps1 | iex
javelin --version
```

This installs `javelin.exe` and `javelin-node.exe` into `%USERPROFILE%\.javelin\bin` and
prints how to add it to your user `PATH`. Set `$env:JAVELIN_VERSION`,
`$env:JAVELIN_INSTALL_DIR` or `$env:JAVELIN_RELEASE_BASE` first to change what it does. The
Windows build and this installer have not run on Windows yet: there is no Windows runner.

## Platforms

| Platform | Target | Needs |
| --- | --- | --- |
| macOS, Apple silicon | `aarch64-apple-darwin` | macOS 11 or newer |
| macOS, Intel | `x86_64-apple-darwin` | macOS 11 or newer (untested) |
| Linux, x86_64 | `x86_64-unknown-linux-gnu` | glibc 2.28 or newer |
| Linux, arm64 | `aarch64-unknown-linux-gnu` | glibc 2.28 or newer |
| Windows, x86_64 | `x86_64-pc-windows-gnu` | Windows 10 or newer (untested) |

musl systems such as Alpine are refused. [releases.md](releases.md) says where each build
runs and what has been tested on it.

## What the installer checks

- The archive's SHA-256 must match its line in that version's `SHA256SUMS` before anything
  is extracted or written. On a mismatch the installer stops and installed binaries stay
  as they were.
- Each binary is written to a temporary file beside its destination and renamed into
  place, so an interrupted install never leaves a partial `javelin`.
- Downloads use HTTPS; plain HTTP is accepted only from `127.0.0.1` or `localhost`.
- Nothing runs with elevated privileges, and nothing outside the install directory changes.

To check the files yourself, download `SHA256SUMS` and the archive from
`RELEASE_BASE/VERSION/` and run `shasum -a 256 -c SHA256SUMS --ignore-missing` (macOS) or
`sha256sum -c SHA256SUMS --ignore-missing` (Linux).

## Upgrade

Run the installer again. It replaces both binaries in place. Your repositories are not
touched: a repository is a `.javelin` directory inside the project, and installing does
not rewrite it.

## Uninstall

```sh
rm -r ~/.javelin/bin                                  # macOS and Linux
```

```powershell
Remove-Item "$HOME\.javelin\bin" -Recurse              # Windows
```

## Known limits

- Binaries are not code-signed or notarized on macOS or Windows.
- `SHA256SUMS` is served from the same host as the archives and is not signed.
- See [cli.md](cli.md) for the commands the CLI has, and the
  [build checklist](../TASKS.md) for what is not built yet.
