// Offline audit of retained cloud evidence. No mounts, sudo or source control.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const out = process.argv[2] ?? fileURLToPath(new URL('evidence/linux-x64/', import.meta.url));
const read = async name => JSON.parse(await readFile(join(out, name), 'utf8'));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const initial = await read('initial.json'), summary = await read('summary.json'), run = await read('cloud-run.json');
assert.equal(summary.verdict, 'VERIFIED');
assert.equal(initial.sourceSHA, run.sourceSHA); assert.equal(summary.sourceSHA, run.sourceSHA);
assert.equal(initial.runId, String(run.runId)); assert.ok(initial.uid > 0);
const sourceNames = ['run.sh', 'inventory.py', 'check.mjs', '../workspace-probe/probe.mjs', '../workspace-probe/atomic-writer.mjs'];
assert.deepEqual(Object.keys(initial.sources), sourceNames);
for (const name of sourceNames) assert.equal(hash(await readFile(new URL(name, import.meta.url))), initial.sources[name], `${name} differs from cloud source`);
assert.deepEqual(summary.checks.map(check => check.id), [
  'initial', 'probe', 'clone', 'before-kill', 'before-remount', 'after-kill', 'after-remount', 'final',
], 'required evidence phase IDs');
for (const check of summary.checks) {
  assert.equal(check.verdict, 'VERIFIED');
  const phase = await read(`${check.id}.json`); assert.equal(phase.verdict, 'VERIFIED');
  for (const command of phase.commands) assert.ok(!command.argv.some(arg => /(^|\/)git$/.test(arg)), 'source control in proof command');
}
const probe = await read('common-probe.json');
assert.equal(probe.checks.length, 11); assert.ok(!probe.fatal);
for (const check of probe.checks) assert.equal(check.verdict, check.id === 'layer-clone-snapshot-isolation' ? 'INCONCLUSIVE' : 'VERIFIED');
for (const command of probe.commands) assert.ok(!command.argv.some(arg => /(^|\/)git$/.test(arg)));
const first = await read('storage-initial.json'), frozen = await read('storage-clone.json'), final = await read('storage-final.json');
assert.deepEqual(first.lower, final.lower); assert.deepEqual(frozen['a-upper'], final['a-upper']);
for (const name of ['a-upper', 'b-upper', 'c-upper', 'a-next-upper', 'c-next-upper']) {
  assert.equal(first[name].entries.length, 1); assert.equal(first[name].fileBytes, 0);
  assert.ok(!final[name].entries.some(row => row.path === 'main.c'));
  assert.ok(final[name].fileBytes < final.lower.fileBytes);
}
for (const name of ['a-next-upper', 'c-next-upper']) { assert.equal(frozen[name].entries.length, 1); assert.equal(frozen[name].fileBytes, 0); }
for (const name of ['delete.txt', 'dir']) {
  const whiteout = frozen['a-upper'].entries.find(row => row.path === name);
  assert.equal(whiteout.type, 'whiteout'); assert.equal(whiteout.major, 0); assert.equal(whiteout.minor, 0);
}
assert.equal(frozen['a-upper'].entries.find(row => row.path === 'renamed-dir').xattrs['trusted.overlay.opaque'], '79');
const before = await read('before-kill.json'), after = await read('after-kill.json');
function fileHash(snapshot, name) { return snapshot.find(row => row[0] === name)?.[3]; }
assert.equal(fileHash(before.snapshot, 'recovery.txt'), hash('old\n'));
assert.equal(fileHash(before.snapshot, 'recovery.txt.saving'), hash('unpublished\n'));
assert.equal(fileHash(after.snapshot, 'recovery.txt'), hash('committed\n'));
assert.equal(fileHash(after.snapshot, 'recovery.txt.saving'), undefined);
for (const phase of [before, after]) assert.equal(phase.commands[0].signal, 'SIGKILL');
const clone = await read('clone.json');
assert.deepEqual(clone.commands.filter(x => x.argv[0].endsWith('/clone-app')).map(x => x.stdout), ['9\n', '11\n']);
const cleanup = await read('cleanup.json');
assert.equal(cleanup.exitCode, 0); assert.equal(cleanup.allUnmountedAndOwnedRootRemoved, true);
const mounts = (await readFile(join(out, 'mounts.txt'), 'utf8')).trim().split('\n');
assert.equal(mounts.length, 7); assert.ok(mounts.every(line => line.includes(' overlay ') && line.includes(`lowerdir=${cleanup.root}/`)));
assert.ok(mounts[3].includes(`/snapshot:${cleanup.root}/base`)); assert.ok(mounts[4].includes(`/snapshot:${cleanup.root}/base`));
const remaining = (await readFile(join(out, 'mounts-after-cleanup.txt'), 'utf8')).trim().split('\n');
assert.ok(!remaining.some(path => path === cleanup.root || path.startsWith(`${cleanup.root}/`)));
assert.equal(summary.machinePowerLoss, 'NOT VERIFIED');
console.log(`VERIFIED retained cloud evidence and matching executable sources at ${run.sourceSHA}`);
