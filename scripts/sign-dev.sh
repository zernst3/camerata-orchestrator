#!/bin/bash
# sign-dev: give dev builds of the camerata binaries one stable code signature.
#
# Why: macOS remembers a Keychain "Always Allow" per code signature. An unsigned (ad hoc) build
# gets a new signature on every rebuild, so macOS asks again whether `camerata` may read the
# Anthropic key, and an unattended run waits on that prompt. Signing every dev build with the same
# local certificate keeps the answer.
#
# Dev builds only: the "Camerata Dev" certificate is a self-signed one in the login keychain. This
# does not replace real release signing.
#
# Usage: scripts/sign-dev.sh <binary> [<binary> ...]
# Each binary is signed with identifier = its file name (so `camerata` gets `camerata`). Missing
# files are skipped. It does nothing (exit 0) off macOS, on CI, when CAMERATA_SKIP_DEV_SIGN=1, or
# when the certificate is not in the keychain, so builds elsewhere never fail because of it.

set -u
IDENTITY="${CAMERATA_DEV_SIGN_IDENTITY:-Camerata Dev}"

[ "$(uname -s)" = "Darwin" ] || exit 0
[ -z "${CI:-}" ] || exit 0
[ "${CAMERATA_SKIP_DEV_SIGN:-0}" = "1" ] && exit 0
if ! security find-identity -p codesigning 2>/dev/null | grep -q "\"$IDENTITY\""; then
    echo "sign-dev: no \"$IDENTITY\" certificate in the keychain; leaving the build unsigned" >&2
    exit 0
fi

status=0
for bin in "$@"; do
    [ -f "$bin" ] || continue
    # Already signed by this certificate and unchanged since: leave it, so a relaunch does not
    # rewrite the file (which would also trigger macOS's first-run scan again).
    if codesign --verify "$bin" 2>/dev/null &&
        codesign -dvv "$bin" 2>&1 | grep -q "^Authority=$IDENTITY\$"; then
        continue
    fi
    if ! codesign --force --sign "$IDENTITY" --identifier "$(basename "$bin")" "$bin" 2>/dev/null; then
        echo "sign-dev: could not sign $bin" >&2
        status=1
    fi
done
exit $status
