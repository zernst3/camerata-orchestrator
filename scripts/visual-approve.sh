#!/usr/bin/env bash
# Re-approve the visual-regression baselines after a REVIEWED UI change.
#
#   scripts/visual-approve.sh                 # re-render every gallery state, update what changed
#   scripts/visual-approve.sh tree/markers detail/plain   # only these states (both widths)
#
# Renders the review gallery, screenshots each state at 1440 and 1000 wide with headless Chrome
# (the same capture `visual_regression_matches_baselines` makes), and copies a screenshot into
# crates/ui/tests/visual/baselines/ only when it differs from the committed baseline past the
# test's tolerance (or is new). It rewrites baselines/CHROME_VERSION and prints the files it
# changed, so the reviewer sees exactly what was re-approved; look at them (and at
# `git diff --stat`) before committing. A state that no longer exists has its baselines removed
# when no state is named.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

CAMERATA_VISUAL_APPROVE_STATES="$*" \
  cargo test -p camerata-ui --features visual-baselines --test visual visual_approve -- --ignored --nocapture

echo
echo "Baseline files changed in the working tree (new, modified or removed):"
git status --short -- crates/ui/tests/visual/baselines
