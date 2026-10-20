# Online design: roadmap item 7 and the hosting phase

Status: proposed design, not accepted. Written 2026-10-20 on `docs/online-design`.
Nothing here is implemented or run. Claims about current code cite `docs/architecture.md`
or PR #52 (protocol v2 on trees, open, unmerged). Claims about the provider are marked
where they rest on search results rather than provider documentation.

Source sections used below:

- `C§` = `docs/build-contract.md`, by heading (Product boundaries, Version control,
  Hosting ownership, Contributions and reviews, Protocol and first provider, Verification
  gates, Work ownership).
- `G§` = `docs/git3-direction.md`, by number or heading (1 Online-first, 2 Data model,
  3 Storage split, 4 Rolling base, 5 No kernel VFS, Rejected, Open questions, Roadmap).
- `A§` = `docs/architecture.md`, by heading.
- `T` = `TASKS.md`, "Prove hosting and collaboration" and "Git 3.0 roadmap".

Dependencies: PR #51 and PR #52 (roadmap item 4), and the rolling base (roadmap item 5,
unchecked). Items 7 depends on 5, so attach waits for it (see units U4).

---

## 0. Done for this design

- Every piece in the brief has: stored data, wire messages, outage behavior, first slice,
  and tests.
- Each contract point the design relies on is cited.
- Every open point is in "Decisions for the user" with a recommended default.
- Ordered PR-sized units with owned paths and dependencies are in "Implementation units".

This design is verified by reading only. Its acceptance is the hub's review and the
units' own tests.

## 1. Contract tensions the hub must reconcile

These are not settled by the contract or G§. The design picks a reading and flags it.

- **T1. Quorum vs. a single coordinator.** `C§Protocol and first provider` names "a small
  World publication coordinator" for Cloudflare. `C§Hosting ownership` says the World
  authority "is a quorum-replicated log, so no single node failure stops Publish." A single
  coordinator is not quorum-replicated unless the provider's storage replicates each
  acknowledged write across failure domains. G§Open questions records this as UNCONFIRMED.
  The design uses one ordering primitive for both cases (section 4.5). Decision D2.
- **T2. "Upload as captured" vs. no daemon.** `C§Version control` says checkpoints "upload as
  they are captured; there is no push." G§Rejected rules out "a client daemon," and `A§Overview`
  says capture runs at Javelin commands and agent lifecycle events. Checkpoints are durable
  locally at capture, but they reach the node only when a command runs. Decision D16.
