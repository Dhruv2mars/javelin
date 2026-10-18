// Gate 2 proof (docs/build-contract.md). Drives a javelin binary through the user flow on
// one filesystem and records a verdict for every gate-2 clause. Exit 0 only when every
// check is VERIFIED; 1 when any is not; 2 when setup fails. See README.md.
import * as fs from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const win = process.platform === 'win32';
const opts = Object.fromEntries(process.argv.slice(2).reduce((pairs, a, i, all) => a.startsWith('--') ? [...pairs, [a.slice(2), all[i + 1]?.startsWith('--') || all[i + 1] === undefined ? 'true' : all[i + 1]]] : pairs, []));
if (opts.privilege) { console.log(JSON.stringify(privilege())); process.exit(0); }
for (const required of ['javelin', 'dir', 'fs', 'expect', 'out']) if (!opts[required]) { console.error(`missing --${required}`); process.exit(2); }
const javelin = resolve(opts.javelin);
const script = javelin.endsWith('.mjs');
const root = resolve(opts.dir);
const only = opts.only ? new Set(opts.only.split(',')) : null;
const bulkFiles = Number(opts['bulk-mb'] || 128) / 2;
const shims = opts.shims ? resolve(opts.shims) : null;
const env = { ...process.env, PATH: shims ? `${shims}${win ? ';' : ':'}${process.env.PATH}` : process.env.PATH };
const report = { harness: 'gate2', fs: opts.fs, expect: opts.expect, platform: process.platform, javelin, started: new Date().toISOString(), checks: [], commands: [], lingering: [] };
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const sleep = ms => new Promise(r => setTimeout(r, ms));

