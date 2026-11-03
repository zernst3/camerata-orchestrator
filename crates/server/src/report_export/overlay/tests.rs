//! The overlay: edits, stale edits, added findings, merges and splits (design 5.5).

use camerata_api_types::audit::FindingKind;
use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use camerata_api_types::edits::{Edit, EditField};

use super::*;
use crate::edits::{added_finding_from_request, EditCtx};
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::onboard::{Finding, ScanReport};
use crate::release_gate::evidence::unverified_without_decision;

const NOW: &str = "2026-11-04T09:00:00Z";

fn row(rule_id: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        detail: format!("Title for {rule_id} \u{2014} The handler trusts the caller here."),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

fn sealed(rows: Vec<Finding>) -> SealedReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.findings = rows;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:00:00Z".to_string();
    report.seal()
}

fn three() -> SealedReport {
    sealed(vec![
        row("SEC-1", "src/a.rs", 10, "let a = secret_a();"),
        row("SEC-1", "src/b.rs", 20, "let b = secret_b();"),
        row("SEC-2", "src/c.rs", 30, "let c = other_c();"),
    ])
}

fn at(sealed: &SealedReport, path: &str) -> Finding {
    sealed
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .clone()
}

fn edit(n: usize, f: &Finding, field: EditField, old: &str, new: &str) -> Edit {
    Edit {
        id: format!("edit-{n:06}"),
        key: f.site_key.clone(),
        field,
        old: old.to_string(),
        new: new.to_string(),
        reason: "SECRET-REASON the engine was wrong".to_string(),
        by: "SECRET-REVIEWER".to_string(),
        at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

fn decision(id: &str, key: &str, state: DecisionState, reason: &str) -> Decision {
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "zach".to_string(),
        decided_at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

#[test]
fn edit_is_applied_after_the_seal_and_the_sealed_report_is_unchanged() {
    let sealed = three();
    let before = serde_json::to_string(&sealed).unwrap();
    let a = at(&sealed, "src/a.rs");
    let edits = vec![
        edit(1, &a, EditField::Severity, "high", "low"),
        edit(
            2,
            &a,
            EditField::Headline,
            "Title for SEC-1",
            "A clearer title",
        ),
        edit(3, &a, EditField::Detail, "", "A clearer explanation."),
        edit(
            4,
            &a,
            EditField::Fix,
            "",
            "Use the `safe_read` helper in src/lib.rs.",
        ),
        edit(5, &a, EditField::Category, "", "secret-exposure"),
        edit(6, &a, EditField::Kind, "", "{\"kind\":\"defect\"}"),
    ];
    let reviewed = apply_overlay(&sealed, &[], &edits, &[]);

    let got = at(reviewed.report(), "src/a.rs");
    assert_eq!(got.severity, "low");
    assert_eq!(crate::ai_audit::finding_headline(&got), "A clearer title");
    assert!(
        got.detail.ends_with("A clearer explanation."),
        "{}",
        got.detail
    );
    assert_eq!(
        got.fix_specific.as_deref(),
        Some("Use the `safe_read` helper in src/lib.rs.")
    );
    assert_eq!(got.category.as_deref(), Some("secret-exposure"));
    assert!(is_reviewer_edited(&got));
    assert_eq!(
        edit_ids_of(&got),
        [
            "edit-000001",
            "edit-000002",
            "edit-000003",
            "edit-000004",
            "edit-000005",
            "edit-000006"
        ]
    );
    assert_eq!(reviewed.applied_edits().len(), 6);
    assert!(reviewed.stale_edits().is_empty());

    // Rows nobody edited carry no trace of the overlay.
    // (The category edit moves the rule's category vote, so only b's place in the rebuilt tree
    // may differ.)
    let mut b = at(reviewed.report(), "src/b.rs");
    let mut b_sealed = at(&sealed, "src/b.rs");
    for row in [&mut b, &mut b_sealed] {
        row.group_path.clear();
        row.group_role = crate::review_tree::GroupRole::Primary;
    }
    assert_eq!(b, b_sealed);
    assert!(!is_reviewer_edited(&b));

    // The sealed report is exactly what it was.
    assert_eq!(serde_json::to_string(&sealed).unwrap(), before);
}

#[test]
fn edit_with_a_different_fingerprint_loads_as_stale_and_is_not_applied() {
    let sealed = three();
    let a = at(&sealed, "src/a.rs");
    // The code at the site changed since the edit: the fingerprint it was made against differs.
    let mut changed = edit(1, &a, EditField::Severity, "high", "low");
    changed.fingerprint_at_edit = "0000000000000000".to_string();
    // The site is gone altogether.
    let mut gone = edit(2, &a, EditField::Headline, "Title for SEC-1", "Gone");
    gone.key = "acme/app\u{0}src/removed.rs\u{0}deadbeef\u{0}0".to_string();
    let fine = edit(3, &a, EditField::Detail, "", "Still applies.");
    let reviewed = apply_overlay(&sealed, &[], &[changed.clone(), gone, fine], &[]);

    let got = at(reviewed.report(), "src/a.rs");
    assert_eq!(got.severity, "high", "a stale edit is not applied");
    assert!(
        got.detail.ends_with("Still applies."),
        "an edit that matches is"
    );
    assert_eq!(edit_ids_of(&got), ["edit-000003"]);

    let stale = reviewed.stale_edits();
    assert_eq!(stale.len(), 2, "{stale:?}");
    assert!(stale.iter().all(|s| s.edit.stale));
    // Shown with the value it was made against, and what the site says now.
    let first = stale.iter().find(|s| s.edit.id == "edit-000001").unwrap();
    assert_eq!(first.edit.old, "high");
    assert_eq!(first.current_value, "high");
    assert_eq!(reviewed.applied_edits().len(), 1);
}

#[test]
fn a_discarded_edit_is_not_applied() {
    let sealed = three();
    let a = at(&sealed, "src/a.rs");
    let mut e = edit(1, &a, EditField::Severity, "high", "low");
    e.discarded_at = Some(NOW.to_string());
    let reviewed = apply_overlay(&sealed, &[], &[e], &[]);
    assert_eq!(at(reviewed.report(), "src/a.rs").severity, "high");
    assert!(
        reviewed.stale_edits().is_empty(),
        "a discarded edit is not stale either"
    );
}

#[test]
fn a_rule_edit_moves_the_row_to_the_new_rule_in_the_rebuilt_tree() {
    let sealed = three();
    let c = at(&sealed, "src/c.rs");
    let reviewed = apply_overlay(
        &sealed,
        &[],
        &[edit(1, &c, EditField::RuleId, "SEC-2", "SEC-1")],
        &[],
    );
    let tree = reviewed.report().tree.as_ref().unwrap();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&c.site_key))
        .expect("the row is in a sub-group");
    assert!(
        node.parent.as_deref().is_some_and(|p| p.contains("SEC-1")),
        "{node:?}"
    );
    // No row lost or added by the rebuild.
    let total: usize = tree
        .nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Subgroup)
        .map(|n| n.members.len())
        .sum();
    assert_eq!(total, 3);
}

