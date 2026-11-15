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

/// The operator-only detail of a held severity.
fn hold_detail(f: &Finding) -> String {
    f.captures
        .get(SEVERITY_HOLD_DETAIL_CAPTURE)
        .cloned()
        .unwrap_or_default()
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
    assert!(
        why.contains("severity held at low pending a decision"),
        "{why}"
    );
    // The client reads the neutral sentence; the reviewer detail is the operator's.
    assert!(!why.contains("not real"), "{why}");
    let detail = hold_detail(&rows[0]);
    assert!(detail.contains("not real"), "{detail}");
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
    assert!(
        why.contains("severity held at low pending a decision"),
        "{why}"
    );
    assert!(!why.contains("misfit"), "{why}");
    let detail = hold_detail(&rows[0]);
    assert!(detail.contains("rule a misfit"), "{detail}");
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

/// A plain convention deviation stays a capped deviation: no reason, or a reason that is not a
/// security or data-loss reason.
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
}

// ── Round 1 of PR #156's review ─────────────────────────────────────────────────────────────

/// The scan tool's row at the same place as `row`, undoubted: no reviewer stage reads it.
fn tool_row_at(row: &Finding, severity: &str) -> Finding {
    Finding {
        severity: severity.to_string(),
        captures: Default::default(),
        preview: true,
        preview_tool: Some("semgrep".to_string()),
        ..row.clone()
    }
}

/// Defect 1: a `real: no` low row, a Semgrep high row folded into it, the rule's minimum medium.
/// The merged row counts as undoubted for severity (the merged high stands) and stays held.
#[test]
fn an_undoubted_tool_row_folded_into_a_doubted_row_keeps_the_merged_severity_and_holds_it() {
    let set = corpus(&[("SYN-MERGE-1", Some("medium"))]);
    let mut rows = vec![reviewed_row("SYN-MERGE-1", "low", "no")];
    let semgrep = tool_row_at(&rows[0], "high");
    let left = crate::routes::dedup_scan_previews_with(&mut rows, vec![semgrep], None, Some(&set));
    assert!(left.is_empty(), "the tool row folds in: {left:?}");
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review, "the row stays held");
    let detail = rows[0]
        .captures
        .get("severity_hold_detail")
        .cloned()
        .unwrap_or_default();
    assert!(detail.contains("one source"), "{detail:?}");
}

/// The same fold, but the group reviewer's doubt arrives after the merge.
#[test]
fn a_group_reviewer_doubt_after_an_undoubted_tool_row_was_folded_in_does_not_lower_the_row() {
    let set = corpus(&[("SYN-MERGE-2", Some("medium"))]);
    let mut rows = vec![reviewed_row("SYN-MERGE-2", "low", "yes")];
    let semgrep = tool_row_at(&rows[0], "high");
    crate::routes::dedup_scan_previews_with(&mut rows, vec![semgrep], None, Some(&set));
    apply_scan_severity_bounds(&mut rows, Some(&set));
    rows[0].review.escalation_reason = Some(EscalationReason::RuleMisfit);
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}

/// A doubted tool-free row: the tool row is promoted over it by precedence. The doubted source
/// is demoted into the merged row, which is held.
#[test]
fn a_doubted_row_demoted_under_a_promoted_tool_row_holds_the_merged_row() {
    let set = corpus(&[("SYN-MERGE-3", Some("medium"))]);
    let mut rows = vec![reviewed_row("SYN-MERGE-3", "low", "no")];
    let mut semgrep = tool_row_at(&rows[0], "high");
    semgrep.rule_id = "syn.tool.rule".to_string();
    let mut selectors = std::collections::HashMap::new();
    selectors.insert("SYN-MERGE-3".to_string(), vec!["syn.tool.rule".to_string()]);
    crate::routes::dedup_scan_previews_with(&mut rows, vec![semgrep], Some(&selectors), Some(&set));
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}

/// Defect 1, the review tree's raise: a tool row collapsed into a doubted survivor raises it to
/// the tool row's severity. The raise stands and the row stays held.
#[test]
fn the_review_trees_collapse_of_a_tool_row_into_a_doubted_row_keeps_the_raise_and_holds_it() {
    let set = corpus(&[("SYN-COLLAPSE-1", Some("medium"))]);
    let mut model = reviewed_row("SYN-COLLAPSE-1", "low", "no");
    model.located = true;
    let mut tool = tool_row_at(&model, "high");
    tool.located = true;
    let outcome = crate::review_tree::collapse_identical(
        vec![model, tool],
        &crate::review_tree::ToolMap::from_selectors(None),
    );
    let mut rows = outcome.rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].severity, "high", "the collapse raises it");
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}

