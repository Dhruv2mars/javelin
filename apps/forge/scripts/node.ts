// A real self-hosted node for tests and local development: builds `javelin` and
// `javelin-node` when needed, seeds a small repository with the `javelin` CLI, and serves
// it on loopback. Nothing here invokes Git.
import { chmod, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "../../..");
const windows = process.platform === "win32";
const exe = windows ? ".exe" : "";

export const DEMO_REPOSITORY = "repo_forge_demo";

async function run(cmd: string[], cwd = root): Promise<string> {
  const proc = Bun.spawn(cmd, { cwd, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  if (code !== 0) throw new Error(`${cmd.join(" ")} exited ${code}\n${stderr}${stdout}`);
  return stdout;
}

/** Paths of the `javelin` and `javelin-node` binaries, from `JAVELIN_BIN_DIR` or a cargo build. */
export async function binaries(): Promise<{ javelin: string; node: string }> {
  const dir = process.env.JAVELIN_BIN_DIR;
  if (dir) return { javelin: join(dir, `javelin${exe}`), node: join(dir, `javelin-node${exe}`) };
  await run(["cargo", "build", "--locked", "-q", "-p", "javelin-cli", "-p", "javelin-node"]);
  const target = join(root, "target", "debug");
  return { javelin: join(target, `javelin${exe}`), node: join(target, `javelin-node${exe}`) };
}

// A tiny PNG: signature, then an IHDR chunk. Enough to be binary.
const PNG = Uint8Array.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
  0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
]);

export const DEMO = {
  readme: `# Lantern

Lantern turns a folder of Markdown notes into a small static site. It reads every \`.md\`
file under \`notes/\`, renders it, and writes one page per note.

## Use it

\`\`\`sh
cargo run -- build notes/ --out site/
\`\`\`

- \`build\` renders every note once.
- \`watch\` rebuilds a note when it changes.

See [the usage guide](docs/usage.md) and [the entry point](src/main.rs).
`,
  cargo: `[package]
name = "lantern"
version = "0.1.0"
edition = "2024"
`,
  main: `use lantern::render;

/// Renders one note from standard input to standard output.
fn main() {
    let input = std::io::read_to_string(std::io::stdin()).expect("read stdin");
    print!("{}", render(&input));
}
`,
  lib: `/// Renders a note's title line as a heading and keeps the rest as paragraphs.
pub fn render(note: &str) -> String {
    let mut out = String::new();
    for line in note.lines() {
        if let Some(title) = line.strip_prefix("# ") {
            out.push_str(&format!("<h1>{title}</h1>\\n"));
        } else if !line.is_empty() {
            out.push_str(&format!("<p>{line}</p>\\n"));
        }
    }
    out
}
`,
  usage: "# Usage\n\nRun `lantern build` from the folder that holds `notes/`.\n",
  script: "#!/bin/sh\nset -eu\ncargo run -- build notes/ --out site/\n",
  png: PNG,
};

/** Seeds `dir` with four published Worlds from three people-and-agent Layers. */
export async function seedDemo(javelin: string, dir: string): Promise<void> {
  const j = (...args: string[]) => run([javelin, ...args, "--repo", dir], dir);
  const layer = async (id: string) => {
    await j("workspace", "create", id);
    return JSON.parse(await j("workspace", "path", id, "--json")).workspace.root as string;
  };
  const put = async (base: string, path: string, data: string | Uint8Array) => {
    await mkdir(join(base, path, ".."), { recursive: true });
    await writeFile(join(base, path), data);
  };
  await run([javelin, "init", dir], dir);

  // World 1: a person scaffolds the project.
  let w = await layer("scaffold");
  await put(w, "README.md", DEMO.readme);
  await put(w, "Cargo.toml", DEMO.cargo);
  await put(w, "src/main.rs", DEMO.main);
  await put(w, "src/lib.rs", DEMO.lib);
  await put(w, "docs/usage.md", DEMO.usage);
  await put(w, "scripts/build.sh", DEMO.script);
  if (!windows) await chmod(join(w, "scripts/build.sh"), 0o755);
  await j("workspace", "publish", "scaffold", "--operation", "seed-scaffold");

  // World 2: an agent adds lists, without a trace.
  w = await layer("lists");
  await put(w, "src/lib.rs", DEMO.lib.replace(
    `        } else if !line.is_empty() {`,
    `        } else if let Some(item) = line.strip_prefix("- ") {
            out.push_str(&format!("<li>{item}</li>\\n"));
        } else if !line.is_empty() {`,
  ));
  await put(w, "tests/render.rs", `#[test]\nfn renders_a_title() {\n    assert_eq!(lantern::render("# Hi"), "<h1>Hi</h1>\\n");\n}\n`);
  await j("workspace", "publish", "lists", "--operation", "seed-lists", "--actor", "agent", "--allow-missing-trace");

  // World 3: an agent rewrites the docs, with a trace kept outside the Layer.
  w = await layer("docs-guide");
  const trace = join(dir, "..", "docs-guide.trace.json");
  await writeFile(trace, JSON.stringify({ agent: "claude-code", steps: 3 }));
  await put(w, "README.md", DEMO.readme.replace("[the usage guide](docs/usage.md)", "[the guide](docs/guide.md)"));
  await put(w, "docs/guide.md", "# Guide\n\nLantern reads `notes/` and writes `site/`.\n\n## Watch mode\n\nRun `lantern watch` to rebuild on save.\n");
  await Bun.file(join(w, "docs/usage.md")).delete();
  await j("workspace", "publish", "docs-guide", "--operation", "seed-docs", "--actor", "agent", "--trace", trace);

  // World 4: a person adds an icon.
  w = await layer("icon");
  await put(w, "assets/icon.png", DEMO.png);
  await j("workspace", "publish", "icon", "--operation", "seed-icon");
}

export async function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as { port: number }).port;
      server.close(() => done(port));
    });
  });
}

export type RunningNode = { url: string; repository: string; dir: string; stop: () => Promise<void> };

/** Serves `dir` as `repository` with `javelin-node serve` on a free loopback port. */
export async function startNode(node: string, dir: string, repository = DEMO_REPOSITORY, port?: number): Promise<RunningNode> {
  const at = port ?? (await freePort());
  const proc = Bun.spawn([node, "serve", dir, repository, `127.0.0.1:${at}`], { stdout: "ignore", stderr: "pipe" });
  const url = `http://127.0.0.1:${at}`;
  const resolveHead = Uint8Array.from([0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
  for (let attempt = 0; ; attempt++) {
    try {
      const reply = await fetch(`${url}/v2/r/${repository}`, { method: "POST", body: resolveHead });
      if (reply.ok) break;
    } catch {
      // not listening yet
    }
    if (proc.exitCode !== null || attempt > 200) {
      proc.kill();
      throw new Error(`javelin-node did not start: ${await new Response(proc.stderr).text()}`);
    }
    await Bun.sleep(50);
  }
  return {
    url,
    repository,
    dir,
    stop: async () => {
      proc.kill();
      await proc.exited;
    },
  };
}

/** A fresh scratch directory for one seeded repository. */
export async function scratch(): Promise<string> {
  const base = await mkdtemp(join(tmpdir(), "javelin-forge-"));
  const dir = join(base, "repo");
  await mkdir(dir);
  return dir;
}
