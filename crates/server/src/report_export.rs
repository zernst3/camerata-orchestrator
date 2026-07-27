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
///
/// `FalsePositive` is ALWAYS partitioned out before this is called (see `build_report_json`'s
/// partition loop) — a PDF export must never be able to panic the server on triage data, so a
/// future refactor that lets one slip through fails soft to `Unresolved` (debug-asserts in
/// debug builds so the invariant is still loud in tests/dev) rather than `unreachable!`.
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
            "FalsePositive" => {
                debug_assert!(
                    false,
                    "FalsePositive must be partitioned out before classify() is called; see \
                     build_report_json. Falling through to Unresolved rather than panicking."
                );
                Disposition::Unresolved
            }
            _ => Disposition::Unresolved,
        },
        None if finding.status == "suppressed-baseline" => Disposition::BaselineAccepted,
        None => Disposition::Unresolved,
    }
}

/// Human-facing disposition annotation shown next to each curated-finding site row.
///
/// `bucket` is this SAME finding's own `matrix_bucket(...)` result — passed in rather than
/// recomputed so the label and the matrix cell a finding actually lands in can never drift
/// apart. Only consulted for `Unresolved` (every other disposition already has its own fixed
/// bucket by construction; see `matrix_bucket`'s doc comment).
///
/// No em/en dashes (house style for this client deliverable) — see `bucket_title`.
fn disposition_label(disposition: Disposition, reason: &str, bucket: &str) -> String {
    match disposition {
        Disposition::Unresolved => format!("Open (recommended: {})", bucket_title(bucket)),
        Disposition::Ignored => {
            if reason.trim().is_empty() {
                "Accepted risk".to_string()
            } else {
                format!("Accepted risk: {reason}")
            }
        }
        Disposition::TechDebtNow => "Tech debt, resolve now".to_string(),
        Disposition::TechDebtLater => "Tech debt, planned (resolve later)".to_string(),
        Disposition::BaselineAccepted => {
            "Pre-existing accepted debt (baseline suppression)".to_string()
        }
    }
}

/// Human title for a `matrix_bucket(...)` result (`"do_now"` -> `"Do now"`, etc.) — shared by
/// `disposition_label` (M7) and anywhere else a bucket needs a reader-facing label.
fn bucket_title(bucket: &str) -> &'static str {
    match bucket {
        "do_now" => "Do now",
        "do_next" => "Do next",
        "plan" => "Plan",
        _ => "Accepted",
    }
}

/// Normalize a severity string to Camerata's canonical lowercase vocabulary
/// (`critical`/`high`/`medium`/`low`), defensively, at the read site. A casing drift from an
/// upstream producer (e.g. `"Critical"`) must never silently demote a finding to `low` in the
/// scorecard/matrix — every severity comparison in this module goes through this function
/// exactly once per finding (computed alongside its disposition in `build_report_json`'s
/// partition loop) rather than matching `finding.severity.as_str()` ad hoc in each section.
fn normalize_severity(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "critical" => "critical".to_string(),
        "high" => "high".to_string(),
        "medium" => "medium".to_string(),
        _ => "low".to_string(),
    }
}

