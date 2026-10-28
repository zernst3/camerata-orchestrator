#!/bin/bash
# Install (or reinstall) the 30-minute cargo-disk-guard launchd job. Idempotent.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
dest="$HOME/Library/LaunchAgents/dev.zachernst.cargo-disk-guard.plist"
repo="$(dirname "$here")"
# The tracked plist is a template; fill in this checkout and this user's home.
sed -e "s|__REPO_DIR__|$repo|g" -e "s|__HOME__|$HOME|g" \
  "$here/launchd/dev.zachernst.cargo-disk-guard.plist" > "$dest"
if grep -q '__[A-Z_]*__' "$dest"; then
  echo "unfilled placeholder in $dest" >&2
  exit 1
fi
launchctl bootout "gui/$(id -u)" "$dest" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$dest"
echo "installed: $(launchctl list | grep cargo-disk-guard)"
