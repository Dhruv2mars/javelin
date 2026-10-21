// `bun run tools/acceptance/run.ts`: the hosted acceptance suite (U14), rerunnable.
//
//   1. Builds a failing `git` stand-in and proves it fires (positive control), then puts it first
//      on PATH for everything after.
//   2. Self-hosted target: `cargo test --test hosted_acceptance` (control plane, browser device
//      approval over HTTP, self-hosted node, proxies), then the `javelin` and `javelin-node`
//      binaries end to end.
//   3. Node target: the same suite's `target_*` tests against a node URL. By default this script
//      starts the Cloudflare Worker under `wrangler dev` (workerd) with fresh state; set
//      `JAVELIN_ACCEPTANCE_NODE_URL` and `JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN` (and optionally
//      `JAVELIN_ACCEPTANCE_TARGET`) to run against another node, such as a deployed Worker.
//   4. Product gaps: the ignored `gap_*` tests, reported NOT VERIFIED while they still fail.
//   5. Proves the stand-in's marker is absent, prints one table, and exits non-zero if any
//      expected assertion FAILED or is missing.
//
// It kills only the processes it started and removes only its own scratch directory.
import { chmod, mkdir, mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";

type Status = "VERIFIED" | "NOT VERIFIED" | "FAILED";
type Row = { target: string; id: string; status: Status; detail: string };
type Output = { stdout: string; stderr: string; code: number };

const root = resolve(import.meta.dir, "../..");
const windows = process.platform === "win32";
const exe = windows ? ".exe" : "";
const scratch = await mkdtemp(join(tmpdir(), "javelin-acceptance-"));
const report = join(scratch, "report.jsonl");
const rows: Row[] = [];
const children: Bun.Subprocess[] = [];

const deployed = process.env.JAVELIN_ACCEPTANCE_NODE_URL;
const nodeTarget = process.env.JAVELIN_ACCEPTANCE_TARGET ?? (deployed ? "deployed" : "cloudflare-workerd");

// What each target must report. A missing line means its test failed before reporting.
const expected: Record<string, string[]> = {
  git: ["git.positive-control", "git.marker-absent"],
  "self-hosted": [
    "flow.browser-device-approval", "flow.public-private", "flow.retries", "flow.permissions",
    "flow.wire-authentication", "flow.membership",
    "gate4.exact-revision-authorization", "gate4.immutable-prior-revisions",
    "gate4.lost-reply-one-outcome", "gate4.fork-data-without-credentials",
    "gate4.restricted-traces", "gate4.distinct-outage-outcomes",
    "outage.local-layer-work", "outage.control-plane-refusals", "outage.stack-node-down",
    "outage.review-node-down", "outage.coordinator-transactions",
  ],
  "self-hosted-cli": ["cli.repo-create", "cli.attach", "cli.attach-node-down"],
  [nodeTarget]: [
    "flow.browser-device-approval", "flow.public-private", "flow.retries", "flow.permissions",
    "outage.node-down-create", "gate4.lost-reply-one-outcome",
    "gate4.exact-revision-authorization", "gate4.immutable-prior-revisions",
    "gate4.fork-data-without-credentials", "gate4.restricted-traces",
    "gate4.distinct-outage-outcomes", "outage.coordinator-transactions",
  ],
  gaps: ["gap.repo-create-control-plane-unavailable", "gap.attach-node-5xx"],
};

function record(target: string, id: string, status: Status, detail: string) {
  rows.push({ target, id, status, detail });
}

async function run(cmd: string[], env: Record<string, string> = {}, cwd = root): Promise<Output> {
  const proc = Bun.spawn(cmd, { cwd, env: { ...process.env, ...env }, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ]);
  return { stdout, stderr, code: await proc.exited };
}

async function exists(path: string): Promise<boolean> {
  return stat(path).then(() => true, () => false);
}

async function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as { port: number }).port;
      server.close(() => done(port));
    });
  });
}

async function waitFor(url: string, what: string): Promise<void> {
  for (let attempt = 0; attempt < 240; attempt++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // not listening yet
    }
    await Bun.sleep(250);
  }
  throw new Error(`${what} did not answer on ${url}`);
}

function json(text: string): any {
  try {
    return JSON.parse(text.trim());
  } catch {
    return undefined;
  }
}

// --- 1. The failing `git` stand-in and its positive control. ---------------------------------
const shim = join(scratch, "git-shim");
const probe = join(scratch, "git-probe");
const marker = join(shim, "invoked");
await mkdir(shim);
await mkdir(probe);
await Bun.write(join(probe, "shim.rs"),
  'fn main() { let d = std::env::current_exe().unwrap(); std::fs::write(d.parent().unwrap().join("invoked"), b"").unwrap(); std::process::exit(97) }\n');
await Bun.write(join(probe, "probe.rs"),
  'fn main() { std::process::exit(std::process::Command::new("git").status().ok().and_then(|s| s.code()).unwrap_or(1)) }\n');
