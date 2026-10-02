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

/// C4-R3: zero-width and byte-order-mark code points. None of these carry any visual
/// information in ANY of the three report artifacts (the Typst-rendered PDF, `findings.json`,
/// the xlsx workbook) — their only observable effect is to silently corrupt an otherwise
/// identical-looking string for exact-match search/grep/diff/copy-paste. A scanned repo CAN
/// legitimately contain one of these in a file name or a pasted snippet (adversarial or just
/// odd source encoding); this list is also what the PDF template's own `breakable()` helper
/// must never introduce as a side effect of wrapping a long path (see that function's doc
/// comment in `templates/audit_report.typ`).
const ZERO_WIDTH_CODEPOINTS: [char; 5] = [
    '\u{200B}', // zero width space
    '\u{200C}', // zero width non-joiner
    '\u{200D}', // zero width joiner
    '\u{FEFF}', // BOM / zero width no-break space
    '\u{2060}', // word joiner
];

/// Strip every [`ZERO_WIDTH_CODEPOINTS`] character out of `s`, borrowing unchanged when none
/// are present (the overwhelming common case) so this is free to call defensively.
pub(crate) fn strip_zero_width(s: &str) -> std::borrow::Cow<'_, str> {
    if s.chars().any(|c| ZERO_WIDTH_CODEPOINTS.contains(&c)) {
        std::borrow::Cow::Owned(
            s.chars()
                .filter(|c| !ZERO_WIDTH_CODEPOINTS.contains(c))
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Apply [`strip_zero_width`] to the handful of a [`Finding`]'s free-text fields that flow,
/// verbatim or near-verbatim, into every report artifact: `path` (the PDF's `raw()` path
/// rendering, the xlsx "Location" column, `findings.json`'s `path` field), `snippet` (the PDF's
/// code block, and — for a dependency-advisory finding — the package name), `detail` (the
/// curated-finding body), and every `captures` value (substituted into authored remediation
/// text by [`instantiate_remediation`]). This is the ONE ingestion point both
/// [`build_report_json`] and `xlsx_export::partition_rows` call before doing anything else with
/// a scan's findings, so neither artifact can drift from the other on this invariant, and no
/// future field added to either builder needs its own copy of this defense.
pub(crate) fn sanitize_finding_text(finding: &Finding) -> Finding {
    let mut f = finding.clone();
    f.path = strip_zero_width(&f.path).into_owned();
    f.snippet = strip_zero_width(&f.snippet).into_owned();
    f.detail = strip_zero_width(&f.detail).into_owned();
    for v in f.captures.values_mut() {
        *v = strip_zero_width(v).into_owned();
    }
    f
}

/// Apply [`sanitize_finding_text`] to every finding in `report`, returning an owned clone — the
/// shared ingestion step for [`build_report_json`] and `xlsx_export::partition_rows`. Clones
/// the whole report (not just `findings`) so the caller can shadow its `report: &ScanReport`
/// parameter with the sanitized value and leave every other line in the function unchanged.
pub(crate) fn sanitize_report_findings(report: &ScanReport) -> ScanReport {
    let mut report = report.clone();
    for f in report.findings.iter_mut() {
        *f = sanitize_finding_text(f);
    }
    report
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

/// C4-R1: the pipeline-wide null-location safety net. No finding above `informational` (any
/// normalized severity other than `"info"`) may ship with BOTH an empty `path` AND `line == 0`
/// — that combination means nothing concrete was ever established for the location this
/// finding supposedly cites, which reads as "deterministic rule asserting a falsehood" rather
/// than a real, actionable defect. This was discovered via a Supabase RLS checker over-firing
/// at `critical` with an empty path/line-0 "last established" fact (see
/// `camerata_checks::supabase::rls_checker`'s own C4-R1 fix, which now avoids triggering this
/// at the source by citing a real policy location) — but the invariant is enforced HERE, at the
/// single place every checker's findings funnel through before bucketing, so any OTHER
/// checker/producer that ever ships an unlocated above-informational finding is caught too,
/// not just this one rule.
///
/// Downgrades the SEVERITY only (to `"info"`, so `is_informational` routes it to the
/// `informational` appendix exactly like any other info-tier finding) — never drops the
/// finding. A client should still see "something was flagged, but the scan couldn't pin down
/// where," rather than losing the observation entirely (the over-tell rule, applied here at the
/// pipeline-integrity level rather than a triage-confidence one).
fn downgrade_unlocated_above_informational(severity: String, f: &Finding) -> String {
    if severity != "info" && f.path.is_empty() && f.line == 0 {
        "info".to_string()
    } else {
        severity
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
    /// W6: one explicit sentence per audit pass that FAILED or timed out this run (e.g. the
    /// alternative-recommendation pass hitting the CLI hang timeout) — see
    /// `failed_pass_disclosures`'s doc comment. Empty on the happy path. Rendered in the
    /// summary so a reader never has to infer a gap from an absence; the SAME strings also
    /// render in `MethodologyJson::failed_passes` for the more technical section.
    #[serde(default)]
    pub failed_passes: Vec<String>,
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
    /// C3-6: this finding's own [`effective_bucket`] result (`"do_now"` | `"do_next"` |
    /// `"plan"` | `"accepted"` | `"informational"`) — which `MatrixJson` vec this ref lives
    /// in is already implied by position, but the explicit field lets every consumer
    /// (`xlsx_export`, a cross-artifact test) assert the label directly rather than
    /// re-deriving or inferring it from array membership.
    pub bucket: String,
    /// Calibration-review fix: whether this finding's own text or calibration justification
    /// records an exploit precondition (`ai_audit::mentions_exploit_precondition`) — e.g.
    /// "exploitation requires forging a session cookie". `0` for an unconditional finding,
    /// `1` when a precondition is recorded. Used ONLY as a tie-break in the "do now" ranking
    /// ([`build_report_json`]'s `do_now_sorted`) so an unconditional critical always outranks
    /// a critical whose note records a precondition — never to change severity or drop/
    /// re-bucket the finding itself.
    pub precondition_count: usize,
    /// Secondary tie-break for the "do now" ranking, after severity and
    /// `precondition_count`: `0` for a clear/confident finding (`confidence == Some("high")`,
    /// or a finding calibration never scored at all — the deterministic floor/preview tier,
    /// which was never in doubt to begin with) and `1` for `needs-review`. Lower ranks first.
    pub confidence_rank: u8,
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
    /// C3-6: the SAME [`effective_bucket`] value `disposition`'s label was derived from (see
    /// `disposition_label`'s `bucket` parameter) — carried as its own field so a test (or a
    /// future template revision) can compare this site's bucket against its `FindingRefJson`
    /// counterpart in `matrix` directly, instead of parsing it back out of the label prose.
    pub bucket: String,
    /// C4-P2: this SITE's own resolved citation ([`citation_for_finding`]) — NOT necessarily
    /// identical to the parent [`CuratedGroupJson::citation`] shown once at the group header
    /// (which is that same call applied to the group's FIRST site only). Exists so a consumer
    /// needing the per-finding citation never has to assume group membership implies a shared
    /// citation; the cross-artifact equality gate compares this against `findings.json`'s
    /// `FindingRow::citation_label`/`citation_urls` for the same finding.
    pub citation: CitationJson,
    /// Raw rule ids absorbed into this finding during P1 clustering — an INTERNAL/traceability
    /// field (registry cross-reference, test assertions) never rendered as-is in any
    /// client-facing artifact. See [`also_matches_titles`] for the human-readable form the
    /// template actually renders.
    pub also_matches: Vec<String>,
    /// C4-P4 (residual defect 2): the human-readable TITLES of `also_matches`, resolved via
    /// [`also_matches_titles`] — this is what the PDF's "Also violates:" line renders. Never the
    /// raw ids above; an id with no resolvable corpus title is simply absent here.
    pub also_matches_titles: Vec<String>,
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
    /// at all) or gave up after retries — the template must render `fix` alone in that case,
    /// never a bare "Fix:" label with nothing after it. C3-1b: a fix-generation failure is a
    /// PIPELINE gap, not a confidence signal — it never hedges the finding or moves it out of
    /// `do_now`; `fix` (the rule's own authored remediation) still renders as the primary Fix
    /// line, so the row never ships with no fix at all.
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
    /// W6: same disclosure list as `ExecutiveSummaryJson::failed_passes` — see
    /// `failed_pass_disclosures`'s doc comment. Rendered here too (never JUST in the
    /// summary) so the more technical Methodology section also states explicitly that a pass
    /// did not run, rather than the reader having to notice its absence from the findings.
    #[serde(default)]
    pub failed_passes: Vec<String>,
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

/// W6: turn every [`crate::ai_audit::FailedPass`] this scan recorded into ONE explicit,
/// factual disclosure sentence — e.g. "Rule-alternative recommendations for owner/repo: not
/// computed — pass failed (Claude CLI timed out after 300s...)." Shared verbatim between
/// `ExecutiveSummaryJson::failed_passes` and `MethodologyJson::failed_passes` (see
/// [`build_report_json`]) so a reader hits the same honest statement whether they read the
/// summary or the methodology — never a silent gap in either. Empty input yields an empty
/// list, which both sections render as nothing (no fabricated "everything ran cleanly"
/// claim needed — the absence of a disclosure IS the claim, because every real gap here is
/// always disclosed when one exists).
fn failed_pass_disclosures(failed_passes: &[crate::ai_audit::FailedPass]) -> Vec<String> {
    failed_passes
        .iter()
        .map(|f| {
            let mut pass = f.pass.clone();
            if let Some(first) = pass.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            format!(
                "{pass} for {}: not computed — pass failed ({}).",
                f.repo, f.reason
            )
        })
        .collect()
}

/// FIX 3 (2026-09-13 review) — a short, FACTUAL "what happens next" paragraph, rendered in a
/// new section before Methodology. Deliberately non-pitch: no urgency language. Three factual
/// steps, one paragraph, no em/en dashes (house style for this client deliverable — see
/// `AUDIT_REPORT_DISCLAIMER`'s doc comment).
///
/// C4-P4 (residual defect 4, 2026-10-01): this used to end step one with "at the rate set out
/// in the engagement" — a placeholder that promised a number (the design doc's public promise
/// 2: "the fix rate is written into the report before the client decides") without ever stating
/// one. A real dollar/hour billing rate genuinely isn't this report's to invent (there is no
/// such field anywhere in `ReportOptions`, and fabricating one would be worse than the
/// placeholder it replaces). The number this report CAN state honestly, because every input to
/// it is already computed for the executive-summary/methodology reconciliation a few lines
/// above this one, is the TRIAGE fix rate: the share of this run's candidate findings that were
/// kept (curated, held for review, or accepted risk — anything NOT excluded as a false
/// positive) rather than thrown out as noise. A zero-candidate (clean) run has no fix rate to
/// report; the paragraph says so plainly instead of dividing by zero or rendering a percentage
/// that implies findings existed.
pub(crate) fn next_steps_note(
    candidates_reviewed: usize,
    excluded_false_positive: usize,
) -> String {
    let kept = candidates_reviewed.saturating_sub(excluded_false_positive);
    let fix_rate_sentence = if candidates_reviewed == 0 {
        "This run produced no candidate findings, so there is no fix rate to report.".to_string()
    } else {
        let pct = (kept as f64 / candidates_reviewed as f64) * 100.0;
        let noun = if candidates_reviewed == 1 {
            "finding"
        } else {
            "findings"
        };
        format!(
            "This run kept {kept} of {candidates_reviewed} candidate {noun} as real, \
             actionable items (a {pct:.0}% fix rate), excluding the rest as likely false \
             positives."
        )
    };
    format!(
        "There are three steps from here. First, the do-now items above get fixed, by your own \
         team, an outside contractor, or the reviewing engineer. {fix_rate_sentence} Second, a \
         retest: the reviewing engineer re-scans the repository once the fixes are in and signs \
         a short addendum confirming each item is closed. Third, ongoing coverage: a monthly \
         delta rescan plus on-call architect availability for anything new the codebase \
         introduces between engagements."
    )
}

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

/// Human-readable title for `finding`'s rule (C4-P2). Prefers, in order:
/// 1. the finding's OWN rule id's corpus title — unchanged from every pre-existing call site
///    (a deterministic/preview finding always has a corpus entry, so this is the only branch
///    that ever fires for one);
/// 2. for an AI-tier finding whose own invented rule id has no corpus entry, the GROUNDING
///    rule's corpus title — the same rule [`citation_for_finding`] already borrows a citation
///    from for this finding's class, so the header and the citation underneath it describe the
///    SAME standard rather than a human-readable citation sitting under a header that is
///    nothing but the model's own invented id repeated;
/// 3. the bare rule id — truly unclassifiable, last resort, never fabricated (identical to the
///    pre-existing behavior for this case).
pub(crate) fn title_for_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(&finding.rule_id)) {
        return rule.title.clone();
    }
    if is_ai_tier(finding) {
        if let Some(class) = classify_ai_finding(
            &finding.rule_id,
            finding.category.as_deref(),
            &finding.detail,
        ) {
            if let Some(rule) = corpus.and_then(|c| c.get_by_id(class.grounding_rule_id())) {
                return rule.title.clone();
            }
        }
    }
    finding.rule_id.clone()
}

/// C4-P4 (residual defect 2): human-readable TITLES for a finding's `also_matches` list (other
/// rule ids absorbed into this one during P1 clustering) — NEVER the raw rule ids themselves.
/// Client-facing prose (the PDF curated-site "Also violates" line, the xlsx/`findings.json`
/// "Also matches" column) must never surface an internal rule-id token, which a reader has no
/// way to decode. An id with no resolvable corpus title is DROPPED from the list entirely
/// rather than falling back to the bare id — unlike [`title_for_finding`]'s last-resort bare-id
/// fallback (fine for a finding's OWN primary title, since that's the only identifier left to
/// show at all) applying that same fallback here would just reintroduce the raw-id leak this
/// function exists to close. Shared by both call sites so the PDF and the xlsx/JSON sibling can
/// never render two different label sets for the same underlying ids.
pub(crate) fn also_matches_titles(
    ids: &[String],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<String> {
    ids.iter()
        .filter_map(|id| {
            corpus
                .and_then(|c| c.get_by_id(id))
                .map(|rule| rule.title.clone())
        })
        .collect()
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
/// True when `token` (already stripped of any leading `a:` prefix) has the SHAPE every real
/// placeholder in this corpus uses: lowercase ASCII letters, digits, and hyphens only (`path`,
/// `function-name`, `secret-kind`, …). C4-R3: authored `remediation`/`finding_*` prose
/// occasionally needs to show a literal angle-bracketed EXAMPLE inline — e.g. an XSS rule's
/// "verify a payload like <img src=x onerror=alert(1)> renders as inert text" — and
/// [`instantiate_remediation`] used to treat every `<...>` span as a token to fill, so that
/// literal example's own `<img ...>` markup got swallowed and replaced by the generic filler,
/// rendering the nonsense sentence "a payload like the affected resource renders as inert
/// text." A real token is never spelled with spaces, `=`, or uppercase letters; this check lets
/// [`instantiate_remediation`] tell the two apart and leave a non-token span exactly as the
/// rule author wrote it.
fn is_placeholder_token_shape(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

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
        // Bare noun (no leading article) — see `onboard::audit::classify_secret_kind`'s doc
        // comment (W4, 2026-09-30): this fallback shares that contract so it can never double
        // an article a template already supplies, and can be safely used with `<a:secret-kind>`
        // where the template wants one computed dynamically instead.
        "secret-kind" => "hardcoded credential",
        "gitignore-status" => "not confirmed against this repository's `.gitignore`",
        "history-status" => "not checked against this repository's commit history",
        _ => "the affected resource",
    }
}

/// Acronym-initial letters whose NAME (when read letter-by-letter, as an acronym is) begins
/// with a vowel SOUND even though the letter itself is a consonant: "F" is "ef", "H" is
/// "aitch", "L" is "el", "M" is "em", "N" is "en", "R" is "ar", "S" is "es", "X" is "ex".
/// English indefinite-article choice follows the SOUND, not the letter — "an SSH key", "an
/// FAQ", "an HTML page", "an MRI" are all correct despite S/F/H/M being consonants. Used by
/// [`indefinite_article`].
const VOWEL_SOUND_ACRONYM_INITIALS: &[char] = &['F', 'H', 'L', 'M', 'N', 'R', 'S', 'X'];

/// Choose `"a"` or `"an"` for the noun phrase `phrase`, based on its first word. Handles the
/// two cases the corpus's authored templates actually need: an ordinary word (vowel-LETTER
/// check) and an ALL-CAPS acronym read letter-by-letter (vowel-SOUND check via
/// [`VOWEL_SOUND_ACRONYM_INITIALS`]) — e.g. `"SSH private key"` needs `"an"`, not `"a"`,
/// because "S" is pronounced "ess". Deliberately scoped to this plain technical vocabulary —
/// no dictionary of irregular exceptions (`"hour"`, `"European"`, …) that this corpus's
/// substituted values never produce. See [`instantiate_remediation`]'s `<a:token>` handling,
/// the caller this exists for.
pub(crate) fn indefinite_article(phrase: &str) -> &'static str {
    let first_word = phrase.split_whitespace().next().unwrap_or("");
    let Some(first_char) = first_word.chars().next() else {
        return "a";
    };
    let is_acronym = first_word.len() >= 2 && first_word.chars().all(|c| c.is_ascii_uppercase());
    let starts_with_vowel_letter =
        matches!(first_char.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u');
    // A vowel-LETTER initial sounds like a vowel whether or not the word is an acronym read
    // letter-by-letter ("AWS" -> "ay") — that case is already covered by the plain check. The
    // acronym exception set only needs to add the CONSONANT letters whose own name starts with
    // a vowel sound ("S" -> "ess").
    let vowel_sound = starts_with_vowel_letter
        || (is_acronym && VOWEL_SOUND_ACRONYM_INITIALS.contains(&first_char));
    if vowel_sound {
        "an"
    } else {
        "a"
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
///
/// # `<a:token>` — a dynamically-chosen indefinite article (W4, 2026-09-30)
/// A token spelled `<a:token>` (e.g. `<a:secret-kind>`) resolves `token` exactly as above, then
/// prepends `"a "`/`"an "` (via [`indefinite_article`]) to the resolved value. Every substituted
/// value in this corpus is now a bare noun phrase with NO leading article of its own (see
/// `onboard::audit::classify_secret_kind`'s doc comment) — a template either supplies its own
/// fixed article ahead of an adjective (`"a live <secret-kind>"`, correct regardless of the
/// noun's own sound, since the article agrees with "live") or, when the placeholder opens the
/// clause with nothing of its own in front of it, requests `<a:token>` so the RESOLVED value's
/// own initial sound picks the right article. Before this existed, a template could only
/// hardcode a literal article, which silently DOUBLED whenever the substituted value already
/// carried one of its own (`"a live a vendor credential token"`) — the exact W4 bug class this
/// mechanism removes structurally, for this token and any future one.
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
        let raw_token = after_open[..end].trim();
        let (token, want_article) = match raw_token.strip_prefix("a:") {
            Some(bare) => (bare.trim(), true),
            None => (raw_token, false),
        };
        if !is_placeholder_token_shape(token) {
            // C4-R3: not a real placeholder — emit the original bracketed span verbatim. Running
            // a literal example (HTML markup, a generic-type snippet, …) through the generic
            // filler would replace the author's own words with nonsense; a malformed span gets
            // the identical verbatim treatment just below in the no-closing-bracket branch.
            out.push_str(&rest[start..start + 1 + end + 1]);
            rest = &after_open[end + 1..];
            continue;
        }
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
        let filled = if want_article {
            format!("{} {filled}", indefinite_article(&filled))
        } else {
            filled
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

/// Category for a specific FINDING (C4-P2), preferred over [`category_for`] wherever a
/// `Finding` (not just a bare rule id) is available. An AI-tier finding invents its own
/// rule id per occurrence (`"AI-SQL-INJECTION-7"`), which has no corpus entry to join
/// against — `category_for` alone then falls back to mangling the invented id's own tokens
/// into a category key (`"AI-SQL-INJECTION-7"` -> `"Ai SQL"`), scattering AI findings across
/// nonsense per-finding "categories" instead of grouping them with their real defect family,
/// and leaving the scorecard's `audited_rules` count for that bogus category at 0 (nothing in
/// `provenance.audited_rule_ids` was ever going to share a made-up key).
///
/// The fix: when the finding's own rule id has no corpus entry, prefer its structured
/// `category` field — the closed taxonomy calibration already classified it into (see
/// `ai_audit::apply_verdicts`'s `is_known_category` gate, which only ever writes a trusted
/// value there) — over mangling the rule id. Falls through to the rule-id mangling only when
/// `category` is also absent, so a pre-calibration/deterministic finding with a real corpus
/// entry is completely unaffected (first branch, unchanged from `category_for`).
pub(crate) fn category_for_finding(
    finding: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    if let Some(rule) = corpus.and_then(|c| c.get_by_id(&finding.rule_id)) {
        return prettify_category_key(&rule.domain);
    }
    if let Some(cat) = finding.category.as_deref() {
        if !cat.trim().is_empty() {
            return prettify_category_key(cat);
        }
    }
    prettify_category_key(&finding.rule_id)
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

/// THE single bucket computation for a finding (C3-6). Reused UNMODIFIED by all three export
/// surfaces — `build_report_json`'s matrix pass, that same function's curated-findings
/// disposition label (both in this file), and `xlsx_export::partition_rows`'s own
/// `FindingRow::bucket` — so a finding's bucket is computed exactly once and every artifact
/// renders that one answer. Before this, `matrix_bucket` alone was "the" bucket function in two
/// of those three call sites, with `is_informational`/`is_uncited_ai_finding` applied as a
/// separate caller-side gate ahead of it; the xlsx partition had NO such gate at all, so a
/// hedged (`needs-review`) or uncited-AI-tier row that the PDF/JSON correctly held out of
/// do_now/do_next/plan could still land in an action tier in the workbook — the root cause of
/// the three artifacts disagreeing on bucket counts.
///
/// This is also the fix for "bucket is a pure function of severity": folding the hedge/
/// provenance gates INTO the bucket function itself (rather than a pre-check some callers
/// remembered to apply and one didn't) means a finding's bucket is now genuinely
/// `f(severity, provenance tier, hedge state, disposition, effort)` — a hedged row, or an
/// AI-tier finding with no grounded citation, can route out of an action tier even though its
/// raw severity×effort quadrant alone would have placed it there. See `is_informational` and
/// `is_uncited_ai_finding` for the individual signals folded in here.
pub(crate) fn effective_bucket(
    finding: &Finding,
    disposition: Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
) -> &'static str {
    if is_informational(finding, disposition, severity, corpus, test_file_count)
        || is_uncited_ai_finding(finding, corpus)
    {
        "informational"
    } else {
        matrix_bucket(disposition, severity, finding.effort.as_deref())
    }
}

/// Severity rank for ordering (0 = most severe, ascending). The canonical mapping for any
/// severity-based sort in this module; `xlsx_export::severity_rank` (same crate) delegates
/// here rather than keeping its own copy, so the two artifacts' tie-breaks can never drift.
pub(crate) fn severity_rank(sev: &str) -> u8 {
    match sev {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// Provenance-tier rank for the WITHIN-bucket sort (C3-6, item 3): a deterministic/grounded
/// citation (a published standard, a real linter rule) outranks a scan-time preview tool,
/// which outranks an ungrounded AI-advisory citation — so a security-floor finding (typically
/// `"grounded"`) never prints after an architecture/advisory note (typically `"advisory"`) of
/// the same severity, even though raw severity alone can't tell them apart. 0 sorts first.
/// Takes a [`CitationJson::kind`] string (`"grounded"` | `"preview"` | `"advisory"`); anything
/// else is treated as the lowest tier rather than panicking on an unrecognized value.
pub(crate) fn provenance_tier_rank(citation_kind: &str) -> u8 {
    match citation_kind {
        "grounded" => 0,
        "preview" => 1,
        _ => 2,
    }
}

// C3-1b (`docs/plans/2026-09-30_cycle2-queue-hardening.md`): there used to be a
// `fix_generation_failed` gate here that read a `"[needs review: fix not generated]"` tag
// off `detail` and demoted a would-be `do_now` finding to `do_next` — conflating a PIPELINE
// failure (the fix-specific generation pass gave up) with a CONFIDENCE judgement about the
// finding. `crate::ai_audit::generate_fix_specifics` no longer writes that tag (or sets
// `needs_review`) on failure at all: it only logs to stderr and leaves `fix_specific` at
// `None`, and the row still ships a usable fix because `resolve_fix` (below) renders the
// rule's own authored remediation as `CuratedSiteJson::fix` regardless of whether
// `fix_specific` generated. Bucket placement is therefore driven ONLY by the finding's own
// severity/effort/disposition (`matrix_bucket`) and calibration's own doubt signal
// (`is_informational`'s `confidence == "needs-review"` check) — never by whether a SEPARATE
// AI pass happened to produce prose for it.

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
    // §2c — a low/medium finding the calibrator itself flagged as debatable. Checks BOTH the
    // structured `needs_review` flag and the `confidence` string (C4-P2: upstream calibration
    // passes are each individually responsible for keeping the two paired — see
    // `ai_audit::apply_verdicts` — but reading both here means a finding that somehow reaches
    // this point with only one of the two set is still correctly routed, rather than silently
    // escaping the informational gate on a technicality).
    if finding.needs_review || finding.confidence.as_deref() == Some("needs-review") {
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

/// Whether `f`'s own calibration justification — its `detail` (which carries any
/// `[calibrated: reason]`/`[needs review: reason]` tag `ai_audit::apply_verdicts` appends)
/// plus its `calibration_rationale` (set by the D5/D6 deterministic passes, or by the
/// upward-calibration guardrail itself) — records an exploit precondition. See
/// [`FindingRefJson::precondition_count`]'s doc comment: this is a RANKING tie-break only,
/// never a severity or bucket change.
fn finding_precondition_count(f: &Finding) -> usize {
    let text = format!(
        "{} {}",
        f.detail,
        f.calibration_rationale.as_deref().unwrap_or("")
    );
    usize::from(crate::ai_audit::mentions_exploit_precondition(&text))
}

/// See [`FindingRefJson::confidence_rank`]'s doc comment. Reads BOTH hedge fields (C4-P2 —
/// see [`is_informational`]'s §2c check for why) rather than `confidence` alone.
fn finding_confidence_rank(f: &Finding) -> u8 {
    u8::from(f.needs_review || f.confidence.as_deref() == Some("needs-review"))
}

// ── C4-P2: one hedge source of truth, shared by findings.json / the xlsx workbook / the PDF ──
//
// Bug family (artifact-consistency hardening, see docs/plans for the originating report): the
// executive summary's "held for review" count (`is_informational`'s 4-signal bucket gate),
// the structured `needs_review` boolean, and the `confidence` string could each tell a
// different story for the SAME finding — a row routed to the informational appendix for a
// reason OTHER than calibration doubt (an `info`-severity note, a testing-style deviation with
// no corpus, an absence-type stance rule) rendered with `needs_review: false` /
// `confidence: "high"` in `findings.json`/the workbook, even though the narrative text calls
// that same row "held for a human reviewer's judgment call" — an unhedged row sitting in the
// bucket whose entire definition is "needs a human to decide". Separately, a raw desync
// between `f.needs_review` and `f.confidence` (a verdict re-application, a merge) could make
// the two structured fields themselves disagree.
//
// The fix: ONE function decides whether a finding is hedged for EXPORT purposes, folding in
// BOTH raw calibration doubt (`f.needs_review` / `f.confidence == "needs-review"`) AND bucket
// placement (`bucket == "informational"` — being held for review IS a hedge, by definition of
// the bucket). `build_report_json` (the PDF's `CuratedSiteJson.confidence`) and
// `xlsx_export::partition_rows` (`FindingRow.confidence`/`FindingRow.needs_review`, which
// `findings.json` serializes verbatim) both call this ONE function with their own
// already-computed `bucket` (itself a single shared computation — see `effective_bucket`) —
// never re-deriving hedge state independently. This does NOT change bucket placement (a
// finding explicitly dispositioned out of `Unresolved` still routes via `matrix_bucket`, so a
// calibration-hedged row CAN legitimately sit in `"plan"` rather than `"informational"` — see
// `is_informational`'s "only an `Unresolved` row" gate); it only guarantees that WHEREVER a
// finding lands, its exported confidence/needs_review fields never contradict each other or
// the bucket that put it there.
pub(crate) fn is_hedged(f: &Finding, bucket: &str) -> bool {
    f.needs_review || f.confidence.as_deref() == Some("needs-review") || bucket == "informational"
}

/// The canonical EXPORTED confidence string for `f`, reconciled against [`is_hedged`] — never
/// `"needs-review"` unless `is_hedged` agrees, and never anything OTHER than `"needs-review"`
/// when it does. This is the one producer of the `confidence == "needs-review" ⇔ needs_review`
/// biconditional every artifact renders (`CuratedSiteJson::confidence`, `FindingRow::confidence`
/// / `FindingRow::needs_review`) — see [`is_hedged`]'s doc comment for the full rationale.
///
/// C4-P3: a deterministic-floor / RLS-replay finding never goes through the AI calibration pass
/// (`ai_audit::verify_findings`), so `f.confidence` is `None` for it FOREVER — not "not yet
/// evaluated", genuinely never evaluated, because there is no judgment call left for calibration
/// to make: the mini schema-state replay / content-rule match either holds or it does not. Left
/// as `None`, this prints "Confidence: not evaluated this run" beside a PROVEN critical (a live
/// secret, an unprotected table, a definer function with no pinned search_path) — which reads as
/// "we are not sure" next to exactly the findings a client is paying to act on first. Once a row
/// has survived [`is_hedged`] above (so R1's null-location downgrade, `is_informational`'s
/// absence/needs-review/testing-style checks, and a raw `needs_review` flag have ALL already had
/// their say — see the module-level doc comment on [`is_hedged`]), a `None`-confidence,
/// non-AI-tier row (`!is_ai_tier(f)`, i.e. origin `Deterministic` per `ai_audit::finding_origin`'s
/// own definition of that origin — `AdoptedAi`/`InventedAi` findings carry a real `confidence` or
/// are gated separately) is reported `"high"` BY CONSTRUCTION. This never fires for an AI-tier
/// finding whose confidence is `None` because ITS OWN calibration pass genuinely failed — that
/// stays `None` (the honest "not evaluated this run" gap is real there, since a model verdict was
/// actually expected and never arrived).
pub(crate) fn hedge_confidence(f: &Finding, bucket: &str) -> Option<String> {
    if is_hedged(f, bucket) {
        Some("needs-review".to_string())
    } else if f.confidence.is_none() && !is_ai_tier(f) {
        Some("high".to_string())
    } else {
        f.confidence.clone()
    }
}

#[allow(clippy::too_many_arguments)]
fn finding_ref(
    f: &Finding,
    severity: &str,
    headline: String,
    bucket: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_option: Option<&str>,
) -> FindingRefJson {
    FindingRefJson {
        rule_id: f.rule_id.clone(),
        repo: f.repo.clone(),
        path: f.path.clone(),
        line: f.line,
        severity: severity.to_string(),
        headline,
        // C4-P3: calibration's effort, else the rule's authored band — see `resolve_effort`'s
        // doc comment. Keeps the matrix table AND the "three things this week" box (which reads
        // `FindingRefJson::effort` for its hour estimate / total) in lockstep with
        // `CuratedSiteJson::effort`, so a proven do-now critical never shows an estimate in the
        // curated findings section but "not yet estimated" in the summary box above it.
        effort: resolve_effort(f, &f.rule_id, corpus, chosen_option),
        bucket: bucket.to_string(),
        precondition_count: finding_precondition_count(f),
        confidence_rank: finding_confidence_rank(f),
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
    // C4-P4 (residual defect 1, 2026-10-01): sentence-boundary detection used to be "first '.'
    // anywhere" (falling back through ". ", then a bare trailing '.', then any bare '.'), which
    // cut a headline off mid-abbreviation any time `detail` led with "(e.g. ...)" / "i.e. ..." /
    // "etc." before its real first sentence ended — the period in the abbreviation looked
    // identical to a sentence-ending period to the old scan. `sentence_boundary` below is a
    // REAL sentence-boundary scan: a '.' only counts when it is outside an open `(...)`
    // parenthetical, not immediately preceded by a known abbreviation, and followed by
    // whitespace+an uppercase letter (or end of string). When no such boundary exists at all
    // (a single long run-on sentence with no real ending in sight), the headline is capped by
    // LENGTH with an ellipsis instead of truncating at an arbitrary/wrong character — never an
    // unbounded wall of text standing in for a "headline".
    match sentence_boundary(trimmed) {
        Some(end) => {
            let mut headline = trimmed[..end].trim().to_string();
            if !headline.ends_with('.') {
                headline.push('.');
            }
            headline
        }
        None => {
            if trimmed.chars().count() > HEADLINE_LENGTH_CAP {
                let capped: String = trimmed.chars().take(HEADLINE_LENGTH_CAP).collect();
                format!("{}...", capped.trim_end())
            } else {
                // Short enough to use whole (no ellipsis needed) — same trailing-period
                // normalization as the real-boundary branch above, for a consistent
                // headline-reads-like-a-sentence shape either way.
                let mut headline = trimmed.to_string();
                if !headline.ends_with('.') {
                    headline.push('.');
                }
                headline
            }
        }
    }
}

/// Headline length cap (characters), only ever applied when `detail` has no real sentence
/// boundary at all within that many characters — see `defect_headline`'s doc comment.
const HEADLINE_LENGTH_CAP: usize = 200;

/// Known sentence-internal abbreviations whose own period must never be mistaken for a
/// sentence-ending period, GENERAL across any `detail` text (not keyed to one rule/fixture) —
/// see `defect_headline`'s doc comment for the bug this closes. Matched case-insensitively
/// against the text immediately BEFORE and INCLUDING the candidate period.
const SENTENCE_ABBREVIATIONS: &[&str] = &["e.g.", "i.e.", "etc.", "vs."];

/// Find the end index (exclusive, just past the period) of the first REAL sentence boundary in
/// `s`, or `None` if there is no such boundary anywhere. A `.` is a real boundary only when:
/// it sits outside any open `(...)` parenthetical (so "(e.g. allowing X)" never splits there,
/// parenthetical or not); it is NOT immediately preceded by a known abbreviation
/// (`SENTENCE_ABBREVIATIONS`, so a bare "i.e." / "etc." outside parens doesn't split either);
/// and it is followed by whitespace then an uppercase letter, OR it is the very last character
/// in the string (a clean trailing sentence with nothing after it).
fn sentence_boundary(s: &str) -> Option<usize> {
    let mut paren_depth: i32 = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => paren_depth += 1,
            ')' => paren_depth = (paren_depth - 1).max(0),
            '.' if paren_depth == 0 => {
                let rest_trimmed = s[i + 1..].trim_start();
                let boundary_shaped = rest_trimmed
                    .chars()
                    .next()
                    .map(char::is_uppercase)
                    .unwrap_or(true); // nothing after it at all: a clean trailing sentence.
                if boundary_shaped && !ends_with_known_abbreviation(&s[..=i]) {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn ends_with_known_abbreviation(prefix: &str) -> bool {
    let lower = prefix.to_ascii_lowercase();
    SENTENCE_ABBREVIATIONS.iter().any(|a| lower.ends_with(a))
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

/// C4-P3: the EXPORTED remediation-effort tier for `f`, reconciled against the rule it violates.
/// Order of preference:
/// 1. `f.effort` — the finding's OWN estimate, whether set by the AI calibration pass
///    (`ai_audit::verify_findings`) or by a deterministic detector's own sensible default at
///    finding-creation time (e.g. `onboard::audit::default_effort_for`). Never second-guessed
///    here.
/// 2. Failing that, the rule's AUTHORED effort band on the option this finding resolves against
///    (`RuleOption::effort`, set in the corpus TOML) — the rule author's floor/default estimate
///    for a finding that never went through calibration and whose detector built a bare
///    `Finding` with no per-finding default (the architectural/RLS-replay checkers —
///    `onboard::architectural::arch_violation_to_finding` sets `effort: None` unconditionally).
///    This is the fix for a PROVEN critical (an unprotected table, a definer function with no
///    pinned search_path) rendering "not estimated this run" forever just because its detector
///    never set one: the rule itself already carries a defensible default.
/// 3. `None` — genuinely no estimate available from either source. The caller's existing
///    "not yet estimated" render (`effort_hours_bounds`) and the run-wide `FailedPass{pass:
///    "hour estimation", ..}` disclosure (`ai_audit::verify_findings`) are the honest way to
///    say so; this function never fabricates a number, and — per the C4-P3 guardrail — nothing
///    downstream may ever refuse or drop a row because this returns `None`.
pub(crate) fn resolve_effort(
    f: &Finding,
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_option: Option<&str>,
) -> Option<String> {
    f.effort.clone().or_else(|| {
        corpus
            .and_then(|c| c.get_by_id(rule_id))
            .and_then(|rule| rule.resolved_option(chosen_option))
            .and_then(|opt| opt.effort.clone())
    })
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
    // C4-R3: sanitize zero-width/BOM code points out of every finding ONCE, here, before any
    // downstream section touches `path`/`snippet`/`detail`/`captures` — see
    // `sanitize_report_findings`'s doc comment. Shadows the parameter so every other line below
    // (and every other use of `report` in this function) is unchanged.
    let report = &sanitize_report_findings(report);
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
        // C4-R1: the null-location safety net — see `downgrade_unlocated_above_informational`'s
        // doc comment. Applied here (once, alongside normalization) so every downstream section
        // — matrix bucketing, curated findings, the scorecard — sees the ALREADY-downgraded
        // severity and can never independently re-derive the pre-downgrade one.
        let severity = downgrade_unlocated_above_informational(severity, f);
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
    // C3-6: ONE bucket computation per finding (`effective_bucket`, which now folds in the
    // informational/uncited-AI gates below `matrix_bucket` always needed a caller to apply
    // separately) — buffered per bucket with an explicit (severity, provenance tier, location)
    // sort key rather than pushed straight into `matrix` in iteration order. Ordering within a
    // bucket is now a real signal (a security-floor medium must rank ahead of an architecture/
    // advisory medium of the same severity), so it can no longer be left to insertion order.
    #[allow(clippy::type_complexity)]
    let mut bucketed: HashMap<
        &'static str,
        Vec<((u8, u8, String, String, usize), FindingRefJson)>,
    > = HashMap::new();
    for (f, disposition, _, severity) in &code_findings {
        // Bug 4: convention-to-consider rows are diverted to the informational appendix BEFORE
        // the severity×effort quadrant — they must never reach do_now/do_next/plan. The
        // invariant that a critical/high is never informational lives in `is_informational`.
        // P3: an AI-tier finding with no grounded citation (`is_uncited_ai_finding`) is ALSO
        // routed here — deliberately independent of severity (see that function's doc
        // comment), since the whole point is to catch the critical/high findings that would
        // otherwise sit in the curated set carrying "AI-advisory, model-inferred." Both gates
        // now live INSIDE `effective_bucket` itself (C3-6) — kept as a separate `gate_uncited`
        // bool here only because the appendix headline below needs to say WHICH reason applied.
        let gate_uncited = is_uncited_ai_finding(f, corpus);
        let bucket = effective_bucket(f, *disposition, severity, corpus, report.test_file_count);
        // C4-P2: `title_for_finding` (not a bare corpus-or-rule_id join) so an AI-tier finding
        // grounded via the P3 class fallback gets a human-readable title too, instead of a
        // header that is nothing but the model's own invented rule id repeated.
        let fallback_title = title_for_finding(f, corpus);
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
        // C3-6 item 3: provenance tier for the within-bucket sort, via `citation_for_finding`
        // (not the plain `resolve_citation`) so an AI-tier finding upgraded to a grounded class
        // (P3) sorts by the SAME resolved citation the curated-findings section shows for it.
        let provenance_rank = provenance_tier_rank(&citation_for_finding(f, corpus).kind);
        let sort_key = (
            severity_rank(severity),
            provenance_rank,
            f.repo.clone(),
            f.path.clone(),
            f.line,
        );
        bucketed.entry(bucket).or_default().push((
            sort_key,
            finding_ref(
                f,
                severity,
                headline,
                bucket,
                corpus,
                chosen_option_for_rule,
            ),
        ));
    }
    let mut matrix = MatrixJson::default();
    for (key, target) in [
        ("do_now", &mut matrix.do_now),
        ("do_next", &mut matrix.do_next),
        ("plan", &mut matrix.plan),
        ("accepted", &mut matrix.accepted),
        ("informational", &mut matrix.informational),
    ] {
        if let Some(mut entries) = bucketed.remove(key) {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            *target = entries.into_iter().map(|(_, e)| e).collect();
        }
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
        // `sites` is only ever populated via `.or_default().push(...)` above — a `rule_id` key
        // only exists in `by_rule` because at least one finding was pushed under it, so the
        // group is non-empty by construction.
        let first_site = sites
            .first()
            .expect("a curated-findings group is only built from a non-empty site list")
            .0;
        // C4-P2: the group header's citation/title render next to `sites[0]` specifically (see
        // the template's `render_site(group.sites.at(0), after_headline: [...group.citation...])`)
        // — so they must be THAT SITE's own resolved values, never independently re-derived
        // from the bare `rule_id` or picked from whichever OTHER member of the group happens to
        // classify into a grounded citation first. Every site in `sites` already passed the P3
        // citation gate (not AI-tier, or AI-tier and grounded — see the `continue` above), so
        // `first_site`'s own citation is always already non-advisory (or, for a non-AI-tier
        // group, identical across every member since it depends only on the shared `rule_id`)
        // — this is never a downgrade from the prior "first non-advisory across all members"
        // derivation, just the same answer computed without a cross-member search that could
        // silently diverge from `sites[0]` if a future change weakened the gate above.
        let citation = citation_for_finding(first_site, corpus);
        let title = title_for_finding(first_site, corpus);
        let site_jsons = sites
            .iter()
            .map(|(f, disposition, reason, severity)| {
                // Bug 4 / C3-6: keep the curated-site LABEL in lockstep with the matrix cell
                // this finding actually lands in — an informational row reads "Convention to
                // consider", never "Open (recommended: Plan)". `effective_bucket` is the SAME
                // call the matrix-building pass above makes for this finding, so the two can
                // never compute a different answer for the same row.
                let bucket =
                    effective_bucket(f, *disposition, severity, corpus, report.test_file_count);
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
                    // C4-P3: calibration's own effort, else the rule's authored effort band —
                    // see `resolve_effort`'s doc comment. Never `f.effort.clone()` alone: that
                    // left every architectural/RLS-replay floor finding (no calibration, no
                    // per-finding default) rendering "not estimated this run" even when the
                    // rule itself carries a defensible authored band.
                    effort: resolve_effort(f, &rule_id, corpus, chosen_option_for_rule),
                    // C4-P2: the canonical reconciled confidence (see `hedge_confidence`'s doc
                    // comment) — never the raw `f.confidence` alone, so a row the matrix pass
                    // above bucketed "informational" for a NON-confidence reason (an
                    // `info`-severity note, a testing-style deviation, an absence-type stance
                    // rule) still shows `"needs-review"` here, matching the narrative's own
                    // "held for a human reviewer's judgment call" framing instead of printing
                    // "high"/blank next to a row that is, by construction, not curated.
                    confidence: hedge_confidence(f, bucket),
                    disposition: disposition_label(*disposition, reason, bucket, confirmed_by_client),
                    bucket: bucket.to_string(),
                    also_matches: f.also_matches.clone(),
                    also_matches_titles: also_matches_titles(&f.also_matches, corpus),
                    // C4-P2: this SITE's own resolved citation — never the group's (which
                    // renders once, next to `sites[0]`, in the template) — so a consumer that
                    // wants the per-finding citation (the cross-artifact equality gate, a future
                    // template revision) never has to assume group membership implies an
                    // identical citation for every site.
                    citation: citation_for_finding(f, corpus),
                    headline,
                    fix: resolve_fix(&rule_id, corpus, f, chosen_option_for_rule),
                    // P2: `f.fix_specific` was generated at SCAN time by
                    // `ai_audit::generate_fix_specifics` (this layer stays pure/synchronous —
                    // no model access here, just a read) — a codebase-specific fix that the
                    // template renders ABOVE `fix` (the rule's generic remediation is now
                    // secondary context only). `None` when generation never ran (a
                    // deterministic-only scan) or gave up after retries — in the latter case
                    // `fix` (resolved just above from the rule's own authored remediation)
                    // still renders as the primary "Fix:" line (C3-1b: a pipeline failure here
                    // never removes the finding's fix, only its codebase-specific flavor).
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
        // C4-P2: `category_for_finding` (not the bare rule-id `category_for`) so an AI-tier
        // finding with no corpus entry groups under its real semantic category (e.g.
        // "Rls Policy") instead of a mangled rule-id-derived key — see that function's doc
        // comment for the "0 rules checked" bug this closes.
        by_category
            .entry(category_for_finding(entry.0, corpus))
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
        // C4-P2: "rules checked" for this category is the UNION of the declared-audited set
        // (`audited_in_category` — the deterministic/preview rules Camerata ran against this
        // repo) and every DISTINCT rule id that actually fired a finding here. An AI-tier
        // finding's invented rule id is never in `provenance.audited_rule_ids` (the AI tier
        // doesn't pre-declare which ids it might invent), so without this a category whose
        // only findings are AI-tier reads "0 rules checked" next to a nonzero finding count —
        // a logical impossibility (you cannot find a violation of a rule you never checked).
        // `clean_rules` is deliberately UNCHANGED: "clean" means a DECLARED rule that produced
        // zero findings, which an ad hoc AI-tier id (by definition fired at least once to even
        // exist here) can never be.
        let audited_rules = audited_in_category.len()
            + rule_ids_with_findings
                .iter()
                .filter(|rid| !audited_in_category.contains(rid))
                .count();
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
    // Calibration-review fix: rank by (severity desc, precondition_count asc, confidence) —
    // an unconditional critical must always outrank a critical whose own justification
    // records an exploit precondition (see `FindingRefJson::precondition_count`'s doc
    // comment), with confidence as the final tie-break. `Vec::sort_by_key` is a STABLE sort,
    // so two findings tied on all three keys keep their prior relative order (the severity/
    // provenance/location order the matrix bucket itself was already sorted by) — this is
    // purely a RANKING change, nothing is dropped or re-bucketed.
    let mut do_now_sorted = matrix.do_now.clone();
    do_now_sorted.sort_by_key(|f| {
        (
            severity_rank(&f.severity),
            f.precondition_count,
            f.confidence_rank,
        )
    });
    // FIX 4 (2026-09-13 review): `top3_do_now` still feeds the "Three things this week" box
    // below (the ONE place these findings are now named) — the exec-summary's own
    // `top_do_now` bullet list and blast-radius lead sentence are gone (see
    // `default_narrative`'s doc comment).
    let top3_do_now: Vec<&FindingRefJson> = do_now_sorted.iter().take(3).collect();
    let dependency_advisories = dependency_snapshot.rows.len();
    // W6: never a silent omission — one explicit sentence per pass that failed/timed out
    // this scan, shared verbatim between the summary and the methodology below.
    let failed_pass_notes = failed_pass_disclosures(&report.failed_passes);
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
        failed_passes: failed_pass_notes.clone(),
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
        next_steps: next_steps_note(candidates_reviewed, excluded_fp),
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
        failed_passes: failed_pass_notes,
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
            failed_passes: Vec::new(),
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

    // ── C4-R3 defect 3: zero-width/BOM code points never survive into JSON/xlsx fields ──

    #[test]
    fn strip_zero_width_removes_every_listed_codepoint_and_borrows_when_clean() {
        let dirty = "apps/web/\u{200B}src/\u{FEFF}a.ts\u{200C}\u{200D}\u{2060}";
        let cleaned = strip_zero_width(dirty);
        assert_eq!(cleaned, "apps/web/src/a.ts");
        for c in ZERO_WIDTH_CODEPOINTS {
            assert!(!cleaned.contains(c));
        }
        // The common case (nothing to strip) must not allocate a new string.
        assert!(matches!(
            strip_zero_width("clean/path.rs"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn sanitize_finding_text_strips_zero_width_from_path_snippet_detail_and_captures() {
        let mut f = finding("SEC-1", "apps/\u{200B}web/a.ts", 1, "high");
        f.snippet = "const x = 1\u{FEFF};".to_string();
        f.detail = "secret\u{200D} found".to_string();
        f.captures
            .insert("table".to_string(), "pro\u{200C}files".to_string());

        let cleaned = sanitize_finding_text(&f);
        assert_eq!(cleaned.path, "apps/web/a.ts");
        assert_eq!(cleaned.snippet, "const x = 1;");
        assert_eq!(cleaned.detail, "secret found");
        assert_eq!(cleaned.captures.get("table").unwrap(), "profiles");
    }

    /// `build_report_json` must sanitize BEFORE anything downstream reads `path` — the
    /// JSON this builds is what both the Typst template and `findings.json` ultimately render,
    /// so a zero-width space planted in a scanned repo's own file name (adversarial or just an
    /// odd encoding) must never reach either artifact.
    #[test]
    fn build_report_json_strips_zero_width_characters_from_finding_paths() {
        let f = finding("SEC-1", "apps/\u{200B}web/\u{FEFF}a.ts", 1, "critical");
        let report = report_with(vec![f], vec!["SEC-1"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let serialized = serde_json::to_string(&json).unwrap();
        for c in ZERO_WIDTH_CODEPOINTS {
            assert!(
                !serialized.contains(c),
                "build_report_json's output must never contain U+{:04X}",
                c as u32
            );
        }
        assert!(serialized.contains("apps/web/a.ts"));
    }

    /// Same invariant, the xlsx/`findings.json` path (`xlsx_export::partition_rows`, exercised
    /// here via `build_findings_export`) — a separate implementation from `build_report_json`,
    /// so it needs its own regression rather than relying on the PDF-JSON test above.
    #[test]
    fn build_findings_export_strips_zero_width_characters_from_finding_paths() {
        let f = finding("SEC-1", "apps/\u{200B}web/\u{FEFF}a.ts", 1, "critical");
        let report = report_with(vec![f], vec!["SEC-1"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let export = crate::xlsx_export::build_findings_export(
            &report,
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        let serialized = serde_json::to_string(&export).unwrap();
        for c in ZERO_WIDTH_CODEPOINTS {
            assert!(
                !serialized.contains(c),
                "build_findings_export's output must never contain U+{:04X}",
                c as u32
            );
        }
        assert!(serialized.contains("apps/web/a.ts"));
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

    // ── C4-P4 residual defect 1: headline truncation at an abbreviation (golden tests) ────

    #[test]
    fn defect_headline_does_not_truncate_inside_an_eg_parenthetical() {
        assert_eq!(
            defect_headline(
                "The handler accepts an insecure default (e.g. allowing GET requests) before \
                 validating the session. This is exploitable without authentication.",
                "fallback"
            ),
            "The handler accepts an insecure default (e.g. allowing GET requests) before \
             validating the session."
        );
    }

    #[test]
    fn defect_headline_does_not_truncate_at_a_bare_eg_abbreviation() {
        assert_eq!(
            defect_headline(
                "Error handling is inconsistent, e.g. some handlers return 500 and others \
                 throw. Fix this before shipping.",
                "fallback"
            ),
            "Error handling is inconsistent, e.g. some handlers return 500 and others throw."
        );
    }

    #[test]
    fn defect_headline_does_not_truncate_at_a_bare_ie_abbreviation() {
        assert_eq!(
            defect_headline(
                "This bypasses auth, i.e. anyone can reach the admin panel without a token. \
                 That is unacceptable.",
                "fallback"
            ),
            "This bypasses auth, i.e. anyone can reach the admin panel without a token."
        );
    }

    #[test]
    fn defect_headline_does_not_truncate_at_an_etc_abbreviation() {
        assert_eq!(
            defect_headline(
                "The config accepts unsafe values like eval, exec, etc. without sanitization. \
                 This must be fixed.",
                "fallback"
            ),
            "The config accepts unsafe values like eval, exec, etc. without sanitization."
        );
    }

    #[test]
    fn defect_headline_does_not_truncate_at_a_vs_abbreviation() {
        assert_eq!(
            defect_headline(
                "Credentials compare via string equality vs. constant-time comparison, which \
                 leaks timing info. Rotate the credential now.",
                "fallback"
            ),
            "Credentials compare via string equality vs. constant-time comparison, which leaks \
             timing info."
        );
    }

    #[test]
    fn defect_headline_caps_by_length_with_an_ellipsis_when_no_real_sentence_boundary_exists() {
        let run_on = "a".repeat(250);
        let headline = defect_headline(&run_on, "fallback");
        assert!(
            headline.ends_with("..."),
            "a boundary-less run-on must be capped with an ellipsis: {headline}"
        );
        assert!(
            headline.chars().count() <= HEADLINE_LENGTH_CAP + 3,
            "the capped headline must not exceed the length cap plus the ellipsis: {headline}"
        );
    }

    #[test]
    fn defect_headline_still_splits_on_a_genuine_non_abbreviation_period() {
        // Regression guard: the abbreviation/parenthetical carve-outs above must never swallow
        // a REAL sentence boundary that merely happens to sit near a parenthetical elsewhere in
        // the text.
        assert_eq!(
            defect_headline(
                "The profiles table has no RLS (confirmed via migration replay). Anyone with \
                 the anon key can read and write every row.",
                "fallback"
            ),
            "The profiles table has no RLS (confirmed via migration replay)."
        );
    }

    #[test]
    fn sentence_boundary_skips_an_abbreviation_period_even_before_a_capitalized_continuation() {
        // "e.g." followed by a capitalized word looks EXACTLY like a real sentence boundary
        // (period + space + uppercase letter) to a naive scan — the abbreviation check must
        // still skip it, proving this isn't just the capitalization check doing the work.
        let s = "Error handling is inconsistent, e.g. Some handlers return 500 while others \
                 throw. Fix this before shipping.";
        let end = sentence_boundary(s).expect("a real boundary exists later in the string");
        assert_eq!(
            &s[..end],
            "Error handling is inconsistent, e.g. Some handlers return 500 while others throw."
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

    // ── Calibration-review fix: precondition tie-break in the do-now ranking ──────────
    //
    // The bug this guards: a High "unverified session read" finding whose own note recorded
    // an exploit precondition was promoted to Critical by calibration and took a top-3
    // do-now slot from a genuinely unconditional critical (an unauthenticated export
    // exploitable with one request). `build_report_json`'s `do_now_sorted` must rank an
    // unconditional critical ahead of a critical whose justification records a precondition,
    // even though both are the same severity — pure RANKING, no bucket/severity change.

    #[test]
    fn three_things_ranks_the_unconditional_critical_ahead_of_a_preconditioned_one() {
        // Alphabetically "a.rs" would naturally sort first in the bucket's own (severity,
        // provenance, repo, path, line) ordering — deliberately putting the PRECONDITIONED
        // finding there proves the tie-break actually reorders rather than coincidentally
        // agreeing with path order.
        let mut preconditioned = finding("SEC-PRECONDITIONED", "a.rs", 1, "critical");
        preconditioned.detail =
            "Server code authorizes from an unverified session read; exploitation requires \
             forging a session cookie."
                .to_string();
        let mut unconditional = finding("SEC-UNCONDITIONAL", "b.rs", 2, "critical");
        unconditional.detail =
            "The export endpoint returns every user's records with no authorization check and \
             is reachable with a single unauthenticated request."
                .to_string();
        let report = report_with(vec![preconditioned, unconditional], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.matrix.do_now.len(), 2, "both are critical do-now items");
        assert_eq!(
            json.three_things.items[0].rule_id, "SEC-UNCONDITIONAL",
            "the unconditional critical must rank first, ahead of the preconditioned one: {:?}",
            json.three_things.items
        );
        assert_eq!(json.three_things.items[1].rule_id, "SEC-PRECONDITIONED");
    }

    #[test]
    fn three_things_order_is_unchanged_for_two_unconditional_criticals() {
        let f1 = finding("SEC-FIRST", "a.rs", 1, "critical");
        let f2 = finding("SEC-SECOND", "b.rs", 2, "critical");
        let report = report_with(vec![f1, f2], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.matrix.do_now.len(), 2);
        assert_eq!(
            json.three_things.items[0].rule_id, "SEC-FIRST",
            "with neither finding preconditioned, today's (repo/path/line) order must hold: {:?}",
            json.three_things.items
        );
        assert_eq!(json.three_things.items[1].rule_id, "SEC-SECOND");
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

    // ── C4-P3: deterministic confidence-by-construction + estimate fallback ────────────────
    //
    // Deterministic-floor and RLS-replay rows never go through AI calibration, so `confidence`
    // and `effort` are `None` on them forever unless something else fills them in. Before this
    // pass, a PROVEN critical (a live secret, an unprotected table, a definer function with no
    // pinned search_path) rendered "Confidence: not evaluated this run" / "Effort: not
    // estimated this run" beside it — which reads as doubt next to exactly the findings a
    // client is paying to act on first.

    /// A deterministic finding (a realistic architectural-checker shape: `arch_violation_to_
    /// finding` never sets `confidence` or `effort`) with a REAL location, against a rule whose
    /// corpus TOML carries an authored effort band (`SUPABASE-FUNC-SEARCH-PATH-1`, authored
    /// `effort = "low"` on its default option), must export `confidence == "high"` AND a
    /// non-empty estimate — in `findings.json` (`CuratedSiteJson`), the xlsx `FindingRow`, AND
    /// the compiled PDF's rendered text. Never "not evaluated this run" / "not estimated this
    /// run" next to a proven defect.
    #[tokio::test]
    async fn deterministic_finding_with_real_location_exports_high_confidence_and_an_estimate() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");

        // Exactly the shape `onboard::architectural::arch_violation_to_finding` produces: no
        // calibration ever touches this finding, so `confidence`/`effort` start (and, absent
        // this fix, would stay) `None`.
        let f = finding(
            "SUPABASE-FUNC-SEARCH-PATH-1",
            "supabase/migrations/1.sql",
            8,
            "high",
        );
        assert_eq!(f.confidence, None, "fixture must simulate never-calibrated");
        assert_eq!(f.effort, None, "fixture must simulate never-calibrated");

        let report = report_with(vec![f.clone()], vec!["SUPABASE-FUNC-SEARCH-PATH-1"]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());

        // ── findings.json (CuratedSiteJson) ──
        let site = &json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == "SUPABASE-FUNC-SEARCH-PATH-1")
            .expect("must be curated, not held out")
            .sites[0];
        assert_eq!(
            site.confidence,
            Some("high".to_string()),
            "a deterministic finding with a real location exports confidence 'high' by \
             construction: {:?}",
            site.confidence
        );
        assert_eq!(
            site.effort,
            Some("low".to_string()),
            "must fall back to the rule's authored effort band: {:?}",
            site.effort
        );

        // ── xlsx FindingRow (via the same findings.json export the product route ships) ──
        // `FindingRow`'s fields are private to `xlsx_export` (only the struct is `pub`), so a
        // sibling module inspects it the same way an external consumer of `findings.json`
        // would: through its serialized shape.
        let findings_export = crate::xlsx_export::build_findings_export(
            &report,
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let export_value =
            serde_json::to_value(&findings_export).expect("FindingsExport must serialize");
        let row = export_value["findings"]
            .as_array()
            .expect("findings must be an array")
            .iter()
            .find(|r| r["rule_id"] == "SUPABASE-FUNC-SEARCH-PATH-1")
            .expect("row must be present in the xlsx/findings.json export");
        assert_eq!(row["confidence"], "high", "xlsx confidence must match the PDF/json");
        assert_eq!(row["effort"], "low", "xlsx effort must fall back to the authored band");

        // ── PDF source ──
        if which_typst().is_none() {
            eprintln!(
                "skipping the PDF leg of \
                 deterministic_finding_with_real_location_exports_high_confidence_and_an_estimate: \
                 typst not on PATH"
            );
            return;
        }
        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed");
        let text = pdf_extract::extract_text_from_mem(&pdf)
            .expect("must be able to extract text from the compiled PDF");
        assert!(
            text.contains("Confidence: high"),
            "the PDF must render the deterministic-by-construction confidence: {text:?}"
        );
        assert!(
            !text.contains("Confidence: not evaluated this run"),
            "a proven deterministic finding must never render as unevaluated: {text:?}"
        );
        assert!(
            !text.contains("Effort: not estimated this run"),
            "a deterministic finding with an authored effort band must never render \
             unestimated: {text:?}"
        );
    }

    /// Sequencing proof with R1 (`downgrade_unlocated_above_informational`): a deterministic
    /// finding downgraded to informational because it carries NO real location (empty path,
    /// `line == 0` — the absence-only / platform-owned shape) must NOT get the "high by
    /// construction" treatment. It stays hedged (`"needs-review"`), exactly like any other
    /// informational row — proving the deterministic-confidence rule runs AFTER R1's downgrade
    /// has already had its say, never before it.
    #[test]
    fn deterministic_finding_downgraded_to_informational_is_not_high() {
        let unlocated = finding("SUPABASE-RLS-POLICY-DISABLED-1", "", 0, "critical");
        assert_eq!(unlocated.confidence, None, "never calibrated");
        let report = report_with(vec![unlocated], vec!["SUPABASE-RLS-POLICY-DISABLED-1"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(
            json.matrix.informational.len(),
            1,
            "R1 must still downgrade the unlocated finding to informational: {:?}",
            json.matrix
        );
        let site = &json.curated_findings[0].sites[0];
        assert_eq!(
            site.confidence,
            Some("needs-review".to_string()),
            "an informational deterministic row must stay hedged, never 'high': {:?}",
            site.confidence
        );
    }

    /// An AI-tier finding the calibrator genuinely marked doubtful keeps its calibrated
    /// `"needs-review"` confidence (never overwritten to `"high"` — `is_ai_tier` excludes it from
    /// the deterministic-by-construction path), and its calibrated effort still renders even
    /// though the row is hedged — a needs-review verdict is a confidence judgement, not an
    /// estimate failure.
    #[tokio::test]
    async fn ai_tier_needs_review_row_keeps_its_confidence_and_still_carries_an_estimate() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly, got errors: {errors:?}");
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "high");
        f.confidence = Some("needs-review".to_string());
        f.needs_review = true;
        f.effort = Some("medium".to_string());
        let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        let site = &json.curated_findings[0].sites[0];
        assert_eq!(
            site.confidence,
            Some("needs-review".to_string()),
            "a genuine calibration doubt verdict must be preserved, never promoted to high"
        );
        assert_eq!(
            site.effort,
            Some("medium".to_string()),
            "a hedged row still carries its own calibrated estimate — doubt about the \
             finding is not the same thing as an unestimated fix"
        );
    }

    /// The C4-P3 guardrail: nothing downstream may ever refuse or drop a row because it has no
    /// calibration and no corpus to fall back on — the honest degrade is "not yet estimated",
    /// never an omitted row. A bare deterministic finding with NO corpus at all (so neither a
    /// calibrated effort NOR an authored band is available) still exports, still lands in its
    /// severity-driven bucket, and still carries the deterministic-by-construction confidence.
    #[test]
    fn a_row_with_no_calibration_and_no_corpus_still_exports_unrefused() {
        let f = finding("ARCH-NEVER-CALIBRATED-1", "a.rs", 1, "high");
        assert_eq!(f.confidence, None);
        assert_eq!(f.effort, None);
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(
            json.curated_findings.len(),
            1,
            "a finding with no calibration and no corpus must still be curated, never dropped"
        );
        let site = &json.curated_findings[0].sites[0];
        assert_eq!(
            site.confidence,
            Some("high".to_string()),
            "still gets the deterministic-by-construction confidence even with no corpus"
        );
        assert_eq!(
            site.effort, None,
            "with neither a calibrated effort nor a corpus to fall back on, the row \
             honestly reports no estimate rather than fabricating one"
        );
        // Bucket placement is untouched by any of this — a high-severity finding still lands
        // somewhere actionable, never silently excluded.
        let in_some_action_bucket = json.matrix.do_now.len()
            + json.matrix.do_next.len()
            + json.matrix.plan.len()
            == 1;
        assert!(
            in_some_action_bucket,
            "the row must land in exactly one action bucket, never vanish: {:?}",
            json.matrix
        );
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

    // ── C3-1b: fix-generation failure is a PIPELINE gap, never a confidence hedge ───────

    /// The deterministic finding from the C3-1b spec: its specific fix failed to generate
    /// (`fix_specific: None`, exactly `generate_fix_specifics`' new no-hedge failure state —
    /// `needs_review: false`, `confidence: None`, `detail` carrying no pipeline-state tag).
    /// It must export UNHEDGED (disposition is never demoted), it must NOT be
    /// informational/out-of-bucket, it must still land in `do_now` on its own severity/effort
    /// merits, and it must carry the RULE-level fix (`resolve_fix`'s authored remediation) so
    /// the row still ships a usable fix even with no codebase-specific one.
    ///
    /// C4-P3: `confidence` is NOT `None` here — this finding never went through AI calibration
    /// (`f.confidence` starts `None` and stays that way), so by `hedge_confidence`'s
    /// deterministic-by-construction rule it exports `"high"`. This is the exact fix C4-P3
    /// targets: before it, this SUPABASE-RLS-ENABLED-1 do_now critical rendered "Confidence: not
    /// evaluated this run" next to a proven defect. The "ONLY hedge source is calibration"
    /// invariant this test originally asserted is still true in spirit — a fix-generation
    /// failure still never HEDGES this row (never flips it to `"needs-review"` or
    /// informational) — it just no longer leaves a non-hedged, never-calibrated row blank.
    #[tokio::test]
    async fn fix_generation_failure_exports_unhedged_with_the_rule_level_fix_in_do_now() {
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
        // The exact post-failure state `generate_fix_specifics` now leaves behind: no
        // fix_specific, no hedge, no tag — see that function's tests in ai_audit.rs.
        f.fix_specific = None;
        f.needs_review = false;
        f.confidence = None;

        let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());

        assert_eq!(
            json.matrix.do_now.len(),
            1,
            "a critical finding must stay in do_now even when its specific fix failed to \
             generate: {:?}",
            json.matrix
        );
        assert!(
            json.matrix.informational.is_empty(),
            "a fix-generation failure must never route a finding to the informational \
             appendix: {:?}",
            json.matrix.informational
        );
        let site = &json.curated_findings[0].sites[0];
        assert_eq!(
            site.confidence,
            Some("high".to_string()),
            "C4-P3: a never-calibrated, non-hedged deterministic finding exports confidence \
             'high' by construction, instead of blank — a fix-generation failure must not \
             change that"
        );
        assert_eq!(
            site.fix_for_this_finding, None,
            "no codebase-specific fix was generated"
        );
        assert!(
            site.fix.is_some(),
            "the row must still carry the rule's own authored remediation as its fix: {:?}",
            site.fix
        );
        assert!(
            !site.detail.contains('['),
            "detail must carry no bracketed pipeline-state text: {:?}",
            site.detail
        );
        assert!(
            site.disposition.to_ascii_lowercase().contains("do now"),
            "the site's own disposition label must not be demoted either: {:?}",
            site.disposition
        );
    }

    /// The other half of the C3-1b contract: a finding the CALIBRATOR itself explicitly
    /// flagged doubtful (`confidence: Some("needs-review")`) still exports hedged — proving
    /// confidence hedging is driven by calibration doubt, not by whether a separate AI pass
    /// (fix-generation) happened to succeed.
    #[tokio::test]
    async fn calibrator_flagged_doubt_still_exports_hedged() {
        // Setting `confidence` marks a finding AI-tier (`is_ai_tier`), which routes an
        // uncited AI finding to the informational appendix and OUT of `curated_findings`
        // entirely (P3's citation gate) — a real corpus rule keeps this finding grounded so
        // the test exercises the hedge itself, not that unrelated gate.
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
            "high",
        );
        f.confidence = Some("needs-review".to_string());
        f.needs_review = true;
        let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        assert_eq!(
            json.curated_findings[0].sites[0].confidence,
            Some("needs-review".to_string()),
            "a genuine calibration doubt verdict must still render as hedged"
        );
    }

    #[test]
    fn a_critical_finding_with_a_valid_fix_still_lands_in_do_now() {
        // A critical finding whose fix-generation ran and succeeded behaves exactly as one
        // that never went through fix-generation at all (e.g. a deterministic-only scan) —
        // bucket placement never depended on `fix_specific` being present.
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

    // ── C4-R3 defect 4: a literal angle-bracketed EXAMPLE in authored prose is not a token ──

    /// General regression for the "a payload like the affected resource renders as inert text"
    /// bug: `instantiate_remediation` used to treat EVERY `<...>` span as a placeholder to
    /// fill, so an authored sentence showing a literal HTML/markup example got its own example
    /// swallowed by the generic filler. A real placeholder token is always a bare
    /// lowercase-and-hyphens identifier (`<path>`, `<function-name>`, `<a:secret-kind>`, …); a
    /// span containing spaces, `=`, or uppercase letters is prose, not a token, and must render
    /// completely unchanged.
    #[test]
    fn instantiate_remediation_treats_a_literal_bracketed_example_as_prose_not_a_placeholder() {
        let captures = std::collections::BTreeMap::new();
        let text = instantiate_remediation(
            "Verify by confirming a payload like <img src=x onerror=alert(1)> renders as \
             inert text or is stripped, never executes.",
            "a.tsx",
            &captures,
        );
        assert_eq!(
            text,
            "Verify by confirming a payload like <img src=x onerror=alert(1)> renders as \
             inert text or is stripped, never executes."
        );
        assert!(
            !text.contains("the affected resource"),
            "a literal example must never be replaced by the generic placeholder filler: {text:?}"
        );
    }

    /// A real, known placeholder token sitting right next to a literal bracketed example in the
    /// SAME sentence must still be filled normally — the token-shape check must not become
    /// overly broad and start treating legitimate tokens as prose too.
    #[test]
    fn instantiate_remediation_still_fills_a_real_token_alongside_a_literal_example() {
        let mut captures = std::collections::BTreeMap::new();
        captures.insert("table".to_string(), "profiles".to_string());
        let text = instantiate_remediation(
            "Enable RLS on <table>; an example payload like <SELECT 1> must then fail.",
            "a.sql",
            &captures,
        );
        assert_eq!(
            text,
            "Enable RLS on profiles; an example payload like <SELECT 1> must then fail."
        );
    }

    #[test]
    fn is_placeholder_token_shape_accepts_only_lowercase_hyphenated_identifiers() {
        assert!(is_placeholder_token_shape("path"));
        assert!(is_placeholder_token_shape("function-name"));
        assert!(is_placeholder_token_shape("secret-kind"));
        assert!(!is_placeholder_token_shape(""));
        assert!(!is_placeholder_token_shape("img src=x onerror=alert(1)"));
        assert!(!is_placeholder_token_shape("SELECT 1"));
        assert!(!is_placeholder_token_shape("Entity"));
        assert!(!is_placeholder_token_shape("T, E"));
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

    // ── W4 (2026-09-30): headline-rewriter grammar bugs ─────────────────────────────
    //
    // Doubled article ("a live a <vendor> secret key") and a self-referential substitution
    // ("a live .env file that is committed to this repository" — the file described as
    // containing itself). Both were template/variable-substitution bugs: every value
    // `classify_secret_kind` (onboard::audit) returns is now a BARE noun phrase, and
    // `instantiate_remediation`'s `<a:token>` syntax computes the correct article at
    // substitution time via `indefinite_article` rather than a template hardcoding one that
    // can collide with a value that already carries its own.

    #[test]
    fn indefinite_article_uses_the_vowel_letter_for_an_ordinary_word() {
        assert_eq!(indefinite_article("vendor credential token"), "a");
        assert_eq!(indefinite_article("Anthropic API key"), "an");
        assert_eq!(indefinite_article("Java key store"), "a");
    }

    #[test]
    fn indefinite_article_uses_the_vowel_sound_for_an_acronym_read_letter_by_letter() {
        // "SSH" is pronounced "ess-ess-aitch" — a vowel SOUND despite "S" being a consonant
        // LETTER. A naive first-letter check would wrongly pick "a".
        assert_eq!(indefinite_article("SSH private key"), "an");
        // "AWS" ("ay-double-u-ess") already starts on a vowel LETTER too, so both checks agree.
        assert_eq!(indefinite_article("AWS access key"), "an");
        // "PKCS#12 key store" — acronym, but "P" is not in the vowel-sound exception set.
        assert_eq!(indefinite_article("PKCS#12 key store"), "a");
    }

    #[test]
    fn instantiate_remediation_a_token_prepends_the_correct_article_without_doubling() {
        let mut captures = std::collections::BTreeMap::new();
        captures.insert("secret-kind".to_string(), "live `.env` file".to_string());
        let text = instantiate_remediation(
            "Your `<path>` file is <a:secret-kind> committed to this repository.",
            ".env",
            &captures,
        );
        assert_eq!(
            text,
            "Your `.env` file is a live `.env` file committed to this repository."
        );
        assert!(!text.contains("a a") && !text.contains("an a") && !text.contains("a an"));
    }

    #[test]
    fn instantiate_remediation_a_token_falls_back_to_the_generic_bare_noun() {
        // No "secret-kind" capture at all — `<a:secret-kind>` must still resolve through the
        // generic fallback (also a bare noun, per its own doc comment) and prepend one article,
        // never leave a raw token or double up.
        let captures = std::collections::BTreeMap::new();
        let text = instantiate_remediation("Found <a:secret-kind> here.", "a.py", &captures);
        assert_eq!(text, "Found a hardcoded credential here.");
    }

    /// Golden-string, secret-FILE case (the self-referential bug's original shape): a real
    /// `.env` committed to the repo. The old template read "…contains a live `.env` file that
    /// is committed to this repository" — describing the file as containing itself. Pinned to
    /// the exact rewritten sentence so a future edit can't reintroduce either bug silently.
    #[tokio::test]
    async fn w4_golden_secret_file_headline_has_no_doubled_article_and_is_not_self_referential() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let files = vec![(".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string())];
        let findings = crate::onboard::audit_files("owner/repo", &files);
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-SECRET-FILE-1")
            .expect("a real committed .env must fire SEC-NO-SECRET-FILE-1")
            .clone();
        let (headline, _detail) =
            resolve_floor_finding_text("SEC-NO-SECRET-FILE-1", Some(&corpus), &f, None)
                .expect("SEC-NO-SECRET-FILE-1 must have an authored floor template");
        assert_eq!(
            headline,
            "Your `.env` file is a live `.env` file committed to this repository."
        );
        for doubled in ["a a ", "an a ", "a an ", "a live a ", "an an "] {
            assert!(
                !headline.contains(doubled),
                "doubled article {doubled:?} in {headline:?}"
            );
        }
        assert!(
            !headline.contains("contains") || !headline.contains("that is committed"),
            "must not describe the file as containing a committed copy of itself: {headline:?}"
        );
    }

    /// Golden-string, vendor-TOKEN case (the doubled-article bug's original shape): a
    /// hardcoded AWS access key. The old template read "…contains a live an AWS access key"
    /// (`"a live <secret-kind>"` with a secret-kind value that already carried its own
    /// article) — pinned to the exact rewritten sentence.
    #[tokio::test]
    async fn w4_golden_vendor_token_headline_has_no_doubled_article() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let files = vec![(
            "config.py".to_string(),
            concat!("aws_key = \"AK", "IAABCDEFGHIJKLMNOP\"\n").to_string(),
        )];
        let findings = crate::onboard::audit_files("owner/repo", &files);
        let f = findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-VENDOR-TOKEN-1")
            .expect("a hardcoded AWS key must fire SEC-NO-VENDOR-TOKEN-1")
            .clone();
        let (headline, _detail) =
            resolve_floor_finding_text("SEC-NO-VENDOR-TOKEN-1", Some(&corpus), &f, None)
                .expect("SEC-NO-VENDOR-TOKEN-1 must have an authored floor template");
        assert_eq!(
            headline,
            "Your `config.py` file contains a live AWS access key."
        );
        for doubled in ["a a ", "an a ", "a an ", "a live a ", "a live an "] {
            assert!(
                !headline.contains(doubled),
                "doubled article {doubled:?} in {headline:?}"
            );
        }
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

    // ── C4-R3: four renderer defects, verified against the REAL compiled PDF's extracted
    // text (not just "it compiled") — a smoke check that only asserts `%PDF` magic bytes is
    // blind to exactly these bugs (a doubled banner, a stray "None", an embedded U+200B, an
    // unfilled placeholder are all still a valid, compiling PDF). `pdf_extract` gives these
    // tests the same read on the artifact a human (or a grep/copy-paste workflow) would get.

    /// The long path shared by every C4-R3 test below: long enough (well past `breakable()`'s
    /// default 40-char chunk size) to force a wrap in both the curated-finding site line and
    /// the narrower severity×effort grid cell.
    const C4_R3_LONG_PATH: &str = "apps/web/src/components/very/deeply/nested/directory/\
        structure/that/goes/on/ForeverLongComponentFileName.tsx";

    /// Builds a two-finding, multi-page RAW report that exercises all four C4-R3 defects at
    /// once: a long path (zero-width-space wrap bug), an empty severity×effort cell (the
    /// "None" bug — this report deliberately has findings in only ONE of the two
    /// severity/effort combinations it surfaces, so the other cell in the grid is empty), the
    /// real corpus's `SEC-NO-UNSAFE-HTML-SINK-1` remediation (the unfilled-placeholder bug,
    /// whose authored text contains a literal `<img ...>` HTML example), and a RAW
    /// (unreviewed) review state spanning several pages (the draft-banner-doubling bug).
    async fn c4_r3_report_and_pdf() -> (AuditReportJson, Vec<u8>) {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        let mut long_path_finding =
            finding("SEC-NO-UNSAFE-HTML-SINK-1", C4_R3_LONG_PATH, 42, "critical");
        long_path_finding.effort = Some("low".to_string());
        long_path_finding.confidence = Some("high".to_string());

        let mut other_finding = finding("SEC-NO-HARDCODED-SECRETS-1", "b.rs", 1, "low");
        other_finding.effort = Some("high".to_string());

        let report = report_with(
            vec![long_path_finding, other_finding],
            vec!["SEC-NO-UNSAFE-HTML-SINK-1", "SEC-NO-HARDCODED-SECRETS-1"],
        );
        let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
        assert_eq!(
            json.review_state,
            ReviewState::Raw,
            "report must be RAW to exercise the draft banner"
        );
        assert!(
            json.priority_grid
                .rows
                .iter()
                .any(|r| r.cells.iter().any(|c| c.findings.is_empty())),
            "the grid must contain at least one empty cell to exercise the 'None' bug"
        );

        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed for the C4-R3 regression report");
        (json, pdf)
    }

    /// Defect 1: the per-page draft banner must render EXACTLY ONCE per page — never doubled
    /// into "DRAFTDRAFT: ..." by a redundant short echo elsewhere on the same or an adjacent
    /// page. Every occurrence of the bare word "DRAFT" anywhere in the document must be part
    /// of the one full banner sentence; a standalone "DRAFT" (the old footer echo) is exactly
    /// what let it collide with the next page's banner.
    #[tokio::test]
    async fn compile_pdf_renders_the_draft_banner_exactly_once_never_doubled() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_renders_the_draft_banner_exactly_once_never_doubled: \
                 typst not on PATH"
            );
            return;
        }
        let (_, pdf) = c4_r3_report_and_pdf().await;
        let text = pdf_extract::extract_text_from_mem(&pdf)
            .expect("must be able to extract text from the compiled PDF");
        assert!(
            !text.contains("DRAFTDRAFT"),
            "the draft banner must never render doubled: {text:?}"
        );
        let bare_draft_count = text.matches("DRAFT").count();
        let full_banner_count = text
            .matches("DRAFT: not yet reviewed by a human reviewer")
            .count();
        assert!(
            full_banner_count >= 1,
            "the draft banner must render at least once: {text:?}"
        );
        assert_eq!(
            bare_draft_count, full_banner_count,
            "every occurrence of the word DRAFT must be part of the one full banner sentence, \
             never a standalone echo elsewhere on the page: {text:?}"
        );
    }

    /// Defect 2: an empty severity×effort matrix cell must render as blank — never the
    /// literal string "None" (a Rust `Option`/Typst `none` stringified instead of omitted) or
    /// "null". The whole document is checked, not just the matrix section, since neither
    /// string is legitimate ANYWHERE in this report's authored prose.
    #[tokio::test]
    async fn compile_pdf_never_renders_the_literal_none_for_an_empty_matrix_cell() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_never_renders_the_literal_none_for_an_empty_matrix_cell: \
                 typst not on PATH"
            );
            return;
        }
        let (_, pdf) = c4_r3_report_and_pdf().await;
        let text = pdf_extract::extract_text_from_mem(&pdf)
            .expect("must be able to extract text from the compiled PDF");
        assert!(
            !text.contains("None"),
            "no cell may render the literal word None: {text:?}"
        );
        assert!(
            !text.contains("null"),
            "no cell may render the literal word null: {text:?}"
        );
    }

    /// Defect 3: wrapping a long path/snippet/package must never splice a zero-width space (or
    /// any other zero-width/BOM code point) into the rendered text — that invisible character
    /// used to survive into the PDF's copy/search/grep layer and corrupt an otherwise-exact
    /// path. The long path in `c4_r3_report_and_pdf` is long enough to force a wrap.
    #[tokio::test]
    async fn compile_pdf_never_embeds_a_zero_width_character_when_wrapping_a_long_path() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_never_embeds_a_zero_width_character_when_wrapping_a_long_path: \
                 typst not on PATH"
            );
            return;
        }
        let (_, pdf) = c4_r3_report_and_pdf().await;
        let text = pdf_extract::extract_text_from_mem(&pdf)
            .expect("must be able to extract text from the compiled PDF");
        for c in ZERO_WIDTH_CODEPOINTS {
            assert!(
                !text.contains(c),
                "rendered text must never contain U+{:04X}: {text:?}",
                c as u32
            );
        }
        // A genuine visual line wrap is expected to introduce ORDINARY whitespace at the break
        // point (a real PDF reader/extractor represents any two-line text that way — that's not
        // the bug) — collapsing all whitespace must still reconstruct the exact original path
        // with NOTHING else spliced in. This is the real discriminator from the old bug: the
        // zero-width space survived even collapsing whitespace, because it is not whitespace.
        let collapsed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            collapsed.contains(C4_R3_LONG_PATH),
            "wrapping the long path must never introduce anything other than incidental \
             whitespace at the break point: reconstructed={collapsed:?}"
        );
    }

    /// Defect 4: an authored remediation's literal angle-bracketed EXAMPLE (the real corpus's
    /// `SEC-NO-UNSAFE-HTML-SINK-1` remediation shows `<img src=x onerror=alert(1)>` as prose,
    /// not a placeholder) must render verbatim — never swallowed by the generic-placeholder
    /// filler into a nonsense sentence like "a payload like the affected resource renders as
    /// inert text".
    #[tokio::test]
    async fn compile_pdf_never_swallows_a_literal_html_example_into_a_generic_placeholder() {
        if which_typst().is_none() {
            eprintln!(
                "skipping \
                 compile_pdf_never_swallows_a_literal_html_example_into_a_generic_placeholder: \
                 typst not on PATH"
            );
            return;
        }
        let (json, pdf) = c4_r3_report_and_pdf().await;
        assert!(
            json.curated_findings
                .iter()
                .any(|g| g.rule_id == "SEC-NO-UNSAFE-HTML-SINK-1"),
            "the HTML-sink finding must survive triage into curated findings"
        );
        let text = pdf_extract::extract_text_from_mem(&pdf)
            .expect("must be able to extract text from the compiled PDF");
        assert!(
            !text.contains("a payload like the affected resource"),
            "the literal <img ...> example must not be replaced by the generic filler: {text:?}"
        );
        assert!(
            text.contains("payload like") && text.contains("renders as inert text"),
            "the authored remediation sentence must still render: {text:?}"
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

    /// W6: the real Typst template must compile cleanly with a non-empty `failed_passes` list
    /// — exercising the new disclosure block in BOTH the summary and the methodology section,
    /// not just the gated-off "empty list" path every other `compile_pdf_*` test above covers.
    #[tokio::test]
    async fn compile_pdf_renders_the_failed_pass_disclosure_block() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_renders_the_failed_pass_disclosure_block: typst not on PATH"
            );
            return;
        }
        let mut report = report_with(
            vec![finding(
                "SEC-NO-HARDCODED-SECRETS-1",
                "src/a.rs",
                10,
                "critical",
            )],
            vec!["SEC-NO-HARDCODED-SECRETS-1"],
        );
        report.failed_passes = vec![crate::ai_audit::FailedPass {
            repo: "acme/widgets".to_string(),
            pass: "rule-alternative recommendations".to_string(),
            reason: "Claude CLI timed out after 300s (no output).".to_string(),
        }];
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.executive_summary.failed_passes.len(), 1);
        assert_eq!(json.methodology.failed_passes.len(), 1);

        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed with a failed-pass disclosure present");
        assert!(pdf.starts_with(b"%PDF"));
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

    // ── D6 ceiling interaction (2026-09-30 cycle-2 queue-hardening) ────────────────
    //
    // R1 (`ai_audit::apply_severity_ceiling_rule`) re-routes a self-hedged finding to
    // needs-review + Low severity; R2 clamps a browser-mediated CORS misconfiguration to
    // exactly Medium. These pin the OUTPUT side here: that the R1 shape is what
    // `is_informational` keys on, and that R2's medium landing spot buckets into "plan" —
    // never "do_now" — so it can never displace a genuine critical from the top action tier.

    /// An R1-shaped finding (severity capped to Low, confidence flagged needs-review by the D6
    /// ceiling) routes to the informational appendix, out of every action bucket.
    #[test]
    fn r1_shaped_low_needs_review_finding_is_informational() {
        let mut f = finding("AI-HEDGE-1", "a.rs", 1, "low");
        f.confidence = Some("needs-review".to_string());
        assert!(
            is_informational(&f, Disposition::Unresolved, "low", None, 0),
            "an R1-shaped (low + needs-review) finding must route to the informational appendix"
        );
    }

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

    /// C4-R1: the pipeline-wide null-location safety net. A synthetic `critical` finding with
    /// an EMPTY path and `line == 0` — the exact shape the Supabase RLS over-firing regression
    /// produced (a deterministic rule asserting a falsehood with no real evidence behind it) —
    /// must be DOWNGRADED to `info` (never dropped) by `build_report_json`, landing in the
    /// `informational` appendix rather than `do_now`. A normally-located critical finding in
    /// the SAME report is completely unaffected, proving this is a location-shaped gate, not a
    /// blanket demotion of the rule or severity.
    #[test]
    fn build_report_json_downgrades_an_unlocated_above_informational_finding_but_never_drops_it() {
        let located_critical = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 10, "critical");
        let unlocated_critical = finding("SUPABASE-RLS-POLICY-DISABLED-1", "", 0, "critical");
        let report = report_with(
            vec![located_critical, unlocated_critical],
            vec![
                "SEC-NO-HARDCODED-SECRETS-1",
                "SUPABASE-RLS-POLICY-DISABLED-1",
            ],
        );
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        // Still present — downgraded, not dropped.
        assert_eq!(
            json.matrix.informational.len(),
            1,
            "the unlocated critical must still ship, re-bucketed to informational: {:?}",
            json.matrix.informational
        );
        assert_eq!(
            json.matrix.informational[0].rule_id,
            "SUPABASE-RLS-POLICY-DISABLED-1"
        );
        assert_eq!(
            json.matrix.informational[0].severity, "info",
            "the empty-path/line-0 finding's severity must be downgraded to info"
        );

        // The normally-located critical is completely unaffected — this is a location-shaped
        // gate, not a general severity cap.
        assert_eq!(
            json.matrix.do_now.len(),
            1,
            "the located critical stays an action item"
        );
        assert_eq!(json.matrix.do_now[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1");
        assert_eq!(json.matrix.do_now[0].severity, "critical");

        // No action tier may ever contain the unlocated finding.
        for tier in [&json.matrix.do_now, &json.matrix.do_next, &json.matrix.plan] {
            assert!(
                tier.iter()
                    .all(|r| r.rule_id != "SUPABASE-RLS-POLICY-DISABLED-1"),
                "the unlocated finding must never land in an action tier: {tier:?}"
            );
        }
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

    // ── C3-6: one bucket computation, read consistently everywhere in the JSON ─────────

    /// `FindingRefJson::bucket` (the matrix entry) and `CuratedSiteJson::bucket` (the SAME
    /// finding's curated-findings row) must read the SAME value — both come from exactly one
    /// `effective_bucket` call per finding now (previously two independent `is_informational`
    /// + `matrix_bucket` call sites in this file, which happened to agree today but had no
    /// structural guarantee against drifting apart). A hedged (`needs-review`) finding is the
    /// sharpest case: it must read `"informational"` in BOTH places, never an action bucket —
    /// and it still ships (over-tell, never dropped), just consistently re-bucketed.
    #[test]
    fn matrix_and_curated_site_bucket_fields_agree_and_a_hedged_finding_is_never_an_action_bucket()
    {
        let mut hedged = finding("SOME-HEDGED-RULE-1", "a.rs", 1, "medium");
        hedged.confidence = Some("needs-review".to_string());
        // A non-`None` `confidence` makes `is_ai_tier` true (it treats any calibrated finding
        // as AI-tier — see that function's doc comment); without a preview tool or grounded
        // citation, the SEPARATE P3 "uncited AI finding" gate would exclude this row from
        // `curated_findings` entirely rather than routing it to the informational appendix.
        // Giving it a preview tool keeps its citation `"preview"` (not `"advisory"`), isolating
        // THIS test to the hedge/confidence signal this test is actually about.
        hedged.preview_tool = Some("clippy".to_string());
        let report = report_with(vec![hedged], vec!["SOME-HEDGED-RULE-1"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.matrix.informational.len(), 1);
        assert_eq!(json.matrix.informational[0].bucket, "informational");
        for tier in [&json.matrix.do_now, &json.matrix.do_next] {
            assert!(
                tier.is_empty(),
                "a hedged finding must never land in an action bucket: {tier:?}"
            );
        }

        let group = json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == "SOME-HEDGED-RULE-1")
            .expect("the hedged finding still ships in curated_findings, as an informational row");
        assert_eq!(group.sites.len(), 1, "the finding must not be dropped");
        assert_eq!(
            group.sites[0].bucket, "informational",
            "the curated site's own bucket field must read informational too"
        );
        assert_eq!(
            group.sites[0].bucket, json.matrix.informational[0].bucket,
            "matrix and curated-site bucket fields must never disagree for the same finding"
        );
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

    // ── FIX 3 / C4-P4 residual defect 4: Next steps + the fix rate ─────────────────

    #[test]
    fn methodology_next_steps_reports_no_fix_rate_for_a_zero_candidate_run() {
        let report = report_with(vec![], vec![]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert!(!json.methodology.next_steps.is_empty());
        assert!(
            json.methodology
                .next_steps
                .contains("no candidate findings, so there is no fix rate to report"),
            "a clean run must honestly say there is no fix rate, never fabricate one or divide \
             by zero: {}",
            json.methodology.next_steps
        );
        assert!(!json.methodology.next_steps.contains('%'));
    }

    /// Public promise 2 ("the fix rate is written into the report before the client decides"):
    /// the next-steps paragraph must contain a real NUMBER, never the old placeholder sentence
    /// ("at the rate set out in the engagement").
    #[test]
    fn methodology_next_steps_contains_a_numeric_fix_rate_not_the_placeholder_sentence() {
        let f1 = finding("SEC-1", "a.rs", 1, "critical");
        let f2 = finding("SEC-2", "b.rs", 2, "high");
        let f3 = finding("SEC-3", "c.rs", 3, "medium");
        let f4 = finding("SEC-4", "d.rs", 4, "low");
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&f4),
            wire("FalsePositive", "generated fixture", ""),
        );
        let report = report_with(vec![f1, f2, f3, f4], vec![]);
        let json = build_report_json(&report, &dispositions, None, &empty_opts());

        assert_eq!(json.methodology.candidates_reviewed, 4);
        assert_eq!(json.methodology.excluded_false_positive, 1);

        let notes = &json.methodology.next_steps;
        assert!(
            !notes.contains("at the rate set out in the engagement"),
            "the old placeholder sentence must be gone: {notes}"
        );
        // 3 of 4 kept -> 75% fix rate.
        assert!(
            notes.contains("kept 3 of 4 candidate findings"),
            "the paragraph must state the real kept/total counts: {notes}"
        );
        assert!(
            notes.contains("75% fix rate"),
            "the paragraph must state the computed numeric fix rate: {notes}"
        );
        let has_digit = notes.chars().any(|c| c.is_ascii_digit());
        assert!(
            has_digit,
            "the next-steps text must contain a number: {notes}"
        );
    }

    #[test]
    fn next_steps_note_singular_finding_noun_at_exactly_one_candidate() {
        let note = next_steps_note(1, 0);
        assert!(
            note.contains("kept 1 of 1 candidate finding as"),
            "a single candidate must use the singular noun, not 'findings': {note}"
        );
    }

    // ── W6: a failed/timed-out pass must be disclosed, never omitted silently ──────

    #[test]
    fn a_failed_pass_renders_the_not_computed_disclosure_in_both_methodology_and_summary() {
        let mut report = report_with(vec![], vec![]);
        report.failed_passes = vec![crate::ai_audit::FailedPass {
            repo: "acme/widgets".to_string(),
            pass: "rule-alternative recommendations".to_string(),
            reason: "Claude CLI timed out after 300s (no output).".to_string(),
        }];
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.methodology.failed_passes.len(), 1);
        assert!(
            json.methodology.failed_passes[0].contains("not computed"),
            "methodology must explicitly state the pass was not computed: {:?}",
            json.methodology.failed_passes
        );
        assert!(
            json.methodology.failed_passes[0].contains("acme/widgets"),
            "methodology disclosure must name the affected repo: {:?}",
            json.methodology.failed_passes
        );
        assert!(
            json.methodology.failed_passes[0].contains("Claude CLI timed out after 300s"),
            "methodology disclosure must carry the real reason verbatim: {:?}",
            json.methodology.failed_passes
        );

        // The SAME disclosure also renders in the executive summary — never JUST the
        // methodology (a reader of only the summary must not be left in the dark either).
        assert_eq!(
            json.executive_summary.failed_passes, json.methodology.failed_passes,
            "the summary and methodology must carry the identical disclosure text"
        );
    }

    #[test]
    fn no_failed_pass_means_no_not_computed_disclosure_anywhere() {
        let report = report_with(vec![], vec![]);
        assert!(
            report.failed_passes.is_empty(),
            "sanity: the base fixture has no failed pass"
        );
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert!(
            json.methodology.failed_passes.is_empty(),
            "a clean run must not fabricate a not-computed disclosure: {:?}",
            json.methodology.failed_passes
        );
        assert!(
            json.executive_summary.failed_passes.is_empty(),
            "a clean run must not fabricate a not-computed disclosure: {:?}",
            json.executive_summary.failed_passes
        );
    }

    /// The Typst template only renders the disclosure block when the list is non-empty, and
    /// renders it in BOTH sections when it is — pins the gating + duplication so a future
    /// edit can't silently drop one of the two renders.
    #[test]
    fn shipped_template_gates_the_failed_pass_disclosure_on_both_sections() {
        let template = include_str!("../templates/audit_report.typ");
        assert_eq!(
            template
                .matches("d.executive_summary.failed_passes.len() > 0")
                .count(),
            1,
            "the summary section must gate the disclosure block on a non-empty list"
        );
        assert_eq!(
            template
                .matches("d.methodology.failed_passes.len() > 0")
                .count(),
            1,
            "the methodology section must gate the disclosure block on a non-empty list"
        );
    }

    // ── C3-1b: no field ever renders a literal "N/A" for a pipeline gap ─────────────

    /// An unestimated `effort`/`confidence` is a PIPELINE gap (the calibration pass never
    /// produced an opinion for this finding this run), not a legitimate "not applicable"
    /// value — `or_na`'s "N/A" reads as the latter. Pins that the shipped template uses the
    /// field-specific placeholders for both fields in the per-finding chip line, never `or_na`.
    #[test]
    fn shipped_template_never_renders_na_for_effort_or_confidence() {
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("or_not_estimated(site.effort)"),
            "an unestimated effort must render \"not estimated this run\", not \"N/A\""
        );
        assert!(
            template.contains("or_not_evaluated(site.confidence)"),
            "an unevaluated confidence must render \"not evaluated this run\", not \"N/A\""
        );
        assert!(
            !template.contains("or_na(site.effort)"),
            "the per-finding effort chip must never fall back to the generic N/A placeholder"
        );
        assert!(
            !template.contains("or_na(site.confidence)"),
            "the per-finding confidence chip must never fall back to the generic N/A placeholder"
        );
    }

    /// C3-1b, W6 wiring: a run where `ai_audit::verify_findings` recorded a run-wide "hour
    /// estimation" `FailedPass` must (a) disclose it in BOTH methodology and summary, exactly
    /// like the pre-existing alternative-recommendation disclosure, and (b) still ship the
    /// unestimated finding in the curated set / matrix — a failed estimation pass degrades
    /// honestly, it never drops a row.
    #[test]
    fn a_failed_estimation_pass_is_disclosed_and_unestimated_rows_still_ship() {
        let f = finding("ARCH-1", "a.rs", 1, "high"); // effort: None (never estimated)
        assert_eq!(
            f.effort, None,
            "test fixture must simulate the unestimated state"
        );
        let mut report = report_with(vec![f], vec![]);
        report.failed_passes = vec![crate::ai_audit::FailedPass {
            repo: "owner/repo".to_string(),
            pass: "hour estimation".to_string(),
            reason: "the calibration pass returned no usable remediation-effort estimate for \
                     any finding this run"
                .to_string(),
        }];
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());

        assert_eq!(json.methodology.failed_passes.len(), 1);
        assert!(
            json.methodology.failed_passes[0].contains("not computed"),
            "methodology must explicitly disclose the estimation pass was not computed: {:?}",
            json.methodology.failed_passes
        );
        assert!(
            json.methodology.failed_passes[0]
                .to_ascii_lowercase()
                .contains("hour estimation"),
            "the disclosure must name the failed pass: {:?}",
            json.methodology.failed_passes
        );
        assert_eq!(
            json.executive_summary.failed_passes, json.methodology.failed_passes,
            "the disclosure must appear identically in both sections"
        );

        // The row is NOT dropped — it still ships in the matrix and the curated findings,
        // carrying `effort: None` for the template to render honestly.
        assert_eq!(
            json.matrix.do_next.len() + json.matrix.do_now.len(),
            1,
            "an unestimated finding must still ship in an action bucket: {:?}",
            json.matrix
        );
        assert_eq!(json.curated_findings[0].sites[0].effort, None);
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
            "the draft banner must be gated on d.review_state"
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

    /// C4-R3 defect 1: the template used to ALSO echo a bare " · DRAFT" in the footer, on top
    /// of the header's full banner — a standalone "DRAFT" immediately ahead of the next page's
    /// full banner sentence in reading order, which collapsed into "DRAFTDRAFT: not yet
    /// reviewed..." under a naive multi-page text read. The banner text must appear in exactly
    /// ONE place in the template source (the header), never echoed a second time in the footer.
    #[test]
    fn shipped_template_does_not_echo_the_draft_banner_a_second_time_in_the_footer() {
        let template = include_str!("../templates/audit_report.typ");
        // The rendered banner CONTENT (not a comment mentioning it) is this exact bracketed
        // Typst markup span — it must appear exactly once (in the header), never a second time
        // anywhere else in the template.
        assert_eq!(
            template
                .matches("[DRAFT: not yet reviewed by a human reviewer]")
                .count(),
            1,
            "the rendered draft banner content must appear exactly once in the template"
        );
        let footer_start = template.find("footer: context [").expect("footer block");
        let footer_end = footer_start + template[footer_start..].find("],").expect("footer close");
        let footer_body = &template[footer_start..footer_end];
        assert!(
            !footer_body.contains("[ · DRAFT]") && !footer_body.contains("[DRAFT]"),
            "the footer must not render its own standalone DRAFT echo — the header banner \
             already marks every page, and a bare echo there used to sit immediately ahead of \
             the next page's full banner, concatenating into \"DRAFTDRAFT: not yet \
             reviewed...\" under a naive multi-page text read: {footer_body}"
        );
    }

    /// C4-R3 defect 2: an empty severity×effort matrix cell used to render the literal word
    /// "None" — a stringified absent value, not an intentional empty state. The template must
    /// render empty content for a zero-finding cell, never the word "None" (or "null").
    #[test]
    fn shipped_template_renders_an_empty_matrix_cell_as_blank_not_the_word_none() {
        let template = include_str!("../templates/audit_report.typ");
        let fn_start = template
            .find("#let grid_cell_content(cell) = {")
            .expect("grid_cell_content definition");
        let fn_end = fn_start + template[fn_start..].find("\n}\n").expect("function close");
        let body = &template[fn_start..fn_end];
        assert!(
            !body.contains("[None]") && !body.contains("[null]"),
            "an empty grid cell must never render the literal word None/null: {body}"
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

    /// W5: a RAW (unreviewed) export must compile cleanly through the now-gated sign-off
    /// block — the new `if is_draft [...] else [...]` branch in the signature block must not be
    /// a Typst syntax trap on the raw path, the one that is actually exercised by default (no
    /// disposition ever recorded).
    #[tokio::test]
    async fn compile_pdf_succeeds_for_an_unreviewed_draft_with_the_gated_signoff() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_succeeds_for_an_unreviewed_draft_with_the_gated_signoff: \
                 typst not on PATH"
            );
            return;
        }
        let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "critical");
        let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
        // No dispositions recorded at all -> Raw/unreviewed, same source of truth the draft
        // banner and the sign-off block both read.
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        assert_eq!(json.review_state, ReviewState::Raw);
        assert!(json.review_state.is_draft());

        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed for an unreviewed draft export");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// Regression (cycle-3 scan): a finding with an EMPTY path must not crash the PDF compile.
    /// `breakable("")` used to hit `().join(sep)`, which is `none` in Typst, and `#raw(none)`
    /// fails the whole compile ("expected string, found none"). A real scan produced a top-3
    /// do-now finding with an empty path and took the entire export down; the `breakable()` guard
    /// (none/empty -> "") fixes it. Critical severity routes the finding into the "three things"
    /// box that renders `#raw(breakable(item.path))`, exercising the crash path.
    #[tokio::test]
    async fn compile_pdf_succeeds_when_a_top_finding_has_an_empty_path() {
        if which_typst().is_none() {
            eprintln!(
                "skipping compile_pdf_succeeds_when_a_top_finding_has_an_empty_path: \
                 typst not on PATH"
            );
            return;
        }
        let f = finding("SEC-NO-HARDCODED-SECRETS-1", "", 0, "critical");
        let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
        let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let pdf = compile_pdf(&json)
            .await
            .expect("compile_pdf must succeed even when a finding has an empty path");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// End-to-end smoke test (typst-present-only, mirrors `compile_pdf_produces_a_real_pdf_
    /// when_typst_is_present`): a REVIEWED export (with a real disposition) must still compile
    /// cleanly through the gated template — the conditional banner/methodology logic must not
    /// be a Typst syntax trap that only happens to work on the (more commonly exercised) raw
    /// path. W5 companion to the test above: the sign-off block's reviewed branch must also
    /// still be valid Typst.
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

    /// W5: the sign-off block used to render "Prepared and signed off by: NAME / DATE"
    /// unconditionally — including on a raw (nobody-has-looked-at-this-yet) export, directly
    /// contradicting the per-page DRAFT banner above it. It must now be gated on the exact same
    /// `is_draft` signal the draft banner reads (not a second, independently-derived flag), so
    /// the two can never disagree again.
    #[test]
    fn shipped_template_signoff_is_gated_on_the_same_is_draft_signal_as_the_draft_banner() {
        let template = include_str!("../templates/audit_report.typ");
        let signoff_pos = template
            .find("Prepared and signed off by")
            .expect("signature block must exist");

        // The nearest `is_draft` check ABOVE the sign-off line must be the one that guards it
        // (`#if is_draft [ ... ] else [ ... Prepared and signed off by ... ]`) — found via
        // `rfind` on the (byte-boundary-safe, since `signoff_pos` came from `find`) prefix, so
        // this can't accidentally match the draft banner's own `is_draft` check way up in the
        // page header.
        let if_draft_pos = template[..signoff_pos]
            .rfind("if is_draft")
            .expect("an `if is_draft` branch must guard the sign-off block");
        let between = &template[if_draft_pos..signoff_pos];
        assert!(
            between.len() < 300,
            "the `if is_draft` guarding the sign-off block must sit directly above it, not be \
             some unrelated earlier use of the signal; gap was {} bytes: {between:?}",
            between.len()
        );
        assert!(
            between.contains("] else ["),
            "the sign-off line must be in the `else` (reviewed) branch of the is_draft check"
        );
        assert!(
            between.contains("unreviewed draft"),
            "the draft branch must explicitly say this is an unreviewed draft"
        );
        assert!(
            between.contains("not signed off"),
            "the draft branch must not claim a sign-off happened"
        );

        // The reviewed branch (only) still pairs the preparer name with a date.
        let reviewed_branch_end = template.len().min(signoff_pos + 200);
        let reviewed_branch = &template[signoff_pos..reviewed_branch_end];
        assert!(reviewed_branch.contains("d.cover.generated_at"));
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
            next_steps_note(10, 3),
            next_steps_note(0, 0),
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
