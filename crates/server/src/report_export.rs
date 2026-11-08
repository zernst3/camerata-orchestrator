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
    /// Who the report is prepared FOR: the cover's "Prepared for" line and the confidentiality
    /// marking on every page and sheet. Server-populated from the project's inspection setup
    /// (`Project::client_name`), never client-supplied (`serde(skip)`), so every export of a
    /// project names the same client. A blank name blocks the export ([`require_client_name`]).
    #[serde(skip)]
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
    /// The backend and provider that ran the inspection, for the manifest and README
    /// provenance. Server-populated, never client-supplied (`serde(skip)`).
    #[serde(skip)]
    pub model: Option<crate::ModelRecord>,
    /// H4: also export the triage log (per rule, what the AI pass proposed, what review kept
    /// and threw out, and why) as a workbook sheet and `csv/triage-log.csv`. OFF by default:
    /// the export dialog (or `camerata inspect` via [`EXPORT_TRIAGE_LOG_ENV`]) asks for it.
    /// See `xlsx_export::triage_log`.
    #[serde(default)]
    pub triage_log: bool,
    /// Every repo the project holds, in the project's order. Server-populated from the project,
    /// never client-supplied (`serde(skip)`). The package assembly refuses a report that covers
    /// fewer repos than this ([`require_all_repos`]); empty means the caller names no project
    /// (a one-off inspection) and only the report's own `repos_not_inspected` is checked.
    #[serde(skip)]
    pub expected_repos: Vec<String>,
    /// Changes the review screen has made that the decision store does not hold yet. The export
    /// dialog lists them from the write-through queue; the release gate refuses to export while
    /// any exist, because the store (what the export reads) would still carry the earlier
    /// decision. Empty for every other caller.
    #[serde(default)]
    pub unsynced_changes: Vec<camerata_api_types::decisions::UnsyncedChange>,
    /// The export is for the operator's own review (the project is not a client engagement, or
    /// `camerata inspect --internal`). Only an internal export may be made from a run that did not
    /// complete, and it is then marked INTERNAL INCOMPLETE on the cover and in `findings.json`.
    /// Server-populated, never client-supplied (`serde(skip)`).
    #[serde(skip)]
    pub internal: bool,
}

/// Env var that turns the optional triage log on for an export when set to `1` or `true`
/// (folded in by [`ReportOptions::apply_env_defaults`], like the brand). Unset means off.
pub const EXPORT_TRIAGE_LOG_ENV: &str = "CAMERATA_EXPORT_TRIAGE_LOG";

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

/// The brand the bundled logo files (`docs/design/brand/`) belong to. The mark renders on a
/// report only when its resolved brand is this one ([`brand_kit_matches`]); the colours and
/// type of the W27 template apply to every report, the logo does not.
pub const BRAND_KIT_NAME: &str = "Cantus Works";

/// Whether `brand` (the resolved [`ReportOptions::brand`], `None` for no brand) is the bundled
/// brand kit's, ignoring case and surrounding whitespace.
pub(crate) fn brand_kit_matches(brand: Option<&str>) -> bool {
    brand.is_some_and(|b| b.trim().eq_ignore_ascii_case(BRAND_KIT_NAME))
}

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
        self.triage_log = self.triage_log
            || triage_log_env_on(std::env::var(EXPORT_TRIAGE_LOG_ENV).ok().as_deref());
    }
}

/// Whether the [`EXPORT_TRIAGE_LOG_ENV`] value turns the triage log on: `1` or `true` (any
/// case), nothing else. Split out so it is testable without touching the process environment.
pub(crate) fn triage_log_env_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
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

/// Model-written (or reviewer-written) prose as a client reads it: no em dash, no en dash, no
/// multiplication sign ([`crate::client_text::plain`]) and no "line 0" for a problem in a whole
/// file ([`crate::client_text::whole_file_places`]).
pub(crate) fn client_prose(text: &str) -> String {
    crate::client_text::whole_file_places(&crate::client_text::plain(text)).into_owned()
}

/// A finding's `detail` as a client reads it. The parser joins a title to its explanation with
/// the engine's own separator (`ai_audit::HEADLINE_SEP`, an em dash that the detail stage splits
/// on again); a client reads "Title: explanation", or "Title. Borderline: explanation" when the
/// explanation opens with a label, and never the separator.
pub(crate) fn client_detail(detail: &str) -> String {
    let joined = match detail.split_once(crate::ai_audit::HEADLINE_SEP) {
        Some((title, rest)) => {
            let (title, rest) = (title.trim_end(), rest.trim_start());
            if title.is_empty() {
                rest.to_string()
            } else if rest.is_empty() {
                // A title with no explanation keeps the colon that says so (the content check
                // reads it as an empty explanation); a reviewer holds such a row.
                format!("{}:", title.trim_end_matches([':', '.']))
            } else if title.ends_with(['.', '!', '?', ':']) {
                format!("{title} {rest}")
            } else if crate::client_text::opens_with_label(rest) {
                format!("{title}. {rest}")
            } else {
                format!("{title}: {rest}")
            }
        }
        None => detail.to_string(),
    };
    client_prose(&joined)
}

/// Apply [`strip_zero_width`] (then secret redaction) to the [`Finding`]'s free-text fields that flow,
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
    // `snippet` is the client's own code, verbatim: it is also the site key's fingerprint input.
    // Zero-width characters are stripped and secrets redacted below; nothing is rewritten.
    f.snippet = strip_zero_width(&f.snippet).into_owned();
    f.detail = client_detail(&strip_zero_width(&f.detail));
    for v in f.captures.values_mut() {
        *v = strip_zero_width(v).into_owned();
    }
    // The three free-text fields this sanitizer used to miss, all of which reach the client:
    // the model-written fix (`fix_specific`), every absorbed evidence site's snippet, and the
    // severity rationale. This gap is exactly where the sweep exports' leaked keys sat.
    if let Some(fix) = f.fix_specific.as_mut() {
        *fix = client_prose(&strip_zero_width(fix));
    }
    if let Some(rationale) = f.calibration_rationale.as_mut() {
        *rationale = client_prose(&strip_zero_width(rationale));
    }
    for site in &mut f.also_locations {
        site.snippet = strip_zero_width(&site.snippet).into_owned();
        site.headline = client_prose(&site.headline);
    }
    // Zero-width characters are stripped FIRST so one inserted inside a key cannot hide it from
    // the detector; then the secret redaction (belt and braces: every producer already redacted
    // at construction, see `onboard::redact`). Idempotent.
    crate::onboard::redact::redact_finding(&mut f);
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

