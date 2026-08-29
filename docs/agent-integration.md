# Agent integration

Javelin does not launch agents. An orchestrator creates a Private Layer, reads its absolute Managed view path, then starts an external actor in that directory.

```sh
layer_path="$(javelin layer create api --from world)"
session="$(javelin provenance begin --layer api --actor codex --kind agent --model gpt-5.6-luna)"

# The external orchestrator runs its actor with $layer_path as cwd.

javelin --project "$layer_path" hook operation-start --session "$session"
javelin --project "$layer_path" hook operation-end --session "$session"
javelin --project "$layer_path" provenance end "$session"
javelin publish api --idempotency-key api-1
```

`operation-start` reconciles the view. `operation-end` reconciles and Refreshes at the caller-declared safe boundary. World or parent updates only emit awareness events between boundaries; Javelin never changes an active view on a timer.

## Machine protocol

Use `--json` for command responses and `events --jsonl` for streams. Every response has `schema_version: 1`. Errors are emitted to stderr with a stable exit code, error code, details, and recovery commands. Schemas live in [`schemas/`](../schemas/).

```sh
javelin layer create worker --from world --json
javelin events --since 42 --follow --jsonl
```

Event cursors are monotonic and resumable. A reconnecting consumer passes the last cursor it processed.

## Provenance

Sessions accept normalized events and immutable native attachments:

```sh
javelin provenance event \
  --session "$session" \
  --event-type tool-call \
  --payload '{"tool":"bun test","summary":"run suite"}'
javelin provenance attach --session "$session" transcript.jsonl --media-type application/jsonl
```

Supported event vocabulary includes prompt, response, tool-call, tool-result, file-read, file-write, subagent, token, error, and lifecycle. Payloads are untrusted annotations. Javelin derives authoritative touched paths from Checkpoint tree transitions.

Ordinary output hides raw payloads. `provenance show SESSION --raw` is explicit access. `provenance purge SESSION` removes retained raw payloads while preserving tombstones, identities, and Contribution relationships.

## Claims

Claims are passive leased declarations. They report likely overlap but never lock files or schedule work.

```sh
javelin layer create ui --claim 'src/ui/**'
javelin claim list --json
```

Use one Private Layer or child Layer per writer isolation boundary. Multiple processes writing one view share fate.
