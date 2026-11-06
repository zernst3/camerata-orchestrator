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
    assert_eq!(new_node.cause.as_deref(), Some("Test fixtures"));
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

#[test]
fn a_reviewer_added_finding_whose_key_a_rerun_takes_is_reported_superseded_not_dropped() {
    let ctx = EditCtx {
        run_id: "insp-1",
        now: NOW,
    };
    let req = camerata_api_types::edits::AddFindingReq {
        repo: "acme/app".into(),
        path: "src/c.rs".into(),
        line: 30,
        rule_id: "SEC-2".into(),
        severity: "high".into(),
        snippet: "let c = other_c();".into(),
        headline: "REVIEWER-HEADLINE".into(),
        detail: "REVIEWER-DETAIL the reviewer's own explanation.".into(),
        fix: "REVIEWER-FIX do this instead.".into(),
        kind: Some(FindingKind::Defect),
        category: None,
        reason: "the scan missed it".into(),
        by: "zach".into(),
    };
    // The rerun's engine row is at the same place with the same rule and snippet, so it has the
    // very key the reviewer's finding was given.
    let sealed = three();
    let files = vec![(
        "src/c.rs".to_string(),
        "\n".repeat(29) + "let c = other_c();\n",
    )];
    let (mut added, _) = added_finding_from_request(&req, &[], &files, None, ctx).unwrap();
    let engine_key = at(&sealed, "src/c.rs").site_key;
    added.finding.site_key = engine_key.clone();

    let reviewed = apply_overlay(&sealed, &[], &[], std::slice::from_ref(&added));
    assert_eq!(
        reviewed.report().findings.len(),
        3,
        "the engine's row is shown"
    );
    let notes = reviewed.superseded();
    assert_eq!(
        notes.len(),
        1,
        "the reviewer's finding is reported, not dropped"
    );
    assert_eq!(notes[0].engine_key, engine_key);
    assert_eq!(notes[0].added.reason, "the scan missed it");
    assert!(notes[0].added.finding.detail.contains("REVIEWER-DETAIL"));
    assert_eq!(
        notes[0].added.finding.fix_specific.as_deref(),
        Some("REVIEWER-FIX do this instead.")
    );
    // Never applied silently: the engine's row carries none of the reviewer's text.
    let shown = at(reviewed.report(), "src/c.rs");
    assert!(!shown.detail.contains("REVIEWER-DETAIL"));
    assert!(!is_reviewer_edited(&shown));

    // The same finding added twice is a duplicate, not a supersession.
    let fresh = three();
    let (own, _) = added_finding_from_request(&req, &[], &files, None, ctx).unwrap();
    let mut own = own;
    own.finding.site_key = "acme/app\u{0}src/own.rs\u{0}ff\u{0}0".to_string();
    let twice = apply_overlay(&fresh, &[], &[], &[own.clone(), own]);
    assert!(twice.superseded().is_empty());
}

#[test]
fn a_split_reason_never_becomes_the_subgroup_cause() {
    let sealed = sealed(vec![
        row("SEC-1", "src/a.rs", 10, "let a = secret_a();"),
        row("SEC-1", "src/b.rs", 20, "let b = secret_b();"),
    ]);
    let b = at(&sealed, "src/b.rs");
    let mut split = edit(1, &b, EditField::Subgroup, "", "Test fixtures");
    split.reason = "SECRET-SPLIT-REASON typed by the reviewer".to_string();
    let reviewed = apply_overlay(&sealed, &[], &[split], &[]);
    let tree = reviewed.report().tree.as_ref().unwrap();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.label == "Test fixtures")
        .unwrap();
    assert_eq!(node.cause.as_deref(), Some("Test fixtures"));
    let json = serde_json::to_string(tree).unwrap();
    assert!(!json.contains("SECRET-SPLIT-REASON"), "{json}");
}

