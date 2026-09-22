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
    /// Whether a real client has actually confirmed this `Ignored` disposition (e.g. "yes,
    /// this is defense-in-depth only"). Defaults to `false` — the SAFE default, since most
    /// dispositions come from the auditor's own judgment before any client conversation has
    /// happened. See `disposition_label`: an `Ignored` finding with this `false` NEVER
    /// renders as if a client confirmed it, however confident `reason`'s prose sounds — it
    /// renders "Needs client confirmation" instead. Only an explicit `true` (set once a real
    /// client conversation happened) unlocks the "Accepted risk: {reason}" wording.
    #[serde(default)]
    pub confirmed_by_client: bool,
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
    /// The AUDITING AGENCY's own brand (e.g. "Cantus Works") — distinct from `client_name`
    /// (who the report is prepared FOR). Owner's hard constraint (2026-09-13 branding pass):
    /// Camerata is the licensable application; the agency running it is not Camerata's to
    /// name. Blank here falls through to the `CAMERATA_REPORT_BRAND` env var
    /// ([`apply_env_defaults`]), then to no brand at all — never a baked-in literal. See
    /// [`resolve_brand`].
    #[serde(default)]
    pub brand: String,
}

/// Env var carrying the STANDING default for [`ReportOptions::brand`] when the export dialog
/// doesn't POST one. Brand-neutral name (not `CANTUS_*`) so a future Camerata licensee sets
/// their OWN agency's value here without touching Camerata source — see the "Branding" section
/// of `docs/design/2026-09-13_audit-deliverable-review-fixes.md`.
pub const REPORT_BRAND_ENV: &str = "CAMERATA_REPORT_BRAND";

/// Env var carrying the standing default for [`ReportOptions::prepared_by`]. Same precedence
/// and neutrality rationale as [`REPORT_BRAND_ENV`].
pub const REPORT_PREPARED_BY_ENV: &str = "CAMERATA_REPORT_PREPARED_BY";

/// The cover title (and PDF metadata title) when no brand resolves at all — no agency name,
/// and NEVER the word "Camerata" (Camerata is the instrument, named only in Methodology; see
/// the Branding section of the design doc). Exported so the template-generation site and any
/// test asserting on the exact fallback string share one literal.
pub const NEUTRAL_COVER_TITLE: &str = "Codebase Audit Report";

