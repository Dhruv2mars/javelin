# Javelin

Javelin is local, agent-native version control. It stores accepted project states as immutable World Versions and gives each human or external coding agent an isolated Private Layer. Stable writes become Layer Checkpoints automatically. Publish integrates one Contribution into current accepted state after conflict handling and configured checks.

The short pitch is "Git 3.0 for the agentic era." That describes the product category and ambition. Javelin is not a Git version, wrapper, compatibility layer, or object translator.

Javelin never launches agents. An editor, coding agent, or orchestrator asks Javelin for a Layer path, runs work in that directory, and invokes hooks or CLI commands at safe boundaries.

## Release state

Version `1.0.0` is under acceptance testing. The current source passes the core lifecycle, crash-boundary, installed-binary, and 100-Layer stress tests on Apple Silicon macOS. No public `v1.0.0` release has been published. Linux and Windows code paths remain unverified until native CI passes.

## Install from source

Requirements: Rust 1.85 or newer and a C toolchain for bundled SQLite and zstd.

```sh
cargo install --path .
javelin version
```

The packaging script creates a platform-labeled archive and SHA-256 manifest under `dist/`:

```sh
./scripts/package-release.sh
```

## First World

```sh
mkdir demo
cd demo
javelin init

javelin layer create api --from world
api_path="$(javelin layer path api)"

# Run an external tool with "$api_path" as its working directory.

javelin --project "$api_path" checkpoint --reason "API ready"
javelin publish api --idempotency-key api-ready-1
javelin world current
```

`javelin init` works in an empty directory or an existing non-Git project. It creates `javelin.toml`, `.javelinignore`, and `.javelin/`. Existing tracked files become World Version `v1`. Foreign VCS metadata is excluded as opaque content and never parsed.

## Everyday lifecycle

Create one Private Layer per isolation boundary:

```sh
javelin layer create server --from world
javelin layer create ui --from world
javelin layer list
```

Each command reconciles the relevant Managed view. The project Monitor also records stable writes after the configured debounce period. No add, stage, or commit ceremony exists.

Inspect tentative work:

```sh
javelin status
javelin diff
javelin history --layer server
javelin show world:src/main.ts
```

Refresh only at a declared safe boundary:

```sh
javelin refresh server
javelin --project "$(javelin layer path server)" hook operation-end
```

Publish into World:

```sh
javelin verify server
javelin publish server --idempotency-key server-ready-1
```

Independent work joins the linear World history. Divergent path states create stored Conflicts with base, latest target, and private evidence:

```sh
javelin conflict list server
javelin conflict show CONFLICT_ID --json
javelin conflict resolve CONFLICT_ID --use private
```

Discard never changes accepted bytes:

```sh
javelin discard experiment
javelin discarded list
javelin discarded recover experiment
javelin discarded purge experiment
```

Restore appends state instead of rewriting history:

```sh
javelin world restore v2
javelin layer restore CHECKPOINT_ID --layer server
```

## Required checks

World Rules use argument arrays. Javelin runs them in an isolated candidate view before acceptance.

```toml
[[verification.rule]]
name = "typecheck"
command = ["bun", "run", "typecheck"]
required = true
timeout_seconds = 600

[[verification.rule]]
name = "tests"
command = ["bun", "test"]
required = true
timeout_seconds = 600
```

Current accepted required rules always run. A candidate that adds a required rule must pass it before the policy becomes accepted. Removing or weakening a rule cannot bypass the accepted policy.

## Agent integration

Critical commands support `--json`. Event streams use JSONL.

```sh
session="$(javelin provenance begin --layer server --actor codex --model gpt-5.6-luna)"
javelin provenance event \
  --session "$session" \
  --event-type prompt \
  --payload '{"summary":"Implement server route"}'
javelin provenance end "$session"
javelin explain src/server.ts --json
javelin events --since 0 --follow --jsonl
```

Javelin derives authoritative touched paths from state transitions. Supplied path claims and trace payloads are annotations. Javelin stores them but does not trust them as evidence that a file changed.

## Recovery

```sh
javelin doctor
javelin fsck
javelin repair
javelin gc --dry-run
```

Javelin Store is canonical. Managed views and immutable root caches are reconstructable. `fsck` validates SQLite integrity, foreign keys, object headers, zstd streams, lengths, types, hashes, and references. `repair` invalidates the selected root cache and rebuilds views from retained Checkpoints.

## Trust boundary

Javelin provides version-control isolation. It does not contain hostile processes. World Rules and external tools run with the user's operating-system authority. Raw provenance can include prompts, file content, tokens, and external tool data. Metadata uses user-only permissions by default; retention and redaction remain local policy.

## Documentation

- [Architecture](docs/architecture.md)
- [Storage format](docs/storage-format.md)
- [CLI reference](docs/cli.md)
- [Agent integration](docs/agent-integration.md)
- [Verification](docs/verification.md)
- [Recovery](docs/recovery.md)
- [Security and trust](docs/security-and-trust.md)
- [Limitations](docs/limitations.md)
- [Roadmap](docs/roadmap.md)
- [Benchmarks](docs/benchmarks.md)
- [Canonical glossary](CONTEXT.md)
- [Architecture decisions](docs/adr/)

## Development

Javelin development uses Git and GitHub. Javelin runtime and tests do not.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
./tests/acceptance/installed_binary.sh
```

The installed-binary acceptance harness places a fake failing `git` executable first on `PATH` and requires zero invocations.

Javelin is licensed under MIT.

