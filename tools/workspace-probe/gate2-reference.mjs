// Reference model of the Layer command surface that gate2.mjs drives. It lets the
// harness run, and be mutation-tested through JAVELIN_REFERENCE_LIE, before the real
// clone-backed Layers and daemonless capture land. It is not product code.
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
  if (group === 'history') { const s = store(action); recover(s); return { command: 'history', versions: worlds(s).map((w, number) => ({ number, manifest: sha(JSON.stringify(w)) })) }; }
  if (group === 'check') { const s = store(action); recover(s); return { command: 'check', blobs: verifyBlobs(s) }; }
  const s = store(opt('--repo') || process.cwd());
  recover(s);
  if (group === 'checkpoint') return { command: 'checkpoint', checkpoint: capture(s, active(s, action)) };
  if (group === 'layer' && action === 'state') { const l = active(s, id); const checkpoint = capture(s, l); return { command: 'layer state', state: { state: 'active', head: { layer: id, revision: l.checkpoints.length, checkpoint } } }; }
  if (group === 'layer' && action === 'diff') { const l = active(s, id); const head = readJson(join(s.dir, 'checkpoints', `${capture(s, l)}.json`)); const base = worlds(s)[l.world]; return { command: 'layer diff', changes: changed(base, head).map(path => ({ path })) }; }
  if (group !== 'workspace') fail('invalid_input', `unknown command ${group}`);
  if (action === 'create') return create(s, id);
  if (action === 'path') { const l = active(s, id); return { command: 'workspace path', path: l.path, strategy: l.strategy }; }
  if (action === 'publish') return publish(s, active(s, id), opt('--operation'));
  if (action === 'discard') return discard(s, id);
  fail('invalid_input', `unknown workspace command ${action}`);
}

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
    if (l.state === 'creating') { fs.rmSync(l.path, { recursive: true, force: true }); fs.rmSync(join(s.dir, 'layers', name)); }
    else if (l.state === 'discarding') writeJson(join(s.dir, 'layers', name), { ...l, state: 'discarded' });
  }
  for (const name of fs.readdirSync(join(s.dir, 'tmp'))) fs.rmSync(join(s.dir, 'tmp', name), { recursive: true, force: true });
  for (const name of fs.readdirSync(join(s.dir, 'trash'))) fs.rmSync(join(s.dir, 'trash', name), { recursive: true, force: true });
}

function walk(root) {
  const out = {};
  (function visit(dir) {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, e.name), path = relative(root, full).split(sep).join('/');
      if (e.isSymbolicLink()) out[path] = { type: 'symlink', target: fs.readlinkSync(full).split(sep).join('/'), dir: Boolean(fs.statSync(full, { throwIfNoEntry: false })?.isDirectory()) };
      else if (e.isDirectory()) visit(full);
      else if (e.isFile()) out[path] = { type: 'file', full, stat: fs.statSync(full) };
    }
  })(root);
  return out;
}

