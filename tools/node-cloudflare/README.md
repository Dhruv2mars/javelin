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

    bunx wrangler secret put NODE_ADMIN_TOKEN    # one-time; paste a random token, never commit it
    bun run deploy                               # wrangler deploy

Then point the CLI at it: `javelin control init`, `javelin repo create OWNER/NAME --operation OP --provider cloudflare --node-url https://<worker>.workers.dev`, with `JAVELIN_NODE_ADMIN_TOKEN` set to the same token.

## Free-plan limits

Checked 2026-10-21 against the Cloudflare docs. "Unverified" means the page did not settle it, or
the page contradicts itself; the design relies on none of these for correctness.

| Item | Value | Source | Status |
| --- | --- | --- | --- |
| Durable Objects on Free | Available, zero commitment | `workers/platform/pricing` ("available both on Workers Free and Workers Paid"); design doc section 9 (changelog 2025-04-07) | Verified (pricing page); changelog re-check pending |
| DO storage class | SQLite-backed only on Free; 100 DO classes per account | `durable-objects/platform/limits` | Verified |
| DO account storage | 5 GB on Free | `durable-objects/platform/limits` | Verified |
| DO per-object storage | Table says 10 GB; storage-exceeded section says 1 GB on Free | `durable-objects/platform/limits` | **Unverified (contradiction)** |
| SQL limits | Row/string/BLOB 2 MB; statement 100 KB; 100 bound params | `durable-objects/platform/limits` | Verified |
| DO CPU | 30 s default, up to 5 min | `durable-objects/platform/limits` | Verified |
| DO request rate | Soft 1,000 req/s per object | `durable-objects/platform/limits` | Verified |
| DO daily quota on Free | 100,000 requests/day; 13,000 GB-s/day; 5M SQLite rows read/day; 100,000 rows written/day | `workers/platform/pricing` | Verified |
| Worker daily quota on Free | 100,000 requests/day, resets midnight UTC (error 1027 when exceeded) | `workers/platform/limits` | Verified |
| Worker CPU on Free | 10 ms per HTTP request | `workers/platform/limits`, `workers/platform/pricing` | Verified |
| Worker memory / subrequests / size | 128 MB; 50 subrequests; 64 MiB | `workers/platform/limits` | Verified |
| Whether DO requests count toward the Worker daily request quota | Not stated | — | **Unverified** |
| Whether DO CPU counts toward the Worker's 10 ms | Not stated | — | **Unverified** |
| Whether a Free account needs a payment method on file | Not stated (Paid page lists a $5/month minimum) | `workers/platform/pricing` | **Unverified**; not checked further, per the user's rule against payment details |
| 3-of-5 quorum replication of SQLite-backed DOs | Earlier 2024 post cited in design doc D2 | not re-fetched in this unit | **Unverified in this unit**; re-check pending |

## wasm decision (D23 step 2)

`cargo build --target wasm32-unknown-unknown` on the `javelin` root crate fails: 11 errors in
`src/workspace.rs` (unix-only symlink, `clone_file`, `copy_file`, `file_clone_available`,
`fingerprint`, `open_stat`) and bundled `rusqlite` C code. `javelin-protocol`, `javelin-identity`
and `javelin-collaboration` depend on it. Making it wasm-clean is a refactor outside U13, so the
fallback applies: the Worker is TypeScript, bun-managed, and tested against the Rust control
plane and the Rust conformance suite (`crates/javelin-node-worker/tests/common`), which runs
the same assertions against `LocalNode` in CI and against this Worker on workerd.

## What the tests prove locally

`bun run test` starts `wrangler dev` on workerd with fresh scratch state and a random admin token,
then checks:

1. The conformance suite passes against the Worker (`workerd_node_conforms`, `wrong_admin_token_is_refused`).
2. `repo create acme/app` against a dead node fails with `node_unavailable`; the retry with the same operation ID succeeds at epoch 1 on the `cloudflare` provider.
3. Node state holds the genesis and the permissions; a repeat is idempotent; a name collision returns `conflict`.

The test kills only the `wrangler` process it started.

CI does not run the Worker. `local_node_conforms` runs the same suite against `LocalNode` in the
`javelin-node-worker` crate, so the shared assertions are gated in CI.

## Pending: deployed-only checks

Need a real Free-plan Cloudflare account, which this unit did not use (no credentials, no login, no deploy):

- `wrangler deploy` succeeds on the Free plan with the SQLite-backed `RepositoryNode` class.
- The same conformance suite and `repo create` run against the deployed `workers.dev` URL.
- Storage and request behavior under load on the deployed object, and the unverified items above.
- `NODE_ADMIN_TOKEN` is set as a secret on the deployed Worker.
- Replication re-check (3-of-5 claim) against current Cloudflare docs.

## Known gaps

- The Worker holds provision, permissions and state. The World-log commit path (U12's spike) is not moved into the Worker.
- `RepositoryRecord` does not store the provider. A retry of the same operation ID with a different `--provider` returns the existing record. `javelin-control` was left untouched to avoid conflicts with U8b and U11.
