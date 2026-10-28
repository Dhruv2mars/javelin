#!/bin/sh
# Installs a staged or published release the way a new developer would, then uses it.
#
#   proof.sh SITE EXPECTED_VERSION [RELEASE_BASE]
#     SITE           where install.sh is served, e.g. http://127.0.0.1:4873
#     RELEASE_BASE   the release host, when it is not SITE (default: SITE)
#
# Runs with a fresh HOME and a PATH of system directories only, so nothing from the build
# machine is found. A git stand-in that records its use comes first on PATH; it must never
# run. Steps: the one-line install, `javelin --version`, `javelin-node --version`,
# `javelin init` and a first publish in a new project, a reinstall over the installed
# binaries, and a tampered archive that must be refused without touching them.
set -eu

base=${1:?usage: proof.sh SITE EXPECTED_VERSION [RELEASE_BASE]}
expected=${2:?usage: proof.sh SITE EXPECTED_VERSION [RELEASE_BASE]}
release=${3:-$base}

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'proof: FAIL: %s\n' "$*" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
else
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/javelin-proof.XXXXXX")
# Sealed Layer bases are read-only; restore write permission so the tree can be removed.
trap 'chmod -R u+rwx "$work" 2>/dev/null || true; rm -rf "$work"' EXIT

export HOME="$work/home"
mkdir -p "$HOME"
shim="$work/git-shim"
mkdir -p "$shim"
printf '#!/bin/sh\n: > "%s/invoked"\nexit 97\n' "$shim" > "$shim/git"
chmod 755 "$shim/git"
export PATH="$shim:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
unset JAVELIN_RELEASE_BASE JAVELIN_VERSION JAVELIN_INSTALL_DIR || true

step "Git stand-in on PATH (positive control first)"
set +e
sh -c 'git status' >/dev/null 2>&1
control=$?
set -e
[ "$control" = 97 ] && [ -e "$shim/invoked" ] || fail "positive control: the git stand-in was not reached (exit $control)"
rm "$shim/invoked"
echo "HOME=$HOME"
command -v javelin >/dev/null 2>&1 && fail "javelin is already on PATH: $(command -v javelin)"
echo "javelin is not installed"

step "curl -fsSL $base/install.sh | sh"
curl -fsSL "$base/install.sh" | sh | tee "$work/install.log"
grep -q "^Installed javelin and javelin-node" "$work/install.log" || fail "the one-line install did not finish (output above)"
grep -q "is not on your PATH" "$work/install.log" || fail "installer did not say how to put javelin on PATH"
bin="$HOME/.javelin/bin"

step "Installed binaries answer --version"
got=$("$bin/javelin" --version)
[ "$got" = "javelin $expected" ] || fail "javelin --version printed '$got', expected 'javelin $expected'"
echo "$got"
got=$("$bin/javelin-node" --version)
[ "$got" = "javelin-node $expected" ] || fail "javelin-node --version printed '$got'"
echo "$got"

step "javelin init and a first publish in a new project"
export PATH="$bin:$PATH"
project="$HOME/hello"
mkdir "$project"
cd "$project"
javelin init
javelin workspace create work >/dev/null
printf 'hello from a fresh install\n' > .javelin/layers/work/hello.txt
javelin checkpoint work >/dev/null
javelin workspace publish work --operation first-publish --actor human >/dev/null
javelin history | tee "$work/history.log"
grep -q '^World 1 ' "$work/history.log" || fail "history does not list World 1"
javelin check | grep '^Integrity OK' || fail "javelin check failed"
cd "$work"

step "Installing again replaces the binaries in place; the project still opens"
curl -fsSL "$base/install.sh" | sh >/dev/null
[ "$(javelin --version)" = "javelin $expected" ] || fail "reinstalled javelin has the wrong version"
javelin check "$project" | grep -q '^Integrity OK' || fail "project does not open after reinstall"
echo "reinstalled; $project still checks"

step "A tampered archive is refused and the installed binaries stay as they were"
version=$(curl -fsSL "$release/latest" | tr -d ' \r\n')
mirror="$work/mirror"
mkdir -p "$mirror/$version"
printf '%s\n' "$version" > "$mirror/latest"
curl -fsSL -o "$mirror/$version/SHA256SUMS" "$release/$version/SHA256SUMS"
for f in $(awk '{print $2}' "$mirror/$version/SHA256SUMS" | grep '\.tar\.gz$'); do
  curl -fsSL -o "$mirror/$version/$f" "$release/$version/$f"
  printf 'x' >> "$mirror/$version/$f"
done
before=$(sha256_of "$bin/javelin")
if curl -fsSL "$base/install.sh" | sh -s -- --release-base "$mirror" 2>"$work/tamper.log"; then
  fail "a tampered archive was installed"
fi
grep -q 'checksum mismatch' "$work/tamper.log" || fail "refusal did not name a checksum mismatch: $(cat "$work/tamper.log")"
[ "$(sha256_of "$bin/javelin")" = "$before" ] || fail "the refused install changed javelin"
cat "$work/tamper.log"

step "No Git was run by the installer or by javelin"
[ ! -e "$shim/invoked" ] || fail "the git stand-in was run"
echo "git stand-in marker absent"

printf '\nproof: PASS (%s, %s %s)\n' "$base" "$(uname -s)" "$(uname -m)"
