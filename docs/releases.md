# Releases: where binaries live and how one is published

GitHub releases are gone with the GitHub account. This page records where Javelin's
binaries live instead, how a release is built and staged, and what publishing does.
[install.md](install.md) is the user-facing side.

**State on 2026-10-28: nothing is published.** The release script, installers and proofs
exist; the first publish waits for the owner's go-ahead. Until then the installers the
forge serves have no release host and say so, and the landing page says installers are
coming (`INSTALLERS_PUBLISHED` in `apps/forge/src/landing.ts`).

## Decision: a public Cloudflare R2 bucket

Binaries go in an R2 bucket, `javelin-releases`, on the owner's Cloudflare account (the
account that already runs the Cloudflare node). The installers stay on the forge's Vercel
site, so the one-liner is `curl -fsSL https://javelin-codes.vercel.app/install.sh | sh`.

R2 free tier, per month, Standard storage only
([R2 pricing](https://developers.cloudflare.com/r2/pricing/), read 2026-10-28):

| Limit | Free | What a release uses |
| --- | --- | --- |
| Storage | 10 GB-month | about 35 MB per version (5 archives of 6 to 9 MB), so roughly 280 versions |
| Class A (writes, lists) | 1 million | about 10 per publish |
| Class B (reads) | 10 million | 3 per install (`latest`, `SHA256SUMS`, the archive) |
| Egress | free, including r2.dev | |

The first publish serves the bucket on its `r2.dev` URL. Cloudflare says that URL is for
non-production use: it has a variable rate limit, answers 429 beyond it, and gets no
caching ([public buckets](https://developers.cloudflare.com/r2/buckets/public-buckets/)).
That is acceptable pre-GA. Before GA, connect a custom domain on Cloudflare (for example
`releases.javelin.codes` once the domain is bought) and restamp the installers; the base URL
is one variable for exactly this. A Worker with an R2 binding on `workers.dev` is the
fallback if the domain is late (Workers free plan: 100,000 requests a day).

Rejected: serving binaries from the forge's Vercel output. A Hobby deployment would carry
every version's archives on every deploy (100 MB per static file, 100 deploys a day), and
downloads would count against Hobby's 100 GB a month of Fast Data Transfer, beyond which
Vercel pauses the project, taking the forge down with it
([Vercel limits](https://vercel.com/docs/limits)). R2 has no egress charge at all.

## Layout on the host

    BASE/latest                                   v0.1.0-alpha.2 (one line)
    BASE/v0.1.0-alpha.2/SHA256SUMS                every file below, sha256sum format
    BASE/v0.1.0-alpha.2/manifest.json             version, source commit, toolchain, per-target
                                                  archive SHA-256, size, binaries' SHA-256,
                                                  where each was built and what was run
    BASE/v0.1.0-alpha.2/javelin-v0.1.0-alpha.2-TARGET.tar.gz   (.zip for Windows)
    BASE/v0.1.0-alpha.2/install.sh, install.ps1   the installers stamped for BASE

Each archive holds `javelin-VERSION-TARGET/bin/javelin`, `bin/javelin-node`, `LICENSE`
and `README.txt`. A version directory is never rewritten; `latest` is the only object that
changes. The installers check the archive against `SHA256SUMS` before they write anything.
`SHA256SUMS` comes from the same host over HTTPS, so it catches corruption and partial
uploads, not a compromised bucket; signing the manifest is not built yet.

## Targets, where they build, and what is tested

`bun tools/release/release.ts build` runs on the owner's Mac (Apple silicon) with the
pinned Rust 1.99.0, cargo-zigbuild 0.24.0 and zig 0.16.0. Linux builds target glibc 2.28.

| Target | Built | Install proof (one-liner, `--version`, `init`, publish, tamper refused) |
| --- | --- | --- |
| `aarch64-apple-darwin` | Mac, cargo | the Mac, loopback host |
| `x86_64-apple-darwin` | Mac, cargo | none: no Intel Mac and no Rosetta here |
| `x86_64-unknown-linux-gnu` | Mac and Buildkite, cargo-zigbuild | Buildkite, `ubuntu:24.04` amd64 |
| `aarch64-unknown-linux-gnu` | Mac and Buildkite, cargo-zigbuild | Buildkite, `ubuntu:24.04` arm64 under qemu |
| `x86_64-pc-windows-gnu` | Mac, cargo-zigbuild | none: Windows has no runner |

Buildkite proves the Linux recipe on every push (`.buildkite/pipeline.yml`: release build,
then `tools/release/proof.sh` in a plain Ubuntu container); it rebuilds rather than testing
the Mac's own archives. The Windows build links the Universal C Runtime (Windows 10 and
later) and is unrun, and `install.ps1` has not run since its rewrite.

## Making a release

    bun tools/release/release.ts build --version v0.1.0-alpha.2 --base-url URL

builds every target this machine can (all five on macOS; the Linux and Windows ones
elsewhere), refuses a dirty worktree or a version whose number differs from
`crates/javelin-cli`, runs the host binaries' `--version`, and stages `dist/release/`.
To try it as a user would:

    bun tools/release/release.ts serve                      # 127.0.0.1:4873
    sh tools/release/proof.sh http://127.0.0.1:4873 0.1.0   # fresh HOME, git stand-in

Stage with `--base-url http://127.0.0.1:4873` for this.

## Publishing (needs the owner's go-ahead)

    CLOUDFLARE_ACCOUNT_ID=... CLOUDFLARE_API_TOKEN=... \
      bun tools/release/release.ts publish --version VERSION --public-url URL [--latest] [--dry-run]

`publish` checks every staged file against `SHA256SUMS`, refuses installers not stamped for
URL, refuses a version already at URL, then uploads `VERSION/*` with wrangler (SHA256SUMS
last, cached for a year as immutable) and, with `--latest`, writes `latest` (cached 60 s).
`--dry-run` prints the uploads without credentials.

The first publish, step by step. Everything is on Cloudflare's and Vercel's free tiers;
nothing is paid:

1. Cloudflare dashboard: create R2 bucket `javelin-releases` (Standard storage, automatic
   location), turn on its public `r2.dev` URL, and create an API token with R2 Object
   Read & Write on that bucket only.
2. Build and stage with `--base-url https://pub-....r2.dev` on a clean `main`.
3. `publish --version v0.1.0-alpha.2 --public-url https://pub-....r2.dev --latest`.
4. Set `DEFAULT_RELEASE_BASE` in `scripts/install.sh` and `$DefaultReleaseBase` in
   `scripts/install.ps1` to that URL, set `INSTALLERS_PUBLISHED = true`, merge, and run
   `bun run deploy` in `apps/forge`.
5. Run `sh tools/release/proof.sh https://javelin-codes.vercel.app 0.1.0 https://pub-....r2.dev`
   on a Mac and in a Linux container.
