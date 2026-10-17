import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { lstat, open, readdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { isDeepStrictEqual } from 'node:util';

const CASES = ['base', 'depth4', 'depth64', 'entries8', 'entries128', 'distinct-blobs', 'distinct-manifests'];
const ACTIONS = ['head', 'accepted', 'stale', 'clone'];
const HASH = /^[a-f0-9]{64}$/;
const MIB = 1024 * 1024;
const PAYLOAD = 262144;

function requireThat(ok, message) {
  if (!ok) throw new Error(message);
}
function equal(actual, expected, message) {
  requireThat(isDeepStrictEqual(actual, expected), message);
}
function integer(value, name, minimum = 0, maximum = Number.MAX_SAFE_INTEGER) {
  requireThat(Number.isSafeInteger(value) && value >= minimum && value <= maximum, `invalid ${name}`);
}
function milliseconds(value, maximum, name) {
  requireThat(Number.isFinite(value) && value >= 0 && value <= maximum, `INCONCLUSIVE: invalid ${name}`);
}
function digest(value, name) {
  requireThat(typeof value === 'string' && HASH.test(value), `invalid ${name}`);
}
function keys(value, expected, name) {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), `invalid ${name}`);
  equal(Object.keys(value).sort(), [...expected].sort(), `unexpected ${name} fields`);
}
function shape(name) {
  return { case: name, depth: name === 'depth4' ? 4 : name === 'depth64' ? 64 : 16,
    entries: name === 'entries8' ? 8 : name === 'entries128' ? 128 : 32,
    shared: name !== 'distinct-blobs', distinct_manifests: name === 'distinct-manifests' };
}
function manifestAt(f, number) {
  const pool = f.shared ? f.shared_manifests : f.distinct_blob_manifests;
  return pool[f.distinct_manifests ? number % f.depth : 0];
}
function world(number, manifest, operation) { return { number, manifest, operation }; }
function seeded(f, empty) {
  return [world(0, empty, null), ...Array.from({ length: f.depth }, (_, i) =>
    world(i + 1, manifestAt(f, i + 1), `seed-${i + 1}`))];
}
function accepted(f, operation) {
  return { outcome: 'accepted', version: world(f.depth + 1, manifestAt(f, f.depth), operation) };
}
function check(actual, f, versions, attempts, layers = 1) {
  equal(actual, { versions, attempts, layers, objects: f.entries + 130 }, 'integrity check counts');
}

// Consume every event in order. A missing, unknown, or moved event cannot be ignored.
function stream(events, child, allowRetry) {
  requireThat(Array.isArray(events) && events.length > 0, `missing records: ${child.id}`);
  let position = 0;
  let cpu = 0;
  function take(type, phase) {
    const event = events[position++];
    requireThat(event?.type === type && (phase === undefined || event.phase === phase),
      `event order: ${child.id} expected ${type}/${phase ?? ''} at ${position - 1}`);
    return event;
  }
  function timingFields(event) {
    integer(event.wall_ns, 'wall_ns', 1, 90000000000);
    integer(event.cpu_ns, 'cpu_ns', 0, 60000000000);
    integer(event.peak_rss_bytes, 'peak_rss_bytes', 1, 256 * MIB);
    cpu += event.cpu_ns;
    requireThat(cpu <= 60000000000, `INCONCLUSIVE: CPU budget ${child.id}`);
  }
  function timing(phase) {
    const event = take('timing', phase);
    timingFields(event);
    equal(event.error, null, `ordinary timing error: ${child.id}/${phase}`);
    return event;
  }
  function calls(phase, operation, expected, manifest) {
    for (let attempt = 0; attempt < 3; attempt++) {
      const event = take('call', phase);
      timingFields(event);
      equal([event.operation, event.expected, event.manifest, event.attempt],
        [operation, expected, manifest, attempt], 'retry input or attempt mismatch');
      if (Object.hasOwn(event.result ?? {}, 'ok')) {
        keys(event.result, ['ok'], 'successful call result');
        return { event, outcome: event.result.ok };
      }
      keys(event.result, ['error'], 'failed call result');
      requireThat(allowRetry && attempt < 2 && ['storage_error', 'io_error'].includes(event.result.error?.code),
        `unpermitted call error: ${child.id}`);
      requireThat(typeof event.result.error.message === 'string', 'missing retry error');
      const reopened = take('reopen', phase);
      equal([reopened.operation, reopened.after_attempt, reopened.error],
        [operation, attempt, null], 'retry needs matching successful reopen');
    }
    throw new Error('INCONCLUSIVE: exhausted retries');
  }
  return { take, timing, calls, done() { equal(position, events.length, `unconsumed events: ${child.id}`); } };
}

