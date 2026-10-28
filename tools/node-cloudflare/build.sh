#!/bin/sh
# Builds the node core for the Worker: the wasm module and its wasm-bindgen glue in build/.
# Needs the wasm32-unknown-unknown target, clang with the WebAssembly backend and llvm-ar
# (SQLite is C; macOS's own ar writes an empty archive for wasm objects), and wasm-bindgen-cli
# at the version Cargo.lock pins for wasm-bindgen.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
if [ -z "${AR_wasm32_unknown_unknown:-}" ]; then
  ar=$(command -v llvm-ar || ls /opt/homebrew/opt/llvm*/bin/llvm-ar 2>/dev/null | tail -1 || true)
  [ -n "$ar" ] || { echo "llvm-ar not found; set AR_wasm32_unknown_unknown" >&2; exit 1; }
  export AR_wasm32_unknown_unknown="$ar"
fi
cargo build --locked --release --target wasm32-unknown-unknown -p javelin-node-cloudflare \
  --manifest-path "$root/Cargo.toml"
wasm-bindgen --target web --experimental-reset-state-function --out-dir "$here/build" \
  "${CARGO_TARGET_DIR:-$root/target}/wasm32-unknown-unknown/release/javelin_node_cloudflare.wasm"
# SQLite must be inside the module: an unresolved C symbol would become an `env` import.
bun -e '
const m = new WebAssembly.Module(await Bun.file(process.argv[1]).arrayBuffer());
const env = WebAssembly.Module.imports(m).filter((i) => i.module === "env");
if (env.length) { console.error("unresolved:", env.map((i) => i.name).join(" ")); process.exit(1); }
' "$here/build/javelin_node_cloudflare_bg.wasm"
