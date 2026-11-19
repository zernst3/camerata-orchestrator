//! Each reason fires on its own synthetic shape and not on a near miss, a row with no reason has
//! no flag, and the flag is worked out from the row alone. Synthetic data only.

use camerata_api_types::audit::FindingKind;
use camerata_api_types::review::{DisagreementField, OutlierReason, Stage2Disagreement};

use super::*;
use crate::ai_audit::STAGE1_CAPTURE;
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::report_export::overlay::{
    PROVENANCE_CAPTURE, REVIEWER_FIELDS_CAPTURE, REVIEWER_PROVENANCE,
};
use crate::severity_bounds::SEVERITY_ADJUSTMENTS_CAPTURE;

/// A model row of a security rule that nobody doubts: stage 1 called it real at the severity it
/// shows, calibration is sure, the evidence verified and it is in production code.
fn clean() -> Finding {
    let mut f = Finding {
        repo: "acme/portal".to_string(),
        path: "src/db/orders.rs".to_string(),
        line: 42,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: "format!(\"select * from orders where id = {}\", id)".to_string(),
        detail: "The query is built from the request.".to_string(),
        confidence: Some("high".to_string()),
        category: Some("injection".to_string()),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    };
    stage1(&mut f, "yes", "high");
    f
}

fn stage1(f: &mut Finding, real: &str, severity: &str) {
    f.captures.insert(
        STAGE1_CAPTURE.to_string(),
        serde_json::json!({"real": real, "reason": "built from the request", "severity": severity})
            .to_string(),
    );
}

fn disagree(f: &mut Finding, field: DisagreementField, stage1: &str, mine: &str) {
    f.review.stage2_disagreement.push(Stage2Disagreement {
        field,
        stage1: stage1.to_string(),
        mine: mine.to_string(),
        why: "synthetic".to_string(),
    });
}

/// A deterministic row: the floor never sets a confidence.
fn deterministic() -> Finding {
    let mut f = clean();
    f.confidence = None;
    f.captures.clear();
    f
}

fn reasons(f: &Finding) -> Vec<FpReason> {
    reasons_of(f, false)
}

fn has(f: &Finding, reason: FpReason) -> bool {
    reasons(f).contains(&reason)
}

#[test]
fn a_row_with_none_of_the_reasons_has_no_flag() {
    let rows = [clean()];
    let index = FpIndex::new(&rows);
    assert_eq!(reasons(&rows[0]), Vec::<FpReason>::new());
    assert_eq!(index.of(&rows[0]), None);
    let det = [deterministic()];
    assert_eq!(FpIndex::new(&det).of(&det[0]), None);
}

// ── (a) the group reviewer ────────────────────────────────────────────────────────────────

#[test]
fn a_group_reviewer_outlier_for_a_different_object_or_an_intended_pattern_is_flagged() {
    for reason in [
        OutlierReason::DifferentObject,
        OutlierReason::CouldBeIntentional,
    ] {
        let mut f = clean();
        f.review.outlier_reason = Some(reason);
        assert!(has(&f, FpReason::GroupReviewerDoubt), "{reason:?}");
    }
}

#[test]
fn an_outlier_for_any_other_reason_is_not_group_reviewer_doubt() {
    for reason in OutlierReason::ALL.into_iter().filter(|r| {
        !matches!(
            r,
            OutlierReason::DifferentObject | OutlierReason::CouldBeIntentional
        )
    }) {
        let mut f = clean();
        f.review.outlier_reason = Some(reason);
        assert!(!has(&f, FpReason::GroupReviewerDoubt), "{reason:?}");
    }
}

#[test]
fn a_downward_group_reviewer_disagreement_is_flagged_and_an_upward_one_is_not() {
    let mut lower = clean();
    disagree(&mut lower, DisagreementField::Severity, "high", "low");
    assert!(has(&lower, FpReason::GroupReviewerDoubt));

    let mut not_real = clean();
    disagree(&mut not_real, DisagreementField::Real, "yes", "no");
    assert!(has(&not_real, FpReason::GroupReviewerDoubt));

    let mut higher = clean();
    disagree(&mut higher, DisagreementField::Severity, "high", "critical");
    assert!(!has(&higher, FpReason::GroupReviewerDoubt));

    let mut real = clean();
    disagree(&mut real, DisagreementField::Real, "unclear", "yes");
    assert!(!has(&real, FpReason::GroupReviewerDoubt));
}

// ── (b) calibration ───────────────────────────────────────────────────────────────────────

#[test]
fn a_held_or_low_confidence_row_is_flagged_for_calibration_doubt() {
    for confidence in ["needs-review", "disputed"] {
        let mut f = clean();
        f.confidence = Some(confidence.to_string());
        assert!(has(&f, FpReason::CalibrationDoubt), "{confidence}");
    }
    let mut held = clean();
    held.needs_review = true;
    assert!(has(&held, FpReason::CalibrationDoubt));
}

