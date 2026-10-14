// Bounded hosted macOS attempt: mounts sparse overlay Layers through loopback
// NFSv3 and exercises them through ordinary paths. Run only on a disposable host:
// it uses sudo for mount_nfs, umount and base ownership. It never invokes Git.
//
// bun attempt.mjs --binary PATH --probe PATH/probe.mjs --work DIR --out DIR
import * as fs from 'node:fs';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';

const args = process.argv.slice(2);
const option = name => { const i = args.indexOf(name); if (i < 0 || !args[i + 1]) throw new Error(`missing ${name}`); return resolve(args[i + 1]); };
const binary = option('--binary'), probe = option('--probe'), work = option('--work'), out = option('--out');
fs.mkdirSync(out, { recursive: true });
fs.mkdirSync(work);
const commandLog = join(out, 'commands.jsonl');
const cap = s => (s.length > 65536 ? s.slice(0, 65536) + '\n[truncated]' : s);
const summary = { schema: 1, startedAt: new Date().toISOString(), checks: [], cleanup: {} };

function run(argv, { timeoutMs = 60000, cwd } = {}) {
  const started = Date.now();
  const r = Bun.spawnSync(argv, { cwd, stdout: 'pipe', stderr: 'pipe', timeout: timeoutMs });
  const rec = { argv, cwd, exitCode: r.exitCode, signal: r.signalCode ?? null, elapsedMs: Date.now() - started,
    stdout: cap(r.stdout.toString()), stderr: cap(r.stderr.toString()) };
  fs.appendFileSync(commandLog, JSON.stringify(rec) + '\n');
  return rec;
}
const must = (argv, opts) => { const r = run(argv, opts); if (r.exitCode !== 0) throw new Error(`${argv.join(' ')} exited ${r.exitCode}: ${r.stderr}`); return r; };

async function check(id, fn) {
  try { summary.checks.push({ id, verdict: 'VERIFIED', evidence: (await fn()) ?? null }); }
  catch (e) { summary.checks.push({ id, verdict: 'NOT VERIFIED', evidence: e.message }); }
}
const assert = (ok, message) => { if (!ok) throw new Error(message); };

// Path, type, size and SHA-256 of every entry; read through ordinary paths.
function tree(root) {
  const rows = [];
  (function visit(rel) {
    const p = join(root, rel), st = fs.lstatSync(p);
    if (st.isSymbolicLink()) rows.push([rel, 'symlink', fs.readlinkSync(p)]);
    else if (st.isDirectory()) { rows.push([rel, 'dir']); for (const n of fs.readdirSync(p).sort()) visit(join(rel, n)); }
    else rows.push([rel, 'file', st.size, createHash('sha256').update(fs.readFileSync(p)).digest('hex')]);
  })('');
  return rows;
}
const digest = rows => createHash('sha256').update(JSON.stringify(rows)).digest('hex');
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

const servers = new Map(), mounts = new Set();
const port = { a: 12051, b: 12052, c: 12053, d: 12054 };
const upper = name => join(work, `upper-${name}`);
const mnt = name => join(work, `mnt-${name}`);
const base = join(work, 'base');

async function serve(name) {
  const child = Bun.spawn([binary, 'serve', '--base', base, '--upper', upper(name), '--port', String(port[name])],
    { stdout: 'pipe', stderr: Bun.file(join(out, `server-${name}.stderr`)) });
  const reader = child.stdout.getReader();
  const line = await Promise.race([reader.read().then(r => new TextDecoder().decode(r.value ?? new Uint8Array())),
    Bun.sleep(10000).then(() => 'timeout')]);
  reader.releaseLock();
  fs.appendFileSync(commandLog, JSON.stringify({ server: name, pid: child.pid, firstLine: line }) + '\n');
  assert(line.startsWith('listening 127.0.0.1:'), `server ${name} not ready: ${line}`);
  servers.set(name, child);
}
async function stop(name, signal = 'SIGINT') {
  const child = servers.get(name); servers.delete(name);
  child.kill(signal);
  const code = await Promise.race([child.exited, Bun.sleep(10000).then(() => 'timeout')]);
  fs.appendFileSync(commandLog, JSON.stringify({ server: name, signal, exited: code }) + '\n');
  assert(code !== 'timeout', `server ${name} did not exit`);
}
function mount(name) {
  fs.mkdirSync(mnt(name), { recursive: true });
  const p = port[name];
  must(['sudo', '-n', '/sbin/mount_nfs', '-o', `vers=3,tcp,port=${p},mountport=${p},locallocks,noac,nonegnamecache,soft`,
    '127.0.0.1:/', mnt(name)], { timeoutMs: 30000 });
  mounts.add(name);
}
function unmount(name, force = false) {
  const r = run(['sudo', '-n', '/sbin/umount', ...(force ? ['-f'] : []), mnt(name)], { timeoutMs: 30000 });
  if (r.exitCode !== 0 && !force) return unmount(name, true);
  if (r.exitCode === 0) mounts.delete(name);
  return r;
}

