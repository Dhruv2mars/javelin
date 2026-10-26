# Cloudflare managed node

The managed repository node for `javelin repo create --provider cloudflare` (U13, D1, D23).
One Worker routes `/admin/r/{repo}/...` to one `RepositoryNode` Durable Object per repository,
which keeps World 0 and the applied permissions in its SQLite storage. The control plane's side
is `crates/javelin-node-worker` (`CloudflareNode`, a `NodeLink`).

Scope is the Free plan only. Nothing here needs or should use a paid tier or a payment method.

## Commands

    bun install
    bun run typecheck        # tsc, strict
    bun run test             # local: wrangler dev (workerd) + Rust conformance + end-to-end repo create

Deploy, once a Cloudflare account on the Free plan exists and is logged in with `wrangler login`:

    bun run deploy                                # wrangler deploy
    bunx wrangler secret put NODE_ADMIN_TOKEN     # one-time; paste a random token, never commit it

Until the secret is set, the Worker refuses every admin call (`unauthorized`).

Then point the CLI at it: `javelin control init`, `javelin repo create OWNER/NAME --operation OP --provider cloudflare --node-url https://<worker>.workers.dev`, with `JAVELIN_NODE_ADMIN_TOKEN` set to the same token.

## The deployed node

Deployed 2026-10-26 on the Workers Free plan and left running (idle costs nothing):

    https://javelin-node.dhruv-sharma10102005.workers.dev

`GET /healthz` is open; every `/admin/r/...` call needs the admin token. The token is a 32-byte
random hex string set with `wrangler secret put`. It is kept on the owner's machine, outside
the repository; it is not in this repository, the PR or any log.

The account is on Workers Free. Measured: a deploy with `limits.cpu_ms` set is refused with
"CPU limits are not supported for the Free plan" (API code 100328). No paid plan, R2 or other
billed product is used. R2 is not needed: objects will live in the Durable Object's SQLite rows
(design doc 4.5).

Checks run against the deployed URL on 2026-10-26, each with a failing `git` stand-in first on
`PATH` (its marker stayed absent):

| Check | Command | Result |
| --- | --- | --- |
| Conformance suite | `JAVELIN_NODE_URL=<url> JAVELIN_NODE_ADMIN_TOKEN=... JAVELIN_NODE_REPOSITORY=repo_conformance_20261026 cargo test -p javelin-node-worker --test workerd -- --ignored` | 2 passed |
| `repo create --provider cloudflare` | the `bun run test` sequence by hand: wrong token, retry on another node, retry, repeat, collision | all five as on workerd: `unauthorized`, `operation_id_reused`, created at epoch 1 (node holds World 0 and epoch-1 permissions, creator `maintain`, private), same repository, `conflict` |
| Hosted acceptance `target_*` | `JAVELIN_ACCEPTANCE_NODE_URL=<url> JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN=... JAVELIN_ACCEPTANCE_TARGET=cloudflare-deployed bun run tools/acceptance/run.ts` | deployed target: 5 verified, 7 not verified, 0 failed (whole run: 23 verified, 16 not verified, 0 failed) |
| U12 commit-path spike | `SPIKE_URL=<spike url> bun run spike.ts` in `tools/spike-do` | 14 of 14 checks passed in 25 s |

Deployed target, verified: `flow.browser-device-approval`, `flow.public-private`,
`flow.retries`, `flow.permissions`, `outage.node-down-create`. Not verified, each because the
Worker does not serve it yet: `gate4.exact-revision-authorization` and
`gate4.immutable-prior-revisions` (review), `gate4.fork-data-without-credentials` (fork),
`gate4.restricted-traces` (Worlds and traces), `gate4.distinct-outage-outcomes` (Layer and
Publish wire), `outage.coordinator-transactions` (commit path; the spike covers it outside the
suite). `gate4.lost-reply-one-outcome` is not verified because a remote node cannot stand behind
the suite's loopback proxy.

