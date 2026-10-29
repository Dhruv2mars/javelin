#!/bin/sh
# Runs a shell command as the unprivileged user `ci`, made on first use: tests assert that
# read-only paths stay read-only, which root ignores. PATH has cargo, bun and, when GIT_SHIM
# names one, the failing git stand-in first; the rest of the environment passes through.
#   sh .buildkite/as-ci.sh 'COMMAND'
set -eu
if ! id ci > /dev/null 2>&1; then
  useradd -m ci
  chown -R ci:ci .
  chmod -R a+rwX /usr/local/cargo
fi
exec su ci -c "export PATH=${GIT_SHIM:+$GIT_SHIM:}/usr/local/package/bin:/usr/local/cargo/bin:\$PATH CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup && $1"
