// U12 spike driver. Starts `wrangler dev` (workerd, local, no account) and checks the
// World log's commit transaction in WorldLog (src/index.ts):
//   1. 50 concurrent publishers on one repository: one version per n, replays hold.
//   1b. Re-creating a published Layer does not reset it; a conflicting create is refused.
//   2. SIGKILL inside a commit (stalled in the transaction): no partial version after
//      restart; the retry commits exactly once.
//   3. SIGKILL after commit, before the reply: the retry replays the same version.
// Results go to evidence/results.json. Exit code 1 on any failed check.

import { spawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, openSync, writeFileSync } from "node:fs";

const PORT = Number(process.env.SPIKE_PORT ?? 18787);
const BASE = `http://127.0.0.1:${PORT}`;
const PUBLISHERS = 50;
const RUN = Date.now().toString(36);

mkdirSync("evidence", { recursive: true });
const checks: { name: string; ok: boolean; detail: string }[] = [];
function check(name: string, ok: boolean, detail = "") {
  checks.push({ name, ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
}

// --- wrangler lifecycle -------------------------------------------------------

let server: ChildProcess | undefined;
const log = openSync("evidence/wrangler.log", "a");

async function start() {
  server = spawn(
    "node_modules/.bin/wrangler",
    [
      "dev", "--local", "--ip", "127.0.0.1", "--port", String(PORT),
      "--persist-to", ".spike-state", "--var", "SPIKE_FAULTS:1",
      "--show-interactive-dev-session=false",
    ],
    {
      detached: true, // own process group, so the kill takes workerd down too
      stdio: ["ignore", log, log],
      env: { ...process.env, CI: "1", WRANGLER_SEND_METRICS: "false" },
    },
  );
  for (let i = 0; i < 120; i++) {
    if (await probe(`${BASE}/r/__ready/head`, 1000)) return;
    await sleep(500);
  }
  throw new Error("wrangler did not become ready");
}

// SIGKILL the whole group, then wait until the port stops answering.
async function kill() {
  if (!server?.pid) return;
  const exited = new Promise((resolve) => server!.once("exit", resolve));
  process.kill(-server.pid, "SIGKILL");
  await exited;
  for (let i = 0; i < 60 && (await probe(`${BASE}/r/__ready/head`, 300)); i++) {
    await sleep(100);
  }
  server = undefined;
}

// --- HTTP helpers --------------------------------------------------------------

type Json = Record<string, any>;

async function probe(url: string, timeout: number): Promise<boolean> {
  try {
    await fetch(url, { signal: AbortSignal.timeout(timeout) });
    return true;
  } catch {
    return false;
  }
}

async function get(path: string, timeout = 10_000): Promise<Json> {
  const response = await fetch(`${BASE}${path}`, { signal: AbortSignal.timeout(timeout) });
  return response.json() as Promise<Json>;
}

async function post(path: string, body: unknown, timeout = 20_000): Promise<Json> {
  const response = await fetch(`${BASE}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(timeout),
  });
  return response.json() as Promise<Json>;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

// --- commit bodies ---------------------------------------------------------------

interface CommitBody {
  operation: string;
  digest: string;
  layer: string;
  expectedLayerHead: string;
  expectedWorld: number;
  version: { id: string; tree: string };
  faults?: { stallInTxnMs?: number; stallAfterCommitMs?: number };
}

// The digest stands in for the Rust side's hash of the encoded request: it covers what the
// operation means, not the head it was composed on, so a retry on a new head keeps it.
function digestOf(body: CommitBody): string {
  const { layer, expectedLayerHead, version } = body;
  return createHash("sha256")
    .update(JSON.stringify([layer, expectedLayerHead, version]))
    .digest("hex");
}

function commitFor(repo: string, operation: string, layer: string, head: Json): CommitBody {
  const body: CommitBody = {
    operation,
    digest: "",
    layer,
    expectedLayerHead: `c-${layer}`,
    expectedWorld: head.n,
    version: { id: `v-${repo}-${operation}-${head.n + 1}`, tree: `t-${operation}` },
  };
  body.digest = digestOf(body);
  return body;
}

// --- test 1: 50 concurrent publishers --------------------------------------------

async function concurrentPublishers() {
  const repo = `pub-${RUN}`;
  const layers = Array.from({ length: PUBLISHERS }, (_, i) => `L${String(i).padStart(2, "0")}`);
  for (const layer of layers) {
    await post(`/r/${repo}/layers`, { layer, head: `c-${layer}` });
  }

  let retries = 0;
  const committed: { operation: string; layer: string; body: CommitBody; n: number }[] = [];
  await Promise.all(
    layers.map(async (layer, p) => {
      const operation = `publish-${p}`;
      for (;;) {
        const head = await get(`/r/${repo}/head`);
        const body = commitFor(repo, operation, layer, head);
        const outcome = await post(`/r/${repo}/commit`, body);
        if (outcome.outcome === "published") {
          committed.push({ operation, layer, body, n: outcome.n });
          return;
        }
        if (outcome.outcome !== "head_moved") throw new Error(JSON.stringify(outcome));
        retries++;
      }
    }),
  );

  check(
    "50 publishers all publish once",
    committed.length === PUBLISHERS,
    `${committed.length} published, ${retries} head_moved retries`,
  );

  // Replay: each publisher resends its committed request; it must get the same n back.
  const replays = await Promise.all(
    committed.map((c) => post(`/r/${repo}/commit`, c.body)),
  );
  const replayOk = replays.every(
    (r, i) => r.outcome === "published" && r.replayed === true && r.n === committed[i].n,
  );
  check("replay returns the recorded version", replayOk);

  // Same operation, different request: refused, nothing written.
  const forged = { ...committed[0].body, version: { id: "forged", tree: "forged" } };
  forged.digest = digestOf(forged);
  const reused = await post(`/r/${repo}/commit`, forged);
  check(
    "reused operation with a different digest is refused",
    reused.outcome === "refused" && reused.code === "operation_id_reused",
    JSON.stringify(reused),
  );

  const state = await get(`/r/${repo}/state`);
  const ns = state.world.map((w: Json) => w.n);
  const contiguous = ns.length === PUBLISHERS + 1 && ns.every((n: number, i: number) => n === i);
  check("one version per n, contiguous 0..50", contiguous, `${ns.length} rows`);
  const ops = new Set(state.world.map((w: Json) => w.operation).filter(Boolean));
  check("each operation recorded once", ops.size === PUBLISHERS && state.ops.length === PUBLISHERS);
  const allPublished = state.layers.every((l: Json) => l.state === "published");
  check("every Layer published exactly once", allPublished && state.layers.length === PUBLISHERS);
}

// --- test 1b: a Layer's lifecycle is not reset by re-creating it -------------------------

async function layerLifecycle() {
  const repo = `life-${RUN}`;
  await post(`/r/${repo}/layers`, { layer: "p", head: "c-p" });
  const head = await get(`/r/${repo}/head`);
  const first = await post(`/r/${repo}/commit`, commitFor(repo, "life-op-1", "p", head));
  check("layer publishes once", first.outcome === "published" && first.n === 1, JSON.stringify(first));

  // Same checkpoint again: a no-op, and the Layer stays published.
  const again = await post(`/r/${repo}/layers`, { layer: "p", head: "c-p" });
  check("re-creating a published Layer with the same head is a no-op", again.state === "published", JSON.stringify(again));

  // A second operation on the published Layer is refused, and nothing is written.
  const second = await post(`/r/${repo}/commit`, commitFor(repo, "life-op-2", "p", { n: 1 }));
  check(
    "a published Layer cannot publish again after re-creation",
    second.outcome === "refused" && second.code === "layer_published",
    JSON.stringify(second),
  );

  // A different checkpoint for an existing Layer is refused.
  const conflict = await post(`/r/${repo}/layers`, { layer: "p", head: "c-other" });
  check("conflicting re-creation is refused", conflict.outcome === "error" && conflict.detail === "layer_exists", JSON.stringify(conflict));

  const state = await get(`/r/${repo}/state`);
  check(
    "the lifecycle left one version and a published Layer",
    state.world.length === 2 && state.layers[0].state === "published" && state.layers[0].head === "c-p",
  );
}

// --- test 2: kill inside a commit ---------------------------------------------------

async function killInsideCommit() {
  const repo = `kill-txn-${RUN}`;
  await post(`/r/${repo}/layers`, { layer: "k", head: "c-k" });
  const body = commitFor(repo, "kill-op", "k", { n: 0 });
  body.faults = { stallInTxnMs: 60_000 };

  // The request stalls inside the transaction. Probe the object while it is stalled:
  // a blocked object does not answer, which shows the commit is still in flight.
  const inflight = post(`/r/${repo}/commit`, body, 120_000).catch((e) => `error: ${e}`);
  await sleep(1500);
  const answered = await probe(`${BASE}/r/${repo}/head`, 1500);
  check("commit is in flight inside the transaction before the kill", !answered);

  await kill();
  await inflight;
  await start();

  const head = await get(`/r/${repo}/head`);
  check("no partial version after the kill", head.n === 0, JSON.stringify(head));
  const record = await get(`/r/${repo}/publication?operation=kill-op`);
  check("no operation record after the kill", record.found === false);
  const state = await get(`/r/${repo}/state`);
  check(
    "Layer still active and unchanged after the kill",
    state.layers.length === 1 && state.layers[0].state === "active" && state.layers[0].head === "c-k",
  );

  const retry = await post(`/r/${repo}/commit`, commitFor(repo, "kill-op", "k", { n: 0 }));
  check(
    "retry after the kill commits exactly once",
    retry.outcome === "published" && retry.replayed === false && retry.n === 1,
    JSON.stringify(retry),
  );
  const after = await get(`/r/${repo}/state`);
  check("one row for the operation after the retry", after.world.filter((w: Json) => w.operation === "kill-op").length === 1);
}

// --- test 3: kill after commit, before the reply ----------------------------------------

async function killAfterCommit() {
  const repo = `kill-reply-${RUN}`;
  await post(`/r/${repo}/layers`, { layer: "r", head: "c-r" });
  const body = commitFor(repo, "reply-op", "r", { n: 0 });
  body.faults = { stallAfterCommitMs: 60_000 };

  const inflight = post(`/r/${repo}/commit`, body, 120_000).catch((e) => `error: ${e}`);
  await sleep(1500);
  // The commit is already durable when the reply is held: the record is visible.
  const record = await get(`/r/${repo}/publication?operation=reply-op`);
  check("commit is durable while its reply is held", record.found === true && record.n === 1, JSON.stringify(record));

  await kill();
  await inflight;
  await start();

  // The retry is the same request without the stall: the operation, not the faults, is retried.
  const retry = await post(`/r/${repo}/commit`, { ...body, faults: undefined });
  check(
    "retry after a lost reply replays the same version",
    retry.outcome === "published" && retry.replayed === true && retry.n === 1,
    JSON.stringify(retry),
  );
  const state = await get(`/r/${repo}/state`);
  check(
    "still one version for the operation",
    state.world.filter((w: Json) => w.operation === "reply-op").length === 1 && state.world.length === 2,
  );
}

// --- run -----------------------------------------------------------------------------

async function main() {
  await start();
  try {
    await concurrentPublishers();
    await layerLifecycle();
    await killInsideCommit();
    await killAfterCommit();
  } finally {
    await kill();
  }
  const ok = checks.every((c) => c.ok);
  writeFileSync(
    "evidence/results.json",
    JSON.stringify({ run: RUN, workerd: "1.20261019.1", wrangler: "4.151.3", ok, checks }, null, 2),
  );
  console.log(ok ? "ALL CHECKS PASSED" : "SOME CHECKS FAILED");
  process.exit(ok ? 0 : 1);
}

main().catch(async (error) => {
  console.error(error);
  await kill();
  process.exit(1);
});
