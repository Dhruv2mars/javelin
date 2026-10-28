#!/bin/sh
# Install javelin and javelin-node on macOS or Linux.
#
#   curl -fsSL https://javelin-codes.vercel.app/install.sh | sh
#   curl -fsSL https://javelin-codes.vercel.app/install.sh | sh -s -- --version v0.1.0
#
# The installer reads the release host's `latest` file (or uses --version), downloads that
# version's SHA256SUMS and the archive for this platform, and checks the archive's SHA-256
# before it extracts or writes anything. Each binary is copied beside its destination and
# renamed into place, so running it again upgrades in place and never leaves a partial
# file. It changes nothing else: no PATH or shell profile edits, no sudo.
#
# Release layout (docs/releases.md): BASE/latest holds a version such as v0.1.0, and
# BASE/VERSION/ holds SHA256SUMS, manifest.json and javelin-VERSION-TARGET.tar.gz.
set -eu

# The release host. tools/release/release.ts stamps it when it stages a release;
# JAVELIN_RELEASE_BASE or --release-base overrides it.
DEFAULT_RELEASE_BASE=

RELEASE_BASE=${JAVELIN_RELEASE_BASE:-$DEFAULT_RELEASE_BASE}
VERSION=${JAVELIN_VERSION:-latest}
INSTALL_DIR=${JAVELIN_INSTALL_DIR:-$HOME/.javelin/bin}
ARCHIVE=
SUMS=

usage() {
  cat <<'EOF'
Usage: install.sh [--version VERSION] [--install-dir DIR] [--release-base URL-OR-DIR]
       install.sh --archive FILE --sums FILE [--install-dir DIR]

  --version VERSION      Version to install, such as v0.1.0 (default: latest)
  --install-dir DIR      Where javelin and javelin-node go (default: ~/.javelin/bin)
  --release-base BASE    Release host URL, or a local directory with the same layout
  --archive FILE         Install this local archive instead of downloading
  --sums FILE            SHA256SUMS file that lists the archive (with --archive)

The same settings can come from JAVELIN_VERSION, JAVELIN_INSTALL_DIR and
JAVELIN_RELEASE_BASE.
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
    --release-base) [ $# -ge 2 ] || die "--release-base needs a value"; RELEASE_BASE=$2; shift 2 ;;
    --archive) [ $# -ge 2 ] || die "--archive needs a value"; ARCHIVE=$2; shift 2 ;;
    --sums) [ $# -ge 2 ] || die "--sums needs a value"; SUMS=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

if { [ -n "$ARCHIVE" ] && [ -z "$SUMS" ]; } || { [ -z "$ARCHIVE" ] && [ -n "$SUMS" ]; }; then
  die "--archive and --sums must be given together"
fi

os=$(uname -s)
arch=$(uname -m)
# A shell under Rosetta reports x86_64 on Apple silicon; install the native build there.
if [ "$os" = Darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=arm64
fi
case "$os-$arch" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64|Linux-amd64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) die "no Javelin build for $os $arch (macOS and Linux on x86_64 or arm64 only)" ;;
esac
if [ "$os" = Linux ] && ldd --version 2>&1 | grep -qi musl; then
  die "the Linux builds need glibc 2.28 or newer; this system uses musl"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  die "need sha256sum or shasum to check the download"
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/javelin-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT INT TERM

# Copy BASE/$1 to $TMP/$2. Plain http is accepted only for a loopback host.
fetch() {
  source=${RELEASE_BASE%/}/$1
  case "$RELEASE_BASE" in
    https://*|file://*|http://127.0.0.1:*|http://127.0.0.1/*|http://localhost:*|http://localhost/*)
      curl -fsSL --proto '=https,http,file' -o "$TMP/$2" "$source" || die "could not download $source" ;;
    http://*) die "refusing plain http for $RELEASE_BASE; use https" ;;
    *) cp "$source" "$TMP/$2" 2>/dev/null || die "could not read $source" ;;
  esac
}

if [ -n "$ARCHIVE" ]; then
  [ -f "$ARCHIVE" ] || die "no such archive: $ARCHIVE"
  [ -f "$SUMS" ] || die "no such checksum file: $SUMS"
  name=$(basename "$ARCHIVE")
  cp "$ARCHIVE" "$TMP/$name"
  cp "$SUMS" "$TMP/SHA256SUMS"
else
  [ -n "$RELEASE_BASE" ] || die "no release host is set yet; pass --release-base or set JAVELIN_RELEASE_BASE"
  if [ "$VERSION" = latest ]; then
    fetch latest latest
    VERSION=$(tr -d ' \r\n' < "$TMP/latest")
  fi
  # A version names a directory on the host: v, then letters, digits, dots, dashes, pluses.
  printf '%s\n' "$VERSION" | grep -Eq '^v[0-9A-Za-z.+-]+$' || die "not a release version: $VERSION"
  name=javelin-$VERSION-$TARGET.tar.gz
  fetch "$VERSION/SHA256SUMS" SHA256SUMS
  fetch "$VERSION/$name" "$name"
fi

expected=$(awk -v name="$name" '$2 == name || $2 == "*" name {print $1}' "$TMP/SHA256SUMS")
[ "$(printf '%s\n' "$expected" | grep -c .)" -eq 1 ] || die "SHA256SUMS has no single entry for $name"
actual=$(sha256_of "$TMP/$name")
if [ "$expected" != "$actual" ]; then
  die "checksum mismatch for $name (expected $expected, got $actual); nothing was installed"
fi

mkdir "$TMP/extract"
tar -xzf "$TMP/$name" -C "$TMP/extract"
for bin in javelin javelin-node; do
  found=$(find "$TMP/extract" -mindepth 3 -maxdepth 3 -type f -path "*/bin/$bin" | head -n 1)
  [ -n "$found" ] || die "archive $name has no bin/$bin"
done

mkdir -p "$INSTALL_DIR"
for bin in javelin javelin-node; do
  found=$(find "$TMP/extract" -mindepth 3 -maxdepth 3 -type f -path "*/bin/$bin" | head -n 1)
  staged=$INSTALL_DIR/.$bin.$$.new
  cp "$found" "$staged"
  chmod 755 "$staged"
  mv -f "$staged" "$INSTALL_DIR/$bin"
done

printf 'Installed javelin and javelin-node (%s) to %s\n' "$name" "$INSTALL_DIR"
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