let fatal;
try {
  summary.host = Object.fromEntries([['sw_vers', ['sw_vers']], ['uname', ['uname', '-a']], ['csrutil', ['csrutil', 'status']],
    ['mount_nfs', ['ls', '-l', '/sbin/mount_nfs']], ['user', ['id']]].map(([k, argv]) => [k, run(argv).stdout.trim()]));
  summary.source = { binarySha256: createHash('sha256').update(fs.readFileSync(binary)).digest('hex'),
    probeSha256: createHash('sha256').update(fs.readFileSync(probe)).digest('hex') };

  fs.mkdirSync(base);
  must(['bun', probe, '--seed', base]);
  const baseRows = tree(base);
  summary.base = { entries: baseRows.length, digest: digest(baseRows) };
  must(['sudo', '-n', 'chown', '-R', 'root:wheel', base]);
  await check('base-not-writable-by-layer-server-user', () => {
    const r = run(['sh', '-c', `printf x > '${join(base, 'README.txt')}'`]);
    assert(r.exitCode !== 0, 'runner user could write the shared base');
    assert(same(tree(base), baseRows), 'base changed');
    return r.stderr.trim();
  });

  for (const n of ['a', 'b', 'c']) { await serve(n); mount(n); }
  summary.mountTable = run(['mount']).stdout.split('\n').filter(l => l.includes(work));
  await check('mounted-views-are-nfs-and-match-base', () => {
    for (const n of ['a', 'b', 'c']) {
      const line = summary.mountTable.find(l => l.includes(` on ${mnt(n)} (`));
      assert(line?.startsWith('127.0.0.1:/ on ') && line.includes('(nfs'), `${n} is not an NFS mount: ${line}`);
      assert(same(tree(mnt(n)), baseRows), `${n} view differs from base`);
    }
    return summary.mountTable;
  });

  fs.writeFileSync(join(work, 'paths.json'), JSON.stringify({ base, layerA: mnt('a'), layerB: mnt('b'), clone: mnt('c') }));
  const probeRun = run(['bun', probe, '--backend', 'paths', '--paths', join(work, 'paths.json'),
    '--out', join(out, 'probe-paths.json')], { timeoutMs: 90000 });
  summary.probe = { exitCode: probeRun.exitCode, stdout: probeRun.stdout };
  const probeReport = JSON.parse(fs.readFileSync(join(out, 'probe-paths.json'), 'utf8'));
  for (const c of probeReport.checks) {
    summary.checks.push({ id: `probe:${c.id}`, verdict: c.verdict,
      evidence: c.id === 'layer-clone-snapshot-isolation' ? 'paths mode cannot clone; see quiescent-clone-and-remount' : c.evidence ?? null });
  }

  await check('busy-layer-clone-refused', () => {
    const r = run([binary, 'clone', '--from', upper('b'), '--to', upper('x')]);
    assert(r.exitCode === 2 && r.stderr.includes('clone refused'), `exit ${r.exitCode}: ${r.stderr}`);
    assert(!fs.existsSync(upper('x')), 'refused clone left output');
    return r.stderr.trim();
  });

  const beforeA = tree(mnt('a'));
  await check('quiescent-clone-and-remount', async () => {
    unmount('a'); await stop('a');
    must([binary, 'clone', '--from', upper('a'), '--to', upper('d')]);
    for (const n of ['a', 'd']) { await serve(n); mount(n); }
    assert(same(tree(mnt('a')), beforeA), 'remounted A differs from its pre-unmount state');
    assert(same(tree(mnt('d')), beforeA), 'clone differs from quiescent A');
    fs.writeFileSync(join(mnt('a'), 'README.txt'), 'A after clone\n');
    fs.writeFileSync(join(mnt('d'), 'value.h'), '#define VALUE 11\n');
    assert(fs.readFileSync(join(mnt('d'), 'README.txt'), 'utf8') !== 'A after clone\n', 'A write leaked into clone');
    assert(fs.readFileSync(join(mnt('a'), 'value.h'), 'utf8') !== '#define VALUE 11\n', 'clone write leaked into A');
    assert(same(tree(mnt('b')), baseRows), 'peer B changed');
    must(['cc', '-Wall', '-Wextra', '-Werror', 'main.c', '-o', 'probe-app'], { cwd: mnt('d') });
    assert(must([join(mnt('d'), 'probe-app')]).stdout === '11\n', 'clone compile output');
    fs.unlinkSync(join(mnt('d'), 'probe-app'));
    return 'A unmounted and server stopped before clone; A remounted unchanged; clone diverged independently';
  });

  await check('backend-kill-recovery', async () => {
    const fd = fs.openSync(join(mnt('a'), 'recovered.txt'), 'w');
    fs.writeSync(fd, 'durable before kill\n'); fs.fsyncSync(fd); fs.closeSync(fd);
    const before = tree(mnt('a'));
    await stop('a', 'SIGKILL');
    const r = unmount('a', true);
    assert(r.exitCode === 0, `forced unmount failed: ${r.stderr}`);
    await serve('a'); mount('a');
    assert(same(tree(mnt('a')), before), 'state after kill and remount differs');
    assert(fs.readdirSync(join(upper('a'), 'tmp')).length === 0, 'staging not cleared');
    return 'server SIGKILL with mount active; forced unmount; restart and remount preserved fsynced state';
  });

  await check('base-unchanged-and-private-state-sparse', () => {
    assert(same(tree(base), baseRows), 'base changed');
    const storage = {};
    for (const n of ['a', 'b', 'c', 'd']) {
      const rows = tree(join(upper(n), 'tree'));
      const whiteouts = fs.existsSync(join(upper(n), 'whiteouts')) ? fs.readFileSync(join(upper(n), 'whiteouts'), 'utf8').split('\n').filter(Boolean) : [];
      storage[n] = { rows, whiteouts, fileBytes: rows.filter(r => r[1] === 'file').reduce((s, r) => s + r[2], 0) };
    }
    summary.storage = storage;
    const baseFiles = baseRows.filter(r => r[1] === 'file');
    for (const n of ['b', 'c']) assert(!storage[n].rows.some(r => r[1] === 'file'), `${n} holds private files`);
    for (const n of ['a', 'd']) {
      const unchangedCopies = storage[n].rows.filter(r => r[1] === 'file' && baseFiles.some(b => b[0] === r[0] && b[3] === r[3]));
      // Directory rename materializes moved children; that is the documented exception.
      assert(unchangedCopies.every(r => r[0].startsWith('renamed-dir/')), `${n} copied unchanged base files: ${JSON.stringify(unchangedCopies)}`);
      assert(!storage[n].rows.some(r => r[0] === 'main.c'), `${n} copied unchanged main.c`);
    }
    return { baseFileBytes: baseFiles.reduce((s, r) => s + r[2], 0), privateFileBytes: Object.fromEntries(Object.entries(storage).map(([k, v]) => [k, v.fileBytes])) };
  });
} catch (e) { fatal = e.message; }
finally {
  for (const n of [...mounts]) unmount(n);
  for (const n of [...servers.keys()]) await stop(n).catch(e => { summary.cleanup[`stop-${n}`] = e.message; });
  const leftover = run(['mount']).stdout.split('\n').filter(l => l.includes(work));
  summary.cleanup.remainingMounts = leftover;
  summary.cleanup.remainingServers = [...servers.keys()];
  if (leftover.length === 0) {
    run(['sudo', '-n', 'chown', '-R', `${process.getuid()}`, work]);
    summary.cleanup.removeWork = run(['rm', '-rf', work]).exitCode;
  }
  summary.fatal = fatal ?? null;
  summary.finishedAt = new Date().toISOString();
  const required = summary.checks.filter(c => c.id !== 'probe:layer-clone-snapshot-isolation');
  summary.verdict = fatal || required.length === 0 ? 'INCONCLUSIVE'
    : required.every(c => c.verdict === 'VERIFIED') && leftover.length === 0 ? 'VERIFIED' : 'NOT VERIFIED';
  fs.writeFileSync(join(out, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
  console.log(`${summary.verdict}${fatal ? `: ${fatal}` : ''}`);
  for (const c of summary.checks) console.log(`${c.verdict}: ${c.id}`);
  process.exit(summary.verdict === 'VERIFIED' ? 0 : 1);
}
