// Builds, packs and, only when asked, publishes Javelin's npm packages (docs/releases.md).
//
//   bun tools/release/release.ts build --version 0.1.0-alpha.2 [--targets T,T] [--out DIR]
//   bun tools/release/release.ts serve [--dir DIR] [--port N]
//   bun tools/release/release.ts publish --tag TAG [--dir DIR] [--dry-run]
//
// `build` compiles javelin and javelin-node for each target and packs, in DIR (default
// dist/npm), one tarball per platform package (@dhruv2mars/javelin-PLATFORM, holding both
// binaries) and one for the wrapper (@dhruv2mars/javelin, whose optionalDependencies pick the
// platform package and whose bin shim runs it), all at one version, plus manifest.json.
// `serve` is a loopback npm registry for those tarballs, so npm, bun and install.sh can be
// tried against a build. `publish` runs `npm publish` on the tarballs, platform packages
// first, with --dry-run printing what would publish; nothing leaves this machine otherwise.
import { createHash } from "node:crypto";
import { chmod, cp, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { basename, join, resolve } from "node:path";

const ROOT = resolve(import.meta.dir, "../..");
const SCOPE = "@dhruv2mars";
const WRAPPER = `${SCOPE}/javelin`;
const TOOLCHAIN = "1.99.0";
const GLIBC = "2.28";
const BINARIES = ["javelin", "javelin-node"];
const REGISTRY = "https://registry.npmjs.org";

// `held` targets build only when named with --targets and are not part of a release: the
// owner held Windows back from the first publish on 2026-10-29.
type Target = { platform: string; os: string; cpu: string; zig: boolean; experimental?: boolean; held?: boolean };
const TARGETS: Record<string, Target> = {
  "aarch64-apple-darwin": { platform: "darwin-arm64", os: "darwin", cpu: "arm64", zig: false },
  "x86_64-apple-darwin": { platform: "darwin-x64", os: "darwin", cpu: "x64", zig: false },
  "x86_64-unknown-linux-gnu": { platform: "linux-x64", os: "linux", cpu: "x64", zig: true },
  "aarch64-unknown-linux-gnu": { platform: "linux-arm64", os: "linux", cpu: "arm64", zig: true },
  "x86_64-pc-windows-gnu": { platform: "win32-x64", os: "win32", cpu: "x64", zig: true, experimental: true, held: true },
};

class Fail extends Error {}

function parse(argv: string[]): Map<string, string> {
  const flags = new Map<string, string>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (!a.startsWith("--")) throw new Fail(`unexpected argument ${a}`);
    const key = a.slice(2);
    if (["dry-run", "allow-dirty"].includes(key)) flags.set(key, "true");
    else {
      const v = argv[++i];
      if (v === undefined) throw new Fail(`${a} needs a value`);
      flags.set(key, v);
    }
  }
  return flags;
}

async function run(cmd: string[], opts: { env?: Record<string, string>; cwd?: string; quiet?: boolean } = {}): Promise<string> {
  const proc = Bun.spawn(cmd, {
    cwd: opts.cwd ?? ROOT,
    env: { ...process.env, ...opts.env },
    stdout: "pipe",
    stderr: opts.quiet ? "pipe" : "inherit",
  });
  const out = await new Response(proc.stdout).text();
  if ((await proc.exited) !== 0) throw new Fail(`${cmd.join(" ")} failed`);
  return out;
}

const digest = async (algorithm: string, path: string, encoding: "hex" | "base64") =>
  createHash(algorithm).update(await readFile(path)).digest(encoding);
const integrity = async (path: string) => `sha512-${await digest("sha512", path, "base64")}`;

/** 0.1.0 or 0.1.0-alpha.2, whose number must be the CLI's package version. */
async function checkVersion(version: string | undefined): Promise<string> {
  if (!version || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(version)) {
    throw new Fail("--version must look like 0.1.0 or 0.1.0-alpha.2");
  }
  const toml = await readFile(join(ROOT, "crates/javelin-cli/Cargo.toml"), "utf8");
  const pkg = /^version = "(.+)"$/m.exec(toml)?.[1];
  if (version.split("-")[0] !== pkg) throw new Fail(`${version} does not match javelin-cli's version ${pkg}`);
  return pkg;
}

const common = (version: string) => ({
  version,
  license: "MIT",
  homepage: "https://javelin-codes.vercel.app",
  repository: "https://entire.com/et/dhruv2mars/javelin",
});

