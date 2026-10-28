#!/bin/sh
# The `javelin` and `javelin-node` commands of the @dhruv2mars/javelin npm package.
#
# npm and bun link this file onto PATH. It finds the binary in this platform's package
# (@dhruv2mars/javelin-PLATFORM, an optional dependency) and execs it, so the native
# program replaces the shell: same process, arguments, signals and exit status. No Node.
set -eu

# Follow the bin links back to this file, then to the package directory.
self=$0
while [ -h "$self" ]; do
  link=$(readlink "$self")
  case $link in
    /*) self=$link ;;
    *) self=$(dirname "$self")/$link ;;
  esac
done
name=$(basename "$self")
pkg=$(cd "$(dirname "$self")/.." && pwd -P)

ext=
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) platform=darwin-arm64 ;;
  Darwin-x86_64) platform=darwin-x64 ;;
  Linux-x86_64) platform=linux-x64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-arm64 ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) platform=win32-x64; ext=.exe ;;
  *) echo "$name: no Javelin build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

# npm nests the platform package under this one; bun and pnpm put it beside it.
for dir in "$pkg/node_modules/@dhruv2mars/javelin-$platform" "$pkg/../javelin-$platform"; do
  if [ -x "$dir/bin/$name$ext" ]; then
    exec "$dir/bin/$name$ext" "$@"
  fi
done
echo "$name: @dhruv2mars/javelin-$platform is not installed. Reinstall @dhruv2mars/javelin without omitting optional dependencies." >&2
exit 1
