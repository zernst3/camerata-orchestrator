//! The release gate refuses a site the Engine Check said No to until it is corrected (review-tree
//! design A2b).
//!
//! The Engine Check is the reviewer's internal quality control of the engine: "did the engine get
//! this right?". A No on a site with no correction yet is a finding the engine got wrong, and the
//! client must not receive it as it stands.

use std::collections::HashSet;

use super::Problem;
use crate::onboard::Finding;
use crate::report_export::ReviewedReport;

/// The rows the Engine Check said No to that nothing has corrected: [`crate::engine_check::
/// uncorrected_keys`] over the evidence attached to `reviewed` and its live decisions, in the
/// order of the report's rows. A No on a site the report no longer holds is not here: the row is
/// not in the package, so nothing wrong can ship through it.
pub fn uncorrected_no<'a>(findings: &'a [Finding], reviewed: &ReviewedReport) -> Vec<&'a Finding> {
    let held: HashSet<String> = reviewed
        .engine_check()
        .uncorrected(reviewed.records())
        .into_iter()
        .collect();
    findings
        .iter()
        .filter(|f| held.contains(&f.site_key))
        .collect()
}

/// One problem per row the Engine Check said No to that nothing has corrected, and one problem
/// naming the Engine Check file when it cannot be read or parsed (fail closed: with the verdicts
/// unknown, no No can be ruled out).
pub fn problems(findings: &[Finding], reviewed: &ReviewedReport) -> Vec<Problem> {
    let mut problems: Vec<Problem> = Vec::new();
    if let Some(file) = reviewed.engine_check().unreadable_file() {
        problems.push(Problem::new(
            file,
            "Engine Check file",
            format!(
                "the Engine Check file {file} cannot be read or parsed, so the gate cannot tell \
                 which findings the Engine Check said No to. Restore the file (a damaged one is \
                 kept beside it with a .corrupt ending), then export again"
            ),
            None,
        ));
    }
    problems.extend(uncorrected_no(findings, reviewed).into_iter().map(|f| {
        Problem::new(
            "findings.json",
            format!("{} line {}", f.path, f.line),
            "the Engine Check said No to this finding and no correction, decision or later edit \
             is recorded for it. Correct it (mark it a false positive, change its severity or \
             edit its text) or decide it, then export again",
            Some(f.rule_id.clone()),
        )
    }));
    problems
}
