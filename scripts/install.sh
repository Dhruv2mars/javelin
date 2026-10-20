#!/bin/sh
# Install the Javelin CLI from a release archive on macOS or Linux.
#
# The archive's SHA-256 is checked against SHA256SUMS before anything is extracted or
# written. The binary lands in --bin-dir through a temporary file and an atomic rename,
# so re-running with a newer release upgrades in place and never leaves a partial file.
# Nothing else is changed: no PATH edits, no shell profiles, no privileges.
#
#   sh install.sh                          latest release
#   sh install.sh --version v0.1.0         pinned release
#   sh install.sh --archive A --sums S     a local archive and its SHA256SUMS
set -eu

RELEASE_BASE=https://github.com/Dhruv2mars/javelin/releases
VERSION=latest
BIN_DIR=${JAVELIN_BIN_DIR:-$HOME/.local/bin}
ARCHIVE=
SUMS=

usage() {
  cat <<'EOF'
Usage: install.sh [--version TAG] [--bin-dir DIR] [--release-base URL-OR-DIR]
       install.sh --archive FILE --sums FILE [--bin-dir DIR]

  --version TAG          Release tag to install (default: latest)
  --bin-dir DIR          Directory for the javelin binary (default: $HOME/.local/bin)
  --release-base BASE    Release location: a GitHub-style URL, or a local directory laid
                         out as BASE/download/TAG/FILE and BASE/latest/download/FILE
  --archive FILE         Install this local archive instead of downloading
  --sums FILE            SHA256SUMS file that lists the archive (with --archive)
EOF
}

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=$2; shift 2 ;;
    --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; BIN_DIR=$2; shift 2 ;;
    --release-base) [ $# -ge 2 ] || die "--release-base needs a value"; RELEASE_BASE=$2; shift 2 ;;
    --archive) [ $# -ge 2 ] || die "--archive needs a value"; ARCHIVE=$2; shift 2 ;;
    --sums) [ $# -ge 2 ] || die "--sums needs a value"; SUMS=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

if [ -n "$ARCHIVE" ] && [ -z "$SUMS" ] || [ -z "$ARCHIVE" ] && [ -n "$SUMS" ]; then
  die "--archive and --sums must be given together"
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TRIPLE=aarch64-apple-darwin ;;
  Darwin-x86_64) TRIPLE=x86_64-apple-darwin ;;
  Linux-x86_64) TRIPLE=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TRIPLE=aarch64-unknown-linux-gnu ;;
  *) die "unsupported platform $(uname -s) $(uname -m); see docs/install.md" ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
else
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/javelin-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT INT TERM

# Fetch $1 (a file name in the release) into $TMP, from the URL base or the local directory.
fetch() {
  if [ "$VERSION" = latest ]; then
    source=$RELEASE_BASE/latest/download/$1
  else
    source=$RELEASE_BASE/download/$VERSION/$1
  fi
  case "$RELEASE_BASE" in
    http://*|https://*) curl -fsSL -o "$TMP/$1" "$source" || die "could not download $source" ;;
    *) cp "$source" "$TMP/$1" 2>/dev/null || die "could not read $source" ;;
  esac
}

if [ -n "$ARCHIVE" ]; then
  [ -f "$ARCHIVE" ] || die "no such archive: $ARCHIVE"
  [ -f "$SUMS" ] || die "no such checksum file: $SUMS"
  name=$(basename "$ARCHIVE")
  cp "$ARCHIVE" "$TMP/$name"
  cp "$SUMS" "$TMP/SHA256SUMS"
else
  fetch SHA256SUMS
  # Exactly one archive per platform is listed in a release's SHA256SUMS.
  name=$(awk -v suffix="-$TRIPLE.tar.gz" 'length($2) > 0 && index($2, "javelin-") == 1 && substr($2, length($2) - length(suffix) + 1) == suffix {print $2}' "$TMP/SHA256SUMS")
  [ "$(printf '%s\n' "$name" | grep -c .)" -eq 1 ] || die "release has no single archive for $TRIPLE"
  fetch "$name"
fi

expected=$(awk -v name="$name" '$2 == name || $2 == "*" name {print $1}' "$TMP/SHA256SUMS")
[ "$(printf '%s\n' "$expected" | grep -c .)" -eq 1 ] || die "SHA256SUMS has no single entry for $name"
actual=$(sha256_of "$TMP/$name")
if [ "$expected" != "$actual" ]; then
  die "checksum mismatch for $name (expected $expected, got $actual); nothing was installed"
fi

mkdir "$TMP/extract"
tar -xzf "$TMP/$name" -C "$TMP/extract"
binary=$(find "$TMP/extract" -mindepth 3 -maxdepth 3 -type f -path '*/bin/javelin' | head -n 1)
[ -n "$binary" ] || die "archive $name has no bin/javelin"

mkdir -p "$BIN_DIR"
staged=$BIN_DIR/.javelin.$$.new
cp "$binary" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$BIN_DIR/javelin"

printf 'Installed javelin (%s) to %s/javelin\n' "$name" "$BIN_DIR"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'Add %s to your PATH to run javelin.\n' "$BIN_DIR" ;;
esac