#[test]
fn a_row_waiting_only_because_it_is_in_a_test_is_not_calibration_doubt() {
    // Calibration sets `needs_review` on every test-scoped row; that is test scope, not doubt.
    let mut f = clean();
    f.in_test = true;
    f.needs_review = true;
    assert!(!has(&f, FpReason::CalibrationDoubt));
    // A deterministic row has no calibrated opinion at all.
    assert!(!has(&deterministic(), FpReason::CalibrationDoubt));
}

// ── (c) test code ─────────────────────────────────────────────────────────────────────────

#[test]
fn a_row_in_test_code_is_flagged() {
    let mut f = clean();
    f.in_test = true;
    assert!(has(&f, FpReason::TestCodeOnly));
    let mut by_path = clean();
    by_path.path = "tests/orders_test.rs".to_string();
    assert!(has(&by_path, FpReason::TestCodeOnly));
}

#[test]
fn a_secret_committed_in_a_test_is_not_flagged_for_test_scope() {
    let mut secret_file = clean();
    secret_file.rule_id = "SEC-NO-SECRET-FILE-1".to_string();
    secret_file.in_test = true;
    secret_file.needs_review = true;
    secret_file.path = "tests/fixtures/.env.production".to_string();
    assert!(!has(&secret_file, FpReason::TestCodeOnly));
    assert_eq!(reasons(&secret_file), Vec::<FpReason>::new());

    let mut exposure = clean();
    exposure.rule_id = "AI-STRIPE-KEY-IN-SOURCE".to_string();
    exposure.category = Some("secret-exposure".to_string());
    exposure.in_test = true;
    assert!(!has(&exposure, FpReason::TestCodeOnly));
}

// ── (d) a convention rule at medium or higher ─────────────────────────────────────────────

#[test]
fn a_convention_rule_at_medium_or_higher_is_flagged() {
    for severity in ["medium", "high", "critical"] {
        let mut f = clean();
        f.rule_id = "STYLE-NAMING-1".to_string();
        f.category = Some("naming".to_string());
        f.severity = severity.to_string();
        stage1(&mut f, "yes", severity);
        assert!(
            has(&f, FpReason::ConventionRuleAtMediumOrHigher),
            "{severity}"
        );
    }
}

#[test]
fn a_convention_rule_at_low_and_a_security_rule_at_high_are_not() {
    let mut low = clean();
    low.rule_id = "STYLE-NAMING-1".to_string();
    low.category = Some("naming".to_string());
    low.severity = "low".to_string();
    stage1(&mut low, "yes", "low");
    assert!(!has(&low, FpReason::ConventionRuleAtMediumOrHigher));
    // A security rule at high is the shape a real defect has.
    assert!(!has(&clean(), FpReason::ConventionRuleAtMediumOrHigher));
    // A security word in the id or a security category makes it a security rule.
    let mut auth = clean();
    auth.rule_id = "ARCH-AUTHZ-ON-SERVER-1".to_string();
    auth.category = Some("authorization".to_string());
    assert!(!has(&auth, FpReason::ConventionRuleAtMediumOrHigher));
}

// ── (e) evidence ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_row_whose_evidence_was_not_found_is_flagged() {
    let mut f = clean();
    f.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the quoted snippet was not found near line 42".to_string(),
    );
    assert!(has(&f, FpReason::EvidenceNotFound));
    assert!(!has(&clean(), FpReason::EvidenceNotFound));
}

// ── (f) a severity that only a rule minimum gives ─────────────────────────────────────────

fn floored() -> Finding {
    let mut f = clean();
    stage1(&mut f, "yes", "low");
    f.captures.insert(
        SEVERITY_ADJUSTMENTS_CAPTURE.to_string(),
        format!(
            "severity raised from low to high: {}",
            camerata_rules::severity_bounds::MIN_SEVERITY_REASON
        ),
    );
    f
}

#[test]
fn a_severity_from_the_rule_minimum_above_both_reviewers_is_flagged() {
    assert!(has(&floored(), FpReason::SeverityFromRuleMinimum));
    // The group reviewer rated it below the shown severity too.
    let mut f = floored();
    disagree(&mut f, DisagreementField::Severity, "low", "medium");
    assert!(has(&f, FpReason::SeverityFromRuleMinimum));
}

