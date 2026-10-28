// FIXTURE (crates/server/tests/pipeline_integrity_ledger_e2e.rs): a SQL query built via
// format-string interpolation. This is a DELIBERATE plant that fires the deterministic-floor
// rule SEC-NO-RAW-SQL-CONCAT-1 (see camerata_gateway::sec_sql_concat_regex — a DML keyword +
// a confirming clause + a `{}`/`{name}` placeholder, all inside the same string literal).
// Proves the W1 ledger's "fired" path through the REAL pipeline: this rule must appear in
// neither "What's healthy" nor "excluded from this audit".
pub fn find_user_query(user_id: &str) -> String {
    format!("SELECT * FROM users WHERE id = {user_id}")
}
