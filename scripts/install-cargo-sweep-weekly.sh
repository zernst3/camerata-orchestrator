#!/bin/bash
# Install (or reinstall) the weekly cargo-sweep launchd job that skips kept scan builds. Idempotent.
# The OWNER runs this once, by hand: scripts/install-cargo-sweep-weekly.sh
# It first boots out the old unguarded hand-installed job (any loaded label ending in .cargo-sweep) if present (the
# old plist file is left where it is, so you can delete it yourself), then installs the new job.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
dest="$HOME/Library/LaunchAgents/dev.camerata.cargo-sweep-weekly.plist"
repo="$(dirname "$here")"
sed -e "s|__REPO_DIR__|$repo|g" -e "s|__HOME__|$HOME|g" \
  "$here/launchd/dev.camerata.cargo-sweep-weekly.plist" > "$dest"
if grep -q '__[A-Z_]*__' "$dest"; then
  echo "unfilled placeholder in $dest" >&2
  exit 1
fi
for oldlabel in $(launchctl list 2>/dev/null | awk '{print $3}' | grep -E '\.cargo-sweep$' || true); do
  launchctl bootout "gui/$(id -u)/$oldlabel" 2>/dev/null || true
  echo "old job $oldlabel booted out (its plist in ~/Library/LaunchAgents is left in place)"
done
launchctl bootout "gui/$(id -u)" "$dest" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$dest"
echo "installed: $(launchctl list | grep cargo-sweep-weekly)"
