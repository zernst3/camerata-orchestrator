#!/bin/bash
# cargo-runner: cargo runs every `cargo run` binary (and every test executable) through this on
# macOS (see .cargo/config.toml). It signs the camerata binaries with scripts/sign-dev.sh before
# launching them, so a rebuilt binary keeps its Keychain access, then runs the program unchanged.
# Anything else is executed straight away.

case "$(basename "$1")" in
    camerata | camerata-ui | camerata-gateway)
        "$(dirname "$0")/sign-dev.sh" "$1" || true
        ;;
esac
exec "$@"
