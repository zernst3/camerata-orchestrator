//! Owner ruling R-7: no floor raises a row a reviewer doubted. Synthetic shapes of the cases
//! observed in the graded runs; every rule id, path and text here is made up.

use super::*;
use crate::severity_bounds::{apply_scan_severity_bounds, recorded_adjustments};
use camerata_api_types::audit::FindingKind;
use camerata_api_types::review::Stage2Disagreement;

fn corpus(rules: &[(&str, Option<&str>)]) -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    for (id, min) in rules {
        let mut rule = camerata_rules::bare_rule(id, "test-fixture");
        rule.min_severity = min.map(str::to_string);
        set.push(rule);
    }
    set
}

/// A located defect row of `rule` at `severity`, with stage 1's verdict recorded the way
/// calibration records it (the severity stage 1 left on the row).
fn reviewed_row(rule: &str, severity: &str, real: &str) -> Finding {
    let mut f = Finding {
        repo: "acme/app".to_string(),
        path: format!("src/{}.ts", rule.to_ascii_lowercase()),
        line: 12,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: "return res.status(401).end();".to_string(),
        detail: "Synthetic detail.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    };
    f.captures.insert(
        crate::ai_audit::STAGE1_CAPTURE.to_string(),
        serde_json::json!({ "real": real, "reason": "synthetic", "severity": severity })
            .to_string(),
    );
    f
}

fn notes(f: &Finding) -> String {
    recorded_adjustments(f).join(" | ")
}

// ── The corpus floor (`min_severity`) ───────────────────────────────────────────────────────

/// The webhook shape: a correct signature check whose row says "returns 401 instead of 400".
/// Stage 1 reported it at low and judged it not real; the rule's floor is critical.
#[test]
fn a_rule_floor_does_not_raise_a_row_stage_one_judged_not_real_and_holds_it() {
    let set = corpus(&[("SYN-WEBHOOK-SIGNATURE-1", Some("critical"))]);
    let mut rows = vec![reviewed_row("SYN-WEBHOOK-SIGNATURE-1", "low", "no")];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    assert_eq!(rows.len(), 1, "nothing is dropped");
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(
        rows[0].needs_review,
        "the doubted row is held for a decision"
    );
    let why = notes(&rows[0]);
    assert!(why.contains("severity held at low"), "{why}");
    assert!(why.contains("not real"), "{why}");
}

/// The billing cascade shape: calibration said not real AND the group reviewer said likely false
/// alarm; the rule floor is high. The export-side backstop must not raise it either.
#[test]
fn a_rule_floor_does_not_raise_a_row_both_reviewers_doubted_at_the_export_backstop_either() {
    let set = corpus(&[("SYN-BILLING-CASCADE-1", Some("high"))]);
    let mut row = reviewed_row("SYN-BILLING-CASCADE-1", "low", "no");
    row.review.escalation_reason = Some(EscalationReason::LikelyFalseAlarmAtHighSeverity);
    let mut rows = vec![row];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
    assert_eq!(
        crate::severity_bounds::bounded_severity(&rows[0], Some(&set)),
        "low",
        "the exporters run the same chain and must agree"
    );
}

/// A row stage 1 called real, raised by the floor before the group reviewer read it, then judged
/// a rule misfit by the group reviewer: it returns to stage 1's severity, held, with the reason.
#[test]
fn a_row_the_group_reviewer_doubted_after_the_floor_returns_to_stage_one_severity() {
    let set = corpus(&[("SYN-STATUS-CODE-1", Some("critical"))]);
    let mut rows = vec![reviewed_row("SYN-STATUS-CODE-1", "low", "yes")];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "critical", "nobody doubted it yet");
    rows[0].review.escalation_reason = Some(EscalationReason::RuleMisfit);
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
    let why = notes(&rows[0]);
    assert!(why.contains("rule a misfit"), "{why}");
    assert_eq!(
        crate::severity_bounds::bounded_severity(&rows[0], Some(&set)),
        "low"
    );
}