function privilege() {
  if (win) {
    const out = spawnSync('whoami', ['/groups', '/fo', 'csv', '/nh'], { encoding: 'utf8' }).stdout || '';
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
  try {
    const detail = await fn();
    report.checks.push({ clause, id, verdict: 'VERIFIED', detail: detail ?? null, ms: Date.now() - started });
    console.log(`VERIFIED      ${clause}/${id}`);
  } catch (e) {
    report.checks.push({ clause, id, verdict: 'NOT VERIFIED', detail: e.message, ms: Date.now() - started });
    console.log(`NOT VERIFIED  ${clause}/${id}: ${e.message}`);
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
  create: async (repo, id, extra = []) => {
    const r = await ok(['workspace', 'create', id, ...extra, '--repo', repo]);
    const path = find(r.json, k => k === 'path');
    layerPaths.set(`${repo}\0${id}`, path);
    return { path, strategy: find(r.json, k => k === 'strategy'), r };
  },
  path: (repo, id) => jv(['workspace', 'path', id, '--repo', repo]),
  createArgs: (repo, id) => ['workspace', 'create', id, '--repo', repo],
  // An ordinary read; the product must capture first (every javelin command captures).
  state: async (repo, id) => hexUnder((await ok(['layer', 'state', id, '--repo', repo])).json, /checkpoint/i),
  diff: async (repo, id) => { const r = await ok(['layer', 'diff', id, '--repo', repo]); return (find(r.json, k => k === 'changes', Array.isArray) || []).map(c => c.path); },
  // The agent lifecycle hook: run from inside the Layer directory, as an agent tool would.
  checkpointArgs: (repo, id) => ['checkpoint', id, '--repo', repo],
  checkpointCwd: (repo, id) => layerPaths.get(`${repo}\0${id}`),
  checkpoint: (repo, id) => jv(S.checkpointArgs(repo, id), S.checkpointCwd(repo, id)),
  capture: async (repo, id) => hexUnder((await ok(S.checkpointArgs(repo, id), S.checkpointCwd(repo, id))).json, /checkpoint/i),
  publish: (repo, id, op) => jv(['workspace', 'publish', id, '--operation', op, '--repo', repo]),
  discardArgs: (repo, id) => ['workspace', 'discard', id, '--repo', repo],
  discard: (repo, id) => ok(['workspace', 'discard', id, '--repo', repo]),
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
async function seed(name, bulk) {
  const repo = join(root, name);
  fs.mkdirSync(repo);
  await S.init(repo);
  const { path } = await S.create(repo, 'seed');
  writeFixture(path, bulk);
  const r = await S.publish(repo, 'seed', 'seed-publish');
  assert(r.code === 0, `seed publish failed: ${r.stderr}`);
  return repo;
}
let snapshots = 0;
async function snapshot(repo, checkpoint) {
  const id = `snapshot-${++snapshots}`;
  const { path } = await S.create(repo, id, ['--checkpoint', checkpoint]);
  const t = tree(path, noBuild);
  await S.discard(repo, id);
  return t;
}
const fileHash = text => `f:${sha(text)}`;
function freeBytes() {
  if (!win) spawnSync('sync');
  const s = fs.statfsSync(root);
  return Number(s.bavail) * Number(s.bsize);
}
async function stableFree() {
  let last = freeBytes();
  for (let i = 0; i < 20; i++) { await sleep(500); const now = freeBytes(); if (Math.abs(now - last) < (1 << 20)) return now; last = now; }
  return last;
}
function explicitRefusal(r, needle) {
  const text = `${r.stdout}\n${r.stderr}\n${r.json?.error?.message ?? ''}\n${r.json?.error?.path ?? ''}`;
  const named = [needle, needle.toLowerCase(), needle.normalize('NFC'), needle.normalize('NFD')].some(n => text.includes(n)) || text.toLowerCase().includes(needle.toLowerCase());
  return r.code !== 0 && r.json?.ok === false && Boolean(r.json?.error?.code) && named;
}
const longPath = p => (win ? `\\\\?\\${resolve(p)}` : p);

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

  const repo = await seed('small', false);
  const world = await S.head(repo);
  const a = await S.create(repo, 'layer-a'), b = await S.create(repo, 'layer-b');
  const pristine = tree(a.path);

  await check('tools', 'layer-on-repository-volume', () => {
    assert(fs.statSync(a.path).dev === fs.statSync(repo).dev, `Layer ${a.path} is not on the repository volume`);
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
    let running = true; done.then(() => { running = false; });
    while (running) {
      const r = await S.checkpoint(repo, 'layer-a');
      assert(r.code === 0 || (r.json?.ok === false && r.json?.error?.code), `capture during rewrites crashed: exit ${r.code} ${r.stderr.slice(0, 300)}`);
      const cp = r.code === 0 ? hexUnder(r.json, /checkpoint/i) : null;
      if (cp && !during.includes(cp)) during.push(cp);
    }
    assert(await done === 0, 'writer failed');
    const snap = await snapshot(repo, await S.capture(repo, 'layer-a'));
    for (const f of ['inplace.txt', 'atomic.txt']) assert(snap[`hot/${f}`] === fileHash(fs.readFileSync(join(a.path, 'hot', f))), `final hot/${f} not captured after rewrites stopped`);
    // Atomic saves replace whole files; every captured version must be a complete one.
    let inspected = 0;
    for (const cp of during.slice(0, 4)) {
      const id = `torn-${cp.slice(0, 12)}`;
      const { path: p } = await S.create(repo, id, ['--checkpoint', cp]);
      const file = join(p, 'hot/atomic.txt');
      const text = fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
      await S.discard(repo, id);
      if (text === null) continue;
      const [body, sum] = text.split('\nsum ');
      assert(sha(body) === sum?.trim(), `torn atomic-save capture in checkpoint ${cp}`);
      inspected++;
    }
    assert(inspected >= 2, `only ${inspected} checkpoints captured during rewrites contained the atomic file`);
    return { checkpointsDuringRewrites: during.length, inspected, finalGeneration: finalGen.trim() };
  });
  await check('tools', 'layer-clone-is-an-isolated-snapshot', async () => {
    const cp = await S.capture(repo, 'layer-a');
    const d = await S.create(repo, 'layer-d', ['--checkpoint', cp]);
    sameTree(tree(a.path, noBuild), tree(d.path, noBuild), 'clone vs source checkpoint');
    fs.writeFileSync(join(a.path, 'src/value.txt'), 'after clone\n');
    fs.writeFileSync(join(d.path, 'README.md'), 'clone edit\n');
    assert(fs.readFileSync(join(d.path, 'src/value.txt'), 'utf8') !== 'after clone\n', 'source edit leaked into clone');
    assert(fs.readFileSync(join(a.path, 'README.md'), 'utf8') !== 'clone edit\n', 'clone edit leaked into source');
    await S.discard(repo, 'layer-d');
  });
  await check('tools', 'fresh-layer-is-pristine', async () => {
    const c = await S.create(repo, 'layer-c', ['--world', String(world)]);
    sameTree(pristine, tree(c.path), 'fresh Layer after peer edits');
    await S.discard(repo, 'layer-c');
  });

  // Portability: Worlds every supported OS can materialize. Refusal must be explicit.
  async function refuse(label, needle, setup) {
    const before = await S.head(repo);
    const id = `refuse-${label}`;
    const { path } = await S.create(repo, id);
    const cleanup = setup(path);
    const cp = await S.checkpoint(repo, id);
    let refused = explicitRefusal(cp, needle);
    let published = null;
    if (!refused) { published = await S.publish(repo, id, `${id}-publish`); refused = explicitRefusal(published, needle); }
    const after = await S.head(repo);
    cleanup();
    if (published?.code !== 0) await S.discard(repo, id);
    assert(refused, `${label} was not refused explicitly (checkpoint exit ${cp.code}, publish exit ${published?.code}): ${(published ?? cp).stderr.slice(0, 300) || (published ?? cp).stdout.slice(0, 300)}`);
    assert(after === before, `World advanced from ${before} to ${after} despite ${label}`);
    return label;
  }
  async function crossLayer(label, first, second) {
    const p = await S.create(repo, `${label}-first`), q = await S.create(repo, `${label}-second`);
    fs.writeFileSync(join(p.path, first), 'first\n');
    const r = await S.publish(repo, `${label}-first`, `${label}-first`);
    assert(r.code === 0, `publishing ${first} failed: ${r.stderr.slice(0, 300)}`);
    const before = await S.head(repo);
    fs.writeFileSync(join(q.path, second), 'second\n');
    const cp = await S.checkpoint(repo, `${label}-second`);
    const pub = explicitRefusal(cp, second.split('/').at(-1)) ? cp : await S.publish(repo, `${label}-second`, `${label}-second`);
    assert(explicitRefusal(pub, second.split('/').at(-1)) || explicitRefusal(pub, first.split('/').at(-1)), `${second} beside ${first} was not refused explicitly (exit ${pub.code}): ${(pub.stderr || pub.stdout).slice(0, 300)}`);
    assert(await S.head(repo) === before, 'World advanced with a colliding path');
    fs.rmSync(join(q.path, second));
    await S.discard(repo, `${label}-second`);
  }
  await check('refusals', 'case-collision', async () => {
    await crossLayer('case', 'src/Makefile', 'src/makefile');
    const probe = join(root, 'case-probe'); fs.mkdirSync(probe); fs.writeFileSync(join(probe, 'x'), ''); fs.writeFileSync(join(probe, 'X'), '');
    const sensitive = fs.readdirSync(probe).length === 2; fs.rmSync(probe, { recursive: true });
    if (sensitive) await refuse('case-same-layer', 'Dup.txt', p => { fs.writeFileSync(join(p, 'Dup.txt'), '1'); fs.writeFileSync(join(p, 'dup.txt'), '2'); return () => fs.rmSync(join(p, 'dup.txt')); });
    return { caseSensitiveVolume: sensitive };
  });
  await check('refusals', 'unicode-normalization-collision', () => crossLayer('nfc', 'docs/café.md', 'docs/café.md'));
  await check('refusals', 'windows-reserved-names', async () => {
    const names = ['CON', 'aux.txt', 'nul.c', 'COM1.h', 'LPT9.log', 'trailing.', 'trailing ', ...(win ? [] : ['a:b.txt', 'q?.txt', 'back\\slash.txt'])];
    const tested = [];
    for (const name of names) {
      let made = false;
      await refuse(`reserved-${tested.length}-${names.indexOf(name)}`, name.trim() || name, p => {
        try { fs.writeFileSync(longPath(join(p, name)), 'x'); made = fs.readdirSync(p).includes(name); } catch { made = false; }
        if (!made) { try { fs.rmSync(longPath(join(p, name)), { force: true }); } catch {} }
        return () => { if (made) fs.rmSync(longPath(join(p, name)), { force: true }); };
      }).then(() => tested.push(name), e => { if (made) throw e; });
    }
    assert(tested.length >= 5, `only ${tested.length} reserved names could be created and tested: ${tested.join(', ')}`);
    return { tested };
  });
  await S.integrity(repo);

  // Shared data, measured by free-space deltas against a byte-copy control on the same volume.
  if (want('sharing') || want('crash')) {
    const bulkRepo = await seed('bulk', true);
    const bulkBytes = bulkFiles * (2 << 20);
    const f0 = await stableFree();
    const l1 = await S.create(bulkRepo, 'bulk-1');
    const f1 = await stableFree();
    const worldTree = tree(l1.path);
    const control = join(root, 'control-copy');
    fs.mkdirSync(control);
    for (const name of fs.readdirSync(join(l1.path, 'bulk'))) fs.writeFileSync(join(control, name), fs.readFileSync(join(l1.path, 'bulk', name)));
    const f2 = await stableFree();
    fs.rmSync(control, { recursive: true });
    const fb = await stableFree();
    fs.writeFileSync(join(l1.path, 'bulk', 'blob-0.bin'), randomBytes(2 << 20));
    const fe = await stableFree();
    await S.capture(bulkRepo, 'bulk-1');
    const fc = await stableFree();
    const textCreate = spawnSync(argvFor(S.createArgs(bulkRepo, 'bulk-2'))[0], argvFor(S.createArgs(bulkRepo, 'bulk-2')).slice(1), { env, encoding: 'utf8' });
    const f3 = await stableFree();
    const mb = n => Math.round(n / 1048576);
    const deltas = { bulkMB: mb(bulkBytes), createMB: mb(f0 - f1), secondCreateMB: mb(fc - f3), controlCopyMB: mb(f1 - f2), editOneFileMB: mb(fb - fe), captureMB: mb(fe - fc) };
    report.disk = { strategy: l1.strategy, deltas };
    await check('sharing', 'byte-copy-control-is-measurable', () => {
      assert(f1 - f2 >= 0.8 * bulkBytes, `a ${mb(bulkBytes)} MB byte copy freed only ${mb(f1 - f2)} MB; free-space deltas are not trustworthy here`);
      return deltas;
    });
    await check('sharing', opts.expect === 'shared' ? 'unchanged-data-shared-with-base' : 'copy-fallback-duplicates-data', () => {
      if (opts.expect === 'shared') assert(f0 - f1 <= 0.2 * bulkBytes && fc - f3 <= 0.2 * bulkBytes, `Layer creation used ${mb(f0 - f1)} and ${mb(fc - f3)} MB for ${mb(bulkBytes)} MB of unchanged data`);
      else assert(f0 - f1 >= 0.8 * bulkBytes, `copy fallback used only ${mb(f0 - f1)} MB for ${mb(bulkBytes)} MB`);
      return deltas;
    });
    await check('sharing', 'edit-and-capture-stay-sparse', () => {
      assert(fb - fe <= 0.2 * bulkBytes, `rewriting one 2 MB file used ${mb(fb - fe)} MB`);
      assert(fe - fc <= 0.2 * bulkBytes, `capturing one changed file used ${mb(fe - fc)} MB`);
      return deltas;
    });
    await check('sharing', 'strategy-reported', () => {
      assert(typeof l1.strategy === 'string' && l1.strategy, `no strategy in ${JSON.stringify(l1.r.json).slice(0, 300)}`);
      const copy = /copy/i.test(l1.strategy);
      assert(copy === (opts.expect === 'copy'), `strategy ${l1.strategy} on ${opts.fs}, expected ${opts.expect}`);
      if (copy) assert(/cop/i.test(textCreate.stdout + textCreate.stderr), `human output does not report the copy: ${textCreate.stdout}`);
      return { strategy: l1.strategy, text: (textCreate.stdout + textCreate.stderr).trim().slice(0, 300) };
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
    await check('crash', 'interrupted-create-recovers', async () => {
      const duration = await timed(S.createArgs(bulkRepo, 'time-create'));
      await S.discard(bulkRepo, 'time-create');
      let landed = 0;
      for (const [i, delay] of points(duration).entries()) {
        const id = `kill-create-${i}`;
        if (await killed(S.createArgs(bulkRepo, id), delay)) landed++;
        await S.head(bulkRepo);
        const p = await S.path(bulkRepo, id);
        if (p.code === 0) sameTree(worldTree, tree(find(p.json, k => k === 'path')), `Layer ${id} reported after an interrupted create`);
        else sameTree(worldTree, tree((await S.create(bulkRepo, id)).path), `retried create ${id}`);
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
      const t = await S.create(bulkRepo, 'time-discard');
      const duration = await timed(S.discardArgs(bulkRepo, 'time-discard'));
      assert(!fs.existsSync(t.path), 'discarded Layer path still exists');
      let landed = 0;
      for (const [i, delay] of points(duration).entries()) {
        const id = `kill-discard-${i}`, marker = `gate2-marker-${randomBytes(6).toString('hex')}`;
        const { path } = await S.create(bulkRepo, id);
        fs.writeFileSync(join(path, marker), 'marker\n');
        const before = tree(path);
        if (await killed(S.discardArgs(bulkRepo, id), delay)) landed++;
        await S.head(bulkRepo);
        const p = await S.path(bulkRepo, id);
        if (p.code === 0) { sameTree(before, tree(path), `Layer ${id} still reported after an interrupted discard`); await S.discard(bulkRepo, id); }
        else assert(!fs.existsSync(path) || fs.readdirSync(path).length === 0, `half-discarded Layer left at ${path}`);
        await S.head(bulkRepo); await S.head(bulkRepo);
        const left = [];
        (function search(d) { for (const e of fs.readdirSync(d, { withFileTypes: true })) { if (e.name === marker) left.push(join(d, e.name)); else if (e.isDirectory() && !e.isSymbolicLink()) search(join(d, e.name)); } })(root);
        assert(left.length === 0, `discarded data not reclaimed: ${left.join(', ')}`);
      }
      assert(landed >= 2, `only ${landed} kills landed inside a ${duration} ms discard`);
      return { durationMs: duration, landed };
    });
    await check('crash', 'repository-integrity-after-interruptions', () => S.integrity(bulkRepo).then(() => true));
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
const failed = report.checks.filter(c => c.verdict !== 'VERIFIED');
report.verdict = exitCode === 2 ? 'INCONCLUSIVE' : failed.length ? 'NOT VERIFIED' : 'VERIFIED';
fs.writeFileSync(opts.out, `${JSON.stringify(report, null, 2)}\n`);
console.log(`gate 2 on ${opts.fs}: ${report.verdict} (${report.checks.length - failed.length}/${report.checks.length} checks, ${report.commands.length} javelin commands)`);
process.exit(exitCode || (failed.length ? 1 : 0));
