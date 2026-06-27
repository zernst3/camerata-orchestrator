//! Integration gate (R3.e): cross-repo contract verification.
#![allow(dead_code)] // The sync `check_integration_gate` is test/CI-only; the live path is server-side.
//!
//! After fan_out assembly, this gate reads the prose contract from the UoW
//! investigation + the assembled per-repo diffs/outputs and verifies:
//! 1. Each repo builds (mechanical).
//! 2. The cross-repo contract holds (agent-driven semantic check).
//!
//! The mechanical build check (1) is the existing per-repo Layer-2 gate —
//! we call that externally. This module owns (2): the agent-driven contract
//! verification.
//!
//! # Seam
//!
//! `check_integration_gate` is the entry point for the SYNCHRONOUS (no-model) path.
//! The live (model-backed) path is `check_integration_gate_live` in
//! `camerata_server::review_agent` — called by the server after L2/L3 complete.
//!
//! When `contract` is `None` (no contract in the UoW — single-repo or no
//! cross-boundary work), returns `IntegrationGateResult::NoContractRequired`.
//! When `contract` is `Some(prose)`, validates the prose is non-empty, then
//! returns `Pending` (no model available in this sync path).
//!
//! The `Pending` variant signals "no model available; the server should decide."
//! In server contexts, `Pending` is never returned because `check_integration_gate_live`
//! runs instead. In CI without API access, callers treat `Pending` as pass-through
//! or block per policy.
//!
//! This is intentionally NOT a per-repo mechanical rule (those can't see both
//! sides of a contract boundary). See spec R3.e.

use crate::fan_out::WorkerResult;

/// The outcome of the integration gate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationGateResult {
    /// No contract was required (single-repo work or no cross-boundary changes).
    /// Assembly can proceed to the per-repo Layer-2 gates.
    NoContractRequired,
    /// A contract exists and was verified: all repos build and the contract holds.
    Passed,
    /// A contract exists but the check found a mismatch. The orchestrator must
    /// reconcile (possibly re-delegating a fix to one repo's worker).
    BounceToOrchestrator { reason: String },
    /// The contract check agent was not invoked (no live model in this
    /// environment). The LIVE check is performed server-side by
    /// `camerata_server::review_agent::check_integration_gate_live`
    /// when a model is available. Callers without a model key (CI without
    /// API access) treat this as pass-through or block per policy.
    /// See `crates/server/src/review_agent.rs`.
    Pending { contract_prose: String },
}

/// The input the integration gate needs to evaluate cross-repo contract coherence.
#[derive(Debug, Clone)]
pub struct IntegrationGateInput<'a> {
    /// The prose contract from the UoW investigation (R3.g). `None` = no contract
    /// in scope (single-repo work or no cross-boundary changes).
    pub contract: Option<&'a str>,
    /// The assembled worker results, one per repo.
    pub assembled: &'a [WorkerResult],
}

/// Run the integration gate.
///
/// Entry point for R3.e. Called after fan_out assembly, before the per-repo
/// Ship panels.
///
/// - If `input.contract` is `None`: returns `NoContractRequired` immediately.
/// - If `input.contract` is `Some(prose)` and prose is empty (or whitespace-only):
///   returns `BounceToOrchestrator` — a contract that exists but is empty is a
///   signal to push back, not pass through.
/// - Otherwise: returns `Pending` — the live model-backed check runs server-side
///   via `camerata_server::review_agent::check_integration_gate_live`.
pub fn check_integration_gate(input: &IntegrationGateInput<'_>) -> IntegrationGateResult {
    let Some(contract) = input.contract else {
        return IntegrationGateResult::NoContractRequired;
    };

    if contract.trim().is_empty() {
        return IntegrationGateResult::BounceToOrchestrator {
            reason: "Contract artifact exists but is empty. A contract that gates development \
                     must contain the agreed interface prose before development starts (R3.g). \
                     Push back to the human or refinement agent to fill it."
                .to_string(),
        };
    }

    // The live agent check is wired server-side in
    // `camerata_server::review_agent::check_integration_gate_live`.
    // This sync path (no model available) returns Pending so callers can decide
    // policy (CI: pass-through; prod: block).
    IntegrationGateResult::Pending {
        contract_prose: contract.to_string(),
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fan_out::WorkerResult;

    fn worker(repo: &str, output: &str) -> WorkerResult {
        WorkerResult {
            repo: repo.to_string(),
            domain: "test".to_string(),
            output: output.to_string(),
            incomplete: output.contains("INCOMPLETE:"),
        }
    }

    #[test]
    fn no_contract_returns_no_contract_required() {
        let assembled = vec![worker("backend", "done"), worker("frontend", "done")];
        let input = IntegrationGateInput {
            contract: None,
            assembled: &assembled,
        };
        assert_eq!(
            check_integration_gate(&input),
            IntegrationGateResult::NoContractRequired
        );
    }

    #[test]
    fn empty_contract_bounces_to_orchestrator() {
        let assembled = vec![worker("backend", "done")];

        // Completely empty string.
        let input = IntegrationGateInput {
            contract: Some(""),
            assembled: &assembled,
        };
        match check_integration_gate(&input) {
            IntegrationGateResult::BounceToOrchestrator { reason } => {
                assert!(reason.contains("empty"), "reason should mention empty: {reason}");
            }
            other => panic!("expected BounceToOrchestrator, got {other:?}"),
        }

        // Whitespace-only string also counts as empty.
        let input_ws = IntegrationGateInput {
            contract: Some("   \n\t  "),
            assembled: &assembled,
        };
        assert!(matches!(
            check_integration_gate(&input_ws),
            IntegrationGateResult::BounceToOrchestrator { .. }
        ));
    }

    #[test]
    fn nonempty_contract_returns_pending() {
        // This exercises the TODO(#105-followup) path: a real contract exists but
        // no live agent check is wired yet. Pending is the correct response.
        let assembled = vec![worker("backend", "done"), worker("frontend", "done")];
        let contract = "GET /api/users returns [{id, name, email}] — agreed between \
                        backend (REST) and frontend (fetch hook).";
        let input = IntegrationGateInput {
            contract: Some(contract),
            assembled: &assembled,
        };
        match check_integration_gate(&input) {
            IntegrationGateResult::Pending { contract_prose } => {
                assert_eq!(contract_prose, contract);
            }
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[test]
    fn no_contract_ignores_assembled_content() {
        // Even with multiple workers, no-contract short-circuits cleanly.
        let assembled = vec![
            worker("backend", "done"),
            worker("frontend", "INCOMPLETE: schema drift"),
            worker("migration", "done"),
        ];
        let input = IntegrationGateInput {
            contract: None,
            assembled: &assembled,
        };
        assert_eq!(
            check_integration_gate(&input),
            IntegrationGateResult::NoContractRequired
        );
    }
}
