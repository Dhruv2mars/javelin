import { open, rename } from 'node:fs/promises';
import { dirname } from 'node:path';

const [target, phase, contents] = process.argv.slice(2);
// Also bound the child's life if the parent is interrupted.
setTimeout(() => process.exit(124), 10000);
const temporary = `${target}.saving`;
const file = await open(temporary, 'wx');
await file.writeFile(contents);
await file.sync();
await file.close();
if (phase === 'after') {
  await rename(temporary, target);
  const directory = await open(dirname(target), 'r');
  await directory.sync();
  await directory.close();
}
process.stdout.write('ready\n');
setInterval(() => {}, 1000);
