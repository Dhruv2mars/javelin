// Windows-native sparse backend checks. Separate fixtures from the unchanged common probe.
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { userInfo } from 'node:os';

const [owned, evidence, phase] = process.argv.slice(2);
assert.equal(process.platform, 'win32');
assert.equal(process.env.GITHUB_ACTIONS, 'true');
assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted');
assert.match(owned, /[\\/]javelin-sparse-proof-[a-f0-9]{32}$/);
assert.ok(evidence === join(owned, 'user-results') || evidence === join(owned, 'admin-results'));
const frozen = join(owned, 'frozen'), native = join(frozen, 'base-native'), renameBase = join(frozen, 'base-rename');
const snap = join(frozen, 'snap'), sparse = join(owned, 'sparse.exe');
const root = name => join(owned, name);
const hash = value => createHash('sha256').update(value).digest('hex');
const read = name => fs.readFile(name, 'utf8');
const save = (name, data) => fs.writeFile(join(evidence, name), JSON.stringify(data, null, 2) + '\n');
const load = async name => JSON.parse(await read(join(evidence, name)));
const result = { id: phase, verdict: 'INCONCLUSIVE', commands: [], evidence: {} };
const binary = Buffer.from(Array.from({ length: 12288 }, (_, i) => (i * 131 + 7) % 256));
const fixture = {
  'README.txt': 'shared base text\n', 'bytes.bin': binary, 'delete.txt': 'deleted in A\n',
  'notes.txt': 'original notes line\n'.repeat(64), 'slow.bin': Buffer.alloc(9000, 0x5a),
  'value.h': '#define VALUE 7\n', 'main.c': '#include <stdio.h>\n#include "value.h"\nint main(void) { printf("%d\\n", VALUE); return 0; }\n',
  'dir/nested.txt': 'nested\n', 'dir2/inner.txt': 'inner\n', 'dir3/deep/leaf.txt': 'leaf\n',
  'cold/sub/cold.txt': 'cold unpopulated\n', 'cold-file.txt': 'cold dehydrated\n',
};
function run(argv, cwd, allowFailure = false) {
  const child = spawnSync(argv[0], argv.slice(1), { cwd, encoding: 'utf8', timeout: 30000, maxBuffer: 65536 });
  const record = { argv, cwd, exitCode: child.status, stdout: child.stdout, stderr: child.stderr, error: child.error?.message };
  result.commands.push(record);
  if (!allowFailure) assert.equal(child.status, 0, `${argv.join(' ')}: ${child.stderr}`);
  return record;
}
const tool = (...args) => { const r = run([sparse, ...args], owned, true); return { ...r, json: r.stdout ? JSON.parse(r.stdout) : null }; };
async function tree(base) {
  const rows = [];
  async function visit(name) {
    const path = join(base, name), stat = await fs.lstat(path);
    if (stat.isSymbolicLink()) rows.push([name, 'symlink', await fs.readlink(path)]);
    else if (stat.isDirectory()) {
      rows.push([name, 'directory']);
      for (const child of (await fs.readdir(path)).sort()) await visit(join(name, child));
    } else rows.push([name, 'file', stat.size, hash(await fs.readFile(path))]);
    if (rows.length > 128) throw new Error('fixture entry bound');
  }
  await visit(''); return rows;
}
// Names only: listing must not read file bytes, so it does not hydrate placeholders.
async function names(base, name = '', skip = null) {
  const rows = [name];
  if (skip && name === skip) return rows; // Leave this directory unpopulated.
  const stat = await fs.lstat(join(base, name));
  if (stat.isDirectory() && !stat.isSymbolicLink()) for (const child of (await fs.readdir(join(base, name))).sort()) rows.push(...await names(base, join(name, child), skip));
  return rows;
}
async function writeSynced(path, contents) {
  const file = await fs.open(path, 'w');
  try { await file.writeFile(contents); await file.sync(); } finally { await file.close(); }
}
const denied = error => ['EACCES', 'EPERM'].includes(error.code);
async function refused(label, action) {
  try { await action(); result.evidence[label] = { refused: false }; return false; }
  catch (error) { result.evidence[label] = { refused: denied(error), code: error.code }; return denied(error); }
}
// Ordinary-user attempts to defeat the frozen read-only data: attribute, ACL, owner, write, delete, rename.
async function protection(target, file, container) {
  const before = await fs.readFile(file);
  const attrib = run(['attrib.exe', '-R', file], target, true);
  const after = run(['attrib.exe', file], target, true);
  const grant = run(['icacls.exe', file, '/grant', `${userInfo().username}:F`], target, true);
  const owner = run(['takeown.exe', '/f', file], target, true);
  const checks = {
    attributeStillReadOnly: after.stdout.slice(0, Math.max(0, after.stdout.toLowerCase().indexOf(file.toLowerCase()))).includes('R'),
    attribDenied: attrib.exitCode !== 0 || /denied/i.test(attrib.stdout + attrib.stderr),
    aclGrantDenied: grant.exitCode !== 0, takeOwnershipDenied: owner.exitCode !== 0,
    writeDenied: await refused(`${target}-write`, () => fs.writeFile(file, 'tamper')),
    deleteDenied: await refused(`${target}-delete`, () => fs.unlink(file)),
    renameDenied: await refused(`${target}-rename`, () => fs.rename(file, `${file}.moved`)),
    createDenied: await refused(`${target}-create`, () => fs.writeFile(join(target, 'planted.txt'), 'x')),
    containerRenameDenied: await refused(`${target}-container-rename`, () => fs.rename(container, `${container}-moved`)),
    frozenRootRenameDenied: await refused(`${target}-frozen-root-rename`, () => fs.rename(frozen, `${frozen}-moved`)),
  };
  assert.deepEqual(await fs.readFile(file), before);
  return checks;
}
try {
  if (phase === 'seed') {
    for (const target of [native, renameBase]) {
      for (const [name, contents] of Object.entries(fixture)) {
        await fs.mkdir(join(target, name, '..'), { recursive: true });
        await fs.writeFile(join(target, name), contents);
      }
      await fs.symlink('README.txt', join(target, 'inside-link'));
      run(['attrib.exe', '+R', join(target, '*'), '/S'], target);
    }
    result.evidence = { native: await tree(native), rename: await tree(renameBase) };
  } else if (phase === 'job-termination') {
    const child = spawnSync(sparse, ['bounded', '1500', process.execPath, process.argv[1], owned, evidence, 'hang-parent'],
      { encoding: 'utf8', timeout: 8000, maxBuffer: 16384 });
    result.commands.push({ argv: ['bounded', '1500', 'hang-parent'], exitCode: child.status, stderr: child.stderr });
    assert.equal(child.status, 124);
    assert.match(child.stderr, /timedOut=1 exit=124 activeAfterTermination=0/);
  } else if (phase === 'hang-parent' || phase === 'hang-child') {
    if (phase === 'hang-parent') spawn(process.execPath, [process.argv[1], owned, evidence, 'hang-child'], { stdio: 'inherit' });
    await new Promise(() => setInterval(() => {}, 1000));
  } else if (phase === 'protect-base') {
    result.evidence.checks = await protection(native, join(native, 'README.txt'), native);
    assert.ok(Object.values(result.evidence.checks).every(Boolean), JSON.stringify(result.evidence.checks));
  } else if (phase === 'protect-snapshot') {
    const blob = (await fs.readdir(join(snap, 'blobs')))[0];
    result.evidence.manifest = await protection(snap, join(snap, 'manifest.tsv'), snap);
    result.evidence.blob = await protection(join(snap, 'blobs'), join(snap, 'blobs', blob), snap);
    const freeze = JSON.parse(await read(join(owned, 'freeze.json')));
    result.evidence.unchangedSinceFreeze = JSON.stringify(await tree(snap)) === JSON.stringify(freeze.tree);
    assert.ok(result.evidence.unchangedSinceFreeze);
    assert.ok([...Object.values(result.evidence.manifest), ...Object.values(result.evidence.blob)].every(Boolean));
  } else if (phase === 'rename-projfs' || phase === 'rename-cfapi') {
    // Same matrix on both backends; outcomes are recorded, not presumed.
    const r = root(phase === 'rename-projfs' ? 'pr' : 'cr'), cases = {};
    const move = (id, from, to) => { cases[id] = tool('move', join(r, from), join(r, to)).json; return cases[id].ok; };
    move('file-placeholder', 'notes.txt', 'notes-moved.txt');
    move('untouched-base-directory', 'dir3', 'dir3-moved');
    await fs.readdir(join(r, 'dir')); assert.equal(await read(join(r, 'dir/nested.txt')), 'nested\n');
    move('enumerated-base-directory', 'dir', 'dir-moved');
    await fs.writeFile(join(r, 'dir2/new.txt'), 'new\n');
    move('dirty-base-directory', 'dir2', 'dir2-moved');
    await fs.mkdir(join(r, 'local-dir')); await fs.writeFile(join(r, 'local-dir/own.txt'), 'own\n');
    if (move('local-full-directory', 'local-dir', 'local-moved')) assert.equal(await read(join(r, 'local-moved/own.txt')), 'own\n');
    await fs.mkdir(join(r, 'local-mixed'));
    move('base-file-into-local-directory', 'README.txt', 'local-mixed/README.txt');
    move('local-directory-holding-base-file', 'local-mixed', 'local-mixed-moved');
    const reads = {};
    for (const path of ['dir3-moved/deep/leaf.txt', 'dir-moved/nested.txt', 'dir2-moved/inner.txt', 'dir2-moved/new.txt', 'local-mixed-moved/README.txt'])
      reads[path] = await read(join(r, path)).then(text => ({ ok: true, text }), error => ({ ok: false, code: error.code }));
    result.evidence = { cases, reads, tree: await tree(r) };
  } else if (phase === 'native-ops') {
    const a = root('a'), b = root('b'), checks = {};
    const step = async (name, action) => {
      try { checks[name] = (await action()) ?? true; } catch (error) { checks[name] = { failed: String(error?.message ?? error), code: error?.code }; }
    };
    await step('textRead', async () => assert.equal(await read(join(a, 'README.txt')), 'shared base text\n'));
    await step('binaryRead', async () => assert.deepEqual(await fs.readFile(join(a, 'bytes.bin')), binary));
    await step('inPlaceWrite', () => writeSynced(join(a, 'README.txt'), 'private A text\n'));
    await step('peerUnchanged', async () => assert.equal(await read(join(b, 'README.txt')), 'shared base text\n'));
    await step('create', () => writeSynced(join(a, 'created.txt'), 'created in A\n'));
    await step('delete', async () => { await fs.unlink(join(a, 'delete.txt')); await assert.rejects(fs.stat(join(a, 'delete.txt')), { code: 'ENOENT' }); });
    await step('atomicSave', async () => {
      await writeSynced(join(a, 'value.h.saving'), '#define VALUE 9\n');
      await fs.rename(join(a, 'value.h.saving'), join(a, 'value.h'));
      assert.equal(await read(join(a, 'value.h')), '#define VALUE 9\n');
    });
    await step('populatedDirectoryRename', async () => {
      await fs.readdir(join(a, 'dir')); await fs.rename(join(a, 'dir'), join(a, 'renamed-dir'));
      assert.equal(await read(join(a, 'renamed-dir/nested.txt')), 'nested\n');
    });
    await step('untouchedDirectoryRename', () => fs.rename(join(a, 'dir3'), join(a, 'renamed-dir3')));
    await step('unchangedFileRename', () => fs.rename(join(a, 'bytes.bin'), join(a, 'moved-bytes.bin')));
    // Cold entries stay unpopulated/dehydrated until capture, exercising the partial-directory and dehydrated-rename paths.
    await step('coldDirectoryRename', () => fs.rename(join(a, 'cold'), join(a, 'cold-moved')));
    await step('coldFileRename', () => fs.rename(join(a, 'cold-file.txt'), join(a, 'cold-file-moved.txt')));
    await step('baseSymlinkRead', async () => {
      assert.equal(await fs.readlink(join(a, 'inside-link')), 'README.txt');
      assert.equal(await read(join(a, 'inside-link')), 'private A text\n');
    });
    await step('symlinkCreate', async () => { await fs.symlink('created.txt', join(a, 'new-link')); assert.equal(await read(join(a, 'new-link')), 'created in A\n'); });
    await step('traversal', async () => {
      const listing = await names(a, '', 'cold-moved');
      for (const name of ['renamed-dir3\\deep\\leaf.txt', 'renamed-dir\\nested.txt', 'moved-bytes.bin', 'created.txt', 'dir2\\inner.txt', 'cold-moved', 'cold-file-moved.txt'])
        assert.ok(listing.includes(name), name);
      for (const name of ['delete.txt', 'dir', 'dir3', 'bytes.bin', 'value.h.saving']) assert.ok(!listing.includes(name), name);
      return listing;
    });
    await step('compiler', async () => {
      run(['cc', '-Wall', '-Wextra', '-Werror', 'main.c', '-o', 'native-build.exe'], a);
      const output = run([join(a, 'native-build.exe')], a).stdout;
      assert.equal(output.replaceAll('\r\n', '\n'), '9\n');
      await fs.unlink(join(a, 'native-build.exe'));
      return { outputHex: Buffer.from(output).toString('hex'), crlf: output === '9\r\n' };
    });
    await step('fileFsync', async () => { const handle = await fs.open(join(a, 'created.txt'), 'r+'); try { await handle.sync(); } finally { await handle.close(); } });
    checks.directoryFlush = tool('flush-directory', a).json; // Recorded only; not a required capability.
    await step('isolation', async () => {
      await writeSynced(join(b, 'b-only.txt'), 'B private\n');
      await assert.rejects(fs.stat(join(a, 'b-only.txt')), { code: 'ENOENT' });
    });
    result.evidence = { checks };
    const failed = Object.entries(checks).filter(([name, value]) => name !== 'directoryFlush' && value?.failed);
    assert.equal(failed.length, 0, JSON.stringify(failed));
  } else if (phase === 'closed-write') {
    await writeSynced(join(root('a'), 'completed.txt'), 'completed and closed\n');
  } else if (phase === 'read-slow') {
    const outcome = await fs.readFile(join(root('a'), 'slow.bin')).then(data => ({ ok: true, sha256: hash(data) }), error => ({ ok: false, code: error.code, message: error.message }));
    await save('read-slow-during-kill.json', outcome);
  } else if (phase === 'after-kills') {
    const a = root('a');
    const slowData = await fs.readFile(join(a, 'slow.bin'));
    const notes = await read(join(a, 'notes.txt')), original = fixture['notes.txt'];
    const evidenceKill = {
      readerDuringKill: await load('read-slow-during-kill.json'),
      slowAfterRestartMatchesBase: hash(slowData) === hash(fixture['slow.bin']),
      completedClosedWrite: await read(join(a, 'completed.txt')),
      inPlaceMidWrite: { startsWithPartial: notes.startsWith('PARTIAL-WRITE'), length: notes.length,
        tailIsOriginal: notes.slice('PARTIAL-WRITE'.length) === original.slice('PARTIAL-WRITE'.length) },
      atomicTargetAfterKill: await read(join(a, 'README.txt')),
      temporaryAfterKill: await read(join(a, 'README.txt.saving')).catch(error => error.code),
    };
    assert.equal(evidenceKill.readerDuringKill.ok, false, 'reader must fail while its hydration is interrupted');
    assert.ok(evidenceKill.slowAfterRestartMatchesBase);
    assert.equal(evidenceKill.completedClosedWrite, 'completed and closed\n');
    assert.equal(evidenceKill.atomicTargetAfterKill, 'private A text\n');
    await fs.unlink(join(a, 'README.txt.saving')).catch(() => {});
    result.evidence = evidenceKill;
  } else if (phase === 'tree-a') {
    await save('tree-a.json', await tree(root('a')));
  } else if (phase === 'clones') {
    const a = await load('tree-a.json'), d = root('d'), e = root('e');
    assert.deepEqual(await tree(d), a); assert.deepEqual(await tree(e), a);
    await writeSynced(join(d, 'README.txt'), 'private D text\n'); await writeSynced(join(d, 'd-only.txt'), 'D\n');
    await writeSynced(join(e, 'e-only.txt'), 'E\n'); await fs.unlink(join(e, 'created.txt'));
    assert.equal(await read(join(e, 'README.txt')), 'private A text\n');
    await assert.rejects(fs.stat(join(e, 'd-only.txt')), { code: 'ENOENT' });
    await assert.rejects(fs.stat(join(d, 'e-only.txt')), { code: 'ENOENT' });
    assert.equal(await read(join(d, 'created.txt')), 'created in A\n');
    assert.equal(await read(join(root('a'), 'README.txt')), 'private A text\n');
    const freeze = JSON.parse(await read(join(owned, 'freeze.json')));
    assert.deepEqual(await tree(snap), freeze.tree);
    result.evidence = { cloneTreesMatchSource: true, privateWritesIsolated: true, snapshotUnchanged: true, entries: a.length };
  } else throw new Error(`unknown phase ${phase}`);
  result.verdict = 'VERIFIED';
} catch (error) {
  result.error = { message: String(error?.stack ?? error), code: error?.code };
}
await save(`native-${phase}.json`, result);
process.exit(result.verdict === 'VERIFIED' ? 0 : 3);
