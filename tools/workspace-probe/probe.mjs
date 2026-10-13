import * as fs from 'node:fs/promises';
import { watch, writeFileSync } from 'node:fs';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { basename, dirname, join, resolve } from 'node:path';
import { tmpdir, platform, arch, release, totalmem } from 'node:os';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const execute = promisify(execFile);
const binary = Buffer.from(Array.from({ length: 8192 }, (_, i) => i % 256));
const fixture = {
  'README.txt': 'shared base text\n',
  'bytes.bin': binary,
  'delete.txt': 'delete only in A\n',
  'dir/nested.txt': 'nested\n',
  'value.h': '#define VALUE 7\n',
  'main.c': '#include <stdio.h>\n#include "value.h"\nint main(void) { printf("%d\\n", VALUE); return 0; }\n',
};
const limits = { views: 3, maxEntriesPerTree: 64, maxBytesPerTree: 256 * 1024,
  commandTimeoutMs: 20000, watchTimeoutMs: 2000, writerTimeoutMs: 5000 };
const report = { schema: 1, startedAt: new Date().toISOString(), backend: null,
  host: { platform: platform(), arch: arch(), release: release(), memoryBytes: totalmem(), bun: Bun.version },
  limits, commands: [], checks: [], contract: [] };

async function command(argv, cwd, timeout = limits.commandTimeoutMs) {
  const started = performance.now();
  try {
    const result = await execute(argv[0], argv.slice(1), { cwd, timeout, maxBuffer: 16384, killSignal: 'SIGKILL' });
    report.commands.push({ argv, cwd, exitCode: 0, stdout: result.stdout, stderr: result.stderr,
      elapsedMs: Math.round(performance.now() - started) });
    return result.stdout;
  } catch (error) {
    report.commands.push({ argv, cwd, exitCode: error.code, signal: error.signal,
      stdout: error.stdout, stderr: error.stderr, elapsedMs: Math.round(performance.now() - started) });
    throw error;
  }
}

async function snapshot(root) {
  const rows = [];
  let bytes = 0;
  async function visit(relative) {
    const path = join(root, relative);
    const stat = await fs.lstat(path);
    assert.ok(rows.length < limits.maxEntriesPerTree, 'tree exceeds entry budget');
    if (stat.isSymbolicLink()) rows.push([relative, 'symlink', await fs.readlink(path)]);
    else if (stat.isDirectory()) {
      rows.push([relative, 'directory']);
      for (const name of (await fs.readdir(path)).sort()) await visit(join(relative, name));
    } else {
      assert.ok(stat.isFile(), `unsupported fixture entry ${relative}`);
      bytes += stat.size;
      assert.ok(bytes <= limits.maxBytesPerTree, 'tree exceeds byte budget');
      rows.push([relative, 'file', stat.size, createHash('sha256').update(await fs.readFile(path)).digest('hex')]);
    }
  }
  await visit('');
  return rows;
}

async function seed(root) {
  assert.equal((await fs.readdir(root)).length, 0, 'seed requires an empty directory');
  await fs.mkdir(join(root, 'dir'));
  for (const [name, bytes] of Object.entries(fixture)) await fs.writeFile(join(root, name), bytes);
  await fs.symlink('README.txt', join(root, 'inside-link'));
}

async function copyTree(source, target, cloneExecutable) {
  await fs.mkdir(target);
  for (const name of await fs.readdir(source)) {
    const from = join(source, name), to = join(target, name);
    const stat = await fs.lstat(from);
    if (stat.isSymbolicLink()) await fs.symlink(await fs.readlink(from), to);
    else if (stat.isDirectory()) await copyTree(from, to, cloneExecutable);
    else {
      if (cloneExecutable) await command([cloneExecutable, from, to]);
      else await fs.copyFile(from, to);
      await fs.chmod(to, 0o644);
    }
  }
}

async function check(id, run) {
  try {
    const evidence = await run();
    report.checks.push({ id, verdict: 'VERIFIED', evidence: evidence ?? 'assertions passed' });
  } catch (error) {
    report.checks.push({ id, verdict: error.code === 'ENOENT' && error.syscall?.startsWith('spawn')
      ? 'INCONCLUSIVE' : 'NOT VERIFIED', evidence: error.message });
  }
}

