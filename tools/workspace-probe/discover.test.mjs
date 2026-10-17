import { afterAll, beforeAll, expect, test } from 'bun:test';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import * as fs from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { platform, tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const execute = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const hostTest = platform() === 'darwin' ? test : test.skip;
let scratch;
beforeAll(async () => {
  scratch = await fs.mkdtemp(join(tmpdir(), 'javelin-discovery-test-'));
  await fs.writeFile(join(scratch, 'header-error.mjs'), `
    import { mock } from 'bun:test';
    import * as fs from 'node:fs/promises';
    const readFile = fs.readFile, stat = fs.stat;
    const matches = path => String(path).endsWith('/FSKit.framework/Headers/' + process.env.PROBE_TEST_HEADER);
    const fail = () => { throw Object.assign(new Error('injected SDK header read failure'), { code: process.env.PROBE_TEST_ERROR }); };
    mock.module('node:fs/promises', () => ({ ...fs,
      readFile: async (path, ...args) => matches(path) ? fail() : readFile(path, ...args),
      stat: async (path, ...args) => matches(path) && process.env.PROBE_TEST_ERROR === 'ENOENT' ? fail() : stat(path, ...args)
    }));
  `);
});
afterAll(async () => { await fs.rm(scratch, { recursive: true, force: true }); });

for (const header of ['FSKitDefines.h', 'FSClient.h']) {
  for (const code of ['ENOENT', 'EACCES']) {
    hostTest(`${code} reading ${header} preserves INCONCLUSIVE host evidence`, async () => {
      const output = join(scratch, `${header}-${code}.json`);
      await execute(process.execPath, ['--preload', join(scratch, 'header-error.mjs'), join(here, 'discover.mjs'), output], {
        env: { ...process.env, PROBE_TEST_HEADER: header, PROBE_TEST_ERROR: code }, timeout: 25000, maxBuffer: 16384,
      });
      const report = JSON.parse(await fs.readFile(output, 'utf8'));
      expect(report.host.platform).toBe('darwin');
      expect(report.commands.find(c => c.argv[0] === '/usr/bin/sw_vers').exitCode).toBe(0);
      expect(report.commands.find(c => c.argv[1] === '--show-sdk-path').stdout.trim()).not.toBe('');
      expect(report.checks.find(c => c.id === `SDK-header:${header}`)).toMatchObject({ verdict: 'INCONCLUSIVE', errorCode: code });
      expect(report.mountedLayerVerdict).toBe('INCONCLUSIVE');
      expect(report.checks.find(c => c.id === 'mounted-sparse-Layer').verdict).toBe('INCONCLUSIVE');
      if (header === 'FSKitDefines.h') expect(report).not.toHaveProperty('sdkAvailability');
      else expect(report).not.toHaveProperty('mountClientRequiresV3');
    }, 30000);
  }
}
