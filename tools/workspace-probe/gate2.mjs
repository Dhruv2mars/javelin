// Gate 2 proof (docs/build-contract.md). Drives a javelin binary through the user flow on
// one filesystem and records a verdict for every gate-2 clause. Exit 0 only when every
// check is VERIFIED; 3 when the only failures are listed in --known; 1 when any other
// check is not VERIFIED, or a listed one passes; 2 when setup fails. See README.md.
import * as fs from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const win = process.platform === 'win32';
const opts = Object.fromEntries(process.argv.slice(2).reduce((pairs, a, i, all) => a.startsWith('--') ? [...pairs, [a.slice(2), all[i + 1]?.startsWith('--') || all[i + 1] === undefined ? 'true' : all[i + 1]]] : pairs, []));
if (opts.privilege) { console.log(JSON.stringify(privilege())); process.exit(0); }
for (const required of ['javelin', 'seed', 'dir', 'fs', 'expect', 'out']) if (!opts[required]) { console.error(`missing --${required}`); process.exit(2); }
const javelin = resolve(opts.javelin);
const script = javelin.endsWith('.mjs');
const seeder = resolve(opts.seed);
const root = resolve(opts.dir);
const only = opts.only ? new Set(opts.only.split(',')) : null;
const bulkFiles = Number(opts['bulk-mb'] || 128) / 2;
const shims = opts.shims ? resolve(opts.shims) : null;
// Product bugs this proof already found, by filesystem: {"ntfs": {"clause/id": "finding"}}.
const known = opts.known ? JSON.parse(fs.readFileSync(opts.known, 'utf8'))[opts.fs] ?? {} : {};
const env = { ...process.env, PATH: shims ? `${shims}${win ? ';' : ':'}${process.env.PATH}` : process.env.PATH };
const report = { harness: 'gate2', fs: opts.fs, expect: opts.expect, platform: process.platform, javelin, started: new Date().toISOString(), checks: [], commands: [], lingering: [] };
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const sleep = ms => new Promise(r => setTimeout(r, ms));

function privilege() {
  if (win) {
    // Absolute: Git for Windows puts its own whoami, which has no /groups, first on PATH.
    const out = spawnSync(join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'whoami.exe'), ['/groups', '/fo', 'csv', '/nh'], { encoding: 'utf8' }).stdout || '';
    const level = out.match(/Mandatory Label\\(\w+) Mandatory Level/)?.[1] ?? 'unknown';
    return { elevated: level !== 'Medium' && level !== 'Low', integrity: level, adminDenyOnly: /BUILTIN\\Administrators[^\n]*deny only/i.test(out) };
  }
  const uid = process.getuid();
  const noNewPrivs = process.platform === 'linux' ? /NoNewPrivs:\s*1/.test(fs.readFileSync('/proc/self/status', 'utf8')) : null;
  return { elevated: uid === 0, uid, noNewPrivs };
}

// ---- checks -------------------------------------------------------------------------
async function check(clause, id, fn) {
  if (only && !only.has(clause)) return;
  const started = Date.now();
  const finding = known[`${clause}/${id}`];
  try {
    const detail = await fn();
    if (finding) throw new Error(`passed, but is listed as known failure "${finding}"; remove it from --known`);
    report.checks.push({ clause, id, verdict: 'VERIFIED', detail: detail ?? null, ms: Date.now() - started });
    console.log(`VERIFIED      ${clause}/${id}`);
  } catch (e) {
    const verdict = finding && !e.message.startsWith('passed, but') ? 'KNOWN FAILURE' : 'NOT VERIFIED';
    report.checks.push({ clause, id, verdict, finding: finding ?? null, detail: e.message, ms: Date.now() - started });
    console.log(`${verdict.padEnd(14)}${clause}/${id}: ${e.message}`);
  }
}
function assert(ok, message) { if (!ok) throw new Error(message); }
const want = clause => !only || only.has(clause);

// ---- running javelin ----------------------------------------------------------------
function argvFor(args) { return script ? [process.execPath, javelin, ...args] : [javelin, ...args]; }
function launch(args, cwd) {
  const [cmd, ...rest] = argvFor([...args, '--json']);
  const child = spawn(cmd, rest, { env, cwd, stdio: ['ignore', 'pipe', 'pipe'] });
  const result = { args, stdout: '', stderr: '', code: null, signal: null, started: Date.now() };
  child.stdout.on('data', d => { result.stdout += d; });
  child.stderr.on('data', d => { result.stderr += d; });
  const exited = new Promise(r => child.on('exit', (code, signal) => { result.code = code; result.signal = signal; result.exitedAt = Date.now(); r(); }));
  const closed = new Promise(r => child.on('close', () => { result.closedAt = Date.now(); r(); }));
  return { child, result, exited, closed };
}
async function settle(run) {
  await run.exited;
  // A descendant that outlives the command keeps the inherited pipes open.
  await Promise.race([run.closed, sleep(2000)]);
  const r = run.result;
  r.ms = r.exitedAt - r.started;
  if (r.closedAt === undefined) report.lingering.push({ args: r.args, reason: 'stdio held open after exit' });
  try { r.json = JSON.parse((r.code === 0 ? r.stdout : r.stderr).trim().split('\n').at(-1)); } catch { r.json = null; }
  const live = javelinProcesses();
  if (live.length) report.lingering.push({ args: r.args, reason: 'javelin process alive after command', processes: live });
  report.commands.push({ args: r.args, code: r.code, signal: r.signal, ms: r.ms, out: (r.code === 0 ? r.stdout : r.stderr).slice(0, 2000) });
  return r;
}
const jv = (args, cwd) => settle(launch(args, cwd));
async function ok(args, cwd) {
  const r = await jv(args, cwd);
  if (r.code !== 0 || r.json?.ok === false) throw new Error(`javelin ${r.args.join(' ')} exited ${r.code}: ${(r.stderr || r.stdout).trim().slice(0, 400)}`);
  return r;
}
function javelinProcesses() {
  let lines;
  if (win && !script) lines = spawnSync('tasklist', ['/FO', 'CSV', '/NH', '/FI', `IMAGENAME eq ${basename(javelin)}`], { encoding: 'utf8' }).stdout.split('\n').filter(l => l.toLowerCase().includes(basename(javelin).toLowerCase()));
  else if (win) lines = spawnSync('powershell', ['-NoProfile', '-Command', 'Get-CimInstance Win32_Process | % { "$($_.ProcessId) $($_.CommandLine)" }'], { encoding: 'utf8' }).stdout.split('\n').filter(l => l.includes(javelin) && !l.includes('gate2.mjs'));
  else lines = spawnSync('ps', ['-axww', '-o', 'pid=,command='], { encoding: 'utf8' }).stdout.split('\n').filter(l => l.includes(javelin) && !l.includes('gate2.mjs') && !l.trim().startsWith(`${process.pid} `));
  return lines.map(l => l.trim()).filter(Boolean);
}

