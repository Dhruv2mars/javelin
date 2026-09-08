const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { binaryPath, run } = require('../packages/npm/bin/javelin.cjs');

test('selects only the three released native targets', () => {
  assert.match(binaryPath('darwin', 'arm64'), /aarch64-apple-darwin[\\/]javelin$/);
  assert.match(binaryPath('linux', 'x64'), /x86_64-unknown-linux-gnu[\\/]javelin$/);
  assert.match(binaryPath('win32', 'x64'), /x86_64-pc-windows-msvc[\\/]javelin\.exe$/);
  assert.throws(() => binaryPath('darwin', 'x64'), /Unsupported platform/);
  assert.throws(() => binaryPath('linux', 'arm64'), /Unsupported platform/);
});

test('binary resolution is absolute and independent of the caller directory', () => {
  assert.ok(path.isAbsolute(binaryPath('linux', 'x64')));
});

test('preserves native exit codes', () => {
  for (const code of [0, 2, 4, 5, 6, 7, 8, 10]) {
    assert.equal(run(['-e', `process.exit(${code})`], process.execPath), code);
  }
});

test('reports missing bundled binaries', () => {
  assert.throws(() => run([], path.join(__dirname, 'missing-javelin')), /Could not start Javelin/);
});

test('installs without lifecycle hooks or runtime dependencies', () => {
  const pkg = require('../packages/npm/package.json');
  assert.equal(pkg.name, 'javelin-cli');
  assert.equal(pkg.bin.javelin, 'bin/javelin.cjs');
  assert.equal(pkg.dependencies, undefined);
  assert.equal(pkg.scripts, undefined);
});
