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
                let concept = if *id == first {
                    "the discount rule"
                } else {
                    ""
                };
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
        // The detail is the problem statement the client reads after the decision: facts only,
        // no decision prompt and no invented cause.
        assert!(
            row.detail.contains("could not tell whether"),
            "{}",
            row.detail
        );
        for prompt in [
            "needs a decision",
            "before it is reported",
            "confirm it",
            "because",
        ] {
            assert!(!row.detail.contains(prompt), "{prompt}: {}", row.detail);
        }
        assert!(
            row.detail.contains("src/"),
            "sites are named: {}",
            row.detail
        );
        assert!(
            !row.detail.contains('\u{2014}') && !row.detail.contains('\u{2013}'),
            "no em or en dash: {}",
            row.detail
        );
        // The client's vocabulary: a problem at places, never a candidate or a site; no serial
        // comma.
        let lower = row.detail.to_ascii_lowercase();
        for word in ["candidate", "judge", "finding", " site "] {
            assert!(!lower.contains(word), "{word}: {}", row.detail);
        }
        assert!(
            !row.detail.contains(", and ") && !row.detail.contains(", or "),
            "no serial comma: {}",
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
    assert!(matches!(rows[0].kind, Some(FindingKind::NotAssessed(_))));
}

#[tokio::test]
async fn confirmed_rows_beside_a_failed_batch_get_no_held_rows_and_no_second_surface() {
    // First batch: every candidate a clone. Second batch: the call fails. The lost batch is a
    // coverage gap (never judged), not held rows: the model never looked at it.
    let (all, set) = synthetic_set(BATCH_SIZE + 2, 0);
    let stub = StubJudge::new(|req, call| {
        if call == 0 {
            Ok(verdicts(
                &asked_ids(req),
                RULE.affirmative_word(),
                "the discount rule",
            ))
        } else {
            anyhow::bail!("connection reset by peer")
        }
    });
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(outcome.not_assessed.is_empty());
    let rows = produced(&outcome);
    assert_eq!(surfaces_reached(&rows), 1);
    assert!(
        !rows.is_empty() && rows.iter().all(|f| !f.needs_review),
        "no held rows"
    );
    assert_eq!(outcome.failed.len(), 1, "the failed pass is still recorded");
    let counts = &outcome.counts[&RULE];
    assert_eq!((counts.judged, counts.unjudged), (BATCH_SIZE as u32, 2));
    // The gap is in the ledger, as the not-assessed part.
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    record_in_ledger(&mut ledger, "acme/shop", &outcome);
    let entry = ledger.rule(RULE.id()).expect("recorded");
    assert!(!entry.ran);
    assert!(
        entry
            .skip_reason
            .as_deref()
            .is_some_and(|r| r.contains("2 of")),
        "{:?}",
        entry.skip_reason
    );
}

#[tokio::test]
async fn rejected_candidates_beside_a_failed_batch_are_one_not_assessed_row() {
    // Nothing reached the Queue, so the gap needs its single not_assessed row.
    let (all, set) = synthetic_set(BATCH_SIZE + 2, 0);
    let stub = StubJudge::new(|req, call| {
        if call == 0 {
            Ok(verdicts(&asked_ids(req), "different", ""))
        } else {
            anyhow::bail!("connection reset by peer")
        }
    });
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(outcome.not_assessed.len(), 1);
    assert_eq!(surfaces_reached(&produced(&outcome)), 1);
}

#[tokio::test]
async fn confirmed_rows_beside_over_cap_candidates_get_no_second_surface() {
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
    let entry = ledger.rule(RULE.id()).expect("recorded");
    assert!(!entry.ran);
}

#[tokio::test]
async fn rejected_candidates_beside_over_cap_candidates_are_one_not_assessed_row() {
    // Judged, all rejected, more over the cap: no Queue row exists, so the review screen would
    // show nothing without the single not_assessed row.
    let (all, set) = synthetic_set(2, 5);
    let stub = StubJudge::answering("different", "");
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(outcome.not_assessed.len(), 1);
    assert_eq!(surfaces_reached(&produced(&outcome)), 1);
}

