// The World log of U12 on Durable Object SQLite. One WorldLog object per repository.
// Its commit is one transactionSync: check the operation record, check the head the
// caller composed on, insert the version and the record, and advance the Layer. The
// caller composes (objects, trees, conflicts); this object only orders and records.

export interface Env {
  WORLD_LOG: DurableObjectNamespace;
  // "1" only in the spike, passed as `wrangler dev --var SPIKE_FAULTS:1`. Enables the
  // stall hooks the kill tests need. Never set on a deployed object.
  SPIKE_FAULTS?: string;
}

interface Faults {
  // Busy-wait inside the transaction, after the inserts and before commit.
  stallInTxnMs?: number;
  // Commit, then hold the reply for this long.
  stallAfterCommitMs?: number;
}

interface Commit {
  operation: string;
  digest: string;
  layer: string;
  expectedLayerHead: string;
  expectedWorld: number;
  version: { id: string; tree: string };
  faults?: Faults;
}

const ZERO = "0".repeat(64);

const SCHEMA = [
  "CREATE TABLE IF NOT EXISTS world (n INTEGER PRIMARY KEY, id TEXT NOT NULL, tree TEXT NOT NULL, operation TEXT NOT NULL)",
  "CREATE TABLE IF NOT EXISTS ops (operation TEXT PRIMARY KEY, digest TEXT NOT NULL, n INTEGER NOT NULL)",
  "CREATE TABLE IF NOT EXISTS layers (name TEXT PRIMARY KEY, state TEXT NOT NULL, head TEXT NOT NULL)",
  // Genesis is version 0 with the all-zero ID, the same shape as every object ID.
  `INSERT OR IGNORE INTO world (n, id, tree, operation) VALUES (0, '${ZERO}', '${ZERO}', '')`,
];

export class WorldLog implements DurableObject {
  private sql: SqlStorage;

  constructor(
    private ctx: DurableObjectState,
    private env: Env,
  ) {
    this.sql = ctx.storage.sql;
    ctx.blockConcurrencyWhile(async () => {
      for (const statement of SCHEMA) this.sql.exec(statement);
    });
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    try {
      if (request.method === "GET" && url.pathname === "/head") {
        return json(this.head());
      }
      if (request.method === "GET" && url.pathname === "/publication") {
        const operation = url.searchParams.get("operation") ?? "";
        return json(this.record(operation));
      }
      if (request.method === "GET" && url.pathname === "/state") {
        return json(this.state());
      }
      if (request.method === "POST" && url.pathname === "/layers") {
        const body = (await request.json()) as { layer: string; head: string };
        // Creation never changes an existing Layer: a repeat with the same checkpoint is a
        // no-op (so a published Layer stays published), a different checkpoint is refused.
        this.sql.exec(
          "INSERT OR IGNORE INTO layers (name, state, head) VALUES (?, 'active', ?)",
          body.layer,
          body.head,
        );
        const layer = this.sql
          .exec("SELECT state, head FROM layers WHERE name = ?", body.layer)
          .one();
        if (layer.head !== body.head) {
          return json({ outcome: "error", detail: "layer_exists" }, 409);
        }
        return json({ ok: true, state: layer.state });
      }
      if (request.method === "POST" && url.pathname === "/commit") {
        const body = (await request.json()) as Commit;
        const result = this.commit(body);
        if (body.faults?.stallAfterCommitMs && this.env.SPIKE_FAULTS === "1") {
          await sleep(body.faults.stallAfterCommitMs);
        }
        return json(result);
      }
      return new Response("not found", { status: 404 });
    } catch (error) {
      return json({ outcome: "error", detail: String(error) }, 500);
    }
  }

  private head(): { n: number; id: string; tree: string } {
    const row = this.sql
      .exec("SELECT n, id, tree FROM world ORDER BY n DESC LIMIT 1")
      .one();
    return { n: row.n as number, id: row.id as string, tree: row.tree as string };
  }

  private record(operation: string) {
    const op = this.sql
      .exec("SELECT digest, n FROM ops WHERE operation = ?", operation)
      .toArray()[0];
    if (!op) return { found: false };
    const version = this.sql
      .exec("SELECT n, id, tree FROM world WHERE n = ?", op.n)
      .one();
    return { found: true, digest: op.digest, ...version };
  }

  private state() {
    return {
      world: this.sql.exec("SELECT n, id, tree, operation FROM world ORDER BY n").toArray(),
      ops: this.sql.exec("SELECT operation, digest, n FROM ops ORDER BY n").toArray(),
      layers: this.sql.exec("SELECT name, state, head FROM layers ORDER BY name").toArray(),
    };
  }

  // One transaction. Everything that can refuse is decided before the first insert, so a
  // refusal writes nothing. A stall hook runs inside the transaction; a kill there must
  // leave no trace, which the driver checks after restart.
  private commit(body: Commit) {
    const faults = this.env.SPIKE_FAULTS === "1" ? body.faults : undefined;
    return this.ctx.storage.transactionSync(() => {
      const recorded = this.sql
        .exec("SELECT digest, n FROM ops WHERE operation = ?", body.operation)
        .toArray()[0];
      if (recorded) {
        if (recorded.digest !== body.digest) {
          return { outcome: "refused", code: "operation_id_reused" };
        }
        const version = this.sql
          .exec("SELECT n, id, tree FROM world WHERE n = ?", recorded.n)
          .one();
        return { outcome: "published", replayed: true, ...version };
      }

      const head = this.head();
      if (head.n !== body.expectedWorld) {
        return { outcome: "head_moved", head };
      }
      const layer = this.sql
        .exec("SELECT state, head FROM layers WHERE name = ?", body.layer)
        .toArray()[0];
      if (!layer) return { outcome: "refused", code: "unknown_layer" };
      if (layer.state !== "active") {
        return { outcome: "refused", code: "layer_published" };
      }
      if (layer.head !== body.expectedLayerHead) {
        return { outcome: "layer_stale", head: layer.head };
      }

      const n = head.n + 1;
      this.sql.exec(
        "INSERT INTO world (n, id, tree, operation) VALUES (?, ?, ?, ?)",
        n,
        body.version.id,
        body.version.tree,
        body.operation,
      );
      this.sql.exec(
        "INSERT INTO ops (operation, digest, n) VALUES (?, ?, ?)",
        body.operation,
        body.digest,
        n,
      );
      this.sql.exec("UPDATE layers SET state = 'published' WHERE name = ?", body.layer);
      if (faults?.stallInTxnMs) busyWait(faults.stallInTxnMs);
      return { outcome: "published", replayed: false, n, id: body.version.id, tree: body.version.tree };
    });
  }
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function busyWait(ms: number) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    // spin: the transaction must still be open when the process is killed
  }
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    // /r/{name}/{rest}: one WorldLog object per repository name.
    const match = /^\/r\/([^/]+)(\/.*)$/.exec(new URL(request.url).pathname);
    if (!match) return new Response("not found", { status: 404 });
    const stub = env.WORLD_LOG.get(env.WORLD_LOG.idFromName(match[1]));
    const url = new URL(request.url);
    url.pathname = match[2];
    return stub.fetch(new Request(url, request));
  },
} satisfies ExportedHandler<Env>;
