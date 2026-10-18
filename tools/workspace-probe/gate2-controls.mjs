// Negative controls: each lie in gate2-reference.mjs must make its own gate-2 check fail.
// Usage: bun gate2-controls.mjs --dir EMPTY_DIR --shims DIR [--fs LABEL]
import * as fs from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const arg = name => { const i = process.argv.indexOf(`--${name}`); return i < 0 ? undefined : process.argv[i + 1]; };
const dir = resolve(arg('dir')), shims = resolve(arg('shims'));
const controls = [
  ['hardlink', 'tools', 'no-hardlinks'],
  ['copy-claims-clone', 'sharing', 'unchanged-data-shared-with-base'],
  ['stat-only', 'capture', 'mtime-preserving-rewrite'],
  ['touch-layer', 'tools', 'capture-writes-nothing-into-the-layer'],
  ['daemon', 'process', 'no-javelin-process-between-commands'],
  ['torn-discard', 'crash', 'interrupted-discard-recovers-on-next-command'],
  ['no-refusal', 'refusals', 'case-collision'],
];
// A volume that keeps Foo and foo apart may hold that World, so the lie is legal there.
fs.mkdirSync(dir, { recursive: true });
fs.writeFileSync(join(dir, 'Fold'), ''); fs.writeFileSync(join(dir, 'fold'), '');
const foldsCase = fs.readdirSync(dir).length === 1;
for (const n of fs.readdirSync(dir)) fs.rmSync(join(dir, n));

let failures = 0;
for (const [lie, clause, id] of controls) {
  if (lie === 'no-refusal' && !foldsCase) { console.log(`N/A     ${lie}: this volume is case-sensitive, so no World collides here`); continue; }
  const run = join(dir, lie), out = join(dir, `${lie}.json`);
  for (const f of fs.existsSync(shims) ? fs.readdirSync(shims).filter(n => n.startsWith('invoked-')) : []) fs.rmSync(join(shims, f));
  const r = spawnSync(process.execPath, [join(here, 'gate2.mjs'), '--javelin', join(here, 'gate2-reference.mjs'), '--seed', join(here, 'gate2-reference.mjs'), '--dir', run, '--fs', arg('fs') || 'control', '--expect', 'shared', '--out', out, '--shims', shims, '--only', `${clause},process`, '--bulk-mb', '64'],
    { env: { ...process.env, JAVELIN_REFERENCE_LIE: lie }, encoding: 'utf8' });
  const report = fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, 'utf8')) : { checks: [] };
  const target = report.checks.find(c => c.clause === clause && c.id === id);
  const caught = r.status === 1 && target?.verdict === 'NOT VERIFIED';
  if (!caught) failures++;
  console.log(`${caught ? 'CAUGHT ' : 'MISSED '} ${lie} -> ${clause}/${id}${target ? `: ${String(target.detail).slice(0, 160)}` : ' (check did not run)'}`);
  fs.rmSync(run, { recursive: true, force: true });
}
console.log(failures ? `${failures} lies went undetected` : 'every applicable lie detected');
process.exit(failures ? 1 : 0);
