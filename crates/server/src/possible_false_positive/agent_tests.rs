//! The flag on the agent finder's rows (rows carrying its captures). The agent finder sets
//! `needs_review` on every row by construction, gives no confidence, and keeps the model's own
//! rating, so reasons (b), (f) and (g) must not read those facts as doubt, while (a), (c), (d)
//! and (e) reach an agent row wherever their data exists. Synthetic data only.

use camerata_api_types::review::OutlierReason;

use super::*;
use crate::ai_audit::agent_finder::reply::{
    FINDER_PIECE_CAPTURE, FINDER_PROBLEM_CAPTURE, FINDER_SURE_CAPTURE,
};
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::severity_bounds::SEVERITY_ADJUSTMENTS_CAPTURE;

/// An agent finder row nobody doubts: a security rule at high in production code, held for review
/// the way every agent row is.
fn agent_row() -> Finding {
    let mut f = Finding {
        repo: "acme/portal".to_string(),
        path: "src/db/orders.rs".to_string(),
        line: 42,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: "format!(\"select * from orders where id = {}\", id)".to_string(),
        detail: "The query is built from the request.".to_string(),
        needs_review: true,
        category: Some("injection".to_string()),
        ..Finding::default()
    };
    f.captures
        .insert(FINDER_PROBLEM_CAPTURE.to_string(), "1".to_string());
    f.captures
        .insert(FINDER_PIECE_CAPTURE.to_string(), "piece-1".to_string());
    f.captures
        .insert(FINDER_SURE_CAPTURE.to_string(), "sure".to_string());
    f
}

fn convention_agent_row(severity: &str) -> Finding {
    let mut f = agent_row();
    f.rule_id = "CONV-NAMING-1".to_string();
    f.category = Some("naming".to_string());
    f.severity = severity.to_string();
    f
}

fn open_question_row() -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "src/db/orders.rs".to_string(),
        line: 7,
        rule_id: "AI-OPEN-QUESTION".to_string(),
        severity: "medium".to_string(),
        snippet: "A listed place could not be checked".to_string(),
        needs_review: true,
        ..Finding::default()
    }
}

fn reasons(f: &Finding) -> Vec<FpReason> {
    reasons_of(f, false)
}

#[test]
fn agent_rows_with_none_of_the_reasons_get_no_flag() {
    let rows = [
        agent_row(),
        convention_agent_row("low"),
        convention_agent_row("info"),
        open_question_row(),
    ];
    let index = FpIndex::new(&rows);
    for f in &rows {
        assert_eq!(reasons(f), Vec::<FpReason>::new(), "{}", f.rule_id);
        assert_eq!(index.of(f), None, "{}", f.rule_id);
    }
}

#[test]
fn being_held_for_review_is_not_calibration_doubt_on_an_agent_row() {
    let f = agent_row();
    assert!(f.needs_review);
    assert!(!reasons(&f).contains(&FpReason::CalibrationDoubt));
    let q = open_question_row();
    assert!(!reasons(&q).contains(&FpReason::CalibrationDoubt));
}

#[test]
fn a_model_only_row_reason_never_fires_on_an_agent_row() {
    // Even with a (hypothetical) low confidence and no deterministic row at its place.
    let mut f = agent_row();
    f.confidence = Some("needs-review".to_string());
    let r = reasons(&f);
    assert!(!r.contains(&FpReason::ModelOnly), "{r:?}");
    assert!(!reasons(&agent_row()).contains(&FpReason::ModelOnly));
}

#[test]
fn an_agent_row_is_not_a_deterministic_row_at_its_place() {
    // The agent row carries no confidence, so it would read as deterministic; it must not make an
    // engine model row beside it look corroborated.
    let agent = agent_row();
    let mut engine = agent_row();
    engine.captures.clear();
    engine.needs_review = false;
    engine.confidence = Some("disputed".to_string());
    let rows = [agent, engine];
    let flag = FpIndex::new(&rows).of(&rows[1]).expect("flagged");
    assert!(flag.reasons.contains(&FpReason::ModelOnly), "{flag:?}");
}

#[test]
fn a_raise_recorded_on_an_agent_row_is_severity_from_a_floor() {
    let mut raised = agent_row();
    raised.captures.insert(
        SEVERITY_ADJUSTMENTS_CAPTURE.to_string(),
        "severity raised from medium to high: Severity floor: unauthenticated access is High by default"
            .to_string(),
    );
    assert!(reasons(&raised).contains(&FpReason::SeverityFromRuleMinimum));

    // No recorded raise: the model's own rating stands.
    assert!(!reasons(&agent_row()).contains(&FpReason::SeverityFromRuleMinimum));

    // A lowering is not a raise.
    let mut lowered = agent_row();
    lowered.captures.insert(
        SEVERITY_ADJUSTMENTS_CAPTURE.to_string(),
        "severity lowered from critical to high: a ceiling".to_string(),
    );
    assert!(!reasons(&lowered).contains(&FpReason::SeverityFromRuleMinimum));
}

