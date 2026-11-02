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

use crate::scan_ledger::{FileCapHit, SkipReason, SkippedTotals, SKIPPED_FILES_LISTED_PER_REASON};

/// The pass name a whole-repo AI review failure is recorded under (the intake side writes it,
/// every exporter reads it). Stable: a persisted report carries it.
pub const AI_REVIEW_ALL_PASSES: &str = "AI review (all passes)";

/// The repos the advisory language-model tier did not run for, in report order. Two sources:
/// a run-wide block or no model call at all (the whole AI review produced nothing, so every repo
/// is listed) and a whole-repo failure recorded as a failed pass named [`AI_REVIEW_ALL_PASSES`].
pub fn repos_without_ai_review(report: &crate::onboard::ScanReport) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |repo: &str| {
        if !out.iter().any(|r| r == repo) {
            out.push(repo.to_string());
        }
    };
    // `provenance.audit_model` is stamped only when a model call was made, so `None` means no
    // model reviewed anything in this run (a deterministic-only scan, or every call blocked).
    //
    // An `ai_error` alone does not list every repo: it is the first whole-repo failure, and the
    // other repos may have been reviewed and carry findings. The repos that failed are the ones
    // with an [`AI_REVIEW_ALL_PASSES`] row below, so only those are listed. Saying the model did
    // not run for a repo whose AI findings are in the table would be a false claim the other way.
    //
    // The exception is an `ai_error` with no row at all (a report saved before the rows existed):
    // nothing says which repo failed, so none is assumed to have been reviewed.
    let has_row = report
        .failed_passes
        .iter()
        .any(|f| f.pass == AI_REVIEW_ALL_PASSES);
    if report.ai_blocked_reason.is_some()
        || report.provenance.audit_model.is_none()
        || (report.ai_error.is_some() && !has_row)
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
    /// One sentence counting every file the scan did not read, by reason, named or not ("12 files
    /// were not scanned: 3 over the 400 KB file size limit, 9 beyond the 20,000-file limit.").
    /// `None` when every auditable file was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_summary: Option<String>,
    /// One sentence per repo that reached the file limit, with how many of its files were not
    /// scanned. The limit is a coverage limit on the run, not only a list of paths.
    pub file_limit_notes: Vec<String>,
    /// One sentence saying how many absorbed sites were not checked against their files (M5), and
    /// why. `None` when every absorbed site was checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_note: Option<String>,
    /// The sentence every renderer prints for them ("and K more skipped files ..."); `None`
    /// when the list is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_beyond_cap_note: Option<String>,
    /// H10: the rules that were checked against only the files in their language or declared
    /// scope, with how many, summed over the repos of the scan. Empty when every rule saw every
    /// file it could apply to.
    #[serde(default)]
    pub scope_limited_rules: Vec<ScopeLimitedRuleJson>,
    /// The one sentence every renderer prints for them; `None` when the list is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_limited_note: Option<String>,
}

/// One rule that was checked against only part of the files, because of its scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeLimitedRuleJson {
    pub rule_id: String,
    /// Files the rule was checked against.
    pub files_checked: usize,
    /// Files the audit was handed: every file of the repositories, or on an incremental run
    /// only the files changed since the earlier inspection ([`Self::changed_files_only`]).
    pub files_total: usize,
    /// True when the counts are over the changed files of an incremental run, not all files.
    pub changed_files_only: bool,
    /// `language` or `declared`.
    pub scope: String,
}

/// The sentence for `rules` (non-empty), in plain words.
fn scope_limited_sentence(rules: &[ScopeLimitedRuleJson]) -> Option<String> {
    let first = rules.first()?;
    let count = rules.len();
    Some(format!(
        "{count} {} checked only against the files in {} scope, not every file in the \
         repositories (for example {}: {} of {}). The counts per rule are on the workbook's \
         Coverage sheet and in findings.json.",
        if count == 1 { "rule was" } else { "rules were" },
        if count == 1 { "its" } else { "their" },
        first.rule_id,
        first.files_checked,
        if first.changed_files_only {
            format!("the {} changed files", first.files_total)
        } else {
            format!("{} files", first.files_total)
        },
    ))
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

/// A file size as Coverage prints it: bytes under a kilobyte, whole kilobytes (1,000 bytes) under
/// a megabyte, then megabytes to one decimal. Decimal units, like the "400 KB" file limit.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1_000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{} KB", (bytes + 500) / 1_000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

/// `n` with a comma between each group of three digits ("20,000").
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn files_noun(n: usize) -> &'static str {
    if n == 1 {
        "file was"
    } else {
        "files were"
    }
}

/// The sentence for `exempt` absorbed sites the evidence check did not apply to. `None` when none.
fn evidence_note_sentence(exempt: usize) -> Option<String> {
    (exempt > 0).then(|| {
        let (noun, pronoun) = if exempt == 1 {
            ("site was", "its file")
        } else {
            ("sites were", "their files")
        };
        format!(
            "{exempt} absorbed {noun} not checked against {pronoun}: they describe an impact \
             rather than quote code, belong to a summary row, sit in another repository or cite \
             no line."
        )
    })
}