The spike ran once against `WorldLog` deployed as its own Worker (`javelin-spike-do`, no fault
hooks), then deleted. 50 concurrent publishers on one object all published once after 655
`head_moved` retries; versions are contiguous 1..50; each operation and Layer is recorded once;
replays return the recorded version; a reused operation ID with another digest is refused. A
reply discarded unread is replayed by the retry as the same version; a request abandoned 5 ms
into flight had not committed, and its retry committed exactly once. The kill tests (2 and 3 in
`tools/spike-do`) stay local: a deployed object cannot be killed from outside.

Usage for all of the above: about 1,600 Worker requests (account analytics), under 2% of one
day's 100,000. Per-object storage peaked at 1.1 GB on the probe object and was deleted.

## Free-plan limits

Checked 2026-10-26 against the Cloudflare docs (pages fetched as Markdown, "last updated" dates
in brackets) and by measurement on this account. `bun run probe` repeats the measurements: it
deploys a short-lived `javelin-free-probe` Worker, measures, deletes everything, and writes
`probe/evidence/results.json`. It ran twice on 2026-10-26 with the same results.

| Item | Value | Source | Status |
| --- | --- | --- | --- |
| Durable Objects on Free | Available, zero commitment | `durable-objects/platform/pricing` [2026-10-18]; changelog 2025-04-07 (re-fetched) | Verified; deployed here |
| DO storage class | SQLite-backed only on Free; 100 DO classes per account | `durable-objects/platform/limits` [2026-06-01] | Verified |
| DO account storage | 5 GB on Free | `durable-objects/platform/limits`, `.../pricing` | Verified |
| DO per-object storage | Docs still contradict: the table says 10 GB, the `SQLITE_FULL` FAQ says "1 GB on the Free plan". Measured: one object accepted writes up to 1,108,000,768 bytes (`databaseSize`) with no `SQLITE_FULL`, also on a write a few minutes later, on two runs. The 1 GB figure is not enforced today; the hard bound seen on Free is the 5 GB account total | `durable-objects/platform/limits`; `bun run probe` | Settled by measurement. Plan for 1 GB per repository on Free anyway, since that is the documented figure |
| SQL limits | Row/string/BLOB 2 MB; statement 100 KB; 100 bound params | `durable-objects/platform/limits` | Verified |
| DO CPU | 30 s per request by default; `limits.cpu_ms` up to 5 min is Paid only | `durable-objects/platform/limits`; deploy refused with code 100328 | Verified |
| DO request rate | Soft 1,000 req/s per object | `durable-objects/platform/limits` | Verified |
| DO daily quota on Free | 100,000 requests/day; 13,000 GB-s/day; 5M SQLite rows read/day; 100,000 rows written/day | `durable-objects/platform/pricing` | Verified |
| Worker daily quota on Free | 100,000 requests/day, resets midnight UTC (error 1027) | `workers/platform/limits` [2026-10-12] | Verified |
| Worker CPU on Free | Docs: 10 ms per HTTP request. Measured: not a hard per-request cutoff. Successful requests used up to 841 ms of CPU (analytics `max.cpuTime`); requests near 1.5 s failed with error 1102 (`exceededResources`), and one of twelve requests of the size that usually succeeded (0.5 to 0.8 s) also failed | `workers/platform/limits`, `workers/platform/pricing`; `bun run probe` | Settled. Design for 10 ms in the Worker; do not rely on the slack |
| Worker memory / subrequests / size | 128 MB; 50 subrequests; 64 MiB | `workers/platform/limits` | Verified |
| Whether DO requests count toward the Worker daily request quota | No: two separate quotas of 100,000/day. The DO pricing table lists its own 100,000 requests/day on Free, and the changelog says "every Worker request able to call a Durable Object on the free plan". Measured: a call to the object is a subrequest of the Worker invocation, not another Worker request (1,527 Worker requests with 1,505 subrequests). A node request uses one of each | `durable-objects/platform/pricing`; changelog 2025-04-07; account analytics | Settled |
| Whether DO CPU counts toward the Worker's 10 ms | No. Measured: the same loop that fails in the Worker at about 1.5 s of CPU (error 1102) ran 2×10⁹ iterations, about 10 s, inside the object and returned 200, four times over two runs. The object is held to its own 30 s | `bun run probe` | Settled by measurement. Put heavy work (compose, hashing) in the object, not the Worker |
| Whether a Free account needs a payment method on file | Docs: Free is the default ("By default, users have access to the Workers Free plan") and DOs are "zero commitment" on it; no page requires a card for Workers Free or Durable Objects | `workers/platform/pricing`; changelog 2025-04-07 | Settled by docs. Not tested without a card: this account has one |
| 3-of-5 replication of SQLite-backed DOs | Only in the 2024 blog post, re-fetched unchanged: changes go to "five follower machines", and a write is confirmed "once at least three of these followers respond". Current docs state no replica count. They promise the output gate: responses wait until the write completes, and if it fails they are discarded and the object restarts | blog.cloudflare.com/sqlite-in-durable-objects (2024-09-26); `durable-objects/reference/glossary`, `.../best-practices/rules-of-durable-objects` | Settled. Javelin relies on the documented output gate, not on the replica count, which cannot be measured from outside |

