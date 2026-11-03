//! Design section 8: stage 1's outlier capture and M4's review flags become outlier reasons in
//! the tree, marked by the engine. They never escalate a site on their own.

use std::collections::BTreeSet;

use super::*;
use crate::inspections::backfill_site_keys;
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::onboard::Finding;

fn row(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("query({path}:{line})"),
        detail: "detail".to_string(),
        ..Finding::default()
    }
}

fn with_flags(mut f: Finding, flags: &[&str]) -> Finding {
    f.captures.insert(
        crate::review_flags::REVIEW_FLAGS_CAPTURE.to_string(),
        serde_json::to_string(&flags).unwrap(),
    );
    f
}

fn tree_of(rows: &mut Vec<Finding>) -> ReviewTree {
    backfill_site_keys(rows);
    build_tree(rows, &mut [], None, &ToolMap::builtin(), "run-1")
}

fn outliers_of<'a>(tree: &'a ReviewTree, key: &str) -> BTreeSet<(OutlierReason, OutlierBy)> {
    tree.nodes
        .iter()
        .flat_map(|n| n.outliers.iter())
        .filter(|o| o.key == key)
        .map(|o| (o.reason, o.by))
        .collect()
}

#[test]
fn each_review_flag_becomes_its_outlier_reason_by_the_engine() {
    let mut rows = vec![
        with_flags(
            row("src/a.rs", 1),
            &["precondition_added", "unverified_name:src/gone.rs"],
        ),
        with_flags(row("src/b.rs", 2), &["fix_ungrounded", "contradiction"]),
        with_flags(row("src/c.rs", 3), &["methodology_leak"]),
        row("src/d.rs", 4),
    ];
    let tree = tree_of(&mut rows);
    let want = |reasons: &[OutlierReason]| -> BTreeSet<(OutlierReason, OutlierBy)> {
        reasons.iter().map(|r| (*r, OutlierBy::Engine)).collect()
    };
    assert_eq!(
        outliers_of(&tree, &rows[0].site_key),
        want(&[
            OutlierReason::PreconditionAdded,
            OutlierReason::UnverifiedName
        ])
    );
    assert_eq!(
        outliers_of(&tree, &rows[1].site_key),
        want(&[OutlierReason::FixUngrounded, OutlierReason::Contradiction])
    );
    assert_eq!(
        outliers_of(&tree, &rows[2].site_key),
        want(&[OutlierReason::MethodologyLeak])
    );
    assert!(outliers_of(&tree, &rows[3].site_key).is_empty());
}

#[test]
fn stage_one_severity_raise_held_and_unverified_evidence_become_outliers() {
    let mut held = row("src/a.rs", 1);
    held.captures.insert(
        crate::ai_audit::OUTLIER_REASONS_CAPTURE.to_string(),
        "severity_raise_held".to_string(),
    );
    let mut unverified = row("src/b.rs", 2);
    unverified.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the quoted code is not in the file".to_string(),
    );
    let mut rows = vec![held, unverified, row("src/c.rs", 3)];
    let tree = tree_of(&mut rows);
    assert_eq!(
        outliers_of(&tree, &rows[0].site_key),
        BTreeSet::from([(OutlierReason::SeverityRaiseHeld, OutlierBy::Engine)])
    );
    assert_eq!(
        outliers_of(&tree, &rows[1].site_key),
        BTreeSet::from([(OutlierReason::EvidenceUnverified, OutlierBy::Engine)])
    );
}

#[test]
fn a_flag_that_was_cleared_is_no_longer_an_outlier_on_a_rebuild() {
    let mut rows = vec![with_flags(row("src/a.rs", 1), &["contradiction"])];
    let tree = tree_of(&mut rows);
    assert_eq!(outliers_of(&tree, &rows[0].site_key).len(), 1);
    rows[0]
        .captures
        .remove(crate::review_flags::REVIEW_FLAGS_CAPTURE);
    let again = build_tree(&mut rows, &mut [], Some(tree), &ToolMap::builtin(), "run-1");
    assert!(outliers_of(&again, &rows[0].site_key).is_empty());
}

#[test]
fn an_outlier_mark_alone_makes_no_escalation_candidate() {
    let mut rows = vec![
        with_flags(row("src/a.rs", 1), &["contradiction", "methodology_leak"]),
        with_flags(row("src/b.rs", 2), &["fix_ungrounded"]),
    ];
    let tree = tree_of(&mut rows);
    assert!(tree.candidates.is_empty());
    assert!(tree.nodes.iter().all(|n| n.escalations.is_empty()));
    let ranked = rank_candidates(&tree.nodes, &rows, &RankSettings::default());
    assert!(
        ranked.candidates.is_empty(),
        "outliers never escalate alone: {:?}",
        ranked.candidates
    );
}