function fixture(event, name, context) {
  const f = event.fixture;
  keys(f, ['case', 'depth', 'entries', 'shared', 'distinct_manifests', 'pool', 'shared_manifests',
    'distinct_blob_manifests', 'source_checkpoint'], 'fixture');
  const config = shape(name);
  for (const key of Object.keys(config)) equal(f[key], config[key], `fixture ${key}`);
  digest(f.source_checkpoint, 'checkpoint');
  requireThat(!context.checkpoints.has(f.source_checkpoint), 'fixture reused between actions');
  context.checkpoints.add(f.source_checkpoint);
  requireThat(Array.isArray(event.blobs) && event.blobs.length === f.entries, 'blob pool size');
  equal(event.payload_bytes, PAYLOAD, 'payload total');
  equal(event.verified_blob_reads, f.entries, 'blob readback count');
  equal(event.verified_manifest_reads, 128, 'manifest readback count');
  for (const blob of event.blobs) {
    keys(blob, ['id', 'length', 'sha256'], 'blob');
    digest(blob.id, 'blob ID');
    digest(blob.sha256, 'blob payload SHA256');
    equal(blob.length, PAYLOAD / f.entries, 'blob length');
  }
  equal(f.pool, event.blobs.map(b => b.id), 'fixture blob IDs');
  equal(new Set(f.pool).size, f.entries, 'distinct blob pool');
  equal(new Set(event.blobs.map(b => b.sha256)).size, f.entries, 'distinct blob payloads');
  requireThat(Array.isArray(event.manifests) && event.manifests.length === 128, 'manifest pool size');
  for (let index = 0; index < 128; index++) {
    const m = event.manifests[index];
    keys(m, ['id', 'shared', 'variant', 'canonical', 'length'], 'manifest');
    digest(m.id, 'manifest ID');
    equal([m.shared, m.variant], [index < 64, index % 64], 'manifest pool order');
    const entries = Object.fromEntries(Array.from({ length: f.entries }, (_, i) =>
      [`f${String(i).padStart(3, '0')}`, { kind: 'regular', content: f.pool[m.shared ? 0 : i],
        executable: i < 6 && (m.variant & (1 << i)) !== 0 }]));
    equal(m.canonical, entries, 'manifest canonical entries');
    // Key order does not affect byte length for these fixed ASCII entries.
    equal(m.length, Buffer.byteLength(JSON.stringify(entries)), 'canonical byte length');
  }
  equal(f.shared_manifests, event.manifests.slice(0, 64).map(m => m.id), 'shared manifest IDs');
  equal(f.distinct_blob_manifests, event.manifests.slice(64).map(m => m.id), 'distinct manifest IDs');
  const ids = [...f.pool, ...f.shared_manifests, ...f.distinct_blob_manifests, f.source_checkpoint];
  equal(new Set(ids).size, ids.length, 'object kind or variant ID collision');
  if (context.empty === undefined) {
    context.empty = event.history?.[0]?.manifest;
    digest(context.empty, 'empty World manifest');
  }
  requireThat(!ids.includes(context.empty), 'empty World ID collision');
  equal(event.history, seeded(f, context.empty), 'seeded history');
  check(event.check, f, f.depth + 1, f.depth);
  const pools = { blobs: event.blobs, manifests: event.manifests };
  if (context.pools.has(f.entries)) equal(pools, context.pools.get(f.entries), 'matched fixture pools');
  else context.pools.set(f.entries, pools);
  return f;
}