/// The same collapse where the undoubted source is another model row, not a tool.
#[test]
fn the_review_trees_collapse_of_an_undoubted_model_row_into_a_doubted_row_keeps_the_raise() {
    let set = corpus(&[("SYN-COLLAPSE-2", Some("medium"))]);
    // The survivor is the deterministic row (tier above the model's), doubted through a verdict.
    let mut survivor = reviewed_row("SYN-COLLAPSE-2", "low", "no");
    survivor.located = true;
    let mut source = reviewed_row("SYN-COLLAPSE-2", "high", "yes");
    source.located = true;
    source.confidence = Some("high".to_string());
    let outcome = crate::review_tree::collapse_identical(
        vec![survivor, source],
        &crate::review_tree::ToolMap::from_selectors(None),
    );
    let mut rows = outcome.rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].severity, "high", "the collapse raises it");
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}

/// Defect 1, the other half of the rule: a row is returned to its reviewed severity only when a
/// floor is what put it where it is. A doubted row more severe than every floor was raised by
/// something else and is left alone.
#[test]
fn a_doubted_row_more_severe_than_every_floor_is_not_lowered() {
    let set = corpus(&[("SYN-OTHER-RAISE-1", Some("medium"))]);
    let mut rows = vec![reviewed_row("SYN-OTHER-RAISE-1", "low", "yes")];
    rows[0].severity = "critical".to_string();
    rows[0].review.escalation_reason = Some(EscalationReason::RuleMisfit);
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "critical", "{}", notes(&rows[0]));
}

// ── Defect 2 and the mutation gaps ──────────────────────────────────────────────────────────

/// A TLS row of a production file whose wording trips the phrase floor. Not held, not in a test
/// file: nothing but the floor's own hold can hold it.
fn tls_production_row(real: &str) -> Finding {
    let mut f = tls_test_row(real);
    f.path = "src/client.ts".to_string();
    f.in_test = false;
    f.needs_review = false;
    f
}

#[test]
fn the_phrase_floor_holds_a_doubted_production_row_that_was_not_held_before() {
    let before = tls_production_row("no");
    assert!(!before.needs_review, "the fixture starts unheld");
    let rows = crate::ai_audit::apply_severity_calibration_rules(vec![before]);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review, "the doubted row is held");
}

/// M2: a phrase-floor raise followed by the group reviewer's doubt goes back to stage 1's
/// severity; with no corpus rule, only the phrase floor can say a floor raised it.
#[test]
fn a_group_reviewer_doubt_after_a_phrase_floor_raise_returns_the_row_to_stage_one_severity() {
    let mut rows =
        crate::ai_audit::apply_severity_calibration_rules(vec![tls_production_row("yes")]);
    assert_eq!(rows[0].severity, "critical", "the phrase floor raised it");
    rows[0].review.escalation_reason = Some(EscalationReason::RuleMisfit);
    follow_reviewers(&mut rows, None);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
}

/// M3: the return holds the row. The phrase floor did not hold it (nobody doubted it then), and
/// no corpus floor holds it afterwards, so the return's own hold is the only one.
#[test]
fn the_return_to_stage_one_severity_after_a_phrase_floor_raise_holds_the_row() {
    let mut rows =
        crate::ai_audit::apply_severity_calibration_rules(vec![tls_production_row("yes")]);
    assert!(
        !rows[0].needs_review,
        "nobody doubted it when it was raised"
    );
    rows[0].review.escalation_reason = Some(EscalationReason::LikelyFalseAlarmAtHighSeverity);
    follow_reviewers(&mut rows, None);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review, "the row is held for a decision");
}

// ── The owner's rulings ─────────────────────────────────────────────────────────────────────

/// A Deviation with a security reason is a Defect at any severity, one stage 1 rated low too.
#[test]
fn a_security_reason_deviation_stage_one_rated_low_becomes_a_defect() {
    let set = corpus(&[("SYN-LOW-CONVENTION-1", None)]);
    let rows = after_group_review(
        vec![deviation_row(
            "SYN-LOW-CONVENTION-1",
            "low",
            Some(EscalationReason::AuthorizationBypass),
        )],
        &set,
    );
    assert!(is_defect(&rows[0]), "{:?}", rows[0].kind);
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
}