## What runs on Cloudflare

Runs on Cloudflare today, in the `RepositoryNode` Durable Object behind the `javelin-node`
Worker:

- Provision: World 0 (the shared genesis) is created once per repository; a repeat is a no-op
  and a different genesis is refused.
- Permissions: the applied permissions with `javelin-identity`'s epoch rules.
- State: a read of both for the control plane and the checks. All three need the admin token.

The control plane (`javelin-control`) does not run on Cloudflare. It runs where the owner runs
the CLI and calls the Worker over HTTPS (`CloudflareNode`, a `NodeLink`).

Not on Cloudflare yet:

- The World-log commit path. `tools/spike-do`'s `WorldLog` proves the transaction on a deployed
  object, but it is a separate spike Worker, composition runs in the caller, and nothing wires it
  into `RepositoryNode`.
- Objects: blobs and checkpoints in 1 MiB segments (design doc 4.5); nothing is stored.
- Layers: `LayerAppend`, `LayerHead`, `LayerStack` and refresh.
- Review: `PublishRequest` revisions and approvals; also forks and agent traces.
- The protocol v2 wire and repository tokens: only the admin routes exist, so `javelin attach`
  and Publish cannot reach this node yet.

## wasm decision (D23 step 2)

`cargo build --target wasm32-unknown-unknown -p javelin --lib` still fails (re-run 2026-10-26):
11 errors in `src/workspace.rs` (unix-only symlink, `clone_file`, `copy_file`,
`file_clone_available`, `fingerprint`, `open_stat`). Every other crate depends on the root
crate. Making it wasm-clean is a refactor outside U13, so the fallback applies: the Worker is
TypeScript, bun-managed, and tested against the Rust control plane and the Rust conformance
suite (`crates/javelin-node-worker/tests/common`), which runs the same assertions against
`LocalNode` in CI and against this Worker on workerd and deployed.

### Sketch: the Rust node core on Cloudflare via wasm

Not built. The aim is one node core in Rust that runs as `javelin-node` on a machine and inside
the Durable Object through `workers-rs`, replacing the TypeScript Worker. Three problems block
it today: the root crate mixes the data model with filesystem work; storage is a concrete
`rusqlite::Connection`, and a Worker has no files, only the object's SQL API; and the node's
HTTP server and file lock are native. What it takes:

