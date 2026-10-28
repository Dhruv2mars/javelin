#!/bin/sh
# Installs Javelin the way a new developer would, from an npm registry, then uses it.
#
#   proof.sh METHOD REGISTRY EXPECTED_VERSION [SITE]
#     METHOD     npm (npm i -g), bun (bun add -g) or curl (curl -fsSL SITE/install.sh | sh)
#     REGISTRY   the npm registry, e.g. http://127.0.0.1:4873 from `release.ts serve`
#     SITE       where install.sh is served (default: REGISTRY, which `serve` also does)
#
# Runs with a fresh HOME and a PATH of system directories plus the one tool METHOD needs,
# so nothing else from the machine is found. A git stand-in that records its use comes
# first on PATH; it must never run. Steps: the install, `javelin --version`,
# `javelin-node --version`, `javelin init` and a first publish in a new project, and a
# reinstall over the installed commands. The curl method also checks that a tampered
# tarball is refused without touching the installed binaries.
set -eu

usage="usage: proof.sh npm|bun|curl REGISTRY EXPECTED_VERSION [SITE]"
method=${1:?$usage}
registry=${2:?$usage}
expected=${3:?$usage}
site=${4:-$registry}
package=@dhruv2mars/javelin

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'proof: FAIL: %s\n' "$*" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
else
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

case $method in
  npm) tool=$(command -v npm) && node=$(command -v node) || fail "npm and node are needed" ;;
  bun) tool=$(command -v bun) || fail "bun is needed"; node= ;;
  curl) tool=$(command -v curl) || fail "curl is needed"; node= ;;
  *) fail "$usage" ;;
esac

work=$(mktemp -d "${TMPDIR:-/tmp}/javelin-proof.XXXXXX")
# Sealed Layer bases are read-only; restore write permission so the tree can be removed.
trap 'chmod -R u+rwx "$work" 2>/dev/null || true; rm -rf "$work"' EXIT

export HOME="$work/home"
mkdir -p "$HOME"
shim="$work/git-shim"
mkdir -p "$shim"
printf '#!/bin/sh\n: > "%s/invoked"\nexit 97\n' "$shim" > "$shim/git"
chmod 755 "$shim/git"
tools=$(dirname "$tool")${node:+:$(dirname "$node")}
export PATH="$shim:$tools:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
unset JAVELIN_NPM_REGISTRY JAVELIN_VERSION JAVELIN_INSTALL_DIR NPM_CONFIG_PREFIX BUN_INSTALL || true

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

install() {
  case $method in
    npm) NPM_CONFIG_PREFIX="$HOME/.npm-global" NPM_CONFIG_UPDATE_NOTIFIER=false NPM_CONFIG_FUND=false \
           NPM_CONFIG_AUDIT=false npm install --global "$package" --registry "$registry" ;;
    bun) bun add --global "$package" --registry "$registry" ;;
    curl) curl -fsSL "$site/install.sh" | JAVELIN_NPM_REGISTRY=$registry sh ;;
  esac
}
case $method in
  npm) bin="$HOME/.npm-global/bin"; shown="npm install --global $package" ;;
  bun) bin="$HOME/.bun/bin"; shown="bun add --global $package" ;;
  curl) bin="$HOME/.javelin/bin"; shown="curl -fsSL $site/install.sh | sh" ;;
esac

step "$shown"
install 2>&1 | tee "$work/install.log"
[ -x "$bin/javelin" ] && [ -x "$bin/javelin-node" ] || fail "the install did not put javelin and javelin-node in $bin (output above)"
if [ "$method" = curl ]; then
  grep -q "is not on your PATH" "$work/install.log" || fail "installer did not say how to put javelin on PATH"
fi
export PATH="$bin:$PATH"

step "javelin --version and javelin-node --version, from PATH"
got=$(javelin --version)
[ "$got" = "javelin $expected" ] || fail "javelin --version printed '$got', expected 'javelin $expected'"
echo "$got  ($(command -v javelin))"
got=$(javelin-node --version)
[ "$got" = "javelin-node $expected" ] || fail "javelin-node --version printed '$got'"
echo "$got"

step "javelin init and a first publish in a new project"
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

step "Installing again keeps the commands working; the project still opens"
install >/dev/null 2>&1 || fail "the second install failed"
[ "$(javelin --version)" = "javelin $expected" ] || fail "reinstalled javelin has the wrong version"
javelin check "$project" | grep -q '^Integrity OK' || fail "project does not open after reinstall"
echo "reinstalled; $project still checks"

if [ "$method" = curl ]; then
  step "A tampered tarball is refused and the installed binaries stay as they were"
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) platform=darwin-arm64 ;;
    Darwin-x86_64) platform=darwin-x64 ;;
    Linux-x86_64) platform=linux-x64 ;;
    *) platform=linux-arm64 ;;
  esac
  name=$package-$platform
  mirror="$work/mirror"
  mkdir -p "$mirror/$name/-"
  curl -fsSL -o "$mirror/$name/latest" "$registry/$name/latest"
  version=$(tr -d '\n' < "$mirror/$name/latest" | grep -o '"version" *: *"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/')
  tarball=javelin-$platform-$version.tgz
  curl -fsSL -o "$mirror/$name/-/$tarball" "$registry/$name/-/$tarball"
  printf 'x' >> "$mirror/$name/-/$tarball"
  before=$(sha256_of "$bin/javelin")
  if curl -fsSL "$site/install.sh" | sh -s -- --registry "file://$mirror" 2>"$work/tamper.log"; then
    fail "a tampered tarball was installed"
  fi
  grep -q 'integrity mismatch' "$work/tamper.log" || fail "refusal did not name an integrity mismatch: $(cat "$work/tamper.log")"
  [ "$(sha256_of "$bin/javelin")" = "$before" ] || fail "the refused install changed javelin"
  cat "$work/tamper.log"
fi

step "No Git was run by the install or by javelin"
[ ! -e "$shim/invoked" ] || fail "the git stand-in was run"
echo "git stand-in marker absent"

printf '\nproof: PASS (%s from %s, %s %s)\n' "$method" "$registry" "$(uname -s)" "$(uname -m)"