- **T3. Publish on the wire is World-level, not Layer-level.** PR #52's `Publish{operation,
  expected: VersionNumber, tree}` advances the World with a whole tree. `A§Publish` defines
  Publish on a Layer head (`expected` is a `LayerHead`), and `C§Version control` says Publish
  "accepts the whole Layer." Section 3 adds Layer-level messages. The tree-level `Publish`
  (tag 0x30) stays for protocol tests and native imports only.
- **T4. `javelin-collaboration` names.** `RevisionRef { base, manifest, context }` still uses
  "manifest." Under G§2 the identity is a tree root. Rename it when unit U9 lands. This is not
  a product change.
- **T5. "Publish setting" is undefined.** `C§Hosting ownership` lists it in repository
  creation. Decision D12 defines it.
- **T6. Revocation vs. outage.** `C§Hosting ownership` says routine operations need no live
  control-plane lookup and that authorized clients continue "within credential validity and
  applied permissions." Revocation and role removal therefore have a bounded lag. Section 4.7
  and decisions D6 and D8 make it explicit.

## 2. Architecture in one page

- **Repository node** (one logical node per repository, on one provider, `C§Hosting ownership`).
  It owns objects, the World log, Layer state, Publish Requests, revisions, approvals,
  checks, traces, applied permissions and operation IDs. Its core is a Rust crate,
  `javelin-node`, behind storage traits. Tests use file-backed stores. The first provider
  uses object storage and a Worker adapter (units U12 and U13).
- **Control plane** (central, not self-hosted, `C§Hosting ownership`). It owns identity, orgs,
  memberships, discovery, provisioning, and the signing keys for tokens. It is a separate
  service in front of the node, not a node component.
- **Client** (`javelin` CLI). Captures locally, keeps a bounded object cache and a rolling base
  (`G§4`), queues remote work in an outbox (section 4.4), and talks to the node directly
  after authentication.
- **Wire**: the protocol v2 frame (`crates/javelin-protocol/src/codec.rs`) over HTTP/2. One
  message per request, with the same encoded body as today. The HTTP `Content-Length`
  replaces the `u32` length prefix. Each message is `POST /v2/r/{repo}` with a bearer token;
  the response is one message. Notifications stay optional and are deferred (D27).
- **One ordering primitive.** Every ordered sequence on the node is a chain of immutable
  slots. A slot is written with a create-if-absent precondition, which is a
  compare-and-swap on "the next number." The World log, each Layer's checkpoint chain, and
  each Publish Request's revision list use this. Each sequence is its own partition, so only
  World advancement is repository-wide (`C§Protocol and first provider`: "Only final World
  advancement needs repository-wide total ordering.").
- **Idempotency.** Every mutation carries an operation ID. The node records
  `(operation, request digest, outcome)`. Same ID and same digest return the recorded
  outcome. Same ID and a different digest return `operation_id_reused` (`A§Layers and
  checkpoints`).

## 3. Wire messages (extending protocol v2)

PR #52 already defines: Hello 0x01, HelloOk 0x02, Want 0x10, Missing 0x11, Get 0x12, Objects
0x20, Stored 0x22, Publish 0x30, Outcome 0x31, Resolve 0x40, Resolved 0x41, Error 0x7f. Kinds:
Blob 1, Tree 4. Kind 2 (Manifest) is retired. Kind 3 is reserved for Checkpoint, as
`A§Storage` already lists it.

Additions, all in `codec.rs`:

| Tag | Message | Fields | Reply |
| --- | --- | --- | --- |
| 0x03 | `Permissions` | none | `Applied` 0x06: `{ epoch, caller_role, visibility }` |
| 0x34 | `LayerAppend` | `{ operation, layer, parent: Option<CheckpointId>, checkpoint: ObjectId }` | `LayerOutcome` |
| 0x35 | `LayerHead` | `{ layer }` | `LayerState` 0x36: `{ layer, base: VersionNumber, seq, head, state }` |
| 0x37 | `LayerStack` | `{ operation, target: LayerId, target_head: CheckpointId, source: CheckpointId }` | `LayerOutcome` |
| 0x38 | `PublishLayer` | `{ operation, layer, expected: CheckpointId, base: VersionNumber, attribution }` | `PublishOutcome` 0x39 |
| 0x3a | `PublishRequest` | `{ operation, request, revision: u32, base: VersionNumber }` | `PublishOutcome` |
| 0x50 | `OpenRequest` | `{ operation, request, layer, checkpoint, base }` | `ReviewOutcome` 0x51 |
| 0x52 | `Revise` | `{ operation, request, expected_revision: u32, checkpoint, base }` | `ReviewOutcome` |
| 0x53 | `Approve` | `{ operation, request, revision: u32 }` | `ReviewOutcome` |
| 0x54 | `Comment` | `{ operation, request, revision: Option<u32>, body }` | `ReviewOutcome` |
| 0x55 | `ReadRequest` | `{ request }` | `RequestState` 0x56 |

Outcome shapes:

- `LayerOutcome`: `Accepted { seq, checkpoint }`, `Stale { head }`, `Refused { code }`.
  A replay returns `Accepted` with the original `seq`.
- `PublishOutcome`: `Published { number, id, tree }`, `Conflict { paths }`, `LayerStale { head }`,
  `Refused { code }`. Replays of a successful publish return `Published` with the original
  version.
- `ReviewOutcome`: `Accepted { revision }` or `Refused { code }`.
- `attribution` in `PublishLayer`: `Human`, or `Agent { context: Option<ObjectId>,
  allow_missing_trace: bool }`. A missing trace without the override refuses with
  `missing_agent_trace` (`A§Publish`).

Unchanged: `Want`/`Missing` for push, `Get`/`Objects` for fetch, `Resolve`/`Resolved` for the
version head. Tree-level `Publish` (0x30) is refused with `forbidden` unless the token carries
the `import` scope (T3).

Object kinds gain Checkpoint (3). Its bytes are the canonical `SparseChanges` JSON
(`A§Layers and checkpoints`). The object ID is SHA-256 over the kind byte and bytes, as for
the others. Limits: `MAX_OBJECTS` 1024, `MAX_BLOB` 8 MiB, `MAX_TREE` (`javelin::tree::MAX_CHUNK`),
`MAX_CHECKPOINT` (a new bound, sized from the largest checkpoint the Layer path produces; value
to measure, not guess).

Error codes used across messages: `unsupported_version`, `unauthorized` (bad, expired or
revoked token), `forbidden` (scope or role), `stale_permissions`, `operation_id_reused`,
`id_mismatch`, `missing_object` (lists the ids), `layer_stale`, `conflict`, `not_open`,
`not_author`, `self_approval`, `stale_revision`, `unknown_revision`, `not_enough_approvals`,
`missing_agent_trace`, `layer_published`. Client-side outcomes that never come from the wire:
`node_unavailable` (provider or network) and `control_plane_unavailable`. They must stay
distinct (`C§Verification gates` 4).

## 4. The pieces

### 4.1 Attach

**Stores.** Local `.javelin/attach.json`: node URL, repository ID, provider, last-known head
`{ number, id, tree }`, and a credentials reference. Local object cache: content-addressed
files for blobs and trees (`G§3(a)`). The rolling base (`G§4`) is one materialized tree per
repository per machine.

**Wire.** `Resolve { version: None }`, then the root-down fetch from PR #52 (`Get` by tree
level, then blobs). Anonymous read of a public repository needs no token (D24).

**Flow.** `javelin attach <repo>`: resolve the head, fetch the whole tree into the rolling
base, write `attach.json` last. Attach is idempotent: a second attach to the same repository
checks the head and exits.

**Outage.** Attach needs the node, so it fails with `node_unavailable` and writes nothing. After
attach, Layer creation from the rolling base works offline. A Layer then starts from the last
known base, which the contract allows (`C§Version control`: Layer work continues during an
outage). Publish later composes onto the current head (section 4.6). The base moves only
after an online advance (`G§4`).

**First slice (U4).** Attach to a repository on the in-process node over loopback, using the
existing full fetch. No base cache of its own: use the rolling base once roadmap item 5 is merged.

**Tests.**
- Attached base equals `manifest_at(head)` for a 70,000-entry snapshot (the PR #52 fixture).
- Attach with the node down exits `node_unavailable` and leaves no `attach.json`.
- Offline Layer create succeeds after attach; the base is not advanced offline.
- Second attach is a no-op.
- The failing `git` stand-in with positive control stays absent from the run (`C§Verification gates`).

### 4.2 Streaming checkpoints

**Stores.**
- Node: checkpoint objects (Checkpoint kind); Layer partition slots `layers/{L}/seq/{k}` with
  `{ seq, parent, checkpoint, operation }`; derived `live_paths` index (path → Layer, from
  each checkpoint's changed paths) for collision radar (`G§1`, `G§6`). The index is derived
  and rebuildable.
- Client: outbox records (section 4.4).

**Wire.** `Want` for the checkpoint and its blobs, `Objects` to upload bytes, then
`LayerAppend`.

**Ordering.** Upload every blob and the checkpoint object first. The node checks each one
(hash, kind, canonical form) before it commits the reference (`C§Protocol and first provider`:
"Objects must be readable and verified before committing references."). Then `LayerAppend`
creates slot `seq = head + 1` with `If-None-Match`. The parent must equal the current head
(`stale` otherwise).

**Outage.** Local capture is complete and durable first. Upload is inline with a bounded
budget (default 2 s, to measure). If the budget or the node fails, the record stays in the
outbox and the command returns `queued`. Visibility is delayed until a later command or
`javelin sync` reaches the node (T2, D16).

**Two writers.** A Layer has one owning machine (D21). A stale parent from a second writer is
reported as `layer_stale`; no write is forced.

**First slice (U5).** Sequential `LayerAppend` over loopback, inline upload, no outbox replay
yet.

**Tests.**
- Three captures in order produce `seq` 1 to 3, and the node's head equals the local head.
- Kill after objects are uploaded and before `LayerAppend`: a retry appends once; the node
  has no orphan reference.
- `LayerAppend` lost reply: the retry returns the same `seq`.
- Stale parent is refused, and the node's state does not change.
- The `live_paths` index rebuilt from checkpoints equals the incremental one.
- A checkpoint whose blob is missing is refused with `missing_object`.

### 4.3 Server-side compose

**What runs on the node.** Publish composition (already `compose_in`, `A§Publish`), Stack
(`javelin-stack` over `javelin-merge`), and conflict checks. The client only captures, uploads
and materializes.

**Store and data.** Inputs: checkpoints and blobs in the object store. Output: a new
checkpoint object and a `seq` slot on the target Layer, or structured conflicts. Nothing is
written on conflict.

**Invariants.**
- The same engine, the same limits (`javelin-merge` `Resource` limits, `A§Stack`), and the
  same bytes for the same inputs, locally and on the node. A golden test checks this.
- A limit refusal returns a code and writes nothing. The exact outcome is not traced
  (`A§Unverified`, `docs/bounded-mechanical-merge.md`); define it in this unit.

**Wire.** `LayerStack` (0x37).

**Outage.** Stack needs the node and is not in the outage queue (D7, D9). It fails with
`node_unavailable`, and local Layer work continues.

**Client effect.** After a remote Stack moves a head, the client must refresh before capture
(`A§Known limits`; `T`, "Clone-backed Layers": refresh a clone-backed directory from a Stack-produced head).
The refresh unit closes that item.

**First slice (U6).** Remote `LayerStack` on the in-process node with the golden test. No CLI
refresh until U6's second commit.

**Tests.**
- Golden: same inputs produce identical checkpoint bytes locally and on the node.
- Conflict writes nothing, and the target head is unchanged.
- Stale `target_head` is refused; a retry after refresh succeeds.
- A merge that exceeds limits returns the defined refusal and writes nothing.

### 4.4 Outage queue and replay

**Store.** An append-only journal per repository: `.javelin/outbox/{repo}.journal`. Each
record has a length, a checksum, `seq`, `kind` (`Upload`, `Append`, `Publish`, `Revise`),
the Layer or request, the operation ID, and references to payloads that are already durable
locally. The journal is primary state: it is not rebuildable from the server (unacknowledged
checkpoints exist only here), so it is not a SQLite index (`G§3`, D17). Each append is
fsynced.

**Order.** Per Layer or request: objects, then `LayerAppend`, then `PublishLayer`. Layers
replay concurrently with each other.

**Outcome handling.**
- Acknowledged records are removed after the node's recorded outcome is read back, or after
  a replay returns it.
- A lost reply means the record stays pending. Replay returns the recorded outcome, so there
  is still one durable outcome (`C§Verification gates` 4).
- A Publish that conflicts on replay records the conflict; the owner resolves it as usual
  (`G§1`).

**Outage classes** (these are different outcomes, `C§Verification gates` 4):

| Situation | Remote-only actions | Local work |
| --- | --- | --- |
| Node unreachable or 5xx (`node_unavailable`) | Uploads, `LayerAppend`, `PublishLayer`, `Revise` queue (D7). Stack, Approve, Comment, attach refuse. | Continues |
| Control plane unreachable (`control_plane_unavailable`), token still valid | Node actions continue, per applied permissions. Token refresh, repo create, membership and discovery refuse. | Continues |
| Control plane unreachable, token expired | Queued work waits for re-authentication. Nothing is dropped. | Continues |
| Both down | Queued work waits. Nothing new reaches the node. | Continues |

**Backoff.** Exponential with jitter, capped. Specific numbers are for measurement (D16).

**First slice (U3).** Journal with checksums, `Append` replay only, and a fault-injection
harness in the in-process node (drop reply after commit, refuse connection, delay).

**Tests.**
- Node down for N operations, then up: every record replays once, in order per Layer.
- Crash after the journal append and before send: replay sends once.
- Crash after server commit and before the client records the outcome: replay returns the
  same outcome; the World has one version.
- Journal with a torn last record (bad checksum): the record is ignored and reported, and
  earlier records are intact.
- Control plane down, token valid: `PublishLayer` succeeds. Token expired: record stays
  queued with `control_plane_unavailable`.

### 4.5 Quorum-replicated World log

**Requirement.** Append-only changeset log, one head, compare-and-swap advancement
(`G§3(b)`). Quorum replication, so no single node failure stops Publish (`C§Hosting
ownership`, `G§1`). Acknowledgement only after the write is durable (`C§Protocol and first
provider`: successful Publishes survive restart).

**Data.** One slot per version: `repo/{id}/w/{N}` holding canonical bytes of
`{ N, parent id, changeset id, root tree id, id, operation, attribution }`. The version `id`
is the hash chain of `A§Storage` (`D3` decides whether the root tree hash enters the chain).
A head hint `repo/{id}/head` holds the last known `N`; readers probe forward from the hint. An
operation record `repo/{id}/ops/{sha256(operation)}` holds the request digest and, after
commit, the version `N`.

**Commit path** (`PublishLayer` and `PublishRequest`):
1. Read the head `H`. Compose onto `H` (section 4.6, the boundary check happens here).
2. Create `ops/{op}` with `If-None-Match` as pending, with the digest.
3. Create slot `w/{H+1}` with `If-None-Match`.
   - On 412, read slot `H+1`. If its operation is ours, the commit already happened:
     return the recorded outcome. Otherwise re-prepare on `H+1`; this is the rebase path of
     `A§Publish`. Intersecting changes conflict.
4. Update `ops/{op}` to committed `N` (with `If-Match`), then the head hint.
5. Reply `Published`.

**Replay.** Read `ops/{op}`. Committed: return. Pending: if slot `w/{N}` exists for the
pending operation, finish step 4; otherwise continue at step 3. Digest mismatch returns
`operation_id_reused`.

**Why this shape.** Only the slot create is the ordering point. The head hint and the ops
record are recoverable from slots. Layer and review partitions reuse the same slot primitive,
so no per-repository coordinator is needed for ordering. Whether this counts as a quorum is
D2's question. Its durability rests on the object store's replication, which this design has
not verified.

**Provider evidence.** Two search results (2026-10-20, standard mode):
- Cloudflare's R2 S3-compatibility docs describe `If-None-Match: *` on PutObject as an
  atomic create, where one of many concurrent writers succeeds and the rest get 412 (CONFIRMED
  in search summary; the official page was not read in full).
- Miniflare had reversed `etagMatches` / `etagDoesNotMatch` behavior with `*` in an issue
  (github.com/cloudflare/miniflare/issues/774). Local emulators are not valid evidence for
  conditional writes. The spike must use a real bucket.
- Durable Objects: the 2024 SQLite-in-DO post says responses are held until their storage
  writes are confirmed (output gating). It does not state the replica count or failure domains.
  The replication claim is UNCONFIRMED.

**Spike (part of U12, before any production code).** On a real R2 bucket: 50 concurrent
publishers on one repository; exactly one per slot; ETag-conditional update of the ops record;
crash between steps 3 and 4 and between 4 and the reply. Record results in the PR.

**Outage.** Object store unavailable: Publish returns `node_unavailable`; the client queues it
(section 4.4). Nothing is written partially, because a slot is either created or not.

**First slice (U2 and U12).** `WorldLog` trait with a file-backed implementation and the
contract tests (uniqueness, replay, pending recovery). The R2 implementation follows the spike.

**Tests.**
- Concurrent publishers: one winner per slot; the rest rebase or conflict.
- Lost reply after slot create: replay returns the same version.
- Crash between slot and ops update: replay finishes the record.
- Stale head hint: readers find the true head.
- Corrupt slot (bad bytes or wrong `N`): refused, never served.

### 4.6 Repository authority, idempotent Publish, immutable transfers

**Authority.** `javelin-node` holds the World log (4.5), Layer partitions, review partitions,
traces and applied permissions behind traits. The in-process authority and the hosted node
use the same protocol and the same core code, with no separate path (`G§Open questions`,
recommended default).

**Immutable transfers.**
- `Objects` writes are verified before they are stored: the bytes hash to the claimed ID, the
  kind limit holds, and tree chunks are canonical (`codec.rs`, PR #52).
- Writes are create-if-absent. An existing ID is never overwritten. A repeated upload returns
  `Stored`.
- A reference (`LayerAppend`, `PublishLayer`, `Revise`) commits only if every referenced
  object is present. Otherwise `missing_object` lists the IDs and nothing is written.
- Transfers go through the node in slice 1 (D15). Scoped presigned direct transfers are the
  contract's direction and come later.

**Idempotent Publish over the wire.** `PublishLayer { operation, layer, expected, base,
attribution }`:
1. Replay check (4.5) first. A recorded outcome is returned as-is.
2. Authorization at the correctness boundary: current applied permissions for the caller
   (`publish` scope and role, epoch), the policy (`required_approvals`, Publish setting),
   and for `PublishRequest` the approval state (4.8). These are read after compose and before
   the slot create, so a change that lands before the commit point is seen (`C§Contributions
   and reviews`).
3. Compose onto the head. Conflict returns `Conflict` and writes nothing.
4. Slot create (4.5).
5. Layer state moves to `Published` (read-only, `C§Version control`). If the node crashes
   before this, the replay finds the committed slot and finishes it.

**Tests.**
- Lost reply after commit: one version; the retry returns the same version.
- Operation ID reused with a different payload: `operation_id_reused`, no change.
- Missing object: `missing_object`, no change.
- Corrupt object bytes: `id_mismatch`, and the stored object is unchanged.
- Permission revoked between compose and commit: refused, no version.
- Missing agent trace without the override: `missing_agent_trace`; with the override, published.
- Two Publishes on one head: one wins, the other rebases and publishes, or conflicts.

**First slice (U2).** `PublishLayer` over loopback with file-backed stores and no auth
(test token). Tests from the list above that do not need auth or review.

### 4.7 Identity, scoped credentials, revocation

**Control plane owns:** users, orgs, memberships, device registrations, the signing key set
(`kid`-rotated), and the repository's epoch-ordered role changes.

**Node owns:** the repository's applied permissions `{ repository, epoch, visibility, roles,
required_approvals }` (`javelin-collaboration` `AppliedPermissions`, `C§Hosting ownership`),
and the revocation set. It verifies tokens offline with cached public keys.

**Token** (signed; claims): `sub` (principal), `dev` (device), `repo` (repository ID, or
`*` for discovery), `scopes` (`read`, `layer:write`, `publish`, `review:write`,
`review:approve`, `repo:create`, `import`), optional `layer` (narrows agent or harness work to
one Layer, D26), `iat`, `exp`, `jti`.

**Effective rights** = scopes ∩ role at the current epoch.

**Lifetimes and revocation** (D6):
- Access token 1 hour. Device refresh token 30 days, usable only through the control plane.
- Revocation: the control plane pushes `Revoke { jti | device }` to the node when it can. The
  node refuses listed tokens immediately.
- If the node cannot hear about a revocation, the token still stops working at `exp`, because
  refresh needs the control plane. So the lag is at most the access TTL.
- Role changes are epoch-ordered by the control plane. The node accepts only newer epochs
  (`stale_permissions` otherwise). During a control-plane outage, a role removal is not applied
  until the plane returns or the token expires (D8).

**Device login.** A device flow against the control plane, with the IdP chosen in D5. The
device's refresh token is stored in `~/.javelin/credentials` (0600), not sent to the node.

**Outage.** With the plane down and a valid token, node operations work (`C§Hosting
ownership`). Refresh fails with `control_plane_unavailable`.

**First slice (U7).** Token verification, scope checks and epoch checks on the in-process
node, using test keys. No control plane yet.

**Tests.**
- Forged or wrongly-signed token: `unauthorized`.
- Expired token: `unauthorized`.
- Revoked `jti`: refused after the push is applied.
- Scope narrower than role: refused (for example, `read` cannot Publish).
- Stale epoch push is ignored; a newer one applies.
- Control plane down, valid token: write succeeds. Expired token during outage: refused and
  queued client-side.
- Restricted trace read without role: `forbidden`.

### 4.8 Author-owned Publish Request revisions and exact approvals

**Stores** (review partition, slot chains per request):
- `request` `{ id, repository, author, layer, base, state: Open | Published | Closed,
  operation }`.
- `revision n` (slot `requests/{R}/rev/{n}`), immutable: `{ n, checkpoint, base, context:
  Option<ObjectId>, author, operation }`. Written only by the author.
- `approval` `{ request, revision n, approver, operation, epoch }`. Approvals stay in history.
  A revision n+1 does not inherit them.
- `comment` `{ request, revision?, author, body (<= 64 KiB), operation }`.
- Policy snapshot at publish time.

**Rules** (`C§Contributions and reviews`, `javelin-collaboration` state machine):
- Only the author submits revisions (`not_author`).
- A revision is immutable. Later Layer checkpoints do not change it.
- An approval applies to exactly `(request, n, checkpoint, context)`. A new revision invalidates
  approvals for its own number, so approvals are checked per revision.
- Self-approval refused (`self_approval`). Approvers need `review:approve` and role
  Review or Maintain.
- `PublishRequest` publishes revision `n`'s checkpoint, not the Layer's current head. So
  the published content is exactly what was approved, even if the Layer moved on.

**Publish boundary.** At the correctness boundary (4.6): request open; `revision` is current
(otherwise `stale_revision`); approvals for that revision reach `required_approvals` under
the current epoch (`not_enough_approvals`); each approver still holds the role at the current
epoch; compose of revision `n` gives the same changeset (a collision refuses, `conflict`).

**Outage.** `Revise` queues (D7). `Approve`, `Comment` and `OpenRequest` refuse with
`node_unavailable`. A queued `PublishRequest` is re-checked at replay time, so an approval
revoked during the outage refuses it.

**First slice (U9).** Revisions and approvals on the node review partition, using the
collaboration crate on tree IDs (T4). No browser UI; that is the separate browser-review item
(`T`, "Implement browser review").

**Tests.**
- Non-author revise: `not_author`. Immutable: resubmitting number `n` returns an error, and
  the stored revision is byte-identical afterwards.
- Approval for revision 1 does not count for revision 2.
- Self-approval refused.
- `PublishRequest` of revision 1 after revision 2 exists: publishes revision 1's content exactly
  (tree equal to compose of revision 1).
- Approver's role removed at a newer epoch before publish: refused.
- Comments on revision 1 remain visible after revision 2.
- Lost reply on `PublishRequest`: one version.
- Approval exists but the request is `Closed`: refused.

### 4.9 Forks with inherited history

**Semantics** (`C§Contributions and reviews`): a fork is a new identity with independent
future publications. It inherits history through a selected version, including permitted
code and context. It keeps that data independently of the source. Credentials, memberships
and policies do not transfer. Restricted traces never gain broader access.

**Mechanics.**
- The fork's World log starts as a copy of the source log up to version `N`. Chain IDs are
  identical because the chain function is the same (`A§Storage`), so the fork's version `N`
  has the same `id` and tree. This gives the "one comparison" check of `G§Open questions`.
- A provenance record `fork_of { source repository, N, id }` is written once.
- Objects are copied into the fork's own namespace (D14 default), so deleting the source
  cannot affect the fork.
- Traces are copied only if the forking principal can read them at fork time. Others are
  omitted and a marker `trace_omitted { trace id }` is recorded. An omitted trace is not in
  the chain, so the version IDs do not change.
- Roles on the fork: the creator is Maintain; source roles are not copied.

**Stores.** Fork state `{ state: Creating | Ready, source, N, progress }` in the control plane
record and the fork node.

**Wire.** The fork node pulls from the source node with `Resolve`, `Get` and `Want`, using the
forking principal's token. The source node does not learn the fork's identity.

**Outage.** Fork needs the control plane for the identity and the source node for the copy.
Either being down refuses the request with the matching code; the fork is not queued. A crash
mid-copy leaves `Creating`; retry resumes. Object copies and slot creates are idempotent, so
resuming is safe.

**First slice (U11).** Fork of a version on the in-process node, two file-backed stores, no
trace restrictions yet (traces come in U10).

**Tests.**
- Fork at `N`: tree equal to the source at `N`; version IDs equal.
- Publish on the fork creates `N+1` only in the fork; the source is unchanged.
- The forker, who is not a source member, has no source role in the fork.
- Trace not readable by the forker: omitted, marker recorded, and no bytes of it in the
  fork's store (scan).
- Delete the source's store after fork: the fork still reads and publishes.
- Crash during copy, then retry: one complete fork, no duplicate versions.

### 4.10 First hosting provider and creation flow

**Provider.** Cloudflare is the provisional first provider (`C§Protocol and first provider`).
Mapping: Worker (node HTTP API and `javelin-node` logic compiled for the adapter), R2 (objects,
and slots if D2 = B), and the ordering coordinator only if D2 = A or C. Whether the control
plane also runs there is D1. This design has not verified Worker compatibility with the Rust
core; that is the first step of U13.

**Creation flow** (`C§Hosting ownership`: "name, visibility, Publish setting, provider, and
create"):

```
javelin repo create <owner>/<name> --visibility public|private \
  --publish direct|requests --provider cloudflare
