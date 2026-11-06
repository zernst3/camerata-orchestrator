//! The gate holds the summary's claim about preview tools to the ledger.

use std::collections::HashMap;

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::ScanReport;
use crate::report_export::{build_report_json, NO_PREVIEW_TOOL_LINE};
use crate::scan_ledger::RuleTier;

fn json_of(report: &ScanReport) -> crate::report_export::AuditReportJson {
    let sealed = report.clone().seal();
    build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &client_options(),
    )
}

fn with_unrouted(rule: &str, ran: bool) -> ScanReport {
    let mut report = scan_report();
    report
        .coverage_notes
        .push(crate::scan_tools::unrouted_note(rule));
    report
        .ledger
        .record_rule("acme/portal", rule, RuleTier::Semantic, ran, None, 4, 0);
    report
}

#[test]
fn a_summary_that_says_nothing_beside_a_ledger_that_agrees_is_clean() {
    let report = with_unrouted("JAVASCRIPT-A-1", true);
    assert!(tool_claim::problems(&report, &json_of(&report)).is_empty());
}

#[test]
fn a_summary_claim_the_ledger_does_not_back_is_a_problem() {
    let report = with_unrouted("JAVASCRIPT-A-1", true);
    let mut json = json_of(&report);
    json.executive_summary
        .failed_passes
        .push(NO_PREVIEW_TOOL_LINE.to_string());
    let problems = tool_claim::problems(&report, &json);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].message.contains("ledger"), "{problems:?}");
}

#[test]
fn a_ledger_gap_the_summary_never_mentions_is_a_problem() {
    let report = with_unrouted("JAVASCRIPT-A-1", false);
    let mut json = json_of(&report);
    json.executive_summary
        .failed_passes
        .retain(|l| l != NO_PREVIEW_TOOL_LINE);
    let problems = tool_claim::problems(&report, &json);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].message.contains("JAVASCRIPT-A-1"),
        "{problems:?}"
    );
}

#[test]
fn the_producer_and_the_gate_agree_on_a_real_package() {
    for ran in [true, false] {
        let report = with_unrouted("JAVASCRIPT-A-1", ran);
        let json = json_of(&report);
        assert_eq!(
            json.executive_summary
                .failed_passes
                .iter()
                .any(|l| l == NO_PREVIEW_TOOL_LINE),
            !ran,
            "the line is there exactly when the ledger shows the rule not run"
        );
        assert!(tool_claim::problems(&report, &json).is_empty(), "ran={ran}");
    }
}

#[test]
fn the_gate_records_the_problem_under_the_content_check() {
    let report = with_unrouted("JAVASCRIPT-A-1", true);
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
    );
    let data = crate::report_export::render_data_json(&json).expect("data.json renders");
    let mut package = assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    assert!(package.tool_claim_problems.is_empty());
    package.tool_claim_problems = vec![Problem::new(
        "findings.json",
        "executive summary",
        "x",
        None,
    )];
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => {
            let content: Vec<_> = failure
                .report
                .checks
                .iter()
                .filter(|o| o.check == CheckId::Content)
                .flat_map(|o| o.problems.iter())
                .collect();
            assert_eq!(content.len(), 1, "{:?}", failure.report);
        }
        other => panic!("a false tool claim must block the release: {other:?}"),
    }
}
