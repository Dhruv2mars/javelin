import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { release, arch } from 'node:os';

const execute = promisify(execFile);
const stage = process.argv[2];
const root = process.env.OVERLAY_ROOT;
const out = process.env.OVERLAY_EVIDENCE;
const stages = ['initial', 'probe', 'clone', 'before-kill', 'before-remount', 'after-kill', 'after-remount', 'final'];
const a = join(root, 'a'), b = join(root, 'b'), c = join(root, 'c'), base = join(root, 'base');
const commands = [];
const read = name => fs.readFile(join(out, name), 'utf8').then(JSON.parse);
const write = (name, value) => fs.writeFile(join(out, name), JSON.stringify(value, null, 2) + '\n');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

async function command(argv, cwd) {
  const result = await execute(argv[0], argv.slice(1), { cwd, timeout: 15000, killSignal: 'SIGKILL', maxBuffer: 16384 });
  commands.push({ argv, cwd, stdout: result.stdout, stderr: result.stderr, exitCode: 0 });
  return result.stdout;
}
async function tree(path) {
  const rows = [];
  let bytes = 0;
  async function visit(relative) {
    assert.ok(rows.length < 64, 'tree entry budget');
    const file = join(path, relative), stat = await fs.lstat(file);
    if (stat.isSymbolicLink()) rows.push([relative, 'symlink', await fs.readlink(file)]);
    else if (stat.isDirectory()) {
      rows.push([relative, 'directory']);
      for (const name of (await fs.readdir(file)).sort()) await visit(join(relative, name));
    } else {
      assert.ok(stat.isFile()); bytes += stat.size; assert.ok(bytes <= 256 * 1024, 'tree byte budget');
      rows.push([relative, 'file', stat.size, hash(await fs.readFile(file))]);
    }
  }
  await visit('');
  return rows;
}
async function integrity() {
  const initial = await read('initial.json');
  assert.deepEqual(await tree(base), initial.base);
  assert.deepEqual(await tree(b), initial.base);
}
async function killedWriter(phase, contents) {
  const target = join(a, 'recovery.txt');
  const writer = new URL('../workspace-probe/atomic-writer.mjs', import.meta.url).pathname;
  const argv = [process.execPath, writer, target, phase, contents];
  const child = spawn(argv[0], argv.slice(1), { stdio: ['ignore', 'pipe', 'pipe'] });
  const ended = new Promise((resolve, reject) => {
    child.once('error', reject); child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  let timer;
  try {
    await new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error('writer readiness deadline exceeded')), 5000);
      let stdout = '', stderr = '';
      child.stdout.on('data', data => { stdout += data; assert.ok(stdout.length < 16384); if (stdout.includes('ready\n')) resolve(); });
      child.stderr.on('data', data => { stderr = (stderr + data).slice(-4096); });
      child.once('error', reject);
      child.once('exit', code => reject(new Error(`writer exited before readiness: ${code} ${stderr}`)));
    });
    child.kill('SIGKILL');
    const result = await ended;
    assert.equal(result.signal, 'SIGKILL');
    commands.push({ argv, ...result, ready: true, phase });
  } finally {
    clearTimeout(timer); child.kill('SIGKILL'); await ended;
  }
}
const entries = (storage, name) => storage[name].entries;
const fileEntries = (storage, name) => entries(storage, name).filter(x => x.type === 'file');
function emptyUpper(storage, name) { assert.deepEqual(fileEntries(storage, name), []); assert.equal(entries(storage, name).length, 1); }
function sparse(storage) {
  for (const name of ['a-upper', 'b-upper', 'c-upper', 'a-next-upper', 'c-next-upper']) {
    assert.ok(!entries(storage, name).some(x => x.path === 'main.c'), `${name} copied untouched compiler source`);
    assert.ok(storage[name].fileBytes < storage.lower.fileBytes, `${name} materialized full base bytes`);
  }
}
function whiteout(storage) {
  const row = entries(storage, 'a-upper').find(x => x.path === 'delete.txt');
  assert.ok(row, 'missing deletion record');
  assert.ok((row.type === 'whiteout' && row.major === 0 && row.minor === 0) ||
    (row.type === 'file' && row.bytes === 0 && row.xattrs['trusted.overlay.whiteout'] !== undefined), 'not a kernel whiteout');
}

