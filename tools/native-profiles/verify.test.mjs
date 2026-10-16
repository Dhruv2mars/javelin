import { afterAll, describe, expect, test } from 'bun:test';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { analyze, verifyDirectory } from './verify.mjs';

const CASES = ['base', 'depth4', 'depth64', 'entries8', 'entries128', 'distinct-blobs', 'distinct-manifests'];
const ACTIONS = ['head', 'accepted', 'stale', 'clone'];
const hash = value => createHash('sha256').update(value).digest('hex');
const fakeID = label => hash(`SYNTHETIC TEST ONLY ${label}`);
const empty = fakeID('empty');
const pools = new Map();
const directories = [];
afterAll(async () => { await Promise.all(directories.map(dir => rm(dir, { recursive: true, force: true }))); });

// These IDs are arbitrary labels, not native hashes. This builder tests recorded
// consistency only; it supplies no accepted native or performance evidence.
function fixture(name, id) {
  const entries = name === 'entries8' ? 8 : name === 'entries128' ? 128 : 32;
  if (!pools.has(entries)) {
    const blobs = Array.from({ length: entries }, (_, i) => ({ id: fakeID(`blob-${entries}-${i}`),
      length: 262144 / entries, sha256: fakeID(`payload-${entries}-${i}`) }));
    const manifests = Array.from({ length: 128 }, (_, index) => {
      const shared = index < 64;
      const variant = index % 64;
      const canonical = Object.fromEntries(blobs.map((blob, i) => [`f${String(i).padStart(3, '0')}`,
        { kind: 'regular', content: blobs[shared ? 0 : i].id, executable: i < 6 && (variant & (1 << i)) !== 0 }]));
      return { id: fakeID(`manifest-${entries}-${index}`), shared, variant, canonical,
        length: Buffer.byteLength(JSON.stringify(canonical)) };
    });
    pools.set(entries, { blobs, manifests });
  }
  const { blobs, manifests } = pools.get(entries);
  const f = { case: name, depth: name === 'depth4' ? 4 : name === 'depth64' ? 64 : 16, entries,
    shared: name !== 'distinct-blobs', distinct_manifests: name === 'distinct-manifests',
    pool: blobs.map(b => b.id), shared_manifests: manifests.slice(0, 64).map(m => m.id),
    distinct_blob_manifests: manifests.slice(64).map(m => m.id), source_checkpoint: fakeID(id) };
  return { type: 'fixture', fixture: f, blobs, manifests, history: history(f), check: check(f),
    payload_bytes: 262144, verified_blob_reads: entries, verified_manifest_reads: 128 };
}
function at(f, n) {
  return (f.shared ? f.shared_manifests : f.distinct_blob_manifests)[f.distinct_manifests ? n % f.depth : 0];
}
function history(f) {
  return [{ number: 0, manifest: empty, operation: null }, ...Array.from({ length: f.depth }, (_, i) =>
    ({ number: i + 1, manifest: at(f, i + 1), operation: `seed-${i + 1}` }))];
}
function check(f, versions = f.depth + 1, attempts = f.depth, layers = 1) {
  return { versions, objects: f.entries + 130, attempts, layers };
}
function timing(phase, wall_ns = 100, cpu_ns = 10) {
  return { type: 'timing', phase, wall_ns, cpu_ns, peak_rss_bytes: 1024, error: null };
}
function accepted(f, operation) {
  return { outcome: 'accepted', version: { number: f.depth + 1, manifest: at(f, f.depth), operation } };
}
function call(phase, f, action, worker, outcome) {
  return { type: 'call', phase, operation: `${action}-${worker}`, expected: action === 'stale' ? f.depth - 1 : f.depth,
    manifest: at(f, f.depth), attempt: 0, wall_ns: 100, cpu_ns: 10, peak_rss_bytes: 1024, result: { ok: outcome } };
}
function prepared(event) {
  const f = event.fixture;
  return [timing('init'), ...f.pool.map(() => timing('put_blob')),
    ...Array.from({ length: 128 }, () => timing('put_manifest')),
    ...Array.from({ length: f.depth }, () => timing('seed_advance')),
    timing('layer_base'), timing('create_layer'), event];
}
function measured(f, action, worker = '0') {
  const events = [timing('open'), timing('head_discovery'), timing('put_blob_negative_control')];
  const result = { type: 'measurement', action, worker, fixture: f, before: history(f), after: history(f),
    outcome: null, replay: null, changed_error: null, clone_head: null, check: check(f) };
  if (['accepted', 'stale', 'race'].includes(action)) {
    const winner = accepted(f, action === 'race' ? 'race-0' : 'accepted-0');
    const outcome = action === 'stale' ? { outcome: 'stale', actual: f.depth }
      : action === 'race' && worker === '1' ? { outcome: 'stale', actual: f.depth + 1 } : winner;
    events.push(call('advance', f, action, worker, outcome), timing('reopen_for_replay'),
      call('exact_replay', f, action, worker, outcome));
    result.outcome = outcome;
    result.replay = outcome;
    result.changed_error = 'operation_id_reused';
    if (action !== 'stale') result.after.push(winner.version);
    result.check = check(f, result.after.length, f.depth + (action === 'race' ? 2 : 1));
  } else if (action === 'clone') {
    events.push(timing('clone_layer'));
    result.clone_head = { repository: fakeID('repository'), layer: 'clone', revision: 0, checkpoint: f.source_checkpoint };
    result.check.layers = 2;
  }
  return [...events, result];
}
function syntheticRun() {
  const run = { format: 1, source_label: 'e56fc86e3415217de2b960a7ea0dbe6d2eb450b1', preregistration: 'b493add',
    metadata: { type: 'metadata', native_sha256: fakeID('native'), tool_sha256: fakeID('tool'), rustc: 'synthetic rustc',
      target: 'synthetic-target', profile: 'release', opt_level: '3', debug: 'false', debug_assertions: false, rustflags: '' },
    binary_sha256: fakeID('binary'), host: { synthetic_test_only: true }, cases: CASES, actions: ACTIONS,
    repetitions: 3, complete: true, children: [], disk_peak_bytes: 1024,
    budget: { child_wall_ms: 90000, total_wall_ms: 600000, child_cpu_s: 60, child_rss_bytes: 268435456,
      profile_rss_bytes: 536870912, active_fixture_bytes: 134217728, retained_bytes: 1073741824, max_native_workers: 2 }, elapsed_ms: 1.5,
    git_detector: { positive_control: true, product_invocations: 0 }, source_files: { 'synthetic.rs': fakeID('source') } };
  const records = new Map();
  function child(name, action, rep, kind, events, worker) {
    const id = `r${rep}-${name}-${action}-${kind}${worker === undefined ? '' : `-${worker}`}`;
    run.children.push({ id, case: name, action, rep, kind, ...(worker === undefined ? {} : { worker }),
      stdout: `${id}.jsonl`, stderr: `${id}.stderr`, exit_code: 0, timed_out: false, wall_ms: 1.5, reason: null });
    records.set(id, events);
  }
  for (let rep = 0; rep < 3; rep++) {
    for (const name of [...CASES.slice(rep), ...CASES.slice(0, rep)]) for (const action of ACTIONS) {
      const event = fixture(name, `${rep}-${name}-${action}`);
      child(name, action, rep, 'prepare', prepared(event));
      child(name, action, rep, 'measure', measured(event.fixture, action));
    }
    const event = fixture('base', `${rep}-race`);
    child('base', 'race', rep, 'prepare', prepared(event));
    const workers = ['0', '1'].map(worker => {
      const events = measured(event.fixture, 'race', worker);
      child('base', 'race', rep, 'race-worker', events, worker);
      return events.at(-1);
    });
    const f = event.fixture;
    const receipts = workers.map(w => ({ worker: w.worker, outcome: w.outcome }));
    child('base', 'race', rep, 'race-inspect', [
      ...receipts.map(r => call('race_final_replay', f, 'race', r.worker, r.outcome)),
      { type: 'race_final', fixture: f, history: workers[0].after, receipts, check: check(f, f.depth + 2, f.depth + 2) }]);
  }
  return { run, records };
}
function select(data, kind, action = 'head', name = 'base', rep = 0) {
  const child = data.run.children.find(c => c.kind === kind && c.action === action && c.case === name && c.rep === rep);
  return { child, events: data.records.get(child.id) };
}
function retry(data) {
  const { events } = select(data, 'race-worker', 'race');
  const success = events[3];
  const failure = { ...success, result: { error: { code: 'storage_error', message: 'synthetic lock' } } };
  const reopen = { type: 'reopen', phase: success.phase, operation: success.operation, after_attempt: 0, error: null };
  events.splice(3, 1, failure, reopen, { ...success, attempt: 1 });
  return events;
}

