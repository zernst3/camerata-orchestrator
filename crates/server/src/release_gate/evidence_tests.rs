//! M5 at the release gate: a package holding a row whose quoted evidence was not found is refused
//! unless the reviewer decided that row. A real package is built through `assemble_package`
//! around a stand-in PDF, like the gate's other tests, so these run without Typst.

use std::collections::HashMap;

use super::evidence::{unverified_without_decision, ReviewerDecisions};
use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::{build_report_json, finding_key, render_data_json, DispositionWire};

fn package_with(
    report: ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(&sealed, dispositions, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        dispositions,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn unverified_row() -> Finding {
    let mut f = Finding {
        repo: "acme/portal".to_string(),
        path: "lib/supabase/admin.ts".to_string(),
        line: 8,
        rule_id: "AI-RAW-SQL".to_string(),
        severity: "low".to_string(),
        snippet: "db.query(sql)".to_string(),
        detail: "Raw SQL \u{2014} Built from input.".to_string(),
        ..Finding::default()
    };
    f.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the snippet was not found within 3 lines of line 8".to_string(),
    );
    f
}

fn report_with(extra: Finding) -> ScanReport {
    let mut report = scan_report();
    report.findings.push(extra);
    report
}

fn decision(state: &str) -> HashMap<String, DispositionWire> {
    let row = unverified_row();
    HashMap::from([(
        finding_key(&row),
        DispositionWire {
            state: state.to_string(),
            reason: "read it".to_string(),
            ..DispositionWire::default()
        },
    )])
}

fn evidence_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::EvidenceUnverified)
        .flat_map(|o| o.problems.iter())
        .collect()
}

#[test]
fn an_unverified_row_with_no_reviewer_decision_refuses_the_export() {
    let package = package_with(report_with(unverified_row()), &HashMap::new());
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = evidence_problems(&failure);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            assert_eq!(problems[0].evidence.as_deref(), Some("AI-RAW-SQL"));
            assert!(
                problems[0].location.contains("lib/supabase/admin.ts"),
                "{}",
                problems[0].location
            );
            // The message names the row, so the operator knows which to open.
            assert!(failure.to_string().contains("lib/supabase/admin.ts line 8"));
        }
        other => panic!("an undecided unverified row must block the release: {other:?}"),
    }
}

#[test]
fn a_row_the_reviewer_decided_in_any_state_is_allowed() {
    for state in ["Confirmed", "Ignored", "TechDebt", "FalsePositive"] {
        let dispositions = decision(state);
        let package = package_with(report_with(unverified_row()), &dispositions);
        let released = release(package);
        assert!(released.is_ok(), "state {state}: {:?}", released.err());
    }
}

#[test]
fn an_empty_or_unreadable_state_is_not_a_decision() {
    for state in ["", "Unresolved", "NoSuchState"] {
        let dispositions = decision(state);
        let package = package_with(report_with(unverified_row()), &dispositions);
        assert!(
            matches!(release(package), Err(ReleaseError::Blocked(_))),
            "state {state:?}"
        );
    }
}

#[test]
fn a_decision_about_another_row_does_not_cover_this_one() {
    let mut other = unverified_row();
    other.line = 99;
    let dispositions = HashMap::from([(
        finding_key(&other),
        DispositionWire {
            state: "Confirmed".to_string(),
            ..DispositionWire::default()
        },
    )]);
    let package = package_with(report_with(unverified_row()), &dispositions);
    assert!(matches!(release(package), Err(ReleaseError::Blocked(_))));
}

#[test]
fn a_verified_row_is_never_affected() {
    let mut verified = unverified_row();
    verified.captures.clear();
    let package = package_with(report_with(verified), &HashMap::new());
    let released = release(package).unwrap_or_else(|e| panic!("{e}"));
    assert!(released
        .report
        .check_names()
        .contains(&"reviewer decision on unverified evidence"));
    assert!(released.report.is_clean());
}

/// The check asks a `ReviewerDecisions`, not a map: a different store answers the same question.
#[test]
fn the_check_takes_its_decisions_from_any_store() {
    struct DecidesEverything;
    impl ReviewerDecisions for DecidesEverything {
        fn has_decision(&self, _: &Finding) -> bool {
            true
        }
    }
    struct DecidesNothing;
    impl ReviewerDecisions for DecidesNothing {
        fn has_decision(&self, _: &Finding) -> bool {
            false
        }
    }
    let rows = [unverified_row()];
    assert!(unverified_without_decision(&rows, &DecidesEverything).is_empty());
    assert_eq!(unverified_without_decision(&rows, &DecidesNothing).len(), 1);
}

#[test]
fn the_refusal_names_the_absorbed_sites_that_failed() {
    let mut row = unverified_row();
    row.captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_SITES_CAPTURE.to_string(),
        "src/a.ts:3\nsrc/b.ts:9".to_string(),
    );
    let problems = unverified_without_decision(&[row], &HashMap::new());
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0]
            .message
            .contains("absorbed sites not verified: src/a.ts:3, src/b.ts:9"),
        "{}",
        problems[0].message
    );
}