function prepare(child, events, context) {
  const s = stream(events, child, false);
  const config = shape(child.case);
  s.timing('init');
  for (let i = 0; i < config.entries; i++) s.timing('put_blob');
  for (let i = 0; i < 128; i++) s.timing('put_manifest');
  for (let i = 0; i < config.depth; i++) s.timing('seed_advance');
  s.timing('layer_base');
  s.timing('create_layer');
  const f = fixture(s.take('fixture'), child.case, context);
  s.done();
  return f;
}

function measurement(child, events, f, empty, samples) {
  const race = child.kind === 'race-worker';
  const worker = child.worker ?? '0';
  const action = child.action;
  const s = stream(events, child, race);
  const collect = event => {
    if (!race) samples.push({ case: child.case, action, rep: child.rep, phase: event.phase,
      wall_ns: event.wall_ns, cpu_ns: event.cpu_ns });
  };
  s.timing('open');
  collect(s.timing('head_discovery'));
  collect(s.timing('put_blob_negative_control'));
  let outcome = null;
  let cloneHead = null;
  let replay = null;
  let changedError = null;
  if (['accepted', 'stale', 'race'].includes(action)) {
    const operation = `${action}-${worker}`;
    const expected = action === 'stale' ? f.depth - 1 : f.depth;
    const candidate = manifestAt(f, f.depth);
    const call = s.calls('advance', operation, expected, candidate);
    collect(call.event);
    outcome = call.outcome;
    if (action === 'accepted') equal(outcome, accepted(f, operation), 'accepted outcome');
    else if (action === 'stale') equal(outcome, { outcome: 'stale', actual: f.depth }, 'stale outcome');
    else if (outcome?.outcome === 'accepted') equal(outcome, accepted(f, operation), 'race accepted outcome');
    else equal(outcome, { outcome: 'stale', actual: f.depth + 1 }, 'race stale outcome');
    s.timing('reopen_for_replay');
    replay = s.calls('exact_replay', operation, expected, candidate).outcome;
    equal(replay, outcome, 'replay changed durable outcome');
    changedError = 'operation_id_reused';
  } else if (action === 'clone') collect(s.timing('clone_layer'));
  const result = s.take('measurement');
  s.done();
  equal(result.action, action, 'measurement action');
  equal(result.worker, worker, 'measurement worker');
  equal(result.fixture, f, 'measurement fixture changed');
  equal(result.before, seeded(f, empty), 'premeasurement history');
  equal(result.outcome, outcome, 'measurement outcome differs from call');
  equal(result.replay, replay, 'measurement replay differs from call');
  equal(result.changed_error, changedError, 'changed input rejection');
  let after = seeded(f, empty);
  let attempts = f.depth;
  if (action === 'accepted') { after.push(outcome.version); attempts++; }
  if (action === 'stale') attempts++;
  if (race) {
    requireThat(Array.isArray(result.after), 'race history absent');
    const winner = result.after.at(-1)?.operation;
    requireThat(['race-0', 'race-1'].includes(winner), 'race winner operation');
    equal(winner === `race-${worker}`, outcome.outcome === 'accepted', 'race winner identity');
    after.push(accepted(f, winner).version);
    attempts = result.check?.attempts;
    requireThat(outcome.outcome === 'accepted' ? [f.depth + 1, f.depth + 2].includes(attempts)
      : attempts === f.depth + 2, 'race intermediate attempts');
  }
  equal(result.after, after, 'postmeasurement history');
  if (action === 'clone') {
    cloneHead = result.clone_head;
    keys(cloneHead, ['repository', 'layer', 'revision', 'checkpoint'], 'clone head');
    digest(cloneHead.repository, 'clone repository');
    equal([cloneHead.layer, cloneHead.revision, cloneHead.checkpoint], ['clone', 0, f.source_checkpoint], 'clone head');
  }
  equal(result.clone_head, cloneHead, 'unexpected clone head');
  check(result.check, f, after.length, attempts, action === 'clone' ? 2 : 1);
  return result;
}

