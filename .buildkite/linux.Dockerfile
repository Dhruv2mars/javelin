# The Linux CI image (.buildkite/ci.sh in_linux). Built on the agent and cached by Docker, so
# a step pays for apt, rustup and bun once, not on every run. Native arm64 on the Mac.
FROM rust:1.99.0
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends clang llvm > /dev/null \
  && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy && rustup target add wasm32-unknown-unknown
ARG BUN=1.4.3
RUN arch=$(dpkg --print-architecture | sed 's/amd64/x64/; s/arm64/aarch64/') \
  && curl -fsSL "https://registry.npmjs.org/@oven/bun-linux-$arch/-/bun-linux-$arch-$BUN.tgz" | tar -xz -C /tmp \
  && mv /tmp/package/bin/bun /usr/local/bin/bun && rm -rf /tmp/package
# Tests assert read-only paths stay read-only, which root ignores: run as a normal user whose
# uid owns the bind-mounted checkout. CARGO_HOME (/cargo) and the target dir are named volumes.
ARG UID=1000
RUN useradd -m -u "$UID" ci && mkdir -p /target /cargo && chown ci /target /cargo
USER ci
