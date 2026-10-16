# Native whole-Layer Publish

`Repository::publish_layer` accepts one complete, exact, latest Active captured
checkpoint. It reads immutable native records, never uncaptured working files.
The caller supplies the operation ID, repository-issued `LayerHead`, declared
attribution and a guard. Partial selection requires preparing another Layer.

```rust
use javelin::{PublishAttribution, PublishOutcome};

let receipt = repo.publish_layer(
    &operation,
    &captured_head,
    &PublishAttribution::Human,
    |locked_world, composed_manifest| Ok(()),
)?;
if let PublishOutcome::Published { version } = &receipt.outcome {
    // version is the next immutable World, with the whole captured Layer delta.
}
let recovered = repo.publication(&operation)?;
```

## Composition and conflicts

One SQLite IMMEDIATE transaction validates the existing World/Layer/publication
records, observes the current World, and computes its end-to-end difference from
the immutable Layer base. It applies the complete canonical Layer delta to the
locked current World. Independent World changes survive; the Layer base stays
unchanged. An empty delta can also publish one next World version.

Exact paths and ancestor/descendant paths intersect. An intersection produces a
durable `Conflict`, even when World and Layer independently reach identical new
bytes. `Conflict.paths` is the sorted unique union of both endpoints of every
intersecting pair. `a` and `ab` do not intersect. Different children of an implicit
directory can compose. File kind, symlink target, executable mode and deletion
all participate in entry equality. Directory deletions still require exact entry
tombstones, as in capture.

World changing A to B and back to A leaves that path unchanged relative to base,
so it does not block composition. This rule uses the base/head manifests, not a
union of intervening changes. Publish adds no semantic merge, forced refresh,
automatic Stack, resolution, discard, tests or reviews.

## Atomic state and retry

Format 3 adds an append-only `publications` table with immutable receipt Blobs.
On success, one ordered commit binds the composed manifest, next World version,
World attempt, exact Layer identity/final checkpoint, attribution/context, receipt
and logical Published state. Existing object kinds, hashes, manifest/checkpoint
bytes, capture tables and their constraints stay unchanged. A partial unique
index permits only one successful publication per Layer.

The three durable outcomes are `Published`, `Conflict`, and `LayerStale`.
LayerStale records the observed Active, Discarded or Published state when the
supplied token is no longer the latest Active head. Same operation and exact
input return the original receipt after later World/Layer advances, including
original conflict paths and stale observations. `publication` recovers it after a
lost acknowledgment. Changed head, attribution, context or override flag returns
`OperationIdReused`. Raw `compare_and_advance` and Publish cannot reuse each
other's operation IDs in either direction, including failed durable publications
that have no World attempt. Existing Layer capture/discard ID scope is separate.

A published Layer returns `LayerState::Published` with its exact final head,
publication operation and World version. Current status projects this terminal
record; history appends it after the unchanged original capture events. Fresh
capture/discard returns `LayerPublished` without objects or rows. Previously
committed capture/discard operations still replay their original historical
outcomes. A new Publish operation records LayerStale; the accepted original
operation replays. Frozen checkpoint read/diff and clone remain available.

Public history/version, repeated init, check, raw CAS, Layer status/mutation and
publication boundaries validate complete native state. Receipt validation
reconstructs composition/conflicts against the original recorded World and stale
state from the original recorded event/publication. It checks exact relational
bindings and canonical receipt Blob bytes. Internal checkpoint base validation
uses the existing raw World validator, avoiding recursive publication validation.
Full object/manifest graph validation and old missing-tail checks remain intact.
Object-only bounded availability/read guarantees are unchanged.

Format 2 is unshipped and is rejected by open/init without migration or rewrite.
Trigger/schema authentication and coherent removal of every historical anchor
remain outside the existing integrity guarantee. This is not tamper-proof history.

## Attribution and guard