function inspect(child, events, f, empty, workers) {
  const s = stream(events, child, true);
  const receipts = ['0', '1'].map(worker => ({ worker,
    outcome: s.calls('race_final_replay', `race-${worker}`, f.depth, manifestAt(f, f.depth)).outcome }));
  const result = s.take('race_final');
  s.done();
  equal(result.fixture, f, 'race final fixture');
  equal(result.receipts, receipts, 'race final receipts');
  for (let i = 0; i < 2; i++) equal(receipts[i].outcome, workers[i].outcome, 'race final replay');
  const winners = receipts.filter(r => r.outcome.outcome === 'accepted');
  equal(winners.length, 1, 'race needs exactly one winner');
  const history = [...seeded(f, empty), winners[0].outcome.version];
  equal(result.history, history, 'race final history');
  for (const worker of workers) equal(worker.after, history, 'race worker final history');
  check(result.check, f, f.depth + 2, f.depth + 2);
}

const median = values => [...values].sort((a, b) => a - b)[1];
function summarize(samples) {
  const medians = [];
  const ratios = [];
  for (const name of CASES) for (const action of ACTIONS) {
    const phases = ['head_discovery', 'put_blob_negative_control',
      ...(['accepted', 'stale'].includes(action) ? ['advance'] : action === 'clone' ? ['clone_layer'] : [])];
    for (const phase of phases) {
      const group = samples.filter(s => s.case === name && s.action === action && s.phase === phase)
        .sort((a, b) => a.rep - b.rep);
      equal(group.map(s => s.rep), [0, 1, 2], 'sample coverage');
      medians.push({ case: name, action, phase, wall_ns: median(group.map(s => s.wall_ns)),
        cpu_ns: median(group.map(s => s.cpu_ns)) });
      if (name === 'base') continue;
      const base = samples.filter(s => s.case === 'base' && s.action === action && s.phase === phase)
        .sort((a, b) => a.rep - b.rep);
      const wall = group.map((s, i) => s.wall_ns / base[i].wall_ns);
      // A zero CPU observation has no defined ratio. Keep it explicit in the evidence.
      const cpu = group.map((s, i) => base[i].cpu_ns === 0 ? null : s.cpu_ns / base[i].cpu_ns);
      ratios.push({ case: name, baseline: 'base', action, phase, wall, cpu,
        wall_median: median(wall), cpu_median: cpu.includes(null) ? null : median(cpu) });
    }
  }
  return { format: 1, samples, medians, ratios };
}

