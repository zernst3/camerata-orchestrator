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
fn an_empty_over_budget_set_lets_only_unverified_evidence_past_the_budget() {
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
    // The severe-reach reasons obey the stored set: none of them gets past the budget.
    assert!(
        ranked
            .candidates
            .iter()
            .filter(|c| c.reason != EscalationReason::EvidenceUnverified)
            .all(|c| !c.escalated && c.over_budget_reason.is_none()),
        "{:?}",
        ranked.candidates
    );
    // Unverified evidence always does, whatever a stored set says (the gate refuses the row
    // without a decision, and a project file from before the settings route refused an empty set
    // can still hold one).
    let unverified: Vec<_> = ranked
        .candidates
        .iter()
        .filter(|c| c.reason == EscalationReason::EvidenceUnverified)
        .collect();
    assert_eq!(unverified.len(), 1);
    assert!(unverified[0].escalated);
    assert_eq!(
        unverified[0].over_budget_reason,
        Some(EscalationReason::EvidenceUnverified)
    );
    assert_eq!(ranked.budget.escalated, 1);
    assert_eq!(ranked.budget.over_budget, 1);
}

// ---- R-1: a slot is a site, not a row ----------------------------------------------------

const RULE_A: &str = "SEC-NO-RAW-SQL-CONCAT-1";
const RULE_B: &str = "SEC-NO-HARDCODED-SECRETS-1";
const RULE_C: &str = "SEC-RLS-ENABLED-1";

fn row_at(rule: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet {rule} {path}"),
        detail: "detail".to_string(),
        ..Finding::default()
    }
}

/// The tree and rows of `rows`, with every row keyed and placed.
fn tree_of_rows(mut rows: Vec<Finding>) -> (ReviewTree, Vec<Finding>) {
    backfill_site_keys(&mut rows);
    let tree = build_tree(&mut rows, &mut [], None, &ToolMap::builtin(), "run-1");
    (tree, rows)
}

fn row_of<'a>(rows: &'a [Finding], rule: &str, path: &str) -> &'a Finding {
    rows.iter()
        .find(|f| f.rule_id == rule && f.path == path)
        .unwrap_or_else(|| panic!("no row of {rule} at {path}"))
}

#[test]
fn rows_of_different_rules_at_one_site_take_one_slot() {
    // Lumen's shape: three sites each flagged by three rules (nine rows, nine slots under the
    // old arithmetic), and six more sites with one row each. A budget of six is three sites
    // plus three of the singles, not two sites.
    let mut rows = Vec::new();
    for s in 0..3 {
        for rule in [RULE_A, RULE_B, RULE_C] {
            rows.push(row_at(
                rule,
                &format!("app/api/site{s}/route.ts"),
                8,
                "high",
            ));
        }
    }
    for n in 0..6 {
        rows.push(row_at(RULE_A, &format!("src/solo{n}.rs"), 4, "high"));
    }
    let (mut tree, rows) = tree_of_rows(rows);
    for s in 0..3 {
        for (rule, node_rank) in [(RULE_A, s + 1), (RULE_B, s + 1), (RULE_C, s + 1)] {
            let f = row_of(&rows, rule, &format!("app/api/site{s}/route.ts")).clone();
            rule_node_of(&mut tree, &f).escalations.push(esc(
                &f,
                EscalationReason::UnauthenticatedReach,
                node_rank as u32,
            ));
        }
    }
    for n in 0..6 {
        let f = row_of(&rows, RULE_A, &format!("src/solo{n}.rs")).clone();
        rule_node_of(&mut tree, &f).escalations.push(esc(
            &f,
            EscalationReason::UnauthenticatedReach,
            n as u32 + 4,
        ));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(6));
    assert_eq!(ranked.candidates.len(), 15, "every row stays a candidate");
    let escalated_at = |path: &str| {
        ranked
            .candidates
            .iter()
            .filter(|c| {
                rows.iter()
                    .any(|f| f.site_key == c.key && f.path == path && c.escalated)
            })
            .count()
    };
    for s in 0..3 {
        assert_eq!(
            escalated_at(&format!("app/api/site{s}/route.ts")),
            3,
            "all three rows of site {s} share one slot and are escalated together"
        );
    }
    let solos_escalated = (0..6)
        .filter(|n| escalated_at(&format!("src/solo{n}.rs")) == 1)
        .count();
    assert_eq!(solos_escalated, 3, "the six freed slots go to other sites");
    assert_eq!(ranked.budget.budget, 6);
    assert_eq!(ranked.budget.escalated, 6, "the budget counts slots");
    assert_eq!(ranked.budget.over_budget, 0);
    assert_eq!(ranked.budget.candidates, 9, "nine sites in all");
}

