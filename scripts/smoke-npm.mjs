import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

const root = mkdtempSync(path.join(tmpdir(), 'javelin-installed-'));
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const packageDir = path.resolve('packages/npm');
const version = JSON.parse(readFileSync(path.join(packageDir, 'package.json'))).version;
try {
  execFileSync(npm, ['pack', packageDir, '--pack-destination', root], { stdio: 'inherit', shell: process.platform === 'win32' });
  execFileSync(npm, ['install', '--prefix', path.join(root, 'install'), '--ignore-scripts', '--no-audit', '--no-fund', path.join(root, `javelin-cli-${version}.tgz`)], { stdio: 'inherit', shell: process.platform === 'win32' });
  const launcher = path.join(root, 'install/node_modules/javelin-cli/bin/javelin.cjs');
  const world = path.join(root, 'world');
  const cli = (...args) => execFileSync(process.execPath, [launcher, '--project', world, ...args], { encoding: 'utf8', timeout: 60000 }).trim();
  assert.ok(cli('version').includes(version));
  cli('init', world);
  const layer = cli('layer', 'create', 'smoke', '--from', 'world');
  writeFileSync(path.join(layer, 'hello.txt'), 'hello from npm\n');
  cli('publish', 'smoke', '--idempotency-key', 'npm-smoke');
  assert.equal(cli('show', 'world:hello.txt'), 'hello from npm');
  cli('fsck');
  console.log(`Installed npm lifecycle passed on ${process.platform}-${process.arch}`);
} finally {
  // This disposable World's Monitor holds its cwd and executable open on Windows.
  try {
    const pid = Number(readFileSync(path.join(root, 'world/.javelin/monitor/pid'), 'utf8'));
    if (Number.isSafeInteger(pid) && pid > 0) process.kill(pid);
  } catch (error) {
    if (!['ENOENT', 'ESRCH'].includes(error.code)) throw error;
  }
  await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 250 });
}
