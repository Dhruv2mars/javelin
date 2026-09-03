import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const binary = process.env.JAVELIN_BIN;
if (!binary) throw new Error("set JAVELIN_BIN to the packaged javelin binary");

type CommandResult = { ms: number; stdout: string };

function run(args: string[], cwd?: string): CommandResult {
  const started = performance.now();
  const result = Bun.spawnSync([binary!, ...args], {
    cwd,
    stdout: "pipe",
    stderr: "pipe",
    env: process.env,
  });
  const ms = performance.now() - started;
  if (result.exitCode !== 0) {
    throw new Error(
      `${args.join(" ")} exited ${result.exitCode}: ${result.stderr.toString()}`,
    );
  }
  return { ms, stdout: result.stdout.toString().trim() };
}

async function writeFixture(root: string, files: number, totalBytes: number) {
  const bytesPerFile = Math.floor(totalBytes / files);
  const started = performance.now();
  for (let offset = 0; offset < files; offset += 128) {
    const batch = Array.from({ length: Math.min(128, files - offset) }, async (_, slot) => {
      const index = offset + slot;
      const directory = join(root, "fixture", String(Math.floor(index / 1000)).padStart(3, "0"));
      await mkdir(directory, { recursive: true });
      const bytes = new Uint8Array(bytesPerFile);
      new DataView(bytes.buffer).setUint32(0, index, false);
      await Bun.write(join(directory, `${String(index).padStart(6, "0")}.bin`), bytes);
    });
    await Promise.all(batch);
  }
  return { generation_ms: performance.now() - started, bytes_per_file: bytesPerFile };
}

async function treeProfile(name: string, files: number, bytes: number) {
  const root = await mkdtemp(join(tmpdir(), `javelin-benchmark-${name}-`));
  try {
    const generated = await writeFixture(root, files, bytes);
    const initialized = run(["init", root, "--json"]);
    const current = JSON.parse(run(["--project", root, "world", "current", "--json"]).stdout);
    const status = run(["--project", root, "status", "--json"]);
    const created = run(["--project", root, "layer", "create", "benchmark", "--from", "world", "--json"]);
    const fsck = run(["--project", root, "fsck", "--json"]);
    return {
      profile: name,
      files,
      requested_bytes: bytes,
      ...generated,
      init_ms: initialized.ms,
      status_ms: status.ms,
      layer_create_ms: created.ms,
      fsck_ms: fsck.ms,
      world_version: current.result.id,
      root_tree: current.result.root_tree,
      fsck: JSON.parse(fsck.stdout).result,
    };
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

async function eventProfile() {
  const root = await mkdtemp(join(tmpdir(), "javelin-benchmark-events-"));
  try {
    run(["init", root]);
    const created = JSON.parse(
      run(["--project", root, "layer", "create", "events", "--from", "world", "--json"]).stdout,
    );
    const view = created.result.path as string;
    const started = performance.now();
    for (let offset = 0; offset < 10_000; offset += 256) {
      await Promise.all(
        Array.from({ length: Math.min(256, 10_000 - offset) }, (_, slot) =>
          Bun.write(join(view, `event-${offset + slot}.txt`), `${offset + slot}\n`),
        ),
      );
    }
    const write_ms = performance.now() - started;
    const checkpoint = run(["--project", view, "checkpoint", "--reason", "10k-event-burst", "--json"]);
    const events = run(["--project", root, "events", "--since", "0", "--jsonl"]);
    run(["--project", root, "fsck"]);
    return {
      profile: "events",
      writes: 10_000,
      write_ms,
      checkpoint_ms: checkpoint.ms,
      emitted_javelin_events: events.stdout.split("\n").filter(Boolean).length,
    };
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

async function traceProfile() {
  const root = await mkdtemp(join(tmpdir(), "javelin-benchmark-traces-"));
  try {
    run(["init", root]);
    run(["--project", root, "layer", "create", "trace", "--from", "world"]);
    const session = run([
      "--project", root, "provenance", "begin", "--layer", "trace", "--actor", "benchmark",
    ]).stdout;
    const attachment = join(root, "trace-1gb.jsonl");
    const chunk = new Uint8Array(1024 * 1024);
    chunk.fill(32);
    chunk[chunk.length - 1] = 10;
    const writer = Bun.file(attachment).writer();
    for (let index = 0; index < 1024; index++) writer.write(chunk);
    await writer.end();
    const attached = run([
      "--project", root, "provenance", "attach", "--session", session, attachment,
      "--media-type", "application/jsonl", "--json",
    ]);
    run(["--project", root, "provenance", "end", session]);
    run(["--project", root, "fsck"]);
    return {
      profile: "traces",
      bytes: 1024 ** 3,
      attach_ms: attached.ms,
      attachment: JSON.parse(attached.stdout).result,
    };
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

const profile = process.argv[2];
const result =
  profile === "small" ? await treeProfile("small", 1_000, 50 * 1024 ** 2)
  : profile === "medium" ? await treeProfile("medium", 25_000, 1024 ** 3)
  : profile === "large" ? await treeProfile("large", 100_000, 5 * 1024 ** 3)
  : profile === "events" ? await eventProfile()
  : profile === "traces" ? await traceProfile()
  : null;

if (!result) throw new Error("profile must be small, medium, large, events, or traces");
console.log(JSON.stringify(result));
