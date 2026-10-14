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
#   4. W13 — the corpus-load integrity harness. "No silent non-execution" (I1's INV-3) had a
#      hole: it asserted that every rule the LEDGER says did not run is named in an artifact,
#      and a rule whose own `.toml` failed to parse produced no ledger entry at all, so it was
#      outside the invariant's reach entirely. This block scans a synthetic repo with a
#      synthetic corpus containing one deliberately malformed rule file, and asserts the rule
#      is recorded, disclosed, and FAILS the integrity gate — plus a control proving the same
#      scan over an intact corpus still passes, so the gate stays switched on.
#   5. W14 — the corpus SCHEMA gates. W13 catches a rule file that will not PARSE; W14 catches
#      one that parses but is wrong: an option whose `why` opens with "Rejected:" and yet
#      carries no `violates = true`, a `decision`-kind rule that carries one anyway, and any
#      unknown field name at any nesting level. The motivating measurement was a hand audit of
#      244 `violates` flags in which 76 (31 percent) were wrong, the dominant shape being a
#      MISSING flag — the engine silent on a fault its own rule describes in plain language.
#      Each gate is proven to FIRE against a synthetic corpus, not merely to pass.
#   6. W16 — the scan-vs-emit split. Every corpus rule must declare `inspectable` (can the
#      scan assess this rule from a repository at all?), the exact set declared
#      `inspectable = false` is pinned by id, and the end-to-end half proves both directions:
#      an emit-only rule never reaches the model, the ledger or "What's healthy", AND it is
#      still installed verbatim into the client's AGENTS.md / CONVENTIONS.md. The second half
#      is the one most likely to break silently, and breaking it deletes client value rather
#      than noise.
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
    -h|--help) sed -n '2,31p' "$0"; exit 0 ;;
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

echo
echo "── W13: corpus-load integrity (a rule file that would not parse) ──────────────"
cargo test -p camerata-server --test corpus_load_failure_e2e
cargo test -p camerata-server --lib req_w13

echo
echo "── W14: corpus SCHEMA gates (Rejected:/violates parity, unknown fields) ───────"
cargo test -p camerata-rules --test w14_corpus_schema_gates

echo
echo "── W16: scan-vs-emit (every rule declares \`inspectable\`; the population is pinned) ──"
cargo test -p camerata-rules --test w16_inspectable_gate
# W17/W18 corpus gates. These were enforced only by `cargo test --workspace`, so a
# pre-push run could pass while the corpus carried a deleted field, an unhandled rule
# kind, a summary silently falling back to `decision.why`, or a dropped source
# annotation. Every one of those is a silent-no-op shape this effort exists to close.
cargo test -p camerata-rules --test w17_tag_field_deleted
cargo test -p camerata-rules --test w17_decision_with_floor_kind
cargo test -p camerata-rules --test w17_qualifies_at_rule_level
cargo test -p camerata-rules --test w17_policy_arms_the_rule
cargo test -p camerata-rules --test w18_source_relevance
cargo test -p camerata-rules --test w11_kind_taxonomy
cargo test -p camerata-rules --test w15_a57_reclassifications
cargo test -p camerata-server --test w16_emit_only_rules_e2e

if [ "$INCLUDE_KNOWN_RED" -eq 1 ]; then
  echo
  echo "── KNOWN RED: deliberately-#[ignore]d measurements (failures ARE the data) ───"
  echo "   (none exist today — C5 was fixed by W7 and its assertion runs by default)"
  cargo test -p camerata-server --test self_measurement_parity_e2e -- --ignored || true
fi

echo
echo "SELF-MEASUREMENT: all green."