#[test]
fn a_shared_site_takes_the_highest_severity_of_its_rows_and_shows_every_reason() {
    let rows = vec![
        row_at(RULE_A, "app/x.ts", 5, "low"),
        row_at(RULE_B, "app/x.ts", 5, "critical"),
        row_at(RULE_A, "app/y.ts", 9, "medium"),
    ];
    let (mut tree, rows) = tree_of_rows(rows);
    let a = row_of(&rows, RULE_A, "app/x.ts").clone();
    let b = row_of(&rows, RULE_B, "app/x.ts").clone();
    let y = row_of(&rows, RULE_A, "app/y.ts").clone();
    rule_node_of(&mut tree, &a).escalations.push(esc(
        &a,
        EscalationReason::UnauthenticatedReach,
        2,
    ));
    rule_node_of(&mut tree, &b)
        .escalations
        .push(esc(&b, EscalationReason::SecretExposure, 1));
    rule_node_of(&mut tree, &y)
        .escalations
        .push(esc(&y, EscalationReason::SecretExposure, 1));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(1));
    let of = |key: &str| ranked.candidates.iter().find(|c| c.key == key).unwrap();
    assert_eq!(
        of(&a.site_key).severity,
        "low",
        "each row keeps its own severity; the site is ordered by its highest"
    );
    assert_eq!(of(&b.site_key).severity, "critical");
    assert_eq!(
        of(&a.site_key).reason,
        EscalationReason::UnauthenticatedReach
    );
    assert_eq!(of(&b.site_key).reason, EscalationReason::SecretExposure);
    assert!(
        of(&a.site_key).escalated && of(&b.site_key).escalated,
        "the whole site is inside the one slot"
    );
    assert!(
        !of(&y.site_key).escalated,
        "the other site is past a budget of one"
    );
}

// ---- R-2: severity first, then reason rank, then the group reviewer's priority --------------

