//! Review round 1 of the lint rule groups: an unknown lint stays per place (fail closed), a row
//! that is not a lint never stays in a stored lint group, two lint rules never share a group, each
//! security branch keeps its own place, and the group summary never claims a held place.
//! Synthetic fixtures only.

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};

use super::lint_tests::{grouped, keys_of, lint_row, lint_rows, rule_groups_of};
use crate::decisions::{apply_to, DecisionCtx, DecisionStore};

// ── Fix 3: two lint rules never share a group ─────────────────────────────────────────────────

#[test]
fn two_lint_rules_under_one_native_rule_are_two_groups_decided_apart() {
    // Both jest rules map to one native rule, so they share a rule node.
    let mut input = lint_rows("acme/web", "eslint", "jest/expect-expect", 6);
    input.extend(
        (0..4).map(|i| lint_row("acme/web", "eslint", "jest/no-conditional-expect", 100 + i)),
    );
    let (rows, tree) = grouped(input);
    let expect = rule_groups_of(&tree, &rows, "jest/expect-expect", "acme/web");
    let conditional = rule_groups_of(&tree, &rows, "jest/no-conditional-expect", "acme/web");
    assert_eq!(expect.len(), 1, "one group for jest/expect-expect");
    assert_eq!(
        conditional.len(),
        1,
        "one group for jest/no-conditional-expect"
    );
    assert_ne!(expect[0].id, conditional[0].id, "two rules, two groups");
    assert!(
        expect[0].label.contains("jest/expect-expect"),
        "{}",
        expect[0].label
    );
    assert!(
        conditional[0].label.contains("jest/no-conditional-expect"),
        "{}",
        conditional[0].label
    );

    // Ruling out one rule leaves the other undecided.
    let node_id = expect[0].id.clone();
    let store = DecisionStore::new();
    let req = NewDecisionReq {
        scope: Scope::Node(node_id.clone()),
        key: String::new(),
        state: DecisionState::FalsePositive,
        reason: "These tests assert through a helper.".to_string(),
        decided_by: "auditor".to_string(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: String::new(),
        decided_at: String::new(),
    };
    let ctx = DecisionCtx {
        run_id: "run-1",
        engine_version: "0.0.0-test",
        now: "2026-11-16T09:00:00Z",
    };
    store
        .record_node("p", &node_id, &tree.batch_members(&node_id), &req, ctx)
        .unwrap();
    let applied = apply_to(&tree, &store.active("p"), &[]);
    for key in keys_of(&rows, "jest/expect-expect", "acme/web") {
        assert!(applied.get(&key).is_some_and(|s| s.decision.is_some()));
    }
    for key in keys_of(&rows, "jest/no-conditional-expect", "acme/web") {
        assert!(
            applied.get(&key).is_none_or(|s| s.decision.is_none()),
            "jest/no-conditional-expect is still undecided"
        );
    }
}
