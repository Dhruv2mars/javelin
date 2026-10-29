#!/usr/bin/env bash
# The steps of .buildkite/pipeline.yml, run on the self-hosted agents on the Mac (docs/ci.md).
#
#   .buildkite/ci.sh mac | linux | gate2 | wasm | release | proof | forge
#
# mac, release, proof and forge run natively on macOS; linux, gate2 and wasm run in the Linux
# image (.buildkite/linux.Dockerfile) under Colima. Build caches outlive the job: target dirs
# in ~/.buildkite-agent/cache and Docker volumes, so warm runs rebuild only what changed.
set -euo pipefail

export RUSTUP_TOOLCHAIN=1.99.0 CARGO_TERM_COLOR=never RUST_BACKTRACE=1
# Backtraces need file and line, not full debug info: smaller caches, faster links.
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
cache=$HOME/.buildkite-agent/cache
# Keep the Mac from idle-sleeping until this job ends (closing the lid still sleeps it).
if command -v caffeinate > /dev/null; then caffeinate -i -w $$ & fi

# Empties a cargo target dir that has outgrown its budget in GiB; the next run rebuilds it.
# The Mac's disk is small, and branches with different Cargo.lock files accumulate artifacts.
cap() {
  local kb
  kb=$(du -sk "$1" 2> /dev/null | cut -f1) || return 0
  if [ "${kb:-0}" -gt $(($2 * 1024 * 1024)) ]; then
    echo "--- $1 holds more than $2 GiB; cleaning it"
    cargo clean --target-dir "$1"
  fi
}

# A git that records any call and fails comes first on PATH for the tests: Javelin's runtime
# and tests must never invoke Git. The probe proves Rust resolves it before the tests rely on it.
git_shim() {
  shim=$(mktemp -d) && local probe
  probe=$(mktemp -d)
  echo 'fn main() { let d = std::env::current_exe().unwrap(); std::fs::write(d.parent().unwrap().join("invoked"), b"").unwrap(); std::process::exit(97) }' > "$probe/shim.rs"
  echo 'fn main() { std::process::exit(std::process::Command::new("git").status().ok().and_then(|s| s.code()).unwrap_or(1)) }' > "$probe/probe.rs"
  rustc -o "$shim/git" "$probe/shim.rs"
  rustc -o "$probe/probe" "$probe/probe.rs"
  if PATH="$shim:$PATH" "$probe/probe"; then exit 1; else test $? -eq 97; fi
  rm "$shim/invoked"
}

# clippy, then the tests with --no-fail-fast so one run reports every failure. The CLI is built
# first because the hosted acceptance tests drive target/debug/javelin.
clippy_and_test() {
  echo "--- clippy"
  cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
  echo "+++ test"
  git_shim
  cargo build --locked -p javelin-cli
  local status=0
  PATH="$shim:$PATH" cargo test --locked -p javelin --all-targets --all-features --no-fail-fast || status=1
  PATH="$shim:$PATH" cargo test --locked --workspace --exclude javelin --all-targets --all-features --no-fail-fast || status=1
  if [ -e "$shim/invoked" ]; then echo "^^^ +++"; echo "a test invoked git" >&2; status=1; fi
  return $status
}

# Runs this script's STEP inside the Linux image, with the checkout at /w. Docker caches the
# image by the Dockerfile's hash. VOLUME is the step's cargo target dir.
in_linux() {
  local volume=$1 step=$2 tag
  tag=javelin-ci:$(shasum -a 256 .buildkite/linux.Dockerfile | cut -c1-12)
  if ! docker image inspect "$tag" > /dev/null 2>&1; then
    echo "--- building $tag"
    docker build -q --build-arg UID="$(id -u)" -t "$tag" -f .buildkite/linux.Dockerfile .buildkite
  fi
  docker run --rm --init -v "$PWD:/w" -w /w \
    -v javelin-cargo-home:/cargo -e CARGO_HOME=/cargo \
    -v "$volume:/target" -e CARGO_TARGET_DIR=/target \
    "$tag" .buildkite/ci.sh "$step"
}