async function killWriter(target, phase, contents) {
  const argv = [process.execPath, join(here, 'atomic-writer.mjs'), target, phase, contents];
  const child = spawn(argv[0], argv.slice(1), { stdio: ['ignore', 'pipe', 'pipe'] });
  const exit = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  let timer;
  try {
    await new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error('writer readiness timed out')), limits.writerTimeoutMs);
      let output = '', errors = '';
      child.stdout.on('data', (data) => {
        output += data.toString();
        if (output.includes('ready\n')) resolve();
      });
      child.stderr.on('data', (data) => { errors = (errors + data.toString()).slice(-4096); });
      child.once('error', reject);
      child.once('exit', (code) => reject(new Error(`writer exited before readiness: ${code} ${errors}`)));
    });
    child.kill('SIGKILL');
    const result = await exit;
    assert.equal(result.signal, 'SIGKILL');
    report.commands.push({ argv, ...result, ready: true });
  } finally {
    clearTimeout(timer);
    child.kill('SIGKILL');
    await exit;
  }
}

async function behavior(base, a, b, c, clone) {
  const original = await snapshot(base);
  for (const root of [a, b, c]) assert.deepEqual(await snapshot(root), original, 'initial view differs from fixture');
  report.fixture = { entries: original.length, fileBytes: Object.values(fixture).reduce((n, x) => n + Buffer.byteLength(x), 0), snapshot: original };
  const stat = await fs.statfs(a);
  report.host.testFilesystem = { type: stat.type, blockSize: stat.bsize, availableBytes: stat.bavail * stat.bsize };
  await check('text-and-binary-read', async () => {
    assert.equal(await command(['cat', 'README.txt'], a), fixture['README.txt']);
    assert.deepEqual(await fs.readFile(join(a, 'bytes.bin')), binary);
    await command(['cmp', join(base, 'bytes.bin'), join(a, 'bytes.bin')]);
  });
  await check('private-in-place-writes', async () => {
    await fs.writeFile(join(a, 'README.txt'), 'private A text\n');
    const bytes = Buffer.from(binary); bytes[4096] = 99;
    const file = await fs.open(join(a, 'bytes.bin'), 'r+');
    try { await file.write(Buffer.from([99]), 0, 1, 4096); } finally { await file.close(); }
    assert.deepEqual(await fs.readFile(join(a, 'bytes.bin')), bytes);
    assert.equal(await fs.readFile(join(a, 'README.txt'), 'utf8'), 'private A text\n');
    assert.deepEqual(await snapshot(base), original);
    assert.deepEqual(await snapshot(b), original);
  });
  await check('create-atomic-save-rename-delete', async () => {
    await fs.writeFile(join(a, '.README.saving'), 'atomic A text\n');
    await fs.rename(join(a, '.README.saving'), join(a, 'README.txt'));
    assert.equal(await command(['cat', 'README.txt'], a), 'atomic A text\n');
    await command(['cp', 'README.txt', 'created.txt'], a);
    await command(['mv', 'dir', 'renamed-dir'], a);
    await fs.rename(join(a, 'renamed-dir'), join(a, 'dir-native-rename'));
    await fs.rename(join(a, 'dir-native-rename'), join(a, 'renamed-dir'));
    await command(['rm', 'delete.txt'], a);
    assert.equal(await fs.readFile(join(a, 'created.txt'), 'utf8'), 'atomic A text\n');
    assert.equal(await fs.readFile(join(a, 'renamed-dir/nested.txt'), 'utf8'), 'nested\n');
    await assert.rejects(fs.stat(join(a, 'delete.txt')), { code: 'ENOENT' });
    await assert.rejects(fs.stat(join(a, 'dir')), { code: 'ENOENT' });
    assert.deepEqual(await snapshot(base), original);
    assert.deepEqual(await snapshot(b), original);
  });
  await check('directory-traversal', async () => {
    const paths = (await command(['find', '.', '-type', 'f'], a)).trim().split('\n').sort();
    assert.deepEqual(paths, ['./README.txt', './bytes.bin', './created.txt', './main.c', './renamed-dir/nested.txt', './value.h'].sort());
  });
  await check('relative-symlink', async () => {
    assert.equal(await fs.readlink(join(a, 'inside-link')), 'README.txt');
    assert.equal(await command(['cat', 'inside-link'], a), 'atomic A text\n');
    await fs.writeFile(join(a, 'inside-link'), 'symlink A text\n');
    assert.equal(await fs.readFile(join(a, 'README.txt'), 'utf8'), 'symlink A text\n');
    assert.deepEqual(await snapshot(b), original);
    assert.deepEqual(await snapshot(base), original);
  });
  await check('watch-notifications', async () => {
    const phases = [];
    async function observe(name, operation) {
      const events = [], peerEvents = [], errors = [];
      let watcher, peer;
      try {
        watcher = watch(a, (type, name) => events.push({ type, name: String(name) }));
        watcher.on('error', e => errors.push(e.message));
        peer = watch(b, (type, name) => peerEvents.push({ type, name: String(name) }));
        peer.on('error', e => errors.push(e.message));
        await operation();
        const deadline = performance.now() + limits.watchTimeoutMs;
        while (!events.some(e => e.name === name) && performance.now() < deadline) await Bun.sleep(20);
        assert.ok(events.some(e => e.name === name), `no watch event for ${name}`);
        await Bun.sleep(100);
        assert.deepEqual(errors, []); assert.deepEqual(peerEvents, []);
        phases.push({ name, events, peerEvents });
      } finally { watcher?.close(); peer?.close(); }
    }
    await observe('watch-create', () => fs.writeFile(join(a, 'watch-create'), 'one'));
    await fs.writeFile(join(a, 'watch-temp'), 'two');
    await observe('watch-create', () => fs.rename(join(a, 'watch-temp'), join(a, 'watch-create')));
    assert.equal(await fs.readFile(join(a, 'watch-create'), 'utf8'), 'two');
    await observe('watch-create', () => fs.unlink(join(a, 'watch-create')));
    await assert.rejects(fs.stat(join(a, 'watch-create')), { code: 'ENOENT' });
    return { phases, scope: 'fresh top-level watcher per operation; no ordering or recursive-watch guarantee' };
  });
  await check('external-compiler-access', async () => {
    await fs.writeFile(join(a, 'value.h'), '#define VALUE 9\n');
    for (const [root, expected] of [[a, '9\n'], [b, '7\n']]) {
      await command(['cc', '-Wall', '-Wextra', '-Werror', 'main.c', '-o', 'probe-app'], root);
      assert.equal(await command([join(root, 'probe-app')], root), expected);
      await fs.unlink(join(root, 'probe-app'));
    }
  });
  await check('independent-process-writes', async () => {
    const beforeA = await snapshot(a);
    await Promise.all([killWriter(join(a, 'concurrent.txt'), 'after', 'A\n'), killWriter(join(b, 'concurrent.txt'), 'after', 'B\n')]);
    assert.equal(await fs.readFile(join(a, 'concurrent.txt'), 'utf8'), 'A\n');
    assert.equal(await fs.readFile(join(b, 'concurrent.txt'), 'utf8'), 'B\n');
    await fs.unlink(join(a, 'concurrent.txt')); await fs.unlink(join(b, 'concurrent.txt'));
    assert.deepEqual(await snapshot(a), beforeA);
    assert.deepEqual(await snapshot(b), original);
    assert.deepEqual(await snapshot(base), original);
  });
  await check('layer-clone-snapshot-isolation', async () => {
    if (!clone) throw Object.assign(new Error('caller-prepared paths do not prove cloning a modified Layer'), { code: 'ENOENT', syscall: 'spawn clone-not-provided' });
    await clone(a, c);
    const frozen = await snapshot(c);
    assert.deepEqual(frozen, await snapshot(a));
    await fs.writeFile(join(a, 'README.txt'), 'A after clone\n');
    await fs.writeFile(join(c, 'value.h'), '#define VALUE 11\n');
    assert.equal(await fs.readFile(join(a, 'README.txt'), 'utf8'), 'A after clone\n');
    assert.equal(await fs.readFile(join(c, 'value.h'), 'utf8'), '#define VALUE 11\n');
    assert.equal(await fs.readFile(join(c, 'README.txt'), 'utf8'), 'symlink A text\n');
    assert.equal(await fs.readFile(join(a, 'value.h'), 'utf8'), '#define VALUE 9\n');
    assert.deepEqual(await snapshot(b), original); assert.deepEqual(await snapshot(base), original);
    return 'modified snapshot cloned; subsequent writes independent; storage model evaluated separately';
  });
  await check('writer-process-interruption', async () => {
    const target = join(a, 'recovery.txt');
    await fs.writeFile(target, 'old\n');
    await killWriter(target, 'before', 'unpublished\n');
    assert.equal(await command(['cat', 'recovery.txt'], a), 'old\n');
    assert.equal(await fs.readFile(`${target}.saving`, 'utf8'), 'unpublished\n');
    await fs.unlink(`${target}.saving`);
    await killWriter(target, 'after', 'committed\n');
    assert.equal(await command(['cat', 'recovery.txt'], a), 'committed\n');
    await assert.rejects(fs.stat(`${target}.saving`), { code: 'ENOENT' });
    await fs.unlink(target);
    return 'SIGKILL before rename leaves old bytes plus a temporary file; after file+directory fsync and rename leaves new bytes';
  });
  await check('final-base-and-peer-integrity', async () => {
    assert.deepEqual(await snapshot(base), original); assert.deepEqual(await snapshot(b), original);
  });
  const outsideLink = join(a, 'outside-link');
  try {
    await fs.symlink(join(b, 'README.txt'), outsideLink);
    const observed = await fs.readFile(outsideLink, 'utf8');
    report.pathBoundary = { verdict: 'NOT VERIFIED', evidence: 'absolute symlink can read sibling view', observed,
      scope: 'confinement is not provided by ordinary directories; explicit link access is not an accidental copy-up leak' };
  } catch (e) { report.pathBoundary = { verdict: 'INCONCLUSIVE', evidence: e.message }; }
  finally { await fs.rm(outsideLink, { force: true }); }
}

