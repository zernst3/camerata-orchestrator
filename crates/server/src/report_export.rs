//! PDF audit-report export (brownfield audit hardening, Pass B).
//!
//! See `docs/design/2026-07-23_brownfield-audit-report.md` Part 2. This module is
//! deliberately TINY — a serializer (`build_report_json`, pure, unit-tested) + a Typst
//! compiler (`compile_pdf`, the only I/O). It is NOT a report subsystem: no persistence,
//! no theming, no multi-format output, no LLM call in the export path. Re-exporting a
//! report re-derives it from `last_scan` + the dispositions the client POSTs; nothing is
//! stored server-side.
//!
//! # The trust feature: the citation join (§4.4)
//! A [`crate::onboard::Finding`] carries only a bare `rule_id`. [`resolve_citation`] joins
//! that id against the loaded rule corpus (`camerata_rules::RuleSet`) to recover the
//! CWE/OWASP/RFC/linter sources a grounded rule cites (e.g. `SEC-NO-UNSAFE-DESERIALIZATION-1`
//! cites OWASP's Deserialization Cheat Sheet — see `crates/rules/principles/universal/
//! sec-no-unsafe-deserialization-1.toml`). A preview finding (the scan-time deterministic
//! tool pass) is labeled with the real tool that enforces it even absent a corpus entry.
//! Anything else (a free-text/AI-tier rule id the corpus never saw) is honestly labeled
//! "AI-advisory, model-inferred." — never dressed up as grounded.
//!
//! # FP exclusion (the explicit ask)
//! A finding the auditor dispositioned `FalsePositive` is excluded from EVERY section
//! (curated findings, matrix, scorecard) and counted exactly once, in the methodology
//! line — see [`build_report_json`]'s doc comment for the full partitioning rule.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::dep_audit::DEP_AUDIT_RULE_ID;
use crate::onboard::{Finding, ScanReport};

// ── Client-supplied inputs ──────────────────────────────────────────────────────

/// One finding's client-local triage disposition, as POSTed by the export button.
/// Deliberately a plain string-keyed DTO (not `camerata_ui_core::triage::TriageState`
/// directly) because `camerata-server` does not depend on `camerata-ui-core` (that crate
/// is UI-only). The string values are EXACTLY the variant names `TriageState`/
/// `TechDebtBucket`'s derived `Serialize` emits — `state` is one of `"Unresolved"` |
/// `"Ignored"` | `"TechDebt"` | `"FalsePositive"`; `bucket` is `"Later"` | `"Now"` (only
/// meaningful when `state == "TechDebt"`). An absent or unrecognized `state` is treated as
/// `Unresolved` (fail-soft — never silently drops a finding from the report).
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct DispositionWire {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub bucket: String,
}

/// Client-authored report framing. Nothing here is inferred — the auditor types it (or
/// leaves it blank; every field degrades to an empty/omitted value in the template).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ReportOptions {
    #[serde(default)]
    pub client_name: String,
    #[serde(default)]
    pub project_title: String,
    #[serde(default)]
    pub prepared_by: String,
    #[serde(default)]
    pub executive_summary_override: Option<String>,
}

/// Stable identity for a `Finding`, matching `camerata_ui_core::triage::finding_key`'s
/// wire format BYTE-FOR-BYTE (same fields, same order, same NUL separator) so a
/// disposition keyed off the client's `FindingView` looks up correctly here. The two
/// crates can't share the function directly (server does not depend on ui-core); the
/// round-trip test below (`finding_key_matches_ui_core_format`) pins the format so the two
/// can never silently drift apart.
pub fn finding_key(f: &Finding) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        f.repo, f.rule_id, f.path, f.line, f.snippet
    )
}

// ── Effective disposition (status + wire, reconciled) ───────────────────────────

/// The report's own disposition classification — richer than the wire `DispositionWire`
/// because it also reconciles `Finding.status == "suppressed-baseline"` (a PRIOR onboarding
/// run's accepted debt, persisted independently of this scan's client-local triage map).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// No triage decision yet this session (the default for an absent/unrecognized entry).
    Unresolved,
    /// Real, accepted risk — the client's `Ignored` disposition, this session.
    Ignored,
    /// Tech debt, pulled into the do-now bucket.
    TechDebtNow,
    /// Tech debt, planned for later.
    TechDebtLater,
    /// A pre-existing baseline suppression from a PRIOR run — accepted debt, but NOT a
    /// finding newly surfaced by this scan. Distinct from `Ignored` so the report never
    /// implies "the auditor just accepted this" when really "this was already accepted."
    BaselineAccepted,
}

/// Classify one finding's effective disposition: `Finding.status` (baseline suppression,
/// a durable PRIOR decision) takes priority over an absent client disposition; an explicit
/// THIS-SESSION disposition from the client always wins over the status default, because it
/// represents the auditor's fresh, deliberate call (e.g. re-flagging a previously-suppressed
/// finding as tech debt this round). Unrecognized/malformed wire values fail soft to
/// `Unresolved` — never silently excluded.
fn classify(finding: &Finding, wire: Option<&DispositionWire>) -> Disposition {
    match wire {
        Some(d) => match d.state.as_str() {
            "Ignored" => Disposition::Ignored,
            "TechDebt" => {
                if d.bucket == "Now" {
                    Disposition::TechDebtNow
                } else {
                    Disposition::TechDebtLater
                }
            }
            "FalsePositive" => unreachable!(
                "FalsePositive is partitioned out before classify() is called; see build_report_json"
            ),
            _ => Disposition::Unresolved,
        },
        None if finding.status == "suppressed-baseline" => Disposition::BaselineAccepted,
        None => Disposition::Unresolved,
    }
}

