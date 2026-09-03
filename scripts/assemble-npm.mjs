import { createHash } from 'node:crypto';
import { chmod, copyFile, mkdir, readFile, rm, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const targets = [
  ['aarch64-apple-darwin', 'tar.gz', 'javelin'],
  ['x86_64-unknown-linux-gnu', 'tar.gz', 'javelin'],
  ['x86_64-pc-windows-msvc', 'zip', 'javelin.exe'],
];

export async function assemble(artifacts, packageDir, version, license) {
  const vendor = path.join(packageDir, 'vendor');
  await rm(vendor, { recursive: true, force: true });
  for (const [target, extension, executable] of targets) {
    const name = `javelin-${version}-${target}`;
    const archive = `${name}.${extension}`;
    const archivePath = path.join(artifacts, archive);
    const manifest = (await readFile(`${archivePath}.sha256`, 'utf8')).trim().split(/\s+/);
    if (manifest.length !== 2 || manifest[1] !== archive) {
      throw new Error(`Invalid checksum manifest for ${archive}`);
    }
    const digest = createHash('sha256').update(await readFile(archivePath)).digest('hex');
    if (digest !== manifest[0].toLowerCase()) {
      throw new Error(`Checksum mismatch for ${archive}`);
    }
    const source = path.join(artifacts, name, executable);
    if (!(await stat(source)).isFile()) {
      throw new Error(`Missing native binary: ${source}`);
    }
    const destination = path.join(vendor, target, executable);
    await mkdir(path.dirname(destination), { recursive: true });
    await copyFile(source, destination);
    await chmod(destination, 0o755);
  }
  await copyFile(license, path.join(packageDir, 'LICENSE'));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  const packageDir = path.join(root, 'packages', 'npm');
  const pkg = JSON.parse(await readFile(path.join(packageDir, 'package.json'), 'utf8'));
  const artifacts = path.resolve(process.argv[2] ?? path.join(root, 'release'));
  await assemble(artifacts, packageDir, pkg.version, path.join(root, 'LICENSE'));
}
