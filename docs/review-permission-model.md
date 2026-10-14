# Review and permission model

Status: draft implementation in `crates/javelin-collaboration` (PR #13).
Base: `e56fc86e3415217de2b960a7ea0dbe6d2eb450b1`, the accepted but unmerged Layer
state dependency. This unit cannot claim independent acceptance.

## Scope

A pure deterministic library for one Publish Request's review state and its
evaluation against the repository's currently applied permissions.

- No SQLite use, second durable database, HTTP, identity provider, credential
  verification, or World advancement. The path dependency on the root `javelin`
  crate is for `ObjectId`, `VersionNumber`, and `OperationId` only. It compiles
  the root crate's SQLite dependency transitively, but this library never opens storage.
- No Git. Tests build proposals with the public native repository API.
- `check_publishable` evaluates the supplied current state only. It is not an
  authorization token or ticket and does not make remote Publish atomic.
- Durable storage, provider integration, protocol mapping, and credential
  checks are future work.

## Caller API

```rust
let review = Review::new(repository_id, request_id);
let applied = review.apply(&Command { operation, actor, action }, &current_permissions)?;
// applied.review is the new value; applied.replayed is true for an identical retry.
let result = check_publishable(&applied.review, &exact_revision, &current_permissions, &publisher);
```

- `RevisionRef { base: VersionNumber, manifest: ObjectId, context: Option<ObjectId> }`.
  The context is an unvalidated opaque reference. No trace kind, storage, or
  confidentiality is implied. No new codec or object ID is defined.
- `PrincipalId`, `RepositoryId`, and `RequestId` are opaque. They are validated at
  the boundary: 1..128 ASCII letters, digits, `-`, `_`, `.`, or `:`.
- `AppliedPermissions { repository, epoch, visibility, roles, required_approvals }`.
  These are the applied permissions and review policy for one repository.
- `Action`: `Open { revision }`, `Revise { expected, revision }`,
  `Approve { index, revision }`, `Comment { index, body }`. Revision indexes start at 1.
- `Refusal` is a typed enum with stable `code()` strings. It never wraps or
  matches `javelin::Error`.

## Rules

| Action | Who | Additional checks |
| --- | --- | --- |
| Open | Any principal with access | Request not yet open. The opener becomes the only author. |
| Revise | The author only, including against maintainers | `expected` equals the current index. The revision differs from the current one. |
| Approve | `Review` or higher, not the author | `index` is current and `revision` equals it exactly. No duplicate approval. |
| Comment | Any principal with access | `index` names an existing revision. |
| Publish check | `Maintain` | `exact` equals the current revision. Enough current approvals exist. |

Further rules:

- Access: a private repository grants access only through `roles`. A public
  repository grants every valid principal `Read`, which covers opening, revising
  their own request, and commenting. Outside contributors cannot approve or Publish.
- Submitted revisions are immutable. A new revision starts with no approvals.
  Earlier revisions keep their approvals as history.
- Approvals bind the revision index and the exact base, manifest, and context.
  Resubmitting earlier content (A→B→A) does not revive old approvals.
- Approvals record principals, never roles. `check_publishable` counts only
  approvers who hold `Review` in the supplied permissions, so revocation and
  downgrade take effect without a stale role cache.
- `required_approvals` may be zero, which makes reviews optional. A `Maintain`
  role is always required.
- `check_publishable` does not compare `exact.base()` with the current World.

## Retries, ordering, and epochs

- Operation IDs bind accepted commands only. An identical retry returns the
  unchanged state with `replayed = true`, even after permissions change. The same
  ID with any different actor or action is refused with `operation_id_reused`.
- Refused commands are not recorded and do not change state. A retry is evaluated
  again, so it can succeed after a permission grant or after a missing revision arrives.
- `Revise.expected` and `Approve.index` detect stale observations. Commands that
  arrive out of order are refused with `not_open`, `stale_revision`, or
  `unknown_revision`, and succeed when replayed in order.
- Operation IDs are scoped to one Publish Request state. The store keys state by
  `(repository, request)`, so an `Open` retry loads the existing state and replays.
  The node must namespace or check operation IDs across requests.
- Each state records the highest permissions epoch it applied and refuses lower
  epochs in both `apply` and `check_publishable`. Equal epochs are accepted.
  Node-wide epoch monotonicity belongs to integration.

## Integration obligations

At the publication correctness boundary, later integration must:

1. Load the current review state and current applied permissions and policy.
2. Rerun `check_publishable` for the exact revision.
3. Advance the World through the repository's own compare-and-advance step, in
   the same correctness boundary. The `expected` version is `RevisionRef.base`
   when the World has not moved. Otherwise the author refreshes and submits a new revision.

A prior `Publishable` result grants nothing and must not be stored or forwarded.

Dependency direction: a later protocol integration depends on collaboration.
Collaboration never depends on the protocol, merge, or provider crates.

## Candidates considered

1. **Direct immutable transitions.** Typed methods such as `review.revise(...)`.
   Retry binding needs a separate input fingerprint per method, which reinvents a
   command value. Rejected.
2. **Command plus retained accepted-command log.** This was selected. One
   `apply(&Command, &AppliedPermissions)` call returns a new value. The accepted
   commands are both the history and the retry-binding record, so no separate
   event type is needed.
3. **Repository-wide state holding every request.** This was proposed by the
   architecture review to make operation IDs repository-scoped. Rejected because
   the contract keeps review state independently partitionable and reserves
   repository-wide total ordering for World advancement. Per-request scoping is
   documented above.

## Verification

Local commands, run from `crates/javelin-collaboration` with Rust 1.99.0:
`cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked --all-targets`, and the compiled `review_flow` example
compared with `examples/review_flow.expected`.

- 12 behavior tests use a real temporary native repository. Each proposal comes from
  a Layer capture whose checkpoint manifest is stored through `put_manifest`, plus
  a context blob.
- A deterministic property test runs 400 seeds × 60 commands. It covers random
  permissions, epoch regressions, verbatim retries, and same-ID different-input
  commands. It checks append-only history, a fixed author, no self-approval, unique
  operation IDs, author-only revisions, determinism, and soundness of every allowed
  publish result.
- A manual mutation check reverted 13 guards one at a time: author check,
  self-approval, current-role filter, replay input comparison, epoch, approval
  reset, approval index, revise expectation, publish role, exact revision,
  public-role ceiling, repository match, and refusal-before-binding. The suite caught all 13.

Hosted three-OS results are recorded in `TASKS.md`.

## Limits

- No durable format or serialization. `VersionNumber` has no `Deserialize`.
- No closing, withdrawing, or dismissing approvals, and no change requests.
- No authenticated identity. Callers supply already-authenticated principals.
- No trace policy, restricted-context access, fork, or organization model.
- Comment bodies are uninterpreted text of at most 64 KiB.