#[test]
fn a_bound_note_is_not_printed_beside_a_severity_the_reviewer_set() {
    let sealed = three();
    let a = at(&sealed, "src/a.rs");
    let mut with_note = a.clone();
    with_note.captures.insert(
        crate::severity_bounds::SEVERITY_ADJUSTMENTS_CAPTURE.to_string(),
        "Severity lowered from high to info: no file or line was identified".to_string(),
    );
    assert!(
        !crate::severity_bounds::severity_adjustment_lines(&with_note, None).is_empty(),
        "an engine-set severity keeps its note"
    );
    let mut e = edit(1, &a, EditField::Severity, "high", "critical");
    e.reason = "reviewed by hand".to_string();
    let mut sealed_with_note = sealed.as_scan_report().clone();
    for f in &mut sealed_with_note.findings {
        if f.site_key == a.site_key {
            f.captures.extend(
                with_note
                    .captures
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
    }
    let reviewed = apply_overlay(&sealed_with_note.seal(), &[], &[e], &[]);
    let edited = at(reviewed.report(), "src/a.rs");
    assert_eq!(edited.severity, "critical");
    assert!(
        crate::severity_bounds::severity_adjustment_lines(&edited, None).is_empty(),
        "no bound note beside the reviewer's severity"
    );
}

// ── node summary edits (review UI spec U4) ────────────────────────────────────────────────

/// `three()` with a model summary on every node, so a node's edit has something to replace.
fn three_with_summaries(model: &str) -> SealedReport {
    three().overlaid(|r| {
        if let Some(tree) = r.tree.as_mut() {
            for n in &mut tree.nodes {
                n.summary = Some(format!("{model} ({})", n.id));
            }
        }
    })
}

/// A summary edit of `node_id`, made against the model summary the node has in `sealed`.
fn summary_edit_of(n: usize, sealed: &SealedReport, node_id: &str, new: &str) -> Edit {
    let node = sealed
        .tree
        .as_ref()
        .and_then(|t| t.node(node_id))
        .unwrap_or_else(|| panic!("no node {node_id}"));
    let model = node.summary.clone().unwrap_or_default();
    Edit {
        id: format!("edit-{n:06}"),
        key: node_id.to_string(),
        field: EditField::Summary,
        old: model.clone(),
        new: new.to_string(),
        reason: "SECRET-REASON the model missed the shared helper".to_string(),
        by: "SECRET-REVIEWER".to_string(),
        at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: super::summary_fingerprint(&model),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

#[test]
fn a_summary_edit_replaces_the_model_summary_and_marks_it_the_reviewers() {
    let sealed = three_with_summaries("The model says");
    let before = serde_json::to_string(&sealed).unwrap();
    let edit = summary_edit_of(
        1,
        &sealed,
        "rule:SEC-1",
        "Both sites read the secret from the environment; fixed by one helper.",
    );
    let reviewed = apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);

    let tree = reviewed.report().tree.as_ref().unwrap();
    let node = tree.node("rule:SEC-1").unwrap();
    assert_eq!(node.summary.as_deref(), Some(edit.new.as_str()));
    assert_eq!(node.summary_source, TextSource::Reviewer);
    assert!(
        reviewed
            .applied_edits()
            .iter()
            .any(|a| a.key == "rule:SEC-1" && a.field == EditField::Summary),
        "{:?}",
        reviewed.applied_edits()
    );
    assert!(reviewed.stale_edits().is_empty());
    // Another node keeps the model's summary, and the sealed report is untouched.
    let other = tree.node("rule:SEC-2").unwrap();
    assert_eq!(other.summary_source, TextSource::Model);
    assert_eq!(serde_json::to_string(&sealed).unwrap(), before);
}

#[test]
fn a_summary_edit_goes_stale_when_the_model_summary_changes() {
    let first_run = three_with_summaries("The model says");
    let edit = summary_edit_of(1, &first_run, "rule:SEC-1", "The reviewer's reading.");
    // A rerun: the model wrote something else for the same node.
    let rerun = three_with_summaries("After a rerun the model says");
    let reviewed = apply_overlay(&rerun, &[], std::slice::from_ref(&edit), &[]);

    let node = reviewed
        .report()
        .tree
        .as_ref()
        .unwrap()
        .node("rule:SEC-1")
        .unwrap();
    assert_eq!(
        node.summary.as_deref(),
        Some("After a rerun the model says (rule:SEC-1)"),
        "a stale edit applies nothing"
    );
    assert_eq!(node.summary_source, TextSource::Model);
    let stale = reviewed.stale_edits();
    assert_eq!(stale.len(), 1, "{stale:?}");
    assert_eq!(stale[0].edit.key, "rule:SEC-1");
    assert!(stale[0].edit.stale);
    assert_eq!(
        stale[0].current_value,
        "After a rerun the model says (rule:SEC-1)"
    );
    assert!(reviewed.applied_edits().is_empty());
}

#[test]
fn a_summary_edit_reaches_the_client_tree_summary_and_the_internal_source() {
    let sealed = three_with_summaries("The model says");
    let edit = summary_edit_of(
        1,
        &sealed,
        "rule:SEC-1",
        "Both sites read the secret from the environment.",
    );
    let reviewed = apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);

    let client = crate::report_export::tree_view::tree_view(&reviewed, false);
    let node = client.iter().find(|n| n.id == "rule:SEC-1").unwrap();
    assert_eq!(node.summary, edit.new);
    let internal = crate::report_export::tree_view::tree_view(&reviewed, true);
    let node = internal.iter().find(|n| n.id == "rule:SEC-1").unwrap();
    assert_eq!(
        node.internal.as_ref().unwrap().summary_source,
        "reviewer",
        "who wrote it stays internal"
    );
    // The reviewer's reason and name reach no client output.
    let client_json = serde_json::to_string(&client).unwrap();
    assert!(!client_json.contains("SECRET-REASON"));
    assert!(!client_json.contains("SECRET-REVIEWER"));
}