async function build(flags: Map<string, string>): Promise<void> {
  const version = flags.get("version")!;
  const pkgVersion = await checkVersion(version);
  const out = resolve(flags.get("out") ?? join(ROOT, "dist/npm"));
  const darwin = process.platform === "darwin";
  const names = flags.get("targets")?.split(",") ?? Object.keys(TARGETS).filter((t) => !TARGETS[t]!.held && (darwin || !t.endsWith("darwin")));
  for (const t of names) {
    if (!TARGETS[t]) throw new Fail(`unknown target ${t}; known: ${Object.keys(TARGETS).join(", ")}`);
    if (t.endsWith("darwin") && !darwin) throw new Fail(`${t} builds only on macOS`);
  }

  const commit = (await run(["git", "rev-parse", "HEAD"], { quiet: true })).trim();
  const dirty = (await run(["git", "status", "--porcelain"], { quiet: true })).trim() !== "";
  if (dirty && !flags.has("allow-dirty")) throw new Fail("the worktree has uncommitted changes; commit them or pass --allow-dirty");
  const epoch = (await run(["git", "log", "-1", "--format=%ct"], { quiet: true })).trim();
  const rustc = (await run(["rustc", "--version"], { env: { RUSTUP_TOOLCHAIN: TOOLCHAIN }, quiet: true })).trim();
  const zig = names.some((t) => TARGETS[t]!.zig) ? (await run(["zig", "version"], { quiet: true })).trim() : undefined;
  const host = `${process.platform}-${process.arch}`;

  await rm(out, { recursive: true, force: true });
  const staging = join(out, "packages");
  await mkdir(staging, { recursive: true });
  const license = join(ROOT, "LICENSE");

  // Packs a staged package directory into DIR and returns its file name.
  const pack = async (dir: string) => {
    const before = new Set(await readdir(out));
    await run(["bun", "pm", "pack", "--destination", out, "--quiet"], { cwd: dir, quiet: true });
    const made = (await readdir(out)).filter((f) => f.endsWith(".tgz") && !before.has(f));
    if (made.length !== 1) throw new Fail(`bun pm pack in ${dir} made ${made.length} tarballs`);
    return made[0]!;
  };

  const packages = [];
  for (const t of names) {
    const target = TARGETS[t]!;
    const name = `${SCOPE}/javelin-${target.platform}`;
    console.log(`\n== ${t} -> ${name}`);
    const env: Record<string, string> = {
      RUSTUP_TOOLCHAIN: TOOLCHAIN,
      SOURCE_DATE_EPOCH: epoch,
      CARGO_TARGET_DIR: join(ROOT, "target/release-dist"),
      // Keep the build machine's paths out of the binaries.
      RUSTFLAGS: `--remap-path-prefix=${ROOT}=/javelin --remap-path-prefix=${process.env.CARGO_HOME ?? join(process.env.HOME ?? "", ".cargo")}=/cargo`,
    };
    const cargoTarget = target.zig && target.os === "linux" ? `${t}.${GLIBC}` : t;
    await run(
      ["cargo", target.zig ? "zigbuild" : "build", "--release", "--locked", "--target", cargoTarget,
        "-p", "javelin-cli", "-p", "javelin-node", "--bin", "javelin", "--bin", "javelin-node"],
      { env },
    );

    const exe = target.os === "win32" ? ".exe" : "";
    const dir = join(staging, `javelin-${target.platform}`);
    await mkdir(join(dir, "bin"), { recursive: true });
    const binaries: Record<string, string> = {};
    for (const b of BINARIES) {
      const dest = join(dir, "bin", b + exe);
      await cp(join(ROOT, "target/release-dist", t, "release", b + exe), dest);
      await chmod(dest, 0o755);
      binaries[b] = await digest("sha256", dest, "hex");
    }
    await cp(license, join(dir, "LICENSE"));
    const requires = target.os === "linux" ? `glibc ${GLIBC} or newer` : target.os === "darwin" ? "macOS 11 or newer" : "Windows 10 or newer";
    await writeFile(
      join(dir, "README.md"),
      `# ${name}\n\nThe \`javelin\` and \`javelin-node\` binaries for ${target.platform} (${requires})` +
        `${target.experimental ? ", experimental and untested" : ""}. Install \`${WRAPPER}\` instead: it picks this package.\n`,
    );
    await writeFile(
      join(dir, "package.json"),
      JSON.stringify(
        {
          name,
          ...common(version),
          description: `Javelin's native binaries for ${target.platform}${target.experimental ? " (experimental)" : ""}`,
          os: [target.os],
          cpu: [target.cpu],
          ...(target.os === "linux" ? { libc: ["glibc"] } : {}),
          files: ["bin", "README.md", "LICENSE"],
          preferUnplugged: true,
        },
        null,
        2,
      ) + "\n",
    );

    // A binary for this machine must answer --version before it is packed.
    let checked = "not run on the build machine";
    if (host === `${target.os}-${target.cpu}`) {
      for (const b of BINARIES) {
        const got = (await run([join(dir, "bin", b), "--version"], { quiet: true })).trim();
        if (got !== `${b} ${pkgVersion}`) throw new Fail(`${b} --version printed ${got}, expected ${b} ${pkgVersion}`);
      }
      checked = "--version ran on the build machine";
    }
    const file = await pack(dir);
    const path = join(out, file);
    packages.push({
      name,
      target: t,
      file,
      integrity: await integrity(path),
      size: (await stat(path)).size,
      binaries,
      built_on: `${host} with ${target.zig ? `cargo-zigbuild, zig ${zig}` : "cargo"}`,
      requires,
      experimental: !!target.experimental,
      checked,
    });
    console.log(`${file}  ${(await stat(path)).size} bytes  ${checked}`);
  }

  // The wrapper names exactly the platform packages built here, at the same version.
  const wrapper = join(staging, "javelin");
  await mkdir(join(wrapper, "bin"), { recursive: true });
  for (const b of BINARIES) {
    await cp(join(import.meta.dir, "npm/shim.sh"), join(wrapper, "bin", b));
    await chmod(join(wrapper, "bin", b), 0o755);
  }
  await cp(license, join(wrapper, "LICENSE"));
  await cp(join(import.meta.dir, "npm/README.md"), join(wrapper, "README.md"));
  await cp(join(import.meta.dir, "npm/install.js"), join(wrapper, "install.js"));
  await writeFile(
    join(wrapper, "package.json"),
    JSON.stringify(
      {
        name: WRAPPER,
        ...common(version),
        description: "Javelin: version control for code that people and agents write together",
        bin: Object.fromEntries(BINARIES.map((b) => [b, `bin/${b}`])),
        // Swaps the bin shims for the native binaries where scripts run (npm); see install.js.
        scripts: { postinstall: "node install.js" },
        files: ["bin", "install.js", "README.md", "LICENSE"],
        optionalDependencies: Object.fromEntries(packages.map((p) => [p.name, version])),
      },
      null,
      2,
    ) + "\n",
  );
  const file = await pack(wrapper);
  packages.push({ name: WRAPPER, file, integrity: await integrity(join(out, file)), size: (await stat(join(out, file))).size });

  const manifest = { schema: 1, version, source_commit: commit, source_dirty: dirty, source_date_epoch: Number(epoch), toolchain: rustc, packages };
  await writeFile(join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`\npacked ${version} in ${out}:`);
  for (const p of packages) console.log(`  ${p.file}  ${p.integrity}`);
}