/// Stable identity for a `Finding`: its site key ([`camerata_api_types::site_key`]: repo, path,
/// rule and snippet fingerprint, ordinal; never the line). `camerata_ui_core::triage::
/// finding_key` is the same function over the UI's `FindingView`, so a disposition keyed off the
/// client's row looks up correctly here; `ui_and_server_site_key_agree_byte_for_byte` pins it.
/// A row whose key was never assigned ([`crate::inspections::backfill_site_keys`]) gets the
/// ordinal-0 key.
pub fn finding_key(f: &Finding) -> String {
    if !f.site_key.is_empty() {
        return f.site_key.clone();
    }
    camerata_api_types::site_key::site_key(&f.repo, &f.path, &f.rule_id, &f.snippet, 0)
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
    /// THE AFFIRMATIVE DISPOSITION (Pass 2 step 3): the auditor read this finding, judged it
    /// REAL, and it ships in the report as a live finding.
    ///
    /// Before this existed, every exit from `Unresolved` was a way of NOT shipping a row
    /// (`Ignored` accepts the risk, `TechDebt*` defers the fix, `FalsePositive` excludes it
    /// outright), so the only way to keep a finding live was to leave it undecided — and the
    /// export could not tell a finding the auditor had read and confirmed from one he had
    /// never opened. The product's first promise is that one person reads every finding and
    /// signs; this is the variant that puts data behind it.
    ///
    /// Semantically it is "real", NOT "scheduled" and NOT "accepted":
    ///
    /// * [`matrix_bucket`] gives it the SAME severity-derived quadrant an `Unresolved` row
    ///   gets. Confirming states that the finding is genuine, which is exactly what the
    ///   quadrant already assumed; it says nothing about when or whether to fix it.
    /// * it counts as STILL OPEN in the executive summary and the category scorecard, because
    ///   a confirmed-and-unfixed finding is outstanding work for the client.
    /// * it DOES flip [`ReviewState`] to `Reviewed` — unlike `BaselineAccepted`/`WaivedInline`
    ///   it is this session's human judgment, which is the whole point of it.
    ///
    /// Client-local, like `FalsePositive`: it is read out of the draft's disposition map by
    /// the export and commits nothing at Process (owner's Q2 ruling, 2026-10-12 — a durable
    /// per-finding record across engagements is a backend surface Pass 2 must not invent).
    Confirmed,
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
///
/// Grading-cycle fix (2026-10-07): `BaselineAccepted` is a FALSE ATTESTATION unless a real
/// baseline record backs it — the client text ("pre-existing accepted debt (baseline
/// suppression)" / xlsx's "accepted in a PRIOR run") says someone already triaged and
/// accepted this exact finding. The ONLY legitimate producer of `status ==
/// "suppressed-baseline"` is `onboard::audit::classify_repo_findings`, which stamps it
/// strictly when a finding's `(rule_id, content fingerprint)` matches an entry actually
/// present in the repo's committed `.camerata/baseline.json` — and that classification pass
/// runs ONLY over the pre-preview finding set (`repo_findings`, before `merge_scan_preview`
/// appends anything). A PREVIEW/external-tool row (`finding.preview == true` — Semgrep,
/// Clippy, Ruff, ESLint rows from `scan_tools::run_scan_tools`/`preview_finding`) is appended
/// to the report AFTER that pass and so can NEVER have gone through real baseline matching,
/// no matter what its raw `status` string claims. Before this fix, `scan_tools::
/// preview_finding`/`note_finding` defaulted every preview row's `status` to
/// `"suppressed-baseline"` (meaning only "not yet wired into CI") and this function trusted
/// that string blindly — stamping "baseline accepted" on repos that had never been triaged
/// at all (3/4/6 rows across three graded fixtures, zero baseline file, zero waiver). A
/// preview row can therefore never classify as `BaselineAccepted` here, defense-in-depth,
/// even if some future producer repeats the same mistake: it downgrades to `Unresolved`
/// (open; still routes to `held` if another gate, e.g. `needs_review`, independently
/// applies) rather than shipping as an accepted disposition with no record behind it. See
/// `scan_tools::preview_finding`'s doc comment for the matching source-side fix, and
/// `tests::a_preview_finding_with_suppressed_baseline_status_is_never_baseline_accepted` for
/// the test-time guard.
pub(crate) fn classify(finding: &Finding, wire: Option<&DispositionWire>) -> Disposition {
    // W28: what the row's OWN STATUS implies, with no triage decision in play. Used both when
    // there is no wire entry and when there is one this version cannot read — see below.
    let from_status = || match finding.status.as_str() {
        "suppressed-baseline" if !finding.preview => Disposition::BaselineAccepted,
        "suppressed-inline" => Disposition::WaivedInline,
        _ => Disposition::Unresolved,
    };

    let Some(d) = wire else {
        return from_status();
    };

    match d.state.as_str() {
        "Confirmed" => Disposition::Confirmed,
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
                 build_report_json. Falling through to the status rather than panicking."
            );
            from_status()
        }
        // W28 — an UNRECOGNISED state falls back to the status, not to `Unresolved`.
        //
        // This arm used to return `Unresolved`, which renders "Open (recommended: …)". That
        // asserted a human judgment nobody made: an unrecognised state is a parse miss or a
        // producer from a different version, never a decision to leave the row open. On a
        // row whose own `status` reads `suppressed-baseline` it also made the export
        // self-contradicting, and the 2026-10-16 sweep shipped 38 rows in exactly that shape.
        //
        // A RECOGNISED state still wins over the status, above: an auditor who confirmed or
        // ignored a baseline-suppressed row made a real decision this session, and
        // overriding the suppression is what a disposition is for. The fallback applies only
        // where there is no decision to honour.
        _ => from_status(),
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
/// See the codebase-inspection hardening notes' P5 section: the bug this
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
/// Why a row held for a reviewer is waiting, in plain words for the client, or `None` when no
/// concrete reason applies. The three gates are checked in the order that decides the bucket:
/// no citation first, then no fix, then no effort estimate (see `effective_bucket`). Each reason
/// ends in the same call to action, because the row is in the "needs a decision" state until a
/// reviewer rules on it.
pub(crate) fn waiting_reason(
    uncited: bool,
    no_fix: bool,
    no_estimate: bool,
) -> Option<&'static str> {
    if uncited {
        Some("No outside standard supports this problem yet, so a reviewer must rule on it before you act.")
    } else if no_fix {
        Some("No fix is available for it yet, so a reviewer must rule on it before you act.")
    } else if no_estimate {
        Some("No effort estimate is available for it yet, so a reviewer must rule on it before you act.")
    } else {
        None
    }
}