#[test]
fn a_medium_or_low_unauthenticated_reach_never_takes_a_slot_ahead_of_a_high_severe_row() {
    let mut rows = Vec::new();
    for n in 0..3 {
        rows.push(row_at(RULE_C, &format!("app/public{n}.ts"), 3, "medium"));
    }
    rows.push(row_at(RULE_C, "app/public3.ts", 3, "low"));
    rows.push(row_at(RULE_B, "lib/secret.ts", 4, "high"));
    rows.push(row_at(RULE_A, "lib/wipe.ts", 5, "high"));
    rows.push(row_at(RULE_A, "lib/tenant.ts", 6, "high"));
    let (mut tree, rows) = tree_of_rows(rows);
    for n in 0..4 {
        let f = row_of(&rows, RULE_C, &format!("app/public{n}.ts")).clone();
        rule_node_of(&mut tree, &f).escalations.push(esc(
            &f,
            EscalationReason::UnauthenticatedReach,
            n as u32 + 1,
        ));
    }
    for (rule, path, reason) in [
        (RULE_B, "lib/secret.ts", EscalationReason::SecretExposure),
        (RULE_A, "lib/wipe.ts", EscalationReason::DataLoss),
        (RULE_A, "lib/tenant.ts", EscalationReason::CrossTenant),
    ] {
        let f = row_of(&rows, rule, path).clone();
        rule_node_of(&mut tree, &f)
            .escalations
            .push(esc(&f, reason, 9));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(3));
    let path_of = |key: &str| {
        rows.iter()
            .find(|f| f.site_key == key)
            .unwrap()
            .path
            .clone()
    };
    let above: std::collections::BTreeSet<String> = ranked
        .candidates
        .iter()
        .filter(|c| c.escalated)
        .map(|c| path_of(&c.key))
        .collect();
    assert_eq!(
        above,
        ["lib/secret.ts", "lib/wipe.ts", "lib/tenant.ts"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "the three high severe rows fill the budget; the medium and low reach rows wait"
    );
    // Inside one severity the reason rank decides.
    let order: Vec<EscalationReason> = ranked.candidates.iter().take(3).map(|c| c.reason).collect();
    assert_eq!(
        order,
        vec![
            EscalationReason::SecretExposure,
            EscalationReason::DataLoss,
            EscalationReason::CrossTenant
        ]
    );
}

#[test]
fn severity_decides_before_reason_and_the_reviewers_priority_decides_last() {
    let rows = vec![
        row_at(RULE_A, "a.ts", 1, "high"),
        row_at(RULE_A, "b.ts", 1, "critical"),
        row_at(RULE_A, "c.ts", 1, "high"),
        row_at(RULE_A, "d.ts", 1, "high"),
    ];
    let (mut tree, rows) = tree_of_rows(rows);
    let put = |tree: &mut ReviewTree, path: &str, reason, rank| {
        let f = row_of(&rows, RULE_A, path).clone();
        rule_node_of(tree, &f)
            .escalations
            .push(esc(&f, reason, rank));
    };
    put(&mut tree, "a.ts", EscalationReason::UnauthenticatedReach, 1);
    put(&mut tree, "b.ts", EscalationReason::RuleMisfit, 4);
    put(&mut tree, "c.ts", EscalationReason::CrossTenant, 3);
    put(&mut tree, "d.ts", EscalationReason::CrossTenant, 2);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    let paths: Vec<&str> = ranked
        .candidates
        .iter()
        .map(|c| {
            rows.iter()
                .find(|f| f.site_key == c.key)
                .unwrap()
                .path
                .as_str()
        })
        .collect();
    assert_eq!(
        paths,
        vec!["b.ts", "a.ts", "d.ts", "c.ts"],
        "critical first, then high by reason rank, then by the reviewer's own rank"
    );
}

// ---- R-3: authorization_bypass and injection join the severe set ---------------------------

#[test]
fn a_high_stored_xss_row_and_a_high_authorization_bypass_row_land_above_the_line() {
    let rows = vec![
        row_at(RULE_A, "components/CommentBody.tsx", 7, "high"),
        row_at(RULE_B, "app/instructor/actions.ts", 14, "high"),
        row_at(RULE_C, "lib/config.ts", 2, "medium"),
    ];
    let (mut tree, rows) = tree_of_rows(rows);
    for (rule, path, reason) in [
        (
            RULE_A,
            "components/CommentBody.tsx",
            EscalationReason::Injection,
        ),
        (
            RULE_B,
            "app/instructor/actions.ts",
            EscalationReason::AuthorizationBypass,
        ),
        (RULE_C, "lib/config.ts", EscalationReason::RuleMisfit),
    ] {
        let f = row_of(&rows, rule, path).clone();
        rule_node_of(&mut tree, &f)
            .escalations
            .push(esc(&f, reason, 1));
    }
    // A budget of zero: only the over-budget valve can put a row above the line.
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    assert_eq!(ranked.candidates.len(), 3, "every one is a candidate");
    let of = |path: &str| {
        ranked
            .candidates
            .iter()
            .find(|c| rows.iter().any(|f| f.site_key == c.key && f.path == path))
            .unwrap()
    };
    let xss = of("components/CommentBody.tsx");
    assert!(xss.escalated, "{xss:?}");
    assert_eq!(xss.over_budget_reason, Some(EscalationReason::Injection));
    let authz = of("app/instructor/actions.ts");
    assert!(authz.escalated, "{authz:?}");
    assert_eq!(
        authz.over_budget_reason,
        Some(EscalationReason::AuthorizationBypass)
    );
    assert!(!of("lib/config.ts").escalated, "a medium misfit waits");
}

#[test]
fn a_high_secret_data_loss_or_cross_tenant_row_goes_over_budget_and_a_high_reach_row_does_not() {
    let rows = vec![
        row_at(RULE_A, "a.sql", 1, "high"),
        row_at(RULE_A, "b.sql", 1, "high"),
        row_at(RULE_A, "c.sql", 1, "high"),
        row_at(RULE_A, "d.ts", 1, "high"),
    ];
    let (mut tree, rows) = tree_of_rows(rows);
    for (path, reason) in [
        ("a.sql", EscalationReason::SecretExposure),
        ("b.sql", EscalationReason::DataLoss),
        ("c.sql", EscalationReason::CrossTenant),
        ("d.ts", EscalationReason::UnauthenticatedReach),
    ] {
        let f = row_of(&rows, RULE_A, path).clone();
        rule_node_of(&mut tree, &f)
            .escalations
            .push(esc(&f, reason, 1));
    }
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    let escalated_reasons: Vec<EscalationReason> = ranked
        .candidates
        .iter()
        .filter(|c| c.escalated)
        .map(|c| c.reason)
        .collect();
    assert_eq!(
        escalated_reasons,
        vec![
            EscalationReason::SecretExposure,
            EscalationReason::DataLoss,
            EscalationReason::CrossTenant
        ],
        "unauthenticated reach still needs critical to pass the budget"
    );
}

// ---- a convention rule's row never carries a security reason ------------------------------

#[test]
fn a_security_reason_on_a_convention_rules_row_is_read_as_a_rule_misfit_beside_a_security_row() {
    // The shape from lumen: the group reviewer gave a non-null-assertion row the secret
    // exposure of the hardcoded key at the same site, and at critical the row passed the
    // budget. The key is a row of its own rule at that site, so the style row is a misfit.
    let convention = "JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1";
    let rows = vec![
        row_at(convention, "scripts/backfill.ts", 5, "critical"),
        row_at(RULE_B, "scripts/backfill.ts", 5, "critical"),
    ];
    let (mut tree, rows) = tree_of_rows(rows);
    let style = row_of(&rows, convention, "scripts/backfill.ts").clone();
    let secret = row_of(&rows, RULE_B, "scripts/backfill.ts").clone();
    rule_node_of(&mut tree, &style).escalations.push(esc(
        &style,
        EscalationReason::SecretExposure,
        1,
    ));
    rule_node_of(&mut tree, &secret).escalations.push(esc(
        &secret,
        EscalationReason::SecretExposure,
        1,
    ));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    let of = |key: &str| ranked.candidates.iter().find(|c| c.key == key).unwrap();
    assert_eq!(
        of(&style.site_key).reason,
        EscalationReason::RuleMisfit,
        "a convention rule's row carries no security reason"
    );
    assert_eq!(
        of(&style.site_key).slot,
        of(&secret.site_key).slot,
        "the two rows share the site's one slot"
    );
    assert_eq!(
        of(&secret.site_key).reason,
        EscalationReason::SecretExposure,
        "the security rule's own row keeps it"
    );
    assert!(of(&secret.site_key).escalated);
}

/// The reviewer's security reason survives on a convention rule's row that is the only row at its
/// site: the security rule list is words, and nothing says a row of the defect's own rule exists.
#[test]
fn a_lone_convention_rule_row_keeps_the_reviewers_security_reason() {
    let convention = "ARCH-SERVER-COMPUTED-AMOUNT-1";
    let rows = vec![row_at(convention, "app/api/checkout/route.ts", 50, "high")];
    let (mut tree, rows) = tree_of_rows(rows);
    let only = rows[0].clone();
    rule_node_of(&mut tree, &only).escalations.push(esc(
        &only,
        EscalationReason::AuthorizationBypass,
        1,
    ));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    assert_eq!(ranked.candidates.len(), 1);
    let c = &ranked.candidates[0];
    assert_eq!(c.reason, EscalationReason::AuthorizationBypass);
    assert!(c.escalated, "a real flaw stays above the line: {c:?}");
    assert_eq!(
        c.over_budget_reason,
        Some(EscalationReason::AuthorizationBypass)
    );
}

// ---- R-4: the reviewer's higher severity orders a row; the row keeps its own ----------------

fn disagree_on_severity(f: &mut Finding, stage1: &str, mine: &str) {
    f.review
        .stage2_disagreement
        .push(camerata_api_types::review::Stage2Disagreement {
            field: camerata_api_types::review::DisagreementField::Severity,
            stage1: stage1.to_string(),
            mine: mine.to_string(),
            why: "the reviewer reads it higher".to_string(),
        });
}

/// Lumen's 0007_storage.sql:6 shape: stage 1 medium, the reviewer said high. Beside a medium row
/// of the same reason that the reviewer ranked first, it now goes first, and its own severity
/// is still medium.
#[test]
fn a_row_the_reviewer_rates_higher_is_ordered_by_that_and_keeps_its_own_severity() {
    let mut rows = vec![
        row_at(RULE_A, "supabase/migrations/0007_storage.sql", 6, "medium"),
        row_at(RULE_A, "app/other.ts", 3, "medium"),
    ];
    disagree_on_severity(&mut rows[0], "medium", "high");
    let (mut tree, rows) = tree_of_rows(rows);
    let storage = row_of(&rows, RULE_A, "supabase/migrations/0007_storage.sql").clone();
    let other = row_of(&rows, RULE_A, "app/other.ts").clone();
    rule_node_of(&mut tree, &storage).escalations.push(esc(
        &storage,
        EscalationReason::CrossTenant,
        2,
    ));
    rule_node_of(&mut tree, &other)
        .escalations
        .push(esc(&other, EscalationReason::CrossTenant, 1));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(1));
    let first = &ranked.candidates[0];
    assert_eq!(
        first.key, storage.site_key,
        "the reviewer's high goes first"
    );
    assert!(first.escalated);
    assert_eq!(first.severity, "medium", "the row keeps its own severity");
    assert_eq!(first.stage2_severity.as_deref(), Some("high"));
    assert!(!ranked.candidates[1].escalated);
    // And the stored candidates re-mark the same way: the reviewer's high is eligible to pass
    // the budget as cross-tenant access at high.
    let mut again = ranked.candidates.clone();
    let mut budget = ranked.budget;
    budget.floor = 0;
    camerata_api_types::review::re_mark(
        &mut again,
        &mut budget,
        &EscalationReason::default_over_budget_set(),
    );
    assert!(again[0].escalated, "{:?}", again[0]);
    assert_eq!(
        again[0].over_budget_reason,
        Some(EscalationReason::CrossTenant)
    );
    assert!(!again[1].escalated);
}

