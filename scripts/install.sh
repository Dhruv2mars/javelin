#!/bin/sh
# Install javelin and javelin-node on macOS or Linux from the npm registry.
#
#   curl -fsSL https://javelin-codes.vercel.app/install.sh | sh
#   curl -fsSL https://javelin-codes.vercel.app/install.sh | sh -s -- --version 0.1.0
#
# The binaries are the ones `npm i -g @dhruv2mars/javelin` installs: the installer reads
# this platform's package, @dhruv2mars/javelin-PLATFORM, from the registry, downloads its
# tarball, and checks it against the registry's `dist.integrity` (SHA-512) before it
# extracts or writes anything. Each binary is copied beside its destination and renamed
# into place, so running it again upgrades in place and never leaves a partial file. It
# changes nothing else: no PATH or shell profile edits, no sudo, no Node.
set -eu

REGISTRY=${JAVELIN_NPM_REGISTRY:-https://registry.npmjs.org}
VERSION=${JAVELIN_VERSION:-latest}
INSTALL_DIR=${JAVELIN_INSTALL_DIR:-$HOME/.javelin/bin}

usage() {
  cat <<'EOF'
Usage: install.sh [--version VERSION] [--install-dir DIR] [--registry URL]

  --version VERSION   npm version or dist-tag to install (default: latest)
  --install-dir DIR   Where javelin and javelin-node go (default: ~/.javelin/bin)
  --registry URL      npm registry (default: https://registry.npmjs.org)

The same settings can come from JAVELIN_VERSION, JAVELIN_INSTALL_DIR and
JAVELIN_NPM_REGISTRY.
EOF
}

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=$2; shift 2 ;;
    --install-dir) [ $# -ge 2 ] || die "--install-dir needs a value"; INSTALL_DIR=$2; shift 2 ;;
    --registry) [ $# -ge 2 ] || die "--registry needs a value"; REGISTRY=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done
REGISTRY=${REGISTRY%/}

os=$(uname -s)
arch=$(uname -m)
# A shell under Rosetta reports x86_64 on Apple silicon; install the native build there.
if [ "$os" = Darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=arm64
fi
case "$os-$arch" in
  Darwin-arm64) PLATFORM=darwin-arm64 ;;
  Darwin-x86_64) PLATFORM=darwin-x64 ;;
  Linux-x86_64|Linux-amd64) PLATFORM=linux-x64 ;;
  Linux-aarch64|Linux-arm64) PLATFORM=linux-arm64 ;;
  *) die "no Javelin build for $os $arch (macOS and Linux on x86_64 or arm64; on Windows use install.ps1)" ;;
esac
if [ "$os" = Linux ] && ldd --version 2>&1 | grep -qi musl; then
  die "the Linux builds need glibc 2.28 or newer; this system uses musl"
fi
PACKAGE=@dhruv2mars/javelin-$PLATFORM

case "$REGISTRY" in
  https://*|file://*|http://127.0.0.1:*|http://127.0.0.1/*|http://localhost:*|http://localhost/*) ;;
  http://*) die "refusing plain http for $REGISTRY; use https" ;;
  *) die "not a registry URL: $REGISTRY" ;;
esac
# A version or dist-tag goes into a URL: letters, digits, dots, dashes, pluses.
printf '%s\n' "$VERSION" | grep -Eq '^[0-9A-Za-z.+-]+$' || die "not a version or tag: $VERSION"

if command -v sha512sum >/dev/null 2>&1; then
  sha512_hex() { sha512sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
  sha512_hex() { shasum -a 512 "$1" | awk '{print $1}'; }
else
  die "need sha512sum or shasum to check the download"
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/javelin-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT INT TERM

fetch() { # URL FILE
  curl -fsSL --proto '=https,http,file' -o "$2" "$1" || die "could not download $1"
}

# The registry's document for one version: {"name":...,"version":"...","dist":{"integrity":...}}.
fetch "$REGISTRY/$PACKAGE/$VERSION" "$TMP/manifest.json"
field() { # FILE NAME: a string field that occurs exactly once
  value=$(tr -d '\n' < "$1" | grep -o "\"$2\" *: *\"[^\"]*\"" | sed 's/.*: *"\(.*\)"/\1/')
  [ "$(printf '%s\n' "$value" | grep -c .)" -eq 1 ] || die "$PACKAGE@$VERSION: no single \"$2\" in $(basename "$1")"
  printf '%s' "$value"
}
version=$(field "$TMP/manifest.json" version)
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || die "not a release version: $version"
integrity=$(field "$TMP/manifest.json" integrity)
case "$integrity" in
  sha512-*) ;;
  *) die "$PACKAGE@$version has no SHA-512 integrity" ;;
esac

name=javelin-$PLATFORM-$version.tgz
fetch "$REGISTRY/$PACKAGE/-/$name" "$TMP/$name"
expected=$(printf '%s' "${integrity#sha512-}" | base64 -d 2>/dev/null | od -An -v -tx1 | tr -d ' \n')
actual=$(sha512_hex "$TMP/$name")
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  die "integrity mismatch for $PACKAGE@$version (registry $integrity); nothing was installed"
fi

mkdir "$TMP/extract"
tar -xzf "$TMP/$name" -C "$TMP/extract"
[ "$(field "$TMP/extract/package/package.json" name)" = "$PACKAGE" ] || die "the tarball is not $PACKAGE"
for bin in javelin javelin-node; do
  [ -f "$TMP/extract/package/bin/$bin" ] || die "$PACKAGE@$version has no bin/$bin"
done

mkdir -p "$INSTALL_DIR"
for bin in javelin javelin-node; do
  staged=$INSTALL_DIR/.$bin.$$.new
  cp "$TMP/extract/package/bin/$bin" "$staged"
  chmod 755 "$staged"
  mv -f "$staged" "$INSTALL_DIR/$bin"
done

printf 'Installed javelin and javelin-node %s (%s) to %s\n' "$version" "$PACKAGE" "$INSTALL_DIR"
case ":$PATH:" in
  *":$INSTALL_DIR:"*)
    printf 'Run: javelin --version\n' ;;
  *)
    case "$(basename "${SHELL:-sh}")" in
      zsh) profile='~/.zshrc' ;;
      bash) if [ "$os" = Darwin ]; then profile='~/.bash_profile'; else profile='~/.bashrc'; fi ;;
      fish) profile= ;;
      *) profile='~/.profile' ;;
    esac
    # Written as $HOME/... when it is under HOME, so the line reads the same for everyone.
    case "$INSTALL_DIR" in
      "$HOME"/*) shown="\$HOME${INSTALL_DIR#"$HOME"}" ;;
      *) shown=$INSTALL_DIR ;;
    esac
    printf '\n%s is not on your PATH. To add it:\n' "$INSTALL_DIR"
    if [ -n "$profile" ]; then
      printf "  echo 'export PATH=\"%s:\$PATH\"' >> %s\n" "$shown" "$profile"
    else
      printf '  fish_add_path %s\n' "$shown"
    fi
    printf 'then open a new terminal and run: javelin --version\n' ;;
esac
