#!/usr/bin/env bash
# W6 — run Camerata's SELF-MEASUREMENT suite. Seconds, deterministic, zero API spend.
#
# > these two prevent overspending on tokens
# >   — Zach, on I1 + I2 (docs/plans/2026-10-09_audit-findings-tracker.md)
#
# A graded sweep of four benchmark repos costs roughly $26 and four hours of wall clock.
# Everything this script catches is money and time not spent. Run it before you push, and
# certainly before you pay for a graded run.
#
# What it runs:
#   1. I1 — the synthetic golden-fixture parity harness (artifact parity, row conservation,
#      no silent non-execution), end to end through the real pipeline. Since Pass-1 W7 fixed
#      C5 (absorbed merge sites now reach every artifact as `also_locations` + `site_count`),
#      the once-#[ignore]d C5 observability assertion runs here by DEFAULT and must be green.
#   2. I2 — the pre-flight integrity gate's own unit suite, both halves (engine + CLI).
#   3. The C11 eslint regression suite.
#
# `--include-known-red` is kept for the next deliberately-red measurement; today there are no
# #[ignore]d tests in the harness, so it adds nothing.
#
# No network, no model calls, no scan of a real repo. Nothing here can cost money.

set -euo pipefail
cd "$(dirname "$0")/.."

INCLUDE_KNOWN_RED=0
for arg in "$@"; do
  case "$arg" in
    --include-known-red) INCLUDE_KNOWN_RED=1 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg (try --help)" >&2; exit 2 ;;
  esac
done

echo "── I1: synthetic golden-fixture parity harness ────────────────────────────────"
cargo test -p camerata-server --test self_measurement_parity_e2e

echo
echo "── I2: pre-flight integrity gate (engine + CLI) ───────────────────────────────"
cargo test -p camerata-server --lib integrity_gate
cargo test -p camerata --lib integrity_gate_cmd

echo
echo "── C11: eslint preview regression suite ───────────────────────────────────────"
cargo test -p camerata-server --lib req_c11

if [ "$INCLUDE_KNOWN_RED" -eq 1 ]; then
  echo
  echo "── KNOWN RED: deliberately-#[ignore]d measurements (failures ARE the data) ───"
  echo "   (none exist today — C5 was fixed by W7 and its assertion runs by default)"
  cargo test -p camerata-server --test self_measurement_parity_e2e -- --ignored || true
fi

echo
echo "SELF-MEASUREMENT: all green."
