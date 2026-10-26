// `bun run probe`: the Free-plan measurements in ../README.md, on the logged-in account.
//   1. deploys `javelin-free-probe` (probe/worker.ts) with a fresh random PROBE_TOKEN secret;
//   2. CPU: burns in the Worker, then in its Durable Object, and records each status;
//   3. storage: fills one object with zero BLOBs until SQLITE_FULL or PROBE_FILL_BYTES
//      (default 1.1 GB, under the Free plan's 5 GB account total), then deletes it all;
//   4. deletes the probe Worker and its Durable Object namespace, always.
// It uses a few dozen requests and leaves nothing behind. Results go to probe/evidence/.
import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const here = resolve(import.meta.dir, "..");
const config = join(here, "probe/wrangler.jsonc");
const wrangler = join(here, "node_modules/.bin/wrangler");
const fillLimit = Number(process.env.PROBE_FILL_BYTES ?? 1_100_000_000);
const token = Array.from(crypto.getRandomValues(new Uint8Array(32)), (b) => b.toString(16).padStart(2, "0")).join("");
const results: Record<string, unknown> = { date: new Date().toISOString() };

async function sh(args: string[], stdin?: string): Promise<string> {
  const proc = Bun.spawn([wrangler, ...args], { cwd: here, stdin: stdin ? new TextEncoder().encode(stdin) : "ignore", stdout: "pipe", stderr: "pipe" });
  const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()]);
  if ((await proc.exited) !== 0) throw new Error(`wrangler ${args[0]} failed:\n${out}\n${err}`);
  return out;
}

let base = "";
async function call(path: string): Promise<{ status: number; body: string }> {
  const response = await fetch(`${base}${path}`, { headers: { authorization: `Bearer ${token}` } });
  return { status: response.status, body: (await response.text()).slice(0, 200) };
}

try {
  const deployed = await sh(["deploy", "-c", config]);
  base = /https:\/\/\S+\.workers\.dev/.exec(deployed)?.[0] ?? "";
  if (!base) throw new Error(`no workers.dev URL in:\n${deployed}`);
  await sh(["secret", "put", "PROBE_TOKEN", "-c", config], token);
  for (let i = 0; i < 60 && (await call("/worker/burn?n=1")).status !== 200; i++) await Bun.sleep(1000);

  // About 4 ns per iteration on current hardware: 1e8 is several hundred ms of CPU, 2e9
  // several seconds. Exact CPU per request is in the account's Workers analytics.
  const cpu: { where: string; n: number; status: number }[] = [];
  for (const [where, n, times] of [["worker", 1e4, 1], ["worker", 1e8, 3], ["worker", 1e9, 2], ["object", 1e4, 1], ["object", 2e9, 2]] as const) {
    for (let i = 0; i < times; i++) {
      const path = where === "worker" ? `/worker/burn?n=${n}` : `/object/cpu/burn?n=${n}`;
      const { status } = await call(path);
      cpu.push({ where, n, status });
      console.log(`cpu ${where} n=${n} -> ${status}`);
    }
  }
  results.cpu = cpu;

  const fill: unknown[] = [];
  for (let i = 0; i < 100; i++) {
    const reply = JSON.parse((await call("/object/store/fill?rows=50&bytes=1000000")).body);
    fill.push(reply);
    console.log(`fill ${JSON.stringify(reply)}`);
    if (reply.full || reply.bytes >= fillLimit) break;
  }
  results.fill = fill;
  results.cleared = (await call("/object/store/clear")).body;
} finally {
  try {
    await sh(["delete", "-c", config, "--force"]);
    console.log("probe Worker deleted");
  } catch (error) {
    console.error(`DELETE THE PROBE BY HAND: ${error}`);
  }
  mkdirSync(join(here, "probe/evidence"), { recursive: true });
  writeFileSync(join(here, "probe/evidence/results.json"), JSON.stringify(results, null, 2));
}
