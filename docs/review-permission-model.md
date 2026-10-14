# Review and permission model

Status: planning. Draft implementation of a pure deterministic Publish Request
review and permission state machine in `crates/javelin-collaboration`.

Base: `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, an accepted but unmerged dependency.

## Scope

- Pure library. No SQLite, second durable database, HTTP, identity provider,
  credential verification, or World advancement.
- `check_publishable` evaluates the supplied current state only. It is not an
  authorization token and does not make remote Publish atomic. Integration must
  recheck the exact revision, permissions, and policy at the publication
  correctness boundary.
- Durable storage and provider integration are future work.
