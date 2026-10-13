import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import * as fs from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { tmpdir, platform, arch, release, totalmem, cpus } from 'node:os';
import { fileURLToPath } from 'node:url';

const execute = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const report = { schema: 1, recordedAt: new Date().toISOString(), host: { platform: platform(), arch: arch(), release: release(),
  memoryBytes: totalmem(), cpuCount: cpus().length, bun: Bun.version }, commands: [], checks: [], mountedLayerVerdict: 'INCONCLUSIVE' };
async function run(argv, env) {
  try {
    const result = await execute(argv[0], argv.slice(1), { env: env ? { ...process.env, ...env } : process.env,
      timeout: 20000, killSignal: 'SIGKILL', maxBuffer: 16384 });
    report.commands.push({ argv, env, exitCode: 0, stdout: result.stdout, stderr: result.stderr });
    return result.stdout;
  } catch (e) {
    report.commands.push({ argv, env, exitCode: e.code, signal: e.signal, stdout: e.stdout, stderr: e.stderr });
    return null;
  }
}
const scratch = await fs.mkdtemp(join(tmpdir(), 'javelin-fs-discovery-'));
try {
  if (platform() === 'darwin') {
    await run(['/usr/bin/sw_vers']);
    await run(['/usr/bin/xcode-select', '-p']);
    const sdk = (await run(['/usr/bin/xcrun', '--show-sdk-path']))?.trim();
    await run(['/usr/bin/xcrun', '--show-sdk-version']);
    await run(['/usr/bin/xcodebuild', '-version']);
    if (await fs.stat('/Applications/Xcode.app/Contents/Developer').catch(() => null)) {
      await run(['/usr/bin/xcodebuild', '-version'], { DEVELOPER_DIR: '/Applications/Xcode.app/Contents/Developer' });
      await run(['/usr/bin/xcodebuild', '-checkFirstLaunchStatus'], { DEVELOPER_DIR: '/Applications/Xcode.app/Contents/Developer' });
    }
    await run(['/usr/bin/security', 'find-identity', '-v', '-p', 'codesigning']);
    await run(['/usr/bin/pluginkit', '-m', '-p', 'com.apple.fskit.fsmodule']);
    const filesystem = await fs.statfs(scratch);
    report.host.testFilesystem = { type: filesystem.type, blockSize: filesystem.bsize, availableBytes: filesystem.bavail * filesystem.bsize };
    for (const path of ['/System/Library/Frameworks/FSKit.framework', sdk && join(sdk, 'System/Library/Frameworks/FSKit.framework/Headers/FSClient.h')].filter(Boolean)) {
      report.checks.push({ id: path, verdict: await fs.stat(path).catch(() => null) ? 'VERIFIED' : 'NOT VERIFIED' });
    }
    if (sdk) {
      const definitions = await fs.readFile(join(sdk, 'System/Library/Frameworks/FSKit.framework/Headers/FSKitDefines.h'), 'utf8');
      report.sdkAvailability = definitions.split('\n').filter(line => /^#define FSKIT_API_AVAILABILITY_V/.test(line));
      const headers = await fs.readFile(join(sdk, 'System/Library/Frameworks/FSKit.framework/Headers/FSClient.h'), 'utf8');
      report.mountClientRequiresV3 = /mountSingleVolumeForResource:[\s\S]*?FSKIT_API_AVAILABILITY_V3/.test(headers);
    }
    const executable = join(scratch, 'fskit-discovery');
    const built = await run(['cc', '-Wall', '-Wextra', '-Werror', '-fobjc-arc', '-framework', 'Foundation', '-framework', 'FSKit', join(here, 'fskit-discovery.m'), '-o', executable]);
    if (built !== null) {
      const observed = await run([executable]);
      if (observed !== null) report.fskit = JSON.parse(observed);
    }
    report.checks.push({ id: 'FSClient-framework-execution', verdict: report.fskit && !report.fskit.error ? 'VERIFIED' : 'INCONCLUSIVE' });
    report.checks.push({ id: 'mounted-sparse-Layer', verdict: 'INCONCLUSIVE', evidence: 'discovery enumerates extensions only; no Layer module built, installed, enabled or mounted' });
  } else {
    report.checks.push({ id: 'FSKit', verdict: 'INCONCLUSIVE', evidence: 'macOS-only discovery not run on this host' });
  }
} finally { await fs.rm(scratch, { recursive: true, force: true }); }
const output = resolve(process.argv[2] ?? 'workspace-host.json');
await fs.writeFile(output, JSON.stringify(report, null, 2) + '\n');
console.log(`Host evidence: ${output}`);