/** records is a Map from child.id to its complete, ordered JSONL event array. */
export function analyze(run, records) {
  requireThat(run?.complete === true, 'INCONCLUSIVE: incomplete run');
  equal(run.format, 1, 'run format');
  equal(run.preregistration, 'b493add', 'preregistration');
  requireThat(typeof run.source_label === 'string' && /^[a-f0-9]{40}$/.test(run.source_label), 'invalid supplied source label');
  digest(run.binary_sha256, 'binary SHA256');
  equal(run.cases, CASES, 'case matrix');
  equal(run.actions, ACTIONS, 'action matrix');
  equal(run.repetitions, 3, 'repetitions');
  equal(run.git_detector, { positive_control: true, product_invocations: 0 }, 'product invocation detector');
  const meta = run.metadata;
  equal(meta?.type, 'metadata', 'binary metadata');
  equal([meta.profile, meta.opt_level, meta.debug_assertions], ['release', '3', false], 'release opt3 binary required');
  requireThat(['false', '0', 'none'].includes(meta.debug), 'debug build metadata');
  digest(meta.native_sha256, 'native fingerprint');
  digest(meta.tool_sha256, 'tool fingerprint');
  for (const field of ['rustc', 'target']) requireThat(typeof meta[field] === 'string' && meta[field].length > 0, `missing ${field}`);
  requireThat(typeof meta.rustflags === 'string', 'missing rustflags');
  requireThat(run.host && typeof run.host === 'object' && Object.keys(run.host).length > 0, 'missing host metadata');
  requireThat(run.source_files && Object.keys(run.source_files).length > 0, 'missing source fingerprints');
  for (const hash of Object.values(run.source_files)) digest(hash, 'source file fingerprint');
  equal(run.budget?.child_wall_ms, 90000, 'child wall budget');
  equal(run.budget?.total_wall_ms, 600000, 'total wall budget');
  for (const [name, value] of Object.entries({ child_cpu_s: 60, child_rss_bytes: 256 * MIB,
    profile_rss_bytes: 512 * MIB, active_fixture_bytes: 128 * MIB, retained_bytes: 1024 * MIB,
    max_native_workers: 2 })) equal(run.budget[name], value, `budget ${name}`);
  milliseconds(run.elapsed_ms, 600000, 'elapsed_ms');
  integer(run.disk_peak_bytes, 'disk peak', 0, 1024 * MIB);
  requireThat(records instanceof Map && Array.isArray(run.children), 'records must be a Map');
  equal(records.size, run.children.length, 'record child inventory');
  const ids = new Set();
  const paths = new Set(['run.json', 'summary.json', 'integrity.json']);
  for (const child of run.children) {
    requireThat(typeof child.id === 'string' && /^[A-Za-z0-9_-]+$/.test(child.id) && !ids.has(child.id), 'duplicate/invalid child ID');
    ids.add(child.id);
    requireThat(records.has(child.id), `missing child records: ${child.id}`);
    for (const path of [child.stdout, child.stderr]) {
      requireThat(typeof path === 'string' && /^[A-Za-z0-9_.-]+$/.test(path) && path !== '.' && path !== '..' && !paths.has(path), 'duplicate/unsafe child output path');
      paths.add(path);
    }
    requireThat(child.exit_code === 0 && child.timed_out === false && child.reason === null,
      `INCONCLUSIVE: failed child ${child.id}`);
    milliseconds(child.wall_ms, 90000, 'child wall_ms');
  }
  let index = 0;
  const context = { pools: new Map(), checkpoints: new Set(), empty: undefined };
  const samples = [];
  function next(name, action, rep, kind, worker) {
    const child = run.children[index++];
    requireThat(child, 'missing child coverage');
    equal([child.case, child.action, child.rep, child.kind], [name, action, rep, kind], 'child coverage/order');
    if (worker !== undefined) equal(child.worker, worker, 'race worker coverage');
    else requireThat(child.worker === undefined || child.worker === '0', 'unexpected worker');
    return [child, records.get(child.id)];
  }
  for (let rep = 0; rep < 3; rep++) {
    const order = [...CASES.slice(rep), ...CASES.slice(0, rep)];
    for (const name of order) for (const action of ACTIONS) {
      const [child, events] = next(name, action, rep, 'prepare');
      const f = prepare(child, events, context);
      measurement(...next(name, action, rep, 'measure'), f, context.empty, samples);
    }
    const [child, events] = next('base', 'race', rep, 'prepare');
    const f = prepare(child, events, context);
    const workers = ['0', '1'].map(worker =>
      measurement(...next('base', 'race', rep, 'race-worker', worker), f, context.empty, samples));
    inspect(...next('base', 'race', rep, 'race-inspect'), f, context.empty, workers);
  }
  equal(index, run.children.length, 'unexpected child coverage');
  return summarize(samples);
}