#[test]
fn an_added_finding_joins_the_rows_the_tree_and_is_marked_reviewer() {
    let sealed = three();
    let ctx = EditCtx {
        run_id: "insp-1",
        now: NOW,
    };
    let req = camerata_api_types::edits::AddFindingReq {
        repo: "acme/app".into(),
        path: "src/new.rs".into(),
        line: 2,
        rule_id: "SEC-1".into(),
        severity: "high".into(),
        snippet: "danger(input);".into(),
        headline: "Input reaches danger".into(),
        detail: "The handler passes the request straight to danger().".into(),
        fix: "Validate the input first.".into(),
        kind: Some(FindingKind::Defect),
        category: None,
        reason: "the scan did not look at this file".into(),
        by: "zach".into(),
    };
    let files = vec![(
        "src/new.rs".to_string(),
        "fn go() {\n    danger(input);\n}\n".to_string(),
    )];
    let (added, evidence) =
        added_finding_from_request(&req, &sealed.findings, &files, None, ctx).unwrap();
    assert!(evidence.verified, "{evidence:?}");
    assert!(!added.finding.site_key.is_empty());

    let reviewed = apply_overlay(&sealed, &[], &[], std::slice::from_ref(&added));
    assert_eq!(reviewed.report().findings.len(), 4);
    let got = at(reviewed.report(), "src/new.rs");
    assert_eq!(
        got.captures.get(PROVENANCE_CAPTURE).map(String::as_str),
        Some(REVIEWER_PROVENANCE)
    );
    assert!(got.kind.is_some());
    // It joins the tree like any row.
    let tree = reviewed.report().tree.as_ref().unwrap();
    assert!(tree.nodes.iter().any(|n| n.members.contains(&got.site_key)));
    // The sealed report did not gain it.
    assert_eq!(sealed.findings.len(), 3);

    // Adding the same finding twice adds one row.
    let twice = apply_overlay(&sealed, &[], &[], &[added.clone(), added]);
    assert_eq!(twice.report().findings.len(), 4);
}