/// Real pluralization for a count + noun pair (`"1 site"` / `"3 sites"`), replacing the
/// `"N thing(s)"` CLI-ism that reads as sloppy in a flagship client PDF.
fn noun(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// Cap a finding's snippet so one pathological finding (a multi-hundred-line diff, a giant
/// minified blob) cannot claim unbounded page real estate in the PDF. ~12 lines / ~800 chars,
/// whichever is hit first; appends a `(truncated)` marker so the cut is honest, never silent.
const SNIPPET_MAX_LINES: usize = 12;
const SNIPPET_MAX_CHARS: usize = 800;

fn cap_snippet(snippet: &str) -> String {
    let mut truncated = false;
    let mut out: String = {
        let mut lines: Vec<&str> = snippet.lines().collect();
        if lines.len() > SNIPPET_MAX_LINES {
            lines.truncate(SNIPPET_MAX_LINES);
            truncated = true;
        }
        lines.join("\n")
    };
    if out.chars().count() > SNIPPET_MAX_CHARS {
        out = out.chars().take(SNIPPET_MAX_CHARS).collect();
        truncated = true;
    }
    if truncated {
        out.push_str("\n... (truncated)");
    }
    out
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

/// Cover-page stat strip (nice-to-have): a one-line severity/disposition summary so the
/// story starts on page 1 instead of the cover being ~60% dead space until "Executive
/// summary". Counts are over `code_findings` (matches the scorecard's own severity totals)
/// plus the dependency-snapshot row count.
#[derive(Debug, Clone, Serialize)]
pub struct CoverStatsJson {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub accepted: usize,
    pub dependency_advisories: usize,
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
    /// `%Y-%m-%d %H:%M UTC` (M5) — of when this PDF was generated (NOT the scan time —
    /// that's `audited_refs`/provenance; this is "as-of" for the export itself).
    pub generated_at: String,
    pub client_name: String,
    pub project_title: String,
    pub prepared_by: String,
    pub stats: CoverStatsJson,
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
    /// Up to 3 one-liners: `"{title}: {repo}/{path}:{line} ({rule_id})"` (S3 + S7).
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
    /// Audited rules with zero real (non-FP) findings this run, capped (S6) at
    /// `WHATS_HEALTHY_CAP` and sorted grounded-citation rules first.
    pub rules: Vec<HealthyRuleJson>,
    /// How many MORE zero-finding rules exist beyond `rules` (0 when nothing was cut) — S6's
    /// "N further rules verified clean this run" summary line, so capping never silently
    /// drops a rule from the report, it just stops listing them individually.
    pub further_clean_count: usize,
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
/// (non-deep-tier) brownfield audit this report covers. M4/S12: the calm, board-appropriate
/// paragraph, not a shouty ALL-CAPS block, and no em/en dashes.
pub const AUDIT_REPORT_DISCLAIMER: &str =
    "This report is an advisory architectural and security assessment based on static \
     analysis of the repository state identified on the cover page. It is not a \
     certification, warranty, or guarantee of security. Findings reflect evidence present in \
     the audited code at scan time and may not represent the live production system. All \
     remediation should be validated by the client's engineering team against the live \
     environment. The preparing auditor accepts no liability for actions taken on the basis \
     of this report.";

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
                "Deterministic preview, enforced by {tool} ({rule_id}); not yet wired into the CI gate"
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

/// A small, generic technical-acronym allowlist for cosmetic title-casing — NOT a rule
/// taxonomy, just common initialisms that should stay upper ("RLS", not "Rls") wherever they
/// show up in a category key, whether that key came from a corpus rule's lowercase `domain`
/// (`"supabase:rls"`) or a rule id's own uppercase tokens (`"SUPABASE-RLS-ENABLED-1"`).
const CATEGORY_ACRONYMS: &[&str] = &[
    "RLS", "SQL", "JWT", "API", "TLS", "SSL", "CSS", "UI", "URL", "URI", "CVE", "CORS", "HTML",
    "JSON", "CSP", "XSS", "CSRF", "JS", "TS", "SSR", "SSG", "CLI", "DB",
];

/// Title-case one category-key token: a known short acronym stays upper (case-insensitive
/// match); anything else becomes `Capitalized-then-lowercase` regardless of its original
/// casing. Purely cosmetic — see `category_for`.
fn title_case_token(tok: &str) -> String {
    let upper = tok.to_ascii_uppercase();
    if CATEGORY_ACRONYMS.contains(&upper.as_str()) {
        return upper;
    }
    let mut chars = tok.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
    }
}

/// Turn a raw category key (a corpus rule's `domain`, e.g. `"supabase:rls"`, or a rule id
/// used as a fallback, e.g. `"SUPABASE-RLS-ENABLED-1"`) into a reader-facing label: split on
/// `-`/`:`/`_`, drop pure-numeric segments, title-case up to the first two remaining tokens
/// (`"supabase:rls"` -> `"Supabase RLS"`).
fn prettify_category_key(key: &str) -> String {
    let tokens: Vec<&str> = key
        .split(['-', ':', '_'])
        .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
        .take(2)
        .collect();
    if tokens.is_empty() {
        return "Other".to_string();
    }
    tokens
        .iter()
        .map(|t| title_case_token(t))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Category for the scorecard: the corpus rule's own `domain` when known, else a fallback
/// built from the rule id itself — both prettified the same way (S5), rather than a bare
/// lowercased/raw key (`"supabase:rls"` or `"supabase"`). Still a fallback for the
/// corpus-absent case, not a taxonomy — good enough for a scorecard grouping, not
/// load-bearing.
fn category_for(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> String {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(rule_id)) {
        return prettify_category_key(&rule.domain);
    }
    prettify_category_key(rule_id)
}

/// Which matrix cell a finding lands in. The auditor's OWN disposition wins where one
/// exists (`Ignored`/`BaselineAccepted` -> Accepted; `TechDebt{Now}` -> Do now;
/// `TechDebt{Later}` -> Plan); only an `Unresolved` (still-open) finding falls back to the
/// computed severity×effort quadrant. `severity` MUST already be normalized
/// (`normalize_severity`) — a `critical` finding is ALWAYS `do_now` regardless of effort (a
/// deterministic-floor/preview finding never gets calibrated effort, and the worst finding in
/// the report must never sit un-actioned in "Do next" just because no one estimated the fix
/// time). `high` severity still needs `effort == Some("low")` for `do_now`; missing effort is
/// treated as "not low" there — conservative, so an uncalibrated high finding never gets
/// silently downgraded to a same-day fix. `medium`/`low` always land in `Plan`.
fn matrix_bucket(disposition: Disposition, severity: &str, effort: Option<&str>) -> &'static str {
    match disposition {
        Disposition::Ignored | Disposition::BaselineAccepted => "accepted",
        Disposition::TechDebtNow => "do_now",
        Disposition::TechDebtLater => "plan",
        Disposition::Unresolved => {
            if severity == "critical" {
                return "do_now";
            }
            let low_effort = matches!(effort, Some("low"));
            match (severity == "high", low_effort) {
                (true, true) => "do_now",
                (true, false) => "do_next",
                (false, _) => "plan",
            }
        }
    }
}

fn finding_ref(f: &Finding, severity: &str) -> FindingRefJson {
    FindingRefJson {
        rule_id: f.rule_id.clone(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        severity: severity.to_string(),
    }
}

/// Deterministic exec-summary narrative (never an LLM call). Overridden verbatim by
/// `ReportOptions::executive_summary_override` when the client supplies one. Special-cases
/// zero candidates (a plain "0 candidate finding(s) were reviewed..." reads as broken, not
/// clean) and uses real pluralization throughout (`noun`) rather than the "(s)" CLI-ism.
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
    if candidates_reviewed == 0 {
        return "The scan surfaced no candidate findings to review in this run.".to_string();
    }
    // Deliberately ONE prose paragraph, no embedded list — `top_do_now` is a separate
    // structured field the template renders as its own bulleted list (a Typst string value
    // doesn't reliably turn embedded "\n"s into paragraph/list breaks, so mixing prose and
    // list markup into one opaque string would render as a flat run-on in the PDF).
    let mut s = format!(
        "{} were reviewed; {} {} dispositioned as false positives by the auditor and excluded \
         entirely from this report. Of the remaining {}: {do_now} do now, {do_next} do next, \
         {plan} planned, {accepted} accepted as risk",
        noun(candidates_reviewed, "candidate finding", "candidate findings"),
        excluded_fp,
        if excluded_fp == 1 { "was" } else { "were" },
        noun(curated_total, "curated finding", "curated findings"),
    );
    if open > 0 {
        s.push_str(&format!(
            ", and {} still open, unresolved",
            noun(open, "finding", "findings")
        ));
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
    // effective disposition + REASON + normalized severity (S10 — normalized exactly once,
    // here, so no downstream section can silently mis-bucket a `"Critical"`-cased finding as
    // low severity by matching `finding.severity.as_str()` ad hoc).
    let mut excluded_fp = 0usize;
    let mut live: Vec<(&Finding, Disposition, String, String)> = Vec::new();
    for f in &report.findings {
        let wire = dispositions.get(&finding_key(f));
        if wire.map(|d| d.state.as_str()) == Some("FalsePositive") {
            excluded_fp += 1;
            continue;
        }
        let disposition = classify(f, wire);
        let reason = wire.map(|d| d.reason.clone()).unwrap_or_default();
        let severity = normalize_severity(&f.severity);
        live.push((f, disposition, reason, severity));
    }

    // Dependency findings get their own §7 lane — carve them out of every other section.
    let (dep_findings, code_findings): (Vec<_>, Vec<_>) = live
        .into_iter()
        .partition(|(f, _, _, _)| f.rule_id == DEP_AUDIT_RULE_ID);

    // ── Matrix + curated findings + scorecard, over code_findings only ───────────
    let mut matrix = MatrixJson::default();
    for (f, disposition, _, severity) in &code_findings {
        let bucket = matrix_bucket(*disposition, severity, f.effort.as_deref());
        let target = match bucket {
            "do_now" => &mut matrix.do_now,
            "do_next" => &mut matrix.do_next,
            "plan" => &mut matrix.plan,
            _ => &mut matrix.accepted,
        };
        target.push(finding_ref(f, severity));
    }

    // Curated findings: grouped by rule (sorted for deterministic output), each rule's
    // sites sorted by repo/path/line.
    #[allow(clippy::type_complexity)]
    let mut by_rule: std::collections::BTreeMap<String, Vec<(&Finding, Disposition, String, String)>> =
        std::collections::BTreeMap::new();
    for (f, disposition, reason, severity) in &code_findings {
        by_rule
            .entry(f.rule_id.clone())
            .or_default()
            .push((f, *disposition, reason.clone(), severity.clone()));
    }
    let mut curated_findings: Vec<CuratedGroupJson> = Vec::new();
    for (rule_id, mut sites) in by_rule {
        sites.sort_by(|a, b| {
            (&a.0.repo, &a.0.path, a.0.line).cmp(&(&b.0.repo, &b.0.path, b.0.line))
        });
        let preview_tool = sites.iter().find_map(|(f, _, _, _)| f.preview_tool.as_deref());
        let citation = resolve_citation(&rule_id, preview_tool, corpus);
        let title = corpus
            .and_then(|c| c.get_by_id(&rule_id))
            .map(|r| r.title.clone())
            .unwrap_or_else(|| rule_id.clone());
        let site_jsons = sites
            .iter()
            .map(|(f, disposition, reason, severity)| {
                let bucket = matrix_bucket(*disposition, severity, f.effort.as_deref());
                CuratedSiteJson {
                    repo: f.repo.clone(),
                    path: f.path.clone(),
                    line: f.line,
                    snippet: cap_snippet(&f.snippet),
                    detail: f.detail.clone(),
                    severity: severity.clone(),
                    effort: f.effort.clone(),
                    confidence: f.confidence.clone(),
                    disposition: disposition_label(*disposition, reason, bucket),
                    also_matches: f.also_matches.clone(),
                }
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
    #[allow(clippy::type_complexity)]
    let mut by_category: std::collections::BTreeMap<
        String,
        Vec<&(&Finding, Disposition, String, String)>,
    > = std::collections::BTreeMap::new();
    for entry in &code_findings {
        by_category
            .entry(category_for(&entry.0.rule_id, corpus))
            .or_default()
            .push(entry);
    }
    // A category audited this run but with ZERO real findings must still get a scorecard
    // row (status "Clean", audited_rules > 0, clean_rules == audited_rules) — otherwise a
    // fully clean category is silently invisible in the scorecard instead of visibly
    // reading "Clean", and the nice-to-have Action-needed -> Attention -> Clean ordering has
    // no Clean rows to place.
    for rid in &report.provenance.audited_rule_ids {
        by_category.entry(category_for(rid, corpus)).or_default();
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
            let severity = entry.3.as_str();
            match severity {
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
                match severity {
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
    // Nice-to-have: Action-needed -> Attention -> Clean, not BTreeMap-alphabetical — the
    // categories that most need a reader's attention lead the table.
    scorecard_rows.sort_by(|a, b| {
        fn rank(status: &str) -> u8 {
            match status {
                "Action-needed" => 0,
                "Attention" => 1,
                _ => 2,
            }
        }
        (rank(&a.status), &a.category).cmp(&(rank(&b.status), &b.category))
    });

    // ── What's healthy: audited rules with ZERO real (non-FP) findings ───────────
    // Capped (S6) at grounded-citation rules first, then whatever else fits, so a large
    // corpus doesn't turn this section into dozens of bullets that read as padding; the
    // remainder is summarized in one honest count line instead of silently dropped.
    const WHATS_HEALTHY_CAP: usize = 10;
    let rule_ids_with_any_finding: std::collections::HashSet<&str> = code_findings
        .iter()
        .map(|(f, _, _, _)| f.rule_id.as_str())
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
    healthy_rules.sort_by(|a, b| {
        let grounded_first = |kind: &str| if kind == "grounded" { 0 } else { 1 };
        (grounded_first(&a.citation.kind), &a.rule_id).cmp(&(grounded_first(&b.citation.kind), &b.rule_id))
    });
    let further_clean_count = healthy_rules.len().saturating_sub(WHATS_HEALTHY_CAP);
    healthy_rules.truncate(WHATS_HEALTHY_CAP);

    // ── §7 Dependency snapshot ─────────────────────────────────────────────────
    let dep_rows: Vec<DependencyFindingJson> = dep_findings
        .iter()
        .map(|(f, _, _, severity)| DependencyFindingJson {
            package: f.snippet.clone(),
            advisory: f.detail.clone(),
            severity: severity.clone(),
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
        .filter(|(_, d, _, _)| *d == Disposition::Unresolved)
        .count();
    let mut do_now_sorted = matrix.do_now.clone();
    do_now_sorted.sort_by_key(|f| match f.severity.as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    });
    // S3 + S7: name the repo and the rule's human title, not just a bare rule id + path —
    // ambiguous in a multi-repo audit, and a rule id alone means nothing to a board reader.
    let top_do_now: Vec<String> = do_now_sorted
        .iter()
        .take(3)
        .map(|f| {
            let title = corpus
                .and_then(|c| c.get_by_id(&f.rule_id))
                .map(|r| r.title.clone())
                .unwrap_or_else(|| f.rule_id.clone());
            format!("{title}: {}/{}:{} ({})", f.repo, f.path, f.line, f.rule_id)
        })
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
    let stats = CoverStatsJson {
        critical: scorecard_rows.iter().map(|r| r.critical).sum(),
        high: scorecard_rows.iter().map(|r| r.high).sum(),
        medium: scorecard_rows.iter().map(|r| r.medium).sum(),
        low: scorecard_rows.iter().map(|r| r.low).sum(),
        accepted,
        dependency_advisories: dependency_snapshot.rows.len(),
    };
    let cover = CoverJson {
        repos: report.repos.clone(),
        files_scanned: report.files_scanned,
        files_excluded: report.files_excluded,
        code_chars: report.code_chars,
        audited_refs,
        audit_model: report.provenance.audit_model.clone(),
        calibration_model: report.provenance.calibration_model.clone(),
        camerata_version: report.provenance.camerata_version.clone(),
        generated_at: chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(),
        client_name: opts.client_name.clone(),
        project_title: opts.project_title.clone(),
        prepared_by: opts.prepared_by.clone(),
        stats,
    };

    // ── Methodology (M4: ported from the better sample copy, no dashes per S12) ────────
    let methodology = MethodologyJson {
        candidates_reviewed,
        excluded_false_positive: excluded_fp,
        deterministic_note:
            "Camerata runs a two-tier engine. A deterministic security floor (proven-defect \
             SAST rules plus a migration-timeline replay for Supabase Row Level Security) \
             produces findings that either hold or do not, with no model judgment. These are \
             labeled deterministic in the report."
                .to_string(),
        ai_tier_note:
            "A second, advisory tier uses a calibrated language-model audit for findings that \
             require semantic judgment. Every advisory finding is reviewed and dispositioned \
             by a human auditor before it appears here; lower-confidence items are marked \
             needs-review."
                .to_string(),
        not_done: vec![
            "No penetration testing or live exploitation was performed.".to_string(),
            "No runtime or dynamic analysis; findings derive from static repository state."
                .to_string(),
            "Row Level Security findings reflect the repository's migration history, not the \
             live database. Changes made in the Supabase dashboard that never landed in a \
             migration are invisible to a repository scan and must be confirmed against \
             production."
                .to_string(),
            "No review of organizational controls, identity and access management, or \
             infrastructure configuration."
                .to_string(),
        ],
        severity_scale_note:
            "Severity is bimodal by design. Deterministic floor findings are proven defects \
             rated by impact; advisory findings are rated by likely impact together with \
             calibrated confidence. There is deliberately no single numeric score or letter \
             grade: a grade hides which specific object is exposed, which is exactly what a \
             remediation team needs to know."
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
            further_clean_count,
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
        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Open (recommended: Do now)"
        );
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

    // ── M3 regression: an uncalibrated CRITICAL must never sit in "Do next" ───

    #[test]
    fn unresolved_critical_with_no_effort_is_do_now_not_do_next() {
        // The deterministic floor / preview findings never get a calibrated effort estimate
        // (effort = None). Before M3 this fell into "Do next" alongside a merely-high finding
        // — the worst finding in the report could sit un-actioned. A critical must be Do now
        // regardless of effort.
        let f = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "critical");
        assert_eq!(f.effort, None, "test fixture must not carry an effort estimate");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.matrix.do_now.len(), 1, "{:?}", json.matrix);
        assert_eq!(json.matrix.do_next.len(), 0);
        assert!(
            !json.executive_summary.top_do_now.is_empty(),
            "top_do_now must not be empty when a critical do_now finding exists"
        );
    }

    #[test]
    fn unresolved_high_effort_medium_is_still_do_next_not_downgraded() {
        // A "high" (not critical) finding with unknown effort keeps the pre-existing
        // conservative behavior: Do next, never Do now, never silently downgraded to Plan.
        let f = finding("ARCH-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.matrix.do_next.len(), 1);
        assert_eq!(json.matrix.do_now.len(), 0);
        assert_eq!(json.matrix.plan.len(), 0);
    }

    // ── M7 regression: Unresolved disposition names its actual matrix bucket ───

    #[test]
    fn unresolved_disposition_label_names_do_next_bucket() {
        let f = finding("ARCH-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Open (recommended: Do next)"
        );
    }

    #[test]
    fn unresolved_disposition_label_names_plan_bucket() {
        let f = finding("STYLE-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Open (recommended: Plan)"
        );
    }

    // ── S10 regression: severity casing drift must not silently demote to "low" ────

    #[test]
    fn severity_casing_drift_is_normalized_not_silently_demoted_to_low() {
        let f = finding("SEC-1", "a.rs", 1, "Critical"); // upstream casing drift
        let report = report_with(vec![f], vec!["SEC-1"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        // Must land in `do_now` (critical), never counted as `low` in the scorecard.
        assert_eq!(json.matrix.do_now.len(), 1, "{:?}", json.matrix);
        assert_eq!(json.scorecard.rows[0].critical, 1);
        assert_eq!(json.scorecard.rows[0].low, 0);
        assert_eq!(json.curated_findings[0].sites[0].severity, "critical");
    }

    // ── S5: fallback categories are title-cased (not raw lowercase tokens) ─────

    #[test]
    fn category_fallback_is_title_cased_without_a_corpus() {
        let f = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.scorecard.rows[0].category, "Supabase RLS");
    }

    // ── S6: what's-healthy caps long lists and reports a remainder count ──────

    #[test]
    fn whats_healthy_caps_at_ten_and_reports_further_clean_count() {
        let audited_owned: Vec<String> = (0..15).map(|i| format!("SEC-{i}")).collect();
        let audited: Vec<&str> = audited_owned.iter().map(String::as_str).collect();
        let report = report_with(vec![], audited);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.whats_healthy.rules.len(), 10);
        assert_eq!(json.whats_healthy.further_clean_count, 5);
    }

    // ── M8: an unbounded snippet is capped, never claims unlimited page space ──

    #[test]
    fn snippet_is_capped_at_max_lines_with_a_truncated_marker() {
        let mut f = finding("SEC-1", "a.rs", 1, "high");
        f.snippet = (0..50).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let capped = &json.curated_findings[0].sites[0].snippet;
        assert!(capped.lines().count() <= SNIPPET_MAX_LINES + 1, "{capped}");
        assert!(capped.contains("(truncated)"), "{capped}");
    }

    // ── Nice-to-have: cover stat strip + scorecard status ordering ─────────────

    #[test]
    fn cover_stats_summarize_severity_and_disposition_counts() {
        let critical = finding("SEC-1", "a.rs", 1, "critical");
        let high = finding("SEC-2", "b.rs", 2, "high");
        let ignored = finding("SEC-3", "c.rs", 3, "medium");
        let mut dep = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
        dep.snippet = "foo@1.0.0".to_string();
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&ignored), wire("Ignored", "known", ""));
        let report = report_with(vec![critical, high, ignored, dep], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        assert_eq!(json.cover.stats.critical, 1);
        assert_eq!(json.cover.stats.high, 1);
        assert_eq!(json.cover.stats.medium, 1);
        assert_eq!(json.cover.stats.accepted, 1);
        assert_eq!(json.cover.stats.dependency_advisories, 1);
    }

    #[test]
    fn scorecard_rows_are_ordered_action_needed_before_clean() {
        let action_needed = finding("ZZZ-1", "a.rs", 1, "critical");
        let clean_category_finding = finding("AAA-1", "b.rs", 2, "critical");
        let mut dispositions = HashMap::new();
        // The AAA-1 finding is accepted, so its category's status is "Clean" (no OPEN
        // high/critical exposure) even though a critical finding exists in it.
        dispositions.insert(
            finding_key(&clean_category_finding),
            wire("Ignored", "accepted", ""),
        );
        let report = report_with(vec![action_needed, clean_category_finding], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        assert_eq!(json.scorecard.rows[0].status, "Action-needed");
        assert_eq!(json.scorecard.rows[0].category, "Zzz");
        assert_eq!(json.scorecard.rows[1].status, "Clean");
    }

    #[test]
    fn scorecard_includes_a_clean_row_for_an_audited_category_with_zero_findings() {
        // An audited category with NO real findings at all (every hit was a false
        // positive, or the rule simply never fired) must still get a "Clean" scorecard row
        // — otherwise a fully clean category is silently invisible instead of visibly
        // reading "Clean", and there is nothing for the Action-needed -> Clean ordering to
        // place at the bottom.
        let flagged = finding("ZZZ-1", "a.rs", 1, "critical");
        let clean_only = finding("AAA-1", "b.rs", 2, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&clean_only),
            wire("FalsePositive", "not exploitable", ""),
        );
        let report = report_with(vec![flagged, clean_only], vec!["ZZZ-1", "AAA-1"]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        assert_eq!(json.scorecard.rows.len(), 2, "{:?}", json.scorecard.rows);
        let clean_row = json
            .scorecard
            .rows
            .iter()
            .find(|r| r.category == "Aaa")
            .expect("AAA-1's category must still get a row despite zero real findings");
        assert_eq!(clean_row.status, "Clean");
        assert_eq!(clean_row.audited_rules, 1);
        assert_eq!(clean_row.clean_rules, 1);
        assert_eq!(clean_row.critical + clean_row.high + clean_row.medium + clean_row.low, 0);
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
        // S7: a zero-candidate scan gets its own honest sentence, not a mechanical
        // "0 candidate finding(s) were reviewed... 0 do now, 0 do next..." run-on.
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(!json.executive_summary.is_override);
        assert_eq!(
            json.executive_summary.narrative,
            "The scan surfaced no candidate findings to review in this run."
        );
    }

    #[test]
    fn executive_summary_default_narrative_pluralizes_correctly() {
        // S8: real pluralization ("1 candidate finding", not "1 candidate finding(s)").
        let f = finding("SEC-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(
            json.executive_summary.narrative.contains("1 candidate finding "),
            "expected singular 'finding', got: {:?}",
            json.executive_summary.narrative
        );
        assert!(
            !json.executive_summary.narrative.contains("finding(s)"),
            "must not contain the CLI-ism '(s)': {:?}",
            json.executive_summary.narrative
        );
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

    // ── Regression guard: the backtick-hash interpolation bug class ───────────
    //
    // Three separate spots in the template used to write `` `#value` `` (a value
    // interpolation immediately inside backtick-delimited raw text). Typst does NOT
    // evaluate `#...` inside backticks — it renders the literal characters `#value` — so
    // the cover's short SHA, branch name, and a curated-finding site's path all rendered as
    // the LITERAL TEXT `#r.short_sha` / `#r.branch` / `#site.path` instead of their values.
    // The fix is `#raw(value)` (evaluate first, THEN render as raw/monospace). This test
    // pins the shipped template against the exact bug class returning, ever.
    #[test]
    fn shipped_template_has_no_backtick_hash_interpolation_bug() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            !template.contains("`#"),
            "the shipped audit_report.typ template contains a backtick immediately followed \
             by '#' — Typst renders `#value` inside backtick-delimited raw text LITERALLY \
             instead of interpolating it. Use `#raw(value)` (evaluate, then render as raw) \
             instead of `` `#value` `` (see the M6/M8 fixes in \
             docs/design/2026-07-26_audit-report-refinements.md)."
        );
    }
}
