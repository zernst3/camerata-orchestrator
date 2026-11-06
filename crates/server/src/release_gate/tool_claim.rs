//! The summary's claim about preview tools matches the ledger.
//!
//! The executive summary tells a client "Some selected checks had no scan-time preview tool
//! available" when a selected rule has no tool wired and the ledger does not show it as run. That
//! sentence is a claim about what the report leaves out, so it must be true both ways: it is
//! present exactly when at least one such rule exists. A rule with no preview tool is judged by
//! the model and the ledger records it as run, so a summary that called it unchecked would
//! understate the report, and a ledger gap that the summary never mentions would hide one.

use super::Problem;
use crate::onboard::ScanReport;
use crate::report_export::{AuditReportJson, NO_PREVIEW_TOOL_LINE};

/// Whether the ledger leaves the rule named by an "unrouted" note unchecked: no entry of it ran.
fn left_unchecked(report: &ScanReport, rule_id: &str) -> bool {
    !report.ledger.rules().any(|e| e.rule_id == rule_id && e.ran)
}

/// The problems of a summary whose preview-tool claim and the ledger disagree: none when they
/// agree.
pub fn problems(report: &ScanReport, report_json: &AuditReportJson) -> Vec<Problem> {
    let unchecked: Vec<&str> = report
        .coverage_notes
        .iter()
        .filter_map(crate::scan_tools::unrouted_note_rule)
        .filter(|rule| left_unchecked(report, rule))
        .collect();
    let claimed = report_json
        .executive_summary
        .failed_passes
        .iter()
        .any(|line| line == NO_PREVIEW_TOOL_LINE);
    match (claimed, unchecked.is_empty()) {
        (true, true) => vec![Problem::new(
            "findings.json",
            "executive summary",
            "the summary says selected checks had no scan-time preview tool, but the ledger \
             shows every rule with no tool as run",
            None,
        )],
        (false, false) => vec![Problem::new(
            "findings.json",
            "executive summary",
            format!(
                "the ledger shows {} not run for want of a scan-time preview tool, and the \
                 summary does not say so",
                unchecked.join(", ")
            ),
            None,
        )],
        _ => Vec::new(),
    }
}
