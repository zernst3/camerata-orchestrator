//! A place of a style linter's rule group never takes an urgent (escalation) slot: the rule is
//! one decision, made as a batch. Synthetic fixtures only.

use std::collections::BTreeSet;

use camerata_api_types::review::EscalationReason;

use super::lint_tests::{grouped, lint_rows, native_row};
use super::*;
use crate::onboard::Finding;

#[test]
fn a_lint_group_never_takes_an_urgent_slot() {
    let mut input = lint_rows("acme/web", "eslint", "no-var", 40);
    input.push(native_row("acme/web", "src/db.js", 12));
    let (mut rows, mut tree) = grouped(input);
    let lint: Vec<Finding> = rows
        .iter()
        .filter(|f| f.rule_id == "no-var")
        .cloned()
        .collect();
    let sql = rows
        .iter()
        .find(|f| f.rule_id == "SEC-NO-RAW-SQL-CONCAT-1")
        .unwrap()
        .clone();
    // The group reviewer nominates three lint places and the SQL place.
    let lint_rule = lint[0].group_path[1].clone();
    let node = tree.nodes.iter_mut().find(|n| n.id == lint_rule).unwrap();
    for (rank, f) in lint.iter().take(3).enumerate() {
        node.escalations.push(NodeEscalation {
            key: f.site_key.clone(),
            reason: EscalationReason::OutlierInGroup,
            rank: rank as u32 + 1,
            why: "This one looks different.".to_string(),
        });
    }
    let sql_rule = sql.group_path[1].clone();
    tree.nodes
        .iter_mut()
        .find(|n| n.id == sql_rule)
        .unwrap()
        .escalations
        .push(NodeEscalation {
            key: sql.site_key.clone(),
            reason: EscalationReason::Injection,
            rank: 1,
            why: "SQL is built from a request value.".to_string(),
        });
    // A lint place whose quote was not found.
    rows.iter_mut()
        .find(|f| f.site_key == lint[5].site_key)
        .unwrap()
        .captures
        .insert(
            crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
            "not found".to_string(),
        );

    let ranked = rank_candidates(&tree.nodes, &rows, &RankSettings::default());
    let lint_keys: BTreeSet<&str> = lint.iter().map(|f| f.site_key.as_str()).collect();
    let urgent: Vec<&str> = ranked
        .candidates
        .iter()
        .map(|c| c.key.as_str())
        .filter(|k| lint_keys.contains(k))
        .collect();
    assert!(
        urgent.is_empty(),
        "a lint group never enters the urgent list: {urgent:?}"
    );
    assert!(
        ranked.candidates.iter().any(|c| c.key == sql.site_key),
        "the SQL place still does"
    );

    // A tree stored with lint candidates drops them when it is assembled again.
    tree.candidates = ranked.candidates.clone();
    tree.candidates
        .push(camerata_api_types::review::EscalationCandidate {
            key: lint[0].site_key.clone(),
            node_id: lint_rule.clone(),
            reason: EscalationReason::OutlierInGroup,
            severity: "medium".to_string(),
            stage2_severity: None,
            stage2_rank: 1,
            global_rank: 2,
            slot: 2,
            escalated: true,
            over_budget_reason: None,
            why: "stored before".to_string(),
        });
    let rebuilt = build_tree(&mut rows, &mut [], Some(tree), &ToolMap::builtin(), "run-1");
    assert!(
        rebuilt
            .candidates
            .iter()
            .all(|c| !lint_keys.contains(c.key.as_str())),
        "a stored lint candidate is dropped"
    );
}
