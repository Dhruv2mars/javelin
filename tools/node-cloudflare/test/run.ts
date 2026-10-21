// `bun run test`: the managed node's checks on workerd, end to end.
//   1. start `wrangler dev` with fresh state and a random admin token (it is the only child
//      this script kills);
//   2. run the Rust conformance suite against it (`crates/javelin-node-worker/tests/workerd.rs`);
//   3. build the `javelin` CLI and run `repo create --provider cloudflare` end to end: a crash
//      before provisioning, then a retry with the same operation ID, then an idempotent repeat.
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const here = resolve(import.meta.dir, "..");
const repoRoot = resolve(here, "../..");
const port = Number(process.env.NODE_PORT ?? 18789);
const base = `http://127.0.0.1:${port}`;
const adminToken = `dev-${crypto.randomUUID()}`;
const scratch = await mkdtemp(join(tmpdir(), "javelin-node-cf-"));

type Output = { stdout: string; stderr: string; code: number };

async function run(cmd: string[], cwd: string, env: Record<string, string> = {}): Promise<Output> {
  const proc = Bun.spawn(cmd, { cwd, env: { ...process.env, ...env }, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ]);
  return { stdout, stderr, code: await proc.exited };
}

function check(condition: boolean, what: string) {
  if (!condition) throw new Error(`check failed: ${what}`);
  console.log(`ok  ${what}`);
}

async function waitForNode(): Promise<void> {
  for (let attempt = 0; attempt < 120; attempt++) {
    try {
      if ((await fetch(`${base}/healthz`)).ok) return;
    } catch {
      // not listening yet
    }
    await Bun.sleep(500);
  }
  throw new Error(`wrangler dev did not answer on ${base}`);
}

async function nodeState(repository: string) {
  const response = await fetch(`${base}/admin/r/${repository}/state`, {
    headers: { authorization: `Bearer ${adminToken}` },
  });
  return { status: response.status, body: await response.json() as any };
}

const wrangler = Bun.spawn(
  ["bunx", "wrangler", "dev", "--port", String(port), "--persist-to", join(scratch, "state"),
    "--var", `NODE_ADMIN_TOKEN:${adminToken}`],
  { cwd: here, stdout: "pipe", stderr: "pipe" },
);

try {
  await waitForNode();
  console.log(`node on ${base}`);

  // 1. Rust conformance suite against workerd.
  const suite = await run(
    ["cargo", "test", "-p", "javelin-node-worker", "--test", "workerd", "--", "--ignored", "--test-threads=1"],
    repoRoot,
    { JAVELIN_NODE_URL: base, JAVELIN_NODE_ADMIN_TOKEN: adminToken },
  );
  if (suite.code !== 0) throw new Error(`conformance failed:\n${suite.stdout}\n${suite.stderr}`);
  check(/2 passed/.test(suite.stdout), "Rust conformance suite passes against workerd");

  // 2. `repo create` end to end with the javelin CLI.
  const build = await run(["cargo", "build", "-p", "javelin-cli"], repoRoot);
  if (build.code !== 0) throw new Error(build.stderr);
  const javelin = join(repoRoot, "target/debug/javelin");
  const env = {
    JAVELIN_HOME: join(scratch, "home"),
    JAVELIN_CONTROL_DIR: join(scratch, "control"),
    JAVELIN_ADMIN_TOKEN: "",
    JAVELIN_NODE_ADMIN_TOKEN: adminToken,
  };
  const init = await run([javelin, "--json", "control", "init"], here, env);
  check(init.code === 0, "control init");
  env.JAVELIN_ADMIN_TOKEN = JSON.parse(init.stdout).admin_token;

  const create = (operation: string, node: string) =>
    run(
      [javelin, "--json", "repo", "create", "acme/app", "--operation", operation,
        "--provider", "cloudflare", "--node-url", node],
      here,
      env,
    );

  // A crash before provisioning: the node is unreachable, so the repository stays Provisioning.
  const crashed = await create("op-create-1", "http://127.0.0.1:1");
  const crashedJson = JSON.parse(crashed.stderr);
  check(crashed.code !== 0 && crashedJson.error.code === "node_unavailable", "unreachable node refuses create");

  // The retry with the same operation ID resumes and completes.
  const retry = await create("op-create-1", base);
  check(retry.code === 0, "retry with the same operation completes");
  const created = JSON.parse(retry.stdout);
  check(created.provider === "cloudflare" && created.epoch === 1, "retry reports cloudflare epoch 1");
  const repository = created.repository as string;

  const state = await nodeState(repository);
  check(state.status === 200 && state.body.provisioned === true, "node is provisioned");
  check(/^[0-9a-f]{64}$/.test(state.body.genesis.id), "node holds a 64-hex World 0 id");
  check(state.body.permissions?.epoch === 1, "node holds applied permissions at epoch 1");
  check(state.body.permissions?.roles?.admin === "maintain", "creator holds maintain on the node");
  check(state.body.permissions?.public === false, "private visibility applied");

  // The same operation again returns the same repository and changes nothing.
  const repeat = await create("op-create-1", base);
  const repeated = JSON.parse(repeat.stdout);
  check(repeat.code === 0 && repeated.repository === repository && repeated.created === false,
    "repeat with the same operation returns the same repository");
  check((await nodeState(repository)).body.permissions?.epoch === 1, "repeat leaves permissions at epoch 1");

  // A different operation for the same name is refused.
  const taken = await create("op-create-2", base);
  check(taken.code !== 0 && JSON.parse(taken.stderr).error.code === "conflict", "name collision is refused");

  console.log("ALL CHECKS PASSED");
} finally {
  wrangler.kill();
  await wrangler.exited;
  await rm(scratch, { recursive: true, force: true });
}