/// The reviewer's value for a convention rule's row cannot be critical: the same cap stage 1 has.
#[test]
fn the_reviewers_severity_for_a_convention_rules_row_is_capped_below_critical() {
    let convention = "ARCH-SERVER-COMPUTED-AMOUNT-1";
    let mut rows = vec![row_at(convention, "app/api/checkout/route.ts", 50, "low")];
    disagree_on_severity(&mut rows[0], "low", "critical");
    let (mut tree, rows) = tree_of_rows(rows);
    let only = rows[0].clone();
    rule_node_of(&mut tree, &only)
        .escalations
        .push(esc(&only, EscalationReason::RuleMisfit, 1));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    assert_eq!(
        ranked.candidates[0].stage2_severity.as_deref(),
        Some("high")
    );
    assert_eq!(ranked.candidates[0].severity, "low");
}

/// The candidate keeps its row's own severity, so a low row of a severe reason does not take
/// its site over budget on a sibling's high.
#[test]
fn a_low_row_does_not_borrow_its_siblings_severity_to_pass_the_budget() {
    let rows = vec![
        row_at(RULE_A, "supabase/functions/hook.ts", 46, "low"),
        row_at(RULE_B, "supabase/functions/hook.ts", 46, "high"),
    ];
    let (mut tree, rows) = tree_of_rows(rows);
    let low = row_of(&rows, RULE_A, "supabase/functions/hook.ts").clone();
    let high = row_of(&rows, RULE_B, "supabase/functions/hook.ts").clone();
    rule_node_of(&mut tree, &low)
        .escalations
        .push(esc(&low, EscalationReason::DataLoss, 1));
    rule_node_of(&mut tree, &high)
        .escalations
        .push(esc(&high, EscalationReason::RuleMisfit, 1));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(0));
    let of = |key: &str| ranked.candidates.iter().find(|c| c.key == key).unwrap();
    assert_eq!(of(&low.site_key).severity, "low");
    assert!(
        !of(&low.site_key).escalated,
        "a low data-loss row is not data loss at high: {:?}",
        ranked.candidates
    );
}

