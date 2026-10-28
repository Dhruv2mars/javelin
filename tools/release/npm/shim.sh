#!/bin/sh
# The `javelin` and `javelin-node` commands of the @dhruv2mars/javelin npm package.
#
# npm and bun link this file onto PATH. Where install scripts run (npm), install.js has
# already replaced it with the native binary. Elsewhere (bun, --ignore-scripts) it finds the
# binary in the platform package the install picked (@dhruv2mars/javelin-PLATFORM, an
# optional dependency) and execs it, so the native program replaces the shell: same
# process, arguments, signals and exit status. No Node. It runs no programs but readlink,
# to stay fast.

# Follow the bin links back to this file.
self=$0
while [ -h "$self" ]; do
  link=$(readlink "$self")
  case $link in
    /*) self=$link ;;
    *) self=${self%/*}/$link ;;
  esac
done
name=${self##*/}
pkg=${self%/*}/..

# npm nests the platform package under this one; bun and pnpm put it beside it. Only the
# package for this machine is installed, so the first one found is the one to run.
for platform in darwin-arm64 darwin-x64 linux-x64 linux-arm64 win32-x64; do
  for dir in "$pkg/node_modules/@dhruv2mars/javelin-$platform" "$pkg/../javelin-$platform"; do
    [ -x "$dir/bin/$name" ] && exec "$dir/bin/$name" "$@"
    [ -x "$dir/bin/$name.exe" ] && exec "$dir/bin/$name.exe" "$@"
  done
done
echo "$name: no @dhruv2mars/javelin-PLATFORM package is installed for this machine. Reinstall @dhruv2mars/javelin without omitting optional dependencies; see its README for the platforms." >&2
exit 1
