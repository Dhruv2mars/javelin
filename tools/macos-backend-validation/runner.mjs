// Hosted-only controller. Backend runtime and evidence audit never use Git.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const out = path.resolve(process.env.SUITABILITY_OUT || '/tmp/javelin-suitability-evidence');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const hash = p => crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const json = (p, value) => fs.writeFileSync(p, JSON.stringify(value, null, 2) + '\n');
const read = p => JSON.parse(fs.readFileSync(p, 'utf8'));
const phaseNames = ['source', 'environment', 'build', 'fixture', 'mount', 'aliases', 'pagination',
  'restart', 'recovery', 'local-access', 'cost-32', 'cost-512', 'cost-2048', 'scale-ceiling',
  'watch-apfs', 'watch-nfs', 'capabilities', 'huge-fixture', 'truncate-controls', 'truncate-nfs',
  'copy-up-stage', 'source-after'];
let ledger, sequence = 0, currentPhase = null;
let deadline = Date.now() + 14 * 60_000;
let cleaning = false;
if (process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_OS !== 'macOS') {
  throw new Error('This fixture runs only on disposable hosted macOS Actions, never the laptop');
}
fs.mkdirSync(out, { recursive: true });
function saveLedger() { json(path.join(out, 'ledger.json'), ledger); }
function record(value) { fs.appendFileSync(path.join(out, 'commands.jsonl'), JSON.stringify(value) + '\n'); }
function killGroup(pid, signal) {
  try { process.kill(-pid, signal); } catch (e) { if (e.code === 'ESRCH') return; }
  // Descendants can be root-owned after sudo. Always bound the kill helper too.
  const c = spawn('/usr/bin/sudo', ['-n', '/bin/kill', `-${signal.replace('SIG', '')}`, '--', `-${pid}`],
    { stdio: 'ignore' });
  const timer = setTimeout(() => c.kill('SIGKILL'), 1500);
  c.once('exit', () => clearTimeout(timer));
}
function launch(argv, { timeout = 30_000, env = {}, cwd = repo, label = 'command' } = {}) {
  if (!cleaning && Date.now() >= deadline) throw new Error('global test budget exhausted');
  const id = `${String(++sequence).padStart(4, '0')}-${label.replace(/[^a-z0-9-]/gi, '-')}`;
  const result = { id, phase: currentPhase, argv, cwd, startedAt: new Date().toISOString(),
    timeoutMs: Math.max(1, Math.min(timeout, cleaning ? timeout : deadline - Date.now())),
    stdout: `${id}.stdout`, stderr: `${id}.stderr`, state: 'started' };
  const stdout = fs.openSync(path.join(out, result.stdout), 'w');
  const stderr = fs.openSync(path.join(out, result.stderr), 'w');
  record(result); // Persist intent before process creation.
  const child = spawn(argv[0], argv.slice(1), { cwd, env: { ...process.env, ...env },
    detached: true, stdio: ['pipe', stdout, stderr] });
  fs.closeSync(stdout); fs.closeSync(stderr);
  result.pid = child.pid;
  if (child.pid) { ledger.groups.push({ pid: child.pid, id, argv }); saveLedger(); }
  const timer = setTimeout(() => {
    result.timedOut = true;
    if (child.pid) killGroup(child.pid, 'SIGTERM');
    setTimeout(() => { if (child.pid) killGroup(child.pid, 'SIGKILL'); }, 1000).unref();
  }, result.timeoutMs);
  const done = new Promise(resolve => {
    let finished = false;
    const finish = (code, signal, error) => {
      if (finished) return;
      finished = true; clearTimeout(timer);
      Object.assign(result, { exit: code, signal, error, finishedAt: new Date().toISOString(), state: 'finished' });
      record(result); resolve(result);
    };
    child.once('error', e => finish(null, null, String(e)));
    child.once('exit', (code, signal) => finish(code, signal, null));
  });
  return { child, result, done };
}
async function run(argv, options) { return await launch(argv, options).done; }
function text(r) { return fs.readFileSync(path.join(out, r.stdout), 'utf8'); }
function requireOK(r) { if (r.exit !== 0 || r.timedOut) throw new Error(`${r.id} failed exit=${r.exit} timeout=${!!r.timedOut}`); return r; }
const worker = (mode, ...args) => ['python3', '-I', path.join(here, 'worker.py'), mode, ...args.map(String)];
async function phase(name, action) {
  currentPhase = name;
  const state = { name, startedAt: new Date().toISOString(), state: 'started' };
  json(path.join(out, `phase-${name}.json`), state);
  try { state.detail = await action(); state.state = 'finished'; }
  catch (e) { state.state = 'inconclusive'; state.error = String(e); }
  state.finishedAt = new Date().toISOString();
  json(path.join(out, `phase-${name}.json`), state);
  currentPhase = null;
  console.log(name, state.state, state.error || '');
  return state;
}
function sourceSnapshot(label) {
  const expected = read(path.join(here, 'backend-source.json'));
  const actual = {};
  for (const p of Object.keys(expected)) {
    actual[p] = hash(path.join(repo, p));
    if (actual[p] !== expected[p]) throw new Error(`inherited source changed: ${p}`);
  }
  json(path.join(out, `${label}.json`), actual);
  return actual;
}
async function waitFor(file, predicate, timeout = 10_000) {
  const until = Math.min(deadline, Date.now() + timeout);
  while (Date.now() < until) {
    if (fs.existsSync(file) && predicate(fs.readFileSync(file, 'utf8'))) return;
    await sleep(100);
  }
  throw new Error(`readiness deadline: ${file}`);
}
const work = () => ledger.work;
const mnt = () => path.join(work(), 'mount');
const base = () => path.join(work(), 'base');
const upper = () => path.join(work(), 'upper');
const binary = () => path.join(work(), 'target/release/macos-sparse-backend');
let server;
async function serve(basePath = base(), upperPath = upper(), env = {}) {
  fs.mkdirSync(upperPath, { recursive: true });
  server = launch([binary(), 'serve', '--base', basePath, '--upper', upperPath, '--port', String(ledger.port)],
    { timeout: Math.max(1, deadline - Date.now()), env, label: 'server' });
  await waitFor(path.join(out, server.result.stdout), s => s.includes('listening'));
  return { pid: server.result.pid, port: ledger.port, binarySha256: hash(binary()), env };
}
async function stop(signal = 'SIGTERM') {
  if (!server) return null;
  killGroup(server.result.pid, signal);
  const r = await server.done;
  server = null;
  return r;
}
async function mount() {
  fs.mkdirSync(mnt(), { recursive: true });
  const options = `vers=3,tcp,port=${ledger.port},mountport=${ledger.port},locallocks,noac,nonegnamecache,soft,timeo=10,retrans=2,rdirplus`;
  ledger.mounts = [mnt()]; saveLedger();
  requireOK(await run(['/usr/bin/sudo', '-n', '/sbin/mount_nfs', '-o', options, '127.0.0.1:/', mnt()],
    { timeout: 20_000, label: 'mount' }));
  return { options, mount: mnt(), identityCommand: (await run(['/sbin/mount'])).id };
}
async function unmount(label) {
  // Forced unmount is recovery/cleanup only. It never repairs a same-mount verdict.
  const normal = await run(['/usr/bin/sudo', '-n', '/sbin/umount', mnt()], { timeout: 10_000, label: `${label}-normal` });
  if (normal.exit !== 0) requireOK(await run(['/usr/bin/sudo', '-n', '/sbin/umount', '-f', mnt()],
    { timeout: 10_000, label: `${label}-force-cleanup-only` }));
  const actual = requireOK(await run(['/sbin/mount']));
  if (text(actual).includes(` on ${mnt()} (`)) throw new Error('mount remains after unmount');
  return { normalExit: normal.exit, forcedCleanupOnly: normal.exit !== 0, mountTable: actual.id };
}
async function cleanup() {
  if (!/^\/private\/tmp\/javelin-suitability-[a-zA-Z0-9]+$/.test(ledger.work) ||
      ledger.mounts.some(p => p !== path.join(ledger.work, 'mount')) ||
      (ledger.account !== null && !/^jvs[0-9]+$/.test(ledger.account))) {
    throw new Error('refuse cleanup outside owned resource ledger');
  }
  cleaning = true;
  currentPhase = 'cleanup';
  const result = { startedAt: new Date().toISOString(), errors: [], remainingMounts: [], remainingGroups: [],
    accountExists: null, portListening: null, workExists: true };
  let mounts = await run(['/sbin/mount'], { timeout: 5000, label: 'cleanup-mounts-before' });
  if (mounts.exit !== 0) result.errors.push('cannot inspect mount table');
  for (const p of ledger.mounts) if (text(mounts).includes(` on ${p} (`)) {
    await run(['/usr/bin/sudo', '-n', '/sbin/umount', '-f', p], { timeout: 10_000, label: 'cleanup-unmount' });
  }
  for (const group of ledger.groups) killGroup(group.pid, 'SIGTERM');
  await sleep(1000);
  for (const group of ledger.groups) killGroup(group.pid, 'SIGKILL');
  mounts = await run(['/sbin/mount'], { timeout: 5000, label: 'cleanup-mounts-after' });
  result.mountTable = mounts.id;
  if (mounts.exit !== 0) result.errors.push('cannot verify mount table');
  result.remainingMounts = ledger.mounts.filter(p => text(mounts).includes(` on ${p} (`));
  const processTable = await run(['/bin/ps', '-axo', 'pid=,pgid=,uid=,command='], { timeout: 5000, label: 'cleanup-process-table' });
  result.processTable = processTable.id;
  if (processTable.exit !== 0) result.errors.push('cannot verify process groups');
  const owned = new Set(ledger.groups.filter(g => g.id !== processTable.id).map(g => g.pid));
  result.remainingGroups = text(processTable).split('\n').filter(line => owned.has(Number(line.trim().split(/\s+/)[1])));
  const port = await run(['/usr/sbin/lsof', '-nP', `-iTCP:${ledger.port}`, '-sTCP:LISTEN'], { timeout: 5000, label: 'cleanup-port' });
  result.portCommand = port.id;
  result.portListening = port.exit === 0 ? true : port.exit === 1 ? false : null;
  if (ledger.account) {
    const accountPath = `/Users/${ledger.account}`;
    await run(['/usr/bin/sudo', '-n', '/usr/bin/dscl', '.', '-delete', accountPath], { timeout: 8000, label: 'cleanup-account-delete' });
    const check = await run(['/usr/bin/dscl', '.', '-read', accountPath], { timeout: 5000, label: 'cleanup-account-check' });
    result.accountCommand = check.id;
    result.accountExists = check.exit === 0 ? true : /DS Error: -14136|eDSRecordNotFound|Invalid Path/.test(text(check) + fs.readFileSync(path.join(out, check.stderr), 'utf8')) ? false : null;
  } else result.accountExists = false;
  if (result.remainingMounts.length === 0 && mounts.exit === 0) {
    const removed = await run(['/usr/bin/sudo', '-n', '/bin/rm', '-rf', ledger.work], { timeout: 15_000, label: 'cleanup-root' });
    if (removed.exit !== 0) result.errors.push('owned root removal failed');
  }
  result.workExists = fs.existsSync(ledger.work);
  const rootCheck = await run(['/bin/test', '!', '-e', ledger.work], { timeout: 5000, label: 'cleanup-root-check' });
  result.rootCommand = rootCheck.id;
  if (rootCheck.exit !== 0) result.errors.push('root absence not verified');
  result.finishedAt = new Date().toISOString();
  result.verified = !result.errors.length && !result.remainingMounts.length && !result.remainingGroups.length &&
    result.accountExists === false && result.portListening === false && !result.workExists;
  json(path.join(out, 'cleanup.json'), result);
  currentPhase = null;
  return result;
}
function seal() {
  const manifest = {};
  function walk(dir) {
    for (const name of fs.readdirSync(dir)) {
      const p = path.join(dir, name), rel = path.relative(out, p);
      if (fs.statSync(p).isDirectory()) walk(p);
      else if (!['manifest.json', 'audit.json'].includes(rel)) manifest[rel] = hash(p);
    }
  }
  walk(out); json(path.join(out, 'manifest.json'), manifest);
}

