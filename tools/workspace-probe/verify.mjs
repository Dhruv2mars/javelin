import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import * as fs from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { tmpdir, platform } from 'node:os';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const execute = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const output = resolve(process.argv[2] ?? join(tmpdir(), 'javelin-probe-evidence'));
await fs.mkdir(output, { recursive: true });
async function run(name, args, expectedExit) {
  const reportPath = join(output, `${name}.json`);
  let code = 0;
  try { await execute(process.execPath, [join(here, 'probe.mjs'), ...args, '--out', reportPath], { timeout: 65000, maxBuffer: 16384 }); }
  catch (e) { code = e.code; }
  assert.equal(code, expectedExit, `${name} exit code`);
  const report = JSON.parse(await fs.readFile(reportPath, 'utf8'));
  assert.equal(report.contract.find(c => c.id === 'real-sparse-backend-gate-2').verdict, 'INCONCLUSIVE');
  console.log(`${name}: exit ${code}, behavior ${report.behaviorVerdict}, sparse backend INCONCLUSIVE`);
  return report;
}
const control = await run('directory', ['--backend', 'directory'], 0);
assert.equal(control.checks.length, 11);
assert.ok(control.checks.every(c => c.verdict === 'VERIFIED'));
assert.equal(control.pathBoundary.verdict, 'NOT VERIFIED');
assert.equal(control.contract.find(c => c.id === 'sparse-changes-and-clones').verdict, 'NOT VERIFIED');
if (platform() === 'darwin') {
  const clonefile = await run('clonefile', ['--backend', 'clonefile'], 0);
  assert.ok(clonefile.checks.every(c => c.verdict === 'VERIFIED'));
  assert.ok(clonefile.commands.filter(c => c.argv[0].endsWith('/clonefile')).length >= 18);
}
const broken = await run('hardlink-negative', ['--backend', 'directory', '--negative-control'], 1);
for (const id of ['private-in-place-writes', 'final-base-and-peer-integrity']) {
  assert.equal(broken.checks.find(c => c.id === id).verdict, 'NOT VERIFIED');
}
const aborted = await run('wall-budget-negative', ['--backend', 'directory', '--wall-ms', '300'], 124);
assert.equal(aborted.behaviorVerdict, 'INCONCLUSIVE');
assert.match(aborted.fatal, /wall budget/);
if (aborted.ownedTemporaryRoot) {
  await fs.chmod(join(aborted.ownedTemporaryRoot, 'base'), 0o755);
  await fs.chmod(join(aborted.ownedTemporaryRoot, 'base/dir'), 0o755);
  await fs.rm(aborted.ownedTemporaryRoot, { recursive: true, force: true });
}
const caller = await fs.mkdtemp(join(tmpdir(), 'javelin-caller-paths-'));
try {
  const base = join(caller, 'base'); await fs.mkdir(base);
  await execute(process.execPath, [join(here, 'probe.mjs'), '--seed', base], { timeout: 5000 });
  const config = { base, layerA: join(caller, 'a'), layerB: join(caller, 'b'), clone: join(caller, 'c') };
  for (const path of [config.layerA, config.layerB, config.clone]) await fs.cp(base, path, { recursive: true, verbatimSymlinks: true });
  const configPath = join(caller, 'paths.json'); await fs.writeFile(configPath, JSON.stringify(config));
  async function rejectOutput(path) {
    let code = 0;
    try { await execute(process.execPath, [join(here, 'probe.mjs'), '--backend', 'paths', '--paths', configPath, '--out', path], { timeout: 5000 }); }
    catch (e) { code = e.code; }
    assert.equal(code, 2);
    assert.equal(await fs.readFile(join(config.layerA, 'README.txt'), 'utf8'), 'shared base text\n');
  }
  await rejectOutput(join(config.layerA, 'README.txt'));
  await fs.symlink(config.layerA, join(caller, 'alias'));
  await rejectOutput(join(caller, 'alias/README.txt'));
  await fs.symlink(join(config.layerA, 'README.txt'), join(caller, 'report-link.json'));
  await rejectOutput(join(caller, 'report-link.json'));
  console.log('rejected report destinations leave caller bytes intact, including symlink aliases');
  const paths = await run('caller-paths', ['--backend', 'paths', '--paths', configPath], 1);
  assert.equal(paths.checks.filter(c => c.verdict === 'VERIFIED').length, 10);
  assert.equal(paths.checks.find(c => c.id === 'layer-clone-snapshot-isolation').verdict, 'INCONCLUSIVE');
  assert.ok(await fs.stat(base));
  await fs.writeFile(configPath, JSON.stringify({ ...config, layerA: base }));
  const rejected = await run('overlapping-paths-negative', ['--backend', 'paths', '--paths', configPath], 2);
  assert.match(rejected.fatal, /distinct and non-nested/);
} finally { await fs.rm(caller, { recursive: true, force: true }); }
console.log(`VERIFIED probe controls. Evidence: ${output}`);
