// Offline verification of one downloaded Windows hardening artifact against this checkout.
// bun tools/windows-sparse-backend/verify-hardening.mjs <attempt-directory>
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../../', import.meta.url));
const attempt = process.argv[2];
assert.ok(attempt, 'attempt directory required');
const raw = join(attempt, 'raw');
const read = name => JSON.parse(readFileSync(join(raw, name), 'utf8'));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const run = JSON.parse(readFileSync(join(attempt, 'run.json')));
const [job] = JSON.parse(readFileSync(join(attempt, 'jobs.json')));
const [artifact] = JSON.parse(readFileSync(join(attempt, 'artifacts.json')));
assert.equal(`sha256:${sha(readFileSync(join(attempt, 'artifact.zip')))}`, artifact.digest, 'archive digest');
const zip = spawnSync('unzip', ['-Z1', join(attempt, 'artifact.zip')], { encoding: 'utf8' });
assert.equal(zip.status, 0);
const names = zip.stdout.trim().split('\n').sort();
assert.deepEqual(readdirSync(raw).sort(), names, 'complete raw file set');
for (const name of names) {
  assert.match(name, /^[a-zA-Z0-9_.-]+$/);
  const entry = spawnSync('unzip', ['-p', join(attempt, 'artifact.zip'), name], { maxBuffer: 1 << 24 });
  assert.equal(entry.status, 0);
  assert.equal(sha(readFileSync(join(raw, name))), sha(entry.stdout), `raw tamper: ${name}`);
}
const summary = read('summary.json'), user = read('user-summary.json'), hardening = read('hardening-checks.json');
assert.equal(summary.sourceSHA, run.head_sha);
assert.equal(job.head_sha, run.head_sha);
assert.deepEqual(job.labels, ['windows-2022']);
assert.equal(job.conclusion, 'success');
assert.equal(run.event, 'pull_request');
const sources = ['run.ps1', 'user-run.ps1', 'sparse.cpp', 'projfs-provider.cpp', 'checks.mjs',
  'hardening-repro.ps1', 'hardening-control.cpp', 'hardening-checks.ps1'].map(p => `tools/windows-sparse-backend/${p}`)
  .concat(['.github/workflows/windows-sparse-backend.yml', 'tools/workspace-probe/probe.mjs', 'tools/workspace-probe/atomic-writer.mjs']);
assert.deepEqual(Object.keys(summary.sources).sort(), [...sources, 'binary/sparse.exe', 'binary/projfs-provider.exe', 'binary/hardening-control.exe'].sort());
for (const path of sources) {
  const bytes = readFileSync(join(repo, path));
  const crlf = Buffer.from(bytes.toString().replaceAll('\r\n', '\n').replaceAll('\n', '\r\n'));
  assert.ok([sha(bytes), sha(crlf)].includes(summary.sources[path]), `current source differs: ${path}`);
}
assert.deepEqual(summary.blockers, []);
for (const flag of ['accountCreated', 'accountRemoved', 'cleanupVerified', 'baseUnchangedAtEnd', 'snapshotUnchangedAtEnd']) assert.equal(summary[flag], true, flag);
assert.equal(user.identity.administrator, false);
assert.match(user.identity.sid, /^S-1-5-21-[0-9-]+$/);
assert.deepEqual(user.identity, hardening.identity);
assert.equal(hardening.sourceSHA, summary.sourceSHA);
assert.equal(user.providersStopped, true);
assert.deepEqual(user.errors, []);
for (const [phase, code] of Object.entries(user.phases)) assert.equal(code, phase === 'common-probe' ? 1 : 0, phase);
assert.equal(hardening.verdict, 'VERIFIED');
assert.equal(hardening.error, null);
const command = label => { const row = hardening.commands.find(c => c.label === label); assert.ok(row, label); return row; };
for (const [suffix, size] of [['zero', 4096], ['resident', 16384]]) {
  const name = `hpartial-${suffix}`;
  assert.equal(JSON.parse(command(`create-${name}`).stdout).processed, 1);
  const state = JSON.parse(command(`state-${name}`).stdout).entries.find(e => e.path === 'moved.bin');
  assert.ok(state.placeholderState & 16);
  assert.equal(state.placeholderState & 8, 0);
  assert.equal(state.logicalBytes, size);
  assert.ok(JSON.parse(command(`read-${name}`).stdout).readError > 0);
  assert.notEqual(command(name).exitCode, 0);
  assert.match(command(name).stderr, /uncertain partial/);
}
for (const id of ['junction', 'audit-junction', 'outside-link', 'snapshot-mode', 'unsealed-capture', 'provider-active', 'oversize', 'entry-budget', 'depth-budget', 'tamper-before-hydration']) assert.notEqual(command(id).exitCode, 0, id);
for (const id of ['escape', 'absolute', 'ads', 'missing', 'mismatch', 'duplicate', 'prefix', 'malformed', 'base', 'link', 'truncated']) assert.notEqual(command(`manifest-${id}`).exitCode, 0, id);
for (const label of ['rename-cfapi', 'native-ops', 'after-kills', 'clones', 'protect-base', 'protect-snapshot']) assert.equal(read(`native-${label}.json`).verdict, 'VERIFIED', label);
for (const label of ['protect-base', 'protect-snapshot']) assert.equal(read(`native-${label}.json`).evidence.actualGranteeSID, user.identity.sid);
const log = readFileSync(join(raw, 'ordinary-user.stderr.txt'), 'utf8');
assert.match(log, /CONTROL_JOB .*wallMs=190000 cpuLimit100ns=1200000000 .*timedOut=0 exit=0 activeAfterTermination=0/);
assert.ok(hardening.commands.every(c => /activeAfterTermination=0/.test(c.stderr)));
console.log(`Verified Windows capture hardening at ${summary.sourceSHA}, ${names.length} raw files, current sources matched. Common probe remains failing.`);
