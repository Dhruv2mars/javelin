// The managed repository node on Cloudflare (U13, D1, D23 fallback). One RepositoryNode
// Durable Object per repository holds the node's state: its World 0 and its applied
// permissions. The control plane reaches it through /admin/r/{repo}/... with the node's
// admin token, the same two pushes `javelin-control` makes to a self-hosted node
// (`NodeLink::provision` and `NodeLink::apply`), plus a read of the state for checks.
// Permissions follow `javelin-identity`'s `Access::apply` epoch rules exactly.

export interface Env {
  REPOSITORY_NODE: DurableObjectNamespace;
  // Set with `wrangler secret put NODE_ADMIN_TOKEN` (or `--var` locally). Required.
  NODE_ADMIN_TOKEN?: string;
}

const HEX64 = /^[0-9a-f]{64}$/;
const REPO = /^[A-Za-z0-9._:-]{1,128}$/;
const ROLES = new Set(["read", "review", "maintain"]);

interface Genesis {
  id: string;
  tree: string;
}

// The same JSON shape `javelin-control`'s LocalNode writes to permissions.json.
interface Permissions {
  repository: string;
  epoch: number;
  public: boolean;
  roles: Record<string, string>;
  required_approvals: number;
}

const SCHEMA = [
  "CREATE TABLE IF NOT EXISTS world (n INTEGER PRIMARY KEY, id TEXT NOT NULL, tree TEXT NOT NULL)",
  "CREATE TABLE IF NOT EXISTS permissions (k INTEGER PRIMARY KEY CHECK (k = 1), epoch INTEGER NOT NULL, body TEXT NOT NULL)",
];

export class RepositoryNode implements DurableObject {
  private sql: SqlStorage;

  constructor(
    private ctx: DurableObjectState,
    _env: Env,
  ) {
    this.sql = ctx.storage.sql;
    ctx.blockConcurrencyWhile(async () => {
      for (const statement of SCHEMA) this.sql.exec(statement);
    });
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    try {
      if (request.method === "PUT" && url.pathname === "/provision") {
        return this.provision((await request.json()) as { genesis: Genesis });
      }
      if (request.method === "PUT" && url.pathname === "/permissions") {
        return this.apply((await request.json()) as Permissions);
      }
      if (request.method === "GET" && url.pathname === "/state") {
        return json(this.state());
      }
      return json({ error: "not_found" }, 404);
    } catch (error) {
      return json({ error: "storage_error", detail: String(error) }, 500);
    }
  }

  // Creates World 0 once. A repeat with the same genesis changes nothing; a different one
  // is refused, so a retry cannot rewrite an existing repository's history.
  private provision(body: { genesis: Genesis }): Response {
    return this.ctx.storage.transactionSync(() => {
      const current = this.genesis();
      if (current) {
        if (current.id !== body.genesis.id || current.tree !== body.genesis.tree) {
          return json({ error: "genesis_mismatch" }, 409);
        }
        return json({ created: false });
      }
      this.sql.exec(
        "INSERT INTO world (n, id, tree) VALUES (0, ?, ?)",
        body.genesis.id,
        body.genesis.tree,
      );
      return json({ created: true });
    });
  }

  // The epoch rules of `javelin-identity`'s `Access::apply`: an older epoch is stale; the same
  // epoch is accepted only when identical; a newer epoch replaces the stored permissions.
  private apply(next: Permissions): Response {
    return this.ctx.storage.transactionSync(() => {
      if (!this.genesis()) return json({ error: "not_provisioned" }, 409);
      const stored = this.sql
        .exec("SELECT epoch, body FROM permissions WHERE k = 1")
        .toArray()[0];
      if (stored) {
        const seen = stored.epoch as number;
        if (next.epoch < seen) return json({ error: "stale_permissions", seen }, 409);
        if (next.epoch === seen) {
          if (canonical(JSON.parse(stored.body as string)) === canonical(next)) {
            return json({ applied: false, epoch: seen });
          }
          return json({ error: "stale_permissions", seen }, 409);
        }
        this.sql.exec(
          "UPDATE permissions SET epoch = ?, body = ? WHERE k = 1",
          next.epoch,
          JSON.stringify(next),
        );
        return json({ applied: true, epoch: next.epoch });
      }
      this.sql.exec(
        "INSERT INTO permissions (k, epoch, body) VALUES (1, ?, ?)",
        next.epoch,
        JSON.stringify(next),
      );
      return json({ applied: true, epoch: next.epoch });
    });
  }