// ---- the command surface (the only place that knows command names and JSON shape) ---
function find(value, keyTest, valueTest = v => typeof v === 'string') {
  if (!value || typeof value !== 'object') return undefined;
  for (const [k, v] of Object.entries(value)) if (keyTest(k) && valueTest(v)) return v;
  for (const v of Object.values(value)) { const hit = find(v, keyTest, valueTest); if (hit !== undefined) return hit; }
}
const hexUnder = (json, re) => {
  const scope = find(json, k => re.test(k), () => true);
  return typeof scope === 'string' ? scope : find(scope, () => true, v => typeof v === 'string' && /^[0-9a-f]{16,}$/.test(v));
};
const layerPaths = new Map();
const S = {
  init: repo => ok(['init', repo]),
  head: async repo => { const r = await ok(['history', repo]); return find(r.json, k => k === 'versions', Array.isArray).at(-1).number; },
  integrity: repo => ok(['check', repo]),
  // Fixture setup, not the flow under test: clone-backed workspaces have no Publish yet,
  // so Worlds are accepted through the library (examples/gate2_seed.rs).
  seedWorld: (repo, spec) => {
    const file = join(root, `spec-${randomBytes(4).toString('hex')}.json`);
    fs.writeFileSync(file, JSON.stringify(spec));
    const argv = seeder.endsWith('.mjs') ? [process.execPath, seeder, 'seed-world', repo, file] : [seeder, repo, file];
    const r = spawnSync(argv[0], argv.slice(1), { env, encoding: 'utf8' });
    fs.rmSync(file);
    return { code: r.status, out: `${r.stdout}${r.stderr}`.trim(), version: r.status === 0 ? JSON.parse(r.stdout.trim().split('\n').at(-1)).version : null };
  },
  createArgs: (repo, id, world) => ['workspace', 'create', id, ...(world === undefined ? [] : ['--world', String(world)]), '--repo', repo],
  create: async (repo, id, world) => S.opened(repo, id, await ok(S.createArgs(repo, id, world))),
  // A Layer started from an immutable checkpoint, then given its directory.
  clone: async (repo, id, checkpoint) => {
    await ok(['layer', 'clone', id, '--checkpoint', checkpoint, '--repo', repo]);
    return S.opened(repo, id, await ok(['workspace', 'open', id, '--repo', repo]));
  },
  opened: (repo, id, r) => {
    const path = find(r.json, k => k === 'root');
    layerPaths.set(`${repo}\0${id}`, path);
    return { path, strategy: find(r.json, k => k === 'strategy'), r };
  },
  path: async (repo, id) => { const r = await jv(['workspace', 'path', id, '--repo', repo]); return { r, path: find(r.json, k => k === 'root'), exists: find(r.json, k => k === 'exists', v => typeof v === 'boolean') }; },
  // An ordinary read; every layer and workspace command captures the Layer first.
  state: async (repo, id) => hexUnder((await ok(['layer', 'state', id, '--repo', repo])).json, /checkpoint/i),
  active: async (repo, id) => (await ok(['layer', 'state', id, '--repo', repo])).json?.state?.state === 'active',
  diff: async (repo, id) => { const r = await ok(['layer', 'diff', id, '--repo', repo]); return (find(r.json, k => k === 'changes', Array.isArray) || []).map(c => c.path); },
  // The agent lifecycle hook: run from inside the Layer directory, as an agent tool would.
  checkpointArgs: (repo, id) => ['checkpoint', id, '--repo', repo],
  checkpointCwd: (repo, id) => layerPaths.get(`${repo}\0${id}`),
  checkpoint: (repo, id) => jv(S.checkpointArgs(repo, id), S.checkpointCwd(repo, id)),
  capture: async (repo, id) => hexUnder((await ok(S.checkpointArgs(repo, id), S.checkpointCwd(repo, id))).json, /checkpoint/i),
  discardArgs: (repo, id, op = `discard-${id}`) => ['workspace', 'discard', id, '--operation', op, '--repo', repo],
  discard: (repo, id, op) => ok(S.discardArgs(repo, id, op)),
};