step=${1:?usage: ci.sh mac|linux|gate2|wasm|release|proof|forge}
case $step in
  mac)
    export CARGO_TARGET_DIR=$cache/mac-target
    cap "$CARGO_TARGET_DIR" 12
    echo "--- fmt"
    cargo fmt --all -- --check
    clippy_and_test
    ;;

  linux) in_linux javelin-linux-target linux-inside ;;
  linux-inside)
    cap /target 12
    clippy_and_test
    ;;

  # Gate 2's crash clause (tools/workspace-probe/README.md). /tmp is the container's overlay
  # filesystem, which takes the copy fallback.
  gate2) in_linux javelin-linux-release gate2-inside ;;
  gate2-inside)
    cap /target 6
    cargo build --locked --release -p javelin-cli
    cargo build --locked --release -p javelin --example gate2_seed
    bun tools/workspace-probe/gate2.mjs --javelin /target/release/javelin --seed /target/release/examples/gate2_seed \
      --dir /tmp/gate2 --fs overlay --expect copy --out /tmp/gate2.json --only crash
    ;;

  # The Cloudflare node's wasm module, SQLite included (clang and llvm-ar build it). The
  # Worker glue (wasm-bindgen) and wrangler run where the node is deployed, not here.
  wasm) in_linux javelin-linux-release wasm-inside ;;
  wasm-inside)
    AR_wasm32_unknown_unknown=llvm-ar cargo build --locked --release --target wasm32-unknown-unknown -p javelin-node-cloudflare
    ;;

  # The release script's Linux targets (tools/release, docs/releases.md), cross-compiled with
  # cargo-zigbuild against glibc 2.28 on macOS, as the release machine does. release.ts builds
  # in ./target/release-dist; ./target points at a cache that outlives the checkout.
  release)
    mkdir -p "$cache/release-target"
    cap "$cache/release-target" 8
    rm -f target && ln -s "$cache/release-target" target
    bun tools/release/release.ts build --version 0.1.0-ci --targets x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu
    tar -cf release-linux.tar dist/npm
    ;;

  # A new developer on Linux, three ways, each from a loopback npm registry serving those
  # packages (`release.ts serve`): `npm i -g` in node:24-slim, `bun add -g` in oven/bun:1-slim
  # (no Node), and the curl installer in ubuntu:24.04. Each runs tools/release/proof.sh as a
  # non-root user with a fresh HOME and a git stand-in. Colima runs linux/amd64 under Rosetta.
  proof)
    buildkite-agent artifact download release-linux.tar . && tar -xf release-linux.tar
    for platform in linux/amd64 linux/arm64; do
      docker run --rm --platform "$platform" alpine:3 true 2> /dev/null ||
        docker run --privileged --rm tonistiigi/binfmt:qemu-v10.2.3 --install "${platform#linux/}"
    done
    docker rm -f javelin-registry > /dev/null 2>&1 || true
    docker run -d --name javelin-registry --network host -v "$PWD:/w:ro" -w /w oven/bun:1-slim bun tools/release/release.ts serve
    trap 'docker rm -f javelin-registry > /dev/null' EXIT
    for _ in $(seq 30); do docker logs javelin-registry 2>&1 | grep -q "npm registry" && break; sleep 1; done
    docker logs javelin-registry
    proof='sh /w/tools/release/proof.sh'
    for platform in linux/amd64 linux/arm64; do
      echo "--- npm i -g on node:24-slim $platform"
      docker run --rm --network host --platform "$platform" --user node -v "$PWD:/w:ro" node:24-slim \
        $proof npm http://127.0.0.1:4873 0.1.0
      echo "--- bun add -g on oven/bun:1-slim $platform"
      docker run --rm --network host --platform "$platform" --user bun -v "$PWD:/w:ro" oven/bun:1-slim \
        $proof bun http://127.0.0.1:4873 0.1.0
      echo "--- curl installer on ubuntu:24.04 $platform"
      docker run --rm --network host --platform "$platform" -v "$PWD:/w:ro" ubuntu:24.04 sh -c "
        apt-get update -qq && apt-get install -y -qq curl ca-certificates > /dev/null &&
        su ubuntu -c '$proof curl http://127.0.0.1:4873 0.1.0'"
    done
    ;;

  # The forge's read-only pages: the TypeScript codec and page data against a real
  # `javelin-node serve` on loopback, seeded with the `javelin` CLI.
  forge)
    export CARGO_TARGET_DIR=$cache/mac-target
    git_shim
    cargo build --locked -p javelin-cli -p javelin-node
    cd apps/forge
    bun install --frozen-lockfile
    bun run typecheck
    bun run build
    PATH="$shim:$PATH" JAVELIN_BIN_DIR="$CARGO_TARGET_DIR/debug" bun test
    test ! -e "$shim/invoked"
    ;;

  *) echo "unknown step: $step" >&2; exit 2 ;;
esac
