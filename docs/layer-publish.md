# Bounded native whole-Layer Publish

Status: design sketch only. Implementation and proof are pending.
Parent: hub-accepted PR18 source `3a54021b15ac5002c21f492b6bed07740c682421`.
Owner: draft PR23, branch `feat/whole-layer-publish`. This unit does not independently
accept its parent. PR18 is evidence-only; protocol PR15 is separate.

## Outcome and boundary

One call publishes the complete exact latest Active captured checkpoint. It never
reads live edits. One SQLite IMMEDIATE transaction validates the full existing
World/Layer graph, checks exact input, computes intersections and composition,
runs the guard, and commits the composed manifest, ordered World advancement,
publication receipt and logical terminal state together. No mandatory tests,
reviews, semantic merge, refresh, Stack or policy engine are added.

World changed paths are the end-to-end difference between the immutable Layer
base manifest and the locked World head manifest. A net revert A to B to A leaves
that path unchanged. An exact or ancestor/descendant intersection conflicts even
if World and Layer independently reach identical new bytes. Composition applies
the complete canonical Layer delta to the locked head, retaining both independent
changes and preserving the original immutable Layer base.

## Two simple shapes

A. One call with a small append-only `publications` table. A successful publication
is the terminal record for its Layer; `layer_state` exposes a distinct Published
variant, while original capture event rows and exact capture retry receipts stay
unchanged. `layer_history` retains the original events and appends the publication
state. The table binds the input, original against-World, observed Layer event,
outcome and immutable canonical receipt Blob. Successful rows share their
operation with the existing accepted World attempt/version. Failed durable rows
have no World attempt. Full validation recomputes composition or conflict paths
against that original World and verifies every receipt reference and byte.

B. One call with a new published discriminator in `layer_heads`, a new Layer event
operation shape and a publication table. This unifies terminal events, but it
changes the capture/discard schema and accepted-attempt validation. It requires
special handling so old capture retries still return their original Active state.

Select A provisionally because it preserves existing capture schema and retry
semantics. Both shapes keep one transaction and one store. Prepare/seal and an
external receipt store are already rejected by the hub for World churn/livelock
and TOCTOU/two-store crash boundaries. This review is not reopening that choice.

## Caller and types

Proposed usage, not executable code yet:

```rust
let receipt = repo.publish_layer(&operation, &captured_head,
    &PublishAttribution::Human, |locked_world, composed_manifest| {
        // Optional host checks current permissions/policy here in future.
        Ok(())
    })?;
let recovered = repo.publication(&operation)?;
```

Proposed public signatures and types:

```rust
pub enum PublishAttribution {
    Human,
    Agent { context: Option<ObjectId>, allow_missing_trace: bool },
}
pub enum PublishWarning { MissingAgentTrace }
pub struct PublicationReceipt {
    pub operation: OperationId,
    pub expected: LayerHead,
    pub attribution: PublishAttribution,
    pub against: WorldVersion,
    pub warnings: Vec<PublishWarning>,
    pub outcome: PublishOutcome,
}
pub enum PublishOutcome {
    Published { version: WorldVersion },
    Conflict { paths: Vec<RepoPath> },
    LayerStale { actual: LayerState },
}
// Add distinct LayerState::Published { head, operation, version }.
pub fn publish_layer<F>(&mut self, operation: &OperationId,
    expected: &LayerHead, attribution: &PublishAttribution, guard: F)
    -> Result<PublicationReceipt>
where F: FnOnce(&WorldVersion, &Manifest) -> Result<()>;
pub fn publication(&self, operation: &OperationId)
    -> Result<Option<PublicationReceipt>>;
```

The guard runs on a publishable candidate under the same write lock before any
new objects or rows. Errors refuse without binding the operation. No guard is
needed to replay the original durable receipt. Missing Agent context refuses
without writes unless explicitly overridden, then records `missing_agent_trace`.
A supplied context must be an existing valid Blob, verified under the lock.
The attribution is declared, not authenticated. Context is opaque immutable bytes.

All durable outcomes replay exactly after later World or Layer changes, including
original conflict paths and terminal/stale state. Changed input and raw operation
collisions reject with OperationIdReused in both directions. Existing Layer capture
operation-ID scope stays separate; no global namespace redesign is intended.
A new capture/discard operation on a published Layer returns a typed refusal.
Existing committed capture retries retain their original outcome without writes.
A new Publish operation on a terminal Layer records LayerStale; the original
accepted Publish operation replays. Clone of the frozen checkpoint is permitted.

## Module and validation ownership

`src/publish.rs` owns the public Publish types, append-only publication schema,
composition/conflict logic, transaction API and full receipt validation.
`src/layer.rs` exposes only necessary crate-private exact head/event access and
adds Published status/history and mutation refusal without altering checkpoint
bytes or capture ID scope. `src/repository.rs` initializes format 3 and publication
immutability triggers and wires complete validation into init/check/history/raw
CAS. Its existing full object/manifest/history validators remain authoritative.
`src/lib.rs` exports the bounded types and any necessary typed refusal errors.
No Cargo, workflow, binding/helper, CLI or other crate changes are planned.
Format 2 is rejected without migration or byte rewriting. Object/checkpoint/hash
formats remain unchanged. Validation avoids recursion: publication validation may
use existing history/layer graph validators; those internal validators must not
recursively call publication validation. Public correctness boundaries call both.

## Required evidence, still pending

(a) Base-head Publish and exact final checkpoint/provenance.
(b) Disjoint current-World composition without Layer base mutation, plus net revert.
(c) Exact/prefix/delete-modify/structural/symlink/executable intersections, including
identical-new-result conflicts.
(d) Stale, discarded and already-published Layer outcomes and immutable clones.
(e) Every durable retry after later advances, changed input and raw reuse both ways.
(f) Real process kill before commit, after commit/lost acknowledgment, readers
without partial references, pipe barriers and release hook exclusion.
(g) Concurrent disjoint Layers all publish contiguously; overlapping Layers have
exactly one winner; retries preserve all results.
(h) Human/Agent attribution, valid/missing/overridden/corrupt/wrong-kind contexts.
(i) Guard observes locked current World and exact candidate; refusal changes no
objects, versions, attempts, publications or Layer heads and reserves no operation.
(j) Tampered receipts/references/composition and old accepted damage reject without
fallback. Compiled guard mutations must fail named defect assertions.
(k) Format 2 refusal preserves bytes; format checks, warnings-denied Clippy,
complete locked debug/release/all-features suites, release-library caller and
actual inherited three-OS CI logs with exact source identities.

A fresh final independent Codex source/evidence review is required. Same-family
review limits will be stated. Logical native mutation refusal does not prove
physical mounted views read-only. Hardware power loss, scale, performance,
network filesystems, full Gate 2, hosting and GA remain unverified.
