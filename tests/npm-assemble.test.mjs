import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { chmod, mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { assemble } from '../scripts/assemble-npm.mjs';

const targets = [
  ['aarch64-apple-darwin', 'tar.gz', 'javelin'],
  ['x86_64-unknown-linux-gnu', 'tar.gz', 'javelin'],
  ['x86_64-pc-windows-msvc', 'zip', 'javelin.exe'],
];

async function fixture() {
  const root = await mkdtemp(path.join(tmpdir(), 'javelin-npm-test-'));
  const artifacts = path.join(root, 'artifacts');
  const packageDir = path.join(root, 'package');
  await mkdir(artifacts);
  await mkdir(packageDir);
  await writeFile(path.join(root, 'LICENSE'), 'license\n');
  for (const [target, extension, executable] of targets) {
    const name = `javelin-1.0.0-${target}`;
    const archive = `${name}.${extension}`;
    await mkdir(path.join(artifacts, name), { recursive: true });
    await writeFile(path.join(artifacts, name, executable), `binary:${target}`);
    await chmod(path.join(artifacts, name, executable), 0o755);
    const archivePath = path.join(artifacts, archive);
    if (extension === 'zip' && process.platform !== 'win32') {
      execFileSync('zip', ['-q', '-r', archivePath, name], { cwd: artifacts });
    } else {
      execFileSync('tar', [extension === 'zip' ? '--format=zip' : '-z', '-cf', archivePath, '-C', artifacts, name]);
    }
    const archiveBytes = await readFile(archivePath);
    const digest = createHash('sha256').update(archiveBytes).digest('hex');
    await writeFile(path.join(artifacts, `${archive}.sha256`), `${digest}  ${archive}\n`);
  }
  return { root, artifacts, packageDir };
}

test('verifies artifacts and assembles all released binaries', async (t) => {
  const paths = await fixture();
  t.after(() => rm(paths.root, { recursive: true, force: true }));
  await writeFile(path.join(paths.artifacts, 'javelin-1.0.0-aarch64-apple-darwin', 'javelin'), 'untrusted unpacked copy');
  await assemble(paths.artifacts, paths.packageDir, '1.0.0', path.join(paths.root, 'LICENSE'));
  for (const [target, , executable] of targets) {
    assert.equal(
      await readFile(path.join(paths.packageDir, 'vendor', target, executable), 'utf8'),
      `binary:${target}`,
    );
  }
  assert.equal(await readFile(path.join(paths.packageDir, 'LICENSE'), 'utf8'), 'license\n');
});

test('rejects an artifact whose checksum does not match', async (t) => {
  const paths = await fixture();
  t.after(() => rm(paths.root, { recursive: true, force: true }));
  await writeFile(path.join(paths.artifacts, 'javelin-1.0.0-aarch64-apple-darwin.tar.gz'), 'tampered');
  await assert.rejects(
    assemble(paths.artifacts, paths.packageDir, '1.0.0', path.join(paths.root, 'LICENSE')),
    /Checksum mismatch/,
  );
});
