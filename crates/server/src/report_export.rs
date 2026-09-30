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
    /// Every rule's PROJECT-level chosen alternative option (uppercased rule id -> option
    /// id) — the `resolve_fix` fallback source for a finding with no `evaluated_option_id`
    /// (see its doc comment). NEVER sent by the export dialog client-side (the client has no
    /// business knowing this); the export handlers populate it server-side from the project's
    /// `ruleset` before calling `build_report_json`/`build_workbook`/`build_findings_export`.
    /// `#[serde(default)]` so a client that omits it (every real client) degrades to an empty
    /// map, i.e. `resolve_fix` falls straight through to the corpus default — today's exact
    /// behavior for any project this feature hasn't touched.
    #[serde(default)]
    pub chosen_options: std::collections::HashMap<String, String>,
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
///
/// P6 (2026-09-29): the product is "Codebase Inspection" — client-facing copy says
/// "inspection"/"Inspection", never "audit"/"Audit" (internal code identifiers, rule ids, and
/// doc-comment cross-references are exempt; see the product-hardening plan's P6 section).
pub const NEUTRAL_COVER_TITLE: &str = "Codebase Inspection Report";

/// The standing default for [`ReportOptions::prepared_by`] / [`REPORT_PREPARED_BY_ENV`] when
/// NEITHER resolves to a non-blank value — P6 (2026-09-29): the cover's "Prepared by" row must
/// NEVER render "N/A" (promise 1: "one person reads every finding and signs the report"), so
/// the ultimate fallback is a real name, not an empty string. A per-report `prepared_by` or the
/// env var still wins over this (see [`resolve_prepared_by`]'s precedence).
pub const DEFAULT_PREPARED_BY: &str = "Zachary Ernst, Cantus Works";

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
/// `CAMERATA_REPORT_PREPARED_BY` env var > [`DEFAULT_PREPARED_BY`]. P6 (2026-09-29): the last
/// step used to be an empty string, which the cover table's `or_na` rendered as "Prepared by
/// N/A" — a report promising "one person reads every finding and signs the report" must never
/// say nobody prepared it, so the floor is a real name now, never blank.
pub(crate) fn resolve_prepared_by(opts: &ReportOptions, env_prepared_by: Option<&str>) -> String {
    resolve_with_env_default(&opts.prepared_by, env_prepared_by)
        .unwrap_or_else(|| DEFAULT_PREPARED_BY.to_string())
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

/// P5: whether this export reflects an auditor's actual THIS-SESSION triage pass over the
/// findings, or is a raw, no-human-touch snapshot straight off the engine. Derived, never
/// client-supplied — see `build_report_json`'s partition loop, which flips this to `Reviewed`
/// the moment any finding carries an explicit `Ignored` / `TechDebt*` / `FalsePositive` wire
/// disposition. A pre-existing `BaselineAccepted` or `WaivedInline` disposition (both sourced
/// from `Finding.status`, a PRIOR/code-level fact rather than this session's human judgment —
/// see `classify`'s doc comment) does NOT flip this to `Reviewed`: those findings were never
/// looked at by anyone THIS run.
///
/// Gates two things:
/// - The narrative voice (`default_narrative`): `Raw` describes what the ENGINE did and never
///   claims a human reviewed or dispositioned anything; `Reviewed` describes the reviewer's
///   actual dispositions.
/// - The PDF's per-page "DRAFT: not yet reviewed" banner (`Raw` only) — see the Typst
///   template's `d.review_state` read.
///
/// See `docs/plans/2026-09-29_codebase-inspection-hardening.md`'s P5 section: the bug this
/// fixes is a raw (nobody-reviewed) export whose narrative said "N were reviewed... 0 were
/// dispositioned as false positives by the auditor" — a flat fabrication, since reviewing
/// something is precisely what had not happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// No finding in this scan carries an explicit THIS-SESSION auditor disposition.
    Raw,
    /// At least one finding carries an explicit THIS-SESSION auditor disposition
    /// (`Ignored` / `TechDebt{Now,Later}` / `FalsePositive`).
    Reviewed,
}