describe('independent evidence analysis with explicitly synthetic records', () => {
  test('complete fixture matrix yields exact samples and medians without mutating inputs', () => {
    const data = syntheticRun();
    const summary = analyze(data.run, data.records);
    expect(summary.samples).toHaveLength(231);
    expect(summary.medians).toHaveLength(77);
    expect(summary.ratios).toHaveLength(66);
    expect(summary.samples.every(s => s.wall_ns === 100 && s.cpu_ns === 10)).toBe(true);
    expect(analyze(data.run, data.records)).toEqual(summary);
  });
  test('paired ratio median is not a ratio of medians, and zero CPU ratios stay null', () => {
    const data = syntheticRun();
    for (let rep = 0; rep < 3; rep++) {
      select(data, 'measure', 'head', 'base', rep).events[1].wall_ns = [1, 10, 100][rep];
      select(data, 'measure', 'head', 'depth4', rep).events[1].wall_ns = [10, 20, 10][rep];
    }
    select(data, 'measure').events[1].cpu_ns = 0;
    const ratio = analyze(data.run, data.records).ratios.find(r => r.case === 'depth4' && r.action === 'head' && r.phase === 'head_discovery');
    expect(ratio.wall).toEqual([10, 2, 0.1]);
    expect(ratio.wall_median).toBe(2);
    expect(ratio.cpu).toEqual([null, 1, 1]);
    expect(ratio.cpu_median).toBeNull();
  });
  test('valid race retry retains exact inputs and requires a successful reopen', () => {
    const data = syntheticRun();
    const events = retry(data);
    expect(() => analyze(data.run, data.records)).not.toThrow();
    events.splice(4, 1);
    expect(() => analyze(data.run, data.records)).toThrow('event order');
  });
  test.each(['expected', 'manifest', 'operation', 'attempt'])('changed retry %s is rejected', field => {
    const data = syntheticRun();
    const events = retry(data);
    events[5][field] = field === 'attempt' ? 2 : field === 'expected' ? 17 : 'changed';
    expect(() => analyze(data.run, data.records)).toThrow('retry input');
  });
  test('failed or reordered reopen is rejected', () => {
    const data = syntheticRun();
    const events = retry(data);
    events[4].error = { code: 'io_error', message: 'synthetic failure' };
    expect(() => analyze(data.run, data.records)).toThrow('successful reopen');
    events[4].error = null;
    [events[4], events[5]] = [events[5], events[4]];
    expect(() => analyze(data.run, data.records)).toThrow('event order');
  });
  test('ordinary advance errors cannot become successful samples', () => {
    const data = syntheticRun();
    select(data, 'measure', 'accepted').events[3].result = { error: { code: 'storage_error', message: 'synthetic error' } };
    expect(() => analyze(data.run, data.records)).toThrow('unpermitted call error');
  });
  test.each(['complete', 'timeout', 'exit', 'reason'])('incomplete %s evidence is inconclusive', mode => {
    const data = syntheticRun();
    if (mode === 'complete') data.run.complete = false;
    if (mode === 'timeout') data.run.children[0].timed_out = true;
    if (mode === 'exit') data.run.children[0].exit_code = 1;
    if (mode === 'reason') data.run.children[0].reason = 'budget stop';
    expect(() => analyze(data.run, data.records)).toThrow('INCONCLUSIVE');
  });
  test('exact coverage, ordering and every meaningful event are required', () => {
    const data = syntheticRun();
    const { events } = select(data, 'measure');
    events.push({ type: 'unknown' });
    expect(() => analyze(data.run, data.records)).toThrow('unconsumed');
    events.pop();
    events.splice(1, 1);
    expect(() => analyze(data.run, data.records)).toThrow('event order');
    data.run.children.pop();
    expect(() => analyze(data.run, data.records)).toThrow('inventory');
  });
  test('debug or unoptimized binaries cannot be accepted', () => {
    const data = syntheticRun();
    data.run.metadata.debug_assertions = true;
    expect(() => analyze(data.run, data.records)).toThrow('release opt3');
    data.run.metadata.debug_assertions = false;
    data.run.metadata.opt_level = '2';
    expect(() => analyze(data.run, data.records)).toThrow('release opt3');
  });
  test.each(['history', 'check', 'readback', 'canonical', 'length', 'pool', 'clone', 'changed', 'race'])('%s binding is independently checked', mode => {
    const data = syntheticRun();
    const prepared = select(data, 'prepare').events;
    prepared[prepared.length - 1] = structuredClone(prepared.at(-1));
    const event = prepared.at(-1);
    if (mode === 'history') event.history[1].operation = 'unknown';
    if (mode === 'check') event.check.attempts++;
    if (mode === 'readback') event.verified_manifest_reads--;
    if (mode === 'canonical') event.manifests[0].canonical.f000.content = event.fixture.pool[1];
    if (mode === 'length') event.manifests[1].length++;
    if (mode === 'pool') event.fixture.pool[1] = event.fixture.pool[0];
    if (mode === 'clone') select(data, 'measure', 'clone').events.at(-1).clone_head.checkpoint = fakeID('wrong');
    if (mode === 'changed') select(data, 'measure', 'accepted').events.at(-1).changed_error = null;
    if (mode === 'race') select(data, 'race-worker', 'race').events.at(-1).check.attempts = 100;
    expect(() => analyze(data.run, data.records)).toThrow();
  });
  test('cross-case pool changes fail even when local IDs and lengths agree', () => {
    const data = syntheticRun();
    const events = select(data, 'prepare', 'head', 'depth4').events;
    events[events.length - 1] = structuredClone(events.at(-1));
    events.at(-1).blobs[0].sha256 = fakeID('altered payload');
    expect(() => analyze(data.run, data.records)).toThrow('matched fixture pools');
  });
});

