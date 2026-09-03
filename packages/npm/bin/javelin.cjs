#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { spawnSync } = require('node:child_process');

const targets = {
  'darwin-arm64': ['aarch64-apple-darwin', 'javelin'],
  'linux-x64': ['x86_64-unknown-linux-gnu', 'javelin'],
  'win32-x64': ['x86_64-pc-windows-msvc', 'javelin.exe'],
};

function binaryPath(platform = process.platform, arch = process.arch) {
  const target = targets[`${platform}-${arch}`];
  if (!target) {
    throw new Error(`Unsupported platform: ${platform}-${arch}. Supported: macOS arm64, Linux x64, Windows x64.`);
  }
  return path.resolve(__dirname, '..', 'vendor', ...target);
}

function run(args, executable = binaryPath()) {
  const result = spawnSync(executable, args, { stdio: 'inherit', windowsHide: false });
  if (result.error) {
    throw new Error(`Could not start Javelin at ${executable}: ${result.error.message}`);
  }
  if (result.signal) {
    process.kill(process.pid, result.signal);
  }
  return result.status ?? 1;
}

if (require.main === module) {
  try {
    process.exitCode = run(process.argv.slice(2));
  } catch (error) {
    console.error(`javelin: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { binaryPath, run };