if (process.argv.includes('--cleanup')) {
  if (fs.existsSync(path.join(out, 'cleanup.json')) && read(path.join(out, 'cleanup.json')).verified) process.exit(0);
  if (fs.existsSync(path.join(out, 'ledger.json'))) {
    ledger = read(path.join(out, 'ledger.json')); sequence = 9000;
    await cleanup(); seal();
  }
  process.exit(0);
}
if (fs.existsSync(path.join(out, 'ledger.json'))) throw new Error('refuse to overwrite retained attempt');
ledger = { work: fs.mkdtempSync('/private/tmp/javelin-suitability-'), mounts: [], groups: [], account: null,
  port: 21000 + Number(process.env.GITHUB_RUN_ID || 1) % 10000 };
saveLedger();
// Everything destructible is confined to this freshly allocated owned root.
process.on('SIGTERM', () => { deadline = 0; });
process.on('SIGINT', () => { deadline = 0; });
try {
  await phase('source', async () => {
    const actual = sourceSnapshot('backend-before');
    const snapshot = path.join(out, 'source'); fs.mkdirSync(snapshot);
    const files = [...Object.keys(actual), ...fs.readdirSync(here).filter(n => fs.statSync(path.join(here, n)).isFile()).map(n => `tools/macos-backend-validation/${n}`), '.github/workflows/macos-backend-suitability.yml'];
    for (const p of files) { const dest = path.join(snapshot, p); fs.mkdirSync(path.dirname(dest), { recursive: true }); fs.copyFileSync(path.join(repo, p), dest); }
    const identity = { sha: process.env.PROOF_SHA, runId: process.env.GITHUB_RUN_ID, attempt: process.env.GITHUB_RUN_ATTEMPT,
      image: process.env.ImageOS, imageVersion: process.env.ImageVersion, arch: process.arch,
      runner: process.env.RUNNER_NAME, backend: '39a065b075f2a55688628eb7b3fd64f75b05334e',
      uid: process.getuid(), gid: process.getgid(), groups: process.getgroups(), startedAt: new Date().toISOString(),
      budgets: { supervisorSeconds: 840, jobMinutes: 20, fixtureBytes: 1073741824, ceilingEntries: 50000 } };
    json(path.join(out, 'identity.json'), identity); return identity;
  });
  await phase('environment', async () => {
    for (const argv of [['/usr/bin/sw_vers'], ['/usr/bin/uname', '-a'], ['/usr/bin/id'], ['/usr/bin/xcrun', '--show-sdk-version'],
      ['/usr/bin/xcodebuild', '-version'], ['rustc', '--version'], ['cargo', '--version'], ['bun', '--version'],
      ['/usr/sbin/diskutil', 'info', ledger.work], ['/usr/bin/sudo', '-n', 'true']]) requireOK(await run(argv));
  });
  const built = await phase('build', async () => {
    requireOK(await run(['cargo', 'build', '--locked', '--release', '--manifest-path', 'tools/macos-sparse-backend/Cargo.toml'],
      { timeout: 200_000, env: { CARGO_TARGET_DIR: path.join(work(), 'target') }, label: 'cargo-build' }));
    requireOK(await run(['/usr/bin/clang', '-Wall', '-Wextra', '-Wno-deprecated-declarations', path.join(here, 'watch.c'),
      '-framework', 'CoreServices', '-o', path.join(work(), 'watch')], { timeout: 30_000, label: 'build-watch' }));
    const hashes = { backend: hash(binary()), watch: hash(path.join(work(), 'watch')) };
    json(path.join(out, 'binaries.json'), hashes); return hashes;
  });
  if (built.state !== 'finished') throw new Error('build prerequisite failed');
  requireOK(await run(['/bin/chmod', '755', work()]));
  await phase('fixture', async () => requireOK(await run(worker('fixture', base()), { timeout: 25_000 })));
  const mounted = await phase('mount', async () => ({ server: await serve(), mount: await mount() }));
  if (mounted.state !== 'finished') throw new Error('hosted mount prerequisite failed');
  await phase('aliases', async () => requireOK(await run(worker('aliases', base(), mnt()), { timeout: 45_000 })));
  await phase('pagination', async () => requireOK(await run(worker('pagination', ledger.port, mnt()), { timeout: 65_000 })));
  await phase('restart', async () => {
    const held = launch(worker('held', mnt()), { timeout: 80_000, label: 'held-fds' });
    await waitFor(path.join(out, held.result.stdout), s => s.includes('"ready": true'));
    const preHandles = requireOK(await run(worker('handles', ledger.port), { timeout: 10_000 }));
    const previousHandle = text(preHandles).trim().split('\n').map(l => JSON.parse(l)).find(v => v.handles).handles.fileHex;
    const beforeMount = requireOK(await run(['/sbin/mount']));
    const old = await stop('SIGKILL'); await sleep(1500);
    const restarted = await serve();
    const postHandles = await run(worker('handles', ledger.port, previousHandle), { timeout: 10_000 });
    held.child.stdin.end('after\n');
    const observation = await held.done;
    const afterMount = requireOK(await run(['/sbin/mount']));
    return { oldServer: old, restarted, observation, preHandles, postHandles, beforeMount: beforeMount.id, afterMount: afterMount.id,
      noUnmountBeforeObservation: true, completedBeforeRecovery: new Date().toISOString() };
  });
  await phase('recovery', async () => {
    const disposal = await unmount('recovery-disposal');
    const mounted = await mount();
    const observation = requireOK(await run(worker('recovered', mnt()), { timeout: 20_000 }));
    return { disposal, mounted, observation };
  });
  await phase('local-access', async () => {
    const users = requireOK(await run(['/usr/bin/dscl', '.', '-list', '/Users', 'UniqueID']));
    const used = new Set(text(users).trim().split('\n').map(l => Number(l.trim().split(/\s+/).at(-1))));
    const uid = Array.from({ length: 100 }, (_, i) => i + 1500).find(n => !used.has(n));
    if (!uid) throw new Error('no unused bounded account UID');
    ledger.account = `jvs${process.env.GITHUB_RUN_ID}`; saveLedger();
    const account = `/Users/${ledger.account}`;
    for (const args of [[], ['UniqueID', String(uid)], ['PrimaryGroupID', '20'], ['NFSHomeDirectory', path.join(work(), 'home')],
      ['UserShell', '/usr/bin/false'], ['RealName', 'Disposable suitability probe'], ['IsHidden', '1']]) {
      requireOK(await run(['/usr/bin/sudo', '-n', '/usr/bin/dscl', '.', '-create', account, ...args], { timeout: 5000 }));
    }
    requireOK(await run(['/usr/bin/sudo', '-n', '-u', ledger.account, '/usr/bin/id']));
    requireOK(await run(['/usr/bin/sudo', '-n', '-u', ledger.account, 'python3', '-I', path.join(here, 'worker.py'),
      'second-user', String(ledger.port), path.join(base(), 'owner-secret')], { timeout: 20_000, label: 'second-user' }));
    return { account: ledger.account, uid, ownerUid: process.getuid() };
  });
  for (const count of [32, 512, 2048]) {
    const r = await phase(`cost-${count}`, async () => {
      const source = path.join(base(), `cost-${count}`);
      const control = path.join(work(), `ordinary-${count}`), cloned = path.join(work(), `clone-${count}`);
      requireOK(await run(['/bin/cp', '-R', source, control], { timeout: 15_000, label: 'prepare-apfs-control' }));
      requireOK(await run(worker('clone', source, cloned), { timeout: 15_000, label: 'prepare-clonefile-control' }));
      for (const [root, kind] of [[control, 'ordinary-apfs'], [cloned, 'direct-clonefile'], [path.join(mnt(), `cost-${count}`), 'nfs']]) {
        requireOK(await run(worker('cost', root, kind === 'nfs' ? upper() : '', count, kind, source),
          { timeout: 100_000, label: `cost-${kind}` }));
      }
    });
    if (r.state !== 'finished' || Date.now() > deadline - 240_000) break;
  }
  await phase('scale-ceiling', async () => ({ status: 'INCONCLUSIVE', requestedCeiling: 50000,
    reason: 'This bounded attempt stops at the deterministic 2048 tier; 50000 capacity is not established.' }));
  await phase('watch-apfs', async () => { const p = path.join(work(), 'watch-control'); fs.mkdirSync(p);
    return requireOK(await run([path.join(work(), 'watch'), p], { timeout: 10_000, label: 'watch-apfs' })); });
  await phase('watch-nfs', async () => requireOK(await run([path.join(work(), 'watch'), path.join(mnt(), 'watch')], { timeout: 15_000, label: 'watch-nfs' })));
  await phase('capabilities', async () => requireOK(await run(worker('capabilities', mnt()), { timeout: 15_000 })));
  await unmount('small-fixture-disposal'); await stop();
  await phase('huge-fixture', async () => {
    if (Date.now() > deadline - 120_000) throw new Error('reserve cleanup budget; skip huge tier');
    return requireOK(await run(worker('huge-fixture', path.join(work(), 'huge-base')), { timeout: 25_000 }));
  });
  const huge = path.join(work(), 'huge-base');
  await phase('truncate-controls', async () => {
    const cloned = path.join(work(), 'huge-clone');
    requireOK(await run(worker('clone', huge, cloned), { timeout: 15_000 }));
    return requireOK(await run(worker('truncate', cloned, '', 'direct-clonefile'), { timeout: 10_000 }));
  });
  await phase('truncate-nfs', async () => {
    await serve(huge, path.join(work(), 'huge-upper')); await mount();
    const result = await run(worker('truncate', mnt(), path.join(work(), 'huge-upper'), 'nfs'), { timeout: 30_000 });
    await unmount('huge-truncate-disposal'); await stop();
    return result;
  });
  await phase('copy-up-stage', async () => {
    const stagedUpper = path.join(work(), 'stage-upper');
    await serve(huge, stagedUpper, { JAVELIN_OVERLAY_CRASH_AT: 'stage' }); await mount();
    const abortedOperation = await run(worker('truncate', mnt(), stagedUpper, 'nfs-stage'), { timeout: 12_000 });
    const diagnostic = requireOK(await run(worker('staged', stagedUpper), { timeout: 15_000 }));
    await unmount('stage-abort-disposal'); const crashed = await stop();
    const ordinary = requireOK(await run(worker('truncate', huge, '', 'ordinary-apfs'), { timeout: 10_000 }));
    return { abortedOperation, diagnostic, crashed, ordinary };
  });
  await phase('source-after', async () => sourceSnapshot('backend-after'));
} catch (e) {
  json(path.join(out, 'fatal.json'), { error: String(e), timestamp: new Date().toISOString() });
} finally {
  // A missing phase is explicit, never inferred successful from a green collector.
  for (const name of phaseNames) if (!fs.existsSync(path.join(out, `phase-${name}.json`))) {
    json(path.join(out, `phase-${name}.json`), { name, state: 'inconclusive',
      reason: 'Earlier prerequisite or budget stopped this phase', finishedAt: new Date().toISOString() });
  }
  const cleaned = await cleanup();
  seal();
  if (!cleaned.verified) process.exitCode = 1;
}
