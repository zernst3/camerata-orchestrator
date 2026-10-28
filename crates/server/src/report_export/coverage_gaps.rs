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

use crate::scan_ledger::{SkipReason, SKIPPED_FILES_LISTED_PER_REASON};

/// The pass name a whole-repo AI review failure is recorded under (the intake side writes it,
/// every exporter reads it). Stable: a persisted report carries it.
pub const AI_REVIEW_ALL_PASSES: &str = "AI review (all passes)";

/// The repos the advisory language-model tier did not run for, in report order. Two sources:
/// a run-wide block or error (the whole AI review produced nothing, so every repo is listed)
/// and a whole-repo failure recorded as a failed pass named [`AI_REVIEW_ALL_PASSES`].
pub fn repos_without_ai_review(report: &crate::onboard::ScanReport) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |repo: &str| {
        if !out.iter().any(|r| r == repo) {
            out.push(repo.to_string());
        }
    };
    if report.ai_blocked_reason.is_some() || report.ai_error.is_some() {
        for repo in &report.repos {
            add(repo);
        }
    }
    for f in &report.failed_passes {
        if f.pass == AI_REVIEW_ALL_PASSES {
            add(&f.repo);
        }
    }
    out
}

/// The Coverage gaps for one report. Empty lists on a scan that covered everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CoverageGapsJson {
    /// One line per failed AI scan pass over files:
    /// `"<repo>, <pass>: pass failed, N files not reviewed by the AI pass"`.
    pub failed_ai_passes: Vec<String>,
    /// The auditable files the scan did not read, in walk order per repo: at most
    /// [`SKIPPED_FILES_LISTED_PER_REASON`] per reason.
    pub skipped_files: Vec<SkippedFileJson>,
    /// Skipped files past that bound, counted but not named.
    pub skipped_beyond_cap_total: usize,
    /// The sentence every renderer prints for them ("and K more skipped files ..."); `None`
    /// when the list is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_beyond_cap_note: Option<String>,
}

/// The "and K more" sentence for `omitted` skipped files that are named nowhere.
pub fn skipped_beyond_cap_sentence(omitted: usize) -> Option<String> {
    (omitted > 0).then(|| {
        format!(
            "and {omitted} more skipped {}, not listed (each list names the first {} per reason)",
            if omitted == 1 { "file" } else { "files" },
            SKIPPED_FILES_LISTED_PER_REASON
        )
    })
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
        self.failed_ai_passes.is_empty()
            && self.skipped_files.is_empty()
            && self.skipped_beyond_cap_total == 0
    }
}

/// The Coverage gaps for `report`. Pure, and the only derivation: the PDF data, the workbook
/// and `findings.json` all call it (or reuse its output), so they cannot disagree.
pub fn coverage_gaps(report: &crate::onboard::ScanReport) -> CoverageGapsJson {
    let mut failed_ai_passes: Vec<String> = report
        .failed_passes
        .iter()
        .filter(|f| f.files_not_reviewed > 0)
        .map(crate::ai_audit::FailedPass::coverage_line)
        .collect();
    // A repo the AI tier did not run for gets a line even when no file count was recorded.
    for repo in repos_without_ai_review(report) {
        let already_listed = report
            .failed_passes
            .iter()
            .any(|f| f.repo == repo && f.pass == AI_REVIEW_ALL_PASSES && f.files_not_reviewed > 0);
        if !already_listed {
            failed_ai_passes.push(format!(
                "{repo}, {AI_REVIEW_ALL_PASSES}: the advisory language-model review did not run"
            ));
        }
    }
    let input = report.ledger.input();
    let skipped_files = input
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
        skipped_beyond_cap_total: input.skipped_beyond_cap_total,
        skipped_beyond_cap_note: skipped_beyond_cap_sentence(input.skipped_beyond_cap_total),
    }
}