#[tokio::test]
async fn a_fully_rejected_rule_with_nothing_unsettled_has_no_row() {
    let (all, set) = synthetic_set(2, 0);
    let stub = StubJudge::answering("different", "");
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(produced(&outcome).is_empty());
}

#[tokio::test]
async fn a_decided_unsure_row_does_not_leave_the_rule_recorded_as_not_run() {
    let (outcome, _) = clone_different_unsure().await;
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    record_in_ledger(&mut ledger, "acme/shop", &outcome);
    let entry = ledger.rule(RULE.id()).expect("recorded");
    assert!(entry.ran, "{:?}", entry.skip_reason);
    assert!(entry.skip_reason.is_none());
    assert_eq!(entry.findings_emitted, outcome.findings.len());
}

#[tokio::test]
async fn unsure_beside_a_gap_splits_the_reviewer_part_from_the_not_assessed_part() {
    // One clone, one unsure, and a failed batch after them: the reason names both parts.
    let (all, set) = synthetic_set(BATCH_SIZE + 2, 0);
    let ids: Vec<String> = set.by_rule[&RULE].iter().map(|c| c.id.clone()).collect();
    let (first, second) = (ids[0].clone(), ids[1].clone());
    let stub = StubJudge::new(move |req, call| {
        if call > 0 {
            anyhow::bail!("timeout");
        }
        let items: Vec<serde_json::Value> = asked_ids(req)
            .iter()
            .map(|id| {
                let word = if *id == first {
                    RULE.affirmative_word()
                } else if *id == second {
                    "unsure"
                } else {
                    "different"
                };
                serde_json::json!({ "id": id, "verdict": word, "concept": "the rule" })
            })
            .collect();
        Ok(serde_json::json!({ "verdicts": items }).to_string())
    });
    let outcome = judge(Some(&stub), &all, set).await;
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    record_in_ledger(&mut ledger, "acme/shop", &outcome);
    let reason = ledger
        .rule(RULE.id())
        .and_then(|e| e.skip_reason.clone())
        .expect("a skip reason");
    assert!(reason.contains("1 left for the reviewer"), "{reason}");
    assert!(reason.contains("2 of"), "{reason}");
}

/// A discount written `copies` times (one candidate family of that many sites) and a shipping
/// cost written twice (a second family). The shipping family is judged a clone, the discount
/// family is unsure. Returns the rows the judge produced.
async fn clone_and_unsure_rows(copies: usize) -> Vec<crate::onboard::Finding> {
    let mut all: Vec<(String, String)> = (0..copies)
        .map(|i| {
            (
                format!("src/cart/total{i}.ts"),
                discount(&format!("applyDiscount{i}"), "0.2"),
            )
        })
        .collect();
    all.extend(files(&[
        ("src/ship/air.ts", &shipping("airCost", "20")),
        ("src/ship/sea.ts", &shipping("seaCost", "20")),
    ]));
    let set = set_for(&all, &[RULE]);
    assert_eq!(set.by_rule[&RULE].len(), 2, "two candidate families");
    let unsure_id = set.by_rule[&RULE]
        .iter()
        .find(|c| {
            c.facts
                .sites
                .iter()
                .all(|s| s.path.starts_with("src/cart/"))
        })
        .expect("the discount family")
        .id
        .clone();
    let stub = StubJudge::new(move |req, _| {
        let items: Vec<serde_json::Value> = asked_ids(req)
            .iter()
            .map(|id| {
                if *id == unsure_id {
                    serde_json::json!({ "id": id, "verdict": "unsure", "concept": "" })
                } else {
                    serde_json::json!({ "id": id, "verdict": RULE.affirmative_word(),
                                        "concept": "the cost rule" })
                }
            })
            .collect();
        Ok(serde_json::json!({ "verdicts": items }).to_string())
    });
    let outcome = judge(Some(&stub), &all, set).await;
    assert!(outcome.not_assessed.is_empty());
    outcome.findings
}