type Packed = { name: string; file: string; integrity: string; target?: string };

async function readManifest(dir: string): Promise<{ version: string; packages: Packed[] }> {
  return JSON.parse(await readFile(join(dir, "manifest.json"), "utf8"));
}

async function serve(flags: Map<string, string>): Promise<void> {
  const dir = resolve(flags.get("dir") ?? join(ROOT, "dist/npm"));
  const port = Number(flags.get("port") ?? 4873);
  const base = `http://127.0.0.1:${port}`;
  const { version, packages } = await readManifest(dir);
  // One packument per package: its package.json plus `dist`, as registry.npmjs.org has it.
  const docs = new Map<string, Record<string, unknown>>();
  const files = new Map<string, string>();
  for (const p of packages) {
    const path = join(dir, p.file);
    const json = JSON.parse(await run(["tar", "-xzOf", path, "package/package.json"], { quiet: true }));
    const tarball = `${base}/${p.name}/-/${basename(p.name)}-${version}.tgz`;
    files.set(new URL(tarball).pathname, path);
    const dist = { tarball, integrity: await integrity(path), shasum: await digest("sha1", path, "hex") };
    docs.set(p.name, { name: p.name, "dist-tags": { latest: version }, versions: { [version]: { ...json, dist } } });
  }
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port,
    async fetch(req) {
      const path = decodeURIComponent(new URL(req.url).pathname);
      let res: Response;
      const doc = (name: string) => docs.get(name);
      const m = /^\/(@[^/]+\/[^/]+)(?:\/([^/]+))?$/.exec(path);
      if (path === "/install.sh" || path === "/install.ps1") {
        res = new Response(Bun.file(join(ROOT, "scripts", path.slice(1))), { headers: { "content-type": "text/plain; charset=utf-8" } });
      } else if (files.has(path)) {
        res = new Response(Bun.file(files.get(path)!), { headers: { "content-type": "application/octet-stream" } });
      } else if (m && doc(m[1]!) && !m[2]) {
        res = Response.json(doc(m[1]!));
      } else if (m && doc(m[1]!) && m[2]) {
        const d = doc(m[1]!) as { "dist-tags": Record<string, string>; versions: Record<string, unknown> };
        const v = d.versions[d["dist-tags"][m[2]] ?? m[2]];
        res = v ? Response.json(v) : new Response("not found\n", { status: 404 });
      } else {
        res = new Response("not found\n", { status: 404 });
      }
      console.log(`${req.method} ${path} ${res.status}`);
      return res;
    },
  });
  console.log(`npm registry for ${version} (${packages.length} packages) on ${server.url}`);
}