impl ReviewState {
    /// Whether the PDF's draft banner must render on every page.
    pub fn is_draft(self) -> bool {
        matches!(self, ReviewState::Raw)
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
/// A real inspection often ships before any client conversation has happened (e.g. a
/// pre-engagement scan of an OSS repo, or the very first draft of a fresh engagement) — there
/// is no "team" to have confirmed anything. `confirmed_by_client` (from `DispositionWire`,
/// default `false`) is the ONLY thing that unlocks "Accepted risk: {reason}" wording for an
/// `Ignored` finding; when it is `false` (the safe default), the reader sees "Needs client
/// confirmation" instead, no matter how confident the reviewer's own `reason` prose reads — the
/// reviewer's proposed rationale is still surfaced, but honestly attributed to the reviewer,
/// never invented as a client's words. This is a deliberate rendering constraint, not just a
/// fixture-content fix: the serializer itself cannot produce "confirmed" language without that
/// explicit flag.
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
                format!("Needs client confirmation (reviewer's proposed rationale: {reason})")
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

/// P6 (2026-09-29): the cover's real "code volume" figure — non-blank source lines (never
/// characters; a raw char count reads as noise to a non-engineer reader) broken out by
/// language, straight from [`crate::onboard::ScanReport::code_lines`]/`code_lines_by_language`.
/// `CoverJson::code_volume` is `None` (never `Some` with a zero `lines`) whenever
/// `code_lines == 0` — see [`build_report_json`]'s construction of it — so the template can
/// gate the whole row on presence rather than ever rendering "0 characters"/"0 lines".
#[derive(Debug, Clone, Serialize)]
pub struct CodeVolumeJson {
    pub lines: usize,
    /// Sorted by line count descending (see `onboard::finalize_language_breakdown`). May be
    /// empty even when `lines > 0` (every scanned file had an unrecognized extension) — the
    /// template must render the total either way and only additionally list languages when
    /// this is non-empty.
    pub by_language: Vec<crate::onboard::LanguageVolume>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverJson {
    pub repos: Vec<String>,
    pub files_scanned: usize,
    pub files_excluded: usize,
    pub code_chars: usize,
    /// See [`CodeVolumeJson`]'s doc comment: `None` whenever no real code volume is known
    /// (0 lines), so the template hides the "Code volume" row entirely instead of rendering a
    /// zero. Never render `d.cover.code_chars` directly in the template for this reason — it
    /// carries the identical "zero means hide, not print 0" problem this field was added to fix.
    pub code_volume: Option<CodeVolumeJson>,
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
    /// P5: findings held out of the curated action tiers for a human reviewer's judgment call
    /// (`matrix.informational.len()`) — the previously-invisible "other 23" in the reconciling
    /// count `candidates_reviewed == curated_total + held_for_review + excluded_false_positive
    /// + dependency_advisories`. Always shown in the narrative, in BOTH review states.
    pub held_for_review: usize,
    /// P5: `DEP_AUDIT_RULE_ID` findings that survived the false-positive filter — carved into
    /// their own `dependency_snapshot` table, but still a real piece of `candidates_reviewed`'s
    /// reconciliation (see `held_for_review`'s doc comment).
    pub dependency_advisories: usize,
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
    /// **P2 (2026-09-29): the PRIMARY fix.** A codebase-specific remediation for THIS
    /// finding — names the real file/symbol/column from the finding's own evidence, and
    /// points at the repo's own correct pattern elsewhere when one exists. Generated at scan
    /// time by [`crate::ai_audit::generate_fix_specifics`] (this layer stays pure/
    /// synchronous — it only reads `Finding::fix_specific`, never calls a model) with a
    /// bounded validate-and-regenerate self-check (identifier grounding + non-contradiction;
    /// see that function's doc comment). The template renders this ABOVE `fix` — the rule's
    /// generic authored remediation above is now SECONDARY context only, never the lead line.
    /// `None` when fix-generation never ran (a deterministic-only scan makes no model calls
    /// at all) or gave up after retries (`fix_generation_failed`, which is also what keeps
    /// such a finding out of `do_now`) — the template must render `fix` alone in that case,
    /// never a bare "Fix:" label with nothing after it.
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
    /// P5: mirrors `ExecutiveSummaryJson::held_for_review` — carried here too so the
    /// Methodology section's own reconciling line (§`typ` "N candidate findings...") can show
    /// it without the template reaching back into `executive_summary`.
    pub held_for_review: usize,
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
    /// P5: `"raw"` (nobody has reviewed anything this session) or `"reviewed"` (at least one
    /// finding carries an explicit auditor disposition). Read directly by the Typst template
    /// (`d.review_state`) to gate the per-page draft banner and the Methodology section's
    /// review-claim wording — see [`ReviewState`]'s doc comment.
    pub review_state: ReviewState,
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
     the inspected code at scan time and may not represent the live production system. All \
     remediation should be validated by the client's engineering team against the live \
     environment. The preparing reviewer accepts no liability for actions taken on the basis \
     of this report.";

/// FIX 3 (2026-09-13 review) — a short, FACTUAL "what happens next" paragraph, rendered in a
/// new section before Methodology. Deliberately non-pitch: no urgency language, no claim about
/// price beyond "the rate set out in the engagement" (a specific number lives in the
/// engagement paperwork, never fabricated here). Three factual steps, one paragraph, no em/en
/// dashes (house style for this client deliverable — see `AUDIT_REPORT_DISCLAIMER`'s doc
/// comment).
pub const NEXT_STEPS_NOTE: &str =
    "There are three steps from here. First, the do-now items above get fixed, by your own \
     team, an outside contractor, or the reviewing engineer, at the rate set out in the \
     engagement. Second, a retest: the reviewing engineer re-scans the repository once the \
     fixes are in and signs a short addendum confirming each item is closed. Third, ongoing \
     coverage: a monthly delta rescan plus on-call architect availability for anything new the \
     codebase introduces between engagements.";

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

// ── P3 (2026-09-29): the citation gate ──────────────────────────────────────────────
//
// Several of the report's MOST important findings — a cross-tenant RLS policy, RLS
// disabled on a payments table, a stored-XSS sink — are produced by the AI tier, which
// invents its own rule id (an `AI-`-prefixed id; see `is_ai_tier`) the corpus never saw.
// `resolve_citation` is honest about that: it labels such a finding "AI-advisory,
// model-inferred." rather than dressing it up. But the product promises every CURATED
// finding cites the published standard it breaks (the hardening plan's bar item 3) — a
// citation-less finding sitting in the curated set falsifies that promise, for exactly
// the findings a client is most likely to act on first.
//
// The fix is NOT to invent a bespoke Rust-side citation per finding (that just moves the
// "trust me" problem into this file instead of a TOML). Instead: classify the AI finding
// into one of a small, closed set of well-known defect CLASSES by its rule id tokens +
// `category` + detail-text keywords (never a fixture-specific string — see
// `classify_ai_finding`'s doc comment), then reuse an EXISTING grounded corpus rule's own
// citation for that class. Where a class had no corpus rule yet, one was added under
// `crates/rules/principles/universal/sec-no-*-1.toml` rather than inventing a citation in
// Rust — grounding always means a published standard or a real linter rule.
//
// A finding that still resolves to "advisory" after this fallback (its class has no
// mapping, or it does not classify into any known class at all) is the CURATION GATE's
// job: `is_uncited_ai_finding` reports it as uncited, and `build_report_json` routes it
// to the informational/held-for-review bucket and EXCLUDES it from `curated_findings`
// entirely. It is never silently dropped (over-tell over under-tell) — it still surfaces
// as a `FindingRefJson` with an honest "needs review (uncited)" headline, just outside the
// curated action tiers.

/// Whether `finding` came from the AI tier (the model-inferred prose/deep-audit pass) as
/// opposed to the deterministic floor or a scan-time preview tool. Mirrors the signal
/// `ai_audit::finding_origin` already uses to distinguish AI output (an `AI-`-prefixed
/// invented rule id, or a calibration-set `confidence`) without reaching into that
/// module's private `Origin` enum — this file only needs the yes/no answer. Deliberately
/// gates the P3 citation fallback/exclusion to ONLY AI-tier findings: a deterministic
/// floor rule that happens to lack a corpus citation (a separate, pre-existing gap — e.g.
/// `SEC-NO-RAW-SQL-CONCAT-1`, which has no TOML entry at all) is untouched by this gate.
pub(crate) fn is_ai_tier(finding: &Finding) -> bool {
    finding.rule_id.starts_with("AI-") || finding.confidence.is_some()
}

/// The closed set of defect classes an otherwise-uncited AI-tier finding is checked
/// against before being allowed into the curated set (P3). Each variant names a corpus
/// rule ([`AiFindingClass::grounding_rule_id`]) whose `[[sources]]` are a real published
/// standard or linter — the class NEVER carries its own bespoke citation text, so there is
/// exactly one place (`crates/rules/principles/**`) an auditor edits to correct or extend
/// a citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AiFindingClass {
    /// Row Level Security disabled, missing, or permissive on an exposed table (a
    /// cross-tenant read, RLS disabled on a payments table, an always-true policy, …).
    Rls,
    /// Stored or reflected cross-site scripting via a raw-HTML sink
    /// (`dangerouslySetInnerHTML`, `innerHTML`, `document.write`, `v-html`, …).
    Xss,
    /// A redirect target derived from user input with no allowlist check.
    OpenRedirect,
    /// A security token/nonce/session id generated from a general-purpose PRNG instead of
    /// a cryptographically secure source.
    WeakTokenRandomness,
    /// User-controlled data concatenated into a query/filter GRAMMAR (PostgREST
    /// `.or()`/`.filter()`, a NoSQL operator string, an LDAP filter) rather than passed as
    /// a bound argument.
    QueryGrammarInjection,
    /// A CORS policy pairing a wildcard or reflected `Origin` with
    /// `Access-Control-Allow-Credentials: true`.
    PermissiveCors,
}

impl AiFindingClass {
    /// The corpus rule id whose `sources` ground this class. Reused verbatim via
    /// `resolve_citation` — never re-derived here — so correcting or extending a citation
    /// is a one-file TOML change, not a second Rust-side copy to keep in sync.
    fn grounding_rule_id(self) -> &'static str {
        match self {
            AiFindingClass::Rls => "SUPABASE-RLS-ENABLED-1",
            AiFindingClass::Xss => "SEC-NO-UNSAFE-HTML-SINK-1",
            AiFindingClass::OpenRedirect => "SEC-NO-OPEN-REDIRECT-1",
            AiFindingClass::WeakTokenRandomness => "SEC-NO-WEAK-TOKEN-RANDOMNESS-1",
            AiFindingClass::QueryGrammarInjection => "SEC-NO-QUERY-GRAMMAR-INJECTION-1",
            AiFindingClass::PermissiveCors => "SEC-NO-PERMISSIVE-CORS-CREDENTIALS-1",
        }
    }
}

/// Classify an AI-tier finding into the P3 closed taxonomy, by RULE-ID TOKENS + the
/// existing semantic `category` field + DETAIL-TEXT keywords — never a fixture-specific
/// rule id or file/table name (the AI tier invents its own rule id per finding, so a
/// fixture-keyed match would only ever fire on one benchmark). General by construction:
/// every signal here is a defect-family vocabulary word (an OWASP/CWE term or the
/// framework API a real fix touches — `dangerouslySetInnerHTML`, `.or(`, `Origin`), not
/// any one repo's own identifiers. Returns `None` when nothing matches — the caller
/// (`is_uncited_ai_finding`) treats that as genuinely uncited, never guesses.
pub(crate) fn classify_ai_finding(
    rule_id: &str,
    category: Option<&str>,
    detail: &str,
) -> Option<AiFindingClass> {
    let id = rule_id.to_ascii_uppercase();
    let det = detail.to_ascii_lowercase();
    let has_id = |needle: &str| id.contains(needle);
    let has_det = |needle: &str| det.contains(needle);

    // RLS: disabled, missing, or overly permissive Row Level Security on an exposed table.
    if category == Some("rls-policy")
        || has_id("RLS")
        || has_id("ROW-LEVEL")
        || has_det("row level security")
        || has_det("row-level security")
    {
        return Some(AiFindingClass::Rls);
    }
    // Stored/reflected XSS via a raw-HTML sink.
    if has_id("XSS")
        || has_id("INNERHTML")
        || has_det("dangerouslysetinnerhtml")
        || has_det("innerhtml")
        || has_det("cross-site scripting")
        || has_det("cross site scripting")
        || has_det("stored xss")
        || has_det("reflected xss")
    {
        return Some(AiFindingClass::Xss);
    }
    // Open redirect: a redirect target with no allowlist check.
    if has_id("OPEN-REDIRECT")
        || has_det("open redirect")
        || has_det("unvalidated redirect")
        || (has_det("redirect")
            && (has_det("attacker")
                || has_det("untrusted")
                || has_det("arbitrary")
                || has_det("user-controlled")
                || has_det("unvalidated")))
    {
        return Some(AiFindingClass::OpenRedirect);
    }
    // Insecure randomness backing a security token/nonce/session id.
    if has_det("math.random")
        || has_det("random.random(")
        || has_det("insecure random")
        || has_det("weak random")
        || has_det("predictable token")
        || has_det("prng")
        || has_det("non-cryptographic")
        || ((has_id("RANDOM") || has_id("PRNG"))
            && (has_id("TOKEN") || has_id("SESSION") || has_id("NONCE") || has_id("SECRET")))
        || (has_det("random")
            && (has_det("token")
                || has_det("nonce")
                || has_det("session id")
                || has_det("password reset")
                || has_det("api key")
                || has_det("share link")))
    {
        return Some(AiFindingClass::WeakTokenRandomness);
    }
    // Query/filter-grammar injection: PostgREST .or()/.filter(), a NoSQL operator string,
    // an LDAP filter — built from interpolated user input rather than a bound argument.
    if has_id("FILTER-INJECT")
        || has_id("QUERY-GRAMMAR")
        || has_id("GRAMMAR-INJECT")
        || has_det(".or(")
        || has_det(".filter(")
        || has_det("query grammar")
        || has_det("filter grammar")
        || has_det("postgrest")
        || has_det("$where")
        || has_det("ldap filter")
    {
        return Some(AiFindingClass::QueryGrammarInjection);
    }
    // Permissive CORS: a wildcard/reflected origin combined with allowed credentials.
    let mentions_cors = has_id("CORS") || has_det("cors") || has_det("access-control-allow-origin");
    let mentions_credentials = has_id("CREDENTIAL")
        || has_det("credential")
        || has_det("allow-credentials")
        || has_det("cookie");
    if mentions_cors && mentions_credentials {
        return Some(AiFindingClass::PermissiveCors);
    }
    None
}

/// Resolve `finding`'s citation, applying the P3 class fallback when its OWN rule id has
/// no grounded corpus citation and no preview tool (`resolve_citation`'s "advisory"
/// branch) — but ONLY for an AI-tier finding (`is_ai_tier`); a deterministic finding's
/// advisory citation is a different, pre-existing gap this pass does not touch. Returns
/// the ORIGINAL citation unchanged in every other case (already grounded, already
/// preview-labeled, not AI-tier, or AI-tier but unclassifiable).
pub(crate) fn citation_for_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> CitationJson {
    let base = resolve_citation(&finding.rule_id, finding.preview_tool.as_deref(), corpus);
    if base.kind != "advisory" || !is_ai_tier(finding) {
        return base;
    }
    match classify_ai_finding(
        &finding.rule_id,
        finding.category.as_deref(),
        &finding.detail,
    ) {
        Some(class) => {
            let grounded = resolve_citation(class.grounding_rule_id(), None, corpus);
            if grounded.kind == "grounded" {
                grounded
            } else {
                base
            }
        }
        None => base,
    }
}

/// The P3 curation gate: true when `finding` is AI-tier AND its citation is still
/// "advisory" after the class-fallback attempt above — i.e. it cannot be honestly
/// presented as grounded. `build_report_json` excludes such a finding from
/// `curated_findings` entirely and routes it to the informational/held-for-review bucket
/// with an explicit "needs review (uncited)" headline, REGARDLESS of severity — unlike
/// `is_informational`'s other four signals, this one is a report-integrity gate, not a
/// triage-confidence signal, so the "a critical/high finding is never informational"
/// invariant there does not apply here on purpose: a critical, uncited finding is
/// PRECISELY the case this gate exists to catch.
pub(crate) fn is_uncited_ai_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> bool {
    is_ai_tier(finding) && citation_for_finding(finding, corpus).kind == "advisory"
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
/// Resolve for the option this finding was ACTUALLY evaluated under, so the report's Fix
/// text matches what was judged, never a stale default (2026-09-22 audit-integrated
/// alternatives, extending the 2026-09-13 Fix-1 mechanism below). Resolution order, via
/// [`camerata_rules::Rule::resolved_option`]:
/// 1. `finding.evaluated_option_id` — the option the audit (or a targeted
///    `rescan-alternatives` call) actually judged this specific finding against, when the
///    rule is multi-option and semantic;
/// 2. `chosen_option` — the project's persisted `RuleSelection.chosen_option` for this rule,
///    passed in by the caller (`ReportOptions::chosen_options`), when the finding predates
///    this field or belongs to a rule the AI audit never tagged (e.g. a legacy persisted
///    finding);
/// 3. the corpus rule's own `default_option` — `resolved_option`'s own fallback, unchanged
///    from before this feature.
pub(crate) fn resolve_fix(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
) -> Option<String> {
    let rule = corpus.and_then(|c| c.get_by_id(rule_id))?;
    let evaluated = finding.evaluated_option_id.as_deref().or(chosen_option);
    let option = rule.resolved_option(evaluated)?;
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

// ── P4 (2026-09-29): floor findings get finding-level treatment ────────────────────
//
// A deterministic/floor finding's `detail` is set at scan time
// (`onboard::audit::title_for`) from the GATE's own rule description — internal, agent-facing
// enforcement prose ("Deny writing a file whose path marks it as secret-bearing…"), never
// authored for a client to read. Left alone, that text flows straight into both the headline
// (`defect_headline` takes its first sentence) and the detail of every curated site for a
// floor rule. [`resolve_floor_finding_text`] is the render-time escape hatch, mirroring
// [`resolve_fix`] exactly: an AUTHORED template lives on the rule's resolved option in the
// corpus (`RuleOption::finding_headline` / `finding_detail`), instantiated from the SAME
// `<path>`/`<file>`/capture placeholders `resolve_fix` already substitutes, so a rule author
// writes ONE specific, plain-language pair per rule and every finding of that rule gets a
// repo-specific rendering for free. `is_ai_tier` findings never reach this function in
// practice (they invent their own rule id, which the corpus never resolves) but nothing here
// assumes that — it degrades to `None` for any rule id the corpus doesn't know, same as
// `resolve_fix`.

/// Resolve the authored, client-facing (headline, detail) pair for a DETERMINISTIC FLOOR
/// finding (P4), instantiating `<path>`/`<file>`/capture placeholders exactly as
/// [`resolve_fix`] does for `remediation`. Returns `None` — the caller then falls back to the
/// pre-P4 `defect_headline(&finding.detail, …)` / `finding.detail.clone()` behavior — when: the
/// corpus is absent, the rule id has no corpus entry, the rule has no resolvable option, or
/// either `finding_headline` or `finding_detail` is absent/blank on that option (authored as a
/// PAIR; never render one half authored and the other the gate's raw text). This is the ONLY
/// path by which a floor finding's client-facing headline/detail can differ from its raw
/// `Finding::detail` — see the module-level comment above for why that indirection exists.
pub(crate) fn resolve_floor_finding_text(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    finding: &Finding,
    chosen_option: Option<&str>,
) -> Option<(String, String)> {
    let rule = corpus.and_then(|c| c.get_by_id(rule_id))?;
    let evaluated = finding.evaluated_option_id.as_deref().or(chosen_option);
    let option = rule.resolved_option(evaluated)?;
    let headline_tpl = option.finding_headline.as_deref()?.trim();
    let detail_tpl = option.finding_detail.as_deref()?.trim();
    if headline_tpl.is_empty() || detail_tpl.is_empty() {
        return None;
    }
    let headline = instantiate_remediation(headline_tpl, &finding.path, &finding.captures);
    let detail = instantiate_remediation(detail_tpl, &finding.path, &finding.captures);
    Some((headline, detail))
}

/// Client-facing (headline, detail) for ONE finding, preferring the P4 authored floor
/// template ([`resolve_floor_finding_text`]) and falling back to the pre-P4 derivation
/// (`defect_headline` over `finding.detail`, `finding.detail` verbatim) when no authored
/// template is available for this rule/option — an AI-tier finding (whose own prose already
/// leads with a plain-language sentence by house convention) or a floor rule not yet authored.
/// Centralizing this join means the matrix headline and the curated site's headline+detail can
/// never drift onto two different derivations of the same finding.
pub(crate) fn client_headline_and_detail(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
    fallback_title: &str,
    chosen_option: Option<&str>,
) -> (String, String) {
    match resolve_floor_finding_text(&finding.rule_id, corpus, finding, chosen_option) {
        Some((headline, detail)) => (headline, detail),
        None => (
            defect_headline(&finding.detail, fallback_title),
            finding.detail.clone(),
        ),
    }
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
        // P4: the secret-shaped floor rules' context-fact tokens (`onboard::audit::
        // enrich_secret_context` / `onboard::attach_secret_history_capture`). The first two
        // are always populated once the rule fires (the enrichment is unconditional and
        // pure); `history-status` is best-effort (needs a real local git checkout), so this
        // fallback is its realistic path, not just defensive dead code.
        "secret-kind" => "a hardcoded credential",
        "gitignore-status" => "not confirmed against this repository's `.gitignore`",
        "history-status" => "not checked against this repository's commit history",
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

/// True when [`crate::ai_audit::generate_fix_specifics`] (P2) exhausted its retries and gave
/// up on this finding — it carries NO usable `fix_specific`, so `CuratedSiteJson::
/// fix_for_this_finding` stays `None` for it. Never `do_now`: a same-week action item must
/// come with an actual fix, not a promise the report doesn't keep — see `matrix_bucket`'s two
/// call sites in [`build_report_json`], which downgrade a would-be `do_now` bucket to
/// `do_next` when this is true.
///
/// Detected from the `"[needs review: fix not generated]"` `detail` tag
/// `generate_fix_specifics` appends on failure — the same free-text-tag convention
/// `apply_verdicts` already uses for its own needs-review reasons (see that function's doc
/// comment). Deliberately a substring check rather than a dedicated bool field: it keys ONLY
/// on the fix-generation failure path, so a finding that is `needs_review` for any OTHER
/// reason (a debatable calibration verdict, an in-test flag, …) is untouched — this must
/// never widen into a blanket "no `fix_specific` yet" gate, which would also catch every
/// finding from a deterministic-only scan (fix-generation is itself an AI pass, gated off
/// entirely when `run_ai_review` is false) and wrongly pull them out of `do_now`.
pub(crate) fn fix_generation_failed(finding: &Finding) -> bool {
    finding.detail.contains("fix not generated")
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
/// bucket-count partition.
///
/// # P5: review-state honesty + full reconciliation
/// `default_narrative` now takes `review_state` and branches on it — this is the fix for a
/// raw (nobody has reviewed anything this run) export whose narrative used to claim "N were
/// reviewed... 0 were dispositioned as false positives by the reviewer" no matter what, even
/// when NO reviewer had ever opened the report. See [`ReviewState`]'s doc comment for the
/// derivation.
/// - `Raw`: describes what the ENGINE did ("The engine analyzed N files and produced C
///   candidate findings..."). NEVER the phrase "reviewed by a human reviewer" or
///   "dispositioned by a human reviewer" — grep-tested (see
///   `raw_export_carries_the_draft_flag_and_a_fully_reconciling_engine_voiced_narrative`).
/// - `Reviewed`: describes the reviewer's actual dispositions, same voice as before this pass.
///
/// In BOTH states the narrative reconciles EVERY candidate finding, with each number shown:
/// `candidates_reviewed == curated_total + held_for_review + excluded_fp +
/// dependency_advisories` always holds (by construction — see `build_report_json`: every
/// finding lands in exactly one of those four buckets). This is the fix for the "41 reviewed;
/// 0 excluded... 18 curated" bug, where the other 23 needs-review rows were held out of the
/// curated set and never counted anywhere in the narrative.
#[allow(clippy::too_many_arguments)]
fn default_narrative(
    review_state: ReviewState,
    files_scanned: usize,
    candidates_reviewed: usize,
    excluded_fp: usize,
    curated_total: usize,
    held_for_review: usize,
    dependency_advisories: usize,
    do_now: usize,
    do_next: usize,
    plan: usize,
    accepted: usize,
) -> String {
    if candidates_reviewed == 0 {
        return "The scan surfaced no candidate findings to review in this run.".to_string();
    }
    let dep_clause = if dependency_advisories == 0 {
        String::new()
    } else {
        format!(
            " A further {} tracked separately in the dependency snapshot.",
            noun(
                dependency_advisories,
                "dependency advisory is",
                "dependency advisories are"
            ),
        )
    };
    match review_state {
        ReviewState::Raw => format!(
            "This is a draft export: no finding in this run has had a human triage pass. The \
             engine analyzed {} and produced {}. {curated_total} {} curated for action \
             ({do_now} do now, {do_next} do next, {plan} planned, {accepted} already carried \
             as accepted risk from a prior baseline or in-code waiver), {held_for_review} {} \
             held for a human reviewer's judgment call, and {excluded_fp} {} auto-excluded as \
             likely false positives.{dep_clause}",
            noun(files_scanned, "file", "files"),
            noun(
                candidates_reviewed,
                "candidate finding",
                "candidate findings"
            ),
            if curated_total == 1 { "is" } else { "are" },
            if held_for_review == 1 { "is" } else { "are" },
            if excluded_fp == 1 { "was" } else { "were" },
        ),
        ReviewState::Reviewed => format!(
            "{} were reviewed by a human reviewer; {} {} dispositioned as false positives and \
             excluded entirely from this report. Of the remaining {}, {curated_total} {} \
             curated for action ({do_now} do now, {do_next} do next, {plan} planned, \
             {accepted} accepted as risk) and {held_for_review} {} held for further review.\
             {dep_clause}",
            noun(
                candidates_reviewed,
                "candidate finding",
                "candidate findings"
            ),
            excluded_fp,
            if excluded_fp == 1 { "was" } else { "were" },
            noun(curated_total + held_for_review, "finding", "findings"),
            if curated_total == 1 { "is" } else { "are" },
            if held_for_review == 1 { "is" } else { "are" },
        ),
    }
}

/// P5: the AI-tier Methodology paragraph must never claim a human already reviewed advisory
/// findings when this export is [`ReviewState::Raw`] — the paragraph used to unconditionally
/// say "Every advisory finding is reviewed and dispositioned by a human reviewer before it
/// appears here", which is a flat fabrication on a raw, pre-review export. Neither branch
/// contains the literal phrases "reviewed by a human reviewer" or "dispositioned by a human
/// reviewer" in the `Raw` case — grep-tested alongside `default_narrative`'s own gate.
fn ai_tier_note_for(review_state: ReviewState) -> String {
    match review_state {
        ReviewState::Reviewed => {
            "A second, advisory tier uses a calibrated language-model review for findings that \
             require semantic judgment. Every advisory finding in this export has been \
             reviewed and dispositioned by a human reviewer; lower-confidence items are marked \
             needs-review."
                .to_string()
        }
        ReviewState::Raw => {
            "A second, advisory tier uses a calibrated language-model review for findings that \
             require semantic judgment. This export has not yet had a human triage pass: every \
             advisory finding here is the engine's own calibrated output, awaiting a human \
             reviewer's judgment before any is accepted, marked tech debt, or ruled out. \
             Lower-confidence items are marked needs-review."
                .to_string()
        }
    }
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
        return "None of these items has a calibrated effort estimate yet; ask the reviewer for \
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
    // P5: `Reviewed` the moment ANY finding carries an explicit THIS-SESSION auditor
    // disposition — see `ReviewState`'s doc comment for why `BaselineAccepted`/`WaivedInline`
    // (sourced from `Finding.status`, not the wire map) never flip this.
    let mut auditor_touched_any_finding = false;
    let mut live: Vec<(&Finding, Disposition, String, String)> = Vec::new();
    for f in &report.findings {
        let wire = dispositions.get(&finding_key(f));
        if let Some(w) = wire {
            if matches!(w.state.as_str(), "Ignored" | "TechDebt" | "FalsePositive") {
                auditor_touched_any_finding = true;
            }
        }
        if wire.map(|d| d.state.as_str()) == Some("FalsePositive") {
            excluded_fp += 1;
            continue;
        }
        let disposition = classify(f, wire);
        let reason = wire.map(|d| d.reason.clone()).unwrap_or_default();
        let severity = normalize_severity(&f.severity);
        live.push((f, disposition, reason, severity));
    }
    let review_state = if auditor_touched_any_finding {
        ReviewState::Reviewed
    } else {
        ReviewState::Raw
    };

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
        // P3: an AI-tier finding with no grounded citation (`is_uncited_ai_finding`) is ALSO
        // routed here — deliberately independent of severity (see that function's doc
        // comment), since the whole point is to catch the critical/high findings that would
        // otherwise sit in the curated set carrying "AI-advisory, model-inferred."
        let gate_uncited = is_uncited_ai_finding(f, corpus);
        let bucket = if is_informational(f, *disposition, severity, corpus, report.test_file_count)
            || gate_uncited
        {
            "informational"
        } else {
            matrix_bucket(*disposition, severity, f.effort.as_deref())
        };
        // P2: a finding whose fix-generation gave up must never sit in `do_now` — a
        // same-week action item without an actual fix would falsify the report's own
        // promise. See `fix_generation_failed`'s doc comment.
        let bucket = if bucket == "do_now" && fix_generation_failed(f) {
            "do_next"
        } else {
            bucket
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
        // P4: a deterministic floor finding renders its AUTHORED, repo-specific headline
        // (never the gate's raw "Deny…" directive) when the corpus has one for this rule;
        // an AI-tier / not-yet-authored finding falls back to the pre-P4 derivation exactly
        // as before. See `client_headline_and_detail`'s doc comment.
        let chosen_option_for_rule = opts
            .chosen_options
            .get(&f.rule_id.to_ascii_uppercase())
            .map(String::as_str);
        let (base_headline, _) =
            client_headline_and_detail(f, corpus, &fallback_title, chosen_option_for_rule);
        // P3: an honest "why is this not curated" marker for the appendix row, distinct
        // from the other informational reasons (which the appendix count doesn't otherwise
        // distinguish either — see `matrix.informational`'s doc comment).
        let headline = if gate_uncited {
            format!("Needs review (uncited — no grounded citation found): {base_headline}")
        } else {
            base_headline
        };
        target.push(finding_ref(f, severity, headline));
    }

    // Curated findings: grouped by rule (sorted for deterministic output), each rule's
    // sites sorted by repo/path/line.
    #[allow(clippy::type_complexity)]
    let mut by_rule: std::collections::BTreeMap<String, Vec<(&Finding, Disposition, String, String)>> =
        std::collections::BTreeMap::new();
    for (f, disposition, reason, severity) in &code_findings {
        // P3 citation gate: an AI-tier finding with no grounded citation (own rule id, nor
        // its mapped class) never enters the curated set — see `is_uncited_ai_finding`'s doc
        // comment. It was already routed to `matrix.informational` above; skipping it here
        // means no `CuratedGroupJson` is ever built for it, so it is IMPOSSIBLE for
        // `curated_findings` to carry an "AI-advisory, model-inferred." citation.
        if is_uncited_ai_finding(f, corpus) {
            continue;
        }
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
        // P3: every site surviving the gate above is EITHER not AI-tier (its citation is
        // whatever `resolve_citation` alone gives, unchanged from before this pass) OR
        // AI-tier and grounded via the class fallback (`citation_for_finding`). Reuse the
        // first non-advisory result any site in the group offers; fall back to the plain
        // rule-level join for a uniform non-AI-tier group (byte-for-byte the old behavior).
        let citation = sites
            .iter()
            .find_map(|(f, _, _, _)| {
                let c = citation_for_finding(f, corpus);
                (c.kind != "advisory").then_some(c)
            })
            .unwrap_or_else(|| resolve_citation(&rule_id, preview_tool, corpus));
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
                // P2: keep this site's own disposition label in lockstep with the matrix
                // override above — never claim "Open (recommended: Do now)" on a site whose
                // fix-generation failed.
                let bucket = if bucket == "do_now" && fix_generation_failed(f) {
                    "do_next"
                } else {
                    bucket
                };
                let confirmed_by_client = dispositions
                    .get(&finding_key(f))
                    .map(|d| d.confirmed_by_client)
                    .unwrap_or(false);
                let chosen_option_for_rule = opts
                    .chosen_options
                    .get(&rule_id.to_ascii_uppercase())
                    .map(String::as_str);
                // P4: prefer the authored, repo-specific (headline, detail) pair over the raw
                // gate text — see `client_headline_and_detail`'s doc comment. `detail` here is
                // NOT `f.detail.clone()` unconditionally anymore: for a floor finding with an
                // authored template, it's the authored client-facing detail instead of the
                // gate's own "Deny…" enforcement prose.
                let (headline, detail) =
                    client_headline_and_detail(f, corpus, &title, chosen_option_for_rule);
                CuratedSiteJson {
                    repo: f.repo.clone(),
                    path: f.path.clone(),
                    line: f.line,
                    snippet: cap_snippet(&f.snippet),
                    detail,
                    severity: severity.clone(),
                    effort: f.effort.clone(),
                    confidence: f.confidence.clone(),
                    disposition: disposition_label(*disposition, reason, bucket, confirmed_by_client),
                    also_matches: f.also_matches.clone(),
                    headline,
                    fix: resolve_fix(&rule_id, corpus, f, chosen_option_for_rule),
                    // P2: `f.fix_specific` was generated at SCAN time by
                    // `ai_audit::generate_fix_specifics` (this layer stays pure/synchronous —
                    // no model access here, just a read) — a codebase-specific fix that the
                    // template renders ABOVE `fix` (the rule's generic remediation is now
                    // secondary context only). `None` when generation never ran (a
                    // deterministic-only scan) or gave up after retries (see
                    // `fix_generation_failed`, which is what keeps such a finding out of
                    // `do_now` above).
                    fix_for_this_finding: f.fix_specific.clone(),
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
    let dependency_advisories = dependency_snapshot.rows.len();
    let (narrative, is_override) = match &opts.executive_summary_override {
        Some(text) if !text.trim().is_empty() => (text.clone(), true),
        _ => (
            default_narrative(
                review_state,
                report.files_scanned,
                candidates_reviewed,
                excluded_fp,
                curated_total,
                informational,
                dependency_advisories,
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
        held_for_review: informational,
        dependency_advisories,
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
    // P6: hide the "Code volume" row entirely rather than ever render "0 characters"/"0
    // lines" — `None` whenever this run genuinely has no real line count (a compliance-blocked
    // AI-only request, or any other path that never read a local file).
    let code_volume = (report.code_lines > 0).then(|| CodeVolumeJson {
        lines: report.code_lines,
        by_language: report.code_lines_by_language.clone(),
    });
    let cover = CoverJson {
        repos: report.repos.clone(),
        files_scanned: report.files_scanned,
        files_excluded: report.files_excluded,
        code_chars: report.code_chars,
        code_volume,
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
        held_for_review: informational,
        next_steps: NEXT_STEPS_NOTE.to_string(),
        deterministic_note:
            "Camerata runs a two-tier engine. A deterministic security floor (proven-defect \
             SAST rules plus a migration-timeline replay for Supabase Row Level Security) \
             produces findings that either hold or do not, with no model judgment. These are \
             labeled deterministic in the report."
                .to_string(),
        ai_tier_note: ai_tier_note_for(review_state),
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
        review_state,
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
    use crate::onboard::{AuditedRef, CoverageNote, LanguageVolume, ScanProvenance};

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
            code_lines: 400,
            code_lines_by_language: vec![
                LanguageVolume {
                    language: "TypeScript".to_string(),
                    lines: 300,
                },
                LanguageVolume {
                    language: "Rust".to_string(),
                    lines: 100,
                },
            ],
            excluded_mechanical_rules: Vec::new(),
            findings,
            proposed_rules: Vec::new(),
            gated: false,
            blocked: false,
            ai_blocked_reason: None,
            ai_error: None,
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
            recommendations: std::collections::HashMap::new(),
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
            "Needs client confirmation (reviewer's proposed rationale: looks like defense-in-depth only)"
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
            json.executive_summary
                .narrative
                .contains("1 do now, 0 do next, 1 planned, 0 already carried as accepted risk"),
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

    // ── P1: deduplication + cross-tier merge, end-to-end through build_report_json ──────────
    // docs/plans/2026-09-29_codebase-inspection-hardening.md, P1. Runs the SAME two merge
    // passes the real audit pipeline runs (`ai_audit::merge_by_location` then
    // `ai_audit::merge_semantic_groups`) over a faktura-SHAPED synthetic fixture — never the
    // real fixture, never a fixture-specific rule/file/table name — then builds the report and
    // asserts the cover's counts reflect the MERGED, distinct-defect set.

    #[tokio::test]
    async fn p1_e2e_cover_counts_and_security_finding_survive_post_merge() {
        // Defect 1: one SQL-injection defect, flagged by BOTH a deterministic floor rule and an
        // AI rule at the exact same location (same file+line+snippet) — must collapse to one
        // critical row, not two.
        let sql_code = "SELECT * FROM t WHERE name ILIKE '%${name}%'";
        let mut det_sql = finding("SEC-NO-RAW-SQL-CONCAT-1", "api/query.ts", 10, "critical");
        det_sql.snippet = sql_code.to_string();
        let mut ai_sql = finding("AI-SQL-INJECTION", "api/query.ts", 10, "critical");
        ai_sql.snippet = sql_code.to_string();

        // Defect 2: a config flag and the handler that trusts it, in DIFFERENT files, tied
        // together only by a shared captured object — must collapse to one medium row.
        let mut config_flag = finding("CONFIG-FLAG-DEBUG-MODE", "config.rs", 5, "medium");
        config_flag
            .captures
            .insert("flag".to_string(), "debug_mode".to_string());
        let mut handler_flag = finding("AI-HANDLER-TRUSTS-DEBUG-FLAG", "handler.rs", 80, "medium");
        handler_flag
            .captures
            .insert("flag".to_string(), "debug_mode".to_string());

        // Defect 3: the canonical security-vs-structure overlap — a low-value structural rule
        // and a genuine AI security finding (reflected-origin CORS with credentials) at the
        // same line. The security finding must survive as the primary, never hidden.
        let cors_code = "app.use(cors())";
        let mut structural = finding("ARCH-MIDDLEWARE-FIRST-1", "middleware.ts", 12, "high");
        structural.snippet = cors_code.to_string();
        let mut security = finding(
            "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS",
            "middleware.ts",
            12,
            "medium",
        );
        security.snippet = cors_code.to_string();

        let files = vec![
            ("api/query.ts".to_string(), sql_code.to_string()),
            ("middleware.ts".to_string(), cors_code.to_string()),
        ];
        let raw = vec![
            det_sql,
            ai_sql,
            config_flag,
            handler_flag,
            structural,
            security,
        ];

        // The real pipeline's two merge passes, in the real order: exact-location first, then
        // cross-tier/cross-file.
        let merged = crate::ai_audit::merge_semantic_groups(
            crate::ai_audit::merge_by_location(raw, &files),
            &files,
        );
        assert_eq!(
            merged.len(),
            3,
            "six raw findings, three distinct defects — merging must land on exactly three"
        );

        // P3: the CORS security finding is an AI-tier rule id the corpus never saw (an
        // invented "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS" id) — the citation gate only lets
        // it survive in `curated_findings` when its class maps to a REAL grounded corpus
        // rule (`SEC-NO-PERMISSIVE-CORS-CREDENTIALS-1`), which requires the real corpus to
        // be loaded rather than `None`.
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(
            corpus_errors.is_empty(),
            "corpus must load cleanly, got errors: {corpus_errors:?}"
        );

        let report = report_with(merged, vec![]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());

        // Cover critical count == distinct criticals: the SQL-injection defect is the only
        // critical among the three merged defects, and it must count ONCE.
        assert_eq!(
            json.cover.stats.critical, 1,
            "the merged SQL-injection defect must count once, not twice"
        );

        // The CORS security finding must survive as its own curated row, never absorbed under
        // (or hidden behind) the structural rule id, and at medium severity or higher.
        let cors_group = json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS")
            .expect("the security finding must survive the merge as the primary, not be hidden");
        assert!(
            matches!(
                cors_group.sites[0].severity.as_str(),
                "medium" | "high" | "critical"
            ),
            "the security finding must never be downgraded below medium: {}",
            cors_group.sites[0].severity
        );
        assert!(cors_group.sites[0]
            .also_matches
            .contains(&"ARCH-MIDDLEWARE-FIRST-1".to_string()));

        // The structural rule id must NOT appear as its own curated row (it lost primacy to the
        // security finding it clustered with).
        assert!(
            !json
                .curated_findings
                .iter()
                .any(|g| g.rule_id == "ARCH-MIDDLEWARE-FIRST-1"),
            "the structural row must not survive as its own peer row once absorbed"
        );
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
        let fix = resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f, None)
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
        let fix = resolve_fix("SEC-TEST-UNAUTHORED-1", Some(&corpus), &f, None);
        assert_eq!(
            fix, None,
            "must omit the Fix block, not fall back to the rule's directive, when remediation is unauthored"
        );
    }

    #[test]
    fn resolve_fix_is_none_not_fabricated_when_corpus_is_absent() {
        let f = finding("SEC-NO-UNSAFE-DESERIALIZATION-1", "a.py", 1, "critical");
        assert_eq!(resolve_fix("SEC-NO-UNSAFE-DESERIALIZATION-1", None, &f, None), None);
    }

    #[tokio::test]
    async fn resolve_fix_is_none_when_rule_id_is_unknown_to_the_corpus() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        assert_eq!(resolve_fix("AI-CUSTOM-ARCH-RULE-1", Some(&corpus), &f, None), None);
    }

    // ── resolve_fix honors the evaluated/chosen option (2026-09-22 audit-integrated
    // alternatives), not always the rule's default ──────────────────────────────────

    /// `SUPABASE-RLS-ENABLED-1` has two options: the default
    /// `enforce-rls-enabled-replayed-end-state` (authored remediation) and
    /// `regex-grep-each-migration-independently` (NO authored remediation — a deliberately
    /// rejected alternative in the corpus). This makes it a real fixture for proving
    /// `evaluated_option_id` genuinely changes which option's remediation renders, not just
    /// which rule's default happens to be picked.
    #[tokio::test]
    async fn resolve_fix_uses_the_finding_evaluated_option_id_over_the_project_chosen_option() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        f.evaluated_option_id = Some("enforce-rls-enabled-replayed-end-state".to_string());
        // The chosen_option PARAMETER disagrees with the finding's evaluated_option_id — the
        // finding's tag must win (it's what this SPECIFIC finding was actually judged under).
        let fix = resolve_fix(
            "SUPABASE-RLS-ENABLED-1",
            Some(&corpus),
            &f,
            Some("regex-grep-each-migration-independently"),
        );
        assert!(
            fix.is_some(),
            "the evaluated option has authored remediation and must win over the chosen_option param"
        );
    }

    /// When the finding carries no `evaluated_option_id` (a legacy finding, or a rule the AI
    /// tier never tagged), `resolve_fix` falls back to the project's `chosen_option` parameter
    /// — and correctly OMITS the Fix block when that specific option has no authored
    /// remediation, proving the per-option (not just per-rule) omit-if-unauthored contract.
    #[tokio::test]
    async fn resolve_fix_falls_back_to_the_chosen_option_param_when_the_finding_has_none() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        assert!(f.evaluated_option_id.is_none());
        let fix = resolve_fix(
            "SUPABASE-RLS-ENABLED-1",
            Some(&corpus),
            &f,
            Some("regex-grep-each-migration-independently"),
        );
        assert_eq!(
            fix, None,
            "the chosen (non-default) option has no authored remediation — must omit, not \
             fall back to the default option's remediation or the directive"
        );
    }

    /// With neither `evaluated_option_id` nor a `chosen_option` param, resolution falls all the
    /// way through to the corpus rule's own default — today's exact pre-feature behavior,
    /// unchanged.
    #[tokio::test]
    async fn resolve_fix_falls_back_to_the_corpus_default_when_nothing_else_is_set() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        let fix = resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f, None);
        assert!(fix.is_some(), "the corpus default option has authored remediation");
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
        // Deliberately NOT an `AI-`-prefixed rule id: this test is about `resolve_fix`
        // (the "Fix:" line), not the P3 citation gate — an `AI-` id with no corpus would
        // also be held out of `curated_findings` entirely as uncited (see
        // `citation_gate_holds_an_unclassifiable_ai_finding_out_of_curated_as_uncited`),
        // which would make `json.curated_findings[0]` panic here for an unrelated reason.
        let f = finding("CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
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

    // ── P4: floor findings get finding-level treatment ──────────────────────────────────

    /// Every AUDIT_RULES floor rule fired against a synthetic (never a fixture/benchmark)
    /// snippet, run through the REAL bundled corpus — no client-facing string (a curated
    /// site's `headline`, its `detail`, or any matrix `FindingRefJson::headline`) may ever
    /// start with "Deny": that word is the gate's own internal enforcement directive
    /// (`camerata_gateway::RULE_REGISTRY`), never authored client prose. General over the
    /// whole floor, not one rule — this is the P4 contract test the plan calls for.
    #[tokio::test]
    async fn no_client_facing_string_begins_with_deny_for_any_floor_rule_that_fires() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");

        let files = vec![
            (".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string()),
            (
                "a.py".to_string(),
                "API_KEY = \"hardcoded-not-a-real-secret-abcdefgh\"\n".to_string(),
            ),
            (
                "b.py".to_string(),
                concat!("token = \"sk_li", "ve_abcdefghijklmnopqrstuvwx\"\n").to_string(),
            ),
            (
                "c.pem".to_string(),
                concat!("-----BEGIN RSA PRIV", "ATE KEY-----\nMIIBFAKEFAKEFAKE\n-----END RSA PRIV", "ATE KEY-----\n")
                    .to_string(),
            ),
            (
                "d.py".to_string(),
                "q = \"SELECT * FROM users WHERE id = \" + user_id\n".to_string(),
            ),
            (
                "e.py".to_string(),
                "url = f\"https://api.example.com/x?api_key={key}\"\n".to_string(),
            ),
            ("f.py".to_string(), "requests.get(url, verify=False)\n".to_string()),
            ("g.py".to_string(), "yaml.load(data)\n".to_string()),
        ];
        let findings = crate::onboard::audit_files("owner/repo", &files);
        assert!(
            findings.len() >= 6,
            "fixture must actually trip most of the floor rules under test, got: {findings:?}"
        );

        let report = report_with(findings, Vec::new());
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());

        let mut offenders = Vec::new();
        for group in &json.curated_findings {
            for site in &group.sites {
                if site.headline.starts_with("Deny") {
                    offenders.push(format!("{} headline: {:?}", group.rule_id, site.headline));
                }
                if site.detail.starts_with("Deny") {
                    offenders.push(format!("{} detail: {:?}", group.rule_id, site.detail));
                }
            }
        }
        let matrix_buckets: [&Vec<FindingRefJson>; 5] = [
            &json.matrix.do_now,
            &json.matrix.do_next,
            &json.matrix.plan,
            &json.matrix.accepted,
            &json.matrix.informational,
        ];
        for bucket in matrix_buckets {
            for f in bucket {
                if f.headline.starts_with("Deny") {
                    offenders.push(format!("matrix headline: {:?}", f.headline));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "client-facing strings must never start with \"Deny\" (the gate's own internal \
             directive): {offenders:#?}"
        );
    }

    /// Every curated finding (floor or AI-tier) must carry a non-empty `est_hours` label — a
    /// deterministic floor finding must never render "not yet estimated" (see
    /// `default_effort_for` in `onboard::audit` and `effort_hours_bounds` above).
    #[tokio::test]
    async fn every_curated_finding_has_a_non_empty_est_hours() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let files = vec![(".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string())];
        let findings = crate::onboard::audit_files("owner/repo", &files);
        assert!(!findings.is_empty());
        let report = report_with(findings, Vec::new());
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        for group in &json.curated_findings {
            for site in &group.sites {
                let (_, hours_label) = effort_hours_bounds(site.effort.as_deref());
                assert_ne!(
                    hours_label, "not yet estimated",
                    "{}: curated finding must carry a real estimate, not the honest-gap label",
                    group.rule_id
                );
            }
        }
    }

    /// A committed-secret floor finding (`SEC-NO-SECRET-FILE-1` on a real `.env`) renders a
    /// headline naming the actual file, PLUS a `.gitignore`-coverage context fact in its
    /// detail — never the gate's own "Deny writing a file whose path marks it as
    /// secret-bearing…" directive.
    #[tokio::test]
    async fn committed_secret_finding_renders_a_specific_headline_and_a_context_fact() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let files = vec![
            (".gitignore".to_string(), "node_modules/\n".to_string()),
            (".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string()),
        ];
        let findings = crate::onboard::audit_files("owner/repo", &files);
        let report = report_with(findings, Vec::new());
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        let group = json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == "SEC-NO-SECRET-FILE-1")
            .expect("SEC-NO-SECRET-FILE-1 must be curated for a real committed .env");
        let site = &group.sites[0];
        assert!(
            site.headline.contains(".env"),
            "headline must name the actual file, got: {:?}",
            site.headline
        );
        assert!(
            !site.headline.starts_with("Deny") && !site.detail.starts_with("Deny"),
            "must never render the gate directive: headline={:?} detail={:?}",
            site.headline,
            site.detail
        );
        assert!(
            site.detail.contains("gitignore") || site.detail.contains(".gitignore"),
            "detail must carry the gitignore-coverage context fact, got: {:?}",
            site.detail
        );
    }

    /// `SEC-NO-RAW-SQL-CONCAT-1` had NO corpus entry at all before P4 (flagged by P3's own
    /// doc comment on `is_ai_tier`) — it must now resolve to a GROUNDED citation (CWE-89 +
    /// OWASP), never "AI-advisory, model-inferred." (it isn't even AI-tier — it's a
    /// deterministic floor rule, which makes the pre-P4 "advisory" label doubly wrong).
    #[tokio::test]
    async fn sec_no_raw_sql_concat_1_resolves_to_a_grounded_citation() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let citation = resolve_citation("SEC-NO-RAW-SQL-CONCAT-1", None, Some(&corpus));
        assert_eq!(
            citation.kind, "grounded",
            "SEC-NO-RAW-SQL-CONCAT-1 must resolve to a grounded citation, got: {citation:?}"
        );
        assert!(
            citation
                .sources
                .iter()
                .any(|s| s.url.contains("cwe.mitre.org/data/definitions/89")),
            "must cite CWE-89 (SQL Injection), got: {:?}",
            citation.sources
        );
        assert!(
            citation.sources.iter().any(|s| s.url.contains("owasp.org")),
            "must also cite OWASP, got: {:?}",
            citation.sources
        );

        // End-to-end: a real finding of this rule, run through the curated-set builder,
        // must carry that same grounded citation — not fall through to "advisory".
        let f = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.py", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        let group = json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == "SEC-NO-RAW-SQL-CONCAT-1")
            .expect("SEC-NO-RAW-SQL-CONCAT-1 must be curated, not held out as uncited");
        assert_eq!(group.citation.kind, "grounded");
    }

    /// A clean repo (no floor findings at all) must still render without panicking, with an
    /// empty curated set and empty matrix — the P4 machinery must never assume at least one
    /// floor finding exists.
    #[tokio::test]
    async fn a_clean_repo_with_no_floor_findings_still_renders_fine() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let files = vec![("a.py".to_string(), "print('hello world')\n".to_string())];
        let findings = crate::onboard::audit_files("owner/repo", &files);
        assert!(findings.is_empty(), "fixture must genuinely be clean");
        let report = report_with(findings, Vec::new());
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        assert!(json.curated_findings.is_empty());
        assert_eq!(json.matrix.do_now.len(), 0);
        assert_eq!(json.matrix.do_next.len(), 0);
        assert_eq!(json.matrix.plan.len(), 0);
        assert_eq!(json.matrix.informational.len(), 0);
    }

    // ── P2: fix_specific shown as the PRIMARY fix, generic `fix` secondary ──────────────

    #[test]
    fn curated_finding_site_shows_fix_specific_as_fix_for_this_finding() {
        let mut f = finding("ARCH-1", "app/auth/signout/route.ts", 12, "high");
        f.fix_specific = Some(
            "Use `safeInternalPath` from lib/redirect.ts, as app/auth/signin/route.ts does."
                .to_string(),
        );
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0]
                .fix_for_this_finding
                .as_deref(),
            Some("Use `safeInternalPath` from lib/redirect.ts, as app/auth/signin/route.ts does.")
        );
    }

    #[test]
    fn curated_finding_site_fix_for_this_finding_is_none_when_never_generated() {
        // Back-compat / deterministic-only-scan case: `fix_specific` was never set.
        let f = finding("ARCH-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.curated_findings[0].sites[0].fix_for_this_finding, None);
    }

    // ── P2: fix_generation_failed (pure) + the do_now gate it drives ────────────────────

    #[test]
    fn fix_generation_failed_detects_the_tag_generate_fix_specifics_appends() {
        let mut f = finding("ARCH-1", "a.rs", 1, "high");
        f.detail = "some real defect [needs review: fix not generated]".to_string();
        assert!(fix_generation_failed(&f));
    }

    #[test]
    fn fix_generation_failed_is_false_with_no_tag() {
        let f = finding("ARCH-1", "a.rs", 1, "high");
        assert!(!fix_generation_failed(&f));
    }

    #[test]
    fn fix_generation_failed_does_not_false_positive_on_an_unrelated_needs_review_reason() {
        // Calibration's OWN needs-review tag (a debatable-preference verdict) must never be
        // mistaken for the fix-generation failure tag — the do_now gate is scoped to fix
        // generation specifically, not every needs-review reason.
        let mut f = finding("ARCH-1", "a.rs", 1, "high");
        f.needs_review = true;
        f.detail = "an over-engineering note on a small codebase [needs review: debatable \
                     architectural preference]"
            .to_string();
        assert!(!fix_generation_failed(&f));
    }

    #[test]
    fn a_critical_finding_whose_fix_generation_failed_is_excluded_from_do_now() {
        let mut f = finding("ARCH-1", "a.rs", 1, "critical");
        f.detail = format!("{} [needs review: fix not generated]", f.detail);
        f.needs_review = true;
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(
            json.matrix.do_now.is_empty(),
            "a finding with no valid fix must never sit in do_now, even at critical severity: \
             {:?}",
            json.matrix.do_now
        );
        assert_eq!(
            json.matrix.do_next.len(),
            1,
            "it still surfaces — just not as do_now"
        );
        assert_eq!(
            json.curated_findings[0].sites[0].fix_for_this_finding, None,
            "and it carries no fix line to promise, matching the downgrade"
        );
    }

    #[test]
    fn a_critical_finding_with_a_valid_fix_still_lands_in_do_now() {
        // Regression pin: the P2 gate must not widen into "no fix_specific -> never do_now"
        // — only the EXPLICIT fix-generation-failed tag downgrades a finding. A critical
        // finding that simply never went through fix-generation (e.g. a deterministic-only
        // scan) keeps today's behavior.
        let f = finding("ARCH-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.matrix.do_now.len(), 1);
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
    fn instantiate_remediation_fills_p4_context_fact_tokens_from_captures_and_generic_fallback() {
        let mut captures = std::collections::BTreeMap::new();
        captures.insert(
            "secret-kind".to_string(),
            "a live Stripe secret key".to_string(),
        );
        let filled = instantiate_remediation(
            "Found <secret-kind> in <path>, which is <gitignore-status>.",
            ".env",
            &captures,
        );
        assert_eq!(
            filled,
            "Found a live Stripe secret key in .env, which is not confirmed against this \
             repository's `.gitignore`."
        );
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
    fn citation_join_still_labels_a_non_ai_tier_unknown_rule_id_as_ai_advisory() {
        // P3 scopes the citation gate to AI-TIER findings only (`is_ai_tier`) — a
        // non-AI-tier finding (no `AI-` rule id, no calibration `confidence`) whose rule id
        // has no corpus entry is a DIFFERENT, pre-existing gap (e.g. `SEC-NO-RAW-SQL-CONCAT-1`
        // has no TOML entry at all) that this pass does not touch: it stays exactly as
        // before, curated with the honest "AI-advisory, model-inferred." label.
        let f = finding("CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.curated_findings[0].citation.kind, "advisory");
        assert_eq!(
            json.curated_findings[0].citation.label,
            "AI-advisory, model-inferred."
        );
    }

    // ── P3 (2026-09-29): the citation gate ──────────────────────────────────────────────
    // docs/plans/2026-09-29_codebase-inspection-hardening.md, P3.

    /// An AI-tier finding (`AI-`-prefixed rule id) whose defect does not classify into any
    /// of the P3 closed taxonomy classes must be held OUT of `curated_findings` entirely —
    /// never rendered with "AI-advisory, model-inferred." on a curated row — and instead
    /// surfaces in the informational/held-for-review appendix with an honest "uncited"
    /// headline, counted in the reconciling totals.
    #[test]
    fn citation_gate_holds_an_unclassifiable_ai_finding_out_of_curated_as_uncited() {
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(
            json.curated_findings.is_empty(),
            "an uncited AI finding must never appear in curated_findings: {:?}",
            json.curated_findings
        );
        assert_eq!(
            json.matrix.informational.len(),
            1,
            "the uncited finding must still surface (over-tell), just outside the action tiers"
        );
        assert_eq!(
            json.matrix.informational[0].rule_id,
            "AI-CUSTOM-ARCH-RULE-1"
        );
        assert!(
            json.matrix.informational[0]
                .headline
                .to_lowercase()
                .contains("uncited"),
            "the appendix row must say WHY it's held out: {}",
            json.matrix.informational[0].headline
        );
        assert_eq!(json.executive_summary.held_for_review, 1);
        assert_eq!(json.executive_summary.curated_total, 0);
        // Reconciliation still holds: nothing is silently dropped.
        assert_eq!(
            json.executive_summary.candidates_reviewed,
            json.executive_summary.curated_total
                + json.executive_summary.held_for_review
                + json.executive_summary.excluded_false_positive
                + json.executive_summary.dependency_advisories
        );
    }

    /// The gate applies REGARDLESS of severity — a critical, uncited AI finding (exactly
    /// the "MOST important findings" the plan's problem statement calls out) must still be
    /// excluded from curated, unlike `is_informational`'s other four signals which
    /// deliberately never touch critical/high.
    #[test]
    fn citation_gate_excludes_a_critical_uncited_ai_finding_despite_severity() {
        let f = finding("AI-SOME-NOVEL-DEFECT", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(json.curated_findings.is_empty());
        assert_eq!(json.matrix.do_now.len(), 0);
        assert_eq!(json.matrix.informational.len(), 1);
        // The severity histogram (cover stats) still honestly reflects it — over-tell, not
        // hidden from the reader entirely, just excluded from the curated action tiers.
        assert_eq!(json.cover.stats.critical, 1);
    }

    /// A non-AI-tier finding's own advisory citation is untouched by the gate (see
    /// `citation_join_still_labels_a_non_ai_tier_unknown_rule_id_as_ai_advisory` above) —
    /// this is the `is_uncited_ai_finding` unit-level lock-in of that same scoping decision.
    #[test]
    fn is_uncited_ai_finding_is_false_for_a_non_ai_tier_finding_even_when_advisory() {
        let f = finding("CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        assert!(!is_ai_tier(&f));
        assert!(!is_uncited_ai_finding(&f, None));
    }

    #[test]
    fn is_ai_tier_detects_the_ai_prefixed_rule_id_or_a_calibration_confidence() {
        let invented = finding("AI-SOME-DEFECT", "a.rs", 1, "medium");
        assert!(is_ai_tier(&invented));

        let mut adopted = finding("SUPABASE-RLS-ENABLED-1", "a.rs", 1, "critical");
        adopted.confidence = Some("high".to_string());
        assert!(is_ai_tier(&adopted));

        let deterministic = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.rs", 1, "critical");
        assert!(!is_ai_tier(&deterministic));
    }

    // ── classify_ai_finding: pure, unit-testable, no model calls ────────────────────────

    #[test]
    fn classify_ai_finding_maps_each_required_class_by_general_keywords() {
        // Every signal is a defect-family vocabulary word (an OWASP/CWE term or the real
        // framework API a fix touches), never a fixture-specific rule id or identifier —
        // these rule ids are deliberately generic/invented, distinct from any one benchmark.
        assert_eq!(
            classify_ai_finding(
                "AI-DEFECT-1",
                Some("rls-policy"),
                "Row Level Security is disabled on the orders table, allowing cross-tenant reads."
            ),
            Some(AiFindingClass::Rls)
        );
        assert_eq!(
            classify_ai_finding(
                "AI-DEFECT-2",
                None,
                "User-supplied comment text is passed to dangerouslySetInnerHTML without sanitization, a stored XSS sink."
            ),
            Some(AiFindingClass::Xss)
        );
        assert_eq!(
            classify_ai_finding(
                "AI-DEFECT-3",
                None,
                "The `next` query parameter is redirected to without validation, an open redirect to an attacker-controlled host."
            ),
            Some(AiFindingClass::OpenRedirect)
        );
        assert_eq!(
            classify_ai_finding(
                "AI-DEFECT-4",
                None,
                "The password-reset token is generated with Math.random(), an insecure randomness source for a security token."
            ),
            Some(AiFindingClass::WeakTokenRandomness)
        );
        assert_eq!(
            classify_ai_finding(
                "AI-DEFECT-5",
                None,
                "User input is concatenated directly into a PostgREST .or() filter grammar expression, a query-grammar injection."
            ),
            Some(AiFindingClass::QueryGrammarInjection)
        );
        assert_eq!(
            classify_ai_finding(
                "AI-DEFECT-6",
                None,
                "The CORS middleware reflects the request's Origin header while Access-Control-Allow-Credentials is true."
            ),
            Some(AiFindingClass::PermissiveCors)
        );
    }

    #[test]
    fn classify_ai_finding_also_matches_on_rule_id_tokens_alone() {
        assert_eq!(
            classify_ai_finding("AI-XSS-COMMENT-RENDER-1", None, "unrelated detail text"),
            Some(AiFindingClass::Xss)
        );
        assert_eq!(
            classify_ai_finding("AI-CORS-REFLECTED-ORIGIN-CREDENTIALS", None, "detail"),
            Some(AiFindingClass::PermissiveCors)
        );
    }

    #[test]
    fn classify_ai_finding_returns_none_for_an_unmapped_defect() {
        assert_eq!(
            classify_ai_finding(
                "AI-SOME-NOVEL-ARCHITECTURAL-OBSERVATION",
                None,
                "The service layer bypasses the repository abstraction in a way nothing above classifies."
            ),
            None
        );
    }

    // ── The class -> corpus-rule mapping is grounded in the REAL bundled corpus ─────────

    /// A conservative allowlist of real, well-known standards/linter-doc hosts a P3 grounded
    /// citation is permitted to cite — a Camerata-internal doc citing itself would fail this,
    /// which is the whole point (see `resolve_citation`'s `is_external_source_url`, which this
    /// complements with a stronger "is it actually a recognized authority" check).
    fn is_known_authority_url(url: &str) -> bool {
        const KNOWN_AUTHORITY_HOSTS: &[&str] = &[
            "cwe.mitre.org",
            "owasp.org",
            "cheatsheetseries.owasp.org",
            "supabase.com",
            "react.dev",
            "developer.mozilla.org",
            "postgrest.org",
        ];
        is_external_source_url(url)
            && KNOWN_AUTHORITY_HOSTS.iter().any(|host| {
                url.starts_with(&format!("https://{host}/"))
                    || url.starts_with(&format!("http://{host}/"))
            })
    }

    #[tokio::test]
    async fn each_required_class_resolves_to_a_grounded_citation_from_a_known_authority() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(
            errors.is_empty(),
            "corpus must load cleanly, got errors: {errors:?}"
        );

        for class in [
            AiFindingClass::Rls,
            AiFindingClass::Xss,
            AiFindingClass::OpenRedirect,
            AiFindingClass::WeakTokenRandomness,
            AiFindingClass::QueryGrammarInjection,
            AiFindingClass::PermissiveCors,
        ] {
            let rule_id = class.grounding_rule_id();
            let citation = resolve_citation(rule_id, None, Some(&corpus));
            assert_eq!(
                citation.kind, "grounded",
                "{rule_id} (class {class:?}) must resolve to a grounded citation"
            );
            assert!(
                !citation.label.trim().is_empty(),
                "{rule_id}'s citation label must not be empty"
            );
            assert_ne!(
                citation.label, "AI-advisory, model-inferred.",
                "{rule_id} must never render the AI-advisory fallback label"
            );
            assert!(
                !citation.sources.is_empty(),
                "{rule_id} must carry at least one real source"
            );
            for source in &citation.sources {
                assert!(
                    is_known_authority_url(&source.url),
                    "{rule_id}'s source {:?} is not a well-formed, known-authority URL",
                    source
                );
            }
        }
    }

    /// Full-report contract test (EXTENSIVE per the plan): a synthetic fixture carries one
    /// AI-tier finding per required class plus one unmappable AI finding and one
    /// non-AI-tier finding with no corpus entry. After `build_report_json`, ZERO curated
    /// findings may carry a model-inferred or empty citation, and the unmapped finding must
    /// be the only one held out as uncited.
    #[tokio::test]
    async fn zero_curated_findings_carry_a_model_inferred_or_empty_citation() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(
            errors.is_empty(),
            "corpus must load cleanly, got errors: {errors:?}"
        );

        let mut rls = finding(
            "AI-CROSS-TENANT-RLS-1",
            "supabase/migrations/1.sql",
            1,
            "critical",
        );
        rls.detail = "Row Level Security is disabled on the payments table.".to_string();

        let mut xss = finding("AI-STORED-XSS-1", "web/comment.tsx", 12, "high");
        xss.detail =
            "A stored comment body is rendered via dangerouslySetInnerHTML without sanitization."
                .to_string();

        let mut redirect = finding("AI-LOGIN-REDIRECT-1", "web/login.ts", 40, "medium");
        redirect.detail =
            "The `next` redirect target is not validated against an allowlist, an open redirect."
                .to_string();

        let mut token = finding("AI-RESET-TOKEN-1", "api/reset.ts", 8, "high");
        token.detail =
            "The password-reset token uses Math.random(), insecure randomness for a security token."
                .to_string();

        let mut grammar = finding("AI-FILTER-INJECTION-1", "api/search.ts", 22, "high");
        grammar.detail =
            "A search term is concatenated into a PostgREST .or() filter grammar expression."
                .to_string();

        let mut cors = finding("AI-CORS-CREDS-1", "middleware.ts", 5, "medium");
        cors.detail =
            "CORS reflects the request Origin while Access-Control-Allow-Credentials is true."
                .to_string();

        let unmapped = finding("AI-SOME-NOVEL-DEFECT", "a.rs", 1, "medium");

        let report = report_with(
            vec![rls, xss, redirect, token, grammar, cors, unmapped],
            vec![],
        );
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());

        assert_eq!(
            json.curated_findings.len(),
            6,
            "the six mapped AI-tier findings must all survive as curated, distinct groups: {:?}",
            json.curated_findings
                .iter()
                .map(|g| &g.rule_id)
                .collect::<Vec<_>>()
        );
        for group in &json.curated_findings {
            assert_ne!(
                group.citation.kind, "advisory",
                "curated rule {} carries an advisory (uncited) citation",
                group.rule_id
            );
            assert!(
                !group.citation.label.trim().is_empty(),
                "curated rule {} carries an empty citation label",
                group.rule_id
            );
            assert_ne!(
                group.citation.label, "AI-advisory, model-inferred.",
                "curated rule {} renders the model-inferred fallback label",
                group.rule_id
            );
        }
        assert!(
            !json
                .curated_findings
                .iter()
                .any(|g| g.rule_id == "AI-SOME-NOVEL-DEFECT"),
            "the unmapped AI finding must never appear in curated_findings"
        );
        assert_eq!(
            json.matrix.informational.len(),
            1,
            "exactly the one unmapped AI finding is held for review"
        );
        assert_eq!(json.matrix.informational[0].rule_id, "AI-SOME-NOVEL-DEFECT");
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
            json.executive_summary.narrative.contains("1 candidate finding."),
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

    /// P2: the real Typst template must compile cleanly with `fix_for_this_finding` SET
    /// (the new primary-fix branch), alongside a generic `fix` from the corpus — exercising
    /// the "both present" path this pass added, not just the pre-existing "neither present"
    /// path the test above already covers.
    #[tokio::test]
    async fn compile_pdf_renders_fix_specific_and_generic_fix_together() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_renders_fix_specific_and_generic_fix_together: typst not \
                 on PATH"
            );
            return;
        }
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(
            errors.is_empty(),
            "corpus must load cleanly, got errors: {errors:?}"
        );
        let mut f = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            "critical",
        );
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        f.fix_specific = Some(
            "Enable RLS on `profiles` directly (see supabase/migrations/2.sql for the pattern \
             this repo already uses on `orders`)."
                .to_string(),
        );
        let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        assert!(json.curated_findings[0].sites[0]
            .fix_for_this_finding
            .is_some());
        assert!(json.curated_findings[0].sites[0].fix.is_some());
        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed with both fix_for_this_finding and fix set");
        assert!(pdf.starts_with(b"%PDF"));
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

    // ── D6 ceiling interaction — R2 (2026-09-30 cycle-2 queue-hardening) ───────────
    //
    // R2 (`ai_audit::apply_severity_ceiling_rule`) clamps a browser-mediated CORS
    // misconfiguration to exactly Medium. This pins the OUTPUT side here: that Medium buckets
    // into "plan" — never "do_now" — so it can never displace a genuine critical from the top
    // action tier.

    /// A medium-severity CORS finding (the R2 ceiling's landing severity) buckets into "plan" —
    /// never "do_now" — so it can never displace a genuine critical finding from the top tier.
    #[test]
    fn r2_medium_cors_finding_lands_in_plan_not_do_now() {
        let bucket = matrix_bucket(Disposition::Unresolved, "medium", None);
        assert_eq!(
            bucket, "plan",
            "a medium finding (R2's landing severity) belongs in Plan"
        );
        assert_ne!(
            bucket, "do_now",
            "a medium finding must never displace a critical in do_now"
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

    /// P7 end-to-end: N occurrences of one needs-review STRUCTURAL rule, grouped by
    /// `crate::ai_audit::group_structural_needs_review` into ONE finding, land as a SINGLE row
    /// in the informational appendix — never one row per occurrence, and never in a curated
    /// action tier — while a genuine critical finding in the same report is untouched.
    #[test]
    fn grouped_structural_needs_review_finding_is_a_single_informational_row() {
        let mut occ1 = finding("ARCH-SOME-PREFERENCE-1", "a.rs", 10, "medium");
        occ1.category = Some("arch-conformance".to_string());
        occ1.confidence = Some("needs-review".to_string());
        let mut occ2 = finding("ARCH-SOME-PREFERENCE-1", "b.rs", 20, "medium");
        occ2.category = Some("arch-conformance".to_string());
        occ2.confidence = Some("needs-review".to_string());
        let mut occ3 = finding("ARCH-SOME-PREFERENCE-1", "c.rs", 30, "medium");
        occ3.category = Some("arch-conformance".to_string());
        occ3.confidence = Some("needs-review".to_string());
        let critical = finding("SEC-NO-HARDCODED-SECRETS-1", "d.rs", 1, "critical");

        // Run the SAME grouping pass the real scan pipeline runs before build_report, proving
        // this test exercises the actual N-rows-to-one-row behavior, not just is_informational.
        let grouped =
            crate::ai_audit::group_structural_needs_review(vec![occ1, occ2, occ3, critical]);
        assert_eq!(
            grouped.len(),
            2,
            "three structural occurrences + one critical must collapse to 2 findings: {grouped:?}"
        );

        let report = report_with(
            grouped,
            vec!["ARCH-SOME-PREFERENCE-1", "SEC-NO-HARDCODED-SECRETS-1"],
        );
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(
            json.matrix.informational.len(),
            1,
            "the grouped structural finding is ONE informational row, not N: {:?}",
            json.matrix.informational
        );
        assert_eq!(
            json.matrix.informational[0].rule_id,
            "ARCH-SOME-PREFERENCE-1"
        );
        // None of the three original locations leak into an action tier as separate rows.
        for tier in [&json.matrix.do_now, &json.matrix.do_next, &json.matrix.plan] {
            assert!(
                tier.iter().all(|r| r.rule_id != "ARCH-SOME-PREFERENCE-1"),
                "the grouped rule must never appear in a curated action tier: {tier:?}"
            );
        }
        // The critical finding is unaffected — still curated, still do_now.
        assert_eq!(json.matrix.do_now.len(), 1);
        assert_eq!(json.matrix.do_now[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1");
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
        // P6: with neither the per-report field nor the env var set, the resolver falls all
        // the way to the real default name, never an empty string ("Prepared by N/A" is the
        // bug this closes).
        assert_eq!(resolve_prepared_by(&empty, None), DEFAULT_PREPARED_BY);
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
        assert_eq!(NEUTRAL_COVER_TITLE, "Codebase Inspection Report");
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
            "the template must gate on `fix_for_this_finding`'s presence, not just `fix`'s"
        );
    }

    /// P2 (2026-09-29): `fix_for_this_finding` (the codebase-specific fix) must be the
    /// PRIMARY "Fix:" line, with the rule's generic `fix` demoted to a secondary line
    /// UNDER it — never the reverse order this template used before fix-generation existed.
    #[test]
    fn shipped_template_renders_fix_specific_before_the_generic_fix() {
        let template = include_str!("../templates/audit_report.typ");
        let primary_idx = template
            .find(r#"if site.fix_for_this_finding != none ["#)
            .expect("the primary branch must gate on fix_for_this_finding first");
        let fix_label_idx = template[primary_idx..]
            .find(r#"[Fix: ]#site.fix_for_this_finding"#)
            .expect("the PRIMARY Fix: line must render fix_for_this_finding, not fix");
        let secondary_idx = template[primary_idx..]
            .find("General guidance: ")
            .expect("the rule's generic fix must render as secondary \"General guidance\" text");
        assert!(
            fix_label_idx < secondary_idx,
            "fix_for_this_finding's Fix: line must render BEFORE the generic General guidance \
             line"
        );
    }

    /// The fallback path (fix-generation never ran or gave up) must still show the rule's
    /// generic remediation as the ONLY Fix line — never silently drop it.
    #[test]
    fn shipped_template_falls_back_to_the_generic_fix_when_no_fix_specific() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("] else if site.fix != none ["),
            "the fallback branch must still render the generic fix alone when \
             fix_for_this_finding is none"
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

    // ── P5: review-state honesty + reconciling narrative ────────────────────────

    /// Pure reconciliation check, independent of `build_report_json`'s much larger fixture
    /// surface: every candidate finding a scan surfaces lands in exactly one of the four
    /// top-level buckets (curated — itself do_now + do_next + plan + accepted — held for
    /// review, excluded as a false positive, or its own dependency-advisory lane).
    #[allow(clippy::too_many_arguments)]
    fn reconciles(
        candidates: usize,
        curated_total: usize,
        do_now: usize,
        do_next: usize,
        plan: usize,
        accepted: usize,
        held_for_review: usize,
        excluded_false_positive: usize,
        dependency_advisories: usize,
    ) -> bool {
        curated_total == do_now + do_next + plan + accepted
            && candidates
                == curated_total + held_for_review + excluded_false_positive + dependency_advisories
    }

    #[test]
    fn reconciliation_holds_across_fixtures_including_the_41_18_23_0_shape() {
        // (candidates, curated_total, do_now, do_next, plan, accepted, held_for_review,
        //  excluded_false_positive, dependency_advisories)
        #[allow(clippy::type_complexity)]
        let fixtures: &[(
            usize,
            usize,
            usize,
            usize,
            usize,
            usize,
            usize,
            usize,
            usize,
        )] = &[
            // The exact bug shape from the P5 writeup: 41 candidates, 18 curated, 23 held for
            // review (previously invisible), 0 excluded, no dependency advisories in play.
            (41, 18, 5, 6, 7, 0, 23, 0, 0),
            // A reviewed run: false positives excluded, one dependency advisory carved out.
            (10, 6, 2, 1, 2, 1, 0, 3, 1),
            // Zero-finding scan.
            (0, 0, 0, 0, 0, 0, 0, 0, 0),
            // Nothing curated yet, everything held for a human reviewer's judgment call.
            (5, 0, 0, 0, 0, 0, 5, 0, 0),
        ];
        for &(candidates, curated_total, do_now, do_next, plan, accepted, held, excluded, dep) in
            fixtures
        {
            assert!(
                reconciles(
                    candidates,
                    curated_total,
                    do_now,
                    do_next,
                    plan,
                    accepted,
                    held,
                    excluded,
                    dep
                ),
                "fixture failed to reconcile: {:?}",
                (
                    candidates,
                    curated_total,
                    do_now,
                    do_next,
                    plan,
                    accepted,
                    held,
                    excluded,
                    dep
                )
            );
        }
        // Deliberately broken shapes must NOT reconcile — proves the check discriminates
        // rather than vacuously passing everything.
        assert!(
            !reconciles(41, 18, 5, 6, 7, 0, 22, 0, 0),
            "an off-by-one held_for_review must fail to reconcile"
        );
        assert!(
            !reconciles(41, 19, 5, 6, 7, 0, 23, 0, 0),
            "a curated_total that doesn't match its own do_now+do_next+plan+accepted must fail"
        );
    }

    /// A finding the SCAN produced but the AUDITOR has never opened this session (no wire
    /// dispositions at all) must export as `Raw` — the honest starting state.
    #[test]
    fn no_dispositions_at_all_is_raw_review_state() {
        let f = finding("SEC-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.review_state, ReviewState::Raw);
        assert!(json.review_state.is_draft());
    }

    /// `BaselineAccepted` (a PRIOR run's suppression) and `WaivedInline` (an in-code waiver) are
    /// both sourced from `Finding.status`, not this session's triage — see `ReviewState`'s doc
    /// comment. Neither may flip the export to `Reviewed`; nobody looked at this finding THIS
    /// session.
    #[test]
    fn baseline_or_inline_disposition_alone_does_not_flip_review_state_to_reviewed() {
        let mut baseline = finding("SEC-1", "a.rs", 1, "high");
        baseline.status = "suppressed-baseline".to_string();
        let mut inline = finding("SEC-2", "b.rs", 2, "high");
        inline.status = "suppressed-inline".to_string();
        let report = report_with(vec![baseline, inline], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.review_state,
            ReviewState::Raw,
            "a prior-run baseline suppression or an in-code waiver is not THIS session's \
             auditor reviewing anything"
        );
    }

    /// An explicit wire disposition of any recognized kind (`Ignored` / `TechDebt` /
    /// `FalsePositive`) on even ONE finding is enough to flip the whole export to `Reviewed`.
    #[test]
    fn a_single_auditor_disposition_flips_the_whole_export_to_reviewed() {
        let f1 = finding("SEC-1", "a.rs", 1, "critical");
        let f2 = finding("SEC-2", "b.rs", 2, "critical");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f1), wire("Ignored", "defense in depth", ""));
        let report = report_with(vec![f1, f2], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        assert_eq!(json.review_state, ReviewState::Reviewed);
        assert!(!json.review_state.is_draft());
    }

    /// The core P5 raw-export contract: the JSON carries the draft flag, the narrative
    /// reconciles with EVERY number shown (candidates == curated + held + excluded), and the
    /// narrative never claims a human reviewed or dispositioned anything.
    #[test]
    fn raw_export_carries_the_draft_flag_and_a_fully_reconciling_engine_voiced_narrative() {
        let mut f1 = finding("SEC-1", "a.rs", 1, "critical"); // -> do_now, curated
        f1.detail = "The profiles table has no RLS.".to_string();
        let mut f2 = finding("SEC-2", "b.rs", 2, "medium"); // -> needs-review, held
        f2.confidence = Some("needs-review".to_string());
        let report = report_with(vec![f1, f2], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        // The flag.
        assert_eq!(json.review_state, ReviewState::Raw);
        assert!(json.review_state.is_draft());

        // The reconciling numbers, each present on the JSON.
        assert_eq!(json.executive_summary.candidates_reviewed, 2);
        assert_eq!(json.executive_summary.curated_total, 1);
        assert_eq!(json.executive_summary.held_for_review, 1);
        assert_eq!(json.executive_summary.excluded_false_positive, 0);
        assert!(reconciles(
            json.executive_summary.candidates_reviewed,
            json.executive_summary.curated_total,
            json.executive_summary.do_now,
            json.executive_summary.do_next,
            json.executive_summary.plan,
            json.executive_summary.accepted,
            json.executive_summary.held_for_review,
            json.executive_summary.excluded_false_positive,
            json.executive_summary.dependency_advisories,
        ));

        // Every one of those numbers is actually visible in the prose, not just the JSON.
        let n = &json.executive_summary.narrative;
        assert!(n.contains("2 candidate findings"), "{n}");
        assert!(n.contains('1'), "{n}"); // curated_total / held_for_review both literally "1"
        assert!(n.contains("0 were auto-excluded"), "{n}");

        // Never claims a human reviewed or dispositioned anything.
        assert!(!n.contains("reviewed by a human reviewer"), "{n}");
        assert!(!n.contains("dispositioned by a human reviewer"), "{n}");
        assert!(!n.contains("were reviewed"), "{n}");
        assert!(
            !json
                .methodology
                .ai_tier_note
                .contains("reviewed by a human reviewer"),
            "{}",
            json.methodology.ai_tier_note
        );
        assert!(
            !json
                .methodology
                .ai_tier_note
                .contains("dispositioned by a human reviewer"),
            "{}",
            json.methodology.ai_tier_note
        );
    }

    /// The P5 reviewed-export contract: reviewer dispositions actually appear, the narrative
    /// reconciles, no draft flag, and reviewed-voice phrasing IS allowed (because it's true).
    #[test]
    fn reviewed_export_reconciles_in_auditor_voice_with_no_draft_flag() {
        let mut ignored = finding("SEC-1", "a.rs", 1, "critical"); // -> accepted, curated
        ignored.detail = "The profiles table has no RLS.".to_string();
        let mut held = finding("SEC-2", "b.rs", 2, "medium"); // -> needs-review, held
        held.confidence = Some("needs-review".to_string());
        let excluded = finding("SEC-3", "c.rs", 3, "critical"); // -> FalsePositive, excluded

        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&ignored),
            wire("Ignored", "defense in depth only", ""),
        );
        dispositions.insert(
            finding_key(&excluded),
            wire("FalsePositive", "not exploitable", ""),
        );

        let report = report_with(vec![ignored, held, excluded], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(json.review_state, ReviewState::Reviewed);
        assert!(!json.review_state.is_draft());

        assert_eq!(json.executive_summary.candidates_reviewed, 3);
        assert_eq!(json.executive_summary.excluded_false_positive, 1);
        assert_eq!(json.executive_summary.curated_total, 1);
        assert_eq!(json.executive_summary.held_for_review, 1);
        assert_eq!(json.executive_summary.accepted, 1);
        assert!(reconciles(
            json.executive_summary.candidates_reviewed,
            json.executive_summary.curated_total,
            json.executive_summary.do_now,
            json.executive_summary.do_next,
            json.executive_summary.plan,
            json.executive_summary.accepted,
            json.executive_summary.held_for_review,
            json.executive_summary.excluded_false_positive,
            json.executive_summary.dependency_advisories,
        ));

        let n = &json.executive_summary.narrative;
        assert!(
            n.contains("3 candidate findings were reviewed by a human reviewer"),
            "{n}"
        );
        assert!(n.contains("1 was dispositioned as false positives"), "{n}");
        assert!(n.contains("1 accepted as risk"), "{n}");
        assert!(n.contains("held for further review"), "{n}");
        assert!(
            json.methodology
                .ai_tier_note
                .contains("reviewed and dispositioned by a human reviewer"),
            "{}",
            json.methodology.ai_tier_note
        );
    }

    /// Zero-candidate raw scans keep the pre-P5 honest empty-state sentence — no draft-banner
    /// noise added to a run that surfaced nothing at all.
    #[test]
    fn zero_candidates_raw_scan_keeps_the_empty_state_sentence() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.review_state, ReviewState::Raw);
        assert_eq!(
            json.executive_summary.narrative,
            "The scan surfaced no candidate findings to review in this run."
        );
    }

    /// The Typst template's per-page draft banner must be gated on `d.review_state`, and the
    /// literal banner text the design calls for must be present.
    #[test]
    fn shipped_template_gates_the_draft_banner_on_review_state() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("d.review_state == \"raw\""),
            "the draft banner (and its footer echo) must be gated on d.review_state"
        );
        assert!(
            template.contains("DRAFT: not yet reviewed by a human reviewer"),
            "the required banner text must be present verbatim"
        );
        // The gate must live inside `#set page(...)`'s `header:` so it renders on EVERY page,
        // not just the cover.
        let page_start = template.find("#set page(").expect("page setup block");
        let is_draft_def = template
            .find("#let is_draft")
            .expect("is_draft let-binding");
        assert!(
            is_draft_def < page_start,
            "is_draft must be defined before the page setup that reads it in header:"
        );
    }

    /// The Methodology section's own reconciling line (separate literal text from the exec
    /// summary) must ALSO be gated — it used to unconditionally say "... reviewed; ...
    /// dispositioned as false positives by the auditor and excluded" no matter what.
    #[test]
    fn shipped_template_methodology_line_is_gated_on_review_state() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("if d.review_state == \"reviewed\""),
            "the methodology reconciling line must branch on review_state"
        );
        assert!(
            template.contains("has not yet had a human triage pass"),
            "the raw branch must say so explicitly"
        );
        assert!(
            template.contains("held for a human reviewer's judgment call"),
            "the raw branch must surface held_for_review, not just candidates/excluded"
        );
    }

    /// End-to-end smoke test (typst-present-only, mirrors `compile_pdf_produces_a_real_pdf_
    /// when_typst_is_present`): a REVIEWED export (with a real disposition) must still compile
    /// cleanly through the gated template — the conditional banner/methodology logic must not
    /// be a Typst syntax trap that only happens to work on the (more commonly exercised) raw
    /// path.
    #[tokio::test]
    async fn compile_pdf_succeeds_for_a_reviewed_export_with_the_gated_template() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_succeeds_for_a_reviewed_export_with_the_gated_template: \
                 typst not on PATH"
            );
            return;
        }
        let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "critical");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f), wire("Ignored", "test fixture only", ""));
        let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());
        assert_eq!(json.review_state, ReviewState::Reviewed);

        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed for a reviewed export");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── P6 (2026-09-29): cover, branding, product name ──────────────────────────────

