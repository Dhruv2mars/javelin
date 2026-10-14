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
- Exception for fixtures only: the tests and the `review_flow` example open a
  temporary native repository and advance its World to seed real proposals.
  The library itself never does either.
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
- `check_publishable` does not compare `exact.base()` with the current World. A
  World advance does not invalidate approvals; only a new revision does.

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
- Each state records the permissions epoch of its last accepted, non-replayed
  transition. Lower epochs are refused by new transitions and by `check_publishable`.
  Equal epochs are accepted. An identical retry of an accepted command replays
  before any permission check, so it succeeds unchanged even with an older epoch.
  Node-wide epoch monotonicity belongs to integration.

## Integration obligations

At the publication correctness boundary, later integration must:

1. Load the current review state and current applied permissions and policy.
2. Rerun `check_publishable` for the exact frozen `RevisionRef`. This validates
   its base, code, and context against the current approved revision, and the
   publisher's current role. It says nothing about the current World.
3. Separately, the future VCS publisher validates the frozen proposal against the
   current World. The proposal base and the compare-and-advance `expected` value
   are different things:
   - `RevisionRef.base` is the immutable base the proposal and its approvals refer to.
   - `expected` is the World head locked for this publication. It equals
     `RevisionRef.base` only when the World has not advanced.
   - If the World advanced without intersecting the proposal's changes, the
     publisher composes the disjoint delta onto the current head and publishes.
     This does not force the author to refresh, and it never mutates the approved
     revision.
   - If the World advanced with intersecting changes, the publisher rechecks and
     reports explicit conflicts. The owner decides whether to refresh and submit a
     new revision, which starts without approvals.
4. Steps 2 and 3 happen within the same publication correctness boundary as the
   World advance.

Example. Alice's revision is approved with base World 4. A maintainer publishes
while the World is at 6, and Worlds 5 and 6 touched only unrelated paths.
`check_publishable(review, &rev, &perms_now, &mona)` is evaluated now. The
publisher composes Alice's frozen delta onto World 6 and calls compare-and-advance
with `expected = 6`. No new revision or approval is needed, and the review state
still names base 4. If World 5 had edited a path Alice changed, the publisher
would report the conflict, and only Alice could submit a replacement revision.

This library does not implement the publisher, composition, or conflict detection.

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

- 13 behavior tests. Twelve use a real temporary native repository with Worlds 1
  and 2 sharing one tree. Each proposal comes from a Layer capture whose
  checkpoint manifest is stored through `put_manifest`, plus a context blob. The
  base-only test changes only the base and checks approval, invalidation,
  publish check, and retry binding. The thirteenth test validates boundary input.
- A deterministic property test runs 400 seeds × 60 commands. It covers random
  permissions, epoch regressions, verbatim retries, and same-ID different-input
  commands. It checks append-only history, a fixed author, no self-approval, unique
  operation IDs, author-only revisions, determinism, and soundness of every allowed
  publish result.
- `scripts/mutations.sh` requires a passing baseline, then reverts 16 guards one
  at a time. Each mutation must compile and then fail at least one named test;
  a mutation that survives, does not compile, or does not apply fails the script. The guards are: author check, self-approval, current-role
  filter, replay input comparison, epoch, approval reset, approval index, revise
  expectation, publish role, exact revision, public-role ceiling, repository match,
  transition permission validation, revision equality on base, revision equality on context,
  and the unchanged-revision refusal. It runs on the Linux CI job. The base
  equality mutation came from independent review r1. It survived the earlier suite
  and is caught only by the base-only test.

Hosted three-OS results, run links, and tested heads are recorded in this unit's
`TASKS.md` section. CI tests the
PR head, not a merge with its base.

## Limits

- No durable format or serialization. `VersionNumber` has no `Deserialize`.
- No closing, withdrawing, or dismissing approvals, and no change requests.
- No authenticated identity. Callers supply already-authenticated principals.
- No trace policy, restricted-context access, fork, or organization model.
- Comment bodies are uninterpreted text of at most 64 KiB.
