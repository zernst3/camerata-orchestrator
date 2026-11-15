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

// ── Cause 4: kind follows the claim, not the rule id ────────────────────────────────────────

/// A row of a multi-option rule with a decided convention, classified a Deviation by rule id
/// (`ai_audit::assign_deviation_kinds`), rated `stage1` by stage 1, with the group reviewer's
/// escalation reason `reason`.
fn deviation_row(rule: &str, stage1: &str, reason: Option<EscalationReason>) -> Finding {
    let mut f = reviewed_row(rule, stage1, "yes");
    f.kind = Some(FindingKind::Deviation(
        camerata_api_types::audit::DeviationKind {
            deviating_sites: 3,
            assessed_sites: Some(9),
            convention_option_id: Some("handler-checks".to_string()),
            convention_label: "Handlers check access themselves".to_string(),
        },
    ));
    f.review.escalation_reason = reason;
    f
}

/// The pipeline as it runs: the bounds (and so the deviation cap) before the group reviewer,
/// the reviewer's reason written on the row, then the pass that follows the reviewers.
fn after_group_review(mut rows: Vec<Finding>, set: &camerata_rules::RuleSet) -> Vec<Finding> {
    let reasons: Vec<Option<EscalationReason>> =
        rows.iter().map(|f| f.review.escalation_reason).collect();
    for f in rows.iter_mut() {
        f.review.escalation_reason = None;
    }
    apply_scan_severity_bounds(&mut rows, Some(set));
    for (f, reason) in rows.iter_mut().zip(reasons) {
        f.review.escalation_reason = reason;
    }
    let before = rows.len();
    follow_reviewers(&mut rows, Some(set));
    assert_eq!(rows.len(), before, "nothing is dropped");
    rows
}

fn is_defect(f: &Finding) -> bool {
    matches!(f.kind, Some(FindingKind::Defect))
}

/// "PATCH updates any record by id without authorizing ownership", reported at medium: an
/// authorization bypass is a defect, not a consistency deviation.
#[test]
fn a_security_reason_deviation_at_medium_becomes_a_defect() {
    let set = corpus(&[("SYN-ACCESS-CONVENTION-1", None)]);
    let rows = after_group_review(
        vec![deviation_row(
            "SYN-ACCESS-CONVENTION-1",
            "medium",
            Some(EscalationReason::AuthorizationBypass),
        )],
        &set,
    );
    assert!(is_defect(&rows[0]), "{:?}", rows[0].kind);
    assert_eq!(rows[0].severity, "medium");
    let why = notes(&rows[0]);
    assert!(why.contains("authorization bypass"), "{why}");
    assert!(why.contains("defect"), "{why}");
}

/// "Full profile row passed to a client component", rated high and capped to medium as a
/// deviation: as a defect it is uncapped and back at high, everywhere.
#[test]
fn a_capped_security_reason_deviation_becomes_an_uncapped_defect() {
    let set = corpus(&[("SYN-DATA-SHAPE-CONVENTION-1", None)]);
    let rows = after_group_review(
        vec![deviation_row(
            "SYN-DATA-SHAPE-CONVENTION-1",
            "high",
            Some(EscalationReason::SecretExposure),
        )],
        &set,
    );
    assert!(is_defect(&rows[0]), "{:?}", rows[0].kind);
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert_eq!(
        crate::severity_bounds::bounded_severity(&rows[0], Some(&set)),
        "high",
        "the exporters run the same chain and must not cap it"
    );
}

/// "Any authenticated user can change roles", rated high and left capped at medium; and a
/// data-loss reason counts as well.
#[test]
fn authorization_and_data_loss_reasons_both_make_a_capped_deviation_a_defect() {
    let set = corpus(&[("SYN-ROLE-CONVENTION-1", None)]);
    let rows = after_group_review(
        vec![
            deviation_row(
                "SYN-ROLE-CONVENTION-1",
                "high",
                Some(EscalationReason::AuthorizationBypass),
            ),
            Finding {
                line: 30,
                ..deviation_row(
                    "SYN-ROLE-CONVENTION-1",
                    "critical",
                    Some(EscalationReason::DataLoss),
                )
            },
        ],
        &set,
    );
    assert!(rows.iter().all(is_defect), "{rows:?}");
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert_eq!(rows[1].severity, "critical", "{}", notes(&rows[1]));
}

/// A plain convention deviation stays a capped deviation: no reason, a reason that is not a
/// security or data-loss reason, or a security reason at stage 1 low.
#[test]
fn a_plain_convention_deviation_stays_a_capped_deviation() {
    let set = corpus(&[("SYN-LAYERING-CONVENTION-1", None)]);
    let rows = after_group_review(
        vec![
            deviation_row("SYN-LAYERING-CONVENTION-1", "high", None),
            Finding {
                line: 31,
                ..deviation_row(
                    "SYN-LAYERING-CONVENTION-1",
                    "high",
                    Some(EscalationReason::OutlierInGroup),
                )
            },
            Finding {
                line: 32,
                ..deviation_row(
                    "SYN-LAYERING-CONVENTION-1",
                    "low",
                    Some(EscalationReason::AuthorizationBypass),
                )
            },
        ],
        &set,
    );
    for f in &rows {
        assert!(
            matches!(f.kind, Some(FindingKind::Deviation(_))),
            "{:?}",
            f.kind
        );
    }
    assert_eq!(rows[0].severity, "medium", "capped");
    assert_eq!(rows[1].severity, "medium", "capped");
    assert_eq!(rows[2].severity, "low");
}
