// Records FSKit prerequisites on a disposable hosted macOS runner and makes one
// bounded attempt to build, register and mount Apple's passthrough sample with
// the signing available there (ad-hoc, no team). It never disables security,
// accepts a license, reboots or invokes Git. A failure is recorded, not hidden.
//
// bun fskit-probe.mjs --work DIR --out DIR
import * as fs from 'node:fs';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';

const SAMPLE_URL = 'https://docs-assets.developer.apple.com/published/0b4283600908/BuildingAPassthroughFileSystem.zip';
const SAMPLE_SHA256 = '81c63039eea41ec539bbe3b7486efa426bce295b8a2c6099104fdf3b7925ed46';

const args = process.argv.slice(2);
const option = name => { const i = args.indexOf(name); if (i < 0 || !args[i + 1]) throw new Error(`missing ${name}`); return resolve(args[i + 1]); };
const work = option('--work'), out = option('--out');
fs.mkdirSync(out, { recursive: true });
fs.mkdirSync(work);
const records = {};
const cap = s => (s.length > 65536 ? s.slice(0, 32768) + '\n[truncated]\n' + s.slice(-32768) : s);

function run(key, argv, { timeoutMs = 120000, cwd } = {}) {
  const started = Date.now();
  const r = Bun.spawnSync(argv, { cwd, stdout: 'pipe', stderr: 'pipe', timeout: timeoutMs });
  records[key] = { argv, cwd, exitCode: r.exitCode, signal: r.signalCode ?? null, elapsedMs: Date.now() - started,
    stdout: cap(r.stdout.toString()), stderr: cap(r.stderr.toString()) };
  return records[key];
}
const find = (root, test) => {
  const hits = [];
  (function walk(d) { for (const e of fs.readdirSync(d, { withFileTypes: true })) {
    const p = join(d, e.name);
    if (test(e, p)) hits.push(p); else if (e.isDirectory() && !e.isSymbolicLink()) walk(p);
  } })(root);
  return hits;
};