// ---- trees and fixtures -------------------------------------------------------------
function tree(dir, skip = () => false) {
  const out = {};
  (function visit(d) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const full = join(d, e.name), path = relative(dir, full).split(sep).join('/');
      if (skip(path)) continue;
      if (e.isSymbolicLink()) out[path] = `link:${fs.readlinkSync(full).split(sep).join('/')}`;
      else if (e.isDirectory()) visit(full);
      else { const st = fs.statSync(full); out[path] = `${!win && st.mode & 0o100 ? 'x' : 'f'}:${sha(fs.readFileSync(full))}`; }
    }
  })(dir);
  return out;
}
const noBuild = p => p === 'target' || p.startsWith('target/');
function sameTree(a, b, label) {
  const diff = [...new Set([...Object.keys(a), ...Object.keys(b)])].filter(p => a[p] !== b[p]);
  assert(diff.length === 0, `${label}: ${diff.length} paths differ: ${diff.slice(0, 6).join(', ')}`);
}
const MAIN_RS = 'fn main() {\n    let built = include_str!("value.txt").trim();\n    let live = std::fs::read_to_string("src/value.txt").unwrap();\n    println!("{}|{}", built, live.trim());\n}\n';
function writeFixture(dir, bulk) {
  fs.mkdirSync(dir, { recursive: true });
  const files = {
    'README.md': 'shared base text\n', 'src/main.rs': MAIN_RS, 'src/value.txt': 'base\n', 'docs/Guide.md': '# Guide\n',
    'data/nested/deep/file.txt': 'deep\n', 'empty.txt': '', 'bin/run.sh': '#!/bin/sh\necho run\n',
  };
  for (const [p, c] of Object.entries(files)) { fs.mkdirSync(dirname(join(dir, p)), { recursive: true }); fs.writeFileSync(join(dir, p), c); }
  if (!win) fs.chmodSync(join(dir, 'bin/run.sh'), 0o755);
  fs.symlinkSync('README.md', join(dir, 'link-readme'), 'file');
  fs.symlinkSync('docs', join(dir, 'link-docs'), 'dir');
  if (!bulk) return;
  fs.mkdirSync(join(dir, 'bulk')); fs.mkdirSync(join(dir, 'small'));
  for (let i = 0; i < bulkFiles; i++) fs.writeFileSync(join(dir, 'bulk', `blob-${i}.bin`), randomBytes(2 << 20));
  for (let i = 0; i < 2000; i++) fs.writeFileSync(join(dir, 'small', `s${i}.txt`), `small ${i}\n`.repeat(100));
}
// A spec for gate2_seed: every file by source path, every symlink by target.
function specOf(dir) {
  const spec = {};
  (function visit(d) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const full = join(d, e.name), path = relative(dir, full).split(sep).join('/');
      if (e.isSymbolicLink()) spec[path] = { link: fs.readlinkSync(full).split(sep).join('/') };
      else if (e.isDirectory()) visit(full);
      else spec[path] = { from: full, exec: !win && (fs.statSync(full).mode & 0o100) !== 0 };
    }
  })(dir);
  return spec;
}
async function seed(name, bulk) {
  const repo = join(root, name), fixture = join(root, `${name}-fixture`);
  fs.mkdirSync(repo);
  await S.init(repo);
  writeFixture(fixture, bulk);
  const r = S.seedWorld(repo, specOf(fixture));
  assert(r.code === 0, `seeding World 1 failed: ${r.out}`);
  // A bulk fixture stays until the disk measurements end: ReFS frees deleted space
  // lazily, and a free landing inside a measured step would hide a copy.
  if (!bulk) fs.rmSync(fixture, { recursive: true });
  return repo;
}
let snapshots = 0;
async function snapshot(repo, checkpoint) {
  const id = `snapshot-${++snapshots}`;
  const { path } = await S.clone(repo, id, checkpoint);
  const t = tree(path, noBuild);
  await S.discard(repo, id);
  return t;
}
const fileHash = text => `f:${sha(text)}`;
// Flush dirty data first: ReFS allocates written data lazily, which would otherwise shift
// one step's usage into the next measurement. Windows has no unprivileged volume flush,
// so flush each file under the run (FlushFileBuffers through fsync).
function flushTree(dir) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, e.name);
    if (e.isDirectory()) flushTree(full);
    else if (e.isFile()) { try { const fd = fs.openSync(full, 'r+'); try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); } } catch {} }
  }
}
function freeBytes() {
  if (win) flushTree(root); else spawnSync('sync');
  const s = fs.statfsSync(root);
  return Number(s.bavail) * Number(s.bsize);
}
// ReFS and APFS settle free space lazily: wait for three steady one-second samples.
async function stableFree() {
  let last = freeBytes(), steady = 0;
  for (let i = 0; i < 60 && steady < 3; i++) { await sleep(1000); const now = freeBytes(); steady = Math.abs(now - last) < (256 << 10) ? steady + 1 : 0; last = now; }
  return last;
}
// Explicit: a failing exit, a JSON error code, and an offending path or directory named.
function explicitRefusal(r, paths) {
  const text = `${r.stdout}\n${r.stderr}`.normalize('NFC').toLowerCase();
  const names = paths.flatMap(p => p.split('/').map((_, i, parts) => parts.slice(0, i + 1).join('/')));
  const named = names.some(n => text.includes(n.normalize('NFC').toLowerCase()) || text.includes(JSON.stringify(n).slice(1, -1).toLowerCase()));
  return r.code !== 0 && r.json?.ok === false && r.json?.error?.code === 'unsupported' && named;
}

// ---- main ---------------------------------------------------------------------------
const mounts = () => process.platform === 'linux' ? fs.readFileSync('/proc/self/mountinfo', 'utf8').split('\n').map(l => l.split(' ')[4]).sort().join('\n')
  : process.platform === 'darwin' ? spawnSync('/sbin/mount', { encoding: 'utf8' }).stdout.split('\n').map(l => l.split(' (')[0]).sort().join('\n') : null;
