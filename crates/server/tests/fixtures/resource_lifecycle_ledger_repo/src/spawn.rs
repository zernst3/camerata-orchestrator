// FIXTURE (crates/server/tests/resource_lifecycle_ledger_credit_e2e.rs): a DELIBERATE plant
// that fires ARCH-RESOURCE-LIFECYCLE-1's spawn facet (crates/checks/src/
// resource_lifecycle_checker.rs) — a tokio::process::Command spawned without
// .kill_on_drop(true) anywhere in its chain. This checker is `advisory_coexisting` (always
// LLM-advisory-eligible, never config-gated) and must be credited directly at the
// Architectural tier by the ledger, never folded into a "no wired detector" classification.
async fn run_it() {
    let _ = Command::new("ls").arg("-la").spawn();
}