/// A row only the group reviewer doubted, with no stage 1 severity: it keeps its severity and is
/// held, because a floor touches it.
#[test]
fn a_row_only_the_group_reviewer_doubted_with_no_stage_one_severity_keeps_its_severity_and_is_held()
{
    let set = corpus(&[("SYN-NO-STAGE1-1", Some("high"))]);
    let mut row = reviewed_row("SYN-NO-STAGE1-1", "low", "yes");
    row.captures.clear();
    let mut rows = vec![row];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "no reviewer had doubted it yet");
    rows[0].review.escalation_reason = Some(EscalationReason::RuleMisfit);
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}

/// Doubted rows no floor touches are not held by this change.
#[test]
fn a_doubted_row_no_floor_touches_is_not_held() {
    let set = corpus(&[("SYN-NO-FLOOR-1", None)]);
    let mut row = reviewed_row("SYN-NO-FLOOR-1", "medium", "yes");
    row.review.escalation_reason = Some(EscalationReason::RuleMisfit);
    let mut rows = vec![row];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "medium");
    assert!(!rows[0].needs_review);
}

fn cors_row(real: &str) -> Finding {
    let mut f = reviewed_row("AI-CORS-REFLECTED-ORIGIN-1", "low", real);
    f.detail = "The CORS configuration reflects any origin and allows credentials.".to_string();
    f
}

fn redirect_row(real: &str) -> Finding {
    let mut f = reviewed_row("AI-LOGIN-REDIRECT-1", "low", real);
    f.detail = "The login handler is an open redirect: it sends the user to the next parameter."
        .to_string();
    f
}

/// The CORS and open-redirect clamps lower a doubted row as before, but never raise one.
#[test]
fn the_cors_and_redirect_clamps_never_raise_a_doubted_row_and_hold_it() {
    for (name, row) in [("cors", cors_row("no")), ("redirect", redirect_row("no"))] {
        let rows = crate::ai_audit::apply_severity_ceiling_rules(vec![row]);
        assert_eq!(rows[0].severity, "low", "{name}: {}", notes(&rows[0]));
        assert!(rows[0].needs_review, "{name}: held for a decision");
    }
}

/// The control: the same rows nobody doubted are clamped up to medium.
#[test]
fn the_cors_and_redirect_clamps_still_raise_a_row_nobody_doubted() {
    for (name, row) in [("cors", cors_row("yes")), ("redirect", redirect_row("yes"))] {
        let rows = crate::ai_audit::apply_severity_ceiling_rules(vec![row]);
        assert_eq!(rows[0].severity, "medium", "{name}: {}", notes(&rows[0]));
        assert!(!rows[0].needs_review, "{name}");
    }
}

/// A doubted row the clamp would LOWER is lowered as before.
#[test]
fn the_cors_clamp_still_lowers_a_doubted_row() {
    let mut row = cors_row("no");
    row.severity = "high".to_string();
    let rows = crate::ai_audit::apply_severity_ceiling_rules(vec![row]);
    assert_eq!(rows[0].severity, "medium", "{}", notes(&rows[0]));
}

// ── Smaller items ───────────────────────────────────────────────────────────────────────────

/// The held note is always kept: a row already at the most notes drops its oldest other note.
#[test]
fn the_held_note_is_kept_when_the_row_already_has_the_most_notes() {
    let mut f = reviewed_row("SYN-NOTES-1", "low", "no");
    for n in 0..8 {
        crate::severity_bounds::record_adjustment(&mut f, format!("severity raised, step {n}"));
    }
    assert_eq!(recorded_adjustments(&f).len(), 8);
    crate::severity_bounds::record_adjustment(
        &mut f,
        "severity held at low pending a decision".to_string(),
    );
    let all = recorded_adjustments(&f);
    assert_eq!(all.len(), 8, "{all:?}");
    assert!(
        all.contains(&"severity held at low pending a decision".to_string()),
        "{all:?}"
    );
    assert!(
        !all.contains(&"severity raised, step 0".to_string()),
        "the oldest note made room: {all:?}"
    );
    // A note that is not a held note is still dropped when the row is full.
    crate::severity_bounds::record_adjustment(&mut f, "severity raised, step 9".to_string());
    assert!(!recorded_adjustments(&f).contains(&"severity raised, step 9".to_string()));
}

