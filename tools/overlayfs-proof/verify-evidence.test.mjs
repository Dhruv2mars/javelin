import { test } from 'bun:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

test('rejects a duplicate phase that omits before-remount evidence', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'javelin-overlay-tamper-'));
  try {
    await cp(fileURLToPath(new URL('evidence/linux-x64/', import.meta.url)), directory, { recursive: true });
    const summaryPath = join(directory, 'summary.json');
    const summary = JSON.parse(await readFile(summaryPath, 'utf8'));
    summary.checks.find(check => check.id === 'before-remount').id = 'initial';
    await writeFile(summaryPath, JSON.stringify(summary));
    await rm(join(directory, 'before-remount.json'));

    assert.throws(() => execFileSync(process.execPath,
      [fileURLToPath(new URL('verify-evidence.mjs', import.meta.url)), directory],
      { timeout: 10000, encoding: 'utf8', stdio: 'pipe' }),
    error => error.status === 1 && error.stderr.includes('required evidence phase IDs'));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