const result = { schema: 1, startedAt: new Date().toISOString(), sample: { url: SAMPLE_URL, expectedSha256: SAMPLE_SHA256 } };
try {
  run('sw_vers', ['sw_vers']);
  run('csrutil', ['csrutil', 'status']);
  run('xcode-select', ['xcode-select', '-p']);
  run('xcodebuild-version', ['xcodebuild', '-version']);
  run('sdk-version', ['xcrun', '--sdk', 'macosx', '--show-sdk-version']);
  const sdk = run('sdk-path', ['xcrun', '--sdk', 'macosx', '--show-sdk-path']).stdout.trim();
  run('license-check', ['xcodebuild', '-license', 'check']);
  run('codesigning-identities', ['security', 'find-identity', '-v', '-p', 'codesigning']);
  run('fskit-headers', ['ls', join(sdk, 'System/Library/Frameworks/FSKit.framework/Headers')]);
  run('fskit-path-resource-header', ['grep', '-n', 'FSPathURLResource', join(sdk, 'System/Library/Frameworks/FSKit.framework/Headers/FSResource.h')]);
  run('fskit-modules-before', ['pluginkit', '-m', '-v', '-p', 'com.apple.fskit.fsmodule']);

  // Downloaded sample stays in its own directory and is built by xcodebuild only.
  const dl = join(work, 'download'); fs.mkdirSync(dl);
  const zip = join(dl, 'sample.zip');
  run('download', ['curl', '-fsSL', '--max-time', '60', '-o', zip, SAMPLE_URL]);
  result.sample.sha256 = fs.existsSync(zip) ? createHash('sha256').update(fs.readFileSync(zip)).digest('hex') : null;
  if (result.sample.sha256 !== SAMPLE_SHA256) throw new Error(`sample digest ${result.sample.sha256}`);
  const src = join(work, 'sample'); fs.mkdirSync(src);
  run('unzip', ['ditto', '-x', '-k', zip, src]);
  const [project] = find(src, e => e.name.endsWith('.xcodeproj'));
  if (!project) throw new Error('sample project not found');
  run('list-schemes', ['xcodebuild', '-list', '-project', project]);
  const build = run('build', ['xcodebuild', '-project', project, '-scheme', 'Passthrough', '-configuration', 'Debug',
    '-derivedDataPath', join(work, 'derived'), 'CODE_SIGN_IDENTITY=-', 'CODE_SIGN_STYLE=Manual', 'DEVELOPMENT_TEAM=',
    'PROVISIONING_PROFILE_SPECIFIER=', 'build'], { timeoutMs: 600000 });
  result.buildExit = build.exitCode;
  if (build.exitCode !== 0) throw new Error('sample build failed');

  const [app] = find(join(work, 'derived/Build/Products'), e => e.name.endsWith('.app'));
  const [appex] = find(app, e => e.name.endsWith('.appex'));
  if (!appex) throw new Error('no extension bundle in build products');
  run('app-signature', ['codesign', '-dvvv', '--entitlements', '-', app]);
  run('appex-signature', ['codesign', '-dvvv', '--entitlements', '-', appex]);
  run('appex-verify', ['codesign', '--verify', '--strict', '--verbose=4', appex]);
  const id = run('appex-id', ['plutil', '-extract', 'CFBundleIdentifier', 'raw', join(appex, 'Contents/Info.plist')]).stdout.trim();
  const short = run('appex-shortname', ['plutil', '-extract', 'EXAppExtensionAttributes.FSShortName', 'raw',
    join(appex, 'Contents/Info.plist')]).stdout.trim();
  run('lsregister', ['/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister', '-f', app]);
  run('pluginkit-add', ['pluginkit', '-a', appex]);
  run('fskit-modules-registered', ['pluginkit', '-m', '-v', '-p', 'com.apple.fskit.fsmodule']);
  // Undocumented for FSKit: Apple documents only the System Settings toggle.
  run('pluginkit-enable', ['pluginkit', '-e', 'use', '-i', id]);
  run('fskit-modules-after-enable', ['pluginkit', '-m', '-v', '-p', 'com.apple.fskit.fsmodule']);

  const source = join(work, 'mount-source'), target = join(work, 'mount-target');
  fs.mkdirSync(source); fs.mkdirSync(target); fs.writeFileSync(join(source, 'hello.txt'), 'hello\n');
  const mounted = run('mount', ['mount', '-F', '-t', short || 'passthrough', source, target], { timeoutMs: 30000 });
  const table = run('mount-table', ['mount']).stdout.split('\n').filter(l => l.includes(target));
  result.mount = { exitCode: mounted.exitCode, signal: mounted.signal, table };
  if (table.length) {
    run('mounted-read', ['cat', join(target, 'hello.txt')]);
    run('unmount', ['umount', target], { timeoutMs: 30000 });
  }
} catch (e) { result.stopped = e.message; }
finally {
  run('fskit-log', ['log', 'show', '--last', '15m', '--style', 'compact', '--predicate',
    'subsystem == "com.apple.FSKit" OR process == "fskitd" OR process == "fskit_agent" OR sender == "AppleMobileFileIntegrity" OR process == "amfid"'], { timeoutMs: 120000 });
  result.remainingMounts = run('final-mount-table', ['mount']).stdout.split('\n').filter(l => l.includes(work));
  result.records = records;
  // Passthrough is not sparse. Even a successful mount only proves FSKit activation.
  result.fskitMountVerdict = result.mount?.exitCode === 0 && result.mount.table.length ? 'VERIFIED' : 'INCONCLUSIVE';
  result.sparseVerdict = 'INCONCLUSIVE';
  result.finishedAt = new Date().toISOString();
  fs.writeFileSync(join(out, 'fskit-probe.json'), JSON.stringify(result, null, 2) + '\n');
  console.log(`fskit mount ${result.fskitMountVerdict}; stopped: ${result.stopped ?? 'no'}`);
}
