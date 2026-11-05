#!/bin/bash
# clickthrough: run the review lanes' click-through tests (review UI spec sections 13 and 14).
#
# Each test starts a fresh `camerata seed-review` server on a free port, the liveview build of the
# review lanes in front of it (`camerata-ui liveview`, feature `liveview-review`) and a headless
# Google Chrome, then clicks and types like a reviewer and reads what the screen and the server's
# own files say. Nothing is left running when a test ends, and no model is called (each test
# asserts the server's usage meter is still at zero).
#
# A missing Chrome FAILS: these tests never skip. Point CAMERATA_CHROME at another Chrome binary if
# yours is not in /Applications.
#
# Usage: scripts/clickthrough.sh [test name filter] [-- cargo test arguments]
set -euo pipefail
cd "$(dirname "$0")/.."

CHROME="${CAMERATA_CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
if [ ! -x "$CHROME" ]; then
    echo "clickthrough: Google Chrome is not at: $CHROME" >&2
    echo "clickthrough: the click-through tests need it and never skip. Install Chrome or set CAMERATA_CHROME." >&2
    exit 1
fi

exec cargo test -p camerata-ui --features liveview-review --test clickthrough "$@"