#[test]
fn added_finding_fails_the_evidence_gate_when_its_snippet_is_not_at_the_line() {
    let sealed = three();
    let ctx = EditCtx {
        run_id: "insp-1",
        now: NOW,
    };
    let req = camerata_api_types::edits::AddFindingReq {
        repo: "acme/app".into(),
        path: "src/new.rs".into(),
        line: 2,
        rule_id: "SEC-1".into(),
        severity: "high".into(),
        snippet: "danger(input);".into(),
        headline: "Input reaches danger".into(),
        detail: "The handler passes the request straight to danger().".into(),
        fix: String::new(),
        kind: Some(FindingKind::Defect),
        category: None,
        reason: "the scan did not look at this file".into(),
        by: "zach".into(),
    };
    // The file has no such line anywhere near line 2.
    let files = vec![(
        "src/new.rs".to_string(),
        "fn go() {\n    safe();\n}\n".to_string(),
    )];
    let (added, evidence) =
        added_finding_from_request(&req, &sealed.findings, &files, None, ctx).unwrap();
    assert!(!evidence.verified);
    assert!(!evidence.note.is_empty());
    assert!(added
        .finding
        .captures
        .contains_key(EVIDENCE_UNVERIFIED_CAPTURE));

    let reviewed = apply_overlay(&sealed, &[], &[], std::slice::from_ref(&added));
    let rows = &reviewed.report().findings;
    let problems = unverified_without_decision(rows, &reviewed);
    assert_eq!(problems.len(), 1, "the gate refuses it: {problems:?}");

    // The reviewer decides it: the gate lets it through.
    let key = at(reviewed.report(), "src/new.rs").site_key;
    let decided = apply_overlay(
        &sealed,
        &[decision("dec-000001", &key, DecisionState::Confirmed, "")],
        &[],
        &[added],
    );
    assert!(unverified_without_decision(&decided.report().findings, &decided).is_empty());
}

#[test]
fn reviewer_merge_keeps_the_merged_row_in_all_findings_with_a_pointer() {
    let sealed = three();
    let a = at(&sealed, "src/a.rs");
    let b = at(&sealed, "src/b.rs");
    let merge = decision(
        "dec-000001",
        &b.site_key,
        DecisionState::MergedInto(a.site_key.clone()),
        "same unsafe read, one fix",
    );
    let reviewed = apply_overlay(&sealed, std::slice::from_ref(&merge), &[], &[]);

    // No row is deleted.
    assert_eq!(reviewed.report().findings.len(), 3);
    let merged = at(reviewed.report(), "src/b.rs");
    assert_eq!(merged_label(&merged), Some("Merged into src/a.rs:10"));
    assert_eq!(merged.captures.get(MERGED_INTO_CAPTURE), Some(&a.site_key));
    // The target is not marked.
    assert!(merged_label(&at(reviewed.report(), "src/a.rs")).is_none());
    assert_eq!(reviewed.merges().len(), 1);
    assert_eq!(reviewed.merges()[0].reason, "same unsafe read, one fix");

    // A merge counts as the reviewer's decision for the gate.
    use crate::release_gate::evidence::ReviewerDecisions;
    assert!(reviewed.has_decision(&b));
    assert!(!reviewed.has_decision(&a));

    // A revoked merge points at nothing.
    let mut revoked = merge;
    revoked.revoked_at = Some(NOW.to_string());
    let undone = apply_overlay(&sealed, &[revoked], &[], &[]);
    assert!(merged_label(&at(undone.report(), "src/b.rs")).is_none());
}

