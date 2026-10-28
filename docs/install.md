# Install and upgrade Javelin

Javelin is pre-GA, and **no release is published yet**: the commands below work once the
first release is on npm ([releases.md](releases.md)). Until then, build from source as the
[quickstart](quickstart.md) describes.

Every install method below installs two commands: `javelin`, the CLI, and `javelin-node`,
which hosts repositories.

## With npm or bun

```sh
npm i -g @dhruv2mars/javelin
# or
bun add -g @dhruv2mars/javelin

javelin --version
javelin init
```

The package `@dhruv2mars/javelin` pulls in one platform package with the native binaries
for your machine. The commands run those binaries, not Node. Do not install with
`--omit=optional` or `--no-optional`, because that leaves out the binaries.

## Without npm (macOS and Linux)

```sh
curl -fsSL https://javelin-codes.vercel.app/install.sh | sh
```

This downloads the same platform package from the npm registry, checks it against the
registry's SHA-512 integrity, and installs both binaries into `~/.javelin/bin`. If that
directory is not on your `PATH`, the installer prints the line to add to your shell profile;
it never edits the profile itself. It needs `curl`, `tar` and `shasum` or `sha512sum`, and no
Node.

Options, after `sh -s --` (or as environment variables):

- `--version 0.1.0` (`JAVELIN_VERSION`) installs that version or dist-tag instead of `latest`.
- `--install-dir DIR` (`JAVELIN_INSTALL_DIR`) installs elsewhere.
- `--registry URL` (`JAVELIN_NPM_REGISTRY`) reads another npm registry, such as a mirror.

## Windows (experimental)

Windows builds are published but have never run: there is no Windows test machine. In
Windows PowerShell 5.1 or PowerShell 7:

```powershell
irm https://javelin-codes.vercel.app/install.ps1 | iex
javelin --version
```

This installs `javelin.exe` and `javelin-node.exe` into `%USERPROFILE%\.javelin\bin` and
prints how to add it to your user `PATH`. `npm i -g` on Windows installs the binaries too,
but its commands go through a `sh` shim, so they need a `sh` on `PATH`.

## Platforms

| Platform | Package | Needs | Status |
| --- | --- | --- | --- |
| macOS, Apple silicon | `@dhruv2mars/javelin-darwin-arm64` | macOS 11 or newer | GA |
| macOS, Intel | `@dhruv2mars/javelin-darwin-x64` | macOS 11 or newer | GA, untested |
| Linux, x86_64 | `@dhruv2mars/javelin-linux-x64` | glibc 2.28 or newer | GA |
| Linux, arm64 | `@dhruv2mars/javelin-linux-arm64` | glibc 2.28 or newer | GA |
| Windows, x86_64 | `@dhruv2mars/javelin-win32-x64` | Windows 10 or newer | experimental, untested |

musl systems such as Alpine have no build. [releases.md](releases.md) says where each build
runs and what has been tested on it.

## What the curl installer checks

- The tarball's SHA-512 must match the registry's `dist.integrity` for that version before
  anything is extracted or written. On a mismatch the installer stops and installed binaries
  stay as they were.
- Each binary is written to a temporary file beside its destination and renamed into place,
  so an interrupted install never leaves a partial `javelin`.
- Downloads use HTTPS; plain HTTP is accepted only from `127.0.0.1` or `localhost`.
- Nothing runs with elevated privileges, and nothing outside the install directory changes.

## Upgrade

Run the same install again (`npm i -g @dhruv2mars/javelin@latest`, `bun add -g
@dhruv2mars/javelin@latest`, or the curl line). Your repositories are not touched: a
repository is a `.javelin` directory inside the project, and installing does not rewrite it.

## Uninstall

```sh
npm rm -g @dhruv2mars/javelin        # or: bun rm -g @dhruv2mars/javelin
rm -r ~/.javelin/bin                 # after the curl installer
```

```powershell
Remove-Item "$HOME\.javelin\bin" -Recurse
```

## Known limits

- Binaries are not code-signed or notarized on macOS or Windows.
- The packages carry no npm provenance, and the curl installer trusts the registry's
  integrity over HTTPS, as npm does.
- See [cli.md](cli.md) for the commands the CLI has, and the
  [build checklist](../TASKS.md) for what is not built yet.
