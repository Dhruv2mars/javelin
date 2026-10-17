// Offline audit of original Windows archives, raw results and matching sources.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const directory = process.argv[2] ?? fileURLToPath(new URL('evidence/', import.meta.url));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const windowsCheckout = bytes => Buffer.from(bytes.toString('utf8').replaceAll('\r\n', '\n').replaceAll('\n', '\r\n'));
const allowedSources = new Set(['tools/windows-workspace-proof/run.ps1', 'tools/windows-workspace-proof/feature.ps1',
  'tools/windows-workspace-proof/api-probe.cpp', '.github/workflows/windows-workspace-proof.yml',
  'tools/windows-workspace-proof/provider.cpp', 'tools/windows-workspace-proof/backend.ps1',
  'tools/windows-workspace-proof/user-run.ps1', 'tools/windows-workspace-proof/checks.mjs',
  'tools/workspace-probe/probe.mjs', 'tools/workspace-probe/atomic-writer.mjs']);
// Greptile P2 fixes after the final run; retained sources still prove what actually executed.
const changedAfterEvidence = new Set(['tools/windows-workspace-proof/backend.ps1', '.github/workflows/windows-workspace-proof.yml']);
let latest;
for (const attempt of (await readdir(directory, { withFileTypes: true })).filter(item => item.isDirectory()).map(item => item.name).sort()) {
  const evidence = join(directory, attempt);
  const read = async name => JSON.parse(await readFile(join(evidence, name), 'utf8'));
  const summary = await read('raw/summary.json'), run = await read('cloud-run.json');
  const jobs = await read('cloud-jobs.json'), artifact = await read('cloud-artifact.json');
  assert.equal(summary.schema, 1);
  assert.equal(summary.sourceSHA, run.head_sha);
  assert.equal(summary.runId, String(run.id));
  assert.equal(summary.runAttempt, String(run.run_attempt));
  assert.equal(run.event, 'pull_request');
  assert.equal(run.path, '.github/workflows/windows-workspace-proof.yml');
  assert.equal(run.status, 'completed');
  const job = jobs.jobs.find(item => ['prerequisites', 'windows-proof'].includes(item.name));
  assert.ok(job); assert.equal(job.status, 'completed');
  assert.equal(job.conclusion, run.conclusion);
  assert.ok(job.labels.includes('windows-2022'));
  // GitHub returned empty step lists. Job identity and original transcript supply evidence.
  assert.equal(artifact.workflow_run.id, run.id);
  assert.equal(artifact.workflow_run.head_sha, summary.sourceSHA);
  assert.equal(artifact.name, `windows-workspace-proof-${run.id}-${run.run_attempt}`);
  const archive = join(evidence, 'original-artifact.zip');
  assert.equal(hash(await readFile(archive)), artifact.digest.replace(/^sha256:/, ''));
  const index = await read('artifact-files.json');
  assert.deepEqual((await readdir(join(evidence, 'raw'))).sort(), Object.keys(index).sort());
  for (const [name, expected] of Object.entries(index)) {
    assert.match(name, /^[a-z][a-z0-9_.-]+$/i);
    const original = spawnSync('unzip', ['-p', archive, name], { maxBuffer: 1024 * 1024 });
    assert.equal(original.status, 0, original.stderr?.toString());
    assert.equal(hash(original.stdout), expected, `Index differs from archive: ${attempt}/${name}`);
    assert.equal(hash(await readFile(join(evidence, 'raw', name))), expected, `Raw artifact changed: ${attempt}/${name}`);
  }
  assert.ok(Object.keys(summary.sources).length >= 6);
  for (const [name, expected] of Object.entries(summary.sources)) {
    assert.ok(allowedSources.has(name));
    assert.equal(hash(await readFile(join(evidence, 'source', name))), expected, `Retained executable differs: ${attempt}/${name}`);
    if (attempt === 'final-attempt') {
      const current = await readFile(join(root, name));
      const matches = hash(current) === expected || hash(windowsCheckout(current)) === expected;
      if (changedAfterEvidence.has(name)) assert.ok(!matches, `Remove ${name} from changedAfterEvidence`);
      else assert.ok(matches, `Current executable differs beyond checkout EOL: ${name}`);
    }
  }
  const commands = await read('raw/commands.json');
  for (const command of commands) {
    assert.ok(!/(^|[\\/])git(?:\.exe)?$/i.test(command.executable));
    assert.equal(command.timedOut, false);
    assert.equal(command.error, null);
    assert.equal(command.exitConfirmed, true);
    assert.equal(command.treeTermination, 'not needed');
    assert.ok(command.timeoutMs <= 60000);
    assert.deepEqual(command, await read(`raw/${command.name}.json`));
  }
  const utilities = await read('raw/utilities.json');
  for (const name of ['cat', 'cp', 'mv', 'rm', 'cmp', 'sh', 'bash']) {
    const utility = utilities[name]; assert.ok(utility);
    if (utility.path) {
      assert.match(utility.sha256, /^[a-f0-9]{64}$/);
      assert.equal(commands.find(command => command.name === `utility-${name}`).executable, utility.path);
    } else assert.equal(utility.verdict, 'INCONCLUSIVE');
  }
  assert.equal(summary.backend, 'INCONCLUSIVE');
  assert.equal(summary.host.runnerEnvironment, 'github-hosted');
  assert.equal(summary.identity.ordinaryUserVerified, false);
  for (const scope of Object.values(summary.scopes)) assert.equal(scope.verdict, 'INCONCLUSIVE');
  if (attempt === 'final-attempt') {
    latest = summary;
    assert.equal(run.conclusion, 'success');
    assert.equal(summary.cleanupVerified, true);
    assert.equal(summary.feature.ready, true);
    assert.equal(summary.compilerReady, true);
    assert.equal(summary.apiReady, true);
    const cleanup = await read('raw/cleanup.json');
    assert.equal(cleanup.removed, true); assert.equal(cleanup.error, null);
    assert.match(cleanup.ownedRoot, /[\\/]javelin-windows-proof-[a-f0-9]{32}$/);
    const backend = await read('raw/backend.json');
    assert.equal(backend.accountRemoved, true);
    const runtime = await read('raw/runtime-summary.json');
    assert.equal(runtime.identity.administrator, false);
    assert.equal(runtime.stoppedProviders, true);
    assert.equal(runtime.providerStarted, true);
    const wrapper = await readFile(join(evidence, 'raw/ordinary-user.stderr.txt'), 'utf8');
    const records = [...wrapper.matchAll(/WINDOWS_JOB parent=\d+ timedOut=(\d+) exit=(\d+) activeAfterTermination=(\d+)/g)];
    assert.ok(records.length > 0);
    for (const record of records) { assert.equal(record[1], '0'); assert.equal(record[3], '0'); }
    const common = await read('raw/common-probe.json');
    assert.equal(common.backend, 'paths');
    assert.equal(common.behaviorVerdict, 'NOT VERIFIED');
    for (const phase of runtime.phases.filter(item => item.exitCode === 0)) {
      assert.equal((await read(`raw/native-${phase.id}.json`)).verdict, 'VERIFIED');
    }
    // Open failures stay recorded: native directory rename and common probe POSIX assumptions.
    assert.equal(runtime.commonProbeExit, 1);
    assert.deepEqual(runtime.phases.filter(item => item.exitCode !== 0), [{ id: 'directory-rename', exitCode: 3 }]);
    const rename = await read('raw/native-directory-rename.json');
    assert.equal(rename.verdict, 'INCONCLUSIVE'); assert.equal(rename.error.code, 'ENOTSUP');
    assert.equal(common.checks.find(check => check.id === 'layer-clone-snapshot-isolation').verdict, 'INCONCLUSIVE');
    // Clone snapshots only full (state 8) private files; unchanged symlink references the immutable base.
    const clone = (await read('raw/native-clone-snapshot.json')).evidence;
    assert.ok(clone.copied.length > 0 && clone.copied.every(item => item.state === 8));
    assert.deepEqual(clone.deleted, ['delete.txt', 'dir']);
    assert.ok(clone.reusedSymlinks.every(item => item.reference === 'immutable base'));
    assert.equal(clone.totalChangedBytes, clone.copied.reduce((total, item) => total + item.bytes, 0));
    assert.equal(clone.metadataPreservation, 'INCONCLUSIVE');
    assert.deepEqual((await read('raw/native-recovery-reopen-after.json')).evidence,
      { renamedBytesPreserved: true, temporaryAbsent: true, deletionPreserved: true });
    assert.equal(cleanup.children.length, 20);
    for (const child of cleanup.children) { assert.equal(child.timedOut, false); assert.equal(child.exitCode, 0); }
  }
  console.log(`VERIFIED archive/source integrity: ${attempt}, run ${run.id}, source ${summary.sourceSHA}, job ${run.conclusion}`);
}
assert.ok(latest, 'Missing final Windows attempt');
console.log(`Windows backend INCONCLUSIVE; executable ${latest.sourceSHA}. See separate native scope evidence.`);
