//! The escalation formula (design 4.4 and 4.8): the ranking is deterministic, interleaves nodes
//! within one reason rank, escalates the first `budget` and the D5 reasons past it, and keeps
//! everything else below the line, sorted.

use camerata_api_types::review::{EscalationBudget, EscalationReason, EscalationState};

use super::*;
use crate::inspections::backfill_site_keys;
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::onboard::Finding;

fn row(rule: &str, n: usize, severity: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: format!("src/{rule}/{n}.rs"),
        line: n + 1,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet {rule} {n}"),
        detail: "detail".to_string(),
        ..Finding::default()
    }
}

fn esc(f: &Finding, reason: EscalationReason, rank: u32) -> NodeEscalation {
    NodeEscalation {
        key: f.site_key.clone(),
        reason,
        rank,
        why: format!("why {rank}"),
    }
}

/// A tree of three rules with `per_rule` rows each, and the rows. Severity is set per row by
/// `severity`.
fn tree_of(
    per_rule: usize,
    severity: impl Fn(&str, usize) -> &'static str,
) -> (ReviewTree, Vec<Finding>) {
    let mut rows: Vec<Finding> = Vec::new();
    for rule in [
        "SEC-NO-RAW-SQL-CONCAT-1",
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-RLS-ENABLED-1",
    ] {
        for n in 0..per_rule {
            rows.push(row(rule, n, severity(rule, n)));
        }
    }
    backfill_site_keys(&mut rows);
    let tree = build_tree(&mut rows, &mut [], None, &ToolMap::builtin(), "run-1");
    (tree, rows)
}

fn rule_node_of<'a>(tree: &'a mut ReviewTree, f: &Finding) -> &'a mut TreeNode {
    let id = f.group_path[1].clone();
    tree.nodes.iter_mut().find(|n| n.id == id).unwrap()
}

fn settings(floor: usize) -> RankSettings {
    RankSettings {
        floor,
        ..RankSettings::default()
    }
}

#[test]
fn ranking_is_deterministic_for_the_same_candidates() {
    let (mut tree, rows) = tree_of(4, |_, _| "high");
    for (i, f) in rows.iter().enumerate() {
        let reason = EscalationReason::ALL[i % 4 + 4];
        let node = rule_node_of(&mut tree, f);
        node.escalations.push(esc(f, reason, (i % 3) as u32 + 1));
    }
    let first = rank_candidates(&tree.nodes, &rows, &settings(5));
    assert!(!first.candidates.is_empty());
    let mut shuffled_nodes = tree.nodes.clone();
    shuffled_nodes.reverse();
    for n in &mut shuffled_nodes {
        n.escalations.reverse();
    }
    let mut shuffled_rows = rows.clone();
    shuffled_rows.reverse();
    let second = rank_candidates(&shuffled_nodes, &shuffled_rows, &settings(5));
    assert_eq!(
        first, second,
        "node and row order do not change the ranking"
    );
    let ranks: Vec<u32> = first.candidates.iter().map(|c| c.global_rank).collect();
    assert_eq!(ranks, (1..=ranks.len() as u32).collect::<Vec<_>>());
}

