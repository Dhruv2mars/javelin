# Security and trust

Javelin provides version-control isolation. It is not hostile-process containment.

## Guarantees within its boundary

- A Private Layer changes no target until successful Publish.
- Accepted objects and state records are immutable.
- Publish selects all candidate paths or none through one short database transaction.
- Conflicts retain base, latest target, and private evidence.
- Managed views and immutable root caches are reconstructable.
- Stored paths reject absolute paths, parent traversal, NUL, non-UTF-8 names, internal metadata, and case-fold collisions.
- Stored symlinks are leaf data and are never followed during materialization.

## Outside its boundary

World Rules, editors, agents, and other external tools run with the user's OS authority. They may access the network, credentials, unrelated files, or other processes unless the caller adds container, VM, sandbox, or account isolation. Javelin does not authenticate users, isolate hostile code, manage secrets, deploy applications, or provide remote synchronization.

## Sensitive data

Raw provenance may contain prompts, responses, file contents, tokens, command output, and external tool data. `.javelin/` receives user-only permissions on supported Unix systems. Normalized indices redact configured secret-shaped keys, ordinary output hides raw payloads, retention is local policy, and purge leaves a non-sensitive tombstone.

`.javelinignore` seeds exact secret and cache patterns. It is not a security boundary: ignored files remain accessible to any process with filesystem permission. `status --ignored` reports excluded paths and matching rules.

Foreign VCS directories such as `.git/` are excluded as opaque paths. Javelin never reads or translates them and never invokes Git.
