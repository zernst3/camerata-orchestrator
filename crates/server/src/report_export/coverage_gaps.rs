//! H4: what a scan did NOT cover, stated by path and by pass, so nothing is lost silently.
//!
//! Two kinds of gap reach the Coverage section of the PDF, the workbook's Coverage sheet and
//! `findings.json`, all from the one [`coverage_gaps`] computation:
//!
//! - an AI scan pass that failed (a transport error, a refusal, or a reply that was still not
//!   JSON after its bounded retries) reads "pass failed, N files not reviewed by the AI pass",
//!   from the existing failed-pass record ([`crate::ai_audit::FailedPass::coverage_line`]);
//! - an auditable file the walk did not read (over the 400 KB size limit, or past the
//!   20,000-file limit) is listed by repo and path, from the scan ledger's input facts
//!   ([`crate::scan_ledger::ScanInputFacts::skipped_files`]).

use serde::Serialize;

use crate::scan_ledger::SkipReason;

/// The Coverage gaps for one report. Empty lists on a scan that covered everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CoverageGapsJson {
    /// One line per failed AI scan pass over files:
    /// `"<repo>, <pass>: pass failed, N files not reviewed by the AI pass"`.
    pub failed_ai_passes: Vec<String>,
    /// Every auditable file the scan did not read, in walk order per repo.
    pub skipped_files: Vec<SkippedFileJson>,
}

/// One file the scan did not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFileJson {
    pub repo: String,
    pub path: String,
    /// Machine-readable reason: `oversize` or `beyond_file_cap`.
    pub reason_code: SkipReason,
    /// The client-facing reason, as the Coverage section states it.
    pub reason: String,
}

impl CoverageGapsJson {
    pub fn is_empty(&self) -> bool {
        self.failed_ai_passes.is_empty() && self.skipped_files.is_empty()
    }
}

/// The Coverage gaps for `report`. Pure, and the only derivation: the PDF data, the workbook
/// and `findings.json` all call it (or reuse its output), so they cannot disagree.
pub fn coverage_gaps(report: &crate::onboard::ScanReport) -> CoverageGapsJson {
    let failed_ai_passes = report
        .failed_passes
        .iter()
        .filter(|f| f.files_not_reviewed > 0)
        .map(crate::ai_audit::FailedPass::coverage_line)
        .collect();
    let skipped_files = report
        .ledger
        .input()
        .skipped_files
        .iter()
        .map(|s| SkippedFileJson {
            repo: s.repo.clone(),
            path: s.path.clone(),
            reason_code: s.reason,
            reason: s.reason.label().to_string(),
        })
        .collect();
    CoverageGapsJson {
        failed_ai_passes,
        skipped_files,
    }
}