`PublishAttribution::Human` needs no trace. Agent attribution carries one optional
opaque immutable Blob reference and `allow_missing_trace`. Missing context with
false refuses with `MissingAgentTrace` and reserves no operation. Missing context
with true commits warning `missing_agent_trace`. A supplied context always passes
existence, kind and hash verification under the transaction; override cannot
excuse a corrupt or wrong-kind context. The override flag remains part of exact
input even with a supplied context. Attribution is declared, not authenticated.

The `FnOnce(&WorldVersion, &Manifest) -> Result<()>` guard runs only for a
publishable candidate, under the same write lock, before inserting any objects or
metadata. Err leaves zero new objects, versions, attempts, publications or Layer
rows and reserves no operation ID. A local caller can return Ok. Conflict,
LayerStale and receipt replay do not invoke it. This is a future host integration
hook, not an authorization ticket. It implements no identity, permissions, policy
store, provider or durable hosted-request closure. A later host must check current
state and reconcile request closure in its actual persistence boundary; this
closure cannot atomically close an independent external database with SQLite.

## Design decisions and limits

The completed current-parent architect review selected a receipt-backed terminal
projection over changing the Layer event discriminator. The implementation keeps
historical events/retries unchanged, checks raw/Publish operation ownership before
replay, and directs full-state validation above raw history/checkpoint validators.
These settle review findings R1, R2 and R4. The selected single transaction avoids
the World-churn/livelock of prepare/seal and two-store TOCTOU/crash boundaries.
The older grounding proposal is design history only; revision base and current
publication head are separate throughout this implementation.

R3 is an explicit binding follow-on. Linux `certain` does not yet classify
`LayerPublished` as a definite refusal, and mount/recovery recognizes Discarded
rather than Published. A binding capture/discard after publication may retain
pending/frozen state; a new mount can mislabel Published as discarded. This PR
changes no binding/helper code. Logical native mutation refusal does not prove
physical mounted views read-only or safe post-publication binding recovery.

Tests in `tests/layer_publish.rs` cover composition, intersections, retries,
terminal state, attribution, guard/refusal, tampered bindings and format refusal.
`tests/support/process.rs` exercises disjoint/overlapping publishers, same-ID and
raw-ID races, capture/discard races, readers, process kills and lost replies with
pipe barriers. Hooks exist only under `cfg(test)`. Hardware power loss, mounted
lifecycle, autosave, Stack/refresh, hosting, performance, network filesystems,
Gate 2 and GA are not established by this bounded native API.


## Implementation checklist

The October 16 workflow replaces the original additional review/audit rounds.
This bounded unit uses existing PR23 and leaves root TASKS unchanged from its
starting dependency so the hub's main checklist survives integration.

- [x] Apply the completed architect's four corrections and report the excluded
  Linux binding integration gap to the hub.
- [x] Implement atomic composition, durable outcomes, exact retries, attribution,
  guard refusal, full receipt validation and logical Published state.
- [x] Run all 16 Publish integration tests and six new pipe-controlled process tests.
- [x] Run the full locked all-target/all-feature debug suite: 97 passed, one ignored
  child-process entry point. The retained run also includes all 16 Publish tests.
- [x] Run Clippy with warnings denied across all targets/features.
- [x] Pass all six release process tests and the locked release build. Compile/run
  an external caller against the production library; verify boundary-hook markers
  are absent from the release library, binary and caller.
- [ ] Make existing PR23 regular/ready; hub owns integration and merge.

Retained local debug log: `/tmp/javelin-publish-pr23/oct16-full-debug.log`.
The earlier parent baseline and architect report are historical inputs, not proof
of this implementation. No new source-review round is required or claimed.

Release logs are `oct16-release-process.log`, `oct16-release-build.log` and
`oct16-release-caller.log` in that same local scratch directory. Formatting and
warnings-denied Clippy pass. These are local macOS results; new-source hosted
Linux/Windows outcomes are not claimed. No full release suite or new independent
source review was run under the proportional October 16 workflow.
