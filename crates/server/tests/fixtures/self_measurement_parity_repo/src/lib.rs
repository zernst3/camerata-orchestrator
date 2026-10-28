// FIXTURE (crates/server/tests/self_measurement_parity_e2e.rs): a deliberately CLEAN module.
//
// Present so the scan has a genuinely healthy file to credit. Without one, "every rule that
// ran emitted a finding" and the harness could not distinguish a verified-clean rule from a
// rule that never ran — which is the exact conflation the ledger exists to prevent.

pub mod orders;

/// Parameterised, as it should be. Fires nothing.
pub fn safe_lookup() -> &'static str {
    "SELECT * FROM orders WHERE id = $1"
}
