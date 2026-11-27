#!/usr/bin/env bash
# The local test gate on cargo-nextest (issue #340): the same checks and the same tests as the
# `cargo test` gate, with the tests run one process each.
#
#   scripts/gate-nextest-local.sh
#   CAMERATA_NEXTEST_JOBS=6 scripts/gate-nextest-local.sh   # more tests at once
#
# Steps, in order; the first failing step stops the gate with its exit status:
#   1. cargo fmt --all -- --check
#   2. cargo clippy --workspace --all-targets -- -D warnings
#   3. cargo nextest run --workspace --locked -j N   (N = CAMERATA_NEXTEST_JOBS, default 3)
#   4. cargo test --doc --workspace --locked         (nextest does not run doctests)
#
# Step 3 runs every test even when one fails (`fail-fast = false` in .config/nextest.toml) and
# never retries one. No test is filtered out here, including tests that cannot pass off macOS.
#
# The default of 3 tests at once leaves cores free on a machine that is also building. Raise it
# when the machine is otherwise idle. Install nextest with `cargo install cargo-nextest --locked`
# (or the release binary); see docs/NEXTEST_GATE.md.

set -euo pipefail
cd "$(dirname "$0")/.."

jobs="${CAMERATA_NEXTEST_JOBS:-3}"
case "$jobs" in
  ''|*[!0-9]*|0)
    echo "gate-nextest-local: CAMERATA_NEXTEST_JOBS must be a positive whole number, got '$jobs'" >&2
    exit 2
    ;;
esac

if ! cargo nextest --version >/dev/null 2>&1; then
  echo "gate-nextest-local: cargo-nextest is not installed (cargo install cargo-nextest --locked)" >&2
  exit 2
fi

step() { printf '\n── %s ──\n' "$1"; }

step "1/4 format"
cargo fmt --all -- --check

step "2/4 clippy (deny warnings)"
cargo clippy --workspace --all-targets -- -D warnings

step "3/4 tests (nextest, ${jobs} at once)"
cargo nextest run --workspace --locked -j "$jobs"

step "4/4 doctests"
cargo test --doc --workspace --locked

printf '\ngate-nextest-local: all green.\n'