1. Split the root `javelin` crate. A new wasm-clean `javelin-core` takes the parts with no file
   I/O: `src/domain.rs`, `src/tree.rs`, `src/ignore.rs`, the World hash chain and format rules
   from `src/repository.rs`, and composition from `src/publish.rs` (`compose_in` takes a
   `&Connection` today, so it must take a store instead). The client side stays in `javelin`:
   `workspace.rs` and `workspace/`, `capture.rs`, `layer.rs`, `outbox.rs`, `attach.rs`,
   `adopt.rs`, `radar.rs`, `blame.rs` and `stack.rs`.
2. Put a store trait in `javelin-core` for the rows the node writes: `world`, `ops`, Layer and
   review rows, and segmented `objects`. It needs one synchronous transaction, which both
   `rusqlite` and the object's `transactionSync` provide. Two implementations: `rusqlite`
   (native, today's `Repository`) and Durable Object SQL (wasm). Check first that `workers-rs`
   exposes `transactionSync`; if it does not, that binding is a prerequisite.
3. `javelin-protocol`: `codec.rs` and `fetch.rs` are already free of `Repository`, and move to
   depend on `javelin-core` only. `receiver.rs`'s `WorldLog` trait returns the concrete
   `Repository` (`crates/javelin-node/src/log_do.rs` says so), so it must return the store trait;
   `sender.rs` and the `main.rs` binary stay native.
4. `javelin-node`: `world_log.rs`, `layer.rs`, `review.rs`, `trace.rs`, `fork.rs` and
   `checks.rs` call `rusqlite` on `node.db`; their SQL moves behind the store trait. `auth.rs`
   is already pure. `server.rs` (loopback HTTP) and the process lock in `world_log.rs` stay
   native, behind a feature. `log_do.rs` (the spike client) goes away.
5. `javelin-merge` has no I/O and `javelin-stack` reads through `Repository`; both move onto
   `javelin-core` and the store trait. `javelin-collaboration` and `javelin-identity` only need
   `javelin-core`; check that `ed25519-dalek` builds for wasm (it should, with no OS randomness
   needed for verification). `javelin-control`, `javelin-cli` and `javelin-node-worker` stay
   native.
6. A thin `javelin-node-cloudflare` crate (`workers-rs`): the Worker checks auth and routes;
   the Durable Object runs the core. Compose and hashing must run in the object: the Worker has
   10 ms of CPU on Free, and the measurements above show the object's CPU is held only to its
   own 30 s.

Then the conformance suite, `target_*` acceptance and the spike checks run unchanged against
the wasm node, and the TypeScript Worker is removed.

## What the tests prove locally

`bun run test` starts `wrangler dev` on workerd with fresh scratch state and a random admin token,
then checks:

1. The conformance suite passes against the Worker (`workerd_node_conforms`, `wrong_admin_token_is_refused`).
2. `repo create acme/app` against a node that refuses the token leaves the repository Provisioning; a retry naming another `--node-url` is refused as `operation_id_reused`; the retry with the same operation ID and node succeeds at epoch 1 on the `cloudflare` provider.
3. Node state holds the genesis and the permissions; a repeat is idempotent; a name collision returns `conflict`.

The test kills only the `wrangler` process it started.

CI runs the same checks in the `Node / Cloudflare (workerd)` job. `local_node_conforms` also runs the
shared suite against `LocalNode` in the `javelin-node-worker` crate.

## Known gaps

- The Worker holds provision, permissions and state only (see "What runs on Cloudflare" below).
- A retry of a create or fork whose record is still `Provisioning` must name the same `--provider` and `--node-url` as the first attempt. A different node is refused with `operation_id_reused` before anything is provisioned there, so no Cloudflare object is left with a genesis and no permissions. A trailing `/` on the URL is ignored. Once a create is `Ready`, a retry returns it unchanged on any node. A fork retry always calls the current node (`head`, `provision`, `fork`) before it returns, so a finished fork retried against another node is not refused. Forks are library-only today; this is left as a follow-up. A record written before the provider was stored has no provider, so every retry of it is refused and its name stays reserved. Nothing releases it: remove that record from the control state by hand (pre-release only).
