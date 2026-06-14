#![allow(clippy::unwrap_used)]
//! Integration-level acceptance test (ORCH-NEW-PATH-TESTS-1).
//!
//! The "planted-violation acceptance run" proving the engine is wired: it
//! constructs the gateway + a fake/echo AgentDriver + a checks runner entirely
//! in-process (NO network, NO live claude) and asserts the layer-1 gate denies
//! a planted GOV-1 violation while allowing a clean control write.

use camerata::acceptance::run_acceptance;
use camerata_core::Decision;

#[tokio::test]
async fn planted_violation_is_denied_engine_is_wired() {
    let result = run_acceptance().await.expect("acceptance run completes");

    // GOV-1 deny on the planted forbidden-path write (mirrors verified slice).
    assert!(
        matches!(result.planted_violation_decision, Decision::Deny { .. }),
        "planted violation must be DENIED — got {:?}",
        result.planted_violation_decision
    );

    // Control write allowed — the gate is selective, not deny-all.
    assert!(
        matches!(result.clean_control_decision, Decision::Allow),
        "clean control write must be ALLOWED — got {:?}",
        result.clean_control_decision
    );

    assert!(result.passed(), "whole acceptance scenario must pass");
}
