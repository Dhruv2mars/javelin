#!/bin/sh
# Javelin must never invoke Git. Builds a failing `git` into DIR that leaves DIR/invoked behind
# when anything runs it, and proves Rust resolves it before the tests rely on it. Put DIR first
# on PATH for the run, then check that DIR/invoked is absent.
#   sh .buildkite/git-shim.sh DIR
set -eu
shim=$1
probe=$(mktemp -d)
mkdir -p "$shim"
echo 'fn main() { let d = std::env::current_exe().unwrap(); std::fs::write(d.parent().unwrap().join("invoked"), b"").unwrap(); std::process::exit(97) }' > "$probe/shim.rs"
echo 'fn main() { std::process::exit(std::process::Command::new("git").status().ok().and_then(|s| s.code()).unwrap_or(1)) }' > "$probe/probe.rs"
rustc -o "$shim/git" "$probe/shim.rs"
rustc -o "$probe/probe" "$probe/probe.rs"
if PATH="$shim:$PATH" "$probe/probe"; then
  echo "the git stand-in did not run" >&2
  exit 1
else
  code=$?
fi
test "$code" -eq 97
rm "$shim/invoked"
rm -r "$probe"
# The tests run as another user, who must be able to leave the marker.
chmod 777 "$shim"