/// Human-facing disposition annotation shown next to each curated-finding site row.
fn disposition_label(disposition: Disposition, reason: &str) -> String {
    match disposition {
        Disposition::Unresolved => "Open".to_string(),
        Disposition::Ignored => {
            if reason.trim().is_empty() {
                "Accepted risk".to_string()
            } else {
                format!("Accepted risk: {reason}")
            }
        }
        Disposition::TechDebtNow => "Tech debt \u{2014} resolve now".to_string(),
        Disposition::TechDebtLater => "Tech debt \u{2014} planned (resolve later)".to_string(),
        Disposition::BaselineAccepted => {
            "Pre-existing accepted debt (baseline suppression)".to_string()
        }
    }
}

// ── Output shape ─────────────────────────────────────────────────────────────────

/// One audited repo's git identity for the cover page. Mirrors `onboard::AuditedRef` plus
/// a pre-truncated `short_sha` (the template has no string-slicing primitive worth trusting
/// for this, so the serializer does it once here).
#[derive(Debug, Clone, Serialize)]
pub struct AuditedRefJson {
    pub repo: String,
    pub sha: Option<String>,
    pub short_sha: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverJson {
    pub repos: Vec<String>,
    pub files_scanned: usize,
    pub files_excluded: usize,
    pub code_chars: usize,
    pub audited_refs: Vec<AuditedRefJson>,
    pub audit_model: Option<String>,
    pub calibration_model: Option<String>,
    pub camerata_version: String,
    /// RFC3339 timestamp of when this PDF was generated (NOT the scan time — that's
    /// `audited_refs`/provenance; this is "as-of" for the export itself).
    pub generated_at: String,
    pub client_name: String,
    pub project_title: String,
    pub prepared_by: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutiveSummaryJson {
    /// Deterministic template narrative, or the client's `executive_summary_override`
    /// verbatim when supplied. Never an LLM call either way.
    pub narrative: String,
    pub is_override: bool,
    pub candidates_reviewed: usize,
    pub excluded_false_positive: usize,
    pub curated_total: usize,
    pub do_now: usize,
    pub do_next: usize,
    pub plan: usize,
    pub accepted: usize,
    pub open: usize,
    /// Up to 3 one-liners: `"{rule_id} \u{2014} {path}:{line}"`.
    pub top_do_now: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryRowJson {
    pub category: String,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    /// `"Clean"` | `"Attention"` | `"Action-needed"` — a status CHIP, never a letter grade.
    pub status: String,
    /// Rules in this category that were actually audited this run (from
    /// `provenance.audited_rule_ids`), so "checked vs found" is visible.
    pub audited_rules: usize,
    /// Of the audited rules in this category, how many produced ZERO real (non-FP)
    /// findings — the category's own "clean rule" count.
    pub clean_rules: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScorecardJson {
    pub rows: Vec<CategoryRowJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingRefJson {
    pub rule_id: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub severity: String,
}

/// The severity×effort action matrix — the money page. Cell membership is driven by the
/// auditor's OWN disposition where one exists (a `TechDebt{Now}` finding is placed in
/// `do_now` regardless of its computed severity/effort quadrant — the human call wins over
/// the heuristic), and by severity×effort only for findings still `Unresolved`/open. See
/// [`matrix_bucket`].
#[derive(Debug, Clone, Serialize, Default)]
pub struct MatrixJson {
    pub do_now: Vec<FindingRefJson>,
    pub do_next: Vec<FindingRefJson>,
    pub plan: Vec<FindingRefJson>,
    pub accepted: Vec<FindingRefJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CitationSourceJson {
    pub title: String,
    pub url: String,
    pub linter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CitationJson {
    /// `"grounded"` (corpus-cited) | `"preview"` (deterministic tool, no corpus citation) |
    /// `"advisory"` (AI-tier, no corpus entry at all).
    pub kind: String,
    /// One-line, template-ready label (e.g. "AI-advisory, model-inferred.").
    pub label: String,
    pub sources: Vec<CitationSourceJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedSiteJson {
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub snippet: String,
    pub detail: String,
    pub severity: String,
    pub effort: Option<String>,
    pub confidence: Option<String>,
    pub disposition: String,
    pub also_matches: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedGroupJson {
    pub rule_id: String,
    pub title: String,
    pub citation: CitationJson,
    pub sites: Vec<CuratedSiteJson>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthyRuleJson {
    pub rule_id: String,
    pub title: String,
    pub citation: CitationJson,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhatsHealthyJson {
    /// Audited rules with zero real (non-FP) findings this run.
    pub rules: Vec<HealthyRuleJson>,
    /// True when the dependency pass ran and found nothing — a §6 positive per the design
    /// doc ("Clean = a §6 positive").
    pub dependency_clean: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyFindingJson {
    /// `<package>@<version>`.
    pub package: String,
    pub advisory: String,
    pub severity: String,
    pub repo: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencySnapshotJson {
    pub rows: Vec<DependencyFindingJson>,
    /// Honesty line(s) about dep-audit coverage (e.g. osv-scanner unavailable this run).
    pub coverage_notes: Vec<String>,
    pub clean: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodologyJson {
    pub candidates_reviewed: usize,
    pub excluded_false_positive: usize,
    pub deterministic_note: String,
    pub ai_tier_note: String,
    pub not_done: Vec<String>,
    pub severity_scale_note: String,
}

/// The serializer's single output type — mirrors the §4 anatomy sections in
/// `docs/design/2026-07-23_brownfield-audit-report.md` in order.
#[derive(Debug, Clone, Serialize)]
pub struct AuditReportJson {
    pub cover: CoverJson,
    pub executive_summary: ExecutiveSummaryJson,
    pub scorecard: ScorecardJson,
    pub matrix: MatrixJson,
    pub curated_findings: Vec<CuratedGroupJson>,
    pub whats_healthy: WhatsHealthyJson,
    pub dependency_snapshot: DependencySnapshotJson,
    pub methodology: MethodologyJson,
    pub disclaimer: String,
}

/// General-audit variant of `ai_audit::DEEP_ADVISORY_DISCLAIMER` / the deep-report export's
/// `DEEP_REPORT_ADVISORY` (`lib.rs`) — same honesty posture, worded for the standard
/// (non-deep-tier) brownfield audit this report covers.
pub const AUDIT_REPORT_DISCLAIMER: &str =
    "ADVISORY: This report combines a DETERMINISTIC security floor (hardcoded secrets, raw \
     SQL concatenation, unsafe deserialization, disabled TLS, and similar proven-defect \
     classes — always critical, never a false alarm by construction) with an AI-INFERRED \
     architectural review (calibrated for confidence/effort, human-triaged before this report \
     was generated). It is NOT a penetration test, NOT a SOC-2 attestation, and NOT a \
     substitute for a qualified security assessment. Every finding requires human review \
     before action. Camerata makes no guarantee of completeness or accuracy.";

// ── Citation join (§4.4) ─────────────────────────────────────────────────────────

/// Join `rule_id` (+ `preview_tool`, when set) against the loaded corpus to recover its
/// authoritative sources. Priority: (1) a corpus rule with non-empty `sources` is
/// "grounded" — cite them verbatim (CWE/OWASP/RFC/linter docs); (2) a deterministic
/// preview finding with no corpus citation is labeled by the REAL tool that enforces it
/// (still honest — just not corpus-documented); (3) anything else (a free-text/AI-tier
/// rule id the corpus never saw) is "AI-advisory, model-inferred." — never dressed up.
fn resolve_citation(
    rule_id: &str,
    preview_tool: Option<&str>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> CitationJson {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(rule_id)) {
        if !rule.sources.is_empty() {
            let sources: Vec<CitationSourceJson> = rule
                .sources
                .iter()
                .map(|s| CitationSourceJson {
                    title: s.title.clone(),
                    url: s.url.clone(),
                    linter: s.linter.clone(),
                })
                .collect();
            let label = sources
                .iter()
                .map(|s| s.title.clone())
                .collect::<Vec<_>>()
                .join("; ");
            return CitationJson {
                kind: "grounded".to_string(),
                label,
                sources,
            };
        }
    }
    if let Some(tool) = preview_tool {
        return CitationJson {
            kind: "preview".to_string(),
            label: format!(
                "Deterministic preview \u{2014} enforced by {tool} ({rule_id}); not yet wired into the CI gate"
            ),
            sources: Vec::new(),
        };
    }
    CitationJson {
        kind: "advisory".to_string(),
        label: "AI-advisory, model-inferred.".to_string(),
        sources: Vec::new(),
    }
}

/// Category for the scorecard: the corpus rule's own `domain` when known, else the
/// rule id's leading hyphen-separated token lowercased (`"SEC-NO-..."` -> `"sec"`). A
/// fallback, not a taxonomy — good enough for a scorecard grouping, not load-bearing.
fn category_for(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> String {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(rule_id)) {
        return rule.domain.clone();
    }
    rule_id
        .split('-')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("other")
        .to_ascii_lowercase()
}

/// Which matrix cell a finding lands in. The auditor's OWN disposition wins where one
/// exists (`Ignored`/`BaselineAccepted` -> Accepted; `TechDebt{Now}` -> Do now;
/// `TechDebt{Later}` -> Plan); only an `Unresolved` (still-open) finding falls back to the
/// computed severity×effort quadrant (`Do now` = high severity + low effort; `Do next` =
/// high severity + anything else; `Plan` = medium/low severity, any effort). Missing effort
/// (deterministic-floor / preview findings calibration never saw) is treated as "not low" —
/// conservative, so an uncalibrated critical finding never gets silently downgraded to a
/// same-day fix.
fn matrix_bucket(finding: &Finding, disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Ignored | Disposition::BaselineAccepted => "accepted",
        Disposition::TechDebtNow => "do_now",
        Disposition::TechDebtLater => "plan",
        Disposition::Unresolved => {
            let high_severity = matches!(finding.severity.as_str(), "critical" | "high");
            let low_effort = matches!(finding.effort.as_deref(), Some("low"));
            match (high_severity, low_effort) {
                (true, true) => "do_now",
                (true, false) => "do_next",
                (false, _) => "plan",
            }
        }
    }
}

fn finding_ref(f: &Finding) -> FindingRefJson {
    FindingRefJson {
        rule_id: f.rule_id.clone(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        severity: f.severity.clone(),
    }
}

/// Deterministic exec-summary narrative (never an LLM call). Overridden verbatim by
/// `ReportOptions::executive_summary_override` when the client supplies one.
fn default_narrative(
    candidates_reviewed: usize,
    excluded_fp: usize,
    curated_total: usize,
    do_now: usize,
    do_next: usize,
    plan: usize,
    accepted: usize,
    open: usize,
) -> String {
    // Deliberately ONE prose paragraph, no embedded list — `top_do_now` is a separate
    // structured field the template renders as its own bulleted list (a Typst string value
    // doesn't reliably turn embedded "\n"s into paragraph/list breaks, so mixing prose and
    // list markup into one opaque string would render as a flat run-on in the PDF).
    let mut s = format!(
        "{candidates_reviewed} candidate finding(s) were reviewed; {excluded_fp} were \
         dispositioned as false positives by the auditor and excluded entirely from this \
         report. Of the remaining {curated_total} curated finding(s): {do_now} do now, \
         {do_next} do next, {plan} planned, {accepted} accepted as risk"
    );
    if open > 0 {
        s.push_str(&format!(", and {open} still open/unresolved"));
    }
    s.push('.');
    s
}

/// Build the report's single output type from a completed scan + the client's triage
/// dispositions + the (optional, best-effort) rule corpus. **Pure** — no I/O, fully
/// unit-testable.
///
/// # FP exclusion / partitioning (design doc §"FP exclusion in the serializer")
/// Every finding is partitioned by `dispositions.get(finding_key(f))`:
/// - `FalsePositive` -> EXCLUDED from every section (findings, scorecard, matrix,
///   what's-healthy pool), counted ONCE in the methodology line.
/// - `Ignored` -> included in curated findings as "accepted risk" (+ reason); matrix
///   cell `Accepted`.
/// - `TechDebt{Now}` -> matrix cell `Do now`; `TechDebt{Later}` -> matrix cell `Plan`.
/// - `Unresolved` (or absent) -> "Open"; matrix cell from the computed severity×effort
///   quadrant. A draft mid-engagement report (some findings still open) is legitimate —
///   this never blocks the export.
/// - `Finding.status == "suppressed-baseline"` with NO client disposition this session ->
///   pre-existing accepted debt (not a new finding), same matrix treatment as `Ignored`.
///
/// `DEP-AUDIT-1` findings are carved out of the scorecard/matrix/curated-findings
/// entirely — they get their OWN table (§7 `dependency_snapshot`); mixing "your code has a
/// SQL-concat bug" and "your lockfile pins a CVE'd package" into one table would blur two
/// very different remediation types (edit code vs. bump a version).
pub fn build_report_json(
    report: &ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> AuditReportJson {
    let candidates_reviewed = report.findings.len();

    // Partition: false positives out (counted once), everything else keeps its finding +
    // effective disposition for every downstream section.
    let mut excluded_fp = 0usize;
    let mut live: Vec<(&Finding, Disposition, String)> = Vec::new();
    for f in &report.findings {
        let wire = dispositions.get(&finding_key(f));
        if wire.map(|d| d.state.as_str()) == Some("FalsePositive") {
            excluded_fp += 1;
            continue;
        }
        let disposition = classify(f, wire);
        let reason = wire.map(|d| d.reason.clone()).unwrap_or_default();
        live.push((f, disposition, reason));
    }

    // Dependency findings get their own §7 lane — carve them out of every other section.
    let (dep_findings, code_findings): (Vec<_>, Vec<_>) = live
        .into_iter()
        .partition(|(f, _, _)| f.rule_id == DEP_AUDIT_RULE_ID);

    // ── Matrix + curated findings + scorecard, over code_findings only ───────────
    let mut matrix = MatrixJson::default();
    for (f, disposition, _) in &code_findings {
        let bucket = matrix_bucket(f, *disposition);
        let target = match bucket {
            "do_now" => &mut matrix.do_now,
            "do_next" => &mut matrix.do_next,
            "plan" => &mut matrix.plan,
            _ => &mut matrix.accepted,
        };
        target.push(finding_ref(f));
    }

    // Curated findings: grouped by rule (sorted for deterministic output), each rule's
    // sites sorted by repo/path/line.
    let mut by_rule: std::collections::BTreeMap<String, Vec<(&Finding, Disposition, String)>> =
        std::collections::BTreeMap::new();
    for (f, disposition, reason) in &code_findings {
        by_rule
            .entry(f.rule_id.clone())
            .or_default()
            .push((f, *disposition, reason.clone()));
    }
    let mut curated_findings: Vec<CuratedGroupJson> = Vec::new();
    for (rule_id, mut sites) in by_rule {
        sites.sort_by(|a, b| {
            (&a.0.repo, &a.0.path, a.0.line).cmp(&(&b.0.repo, &b.0.path, b.0.line))
        });
        let preview_tool = sites.iter().find_map(|(f, _, _)| f.preview_tool.as_deref());
        let citation = resolve_citation(&rule_id, preview_tool, corpus);
        let title = corpus
            .and_then(|c| c.get_by_id(&rule_id))
            .map(|r| r.title.clone())
            .unwrap_or_else(|| rule_id.clone());
        let site_jsons = sites
            .iter()
            .map(|(f, disposition, reason)| CuratedSiteJson {
                repo: f.repo.clone(),
                path: f.path.clone(),
                line: f.line,
                snippet: f.snippet.clone(),
                detail: f.detail.clone(),
                severity: f.severity.clone(),
                effort: f.effort.clone(),
                confidence: f.confidence.clone(),
                disposition: disposition_label(*disposition, reason),
                also_matches: f.also_matches.clone(),
            })
            .collect();
        curated_findings.push(CuratedGroupJson {
            rule_id,
            title,
            citation,
            sites: site_jsons,
        });
    }

    // Scorecard: group by category, over code_findings (all of them, so an accepted
    // high-severity item still shows up in the severity counts — only the STATUS chip
    // ignores accepted/baseline risk, so "Action-needed" means genuinely open exposure).
    let mut by_category: std::collections::BTreeMap<String, Vec<&(&Finding, Disposition, String)>> =
        std::collections::BTreeMap::new();
    for entry in &code_findings {
        by_category
            .entry(category_for(&entry.0.rule_id, corpus))
            .or_default()
            .push(entry);
    }
    let mut scorecard_rows: Vec<CategoryRowJson> = Vec::new();
    for (category, entries) in &by_category {
        let mut critical = 0usize;
        let mut high = 0usize;
        let mut medium = 0usize;
        let mut low = 0usize;
        let mut open_high_or_critical = false;
        let mut open_medium = false;
        let mut rule_ids_with_findings: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for entry in entries.iter() {
            let f = entry.0;
            let disposition = entry.1;
            match f.severity.as_str() {
                "critical" => critical += 1,
                "high" => high += 1,
                "medium" => medium += 1,
                _ => low += 1,
            }
            rule_ids_with_findings.insert(f.rule_id.as_str());
            let still_open = matches!(
                disposition,
                Disposition::Unresolved | Disposition::TechDebtNow | Disposition::TechDebtLater
            );
            if still_open {
                match f.severity.as_str() {
                    "critical" | "high" => open_high_or_critical = true,
                    "medium" => open_medium = true,
                    _ => {}
                }
            }
        }
        let status = if open_high_or_critical {
            "Action-needed"
        } else if open_medium {
            "Attention"
        } else {
            "Clean"
        };
        let audited_in_category: Vec<&str> = report
            .provenance
            .audited_rule_ids
            .iter()
            .map(String::as_str)
            .filter(|rid| category_for(rid, corpus) == *category)
            .collect();
        let audited_rules = audited_in_category.len();
        let clean_rules = audited_in_category
            .iter()
            .filter(|rid| !rule_ids_with_findings.contains(*rid))
            .count();
        scorecard_rows.push(CategoryRowJson {
            category: category.clone(),
            critical,
            high,
            medium,
            low,
            status: status.to_string(),
            audited_rules,
            clean_rules,
        });
    }

    // ── What's healthy: audited rules with ZERO real (non-FP) findings ───────────
    let rule_ids_with_any_finding: std::collections::HashSet<&str> = code_findings
        .iter()
        .map(|(f, _, _)| f.rule_id.as_str())
        .collect();
    let mut healthy_rules: Vec<HealthyRuleJson> = report
        .provenance
        .audited_rule_ids
        .iter()
        .filter(|rid| !rule_ids_with_any_finding.contains(rid.as_str()))
        .map(|rid| HealthyRuleJson {
            rule_id: rid.clone(),
            title: corpus
                .and_then(|c| c.get_by_id(rid))
                .map(|r| r.title.clone())
                .unwrap_or_else(|| rid.clone()),
            citation: resolve_citation(rid, None, corpus),
        })
        .collect();
    healthy_rules.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));

    // ── §7 Dependency snapshot ─────────────────────────────────────────────────
    let dep_rows: Vec<DependencyFindingJson> = dep_findings
        .iter()
        .map(|(f, _, _)| DependencyFindingJson {
            package: f.snippet.clone(),
            advisory: f.detail.clone(),
            severity: f.severity.clone(),
            repo: f.repo.clone(),
        })
        .collect();
    let dep_coverage_notes: Vec<String> = report
        .coverage_notes
        .iter()
        .filter(|n| n.tool == "osv-scanner")
        .map(|n| n.message.clone())
        .collect();
    let dependency_clean = dep_rows.is_empty();
    let dependency_snapshot = DependencySnapshotJson {
        rows: dep_rows,
        coverage_notes: dep_coverage_notes,
        clean: dependency_clean,
    };

    // ── Executive summary ──────────────────────────────────────────────────────
    let curated_total = code_findings.len();
    let do_now = matrix.do_now.len();
    let do_next = matrix.do_next.len();
    let plan = matrix.plan.len();
    let accepted = matrix.accepted.len();
    let open = code_findings
        .iter()
        .filter(|(_, d, _)| *d == Disposition::Unresolved)
        .count();
    let mut do_now_sorted = matrix.do_now.clone();
    do_now_sorted.sort_by_key(|f| match f.severity.as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    });
    let top_do_now: Vec<String> = do_now_sorted
        .iter()
        .take(3)
        .map(|f| format!("{} \u{2014} {}:{}", f.rule_id, f.path, f.line))
        .collect();
    let (narrative, is_override) = match &opts.executive_summary_override {
        Some(text) if !text.trim().is_empty() => (text.clone(), true),
        _ => (
            default_narrative(
                candidates_reviewed,
                excluded_fp,
                curated_total,
                do_now,
                do_next,
                plan,
                accepted,
                open,
            ),
            false,
        ),
    };
    let executive_summary = ExecutiveSummaryJson {
        narrative,
        is_override,
        candidates_reviewed,
        excluded_false_positive: excluded_fp,
        curated_total,
        do_now,
        do_next,
        plan,
        accepted,
        open,
        top_do_now,
    };

    // ── Cover ──────────────────────────────────────────────────────────────────
    let audited_refs: Vec<AuditedRefJson> = report
        .provenance
        .audited_refs
        .iter()
        .map(|r| AuditedRefJson {
            repo: r.repo.clone(),
            sha: r.sha.clone(),
            short_sha: r.sha.as_ref().map(|s| s.chars().take(7).collect()),
            branch: r.branch.clone(),
            dirty: r.dirty,
        })
        .collect();
    let cover = CoverJson {
        repos: report.repos.clone(),
        files_scanned: report.files_scanned,
        files_excluded: report.files_excluded,
        code_chars: report.code_chars,
        audited_refs,
        audit_model: report.provenance.audit_model.clone(),
        calibration_model: report.provenance.calibration_model.clone(),
        camerata_version: report.provenance.camerata_version.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        client_name: opts.client_name.clone(),
        project_title: opts.project_title.clone(),
        prepared_by: opts.prepared_by.clone(),
    };

    // ── Methodology ────────────────────────────────────────────────────────────
    let methodology = MethodologyJson {
        candidates_reviewed,
        excluded_false_positive: excluded_fp,
        deterministic_note:
            "The deterministic floor (hardcoded secrets, raw SQL concatenation, unsafe \
             deserialization, disabled TLS, private keys, vendor tokens, secret files) is a \
             proven-defect class: every hit is a real match against a known-bad pattern, \
             always critical, never advisory."
                .to_string(),
        ai_tier_note:
            "The AI tier (architectural/structural review) is advisory and calibrated: each \
             finding carries a confidence tag and an effort estimate, and every finding in \
             this report has already been through a human triage pass before export."
                .to_string(),
        not_done: vec![
            "No penetration test (no live exploitation attempted).".to_string(),
            "No runtime/dynamic analysis (static code only).".to_string(),
            "No organizational-controls review (policies, HR, vendor contracts, physical \
             security)."
                .to_string(),
        ],
        severity_scale_note:
            "Severity is bimodal by design: the deterministic floor is always \u{2018}critical\u{2019} \
             (a proven defect class); the AI tier uses \u{2018}high\u{2019}/\u{2018}medium\u{2019}/\u{2018}low\u{2019} \
             based on calibrated judgment."
                .to_string(),
    };

    AuditReportJson {
        cover,
        executive_summary,
        scorecard: ScorecardJson {
            rows: scorecard_rows,
        },
        matrix,
        curated_findings,
        whats_healthy: WhatsHealthyJson {
            rules: healthy_rules,
            dependency_clean,
        },
        dependency_snapshot,
        methodology,
        disclaimer: AUDIT_REPORT_DISCLAIMER.to_string(),
    }
}

// ── Typst compile ────────────────────────────────────────────────────────────────

/// Compile `json` into a PDF via a bundled Typst template. Self-contained: the template
/// text is embedded in the binary (`include_str!`), so no external file needs to ship
/// alongside the server. Fails soft with an actionable message when `typst` isn't on PATH
/// (matches the dep-audit coverage-note tone: name the tool, name the fix).
///
/// Runs `typst compile report.typ report.pdf --root <tmpdir>` in a scratch temp dir (RAII
/// cleanup on drop) with a 30s timeout. The template reads `data.json` via Typst's own
/// `json()` builtin — no `--input` plumbing needed.
pub async fn compile_pdf(json: &AuditReportJson) -> anyhow::Result<Vec<u8>> {
    let tmp = tempfile::tempdir().context("could not create a temp dir for the PDF export")?;
    let data_path = tmp.path().join("data.json");
    let template_path = tmp.path().join("report.typ");
    let pdf_path = tmp.path().join("report.pdf");

    let data_json =
        serde_json::to_string(json).context("could not serialize the report to JSON")?;
    tokio::fs::write(&data_path, data_json)
        .await
        .context("could not write data.json")?;
    tokio::fs::write(
        &template_path,
        include_str!("../templates/audit_report.typ"),
    )
    .await
    .context("could not write report.typ")?;

    let mut cmd = tokio::process::Command::new("typst");
    cmd.current_dir(tmp.path())
        .arg("compile")
        .arg("report.typ")
        .arg("report.pdf")
        .arg("--root")
        .arg(tmp.path())
        .kill_on_drop(true);

    let output = match tokio::time::timeout(Duration::from_secs(30), cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("Install Typst: brew install typst");
        }
        Ok(Err(e)) => return Err(anyhow::Error::new(e).context("could not run typst")),
        Err(_elapsed) => anyhow::bail!("typst compile timed out after 30s"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("typst compile failed: {stderr}");
    }

    tokio::fs::read(&pdf_path)
        .await
        .context("typst reported success but report.pdf was not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::{AuditedRef, CoverageNote, ScanProvenance};

    fn finding(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
        Finding {
            repo: "owner/repo".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: severity.to_string(),
            snippet: format!("snippet-{line}"),
            detail: format!("detail for {rule_id}"),
            ..Finding::default()
        }
    }

    fn report_with(findings: Vec<Finding>, audited_rule_ids: Vec<&str>) -> ScanReport {
        ScanReport {
            repos: vec!["owner/repo".to_string()],
            stacks: Vec::new(),
            files_scanned: 10,
            files_excluded: 2,
            code_chars: 5000,
            excluded_mechanical_rules: Vec::new(),
            findings,
            proposed_rules: Vec::new(),
            gated: false,
            message: None,
            actual_usage: None,
            deep: None,
            coverage_notes: Vec::new(),
            provenance: ScanProvenance {
                audited_refs: vec![AuditedRef {
                    repo: "owner/repo".to_string(),
                    sha: Some("abcdef1234567890".to_string()),
                    branch: Some("main".to_string()),
                    dirty: false,
                }],
                audit_model: Some("test-model".to_string()),
                calibration_model: Some("test-cal-model".to_string()),
                mode: "parallel".to_string(),
                thorough: false,
                deep: false,
                rules_fingerprint: "fp".to_string(),
                audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
                camerata_version: "0.0.0-test".to_string(),
                osv_scanner_version: Some("1.9.0".to_string()),
                started_at: "2026-07-23T00:00:00Z".to_string(),
                finished_at: "2026-07-23T00:05:00Z".to_string(),
            },
        }
    }

    fn wire(state: &str, reason: &str, bucket: &str) -> DispositionWire {
        DispositionWire {
            state: state.to_string(),
            reason: reason.to_string(),
            bucket: bucket.to_string(),
        }
    }

    fn empty_opts() -> ReportOptions {
        ReportOptions::default()
    }

    // ── finding_key round-trip with ui-core's format ──────────────────────────

    /// Pins the wire format so `report_export::finding_key` can never silently drift from
    /// `camerata_ui_core::triage::finding_key` — the two crates can't share the function
    /// (server doesn't depend on ui-core), so this is the contract test.
    #[test]
    fn finding_key_matches_ui_core_format() {
        let f = finding("RULE-X", "src/a.rs", 7, "high");
        let key = finding_key(&f);
        // Exact format: repo\0rule_id\0path\0line\0snippet
        let expected = format!("owner/repo\u{0}RULE-X\u{0}src/a.rs\u{0}7\u{0}{}", f.snippet);
        assert_eq!(key, expected);
    }

    // ── FP exclusion ───────────────────────────────────────────────────────────

    #[test]
    fn false_positive_findings_are_excluded_and_counted_once() {
        let f1 = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "high");
        let f2 = finding("SEC-NO-HARDCODED-SECRETS-1", "b.rs", 2, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&f1),
            wire("FalsePositive", "generated fixture", ""),
        );
        let report = report_with(vec![f1, f2], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(json.methodology.candidates_reviewed, 2);
        assert_eq!(json.methodology.excluded_false_positive, 1);
        assert_eq!(json.executive_summary.excluded_false_positive, 1);
        // Only the non-FP finding survives into curated findings.
        let total_sites: usize = json.curated_findings.iter().map(|g| g.sites.len()).sum();
        assert_eq!(total_sites, 1);
        assert_eq!(json.curated_findings[0].sites[0].path, "b.rs");
    }

    // ── Ignored -> accepted risk ───────────────────────────────────────────────

    #[test]
    fn ignored_finding_is_accepted_risk_with_reason() {
        let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f), wire("Ignored", "accepted for now", ""));
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Accepted risk: accepted for now"
        );
        assert_eq!(json.matrix.accepted.len(), 1);
        assert_eq!(json.executive_summary.accepted, 1);
    }

    // ── TechDebt Now/Later -> do-now / planned ─────────────────────────────────

    #[test]
    fn tech_debt_now_and_later_route_to_matrix_do_now_and_plan() {
        let now_f = finding("ARCH-1", "a.rs", 1, "low");
        let later_f = finding("ARCH-1", "b.rs", 2, "low");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&now_f), wire("TechDebt", "", "Now"));
        dispositions.insert(finding_key(&later_f), wire("TechDebt", "", "Later"));
        let report = report_with(vec![now_f, later_f], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(json.matrix.do_now.len(), 1);
        assert_eq!(json.matrix.plan.len(), 1);
        assert_eq!(json.matrix.do_now[0].path, "a.rs");
        assert_eq!(json.matrix.plan[0].path, "b.rs");
    }

    // ── Unresolved -> open, severity×effort quadrant ───────────────────────────

    #[test]
    fn unresolved_high_severity_low_effort_is_do_now() {
        let mut f = finding("SEC-1", "a.rs", 1, "critical");
        f.effort = Some("low".to_string());
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.matrix.do_now.len(), 1);
        assert_eq!(json.executive_summary.open, 1);
        assert_eq!(json.curated_findings[0].sites[0].disposition, "Open");
    }

    #[test]
    fn unresolved_high_severity_unknown_effort_is_do_next_not_do_now() {
        // No calibration ran (effort = None) — must NOT be treated as low-effort.
        let f = finding("SEC-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.matrix.do_next.len(), 1);
        assert_eq!(json.matrix.do_now.len(), 0);
    }

    #[test]
    fn unresolved_low_severity_is_plan() {
        let f = finding("STYLE-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.matrix.plan.len(), 1);
    }

    // ── Suppressed-baseline -> pre-existing accepted debt ──────────────────────

    #[test]
    fn suppressed_baseline_with_no_wire_disposition_is_baseline_accepted() {
        let mut f = finding("SEC-1", "a.rs", 1, "high");
        f.status = "suppressed-baseline".to_string();
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Pre-existing accepted debt (baseline suppression)"
        );
        assert_eq!(json.matrix.accepted.len(), 1);
    }

    #[test]
    fn suppressed_baseline_with_explicit_wire_disposition_defers_to_client() {
        // The auditor re-triaged a previously-suppressed finding this session — the
        // fresh, explicit decision wins over the durable baseline status.
        let mut f = finding("SEC-1", "a.rs", 1, "high");
        f.status = "suppressed-baseline".to_string();
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f), wire("TechDebt", "", "Now"));
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        assert_eq!(json.matrix.do_now.len(), 1);
    }

    // ── Citation join ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn citation_join_resolves_a_real_corpus_rule_id() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(
            errors.is_empty(),
            "corpus must load cleanly, got errors: {errors:?}"
        );
        let f = finding("SEC-NO-UNSAFE-DESERIALIZATION-1", "a.py", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());

        let group = &json.curated_findings[0];
        assert_eq!(group.citation.kind, "grounded");
        assert!(
            group
                .citation
                .sources
                .iter()
                .any(|s| s.url.contains("owasp.org")),
            "must cite OWASP for SEC-NO-UNSAFE-DESERIALIZATION-1, got: {:?}",
            group.citation.sources
        );
    }

    #[test]
    fn citation_join_labels_unknown_rule_id_as_ai_advisory() {
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.curated_findings[0].citation.kind, "advisory");
        assert_eq!(
            json.curated_findings[0].citation.label,
            "AI-advisory, model-inferred."
        );
    }

    #[test]
    fn citation_join_labels_preview_finding_by_its_real_tool() {
        let mut f = finding("PREVIEW-RULE-1", "a.py", 1, "medium");
        f.preview = true;
        f.preview_tool = Some("ruff".to_string());
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.curated_findings[0].citation.kind, "preview");
        assert!(json.curated_findings[0].citation.label.contains("ruff"));
    }

    // ── What's-healthy derivation ──────────────────────────────────────────────

    #[test]
    fn whats_healthy_is_audited_rules_minus_rules_with_findings() {
        let f = finding("SEC-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec!["SEC-1", "SEC-2", "SEC-3"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let healthy_ids: Vec<&str> = json
            .whats_healthy
            .rules
            .iter()
            .map(|r| r.rule_id.as_str())
            .collect();
        assert_eq!(healthy_ids, vec!["SEC-2", "SEC-3"]);
    }

    #[test]
    fn whats_healthy_recovers_a_rule_whose_only_finding_was_a_false_positive() {
        // The tool flagged SEC-1 once, but the auditor proved it wrong (FalsePositive) —
        // the rule is honestly "clean" this run: excluded findings don't count against it.
        let f = finding("SEC-1", "a.rs", 1, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&f),
            wire("FalsePositive", "not exploitable", ""),
        );
        let report = report_with(vec![f], vec!["SEC-1"]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        let healthy_ids: Vec<&str> = json
            .whats_healthy
            .rules
            .iter()
            .map(|r| r.rule_id.as_str())
            .collect();
        assert_eq!(healthy_ids, vec!["SEC-1"]);
    }

    // ── Dependency carve-out ───────────────────────────────────────────────────

    #[test]
    fn dep_audit_findings_are_carved_out_of_code_sections_into_dependency_snapshot() {
        let mut f = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
        f.snippet = "time@0.3.20".to_string();
        f.detail = "RUSTSEC-2023-0001: potential segfault (affects time@0.3.20)".to_string();
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert!(
            json.curated_findings.is_empty(),
            "dep findings must not appear in curated findings"
        );
        assert_eq!(
            json.matrix.do_now.len()
                + json.matrix.do_next.len()
                + json.matrix.plan.len()
                + json.matrix.accepted.len(),
            0
        );
        assert_eq!(json.dependency_snapshot.rows.len(), 1);
        assert_eq!(json.dependency_snapshot.rows[0].package, "time@0.3.20");
        assert!(!json.dependency_snapshot.clean);
    }

    #[test]
    fn dependency_snapshot_clean_when_no_dep_findings() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(json.dependency_snapshot.clean);
        assert!(json.whats_healthy.dependency_clean);
    }

    #[test]
    fn dependency_coverage_note_surfaces_osv_scanner_failures_only() {
        let mut report = report_with(vec![], vec![]);
        report.coverage_notes = vec![
            CoverageNote {
                tool: "osv-scanner".to_string(),
                message: "dependency audit did not run: network unavailable".to_string(),
            },
            CoverageNote {
                tool: "ruff".to_string(),
                message: "ruff not installed".to_string(),
            },
        ];
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.dependency_snapshot.coverage_notes.len(), 1);
        assert!(json.dependency_snapshot.coverage_notes[0].contains("network unavailable"));
    }

    // ── Executive summary override ─────────────────────────────────────────────

    #[test]
    fn executive_summary_override_is_used_verbatim() {
        let report = report_with(vec![], vec![]);
        let mut opts = empty_opts();
        opts.executive_summary_override = Some("Custom hand-written summary.".to_string());
        let json = build_report_json(&report, &HashMap::new(), None, &opts);
        assert_eq!(
            json.executive_summary.narrative,
            "Custom hand-written summary."
        );
        assert!(json.executive_summary.is_override);
    }

    #[test]
    fn executive_summary_default_narrative_when_no_override() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(!json.executive_summary.is_override);
        assert!(json
            .executive_summary
            .narrative
            .contains("0 candidate finding(s)"));
    }

