# Online design: roadmap item 7 and the hosting phase

Status: decisions recorded 2026-10-20 (section 7). Not accepted, not implemented. Written on
`docs/online-design`; decisions on `docs/online-decisions`.
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
- Every open point is in "Decisions for the user", with a decision recorded 2026-10-20.
- Ordered PR-sized units with owned paths and dependencies are in "Implementation units".

This design is verified by reading only. Its acceptance is the hub's review and the
units' own tests. Provider claims cite Cloudflare documentation read on 2026-10-20 (section 9);
anything else is unverified until its unit runs.

## 1. Contract tensions the hub must reconcile

These are not settled by the contract or G§. The design picks a reading and flags it.

- **T1. Quorum vs. a single coordinator.** `C§Protocol and first provider` names "a small
  World publication coordinator" for Cloudflare. `C§Hosting ownership` says the World
  authority "is a quorum-replicated log, so no single node failure stops Publish." A single
  coordinator is not quorum-replicated unless the provider's storage replicates each
  acknowledged write across failure domains. G§Open questions recorded this as UNCONFIRMED;
  Cloudflare's 2024 SQLite-in-Durable-Objects post now supports the replication claim (section 9).
  Decision D2: one coordinator per repository, with durability from the storage layer. The
  contract's quorum wording needs the hub's amendment.
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
  `javelin-node`, behind storage traits. Self-hosting runs the `javelin-node` binary on
  file-backed stores (U2). The first managed provider is Cloudflare: a Worker, and one
  SQLite-backed Durable Object per repository (U12, U13; D1).
- **Control plane** (central, not self-hosted, `C§Hosting ownership`). It owns identity, orgs,
  memberships, discovery, provisioning, and the signing keys for tokens. It is a separate
  service in front of the node, not a node component on managed hosting. Self-hosting runs the
  same control-plane code inside the node (D1). That contradicts `C§Hosting ownership`'s "not
  self-hosted"; the hub must amend it.
- **Client** (`javelin` CLI). Captures locally, keeps a bounded object cache and a rolling base
  (`G§4`), queues remote work in an outbox (section 4.4), and talks to the node directly
  after authentication.
- **Wire**: the protocol v2 frame (`crates/javelin-protocol/src/codec.rs`) over HTTP/2. One
  message per request, with the same encoded body as today. The HTTP `Content-Length`
  replaces the `u32` length prefix. Each message is `POST /v2/r/{repo}` with a bearer token;
  the response is one message. Notifications stay optional and are deferred (D27).
