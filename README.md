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

Pre-GA. The release pipeline ships install scripts for macOS, Linux and Windows that
verify SHA-256 checksums and upgrade in place. Pin a release tag until GA:

```sh
curl -fsSLO https://github.com/Dhruv2mars/javelin/releases/download/TAG/install.sh
sh install.sh --version TAG
```

On Windows, use `install.ps1` with `-Version TAG`. Details, limits, and upgrade notes
are in [docs/install.md](docs/install.md). Without a release, build from source with
the [quickstart](docs/quickstart.md).
