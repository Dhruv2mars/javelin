# Durable Layer Stack

Stack imports a frozen source checkpoint into an exact target Layer head. Clean
changes, including mechanically compatible edits in one file, enter one target
head event. The target keeps its original World base and unrelated private or
accepted content. Neither Layer is refreshed or published automatically.

The standalone `javelin-stack` crate depends on `javelin` and `javelin-merge`.
The native library never depends on either downstream crate. The current root
binary cannot depend on this adapter without a cycle; a later separate CLI
package owns that integration. This unit adds no CLI or filesystem runtime flow.

## Public caller API

```rust,ignore
use javelin_stack::{stack_layer, StackError, StackOutcome};

let outcome = stack_layer(&mut repo, &operation, &target_head, source_checkpoint)?;
match outcome {
    StackOutcome::Updated(javelin::LayerUpdate::Applied { state }) => { /* captured */ }
    StackOutcome::Updated(javelin::LayerUpdate::Stale { state }) => { /* owner rechecks */ }
    StackOutcome::Conflicts(conflicts) => { /* owner resolves explicitly */ }
}
```

Arguments are `&mut Repository`, `&OperationId`, `&LayerHead`, and `CheckpointId`.
`StackError::Storage(javelin::Error)` preserves native failures.
`StackError::Merge(javelin_merge::MergeError<javelin::Error>)` preserves typed
`Load` and `Resource` outcomes. Conflict entries retain exact base/target/source
entries and structural collision partners. No guessed semantic resolution occurs.

The adapter uses the source's immutable World manifest as the three-way base,
the exact target manifest as ours, and the frozen source manifest as theirs.
An unchanged source path takes the target entry, including absence. Thus unrelated
accepted changes from a newer source base do not leak into an older target, and
an older source does not remove unrelated content accepted in a newer target.
The clean result is normalized back into a complete delta against the target's
original World base.

Only the public bounded merge API is used. Blob loads use its per-Blob admission
limit. Resource and loader failures and conflicts write no objects, checkpoints,
head events or operation reservations. Newly generated merged Blobs are stored
only after the whole merge is clean. They can remain unreferenced if a later
storage error or concurrent writer prevents the target CAS. This is ordinary
immutable object preparation; no partial target checkpoint is committed.
Existing whole-graph validation still reads and verifies stored content. The
merge admission limits are not a new global repository-validation resource bound.

## Native atomic storage boundary

`Repository::commit_stack(operation, expected, source, changes)` accepts a clean
prepared complete target-base delta. It is a storage primitive; callers needing
mechanical merge semantics use the adapter. In one immediate SQLite transaction
it validates the state and both graphs, prepares the checkpoint with source
ancestry, applies the target revision CAS, and records the exact input and outcome.
It returns `javelin::Result<LayerUpdate>`. Published targets refuse new writes
with the existing typed `LayerPublished` error.

`Repository::stack_operation(operation, expected, source)` returns
`javelin::Result<Option<LayerUpdate>>`. The adapter uses this before preparation.
Original success and stale retries return the original observation even after
later capture, discard or Publish. Changing the target token or source with the
same ID rejects. The native commit also binds the normalized prepared result and
rejects capture/discard reuse of a Stack operation, and the reverse. Existing
World/Layer operation namespaces and capture/Publish replay behavior are retained.
Conflict and resource refusals reserve no ID.

`LayerCheckpoint::stack_sources()` exposes sorted unique frozen source checkpoint
IDs. Capture carries these references forward; clone shares the checkpoint;
discard retains it; Publish retains its final head and history. Source advance,
discard or Publish cannot change the imported snapshot. A source checkpoint can
itself retain sources, so ancestry is transitive without copying source content.
All source edges are in the content-addressed checkpoint bytes. An iterative
graph walk verifies every source and its content, rejects cycles and invalid
references, and avoids revisiting shared subgraphs within a read. Existing
checkpoint, Layer-state, mutation and repository integrity boundaries validate
these graphs. The exact Stack input and original outcome have an immutable hash-bound record,
protecting source bindings even when two sources were already retained.

## Unshipped storage format 4

Format 4 adds append-only `stacks` rows referring to Layer attempts, frozen source
checkpoints and input-binding Blobs. Their update/delete triggers match the other
immutable tables. Checkpoints add a canonical `sources` field only when nonempty;
ordinary checkpoints without ancestry keep their previous byte encoding.
Formats 1–3 are unshipped and refused by open/init without migration or rewrite.
The format-3 refusal test compares the closed database bytes before and after both
calls. This is a small development-format boundary, not a migration facility.

## Verification scope

`tests/layer_stack.rs` checks input bindings, native stale/success retries, ancestry
retention through capture/clone/discard/Publish, transaction rollback, corrupted
source graphs and bindings, canonical encoding, and old-format refusal.
`tests/support/process.rs` uses pipe barriers and independent processes to prove
kill-before-commit rollback, kill-after-commit recovery, Stack/capture CAS races,
and simultaneous exact retries appending once. Hooks exist only in library tests.
`crates/javelin-stack/tests/stack.rs` exercises actual bounded merging, within-file
and disjoint changes, every conflict category, resource/loader refusal, different
World bases, source lifetime and transitive ancestry.

Local macOS checks establish this native API behavior. Hosted Linux/Windows runs,
hardware power loss, mounted workspace integration, automatic capture, explicit
resolution commands, owner-controlled refresh and the later CLI command remain
outside this unit. No runtime, storage, or test code invokes or interprets Git.
