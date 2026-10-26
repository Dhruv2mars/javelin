// Free-plan probe (see ../README.md, "Free-plan limits"). Measures on a deployed account what
// the docs leave open: whether a Durable Object's CPU is held to the Worker's 10 ms, and where
// a SQLite-backed object's storage stops. Every route needs the PROBE_TOKEN secret.

export interface Env {
  PROBE: DurableObjectNamespace;
  PROBE_TOKEN?: string;
}

// Pure CPU: no I/O, so CPU time is the whole cost. Date.now() is frozen while code runs on
// Workers, so the loop counts iterations, not milliseconds.
function burn(iterations: number): number {
  let x = 1;
  for (let i = 0; i < iterations; i++) x = Math.imul(x ^ i, 2654435761) >>> 0;
  return x;
}

function count(url: URL, name: string, max: number): number {
  const value = Number(url.searchParams.get(name) ?? "0");
  return Number.isSafeInteger(value) && value >= 0 && value <= max ? value : 0;
}

export class Probe implements DurableObject {
  private sql: SqlStorage;

  constructor(
    private ctx: DurableObjectState,
    _env: Env,
  ) {
    this.sql = ctx.storage.sql;
    this.sql.exec("CREATE TABLE IF NOT EXISTS fill (k INTEGER PRIMARY KEY, b BLOB NOT NULL)");
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    switch (url.pathname) {
      case "/burn":
        return Response.json({ x: burn(count(url, "n", 1e10)) });
      case "/size":
        return Response.json({ bytes: this.sql.databaseSize });
      case "/fill": {
        // Appends `rows` zero-filled BLOBs of `bytes` each. SQLite stores zeroblob pages in
        // full, so the database grows by about rows * bytes.
        const rows = count(url, "rows", 1000);
        const bytes = count(url, "bytes", 1_900_000);
        let inserted = 0;
        try {
          for (; inserted < rows; inserted++) {
            this.sql.exec("INSERT INTO fill (b) VALUES (zeroblob(?))", bytes);
          }
          return Response.json({ full: false, inserted, bytes: this.sql.databaseSize });
        } catch (error) {
          // Only SQLITE_FULL is the storage limit. Any other failure is not a measurement, and
          // is reported as an error so the runner stops and fails.
          const message = String(error);
          const full = message.includes("SQLITE_FULL");
          return Response.json(
            { full, inserted, error: message, bytes: this.sql.databaseSize },
            { status: full ? 200 : 500 },
          );
        }
      }
      case "/clear":
        await this.ctx.storage.deleteAll();
        return Response.json({ cleared: true });
      default:
        return Response.json({ error: "not_found" }, { status: 404 });
    }
  }
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const want = env.PROBE_TOKEN ? `Bearer ${env.PROBE_TOKEN}` : null;
    if (!want || request.headers.get("authorization") !== want) {
      return Response.json({ error: "unauthorized" }, { status: 401 });
    }
    const url = new URL(request.url);
    if (url.pathname === "/worker/burn") {
      return Response.json({ x: burn(count(url, "n", 1e10)) });
    }
    const match = /^\/object\/([a-z0-9-]{1,32})(\/[a-z]+)$/.exec(url.pathname);
    if (!match) return Response.json({ error: "not_found" }, { status: 404 });
    const stub = env.PROBE.get(env.PROBE.idFromName(match[1]));
    return stub.fetch(new Request(`https://probe${match[2]}${url.search}`));
  },
} satisfies ExportedHandler<Env>;