/// Precedence resolver: the per-report field wins when non-blank, else the env value when
/// non-blank, else `None`/empty. Pure — takes the env value as an explicit `Option<&str>`
/// rather than calling `std::env::var` itself, so it stays unit-testable without mutating real
/// process env state (which parallel `cargo test` threads would otherwise race on). Shared by
/// [`resolve_brand`] and [`resolve_prepared_by`].
fn resolve_with_env_default(field: &str, env: Option<&str>) -> Option<String> {
    let field = field.trim();
    if !field.is_empty() {
        return Some(field.to_string());
    }
    env.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Resolve the report's brand per the owner's precedence: per-report `ReportOptions::brand` >
/// `CAMERATA_REPORT_BRAND` env var > no brand (`None`). `None` means the cover/footer/PDF
/// title must render the brand-neutral fallback ([`NEUTRAL_COVER_TITLE`]) with no agency name
/// and no "Camerata" literal — never a fabricated brand.
pub(crate) fn resolve_brand(opts: &ReportOptions, env_brand: Option<&str>) -> Option<String> {
    resolve_with_env_default(&opts.brand, env_brand)
}

/// Resolve the report's "prepared by" line: per-report `ReportOptions::prepared_by` >
/// `CAMERATA_REPORT_PREPARED_BY` env var > empty (the cover table's `or_na` already renders an
/// empty string as "N/A").
pub(crate) fn resolve_prepared_by(opts: &ReportOptions, env_prepared_by: Option<&str>) -> String {
    resolve_with_env_default(&opts.prepared_by, env_prepared_by).unwrap_or_default()
}

impl ReportOptions {
    /// Fold the `CAMERATA_REPORT_BRAND`/`CAMERATA_REPORT_PREPARED_BY` env vars into this
    /// struct's blank fields, IN PLACE. Call this exactly ONCE, at the HTTP boundary (the
    /// `/audit-report` and `/product-export` handlers in `lib.rs`), BEFORE `build_report_json`
    /// — that keeps `build_report_json` itself free of direct env access (pure, no I/O, per
    /// its own doc comment) while still honoring the env-default precedence end to end. Real
    /// `std::env::var` reads live here, not in [`resolve_brand`]/[`resolve_prepared_by`], so
    /// those stay unit-testable with explicit values.
    pub fn apply_env_defaults(&mut self) {
        self.brand = resolve_brand(self, std::env::var(REPORT_BRAND_ENV).ok().as_deref())
            .unwrap_or_default();
        self.prepared_by =
            resolve_prepared_by(self, std::env::var(REPORT_PREPARED_BY_ENV).ok().as_deref());
    }
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
///
/// `pub(crate)`: shared with `xlsx_export` (product export, Pass C) so the workbook's own
/// partition pass reuses this SAME classification, never a re-derived copy — see that
/// module's doc comment.
///
/// `Serialize`: so `xlsx_export::FindingRow` (whose `disposition_kind: Option<Disposition>`
/// field feeds `findings.json`, the product export's machine-readable sibling of the
/// workbook) can serialize this classification verbatim rather than re-deriving a string
/// form for JSON specifically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum Disposition {
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
    /// Waived at the code site by an inline `camerata:allow -- reason` comment. A reviewable,
    /// reasoned, in-diff acceptance — the linter-waiver equivalent of `BaselineAccepted`, kept
    /// distinct so the report can honestly say "waived in code" vs "carried in the baseline
    /// file". Lands in the Accepted matrix cell, NOT `Unresolved` (the bug this fixes: an
    /// inline-suppressed finding was being reported as still-open).
    WaivedInline,
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
pub(crate) fn classify(finding: &Finding, wire: Option<&DispositionWire>) -> Disposition {
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
        None if finding.status == "suppressed-inline" => Disposition::WaivedInline,
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
/// # Never fabricate a client disposition
/// A real audit often ships before any client conversation has happened (e.g. a pre-engagement
/// scan of an OSS repo, or the very first draft of a fresh engagement) — there is no "team" to
/// have confirmed anything. `confirmed_by_client` (from `DispositionWire`, default `false`) is
/// the ONLY thing that unlocks "Accepted risk: {reason}" wording for an `Ignored` finding; when
/// it is `false` (the safe default), the reader sees "Needs client confirmation" instead, no
/// matter how confident the auditor's own `reason` prose reads — the auditor's proposed
/// rationale is still surfaced, but honestly attributed to the auditor, never invented as a
/// client's words. This is a deliberate rendering constraint, not just a fixture-content fix:
/// the serializer itself cannot produce "confirmed" language without that explicit flag.
///
/// No em/en dashes (house style for this client deliverable) — see `bucket_title`.
pub(crate) fn disposition_label(
    disposition: Disposition,
    reason: &str,
    bucket: &str,
    confirmed_by_client: bool,
) -> String {
    match disposition {
        Disposition::Unresolved if bucket == "informational" => {
            "Convention to consider (informational)".to_string()
        }
        Disposition::Unresolved => format!("Open (recommended: {})", bucket_title(bucket)),
        Disposition::Ignored => {
            if confirmed_by_client {
                if reason.trim().is_empty() {
                    "Accepted risk".to_string()
                } else {
                    format!("Accepted risk: {reason}")
                }
            } else if reason.trim().is_empty() {
                "Needs client confirmation".to_string()
            } else {
                format!("Needs client confirmation (auditor's proposed rationale: {reason})")
            }
        }
        Disposition::TechDebtNow => "Tech debt, resolve now".to_string(),
        Disposition::TechDebtLater => "Tech debt, planned (resolve later)".to_string(),
        Disposition::BaselineAccepted => {
            "Pre-existing accepted debt (baseline suppression)".to_string()
        }
        Disposition::WaivedInline => "Waived in code (inline camerata:allow)".to_string(),
    }
}

/// Human title for a `matrix_bucket(...)` result (`"do_now"` -> `"Do now"`, etc.) — shared by
/// `disposition_label` (M7) and anywhere else a bucket needs a reader-facing label.
pub(crate) fn bucket_title(bucket: &str) -> &'static str {
    match bucket {
        "do_now" => "Do now",
        "do_next" => "Do next",
        "plan" => "Plan",
        "informational" => "Informational",
        _ => "Accepted",
    }
}

/// Normalize a severity string to Camerata's canonical lowercase vocabulary
/// (`critical`/`high`/`medium`/`low`), defensively, at the read site. A casing drift from an
/// upstream producer (e.g. `"Critical"`) must never silently demote a finding to `low` in the
/// scorecard/matrix — every severity comparison in this module goes through this function
/// exactly once per finding (computed alongside its disposition in `build_report_json`'s
/// partition loop) rather than matching `finding.severity.as_str()` ad hoc in each section.
pub(crate) fn normalize_severity(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "critical" => "critical".to_string(),
        "high" => "high".to_string(),
        "medium" => "medium".to_string(),
        // `info` is a real, distinct tier (Bug 4): a convention-to-consider, ranked BELOW
        // `low`. It must survive normalization rather than collapsing into `low`, or an
        // informational stance note would silently re-enter the plan/low action tier.
        "info" => "info".to_string(),
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
    /// The resolved auditing-agency brand ([`resolve_brand`]'s output, already folded through
    /// `ReportOptions::apply_env_defaults` by the time this JSON is built) — `None` means no
    /// brand resolved at all; the template must render [`NEUTRAL_COVER_TITLE`] with no agency
    /// name and no "Camerata" literal on the cover. See the Branding section of
    /// `docs/design/2026-09-13_audit-deliverable-review-fixes.md`.
    pub brand: Option<String>,
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
}

/// Item 7: "If you only do three things this week" — a half-page box right after the
/// executive summary. A buyer pricing remediation reads "these two criticals are about four
/// hours of work total" as the sentence that converts anxiety into a purchase order; a bare
/// finding list does not do that job.
#[derive(Debug, Clone, Serialize)]
pub struct ThreeThingsItemJson {
    pub headline: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    /// A rough, LABELED-as-rough hour estimate (e.g. `"2 to 4 hours"`), never a bare number
    /// dressed up as precise — see `effort_hours_bounds`.
    pub hours_label: String,
    /// FIX 7 (2026-09-13 review): one FACTUAL sentence of what this exposure MEANS in plain
    /// business terms (e.g. "A public bucket serves any object to anyone who has, or can
    /// guess, its URL..."), never fear-selling. `None` when no impact sentence is authored yet
    /// for this rule — the template must omit the line gracefully rather than render a blank
    /// one. See [`business_impact_for_rule`].
    pub impact: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreeThingsJson {
    pub items: Vec<ThreeThingsItemJson>,
    /// e.g. `"roughly 6 to 12 hours total (rough estimate)"`, or a note that some items have
    /// no effort estimate yet and were left out of the sum (never silently guessed).
    pub total_hours_label: String,
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
    /// The DEFECT at this specific object ("profiles table has no RLS: all member PII
    /// publicly readable and writable with the anon key."), never the rule's own invariant
    /// title ("Every table... has RLS enabled") — read cold, the invariant sounds like a
    /// clean bill of health. See `defect_headline`.
    pub headline: String,
    /// Carried through so item 7's "If you only do three things this week" box can map effort
    /// to a rough hour estimate without re-joining back to the original `Finding`.
    pub effort: Option<String>,
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
    /// Bug 4's "Conventions to consider" appendix: absence-type stance/architecture notes,
    /// `needs-review` low/medium rows, testing-style deviations in a repo with no test corpus,
    /// and any `info`-severity finding. VISIBLE (over-tell preserved) but deliberately OUTSIDE
    /// the do_now/do_next/plan action tiers and excluded from `curated_total` — a report
    /// appendix, not a work item. A critical or high finding is NEVER placed here (see
    /// `is_informational`).
    #[serde(default)]
    pub informational: Vec<FindingRefJson>,
}

// ── FIX 6: the real severity x effort priority grid ─────────────────────────────

/// One finding placed in a [`GridCellJson`] — just enough to render a short, readable tag in a
/// crowded cell (never the full curated-findings prose; that lives in its own section).
#[derive(Debug, Clone, Serialize)]
pub struct GridTagJson {
    pub rule_id: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub headline: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GridCellJson {
    pub findings: Vec<GridTagJson>,
}

/// One severity row of the grid, with one cell per [`PriorityGridJson::columns`] entry
/// (same length, same order — the template zips them positionally rather than looking a
/// column up by name).
#[derive(Debug, Clone, Serialize)]
pub struct GridRowJson {
    pub severity: String,
    pub cells: Vec<GridCellJson>,
}

/// FIX 6 (2026-09-13 review) — the ACTUAL severity-rows x effort-columns priority grid,
/// replacing the old 4-bucket list ("Do now" / "Do next" / "Plan" / "Accepted" rendered as 4
/// unrelated boxes, not a 2-D placement) that used to live under this section's heading. Built
/// by [`build_priority_grid`] from the SAME `do_now`/`do_next`/`plan` partition
/// `MatrixJson` already computed — never a second, independent bucketing pass. Accepted and
/// Informational findings are explicitly OUT OF SCOPE for this grid (an accepted-risk item or
/// an appendix note isn't a prioritization decision); their counts are surfaced as a footnote
/// instead of silently vanishing.
#[derive(Debug, Clone, Serialize)]
pub struct PriorityGridJson {
    /// Effort column keys present this run (`"low"` | `"medium"` | `"high"` | `"unscoped"`),
    /// in that fixed order, filtered to the ones at least one row actually uses.
    pub columns: Vec<String>,
    /// Severity rows present this run (`"critical"` | `"high"` | `"medium"` | `"low"`), in
    /// that fixed order, filtered to the ones with at least one finding.
    pub rows: Vec<GridRowJson>,
    pub accepted_count: usize,
    pub informational_count: usize,
}

/// Normalize an OPTIONAL calibrated effort into one of the grid's 4 fixed column keys. `None`
/// (a deterministic-floor/preview finding never gets a calibrated effort estimate — see
/// `effort_hours_bounds`) and any value outside `low`/`medium`/`high` both land in
/// `"unscoped"` rather than silently dropping the finding from the grid.
fn grid_effort_key(effort: Option<&str>) -> &'static str {
    match effort {
        Some("low") => "low",
        Some("medium") => "medium",
        Some("high") => "high",
        _ => "unscoped",
    }
}

/// FIX 6 — project the `do_now`/`do_next`/`plan` findings (Accepted and Informational are OUT
/// OF SCOPE, see [`PriorityGridJson`]'s doc comment) into the real 2-D grid. Rows/columns are
/// fixed orderings but only emitted when at least one finding uses them, so a small scan's
/// grid stays small instead of always rendering a fixed 4x4 of mostly-empty cells.
pub(crate) fn build_priority_grid(matrix: &MatrixJson) -> PriorityGridJson {
    const SEVERITY_ORDER: [&str; 4] = ["critical", "high", "medium", "low"];
    const EFFORT_ORDER: [&str; 4] = ["low", "medium", "high", "unscoped"];

    let action_findings: Vec<&FindingRefJson> = matrix
        .do_now
        .iter()
        .chain(matrix.do_next.iter())
        .chain(matrix.plan.iter())
        .collect();

    let present_severities: Vec<&str> = SEVERITY_ORDER
        .iter()
        .copied()
        .filter(|sev| action_findings.iter().any(|f| f.severity == *sev))
        .collect();
    let present_efforts: Vec<&str> = EFFORT_ORDER
        .iter()
        .copied()
        .filter(|eff| {
            action_findings
                .iter()
                .any(|f| grid_effort_key(f.effort.as_deref()) == *eff)
        })
        .collect();

    let rows = present_severities
        .iter()
        .map(|sev| {
            let cells = present_efforts
                .iter()
                .map(|eff| {
                    let mut findings: Vec<GridTagJson> = action_findings
                        .iter()
                        .filter(|f| {
                            f.severity == *sev && grid_effort_key(f.effort.as_deref()) == *eff
                        })
                        .map(|f| GridTagJson {
                            rule_id: f.rule_id.clone(),
                            repo: f.repo.clone(),
                            path: f.path.clone(),
                            line: f.line,
                            headline: f.headline.clone(),
                        })
                        .collect();
                    findings.sort_by(|a, b| {
                        (&a.repo, &a.path, a.line).cmp(&(&b.repo, &b.path, b.line))
                    });
                    GridCellJson { findings }
                })
                .collect();
            GridRowJson {
                severity: sev.to_string(),
                cells,
            }
        })
        .collect();

    PriorityGridJson {
        columns: present_efforts.into_iter().map(String::from).collect(),
        rows,
        accepted_count: matrix.accepted.len(),
        informational_count: matrix.informational.len(),
    }
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
    /// The defect at THIS object (see `defect_headline`) — the template renders this as the
    /// bold per-finding heading; the group's own rule id + invariant title (`CuratedGroupJson`)
    /// is demoted to a smaller subtitle line for registry traceability.
    pub headline: String,
    /// The rule's AUTHORED, client-facing remediation text (see [`resolve_fix`]) — rendered as
    /// its own "Fix:" line in the template, distinct from `detail`'s explanation of the
    /// violation. Placeholder tokens (`<table>`, `<function-name>`, `<bucket>`, …) have already
    /// been substituted with this finding's own `path`/`captures` (or a readable generic when
    /// unfilled) — nothing downstream needs to re-resolve tokens. `None` (never a fabricated
    /// sentence, and NEVER the rule's `directive` — see [`resolve_fix`]'s doc comment) when the
    /// rule's default option has no authored `remediation` yet: the template must omit the Fix
    /// block entirely in that case rather than render an empty line.
    pub fix: Option<String>,
    /// An optional, SINGLE labeled line to render UNDER the authored `fix` block ("For this
    /// finding: …") when the model-written `detail` carries a finding-specific remediation
    /// sentence distinct from both `fix` and the body `detail` text. `None` in every case as of
    /// this pass (see [`resolve_fix`]'s module doc for why: a reliable distinct-sentence
    /// extractor was judged not worth the risk of rendering duplicated/low-quality text) — the
    /// field exists so the template layer has a stable place to read from once/if that
    /// extraction is built. Never rendered in place of `fix`; only ever alongside it.
    pub fix_for_this_finding: Option<String>,
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
    /// FIX 3 (2026-09-13 review): a short, FACTUAL "what happens next" paragraph, rendered in
    /// its own section right before Methodology (`typ:397`'s heading). Authored prose, same
    /// pattern as `deterministic_note`/`ai_tier_note` below — never an LLM call, never
    /// pitch-toned framing (no urgency language, no pricing claims beyond "the rate herein").
    pub next_steps: String,
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
    pub three_things: ThreeThingsJson,
    pub scorecard: ScorecardJson,
    pub matrix: MatrixJson,
    /// FIX 6 (2026-09-13 review): the real severity x effort grid the template renders under
    /// the "Severity x effort" heading — derived from `matrix` (see [`build_priority_grid`]),
    /// not an independent bucketing. `matrix` itself is kept (still backs the exec-summary
    /// counts and the "Three things this week" box's `do_now` selection); the template no
    /// longer reads `matrix` directly for its own section.
    pub priority_grid: PriorityGridJson,
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

/// FIX 3 (2026-09-13 review) — a short, FACTUAL "what happens next" paragraph, rendered in a
/// new section before Methodology. Deliberately non-pitch: no urgency language, no claim about
/// price beyond "the rate set out in the engagement" (a specific number lives in the
/// engagement paperwork, never fabricated here). Three factual steps, one paragraph, no em/en
/// dashes (house style for this client deliverable — see `AUDIT_REPORT_DISCLAIMER`'s doc
/// comment).
pub const NEXT_STEPS_NOTE: &str =
    "There are three steps from here. First, the do-now items above get fixed, by your own \
     team, an outside contractor, or the auditor, at the rate set out in the engagement. \
     Second, a retest: the auditor re-scans the repository once the fixes are in and signs a \
     short addendum confirming each item is closed. Third, ongoing coverage: a monthly delta \
     rescan plus on-call architect availability for anything new the codebase introduces \
     between engagements.";

// ── FIX 7 business-impact map (§"If you only do three things this week") ───────────

/// FIX 7 (2026-09-13 review) — one FACTUAL, non-fear-selling sentence of what a rule's
/// exposure MEANS in plain business terms, authored per `rule_id`. Deliberately a small Rust
/// map, NOT a corpus TOML field: the corpus-wide `remediation` authoring pass (FIX 1, Phase
/// 2a) runs in parallel against these same TOML files, and folding a second authored-prose
/// field into that same parallel-editable surface risks a merge collision over the same lines
/// for no benefit — this is a small, independently-owned map instead. Covers the Supabase
/// pack (the corpus currently backing the do-now items the flagship sample exercises) plus the
/// handful of universal/deterministic-floor rules most likely to land in "do now" on a typical
/// scan. Returns `None` for anything not yet authored — the template must omit the line
/// gracefully in that case, never fabricate one. NOTE: this could migrate to a corpus `impact`
/// field alongside `remediation` once the two authoring passes are no longer running in
/// parallel; deliberately not done in this pass.
pub(crate) fn business_impact_for_rule(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        // ── Supabase: RLS ──────────────────────────────────────────────────────────
        "SUPABASE-RLS-ENABLED-1" => Some(
            "No RLS on this table means every row is readable and writable by anyone holding \
             the public anon key, with no login required.",
        ),
        "SUPABASE-RLS-NO-POLICY-1" => Some(
            "RLS enabled with zero policies denies all access by default, which typically \
             breaks the feature for real users rather than exposing data, and often gets \
             patched with an overly permissive policy under deadline pressure.",
        ),
        "SUPABASE-RLS-POLICY-DISABLED-1" => Some(
            "A policy exists but RLS itself is off, so the policy is decorative: every row is \
             exposed exactly as if no policy had ever been written.",
        ),
        "SUPABASE-RLS-PERMISSIVE-TRUE-1" => Some(
            "A policy that always evaluates to true grants every anon or authenticated caller \
             full access to the table, which is functionally the same exposure as having no \
             RLS at all.",
        ),
        "SUPABASE-RLS-USER-METADATA-1" => Some(
            "Authorizing against user-editable metadata lets any authenticated user grant \
             themselves elevated access by editing their own profile fields.",
        ),
        "SUPABASE-RLS-VIEW-INVOKER-1" => Some(
            "A view without security_invoker runs with the view creator's privileges, so it \
             can silently bypass the Row Level Security policies protecting the underlying \
             tables.",
        ),
        "SUPABASE-RLS-INITPLAN-1" => Some(
            "This is a performance defect, not an access hole: unwrapped auth calls \
             re-evaluate once per row and can make an otherwise-correct RLS policy time out \
             under real load.",
        ),
        // ── Supabase: auth ─────────────────────────────────────────────────────────
        "SUPABASE-AUTH-EDGE-JWT-1" => Some(
            "An edge function reachable with no authentication check can be invoked by anyone \
             on the internet, including for state-changing operations like a payment.",
        ),
        "SUPABASE-AUTH-GETSESSION-SERVER-1" => Some(
            "Trusting an unverified session cookie on the server lets a forged cookie \
             impersonate any user at that code path.",
        ),
        "SUPABASE-AUTH-SERVICE-ROLE-BYPASS-1" => Some(
            "A service-role client bypasses Row Level Security entirely, so a request handler \
             using it without its own authorization check grants full database access to \
             whoever can reach that endpoint.",
        ),
        "SUPABASE-AUTH-USERS-EXPOSED-1" => Some(
            "Exposing auth.users through a view or grant leaks every user's email, phone, and \
             identity metadata to anyone who can query it.",
        ),
        // ── Supabase: secrets, storage, exposure, functions ───────────────────────────
        "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1" => Some(
            "A service-role key shipped to the browser bypasses Row Level Security and grants \
             full read and write access to every table to anyone who opens developer tools.",
        ),
        "SUPABASE-STORAGE-PUBLIC-BUCKET-1" => Some(
            "A public bucket serves any object to anyone who has, or can guess, its URL, with \
             no login and no access policy involved.",
        ),
        "SUPABASE-STORAGE-OBJECT-POLICY-1" => Some(
            "Unscoped write or delete access on storage objects lets any caller overwrite or \
             delete files that belong to other users.",
        ),
        "SUPABASE-EXPOSURE-MATVIEW-1" => Some(
            "A materialized view cannot carry row-level policies, so exposing it in the API \
             makes it all-or-nothing: every row is visible to anyone who can query it.",
        ),
        "SUPABASE-EXPOSURE-SCHEMAS-1" => Some(
            "Every schema added to the exposed-schemas list becomes reachable over the API, \
             including any table in it that was never reviewed for Row Level Security.",
        ),
        "SUPABASE-FUNC-SEARCH-PATH-1" => Some(
            "A SECURITY DEFINER function without a fixed search_path can be hijacked into \
             running an attacker-created function with the function owner's elevated \
             privileges.",
        ),
        // ── Universal / deterministic-floor ────────────────────────────────────────
        "ARCH-NO-SECRETS-IN-URL-1" => Some(
            "A credential carried in a URL is written into proxy logs, browser history, and \
             server access logs by default, which is a durable exposure even after the \
             credential is rotated.",
        ),
        "SEC-NO-PRIVATE-KEY-1" => Some(
            "A private key committed to the repository is exposed to everyone with repository \
             access, past and future, and stays readable in git history even after the file \
             is deleted.",
        ),
        "SEC-NO-SECRET-FILE-1" => Some(
            "A committed secret-bearing file (a private key, keystore, or real .env) is \
             exposed to everyone with repository access, past and future.",
        ),
        "SEC-NO-HARDCODED-SECRETS-1" => Some(
            "A hardcoded credential in source is exposed to everyone with repository access, \
             and to anyone who ever receives a copy of the build.",
        ),
        "SEC-NO-DISABLED-TLS-1" => Some(
            "Disabled certificate verification lets any network position between the client \
             and server intercept or alter traffic without detection.",
        ),
        "SEC-NO-UNSAFE-DESERIALIZATION-1" => Some(
            "Deserializing untrusted input without safeguards can let an attacker execute \
             arbitrary code on the server simply by controlling the input.",
        ),
        "SEC-NO-RAW-SQL-CONCAT-1" => Some(
            "String-concatenated SQL lets an attacker who controls any part of the input \
             read, modify, or delete arbitrary rows in the database.",
        ),
        _ => None,
    }
}

// ── Citation join (§4.4) ─────────────────────────────────────────────────────────

/// Join `rule_id` (+ `preview_tool`, when set) against the loaded corpus to recover its
/// authoritative sources. Priority: (1) a corpus rule with non-empty `sources` is
/// "grounded" — cite them verbatim (CWE/OWASP/RFC/linter docs); (2) a deterministic
/// preview finding with no corpus citation is labeled by the REAL tool that enforces it
/// (still honest — just not corpus-documented); (3) anything else (a free-text/AI-tier
/// rule id the corpus never saw) is "AI-advisory, model-inferred." — never dressed up.
/// Item 3: a source counts as an external authority only when its `url` is a real, fetchable
/// external URL (CWE / OWASP / RFC / Supabase docs / a linter's own docs...). Camerata's own
/// internal scaffolding docs (`docs/ENFORCEMENT.md` §RULE_REGISTRY entries, which exist only in
/// Camerata-built repos, never in an arbitrary client's) are filtered out entirely rather than
/// rendered as if they were an authority a client could go verify.
fn is_external_source_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

pub(crate) fn resolve_citation(
    rule_id: &str,
    preview_tool: Option<&str>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> CitationJson {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(rule_id)) {
        let sources: Vec<CitationSourceJson> = rule
            .sources
            .iter()
            .filter(|s| is_external_source_url(&s.url))
            .map(|s| CitationSourceJson {
                title: s.title.clone(),
                url: s.url.clone(),
                linter: s.linter.clone(),
            })
            .collect();
        if !sources.is_empty() {
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

/// Join `rule_id` against the loaded corpus to recover its DEFAULT option's AUTHORED
/// `remediation` text, then instantiate that text's placeholder tokens (`<table>`,
/// `<function-name>`, `<bucket>`, `<path>`, …) from `finding`'s own `path`/`captures`. This is a
/// serialization-time join exactly like [`resolve_citation`] for the RULE-level lookup, plus a
/// per-FINDING instantiation step — the authored text is a property of the rule, but naming the
/// specific table/function/bucket in THIS codebase is a property of the finding.
///
/// # `remediation`, never `directive` (2026-09-13 product-review Fix 1)
/// `directive` is the rule's DETECTION/enforcement recipe — internal, agent-facing instructions
/// for how Camerata itself finds this defect (e.g. "Regex-scan supabase/config.toml for
/// `verify_jwt=false`… feed the matched source to the prose tier…"). It leaks internals and
/// confuses a paying client. `remediation` is a SEPARATE, authored field: 2-4 imperative
/// sentences telling the CLIENT what to change, where, and how to verify the fix. This function
/// binds ONLY to `remediation` and must NEVER fall back to `directive` (or any other rule field)
/// when `remediation` is absent — fail-safe here means silently omitting the Fix block, not
/// leaking the recipe. See `docs/design/2026-09-13_audit-deliverable-review-fixes.md` Fix 1.
///
/// Returns `None` (never a fabricated sentence, never `directive`) when: the corpus is absent,
/// the rule id has no corpus entry, the rule has no options at all, the rule has no adopted
/// DEFAULT option, or the resolved option's `remediation` field is itself absent/blank in the
/// TOML (not yet authored — the common case until the corpus-wide authoring pass in Phase 2a
/// lands). Surfaced as the PDF's "Fix:" line (`CuratedSiteJson::fix`) and the workbook's
/// "Recommended Fix" column / `findings.json`'s `fix` field (`FindingRow::fix`) — same join, all
/// three, so an absent remediation reads as an honestly OMITTED Fix block everywhere rather than
/// one artifact inventing text the others don't have.
pub(crate) fn resolve_fix(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
) -> Option<String> {
    let rule = corpus.and_then(|c| c.get_by_id(rule_id))?;
    // `chosen_option = None`: the report has no notion of a per-project chosen option today
    // (that lives in onboarding's `SelectedRule` binding, not in a `Finding`/`ScanReport`), so
    // this always resolves the rule's own DEFAULT — matching the design doc's "the corpus
    // rule's default `[[option]].remediation`" wording exactly, not a project-specific choice.
    let option = rule.resolved_option(None)?;
    let remediation = option.remediation.as_deref()?.trim();
    if remediation.is_empty() {
        return None;
    }
    Some(instantiate_remediation(
        remediation,
        &finding.path,
        &finding.captures,
    ))
}

/// The readable, never-internal-sounding generic noun phrase substituted for a placeholder
/// token when the finding carries no captured value for it. Keyed by the token's bare name
/// (angle brackets stripped) exactly as it appears in authored `remediation` (and, before it,
/// the corpus rules' own `directive`) text — extend this map when a new rule introduces a new
/// domain token, so its remediation can still render something readable before a detector is
/// wired to capture the real object. `<path>`/`<file>` are handled separately in
/// [`instantiate_remediation`] (filled from the finding's own `path`, which is always present),
/// so they are not listed here; the catch-all arm below is still a safe fallback for them too if
/// `path` is ever empty.
fn generic_placeholder_filler(token: &str) -> &'static str {
    match token {
        "table" => "the affected table",
        "function-name" | "function" => "the affected function",
        "bucket" => "the affected bucket",
        "view" => "the affected view",
        "matview" => "the affected materialized view",
        "schema" => "the affected schema",
        "policy" => "the affected policy",
        "path" | "file" => "the affected file",
        _ => "the affected resource",
    }
}

/// Substitute every `<token>` placeholder in authored `remediation` text with a concrete value,
/// so the same authored sentence names the actual object in THIS codebase. Resolution order per
/// token: (1) `<path>`/`<file>` always resolve to `path` (the finding's own path) when it is
/// non-empty; (2) any other token resolves to `captures.get(token)` when the detector populated
/// one; (3) otherwise a readable generic from [`generic_placeholder_filler`]. This function is
/// the SOLE gate between authored text and the client, so it is deliberately exhaustive: EVERY
/// well-formed `<...>` span found in `remediation` is replaced by construction — there is no
/// code path that can leave a raw `<token>` in the returned string. A malformed span (a bare `<`
/// with no matching `>` anywhere after it) is passed through unmodified rather than guessed at,
/// since that is prose, not a token — this can only happen if a rule author literally types an
/// unmatched `<` in remediation copy, not from anything a detector or the finding data supplies.
pub(crate) fn instantiate_remediation(
    remediation: &str,
    path: &str,
    captures: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut out = String::with_capacity(remediation.len());
    let mut rest = remediation;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        let Some(end) = after_open.find('>') else {
            // No closing bracket anywhere ahead — not a well-formed token. Emit the remainder
            // verbatim rather than treating a stray `<` as something to defend against.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token = after_open[..end].trim();
        let filled = if matches!(token, "path" | "file") && !path.trim().is_empty() {
            path.to_string()
        } else {
            captures
                .get(token)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| generic_placeholder_filler(token).to_string())
        };
        out.push_str(&filled);
        rest = &after_open[end + 1..];
    }
    out.push_str(rest);
    out
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
pub(crate) fn category_for(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> String {
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
pub(crate) fn matrix_bucket(
    disposition: Disposition,
    severity: &str,
    effort: Option<&str>,
) -> &'static str {
    match disposition {
        Disposition::Ignored | Disposition::BaselineAccepted | Disposition::WaivedInline => {
            "accepted"
        }
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

/// A style rule needs an established corpus before deviations are findings (Bug 4 §2d). A
/// repo with fewer than this many test files has no test corpus to speak of, so
/// `testing-style` deviations are conventions-to-consider, not defects.
pub(crate) const MIN_STYLE_CORPUS_FILES: usize = 3;

/// Rule DOMAINS whose `structured` (decision-shaped) rules are architectural/framework STANCE
/// rules — "the project hasn't adopted X" conventions rather than concrete defects. This is
/// the "universal or framework layer" of Bug 4 §2a, expressed over the corpus's real domain
/// vocabulary (the folder each rule lives under). Language-core domains (`rust`, `go`,
/// `python`, `java`, `csharp`, `ruby`) and security/infra-checker domains (`supabase`, `sql`,
/// `permissions`, `iac`, `ci-cd`) are deliberately ABSENT: a located violation there is a real
/// defect even in a tiny app. A whitelist (not a blacklist) so an unrecognized future domain
/// fails OPEN to over-telling — it stays in the action tier rather than being auto-demoted.
pub(crate) const STANCE_LAYER_DOMAINS: &[&str] =
    &["universal", "api-layer", "fullstack", "ui", "javascript", "integration"];

/// Whether a finding is a "convention to consider" rather than an action item (Bug 4). Routes
/// to the `informational` matrix appendix instead of do_now/do_next/plan. NEVER true for a
/// critical/high finding or for a finding the auditor has explicitly dispositioned — the hard
/// invariant that keeps true defects in the action tiers. `severity` must already be
/// normalized. The four independent informational signals (design §2a/2c/2d + the `info` tier
/// itself):
///   - `info`-severity (e.g. the unexposed-schema RLS note from I3),
///   - `testing-style` category in a repo below the test-corpus threshold (§2d),
///   - `needs-review` confidence at ≤ medium severity (§2c),
///   - absence-type (`located == false`) `structured` stance-layer rule at ≤ medium (§2a).
pub(crate) fn is_informational(
    finding: &Finding,
    disposition: Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
) -> bool {
    // Only ever re-bucket an OPEN row — an auditor's explicit call (accepted/tech-debt/FP)
    // keeps its own destination.
    if disposition != Disposition::Unresolved {
        return false;
    }
    // Hard invariant: a critical or high finding is never auto-informational, whatever family.
    if severity == "critical" || severity == "high" {
        return false;
    }
    // The `info` tier is informational by definition (nothing below `low` is an action item).
    if severity == "info" {
        return true;
    }
    // §2d — testing-style deviation with no test corpus to deviate FROM.
    if finding.category.as_deref() == Some("testing-style") && test_file_count < MIN_STYLE_CORPUS_FILES
    {
        return true;
    }
    // §2c — a low/medium finding the calibrator itself flagged as debatable.
    if finding.confidence.as_deref() == Some("needs-review") {
        return true;
    }
    // §2a — an absence-type structured stance-rule note (the generic-arch/style over-firing).
    if !finding.located {
        if let Some(rule) = corpus.and_then(|c| c.get_by_id(&finding.rule_id)) {
            if rule.enforcement == camerata_rules::EnforcementKind::Structured
                && STANCE_LAYER_DOMAINS.contains(&rule.domain.as_str())
            {
                return true;
            }
        }
    }
    false
}

fn finding_ref(f: &Finding, severity: &str, headline: String) -> FindingRefJson {
    FindingRefJson {
        rule_id: f.rule_id.clone(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        severity: severity.to_string(),
        headline,
        effort: f.effort.clone(),
    }
}

/// Item 1 (the biggest ask): derive a defect-first HEADLINE for one finding, distinct from
/// the rule's own invariant title. Mechanical, not hand-faked — every finding already carries
/// a `detail` string (the checker's own explanation of the violation, whether it came from the
/// deterministic floor, a preview tool, or the calibrated AI tier), and house convention is for
/// that `detail` to LEAD with the defect statement itself ("the profiles table has no RLS...")
/// before supporting evidence. Taking `detail`'s leading sentence as the headline therefore
/// generalizes beyond this one report's fixture: it works for any finding whose `detail` text
/// follows that convention, and degrades safely (falls back to the rule's own
/// title/rule_id, never an empty string) for the rare finding with no `detail` at all.
pub(crate) fn defect_headline(detail: &str, fallback: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    // First sentence boundary: prefer ". " (mid-paragraph), else a bare trailing '.', else the
    // first '.' found anywhere, else the whole (short, presumably headline-shaped) string.
    let end = if let Some(idx) = trimmed.find(". ") {
        idx + 1
    } else if let Some(idx) = trimmed.find('.') {
        idx + 1
    } else {
        trimmed.len()
    };
    let mut headline = trimmed[..end].trim().to_string();
    if !headline.ends_with('.') {
        headline.push('.');
    }
    headline
}

/// Deterministic exec-summary narrative (never an LLM call). Overridden verbatim by
/// `ReportOptions::executive_summary_override` when the client supplies one. Special-cases
/// zero candidates (a plain "0 candidate finding(s) were reviewed..." reads as broken, not
/// clean) and uses real pluralization throughout (`noun`) rather than the "(s)" CLI-ism.
///
/// FIX 4 (2026-09-13 review, "page 3 states the top-3 three times"): this used to LEAD with a
/// blast-radius sentence composed from the top do-now findings' own headlines — the SAME 3
/// findings the "If you only do three things this week" box (immediately below, on the same
/// page) already names in full. Between that box and the (also-dropped) "Top priority items"
/// bullet list, the top 3 were stated three separate times on one page. The exec summary is
/// now curation-and-counts ONLY: the false-positive triage line, then the single one-pass
/// bucket-count partition. `do_now + do_next + plan + accepted == curated_total` ALWAYS (every
/// code finding lands in exactly one matrix bucket by construction — see `matrix_bucket`), so
/// listing all four counts once is a complete, self-checking picture; there is deliberately no
/// "and N still open" tacked on (a SUBSET of those same four counts) and, as of this pass, no
/// restated finding headline either — the "Three things this week" box is now the ONE place on
/// the page that names the top items.
fn default_narrative(
    candidates_reviewed: usize,
    excluded_fp: usize,
    curated_total: usize,
    do_now: usize,
    do_next: usize,
    plan: usize,
    accepted: usize,
) -> String {
    if candidates_reviewed == 0 {
        return "The scan surfaced no candidate findings to review in this run.".to_string();
    }
    format!(
        "{} were reviewed; {} {} dispositioned as false positives by the auditor and excluded \
         entirely from this report. Of the remaining {}: {do_now} do now, {do_next} do next, \
         {plan} planned, and {accepted} accepted as risk.",
        noun(candidates_reviewed, "candidate finding", "candidate findings"),
        excluded_fp,
        if excluded_fp == 1 { "was" } else { "were" },
        noun(curated_total, "curated finding", "curated findings"),
    )
}

/// Item 7's effort -> rough-hour mapping (a judgment call, documented here rather than buried
/// in a magic number): the calibration pass's three qualitative tiers translate to rough,
/// LABELED-as-rough wall-clock ranges a buyer can price against. `low` ~= a same-day, scoped
/// fix (rotate a key, flip a migration flag); `medium` ~= about a business day (touches a few
/// call sites or needs a short design pass); `high` ~= the better part of a week (a real
/// refactor or a cross-cutting change). Returns `None` for the hour bounds when effort was
/// never calibrated (a deterministic-floor/preview finding, per M3) — the label still reads
/// honestly ("not yet estimated") rather than silently guessing a number.
pub(crate) fn effort_hours_bounds(effort: Option<&str>) -> (Option<(u32, u32)>, String) {
    match effort {
        Some("low") => (Some((2, 4)), "2 to 4 hours".to_string()),
        Some("medium") => (Some((8, 16)), "1 to 2 days (about 8 to 16 hours)".to_string()),
        Some("high") => (Some((24, 40)), "3 to 5 days (about 24 to 40 hours)".to_string()),
        _ => (None, "not yet estimated".to_string()),
    }
}

/// Sum the rough hour ranges across item 7's (up to 3) do-now findings. Only sums items with a
/// calibrated effort; when one or more items have none, the total says so explicitly rather
/// than folding a guessed number into the sum.
fn total_hours_label(items: &[&FindingRefJson]) -> String {
    if items.is_empty() {
        return "No do-now items this run.".to_string();
    }
    let mut lo_sum = 0u32;
    let mut hi_sum = 0u32;
    let mut uncounted = 0usize;
    for f in items {
        match effort_hours_bounds(f.effort.as_deref()).0 {
            Some((lo, hi)) => {
                lo_sum += lo;
                hi_sum += hi;
            }
            None => uncounted += 1,
        }
    }
    let counted = items.len() - uncounted;
    if counted == 0 {
        return "None of these items has a calibrated effort estimate yet; ask the auditor for \
                a rough scoping pass before pricing remediation."
            .to_string();
    }
    let mut label = format!("Roughly {lo_sum} to {hi_sum} hours total (rough estimate)");
    if uncounted > 0 {
        label.push_str(&format!(
            "; {} without an effort estimate yet {} not included in this total",
            noun(uncounted, "item", "items"),
            if uncounted == 1 { "is" } else { "are" }
        ));
    }
    label.push('.');
    label
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
        // Bug 4: convention-to-consider rows are diverted to the informational appendix BEFORE
        // the severity×effort quadrant — they must never reach do_now/do_next/plan. The
        // invariant that a critical/high is never informational lives in `is_informational`.
        let bucket = if is_informational(f, *disposition, severity, corpus, report.test_file_count) {
            "informational"
        } else {
            matrix_bucket(*disposition, severity, f.effort.as_deref())
        };
        let target = match bucket {
            "do_now" => &mut matrix.do_now,
            "do_next" => &mut matrix.do_next,
            "plan" => &mut matrix.plan,
            "informational" => &mut matrix.informational,
            _ => &mut matrix.accepted,
        };
        let fallback_title = corpus
            .and_then(|c| c.get_by_id(&f.rule_id))
            .map(|r| r.title.clone())
            .unwrap_or_else(|| f.rule_id.clone());
        let headline = defect_headline(&f.detail, &fallback_title);
        target.push(finding_ref(f, severity, headline));
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
                // Bug 4: keep the curated-site LABEL in lockstep with the matrix cell this
                // finding actually lands in — an informational row reads "Convention to
                // consider", never "Open (recommended: Plan)".
                let bucket =
                    if is_informational(f, *disposition, severity, corpus, report.test_file_count) {
                        "informational"
                    } else {
                        matrix_bucket(*disposition, severity, f.effort.as_deref())
                    };
                let confirmed_by_client = dispositions
                    .get(&finding_key(f))
                    .map(|d| d.confirmed_by_client)
                    .unwrap_or(false);
                CuratedSiteJson {
                    repo: f.repo.clone(),
                    path: f.path.clone(),
                    line: f.line,
                    snippet: cap_snippet(&f.snippet),
                    detail: f.detail.clone(),
                    severity: severity.clone(),
                    effort: f.effort.clone(),
                    confidence: f.confidence.clone(),
                    disposition: disposition_label(*disposition, reason, bucket, confirmed_by_client),
                    also_matches: f.also_matches.clone(),
                    headline: defect_headline(&f.detail, &title),
                    fix: resolve_fix(&rule_id, corpus, f),
                    // See `CuratedSiteJson::fix_for_this_finding`'s doc comment: deliberately
                    // never populated in this pass.
                    fix_for_this_finding: None,
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
    // Bug 4: informational (appendix) rows are NOT curated action items — they sit outside the
    // four-bucket partition, so `curated_total` excludes them and the self-checking narrative
    // invariant `do_now + do_next + plan + accepted == curated_total` still holds exactly.
    let informational = matrix.informational.len();
    let curated_total = code_findings.len() - informational;
    let do_now = matrix.do_now.len();
    let do_next = matrix.do_next.len();
    let plan = matrix.plan.len();
    let accepted = matrix.accepted.len();
    // Still-open action items (every informational row is Unresolved by construction — see
    // `is_informational` — so subtracting them keeps `open` an ACTION count, not an appendix one).
    let open = code_findings
        .iter()
        .filter(|(_, d, _, _)| *d == Disposition::Unresolved)
        .count()
        - informational;
    let mut do_now_sorted = matrix.do_now.clone();
    do_now_sorted.sort_by_key(|f| match f.severity.as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    });
    // FIX 4 (2026-09-13 review): `top3_do_now` still feeds the "Three things this week" box
    // below (the ONE place these findings are now named) — the exec-summary's own
    // `top_do_now` bullet list and blast-radius lead sentence are gone (see
    // `default_narrative`'s doc comment).
    let top3_do_now: Vec<&FindingRefJson> = do_now_sorted.iter().take(3).collect();
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
    };

    // ── Item 7: "If you only do three things this week" ───────────────────────
    // The same top (up to 3) do-now findings, plus a rough hour estimate per item and a total —
    // the sentence that turns remediation anxiety into a purchase order.
    let three_things_items: Vec<ThreeThingsItemJson> = top3_do_now
        .iter()
        .map(|f| {
            let (_, hours_label) = effort_hours_bounds(f.effort.as_deref());
            ThreeThingsItemJson {
                headline: f.headline.clone(),
                repo: f.repo.clone(),
                path: f.path.clone(),
                line: f.line,
                rule_id: f.rule_id.clone(),
                severity: f.severity.clone(),
                hours_label,
                impact: business_impact_for_rule(&f.rule_id).map(str::to_string),
            }
        })
        .collect();
    let total_hours_label = total_hours_label(&top3_do_now);
    let three_things = ThreeThingsJson {
        items: three_things_items,
        total_hours_label,
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
        // `build_report_json` stays pure/no-I/O (per its own doc comment): it does NOT read
        // `CAMERATA_REPORT_BRAND`/`CAMERATA_REPORT_PREPARED_BY` itself. By the time `opts`
        // reaches here the HTTP handler has already called `ReportOptions::apply_env_defaults`
        // (which does the real env read), so `opts.brand`/`opts.prepared_by` already carry the
        // resolved precedence. Re-running them through the resolver here with `env: None` is
        // just the same blank/non-blank -> `Option<String>` normalization, not a second env
        // lookup, so a direct unit test (which never calls `apply_env_defaults`) still gets
        // correct precedence semantics by setting `opts.brand`/`opts.prepared_by` directly.
        prepared_by: resolve_prepared_by(opts, None),
        brand: resolve_brand(opts, None),
        stats,
    };

    // ── Methodology (M4: ported from the better sample copy, no dashes per S12) ────────
    let methodology = MethodologyJson {
        candidates_reviewed,
        excluded_false_positive: excluded_fp,
        next_steps: NEXT_STEPS_NOTE.to_string(),
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

    let priority_grid = build_priority_grid(&matrix);

    AuditReportJson {
        cover,
        executive_summary,
        three_things,
        scorecard: ScorecardJson {
            rows: scorecard_rows,
        },
        matrix,
        priority_grid,
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
            test_file_count: 0,
            files_excluded: 2,
            code_chars: 5000,
            excluded_mechanical_rules: Vec::new(),
            findings,
            proposed_rules: Vec::new(),
            gated: false,
            blocked: false,
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
            confirmed_by_client: false,
        }
    }

    /// Like `wire`, but for an `Ignored` disposition a REAL client has actually confirmed.
    fn confirmed_wire(reason: &str) -> DispositionWire {
        DispositionWire {
            state: "Ignored".to_string(),
            reason: reason.to_string(),
            bucket: String::new(),
            confirmed_by_client: true,
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
    fn ignored_finding_is_accepted_risk_with_reason_when_client_confirmed() {
        let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f), confirmed_wire("accepted for now"));
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Accepted risk: accepted for now"
        );
        assert_eq!(json.matrix.accepted.len(), 1);
        assert_eq!(json.executive_summary.accepted, 1);
    }

    // ── Item 4 regression: never fabricate a client disposition ───────────────

    #[test]
    fn ignored_finding_without_client_confirmation_needs_client_confirmation() {
        // The DEFAULT (confirmed_by_client absent/false) must NEVER read as if a client
        // confirmed anything, no matter how confident the auditor's own reason sounds — a
        // security-literate reader who spots an invented client disposition discards the
        // whole document.
        let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&f),
            wire("Ignored", "looks like defense-in-depth only", ""),
        );
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Needs client confirmation (auditor's proposed rationale: looks like defense-in-depth only)"
        );
        assert_eq!(json.matrix.accepted.len(), 1, "still an accepted-bucket disposition");
    }

    #[test]
    fn ignored_finding_without_reason_or_confirmation_is_plain_needs_client_confirmation() {
        let f = finding("ARCH-NO-SECRETS-IN-URL-1", "a.rs", 1, "medium");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f), wire("Ignored", "", ""));
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Needs client confirmation"
        );
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
            !json.three_things.items.is_empty(),
            "the three-things box must not be empty when a critical do_now finding exists"
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

    // ── Item 1: defect headline, not the rule's invariant title ────────────────

    #[test]
    fn defect_headline_takes_the_first_sentence_of_detail() {
        assert_eq!(
            defect_headline(
                "The profiles table has no RLS. Anyone with the anon key can read and write \
                 every row.",
                "Every table has Row Level Security enabled"
            ),
            "The profiles table has no RLS."
        );
    }

    #[test]
    fn defect_headline_falls_back_to_the_rule_title_when_detail_is_empty() {
        assert_eq!(
            defect_headline("", "Every table has Row Level Security enabled"),
            "Every table has Row Level Security enabled"
        );
    }

    #[test]
    fn curated_and_matrix_findings_carry_a_defect_headline_distinct_from_the_rule_title() {
        let mut f = finding("SEC-1", "a.rs", 1, "critical");
        f.detail = "A service_role key ships to every browser. It bypasses all Row Level \
                    Security."
            .to_string();
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(
            json.curated_findings[0].sites[0].headline,
            "A service_role key ships to every browser."
        );
        assert_eq!(
            json.matrix.do_now[0].headline,
            "A service_role key ships to every browser."
        );
        // The rule's own title/id is still carried separately, for registry traceability.
        assert_eq!(json.curated_findings[0].rule_id, "SEC-1");
        // FIX 4 (2026-09-13 review): the "Three things this week" box leads with the headline,
        // not the rule id — this is now the ONE place the exec-summary page names a do-now
        // finding (see `narrative_does_not_restate_the_top_do_now_headline` below).
        assert!(json.three_things.items[0].headline.starts_with("A service_role key ships"));
    }

    // ── Item 2 / FIX 4: exec-summary is counts-only, no restated top-3 ─────────

    #[test]
    fn narrative_does_not_restate_the_top_do_now_headline() {
        // FIX 4 (2026-09-13 review, "page 3 states the top-3 three times"): the narrative used
        // to LEAD with "As shipped: {top do-now headline}" — the same finding the "Three
        // things this week" box (and, before this fix, a "Top priority items" bullet list)
        // already names on the same page. The exec summary is now curation + counts only.
        let mut f = finding("SEC-1", "a.rs", 1, "critical");
        f.detail = "The profiles table has no RLS.".to_string();
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(
            !json.executive_summary.narrative.contains("As shipped"),
            "{}",
            json.executive_summary.narrative
        );
        assert!(
            !json
                .executive_summary
                .narrative
                .contains("The profiles table has no RLS."),
            "the narrative must not restate the do-now finding's own headline: {}",
            json.executive_summary.narrative
        );
    }

    #[test]
    fn narrative_bucket_counts_are_a_one_pass_partition_with_no_subset_restatement() {
        // Previously: "... 2 do now, 2 do next, 1 planned, 1 accepted ... and 5 still open"
        // forced the reader to notice 5 was a SUBSET restatement of the four counts just
        // given, not new information. The fix: state the four-bucket partition once and stop.
        let f1 = finding("SEC-1", "a.rs", 1, "critical");
        let f2 = finding("SEC-2", "b.rs", 2, "low");
        let report = report_with(vec![f1, f2], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(
            !json.executive_summary.narrative.contains("still open"),
            "{}",
            json.executive_summary.narrative
        );
        assert!(
            json.executive_summary.narrative.contains("1 do now, 0 do next, 1 planned, and 0 accepted as risk"),
            "{}",
            json.executive_summary.narrative
        );
    }

    // ── Item 7: "If you only do three things this week" ────────────────────────

    #[test]
    fn effort_hours_bounds_maps_each_tier_to_a_labeled_rough_range() {
        assert_eq!(effort_hours_bounds(Some("low")).1, "2 to 4 hours");
        assert_eq!(
            effort_hours_bounds(Some("medium")).1,
            "1 to 2 days (about 8 to 16 hours)"
        );
        assert_eq!(
            effort_hours_bounds(Some("high")).1,
            "3 to 5 days (about 24 to 40 hours)"
        );
        assert_eq!(effort_hours_bounds(None).1, "not yet estimated");
    }

    #[test]
    fn three_things_box_lists_up_to_three_do_now_items_with_hours_and_a_total() {
        let mut f1 = finding("SEC-1", "a.rs", 1, "critical");
        f1.effort = Some("low".to_string());
        f1.detail = "The profiles table has no RLS.".to_string();
        let mut f2 = finding("SEC-2", "b.rs", 2, "critical");
        f2.effort = Some("low".to_string());
        f2.detail = "The service_role key ships to the browser.".to_string();
        let report = report_with(vec![f1, f2], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.three_things.items.len(), 2);
        assert_eq!(json.three_things.items[0].hours_label, "2 to 4 hours");
        assert_eq!(
            json.three_things.total_hours_label,
            "Roughly 4 to 8 hours total (rough estimate)."
        );
    }

    #[test]
    fn three_things_box_flags_items_with_no_effort_estimate_instead_of_guessing() {
        // A do_now item can be uncalibrated (a deterministic-floor critical never gets a
        // calibrated effort, per M3) — the total must say so, never silently fold in a
        // guessed number.
        let f = finding("SEC-1", "a.rs", 1, "critical");
        assert_eq!(f.effort, None);
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.three_things.items[0].hours_label, "not yet estimated");
        assert!(
            json.three_things
                .total_hours_label
                .contains("None of these items has a calibrated effort estimate"),
            "{}",
            json.three_things.total_hours_label
        );
    }

    #[test]
    fn three_things_box_total_notes_partial_uncalibrated_items_without_guessing() {
        // Two items DO have effort; a third do_now item does not (a mixed run) — the total
        // must sum the calibrated ones and flag the uncalibrated one by count, not fold a
        // guessed number into the sum.
        let mut f1 = finding("SEC-1", "a.rs", 1, "critical");
        f1.effort = Some("low".to_string());
        let mut f2 = finding("SEC-2", "b.rs", 2, "critical");
        f2.effort = Some("low".to_string());
        let f3 = finding("SEC-3", "c.rs", 3, "critical"); // no effort estimate
        let report = report_with(vec![f1, f2, f3], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.three_things.items.len(), 3);
        assert_eq!(
            json.three_things.total_hours_label,
            "Roughly 4 to 8 hours total (rough estimate); 1 item without an effort estimate yet \
             is not included in this total."
        );
    }

    #[test]
    fn three_things_box_is_empty_when_no_do_now_items_exist() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(json.three_things.items.is_empty());
        assert_eq!(json.three_things.total_hours_label, "No do-now items this run.");
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

    // ── Suppressed-inline -> waived in code ────────────────────────────────────

    #[test]
    fn suppressed_inline_with_no_wire_disposition_is_waived_in_code() {
        // An inline `camerata:allow` waiver must land in Accepted, not Unresolved/open —
        // the bug: an inline-suppressed finding was reported as still-open.
        let mut f = finding("SEC-1", "a.rs", 1, "high");
        f.status = "suppressed-inline".to_string();
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0].disposition,
            "Waived in code (inline camerata:allow)"
        );
        assert_eq!(json.matrix.accepted.len(), 1);
        assert_eq!(json.matrix.do_now.len(), 0);
    }

    #[test]
    fn suppressed_inline_with_explicit_wire_disposition_defers_to_client() {
        let mut f = finding("SEC-1", "a.rs", 1, "high");
        f.status = "suppressed-inline".to_string();
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

    // ── Recommended-Fix: binds to authored `remediation`, NEVER `directive` (Fix 1) ─────

    #[tokio::test]
    async fn resolve_fix_binds_to_the_authored_remediation_and_substitutes_captures() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        f.captures.insert("table".to_string(), "profiles".to_string());
        let fix = resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f)
            .expect("SUPABASE-RLS-ENABLED-1 must have authored remediation");
        assert!(
            fix.contains("`profiles`") || fix.contains("profiles"),
            "expected the captured table name substituted into the authored remediation, got: {fix:?}"
        );
        assert!(
            !fix.to_lowercase().contains("timeline replay") && !fix.contains("emit a critical finding"),
            "must bind to the client-facing `remediation` text, never the detection-recipe \
             `directive` (which mentions the replay mechanism), got: {fix:?}"
        );
    }

    #[test]
    fn resolve_fix_never_falls_back_to_directive_when_remediation_is_unauthored() {
        // Every real corpus rule's default option now has authored `remediation` (the corpus-wide
        // authoring passes closed that gap), so no REAL rule id can demonstrate the "unauthored"
        // path anymore — hardcoding one here would silently start asserting nothing the moment the
        // next rule got authored. Instead build a synthetic single-rule corpus via
        // `ruleset_with_unauthored_rule`: a real, resolvable default option with a real `directive`,
        // but `remediation: None`. This pins the fail-safe permanently, independent of authoring
        // state: absent remediation means an omitted Fix, never the directive text.
        let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
        let f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
        let fix = resolve_fix("SEC-TEST-UNAUTHORED-1", Some(&corpus), &f);
        assert_eq!(
            fix, None,
            "must omit the Fix block, not fall back to the rule's directive, when remediation is unauthored"
        );
    }

    #[test]
    fn resolve_fix_is_none_not_fabricated_when_corpus_is_absent() {
        let f = finding("SEC-NO-UNSAFE-DESERIALIZATION-1", "a.py", 1, "critical");
        assert_eq!(resolve_fix("SEC-NO-UNSAFE-DESERIALIZATION-1", None, &f), None);
    }

    #[tokio::test]
    async fn resolve_fix_is_none_when_rule_id_is_unknown_to_the_corpus() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        assert_eq!(resolve_fix("AI-CUSTOM-ARCH-RULE-1", Some(&corpus), &f), None);
    }

    #[tokio::test]
    async fn curated_finding_site_carries_the_fix_line_populated_from_authored_remediation() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        f.captures.insert("table".to_string(), "profiles".to_string());
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        let fix = json.curated_findings[0].sites[0]
            .fix
            .as_deref()
            .expect("curated site's fix line must be populated from authored remediation");
        assert!(fix.contains("profiles"));
    }

    #[test]
    fn curated_finding_site_fix_is_none_not_fabricated_without_a_corpus() {
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.curated_findings[0].sites[0].fix, None);
    }

    #[test]
    fn curated_finding_site_fix_is_none_when_remediation_is_unauthored() {
        // Same rationale as `resolve_fix_never_falls_back_to_directive_when_remediation_is_unauthored`
        // above: every real corpus rule is now authored, so a synthetic single-rule corpus (via
        // `ruleset_with_unauthored_rule`) is the only way to permanently pin the "unauthored
        // remediation omits the Fix line" fail-safe, independent of corpus authoring state.
        let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
        let f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0].fix, None,
            "an unauthored rule's Fix block must be omitted (None), never backfilled from directive"
        );
    }

    // ── Placeholder substitution helper ─────────────────────────────────────────

    #[test]
    fn instantiate_remediation_fills_known_tokens_from_captures() {
        let mut captures = std::collections::BTreeMap::new();
        captures.insert("table".to_string(), "profiles".to_string());
        captures.insert("bucket".to_string(), "avatars".to_string());
        let text = instantiate_remediation(
            "Enable RLS on `<table>` and lock down the `<bucket>` bucket.",
            "supabase/migrations/1.sql",
            &captures,
        );
        assert_eq!(text, "Enable RLS on `profiles` and lock down the `avatars` bucket.");
    }

    #[test]
    fn instantiate_remediation_fills_path_and_file_tokens_from_the_finding_path() {
        let captures = std::collections::BTreeMap::new();
        let text = instantiate_remediation(
            "Fix the issue in <path> (also see <file>).",
            "apps/web/lib/analytics.ts",
            &captures,
        );
        assert_eq!(
            text,
            "Fix the issue in apps/web/lib/analytics.ts (also see apps/web/lib/analytics.ts)."
        );
    }

    #[test]
    fn instantiate_remediation_falls_back_to_a_readable_generic_for_unfilled_known_tokens() {
        let captures = std::collections::BTreeMap::new();
        let text = instantiate_remediation(
            "Re-enable verify_jwt for <function-name>.",
            "supabase/config.toml",
            &captures,
        );
        assert_eq!(text, "Re-enable verify_jwt for the affected function.");
    }

    #[test]
    fn instantiate_remediation_never_emits_a_raw_token_for_an_unknown_placeholder() {
        let captures = std::collections::BTreeMap::new();
        let text = instantiate_remediation(
            "Check <some-totally-unmapped-token> before shipping.",
            "a.rs",
            &captures,
        );
        assert!(!text.contains('<') && !text.contains('>'), "raw token escaped: {text:?}");
        assert_eq!(text, "Check the affected resource before shipping.");
    }

    #[test]
    fn instantiate_remediation_prefers_a_capture_over_the_generic_even_when_both_available() {
        let mut captures = std::collections::BTreeMap::new();
        captures.insert("table".to_string(), "orders".to_string());
        let text = instantiate_remediation("Review <table> now.", "a.sql", &captures);
        assert_eq!(text, "Review orders now.");
    }

    // ── Item 3: what's-healthy / curated-finding citations are EXTERNAL authorities only ──

    #[tokio::test]
    async fn citation_join_drops_internal_enforcement_md_citation_and_keeps_external_ones() {
        // SEC-NO-UNSAFE-DESERIALIZATION-1's corpus entry carries an internal
        // `docs/ENFORCEMENT.md` source (Camerata's own scaffolding, meaningless in an
        // arbitrary client repo) ALONGSIDE two real external OWASP sources. The report must
        // cite the OWASP sources and drop the internal one entirely, not just visually
        // de-emphasize it.
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let citation = resolve_citation("SEC-NO-UNSAFE-DESERIALIZATION-1", None, Some(&corpus));
        assert_eq!(citation.kind, "grounded");
        assert!(
            !citation.sources.iter().any(|s| !is_external_source_url(&s.url)),
            "an internal (non http/https) source leaked into the report: {:?}",
            citation.sources
        );
        assert!(
            !citation.label.contains("ENFORCEMENT.md") && !citation.label.contains("RULE_REGISTRY"),
            "the citation label must not mention Camerata's own internal scaffolding docs: {}",
            citation.label
        );
        assert!(
            citation.sources.iter().any(|s| s.url.contains("owasp.org")),
            "the real external OWASP sources must still be cited: {:?}",
            citation.sources
        );
    }

    #[test]
    fn is_external_source_url_accepts_only_real_http_urls() {
        assert!(is_external_source_url("https://owasp.org/foo"));
        assert!(is_external_source_url("http://example.com"));
        assert!(!is_external_source_url("docs/ENFORCEMENT.md"));
        assert!(!is_external_source_url(""));
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

    /// Item 6 regression guard: the category scorecard renders as a compact heat-grid (color
    /// intensity keyed to severity counts, numbers kept), the ONE visual addition the owner
    /// approved — no funnels/bar charts/gauges/scatter plots. Pins the template helper names
    /// so a future edit can't silently drop the heat-grid back to plain numeric cells (or add
    /// an unapproved chart type) without this test flagging it.
    #[test]
    fn shipped_template_renders_the_scorecard_as_a_heat_grid_and_adds_no_other_chart_types() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("heat_bg") && template.contains("heat_cell"),
            "the scorecard's heat-grid coloring helpers are missing from the shipped template"
        );
        for banned in ["chart(", "bar-chart", "gauge(", "funnel", "scatter"] {
            assert!(
                !template.contains(banned),
                "found a banned decorative chart primitive ({banned:?}) — the owner's ruling \
                 is the scorecard heat-grid and the severity x effort matrix are the ONLY \
                 visuals; anything else reads as marketing."
            );
        }
    }

    // ── Item 2 (Bug 4): informational bucketing predicate ─────────────────────────
    //
    // `is_informational` is the single gate that diverts a low-signal OPEN finding out of the
    // do_now/do_next/plan action tiers into the visible-but-advisory `informational` appendix.
    // These pin the four independent signals AND the two hard invariants (never critical/high,
    // never a dispositioned finding) that keep real defects in the action tiers.

    /// The `info` severity tier is informational by definition — nothing below `low` is an
    /// action item (e.g. the unexposed-schema RLS note from I3).
    #[test]
    fn info_severity_is_always_informational() {
        let f = finding("SOME-RULE-1", "a.rs", 1, "info");
        assert!(is_informational(
            &f,
            Disposition::Unresolved,
            "info",
            None,
            0
        ));
    }

    /// The load-bearing invariant: a critical or high finding is NEVER auto-demoted to the
    /// appendix, no matter which other signals fire. A buyer must see every real defect in the
    /// action tiers.
    #[test]
    fn critical_and_high_are_never_informational() {
        // Even with every other informational signal set (testing-style, needs-review,
        // not-located), a high/critical stays an action item.
        let mut f = finding("ARCH-SERVICE-DI-1", "a.rs", 1, "high");
        f.category = Some("testing-style".to_string());
        f.confidence = Some("needs-review".to_string());
        f.located = false;
        assert!(
            !is_informational(&f, Disposition::Unresolved, "high", None, 0),
            "a high finding must never be auto-informational"
        );
        assert!(
            !is_informational(&f, Disposition::Unresolved, "critical", None, 0),
            "a critical finding must never be auto-informational"
        );
    }

    /// §2d — a `testing-style` note is informational only when the repo has too few test files
    /// to have a house style to deviate FROM. At/above the corpus threshold it's a real note.
    #[test]
    fn testing_style_is_gated_on_test_corpus_size() {
        let mut f = finding("STYLE-TEST-1", "a.rs", 1, "low");
        f.category = Some("testing-style".to_string());
        // Below the threshold: no corpus to deviate from → informational.
        assert!(is_informational(
            &f,
            Disposition::Unresolved,
            "low",
            None,
            MIN_STYLE_CORPUS_FILES - 1
        ));
        // At/above the threshold: a genuine style deviation → stays an action item.
        assert!(!is_informational(
            &f,
            Disposition::Unresolved,
            "low",
            None,
            MIN_STYLE_CORPUS_FILES
        ));
    }

    /// §2c — a low/medium finding the calibrator itself flagged `needs-review` (debatable /
    /// theoretical / under-evidenced) is advisory, not an action item.
    #[test]
    fn needs_review_confidence_is_informational_at_low_severity() {
        let mut f = finding("SOME-RULE-1", "a.rs", 1, "medium");
        f.confidence = Some("needs-review".to_string());
        assert!(is_informational(
            &f,
            Disposition::Unresolved,
            "medium",
            None,
            0
        ));
        // But a high-confidence low finding is a normal (if minor) action item.
        f.confidence = Some("high".to_string());
        assert!(!is_informational(
            &f,
            Disposition::Unresolved,
            "medium",
            None,
            0
        ));
    }

    /// A finding the auditor has EXPLICITLY dispositioned (accepted / tech-debt / FP) keeps its
    /// own destination — the predicate only ever re-buckets an OPEN (`Unresolved`) row.
    #[test]
    fn dispositioned_findings_are_never_re_bucketed() {
        let f = finding("SOME-RULE-1", "a.rs", 1, "info");
        // Even an `info`-severity finding, which is informational when open, is left alone once
        // an auditor has ruled on it.
        assert!(!is_informational(
            &f,
            Disposition::BaselineAccepted,
            "info",
            None,
            0
        ));
        assert!(!is_informational(
            &f,
            Disposition::TechDebtLater,
            "info",
            None,
            0
        ));
    }

    /// §2a — an ABSENCE-type (`located == false`) `structured` stance rule in the universal /
    /// framework layer is a "the project hasn't adopted X" convention, not a concrete defect.
    /// Uses the REAL corpus (the predicate reads the rule's enforcement + domain), and the real
    /// stance rule `ARCH-SERVICE-DI-1` (domain `api-layer`, enforcement `structured`).
    #[tokio::test]
    async fn absence_type_structured_stance_rule_is_informational() {
        let corpus = camerata_rules::load_corpus(&camerata_rules::corpus_path())
            .await
            .expect("bundled corpus must load");
        // Sanity: the rule exists and is the shape §2a keys on.
        let rule = corpus
            .get_by_id("ARCH-SERVICE-DI-1")
            .expect("ARCH-SERVICE-DI-1 must exist in the bundled corpus");
        assert_eq!(rule.enforcement, camerata_rules::EnforcementKind::Structured);
        assert_eq!(rule.domain, "api-layer");

        let mut f = finding("ARCH-SERVICE-DI-1", "src/svc.rs", 1, "medium");
        // Absence-type: the "snippet" is a description, not code located in the file.
        f.located = false;
        assert!(
            is_informational(&f, Disposition::Unresolved, "medium", Some(&corpus), 100),
            "an absence-type structured stance note is informational"
        );

        // A PRESENCE-type violation of the same rule (real code located) IS a defect → action.
        f.located = true;
        assert!(
            !is_informational(&f, Disposition::Unresolved, "medium", Some(&corpus), 100),
            "a located (presence-type) violation of the stance rule stays an action item"
        );

        // And a high-severity instance is never demoted, absence-type or not.
        f.located = false;
        assert!(
            !is_informational(&f, Disposition::Unresolved, "high", Some(&corpus), 100),
            "severity still overrides the stance-layer signal"
        );
    }

    /// End-to-end at the `build_report_json` level: an `info`-severity finding lands in the
    /// `informational` appendix, NEVER in an action tier, and the curated four-bucket invariant
    /// `do_now + do_next + plan + accepted == curated_total` still holds exactly (informational
    /// is excluded from `curated_total`).
    #[test]
    fn build_report_json_routes_info_to_appendix_and_preserves_the_invariant() {
        let action = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 10, "critical");
        let advisory = finding("SOME-RULE-1", "b.rs", 20, "info");
        let report = report_with(
            vec![action, advisory],
            vec!["SEC-NO-HARDCODED-SECRETS-1", "SOME-RULE-1"],
        );
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.matrix.informational.len(), 1, "the info finding is in the appendix");
        assert_eq!(json.matrix.informational[0].path, "b.rs");
        assert_eq!(json.matrix.do_now.len(), 1, "the critical is an action item");

        // No info-severity finding may appear in any action tier.
        for tier in [&json.matrix.do_now, &json.matrix.do_next, &json.matrix.plan] {
            assert!(
                tier.iter().all(|r| r.severity != "info"),
                "no info-severity finding may land in an action tier"
            );
        }

        // The self-checking curated partition still balances, appendix excluded.
        let curated = json.executive_summary.curated_total;
        let summed = json.matrix.do_now.len()
            + json.matrix.do_next.len()
            + json.matrix.plan.len()
            + json.matrix.accepted.len();
        assert_eq!(summed, curated, "do_now+do_next+plan+accepted must equal curated_total");
        assert_eq!(curated, 1, "only the critical is curated; the info note is appendix-only");
    }

    // ── Branding (2026-09-13 review): precedence + neutral fallback ────────────────

    #[test]
    fn resolve_brand_prefers_the_per_report_field_over_env() {
        let mut opts = empty_opts();
        opts.brand = "Cantus Works".to_string();
        assert_eq!(
            resolve_brand(&opts, Some("Some Other Agency")),
            Some("Cantus Works".to_string())
        );
    }

    #[test]
    fn resolve_brand_falls_back_to_env_when_the_report_field_is_blank() {
        let opts = empty_opts();
        assert_eq!(
            resolve_brand(&opts, Some("Cantus Works")),
            Some("Cantus Works".to_string())
        );
        // Whitespace-only counts as "not supplied" too.
        let mut whitespace_opts = empty_opts();
        whitespace_opts.brand = "   ".to_string();
        assert_eq!(
            resolve_brand(&whitespace_opts, Some("Cantus Works")),
            Some("Cantus Works".to_string())
        );
    }

    #[test]
    fn resolve_brand_is_none_when_neither_the_field_nor_env_is_set() {
        let opts = empty_opts();
        assert_eq!(resolve_brand(&opts, None), None);
        assert_eq!(resolve_brand(&opts, Some("")), None);
        assert_eq!(resolve_brand(&opts, Some("   ")), None);
    }

    #[test]
    fn resolve_prepared_by_follows_the_same_precedence() {
        let mut opts = empty_opts();
        opts.prepared_by = "Zachary Ernst".to_string();
        assert_eq!(
            resolve_prepared_by(&opts, Some("Someone Else")),
            "Zachary Ernst"
        );
        let empty = empty_opts();
        assert_eq!(resolve_prepared_by(&empty, Some("Someone Else")), "Someone Else");
        assert_eq!(resolve_prepared_by(&empty, None), "");
    }

    #[test]
    fn cover_brand_is_some_when_report_options_brand_is_set() {
        let f = finding("SEC-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let mut opts = empty_opts();
        opts.brand = "Cantus Works".to_string();
        let json = build_report_json(&report, &HashMap::new(), None, &opts);
        assert_eq!(json.cover.brand, Some("Cantus Works".to_string()));
    }

    #[test]
    fn cover_brand_is_none_with_no_brand_option_set_the_neutral_fallback_case() {
        // `build_report_json` never reads the `CAMERATA_REPORT_BRAND` env var itself (see
        // `ReportOptions::apply_env_defaults`'s doc comment) — with a blank `opts.brand` (the
        // state an un-mutated `ReportOptions` is always in), `cover.brand` must be `None`, the
        // exact case the template renders as `NEUTRAL_COVER_TITLE` with no agency name and no
        // "Camerata" on the cover.
        let f = finding("SEC-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.cover.brand, None);
    }

    #[test]
    fn neutral_cover_title_constant_matches_the_documented_fallback_string() {
        assert_eq!(NEUTRAL_COVER_TITLE, "Codebase Audit Report");
    }

    #[test]
    fn apply_env_defaults_fills_blank_fields_only() {
        // Real env mutation, scoped to this one test and cleaned up immediately after — every
        // other precedence case above goes through the pure `resolve_brand`/`resolve_prepared_by`
        // helpers instead specifically to avoid relying on process-global env state.
        // SAFETY-ish note: `cargo test` runs this crate's tests in threads within one process;
        // if this ever becomes flaky under parallel execution, these var names are unique
        // enough (`CAMERATA_REPORT_BRAND`/`_PREPARED_BY`) that no other test should collide.
        std::env::set_var(REPORT_BRAND_ENV, "Env Agency");
        std::env::set_var(REPORT_PREPARED_BY_ENV, "Env Preparer");

        let mut blank = ReportOptions::default();
        blank.apply_env_defaults();
        assert_eq!(blank.brand, "Env Agency");
        assert_eq!(blank.prepared_by, "Env Preparer");

        let mut already_set = ReportOptions {
            brand: "Cantus Works".to_string(),
            prepared_by: "Zachary Ernst".to_string(),
            ..Default::default()
        };
        already_set.apply_env_defaults();
        assert_eq!(already_set.brand, "Cantus Works");
        assert_eq!(already_set.prepared_by, "Zachary Ernst");

        std::env::remove_var(REPORT_BRAND_ENV);
        std::env::remove_var(REPORT_PREPARED_BY_ENV);
    }

    // ── FIX 6: the real severity x effort priority grid ─────────────────────────

    #[test]
    fn priority_grid_places_findings_in_the_right_severity_by_effort_cell() {
        let mut critical_low = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        critical_low.effort = Some("low".to_string());
        let mut high_medium = finding("SUPABASE-AUTH-EDGE-JWT-1", "b.toml", 2, "high");
        high_medium.effort = Some("medium".to_string());
        let mut high_uncalibrated = finding("ARCH-1", "c.rs", 3, "high");
        high_uncalibrated.effort = None; // deterministic-floor: never calibrated
        let mut low_sev = finding("STYLE-1", "d.rs", 4, "low");
        low_sev.effort = Some("low".to_string());

        let report = report_with(
            vec![critical_low, high_medium, high_uncalibrated, low_sev],
            vec![],
        );
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let grid = &json.priority_grid;

        // Row/column presence: critical, high, low severities all appear (all land in an
        // action tier); low/medium/unscoped effort columns all appear.
        assert!(grid.rows.iter().any(|r| r.severity == "critical"));
        assert!(grid.rows.iter().any(|r| r.severity == "high"));
        assert!(grid.rows.iter().any(|r| r.severity == "low"));
        assert!(grid.columns.contains(&"low".to_string()));
        assert!(grid.columns.contains(&"medium".to_string()));
        assert!(grid.columns.contains(&"unscoped".to_string()));

        let cell = |sev: &str, eff: &str| -> &GridCellJson {
            let row = grid.rows.iter().find(|r| r.severity == sev).unwrap();
            let col_idx = grid.columns.iter().position(|c| c == eff).unwrap();
            &row.cells[col_idx]
        };

        assert_eq!(cell("critical", "low").findings.len(), 1);
        assert_eq!(cell("critical", "low").findings[0].rule_id, "SUPABASE-RLS-ENABLED-1");
        // `high_medium` is do_next (high severity, medium effort is not "low"); still an
        // ACTION-tier finding, so it must still land in the grid.
        assert_eq!(cell("high", "medium").findings.len(), 1);
        assert_eq!(cell("high", "medium").findings[0].rule_id, "SUPABASE-AUTH-EDGE-JWT-1");
        // An uncalibrated effort normalizes to the "unscoped" column, never dropped.
        assert_eq!(cell("high", "unscoped").findings.len(), 1);
        assert_eq!(cell("high", "unscoped").findings[0].rule_id, "ARCH-1");
        assert_eq!(cell("low", "low").findings.len(), 1);
        assert_eq!(cell("low", "low").findings[0].rule_id, "STYLE-1");
    }

    #[test]
    fn priority_grid_excludes_accepted_and_informational_but_counts_them() {
        let mut accepted = finding("SEC-1", "a.rs", 1, "high");
        accepted.effort = Some("low".to_string());
        let info = finding("SOME-RULE-1", "b.rs", 2, "info");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&accepted), wire("Ignored", "accepted for now", ""));
        let report = report_with(vec![accepted, info], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        // Neither finding appears in any grid cell.
        for row in &json.priority_grid.rows {
            for cell in &row.cells {
                assert!(
                    cell.findings.iter().all(|f| f.rule_id != "SEC-1" && f.rule_id != "SOME-RULE-1"),
                    "accepted/informational findings must never appear in the priority grid"
                );
            }
        }
        assert_eq!(json.priority_grid.accepted_count, 1);
        assert_eq!(json.priority_grid.informational_count, 1);
    }

    #[test]
    fn priority_grid_is_empty_when_no_action_tier_findings_exist() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(json.priority_grid.rows.is_empty());
        assert!(json.priority_grid.columns.is_empty());
    }

    // ── FIX 7: business-impact map ───────────────────────────────────────────────

    #[test]
    fn business_impact_is_authored_for_the_sample_fixture_do_now_rules() {
        for rule_id in [
            "SUPABASE-RLS-ENABLED-1",
            "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
            "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
        ] {
            assert!(
                business_impact_for_rule(rule_id).is_some(),
                "{rule_id} should have an authored business-impact sentence"
            );
        }
    }

    #[test]
    fn business_impact_is_none_for_an_unauthored_rule_never_fabricated() {
        assert_eq!(business_impact_for_rule("SOME-RULE-NEVER-AUTHORED-1"), None);
    }

    #[test]
    fn three_things_items_carry_the_authored_impact_when_present() {
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        f.effort = Some("low".to_string());
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.three_things.items[0].impact,
            business_impact_for_rule("SUPABASE-RLS-ENABLED-1").map(str::to_string)
        );
    }

    #[test]
    fn three_things_items_impact_is_none_for_an_unauthored_rule() {
        let f = finding("ARCH-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.three_things.items[0].impact, None);
    }

    // ── FIX 3: Next steps ─────────────────────────────────────────────────────────

    #[test]
    fn methodology_next_steps_is_the_authored_constant() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.methodology.next_steps, NEXT_STEPS_NOTE);
        assert!(!json.methodology.next_steps.is_empty());
    }

    // ── Template regressions (2026-09-13 review) ────────────────────────────────

    /// FIX 1 rendering bug this pass fixes: `CuratedSiteJson.fix`/`fix_for_this_finding` are
    /// `Option<String>`, which serialize to JSON `null` and parse in Typst as `none` — NOT an
    /// empty string. A `!= ""` guard would let `none` fall into the render branch and print a
    /// bare "Fix:" label with nothing after it. Pins the correct `!= none` guard in the shipped
    /// template so this can't regress silently.
    #[test]
    fn shipped_template_gates_the_fix_block_on_none_not_empty_string() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("if site.fix != none"),
            "the Fix block must be gated on `!= none` (Option<String> -> JSON null -> Typst \
             none), not `!= \"\"` — see report_export::CuratedSiteJson::fix's doc comment"
        );
        assert!(
            !template.contains("if site.fix != \"\""),
            "the old `!= \"\"` gate must not come back"
        );
        assert!(
            template.contains("site.fix_for_this_finding != none"),
            "the template must be wired to render `fix_for_this_finding` under the Fix block \
             once it's ever populated"
        );
    }

    /// FIX 4 regression: the exec summary's own "Top priority items" bullet list (and its
    /// `top_do_now` feed) must not come back — the "Three things this week" box is the one
    /// place page 3 names the top do-now findings.
    #[test]
    fn shipped_template_has_no_top_priority_items_bullet_list() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(!template.contains("Top priority items"));
        assert!(!template.contains("executive_summary.top_do_now"));
    }

    /// FIX 5 regression: the scorecard's "checked/clean" column must read as words, not a bare
    /// numeric fraction that reads like a grade.
    #[test]
    fn shipped_template_scorecard_spells_out_checked_and_clean() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(template.contains("Rules checked"));
        assert!(template.contains("Rules clean"));
        assert!(
            !template.contains("row.audited_rules)/#str(row.clean_rules"),
            "the scorecard must not render a bare N/M fraction"
        );
    }

    /// FIX 6 regression: the severity x effort section must render the real 2-D
    /// `d.priority_grid` table, not the old 4-box bucket list keyed off `d.matrix`.
    #[test]
    fn shipped_template_renders_the_priority_grid_not_the_old_bucket_list() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(template.contains("d.priority_grid"));
        assert!(
            !template.contains("d.matrix.do_now") && !template.contains("d.matrix.accepted"),
            "the severity x effort section must read from d.priority_grid, not d.matrix directly"
        );
    }

    /// Branding regression: no hardcoded agency/"Camerata" cover title, no "Brownfield".
    #[test]
    fn shipped_template_has_no_hardcoded_cover_title() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(!template.contains("Camerata Brownfield Audit Report"));
        assert!(!template.contains("Brownfield"));
        assert!(template.contains("brand_title"));
        assert!(template.contains(NEUTRAL_COVER_TITLE));
    }

    /// Branding regression: Audit model / Calibration model must not render on the cover table
    /// anymore (they moved into Methodology, alongside the Camerata version line).
    #[test]
    fn shipped_template_cover_table_omits_audit_and_calibration_model() {
        let template = include_str!("../templates/audit_report.typ");
        let cover_start = template.find("── 1. Cover").expect("cover section marker");
        let cover_end = template.find("── ToC").expect("ToC section marker");
        let cover_section = &template[cover_start..cover_end];
        assert!(!cover_section.contains("Audit model"));
        assert!(!cover_section.contains("Calibration model"));
        assert!(
            template.contains("Scanned with Camerata v"),
            "the model/version provenance line must still render, just in Methodology"
        );
    }
}