/// No em/en dashes (house style for this client deliverable) — see `bucket_title`.
pub(crate) fn disposition_label(
    disposition: Disposition,
    reason: &str,
    bucket: &str,
    confirmed_by_client: bool,
) -> String {
    match disposition {
        // C5-4: a calibration hedge (any severity) reads as a distinct "held for review" call
        // to action, never the low-stakes "Convention to consider" wording the `informational`
        // appendix uses — a high/critical row the calibrator itself flagged debatable must never
        // read like a style note. C5-7: the `held` bucket now also carries a SECOND, unrelated
        // reason (a high/critical uncited AI-tier finding — see `is_uncited_ai_finding`), so
        // this label is deliberately reason-agnostic rather than naming "calibration"
        // specifically; the row's `why_waiting` states the concrete reason right next to it.
        Disposition::Unresolved if bucket == "held" => {
            "Held for review (a human reviewer's judgment call is needed before this can be \
             actioned)"
                .to_string()
        }
        Disposition::Unresolved if bucket == "informational" => {
            "Convention to consider (informational)".to_string()
        }
        Disposition::Unresolved => format!("Open (recommended: {})", bucket_title(bucket)),
        // The affirmative call, stated as the auditor's own and nobody else's. It names the
        // same recommended bucket an open row names — confirming establishes that the finding
        // is real, not when it gets fixed — but it says WHO decided, which is the difference
        // the reader cannot otherwise see.
        Disposition::Confirmed => {
            format!(
                "Confirmed by the reviewer (recommended: {})",
                bucket_title(bucket)
            )
        }
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
        "held" => "Held for review",
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
    camerata_rules::severity_bounds::normalize_severity(raw)
}

// The four severity bounds (E2 `max_severity`, W8 `min_severity`, A5.2 `deviation_ceiling`,
// C4-R1 unlocated) and their order live in `camerata_rules::severity_bounds`, shared with the
// `camerata-check` CI gate; the exporters apply them through
// `crate::severity_bounds::bounded_severity`. The three adapters below keep this module's
// unit tests pinned to the same chain, resolving the rule through [`corpus_rule_for`].

#[cfg(test)]
pub(crate) fn clamp_to_rule_max_severity(
    severity: String,
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    camerata_rules::severity_bounds::clamp_to_max_severity(
        severity,
        corpus_rule_for(rule_id, corpus),
    )
}

#[cfg(test)]
pub(crate) fn raise_to_rule_min_severity(
    severity: String,
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    camerata_rules::severity_bounds::raise_to_min_severity(
        severity,
        f,
        corpus_rule_for(&f.rule_id, corpus),
    )
}

#[cfg(test)]
pub(crate) fn clamp_deviation_severity(
    severity: String,
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    camerata_rules::severity_bounds::clamp_deviation_severity(
        severity,
        f,
        corpus_rule_for(&f.rule_id, corpus),
    )
}

mod citations_fixes;
mod classification;
mod json_types;
mod marking;

pub use citations_fixes::*;
pub use classification::*;
pub use json_types::*;
pub use marking::*;

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
    reviewed: &ReviewedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> AuditReportJson {
    // The PDF's data is built from the reviewed report and nothing else (design 3B): the rows
    // with the reviewer's overlay applied, and the reviewer's decisions.
    let report = reviewed.report();
    let dispositions = reviewed.dispositions();
    // Pass-1 W1 seal fix: the evidence-of-violation gate used to run HERE, against a private
    // clone that then shadowed `report` — see `apply_evidence_gate`'s doc comment for the full
    // defect this caused. It now runs upstream, as part of the scan pipeline, strictly before
    // `report` was ever sealed — `report: &SealedReport` above is the compile-time guarantee
    // that it already has.
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
            if matches!(
                w.state.as_str(),
                "Confirmed" | "Ignored" | "TechDebt" | "FalsePositive"
            ) {
                auditor_touched_any_finding = true;
            }
        }
        if wire.map(|d| d.state.as_str()) == Some("FalsePositive") {
            excluded_fp += 1;
            continue;
        }
        let disposition = classify(f, wire);
        let reason = wire.map(|d| d.reason.clone()).unwrap_or_default();
        // Every severity bound, in one shared definition: normalize, the rule's `min_severity`
        // FLOOR (W8), the C4-R1 null-location safety net, then the `max_severity` and deviation
        // ceilings (E2, A5.2), which get the last word. The live scan and the workbook / CSV /
        // JSON partition call the same function, so no surface can disagree with the PDF; see
        // `severity_bounds`. Applied here once, so every downstream section (matrix bucketing,
        // curated findings, the scorecard) sees the ALREADY-bounded severity and can never
        // independently re-derive the pre-bound one.
        let severity = crate::severity_bounds::bounded_severity(f, corpus);
        live.push((f, disposition, reason, severity));
    }
    let review_state = if auditor_touched_any_finding {
        ReviewState::Reviewed
    } else {
        ReviewState::Raw
    };
    // The repos that flag a credential under the same name, for the display-only note on each
    // such site. Built over the rows the report carries: a row the reviewer dismissed as a false
    // positive is not in the report, so no note may point at it. Nothing to link in one repo.
    let credentials = if report.repos.len() > 1 {
        crate::onboard::credential_cross_ref::CredentialIndex::from_findings(
            live.iter().map(|(f, _, _, _)| *f),
        )
    } else {
        crate::onboard::credential_cross_ref::CredentialIndex::default()
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
        // otherwise sit in the curated set carrying "AI-advisory, model-inferred." C5-8 adds a
        // THIRD such signal (an action-tier row with no fix to show at all). All three gates
        // now live INSIDE `effective_bucket` itself (C3-6) — kept as separate bools here only
        // because the appendix headline below needs to say WHICH reason applied.
        // `chosen_option_for_rule` is needed by `effective_bucket`'s C5-8 "has a fix" check
        // below, so it is computed here, ahead of that call, rather than after it as before.
        let chosen_option_for_rule = opts
            .chosen_options
            .get(&f.rule_id.to_ascii_uppercase())
            .map(String::as_str);
        let gate_uncited = is_uncited_ai_finding(f, corpus);
        let gate_no_fix = is_action_row_missing_a_fix(
            f,
            *disposition,
            severity,
            corpus,
            report.test_file_count,
            chosen_option_for_rule,
        );
        // C6-B4: computed independently (same pattern as `gate_no_fix` just above) purely so
        // the headline below can say WHICH held-for-review reason applied — `effective_bucket`
        // checks `is_action_row_missing_a_fix` FIRST, so when both this and `gate_no_fix` are
        // true the fix-missing reason wins in the headline below, matching the bucket the two
        // gates agree on either way (both route to "held").
        let gate_no_estimate = is_action_row_missing_an_estimate(
            f,
            *disposition,
            severity,
            corpus,
            report.test_file_count,
            chosen_option_for_rule,
        );
        let bucket = effective_bucket(
            f,
            *disposition,
            severity,
            corpus,
            report.test_file_count,
            chosen_option_for_rule,
        );
        // C4-P2: `title_for_finding` (not a bare corpus-or-rule_id join) so an AI-tier finding
        // grounded via the P3 class fallback gets a human-readable title too, instead of a
        // header that is nothing but the model's own invented rule id repeated.
        let fallback_title = title_for_finding(f, corpus);
        // P4: a deterministic floor finding renders its AUTHORED, repo-specific headline
        // (never the gate's raw "Deny…" directive) when the corpus has one for this rule;
        // an AI-tier / not-yet-authored finding falls back to the pre-P4 derivation exactly
        // as before. See `client_headline_and_detail`'s doc comment.
        let (base_headline, _) =
            client_headline_and_detail(f, corpus, &fallback_title, chosen_option_for_rule);
        // P3 / C5-8 / C6-B4: an honest "why is this not curated" marker for the appendix row,
        // distinct from the other informational/held reasons (which the appendix count
        // doesn't otherwise distinguish either — see `matrix.informational`'s doc comment).
        // `gate_uncited` is mutually exclusive with the other two by construction
        // (`effective_bucket` checks `is_uncited_ai_finding` before ever reaching either
        // fix/estimate-availability check); `gate_no_fix` and `gate_no_estimate` can both be
        // true for the same row (no fix usually means no estimate either), so `gate_no_fix`'s
        // arm is checked first and wins the headline in that case — not an ordering ambiguity,
        // just a priority pick between two reasons that both land the SAME row in "held".
        // A row held for a reviewer keeps a plain headline. The concrete reason it is waiting is
        // its own field (`why_waiting`), never a prefix on the headline: "Needs review (uncited
        // ...)" is the engine talking to itself.
        let headline = base_headline;
        // C6-B4 (amended): a missing estimate NO LONGER routes a row to held, so the reason only
        // applies when the row genuinely IS out of the action tier for some other reason. A
        // curated action row that merely lacks an effort band keeps no reason and renders an
        // honest "not estimated this run" in its effort field instead.
        let why_waiting = waiting_reason(
            gate_uncited,
            gate_no_fix,
            gate_no_estimate && (bucket == "held" || bucket == "informational"),
        );
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
        let mut reference = finding_ref(
            f,
            severity,
            headline,
            bucket,
            corpus,
            chosen_option_for_rule,
        );
        reference.why_waiting = why_waiting.map(str::to_string);
        bucketed
            .entry(bucket)
            .or_default()
            .push((sort_key, reference));
    }
    let mut matrix = MatrixJson::default();
    for (key, target) in [
        ("do_now", &mut matrix.do_now),
        ("do_next", &mut matrix.do_next),
        ("plan", &mut matrix.plan),
        ("accepted", &mut matrix.accepted),
        ("informational", &mut matrix.informational),
        ("held", &mut matrix.held),
    ] {
        if let Some(mut entries) = bucketed.remove(key) {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            *target = entries.into_iter().map(|(_, e)| e).collect();
        }
    }

    // ── W1: report-build-time ledger stage ────────────────────────────────────────
    // `has_rule_ledger_data` gates every ledger-derived section below on whether `report`
    // actually carries real per-rule facts (the live pipeline always populates these via
    // `onboard::audit_repos`) — a report built before W1, or a test fixture that never
    // touches `report.ledger`, falls back to the pre-W1 derivation so existing behavior is
    // unchanged. `report_ledger` is a CLONE (this function stays pure/no-mutation of
    // `report`) that additionally records THIS function's own row-accounting: every
    // `code_finding` is either curated (do_now/do_next/plan/accepted), held, informational,
    // excluded as a false positive, or carved into the dependency snapshot — see
    // `StageAccounting::other` for the last two, which don't fit the four named buckets.
    let has_rule_ledger_data = report.ledger.rules().next().is_some();
    let mut report_ledger = report.ledger.clone();
    {
        let rows_in = candidates_reviewed;
        let rows_out =
            matrix.do_now.len() + matrix.do_next.len() + matrix.plan.len() + matrix.accepted.len();
        let mut acc = crate::scan_ledger::StageAccounting {
            routed_held: matrix.held.len(),
            routed_informational: matrix.informational.len(),
            ..Default::default()
        };
        acc.other
            .push(("excluded_false_positive".to_string(), excluded_fp));
        acc.other.push((
            "dependency_snapshot_carve_out".to_string(),
            dep_findings.len(),
        ));
        report_ledger.record_stage("report-build-partition", rows_in, rows_out, acc);
    }

    // Curated findings: grouped by rule (sorted for deterministic output), each rule's
    // sites sorted by repo/path/line.
    #[allow(clippy::type_complexity)]
    let mut by_rule: std::collections::BTreeMap<
        String,
        Vec<(&Finding, Disposition, String, String)>,
    > = std::collections::BTreeMap::new();
    // C6-B1 (closes a client-facing "counted but never rendered" bug): this loop used to
    // `continue` past an `is_uncited_ai_finding` row entirely — the stale reasoning (see the
    // removed comment, and `uncited_ai_row_provenance_and_citation_are_never_borrowed_from_a_
    // sibling`'s now-outdated doc comment) was "it was already routed to `matrix.informational`
    // above, so no `CuratedGroupJson` is needed for it." That was true ONLY of the gate's
    // original, pre-C5-7 shape, where an uncited AI finding always landed in `informational`.
    // C5-7 (see `effective_bucket`'s doc comment) now routes a high/critical uncited finding to
    // `"held"` instead — a severity-unbounded bucket the "Held for review" PDF section (and the
    // cover/scorecard/xlsx counts, all of which tally `code_findings` directly) already expect
    // to see rendered. Skipping the row here left it counted in `matrix.held`, the cover severity
    // strip, the category scorecard and `findings.json`/the workbook, while the PDF renderer
    // never emitted it anywhere — not the matrix (expected: held is out of scope for the
    // severity x effort grid), not curated (expected: this gate's whole point), and NOT Held for
    // review either, because no site ever existed for it to render. Every `code_findings` row
    // now unconditionally gets a `CuratedSiteJson` here; the uncited-AI gate still keeps it OUT
    // of `curated_findings` — the actual client-facing guarantee — via the bucket-based
    // `curated_findings` / `held_for_review_findings` partition below (its `bucket` field is
    // already `"informational"` or `"held"`, never an action tier, for exactly this finding; see
    // `effective_bucket`), not by erasing the row from existence.
    for (f, disposition, reason, severity) in &code_findings {
        by_rule.entry(f.rule_id.clone()).or_default().push((
            f,
            *disposition,
            reason.clone(),
            severity.clone(),
        ));
    }
    let mut all_curated_groups: Vec<CuratedGroupJson> = Vec::new();
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
        let site_jsons: Vec<CuratedSiteJson> = sites
            .iter()
            .map(|(f, disposition, reason, severity)| {
                // `chosen_option_for_rule` feeds `effective_bucket`'s C5-8 "has a fix" check
                // below, so (same reordering as the matrix-building loop above) it is computed
                // ahead of that call now, not after it.
                let chosen_option_for_rule = opts
                    .chosen_options
                    .get(&rule_id.to_ascii_uppercase())
                    .map(String::as_str);
                // Bug 4 / C3-6: keep the curated-site LABEL in lockstep with the matrix cell
                // this finding actually lands in — an informational row reads "Convention to
                // consider", never "Open (recommended: Plan)". `effective_bucket` is the SAME
                // call the matrix-building pass above makes for this finding, so the two can
                // never compute a different answer for the same row.
                let bucket = effective_bucket(
                    f,
                    *disposition,
                    severity,
                    corpus,
                    report.test_file_count,
                    chosen_option_for_rule,
                );
                // C5-8: a no-fix-held row still appears here — same rule, same group, just this
                // one site's `bucket` is `"held"` instead of an action tier. Disclose WHY in its
                // own headline too, not only via `disposition_label`'s generic held-for-review
                // wording, so a reader scanning this site within its rule's curated group sees
                // the concrete reason right next to it.
                let gate_no_fix = is_action_row_missing_a_fix(
                    f,
                    *disposition,
                    severity,
                    corpus,
                    report.test_file_count,
                    chosen_option_for_rule,
                );
                // C6-B4: same disclosure pattern as `gate_no_fix` just above — see that gate's
                // sibling-ordering note in the matrix-bucketing loop for why `gate_no_fix` wins
                // the headline when both are true for the same row.
                let gate_no_estimate = is_action_row_missing_an_estimate(
                    f,
                    *disposition,
                    severity,
                    corpus,
                    report.test_file_count,
                    chosen_option_for_rule,
                );
                // C6-B1: same disclosure for the uncited-AI gate, now that this loop no longer
                // `continue`s past such a finding (see the `by_rule`-population loop above) —
                // mirrors the identical `gate_uncited` headline prefix the matrix-bucketing loop
                // computes for the same row, so the wording is identical wherever the row renders.
                let gate_uncited = is_uncited_ai_finding(f, corpus);
                let confirmed_by_client = dispositions
                    .get(&finding_key(f))
                    .map(|d| d.confirmed_by_client)
                    .unwrap_or(false);
                // P4: prefer the authored, repo-specific (headline, detail) pair over the raw
                // gate text — see `client_headline_and_detail`'s doc comment. `detail` here is
                // NOT `f.detail.clone()` unconditionally anymore: for a floor finding with an
                // authored template, it's the authored client-facing detail instead of the
                // gate's own "Deny…" enforcement prose.
                let (base_headline, detail) =
                    client_headline_and_detail(f, corpus, &title, chosen_option_for_rule);
                // The headline stays plain; the reason a row is waiting is `why_waiting`.
                let headline = base_headline;
                let why_waiting = waiting_reason(
                    gate_uncited,
                    gate_no_fix,
                    gate_no_estimate && (bucket == "held" || bucket == "informational"),
                );
                let specific_fix = f
                    .fix_specific
                    .as_deref()
                    .map(fix_text::prepare)
                    .filter(|s| !s.is_empty());
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
                    disposition: overlay::merged_label(f).map(str::to_string).unwrap_or_else(
                        || disposition_label(*disposition, reason, bucket, confirmed_by_client),
                    ),
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
                    why_waiting: why_waiting.map(str::to_string),
                    // P2: `f.fix_specific` was generated at SCAN time by
                    // `ai_audit::generate_fix_specifics` (this layer stays pure/synchronous —
                    // no model access here, just a read) — a codebase-specific fix that the
                    // template renders ABOVE `fix` (the rule's generic remediation is now
                    // secondary context only). `None` when generation never ran (a
                    // deterministic-only scan) or gave up after retries — in the latter case
                    // `fix` (resolved just above from the rule's own authored remediation)
                    // still renders as the primary "Fix:" line (C3-1b: a pipeline failure here
                    // never removes the finding's fix, only its codebase-specific flavor).
                    //
                    // C6-B3: a give-up no longer always leaves `fix_specific` (and therefore
                    // `fix_for_this_finding`) at `None` — `generate_fix_specifics` now
                    // instantiates the rule's OWN authored remediation against this finding's
                    // own identifiers and writes THAT into `fix_specific`, via the exact same
                    // `resolve_fix` machinery this `fix` field below calls. When it does, the
                    // two fields would otherwise render the IDENTICAL sentence twice (a bold
                    // "Fix:" line immediately followed by a "General guidance:" line repeating
                    // it verbatim) — `fix` is suppressed here whenever it's byte-identical to
                    // `fix_for_this_finding`, so the template's secondary line only ever adds
                    // information, never echoes the primary line back.
                    fix: {
                        let generic =
                            resolve_fix_for_row(&rule_id, corpus, f, chosen_option_for_rule);
                        if generic == specific_fix {
                            None
                        } else {
                            generic
                        }
                    },
                    // D8: the label beside the fix, from the same two facts the fields above
                    // were built from (the specific fix and the rule's own authored one).
                    fix_type: {
                        let authored =
                            resolve_fix_for_row(&rule_id, corpus, f, chosen_option_for_rule);
                        fix_type_label(
                            f,
                            specific_fix.is_some(),
                            authored.is_some(),
                            specific_fix.is_some() && authored == specific_fix,
                        )
                        .to_string()
                    },
                    // The model's text gets the same wrap-joining as the authored text, so the
                    // equality above compares like with like.
                    fix_for_this_finding: specific_fix,
                    // W7 (C5): the absorbed sites, verbatim — see the field's doc comment.
                    also_locations: f.also_locations.clone(),
                    also_sites: also_sites_for(f, corpus),
                    also_flagged_here: also_flagged_here_for(f, corpus),
                    same_credential_note: crate::onboard::credential_cross_ref::note(
                        &credentials.also_flagged_in(f),
                    ),
                    severity_adjustments: crate::severity_bounds::severity_adjustment_lines(
                        f, corpus,
                    ),
                    site_count: f.distinct_site_count(),
                    reviewer_edited: overlay::is_reviewer_edited(f),
                    edit_ids: overlay::edit_ids_of(f),
                    kind: camerata_api_types::audit::KindOnWire(f.kind.clone()),
                }
            })
            .collect();
        let repos = if report.repos.len() > 1 {
            let mut repos: Vec<String> = site_jsons.iter().map(|s| s.repo.clone()).collect();
            repos.sort();
            repos.dedup();
            repos
        } else {
            Vec::new()
        };
        all_curated_groups.push(CuratedGroupJson {
            rule_id,
            title,
            citation,
            sites: site_jsons,
            repos,
        });
    }

    // C4-P4 (residual defect 5) / C5-4: split fully-informational-or-held groups into their own
    // section, then order the remaining (genuinely curated) groups by severity desc / bucket /
    // confidence — see `AuditReportJson::curated_findings`'s and `::held_for_review_findings`'s
    // doc comments for the eligibility rule and `curated_group_sort_key` for the ordering
    // itself. `"held"` (C5-4) is treated identically to `"informational"` here: a group whose
    // every site is either appendix-worthy or calibration-hedged (at ANY severity) is never
    // curated, and a high/critical hedge must land in `held_for_review_findings`, not
    // `curated_findings` — that is the whole point of the C5-4 fix.
    let (mut curated_findings, mut held_for_review_findings): (
        Vec<CuratedGroupJson>,
        Vec<CuratedGroupJson>,
    ) = all_curated_groups.into_iter().partition(|g| {
        g.sites
            .iter()
            .any(|s| !matches!(s.bucket.as_str(), "informational" | "held"))
    });
    curated_findings.sort_by_key(curated_group_sort_key);
    held_for_review_findings.sort_by_key(curated_group_sort_key);

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
    // C5-1 (coverage accounting): a rule counts as "matched this run" if it fired as a PRIMARY
    // (`f.rule_id`) OR survived only as a merged `also_matches` member — a rule demoted into
    // `also_matches` by `ai_audit::merge_location_group`/`merge_semantic_group` genuinely fired;
    // it just lost the primary slot to another finding describing the same defect. Before this,
    // `rule_ids_with_findings`/`rule_ids_with_any_finding` below only ever looked at the
    // PRIMARY's own `rule_id`, so an absorbed rule — most commonly the deterministic rule a
    // pre-fix merge used to demote behind an inflated AI primary (see `merge_semantic_group`'s
    // `primary_rank` doc comment) — showed 0 findings in the Coverage sheet and was listed as
    // "verified clean" even though it fired and was merged into a visible row. Keyed by each
    // absorbed id's OWN category (via `category_for`, the same join `audited_rule_ids` uses just
    // above) since an absorbed AI/adopted id can belong to a different category than the
    // primary's — a member matched under ITS category, not necessarily the row's displayed one.
    let mut matched_rule_ids_by_category: std::collections::BTreeMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::BTreeMap::new();
    for (f, _, _, _) in &code_findings {
        matched_rule_ids_by_category
            .entry(category_for_finding(f, corpus))
            .or_default()
            .insert(f.rule_id.clone());
        for also_id in &f.also_matches {
            matched_rule_ids_by_category
                .entry(category_for(also_id, corpus))
                .or_default()
                .insert(also_id.clone());
        }
        // H6: a rule that survived only as an absorbed site (`also_locations`) fired just as
        // genuinely as one demoted into `also_matches`; counting it clean in its own category
        // was the same false statement one channel over.
        for site in &f.also_locations {
            matched_rule_ids_by_category
                .entry(category_for(&site.rule_id, corpus))
                .or_default()
                .insert(site.rule_id.clone());
        }
    }
    let mut scorecard_rows: Vec<CategoryRowJson> = Vec::new();
    for (category, entries) in &by_category {
        let mut critical = 0usize;
        let mut high = 0usize;
        let mut medium = 0usize;
        let mut low = 0usize;
        let mut open_high_or_critical = false;
        let mut open_medium = false;
        let mut rule_ids_with_findings: std::collections::HashSet<String> =
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
            rule_ids_with_findings.insert(f.rule_id.clone());
            // `Confirmed` is still open: the auditor has established that the finding is real,
            // which is the opposite of it having been dealt with. Omitting it here would let a
            // fully-reviewed scan of real criticals report its category as "Clean".
            let still_open = matches!(
                disposition,
                Disposition::Unresolved
                    | Disposition::Confirmed
                    | Disposition::TechDebtNow
                    | Disposition::TechDebtLater
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
        // C5-1: fold in every rule id absorbed into THIS category via `also_matches` (see the
        // doc comment on `matched_rule_ids_by_category` above) — a rule that only survived as a
        // merged member still genuinely matched and must count toward `audited_rules`/never
        // toward `clean_rules`.
        if let Some(absorbed) = matched_rule_ids_by_category.get(category) {
            rule_ids_with_findings.extend(absorbed.iter().cloned());
        }
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
                .filter(|rid| !audited_in_category.contains(&rid.as_str()))
                .count();
        let clean_rules = audited_in_category
            .iter()
            .filter(|rid| !rule_ids_with_findings.contains(**rid))
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
    // C5-1: a rule that only survived a merge as an absorbed `also_matches` member (never its
    // own primary row) still genuinely fired this run — see `matched_rule_ids_by_category`'s doc
    // comment above for the full "0 in Coverage, listed as verified clean" bug this closes.
    // "Healthy" is global (not per-category, unlike the scorecard above), so this folds every
    // `also_matches` id in directly rather than going through the per-category map.
    //
    // W23 (2026-10-20): `also_locations` is folded in for the SAME reason, and its omission
    // was the C5-1 defect one channel over. `also_matches` carries a same-location demotion
    // (a rule id set); `also_locations` carries a MERGE ABSORPTION (one entry per absorbed
    // finding, each with its own originating `rule_id`). A rule that survived only as an
    // absorbed location fired just as genuinely as one that survived as a demotion, and
    // listing it clean is the same false statement.
    //
    // Measured on the 2026-10-18 hearth export: SEC-NO-SECRET-FILE-1 (a committed
    // `.env.production`, CRITICAL) had two `also_locations` entries and no direct row, and
    // ARCH-OBSERVABILITY-SILENT-FAILURE-1 one. Both were eligible for "What's healthy" — the
    // report would have told the client their committed secret belonged to a verified-clean
    // rule.
    let rule_ids_with_any_finding: std::collections::HashSet<String> = code_findings
        .iter()
        .flat_map(|(f, _, _, _)| {
            std::iter::once(f.rule_id.clone())
                .chain(f.also_matches.iter().cloned())
                .chain(f.also_locations.iter().map(|l| l.rule_id.clone()))
        })
        .collect();
    // W1: "verified clean" is sourced from the LEDGER (`ran && findings_emitted == 0`) when
    // the scan populated one — a rule the ledger knows did NOT run (e.g. the item-4 defect:
    // declares mechanical enforcement with no wired detector) can never appear here just
    // because it was merely SELECTED. `report.provenance.audited_rule_ids` (every selected
    // rule, regardless of execution) remains the fallback for a report with no ledger data
    // (pre-W1 persisted reports, or a test fixture that never populates `report.ledger`), so
    // existing behavior is unchanged there.
    let healthy_candidate_ids: Vec<String> = if has_rule_ledger_data {
        report_ledger.healthy_rule_ids()
    } else {
        report.provenance.audited_rule_ids.clone()
    };
    let mut healthy_rules: Vec<HealthyRuleJson> = healthy_candidate_ids
        .iter()
        .filter(|rid| !rule_ids_with_any_finding.contains(rid.as_str()))
        .map(|rid| {
            let (clean_in, not_applicable_in) = if has_rule_ledger_data && report.repos.len() > 1 {
                healthy_repos_for(report, rid)
            } else {
                (Vec::new(), Vec::new())
            };
            HealthyRuleJson {
                rule_id: rid.clone(),
                title: corpus
                    .and_then(|c| c.get_by_id(rid))
                    .map(|r| r.title.clone())
                    .unwrap_or_else(|| crate::client_text::plain_rule_name(rid)),
                citation: resolve_citation(rid, None, corpus),
                clean_in,
                not_applicable_in,
            }
        })
        .collect();
    healthy_rules.sort_by(|a, b| {
        let grounded_first = |kind: &str| if kind == "grounded" { 0 } else { 1 };
        (grounded_first(&a.citation.kind), &a.rule_id)
            .cmp(&(grounded_first(&b.citation.kind), &b.rule_id))
    });
    let further_clean_count = healthy_rules.len().saturating_sub(WHATS_HEALTHY_CAP);
    healthy_rules.truncate(WHATS_HEALTHY_CAP);

    // ── W1: "excluded from this audit" — rules the ledger confirms did NOT run ───
    // Only ever populated from the ledger. There is no pre-W1 fallback source for this
    // section, and in particular `ScanReport::excluded_mechanical_rules` is NOT one: it is a
    // different, older field that is now VESTIGIAL. `split_scannable_rules_from` has returned
    // a hard-coded `Vec::new()` for it since W4 dropped CI-tier exclusion from the semantic
    // pass ("Every selected rule reaches the semantic pass now — no CI-tier exclusion"), so
    // nothing populates it and its two remaining readers — the cockpit scan header and
    // `xlsx_export::write_coverage_sheet`'s "Excluded from this code-only audit" block — can
    // only ever render empty. Left in place rather than removed: it is persisted on
    // `ScanReport`, and a pre-W4 report loaded from disk could still carry real content.
    let mut rules_not_run: Vec<ExcludedRuleJson> = if has_rule_ledger_data {
        report_ledger
            .excluded_rules()
            .into_iter()
            .map(|(rid, reason)| ExcludedRuleJson {
                rule_id: rid.to_string(),
                title: corpus
                    .and_then(|c| c.get_by_id(rid))
                    .map(|r| r.title.clone())
                    .unwrap_or_else(|| rid.to_string()),
                reason: reason.to_string(),
            })
            .collect()
    } else {
        Vec::new()
    };
    rules_not_run.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));

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
    // M11: "clean" means checked and nothing found. A dependency audit that did not run (no
    // network, a timeout, an unreadable result) or that left manifests unaudited used to leave
    // the rows empty and so read as clean. Only a run that proves it checked everything may say
    // so.
    // An AI-only scan never started osv-scanner (`osv_scanner_version` is `None`): it checked
    // nothing, so it cannot say nothing was found.
    let dependency_clean = dep_rows.is_empty()
        && report.provenance.osv_scanner_version.is_some()
        && report
            .coverage_notes
            .iter()
            .filter(|n| n.tool == "osv-scanner")
            .all(crate::dep_audit::note_proves_clean);
    let dependency_snapshot = DependencySnapshotJson {
        rows: dep_rows,
        coverage_notes: dep_coverage_notes,
        clean: dependency_clean,
    };

    // ── Executive summary ──────────────────────────────────────────────────────
    // Bug 4 / C5-4: informational (appendix) AND held (calibration-hedge, any severity) rows are
    // NOT curated action items — they sit outside the four-bucket partition, so `curated_total`
    // excludes BOTH and the self-checking narrative invariant
    // `do_now + do_next + plan + accepted == curated_total` still holds exactly.
    // `held_for_review_total` is the SINGLE reconciling count the executive summary and
    // methodology both show — see `ExecutiveSummaryJson::held_for_review`'s doc comment for why
    // this must be `informational.len() + held.len()`, not `informational.len()` alone (the
    // C5-4 regression this closes): a high/critical calibration hedge never lands in
    // `informational` (the hard "critical/high is never informational" invariant), but it is
    // still held out of every action tier via the separate `held` bucket, and must still count
    // toward "held for review" or the total silently undercounts every needs-review row above
    // medium severity.
    let informational = matrix.informational.len();
    let held = matrix.held.len();
    let held_for_review_total = informational + held;
    let curated_total = code_findings.len() - held_for_review_total;
    let do_now = matrix.do_now.len();
    let do_next = matrix.do_next.len();
    let plan = matrix.plan.len();
    let accepted = matrix.accepted.len();
    // Still-open action items (every informational/held row is Unresolved by construction — see
    // `is_informational`/`is_held_for_review` — so subtracting them keeps `open` an ACTION
    // count, not an appendix one).
    // `Confirmed` counts here for the same reason it counts in `still_open` above: the auditor
    // established the finding is real and nobody has fixed it. Subtracting
    // `held_for_review_total` stays valid because every informational/held row is `Unresolved`
    // by construction (both gates return early on any other disposition), so confirming a row
    // removes it from that bucket before it is ever counted there.
    let open = code_findings
        .iter()
        .filter(|(_, d, _, _)| matches!(d, Disposition::Unresolved | Disposition::Confirmed))
        .count()
        - held_for_review_total;
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
    //
    // C6-B2 design point 3: walk `do_now_sorted` in rank order, but SKIP a row that shares a
    // non-empty root-cause credential token (`FindingRefJson::root_cause_keys`) with a row
    // already selected — the defense-in-depth distinctness check for the case the merge pass
    // (`ai_audit::group_semantic` / `backfill_credential_identity`) conservatively left
    // several sites of ONE leaked credential as separate rows. A skipped row is simply not
    // chosen for THIS box; `do_now_sorted`/`matrix.do_now` are never mutated, so it still ships
    // normally everywhere else the report lists do-now findings.
    //
    // W24 (2026-10-20): the credential-token check above is gated on `!root_cause_keys
    // .is_empty()`, so a row carrying NO token skipped it entirely and was always selected.
    // Three rows of one rule — three tables missing RLS, three endpoints missing one guard —
    // could therefore fill all three slots, which the graders observed as "top-3 listing one
    // root cause three times".
    //
    // The fallback is the RULE ID. For THIS box that is the right granularity even though
    // the rows are all genuine: the box asks the client to do three things this week, and
    // three instances of one rule are one thing ("enable RLS on these tables"). Spending
    // every slot on it hides the other two problems, which is the opposite of what the box
    // is for. Everywhere else the report still lists all of them — `do_now_sorted` and
    // `matrix.do_now` are untouched, exactly as the credential check already promised.
    let mut top3_do_now: Vec<&FindingRefJson> = Vec::new();
    let mut selected_root_causes: std::collections::HashSet<&str> =
        std::collections::HashSet::new();
    let mut selected_rule_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for f in &do_now_sorted {
        if top3_do_now.len() >= 3 {
            break;
        }
        let shares_selected_root_cause = !f.root_cause_keys.is_empty()
            && f.root_cause_keys
                .iter()
                .any(|k| selected_root_causes.contains(k.as_str()));
        if shares_selected_root_cause || selected_rule_ids.contains(f.rule_id.as_str()) {
            continue;
        }
        selected_rule_ids.insert(f.rule_id.as_str());
        top3_do_now.push(f);
        selected_root_causes.extend(f.root_cause_keys.iter().map(String::as_str));
    }
    let dependency_advisories = dependency_snapshot.rows.len();
    // W6: never a silent omission — one explicit sentence per pass that failed/timed out
    // this scan, shared verbatim between the summary and the methodology below.
    // W1: fold in a disclosure for EVERY stage-level ledger integrity gap — every scan-time
    // stage `report.ledger` already carried PLUS this function's own just-recorded
    // report-build-partition stage — deduped against whatever `report.failed_passes` already
    // carries (the real pipeline, `onboard::audit_repos`, pushes scan-time disclosures there
    // itself; a report built directly from a populated ledger, e.g. in tests, has none yet).
    // Rides the SAME existing `FailedPass` mechanism as every other disclosure, rather than a
    // parallel string list.
    let mut failed_passes_for_report = report.failed_passes.clone();
    let repo_label_for_ledger = report.repos.join(", ");
    for stage in report_ledger.stages() {
        if let Some(fp) = crate::scan_ledger::stage_disclosure(&repo_label_for_ledger, stage) {
            if !failed_passes_for_report.contains(&fp) {
                failed_passes_for_report.push(fp);
            }
        }
    }
    let mut failed_pass_notes = failed_pass_disclosures(&failed_passes_for_report, opts.internal);
    // Client-report fix (2026-10-07 grading pass): a rule that declares mechanical/
    // architectural enforcement but has no wired detector IS a real pipeline gap worth
    // disclosing, but it is an ENGINEERING fact, not a client-facing one — the registry names
    // ("arch_checker registry", "gateway rule registry", "Semgrep mapping", ...) and the
    // per-rule repetition belong in the ledger (`camerata inspect` / `render_ledger_summary`)
    // and the logs, never in the deliverable. This used to call `rule_disclosure` once PER
    // excluded rule and push each as its own `FailedPass`, so a corpus with 19 such rules
    // printed 19 internal-vocabulary bullets, duplicated in both the executive summary and
    // methodology (38 bullets total, ~3 pages on a real export). The client gets exactly ONE
    // plain, factual sentence instead, and only when the count is nonzero.
    let no_wired_detector_count = report_ledger
        .rules_merged()
        .filter(|r| {
            !r.ran
                && r.skip_reason
                    .as_deref()
                    .unwrap_or("")
                    .contains(crate::scan_ledger::NO_WIRED_DETECTOR_REASON)
        })
        .count();
    if no_wired_detector_count > 0 {
        failed_pass_notes.push(format!(
            "{} could not be mechanically checked this run and {} reviewed by the model \
             instead.",
            noun(no_wired_detector_count, "rule", "rules"),
            if no_wired_detector_count == 1 {
                "was"
            } else {
                "were"
            },
        ));
    }
    // W7 (G1/G3): non-dependency tool-coverage gaps reach the client artifacts — one concise
    // line per affected tool, through the SAME failed_passes channel the template already
    // renders in both the executive summary and the methodology. See
    // `tool_coverage_disclosures`'s doc comment for the honesty hole this closes.
    failed_pass_notes.extend(tool_coverage_disclosures_for(
        &report.coverage_notes,
        &report_ledger,
    ));
    let (narrative, is_override) = match &opts.executive_summary_override {
        Some(text) if !text.trim().is_empty() => (text.clone(), true),
        _ => (
            default_narrative(
                review_state,
                report.files_scanned,
                candidates_reviewed,
                excluded_fp,
                curated_total,
                held_for_review_total,
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
        held_for_review: held_for_review_total,
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
        per_repo: if report.repos.len() > 1 {
            report
                .volume_by_repo
                .iter()
                .map(|v| CodeVolumeRepoJson {
                    repo: v.repo.clone(),
                    files: v.files,
                    lines: v.lines,
                })
                .collect()
        } else {
            Vec::new()
        },
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
        model_backend: opts
            .model
            .as_ref()
            .map(|m| format!("{} ({})", m.backend, m.provider)),
        camerata_version: report.provenance.camerata_version.clone(),
        engine_commit: crate::ENGINE_COMMIT.map(str::to_string),
        corpus_version: corpus.map(camerata_rules::corpus_version),
        // The SCAN time, not the wall clock at serialization — see `generated_at_label`.
        generated_at: generated_at_label(&report.provenance),
        client_name: clean_client_name(&opts.client_name),
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
        incremental_note: report.ledger.incremental().map(incremental_cover_note),
        internal_incomplete: internal_incomplete_marking(report, opts),
    };

    // ── Methodology (M4: ported from the better sample copy, no dashes per S12) ────────
    // A project of several repos is several independent services, so the prose that speaks of
    // "the repository" or "the codebase" speaks of the repositories and the services instead.
    let multi_repo = report.repos.len() > 1;
    let methodology = MethodologyJson {
        candidates_reviewed,
        excluded_false_positive: excluded_fp,
        held_for_review: held_for_review_total,
        next_steps: next_steps_note_for(
            multi_repo,
            review_state,
            candidates_reviewed,
            curated_total,
            held_for_review_total,
            excluded_fp,
        ),
        deterministic_note:
            "Camerata runs a two-tier engine. A deterministic security floor (proven-defect \
             SAST rules plus a migration-timeline replay for Supabase Row Level Security) \
             produces findings that either hold or do not, with no model judgment. These are \
             labeled deterministic in the report."
                .to_string(),
        ai_tier_note: ai_tier_note_for(
            review_state,
            coverage_gaps::repos_without_ai_review(report).len(),
            report.repos.len(),
        ),
        citation_note: RELATED_STANDARD_NOTE.to_string(),
        not_done: vec![
            "No penetration testing or live exploitation was performed.".to_string(),
            if multi_repo {
                "No runtime or dynamic analysis; findings derive from the static state of the \
                 repositories."
                    .to_string()
            } else {
                "No runtime or dynamic analysis; findings derive from static repository state."
                    .to_string()
            },
            if multi_repo {
                "Row Level Security findings reflect each repository's migration history, not \
                 the live databases. Changes made in the Supabase dashboard that never landed \
                 in a migration are invisible to a repository scan and must be confirmed \
                 against production."
                    .to_string()
            } else {
                "Row Level Security findings reflect the repository's migration history, not the \
                 live database. Changes made in the Supabase dashboard that never landed in a \
                 migration are invisible to a repository scan and must be confirmed against \
                 production."
                    .to_string()
            },
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

    // WO9 (design 3B item 1): the escalations and one pattern for each sub-group, worked out from
    // the reviewed report. The sentence is derived from the tree being exported, never copied from
    // what stage 2 recorded.
    let (escalated_items, sub_group_patterns) =
        crate::xlsx_export::pdf_sections(reviewed, corpus, &opts.chosen_options);
    // (The reviewed report's ledger block is already derived from its tree by `apply_overlay`.)
    let escalation_sentence = report
        .ledger
        .escalation()
        .map(crate::scan_ledger::EscalationBlock::sentence)
        .unwrap_or_default();
    // Amendment A14 item 4: above the limit only the reshaped PDF is rendered. The per-site
    // cards, the held-for-review cards and the matrix lists are left out, and the matrix's counts
    // stay so the release gate can still reconcile the PDF with the other files.
    let site_total: usize = code_findings
        .iter()
        .map(|(f, _, _, _)| f.distinct_site_count())
        .sum();
    let reduced = site_total > FULL_PDF_SITE_LIMIT;
    let mut reduced_matrix = Vec::new();
    if reduced {
        for (bucket, list) in [
            ("do_now", &matrix.do_now),
            ("do_next", &matrix.do_next),
            ("plan", &matrix.plan),
            ("accepted", &matrix.accepted),
            ("informational", &matrix.informational),
            ("held", &matrix.held),
        ] {
            let mut counts: std::collections::BTreeMap<(String, String), usize> =
                std::collections::BTreeMap::new();
            for f in list {
                *counts
                    .entry((f.severity.clone(), f.kind.tag().to_string()))
                    .or_default() += 1;
            }
            reduced_matrix.extend(counts.into_iter().map(|((severity, kind), count)| {
                MatrixTallyJson {
                    bucket: bucket.to_string(),
                    severity,
                    kind,
                    count,
                }
            }));
        }
        matrix = MatrixJson::default();
        curated_findings = Vec::new();
        held_for_review_findings = Vec::new();
    }

    AuditReportJson {
        internal: opts.internal,
        site_total,
        reduced,
        reduced_matrix,
        escalations: EscalationsJson {
            sentence: escalation_sentence,
            items: escalated_items,
        },
        sub_group_patterns,
        sub_group_patterns_note: crate::xlsx_export::no_tree_note(reviewed).to_string(),
        review_state,
        brand_mark: brand_kit_matches(cover.brand.as_deref()),
        confidentiality: confidentiality_marking(&report.provenance, opts),
        cover,
        executive_summary,
        three_things,
        scorecard: ScorecardJson {
            rows: scorecard_rows,
        },
        matrix,
        priority_grid,
        curated_findings,
        held_for_review_findings,
        patterns_across_services: patterns_across_services_json(report, corpus),
        whats_healthy: WhatsHealthyJson {
            rules: healthy_rules,
            further_clean_count,
            dependency_clean,
        },
        rules_not_run,
        dependency_snapshot,
        methodology,
        coverage_gaps: coverage_gaps::coverage_gaps(report),
        disclaimer: audit_report_disclaimer(report.repos.len()).to_string(),
    }
}

// ── Typst compile ────────────────────────────────────────────────────────────────

/// The UNIX timestamp to stamp into the compiled PDF, read back out of the report's OWN stated
/// generation time (`cover.generated_at`, which [`generated_at_label`] derived from the scan
/// time).
///
/// Reading it back from the cover rather than taking a second `&ScanProvenance` parameter is
/// deliberate: it makes the PDF's metadata and the PDF's printed cover line the same fact by
/// construction, so they cannot disagree. The string was written in [`GENERATED_AT_FORMAT`],
/// so the round-trip is exact; `None` only if a caller hand-built an `AuditReportJson` with
/// some other string in that field.
fn creation_epoch(data_json: &str) -> Option<i64> {
    let data: serde_json::Value = serde_json::from_str(data_json).ok()?;
    let generated_at = data.get("cover")?.get("generated_at")?.as_str()?;
    chrono::NaiveDateTime::parse_from_str(generated_at.trim(), GENERATED_AT_FORMAT)
        .ok()
        .map(|naive| naive.and_utc().timestamp())
}

/// Serialize the PDF's data: the ONE `data.json` the Typst template reads. The release gate
/// inspects these exact bytes, so what it checks is byte-identical to what Typst renders.
pub fn render_data_json(json: &AuditReportJson) -> anyhow::Result<String> {
    serde_json::to_string(json).context("could not serialize the report to JSON")
}

/// [`compile_pdf_from_data`] over a structured report. For tests and diagnostics: production
/// exports go through `release_gate::build_product_package`, which keeps the `data.json` bytes
/// so the release gate can inspect them (a source-grep test pins that).
pub async fn compile_pdf(json: &AuditReportJson) -> anyhow::Result<Vec<u8>> {
    compile_pdf_from_data(&render_data_json(json)?).await
}

// Test-only seam: a test that needs a different `typst` (a recording fake) scopes it onto ITS
// OWN task with `TYPST_PROGRAM.scope(path, fut)`. The override is task-local, so it can neither
// leak into a concurrently running test nor require changing the process-wide `PATH`, which is
// what made unrelated typst-using tests flaky when a sibling swapped `PATH` for a fake.
#[cfg(test)]
tokio::task_local! {
    pub(crate) static TYPST_PROGRAM: std::path::PathBuf;
}

/// The `typst` executable to run: the task-local test override if one is scoped, else `typst`
/// from `PATH`.
fn typst_program() -> std::ffi::OsString {
    #[cfg(test)]
    if let Ok(p) = TYPST_PROGRAM.try_with(|p| p.clone()) {
        return p.into_os_string();
    }
    "typst".into()
}

/// The files the template reads besides `data.json`, written next to it for every compile: the
/// embedded fonts (W27: Inter for text, JetBrains Mono for code, both SIL OFL, licence files
/// beside them in `templates/fonts/`) and the brand kit's two logo marks. Embedded in the
/// binary like the template itself, so nothing has to ship alongside the server and the PDF
/// never depends on what fonts the exporting machine happens to have installed.
const TEMPLATE_ASSETS: [(&str, &[u8]); 7] = [
    (
        "fonts/Inter-Regular.ttf",
        include_bytes!("../templates/fonts/Inter-Regular.ttf"),
    ),
    (
        "fonts/Inter-Italic.ttf",
        include_bytes!("../templates/fonts/Inter-Italic.ttf"),
    ),
    (
        "fonts/Inter-SemiBold.ttf",
        include_bytes!("../templates/fonts/Inter-SemiBold.ttf"),
    ),
    (
        "fonts/Inter-SemiBoldItalic.ttf",
        include_bytes!("../templates/fonts/Inter-SemiBoldItalic.ttf"),
    ),
    (
        "fonts/JetBrainsMono-Regular.ttf",
        include_bytes!("../templates/fonts/JetBrainsMono-Regular.ttf"),
    ),
    (
        "brand-mark.svg",
        include_bytes!("../../../docs/design/brand/cantus-logo-mark.svg"),
    ),
    (
        "brand-mark-small.svg",
        include_bytes!("../../../docs/design/brand/cantus-logo-mark-small.svg"),
    ),
];

/// Which fonts Typst may draw from. Production uses [`FontSources::BundledFirst`]; the
/// embedding test uses [`FontSources::BundledOnly`] to prove the bundle alone renders the
/// report on a machine with no Inter or JetBrains Mono installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontSources {
    /// `--font-path fonts`: Typst searches the bundled files BEFORE system fonts, so Inter
    /// and JetBrains Mono always come from the bundle, and a glyph neither has (a CJK path, an
    /// emoji) still falls back to a system font instead of rendering as a missing box.
    ///
    /// Production stays on this, deliberately: every character the bundled fonts cover is drawn
    /// from them on any machine, and forcing [`FontSources::BundledOnly`] would silently drop
    /// the characters they lack (the multi-byte path test pins that a report keeps an accented,
    /// CJK or emoji path and snippet). Installed fonts reach the output only for such a glyph.
    BundledFirst,
    /// `--font-path fonts --ignore-system-fonts`.
    BundledOnly,
}

/// Compile `data_json` (from [`render_data_json`]) into a PDF via a bundled Typst template.
/// Self-contained: the template
/// text is embedded in the binary (`include_str!`), so no external file needs to ship
/// alongside the server. Fails soft with an actionable message when `typst` isn't on PATH
/// (matches the dep-audit coverage-note tone: name the tool, name the fix).
///
/// Runs `typst compile report.typ report.pdf --root <tmpdir>` in a scratch temp dir (RAII
/// cleanup on drop) with a time budget that scales with the report (see [`typst_timeout`]),
/// with `SOURCE_DATE_EPOCH` pinned to the report's own
/// generation time (see [`creation_epoch`]). The template reads `data.json` via Typst's own
/// `json()` builtin — no `--input` plumbing needed.
pub async fn compile_pdf_from_data(data_json: &str) -> anyhow::Result<Vec<u8>> {
    let (pdf, warnings) = run_typst(data_json, FontSources::BundledFirst).await?;
    if !warnings.trim().is_empty() {
        tracing::warn!(%warnings, "typst reported warnings while compiling the report");
    }
    Ok(pdf)
}

/// The least time Typst is given to compile a report, however small.
pub(crate) const TYPST_TIMEOUT_BASE: Duration = Duration::from_secs(30);

/// Time added for every site the PDF prints. Measured, not guessed: a synthetic report of
/// 30,000 sites compiled in about 22 seconds on a development laptop (0.75 ms a site, linear
/// from 2,000 sites up), so 5 ms a site is about six times what the measurement needs and
/// leaves room for a slower machine.
pub(crate) const TYPST_TIMEOUT_PER_SITE: Duration = Duration::from_millis(5);

/// The most time Typst is ever given. A report that needs more than this is too large to render
/// as one PDF, and the export says so rather than waiting on it.
pub(crate) const TYPST_TIMEOUT_CEILING: Duration = Duration::from_secs(15 * 60);

/// How long Typst may take to compile a report that prints `sites` sites: [`TYPST_TIMEOUT_BASE`]
/// plus [`TYPST_TIMEOUT_PER_SITE`] for each, never more than [`TYPST_TIMEOUT_CEILING`].
pub(crate) fn typst_timeout(sites: usize) -> Duration {
    let per_site = u32::try_from(sites)
        .ok()
        .and_then(|n| TYPST_TIMEOUT_PER_SITE.checked_mul(n));
    match per_site.and_then(|extra| TYPST_TIMEOUT_BASE.checked_add(extra)) {
        Some(total) => total.min(TYPST_TIMEOUT_CEILING),
        None => TYPST_TIMEOUT_CEILING,
    }
}

/// How many sites the PDF prints: every site on a curated card and on a held-for-review card.
/// The size the time budget scales with. Zero for data that does not parse, which gets the base
/// budget; a malformed document fails in Typst quickly, not slowly.
fn pdf_site_count(data_json: &str) -> usize {
    #[derive(serde::Deserialize, Default)]
    struct Group {
        #[serde(default)]
        sites: Vec<serde::de::IgnoredAny>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Probe {
        #[serde(default)]
        curated_findings: Vec<Group>,
        #[serde(default)]
        held_for_review_findings: Vec<Group>,
    }
    serde_json::from_str::<Probe>(data_json)
        .map(|p| {
            p.curated_findings
                .iter()
                .chain(&p.held_for_review_findings)
                .map(|g| g.sites.len())
                .sum()
        })
        .unwrap_or(0)
}

/// The error for a compile that outlived its budget: the size of the report, the time it was
/// given and, when that was the ceiling, that the report is too large to render as one PDF.
fn typst_timeout_message(sites: usize, allowed: Duration) -> String {
    let secs = allowed.as_secs();
    if allowed >= TYPST_TIMEOUT_CEILING {
        format!(
            "typst compile timed out after {secs}s on a report of {sites} sites: that is the \
             ceiling ({}s) and the report is too large to render as one PDF. No PDF was produced.",
            TYPST_TIMEOUT_CEILING.as_secs()
        )
    } else {
        format!(
            "typst compile timed out after {secs}s on a report of {sites} sites (the time \
             allowed for that size: {}s plus {} ms a site). No PDF was produced.",
            TYPST_TIMEOUT_BASE.as_secs(),
            TYPST_TIMEOUT_PER_SITE.as_millis()
        )
    }
}

/// The compile itself: the PDF bytes and Typst's stderr (its warnings, on success). Given the
/// time [`typst_timeout`] allows for a report of this size.
async fn run_typst(data_json: &str, fonts: FontSources) -> anyhow::Result<(Vec<u8>, String)> {
    let allowed = typst_timeout(pdf_site_count(data_json));
    run_typst_within(data_json, fonts, allowed).await
}

/// [`run_typst`] with an explicit time budget. A compile that exceeds it is killed and fails the
/// export: whatever Typst had written is never read, so a partial PDF cannot ship.
async fn run_typst_within(
    data_json: &str,
    fonts: FontSources,
    allowed: Duration,
) -> anyhow::Result<(Vec<u8>, String)> {
    let tmp = tempfile::tempdir().context("could not create a temp dir for the PDF export")?;
    let data_path = tmp.path().join("data.json");
    let template_path = tmp.path().join("report.typ");
    let pdf_path = tmp.path().join("report.pdf");

    tokio::fs::write(&data_path, data_json)
        .await
        .context("could not write data.json")?;
    tokio::fs::write(
        &template_path,
        include_str!("../templates/audit_report.typ"),
    )
    .await
    .context("could not write report.typ")?;
    tokio::fs::create_dir_all(tmp.path().join("fonts"))
        .await
        .context("could not create the fonts dir for the PDF export")?;
    for (name, bytes) in TEMPLATE_ASSETS {
        tokio::fs::write(tmp.path().join(name), bytes)
            .await
            .with_context(|| format!("could not write {name}"))?;
    }

    let mut cmd = tokio::process::Command::new(typst_program());
    cmd.current_dir(tmp.path())
        .arg("compile")
        .arg("report.typ")
        .arg("report.pdf")
        .arg("--root")
        .arg(tmp.path())
        .arg("--font-path")
        .arg(tmp.path().join("fonts"))
        .kill_on_drop(true);
    if fonts == FontSources::BundledOnly {
        cmd.arg("--ignore-system-fonts");
    }
    // Reproducibility: pin the PDF's creation timestamp instead of letting Typst read the wall
    // clock. Typst stamps that clock into FIVE places — the Info dict's `/CreationDate` and
    // `/ModDate`, the XMP packet's `xmp:CreateDate`, `xmp:ModifyDate`, `xmpMM:InstanceID` and
    // `xmpMM:DocumentID`, and the trailer's `/ID` (which it derives from content AND time) — so
    // two compiles of one report were never byte-identical and the product could not prove two
    // deliverables were the same report.
    //
    // `SOURCE_DATE_EPOCH` (the reproducible-builds standard) rather than the equivalent
    // `--creation-timestamp` flag: both exist on the installed Typst 0.15.0 and the flag is
    // documented as reading this very variable as its default, but an env var a Typst that
    // does not support the feature simply ignores, where an unrecognised ARGUMENT makes it exit
    // non-zero. A client machine on an older Typst degrades to today's behaviour instead of
    // failing the export outright.
    //
    // Verified against the installed Typst 0.15.0, not assumed: two compiles of identical
    // source two seconds apart are byte-for-byte identical with the variable set (XMP packet,
    // Info dict and trailer `/ID` all included) and differ without it.
    //
    // Set UNCONDITIONALLY. On the degenerate path where the report carries no parseable time,
    // an explicit `now` is still better than leaving the variable unset, because the server
    // process could otherwise inherit an unrelated `SOURCE_DATE_EPOCH` from its environment and
    // silently stamp a client deliverable with a build timestamp from somewhere else entirely.
    let epoch = creation_epoch(data_json).unwrap_or_else(|| chrono::Utc::now().timestamp());
    cmd.env("SOURCE_DATE_EPOCH", epoch.to_string());

    let output = match tokio::time::timeout(allowed, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("Install Typst: brew install typst");
        }
        Ok(Err(e)) => return Err(anyhow::Error::new(e).context("could not run typst")),
        Err(_elapsed) => anyhow::bail!(typst_timeout_message(pdf_site_count(data_json), allowed)),
    };

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        anyhow::bail!("typst compile failed: {stderr}");
    }

    tokio::fs::read(&pdf_path)
        .await
        .map(|pdf| (pdf, stderr))
        .context("typst reported success but report.pdf was not found")
}

pub mod coverage_gaps;
pub(crate) mod fix_text;
pub use coverage_gaps::{CoverageGapsJson, SkippedFileJson};

pub mod decisions_file;
pub mod overlay;
pub mod prose_checks;
pub mod row_facts;
pub mod tree_view;
pub use overlay::{
    apply_overlay, edit_ids_of, is_reviewer_edited, key_for_added, re_mark_line,
    summary_fingerprint, AppliedEdit, MergeNote, ReviewedReport, StaleEditNote, SupersededNote,
};
pub use row_facts::{internal_triage, site_id, InternalTriage, RowFacts, RowFactsIndex};

#[cfg(test)]
mod coverage_gaps_tests;

#[cfg(test)]
mod wo9 {
    mod decisions_file_tests;
    mod edit_ids_tests;
    mod pdf_tests;
    mod plan_tests;
    mod review_level_tests;
    mod row_set_tests;
    mod signature_tests;
    mod sites_tests;
    mod stage2_tests;
    pub(super) mod support;
    mod tree_tests;
    mod workbook_tests;
}

#[cfg(test)]
mod evidence_hold_tests;

#[cfg(test)]
mod engine_identity_tests;

#[cfg(test)]
mod fix_text_tests;

#[cfg(test)]
mod fn_batch_b_tests;

#[cfg(test)]
mod fn_batch_c_adjust_tests;

#[cfg(test)]
mod fn_batch_c_dep_tests;

#[cfg(test)]
mod fn_batch_c_followup_tests;

#[cfg(test)]
mod fn_batch_c_incremental_tests;

#[cfg(test)]
mod fn_batch_c_sites_tests;

#[cfg(test)]
mod fn_batch_a_followup_tests;

#[cfg(test)]
mod fn_batch_c_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod export_invariants_gate;
