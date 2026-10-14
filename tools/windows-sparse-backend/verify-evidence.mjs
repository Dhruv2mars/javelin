// Offline audit of retained hosted archives. Never projects a root, installs anything or invokes Git.
// Usage: bun tools/windows-sparse-backend/verify-evidence.mjs [--raw <dir>]
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url)), repo = join(here, '../..'), evidence = join(here, 'evidence');
const rawOverride = process.argv.includes('--raw') ? process.argv[process.argv.indexOf('--raw') + 1] : null;
const sha = data => createHash('sha256').update(data).digest('hex');
const json = path => JSON.parse(readFileSync(path, 'utf8'));
const attempts = { '1-compile-launch': 'bd00005', '2-compile-path': '6e849b5', '3-cmd-diagnosis': 'cf8a8c1', '4-cmd-fixed': '60fdf62',
  '5-acl-setup': 'e738c3f', '6-first-full': 'ee23f17', '7-final': '4588385' };
assert.deepEqual(readdirSync(evidence).sort(), Object.keys(attempts).sort());
for (const [name, head] of Object.entries(attempts)) {
  const dir = join(evidence, name), run = json(join(dir, 'run.json')), [artifact] = json(join(dir, 'artifacts.json')), [job] = json(join(dir, 'jobs.json'));
  assert.ok(run.head_sha.startsWith(head), `${name} head`);
  assert.equal(run.event, 'pull_request'); assert.deepEqual(job.labels, ['windows-2022']);
  assert.equal(`sha256:${sha(readFileSync(join(dir, 'artifact.zip')))}`, artifact.digest, `${name} archive digest`);
}
const finalDir = join(evidence, '7-final'), raw = rawOverride ?? join(finalDir, 'raw'), zip = join(finalDir, 'artifact.zip');
const listing = spawnSync('unzip', ['-Z1', zip], { encoding: 'utf8' });
assert.equal(listing.status, 0, 'unzip listing');
const entries = listing.stdout.trim().split('\n').sort();
assert.deepEqual(readdirSync(raw).sort(), entries, 'raw file set equals archive');
for (const entry of entries) {
  const original = spawnSync('unzip', ['-p', zip, entry], { maxBuffer: 1 << 24 });
  assert.equal(sha(readFileSync(join(raw, entry))), sha(original.stdout), `raw ${entry} differs from archive`);
}
const summary = json(join(raw, 'summary.json')), user = json(join(raw, 'user-summary.json'));
assert.equal(summary.sourceSHA, '45883857262afb0f47520a8de8f862fece5bc1a0');
for (const [path, hash] of Object.entries(summary.sources)) {
  if (path.startsWith('binary/')) continue;
  const bytes = readFileSync(join(repo, path));
  // Owned files are checked out without conversion; files outside the owned directory received Windows CRLF conversion.
  const tested = path.startsWith('tools/windows-sparse-backend/') ? bytes : Buffer.from(bytes.toString('binary').replaceAll('\r\n', '\n').replaceAll('\n', '\r\n'), 'binary');
  assert.equal(sha(tested), hash, `source ${path}`);
}
assert.deepEqual(summary.blockers, []); assert.equal(summary.cleanupVerified, true); assert.equal(summary.accountRemoved, true);
assert.equal(summary.snapshotUnchangedAtEnd, true); assert.equal(summary.baseUnchangedAtEnd, true);
assert.equal(user.identity.administrator, false); assert.equal(user.providersStopped, true); assert.deepEqual(user.errors, []);
for (const [phase, code] of Object.entries(user.phases)) assert.equal(code, phase === 'common-probe' ? 1 : 0, `phase ${phase}`);
const jobs = readFileSync(join(raw, 'ordinary-user.stderr.txt'), 'utf8').match(/WINDOWS_JOB .*/g);
assert.ok(jobs.length > 10 && jobs.every(line => line.includes('activeAfterTermination=0')));
const phase = id => json(join(raw, `native-${id}.json`));
for (const id of ['protect-base', 'protect-snapshot', 'rename-projfs', 'rename-cfapi', 'native-ops', 'after-kills', 'clones']) assert.equal(phase(id).verdict, 'VERIFIED', id);
const projfs = phase('rename-projfs').evidence.cases, cfapi = phase('rename-cfapi').evidence.cases;
for (const id of ['untouched-base-directory', 'enumerated-base-directory', 'dirty-base-directory']) {
  assert.deepEqual([projfs[id].ok, projfs[id].win32], [false, 50], `projfs ${id}`); assert.equal(cfapi[id].ok, true, `cfapi ${id}`);
}
for (const id of ['file-placeholder', 'local-full-directory', 'base-file-into-local-directory', 'local-directory-holding-base-file']) assert.ok(projfs[id].ok && cfapi[id].ok, id);
const kills = phase('after-kills').evidence;
assert.equal(kills.readerDuringKill.ok, false); assert.equal(kills.slowAfterRestartMatchesBase, true);
assert.deepEqual(kills.inPlaceMidWrite, { startsWithPartial: true, length: 1280, tailIsOriginal: true });
assert.equal(kills.atomicTargetAfterKill, 'private A text\n'); assert.equal(kills.temporaryAfterKill, 'NEW CONTENT PARTIAL');
const slowBefore = json(join(raw, 'state-before-hydration-kill-a.json')).entries.find(e => e.path === 'slow.bin');
assert.deepEqual([slowBefore.placeholderState, slowBefore.allocatedBytes], [57, 0]);
const manifest = readFileSync(join(raw, 'snapshot-manifest.tsv'), 'utf8').trim().split('\n');
assert.ok(manifest.includes('B\tmoved-bytes.bin\tbytes.bin') && manifest.filter(line => line.startsWith('F\t')).length === 5);
const peer = json(join(raw, 'state-final-b.json')).entries.filter(e => !['README.txt', 'b-only.txt', 'inside-link'].includes(e.path));
assert.ok(peer.length === 9 && peer.every(e => e.placeholderState === 57 && e.allocatedBytes === 0), 'untouched peer entries stay dehydrated');
console.log(`verified 7 archives, ${entries.length} final raw files, ${Object.keys(summary.sources).length - 2} sources and recorded verdicts`);
