// Builds, stages and, only when asked, publishes a Javelin release (docs/releases.md).
//
//   bun tools/release/release.ts build --version v0.1.0 [--targets T,T] [--out DIR] [--base-url URL]
//   bun tools/release/release.ts serve [--dir DIR] [--port N]
//   bun tools/release/release.ts publish --version v0.1.0 --public-url URL [--dir DIR]
//                                        [--bucket NAME] [--latest] [--dry-run]
//
// `build` compiles javelin and javelin-node for each target, packs one archive per target,
// and stages the host layout in DIR (default dist/release):
//
//   DIR/latest                       the staged version, one line
//   DIR/install.sh, DIR/install.ps1  the installers, stamped with --base-url
//   DIR/VERSION/                     archives, manifest.json, SHA256SUMS, the installers
//
// `serve` serves DIR on 127.0.0.1 so the installers can be tried against a staged release.
// `publish` uploads DIR/VERSION to a Cloudflare R2 bucket with wrangler and, with --latest,
// then points `latest` at it. Nothing is uploaded unless `publish` runs without --dry-run.
import { createHash } from "node:crypto";
import { chmod, cp, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { basename, join, resolve, sep } from "node:path";

const ROOT = resolve(import.meta.dir, "../..");
const TOOLCHAIN = "1.99.0";
const GLIBC = "2.28";
const WRANGLER = "wrangler@4.151.3";
const BINARIES = ["javelin", "javelin-node"];

type Target = { zig: boolean; windows: boolean; darwin: boolean };
const TARGETS: Record<string, Target> = {
  "aarch64-apple-darwin": { zig: false, windows: false, darwin: true },
  "x86_64-apple-darwin": { zig: false, windows: false, darwin: true },
  "x86_64-unknown-linux-gnu": { zig: true, windows: false, darwin: false },
  "aarch64-unknown-linux-gnu": { zig: true, windows: false, darwin: false },
  "x86_64-pc-windows-gnu": { zig: true, windows: true, darwin: false },
};

class Fail extends Error {}

function parse(argv: string[]): Map<string, string> {
  const flags = new Map<string, string>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (!a.startsWith("--")) throw new Fail(`unexpected argument ${a}`);
    const key = a.slice(2);
    if (["latest", "dry-run", "allow-dirty"].includes(key)) flags.set(key, "true");
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

const sha256 = async (path: string) => createHash("sha256").update(await readFile(path)).digest("hex");

/** v0.1.0 or v0.1.0-alpha.2, whose number must be the CLI's package version. */
async function checkVersion(version: string | undefined): Promise<string> {
  if (!version || !/^v\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(version)) {
    throw new Fail("--version must look like v0.1.0 or v0.1.0-alpha.2");
  }
  const toml = await readFile(join(ROOT, "crates/javelin-cli/Cargo.toml"), "utf8");
  const pkg = /^version = "(.+)"$/m.exec(toml)?.[1];
  if (version.slice(1).split("-")[0] !== pkg) throw new Fail(`${version} does not match javelin-cli's version ${pkg}`);
  return pkg;
}

/** Writes the installers with the release host stamped as their default. */
async function stampInstallers(baseUrl: string, dirs: string[]): Promise<void> {
  const sh = await readFile(join(ROOT, "scripts/install.sh"), "utf8");
  const ps1 = await readFile(join(ROOT, "scripts/install.ps1"), "utf8");
  const stampedSh = sh.replace(/^DEFAULT_RELEASE_BASE=.*$/m, `DEFAULT_RELEASE_BASE=${baseUrl}`);
  const stampedPs1 = ps1.replace(/^\$DefaultReleaseBase = .*$/m, `$DefaultReleaseBase = '${baseUrl}'`);
  if (stampedSh === sh && !sh.includes(`DEFAULT_RELEASE_BASE=${baseUrl}\n`)) throw new Fail("install.sh has no DEFAULT_RELEASE_BASE line");
  if (stampedPs1 === ps1 && !ps1.includes(`$DefaultReleaseBase = '${baseUrl}'`)) throw new Fail("install.ps1 has no $DefaultReleaseBase line");
  for (const dir of dirs) {
    await writeFile(join(dir, "install.sh"), stampedSh);
    await chmod(join(dir, "install.sh"), 0o755);
    await writeFile(join(dir, "install.ps1"), stampedPs1);
  }
}

async function build(flags: Map<string, string>): Promise<void> {
  const version = flags.get("version")!;
  const pkg = await checkVersion(version);
  const out = resolve(flags.get("out") ?? join(ROOT, "dist/release"));
  const baseUrl = (flags.get("base-url") ?? "").replace(/\/+$/, "");
  if (baseUrl && !/^(https:\/\/|http:\/\/(127\.0\.0\.1|localhost)[:/])/.test(baseUrl + "/")) {
    throw new Fail("--base-url must be https, or http on 127.0.0.1 or localhost");
  }
  const darwin = process.platform === "darwin";
  const names = flags.get("targets")?.split(",") ?? Object.keys(TARGETS).filter((t) => darwin || !TARGETS[t]!.darwin);
  for (const t of names) {
    if (!TARGETS[t]) throw new Fail(`unknown target ${t}; known: ${Object.keys(TARGETS).join(", ")}`);
    if (TARGETS[t]!.darwin && !darwin) throw new Fail(`${t} builds only on macOS`);
  }

  const commit = (await run(["git", "rev-parse", "HEAD"], { quiet: true })).trim();
  const dirty = (await run(["git", "status", "--porcelain"], { quiet: true })).trim() !== "";
  if (dirty && !flags.has("allow-dirty")) throw new Fail("the worktree has uncommitted changes; commit them or pass --allow-dirty");
  const epoch = (await run(["git", "log", "-1", "--format=%ct"], { quiet: true })).trim();
  const rustc = (await run(["rustc", "--version"], { env: { RUSTUP_TOOLCHAIN: TOOLCHAIN }, quiet: true })).trim();
  const zig = names.some((t) => TARGETS[t]!.zig) ? (await run(["zig", "version"], { quiet: true })).trim() : undefined;
  const host = `${process.platform}-${process.arch}`;
  const hostTarget = { "darwin-arm64": "aarch64-apple-darwin", "darwin-x64": "x86_64-apple-darwin", "linux-x64": "x86_64-unknown-linux-gnu", "linux-arm64": "aarch64-unknown-linux-gnu" }[host];

  const vdir = join(out, version);
  await rm(out, { recursive: true, force: true });
  await mkdir(vdir, { recursive: true });
  const work = join(ROOT, "target/release-stage");
  await rm(work, { recursive: true, force: true });

  const artifacts = [];
  for (const t of names) {
    const target = TARGETS[t]!;
    console.log(`\n== ${t}`);
    const env: Record<string, string> = {
      RUSTUP_TOOLCHAIN: TOOLCHAIN,
      SOURCE_DATE_EPOCH: epoch,
      CARGO_TARGET_DIR: join(ROOT, "target/release-dist"),
      // Keep the build machine's paths out of the binaries.
      RUSTFLAGS: `--remap-path-prefix=${ROOT}=/javelin --remap-path-prefix=${process.env.CARGO_HOME ?? join(process.env.HOME ?? "", ".cargo")}=/cargo`,
    };
    const cargoTarget = target.zig && !target.windows ? `${t}.${GLIBC}` : t;
    await run(
      ["cargo", target.zig ? "zigbuild" : "build", "--release", "--locked", "--target", cargoTarget,
        "-p", "javelin-cli", "-p", "javelin-node", "--bin", "javelin", "--bin", "javelin-node"],
      { env },
    );
    const exe = target.windows ? ".exe" : "";
    const top = `javelin-${version}-${t}`;
    const stage = join(work, top);
    await mkdir(join(stage, "bin"), { recursive: true });
    const binaries: Record<string, string> = {};
    for (const b of BINARIES) {
      const dest = join(stage, "bin", b + exe);
      await cp(join(ROOT, "target/release-dist", t, "release", b + exe), dest);
      await chmod(dest, 0o755);
      binaries[b] = await sha256(dest);
    }
    await cp(join(ROOT, "LICENSE"), join(stage, "LICENSE"));
    await writeFile(
      join(stage, "README.txt"),
      `Javelin ${version} for ${t}\n\nbin/javelin is the CLI and bin/javelin-node hosts repositories.\n` +
        `Put bin/ on your PATH, then run: javelin --version\nSource: https://entire.com/et/dhruv2mars/javelin\n`,
    );

    // A binary for this machine must answer --version before it is packed.
    let ran = "not run on the build machine";
    if (t === hostTarget) {
      for (const b of BINARIES) {
        const got = (await run([join(stage, "bin", b), "--version"], { quiet: true })).trim();
        if (got !== `${b} ${pkg}`) throw new Fail(`${b} --version printed ${got}, expected ${b} ${pkg}`);
      }
      ran = "--version ran on the build machine";
    }

    const file = `${top}.${target.windows ? "zip" : "tar.gz"}`;
    const archive = join(vdir, file);
    if (target.windows) {
      await run(["zip", "-q", "-X", "-r", archive, top], { cwd: work });
    } else {
      const bsd = (await run(["tar", "--version"], { quiet: true })).includes("bsdtar");
      const owner = bsd ? ["--uid", "0", "--gid", "0", "--uname", "root", "--gname", "root", "--no-mac-metadata"] : ["--owner=0", "--group=0", "--numeric-owner", "--sort=name"];
      await run(["tar", ...owner, "-czf", archive, top], { cwd: work, env: { COPYFILE_DISABLE: "1" } });
    }
    const st = await stat(archive);
    artifacts.push({
      target: t,
      file,
      sha256: await sha256(archive),
      size: st.size,
      binaries,
      built_on: `${host} with ${target.zig ? `cargo-zigbuild, zig ${zig}` : "cargo"}`,
      requires: target.zig && !target.windows ? `glibc ${GLIBC} or newer` : target.windows ? "Windows x86_64" : "macOS 11 or newer",
      checked: ran,
    });
    console.log(`${file}  ${st.size} bytes  ${ran}`);
  }

  const manifest = {
    schema: 1,
    name: "javelin",
    version,
    package_version: pkg,
    source_commit: commit,
    source_dirty: dirty,
    source_date_epoch: Number(epoch),
    toolchain: rustc,
    artifacts,
  };
  await writeFile(join(vdir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  await stampInstallers(baseUrl, [out, vdir]);
  const listed = [...artifacts.map((a) => a.file), "install.ps1", "install.sh", "manifest.json"].sort();
  const sums = (await Promise.all(listed.map(async (f) => `${await sha256(join(vdir, f))}  ${f}\n`))).join("");
  await writeFile(join(vdir, "SHA256SUMS"), sums);
  await writeFile(join(out, "latest"), version + "\n");
  await rm(work, { recursive: true, force: true });
  console.log(`\nstaged ${version} in ${out}${baseUrl ? ` for ${baseUrl}` : " (no release host stamped)"}\n${sums}`);
}

async function serve(flags: Map<string, string>): Promise<void> {
  const dir = resolve(flags.get("dir") ?? join(ROOT, "dist/release"));
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: Number(flags.get("port") ?? 4873),
    async fetch(req) {
      const path = resolve(dir, "." + decodeURIComponent(new URL(req.url).pathname));
      const file = Bun.file(path);
      const ok = (path === dir || path.startsWith(dir + sep)) && (await file.exists());
      console.log(`${req.method} ${new URL(req.url).pathname} ${ok ? 200 : 404}`);
      if (!ok) return new Response("not found\n", { status: 404 });
      const type = /\.(sh|ps1)$|\/(latest|SHA256SUMS)$/.test(path) ? "text/plain; charset=utf-8" : undefined;
      return new Response(file, type ? { headers: { "content-type": type } } : undefined);
    },
  });
  console.log(`serving ${dir} on ${server.url}`);
}

async function publish(flags: Map<string, string>): Promise<void> {
  const version = flags.get("version")!;
  await checkVersion(version);
  const dir = resolve(flags.get("dir") ?? join(ROOT, "dist/release"));
  const vdir = join(dir, version);
  const bucket = flags.get("bucket") ?? "javelin-releases";
  const publicUrl = flags.get("public-url")?.replace(/\/+$/, "");
  const dry = flags.has("dry-run");
  if (!publicUrl?.startsWith("https://")) throw new Fail("--public-url must be the bucket's https URL");

  // The staged files must still match SHA256SUMS, and the installers must point at this host.
  const sums = (await readFile(join(vdir, "SHA256SUMS"), "utf8")).trim().split("\n");
  const files = (await readdir(vdir)).sort();
  for (const line of sums) {
    const [hex, name] = line.split(/\s+/);
    if (!name || (await sha256(join(vdir, name))) !== hex) throw new Fail(`${name} does not match SHA256SUMS`);
  }
  if (files.length !== sums.length + 1) throw new Fail(`${vdir} holds files SHA256SUMS does not list`);
  const stamps = { "install.sh": `\nDEFAULT_RELEASE_BASE=${publicUrl}\n`, "install.ps1": `\n$DefaultReleaseBase = '${publicUrl}'\n` };
  for (const [f, line] of Object.entries(stamps)) {
    if (!(await readFile(join(vdir, f), "utf8")).includes(line)) {
      throw new Fail(`${f} is not stamped with ${publicUrl}; rebuild with --base-url ${publicUrl}`);
    }
  }

  // A published version is never replaced.
  const existing = await fetch(`${publicUrl}/${version}/SHA256SUMS`, { method: "HEAD" }).catch(() => undefined);
  if (existing?.ok) throw new Fail(`${version} is already published at ${publicUrl}`);
  if (!dry && !(process.env.CLOUDFLARE_API_TOKEN && process.env.CLOUDFLARE_ACCOUNT_ID)) {
    throw new Fail("set CLOUDFLARE_API_TOKEN (R2 edit on this bucket) and CLOUDFLARE_ACCOUNT_ID");
  }

  const put = async (key: string, path: string, type: string, cache: string) => {
    const cmd = ["bunx", WRANGLER, "r2", "object", "put", `${bucket}/${key}`, "--file", path,
      "--content-type", type, "--cache-control", cache, "--remote"];
    console.log(`${dry ? "would run" : "run"}: ${cmd.join(" ")}`);
    if (!dry) await run(cmd);
  };
  const typeOf = (f: string) =>
    f.endsWith(".tar.gz") ? "application/gzip" : f.endsWith(".zip") ? "application/zip" : f.endsWith(".json") ? "application/json" : "text/plain; charset=utf-8";
  // SHA256SUMS goes last, so a version is visible to installers only once all its files are.
  for (const f of files.filter((f) => f !== "SHA256SUMS")) {
    await put(`${version}/${f}`, join(vdir, f), typeOf(f), "public, max-age=31536000, immutable");
  }
  await put(`${version}/SHA256SUMS`, join(vdir, "SHA256SUMS"), typeOf("SHA256SUMS"), "public, max-age=31536000, immutable");
  if (flags.has("latest")) {
    const latest = join(dir, "latest");
    await writeFile(latest, version + "\n");
    await put("latest", latest, typeOf("latest"), "public, max-age=60");
  }
  const note = flags.has("latest") ? (dry ? "; latest would point at it" : "; latest points at it") : "";
  console.log(`${dry ? "dry run: nothing uploaded" : `published ${version} to ${bucket}`}${note}`);
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