```

1. The CLI authenticates with a token carrying `repo:create`.
2. The control plane validates the name and creates the record `state = Provisioning` with an
   operation ID.
3. It provisions the node: object namespace, slot store, Worker route.
4. It writes applied permissions at epoch 1 (creator = Maintain) and version 0 (empty World).
5. It marks `Ready` and returns the repository URL. No secret is echoed.

Repeating the same operation ID returns the same repository. A crash leaves `Provisioning`,
and a retry resumes it. The CLI then runs `javelin attach`.

**Outage.** Creation needs the control plane, so it refuses with `control_plane_unavailable`
and is not queued. A provider outage during provisioning leaves `Provisioning` until retry.

**First slice (U8, after U7).** `repo create` against the in-process node with the in-process
control plane. No Cloudflare yet. U13 adds the Worker and R2 adapter.

**Tests.**
- Create twice with the same operation ID: one repository.
- Name collision: refused.
- Crash in step 3: retry completes; no duplicate namespace.
- `attach` after create works with the empty World.
- The git stand-in marker stays absent through create and attach.

## 5. Outage matrix (summary)

| Action | Node down | Control plane down | Local |
| --- | --- | --- | --- |
| Layer create, capture | Works | Works | Works |
| Checkpoint upload | Queued | Works if token valid | Durable |
| Stack | Refused (`node_unavailable`) | Works if token valid | n/a |
| PublishLayer | Queued | Works if token valid | n/a |
| Revise | Queued | Works if token valid | n/a |
| Approve, Comment | Refused | Works if token valid | n/a |
| Attach | Refused | Works if token valid | n/a |
| Repo create, fork, refresh token, membership | n/a | Refused (`control_plane_unavailable`) | n/a |

## 6. Test strategy and acceptance mapping

- **Harness.** The in-process node serves the protocol over loopback HTTP, with fault injection
  (drop reply after commit, refuse, delay, expire token, bump epoch at the boundary).
- **Git stand-in.** Every acceptance run uses a failing `git` on PATH with a marker, plus a
  positive control that proves the stand-in fires (`C§Verification gates`).
- **Provider evidence.** The conditional-write behavior is tested only against a real R2 bucket
  (4.5). Emulators are not accepted for it.
- **Contract gate 4 assertions** (`C§Verification gates` 4), in one rerunnable script (U14):
  exact revision authorization; immutable prior revisions; one durable outcome after a lost
  reply; inherited fork data without inherited credentials; no access to restricted traces;
  distinct `node_unavailable` and `control_plane_unavailable` outcomes.
- **Not covered here.** Browser review UI, CI execution (D4), and the measurement gate (gate 6).
  Each has its own unit.

## 7. Decisions for the user

Each is open. The recommended default applies unless the user says otherwise. Defaults do not
change any contract text.

- **D1. First provider and control plane.** Default: Cloudflare for the node, and the control
  plane also on Cloudflare (`C§Protocol and first provider` is provisional). Alternatives: a
  different provider, or a separate control-plane host.
- **D2. World log quorum (T1).** Options: (A) one coordinator per repository; not quorum unless
  the provider confirms replica confirmation before ack. (B) R2 create-if-absent slots (section
  4.5); quorum rests on R2 durability, unverified. (C) N coordinators with a consensus protocol
  of our own; highest complexity. Default: B, with the spike in U12. If the spike fails or the
  hub does not accept B as quorum, use C. Either way, the contract's quorum wording needs the
  hub's decision.
- **D3. Version identity.** Include the root tree hash in the version ID chain (`G§Open
  questions`). Default: yes. This changes the hash chain format. Pre-GA, so no migration.
- **D4. CI execution.** Default for this phase: no Javelin-run runners. Checks are results posted
  by external runners and stored by the node with the reporter's identity. No check gates Publish
  by default (`C§Version control`: optional policy). Runners and required-check policy are
  decided in unit U15. Alternatives: Javelin-operated runners on the provider (compute
  availability on the provider is UNCONFIRMED), or external CI only.
- **D5. Login method.** Default: email magic link for the browser plus device code for the CLI,
  both through the control plane. Alternatives: passkeys (later), or an external OAuth provider.
- **D6. Credential lifetimes.** Default: access token 1 hour, refresh token 30 days,
  refresh through the control plane only. Revocation lag is at most the access TTL.
- **D7. What queues.** Default: checkpoint uploads, `LayerAppend`, `PublishLayer`,
  `PublishRequest`, and `Revise` queue. Approvals, comments, OpenRequest, Stack, attach, repo
  create and fork never queue. `C§Hosting ownership` names only checkpoints and Publishes;
  revisions are an added choice.
- **D8. Role removal during a control-plane outage.** Default: follow the contract literally.
  A role removal applies when the control plane returns or the token expires, not earlier. If
  the user wants faster removal, the alternative is bounded permission staleness that pauses
  writes after a set age, which the contract does not allow for "continue" outcomes.
- **D9. Server-side Stack during outage.** Default: refuse (`node_unavailable`). Alternative:
  a local fallback Stack. That produces results the node did not compose, so it is not the
  default.
- **D10. Approval survives non-intersecting World advances.** Default: yes. The approval binds
  revision content and context, not the base version. This follows `C§Version control`:
  "Nonintersecting World advances do not force a Layer refresh." Confirm.
- **D11. Approval defaults.** `required_approvals` default 1. Self-approval refused. Approvers:
  Review or Maintain role. Maintainers never rewrite an author's revision (`C§Contributions and
  reviews`).
- **D12. Publish setting (T5).** Default: `direct` (members with Maintain may Publish directly;
  others submit requests) for private repositories, and `requests` for public. Alternatives:
  `direct` for everyone with Publish scope, or `requests` only.
- **D13. Fork permission.** Default: anyone with read access to the source may fork; a public
  repository is forkable by any signed-in user. Restricted traces are omitted with a marker.
- **D14. Fork storage.** Default: copy objects into the fork's namespace (duplicates bytes;
  deleting the source cannot affect the fork). Alternative: one content store with per-repo
  access rules. Deduplication can come later.
- **D15. Object transfer path.** Default: through the node in slice 1. Scoped presigned direct
  transfers come later, because the contract's direction is "scoped direct object transfers."
- **D16. Upload model.** Default: no daemon. Inline upload with a bounded budget (2 s, to be
  measured); failures stay in the outbox; a later command or `javelin sync` replays. This
  rejects a background flusher, which `G§Rejected` rules out. Confirm (T2).
- **D17. Outbox storage.** Default: append-only checksummed journal file, not SQLite. Alternative:
  a SQLite table. The outbox is primary state, not a rebuildable index (`G§3`).
- **D18. Local cache bound and eviction (`G§Open questions`).** Default: LRU over objects not
  referenced by the rolling base, live Layers or unacknowledged outbox records. Cap default
  5 GiB, to be measured under gate 6.
- **D19. Attach materialization.** Default: full rolling base, because offline Layer create
  needs it. Alternative: sparse attach, which needs a separate offline design.
- **D20. Offline Layer create from the last-known base.** Default: allowed (`C§Version
  control`: Layer work continues during an outage). Publish composes later.
- **D21. Second writer on a Layer.** Default: refuse; one owning machine per Layer.
- **D22. Protocol versioning.** Default: extend v2 additively until the first deployed node,
  then bump. PR #52 treats version 1 as non-authoritative, so this is consistent.
- **D23. Node runtime.** Default: the Rust core crate plus a Worker adapter, if the spike shows
  the core compiles for the Worker target (UNCONFIRMED). Fallback: a TypeScript shim calling
  Rust compiled to WebAssembly. JS tooling uses bun (`CLAUDE.md`).
- **D24. Anonymous read of public repositories.** Default: yes for attach and fetch. Publish,
  OpenRequest, Revise, Approve and Comment need identity.
- **D25. Trace visibility.** Default: a trace inherits repository visibility. A `restricted`
  trace is readable by Maintain only.
- **D26. Agent identities.** Default: none in this phase. Agents use the owning device's token,
  narrowed by the `layer` claim when the harness supplies one.
- **D27. Notifications.** Default: deferred. Clients poll `LayerHead` and `ReadRequest`.
- **D28. Out of scope.** Repository deletion, data export and retention are not designed here.
  Add a unit when the hub asks.

## 8. Implementation units

Each unit is one regular PR. Each names its base. Units marked "after" need the named PR
merged first, or a verified unmerged base with the dependent PR naming it (`C§Work ownership`).

| # | Unit (PR title) | Owned paths | Depends on |
| --- | --- | --- | --- |
| U1 | `feat: protocol v2 hosting messages` (tags, Checkpoint kind, error codes, codec tests; no behavior) | `crates/javelin-protocol/src/codec.rs`, `src/lib.rs` (limits), `tests/size.rs`, new `tests/hosting_codec.rs` | PR #52 merged |
| U2 | `feat: node core with file-backed stores and idempotent PublishLayer` (`WorldLog` trait, file impl, object store with verified create-if-absent, loopback server, `PublishLayer`, operation records) | new `crates/javelin-node/`, `crates/javelin-protocol/src/receiver.rs` | U1 |
| U3 | `feat: client outbox journal and replay` (journal, replay engine, fault-injection harness) | new `src/outbox.rs`, `tests/outbox*.rs`, `crates/javelin-cli` (outbox hooks only) | U2 |
| U4 | `feat: attach to a hosted repository` (`attach.json`, full fetch into the rolling base) | new `src/attach.rs`, new `crates/javelin-cli/src/cli_remote.rs`, `tests/attach*.rs` | U2; roadmap item 5 (rolling base) merged |
| U5 | `feat: streaming checkpoints through LayerAppend` (checkpoint upload in capture, `LayerAppend`, `live_paths` index) | `src/capture.rs` (call site only), `crates/javelin-node/` (layer partition), `crates/javelin-protocol/src/sender.rs`, `tests/checkpoints*.rs` | U2, U3, U4 |
| U6 | `feat: server-side Stack and refresh from head` (`LayerStack`, golden test, CLI refresh) | `crates/javelin-stack/`, `src/stack.rs`, `crates/javelin-node/` (stack handler), `crates/javelin-cli` (refresh) | U5 |
| U7 | `feat: scoped tokens, applied permissions and revocation on the node` (verifier, scopes, epochs, revocation set, test keys) | new `crates/javelin-identity/`, `crates/javelin-node/src/auth.rs` | U2 |
| U8 | `feat: control plane minimum: repo create and applied-permission push` (in-process control plane, idempotent create, device login behind the D5 decision) | new `crates/javelin-control/`, `crates/javelin-cli` (repo create) | U7; D5 for login |
| U9 | `feat: author-owned revisions and exact approvals` (`PublishRequest`, review partition, tree IDs in collaboration) | `crates/javelin-collaboration/`, `crates/javelin-node/src/review.rs` | U2, U7 |
| U10 | `feat: restricted agent traces` (trace store, visibility, `missing_agent_trace` at node) | `crates/javelin-node/src/trace.rs` | U2, U7; D25 |
| U11 | `feat: forks with inherited history` (log copy, object copy, provenance, trace omission) | `crates/javelin-node/src/fork.rs`, `crates/javelin-control/` (fork op) | U2, U7, U10; D13, D14 |
| U12 | `feat: quorum-replicated World log on object storage` (spike results in PR, then R2 `WorldLog`) | `crates/javelin-node/src/log_r2.rs`, `tools/spike-r2/` | U2; D2 |
| U13 | `feat: Cloudflare node adapter and provisioning` (Worker shim, R2 object store, `repo create` end to end) | new `tools/node-cloudflare/` (bun-managed) | U8, U12; D1, D23 |
| U14 | `test: hosted acceptance suite` (contract gate 4 assertions, outage matrix, git stand-in with positive control) | new `tools/acceptance/`, `tests/hosted_*.rs` | U3, U5, U9, U11, U13 |
| U15 | `docs: CI execution design` (docs only; check ingestion and runner options) | new `docs/ci-execution.md` | D4 (can start now; no code) |
| U16 | `test: hosting measurements` (gate 6 fixtures, limits, cost) | `tools/` | U14; measurements committed before the run (`C§Verification gates` 6) |

Order that gets working behavior fastest: U1 → U2 → U3 → U5 (checkpoints and publish on a
local node) with U4 as soon as the rolling base merges; then U7 → U8 → U9; then U10 → U11; then
U12 → U13; then U14. U15 runs in parallel at any time.

## 9. Sources

- Cloudflare R2 S3 API compatibility, conditional PutObject: https://developers.cloudflare.com/r2/platform/s3-compatibility/api/ (read through search excerpt, not in full).
- Cloudflare R2 release notes (bindings `onlyIf`, ETag conditions): https://developers.cloudflare.com/r2/platform/release-notes/ (search excerpt).
- Cloudflare SQLite in Durable Objects, GA: https://developers.cloudflare.com/changelog/2025-04-07-sqlite-in-durable-objects-ga and the 2024 post https://blog.cloudflare.com/sqlite-in-durable-objects (search excerpts; replica count and failure domains NOT FOUND).
- Miniflare conditional-ETag behavior report: https://github.com/cloudflare/miniflare/issues/774 (archived repository; behavior not confirmed against production R2).
- Not checked: Worker compatibility with the Rust core (D23), Cloudflare compute for CI (D4), Cloudflare control-plane hosting (D1).