for (const [out, src] of [[join(shim, `git${exe}`), "shim.rs"], [join(probe, `probe${exe}`), "probe.rs"]]) {
  const built = await run(["rustc", "-o", out, join(probe, src)]);
  if (built.code !== 0) throw new Error(`rustc ${src}:\n${built.stderr}`);
  if (!windows) await chmod(out, 0o755);
}
const PATH = `${shim}${delimiter}${process.env.PATH ?? ""}`;
const fired = await run([join(probe, `probe${exe}`)], { PATH });
if (fired.code === 97 && (await exists(marker))) {
  record("git", "git.positive-control", "VERIFIED",
    "a program that runs `git` with the stand-in first on PATH exits 97 and leaves the marker");
} else {
  record("git", "git.positive-control", "FAILED", `probe exit ${fired.code}, marker ${await exists(marker)}`);
}
await rm(marker, { force: true });
// Every later step runs with the stand-in first on PATH.
process.env.PATH = PATH;

// Reads the report lines the Rust suite wrote, then truncates the file for the next step.
async function collect() {
  if (!(await exists(report))) return;
  for (const line of (await readFile(report, "utf8")).split("\n").filter(Boolean)) {
    const row = JSON.parse(line) as Row;
    record(row.target, row.id, row.status, row.detail);
  }
  await rm(report);
}

async function cargoTest(args: string[], env: Record<string, string>): Promise<Output> {
  const out = await run(
    ["cargo", "test", "--locked", "--test", "hosted_acceptance", ...args],
    { JAVELIN_ACCEPTANCE_REPORT: report, ...env },
  );
  await collect();
  if (out.code !== 0) {
    console.error(out.stdout.split("\n").filter((l) => /panicked|FAILED|left:|right:|error/.test(l)).join("\n"));
  }
  return out;
}

