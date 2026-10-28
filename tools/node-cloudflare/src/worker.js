// The Javelin node on Cloudflare. The node itself is the Rust node core, compiled to wasm from
// `crates/javelin-node-cloudflare` (`bun run build`); this file is only the runtime glue:
//
// - The Worker routes each repository to its own `RepositoryNode` Durable Object. It checks
//   the admin token for `/admin/r/{repo}/{provision,permissions,keys,state}` and passes
//   protocol version 2 (`POST /v2/r/{repo}`) through with the caller's own token.
// - The object keeps the node's files as page rows in its SQLite storage and calls the core
//   inside `transactionSync`, so each request's writes commit together or not at all.

import { DurableObject } from "cloudflare:workers";
import wasm from "../build/javelin_node_cloudflare_bg.wasm";
import {
  initSync,
  maxFrame,
  pageSize,
  WasmNode,
  __wbg_reset_state as resetWasm,
} from "../build/javelin_node_cloudflare.js";

initSync({ module: wasm });
const PAGE = pageSize();
const MAX_FRAME = maxFrame();
// Bumped when a trap resets the wasm instance; every node opened before it is gone.
let generation = 0;

const REPO = /^[A-Za-z0-9._:-]{1,128}$/;
const ADMIN = /^\/admin\/r\/([^/]+)\/(provision|permissions|keys|state)$/;
const V2 = /^\/v2\/r\/([^/]+)$/;
const PREFLIGHT = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "POST",
  "access-control-allow-headers": "content-type",
  "access-control-max-age": "600",
};

export class RepositoryNode extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec(
      "CREATE TABLE IF NOT EXISTS files (name TEXT PRIMARY KEY, size INTEGER NOT NULL) WITHOUT ROWID",
    );
    sql.exec(
      "CREATE TABLE IF NOT EXISTS pages (name TEXT NOT NULL, n INTEGER NOT NULL, data BLOB NOT NULL, PRIMARY KEY (name, n)) WITHOUT ROWID",
    );
    // `JsPages` in `crates/javelin-node-cloudflare/src/worker.rs`.
    this.pages = {
      size: (name) => sql.exec("SELECT size FROM files WHERE name = ?", name).toArray()[0]?.size,
      setSize: (name, size) => {
        sql.exec(
          "INSERT INTO files (name, size) VALUES (?, ?) ON CONFLICT (name) DO UPDATE SET size = excluded.size",
          name,
          size,
        );
        sql.exec("DELETE FROM pages WHERE name = ? AND n >= ?", name, Math.ceil(size / PAGE));
      },
      get: (name, n) => {
        const row = sql.exec("SELECT data FROM pages WHERE name = ? AND n = ?", name, n).toArray()[0];
        return row ? new Uint8Array(row.data) : undefined;
      },
      put: (name, n, bytes) => {
        sql.exec(
          "INSERT INTO pages (name, n, data) VALUES (?, ?, ?) ON CONFLICT (name, n) DO UPDATE SET data = excluded.data",
          name,
          n,
          bytes.slice(),
        );
      },
      remove: (name) => {
        sql.exec("DELETE FROM pages WHERE name = ?", name);
        sql.exec("DELETE FROM files WHERE name = ?", name);
      },
    };
    this.node = null;
  }

  async fetch(request) {
    const url = new URL(request.url);
    const name = request.headers.get("x-javelin-repository");
    const body = new Uint8Array(await request.arrayBuffer());
    if (url.pathname === "/v2") {
      const authorization = request.headers.get("authorization") ?? undefined;
      const origin = request.headers.get("x-javelin-origin") === "1";
      return this.call(name, (node) => node.v2(authorization, origin, body));
    }
    const action = url.pathname.slice("/admin/".length);
    const response = this.call(name, (node) => node.admin(action, body));
    // The object's whole SQLite storage, for the node's budget against the per-object limit.
    response.headers.set("x-javelin-database-bytes", String(this.ctx.storage.sql.databaseSize));
    return response;
  }

  // Runs one call on the node in one storage transaction. A failure rolls the request's
  // writes back, and the node is dropped, since its caches may hold what was rolled back.
  call(name, run) {
    let reply;
    try {
      reply = this.ctx.storage.transactionSync(() => run(this.open(name)));
    } catch (error) {
      const old = this.node;
      this.node = null;
      if (error instanceof WebAssembly.RuntimeError) {
        // A trap leaves the instance unusable: start a new one for every object here.
        resetWasm();
        generation += 1;
      } else if (old?.generation === generation) {
        old.wasm.free();
      }
      return json({ error: "node_unavailable", detail: String(error) }, 503);
    }
    const headers = { "content-type": reply.json ? "application/json" : "application/octet-stream" };
    if (reply.cors) headers["access-control-allow-origin"] = "*";
    const status = reply.status;
    const bytes = reply.body;
    reply.free();
    return new Response(status === 204 ? null : bytes, { status, headers });
  }

  open(name) {
    if (this.node && this.node.generation === generation) return this.node.wasm;
    const wasmNode = new WasmNode(name, this.pages);
    this.node = { wasm: wasmNode, generation };
    this.migrate(wasmNode);
    return wasmNode;
  }

  // An object the TypeScript node (before the Rust core) provisioned holds World 0 and the
  // applied permissions in its own tables. They move into the node once, in this request's
  // transaction.
  migrate(node) {
    const sql = this.ctx.storage.sql;
    const legacy = sql
      .exec("SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('world', 'permissions')")
      .toArray();
    if (legacy.length === 0) return;
    const encode = (value) => new TextEncoder().encode(JSON.stringify(value));
    const settle = (reply) => {
      const status = reply.status;
      reply.free();
      if (status !== 200) throw new Error(`migration refused with ${status}`);
    };
    const world = legacy.some((t) => t.name === "world")
      ? sql.exec("SELECT id, tree FROM world WHERE n = 0").toArray()[0]
      : undefined;
    if (world) settle(node.admin("provision", encode({ genesis: { id: world.id, tree: world.tree } })));
    const stored = legacy.some((t) => t.name === "permissions")
      ? sql.exec("SELECT body FROM permissions WHERE k = 1").toArray()[0]
      : undefined;
    if (stored) settle(node.admin("permissions", new TextEncoder().encode(stored.body)));
    for (const table of legacy) sql.exec(`DROP TABLE ${table.name}`);
  }
}

