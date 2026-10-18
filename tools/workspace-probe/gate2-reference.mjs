// Reference model of the clone-backed workspace command surface that gate2.mjs drives.
// It shows the harness passes against an honest implementation on each filesystem, and
// is mutation-tested through JAVELIN_REFERENCE_LIE (gate2-controls.mjs). Not product code.
import * as fs from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { join, posix, relative, sep } from 'node:path';

const lie = process.env.JAVELIN_REFERENCE_LIE || '';
const argv = process.argv.slice(2).filter(a => a !== '--json');

function fail(code, message, path) { throw Object.assign(new Error(message), { code, path }); }
function opt(name) { const i = argv.indexOf(name); return i < 0 ? undefined : argv[i + 1]; }
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
function writeJson(path, value) {
  const tmp = `${path}.${randomBytes(4).toString('hex')}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value)); fs.renameSync(tmp, path);
}
const readJson = path => JSON.parse(fs.readFileSync(path, 'utf8'));

function main([group, action, id]) {
  if (group === 'init') { init(action); return { command: 'init' }; }
  if (group === 'seed-world') return seedWorld(store(action), id ?? argv[2]);
  if (group === 'history') { const s = store(action); return { command: 'history', versions: worlds(s).map((w, number) => ({ number, manifest: sha(JSON.stringify(w)) })) }; }
  if (group === 'check') { const s = store(action); return { command: 'check', blobs: verifyBlobs(s) }; }
  const s = store(opt('--repo') || process.cwd());
  recover(s);
  if (group === 'checkpoint') return { command: 'checkpoint', state: head(s, action, capture(s, active(s, action))) };
  // Every command naming a Layer captures its directory first, as javelin does.
  const named = id && fs.existsSync(layerFile(s, id)) ? readJson(layerFile(s, id)) : null;
  if (named?.state === 'active' && fs.existsSync(named.path)) capture(s, named);
  if (group === 'layer' && action === 'state') {
    if (named?.state === 'discarded') return { command: 'layer state', state: { state: 'discarded', layer: id, checkpoint: named.checkpoints.at(-1) } };
    return { command: 'layer state', state: head(s, id, active(s, id).checkpoints.at(-1)) };
  }
  if (group === 'layer' && action === 'diff') { const l = active(s, id); const base = worlds(s)[l.world]; return { command: 'layer diff', changes: changed(base, manifest(s, l)).map(path => ({ path })) }; }
  if (group === 'layer' && action === 'clone') return clone(s, id, opt('--checkpoint'));
  if (group !== 'workspace') fail('invalid_input', `unknown command ${group}`);
  if (action === 'create') return create(s, id);
  if (action === 'open') return { command: 'workspace open', workspace: open(s, active(s, id)) };
  if (action === 'path') { const l = readJson(layerFile(s, id)); return { command: 'workspace path', workspace: { layer: id, root: l.path, exists: fs.existsSync(l.path) } }; }
  if (action === 'discard') return discard(s, id, opt('--operation'));
  if (action === 'recover') return { command: 'workspace recover' };
  fail('invalid_input', `unknown workspace command ${action}`);
}
const head = (s, id, checkpoint) => ({ state: 'active', head: { layer: id, revision: readJson(layerFile(s, id)).checkpoints.length - 1, checkpoint } });

function init(root) {
  const dir = join(root, '.javelin-reference');
  for (const d of ['blobs', 'layers', 'checkpoints', 'bases', 'trash', 'tmp']) fs.mkdirSync(join(dir, d), { recursive: true });
  writeJson(join(dir, 'worlds.json'), [{}]);
  fs.mkdirSync(join(dir, 'bases', '0'));
}
function store(root) {
  const dir = join(root, '.javelin-reference');
  if (!fs.existsSync(dir)) fail('not_a_repository', `no repository at ${root}`);
  return { root, dir };
}
const worlds = s => readJson(join(s.dir, 'worlds.json'));
const layerNames = s => fs.readdirSync(join(s.dir, 'layers')).filter(n => n.endsWith('.json') && !n.endsWith('.cache.json'));
const layerFile = (s, id) => join(s.dir, 'layers', `${id}.json`);
function active(s, id) {
  if (!fs.existsSync(layerFile(s, id))) fail('not_found', `no Layer ${id}`);
  const l = readJson(layerFile(s, id));
  if (l.state !== 'active') fail('not_active', `Layer ${id} is ${l.state}`);
  return l;
}
const manifest = (s, l) => readJson(join(s.dir, 'checkpoints', `${l.checkpoints.at(-1)}.json`));

// Roll interrupted work forward or back, then reclaim trash. Runs at every command.
function recover(s) {
  for (const name of layerNames(s)) {
    const l = readJson(join(s.dir, 'layers', name));
    if (l.state === 'discarding') {
      if (fs.existsSync(l.path)) fs.renameSync(l.path, join(s.dir, 'trash', `${l.id}-${randomBytes(4).toString('hex')}`));
      writeJson(join(s.dir, 'layers', name), { ...l, state: 'discarded' });
    }
  }
  for (const name of fs.readdirSync(join(s.dir, 'tmp'))) fs.rmSync(join(s.dir, 'tmp', name), { recursive: true, force: true });
  for (const name of fs.readdirSync(join(s.dir, 'trash'))) fs.rmSync(join(s.dir, 'trash', name), { recursive: true, force: true });
}

function walk(root) {
  const out = {};
  (function visit(dir) {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, e.name), path = relative(root, full).split(sep).join('/');
      if (e.isSymbolicLink()) out[path] = { type: 'symlink', target: fs.readlinkSync(full).split(sep).join('/'), dir: (() => { try { return fs.statSync(full).isDirectory(); } catch { return false; } })() };
      else if (e.isDirectory()) visit(full);
      else if (e.isFile()) out[path] = { type: 'file', full, stat: fs.statSync(full) };
    }
  })(root);
  return out;
}

function capture(s, l) {
  const cacheFile = join(s.dir, 'layers', `${l.id}.cache.json`);
  const cache = lie === 'stat-only' && fs.existsSync(cacheFile) ? readJson(cacheFile) : {};
  const next = {}, newCache = {};
  for (const [path, e] of Object.entries(walk(l.path))) {
    if (e.type === 'symlink') { next[path] = e; continue; }
    const key = `${e.stat.size}:${e.stat.mtimeMs}`;
    let hash = cache[path]?.key === key ? cache[path].hash : undefined;
    if (!hash) {
      const bytes = fs.readFileSync(e.full);
      hash = sha(bytes);
      const blob = join(s.dir, 'blobs', hash);
      if (!fs.existsSync(blob)) { const tmp = join(s.dir, 'tmp', randomBytes(6).toString('hex')); fs.writeFileSync(tmp, bytes); fs.renameSync(tmp, blob); }
    }
    newCache[path] = { key, hash };
    next[path] = { type: 'file', hash, exec: process.platform !== 'win32' && (e.stat.mode & 0o100) !== 0 };
  }
  if (lie === 'stat-only') writeJson(cacheFile, newCache);
  if (lie === 'touch-layer') fs.writeFileSync(join(l.path, '.reference-cache'), String(Date.now()));
  const id = sha(JSON.stringify(next));
  if (id === l.checkpoints.at(-1)) return id;
  const file = join(s.dir, 'checkpoints', `${id}.json`);
  if (!fs.existsSync(file)) writeJson(file, next);
  writeJson(layerFile(s, l.id), { ...l, checkpoints: [...l.checkpoints, id] });
  return id;
}

function verifyBlobs(s) {
  const manifests = [...worlds(s), ...fs.readdirSync(join(s.dir, 'checkpoints')).filter(n => n.endsWith('.json')).map(n => readJson(join(s.dir, 'checkpoints', n)))];
  const hashes = new Set(manifests.flatMap(m => Object.values(m).filter(e => e.type === 'file').map(e => e.hash)));
  for (const h of hashes) if (sha(fs.readFileSync(join(s.dir, 'blobs', h))) !== h) fail('corrupt', `blob ${h} is corrupt`);
  return hashes.size;
}

function changed(a, b) {
  return [...new Set([...Object.keys(a), ...Object.keys(b)])].filter(p => JSON.stringify(a[p] ?? null) !== JSON.stringify(b[p] ?? null)).sort();
}

// `all` is the whole tree, so a symlink to a directory outside `m` still gets its type.
function materialize(s, m, dir, all = m) {
  fs.mkdirSync(dir, { recursive: true });
  for (const [path, e] of Object.entries(m)) {
    const full = join(dir, ...path.split('/'));
    fs.mkdirSync(join(full, '..'), { recursive: true });
    if (e.type === 'symlink') {
      const resolved = posix.normalize(posix.join(posix.dirname(path), e.target));
      const isDir = Object.keys(all).some(p => p.startsWith(`${resolved}/`));
      fs.symlinkSync(e.target, full, isDir ? 'dir' : 'file');
    } else { fs.copyFileSync(join(s.dir, 'blobs', e.hash), full); if (e.exec) fs.chmodSync(full, 0o755); }
  }
}

function cloneTree(source, target) {
  if (lie === 'hardlink') {
    for (const [path, e] of Object.entries(walk(source))) {
      const full = join(target, ...path.split('/'));
      fs.mkdirSync(join(full, '..'), { recursive: true });
      if (e.type === 'symlink') fs.symlinkSync(e.target, full, e.dir ? 'dir' : 'file'); else fs.linkSync(e.full, full);
    }
    return 'reflink';
  }
  // Read and write every byte, so no filesystem can share the data behind our back.
  // Windows uses it too: CopyFile may block-clone on ReFS, which would blur an honest 'copy'.
  if (lie === 'copy-claims-clone' || process.platform === 'win32') {
    for (const [path, e] of Object.entries(walk(source))) {
      const full = join(target, ...path.split('/'));
      fs.mkdirSync(join(full, '..'), { recursive: true });
      if (e.type === 'symlink') fs.symlinkSync(e.target, full, e.dir ? 'dir' : 'file'); else fs.writeFileSync(full, fs.readFileSync(e.full), { mode: e.stat.mode });
    }
    return lie ? 'clonefile' : 'copy';
  }
  if (process.platform === 'darwin') { cp(['-cR', source, target]); return 'clonefile'; }
  if (spawnSync('cp', ['-a', '--reflink=always', source, target]).status === 0) return 'reflink';
  fs.rmSync(target, { recursive: true, force: true });
  cp(['-a', source, target]);
  return 'copy';
}
function cp(args) { const r = spawnSync('cp', args, { stdio: 'inherit' }); if (r.status !== 0) fail('clone_failed', `cp ${args.join(' ')} failed`); }

// The next World holds exactly the spec's paths (gate2_seed's spec format).
function seedWorld(s, specFile) {
  const spec = readJson(specFile), next = {};
  for (const [path, e] of Object.entries(spec)) {
    if (e.link !== undefined) { next[path] = { type: 'symlink', target: e.link }; continue; }
    const bytes = e.from !== undefined ? fs.readFileSync(e.from) : Buffer.from(e.text);
    const hash = sha(bytes);
    fs.writeFileSync(join(s.dir, 'blobs', hash), bytes);
    next[path] = { type: 'file', hash, exec: Boolean(e.exec) };
  }
  const all = worlds(s);
  materialize(s, next, join(s.dir, 'bases', String(all.length)));
  writeJson(join(s.dir, 'worlds.json'), [...all, next]);
  return { version: all.length };
}

function create(s, id) {
  const all = worlds(s);
  const world = opt('--world') === undefined ? all.length - 1 : Number(opt('--world'));
  if (fs.existsSync(layerFile(s, id))) {
    const l = readJson(layerFile(s, id));
    if (l.world !== world) fail('invalid_input', `Layer ${id} exists from World ${l.world}`);
    return { command: 'workspace create', workspace: open(s, active(s, id)) };
  }
  return { command: 'workspace create', workspace: open(s, newLayer(s, id, world, all[world])) };
}
function clone(s, id, from) {
  const m = readJson(join(s.dir, 'checkpoints', `${from}.json`));
  const owner = layerNames(s).map(n => readJson(join(s.dir, 'layers', n))).find(o => o.checkpoints.includes(from));
  if (!fs.existsSync(layerFile(s, id))) newLayer(s, id, owner.world, m);
  return { command: 'layer clone', state: head(s, id, readJson(layerFile(s, id)).checkpoints[0]) };
}
function newLayer(s, id, world, m) {
  const checkpoint = sha(JSON.stringify(m));
  if (!fs.existsSync(join(s.dir, 'checkpoints', `${checkpoint}.json`))) writeJson(join(s.dir, 'checkpoints', `${checkpoint}.json`), m);
  const l = { id, world, path: join(s.root, '.javelin-reference-layers', id), state: 'active', checkpoints: [checkpoint] };
  writeJson(layerFile(s, id), l);
  return l;
}
// Clone the World base, apply the checkpoint, and refuse a tree this volume cannot hold.
function open(s, l) {
  if (fs.existsSync(l.path)) return { layer: l.id, root: l.path, created: false, clone: { strategy: l.strategy } };
  const staging = join(s.dir, 'tmp', `${l.id}-${randomBytes(4).toString('hex')}`);
  const strategy = cloneTree(join(s.dir, 'bases', String(l.world)), staging);
  const want = manifest(s, l), base = worlds(s)[l.world];
  const delta = changed(base, want);
  for (const p of delta) fs.rmSync(join(staging, ...p.split('/')), { force: true });
  materialize(s, Object.fromEntries(delta.filter(p => want[p]).map(p => [p, want[p]])), staging, want);
  if (lie !== 'no-refusal') {
    const got = walk(staging);
    const lost = [...new Set([...Object.keys(want), ...Object.keys(got)])].find(p => !want[p] || !got[p]);
    if (lost !== undefined) { fs.rmSync(staging, { recursive: true, force: true }); fail('unsupported', `path ${JSON.stringify(lost)} cannot be materialized here`, lost); }
  }
  fs.mkdirSync(join(l.path, '..'), { recursive: true });
  fs.renameSync(staging, l.path);
  writeJson(layerFile(s, l.id), { ...readJson(layerFile(s, l.id)), strategy });
  if (lie === 'daemon') spawn(process.execPath, [process.argv[1], '__linger'], { detached: true, stdio: 'ignore' }).unref();
  const warning = strategy === 'copy' ? 'Warning: this volume cannot clone files, so unchanged data was duplicated' : null;
  return { layer: l.id, root: l.path, created: true, clone: { strategy }, warning };
}

function discard(s, id, operation) {
  if (!operation) fail('invalid_input', 'discard needs --operation');
  if (!fs.existsSync(layerFile(s, id))) fail('not_found', `no Layer ${id}`);
  const l = readJson(layerFile(s, id));
  const out = { command: 'workspace discard', operation, update: { outcome: 'applied', state: { layer: id, state: 'discarded' } } };
  if (l.state === 'discarded') return out;
  if (lie === 'torn-discard') {
    for (const e of Object.values(walk(l.path))) if (e.full) fs.rmSync(e.full, { force: true });
    fs.rmSync(l.path, { recursive: true, force: true });
    writeJson(layerFile(s, id), { ...l, state: 'discarded' });
  } else {
    writeJson(layerFile(s, id), { ...l, state: 'discarding' });
    recover(s);
  }
  return out;
}

if (argv[0] === '__linger') { setTimeout(() => {}, 30000); } else {
  try { const out = main(argv); console.log(JSON.stringify({ ok: true, ...out })); }
  catch (e) {
    console.error(JSON.stringify({ ok: false, error: { code: e.code || 'internal', message: e.message, path: e.path } }));
    process.exit(e.code === 'invalid_input' ? 2 : 1);
  }
}