  private genesis(): Genesis | null {
    const row = this.sql.exec("SELECT id, tree FROM world WHERE n = 0").toArray()[0];
    return row ? { id: row.id as string, tree: row.tree as string } : null;
  }

  private state() {
    const genesis = this.genesis();
    const stored = this.sql.exec("SELECT body FROM permissions WHERE k = 1").toArray()[0];
    return {
      provisioned: genesis !== null,
      genesis,
      permissions: stored ? JSON.parse(stored.body as string) : null,
    };
  }
}

// Key order does not matter to the node: `BTreeMap` roles compare equal to any order.
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    const entries = Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical((value as Record<string, unknown>)[key])}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(value);
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

// Checks the admin token in constant time, so a wrong guess learns nothing about the prefix.
function authorized(request: Request, expected: string | undefined): boolean {
  if (!expected) return false;
  const given = request.headers.get("authorization") ?? "";
  const want = `Bearer ${expected}`;
  if (given.length !== want.length) return false;
  let diff = 0;
  for (let i = 0; i < want.length; i++) diff |= given.charCodeAt(i) ^ want.charCodeAt(i);
  return diff === 0;
}

// The Worker checks the body before forwarding; it never reaches storage malformed.
function validPermissions(body: unknown, repository: string): body is Permissions {
  if (!body || typeof body !== "object") return false;
  const p = body as Permissions;
  if (p.repository !== repository) return false;
  if (!Number.isSafeInteger(p.epoch) || p.epoch < 0) return false;
  if (typeof p.public !== "boolean") return false;
  if (!Number.isSafeInteger(p.required_approvals) || p.required_approvals < 0) return false;
  if (!p.roles || typeof p.roles !== "object") return false;
  return Object.values(p.roles).every((role) => ROLES.has(role));
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/healthz") {
      return new Response("ok\n");
    }
    const match = /^\/admin\/r\/([^/]+)\/(provision|permissions|state)$/.exec(url.pathname);
    if (!match) return json({ error: "not_found" }, 404);
    const [, repository, action] = match;
    if (!REPO.test(repository)) return json({ error: "invalid_input" }, 400);
    if (!authorized(request, env.NODE_ADMIN_TOKEN)) {
      return json({ error: "unauthorized" }, 401);
    }

    const writes = action !== "state";
    if (writes !== (request.method === "PUT")) {
      return json({ error: "method_not_allowed" }, 405);
    }
    // Writes are validated here, before any storage call, and forwarded as a fresh body.
    let body = "";
    if (writes) {
      const parsed = await request.json().catch(() => null);
      if (action === "provision") {
        const genesis = (parsed as { genesis?: Genesis } | null)?.genesis;
        if (!genesis || !HEX64.test(genesis.id) || !HEX64.test(genesis.tree)) {
          return json({ error: "invalid_input", detail: "genesis needs 64-hex id and tree" }, 400);
        }
      } else if (!validPermissions(parsed, repository)) {
        return json({ error: "invalid_input", detail: "malformed permissions" }, 400);
      }
      body = JSON.stringify(parsed);
    }

    const stub = env.REPOSITORY_NODE.get(env.REPOSITORY_NODE.idFromName(repository));
    return stub.fetch(
      new Request(`https://node/${action}`, {
        method: request.method,
        body: writes ? body : undefined,
      }),
    );
  },
} satisfies ExportedHandler<Env>;