#[tokio::test]
async fn an_unsure_row_with_three_sites_lists_them_without_a_serial_comma() {
    let rows = clone_and_unsure_rows(3).await;
    let unsure: Vec<_> = rows.iter().filter(|f| f.needs_review).collect();
    assert_eq!(unsure.len(), 3, "one row per site");
    let detail = &unsure[0].detail;
    for i in 0..3 {
        assert!(
            detail.contains(&format!("src/cart/total{i}.ts")),
            "{detail}"
        );
    }
    assert!(detail.contains(" and "), "{detail}");
    assert!(
        !detail.contains(", and ") && !detail.contains(", or "),
        "{detail}"
    );
}

#[tokio::test]
async fn the_release_gate_holds_each_unsure_row_until_it_is_decided_and_a_decision_releases_it() {
    use crate::release_gate::needs_decision_tests::{
        decision_problems, edit_on, package_of_reviewed, package_with, sealed_row,
    };
    use crate::release_gate::{release, ReleaseError};
    use crate::report_export::{finding_key, DispositionWire};
    use camerata_api_types::edits::EditField;

    let rows = clone_and_unsure_rows(2).await;
    let unsure: Vec<_> = rows.iter().filter(|f| f.needs_review).cloned().collect();
    assert_eq!(
        unsure.len(),
        2,
        "one unsure candidate of two sites: {rows:#?}"
    );
    let report_of = |rows: &[crate::onboard::Finding]| {
        let mut report = crate::release_gate::tests::scan_report();
        report.findings.extend(rows.iter().cloned());
        report
    };
    let names_an_unsure_site = |p: &crate::release_gate::Problem| {
        unsure
            .iter()
            .any(|u| p.location == format!("{} line {}", u.path, u.line))
    };

    // Undecided: the gate refuses, naming each unsure site and saying the finder was unsure.
    match release(package_with(
        report_of(&rows),
        &std::collections::HashMap::new(),
    )) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = decision_problems(&failure);
            let held: Vec<_> = problems
                .iter()
                .filter(|p| names_an_unsure_site(p))
                .collect();
            assert_eq!(held.len(), 2, "{problems:?}");
            assert!(held.iter().all(|p| p.message.contains("unsure")));
        }
        other => panic!("an undecided unsure clone row must block the release: {other:?}"),
    }

    // A confirmation or a false-positive ruling releases it, and the exported problem
    // statement is still true afterwards: facts, no decision prompt.
    for state in ["Confirmed", "FalsePositive"] {
        let decisions: std::collections::HashMap<String, DispositionWire> = unsure
            .iter()
            .map(|u| {
                (
                    finding_key(u),
                    DispositionWire {
                        state: state.to_string(),
                        reason: "read it in person".to_string(),
                        ..DispositionWire::default()
                    },
                )
            })
            .collect();
        let package = package_with(report_of(&rows), &decisions);
        if state == "Confirmed" {
            let cells = crate::release_gate::xlsx_strings(&package.xlsx).expect("workbook strings");
            let statements: Vec<_> = cells
                .iter()
                .filter(|c| c.contains("-token block") || c.contains("could not tell whether"))
                .filter(|c| c.contains("src/cart/") || c.contains("could not tell whether"))
                .collect();
            assert!(
                statements
                    .iter()
                    .any(|c| c.contains("could not tell whether")),
                "the problem statement is exported: {statements:?}"
            );
            for text in statements {
                for prompt in ["needs a decision", "before it is reported", "confirm it"] {
                    assert!(
                        !text.contains(prompt),
                        "{prompt} shipped after a decision: {text}"
                    );
                }
            }
        }
        let released = release(package);
        assert!(released.is_ok(), "state {state}: {:?}", released.err());
    }

    // A severity edit is the reviewer's decision too, whichever way it moves.
    let sealed = report_of(&rows).seal();
    let edits: Vec<_> = unsure
        .iter()
        .enumerate()
        .map(|(n, u)| {
            let row = sealed_row(&sealed, &u.path);
            let new = if row.severity == "low" {
                "medium"
            } else {
                "low"
            };
            edit_on(n + 1, &row, EditField::Severity, &row.severity, new)
        })
        .collect();
    let reviewed = crate::report_export::apply_overlay(&sealed, &[], &edits, &[]);
    let released = release(package_of_reviewed(&reviewed));
    assert!(released.is_ok(), "severity edit: {:?}", released.err());
}