// ---- a rule minimum above both reviewers is a mechanical candidate (R-5) -----------------------

const RULE_FLOOR: &str = RULE_C;

/// A row of `RULE_FLOOR` shown at `shown`, whose stage 1 rated `rated` (`None`: a record saved
/// before the rating was kept) and answered `real`.
fn floored_row(path: &str, shown: &str, real: &str, rated: Option<&str>) -> Finding {
    let mut f = row_at(RULE_FLOOR, path, 8, shown);
    let mut record = serde_json::json!({"real": real, "reason": "a hardening gap"});
    if let Some(rated) = rated {
        record["severity"] = serde_json::Value::String(rated.to_string());
    }
    f.captures.insert(
        crate::ai_audit::STAGE1_CAPTURE.to_string(),
        record.to_string(),
    );
    f
}

/// `n` rows of another rule at `shown`; [`nominate`] then has stage 2 name them.
fn nominated(rows: &mut Vec<Finding>, tag: &str, n: usize, shown: &str) {
    for i in 0..n {
        rows.push(row_at(RULE_A, &format!("src/{tag}{i}.rs"), 4, shown));
    }
}

fn nominate(
    tree: &mut ReviewTree,
    rows: &[Finding],
    tag: &str,
    n: usize,
    reason: EscalationReason,
) {
    for i in 0..n {
        let f = row_of(rows, RULE_A, &format!("src/{tag}{i}.rs")).clone();
        rule_node_of(tree, &f)
            .escalations
            .push(esc(&f, reason, i as u32 + 1));
    }
}

