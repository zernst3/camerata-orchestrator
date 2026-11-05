//! The release gate refuses a site the Engine Check said No to until it is corrected (review-tree
//! design A2b).
//!
//! The Engine Check is the reviewer's internal quality control of the engine: "did the engine get
//! this right?". A No on a site with no correction yet is a finding the engine got wrong, and the
//! client must not receive it as it stands.
//!
//! That holds for a thrown-out finding too (design A10 item 2): a No on a row the engine threw out
//! says the engine threw out a real problem. The row is not in the package either way, so what is
//! held is the export: it is refused until the row is put back (it then sits in its rule node,
//! held as a restore until decided) or a correction confirms the throw-out was right.

use std::collections::HashSet;

use super::Problem;
use crate::onboard::Finding;
use crate::report_export::ReviewedReport;

/// The site keys the Engine Check said No to that nothing has corrected: [`crate::engine_check::
/// uncorrected_keys`] over the evidence attached to `reviewed` and its live decisions.
fn uncorrected_keys(reviewed: &ReviewedReport) -> HashSet<String> {
    reviewed
        .engine_check()
        .uncorrected(reviewed.records())
        .into_iter()
        .collect()
}

/// The kept rows the Engine Check said No to that nothing has corrected, in the order of the
/// report's rows. A No on a site the report no longer holds, kept or thrown out, is not here: the
/// row is not in the package, so nothing wrong can ship through it.
pub fn uncorrected_no<'a>(findings: &'a [Finding], reviewed: &ReviewedReport) -> Vec<&'a Finding> {
    let held = uncorrected_keys(reviewed);
    findings
        .iter()
        .filter(|f| held.contains(&f.site_key))
        .collect()
}

/// The thrown-out rows the Engine Check said No to that nothing has corrected, in the order of the
/// report's thrown-out rows. The row never ships, but the No says the engine threw out a real
/// problem, so the export waits until the reviewer puts it back or a correction confirms the
/// throw-out was right. A row put back leaves `discarded`; it is then a kept row.
pub fn uncorrected_no_thrown_out<'a>(
    discarded: &'a [Finding],
    reviewed: &ReviewedReport,
) -> Vec<&'a Finding> {
    uncorrected_no(discarded, reviewed)
}

/// One problem per row, kept or thrown out, the Engine Check said No to that nothing has
/// corrected, and one problem naming the Engine Check file when it cannot be read or parsed (fail
/// closed: with the verdicts unknown, no No can be ruled out).
pub fn problems(
    findings: &[Finding],
    discarded: &[Finding],
    reviewed: &ReviewedReport,
) -> Vec<Problem> {
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
    problems.extend(
        uncorrected_no_thrown_out(discarded, reviewed)
            .into_iter()
            .map(|f| {
                Problem::new(
                    THROWN_OUT_FILE,
                    format!("{} line {}", f.path, f.line),
                    "the engine threw this finding out and the Engine Check said No to that, so \
                     a real problem may be hidden, and no correction or decision is recorded for \
                     it. Put it back into review, or record a correction that confirms the \
                     throw-out was right, then export again",
                    Some(f.rule_id.clone()),
                )
            }),
    );
    problems
}

/// What a problem about a thrown-out row names as its file: the row is in no package file.
pub const THROWN_OUT_FILE: &str = "thrown-out findings";
