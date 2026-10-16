#!/usr/bin/env bash
set -euo pipefail

# Disposable GitHub-hosted Linux only. Root operations, mounts and helper
# installation never run on a workstation or self-hosted runner.
[[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_ENVIRONMENT:-} == github-hosted && ${RUNNER_OS:-} == Linux ]] || {
  echo 'Refusing mounts outside a disposable GitHub-hosted Linux job' >&2; exit 2;
}
[[ $(uname -s) == Linux && $(id -u) != 0 && $# == 0 ]] || exit 2
[[ ${SOURCE_SHA:-} =~ ^[0-9a-f]{40}$ && ${PROBE_SHA:-} =~ ^[0-9a-f]{40}$ ]] || exit 2
probe=$(realpath -e -- "${JAVELIN_COMMON_PROBE:?}")
probe_dir=$(dirname -- "$probe")
# Unchanged common probe from the verified Linux proof dependency.
[[ $(sha256sum < "$probe" | cut -d' ' -f1) == 617f009e1686dddfd5bacd4de007731d71e309d5a4075365235124ae7dfa0b49 ]] || exit 2
[[ $(sha256sum < "$probe_dir/atomic-writer.mjs" | cut -d' ' -f1) == 14d6eb4e35bc8aef0914c1e79004c6c6b41a53dc774127007603b42a1b77ec22 ]] || exit 2

parent=/var/lib/javelin-layer-ci
libexec=/usr/libexec/javelin-ci
runner_temp=$(realpath -e -- "${RUNNER_TEMP:?}")
evidence=$(mktemp -d "$runner_temp/javelin-layer-evidence.XXXXXX")
printf 'LAYER_EVIDENCE=%s\n' "$evidence" >> "${GITHUB_ENV:?}"
exec > >(tee "$evidence/run.log") 2>&1
[[ ! -e $parent ]] || { echo "refusing existing $parent"; exit 2; }
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "$script_dir/../.." && pwd -P)

under_parent() { awk -v p="$parent/" 'index($0, p) == 1'; }
cleanup() {
  local result=$? failed=0
  trap - EXIT INT TERM
  set +e
  if [[ -e $parent ]]; then
    findmnt -rn -o TARGET | under_parent | sort -r > "$evidence/mounts-before-cleanup.txt"
    while read -r target; do
      sudo -n timeout 15s umount -- "$target" || failed=1
    done < "$evidence/mounts-before-cleanup.txt"
    findmnt -rn -o TARGET > "$evidence/mounts-after-cleanup.txt"
    [[ -z $(under_parent < "$evidence/mounts-after-cleanup.txt") ]] || failed=1
    if [[ $failed == 0 ]]; then
      sudo -n timeout 30s rm -rf --one-file-system -- "$parent" || failed=1
    fi
    [[ ! -e $parent ]] || failed=1
  fi
  printf '{"exitCode":%s,"allUnmountedAndRemoved":%s}\n' "$result" \
    "$([[ $failed == 0 ]] && echo true || echo false)" > "$evidence/cleanup.json"
  timeout 15s python3 -I "$script_dir/summary.py" "$evidence" || failed=1
  [[ $failed == 0 ]] || result=1
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cd "$repo_dir"
cargo build --locked --release --bin javelin-linux-layer-helper
sudo -n install -d -o root -g root -m 0755 "$parent" "$libexec" /etc/javelin
printf '%s\n' "$parent" | sudo -n tee /etc/javelin/linux-layer-parents > /dev/null
sudo -n chmod 0644 /etc/javelin/linux-layer-parents
sudo -n install -o root -g root -m 0755 target/release/javelin-linux-layer-helper "$libexec/"
sudo -n install -o root -g root -m 0644 "$script_dir/inventory.py" "$libexec/"
stat -c '%n %U:%G %a' "$parent" "$libexec" "$libexec"/* /etc/javelin/linux-layer-parents /usr/bin/sudo > "$evidence/installed.txt"

python3 -I - "$evidence" <<'PY'
import hashlib, json, os, platform, subprocess, sys
from pathlib import Path
def sha(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()
sources = sorted(str(p) for pattern in ["src/**/*.rs", "tests/support/linux_layer_mounts.rs",
    "tools/linux-layer-runtime/*.*", "Cargo.toml", "Cargo.lock", ".github/workflows/linux-layer-runtime.yml"]
    for p in Path(".").glob(pattern))
probe = Path(os.environ["JAVELIN_COMMON_PROBE"])
json.dump({
    "sourceSHA": os.environ["SOURCE_SHA"], "probeDependencySHA": os.environ["PROBE_SHA"],
    "sources": {p: sha(p) for p in sources},
    "probeSources": {n: sha(probe.parent / n) for n in ["probe.mjs", "atomic-writer.mjs"]},
    "installedHelperSHA256": sha("/usr/libexec/javelin-ci/javelin-linux-layer-helper"),
    "kernel": platform.release(), "machine": platform.machine(), "uid": os.getuid(),
    "runnerImage": os.environ.get("ImageVersion"), "runId": os.environ.get("GITHUB_RUN_ID"),
    "runAttempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
    "rustc": subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip(),
    "bun": subprocess.run(["bun", "--version"], capture_output=True, text=True).stdout.strip(),
}, open(Path(sys.argv[1]) / "initial.json", "w"), indent=1)
PY

export JAVELIN_LINUX_LAYER_MOUNT_TESTS=1
export JAVELIN_LINUX_LAYER_HELPER=$libexec/javelin-linux-layer-helper
export JAVELIN_LINUX_LAYER_PARENT=$parent
export JAVELIN_LINUX_LAYER_EVIDENCE=$evidence
export JAVELIN_LINUX_LAYER_INVENTORY="sudo -n timeout 15s python3 -I $libexec/inventory.py"
export JAVELIN_COMMON_PROBE=$probe
JAVELIN_BUN=$(command -v bun)
export JAVELIN_BUN
timeout --signal=TERM --kill-after=30s 420s cargo test --locked --lib linux_layer::mount_tests \
  -- --ignored --test-threads=1 --nocapture 2>&1 | tee "$evidence/mount-tests.log"
