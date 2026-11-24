#!/bin/bash
# Install (or reinstall) the weekly cargo-sweep launchd job that skips kept scan builds. Idempotent.
# The OWNER runs this once, by hand: scripts/install-cargo-sweep-weekly.sh
# It first boots out AND disables the old unguarded hand-installed job (a loaded label ending in
# .cargo-sweep whose plist runs cargo-sweep), so it stays off after a restart; the old plist file is
# left where it is. Then it installs the new job.
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
# Old job: a loaded label ending in .cargo-sweep whose plist (in ~/Library/LaunchAgents) runs
# cargo-sweep in its ProgramArguments. Anything else is never touched. The plist file is neither
# edited nor deleted; bootout stops it now and disable stops launchd loading it at next login.
uid="$(id -u)"
for oldlabel in $(launchctl list 2>/dev/null | awk '{print $3}' | grep -E '\.cargo-sweep$' || true); do
  oldplist="$HOME/Library/LaunchAgents/$oldlabel.plist"
  [ -f "$oldplist" ] || continue
  sed -n '/<key>ProgramArguments<\/key>/,/<\/array>/p' "$oldplist" | grep -q 'cargo-sweep' || continue
  launchctl bootout "gui/$uid/$oldlabel" 2>/dev/null || true
  launchctl disable "gui/$uid/$oldlabel"
  echo "old job disabled: $oldlabel (plist left in place). To undo: launchctl enable gui/$uid/$oldlabel"
done
launchctl bootout "gui/$(id -u)" "$dest" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$dest"
echo "installed: $(launchctl list | grep cargo-sweep-weekly)"
