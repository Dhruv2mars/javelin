// Rewrites two self-verifying files for a fixed time: one in place, one by atomic rename.
// Each version is `gen N` plus filler, followed by `\nsum <sha256 of everything before>`.
import * as fs from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { join } from 'node:path';

const [dir, ms] = process.argv.slice(2);
const until = Date.now() + Number(ms);
const version = n => { const body = `gen ${n}\n${randomBytes(48 << 10).toString('hex')}`; return `${body}\nsum ${createHash('sha256').update(body).digest('hex')}\n`; };
let n = 0, retries = 0;
const pause = new Int32Array(new SharedArrayBuffer(4));
while (Date.now() < until) {
  n++;
  const fd = fs.openSync(join(dir, 'inplace.txt'), fs.existsSync(join(dir, 'inplace.txt')) ? 'r+' : 'w');
  fs.writeSync(fd, version(n), 0); fs.closeSync(fd);
  fs.writeFileSync(join(dir, 'atomic.txt.tmp'), version(n));
  // Windows refuses to replace a file another process has open; editors retry, so do we.
  for (let tries = 0; ; tries++) {
    try { fs.renameSync(join(dir, 'atomic.txt.tmp'), join(dir, 'atomic.txt')); break; }
    catch (e) { if (!['EPERM', 'EBUSY', 'EACCES'].includes(e.code) || tries > 200) throw e; retries++; Atomics.wait(pause, 0, 0, 5); }
  }
}
console.log(`${n} generations, ${retries} rename retries`);