let evidence;
try {
  assert.equal(process.getuid() > 0, true, 'normal writes must run as ordinary user');
  if (stage === 'initial') {
    assert.match(process.env.SOURCE_SHA, /^[0-9a-f]{40}$/);
    const storage = await read('storage-initial.json');
    for (const name of ['a-upper', 'b-upper', 'c-upper', 'a-next-upper', 'c-next-upper']) emptyUpper(storage, name);
    const original = await tree(base);
    for (const path of [a, b, c]) {
      assert.equal((await fs.statfs(path)).type, 0x794c7630, 'not OverlayFS');
      assert.deepEqual(await tree(path), original);
    }
    await assert.rejects(fs.writeFile(join(base, 'README.txt'), 'bad'), { code: 'EROFS' });
    await assert.rejects(fs.readdir(join(root, 'backing')), { code: 'EACCES' });
    assert.deepEqual(await tree(base), original);
    await write('paths.json', { base, layerA: a, layerB: b, clone: c });
    const sources = {};
    for (const name of ['run.sh', 'inventory.py', 'check.mjs', '../workspace-probe/probe.mjs', '../workspace-probe/atomic-writer.mjs']) {
      sources[name] = hash(await fs.readFile(new URL(name, import.meta.url)));
    }
    evidence = { base: original, sourceSHA: process.env.SOURCE_SHA, sources, uid: process.getuid(),
      kernel: release(), arch: arch(), bun: Bun.version, runId: process.env.GITHUB_RUN_ID,
      runAttempt: process.env.GITHUB_RUN_ATTEMPT, runnerImage: process.env.ImageVersion,
      immutableBase: 'read-only bind mount; private backing alias inaccessible to ordinary runner',
      initialUpperRegularBytes: 0 };
  } else if (stage === 'probe') {
    const probe = await read('common-probe.json');
    assert.ok(!probe.fatal, probe.fatal);
    assert.equal(probe.checks.length, 11);
    for (const check of probe.checks) assert.equal(check.verdict,
      check.id === 'layer-clone-snapshot-isolation' ? 'INCONCLUSIVE' : 'VERIFIED', check.id);
    assert.equal(probe.behaviorVerdict, 'INCONCLUSIVE');
    const storage = await read('storage-probe.json'); sparse(storage); whiteout(storage);
    await integrity();
    evidence = { snapshot: await tree(a), verifiedChecks: 10,
      commonCloneVerdict: 'INCONCLUSIVE; independent backend clone check follows',
      pathBoundary: probe.pathBoundary, storage: 'untouched main.c absent from every upper; delete.txt has kernel whiteout' };
  } else if (stage === 'clone') {
    const before = await read('probe.json');
    assert.deepEqual(await tree(a), before.snapshot); assert.deepEqual(await tree(c), before.snapshot);
    await assert.rejects(fs.writeFile(join(root, 'snapshot/README.txt'), 'bad'), { code: 'EROFS' });
    await assert.rejects(fs.stat(join(c, 'delete.txt')), { code: 'ENOENT' });
    await assert.rejects(fs.stat(join(c, 'dir')), { code: 'ENOENT' });
    const storage = await read('storage-clone.json');
    emptyUpper(storage, 'a-next-upper'); emptyUpper(storage, 'c-next-upper'); whiteout(storage);
    assert.deepEqual(entries(storage, 'a-upper'), entries(await read('storage-probe.json'), 'a-upper'));
    await fs.writeFile(join(a, 'README.txt'), 'A after clone\n');
    await fs.writeFile(join(c, 'value.h'), '#define VALUE 11\n');
    assert.equal(await fs.readFile(join(c, 'README.txt'), 'utf8'), 'symlink A text\n');
    assert.equal(await fs.readFile(join(a, 'value.h'), 'utf8'), '#define VALUE 9\n');
    assert.equal(await fs.readFile(join(a, 'inside-link'), 'utf8'), 'A after clone\n');
    assert.equal(await fs.readFile(join(c, 'inside-link'), 'utf8'), 'symlink A text\n');
    for (const [path, expected] of [[a, '9\n'], [c, '11\n']]) {
      await command(['cc', '-Wall', '-Wextra', '-Werror', 'main.c', '-o', 'clone-app'], path);
      assert.equal(await command([join(path, 'clone-app')], path), expected);
      await fs.unlink(join(path, 'clone-app'));
    }
    await integrity();
    evidence = { cloneStartsAtExactSourceSnapshot: true, newUppersInitiallyEmpty: true,
      sourceAfter: await tree(a), cloneAfter: await tree(c), boundary: 'all writers exited; source unmounted before its upper is frozen and reused as read-only lower' };
  } else if (stage === 'before-kill') {
    await fs.writeFile(join(a, 'recovery.txt'), 'old\n');
    await killedWriter('before', 'unpublished\n');
    assert.equal(await fs.readFile(join(a, 'recovery.txt'), 'utf8'), 'old\n');
    assert.equal(await fs.readFile(join(a, 'recovery.txt.saving'), 'utf8'), 'unpublished\n');
    evidence = { snapshot: await tree(a), phase: 'SIGKILL after temp file fsync, before rename' };
  } else if (stage === 'before-remount') {
    assert.deepEqual(await tree(a), (await read('before-kill.json')).snapshot);
    await fs.unlink(join(a, 'recovery.txt.saving')); await integrity();
    evidence = { oldAndTemporaryBytesSurviveUnmountRemount: true };
  } else if (stage === 'after-kill') {
    await killedWriter('after', 'committed\n');
    assert.equal(await fs.readFile(join(a, 'recovery.txt'), 'utf8'), 'committed\n');
    await assert.rejects(fs.stat(join(a, 'recovery.txt.saving')), { code: 'ENOENT' });
    evidence = { snapshot: await tree(a), phase: 'SIGKILL after rename and directory fsync' };
  } else if (stage === 'after-remount') {
    assert.deepEqual(await tree(a), (await read('after-kill.json')).snapshot);
    assert.deepEqual(await tree(c), (await read('clone.json')).cloneAfter); await integrity();
    evidence = { committedBytesSurviveUnmountRemount: true, machinePowerLoss: 'NOT VERIFIED' };
  } else if (stage === 'final') {
    await integrity();
    const initial = await read('storage-initial.json'), final = await read('storage-final.json');
    assert.deepEqual(final.lower, initial.lower);
    assert.deepEqual(final['a-upper'], (await read('storage-clone.json'))['a-upper']);
    sparse(final); whiteout(final);
    evidence = { lowerBytesMetadataAndInodesUnchanged: true, frozenSnapshotUnchanged: true,
      upperFileBytes: Object.fromEntries(Object.entries(final).map(([key, value]) => [key, value.fileBytes])) };
  } else if (stage === 'summary') {
    const cleanup = await read('cleanup.json'), initial = await read('initial.json').catch(() => null);
    const checks = await Promise.all(stages.map(async name => {
      const result = await read(`${name}.json`).catch(() => null);
      return { id: name, verdict: result?.verdict ?? 'INCONCLUSIVE', error: result?.error };
    }));
    const verified = cleanup.exitCode === 0 && cleanup.allUnmountedAndOwnedRootRemoved && checks.every(x => x.verdict === 'VERIFIED');
    await write('summary.json', { verdict: verified ? 'VERIFIED' : 'NOT VERIFIED', sourceSHA: initial?.sourceSHA ?? process.env.SOURCE_SHA,
      checks, cleanup, scope: 'bounded native Linux sparse Layer filesystem proof',
      productGate2: 'INCONCLUSIVE; hub acceptance, native DB integration, automatic checkpoints, permissions and production lifecycle remain open',
      machinePowerLoss: 'NOT VERIFIED', absoluteSymlinkConfinement: 'NOT VERIFIED' });
    assert.ok(verified, 'incomplete or failed proof; see retained artifacts');
    console.log(`VERIFIED bounded OverlayFS proof at ${initial.sourceSHA}`);
    process.exit(0);
  } else throw new Error(`unknown check stage ${stage}`);
  await write(`${stage}.json`, { verdict: 'VERIFIED', ...evidence, commands });
  console.log(`VERIFIED: ${stage}`);
} catch (error) {
  if (stage !== 'summary') await write(`${stage}.json`, { verdict: 'NOT VERIFIED', error: error.message, commands });
  console.error(error.stack); process.exitCode = 1;
}
