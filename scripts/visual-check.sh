#!/usr/bin/env bash
# Run the pixel-diff visual regression over the review gallery (crates/ui/tests/visual.rs).
#
#   scripts/visual-check.sh
#
# Renders every gallery state at 1440 and 1000 wide with headless Chrome and compares each with
# its approved baseline in crates/ui/tests/visual/baselines/. It needs the Chrome major version
# and platform the baselines were approved with (baselines/CHROME_VERSION), so it runs in the
# local release gate on macOS and not in GitHub CI (whose default `cargo test --workspace` does
# not build this target; the comparison's unit tests in tests/visual_compare.rs do run there).
# On a failure the actual and diff PNGs are written under target/visual-diff/. After a reviewed
# change, re-approve with scripts/visual-approve.sh.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

exec cargo test -p camerata-ui --features visual-baselines --test visual "$@"
