#!/bin/sh
# Sets up (or repairs) the self-hosted Buildkite agents on this Mac. Idempotent. docs/ci.md.
#
#   tools/ci/setup-mac-agents.sh [SPAWN]
#
# Needs `bk` signed in to the dhruv2mars org, Homebrew, rustup and bun. Installs
# buildkite-agent into ~/.buildkite-agent, creates an agent token on the Default cluster on
# first run (kept only in ~/.buildkite-agent/buildkite-agent.cfg, mode 600), installs Colima
# and the docker CLI, and loads two launchd user services: the agents
# (codes.javelin.buildkite-agent) and Colima (homebrew.mxcl.colima).
set -eu

spawn=${1:-3}
cluster=0619df66-0438-489b-bea4-4c5c0935e98d
home=$HOME/.buildkite-agent
cfg=$home/buildkite-agent.cfg
label=codes.javelin.buildkite-agent
plist=$HOME/Library/LaunchAgents/$label.plist

mkdir -p "$home/bin" "$home/builds" "$home/plugins" "$home/git-mirrors" "$home/cache" "$home/logs"

if ! [ -x "$home/bin/buildkite-agent" ] || ! grep -q '^token=' "$cfg" 2>/dev/null; then
  bk agent install --yes --no-input --dest "$home/bin" --config-path "$cfg" --cluster-uuid "$cluster" > /dev/null
fi

# Rewrite everything but the token.
token=$(grep '^token=' "$cfg")
umask 077
{
  echo "$token"
  cat <<CFG
name="%hostname-%spawn"
tags="queue=mac,os=macos,arch=arm64"
spawn=$spawn
build-path="$home/builds"
plugins-path="$home/plugins"
git-mirrors-path="$home/git-mirrors"
allowed-plugins="entire-buildkite-plugin"
cancel-grace-period=20
CFG
} > "$cfg.new"
mv "$cfg.new" "$cfg"
umask 022

# Colima: a Linux VM for the container steps. Apple's Virtualization.framework with Rosetta
# runs linux/amd64 images; virtiofs shares the agents' checkouts.
command -v colima > /dev/null && command -v docker > /dev/null || brew install colima docker
# The first start writes ~/.colima/default/colima.yaml; the launchd service reuses it.
if ! [ -f "$HOME/.colima/default/colima.yaml" ]; then
  colima start --vm-type vz --vz-rosetta --mount-type virtiofs --cpus 6 --memory 6 --disk 60
  colima stop
fi
brew services start colima > /dev/null
i=0
until docker info > /dev/null 2>&1; do
  i=$((i + 1)); [ $i -lt 120 ] || { echo "Colima did not start; see colima status" >&2; exit 1; }
  sleep 1
done

mkdir -p "$(dirname "$plist")"
cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$home/bin/buildkite-agent</string>
    <string>start</string>
    <string>--config</string>
    <string>$cfg</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key><string>$HOME</string>
    <key>PATH</key><string>$HOME/.local/bin:$HOME/.cargo/bin:$home/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>LANG</key><string>en_US.UTF-8</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Standard</string>
  <!-- CI yields to interactive work and other local test runs. -->
  <key>Nice</key><integer>5</integer>
  <key>StandardOutPath</key><string>$home/logs/agent.log</string>
  <key>StandardErrorPath</key><string>$home/logs/agent.log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)/$label" 2> /dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
echo "agents: launchctl print gui/$(id -u)/$label | grep state; log: $home/logs/agent.log"