#[test]
fn ranking_interleaves_nodes_within_one_reason_rank() {
    // Three rules, four candidates each, all the same reason and severity: the list must go
    // rank 1 of every node, then rank 2 of every node, so one noisy rule cannot take the budget.
    let (mut tree, rows) = tree_of(4, |_, _| "high");
    for f in &rows {
        let rank = (f.line - 1) as u32 + 1; // 1..=4 within a rule
        let node = rule_node_of(&mut tree, f);
        node.escalations
            .push(esc(f, EscalationReason::OutlierInGroup, rank));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    assert_eq!(ranked.candidates.len(), 12);
    let stage2_ranks: Vec<u32> = ranked.candidates.iter().map(|c| c.stage2_rank).collect();
    assert_eq!(
        stage2_ranks,
        vec![1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4],
        "round-robin across the three nodes"
    );
    let first_round: std::collections::BTreeSet<&str> = ranked.candidates[..3]
        .iter()
        .map(|c| c.node_id.as_str())
        .collect();
    assert_eq!(
        first_round.len(),
        3,
        "the first round has one from each node"
    );
}

#[test]
fn a_higher_reason_rank_always_comes_before_a_lower_one_whatever_the_node() {
    let (mut tree, rows) = tree_of(2, |_, _| "high");
    let a = &rows[0];
    let b = &rows[2];
    let c = &rows[4];
    rule_node_of(&mut tree, c)
        .escalations
        .push(esc(c, EscalationReason::SecretExposure, 9));
    rule_node_of(&mut tree, a)
        .escalations
        .push(esc(a, EscalationReason::NeedsMoreCode, 1));
    rule_node_of(&mut tree, b)
        .escalations
        .push(esc(b, EscalationReason::ConflictingVerdicts, 1));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    let reasons: Vec<EscalationReason> = ranked.candidates.iter().map(|c| c.reason).collect();
    assert_eq!(
        reasons,
        vec![
            EscalationReason::SecretExposure,
            EscalationReason::ConflictingVerdicts,
            EscalationReason::NeedsMoreCode
        ]
    );
}

#[test]
fn over_budget_item_always_records_its_reason() {
    let (mut tree, rows) = tree_of(3, |_, n| if n == 0 { "critical" } else { "high" });
    // Nine candidates; a budget of 2. The critical secret exposure and the unverified one are
    // past the line but are escalated, and say why.
    for (i, f) in rows.iter().enumerate() {
        let reason = if f.line == 1 {
            EscalationReason::SecretExposure
        } else {
            EscalationReason::RuleMisfit
        };
        rule_node_of(&mut tree, f)
            .escalations
            .push(esc(f, reason, i as u32 + 1));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(2));
    assert_eq!(ranked.budget.budget, 2);
    let over: Vec<&EscalationCandidate> = ranked
        .candidates
        .iter()
        .filter(|c| c.escalated && c.over_budget_reason.is_some())
        .collect();
    assert_eq!(
        over.len(),
        1,
        "the third critical secret is over: {over:#?}"
    );
    assert_eq!(
        over[0].over_budget_reason,
        Some(EscalationReason::SecretExposure)
    );
    for c in &ranked.candidates {
        if c.escalated && c.global_rank > 2 {
            assert!(
                c.over_budget_reason.is_some(),
                "{c:?} is past the budget and escalated, so it records why"
            );
        }
        if !c.escalated {
            assert!(c.over_budget_reason.is_none());
        }
    }
    assert_eq!(ranked.budget.over_budget, 1);
    assert_eq!(ranked.budget.escalated, 3);
}

#[test]
fn candidates_below_the_line_are_kept_and_ordered() {
    let (mut tree, rows) = tree_of(3, |_, _| "high");
    for (i, f) in rows.iter().enumerate() {
        rule_node_of(&mut tree, f).escalations.push(esc(
            f,
            EscalationReason::RuleMisfit,
            i as u32 + 1,
        ));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(4));
    assert_eq!(ranked.candidates.len(), 9, "none is dropped");
    assert_eq!(ranked.budget.candidates, 9);
    let escalated: Vec<bool> = ranked.candidates.iter().map(|c| c.escalated).collect();
    assert_eq!(
        escalated,
        [vec![true; 4], vec![false; 5]].concat(),
        "the first four are above the line and the rest follow in order"
    );
    let below: Vec<u32> = ranked
        .candidates
        .iter()
        .filter(|c| !c.escalated)
        .map(|c| c.global_rank)
        .collect();
    assert_eq!(below, vec![5, 6, 7, 8, 9]);
}

#[test]
fn unverified_evidence_is_always_a_candidate_and_always_escalated() {
    let (mut tree, mut rows) = tree_of(2, |_, _| "low");
    rows[3].captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the quoted code is not in the file".to_string(),
    );
    // The group reviewer cited three other sites and said nothing about the unverified one.
    for i in [0usize, 1, 2] {
        let f = rows[i].clone();
        rule_node_of(&mut tree, &f).escalations.push(esc(
            &f,
            EscalationReason::RuleMisfit,
            i as u32 + 1,
        ));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(1));
    let unverified = ranked
        .candidates
        .iter()
        .find(|c| c.key == rows[3].site_key)
        .expect("the unverified row is a candidate the engine added");
    assert_eq!(unverified.reason, EscalationReason::EvidenceUnverified);
    assert!(unverified.escalated, "{unverified:?}");
    assert_eq!(
        unverified.over_budget_reason,
        Some(EscalationReason::EvidenceUnverified)
    );
}

#[test]
fn an_escalation_for_a_site_that_is_not_in_the_report_is_ignored() {
    let (mut tree, rows) = tree_of(1, |_, _| "high");
    let ghost = NodeEscalation {
        key: "no such site".to_string(),
        reason: EscalationReason::DataLoss,
        rank: 1,
        why: "x".to_string(),
    };
    tree.nodes
        .iter_mut()
        .find(|n| n.level == NodeLevel::Rule)
        .unwrap()
        .escalations
        .push(ghost);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(5));
    assert!(ranked.candidates.is_empty());
}

