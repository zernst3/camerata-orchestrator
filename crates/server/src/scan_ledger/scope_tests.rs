//! A rule that evaluated no file in scope for a repo is never "clean" there: it had nothing to look
//! at. Same principle as the rest of run-to-completion: never clean without evidence.

use super::*;

const RULE: &str = "ARCH-TEST-SCOPE-1";

#[test]
fn an_architectural_rule_that_evaluated_no_file_is_not_applicable_not_clean() {
    let mut ledger = ScanLedger::new();
    ledger.record_rule("acme/one", RULE, RuleTier::Architectural, true, None, 0, 0);
    match ledger.cell_state(RULE, "acme/one", true) {
        CellState::NotApplicable { reason } => {
            assert!(reason.contains("no files in scope"), "{reason}")
        }
        other => panic!("expected not applicable, got {other:?}"),
    }
}

#[test]
fn a_genuinely_repo_level_rule_with_no_files_stays_clean() {
    let mut ledger = ScanLedger::new();
    ledger.record_rule("acme/one", RULE, RuleTier::Architectural, true, None, 0, 0);
    ledger.mark_repo_level_rule("acme/one", RULE);
    assert_eq!(ledger.cell_state(RULE, "acme/one", true), CellState::Clean);
}

#[test]
fn an_architectural_rule_that_looked_at_files_and_found_nothing_is_clean() {
    let mut ledger = ScanLedger::new();
    ledger.record_rule("acme/one", RULE, RuleTier::Architectural, true, None, 3, 0);
    assert_eq!(ledger.cell_state(RULE, "acme/one", true), CellState::Clean);
}

#[test]
fn a_finding_still_makes_the_cell_violated_whatever_the_file_count() {
    let mut ledger = ScanLedger::new();
    ledger.record_rule("acme/one", RULE, RuleTier::Architectural, true, None, 0, 2);
    assert_eq!(
        ledger.cell_state(RULE, "acme/one", true),
        CellState::Violated { sites: 2 }
    );
}

#[test]
fn a_carried_semantic_result_with_no_fresh_files_is_still_clean() {
    let mut ledger = ScanLedger::new();
    ledger.record_rule("acme/one", RULE, RuleTier::Semantic, true, None, 0, 0);
    assert_eq!(ledger.cell_state(RULE, "acme/one", true), CellState::Clean);
}

#[test]
fn a_repo_with_nothing_to_look_at_does_not_make_services_differ() {
    // One service violates the rule, the other has no file the rule covers: that is not a
    // service handling the rule differently, so the matrix has no row for it.
    let mut ledger = ScanLedger::new();
    ledger.record_rule("acme/one", RULE, RuleTier::Architectural, true, None, 4, 2);
    ledger.record_rule("acme/two", RULE, RuleTier::Architectural, true, None, 0, 0);
    let one = ledger.cell_state(RULE, "acme/one", true);
    let two = ledger.cell_state(RULE, "acme/two", true);
    assert!(matches!(one, CellState::Violated { .. }));
    assert!(
        !matches!(two, CellState::Clean),
        "the second service must not read as clean: {two:?}"
    );
}
