#!/bin/bash
# Install (or reinstall) the 30-minute cargo-disk-guard launchd job. Idempotent.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
dest="$HOME/Library/LaunchAgents/dev.zachernst.cargo-disk-guard.plist"
cp "$here/launchd/dev.zachernst.cargo-disk-guard.plist" "$dest"
launchctl bootout "gui/$(id -u)" "$dest" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$dest"
echo "installed: $(launchctl list | grep cargo-disk-guard)"
