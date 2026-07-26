// FIXTURE (crates/server/tests/e2e_report_pipeline.rs): this AWS-shaped secret
// literal is a DELIBERATE plant. It fires the deterministic-floor rule
// SEC-NO-HARDCODED-SECRETS-1 (see camerata_gateway::sec_secrets_regex — the
// `AKIA` + 16 uppercase-alphanumeric-char pattern). The e2e test dispositions
// THIS finding as FalsePositive and asserts it is excluded from the report.
pub const AWS_SECRET_ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLEKEYDATA1234567890ABC";