#[test]
fn split_moves_the_key_and_the_tree_rebuilds_without_losing_a_member() {
    let sealed = sealed(vec![
        row("SEC-1", "src/a.rs", 10, "let a = secret_a();"),
        row("SEC-1", "src/b.rs", 20, "let b = secret_b();"),
        row("SEC-1", "src/c.rs", 30, "let c = secret_c();"),
        row("SEC-2", "src/d.rs", 40, "let d = other_d();"),
    ]);
    let members = |tree: &ReviewTree| -> Vec<String> {
        let mut all: Vec<String> = tree
            .nodes
            .iter()
            .filter(|n| n.level == NodeLevel::Subgroup)
            .flat_map(|n| n.members.clone())
            .collect();
        all.sort();
        all
    };
    let before_tree = sealed.tree.clone().expect("a sealed report has a tree");
    let before = members(&before_tree);
    assert_eq!(before.len(), 4);

    let b = at(&sealed, "src/b.rs");
    let from = before_tree
        .nodes
        .iter()
        .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&b.site_key))
        .unwrap()
        .clone();
    let mut split = edit(1, &b, EditField::Subgroup, &from.id, "Test fixtures");
    split.reason = "these live in test code".to_string();
    let reviewed = apply_overlay(&sealed, &[], &[split], &[]);

    let tree = reviewed.report().tree.as_ref().unwrap();
    assert_eq!(members(tree), before, "no site lost or added");
    let new_node = tree
        .nodes
        .iter()
        .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&b.site_key))
        .unwrap();
    assert_eq!(new_node.label, "Test fixtures");
    assert_ne!(new_node.id, from.id, "the key moved");
    assert_eq!(new_node.members, vec![b.site_key.clone()]);
    assert_eq!(new_node.cause.as_deref(), Some("these live in test code"));
    assert_eq!(new_node.parent, from.parent, "it stays under the same rule");
    if let Some(old) = tree.nodes.iter().find(|n| n.id == from.id) {
        assert!(!old.members.contains(&b.site_key));
    }
    // The row's own place follows.
    assert_eq!(
        at(reviewed.report(), "src/b.rs").group_path.get(2),
        Some(&new_node.id)
    );
    // Splitting a second site into the same sub-group joins it.
    let c = at(&sealed, "src/c.rs");
    let mut second = edit(2, &c, EditField::Subgroup, &from.id, "Test fixtures");
    second.reason = "same".to_string();
    let mut first = edit(1, &b, EditField::Subgroup, &from.id, "Test fixtures");
    first.reason = "same".to_string();
    let both = apply_overlay(&sealed, &[], &[first, second], &[]);
    let tree = both.report().tree.as_ref().unwrap();
    assert_eq!(members(tree), before);
    let group = tree
        .nodes
        .iter()
        .find(|n| n.label == "Test fixtures")
        .unwrap();
    assert_eq!(group.members.len(), 2);
}

#[test]
fn an_overlay_of_nothing_leaves_the_report_as_it_was() {
    let sealed = three();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    assert_eq!(
        serde_json::to_string(reviewed.report().as_scan_report()).unwrap(),
        serde_json::to_string(sealed.as_scan_report()).unwrap()
    );
}

#[test]
fn a_restored_thrown_out_row_returns_marked_and_held() {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.findings = vec![row("SEC-1", "src/a.rs", 10, "let a = secret_a();")];
    let mut thrown = row("AI-NOTE", "src/note.rs", 4, "db.query(q)");
    crate::engine_discard::mark(&mut thrown, crate::engine_discard::DISCARD_VERDICT);
    report.discarded = vec![thrown];
    report.ledger.set_discarded(&report.discarded);
    let sealed = report.seal();
    let key = sealed.discarded[0].site_key.clone();

    let restore = decision(
        "dec-000001",
        &key,
        DecisionState::EngineRestored,
        "a real problem",
    );
    let reviewed = apply_overlay(&sealed, &[restore], &[], &[]);
    assert!(reviewed.report().discarded.is_empty());
    let back = at(reviewed.report(), "src/note.rs");
    assert!(!crate::engine_discard::is_discarded(&back));
    assert_eq!(
        back.captures
            .get(crate::engine_discard::ENGINE_RESTORED_CAPTURE)
            .map(String::as_str),
        Some(crate::engine_discard::DISCARD_VERDICT)
    );
    assert!(reviewed.report().discarded_reconciles());
    // It is in the tree again.
    let tree = reviewed.report().tree.as_ref().unwrap();
    assert!(tree.nodes.iter().any(|n| n.members.contains(&key)));
    // The sealed report still holds it as thrown out.
    assert_eq!(sealed.discarded.len(), 1);
}
