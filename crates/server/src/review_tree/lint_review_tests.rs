//! Review round 1 of the lint rule groups: an unknown lint stays per place (fail closed), a row
//! that is not a lint never stays in a stored lint group, two lint rules never share a group, each
//! security branch keeps its own place, and the group summary never claims a held place.
//! Synthetic fixtures only.

use std::collections::HashMap;

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::review::EscalationReason;

use super::lint_tests::{grouped, grouped_with, keys_of, lint_row, lint_rows, rule_groups_of};
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

// ── Fix 2: a row that is not a lint never stays in a stored lint group ────────────────────────

#[test]
fn a_security_lint_stored_in_a_lint_group_is_placed_again_outside_it() {
    let (mut rows, tree) = grouped(lint_rows("acme/api", "ruff", "S608", 5));
    // A tree stored before S608 was known to be a security check put it in a lint group.
    let rule = rows[0].group_path[1].clone();
    let lint_id = format!(
        "sub:{}/lint-0123456789ab",
        rule.strip_prefix("rule:").unwrap()
    );
    for f in &mut rows {
        f.group_path[2] = lint_id.clone();
    }
    let tree = build_tree(&mut rows, &mut [], Some(tree), &ToolMap::builtin(), "run-1");
    assert_per_place(&rows, &tree, "S608", "acme/api");
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

// ── Fix 4: each security branch keeps its own place ───────────────────────────────────────────

/// A tool map in which the corpus maps `lint` (a Clippy lint, without its prefix) to `native`.
fn corpus_map(native: &str, lint: &str) -> ToolMap {
    let selectors: HashMap<String, Vec<String>> =
        HashMap::from([(native.to_string(), vec![lint.to_string()])]);
    ToolMap::from_selectors(Some(&selectors))
}

#[test]
fn a_lint_the_corpus_maps_to_a_security_rule_stays_per_place() {
    let tools = corpus_map("SEC-NO-HARDCODED-SECRETS-1", "example_secret_literal");
    assert!(is_security_family_lint(
        "clippy",
        "clippy::example_secret_literal",
        &tools
    ));
    let (rows, tree) = grouped_with(
        lint_rows("acme/core", "clippy", "clippy::example_secret_literal", 4),
        &tools,
    );
    assert_per_place(&rows, &tree, "clippy::example_secret_literal", "acme/core");
}

#[test]
fn a_lint_the_tables_give_a_security_category_is_a_security_check() {
    // An id only the security-category table knows: its id names no security plugin, and no
    // single floor rule shares its category, so no table maps it to a rule.
    let id = "camerata.security.weak-hash-js";
    assert!(crate::finding_security_category(id).is_some());
    assert!(!is_security_lint_id("eslint", id));
    assert!(ToolMap::builtin().native_for(id).is_none());
    assert!(is_security_family_lint("eslint", id, &ToolMap::builtin()));
}

#[test]
fn a_lint_stored_under_a_security_rule_keeps_its_place_when_rebuilt_with_the_built_in_tables() {
    // The scan mapped the lint to a security rule with the corpus loaded; the built-in tables do
    // not know the mapping. The stored rule node still says what the lint is.
    let tools = corpus_map("SEC-NO-HARDCODED-SECRETS-1", "example_secret_literal");
    let (mut rows, tree) = grouped_with(
        lint_rows("acme/core", "clippy", "clippy::example_secret_literal", 4),
        &tools,
    );
    assert!(rows
        .iter()
        .all(|f| f.group_path[1] == "rule:SEC-NO-HARDCODED-SECRETS-1"));
    let tree = build_tree(&mut rows, &mut [], Some(tree), &ToolMap::builtin(), "run-1");
    assert_per_place(&rows, &tree, "clippy::example_secret_literal", "acme/core");
}

#[test]
fn a_lint_stored_under_a_style_rule_stays_one_group_when_rebuilt_with_the_built_in_tables() {
    // The corpus maps an unlisted Clippy lint to a non-security rule: one group, and the stored
    // rule node keeps it one group when the tree is assembled without the corpus.
    let tools = corpus_map("RUST-NAMING-1", "example_style_lint");
    let (mut rows, tree) = grouped_with(
        lint_rows("acme/core", "clippy", "clippy::example_style_lint", 6),
        &tools,
    );
    let tree = build_tree(&mut rows, &mut [], Some(tree), &ToolMap::builtin(), "run-1");
    let groups = rule_groups_of(&tree, &rows, "clippy::example_style_lint", "acme/core");
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert!(is_lint_sub_id(&groups[0].id), "{}", groups[0].id);
}

// ── The summary never claims a held place ─────────────────────────────────────────────────────

#[test]
fn the_lint_group_summary_leaves_out_the_places_held_for_their_own_decision() {
    let (rows, tree) = grouped(lint_rows("acme/web", "eslint", "no-var", 3));
    let group = rule_groups_of(&tree, &rows, "no-var", "acme/web")[0];
    let summary = group.summary.clone().unwrap_or_default();
    assert!(
        summary.contains("every place not held for its own decision"),
        "{summary}"
    );
    let (rows, tree) = grouped(lint_rows("acme/web", "eslint", "no-var", 1));
    let group = rule_groups_of(&tree, &rows, "no-var", "acme/web")[0];
    let summary = group.summary.clone().unwrap_or_default();
    assert!(
        summary.contains("unless it is held for its own decision"),
        "{summary}"
    );
}
