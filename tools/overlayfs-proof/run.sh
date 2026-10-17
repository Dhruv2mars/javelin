#!/usr/bin/env bash
set -euo pipefail

# This is an ephemeral CI fixture, not a caller-tree mounting interface.
[[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_ENVIRONMENT:-} == github-hosted && ${RUNNER_OS:-} == Linux ]] || {
  echo 'Refusing mounts outside a disposable GitHub-hosted Linux job' >&2; exit 2;
}
[[ $(uname -s) == Linux && $(id -u) != 0 && $# == 0 ]] || exit 2
runner_temp=$(realpath -e -- "${RUNNER_TEMP:?}")
[[ $runner_temp =~ ^/[a-zA-Z0-9_./-]+$ && $runner_temp != / ]] || exit 2
proof_root=$(mktemp -d "$runner_temp/javelin-overlay-proof.XXXXXX")
evidence=$(mktemp -d "$runner_temp/javelin-overlay-evidence.XXXXXX")
printf 'OVERLAY_EVIDENCE=%s\n' "$evidence" >> "${GITHUB_ENV:?}"
exec > >(tee "$evidence/run.log") 2>&1
printf '%s\n' "$proof_root" > "$proof_root/owner"
export OVERLAY_ROOT=$proof_root OVERLAY_EVIDENCE=$evidence
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
probe="$script_dir/../workspace-probe/probe.mjs"

guard() {
  [[ $proof_root == "$runner_temp"/javelin-overlay-proof.* && ! -L $proof_root &&
     $(realpath -e -- "$proof_root") == "$proof_root" &&
     $(cat "$proof_root/owner") == "$proof_root" ]] || return 2
}
privileged() { guard || return 2; sudo -n timeout 15s "$@"; }
cleanup() {
  local result=$? failed=0
  trap - EXIT INT TERM
  set +e
  guard || failed=1
  if [[ $failed == 0 ]]; then
    for name in c a b snapshot base; do
      if mountpoint -q "$proof_root/$name"; then
        privileged umount -- "$proof_root/$name" || failed=1
      fi
    done
    findmnt -rn -o TARGET > "$evidence/mounts-after-cleanup.txt"
    if awk -v root="$proof_root" '$0 == root || index($0, root "/") == 1 { found=1 } END { exit !found }' "$evidence/mounts-after-cleanup.txt"; then
      failed=1
    fi
    if [[ $failed == 0 ]]; then
      privileged rm -rf --one-file-system -- "$proof_root" || failed=1
      [[ ! -e $proof_root ]] || failed=1
    fi
  fi
  printf '{"exitCode":%s,"allUnmountedAndOwnedRootRemoved":%s,"root":"%s"}\n' \
    "$result" "$([[ $failed == 0 ]] && echo true || echo false)" "$proof_root" > "$evidence/cleanup.json"
  timeout 15s bun "$script_dir/check.mjs" summary || failed=1
  [[ $failed == 0 ]] || result=1
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir "$proof_root"/{backing,base,snapshot,a,b,c}
mkdir "$proof_root/backing"/{lower,a-upper,a-work,b-upper,b-work,c-upper,c-work,a-next-upper,a-next-work,c-next-upper,c-next-work}
timeout 20s bun "$probe" --seed "$proof_root/backing/lower"
# Hide writable backing aliases from the ordinary runner. Overlay uses the
# privileged mounter's stored credentials; visible paths retain runner ownership.
privileged chown root:root "$proof_root/backing"
privileged chmod 700 "$proof_root/backing"
privileged mount --bind "$proof_root/backing/lower" "$proof_root/base"
privileged mount -o remount,bind,ro "$proof_root/base"
mount_layer() {
  local name=$1 generation=$2 lowers=$3
  privileged mount -t overlay overlay -o \
    "lowerdir=$lowers,upperdir=$proof_root/backing/$generation-upper,workdir=$proof_root/backing/$generation-work,index=off,metacopy=off,redirect_dir=nofollow" \
    "$proof_root/$name"
  findmnt -rn -M "$proof_root/$name" -o TARGET,FSTYPE,OPTIONS >> "$evidence/mounts.txt"
}
inventory() {
  privileged python3 "$script_dir/inventory.py" "$proof_root" > "$evidence/storage-$1.json"
}
check() { timeout 25s bun "$script_dir/check.mjs" "$1"; }
for name in a b c; do mount_layer "$name" "$name" "$proof_root/base"; done
inventory initial
check initial

set +e
timeout 70s bun "$probe" --backend paths --paths "$evidence/paths.json" --out "$evidence/common-probe.json"
probe_exit=$?
set -e
[[ $probe_exit == 1 ]] || { echo "Unexpected common probe exit $probe_exit"; exit 1; }
inventory probe
check probe

# No writers remain. Detach the source before its upper becomes a shared lower.
privileged umount -- "$proof_root/a"
privileged umount -- "$proof_root/c"
privileged mount --bind "$proof_root/backing/a-upper" "$proof_root/snapshot"
privileged mount -o remount,bind,ro "$proof_root/snapshot"
mount_layer a a-next "$proof_root/snapshot:$proof_root/base"
mount_layer c c-next "$proof_root/snapshot:$proof_root/base"
inventory clone
check clone
check before-kill
privileged umount -- "$proof_root/a"
mount_layer a a-next "$proof_root/snapshot:$proof_root/base"
check before-remount
check after-kill
privileged umount -- "$proof_root/a"
mount_layer a a-next "$proof_root/snapshot:$proof_root/base"
check after-remount
inventory final
check final
