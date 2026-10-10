// FIXTURE (crates/server/tests/self_measurement_parity_e2e.rs): Rust only, and ONE planted
// violation so the scan is not vacuously empty.
//
// The planted site fires SEC-NO-RAW-SQL-CONCAT-1 (a DML keyword, a confirming clause, and a
// `{}` placeholder inside one string literal — see camerata_gateway::sec_sql_concat_regex).
// Its presence matters: it proves the deterministic floor genuinely RAN on this repo, so when
// the harness then asserts the eslint tier did not run, that is a statement about eslint and
// not about a scan that did nothing at all.

pub fn find_widget(widget_id: &str) -> String {
    format!("SELECT * FROM widgets WHERE id = {widget_id}")
}

pub fn safe_find() -> &'static str {
    "SELECT * FROM widgets WHERE id = $1"
}
