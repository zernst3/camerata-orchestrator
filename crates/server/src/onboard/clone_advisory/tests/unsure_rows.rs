//! An unsure clone candidate is shown for a decision, never hidden as "not assessed" (Zach,
//! 2026-11-20). A rule with at least one judged candidate never also carries a `not_assessed`
//! row: the review screen's one-rule-one-surface check would flag it. A rule where no candidate
//! could be settled keeps its single `not_assessed` row.

use super::*;
use camerata_api_types::audit::FindingKind;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

/// Everything the scan would hand the review screen for the rule: confirmed rows, unsure rows
/// and `not_assessed` rows, in the order `onboard` extends them.
fn produced(outcome: &JudgeOutcome) -> Vec<crate::onboard::Finding> {
    let mut all = outcome.findings.clone();
    all.extend(outcome.not_assessed.iter().cloned());
    all
}

/// The review screen's own check, run on the rows the judge produced.
fn surfaces_reached(rows: &[crate::onboard::Finding]) -> usize {
    let views: Vec<camerata_ui_core::triage::FindingView> = rows
        .iter()
        .map(|f| {
            serde_json::from_value(serde_json::to_value(f).expect("a finding serializes"))
                .expect("the review screen reads the finding")
        })
        .collect();
    camerata_ui_core::triage::rules_on_multiple_surfaces(&views)
        .iter()
        .map(|(_, surfaces)| surfaces.len())
        .max()
        .unwrap_or(1)
}

/// Three candidates: the first is a real clone, the second is judged different and the third is
/// unsure.
async fn clone_different_unsure() -> (JudgeOutcome, String) {
    let (all, set) = synthetic_set(3, 0);
    let ids: Vec<String> = set.by_rule[&RULE].iter().map(|c| c.id.clone()).collect();
    let (first, second) = (ids[0].clone(), ids[1].clone());
    let stub = StubJudge::new(move |req, _| {
        let items: Vec<serde_json::Value> = asked_ids(req)
            .iter()
            .map(|id| {
                let word = if *id == first {
                    RULE.affirmative_word()
                } else if *id == second {
                    "different"
                } else {
                    "unsure"
                };
                let concept = if *id == first { "the discount rule" } else { "" };
                serde_json::json!({ "id": id, "verdict": word, "concept": concept })
            })
            .collect();
        Ok(serde_json::json!({ "verdicts": items }).to_string())
    });
    let outcome = judge(Some(&stub), &all, set).await;
    (outcome, ids[2].clone())
}

#[tokio::test]
async fn two_judged_and_one_unsure_gives_the_clone_rows_and_an_unsure_row_but_no_not_assessed() {
    let (outcome, unsure_id) = clone_different_unsure().await;

    assert!(
        outcome.not_assessed.is_empty(),
        "a rule with a judged candidate carries no not_assessed row: {:?}",
        outcome.not_assessed
    );
    let rows = produced(&outcome);
    assert!(
        !rows
            .iter()
            .any(|f| matches!(f.kind, Some(FindingKind::NotAssessed(_)))),
        "{rows:#?}"
    );
    assert_eq!(surfaces_reached(&rows), 1, "one surface for the rule");

    let unsure: Vec<_> = rows.iter().filter(|f| f.needs_review).collect();
    let sure: Vec<_> = rows.iter().filter(|f| !f.needs_review).collect();
    assert!(!sure.is_empty(), "the confirmed clone still ships rows");
    assert!(sure.iter().all(|f| f.confidence.as_deref() == Some("high")));
    assert!(!unsure.is_empty(), "the unsure candidate is a row");

    // Marked unsure the way calibration marks it, in the Queue as a defect row.
    let family = unsure_id
        .strip_prefix(&format!("{}:", RULE.id()))
        .unwrap()
        .to_string();
    for row in &unsure {
        assert_eq!(row.kind, Some(FindingKind::Defect));
        assert_eq!(row.confidence.as_deref(), Some("needs-review"));
        assert_eq!(row.rule_id, RULE.id());
        assert_eq!(row.captures.get("family"), Some(&family));
        assert_eq!(
            row.captures.get("judge_verdict").map(String::as_str),
            Some("unsure")
        );
        // It names its candidate sites and says the judge was unsure and why.
        assert!(row.detail.contains("the judge was unsure"), "{}", row.detail);
        assert!(
            row.detail.contains("did not settle whether"),
            "{}",
            row.detail
        );
        assert!(row.detail.contains("needs a reviewer's decision"));
        assert!(row.detail.contains("src/"), "sites are named: {}", row.detail);
        assert!(
            !row.detail.contains('\u{2014}') && !row.detail.contains('\u{2013}'),
            "no em or en dash: {}",
            row.detail
        );
    }

    // The ledger still says the rule was not fully assessed: an unsure candidate is not clean.
    let counts = &outcome.counts[&RULE];
    assert_eq!((counts.judged, counts.unjudged, counts.unsure), (2, 1, 1));
    assert!(!counts.fully_assessed());
}

#[tokio::test]
async fn every_candidate_unsure_is_one_not_assessed_row_and_nothing_else() {
    let (all, set) = synthetic_set(3, 0);
    let stub = StubJudge::answering("unsure", "");
    let outcome = judge(Some(&stub), &all, set).await;

    assert!(outcome.findings.is_empty(), "{:?}", outcome.findings);
    assert_eq!(outcome.not_assessed.len(), 1);
    let rows = produced(&outcome);
    assert_eq!(surfaces_reached(&rows), 1);
    assert!(matches!(
        rows[0].kind,
        Some(FindingKind::NotAssessed(_))
    ));
}

#[tokio::test]
async fn a_failed_batch_beside_a_judged_one_is_held_rows_not_a_second_surface() {
    // Two batches: the first answers, the second errors. The second batch's candidates have no
    // answer, but the rule has judged candidates, so they are unsure rows, not a not_assessed row.
    let (all, set) = synthetic_set(BATCH_SIZE + 2, 0);
    let stub = StubJudge::new(|req, call| {
        if call == 0 {
            Ok(verdicts(&asked_ids(req), "different", ""))
        } else {
            anyhow::bail!("connection reset by peer")
        }
    });
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(outcome.not_assessed.is_empty());
    let rows = produced(&outcome);
    assert_eq!(surfaces_reached(&rows), 1);
    assert!(!rows.is_empty() && rows.iter().all(|f| f.needs_review));
    assert!(rows.iter().all(|f| f.captures.get("judge_verdict").map(String::as_str)
        == Some("no_answer")));
    assert!(rows[0].detail.contains("no usable answer"), "{}", rows[0].detail);
}

#[tokio::test]
async fn candidates_over_the_cap_never_put_a_judged_rule_on_a_second_surface() {
    // Everything inside the cap was judged a clone; more were left over the cap. The leftover
    // candidates have no site to show, so the ledger discloses them and the rule stays on one
    // surface.
    let (all, set) = synthetic_set(2, 5);
    let stub = StubJudge::answering(RULE.affirmative_word(), "the discount rule");
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(outcome.not_assessed.is_empty());
    assert_eq!(surfaces_reached(&produced(&outcome)), 1);

    let mut ledger = crate::scan_ledger::ScanLedger::default();
    record_in_ledger(&mut ledger, "acme/shop", &outcome);
    let counts = &outcome.counts[&RULE];
    assert_eq!((counts.judged, counts.over_cap), (2, 5));
    assert!(!counts.fully_assessed(), "the cap gap is still disclosed");
}