/// The group reviewer's own `real: no` disagreement is doubt as well.
#[test]
fn a_group_reviewer_real_no_disagreement_is_doubt() {
    let set = corpus(&[("SYN-STATUS-CODE-1", Some("high"))]);
    let mut rows = vec![reviewed_row("SYN-STATUS-CODE-1", "medium", "yes")];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high");
    rows[0].review.stage2_disagreement.push(Stage2Disagreement {
        field: DisagreementField::Real,
        stage1: "yes".to_string(),
        mine: "no".to_string(),
        why: "synthetic".to_string(),
    });
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "medium", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}

/// The planted search_path shape: a definer function with no pinned search_path, raised by the
/// rule's floor, and no reviewer doubted it. It must still rise, before and after the group
/// reviewer's pass, and is not held.
#[test]
fn an_undoubted_row_is_still_raised_by_the_floor() {
    let set = corpus(&[("SYN-DEFINER-SEARCH-PATH-1", Some("critical"))]);
    let mut rows = vec![
        reviewed_row("SYN-DEFINER-SEARCH-PATH-1", "medium", "yes"),
        // A deterministic row of the same rule: never seen by stage 1, never doubted.
        Finding {
            captures: Default::default(),
            line: 40,
            ..reviewed_row("SYN-DEFINER-SEARCH-PATH-1", "medium", "yes")
        },
    ];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows.len(), 2);
    for f in &rows {
        assert_eq!(f.severity, "critical", "{}", notes(f));
        assert!(!f.needs_review);
        assert!(notes(f).contains("severity raised from medium to critical"));
    }
}

// ── The phrase floor (`apply_severity_calibration_rules`) ───────────────────────────────────

/// A test file's TLS row whose text trips the unauthenticated phrase floor. Calibration judged it
/// not real at low: the phrase floor must not raise it to critical.
fn tls_test_row(real: &str) -> Finding {
    let mut f = reviewed_row("AI-tls-verification-disabled", "low", real);
    f.path = "tests/fixtures/client_test.ts".to_string();
    f.in_test = true;
    f.needs_review = true;
    f.snippet = "rejectUnauthorized: false".to_string();
    f.detail = "TLS certificate verification is disabled, so an unauthenticated network \
                attacker can intercept this client's traffic."
        .to_string();
    f
}

#[test]
fn the_phrase_floor_does_not_raise_a_doubted_test_file_row() {
    let rows = crate::ai_audit::apply_severity_calibration_rules(vec![tls_test_row("no")]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
    let why = notes(&rows[0]);
    assert!(why.contains("severity held at low"), "{why}");
}

/// The control: the same row nobody doubted still rises on the phrase floor.
#[test]
fn the_phrase_floor_still_raises_the_same_row_when_nobody_doubted_it() {
    let rows = crate::ai_audit::apply_severity_calibration_rules(vec![tls_test_row("yes")]);
    assert_eq!(rows[0].severity, "critical", "{}", notes(&rows[0]));
}

#[test]
fn reviewer_doubt_reads_only_the_stored_reviewer_answers() {
    assert_eq!(
        reviewer_doubt(&reviewed_row("SYN-1", "low", "no")),
        Some(Doubt::Stage1NotReal)
    );
    assert_eq!(reviewer_doubt(&reviewed_row("SYN-1", "low", "yes")), None);
    assert_eq!(
        reviewer_doubt(&reviewed_row("SYN-1", "low", "unclear")),
        None
    );
    let mut f = reviewed_row("SYN-1", "low", "yes");
    f.review.escalation_reason = Some(EscalationReason::OutlierInGroup);
    assert_eq!(reviewer_doubt(&f), None);
    f.review.escalation_reason = Some(EscalationReason::LikelyFalseAlarmAtHighSeverity);
    assert_eq!(reviewer_doubt(&f), Some(Doubt::LikelyFalseAlarm));
}
