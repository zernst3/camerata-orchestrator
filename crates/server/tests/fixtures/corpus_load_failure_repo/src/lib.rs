// FIXTURE (crates/server/tests/corpus_load_failure_e2e.rs). SYNTHETIC BY CONSTRUCTION — see
// this fixture's Cargo.toml.
//
// Its only job is to make the W13 scan NON-VACUOUS. The defect under test is that a rule file
// which fails to parse disappears from a scan with nothing reporting it, and a harness that
// proved that over a scan which found nothing at all would prove nothing: "no rows" and "the
// engine never ran" would be indistinguishable, which is the exact conflation this whole
// workstream exists to close one level up.
//
// So this module plants ONE unambiguous violation of a real corpus rule with a real wired
// detector (SEC-NO-RAW-SQL-CONCAT-1) and keeps one genuinely clean function beside it. The
// scan therefore demonstrably ran: something fired, and something was credited clean.

/// Raw SQL built by string concatenation. Fires SEC-NO-RAW-SQL-CONCAT-1, proving the
/// deterministic floor actually ran over this fixture.
pub fn find_invoice(invoice_id: &str) -> String {
    format!("SELECT * FROM invoices WHERE id = {invoice_id}")
}

/// Parameterised, as it should be. Fires nothing, so "ran and found nothing" stays
/// distinguishable from "never ran".
pub fn safe_invoice_lookup() -> &'static str {
    "SELECT * FROM invoices WHERE id = $1"
}
