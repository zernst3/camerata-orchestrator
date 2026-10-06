// FIXTURE (crates/server/tests/pipeline_integrity_ledger_e2e.rs): an innocuous, clean file —
// no deterministic-floor violation anywhere in it. Exists so SEC-NO-HARDCODED-SECRETS-1 has a
// real file to run against and genuinely find nothing, proving the W1 ledger's "ran == true &&
// findings_emitted == 0" healthy path through the REAL pipeline, not a synthetic ledger entry.
pub fn noop() {}
