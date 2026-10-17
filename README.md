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