function sha(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
function parse(bytes, name) {
  try { return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
  catch (error) { throw new Error(`invalid JSON ${name}: ${error.message}`); }
}

/** Reads a bounded snapshot, never follows symlinks, and never writes input files. */
export async function readEvidenceDirectory(directory) {
  const root = resolve(directory);
  requireThat((await lstat(root)).isDirectory() && !(await lstat(root)).isSymbolicLink(), 'unsafe evidence directory');
  const bytes = new Map();
  let total = 0;
  async function load(relative = '') {
    for (const entry of await readdir(join(root, relative), { withFileTypes: true })) {
      const path = relative ? `${relative}/${entry.name}` : entry.name;
      requireThat(!entry.isSymbolicLink(), `symlink in evidence: ${path}`);
      if (entry.isDirectory()) { await load(path); continue; }
      requireThat(entry.isFile(), `nonregular evidence file: ${path}`);
      const handle = await open(join(root, path), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      try {
        const before = await handle.stat();
        requireThat(before.isFile(), `nonregular evidence file: ${path}`);
        total += before.size;
        requireThat(total <= 1024 * MIB && bytes.size < 10000, 'INCONCLUSIVE: evidence exceeds budget');
        const data = await handle.readFile();
        const after = await handle.stat();
        requireThat(before.size === data.length && before.size === after.size && before.mtimeMs === after.mtimeMs
          && before.ctimeMs === after.ctimeMs, `evidence changed while reading: ${path}`);
        bytes.set(path, data);
      } finally { await handle.close(); }
    }
  }
  await load();
  requireThat(bytes.has('integrity.json'), 'missing integrity.json');
  const inventory = parse(bytes.get('integrity.json'), 'integrity.json');
  keys(inventory, ['files'], 'integrity inventory');
  requireThat(inventory.files && typeof inventory.files === 'object' && !Array.isArray(inventory.files), 'invalid file inventory');
  equal(Object.keys(inventory.files).sort(), [...bytes.keys()].filter(p => p !== 'integrity.json').sort(), 'file inventory mismatch');
  for (const [path, hash] of Object.entries(inventory.files)) {
    digest(hash, 'inventory SHA256');
    equal(sha(bytes.get(path)), hash, `SHA256 mismatch: ${path}`);
  }
  return bytes;
}

export async function verifyDirectory(directory) {
  const bytes = await readEvidenceDirectory(directory);
  requireThat(bytes.has('run.json'), 'missing run.json');
  const run = parse(bytes.get('run.json'), 'run.json');
  requireThat(run?.complete === true, 'INCONCLUSIVE: incomplete run');
  requireThat(bytes.has('summary.json'), 'missing summary.json');
  requireThat(Array.isArray(run.children), 'missing child inventory');
  const records = new Map();
  for (const child of run.children) {
    requireThat(bytes.has(child.stdout) && bytes.has(child.stderr), `missing output: ${child.id}`);
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.get(child.stdout));
    requireThat(text.endsWith('\n') && !text.includes('\r') && text.trim().length > 0, `invalid JSONL: ${child.id}`);
    const lines = text.slice(0, -1).split('\n');
    records.set(child.id, lines.map((line, i) => parse(Buffer.from(line), `${child.stdout}:${i + 1}`)));
    requireThat(bytes.get(child.stderr).length === 0, `INCONCLUSIVE: child stderr ${child.id}`);
  }
  const summary = analyze(run, records);
  equal(parse(bytes.get('summary.json'), 'summary.json'), summary, 'recomputed summary mismatch');
  return summary;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    requireThat(process.argv.length === 3, 'usage: bun verify.mjs DIR');
    console.log(JSON.stringify(await verifyDirectory(process.argv[2])));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}

export function checkProfileFixture(events, context) {
  return prepare({id:'profile-prepare',case:'depth64'}, events, context);
}
export function checkProfileLoop(loop, f, empty, action) {
  equal(loop?.type, 'profile_loop', 'profile loop absent');
  equal(loop.action, action, 'profile action');
  equal(loop.iterations, 400, 'profile iteration count');
  integer(loop.wall_ns, 'profile wall', 1, 90000000000);
  integer(loop.cpu_ns, 'profile CPU', 1, 60000000000);
  integer(loop.peak_rss_bytes, 'profile RSS', 1, 512*MIB);
  equal(loop.history, seeded(f, empty), 'profile history');
  check(loop.check, f, f.depth+1, f.depth+(action==='stale'?1:0));
}