/// The file limit a hit applied: the one it recorded, or the production limit for a ledger
/// written before it was recorded.
fn limit_of(hit: &FileCapHit) -> usize {
    if hit.limit == 0 {
        crate::onboard::files::HARD_CAP_FILES
    } else {
        hit.limit
    }
}

/// How a file past the limit of `repo` is described: "beyond the 3-file limit", naming the limit
/// that was applied. With no record of a hit for the repo (a ledger that predates it) the
/// production limit is named, which is what [`SkipReason::label`] says.
fn beyond_limit_label(hits: &[FileCapHit], repo: &str) -> String {
    match hits.iter().find(|h| h.repo == repo) {
        Some(hit) => format!("beyond the {}-file limit", group_digits(limit_of(hit))),
        None => SkipReason::BeyondFileCap.label().to_string(),
    }
}

/// The one sentence counting every file the scan did not read, by reason. `None` when none.
fn skipped_summary_sentence(totals: &SkippedTotals, hits: &[FileCapHit]) -> Option<String> {
    let total = totals.total();
    if total == 0 {
        return None;
    }
    // The limit named is the one every hit applied; repos that applied different limits are
    // summed under a plain "beyond the file limit".
    let beyond = match hits.split_first() {
        None => SkipReason::BeyondFileCap.label().to_string(),
        Some((first, rest)) if rest.iter().all(|h| limit_of(h) == limit_of(first)) => {
            format!("beyond the {}-file limit", group_digits(limit_of(first)))
        }
        Some(_) => "beyond the file limit".to_string(),
    };
    let parts: Vec<String> = [
        SkipReason::Oversize,
        SkipReason::BeyondFileCap,
        SkipReason::NotText,
    ]
    .into_iter()
    .filter(|r| totals.of(*r) > 0)
    .map(|r| {
        let label = if r == SkipReason::BeyondFileCap {
            beyond.as_str()
        } else {
            r.label()
        };
        format!("{} {label}", totals.of(r))
    })
    .collect();
    Some(format!(
        "{total} {} not scanned: {}.",
        files_noun(total),
        parts.join(", ")
    ))
}

/// One sentence per repo that reached the file limit, naming the limit that was applied and the
/// number of files not scanned.
fn file_limit_sentences(hits: &[FileCapHit]) -> Vec<String> {
    hits.iter()
        .map(|h| {
            let limit = group_digits(limit_of(h));
            format!(
                "{} reached the {limit}-file limit, so the scan read the first {limit} files in \
                 path order. {} {} not scanned.",
                h.repo,
                h.not_scanned,
                files_noun(h.not_scanned),
            )
        })
        .collect()
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
    /// The file's size in bytes, when the walk could measure it.
    pub size_bytes: Option<u64>,
    /// The size as the Coverage section prints it ("612 KB"); empty when unknown.
    pub size: String,
}

impl CoverageGapsJson {
    pub fn is_empty(&self) -> bool {
        self.failed_ai_passes.is_empty()
            && self.dependency_gaps.is_empty()
            && self.skipped_files.is_empty()
            && self.skipped_beyond_cap_total == 0
            && self.skipped_summary.is_none()
            && self.file_limit_notes.is_empty()
            && self.evidence_note.is_none()
            && self.scope_limited_rules.is_empty()
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
            reason: if s.reason == SkipReason::BeyondFileCap {
                beyond_limit_label(&input.file_cap_hits, &s.repo)
            } else {
                s.reason.label().to_string()
            },
            size_bytes: s.bytes,
            size: s.bytes.map(format_size).unwrap_or_default(),
        })
        .collect();
    let scope_limited_rules: Vec<ScopeLimitedRuleJson> = report
        .ledger
        .scope_limited_rules()
        .into_iter()
        .map(|(rule_id, scope)| ScopeLimitedRuleJson {
            rule_id: rule_id.to_string(),
            files_checked: scope.checked,
            files_total: scope.of,
            changed_files_only: scope.changed_files_only,
            scope: scope.kind.clone(),
        })
        .collect();
    CoverageGapsJson {
        failed_ai_passes,
        dependency_gaps,
        skipped_files,
        skipped_beyond_cap_total: input.skipped_beyond_cap_total,
        skipped_beyond_cap_note: skipped_beyond_cap_sentence(input.skipped_beyond_cap_total),
        skipped_summary: skipped_summary_sentence(&input.skipped_totals, &input.file_cap_hits),
        file_limit_notes: file_limit_sentences(&input.file_cap_hits),
        evidence_note: evidence_note_sentence(report.ledger.evidence_check().site_exempt),
        scope_limited_note: scope_limited_sentence(&scope_limited_rules),
        scope_limited_rules,
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
