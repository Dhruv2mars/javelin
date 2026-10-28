# Releases: Javelin on npm

GitHub releases are gone with the GitHub account. Javelin's binaries ship as npm packages
instead, and npm's registry is the only host: `npm i -g`, `bun add -g` and the curl
installer all read the same packages. [install.md](install.md) is the user-facing side.

**State on 2026-10-28: nothing is published.** The release script, installers and proofs
exist; the first publish waits for the owner's go-ahead and npm login. Until then the
landing page says installers are coming (`INSTALLERS_PUBLISHED` in
`apps/forge/src/landing.ts`).

GA platforms are macOS and Linux. Windows is experimental: it is built and published, never
run.

## Packages

All at one version, published together:

| Package | Holds |
| --- | --- |
| `@dhruv2mars/javelin` | the `javelin` and `javelin-node` commands (a bin shim and `install.js`), and every platform package as an exact-version optional dependency |
| `@dhruv2mars/javelin-darwin-arm64` | both binaries; `os: darwin`, `cpu: arm64` |
| `@dhruv2mars/javelin-darwin-x64` | both binaries; `os: darwin`, `cpu: x64` |
| `@dhruv2mars/javelin-linux-x64` | both binaries; `os: linux`, `cpu: x64`, `libc: glibc` (2.28 or newer) |
| `@dhruv2mars/javelin-linux-arm64` | both binaries; `os: linux`, `cpu: arm64`, `libc: glibc` (2.28 or newer) |
| `@dhruv2mars/javelin-win32-x64` | `javelin.exe` and `javelin-node.exe`; experimental |

This is the esbuild and Biome pattern. npm and bun install only the platform package whose
`os`, `cpu` and `libc` match. npm also runs the wrapper's `postinstall` (`install.js`), which
hard-links (or copies) the platform binaries over the bin shims, so `javelin` on PATH is the
native program. bun does not run a dependency's install scripts by default, so it links the shim
(`tools/release/npm/shim.sh`), a POSIX `sh` script. The shim follows its bin links, finds the
installed platform package, and `exec`s the binary, so it needs no Node. Measured on an M-series
Mac, `javelin --version` took about 20 to 25 ms run directly or through npm's linked binary, and
about 35 to 50 ms through the shim. npm's `bin` field cannot point into another package, so
the shim is the only way for a bin to reach the platform package's file.

The curl installer (`scripts/install.sh`, served by the forge at `/install.sh`) reads
`REGISTRY/@dhruv2mars/javelin-PLATFORM/VERSION` (default `latest`), downloads that version's
tarball from the same registry, and checks it against the registry's `dist.integrity`
(SHA-512) before it writes anything. Then it installs both binaries into `~/.javelin/bin`.
`install.ps1` does the same on Windows and is experimental. Both trust the registry over HTTPS
as npm itself does. Packages are not signed and carry no npm provenance (that needs a CI
publisher with OIDC).

npm costs nothing for public packages. Each platform tarball is 6 to 9 MB, under npm's
package size limits.

## Targets, where they build, and what is tested

`bun tools/release/release.ts build` runs on the owner's Mac (Apple silicon) with the
pinned Rust 1.99.0, cargo-zigbuild 0.24.0 and zig 0.16.0.

| Package | Built | Install proof (`tools/release/proof.sh`) |
| --- | --- | --- |
| `darwin-arm64` | Mac, cargo | the Mac: `npm i -g`, `bun add -g` and curl, loopback registry |
| `darwin-x64` | Mac, cargo | none: no Intel Mac and no Rosetta here |
| `linux-x64` | Mac and Buildkite, cargo-zigbuild | Buildkite: npm (`node:24-slim`), bun (`oven/bun:1-slim`), curl (`ubuntu:24.04`) |
| `linux-arm64` | Mac and Buildkite, cargo-zigbuild | Buildkite: the same three, under qemu |
| `win32-x64` | Mac, cargo-zigbuild | none: Windows has no runner |

Each proof runs under a fresh `HOME`, as a non-root user on Linux, with a git stand-in first
on PATH. It installs, runs `javelin --version`, `javelin-node --version`, `javelin init` and a
first publish, then installs again. The npm proof also requires the native binary on PATH;
the curl proof also feeds the installer a tampered tarball, which must be refused. Buildkite
proves the Linux recipe on every push (`.buildkite/pipeline.yml`); it rebuilds rather than
testing the Mac's own tarballs, which come out byte-identical on rebuild from the same commit.

## Making a release

    bun tools/release/release.ts build --version 0.1.0-alpha.2

builds every target this machine can (all five on macOS), refuses a dirty worktree or a version
whose number differs from `crates/javelin-cli`, runs the host binaries' `--version`, and packs
the six tarballs with `bun pm pack` into `dist/npm/`, with `manifest.json`. On a subset of
targets the wrapper names only those platform packages, and `publish` refuses the set. To try
the packages as a user would:

    bun tools/release/release.ts serve                          # npm registry on 127.0.0.1:4873
    sh tools/release/proof.sh npm  http://127.0.0.1:4873 0.1.0
    sh tools/release/proof.sh bun  http://127.0.0.1:4873 0.1.0
    sh tools/release/proof.sh curl http://127.0.0.1:4873 0.1.0

## Publishing (needs the owner's go-ahead)

    bun tools/release/release.ts publish --tag TAG [--dry-run]

`publish` needs all six tarballs, unchanged since they were packed. It runs `npm publish` on
each, platform packages first and the wrapper last, with `--access public --tag TAG`. With
`--dry-run` it runs `npm publish --dry-run` and prints what would publish. A real run needs
`npm whoami` to succeed (`npm login`, or an automation token in `~/.npmrc`). After a partial
failure, rerun the same command: it skips packages already published with these exact bytes,
and stops if a version is up with different bytes.

The first publish, step by step:

1. On the owner's npm account (`dhruv2mars`, which owns the `@dhruv2mars` scope), run
   `npm login`, or create a granular automation token that can publish `@dhruv2mars/*`.
2. Build on a clean `main`: `bun tools/release/release.ts build --version 0.1.0-alpha.2`.
3. `bun tools/release/release.ts publish --tag latest --dry-run`, then without `--dry-run`.
4. Set `INSTALLERS_PUBLISHED = true` in `apps/forge/src/landing.ts`, merge, and run
   `bun run deploy` in `apps/forge`.
5. Run each proof against the real registry and site on a Mac and in Linux containers, e.g.
   `sh tools/release/proof.sh curl https://registry.npmjs.org 0.1.0 https://javelin-codes.vercel.app`.