const RESERVED = /^(con|prn|aux|nul|com[0-9¹²³]|lpt[0-9¹²³])(\..*)?$/i;
function portability(paths) {
  if (lie === 'no-refusal') return;
  const seen = new Map();
  for (const path of paths) {
    for (const part of path.split('/')) {
      if (RESERVED.test(part) || /[. ]$/.test(part) || /[<>:"|?*\\\x00-\x1f]/.test(part)) fail('unportable_path', `path ${JSON.stringify(path)} is not portable to Windows`, path);
    }
    const key = path.normalize('NFC').toLowerCase();
    if (seen.has(key)) fail('unportable_path', `paths ${JSON.stringify(seen.get(key))} and ${JSON.stringify(path)} collide on case-insensitive or normalizing filesystems`, path);
    seen.set(key, path);
  }
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
  portability(Object.keys(next));
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

function materialize(s, m, dir) {
  fs.mkdirSync(dir, { recursive: true });
  for (const [path, e] of Object.entries(m)) {
    const full = join(dir, ...path.split('/'));
    fs.mkdirSync(join(full, '..'), { recursive: true });
    if (e.type === 'symlink') {
      const resolved = posix.normalize(posix.join(posix.dirname(path), e.target));
      const isDir = Object.keys(m).some(p => p.startsWith(`${resolved}/`));
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

function create(s, id) {
  if (fs.existsSync(layerFile(s, id))) fail('layer_exists', `Layer ${id} exists`);
  const all = worlds(s);
  let world = opt('--world') === undefined ? all.length - 1 : Number(opt('--world'));
  let m = all[world];
  const from = opt('--checkpoint');
  if (from) {
    m = readJson(join(s.dir, 'checkpoints', `${from}.json`));
    const owner = layerNames(s).map(n => readJson(join(s.dir, 'layers', n))).find(o => o.checkpoints.includes(from));
    world = owner ? owner.world : world;
  }
  const path = join(s.root, '.javelin-reference-layers', id);
  fs.mkdirSync(join(path, '..'), { recursive: true });
  const record = { id, world, path, state: 'creating', checkpoints: [] };
  writeJson(layerFile(s, id), record);
  let strategy;
  if (from) { materialize(s, m, path); strategy = 'materialize'; }
  else strategy = cloneTree(join(s.dir, 'bases', String(world)), path);
  const checkpoint = sha(JSON.stringify(m));
  if (!fs.existsSync(join(s.dir, 'checkpoints', `${checkpoint}.json`))) writeJson(join(s.dir, 'checkpoints', `${checkpoint}.json`), m);
  writeJson(layerFile(s, id), { ...record, state: 'active', strategy, checkpoints: [checkpoint] });
  if (lie === 'daemon') spawn(process.execPath, [process.argv[1], '__linger'], { detached: true, stdio: 'ignore' }).unref();
  const note = strategy === 'copy' ? 'copied every file: this filesystem has no clone support' : `cloned with ${strategy}`;
  return { command: 'workspace create', layer: id, path, strategy, note, checkpoint };
}

function publish(s, l, operation) {
  if (!operation) fail('invalid_input', 'publish needs --operation');
  const opsFile = join(s.dir, 'operations.json');
  const ops = fs.existsSync(opsFile) ? readJson(opsFile) : {};
  if (ops[operation]) return ops[operation];
  capture(s, l);
  const head = manifest(s, readJson(layerFile(s, l.id)));
  const all = worlds(s), base = all[l.world], current = all.at(-1);
  const paths = changed(base, head);
  const conflicts = paths.filter(p => JSON.stringify(base[p] ?? null) !== JSON.stringify(current[p] ?? null));
  if (conflicts.length) fail('conflict', `World changed ${conflicts.join(', ')}`);
  const next = { ...current };
  for (const p of paths) { if (head[p]) next[p] = head[p]; else delete next[p]; }
  portability(Object.keys(next));
  const version = all.length;
  materialize(s, next, join(s.dir, 'bases', String(version)));
  writeJson(join(s.dir, 'worlds.json'), [...all, next]);
  writeJson(layerFile(s, l.id), { ...readJson(layerFile(s, l.id)), state: 'published' });
  const out = { command: 'workspace publish', receipt: { operation, version } };
  writeJson(opsFile, { ...ops, [operation]: out });
  return out;
}

function discard(s, id) {
  if (!fs.existsSync(layerFile(s, id))) fail('not_found', `no Layer ${id}`);
  const l = readJson(layerFile(s, id));
  if (l.state === 'discarded') return { command: 'workspace discard', layer: id, state: 'discarded' };
  if (lie === 'torn-discard') {
    for (const e of Object.values(walk(l.path))) if (e.full) fs.rmSync(e.full, { force: true });
    fs.rmSync(l.path, { recursive: true, force: true });
  } else {
    writeJson(layerFile(s, id), { ...l, state: 'discarding' });
    if (fs.existsSync(l.path)) fs.renameSync(l.path, join(s.dir, 'trash', `${id}-${randomBytes(4).toString('hex')}`));
  }
  writeJson(layerFile(s, id), { ...l, state: 'discarded' });
  recover(s);
  return { command: 'workspace discard', layer: id, state: 'discarded' };
}

if (argv[0] === '__linger') { setTimeout(() => {}, 30000); } else {
  try { const out = main(argv); console.log(JSON.stringify({ ok: true, ...out })); }
  catch (e) {
    console.error(JSON.stringify({ ok: false, error: { code: e.code || 'internal', message: e.message, path: e.path } }));
    process.exit(e.code === 'invalid_input' ? 2 : 1);
  }
}
