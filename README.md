# Javelin

Javelin is third-generation version control, built from scratch for agentic development.
The `javelin` CLI replaces Git, and the javelin.codes forge replaces GitHub. Each repository
lives on a node that runs on infrastructure its owner connects; Javelin runs no managed
hosting. The core is immutable World versions, isolated Layers and an atomic Publish, and
nothing in it invokes Git. Read [the direction](docs/direction.md) first.

Start with the [developer quickstart](docs/quickstart.md) to build from source.
Clone-backed Layers pass the filesystem checks on APFS, btrfs, XFS and ReFS, and the copy
fallback passes on ext4 and NTFS ([workspace approach](docs/workspace-approach.md)).

The CLI, the node and the forge are under active development. There is no GA release and
no complete hosted workflow yet. See the [architecture](docs/architecture.md),
[CLI and setup reference](docs/cli.md), [build contract](docs/build-contract.md), and
[build checklist](TASKS.md).

## Install

Pre-GA. Release builds with installers for macOS, Linux and Windows are coming. Until then,
build the CLI from source. Cloning needs an Entire login (`entire login`); the source is also
readable at https://entire.com/et/dhruv2mars/javelin. You need a Rust toolchain and a C
compiler:

```sh
entire repo clone /et/dhruv2mars/javelin
cd javelin
cargo install --locked --path crates/javelin-cli
javelin --version
```

This installs `javelin` into Cargo's bin directory. The [quickstart](docs/quickstart.md) walks
through a first repository. [docs/install.md](docs/install.md) describes the release
installers, which are not published yet.
