// FIXTURE (crates/server/tests/e2e_report_pipeline.rs): a webhook URL that
// carries a secret in its query string. This is a DELIBERATE plant that fires
// the deterministic-floor rule ARCH-NO-SECRETS-IN-URL-1 (see
// camerata_gateway::arch_url_secret_regex — an http(s) URL with a
// `?`/`&`-bound `token=` param). The e2e test leaves THIS finding
// Unresolved and asserts it still appears as an "Open" item in the report.
pub fn webhook_url() -> String {
    "https://hooks.example.com/notify?token=abc123def456".to_string()
}