fn candidate_of<'a>(
    ranked: &'a Ranked,
    rows: &[Finding],
    path: &str,
) -> Option<&'a EscalationCandidate> {
    let key = &row_of(rows, RULE_FLOOR, path).site_key;
    ranked.candidates.iter().find(|c| &c.key == key)
}

#[test]
fn a_rule_minimum_over_both_reviewers_lands_above_the_line_against_mediums() {
    let mut rows = vec![floored_row(
        "supabase/0006.sql",
        "high",
        "yes",
        Some("medium"),
    )];
    nominated(&mut rows, "med", 3, "medium");
    let (mut tree, rows) = tree_of_rows(rows);
    nominate(&mut tree, &rows, "med", 3, EscalationReason::RuleMisfit);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(2));
    let found = candidate_of(&ranked, &rows, "supabase/0006.sql");
    assert!(found.is_some(), "the floor row is a candidate");
    let c = found.unwrap();
    assert_eq!(c.reason, EscalationReason::ConflictingVerdicts);
    assert!(c.escalated, "above the line: {c:?}");
    assert_eq!(c.slot, 1, "a high site sorts before every medium one");
    assert_eq!(
        ranked.candidates.iter().filter(|c| c.escalated).count(),
        2,
        "the budget of two slots is the floor row and the first medium"
    );
}

#[test]
fn a_row_stage_1_called_not_real_is_not_a_mechanical_candidate() {
    let rows = vec![floored_row("a.sql", "high", "no", Some("medium"))];
    let (tree, rows) = tree_of_rows(rows);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(5));
    assert!(candidate_of(&ranked, &rows, "a.sql").is_none());
    assert!(ranked.candidates.is_empty());
}

#[test]
fn a_row_shown_at_the_severity_stage_1_rated_is_not_a_mechanical_candidate() {
    let rows = vec![
        floored_row("a.sql", "high", "yes", Some("high")),
        floored_row("b.sql", "critical", "yes", Some("critical")),
        floored_row("c.sql", "medium", "yes", Some("low")),
    ];
    let (tree, rows) = tree_of_rows(rows);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(5));
    assert!(
        ranked.candidates.is_empty(),
        "equal severity, and a raise to medium, are not candidates: {:?}",
        ranked.candidates
    );
}

#[test]
fn a_saved_run_without_stage_1_severity_yields_no_mechanical_candidate() {
    let rows = vec![floored_row("old.sql", "high", "yes", None)];
    let (tree, rows) = tree_of_rows(rows);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(5));
    assert!(
        ranked.candidates.is_empty(),
        "no guess from a missing rating"
    );
}

#[test]
fn a_mechanical_candidate_never_goes_over_budget_or_displaces_a_reasoned_high() {
    let mut rows = vec![floored_row(
        "supabase/0006.sql",
        "high",
        "yes",
        Some("medium"),
    )];
    nominated(&mut rows, "hi", 2, "high");
    let (mut tree, rows) = tree_of_rows(rows);
    // The weakest reasons in the list: the mechanical row still sorts after them.
    nominate(&mut tree, &rows, "hi", 2, EscalationReason::NeedsMoreCode);
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(2));
    let found = candidate_of(&ranked, &rows, "supabase/0006.sql");
    assert!(found.is_some(), "the floor row is a candidate");
    let c = found.unwrap();
    assert!(!c.escalated, "the budget is full of reasoned highs: {c:?}");
    assert_eq!(c.over_budget_reason, None);
    assert_eq!(c.slot, 3);
    assert_eq!(ranked.budget.escalated, 2);
    assert_eq!(ranked.budget.over_budget, 0);
}

#[test]
fn a_site_stage_2_nominated_is_not_added_again() {
    let rows = vec![floored_row("a.sql", "high", "yes", Some("medium"))];
    let (mut tree, rows) = tree_of_rows(rows);
    let f = rows[0].clone();
    rule_node_of(&mut tree, &f)
        .escalations
        .push(esc(&f, EscalationReason::OutlierInGroup, 1));
    let ranked = rank_candidates(&tree.nodes, &rows, &settings(5));
    assert_eq!(ranked.candidates.len(), 1);
    assert_eq!(
        ranked.candidates[0].reason,
        EscalationReason::OutlierInGroup
    );
}