#[test]
fn a_severity_a_reviewer_reached_or_another_bound_set_is_not_from_the_rule_minimum() {
    // The group reviewer rated it at the shown severity: a reviewer stands behind it.
    let mut stage2_agrees = floored();
    disagree(
        &mut stage2_agrees,
        DisagreementField::Severity,
        "low",
        "high",
    );
    assert!(!has(&stage2_agrees, FpReason::SeverityFromRuleMinimum));
    // Stage 1 rated it at the shown severity.
    let mut stage1_agrees = floored();
    stage1(&mut stage1_agrees, "yes", "high");
    assert!(!has(&stage1_agrees, FpReason::SeverityFromRuleMinimum));
    // Raised by something other than a rule minimum (a severity rule of thumb).
    let mut thumb = floored();
    thumb.captures.remove(SEVERITY_ADJUSTMENTS_CAPTURE);
    assert!(!has(&thumb, FpReason::SeverityFromRuleMinimum));
    // The reviewer set the severity.
    let mut edited = floored();
    edited
        .captures
        .insert(REVIEWER_FIELDS_CAPTURE.to_string(), "severity".to_string());
    assert!(!has(&edited, FpReason::SeverityFromRuleMinimum));
}

// ── (g) a model row alone at its place ────────────────────────────────────────────────────

fn model_alone() -> Finding {
    let mut f = clean();
    f.rule_id = "AI-UNSCOPED-ORDER-QUERY".to_string();
    f.confidence = Some("needs-review".to_string());
    f
}

#[test]
fn a_low_confidence_model_row_with_no_deterministic_row_at_its_place_is_flagged() {
    let rows = [model_alone()];
    let flag = FpIndex::new(&rows).of(&rows[0]).expect("flagged");
    assert!(flag.reasons.contains(&FpReason::ModelOnly), "{flag:?}");
}

#[test]
fn a_model_row_beside_a_deterministic_row_or_with_high_confidence_is_not_model_only() {
    let rows = [model_alone(), deterministic()];
    let flag = FpIndex::new(&rows)
        .of(&rows[0])
        .expect("still calibration doubt");
    assert!(!flag.reasons.contains(&FpReason::ModelOnly), "{flag:?}");
    assert!(flag.reasons.contains(&FpReason::CalibrationDoubt));
    // A deterministic row at another line of the file is not at the same place.
    let mut elsewhere = deterministic();
    elsewhere.line = 7;
    let rows = [model_alone(), elsewhere];
    let flag = FpIndex::new(&rows).of(&rows[0]).expect("flagged");
    assert!(flag.reasons.contains(&FpReason::ModelOnly), "{flag:?}");
    // A sure model row.
    let mut sure = model_alone();
    sure.confidence = Some("high".to_string());
    assert!(!has(&sure, FpReason::ModelOnly));
    // The deterministic row itself.
    assert!(!has(&deterministic(), FpReason::ModelOnly));
}

// ── The flag as a whole ───────────────────────────────────────────────────────────────────

#[test]
fn every_reason_is_reachable_and_listed_once_in_order() {
    let mut f = floored();
    f.rule_id = "AI-STYLE-NAMING".to_string();
    f.category = Some("naming".to_string());
    f.confidence = Some("disputed".to_string());
    f.in_test = true;
    f.review.outlier_reason = Some(OutlierReason::DifferentObject);
    f.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found".to_string(),
    );
    let flag = FpIndex::new([&f]).of(&f).expect("flagged");
    let mut want: Vec<FpReason> = FpReason::ALL
        .into_iter()
        .filter(|r| *r != FpReason::ConventionRuleAtMediumOrHigher)
        // Agent rows only (agent_tests.rs): this is an engine row.
        .filter(|r| *r != FpReason::FinderUnsure)
        .collect();
    want.sort();
    assert_eq!(flag.reasons, want);
    // A corpus convention rule at medium adds the last one.
    let mut convention = clean();
    convention.rule_id = "STYLE-NAMING-1".to_string();
    convention.category = Some("naming".to_string());
    convention.severity = "medium".to_string();
    stage1(&mut convention, "yes", "medium");
    assert_eq!(
        reasons(&convention),
        vec![FpReason::ConventionRuleAtMediumOrHigher]
    );
}

#[test]
fn a_row_the_reviewer_added_is_never_flagged() {
    let mut f = model_alone();
    f.captures.insert(
        PROVENANCE_CAPTURE.to_string(),
        REVIEWER_PROVENANCE.to_string(),
    );
    f.in_test = true;
    assert_eq!(FpIndex::new([&f]).of(&f), None);
}

#[test]
fn working_out_the_flag_changes_nothing_on_the_row() {
    let mut f = floored();
    f.review.outlier_reason = Some(OutlierReason::CouldBeIntentional);
    f.in_test = true;
    let before = f.clone();
    let rows = vec![f];
    let index = FpIndex::new(&rows);
    for _ in 0..3 {
        assert!(index.of(&rows[0]).is_some());
    }
    assert_eq!(
        rows,
        vec![before],
        "the flag is read from the row and never written to it"
    );
}
