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
    // `provenance.audit_model` is stamped only when a model call was made, so `None` means no
    // model reviewed anything in this run (a deterministic-only scan, or every call blocked).
    if report.ai_blocked_reason.is_some()
        || report.ai_error.is_some()
        || report.provenance.audit_model.is_none()
    {
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
    /// M11/M12: dependency-audit gaps, one line each: the audit did not run, or it left
    /// manifests unaudited because no lockfile covers them. Empty when osv-scanner ran
    /// everywhere it could.
    pub dependency_gaps: Vec<String>,
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
            && self.dependency_gaps.is_empty()
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
    let dependency_gaps = dependency_gaps(report);
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
        dependency_gaps,
        skipped_files,
        skipped_beyond_cap_total: input.skipped_beyond_cap_total,
        skipped_beyond_cap_note: skipped_beyond_cap_sentence(input.skipped_beyond_cap_total),
    }
}

/// The dependency-audit gaps of `report`: every osv-scanner coverage note that does not prove a
/// clean check (see [`crate::dep_audit::note_proves_clean`]), as a sentence with its first
/// letter capitalised. A repo with no lockfiles at all is a completed check, not a gap.
pub fn dependency_gaps(report: &crate::onboard::ScanReport) -> Vec<String> {
    let mut gaps: Vec<String> = Vec::new();
    // `provenance.osv_scanner_version` is stamped only when the deterministic tier (and with it
    // the dependency audit) ran. `None` is an AI-only scan: nothing was checked, and no note
    // says so, because the audit never started.
    if report.provenance.osv_scanner_version.is_none() {
        gaps.push(
            "Dependency audit (osv-scanner) did not run in this scan, so known vulnerable \
             dependencies were not checked."
                .to_string(),
        );
    }
    gaps.extend(
        report
            .coverage_notes
            .iter()
            .filter(|n| n.tool == "osv-scanner")
            .filter(|n| !crate::dep_audit::note_proves_clean(n))
            .map(|n| {
                let message = n.message.trim();
                let mut chars = message.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }),
    );
    gaps
}
