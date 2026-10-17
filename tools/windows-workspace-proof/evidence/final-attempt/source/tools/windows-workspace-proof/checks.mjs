// Windows-native evidence is separate from the unchanged common probe.
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { watch } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { join, dirname, relative } from 'node:path';

const [owned, evidence, phase] = process.argv.slice(2);
assert.equal(process.platform, 'win32');
assert.equal(process.env.GITHUB_ACTIONS, 'true');
assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted');
assert.match(owned, /[\\/]javelin-windows-proof-[a-f0-9]{32}$/);
assert.equal(evidence, join(owned, 'user-results'));
const base = join(owned, 'base'), a = join(owned, 'a'), b = join(owned, 'b'), d = join(owned, 'd');
const hash = value => createHash('sha256').update(value).digest('hex');
const read = name => fs.readFile(name, 'utf8');
const save = (name, data) => fs.writeFile(join(evidence, name), JSON.stringify(data, null, 2) + '\n');
const load = async name => JSON.parse(await read(join(evidence, name)));
const result = { id: phase, verdict: 'INCONCLUSIVE', commands: [], evidence: {} };
if (phase === 'hang-parent' || phase === 'hang-child') {
  await save(`job-${phase}-pid.json`, { pid: process.pid });
  if (phase === 'hang-parent') spawn(process.execPath, [process.argv[1], owned, evidence, 'hang-child'], { stdio: 'inherit' });
  await new Promise(() => { setInterval(() => {}, 1000); });
}
async function snapshot(root) {
  const rows = [];
  async function visit(name) {
    const path = join(root, name), stat = await fs.lstat(path);
    if (stat.isSymbolicLink()) rows.push([name, 'symlink', await fs.readlink(path)]);
    else if (stat.isDirectory()) {
      rows.push([name, 'directory']);
      for (const child of (await fs.readdir(path)).sort()) await visit(join(name, child));
    } else rows.push([name, 'file', stat.size, hash(await fs.readFile(path))]);
    if (rows.length > 128) throw new Error('fixture entry bound');
  }
  await visit(''); return rows;
}
function run(argv, cwd) {
  const process = spawnSync(argv[0], argv.slice(1), { cwd, encoding: 'utf8', timeout: 20000, maxBuffer: 16384 });
  result.commands.push({ argv, cwd, exitCode: process.status, stdout: process.stdout, stderr: process.stderr, error: process.error?.message });
  assert.equal(process.status, 0, process.stderr); return process.stdout;
}
async function writeSynced(path, contents) {
  const file = await fs.open(path, 'w');
  try { await file.writeFile(contents); await file.sync(); } finally { await file.close(); }
}
try {
  if (phase === 'job-termination') {
    const provider = join(owned, 'provider.exe');
    const child = spawnSync(provider, ['bounded', '1500', process.execPath, process.argv[1], owned, evidence, 'hang-parent'],
      { encoding: 'utf8', timeout: 8000, maxBuffer: 16384 });
    result.commands.push({ argv: [provider, 'bounded', '1500', process.execPath, process.argv[1], owned, evidence, 'hang-parent'],
      exitCode: child.status, stdout: child.stdout, stderr: child.stderr });
    assert.equal(child.status, 124);
    assert.match(child.stderr, /timedOut=1 exit=124 activeAfterTermination=0/);
    result.evidence = { parent: await load('job-hang-parent-pid.json'), child: await load('job-hang-child-pid.json'),
      scope: 'actual suspended/assigned Windows job kills hung parent and child; active job processes reached zero' };
  } else if (phase === 'base') {
    const frozen = await snapshot(base);
    await assert.rejects(fs.writeFile(join(base, 'README.txt'), 'forbidden'), error => ['EACCES', 'EPERM'].includes(error.code));
    assert.deepEqual(await snapshot(base), frozen);
    await save('base-initial.json', frozen);
    result.evidence = { entries: frozen.length, baseWriteDenied: true };
  } else if (phase === 'metadata') {
    assert.equal((await fs.stat(join(a, 'README.txt'))).size, 17);
  } else if (phase === 'hydrate') {
    assert.equal(await read(join(a, 'README.txt')), 'shared base text\n');
  } else if (phase === 'build') {
    run(['cc', '-Wall', '-Wextra', '-Werror', 'main.c', '-o', 'native-build.exe'], a);
    assert.equal(run([join(a, 'native-build.exe')], a).replaceAll('\r', ''), '7\n');
  } else if (phase === 'build-clean') {
    await fs.unlink(join(a, 'native-build.exe'));
  } else if (phase === 'directory-rename') {
    await fs.rename(join(a, 'dir'), join(a, 'native-renamed-dir'));
    assert.equal(await read(join(a, 'native-renamed-dir/nested.txt')), 'nested\n');
    await fs.rename(join(a, 'native-renamed-dir'), join(a, 'dir'));
  } else if (phase === 'mutate') {
    const peer = await read(join(b, 'README.txt'));
    await writeSynced(join(a, 'native-saving.txt'), 'native atomic A\n');
    await fs.rename(join(a, 'native-saving.txt'), join(a, 'README.txt'));
    assert.equal(await read(join(a, 'README.txt')), 'native atomic A\n');
    const binary = await fs.readFile(join(a, 'bytes.bin')); binary[4096] = 99;
    await fs.writeFile(join(a, 'bytes.bin'), binary);
    await fs.writeFile(join(a, 'native-created.txt'), 'private creation\n');
    if (await fs.stat(join(a, 'dir')).catch(() => null)) {
      await fs.unlink(join(a, 'dir/nested.txt'));
      await fs.rmdir(join(a, 'dir'));
    }
    if (await fs.stat(join(a, 'delete.txt')).catch(() => null)) await fs.unlink(join(a, 'delete.txt'));
    await assert.rejects(fs.stat(join(a, 'delete.txt')), { code: 'ENOENT' });
    await assert.rejects(fs.stat(join(a, 'dir')), { code: 'ENOENT' });
    assert.equal(await fs.readlink(join(a, 'inside-link')), 'README.txt');
    assert.equal(await read(join(a, 'inside-link')), 'native atomic A\n');
    await fs.writeFile(join(a, 'inside-link'), 'native symlink A\n');
    assert.equal(await read(join(a, 'README.txt')), 'native symlink A\n');
    assert.equal(await read(join(b, 'README.txt')), peer);
    assert.deepEqual(await fs.readFile(join(b, 'bytes.bin')), await fs.readFile(join(base, 'bytes.bin')));
    assert.equal(await read(join(b, 'delete.txt')), 'delete only in A\n');
    assert.equal(await read(join(b, 'dir/nested.txt')), 'nested\n');
    await assert.rejects(fs.stat(join(b, 'native-created.txt')), { code: 'ENOENT' });
    assert.equal(await read(join(base, 'README.txt')), 'shared base text\n');
    const events = [], peerEvents = [];
    const watcher = watch(a, (type, name) => events.push({ type, name: String(name) }));
    const peerWatcher = watch(b, (type, name) => peerEvents.push({ type, name: String(name) }));
    try {
      await fs.writeFile(join(a, 'native-watch'), 'one');
      const deadline = Date.now() + 2000;
      while (!events.some(event => event.name === 'native-watch') && Date.now() < deadline) await Bun.sleep(20);
      assert.ok(events.some(event => event.name === 'native-watch'));
      await Bun.sleep(100); assert.deepEqual(peerEvents, []);
      await fs.unlink(join(a, 'native-watch'));
    } finally { watcher.close(); peerWatcher.close(); }
    result.evidence = { events, peerEvents, traversal: await snapshot(a), scope: 'content and namespace; no directory-fsync durability claim' };
    await save('clone-expected.json', await snapshot(a));
  } else if (phase === 'clone-snapshot') {
    const state = await load('state-after-mutate-a.json');
    assert.equal(state.walkError, 0, 'physical enumeration of stopped root failed; clone inconclusive');
    const snapshotRoot = join(owned, 'snapshot'), changed = join(snapshotRoot, 'changed');
    await fs.mkdir(changed, { recursive: true });
    const copied = [], deleted = [], metadata = [], aliases = [], reusedSymlinks = [];
    const baseRows = await load('base-initial.json');
    const baseFiles = baseRows.filter(row => row[1] === 'file');
    for (const entry of state.entries) {
      assert.ok(!relative(a, join(a, entry.path)).startsWith('..'));
      if (entry.state & 16) deleted.push(entry.path);
      else if ((entry.state & 8) || (entry.state > 0 && !baseRows.some(row => row[0] === entry.path) && !(entry.attributes & 16))) {
        assert.equal(entry.hresult, 0);
        const source = join(a, entry.path), destination = join(changed, entry.path);
        const stat = await fs.lstat(source);
        if (stat.isDirectory()) await fs.mkdir(destination, { recursive: true });
        else if (stat.isSymbolicLink()) {
          const target = await fs.readlink(source);
          const original = baseRows.find(row => row[0] === entry.path && row[1] === 'symlink');
          assert.ok(original && original[2] === target, 'changed local symlink snapshot unsupported');
          reusedSymlinks.push({ path: entry.path, target, state: entry.state, reference: 'immutable base' });
        }
        else {
          const bytes = await fs.readFile(source), digest = hash(bytes);
          const backing = baseFiles.find(row => row[3] === digest);
          if (backing) {
            if (backing[0] !== entry.path) aliases.push({ path: entry.path, base: backing[0], hash: digest });
            continue; // Reuse immutable content, including unchanged bytes at a renamed path.
          }
          assert.ok(entry.state & 8, 'non-full cache bytes cannot be private changed content');
          await fs.mkdir(dirname(destination), { recursive: true });
          await fs.copyFile(source, destination);
          await fs.chmod(destination, 0o444);
          copied.push({ path: entry.path, bytes: stat.size, hash: hash(await fs.readFile(destination)), state: entry.state });
        }
      } else if (entry.state & 4) metadata.push(entry.path);
    }
    assert.ok(deleted.includes('delete.txt'), 'tombstone lookup did not discover deleted base file');
    assert.ok(deleted.includes('dir'), 'tombstone lookup did not discover renamed base directory');
    assert.ok(copied.some(entry => entry.path === 'README.txt'));
    assert.ok(!copied.some(entry => entry.path === 'main.c'), 'hydrated unchanged compiler source counted as private content');
    await fs.writeFile(join(snapshotRoot, 'deleted.txt'), deleted.join('\n') + '\n');
    await fs.chmod(join(snapshotRoot, 'deleted.txt'), 0o444);
    await fs.writeFile(join(snapshotRoot, 'aliases.txt'), aliases.map(row => `${row.path}\t${row.base}`).join('\n'));
    await fs.chmod(join(snapshotRoot, 'aliases.txt'), 0o444);
    const frozen = await snapshot(snapshotRoot); await save('snapshot-frozen.json', frozen);
    result.evidence = { copied, deleted, aliases, reusedSymlinks, metadata, totalChangedBytes: copied.reduce((n, row) => n + row.bytes, 0), metadataPreservation: 'INCONCLUSIVE' };
  } else if (phase === 'clone-verify') {
    assert.deepEqual(await snapshot(d), await load('clone-expected.json'));
    await fs.writeFile(join(a, 'README.txt'), 'source after clone\n');
    await fs.writeFile(join(d, 'value.h'), '#define VALUE 11\n');
    assert.equal(await read(join(d, 'README.txt')), 'native symlink A\n');
    assert.equal(await read(join(a, 'README.txt')), 'source after clone\n');
    assert.notEqual(await read(join(a, 'value.h')), '#define VALUE 11\n');
    run(['cc', '-Wall', '-Wextra', '-Werror', 'main.c', '-o', 'clone-native.exe'], d);
    assert.equal(run([join(d, 'clone-native.exe')], d).replaceAll('\r', ''), '11\n');
    await fs.unlink(join(d, 'clone-native.exe'));
    await assert.rejects(fs.writeFile(join(owned, 'snapshot/changed/README.txt'), 'forbidden'), error => ['EACCES', 'EPERM'].includes(error.code));
    assert.deepEqual(await snapshot(join(owned, 'snapshot')), await load('snapshot-frozen.json'));
  } else if (phase === 'recovery-before') {
    await writeSynced(join(a, 'native-recovery.txt'), 'old\n');
    await writeSynced(join(a, 'native-recovery.txt.saving'), 'unpublished\n');
  } else if (phase === 'recovery-reopen-before') {
    assert.equal(await read(join(a, 'native-recovery.txt')), 'old\n');
    assert.equal(await read(join(a, 'native-recovery.txt.saving')), 'unpublished\n');
    await assert.rejects(fs.stat(join(a, 'delete.txt')), { code: 'ENOENT' });
    assert.equal((await fs.readFile(join(a, 'bytes.bin')))[4096], 99);
    result.evidence = { oldBytesAndTemporaryPreserved: true, deletionPreserved: true };
  } else if (phase === 'recovery-after') {
    await fs.rename(join(a, 'native-recovery.txt.saving'), join(a, 'native-recovery.txt'));
  } else if (phase === 'recovery-reopen-after') {
    assert.equal(await read(join(a, 'native-recovery.txt')), 'unpublished\n');
    await assert.rejects(fs.stat(join(a, 'native-recovery.txt.saving')), { code: 'ENOENT' });
    await assert.rejects(fs.stat(join(a, 'delete.txt')), { code: 'ENOENT' });
    result.evidence = { renamedBytesPreserved: true, temporaryAbsent: true, deletionPreserved: true };
  } else if (phase === 'final') {
    assert.deepEqual(await snapshot(base), await load('base-initial.json'));
    if (await fs.stat(join(evidence, 'snapshot-frozen.json')).catch(() => null)) assert.deepEqual(await snapshot(join(owned, 'snapshot')), await load('snapshot-frozen.json'));
  } else throw new Error(`unknown phase ${phase}`);
  result.verdict = 'VERIFIED';
} catch (error) {
  result.verdict = 'INCONCLUSIVE'; result.error = { message: String(error), code: error.code,
    syscall: error.syscall, path: error.path, stack: error.stack }; process.exitCode = 3;
}
await save(`native-${phase}.json`, result);