fs.mkdirSync(root, { recursive: true });
assert(fs.readdirSync(root).length === 0, `--dir ${root} must be empty`);
const mountsBefore = mounts();
let exitCode = 0;
try {
  await check('unprivileged', 'not-elevated', () => {
    const p = privilege();
    assert(!p.elevated, `running elevated: ${JSON.stringify(p)}`);
    if (process.platform === 'linux') assert(p.noNewPrivs, 'Linux run must use no_new_privs so setuid helpers such as sudo cannot elevate');
    return p;
  });
  await check('unprivileged', 'privileged-tool-shims-armed', () => {
    assert(shims, '--shims is required: failing git, sudo and mount stand-ins prove Javelin never calls them');
    const r = spawnSync('sudo', ['-n', 'true'], { env });
    assert(r.status === 97 && fs.existsSync(join(shims, 'invoked-sudo')), `sudo shim did not intercept (exit ${r.status})`);
    fs.rmSync(join(shims, 'invoked-sudo'));
    return fs.readdirSync(shims).filter(n => !n.startsWith('invoked-'));
  });

  // Shared data, measured by free-space deltas against a byte-copy control on the same volume.
  if (want('sharing') || want('crash')) {
    const bulkRepo = await seed('bulk', true);
    const bulkBytes = bulkFiles * (2 << 20);
    // The first Layer of a World also materializes its base, once; measure Layers after it.
    const warm = await S.create(bulkRepo, 'bulk-0');
    const worldTree = tree(warm.path);
    // Reclaim all trash now, so no deferred reclaim frees space inside a measured create.
    await ok(['workspace', 'recover', '--repo', bulkRepo]);
    // No deletions between readings: ReFS frees deleted space lazily, and a free landing
    // inside a measured step would hide a copy. The byte-copy control therefore runs last.
    // A one-off free (an earlier deletion or truncation settling) spoils one attempt: free
    // space rises in a step, or the two identical creates disagree. Repeat with fresh
    // Layers, deleting nothing, until an attempt is clean.
    const mb = n => Math.round(n / 1048576);
    const spoiled = d => Object.entries(d).filter(([k, v]) => k !== 'bulkMB' && v < -0.05 * d.bulkMB).map(([k, v]) => `${k} ${v}`)
      .concat(Math.abs(d.createMB - d.secondCreateMB) > 0.25 * d.bulkMB ? [`identical creates used ${d.createMB} and ${d.secondCreateMB} MB`] : []);
    const attempts = [];
    let l1, textCreate, f0, f1, f2, fe, fc, fk, deltas;
    for (let attempt = 0; attempt < 3; attempt++) {
      f0 = await stableFree();
      l1 = await S.create(bulkRepo, `bulk-1-${attempt}`);
      f1 = await stableFree();
      const textArgv = argvFor(S.createArgs(bulkRepo, `bulk-2-${attempt}`));
      textCreate = spawnSync(textArgv[0], textArgv.slice(1), { env, encoding: 'utf8' });
      f2 = await stableFree();
      fs.writeFileSync(join(l1.path, 'bulk', 'blob-0.bin'), randomBytes(2 << 20));
      fe = await stableFree();
      await S.capture(bulkRepo, `bulk-1-${attempt}`);
      fc = await stableFree();
      const control = join(root, `control-copy-${attempt}`);
      fs.mkdirSync(control);
      for (const name of fs.readdirSync(join(l1.path, 'bulk'))) fs.writeFileSync(join(control, name), fs.readFileSync(join(l1.path, 'bulk', name)));
      fk = await stableFree();
      deltas = { bulkMB: mb(bulkBytes), createMB: mb(f0 - f1), secondCreateMB: mb(f1 - f2), editOneFileMB: mb(f2 - fe), captureMB: mb(fe - fc), controlCopyMB: mb(fc - fk) };
      attempts.push(deltas);
      if (!spoiled(deltas).length) break;
    }
    report.disk = { strategy: l1.strategy, deltas, attempts, base: find(warm.r.json, k => k === 'base', () => true) };
    await check('sharing', 'byte-copy-control-is-measurable', () => {
      assert(fc - fk >= 0.8 * bulkBytes, `a ${mb(bulkBytes)} MB byte copy used only ${mb(fc - fk)} MB; free-space deltas are not trustworthy here`);
      const why = spoiled(deltas);
      assert(why.length === 0, `every one of ${attempts.length} attempts was spoiled by a concurrent free (last: ${why.join(', ')}); it could hide a copy`);
      return { deltas, attempts: attempts.length };
    });
    await check('sharing', opts.expect === 'shared' ? 'unchanged-data-shared-with-base' : 'copy-fallback-duplicates-data', () => {
      if (opts.expect === 'shared') assert(f0 - f1 <= 0.2 * bulkBytes && f1 - f2 <= 0.2 * bulkBytes, `Layer creation used ${mb(f0 - f1)} and ${mb(f1 - f2)} MB for ${mb(bulkBytes)} MB of unchanged data`);
      else assert(f0 - f1 >= 0.8 * bulkBytes, `copy fallback used only ${mb(f0 - f1)} MB for ${mb(bulkBytes)} MB`);
      return deltas;
    });
    await check('sharing', 'edit-and-capture-stay-sparse', () => {
      assert(f2 - fe <= 0.2 * bulkBytes, `rewriting one 2 MB file used ${mb(f2 - fe)} MB`);
      assert(fe - fc <= 0.2 * bulkBytes, `capturing one changed file used ${mb(fe - fc)} MB`);
      return deltas;
    });
    await check('sharing', 'strategy-reported', () => {
      assert(typeof l1.strategy === 'string' && l1.strategy, `no strategy in ${JSON.stringify(l1.r.json).slice(0, 300)}`);
      const copy = /copy/i.test(l1.strategy);
      assert(copy === (opts.expect === 'copy'), `strategy ${l1.strategy} on ${opts.fs}, expected ${opts.expect}`);
      const text = `${textCreate.stdout}${textCreate.stderr}`;
      assert(textCreate.status === 0, `plain create failed: ${text.slice(0, 300)}`);
      if (copy) assert(/warning/i.test(text) && /duplicat/i.test(text), `plain output does not warn that the copy duplicated data: ${text.slice(0, 300)}`);
      return { strategy: l1.strategy, text: text.trim().slice(0, 300) };
    });

    // Crash safety: kill each operation at spread points across its own measured duration.
    async function killed(args, delay, cwd) {
      const run = launch(args, cwd);
      let exited = false; run.exited.then(() => { exited = true; });
      await sleep(delay);
      const landed = !exited;
      if (landed) run.child.kill('SIGKILL');
      await run.exited; await Promise.race([run.closed, sleep(2000)]);
      report.commands.push({ args, killedAfterMs: delay, landed, code: run.result.code, signal: run.result.signal });
      return landed;
    }
    const points = d => [0.05, 0.15, 0.3, 0.45, 0.6, 0.75, 0.9].map(f => Math.max(1, Math.round(d * f)));
    const timed = async (args, cwd) => { const r = await ok(args, cwd); return r.ms; };
    const bulkWorld = await S.head(bulkRepo);
    await check('crash', 'interrupted-create-recovers', async () => {
      // The fastest of three runs, so the kill points fall inside a typical create.
      let duration = Infinity;
      for (let t = 0; t < 3; t++) { duration = Math.min(duration, await timed(S.createArgs(bulkRepo, `time-create-${t}`, bulkWorld))); await S.discard(bulkRepo, `time-create-${t}`); }
      let landed = 0;
      for (const [i, delay] of points(duration).entries()) {
        const id = `kill-create-${i}`;
        if (await killed(S.createArgs(bulkRepo, id, bulkWorld), delay)) landed++;
        // The next command sees either no Layer, or one whose directory is complete or absent.
        const p = await S.path(bulkRepo, id);
        if (p.r.code === 0 && p.exists) sameTree(worldTree, tree(p.path), `Layer ${id} directory after an interrupted create`);
        sameTree(worldTree, tree((await S.create(bulkRepo, id, bulkWorld)).path), `retried create ${id}`);
        await S.discard(bulkRepo, id);
      }
      assert(landed >= 2, `only ${landed} kills landed inside a ${duration} ms create`);
      return { durationMs: duration, landed };
    });
    await check('crash', 'interrupted-checkpoint-recovers', async () => {
      const k = await S.create(bulkRepo, 'kill-checkpoint');
      const rewrite = n => { for (let i = 1; i < Math.min(9, bulkFiles); i++) fs.writeFileSync(join(k.path, 'bulk', `blob-${i}.bin`), randomBytes(2 << 20)); fs.writeFileSync(join(k.path, 'round.txt'), `${n}\n`); };
      rewrite(-2); await S.capture(bulkRepo, 'kill-checkpoint');
      rewrite(-1);
      const duration = await timed(S.checkpointArgs(bulkRepo, 'kill-checkpoint'), k.path);
      let landed = 0;
      for (const [i, delay] of points(duration).slice(0, 5).entries()) {
        rewrite(i);
        if (await killed(S.checkpointArgs(bulkRepo, 'kill-checkpoint'), delay, k.path)) landed++;
        const snap = await snapshot(bulkRepo, await S.capture(bulkRepo, 'kill-checkpoint'));
        sameTree(tree(k.path), snap, `checkpoint after interrupted capture ${i}`);
      }
      await S.discard(bulkRepo, 'kill-checkpoint');
      assert(landed >= 2, `only ${landed} kills landed inside a ${duration} ms checkpoint`);
      return { durationMs: duration, landed };
    });
    await check('crash', 'interrupted-discard-recovers-on-next-command', async () => {
      let duration = Infinity;
      for (let n = 0; n < 3; n++) {
        const t = await S.create(bulkRepo, `time-discard-${n}`);
        duration = Math.min(duration, await timed(S.discardArgs(bulkRepo, `time-discard-${n}`)));
        assert(!fs.existsSync(t.path), 'discarded Layer path still exists');
      }
      let landed = 0;
      const outcomes = [];
      for (const [i, delay] of points(duration).entries()) {
        const id = `kill-discard-${i}`, marker = `gate2-marker-${randomBytes(6).toString('hex')}`;
        const { path } = await S.create(bulkRepo, id);
        fs.writeFileSync(join(path, marker), 'marker\n');
        const before = tree(path);
        if (await killed(S.discardArgs(bulkRepo, id), delay)) landed++;
        // The next command finishes or undoes the discard: an active Layer is intact; a
        // discarded one has no directory, and its data is reclaimed.
        await S.path(bulkRepo, id);
        if (await S.active(bulkRepo, id)) {
          sameTree(before, tree(path), `Layer ${id} still active after an interrupted discard`);
          await S.discard(bulkRepo, id);
          outcomes.push('rolled-back');
        } else {
          assert(!fs.existsSync(path), `Layer ${id} is discarded but its directory is still at ${path} after the next command`);
          outcomes.push('rolled-forward');
        }
        // Only create, open, discard and recover take the store lock and reclaim trash
        // (bounded); the next of those, here a fresh Layer, must reclaim this one.
        await S.create(bulkRepo, `${id}-next`);
        await S.discard(bulkRepo, `${id}-next`);
        const left = [];
        // Sealed World bases are unreadable (mode 000) and hold only World data.
        const entries = d => { try { return fs.readdirSync(d, { withFileTypes: true }); } catch (e) { if (e.code === 'EACCES') return []; throw e; } };
        (function search(d) { for (const e of entries(d)) { if (e.name === marker) left.push(join(d, e.name)); else if (e.isDirectory() && !e.isSymbolicLink()) search(join(d, e.name)); } })(root);
        assert(left.length === 0, `discarded data not reclaimed by the next reclaiming workspace command: ${left.join(', ')}`);
      }
      assert(landed >= 2, `only ${landed} kills landed inside a ${duration} ms discard`);
      return { durationMs: duration, landed, outcomes };
    });
    await check('crash', 'repository-integrity-after-interruptions', () => S.integrity(bulkRepo).then(() => true));
    fs.rmSync(join(root, 'bulk-fixture'), { recursive: true });
    for (let a = 0; a < attempts.length; a++) fs.rmSync(join(root, `control-copy-${a}`), { recursive: true });
  }

  const repo = await seed('small', false);
  const world = await S.head(repo);
  const a = await S.create(repo, 'layer-a'), b = await S.create(repo, 'layer-b');
  const pristine = tree(a.path);

  await check('tools', 'layer-on-repository-volume', () => {
    // A btrfs snapshot is its own subvolume with its own st_dev; compare mounts on Linux.
    const mountOf = p => fs.readFileSync('/proc/self/mountinfo', 'utf8').split('\n').map(l => l.split(' ')[4]).filter(m => m && (p === m || p.startsWith(m.endsWith('/') ? m : `${m}/`))).sort((x, y) => y.length - x.length)[0];
    const same = process.platform === 'linux' ? mountOf(fs.realpathSync(a.path)) === mountOf(fs.realpathSync(repo)) : fs.statSync(a.path).dev === fs.statSync(repo).dev;
    assert(same, `Layer ${a.path} is not on the repository volume`);
    return a.path;
  });
  await check('tools', 'read-and-traverse', () => {
    assert(Object.keys(pristine).length === 9, `expected 9 entries, saw ${Object.keys(pristine).join(', ')}`);
    assert(fs.readFileSync(join(a.path, 'data/nested/deep/file.txt'), 'utf8') === 'deep\n', 'deep read');
    sameTree(pristine, tree(b.path), 'peer Layers differ at creation');
  });
  await check('tools', 'no-hardlinks', () => {
    const shared = Object.keys(pristine).filter(p => !pristine[p].startsWith('link:') && fs.statSync(join(a.path, p)).nlink !== 1);
    assert(shared.length === 0, `hard-linked files: ${shared.join(', ')}`);
  });
  await check('tools', 'symlinks', () => {
    assert(fs.lstatSync(join(a.path, 'link-readme')).isSymbolicLink() && fs.readlinkSync(join(a.path, 'link-readme')) === 'README.md', 'file symlink not materialized as a symlink');
    assert(fs.readFileSync(join(a.path, 'link-docs', 'Guide.md'), 'utf8') === '# Guide\n', 'directory symlink does not resolve');
    fs.symlinkSync('src/value.txt', join(a.path, 'link-new'), 'file');
  });
  await check('tools', 'in-place-write-isolated', () => {
    const fd = fs.openSync(join(a.path, 'README.md'), 'r+'); fs.writeSync(fd, 'PRIVATE'); fs.closeSync(fd);
    assert(fs.readFileSync(join(b.path, 'README.md'), 'utf8') === 'shared base text\n', 'in-place write leaked into a peer Layer');
  });

  // Watchers: an edit in A is seen in A, never in B, and capture itself writes nothing.
  const watch = dir => { const events = []; const w = fs.watch(dir, { recursive: true }, (t, f) => events.push(`${t}:${f}`)); return { events, close: () => w.close() }; };
  const wa = watch(a.path), wb = watch(b.path);
  await sleep(500);
  wa.events.length = 0; wb.events.length = 0;
  await check('tools', 'atomic-save-rename', async () => {
    const target = join(a.path, 'docs/Guide.md');
    fs.writeFileSync(`${target}.tmp`, '# Guide, saved atomically\n'); fs.renameSync(`${target}.tmp`, target);
    for (let i = 0; i < 50 && !wa.events.length; i++) await sleep(100);
    assert(wa.events.length > 0, 'watcher on the Layer saw no event for an atomic save');
    await sleep(1000);
    assert(wb.events.length === 0, `peer Layer watcher saw ${wb.events.join(', ')}`);
    return { events: wa.events.slice(0, 4) };
  });
  await check('tools', 'backup-rename-save', () => {
    const target = join(a.path, 'README.md');
    fs.renameSync(target, `${target}~`); fs.writeFileSync(target, 'saved vim-style\n'); fs.rmSync(`${target}~`);
  });
  await check('tools', 'capture-writes-nothing-into-the-layer', async () => {
    await sleep(500); wa.events.length = 0;
    await S.capture(repo, 'layer-a'); await S.state(repo, 'layer-a');
    await sleep(1500);
    assert(wa.events.length === 0, `capture touched the Layer: ${wa.events.slice(0, 5).join(', ')}`);
  });
  wa.close(); wb.close();

  await check('tools', 'compiler-and-test-run', () => {
    const exe = win ? 'app.exe' : 'app';
    const outputs = [];
    for (const [layer, value] of [[a.path, 'alpha'], [b.path, 'beta']]) {
      fs.writeFileSync(join(layer, 'src/value.txt'), `${value}\n`);
      fs.mkdirSync(join(layer, 'target'), { recursive: true });
      const build = spawnSync('rustc', ['--edition', '2021', join('src', 'main.rs'), '-o', join('target', exe)], { cwd: layer, env, encoding: 'utf8' });
      assert(build.status === 0, `rustc failed in ${layer}: ${build.stderr}`);
      const run = spawnSync(join(layer, 'target', exe), { cwd: layer, encoding: 'utf8' });
      assert(run.stdout.trim() === `${value}|${value}`, `built program printed ${JSON.stringify(run.stdout)}`);
      outputs.push(run.stdout.trim());
    }
    return outputs;
  });
  await check('tools', 'delete-file-and-directories', () => {
    fs.rmSync(join(a.path, 'data'), { recursive: true });
    assert(fs.existsSync(join(b.path, 'data/nested/deep/file.txt')), 'delete leaked into peer');
  });
  await check('tools', 'case-only-rename', async () => {
    fs.renameSync(join(a.path, 'docs/Guide.md'), join(a.path, 'docs/GUIDE.md'));
    const snap = await snapshot(repo, await S.capture(repo, 'layer-a'));
    assert(snap['docs/GUIDE.md'] && !snap['docs/Guide.md'], `captured docs entries: ${Object.keys(snap).filter(p => p.startsWith('docs/')).join(', ')}`);
  });
  await check('capture', 'every-edit-captured-by-ordinary-commands', async () => {
    fs.mkdirSync(join(a.path, 'edits'));
    for (let i = 0; i < 5; i++) {
      fs.writeFileSync(join(a.path, 'edits', `e${i}.txt`), `edit ${i}\n`);
      const changes = await S.diff(repo, 'layer-a');
      assert(changes.includes(`edits/e${i}.txt`), `edit ${i} not captured by the next command; diff: ${changes.join(', ')}`);
    }
    const snap = await snapshot(repo, await S.state(repo, 'layer-a'));
    sameTree(tree(a.path, noBuild), snap, 'captured checkpoint vs Layer on disk');
  });
  await check('capture', 'unchanged-layer-captures-nothing-new', async () => {
    const first = await S.capture(repo, 'layer-a'), second = await S.capture(repo, 'layer-a'), third = await S.state(repo, 'layer-a');
    assert(first === second && second === third, `checkpoint moved without an edit: ${first} -> ${second} -> ${third}`);
  });
  await check('capture', 'mtime-preserving-rewrite', async () => {
    // cp -p, tar and rsync -t restore an old mtime; only ctime or inode reveals the rewrite.
    const f = join(a.path, 'src/value.txt'), old = new Date('2020-01-02T03:04:05Z');
    fs.writeFileSync(f, 'one\n'); fs.utimesSync(f, old, old); await S.capture(repo, 'layer-a');
    fs.writeFileSync(f, 'two\n'); fs.utimesSync(f, old, old);
    const snap = await snapshot(repo, await S.capture(repo, 'layer-a'));
    assert(snap['src/value.txt'] === fileHash('two\n'), 'a same-size rewrite that keeps mtime (cp -p, tar, rsync -t) was not captured');
  });
  await check('capture', 'same-second-rewrite', async () => {
    const f = join(a.path, 'src/value.txt'), second = new Date(Math.floor(Date.now() / 1000) * 1000);
    fs.writeFileSync(f, 'sec1\n'); fs.utimesSync(f, second, second); await S.capture(repo, 'layer-a');
    fs.writeFileSync(f, 'sec2\n'); fs.utimesSync(f, second, second);
    const snap = await snapshot(repo, await S.capture(repo, 'layer-a'));
    assert(snap['src/value.txt'] === fileHash('sec2\n'), 'a rewrite within the same mtime second (racy entry) was not captured');
  });
  await check('capture', 'concurrent-rewrites-during-capture', async () => {
    fs.mkdirSync(join(a.path, 'hot'));
    const writer = spawn(process.execPath, [join(here, 'gate2-writer.mjs'), join(a.path, 'hot'), '3000'], { stdio: ['ignore', 'pipe', 'inherit'] });
    let finalGen = ''; writer.stdout.on('data', d => { finalGen += d; });
    const done = new Promise(r => writer.on('exit', r));
    await sleep(200);
    const during = [];
    let uncertain = 0;
    let running = true; done.then(() => { running = false; });
    while (running) {
      const r = await S.checkpoint(repo, 'layer-a');
      assert(r.code === 0 || (r.json?.ok === false && r.json?.error?.code), `capture during rewrites crashed: exit ${r.code} ${r.stderr.slice(0, 300)}`);
      const cp = r.code === 0 ? hexUnder(r.json, /checkpoint/i) : null;
      uncertain += find(r.json, k => k === 'uncertain', Array.isArray)?.length ?? 0;
      if (cp && !during.includes(cp)) during.push(cp);
    }
    assert(await done === 0, 'writer failed');
    const snap = await snapshot(repo, await S.capture(repo, 'layer-a'));
    for (const f of ['inplace.txt', 'atomic.txt']) assert(snap[`hot/${f}`] === fileHash(fs.readFileSync(join(a.path, 'hot', f))), `final hot/${f} not captured after rewrites stopped`);
    // Atomic saves replace whole files; every captured version must be a complete one.
    let inspected = 0;
    for (const cp of during.slice(0, 4)) {
      const id = `torn-${cp.slice(0, 12)}`;
      const { path: p } = await S.clone(repo, id, cp);
      const file = join(p, 'hot/atomic.txt');
      const text = fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
      await S.discard(repo, id);
      if (text === null) continue;
      const [body, sum] = text.split('\nsum ');
      assert(sha(body) === sum?.trim(), `torn atomic-save capture in checkpoint ${cp}`);
      inspected++;
    }
    assert(inspected >= 2, `only ${inspected} checkpoints captured during rewrites contained the atomic file`);
    return { checkpointsDuringRewrites: during.length, inspected, uncertain, finalGeneration: finalGen.trim() };
  });
  await check('tools', 'layer-clone-is-an-isolated-snapshot', async () => {
    const cp = await S.capture(repo, 'layer-a');
    const d = await S.clone(repo, 'layer-d', cp);
    sameTree(tree(a.path, noBuild), tree(d.path, noBuild), 'clone vs source checkpoint');
    fs.writeFileSync(join(a.path, 'src/value.txt'), 'after clone\n');
    fs.writeFileSync(join(d.path, 'README.md'), 'clone edit\n');
    assert(fs.readFileSync(join(d.path, 'src/value.txt'), 'utf8') !== 'after clone\n', 'source edit leaked into clone');
    assert(fs.readFileSync(join(a.path, 'README.md'), 'utf8') !== 'clone edit\n', 'clone edit leaked into source');
    await S.discard(repo, 'layer-d');
  });
  await check('tools', 'fresh-layer-is-pristine', async () => {
    const c = await S.create(repo, 'layer-c', world);
    sameTree(pristine, tree(c.path), 'fresh Layer after peer edits');
    await S.discard(repo, 'layer-c');
  });

  await S.integrity(repo);

  // Worlds this volume cannot hold path for path (case or Unicode-normalization folding,
  // Windows reserved names) must be refused explicitly; any other volume must
  // materialize them exactly. Never a silent merge or rename.
  if (want('refusals')) {
    const portable = join(root, 'portable');
    fs.mkdirSync(portable);
    await S.init(portable);
    const probeDir = join(root, 'volume-probe');
    // Whether this volume keeps all names distinct and exactly as written.
    const listing = dir => fs.readdirSync(dir, { withFileTypes: true }).flatMap(e => e.isDirectory() ? listing(join(dir, e.name)).map(x => `${e.name}/${x}`) : [e.name]);
    // Windows can create reserved device names and trailing dots or spaces only through
    // \\?\ paths that ordinary tools cannot open, so they never count as held there.
    const windowsOnly = x => /(^|\/)(con|prn|aux|nul|com[0-9¹²³]|lpt[0-9¹²³])(\.[^/]*)?(\/|$)|[. ](\/|$)/i.test(x);
    const holds = names => {
      if (win && names.some(windowsOnly)) return false;
      fs.rmSync(probeDir, { recursive: true, force: true }); fs.mkdirSync(probeDir);
      try {
        for (const x of names) { fs.mkdirSync(dirname(join(probeDir, x)), { recursive: true }); fs.writeFileSync(join(probeDir, x), x); }
        const got = listing(probeDir);
        return got.length === names.length && names.every(x => got.includes(x) && fs.readFileSync(join(probeDir, x), 'utf8') === x);
      } catch { return false; } finally { fs.rmSync(probeDir, { recursive: true, force: true }); }
    };
    let n = 0;
    async function world(label, names) {
      const fits = holds(names);
      const spec = Object.fromEntries(names.map(x => [x, { text: `${x}\n` }]));
      const seeded = S.seedWorld(portable, spec);
      if (seeded.code !== 0) return { label, refusedAt: 'world', detail: seeded.out.slice(0, 200) };
      const id = `portable-${++n}`;
      const r = await jv(S.createArgs(portable, id, seeded.version));
      if (!fits) {
        assert(explicitRefusal(r, names), `${label}: a World this volume cannot hold was not refused explicitly (exit ${r.code}): ${(r.stderr || r.stdout).slice(0, 300)}`);
        const p = await S.path(portable, id);
        assert(!p.exists, `${label}: a refused Layer left a directory`);
        return { label, refusedAt: 'materialize', error: r.json.error.message.slice(0, 160) };
      }
      assert(r.code === 0, `${label}: this volume holds ${names.join(', ')} distinctly, but create failed: ${(r.stderr || r.stdout).slice(0, 300)}`);
      const got = tree(find(r.json, k => k === 'root'));
      sameTree(Object.fromEntries(names.map(x => [x, fileHash(`${x}\n`)])), got, `${label}: materialized World`);
      await S.discard(portable, id);
      return { label, materialized: true };
    }
    await check('refusals', 'case-collision', async () => [
      await world('case', ['src/Makefile', 'src/makefile']),
      await world('directory case', ['Dir/a.txt', 'dir/b.txt']),
    ]);
    await check('refusals', 'unicode-normalization-collision', async () => [await world('nfc/nfd', ['docs/café.md', 'docs/café.md'])]);
    await check('refusals', 'windows-reserved-names', async () => {
      const out = [];
      for (const name of ['CON', 'aux.txt', 'nul.c', 'COM1.h', 'LPT9.log', 'trailing.', 'trailing ', 'dir./x.txt']) out.push(await world(JSON.stringify(name), ['ok.txt', name]));
      return out;
    });
    await check('refusals', 'repository-integrity-after-refusals', () => S.integrity(portable).then(() => true));
  }

  await check('process', 'no-javelin-process-between-commands', () => {
    assert(report.lingering.length === 0, `${report.lingering.length} lingering: ${JSON.stringify(report.lingering[0])}`);
    return { commands: report.commands.length };
  });
  await check('unprivileged', 'no-git-sudo-or-mount-invoked', () => {
    const hits = fs.readdirSync(shims).filter(n => n.startsWith('invoked-'));
    assert(hits.length === 0, `invoked: ${hits.join(', ')}`);
    const after = mounts();
    assert(after === mountsBefore, 'the mount table changed during the run');
  });
} catch (e) {
  report.fatal = `${e.stack || e}`;
  console.error(`FATAL ${e.message}`);
  exitCode = 2;
}
report.finished = new Date().toISOString();
const failed = report.checks.filter(c => c.verdict === 'NOT VERIFIED');
const knownFailed = report.checks.filter(c => c.verdict === 'KNOWN FAILURE');
report.verdict = exitCode === 2 ? 'INCONCLUSIVE' : failed.length ? 'NOT VERIFIED' : knownFailed.length ? 'NOT VERIFIED (known product failures only)' : 'VERIFIED';
fs.writeFileSync(opts.out, `${JSON.stringify(report, null, 2)}\n`);
const passed = report.checks.length - failed.length - knownFailed.length;
console.log(`gate 2 on ${opts.fs}: ${report.verdict} (${passed}/${report.checks.length} checks, ${knownFailed.length} known failures, ${report.commands.length} javelin commands)`);
process.exit(exitCode || (failed.length ? 1 : knownFailed.length ? 3 : 0));