function json(value, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

// Checks the admin token in constant time, so a wrong guess learns nothing about the prefix.
function authorized(request, expected) {
  if (!expected) return false;
  const given = request.headers.get("authorization") ?? "";
  const want = `Bearer ${expected}`;
  if (given.length !== want.length) return false;
  let diff = 0;
  for (let i = 0; i < want.length; i++) diff |= given.charCodeAt(i) ^ want.charCodeAt(i);
  return diff === 0;
}

function forward(env, repository, path, init) {
  const stub = env.REPOSITORY_NODE.get(env.REPOSITORY_NODE.idFromName(repository));
  init.headers["x-javelin-repository"] = repository;
  return stub.fetch(new Request(`https://node${path}`, init));
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/healthz") {
      return new Response("ok\n");
    }

    // Protocol version 2, with the transport statuses of the self-hosted node's server.
    const v2 = V2.exec(url.pathname);
    if (v2) {
      const [, repository] = v2;
      if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: PREFLIGHT });
      if (request.method !== "POST") return new Response(null, { status: 405 });
      if (!REPO.test(repository)) return new Response(null, { status: 404 });
      const length = request.headers.get("content-length");
      if (length === null) return new Response(null, { status: 411 });
      if (Number(length) > MAX_FRAME) return new Response(null, { status: 413 });
      const headers = { "x-javelin-origin": request.headers.has("origin") ? "1" : "0" };
      const authorization = request.headers.get("authorization");
      if (authorization !== null) headers.authorization = authorization;
      return forward(env, repository, "/v2", {
        method: "POST",
        headers,
        body: await request.arrayBuffer(),
      });
    }

    const admin = ADMIN.exec(url.pathname);
    if (!admin) return json({ error: "not_found" }, 404);
    const [, repository, action] = admin;
    if (!REPO.test(repository)) return json({ error: "invalid_input" }, 400);
    if (!authorized(request, env.NODE_ADMIN_TOKEN)) {
      return json({ error: "unauthorized" }, 401);
    }
    const writes = action !== "state";
    if (writes !== (request.method === "PUT")) {
      return json({ error: "method_not_allowed" }, 405);
    }
    return forward(env, repository, `/admin/${action}`, {
      method: writes ? "PUT" : "GET",
      headers: {},
      body: writes ? await request.arrayBuffer() : undefined,
    });
  },
};
