#!/usr/bin/env bash
# Clean-machine install and upgrade check for one release platform.
#
# Runs on a fresh runner with only the release files: installs the previous build,
# seeds a repository with it, verifies a corrupted archive is refused, upgrades in
# place to the new build, opens the older repository, runs a smoke scenario on a new
# one, and checks the pinned and latest download paths against a local mirror of the
# release layout. Every javelin call runs with a failing git first on PATH (the shim
# technique from .github/workflows/ci.yml); the shim's marker must stay absent.
#
#   clean-machine-check.sh NEW_DIR PREVIOUS_DIR TAG EXPECTED_VERSION TARGET
#     NEW_DIR        the release archives and SHA256SUMS for all platforms
#     PREVIOUS_DIR   the previous build's archive and SHA256SUMS for TARGET
#     TAG            the release tag, used for the pinned download path
#     EXPECTED_VERSION  exact output of `javelin --version` for the new build
#     TARGET         Rust target triple of this runner
set -euo pipefail

new_dir=$1
prev_dir=$2
tag=$3
expected=$4
target=$5
here=$(cd "$(dirname "$0")" && pwd)

windows=0
exe=javelin
ext=tar.gz
case "$target" in
  *windows*) windows=1; exe=javelin.exe; ext=zip ;;
esac

if [ "$windows" = 1 ]; then
  bin_dir="$(cygpath -u "$LOCALAPPDATA")/Programs/Javelin/bin"
else
  bin_dir="$HOME/.local/bin"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
else
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'clean-machine-check: FAIL: %s\n' "$*" >&2; exit 1; }

# Paths handed to native Windows programs must be mixed-form (C:/...), not /c/...
native() {
  if [ "$windows" = 1 ]; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

archive_for() {
  # Exactly one archive for this target in the directory.
  ls "$1"/javelin-*-"$target"."$ext"
}

install_local() { # ARCHIVE SUMS
  if [ "$windows" = 1 ]; then
    pwsh -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$here/install.ps1")" \
      -Archive "$(cygpath -w "$1")" -Sums "$(cygpath -w "$2")"
  else
    sh "$here/install.sh" --archive "$1" --sums "$2"
  fi
}

install_release() { # VERSION RELEASE_BASE_DIR
  if [ "$windows" = 1 ]; then
    pwsh -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$here/install.ps1")" \
      -Version "$1" -ReleaseBase "$(cygpath -w "$2")"
  else
    sh "$here/install.sh" --version "$1" --release-base "$2"
  fi
}

check_version() {
  local got
  got=$(javelin --version)
  [ "$got" = "$expected" ] || fail "javelin --version printed '$got', expected '$expected'"
  echo "version: $got"
}

seed_repo() { # REPO LABEL: init, workspace create, edit, checkpoint, publish
  local repo=$1 label=$2 r
  r=$(native "$repo")
  mkdir -p "$repo"
  javelin init "$r"
  javelin workspace create work --repo "$r" >/dev/null
  printf 'written by %s\n' "$label" > "$repo/.javelin/layers/work/hello.txt"
  javelin checkpoint work --repo "$r" >/dev/null
  javelin workspace publish work --operation "publish-$label" --actor human --repo "$r" >/dev/null
}

expect_world() { # REPO WORLD: history lists that World and check passes
  local r
  r=$(native "$1")
  javelin history "$r" | grep -q "^World $2 " || fail "$1 does not list World $2"
  javelin check "$r" | grep -q '^Integrity OK' || fail "$1 failed javelin check"
}

work=$(mktemp -d "${TMPDIR:-/tmp}/clean-machine.XXXXXX")
# Sealed Layer bases are read-only; restore write permission so the tree can be removed.
trap 'chmod -R u+rwx "$work" 2>/dev/null || true; rm -rf "$work"' EXIT

step "Git stand-in on PATH (positive control first)"
shim="$work/git-shim"
probe="$work/git-probe"
suffix=
[ "$windows" = 1 ] && suffix=.exe
mkdir -p "$shim" "$probe"
echo 'fn main() { let d = std::env::current_exe().unwrap(); std::fs::write(d.parent().unwrap().join("invoked"), b"").unwrap(); std::process::exit(97) }' > "$probe/shim.rs"
echo 'fn main() { std::process::exit(std::process::Command::new("git").status().ok().and_then(|s| s.code()).unwrap_or(1)) }' > "$probe/probe.rs"
rustc -o "$shim/git$suffix" "$probe/shim.rs"
rustc -o "$probe/probe$suffix" "$probe/probe.rs"
set +e
PATH="$shim:$PATH" "$probe/probe$suffix"
control=$?
set -e
[ "$control" = 97 ] || fail "positive control: the git stand-in was not reached (exit $control)"
rm "$shim/invoked"
export PATH="$shim:$bin_dir:$PATH"
echo "shim active; javelin will resolve from $bin_dir"

step "Clean machine: javelin is not installed yet"
[ ! -e "$bin_dir/$exe" ] || fail "$bin_dir/$exe already exists on a supposedly clean runner"

step "Install the previous build (default location) and seed a repository with it"
install_local "$(archive_for "$prev_dir")" "$prev_dir/SHA256SUMS"
[ -f "$bin_dir/$exe" ] || fail "install did not write $bin_dir/$exe"
seed_repo "$work/previous-repo" previous
expect_world "$work/previous-repo" 1

step "A corrupted archive is refused and leaves the installed binary unchanged"
mkdir "$work/corrupt"
corrupt="$work/corrupt/$(basename "$(archive_for "$new_dir")")"
cp "$(archive_for "$new_dir")" "$corrupt"
printf 'x' >> "$corrupt"
before=$(sha256_of "$bin_dir/$exe")
if install_local "$corrupt" "$new_dir/SHA256SUMS" 2>"$work/corrupt.log"; then
  fail "corrupted archive was installed"
fi
grep -q 'checksum mismatch' "$work/corrupt.log" || fail "refusal did not name a checksum mismatch"
[ "$(sha256_of "$bin_dir/$exe")" = "$before" ] || fail "refused install changed the binary"
echo "refused as expected: $(cat "$work/corrupt.log")"

step "Upgrade in place to the new build"
install_local "$(archive_for "$new_dir")" "$new_dir/SHA256SUMS"
check_version

step "The repository created by the previous build still opens"
expect_world "$work/previous-repo" 1

step "Smoke scenario on a new repository with the new build"
seed_repo "$work/new-repo" new
expect_world "$work/new-repo" 1

step "Pinned and latest downloads from a release-layout mirror"
mirror="$work/mirror"
mkdir -p "$mirror/download/$tag" "$mirror/latest/download"
cp "$new_dir"/* "$mirror/download/$tag/"
cp "$new_dir"/* "$mirror/latest/download/"
install_release "$tag" "$mirror"
check_version
install_release latest "$mirror"
check_version

step "No Git was invoked by any javelin or installer command"
[ ! -e "$shim/invoked" ] || fail "the git stand-in was invoked"
echo "git stand-in marker absent"

echo
echo "clean-machine-check: PASS ($target)"