    /// The cover title: brand_title (both the branded and neutral forms) must say
    /// "Codebase Inspection", never "Codebase Audit" — see `NEUTRAL_COVER_TITLE` and the
    /// template's `brand_title` binding.
    #[test]
    fn shipped_template_brand_title_says_codebase_inspection_not_audit() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(template.contains(NEUTRAL_COVER_TITLE));
        assert!(template.contains(" - Codebase Inspection"));
        assert!(!template.contains("Codebase Audit"));
    }

    /// The per-page footer keeps "(advisory, not a certification)" verbatim (explicitly
    /// required to survive the rebrand) but the word in front of it is now "inspection".
    #[test]
    fn shipped_template_footer_says_inspection_report_and_keeps_the_advisory_caveat() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(template.contains("inspection report (advisory, not a certification)"));
        assert!(!template.contains("audit report (advisory"));
    }

    /// The draft banner (raw-export only) no longer attributes the pending review to "the
    /// auditor" — client-facing role language is "a human reviewer" throughout.
    #[test]
    fn shipped_template_draft_banner_says_reviewer_not_auditor() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(template.contains("DRAFT: not yet reviewed by a human reviewer"));
        assert!(!template.contains("reviewed by the auditor"));
    }

    /// A signature block exists (promise 1: "one person reads every finding and signs the
    /// report") and it renders the real, resolved preparer name.
    #[test]
    fn shipped_template_has_a_signature_block_naming_the_preparer() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(template.contains("Prepared and signed off by"));
        assert!(template.contains("d.cover.prepared_by"));
    }

    /// Contract test: no CLIENT-FACING string in the shipped template says "audit"/"Audit".
    /// Typst line comments (developer-facing design commentary, never rendered) are stripped
    /// first; the three JSON field-accessor tokens that happen to spell "audit" as part of a
    /// Rust struct field name (`d.cover.audited_refs`, `d.cover.audit_model`,
    /// `row.audited_rules` — internal identifiers, exempt per the plan) are scrubbed before the
    /// word-boundary scan so they don't produce a false positive.
    #[test]
    fn shipped_template_client_facing_text_has_no_audit_word() {
        let template = include_str!("../templates/audit_report.typ");
        let code_only: String = template
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let scrubbed = code_only
            .replace("d.cover.audited_refs", "")
            .replace("d.cover.audit_model", "")
            .replace("row.audited_rules", "");
        let re = regex::Regex::new(r"(?i)\baudit\w*\b").unwrap();
        let hits: Vec<&str> = re.find_iter(&scrubbed).map(|m| m.as_str()).collect();
        assert!(
            hits.is_empty(),
            "client-facing template text still says audit: {hits:?}"
        );
    }

    /// Contract test: none of the authored, client-facing prose this module PRODUCES (the
    /// disclaimer, the next-steps note, both methodology AI-tier notes, both narrative voices,
    /// and the disposition labels) says "audit"/"Audit". Internal identifiers/rule ids are a
    /// separate, non-rendered surface and are exempt.
    #[test]
    fn no_client_facing_authored_string_contains_the_word_audit() {
        let candidates: Vec<String> = vec![
            AUDIT_REPORT_DISCLAIMER.to_string(),
            NEXT_STEPS_NOTE.to_string(),
            ai_tier_note_for(ReviewState::Raw),
            ai_tier_note_for(ReviewState::Reviewed),
            default_narrative(ReviewState::Raw, 10, 3, 1, 1, 1, 0, 1, 0, 0, 0),
            default_narrative(ReviewState::Reviewed, 10, 3, 1, 1, 1, 0, 1, 0, 0, 0),
            disposition_label(Disposition::Ignored, "some reason", "accepted", false),
            disposition_label(Disposition::Ignored, "some reason", "accepted", true),
            disposition_label(Disposition::Unresolved, "", "do_now", false),
        ];
        for s in &candidates {
            assert!(
                !s.to_lowercase().contains("audit"),
                "client-facing string still says audit: {s}"
            );
        }
    }

    /// "Prepared by" must never render blank/"N/A" with default (un-mutated) options — the
    /// resolver's floor is a real name (promise 1).
    #[test]
    fn cover_prepared_by_is_never_blank_with_default_options() {
        let f = finding("SEC-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.cover.prepared_by, DEFAULT_PREPARED_BY);
        assert_ne!(json.cover.prepared_by, "");
    }

    /// Zero real code volume (no local file was ever read this run) must HIDE the cover's
    /// code-volume field entirely — never render a zero.
    #[test]
    fn cover_code_volume_is_none_when_the_scan_has_no_real_line_count() {
        let f = finding("SEC-1", "a.rs", 1, "low");
        let mut report = report_with(vec![f], vec![]);
        report.code_lines = 0;
        report.code_lines_by_language = Vec::new();
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(
            json.cover.code_volume.is_none(),
            "zero code_lines must hide the row, not print a zero"
        );
    }

    /// A real line count produces `Some(CodeVolumeJson)` with the per-language breakdown
    /// carried straight through from the scan (`report_with`'s fixture sets 400 lines split
    /// TypeScript 300 / Rust 100).
    #[test]
    fn cover_code_volume_is_some_with_real_lines_and_language_breakdown_when_known() {
        let f = finding("SEC-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let cv = json
            .cover
            .code_volume
            .expect("a nonzero code_lines must produce Some");
        assert_eq!(cv.lines, 400);
        assert_eq!(cv.by_language.len(), 2);
        assert_eq!(cv.by_language[0].language, "TypeScript");
        assert_eq!(cv.by_language[0].lines, 300);
        assert_eq!(cv.by_language[1].language, "Rust");
        assert_eq!(cv.by_language[1].lines, 100);
    }

    /// The template never has an unconditional "characters"-suffixed render of the raw char
    /// count any more (the bug: "0 characters" always rendered regardless of value) — the row
    /// is now built from the gated `code_volume_rows` array.
    #[test]
    fn shipped_template_never_unconditionally_renders_code_chars_as_characters() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            !template.contains("thousands(d.cover.code_chars) characters"),
            "the old unconditional zero-prone render must be gone"
        );
        assert!(template.contains("code_volume_rows"));
        assert!(template.contains("d.cover.code_volume"));
    }

    /// P6: extends the existing P1 e2e coverage
    /// (`p1_e2e_cover_counts_and_security_finding_survive_post_merge`, which already pins
    /// `cover.stats.critical`) to the high/medium buckets: two raw findings at the exact same
    /// file+line must merge into ONE finding before the cover ever counts severities, so
    /// `cover.stats.high` reads 1, not 2.
    #[tokio::test]
    async fn cover_stats_high_bucket_also_reflects_post_merge_counts() {
        let cors_code = "app.use(cors())";
        let mut det = finding("ARCH-MIDDLEWARE-FIRST-1", "middleware.ts", 12, "high");
        det.snippet = cors_code.to_string();
        let mut sibling = finding("SOME-OTHER-HIGH-RULE", "middleware.ts", 12, "high");
        sibling.snippet = cors_code.to_string();
        let files = vec![("middleware.ts".to_string(), cors_code.to_string())];
        let merged = crate::ai_audit::merge_semantic_groups(
            crate::ai_audit::merge_by_location(vec![det, sibling], &files),
            &files,
        );
        assert_eq!(
            merged.len(),
            1,
            "two raw findings at the same file+line must merge to one before counting"
        );
        let report = report_with(merged, vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(
            json.cover.stats.high, 1,
            "the merged high finding must count once, not twice"
        );
        assert_eq!(json.cover.stats.critical, 0);
        assert_eq!(json.cover.stats.medium, 0);
    }

    /// End-to-end (typst-present-only): a scan with NO real code-volume figure must still
    /// compile cleanly through the template — the `code_volume_rows` conditional is not a
    /// Typst syntax trap on the empty-array branch.
    #[tokio::test]
    async fn compile_pdf_succeeds_with_zero_code_lines_hiding_the_code_volume_row() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_succeeds_with_zero_code_lines_hiding_the_code_volume_row: \
                 typst not on PATH"
            );
            return;
        }
        let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "critical");
        let mut report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
        report.code_lines = 0;
        report.code_lines_by_language = Vec::new();
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(json.cover.code_volume.is_none());

        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed with the code-volume row hidden");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// `sample-report/audit_report.typ` is a PREVIEW COPY of the shipped template (used to
    /// regenerate the sample PDF/PNGs) and must stay byte-identical to it — a drifted copy
    /// means the sample deliverable stops representing what the app actually ships. This
    /// caught real drift as of this pass (the sample copy predated the P2/P6 template edits).
    #[test]
    fn sample_report_template_copy_is_byte_identical_to_the_shipped_template() {
        let shipped = include_str!("../templates/audit_report.typ");
        let sample = include_str!("../../../sample-report/audit_report.typ");
        assert_eq!(
            shipped, sample,
            "sample-report/audit_report.typ has drifted from the shipped template — copy the \
             shipped file over it"
        );
    }
}
