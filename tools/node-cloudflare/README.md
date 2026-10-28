# Cloudflare node

The repository node on Cloudflare (D1, D23), for `javelin repo create --provider cloudflare`.
It is the Rust node core, the same code as the self-hosted `javelin-node`, compiled to
wasm32-unknown-unknown and run in one `RepositoryNode` Durable Object per repository. The
control plane's side is `crates/javelin-node-worker` (`CloudflareNode`, a `NodeLink`).

Scope is the Free plan only. Nothing here needs or should use a paid tier or a payment method.

## How it runs

- `crates/javelin-node` without its `native` feature is the node core: the World-log commit
  path, objects, Layers (`LayerCreate`) and their writer leases, Stack, Publish Requests and
  their reads (`ListRequests`, `ReadReview`), checks, traces, audiences, and disclosure (delay,
  embargo, release and the public frontier a caller without a role is held at), and
  `wire::handle`, which answers one protocol version 2 body. It reaches its provider only through two traits in
  `storage.rs`: `Storage` (the repository, the node's records, two documents) and
  `Coordination` (the one ordering point per repository). The root `javelin` crate without its
  `fs` feature is the data model under it, with no filesystem code.
- The self-hosted node implements both traits with `Directory` (files, fsync and a process
  lock) and serves loopback HTTP. Nothing about it changed.
- The node's clock (token expiry, the frontier, embargo deadlines) is passed to
  `FileWorldLog::on`: the operating system's on a self-hosted node, the Worker's `Date` in the
  object, which the runtime advances between requests.
- `crates/javelin-node-cloudflare` implements them with `PageStorage`: the node's two SQLite
  databases run unchanged inside the wasm module, on a SQLite VFS that keeps each 64 KiB page as
  one row of the object's own SQLite storage (`pages`, `files`). Rows written per day is the
  tightest Free-plan quota, so pages are the largest SQLite allows. The rollback journal is in
  memory: the Worker calls the core inside `transactionSync`, so a request's page writes commit
  together or not at all, and a failed request drops the node so no cached page outlives a
  rollback. Coordination is the object itself: one instance per repository, one request at a
  time, since the core never awaits.
- `src/worker.js` is the only JavaScript: the Worker routes `/v2/r/{repo}` and
  `/admin/r/{repo}/...` to the repository's object and checks the admin token; the object gives
  the core its page rows. Compose, hashing and SQLite all run in the object, which has its own
  30 s CPU limit; the Worker spends at most about 2 ms per request (measured below).
- An object the TypeScript node provisioned (before 2026-10-28) keeps its World 0 and applied
  permissions: on its first request the object provisions the core from the old `world` table,
  applies the old `permissions` row and drops both tables, in that request's transaction.

## Routes

| Route | Auth | Answer |
| --- | --- | --- |
| `GET /healthz` | none | `ok` |
| `PUT /admin/r/{repo}/provision` `{"genesis":{"id","tree"}}` | admin token | `{"created":bool}`; another genesis is `409 genesis_mismatch` |
| `PUT /admin/r/{repo}/permissions` (`permissions.json` layout) | admin token | `{"applied","epoch"}`; `409 stale_permissions` with `seen`; `409 not_provisioned` |
| `PUT /admin/r/{repo}/keys` (`KeySet` JSON) | admin token | `{"trusted":true}`: the keys the node verifies bearer tokens with |
| `GET /admin/r/{repo}/state` | admin token | `{"provisioned","genesis","permissions"}` |
| `GET /admin/r/{repo}/budget`, `PUT` with a `budget.json` body | admin token | `{"budget":{"storage_bytes","rows_written_per_day","largest_object"},"usage":{"day",<budget>:{"used","limit"}}}`; `PUT` replaces the owner's budget (absent fields keep their default); `409 not_provisioned`; `400 invalid_input` |
| `POST /v2/r/{repo}` | caller's bearer token, or anonymous | one protocol message, as from the self-hosted node: 200 for every reply, 400 for a credential that is not a bearer token, 404 for a repository the node never provisioned, 405, 411, 413; `OPTIONS` answers the CORS preflight |

Admin replies carry `x-javelin-database-bytes`, the object's whole SQLite storage.

## Commands

    bun install
    bun run build            # the wasm module and its glue into build/ (wrangler runs it too)
    bun run test             # local: wrangler dev (workerd) + Rust conformance + end-to-end repo create
    bun run deploy           # wrangler deploy
    bunx wrangler secret put NODE_ADMIN_TOKEN     # one-time; paste a random token, never commit it

`build.sh` needs the `wasm32-unknown-unknown` Rust target, clang with the WebAssembly backend,
`llvm-ar` (macOS's `ar` writes an empty archive for wasm objects, so SQLite would be missing; the
script finds Homebrew's or takes `AR_wasm32_unknown_unknown`), and `wasm-bindgen-cli` at the
version `Cargo.lock` pins for `wasm-bindgen` (`cargo install wasm-bindgen-cli --version 0.2.129`).
It fails if any SQLite symbol is left unresolved in the module.

Until the secret is set, the Worker refuses every admin call (`unauthorized`). Then point the
CLI at it: `javelin control init`, `javelin repo create OWNER/NAME --operation OP --provider
cloudflare --node-url https://<worker>.workers.dev`, with `JAVELIN_NODE_ADMIN_TOKEN` set to the
same token.

## The deployed node

    https://javelin-node.dhruv-sharma10102005.workers.dev

The Rust node replaced the TypeScript node there on 2026-10-28 (first version
`3ffe1098-5392-44b7-8cec-91bafda4d764`, current `d0499fbb-bf29-4d2f-866a-68d659115c60`, which
carries requests and sync and the public frontier), on
Workers Free. The admin token is a 32-byte
random hex string set with `wrangler secret put` and kept on the owner's machine in
`~/.config/javelin/cloudflare-node-admin-token`; it is not in this repository or any log.

Checks run against it on 2026-10-28:

| Check | Command | Result |
| --- | --- | --- |
| Conformance suite, admin and protocol version 2 (commit path, request loop, embargo) | `JAVELIN_NODE_URL=<url> JAVELIN_NODE_ADMIN_TOKEN=... JAVELIN_NODE_REPOSITORY=repo_conformance_20261028e cargo test -p javelin-node-worker --test workerd -- --ignored --test-threads=1` | 3 passed |
| A repository the TypeScript node provisioned | `GET /admin/r/repo_conformance_20261026/state` before and after the deploy | identical genesis and epoch-2 permissions |

## Measured on Workers Free

Measured on the deployed node on 2026-10-28, last with version `d0499fbb` (`wrangler deploy`
output, `wrangler tail --format json` during the conformance run, the account's GraphQL
analytics, and `x-javelin-database-bytes`). Each figure is against the Free-plan limit in the
table below.

| Item | Measured | Free-plan limit | Headroom |
| --- | --- | --- | --- |
| Bundle | 3,424 KiB uploaded, 1,242 KiB gzip (with leases, requests and disclosure) | 64 MiB uncompressed; no compressed limit | 19x |
| Worker startup | 3 to 6 ms | 1 s | 160x |
| Worker CPU per request | max 1.2 ms on 2026-10-28 UTC, 2.07 ms on 2026-10-27 (analytics `max.cpuTime`); p50 0 ms in the tail | 10 ms | 4.8x |
| Object CPU per request | max 44 ms (provisioning: wasm instantiation, SQLite schema, World 0), next highest 25 ms; p50 1 ms over 27 sampled requests of the run that publishes, reviews and embargoes. The first deploy's first request took 102 ms | 30 s | 290x |
| Object storage per repository | 4,120,576 bytes for a provisioned, empty repository (63 pages of 64 KiB, one B-tree or more each); 4,460,544 bytes after the suite's three publishes, a request and an embargo | 1 GB documented per object, 5 GB per account | about 1,200 empty repositories per account |
| Rows per provision | about 71 written and 69 read (one provision and one state read, analytics delta on the first deploy) | 100,000 written and 5,000,000 read per day | about 1,400 provisions a day |
| The whole deployed conformance run (2026-10-28 02:36 to 02:37 UTC, per-minute analytics): two repositories, one provisioned with permissions pushed, one taken through uploads, three publishes, a Publish Request with its reads, an embargo and its release | 504 rows written, 426 read, 347 ms object CPU in total | as above | 0.5% of a day's writes |

The page size trades storage for rows: smaller pages would shrink an empty repository
(an estimated 0.8 MB at 16 KiB, not measured) but write more rows for every large object, and rows written per day
is the quota a busy repository meets first.

## The budget

The node core refuses a write that would cross its owner's budget with `quota_exceeded`
before it changes anything (`crates/javelin-node/src/budget.rs`, `docs/architecture.md`, Node
budget). The defaults are the Free-plan figures above: 1,000,000,000 bytes of storage per
repository (the documented per-object figure, though only the 5 GB account total was seen
enforced), 100,000 rows written per UTC day, and the protocol's 8 MiB per object (objects are
split into 64 KiB page rows, so the 2 MB SQL row limit does not bind). Rows written per day
and the 5 GB of storage are per account: an owner with several repositories on one account
lowers each repository's budget so their sum fits, with `PUT /admin/r/{repo}/budget`.
Requests per day, Worker CPU and object CPU are not budgeted: a refused request costs the same
request, the frame and merge bounds limit one request's work, and the measured CPU above has
4.8x headroom in the Worker and 290x in the object.

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
| Worker memory / subrequests / size | 128 MB; 50 subrequests; 64 MiB uncompressed, "no compressed size limit"; startup 1 s | `workers/platform/limits` [2026-10-12, re-fetched 2026-10-28] | Verified |
| Whether DO requests count toward the Worker daily request quota | No: two separate quotas of 100,000/day. The DO pricing table lists its own 100,000 requests/day on Free, and the changelog says "every Worker request able to call a Durable Object on the free plan". Measured: a call to the object is a subrequest of the Worker invocation, not another Worker request (1,527 Worker requests with 1,505 subrequests). A node request uses one of each | `durable-objects/platform/pricing`; changelog 2025-04-07; account analytics | Settled |
| Whether DO CPU counts toward the Worker's 10 ms | No. Measured: the same loop that fails in the Worker at about 1.5 s of CPU (error 1102) ran 2×10⁹ iterations, about 10 s, inside the object and returned 200, four times over two runs. The object is held to its own 30 s | `bun run probe` | Settled by measurement. Put heavy work (compose, hashing) in the object, not the Worker |
| Whether a Free account needs a payment method on file | Docs: Free is the default ("By default, users have access to the Workers Free plan") and DOs are "zero commitment" on it; no page requires a card for Workers Free or Durable Objects | `workers/platform/pricing`; changelog 2025-04-07 | Settled by docs. Not tested without a card: this account has one |
| 3-of-5 replication of SQLite-backed DOs | Only in the 2024 blog post, re-fetched unchanged: changes go to "five follower machines", and a write is confirmed "once at least three of these followers respond". Current docs state no replica count. They promise the output gate: responses wait until the write completes, and if it fails they are discarded and the object restarts | blog.cloudflare.com/sqlite-in-durable-objects (2024-09-26); `durable-objects/reference/glossary`, `.../best-practices/rules-of-durable-objects` | Settled. Javelin relies on the documented output gate, not on the replica count, which cannot be measured from outside |


## What the tests prove

- `crates/javelin-node-cloudflare/tests/parity.rs` runs one scenario on a self-hosted data
  directory and on `PageStorage` over in-memory pages, natively: uploads, `LayerAppend`,
  `PublishLayer` (the commit path), its replay and a reused operation, the writer lease
  (`LayerHead`, `LayerTake`), reads that follow the caller's audiences, a write refused for a
  hidden path, a Publish Request that publishes with its list, review and comment, a version
  held by an embargo from outsiders until it is released, and the browser, anonymous and
  outsider (`forbidden`) refusals. Both stores run on one moved clock. A second scenario holds
  both stores to a budget a few KB above their use: the crossing upload and Publish are
  refused with `quota_exceeded`, nothing is stored, and the same writes succeed once the budget
  is raised; a spent day's rows are accepted again the next UTC day. The rows the node counts
  are checked against the page rows the object writes. Every reply matches, object IDs masked (hidden entries are
  salted per store). It also checks that a reopened node keeps what it committed and that a
  second connection to an open database is refused.
- The conformance suite in `crates/javelin-node-worker/tests/common` runs against the
  self-hosted node (`local_node.rs`: `LocalNode` and the loopback server) and against the Worker
  (`workerd.rs`): the admin pushes, then over HTTP the transport statuses, an anonymous and
  browser read of World 0, an upload only its uploader can confirm, Layer messages for an
  unknown Layer, and the anonymous, untrusted-token, reviewer and browser refusals; then a
  Layer created, written and published with its replay, a Publish Request opened, listed, read
  and published, a signed-in outsider refused requests and Layers, and an embargoed version
  held from outsiders until a maintainer releases it; then the budget: a Publish and an upload
  past a few KB of storage, and an object past `largest_object`, refused and then accepted
  once the budget is raised (the Worker's `budget` route, or `budget.json` on the self-hosted
  node).
- `bun run test` starts `wrangler dev` with fresh state, runs that suite against it, then
  `repo create acme/app` end to end with the CLI: a refused token leaves the create
  Provisioning, a retry naming another node is `operation_id_reused`, the retry completes at
  epoch 1, a repeat returns the same repository, and a name collision is `conflict`. It kills
  only the `wrangler` process it started.

## Known gaps

- The hosted acceptance `target_*` checks that need Layers on a remote node are still marked
  not verified: the conformance suite drives those paths through the Worker, but the
  acceptance suite has not been rerun against it.
- Fork is not served: `NodeLink::fork` needs two repositories' objects to stream between.
- Stack on the object merges without the volume alias probe the self-hosted node runs on its
  own disk (`javelin-stack`'s `fs` feature): the object has no volume. The client's refresh
  still meets any case or Unicode alias when it writes the files.
- Two credentials in one request are a 400 on the self-hosted node; the Workers runtime joins
  repeated `Authorization` headers into one value, which then fails as an invalid token.
- The budget does not count admin pushes (about 71 rows per provision) or the Worker's own
  rows, and each repository counts only its own rows against the account's daily quota.
- A retry of a create or fork whose record is still `Provisioning` must name the same `--provider` and `--node-url` as the first attempt. A different node is refused with `operation_id_reused` before anything is provisioned there, so no Cloudflare object is left with a genesis and no permissions. A trailing `/` on the URL is ignored. Once a create is `Ready`, a retry returns it unchanged on any node. A record written before the provider was stored has no provider, so every retry of it is refused and its name stays reserved. Nothing releases it: remove that record from the control state by hand (pre-release only).
