// postinstall for @dhruv2mars/javelin: puts the platform package's binaries in place of the
// bin shims, so `javelin` starts the native program directly, as esbuild does. Hard link,
// or copy when the link fails, then rename over the shim. Any failure keeps the shim,
// which finds and runs the same binary itself, so the install never fails here.
const fs = require("fs");
const path = require("path");

function optimize() {
  if (process.platform === "win32") return;
  let dir;
  try {
    dir = path.dirname(require.resolve(`@dhruv2mars/javelin-${process.platform}-${process.arch}/package.json`));
  } catch {
    return;
  }
  for (const name of ["javelin", "javelin-node"]) {
    const source = path.join(dir, "bin", name);
    const shim = path.join(__dirname, "bin", name);
    const temp = `${shim}.${process.pid}.tmp`;
    try {
      try {
        fs.linkSync(source, temp);
      } catch {
        fs.copyFileSync(source, temp);
      }
      fs.chmodSync(temp, 0o755);
      fs.renameSync(temp, shim);
    } catch {
      fs.rmSync(temp, { force: true });
    }
  }
}

optimize();
