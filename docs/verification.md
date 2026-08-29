# Verification

World Rules are explicit project commands stored in `javelin.toml`. Javelin runs them in an isolated candidate Managed view before a World Publish or restore.

```toml
[[verification.rule]]
name = "typecheck"
command = ["bun", "run", "typecheck"]
required = true
timeout_seconds = 600

[[verification.rule]]
name = "tests"
command = ["bun", "test"]
required = false
timeout_seconds = 600
```

Commands are argument arrays, not shell strings. A timeout records exit code `124`. Required failures stop Publish with exit code `5`; informational failures are recorded but do not stop acceptance.

Javelin applies policy without self-bypass:

1. Run required rules from current accepted target policy.
2. Also run new required rules introduced by the candidate.
3. A removed or weakened rule remains effective while that policy change is evaluated.
4. Store command, required flag, exit code, duration, environment summary, stdout/stderr object IDs, candidate root, and policy hash.

`javelin verify LAYER` runs the same candidate checks without accepting state. Ordinary Publish has no bypass. Administrative World restore may use `--accept-failing --reason TEXT`; the new Version records the failing evidence and explicit override.

World Rules are trusted project code executed with the user's operating-system authority. Candidate filesystem isolation does not sandbox processes, network, credentials, or the host.
