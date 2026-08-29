# CLI reference

## Global options

```text
--project PATH   discover World from PATH instead of current directory
--json           emit schema-versioned machine output
```

Global options may appear before or after subcommands. Human output and JSON report the same state. Streaming events use JSONL.

## World and inspection

```text
javelin init [PATH]
javelin version
javelin status [--ignored]
javelin checkpoint [--reason TEXT]
javelin diff [FROM] [TO] [-- PATH]
javelin history [--layer LAYER] [--path PATH]
javelin show REF[:PATH]
javelin world current
javelin world history
javelin world restore VERSION [--accept-failing --reason TEXT]
```

`show` streams human file output. JSON includes hexadecimal bytes up to 1 MiB and reports larger content as omitted with object metadata.

## Private Layers

```text
javelin layer create NAME [--from REF] [--target world|layer:LAYER] [--claim GLOB]
javelin layer list
javelin layer show LAYER
javelin layer path LAYER
javelin layer restore CHECKPOINT [--layer LAYER]
javelin refresh [LAYER]
javelin verify [LAYER]
javelin publish [LAYER] [--idempotency-key KEY]
javelin discard [LAYER] [--cascade|--reparent REF] [--purge]
```

`layer create` prints absolute Managed view path. With no `--from`, creation uses current view's latest Checkpoint. `--from world` pins Current World. Child targets use `--target layer:PARENT`.

## Conflicts

```text
javelin conflict list [LAYER]
javelin conflict show ID
javelin conflict resolve ID --use base|target|private|edited
```

Resolution appends a Checkpoint. Prior Conflict evidence remains unchanged. `edited` scans selected Layer view under synchronized tracking policy.

## Provenance

```text
javelin provenance begin --actor NAME [--kind KIND] [--model MODEL] [--layer LAYER]
javelin provenance event --session ID --event-type TYPE [--payload JSON]
javelin provenance attach --session ID PATH [--media-type TYPE]
javelin provenance end ID
javelin provenance show ID [--raw]
javelin provenance search QUERY
javelin provenance purge ID
javelin explain PATH
```

`show` hides event payloads unless `--raw` is explicit. Normalized raw events use `javelin.provenance.v1` envelope.

## Claims and events

```text
javelin claim list
javelin claim renew ID [--seconds N]
javelin claim release ID
javelin events [--since CURSOR] [--follow] [--jsonl]
```

Claims are informational leased declarations. Javelin reports simple path overlap and never blocks work because of a Claim.

## Hooks

```text
javelin hook operation-start [--session ID]
javelin hook operation-end [--session ID]
javelin hook session-start [--session ID]
javelin hook session-end [--session ID]
```

Every hook reconciles current view. `operation-end` also Refreshes because caller declared a safe mutation boundary.

## Operations

```text
javelin doctor
javelin fsck
javelin repair [--view LAYER]
javelin gc [--dry-run]
javelin discarded list
javelin discarded recover LAYER
javelin discarded purge LAYER
javelin completions bash|elvish|fish|powershell|zsh
```

## References

Accepted forms:

```text
world, current
v1, v2, ...
layer:NAME
Layer name or ID
Layer Checkpoint ID
```

## Exit codes

```text
0   success
2   invalid command or argument
3   no Javelin World found
4   Conflict requires resolution
5   required verification failed
6   stale state or retryable concurrent change
7   storage corruption or failed integrity check
8   resource busy or lock timeout
9   unsupported filesystem or path feature
10  policy or permission rejection
```

Machine errors:

```json
{
  "schema_version": 1,
  "ok": false,
  "error": {
    "code": "CONFLICT",
    "message": "short stable explanation",
    "details": {},
    "recovery": ["javelin conflict list layer-name"]
  }
}
```

Machine successes use `schema_version: 1`, `ok: true`, and `result`.
