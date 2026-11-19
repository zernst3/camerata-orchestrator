//! Defect 2: a rule an agent problem was cited to never reads clean in the ledger.

use super::*;

#[tokio::test]
async fn every_rule_a_problem_is_cited_to_reads_as_having_a_problem_in_the_ledger() {
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }")],
        vec![
            json!({"n": 1, "rule": "ARCH-ONE-1"}),
            json!({"n": 1, "rule": "SEC-TWO-1"}),
        ],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "problems", "problems": [1]}),
            json!({"rule": "SEC-TWO-1", "answer": "problems", "problems": [1]}),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (rows, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(rows.len(), 1, "one row stands for the one problem");
    // What the onboard block does: count by the rows' rule ids, add the finder's rules, record.
    let (mut counts, _) = crate::onboard::split_fired_and_not_assessed(&rows);
    extras.add_rules_with_problems(&mut counts);
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    for rule in ["ARCH-ONE-1", "SEC-TWO-1"] {
        let emitted = counts.get(rule).copied().unwrap_or(0);
        let entry = ledger.record_rule(
            REPO,
            rule,
            crate::scan_ledger::RuleTier::Semantic,
            true,
            None,
            2,
            emitted,
        );
        assert!(
            !entry.verified_clean(),
            "{rule} has a problem cited to it and must not read clean"
        );
    }
}