const args = process.argv.slice(2);
function option(name, fallback) { const index = args.indexOf(name); return index < 0 ? fallback : args[index + 1]; }
const mode = option('--backend', 'directory');
let output = resolve(option('--out', 'workspace-probe.json'));
let outputApproved = false;
report.backend = mode;
report.contract = ['immutable-shared-base', 'sparse-changes-and-clones', 'backend-interruption-recovery', 'real-sparse-backend-gate-2']
  .map(id => ({ id, verdict: 'INCONCLUSIVE', evidence: 'probe did not complete' }));
// A stuck caller-provided filesystem must not make this an unbounded stress run.
// Exit 124 means incomplete evidence; it must never be interpreted as success.
const deadline = setTimeout(() => {
  report.fatal = 'Probe exceeded 60-second wall budget'; report.behaviorVerdict = 'INCONCLUSIVE';
  report.cleanup = 'aborted; fixture cleanup not confirmed'; report.finishedAt = new Date().toISOString();
  if (outputApproved) writeFileSync(output, JSON.stringify(report, null, 2) + '\n');
  console.error(report.fatal); process.exit(124);
}, 60000);
let temporary;
try {
  if (args.includes('--seed')) {
    const root = await fs.realpath(option('--seed'));
    await seed(root); console.log(`Seeded ${root}`); process.exit(0);
  }
  const destination = await fs.lstat(output).catch(e => { if (e.code !== 'ENOENT') throw e; return null; });
  assert.ok(!destination?.isSymbolicLink(), 'report destination must not be a symlink');
  output = join(await fs.realpath(dirname(output)), basename(output));
  assert.ok(['directory', 'clonefile', 'paths'].includes(mode), 'unknown backend');
  if (platform() === 'darwin') report.host.macOS = (await command(['/usr/bin/sw_vers'])).trim();
  let base, a, b, c, cloneExecutable;
  if (mode === 'paths') {
    const config = JSON.parse(await fs.readFile(option('--paths'), 'utf8'));
    [base, a, b, c] = await Promise.all(['base', 'layerA', 'layerB', 'clone'].map(k => fs.realpath(config[k])));
    const roots = [base, a, b, c];
    assert.ok(roots.every(p => output !== p && !output.startsWith(`${p}/`)), 'report must be outside tested paths');
    outputApproved = true;
    for (let i = 0; i < roots.length; i++) for (let j = i + 1; j < roots.length; j++) {
      assert.ok(roots[i] !== roots[j] && !roots[i].startsWith(`${roots[j]}/`) && !roots[j].startsWith(`${roots[i]}/`), 'paths must be distinct and non-nested');
    }
    const expectedRoot = await fs.mkdtemp(join(tmpdir(), 'javelin-fixture-'));
    try { await seed(expectedRoot); assert.deepEqual(await snapshot(base), await snapshot(expectedRoot), 'base must match minimal fixture exactly'); }
    finally { await fs.rm(expectedRoot, { recursive: true, force: true }); }
  } else {
    outputApproved = true;
    temporary = await fs.mkdtemp(join(tmpdir(), 'javelin-fs-probe-'));
    base = join(temporary, 'base'); a = join(temporary, 'a'); b = join(temporary, 'b'); c = join(temporary, 'c');
    await fs.mkdir(base); await seed(base);
    if (mode === 'clonefile') {
      assert.equal(platform(), 'darwin', 'clonefile control requires macOS');
      cloneExecutable = join(temporary, 'clonefile');
      await command(['cc', '-Wall', '-Wextra', '-Werror', join(here, 'clonefile.c'), '-o', cloneExecutable]);
    }
    for (const view of [a, b, c]) await copyTree(base, view, cloneExecutable);
    if (args.includes('--negative-control')) {
      await fs.unlink(join(b, 'README.txt')); await fs.link(join(a, 'README.txt'), join(b, 'README.txt'));
    }
    for (const name of Object.keys(fixture)) await fs.chmod(join(base, name), 0o444);
    await fs.chmod(join(base, 'dir'), 0o555); await fs.chmod(base, 0o555);
  }
  report.paths = { base, layerA: a, layerB: b, clone: c };
  await behavior(base, a, b, c, mode === 'paths' ? null : async (source, target) => {
    await fs.rm(target, { recursive: true }); await copyTree(source, target, cloneExecutable);
  });
  report.contract = [
    { id: 'immutable-shared-base', verdict: mode === 'paths' ? 'INCONCLUSIVE' : 'NOT VERIFIED', evidence: mode === 'paths'
      ? 'path observations do not establish immutable underlying shared storage'
      : 'independent directory namespaces; owner can chmod base; mode bits are not immutable storage' },
    { id: 'sparse-changes-and-clones', verdict: mode === 'paths' ? 'INCONCLUSIVE' : 'NOT VERIFIED', evidence: mode === 'paths'
      ? 'no backend storage or clone implementation supplied'
      : 'enumerates and materializes every base file and directory in each view; clonefile shares file extents but is not a sparse Layer namespace' },
    { id: 'backend-interruption-recovery', verdict: 'INCONCLUSIVE', evidence: 'writer process killed; no backend daemon, remount, machine crash, checkpoint or restart exercised' },
    { id: 'real-sparse-backend-gate-2', verdict: 'INCONCLUSIVE', evidence: 'no real sparse backend implementation and provenance supplied; behavior controls cannot close Gate 2' },
  ];
  report.behaviorVerdict = report.checks.some(c => c.verdict === 'NOT VERIFIED') ? 'NOT VERIFIED'
    : report.checks.some(c => c.verdict === 'INCONCLUSIVE') ? 'INCONCLUSIVE' : 'VERIFIED';
  process.exitCode = report.behaviorVerdict === 'VERIFIED' ? 0 : 1;
} catch (error) { report.fatal = error.message; report.behaviorVerdict = 'INCONCLUSIVE'; process.exitCode = 2; }
finally {
  if (temporary) {
    await fs.chmod(join(temporary, 'base'), 0o755).catch(() => {});
    await fs.chmod(join(temporary, 'base/dir'), 0o755).catch(() => {});
    await fs.rm(temporary, { recursive: true, force: true });
    report.cleanup = 'removed owned temporary fixture';
  } else report.cleanup = 'caller paths retained';
  report.finishedAt = new Date().toISOString();
  if (outputApproved) await fs.writeFile(output, JSON.stringify(report, null, 2) + '\n');
  else console.error('Report destination was not approved; no report written');
  clearTimeout(deadline);
  console.log(`${report.behaviorVerdict}: ${output}`);
  for (const check of report.checks) console.log(`${check.verdict}: ${check.id}`);
}