    // ── End-to-end PDF compile (typst-present-only) ────────────────────────────

    /// Compiles a small report to a real PDF via the bundled template. Skips gracefully
    /// (prints a note, does not fail) when `typst` is not on PATH — CI environments
    /// without Typst installed must never hard-fail on this test.
    #[tokio::test]
    async fn compile_pdf_produces_a_real_pdf_when_typst_is_present() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_produces_a_real_pdf_when_typst_is_present: typst not on PATH"
            );
            return;
        }
        let report = report_with(
            vec![finding(
                "SEC-NO-HARDCODED-SECRETS-1",
                "src/a.rs",
                10,
                "critical",
            )],
            vec!["SEC-NO-HARDCODED-SECRETS-1"],
        );
        let mut opts = empty_opts();
        opts.client_name = "Acme Corp".to_string();
        opts.project_title = "Acme Backend Audit".to_string();
        opts.prepared_by = "Camerata".to_string();
        let json = build_report_json(&report, &HashMap::new(), None, &opts);

        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed when typst is present");
        assert!(!pdf.is_empty(), "PDF bytes must be non-empty");
        assert!(
            pdf.starts_with(b"%PDF"),
            "output must start with the PDF magic bytes"
        );
    }

    /// Best-effort `typst` presence check for the compile test's skip gate (mirrors the
    /// same PATH lookup `compile_pdf` itself relies on via `Command::new("typst")`, just
    /// synchronous and side-effect-free here).
    fn which_typst() -> Option<()> {
        std::process::Command::new("typst")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| ())
    }
}