try {
  // --- 2. Self-hosted: the Rust suite, then the binaries. --------------------------------------
  const build = await run(["cargo", "build", "--locked", "-p", "javelin-cli", "-p", "javelin-node"]);
  if (build.code !== 0) throw new Error(`cargo build:\n${build.stderr}`);
  const targetDir = process.env.CARGO_TARGET_DIR ?? join(root, "target");
  const javelin = join(targetDir, "debug", `javelin${exe}`);
  const javelinNode = join(targetDir, "debug", `javelin-node${exe}`);

  await cargoTest(["--", "--test-threads=4"], {});

  const env: Record<string, string> = {
    JAVELIN_HOME: join(scratch, "home"),
    JAVELIN_CONTROL_DIR: join(scratch, "control"),
    JAVELIN_ADMIN_TOKEN: "",
  };
  const cli = (args: string[], cwd = scratch) => run([javelin, "--json", ...args], env, cwd);
  const init = await cli(["control", "init"]);
  env.JAVELIN_ADMIN_TOKEN = json(init.stdout)?.admin_token ?? "";
  const site = json((await cli(["repo", "create", "acme/site", "--operation", "op-site",
    "--visibility", "public", "--publish", "requests"])).stdout);
  const vault = json((await cli(["repo", "create", "acme/vault", "--operation", "op-vault",
    "--visibility", "private", "--publish", "direct"])).stdout);
  const retry = json((await cli(["repo", "create", "acme/vault", "--operation", "op-vault",
    "--visibility", "private", "--publish", "direct"])).stdout);
  const taken = await cli(["repo", "create", "acme/vault", "--operation", "op-vault-2"]);
  const created =
    site?.visibility === "public" && site?.epoch === 1 && vault?.visibility === "private" && vault?.epoch === 1 &&
    retry?.repository === vault?.repository && retry?.created === false &&
    taken.code !== 0 && json(taken.stderr)?.error?.code === "conflict";
  record("self-hosted-cli", "cli.repo-create", created ? "VERIFIED" : "FAILED", created
    ? "javelin control init, then repo create for a public and a private repository on the self-hosted provider; the retry returns the same repository (created=false); a collision is conflict"
    : `site=${JSON.stringify(site)} vault=${JSON.stringify(vault)} retry=${JSON.stringify(retry)} taken=${taken.stderr}`);

  if (site?.repository) {
    const port = await freePort();
    const nodeDir = join(env.JAVELIN_CONTROL_DIR, "nodes", site.repository);
    const node = Bun.spawn([javelinNode, "serve", nodeDir, "site", `127.0.0.1:${port}`],
      { stdout: "ignore", stderr: "pipe" });
    children.push(node);
    // The server writes one line once it listens.
    const reader = node.stderr.getReader();
    await reader.read();
    reader.releaseLock();
    const project = join(scratch, "project");
    await mkdir(project);
    const attached = await cli(["attach", `http://127.0.0.1:${port}/site`, project], project);
    const ok = attached.code === 0;
    record("self-hosted-cli", "cli.attach", ok ? "VERIFIED" : "FAILED", ok
      ? "javelin-node serves the repository the control plane provisioned; javelin attach links a project to it over loopback HTTP"
      : `attach exit ${attached.code}: ${attached.stderr}`);
    node.kill();
    await node.exited;
    const down = await cli(["attach", `http://127.0.0.1:${port}/site`, join(scratch, "project-2")]);
    const code = json(down.stderr)?.error?.code;
    record("self-hosted-cli", "cli.attach-node-down", code === "node_unavailable" ? "VERIFIED" : "FAILED",
      code === "node_unavailable"
        ? "with the node stopped, javelin attach fails with node_unavailable and writes nothing"
        : `expected node_unavailable, got ${down.stderr}`);
  }

  // --- 3. The node target: workerd by default, or the node URL given. ------------------------
  let url = deployed;
  let token = process.env.JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN ?? "";
  if (!url) {
    const worker = join(root, "tools/node-cloudflare");
    const wrangler = join(worker, "node_modules/.bin", windows ? "wrangler.cmd" : "wrangler");
    if (!(await exists(wrangler))) {
      const installed = await run(["bun", "install", "--frozen-lockfile"], {}, worker);
      if (installed.code !== 0) throw new Error(`bun install:\n${installed.stderr}`);
    }
    const port = await freePort();
    url = `http://127.0.0.1:${port}`;
    token = `dev-${crypto.randomUUID()}`;
    children.push(Bun.spawn(
      [wrangler, "dev", "--port", String(port), "--persist-to", join(scratch, "workerd"),
        "--var", `NODE_ADMIN_TOKEN:${token}`],
      // Inherited, so wrangler never blocks on a full pipe.
      { cwd: worker, stdout: "inherit", stderr: "inherit" },
    ));
    await waitFor(`${url}/healthz`, "wrangler dev");
  }
  await cargoTest(["target_", "--", "--ignored", "--test-threads=1"], {
    JAVELIN_ACCEPTANCE_NODE_URL: url,
    JAVELIN_ACCEPTANCE_NODE_ADMIN_TOKEN: token,
    JAVELIN_ACCEPTANCE_TARGET: nodeTarget,
  });
  if (!deployed) {
    record(nodeTarget, "deployed.free-plan", "NOT VERIFIED",
      "needs a deployed Free-plan Worker (wrangler deploy, NODE_ADMIN_TOKEN secret); out of scope: no account, login or token. Rerun with JAVELIN_ACCEPTANCE_NODE_URL set to it");
  }

  // --- 4. Product gaps: designed outcomes the product does not give yet. -----------------------
  for (const [id, test, what] of [
    ["gap.repo-create-control-plane-unavailable", "gap_repo_create_without_a_control_plane",
      "repo create with the control plane unreachable should be control_plane_unavailable; it is storage_error"],
    ["gap.attach-node-5xx", "gap_attach_to_a_failing_node",
      "a node answering HTTP 5xx should be node_unavailable; attach reports node_error"],
  ]) {
    const out = await run(["cargo", "test", "--locked", "--test", "hosted_acceptance", test, "--", "--ignored"],
      { JAVELIN_ACCEPTANCE_CLI: javelin });
    record("gaps", id, out.code === 0 ? "VERIFIED" : "NOT VERIFIED", out.code === 0
      ? `fixed: ${test} passes now; remove its #[ignore]`
      : `product gap recorded in TASKS.md (U14): ${what}`);
  }
} finally {
  for (const child of children) {
    child.kill();
    await child.exited;
  }
}

// --- 5. The stand-in never fired during product workflows. -----------------------------------
const fires = await exists(marker);
record("git", "git.marker-absent", fires ? "FAILED" : "VERIFIED", fires
  ? "the git stand-in was invoked during a product workflow"
  : "the stand-in's marker is absent after every step above ran with it first on PATH");

for (const [target, ids] of Object.entries(expected)) {
  for (const id of ids) {
    if (!rows.some((row) => row.target === target && row.id === id)) {
      record(target, id, "FAILED", "no result: its test failed before reporting");
    }
  }
}

const width = (key: keyof Row) => Math.max(...rows.map((row) => row[key].length), key.length);
const [t, i, s] = [width("target"), width("id"), width("status")];
console.log(`\n${"target".padEnd(t)}  ${"id".padEnd(i)}  ${"status".padEnd(s)}  detail`);
for (const row of rows) {
  console.log(`${row.target.padEnd(t)}  ${row.id.padEnd(i)}  ${row.status.padEnd(s)}  ${row.detail}`);
}
const failed = rows.filter((row) => row.status === "FAILED");
const notVerified = rows.filter((row) => row.status === "NOT VERIFIED");
console.log(`\n${rows.length - failed.length - notVerified.length} verified, ${notVerified.length} not verified, ${failed.length} failed`);
// Stored objects are read-only; make them writable so the scratch directory can go.
if (!windows) await run(["chmod", "-R", "u+rwX", scratch]);
await rm(scratch, { recursive: true, force: true }).catch((error) => console.error(`cleanup: ${error}`));
process.exit(failed.length === 0 ? 0 : 1);