/// An exported doubted row lands in the held bucket, not among the curated findings.
#[test]
fn an_exported_doubted_row_lands_in_the_held_bucket() {
    let set = corpus(&[("SYN-HELD-BUCKET-1", Some("critical"))]);
    let mut rows = vec![reviewed_row("SYN-HELD-BUCKET-1", "low", "no")];
    apply_scan_severity_bounds(&mut rows, Some(&set));
    let sealed = {
        let mut report = crate::release_gate::test_scan_report();
        report.findings = rows;
        report.seal()
    };
    let json = crate::report_export::build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &Default::default()),
        Some(&set),
        &crate::report_export::ReportOptions::default(),
    );
    let sites = |groups: &[crate::report_export::CuratedGroupJson]| {
        groups.iter().map(|g| g.sites.len()).sum::<usize>()
    };
    assert_eq!(sites(&json.held_for_review_findings), 1);
    assert_eq!(sites(&json.curated_findings), 0);
}

// ── Round 2 of PR #156's review ─────────────────────────────────────────────────────────────

/// Defect 1 (round 2): the survivor of a collapse is the deterministic row (no stage 1 record, so
/// nobody doubted it); the folded source is an identical model row stage 1 judged not real. The
/// survivor takes the source's stage 1 record, and that doubt must not strip its rule minimum.
#[test]
fn an_undoubted_survivor_does_not_inherit_a_folded_sources_doubt() {
    let set = corpus(&[("SYN-INHERIT-1", Some("critical"))]);
    let mut survivor = reviewed_row("SYN-INHERIT-1", "medium", "yes");
    survivor.captures.remove(crate::ai_audit::STAGE1_CAPTURE);
    survivor.located = true;
    let mut source = reviewed_row("SYN-INHERIT-1", "low", "no");
    source.located = true;
    source.confidence = Some("medium".to_string());
    let outcome = crate::review_tree::collapse_identical(
        vec![survivor, source],
        &crate::review_tree::ToolMap::from_selectors(None),
    );
    let mut rows = outcome.rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].confidence.is_none(),
        "the deterministic row survived"
    );
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "critical", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review, "the row stays held");
}

/// A row of another rule that absorbed `site`, the way the review tree leaves a survivor.
fn host_with_site(site: crate::onboard::MergedLocation) -> Finding {
    let mut host = reviewed_row("SYN-UNF-HOST-1", "low", "yes");
    host.path = "src/host.ts".to_string();
    host.also_locations = vec![site];
    host
}

/// Defect 2a (round 2): an absorbed site rated higher than the doubted row that stands for its
/// place raises that row on unfold. The raise carries no marker unless unfold sets one, so
/// `follow_reviewers` wrote the row back to its reviewed severity.
#[test]
fn an_absorbed_sites_higher_rating_raises_a_doubted_row_and_keeps_it_held() {
    let set = corpus(&[("SYN-UNF-1", Some("high"))]);
    let doubted = reviewed_row("SYN-UNF-1", "low", "no");
    let site = crate::onboard::MergedLocation::site(
        &doubted.repo,
        &doubted.path,
        doubted.line,
        &doubted.rule_id,
        &doubted.snippet,
        false,
    )
    .with_rating("high", "Synthetic headline");
    let mut rows = vec![doubted, host_with_site(site)];
    assert_eq!(crate::review_tree::unfold_also_locations(&mut rows), 0);
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review, "the row stays held");
}

/// Defect 2b (round 2): a promoted site row copies its host, stage 1 record and all. Its severity
/// follows its own rating and the rule's minimum, and it is held.
#[test]
fn a_promoted_site_row_does_not_carry_the_hosts_doubt() {
    let set = corpus(&[("SYN-UNF-2", Some("high"))]);
    let mut host = reviewed_row("SYN-UNF-2", "low", "no");
    host.also_locations = vec![crate::onboard::MergedLocation::site(
        &host.repo,
        "src/other.ts",
        30,
        &host.rule_id,
        "another site",
        false,
    )
    .with_rating("medium", "Synthetic headline")];
    let mut rows = vec![host];
    assert_eq!(crate::review_tree::unfold_also_locations(&mut rows), 1);
    apply_scan_severity_bounds(&mut rows, Some(&set));
    follow_reviewers(&mut rows, Some(&set));
    let promoted = rows.iter().find(|f| f.path == "src/other.ts").unwrap();
    assert_eq!(promoted.severity, "high", "{}", notes(promoted));
    assert!(promoted.needs_review, "{}", notes(promoted));
    // The host's own doubt still holds it at its reviewed severity.
    assert_eq!(rows[0].severity, "low", "{}", notes(&rows[0]));
    assert!(rows[0].needs_review);
}
