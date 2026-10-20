# Contributing to Javelin

Javelin is in active development and has no GA release yet. Read [AGENTS.md](AGENTS.md)
for repository rules and [docs/build-contract.md](docs/build-contract.md) before changing
product behavior. Open an issue first for a large change so the design can be agreed.

## Setup

- Rust 1.99.0, the toolchain CI pins:
  `rustup toolchain install 1.99.0 --profile minimal --component rustfmt --component clippy`
- A C compiler. SQLite is bundled.
- Optional: [bun](https://bun.sh) 1.4.3 for the filesystem probe in `tools/workspace-probe`.

## Checks

These are the checks CI runs on macOS, Linux and Windows. Run them before opening a PR:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
```

Javelin's runtime, storage, tests and user workflows must never invoke or interpret Git.
CI proves this with a failing `git` placed first on `PATH`. To reproduce locally:

```sh
shim=$(mktemp -d)
rustc --edition 2021 -o "$shim/git" tools/workspace-probe/shim.rs
PATH="$shim:$PATH" cargo test --locked --workspace --all-targets --all-features
test ! -e "$shim/invoked-git"
```

Git may be used to develop Javelin itself, such as for branches and commits. Product
code and tests may not call it.

Dependency licenses, RustSec advisories and sources are checked by `cargo deny check`
(policy in [deny.toml](deny.toml)). Install it with `cargo install cargo-deny --locked`.

## Gate 2 locally

Gate 2 proves ordinary filesystem behavior for clone-backed Layers. CI runs it on APFS,
ext4, XFS, btrfs, NTFS and ReFS. Locally you can run one volume. `--fs` is a label for
that volume, and `--expect` is `shared` on a clone-capable volume or `copy` on a plain
one. `--dir` must be an empty directory on that volume:

```sh
rustc --edition 2021 -o /tmp/shim tools/workspace-probe/shim.rs
mkdir -p /tmp/g2shims && for t in git sudo mount umount; do cp /tmp/shim /tmp/g2shims/$t; done
cargo build --locked --release -p javelin-cli
cargo build --locked --release -p javelin --example gate2_seed
bun tools/workspace-probe/gate2.mjs --javelin target/release/javelin \
  --seed target/release/examples/gate2_seed \
  --dir /tmp/gate2-run --fs apfs --expect shared --out /tmp/gate2.json --shims /tmp/g2shims
```

Exit 0 means every check passed. Exit 3 means only the failures listed in
`tools/workspace-probe/gate2-known-failures.json` occurred. Gate 2 is only proven once that
list is empty. The full harness and its controls are described in
[tools/workspace-probe/README.md](tools/workspace-probe/README.md). World bases are sealed
with mode 000, so run `chmod -R u+rwx` on the run directory before deleting it.

## Commits and pull requests

- Branch from `main`. Keep one task per branch.
- Make small commits prefixed `feat:`, `fix:`, `test:`, `chore:` or `refactor:`.
- Open a regular pull request against `main` that says what changed and how you checked it.