async function publish(flags: Map<string, string>): Promise<void> {
  const dir = resolve(flags.get("dir") ?? join(ROOT, "dist/npm"));
  const tag = flags.get("tag");
  const dry = flags.has("dry-run");
  if (!tag || !/^[a-z][a-z0-9-]*$/.test(tag)) throw new Fail("--tag is required, such as latest or next");
  const { version, packages } = await readManifest(dir);
  await checkVersion(version);

  // Every platform package and the wrapper, unchanged since they were packed.
  const want = [...Object.values(TARGETS).filter((t) => !t.held).map((t) => `${SCOPE}/javelin-${t.platform}`), WRAPPER];
  const names = packages.map((p) => p.name);
  if (want.some((n) => !names.includes(n)) || names.length !== want.length) {
    throw new Fail(`a release needs all of ${want.join(", ")}; ${dir} has ${names.join(", ")}`);
  }
  for (const p of packages) {
    if ((await integrity(join(dir, p.file))) !== p.integrity) throw new Fail(`${p.file} changed since it was packed`);
  }
  const npm = (args: string[], quiet = false) => run(["npm", ...args, "--registry", REGISTRY], { quiet });
  if (!dry) {
    const who = await npm(["whoami"], true).catch(() => {
      throw new Fail("npm is not logged in: run `npm login`, or set NODE_AUTH_TOKEN with an .npmrc that uses it");
    });
    console.log(`publishing as ${who.trim()}`);
  }

  // Platform packages first, so the wrapper never names a version that is not there yet.
  for (const p of [...packages.filter((p) => p.name !== WRAPPER), packages.find((p) => p.name === WRAPPER)!]) {
    if (!dry) {
      // A rerun after a partial publish skips what is already up, if it is these bytes.
      const existing = (await npm(["view", `${p.name}@${version}`, "dist.integrity"], true).catch(() => "")).trim();
      if (existing === p.integrity) {
        console.log(`${p.name}@${version} is already published; skipping`);
        continue;
      }
      if (existing) throw new Fail(`${p.name}@${version} is published with different contents (${existing})`);
    }
    console.log((await npm(["publish", join(dir, p.file), "--access", "public", "--tag", tag, ...(dry ? ["--dry-run"] : [])])).trim());
  }
  console.log(`${dry ? "dry run: nothing published" : "published"}: ${packages.length} packages at ${version}, tag ${tag}`);
}

const [command, ...rest] = Bun.argv.slice(2);
try {
  const flags = parse(rest);
  if (command === "build") await build(flags);
  else if (command === "serve") await serve(flags);
  else if (command === "publish") await publish(flags);
  else throw new Fail(`usage: release.ts build|serve|publish (see the header of ${basename(import.meta.path)})`);
} catch (e) {
  if (!(e instanceof Fail)) throw e;
  console.error(`release.ts: ${e.message}`);
  process.exit(1);
}
