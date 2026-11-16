//! Review round 1 of the lint rule groups: an unknown lint stays per place (fail closed), a row
//! that is not a lint never stays in a stored lint group, two lint rules never share a group, each
//! security branch keeps its own place, and the group summary never claims a held place.
//! Synthetic fixtures only.

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::review::EscalationReason;

use super::lint_tests::{grouped, keys_of, lint_row, lint_rows, rule_groups_of};
use super::*;
use crate::decisions::{apply_to, DecisionCtx, DecisionStore};
use crate::onboard::Finding;

/// Whether every place of `rule` in `repo` keeps per-place handling: none is in a lint group, and a
/// group reviewer's nomination of one of them reaches the candidate list.
fn assert_per_place(rows: &[Finding], tree: &ReviewTree, rule: &str, repo: &str) {
    let keys = keys_of(rows, rule, repo);
    assert!(!keys.is_empty(), "{rule} has places");
    for f in rows.iter().filter(|f| f.rule_id == rule && f.repo == repo) {
        assert!(
            !in_lint_group(f),
            "{rule} stays per place, but it is in {:?}",
            f.group_path
        );
    }
    for g in rule_groups_of(tree, rows, rule, repo) {
        assert!(
            !is_lint_sub_id(&g.id),
            "{rule} is in the lint group {}",
            g.id
        );
    }
    let f = rows
        .iter()
        .find(|f| f.rule_id == rule && f.repo == repo)
        .unwrap();
    let mut nodes = tree.nodes.clone();
    nodes
        .iter_mut()
        .find(|n| n.id == f.group_path[1])
        .unwrap()
        .escalations
        .push(NodeEscalation {
            key: f.site_key.clone(),
            reason: EscalationReason::Injection,
            rank: 1,
            why: "Code is run from a string.".to_string(),
        });
    let ranked = rank_candidates(&nodes, rows, &RankSettings::default());
    assert!(
        ranked.candidates.iter().any(|c| c.key == f.site_key),
        "{rule} can take an urgent slot"
    );
}

// ── Fix 1: an unknown lint id stays per place ─────────────────────────────────────────────────

#[test]
fn an_eslint_rule_no_table_knows_stays_per_place_and_can_be_urgent() {
    let mut input = Vec::new();
    for rule in [
        "no-eval",
        "no-implied-eval",
        "no-new-func",
        "react/no-danger",
        "no-unsanitized/property",
        "no-secrets/no-secrets",
        "@microsoft/sdl/no-inner-html",
    ] {
        input.extend(lint_rows("acme/web", "eslint", rule, 3));
    }
    let (rows, tree) = grouped(input);
    for rule in [
        "no-eval",
        "no-implied-eval",
        "no-new-func",
        "react/no-danger",
        "no-unsanitized/property",
        "no-secrets/no-secrets",
        "@microsoft/sdl/no-inner-html",
    ] {
        assert_per_place(&rows, &tree, rule, "acme/web");
    }
}

#[test]
fn an_unmapped_clippy_lint_stays_per_place_and_can_be_urgent() {
    let (rows, tree) = grouped(lint_rows("acme/core", "clippy", "clippy::unwrap_used", 6));
    assert_per_place(&rows, &tree, "clippy::unwrap_used", "acme/core");
}

#[test]
fn a_style_lint_row_whose_own_category_is_security_stays_per_place() {
    // `no-var` is a style rule, but this row was classified as an injection problem.
    let input: Vec<Finding> = (0..4)
        .map(|i| Finding {
            category: Some("injection".to_string()),
            ..lint_row("acme/web", "eslint", "no-var", i)
        })
        .collect();
    let (rows, tree) = grouped(input);
    assert_per_place(&rows, &tree, "no-var", "acme/web");
}

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