async function evidenceDirectory() {
  const data = syntheticRun();
  const dir = await mkdtemp(join(tmpdir(), 'javelin-synthetic-verifier-'));
  directories.push(dir);
  const files = new Map([
    ['run.json', Buffer.from(JSON.stringify(data.run))],
    ['summary.json', Buffer.from(JSON.stringify(analyze(data.run, data.records)))],
  ]);
  for (const child of data.run.children) {
    files.set(child.stdout, Buffer.from(data.records.get(child.id).map(e => JSON.stringify(e)).join('\n') + '\n'));
    files.set(child.stderr, Buffer.alloc(0));
  }
  const inventory = { files: Object.fromEntries([...files].map(([name, bytes]) => [name, hash(bytes)])) };
  await Promise.all([...files].map(([name, bytes]) => writeFile(join(dir, name), bytes)));
  await writeFile(join(dir, 'integrity.json'), JSON.stringify(inventory));
  return { ...data, dir, files, inventory };
}
async function replaceFile(data, name, content) {
  const bytes = Buffer.from(content);
  data.inventory.files[name] = hash(bytes);
  await writeFile(join(data.dir, name), bytes);
  await writeFile(join(data.dir, 'integrity.json'), JSON.stringify(data.inventory));
}

test('directory integrity, rehashed semantic tampering, summary recomputation, and read-only verification', async () => {
  const data = await evidenceDirectory();
  expect(await verifyDirectory(data.dir)).toEqual(analyze(data.run, data.records));
  const target = select(data, 'measure', 'accepted');
  const path = join(data.dir, target.child.stdout);
  await writeFile(path, Buffer.concat([data.files.get(target.child.stdout), Buffer.from(' ')]));
  await expect(verifyDirectory(data.dir)).rejects.toThrow('SHA256 mismatch');
  await writeFile(path, data.files.get(target.child.stdout));

  // The unchanged, matching summary and freshly rehashed inventory cannot conceal
  // a changed native check report, because analysis does not trust summary.json.
  target.events.at(-1).check.attempts++;
  await replaceFile(data, target.child.stdout, target.events.map(e => JSON.stringify(e)).join('\n') + '\n');
  await expect(verifyDirectory(data.dir)).rejects.toThrow('check counts');
  await replaceFile(data, target.child.stdout, data.files.get(target.child.stdout));
  const summary = JSON.parse(data.files.get('summary.json'));
  summary.samples[0].wall_ns++;
  await replaceFile(data, 'summary.json', JSON.stringify(summary));
  await expect(verifyDirectory(data.dir)).rejects.toThrow('recomputed summary');
  await replaceFile(data, 'summary.json', data.files.get('summary.json'));
  const before = await readFile(join(data.dir, 'integrity.json'));
  await verifyDirectory(data.dir);
  expect(await readFile(join(data.dir, 'integrity.json'))).toEqual(before);
  for (const [name, bytes] of data.files) expect(await readFile(join(data.dir, name))).toEqual(bytes);

  await writeFile(join(data.dir, 'unlisted'), 'unlisted bytes');
  await expect(verifyDirectory(data.dir)).rejects.toThrow('inventory mismatch');
  await rm(join(data.dir, 'unlisted'));
  await symlink('summary.json', join(data.dir, 'link'));
  await expect(verifyDirectory(data.dir)).rejects.toThrow('symlink');
  await rm(join(data.dir, 'link'));
  data.run.complete = false;
  await replaceFile(data, 'run.json', JSON.stringify(data.run));
  // A killed child can leave truncated JSONL and no summary. Retain both facts
  // and classify the intact evidence inventory as an incomplete measurement.
  await replaceFile(data, target.child.stdout, '{"type":"call"');
  await rm(join(data.dir, 'summary.json'));
  delete data.inventory.files['summary.json'];
  await writeFile(join(data.dir, 'integrity.json'), JSON.stringify(data.inventory));
  await expect(verifyDirectory(data.dir)).rejects.toThrow('INCONCLUSIVE');
});