#[test]
fn each_kept_reason_fires_on_an_agent_row_of_its_own_shape() {
    use camerata_api_types::review::{DisagreementField, Stage2Disagreement};

    let mut doubt = agent_row();
    doubt.review.outlier_reason = Some(OutlierReason::CouldBeIntentional);
    assert_eq!(reasons(&doubt), vec![FpReason::GroupReviewerDoubt]);

    let mut down = agent_row();
    down.review.stage2_disagreement.push(Stage2Disagreement {
        field: DisagreementField::Real,
        stage1: "yes".to_string(),
        mine: "no".to_string(),
        why: "synthetic".to_string(),
    });
    assert_eq!(reasons(&down), vec![FpReason::GroupReviewerDoubt]);

    let mut in_test = agent_row();
    in_test.path = "tests/fixtures/orders.rs".to_string();
    in_test.rule_id = "CONV-NAMING-1".to_string();
    in_test.severity = "low".to_string();
    assert_eq!(reasons(&in_test), vec![FpReason::TestCodeOnly]);

    // A secret committed in a test is still not flagged for test scope.
    let mut secret = agent_row();
    secret.path = "tests/fixtures/keys.rs".to_string();
    secret.rule_id = "SEC-NO-SECRET-FILE-1".to_string();
    assert_eq!(reasons(&secret), Vec::<FpReason>::new());

    assert_eq!(
        reasons(&convention_agent_row("medium")),
        vec![FpReason::ConventionRuleAtMediumOrHigher]
    );

    let mut evidence = agent_row();
    evidence.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the cited line was not found".to_string(),
    );
    assert_eq!(reasons(&evidence), vec![FpReason::EvidenceNotFound]);
}

#[test]
fn the_flag_on_an_agent_row_changes_nothing_on_the_row() {
    let rows = [convention_agent_row("high")];
    let before = rows[0].clone();
    let _ = FpIndex::new(&rows).of(&rows[0]);
    assert_eq!(rows[0].severity, before.severity);
    assert_eq!(rows[0].needs_review, before.needs_review);
    assert_eq!(rows[0].captures, before.captures);
}

#[test]
fn an_agent_row_the_finder_marked_unsure_is_flagged_for_the_models_own_doubt() {
    let mut f = agent_row();
    f.captures
        .insert(FINDER_SURE_CAPTURE.to_string(), "unsure".to_string());
    assert_eq!(reasons(&f), vec![FpReason::FinderUnsure]);
    // It is its own reason: calibration doubt stays off.
    assert!(!reasons(&f).contains(&FpReason::CalibrationDoubt));
    let rows = [f.clone()];
    assert_eq!(
        FpIndex::new(&rows).of(&f).map(|p| p.reasons),
        Some(vec![FpReason::FinderUnsure])
    );
    // A sure row is not flagged, and a row that is not an agent row never is, whatever it carries.
    let mut sure = agent_row();
    sure.captures
        .insert(FINDER_SURE_CAPTURE.to_string(), "yes".to_string());
    assert_eq!(reasons(&sure), Vec::<FpReason>::new());
    let mut engine = agent_row();
    engine.captures.clear();
    engine
        .captures
        .insert(FINDER_SURE_CAPTURE.to_string(), "unsure".to_string());
    engine.needs_review = false;
    assert!(!is_agent_row(&engine));
    assert!(!reasons(&engine).contains(&FpReason::FinderUnsure));
}

// ── The row's own sentence beside the reason ─────────────────────────────────────────────────

#[test]
fn a_finder_unsure_row_carries_the_finders_own_sentence() {
    use crate::ai_audit::agent_finder::reply::FINDER_UNSURE_WHY_CAPTURE;
    let mut f = agent_row();
    f.captures
        .insert(FINDER_SURE_CAPTURE.to_string(), "unsure".to_string());
    f.captures.insert(
        FINDER_UNSURE_WHY_CAPTURE.to_string(),
        "The caller may be checked in a layer not shown.".to_string(),
    );
    let flag = FpIndex::new(std::slice::from_ref(&f))
        .of(&f)
        .expect("flagged");
    assert_eq!(
        flag.specifics
            .get(&FpReason::FinderUnsure)
            .map(String::as_str),
        Some("The caller may be checked in a layer not shown.")
    );
    assert!(flag
        .sentences()
        .ends_with("The caller may be checked in a layer not shown."));
}

#[test]
fn a_group_reviewer_doubt_row_carries_the_reviewers_own_why() {
    use camerata_api_types::review::{DisagreementField, Stage2Disagreement};
    let mut by_disagreement = agent_row();
    by_disagreement
        .review
        .stage2_disagreement
        .push(Stage2Disagreement {
            field: DisagreementField::Real,
            stage1: "yes".to_string(),
            mine: "no".to_string(),
            why: "The value is a constant, not request input.".to_string(),
        });
    let mut by_outlier = agent_row();
    by_outlier.review.outlier_reason = Some(OutlierReason::DifferentObject);
    by_outlier.review.outlier_why = "It queries a lookup table, not orders.".to_string();
    for (f, want) in [
        (
            by_disagreement,
            "The value is a constant, not request input.",
        ),
        (by_outlier, "It queries a lookup table, not orders."),
    ] {
        let flag = FpIndex::new(std::slice::from_ref(&f))
            .of(&f)
            .expect("flagged");
        assert_eq!(
            flag.specifics
                .get(&FpReason::GroupReviewerDoubt)
                .map(String::as_str),
            Some(want)
        );
    }
}

#[test]
fn a_reason_without_a_row_sentence_shows_only_its_standard_sentence() {
    let mut f = agent_row();
    f.captures
        .insert(FINDER_SURE_CAPTURE.to_string(), "unsure".to_string());
    let flag = FpIndex::new(std::slice::from_ref(&f))
        .of(&f)
        .expect("flagged");
    assert!(flag.specifics.is_empty());
    assert_eq!(flag.sentences(), FpReason::FinderUnsure.sentence());
    // And a reviewer's why that is blank adds nothing.
    let mut g = agent_row();
    g.review.outlier_reason = Some(OutlierReason::CouldBeIntentional);
    g.review.outlier_why = "  ".to_string();
    let flag = FpIndex::new(std::slice::from_ref(&g))
        .of(&g)
        .expect("flagged");
    assert!(flag.specifics.is_empty());
}