#[test]
fn changing_the_rate_after_a_run_re_marks_without_a_model_call() {
    let (mut tree, mut rows) = tree_of(3, |_, _| "high");
    for (i, f) in rows.clone().iter().enumerate() {
        rule_node_of(&mut tree, f).escalations.push(esc(
            f,
            EscalationReason::OutlierInGroup,
            i as u32 + 1,
        ));
    }
    let ranked = rank_candidates(
        &tree.nodes,
        &rows,
        &RankSettings {
            counted_lines: 2_000,
            ..settings(3)
        },
    );
    tree.budget = ranked.budget;
    tree.candidates = ranked.candidates;
    tree.sync_rows(&mut rows);
    assert_eq!(tree.budget.budget, 3);
    assert_eq!(
        rows.iter()
            .filter(|f| f.review.escalation_state == EscalationState::Escalated)
            .count(),
        3
    );

    // The architect lowers the line to 1 per 1,000 lines with a floor of 1 and then raises it:
    // no model is involved, only the stored candidates.
    tree.re_mark(
        &RankSettings {
            floor: 1,
            counted_lines: 4_000,
            ..RankSettings::default()
        },
        &mut rows,
    );
    assert_eq!(
        tree.budget.budget, 4,
        "ceil(4000 / 1000 * 1.0) beats the floor"
    );
    assert_eq!(tree.budget.escalated, 4);
    assert_eq!(
        rows.iter()
            .filter(|f| f.review.escalation_state == EscalationState::Escalated)
            .count(),
        4
    );
    assert_eq!(
        rows.iter()
            .filter(|f| f.review.escalation_state == EscalationState::BelowTheLine)
            .count(),
        5
    );
    tree.re_mark(
        &RankSettings {
            rate_per_kloc: 0.0,
            floor: 2,
            counted_lines: 4_000,
            ..RankSettings::default()
        },
        &mut rows,
    );
    assert_eq!(tree.budget.budget, 2);
    assert_eq!(tree.budget.escalated, 2);
    let want = EscalationBudget::compute(0.0, 2, 4_000).budget;
    assert_eq!(tree.budget.budget, want);
    assert_eq!(
        tree.candidates.len(),
        9,
        "re-marking never drops a candidate"
    );
}

#[test]
fn the_projects_over_budget_reason_set_decides_what_goes_past_the_line() {
    // One critical unauthenticated reach and one critical secret exposure, both past a budget of
    // zero. The project allows only secret exposure past the line.
    let (mut tree, mut rows) = tree_of(2, |_, _| "critical");
    let reach = rows[0].clone();
    let secret = rows[2].clone();
    rule_node_of(&mut tree, &reach).escalations.push(esc(
        &reach,
        EscalationReason::UnauthenticatedReach,
        1,
    ));
    rule_node_of(&mut tree, &secret).escalations.push(esc(
        &secret,
        EscalationReason::SecretExposure,
        1,
    ));
    let only_secrets = RankSettings {
        over_budget_reasons: vec![EscalationReason::SecretExposure],
        ..settings(0)
    };
    let ranked = rank_candidates(&tree.nodes, &rows, &only_secrets);
    let of = |key: &str| ranked.candidates.iter().find(|c| c.key == key).unwrap();
    assert!(of(&secret.site_key).escalated);
    assert_eq!(
        of(&secret.site_key).over_budget_reason,
        Some(EscalationReason::SecretExposure)
    );
    assert!(
        !of(&reach.site_key).escalated,
        "unauthenticated reach is not in this project's set"
    );
    assert_eq!(of(&reach.site_key).over_budget_reason, None);
    assert_eq!(ranked.budget.escalated, 1);
    assert_eq!(ranked.budget.over_budget, 1);
    // The same holds when the line is moved again over the stored candidates.
    tree.budget = ranked.budget;
    tree.candidates = ranked.candidates;
    tree.re_mark(&only_secrets, &mut rows);
    let state = |key: &str| {
        rows.iter()
            .find(|f| f.site_key == key)
            .unwrap()
            .review
            .escalation_state
    };
    assert_eq!(state(&secret.site_key), EscalationState::Escalated);
    assert_eq!(state(&reach.site_key), EscalationState::BelowTheLine);
}

#[test]
fn an_empty_over_budget_set_lets_nothing_past_the_budget_not_even_unverified_evidence() {
    let (mut tree, mut rows) = tree_of(2, |_, _| "critical");
    rows[4].captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found".to_string(),
    );
    let reach = rows[0].clone();
    let secret = rows[2].clone();
    rule_node_of(&mut tree, &reach).escalations.push(esc(
        &reach,
        EscalationReason::UnauthenticatedReach,
        1,
    ));
    rule_node_of(&mut tree, &secret).escalations.push(esc(
        &secret,
        EscalationReason::SecretExposure,
        1,
    ));
    let none_allowed = RankSettings {
        over_budget_reasons: Vec::new(),
        ..settings(0)
    };
    let ranked = rank_candidates(&tree.nodes, &rows, &none_allowed);
    assert_eq!(
        ranked.candidates.len(),
        3,
        "the unverified row is a candidate"
    );
    assert!(
        ranked.candidates.iter().all(|c| !c.escalated),
        "{:?}",
        ranked.candidates
    );
    assert!(ranked
        .candidates
        .iter()
        .all(|c| c.over_budget_reason.is_none()));
    assert_eq!(ranked.budget.escalated, 0);
    assert_eq!(ranked.budget.over_budget, 0);
}
