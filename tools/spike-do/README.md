# spike-do: the World log on a Durable Object (U12)

`src/index.ts` is the `WorldLog` Durable Object: one SQLite-backed object per repository
with the commit transaction of `docs/online-design.md` 4.5. `spike.ts` runs it under
`wrangler dev` (workerd, local, no account) and checks the properties U12 names.

    bun install
    bun run typecheck
    bun run spike          # exit 0 and "ALL CHECKS PASSED" on success

The spike starts its own `wrangler dev` on port 18787 (`SPIKE_PORT` overrides it), with
persisted state in `.spike-state/` and results in `evidence/results.json`. Both are ignored.

To run the Rust client test (`crates/javelin-node/tests/log_do.rs`) against the object,
start `wrangler dev` yourself and point `JAVELIN_DO_URL` at it.

## What the spike checks

1. 50 concurrent publishers on one repository, each composing on the head it read and
   retrying on `head_moved`: every one publishes once; versions are contiguous 1..50; each
   operation is recorded once; each Layer publishes once; a replay returns the recorded
   version; the same operation with another digest is refused.
2. SIGKILL while a commit is stalled inside the transaction (probed: the object stops
   answering, so the commit is in flight). After restart: no version, no record, Layer
   still active. The retry commits exactly once.
3. SIGKILL after commit, before the reply (probed: the record is visible while the reply is
   held). After restart the retry replays the same version.

## Limits of this evidence

- workerd local storage is not the production Durable Object storage. Replication to
  followers does not happen locally, so the 3-of-5 confirmation is not exercised here.
- Deployed-object checks (Free plan, real storage, real replication) are U13.
- Composition runs in the caller, not inside the transaction. The object checks that the
  head the caller composed on is still the head. U13 moves composition into the Worker.
- The `SPIKE_FAULTS` stall hooks are honoured only when wrangler is started with
  `--var SPIKE_FAULTS:1`. They are for the spike; do not deploy with them.
