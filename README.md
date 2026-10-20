# Javelin

Javelin is agent-native version control built around immutable accepted World versions
and isolated Layers. Its runtime is independent of Git.

Start with the [developer quickstart](docs/quickstart.md) to build from source,
try repository metadata commands, and follow the prerequisite-gated Linux workflow.
Metadata works on macOS, Linux, and Windows; writable native workspaces are Linux-only
today, with explicit checkpoints. Clone-backed Layers for all three platforms are
next ([workspace approach](docs/workspace-approach.md)).

The local VCS and hosting service are under active development; no GA release or
complete hosting workflow is available. See the [architecture](docs/architecture.md),
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
