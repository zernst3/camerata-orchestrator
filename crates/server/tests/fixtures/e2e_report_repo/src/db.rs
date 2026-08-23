// FIXTURE (crates/server/tests/e2e_report_pipeline.rs): a SQL query built via
// format-string interpolation. This is a DELIBERATE plant that fires the
// deterministic-floor rule SEC-NO-RAW-SQL-CONCAT-1 (see
// camerata_gateway::sec_sql_concat_regex — a DML keyword + a confirming
// clause + a `{}`/`{name}` placeholder, all inside the same string literal).
// The e2e test dispositions THIS finding as Ignored (accepted risk, with a
// reason) and asserts it survives into the report as an accepted-risk row.
pub fn find_user_query(user_id: &str) -> String {
    format!("SELECT * FROM users WHERE id = {user_id}")
}