- **One ordering point per repository.** Each repository has one coordinator (D2): a process
  lock on the node when self-hosted, or the repository's Durable Object when managed. Every
  ordered sequence is a chain of immutable rows, each written in one transaction with a
  unique-key precondition: the World log, each Layer's checkpoint chain, and each Publish
  Request's revision list. Only World advancement is repository-wide (`C§Protocol and first
  provider`: "Only final World advancement needs repository-wide total ordering.").
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
`missing_agent_trace`, `layer_published`, `storage_full`. Client-side outcomes that never come from the wire:
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
- Node: checkpoint objects (Checkpoint kind); Layer sequence rows `layer_seq(layer, seq)` with
  `{ seq, parent, checkpoint, operation }`; derived `live_paths` index (path → Layer, from
  each checkpoint's changed paths) for collision radar (`G§1`, `G§6`). The index is derived
  and rebuildable.
- Client: outbox records (section 4.4).

**Wire.** `Want` for the checkpoint and its blobs, `Objects` to upload bytes, then
`LayerAppend`.

**Ordering.** Upload every blob and the checkpoint object first. The node checks each one
(hash, kind, canonical form) before it commits the reference (`C§Protocol and first provider`:
"Objects must be readable and verified before committing references."). Then `LayerAppend`
inserts row `seq = head + 1` in one transaction; the unique key on `(layer, seq)` makes it a conditional create. The parent must equal the current head
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
checkpoint object and a `seq` row on the target Layer, or structured conflicts. Nothing is
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

### 4.5 World log: one coordinator per repository

**Requirement.** Append-only changeset log, one head, compare-and-swap advancement
(`G§3(b)`). Acknowledgement only after the write is durable (`C§Protocol and first
provider`: successful Publishes survive restart). On managed hosting, durability comes from the
storage layer (D2).

**Coordinator (D2).** One per repository. Self-hosted: the `javelin-node` process holds a lock
on the repository's data directory; a second process refuses to start. Managed: the
repository's Durable Object (one per repository, D1). Every World, Layer and review write for
the repository goes through it, and it serializes them.

**Data.** Tables in the coordinator's store (file-backed when self-hosted, DO SQLite when
managed):
- `world(n PRIMARY KEY, parent, changeset, root_tree, id, operation, attribution)`: one row per
  version. `n` is the ordering point. The version `id` is the hash chain of `A§Storage`, with
  the root tree hash in it (D3).
- `ops(operation PRIMARY KEY, digest, n)`: the operation record. `n` is set in the same
  transaction that creates the version.
- `layer_seq(layer, seq, parent, checkpoint, operation)` with key `(layer, seq)`, and the review
  revision rows (4.8): same pattern.
- `objects(id, kind, index, bytes)` with key `(id, index)`. Blobs and checkpoints are split into
  segments of at most 1 MiB, so a whole row (segment plus `id`, `kind` and `index`) stays under
  the 2 MB row limit (section 9). A read
  reassembles the segments and checks the whole-object hash.

The head is the largest `n`, read inside the transaction. There is no head hint.

**Commit path** (`PublishLayer` and `PublishRequest`), one transaction:
1. Read the head `H`. Compose onto `H`, and read the authorization inputs (4.6, step 2), all
   inside this transaction, so the reads see every change committed before it.
2. Insert `ops(operation)`. If it exists, compare the digest: the same digest returns the
   recorded outcome; a different digest returns `operation_id_reused`.
3. Insert `world(H+1)`. A unique-key violation means another writer committed first: roll back
   and re-prepare on the new head. This is the rebase path of `A§Publish`; intersecting changes
   conflict.
4. Set `ops(operation).n = H+1`; for `PublishLayer`, set the Layer to `Published` in the same
   transaction; commit.
5. Reply `Published`.

A transaction either commits or leaves nothing, so there is no pending state. The reply is sent
after commit. On managed hosting the storage layer confirms the write before the reply leaves
(section 9).

**Replay.** Read `ops(operation)`. Found with `n`: return the recorded outcome. Not found: run
the commit path. Digest mismatch: `operation_id_reused`.

**Why this shape.** One coordinator serializes writes, so a unique key is the whole ordering
mechanism; no compare-and-swap protocol is needed. Replication is the provider's job on managed
hosting. The 2024 Cloudflare post says a write is confirmed once three of five follower replicas
respond (section 9). U12 re-checks this against current docs and records the citation.
Self-hosted, the node is one machine: durability is local fsync, with no replica. The contract's
"no single node failure stops Publish" therefore holds only for managed hosting, and the hub's
amendment must say so (T1).

**Limits (managed).** A row is at most 2 MB, so objects are segmented (above). A SQL statement is
at most 100 KB and binds at most 100 parameters, so large writes are batched. At the storage cap,
writes fail with `SQLITE_FULL`; the node refuses the whole commit with `storage_full`, and
nothing is written. Read-only requests still succeed.

**Outage.** Coordinator unreachable: Publish returns `node_unavailable`, and the client queues it
(section 4.4). Nothing is written partially, because the transaction commits or does not.

**Spike (part of U12, before production code).** On a real Cloudflare Durable Object on the
Free plan, deployed, not Miniflare: 50 concurrent publishers on one repository; exactly one per
version; a process kill inside a commit leaves no partial version; a kill after commit and before
the reply replays to the same version. Re-check the replication citation against current docs.
Record results and citations in the PR.

**First slice (U2, then U12).** `WorldLog` trait with a file-backed implementation (U2) and
contract tests (uniqueness, replay, crash recovery). U12 runs the same tests against the Durable
Object implementation.

**Tests.**
- Concurrent publishers: one version per `n`; the rest rebase or conflict.
- Lost reply after commit: replay returns the same version.
- Kill before commit: nothing visible; the retry commits once.
- Corrupt or missing segment: refused (`id_mismatch` or `missing_object`), never served.
- Second coordinator on a self-hosted repository: refused.
- Storage cap reached: `storage_full`, nothing written.

### 4.6 Repository authority, idempotent Publish, immutable transfers

**Authority.** `javelin-node` holds the World log (4.5), Layer partitions, review partitions,
traces and applied permissions behind traits. The in-process authority and the hosted node
use the same protocol. On the wasm path they share the same core code. On the TypeScript
fallback (D23), parity with the Rust node is a test, not shared code (`G§Open questions`,
recommended default, adopted 2026-10-20).

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
   and for `PublishRequest` the approval state (4.8). All of these are read inside the commit
   transaction (4.5), with the compose in step 3, so a change that commits before it is seen (`C§Contributions
   and reviews`).
3. Compose onto the head. Conflict returns `Conflict` and writes nothing.
4. Commit transaction (4.5), which also sets the Layer to `Published`.
5. The Layer is `Published` (read-only, `C§Version control`) from the commit onward, so a
   replay that finds the committed version also finds the Layer already published.

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

**Login (D5).** No email. Bootstrap: the first admin token is printed once when a node or
repository is created; the node stores only its hash. The token grants the creator's Maintain
role and enrolls the first passkey. Routine login: `javelin login` asks the control plane for a
device code and prints a short code and URL; the user approves it in a browser session signed in
with a passkey (WebAuthn). The device's refresh token is stored in `~/.javelin/credentials`
(0600), not sent to the node.

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

**Stores** (review partition, row chains per request):
- `request` `{ id, repository, author, layer, base, state: Open | Published | Closed,
  operation }`.
- `revision n` (row `rev(request, n)`), immutable: `{ n, checkpoint, base, context:
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
- `PublishRequest` of revision 1 after revision 2 exists: refused with `stale_revision`
  (4.8). The exact approved content is tested by advancing the Layer without a new revision:
  publishing revision 1 still publishes its checkpoint, not the Layer head.
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
mid-copy leaves `Creating`; retry resumes. Object copies and row inserts are idempotent, so
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

**Provider** (`C§Protocol and first provider`; D1). Two paths. Self-hosted: the `javelin-node`
binary with file-backed stores, runnable free anywhere; the reference implementation and test
target. Managed (first): Cloudflare Workers Free, with one SQLite-backed Durable Object per
repository as both coordinator and object store (rows of at most 2 MB, section 4.5). No R2. The
Worker runs the Rust core compiled to wasm via workers-rs (D23). If that spike fails, a thin
TypeScript Worker (bun-managed) speaks the same protocol and is tested against the Rust node.
U13's first step verifies the Free-plan limits (section 9); until then they are unverified.

**Creation flow** (`C§Hosting ownership`: "name, visibility, Publish setting, provider, and
create"):

```
javelin repo create <owner>/<name> --visibility public|private \
  --publish direct|requests --provider cloudflare|self
```

1. The CLI authenticates with a token carrying `repo:create` (the admin token from bootstrap, or
   a device session).
2. The control plane validates the name and creates the record `state = Provisioning` with an
   operation ID.
3. It provisions the node: the repository's Durable Object (managed) or data directory
   (self-host), and the Worker route.
4. It writes applied permissions at epoch 1 (creator = Maintain) and version 0 (empty World).
5. It marks `Ready` and returns the repository URL. Ordinary responses echo no secret; the
   bootstrap admin token is shown once, in the output of node or repository creation (4.7, D5).

Repeating the same operation ID returns the same repository. A crash leaves `Provisioning`,
and a retry resumes it. The CLI then runs `javelin attach`.

**Outage.** Creation needs the control plane, so it refuses with `control_plane_unavailable`
and is not queued. A provider outage during provisioning leaves `Provisioning` until retry.

**First slice (U8, after U7).** `repo create` against the in-process node with the in-process
control plane. No Cloudflare yet. U13 adds the Worker and Durable Object adapter.

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
- **Provider evidence.** The coordinator's transaction behavior (4.5) is tested only against a
  real Cloudflare Durable Object (U12, U13). Miniflare and other local emulators are not accepted
  for it.
- **Contract gate 4 assertions** (`C§Verification gates` 4), in one rerunnable script (U14):
  exact revision authorization; immutable prior revisions; one durable outcome after a lost
  reply; inherited fork data without inherited credentials; no access to restricted traces;
  distinct `node_unavailable` and `control_plane_unavailable` outcomes.
- **Not covered here.** Browser review UI, CI execution (D4), and the measurement gate (gate 6).
  Each has its own unit.

## 7. Decisions for the user

Decided 2026-10-20 by the hub, from the user's delegation. Each entry has the decision and a
one-line rationale. Where a decision needs the contract changed, the entry says so; this design
does not change contract text.

- **D1. First provider and control plane.** Decided 2026-10-20: the self-hosted `javelin-node`
  binary with file-backed stores (U2) is the reference implementation and test target, runnable
  free anywhere. The first managed provider is Cloudflare Workers Free with one SQLite-backed
  Durable Object per repository; objects are DO SQLite rows of at most 2 MB, with no R2 (R2 needs
  a payment method, per the user; not checked here). U13 verifies the Free-plan limits first, and
  they stay unverified until then. The control plane's location is a derived default, not a user
  decision: embedded in the node when self-hosted, on Workers when managed. Contract tension:
  `C§Hosting ownership` says the control plane is not self-hosted, so the hub must amend that
  clause before self-host embeds it. Rationale: only starts that need no paid tier, no card and no
  extra signup.
- **D2. World log quorum (T1).** Decided 2026-10-20: one coordinator per repository, the node's
  process lock when self-hosted or the repository's Durable Object when managed, with durability
  from the storage layer. Cloudflare's 2024 post says a write is confirmed once three of five
  follower replicas respond (verified; U12 re-checks it against current docs). Own quorum (option
  C) is a later option. Rationale: one coordinator makes ordering a single transaction, and the
  provider supplies replication. The contract's quorum wording needs the hub's amendment, since
  self-hosting is one machine.
- **D3. Version identity.** Decided 2026-10-20: the root tree hash is in the version ID chain.
  Rationale: pre-GA, so the format change needs no migration.
- **D4. CI execution.** Decided 2026-10-20: no Javelin-run runners in this phase. External runners
  post results, stored with the reporter's identity, and no check gates Publish by default.
  Rationale: nothing here needs provider compute, which is unverified; U15 decides runners and
  required-check policy.
- **D5. Login method.** Decided 2026-10-20: no email dependency. The first admin token is printed
  once at node or repository creation. The CLI uses a device-code flow, approved from a browser
  session signed in with a passkey (WebAuthn). Magic links are deferred. Rationale: needs no email
  provider or signup.
- **D6. Credential lifetimes.** Decided 2026-10-20: access token 1 hour; refresh token 30 days,
  refreshed through the control plane only. Rationale: revocation lag is bounded by the access
  TTL, with no live lookup.
- **D7. What queues.** Decided 2026-10-20: checkpoint uploads, `LayerAppend`, `PublishLayer`,
  `PublishRequest` and `Revise` queue; approvals, comments, `OpenRequest`, Stack, attach, repo
  create and fork never queue. Rationale: the contract names only checkpoints and Publishes;
  revisions are the one added queued action, and the rest need a live answer.
- **D8. Role removal during a control-plane outage.** Decided 2026-10-20: follow the contract
  literally; a role removal applies when the control plane returns or the token expires.
  Rationale: the contract allows no faster removal for "continue" outcomes.
- **D9. Server-side Stack during outage.** Decided 2026-10-20: refuse with `node_unavailable`.
  Rationale: a local Stack would produce results the node did not compose.
- **D10. Approval survives non-intersecting World advances.** Decided 2026-10-20: yes. Rationale:
  `C§Version control` says those advances do not force a Layer refresh; the approval binds content
  and context, not the base.
- **D11. Approval defaults.** Decided 2026-10-20: `required_approvals` defaults to 1; self-approval
  is refused; approvers hold Review or Maintain. Rationale: matches `C§Contributions and reviews`,
  under which maintainers never rewrite an author's revision.
- **D12. Publish setting (T5).** Decided 2026-10-20: `direct` for private repositories, `requests`
  for public ones. Rationale: public work goes through review by default.
- **D13. Fork permission.** Decided 2026-10-20: anyone with read access may fork; a public
  repository is forkable by any signed-in user; restricted traces are omitted with a marker.
  Rationale: a fork never gets more than the forker could read.
- **D14. Fork storage.** Decided 2026-10-20: copy objects into the fork's namespace. Rationale:
  deleting the source cannot affect the fork; deduplication can come later.
- **D15. Object transfer path.** Decided 2026-10-20: transfers go through the node in slice 1;
  scoped presigned direct transfers come later. Rationale: one path to build and test first.
- **D16. Upload model.** Decided 2026-10-20: no daemon. Inline upload with a bounded budget (2 s,
  to measure); failures stay in the outbox, and a later command or `javelin sync` replays them.
  Rationale: `G§Rejected` rules out a client daemon.
- **D17. Outbox storage.** Decided 2026-10-20: an append-only checksummed journal file, not
  SQLite. Rationale: the outbox is primary state that the server cannot rebuild.
- **D18. Local cache bound and eviction.** Decided 2026-10-20: LRU over objects not referenced by
  the rolling base, live Layers or unacknowledged outbox records; cap 5 GiB, to measure under gate
  6. Rationale: bounded disk without evicting live data.
- **D19. Attach materialization.** Decided 2026-10-20: full rolling base. Rationale: offline Layer
  create needs it; sparse attach needs its own design.
- **D20. Offline Layer create.** Decided 2026-10-20: allowed from the last-known base; Publish
  composes later. Rationale: `C§Version control` says Layer work continues during an outage.
- **D21. Second writer on a Layer.** Decided 2026-10-20: refuse; one owning machine per Layer.
  Rationale: no forced writes.
- **D22. Protocol versioning.** Decided 2026-10-20: extend v2 additively until the first deployed
  node, then bump. Rationale: PR #52 treats version 1 as non-authoritative, so nothing deployed
  needs migrating.
- **D23. Node runtime.** Decided 2026-10-20: the self-hosted node is native Rust. Managed: U13 tries
  the Rust core compiled to wasm in a Worker (workers-rs). If that spike fails, use a thin
  TypeScript Worker (bun-managed) that speaks the same protocol and is tested against the Rust
  node. Rationale: one core for both paths, with a fallback if wasm does not build.
- **D24. Anonymous read of public repositories.** Decided 2026-10-20: yes for attach and fetch;
  Publish, OpenRequest, Revise, Approve and Comment need identity. Rationale: public reads need no
  account.
- **D25. Trace visibility.** Decided 2026-10-20: a trace inherits repository visibility; a
  `restricted` trace is readable by Maintain only. Rationale: the narrowest rule that protects
  restricted traces.
- **D26. Agent identities.** Decided 2026-10-20: none in this phase; agents use the owning device's
  token, narrowed by the `layer` claim. Rationale: no new identity model before the first provider
  works.
- **D27. Notifications.** Decided 2026-10-20: deferred; clients poll `LayerHead` and `ReadRequest`.
  Rationale: polling works with the wire already designed.
- **D28. Out of scope.** Decided 2026-10-20: repository deletion, data export and retention stay
  out of this design; add a unit when the hub asks. Rationale: none blocks the first hosted slice.

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
| U8 | `feat: control plane minimum: admin-token bootstrap, device-code API, CLI login, repo create` (in-process control plane, idempotent create, applied-permission push; no passkeys yet) | new `crates/javelin-control/`, `crates/javelin-cli` (login, repo create) | U7; D5 |
| U8b | `feat: passkey sign-in for device approval` (WebAuthn enrollment and sign-in on the control plane; browser approval page for device codes; no email) | `crates/javelin-control/src/passkey.rs`, `crates/javelin-control/src/device.rs` | U8 (the code lives in the crate U8 creates); D5 |
| U9 | `feat: author-owned revisions and exact approvals` (`PublishRequest`, review partition, tree IDs in collaboration) | `crates/javelin-collaboration/`, `crates/javelin-node/src/review.rs` | U2, U7 |
| U10 | `feat: restricted agent traces` (trace store, visibility, `missing_agent_trace` at node) | `crates/javelin-node/src/trace.rs` | U2, U7; D25 |
| U11 | `feat: forks with inherited history` (log copy, object copy, provenance, trace omission) | `crates/javelin-node/src/fork.rs`, `crates/javelin-control/` (fork op) | U2, U7, U10; D13, D14 |
| U12 | `feat: World log in one Durable Object per repository` (`WorldLog` on DO SQLite transactions; spike on a real DO in the PR: 50 concurrent publishers, kill inside a commit, replication citation re-checked against current docs) | `crates/javelin-node/src/log_do.rs`, `tools/spike-do/` (bun-managed) | U2; D1, D2 |
| U13 | `feat: Cloudflare managed node: Free-plan check, Worker, Durable Object, repo create` (step 1: verify Free-plan limits from Cloudflare docs and record citations, else mark unverified; step 2: Rust core to wasm via workers-rs, or the fallback thin TypeScript Worker, bun-managed, tested against the Rust node; step 3: `repo create` end to end) | `crates/javelin-node-worker/` (workers-rs entry), `tools/node-cloudflare/` (fallback only, bun-managed) | U8, U12; D1, D23 |
| U14 | `test: hosted acceptance suite` (contract gate 4 assertions, outage matrix, git stand-in with positive control) | new `tools/acceptance/`, `tests/hosted_*.rs` | U3, U5, U9, U11, U13 |
| U15 | `docs: CI execution design` (docs only; check ingestion and runner options) | new `docs/ci-execution.md` | D4 (can start now; no code) |
| U16 | `test: hosting measurements` (gate 6 fixtures, limits, cost) | `tools/` | U14; measurements committed before the run (`C§Verification gates` 6) |

Order that gets working behavior fastest: U1 → U2 → U3 → U5 (checkpoints and publish on a
local node) with U4 as soon as the rolling base merges; then U7 → U8 → U8b → U9; then U10 → U11; then
U12 → U13; then U14. U15 runs in parallel at any time.

## 9. Sources

Read 2026-10-20 (WebFetch unless noted):
- Cloudflare Durable Objects limits, https://developers.cloudflare.com/durable-objects/platform/limits/.
  SQLite-backed objects are available on Workers Free. Row and value size 2 MB; SQL statement
  100 KB; 100 bound parameters; CPU 30 s default, `limits.cpu_ms` up to 300,000 ms; soft limit
  1,000 requests/s per object; writes fail with `SQLITE_FULL` at the cap, reads still succeed.
  Account storage on Free is 5 GB (table). Per-object storage is 10 GB in the table but 1 GB on
  Free in the FAQ. UNRESOLVED; U13 checks.
- Cloudflare Workers limits, https://developers.cloudflare.com/workers/platform/limits/. Free:
  100,000 requests/day; 10 ms CPU per HTTP request; 128 MB memory; 50 subrequests per invocation;
  64 MiB Worker size. The page does not say whether Durable Object requests or CPU count against
  these. UNRESOLVED.
- Cloudflare Durable Objects pricing, https://developers.cloudflare.com/durable-objects/platform/pricing/.
  SQLite storage is included on Free for SQLite-backed objects; 5 GB total on Free. No statement
  about a payment method.
- Cloudflare blog, "SQLite in Durable Objects" (2024), https://blog.cloudflare.com/sqlite-in-durable-objects.
  Writes go to five follower replicas in different data centers; a write is confirmed once at
  least three of five respond; the output gate holds responses until confirmation. The post does
  not use the word "quorum" and is dated. U12 re-checks against current docs.
- Cloudflare changelog, Durable Objects on the Workers Free plan (2025-04-07),
  https://developers.cloudflare.com/changelog/post/2025-04-07-durable-objects-free-tier/ (fetched
  2026-10-20): "Durable Objects can now be used with zero commitment on the Workers Free plan";
  free plan limits apply. The SQLite-in-DO GA changelog
  (https://developers.cloudflare.com/changelog/2025-04-07-sqlite-in-durable-objects-ga, search
  excerpt) does not state Free availability.

Not verified (marked unverified until checked):
- A Free plan without a payment method. No official Cloudflare text was found (search,
  2026-10-20); third-party pages say no card is needed; the official plans text lists a $5/month
  minimum for Paid. U13 checks at signup.
- Whether R2 needs a payment method. From the user's account; not checked. R2 is not used.
- Whether Durable Object CPU counts against the Free 10 ms CPU per request.
- Whether the 100,000 requests/day is shared by Workers and Durable Object requests.
- The Rust core compiled to wasm via workers-rs (D23). U13 spike.
- Cloudflare compute for CI (D4). Control-plane location (D1, derived default).

Dropped: the R2 conditional-write search results and the Miniflare issue #774 from the first
draft. No decision rests on them now.
