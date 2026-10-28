//! Excel workbook serializer for the product export (audit-report hardening, Pass C).
//!
//! See `docs/design/2026-07-27_product-export.md`. This module is the Excel sibling of
//! [`crate::report_export`]'s PDF serializer: [`build_workbook`] is pure aside from the
//! in-memory `rust_xlsxwriter` buffer, unit-tested, and re-derives everything from
//! `last_scan` + the client's POSTed dispositions on every call — nothing is persisted
//! server-side, matching `report_export`'s own contract.
//!
//! # The workbook must never disagree with the PDF
//! Both artifacts are serializers over the SAME `ScanReport` + dispositions + corpus. This
//! module does NOT re-derive classification math independently — it calls the exact same
//! `pub(crate)` helpers `report_export::build_report_json` uses internally (`classify`,
//! `normalize_severity`, `category_for`, `effective_bucket`, `disposition_label`,
//! `client_headline_and_detail`, `resolve_citation`, `resolve_fix`, `effort_hours_bounds`,
//! `finding_key`, `bucket_title`, `provenance_tier_rank`, `Disposition`). A finding's severity,
//! category, matrix bucket, disposition label, citation, and recommended fix are computed
//! identically in both places; only the OUTPUT SHAPE differs. C3-6 (2026-10-01): `bucket` in
//! particular used to be this module's OWN `matrix_bucket(...)` call, with none of
//! `report_export::build_report_json`'s `is_informational`/`is_uncited_ai_finding` gates
//! applied first — a hedged or uncited-AI row could therefore sit in an action tier here while
//! the PDF/JSON correctly held it out, and the three artifacts' bucket counts could disagree.
//! `effective_bucket` (which now folds those two gates in) is the fix: this module calls that
//! one function, never `matrix_bucket` directly. W3 (2026-09-30), extended C4-P2 (2026-10-01):
//! the headline AND detail both go through `client_headline_and_detail` — the SAME
//! plain-language rewrite the PDF uses (preferring a floor rule's authored
//! `finding_headline`/`finding_detail` template, falling back to `defect_headline`/the raw
//! `detail` only when no template is authored) — so the product export (`findings.json` /
//! xlsx) can never leak raw internal gate prose ("Deny…"/"Require…") that the PDF has already
//! rewritten. Before C4-P2, only the headline went through the rewrite; `FindingRow::detail`
//! took the raw `finding.detail` unconditionally, so a floor rule with an authored
//! `finding_detail` template rewrote cleanly in the PDF's `CuratedSiteJson::detail` while
//! `findings.json`/the workbook still printed the gate's own enforcement prose for the exact
//! same finding. `FindingRow::detail` is NOT an internal-completeness field — the raw
//! `finding.detail` lives only on `Finding` itself (never re-exported verbatim here).
//!
//! C4-P2 also extends this module's citation/category/confidence computation to call
//! `citation_for_finding`/`category_for_finding`/`hedge_confidence` (not the plain
//! `resolve_citation`/`category_for`/a bare `finding.confidence` read) — see those functions'
//! doc comments for the AI-tier citation/category/hedge-consistency bugs this closes.
//!
//! # Why this is fed from `ScanReport`, not `AuditReportJson`
//! `AuditReportJson` already excludes false positives, caps snippets/what's-healthy, and
//! partitions dependency findings into a separate lane — exactly the curation the PDF wants.
//! The workbook's whole point is the UNCUT dataset (false positives get their own sheet with
//! reasons, snippets are capped only defensively against Excel's cell limit, every audited
//! rule appears in Coverage regardless of cap). So [`build_workbook`] runs its OWN partition
//! pass over `report.findings`, reusing only the low-level pure functions above — never the
//! already-curated `AuditReportJson` type.
//!
//! # Pass 3 — the workbook is partitioned by KIND, not flattened onto one sheet
//! The workbook was designed on a BINARY premise: a row is a violation or it is not. An
//! inspection now returns five kinds of statement with genuinely different payloads
//! ([`camerata_api_types::audit::FindingKind`]), and W3 carried them here by appending a
//! single `Kind` column to the one findings sheet — a correct minimal carry, explicitly
//! deferring the redesign to Pass 3. This is that redesign, and its rule is: a row renders
//! where ITS OWN columns mean something, and in exactly one place.
//!
//! | Kind | Sheet | Why there |
//! |---|---|---|
//! | `Defect`, `Deviation`, absent kind | **All Findings** (+ per-area views) | something is wrong at a site. An ABSENT kind lands here deliberately: an unmapped emission path must surface among the accusing rows, never be filed into an informational tab. |
//! | `Deviation` (again, per RULE) | **Deviations** | the deviation payload is GROUP-level by construction (`deviating_sites` / `assessed_sites` repeat verbatim on every row of the rule). One row per rule states "N of M sites deviate from <convention>" from columns, instead of repeating the same two numbers down N rows. NOT a filtered duplicate of All Findings — a different grain. |
//! | `ConventionObserved`, `NoConvention` | **Conventions** | nothing is wrong at a site. This is the derived decision set (A5.5) that becomes the client's `AGENTS.md`, so it is a shipped deliverable, not filler. |
//! | `NotAssessed` | **Coverage** | "armed but not evaluable" is a coverage disclosure, not a finding. It gets honest billing next to the tool-coverage gaps and the never-run rules, not equal billing with defects. |
//!
//! Row conservation across that partition is a test, not a hope:
//! `req_pass3_every_exported_row_lands_on_exactly_one_sheet`. It is also what keeps the
//! self-measurement parity harness honest — that harness requires every `findings.json` row's
//! headline to appear verbatim somewhere in the workbook, so a kind whose sheet forgot to
//! render a headline column would be a PARITY FAULT, by name.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Context;
use camerata_api_types::audit::{FindingKind, NotAssessedReason};
use rust_xlsxwriter::{
    Color, ConditionalFormatText, ConditionalFormatTextRule, Format, FormatAlign, Workbook,
    Worksheet, XlsxError,
};
use serde::Serialize;

use crate::dep_audit::DEP_AUDIT_RULE_ID;
use crate::onboard::ScanReport;
use crate::report_export::{
    bucket_title, category_for, category_for_finding, citation_for_finding, classify,
    client_headline_and_detail, disposition_label, effective_bucket, effort_hours_bounds,
    finding_key, hedge_confidence, is_hedged, normalize_severity, provenance_tier_rank,
    resolve_citation, resolve_effort, resolve_fix, sanitize_report_findings, title_for_finding,
    Disposition, DispositionWire, ReportOptions,
};

// ── Row model (the intermediate shape shared by every findings-style sheet) ────────

/// One row of the "All Findings" / per-category / "False Positives" sheets — the ~21-column
/// spec from the design doc, PLUS the Fix-specific and Recommended-Fix columns (approved
/// additions). Computed
/// once per non-dependency [`crate::onboard::Finding`] in [`partition_rows`], then written
/// verbatim by every sheet writer (never re-derived per sheet).
///
/// `Serialize`: this is ALSO the row shape `findings.json` (the product export's
/// machine-readable sibling of the workbook, see [`build_findings_export`]) serializes
/// verbatim — the exact same struct, not a parallel DTO, so the JSON and the xlsx can never
/// disagree on a field. Field names are the stable wire contract for `findings.json`; treat
/// a rename here as a breaking change for anything that parses that file.
#[derive(Serialize)]
pub struct FindingRow {
    severity: String,
    headline: String,
    repo: String,
    path: String,
    line: usize,
    rule_id: String,
    category: String,
    /// `"do_now"` | `"do_next"` | `"plan"` | `"accepted"` | `"informational"` | `"held"` (C5-4;
    /// matches [`effective_bucket`] — C3-6), or empty for a false-positive row (a FP has no
    /// matrix cell — it is excluded from the matrix entirely, same as the PDF).
    /// `"informational"` is the severity-capped (≤ medium) conventions-to-consider appendix;
    /// `"held"` is a calibration hedge at ANY severity (C5-4). Both are held-for-review
    /// destinations the PDF/JSON route a hedged or uncited-AI finding to, never an action tier —
    /// see the Index sheet's disposition summary line.
    bucket: &'static str,
    /// The classified [`Disposition`] this session, or `None` for a false-positive row
    /// (`classify` must never be called on an FP-dispositioned finding — see
    /// `report_export::classify`'s doc comment). Drives the Index sheet's disposition
    /// summary line and each category sheet's tab color / worst-open-severity ranking.
    disposition_kind: Option<Disposition>,
    disposition_label: String,
    effort: String,
    est_hours: String,
    /// C4-P2: the CANONICAL confidence (`report_export::hedge_confidence`), not a raw read of
    /// `Finding::confidence` — `"needs-review"` whenever `needs_review` below is `true`, and
    /// never otherwise (a strict biconditional, enforced at the one point both fields are set).
    confidence: String,
    /// C4-P2 / C5-4: the CANONICAL hedge flag (`report_export::is_hedged`) — `true` whenever
    /// this row's own calibration confidence says `"needs-review"` OR `bucket` above is
    /// `"informational"` or `"held"` (being held for review is itself a hedge, regardless of
    /// which signal routed it there).
    needs_review: bool,
    in_test: bool,
    /// `"Deterministic"` | `"Preview: {tool}"` | `"AI-advisory"` — derived from
    /// `resolve_citation(...).kind`, per the design doc's Provenance column.
    provenance: String,
    /// C3-6: the RAW citation kind (`"grounded"` | `"preview"` | `"advisory"`) behind
    /// `provenance`'s display label above — never serialized (the label is the wire value for
    /// `findings.json`); kept only so the row sort below can call the SAME
    /// `report_export::provenance_tier_rank` the within-bucket matrix ordering uses, rather
    /// than re-deriving a rank from the formatted label string.
    #[serde(skip)]
    citation_kind: String,
    citation_label: String,
    /// Newline-separated citation source URLs (plain text, not hyperlink objects — a cell
    /// can carry several URLs).
    citation_urls: String,
    /// C4-P4 (residual defect 2): human-readable rule TITLES (never raw rule ids) — see
    /// `report_export::also_matches_titles`'s doc comment. This is ALSO the `findings.json`
    /// "also_matches" field (this struct serializes verbatim), so the raw-id leak closed in the
    /// PDF's "Also violates" line had a second, independent instance right here.
    also_matches: String,
    status: String,
    snippet: String,
    detail: String,
    /// **P2 (2026-09-29): the PRIMARY fix, shown FIRST** — a codebase-specific remediation
    /// for THIS finding, generated at scan time by
    /// [`crate::ai_audit::generate_fix_specifics`] (see `report_export::CuratedSiteJson::
    /// fix_for_this_finding`'s doc comment, which this field mirrors verbatim). `None` when
    /// fix-generation never ran (a deterministic-only scan) or gave up after retries — the
    /// latter is a pipeline gap only (C3-1b), never a hedge; the `fix` column below still
    /// carries the rule's authored remediation in that case.
    fix_specific: Option<String>,
    /// The rule's AUTHORED, client-facing remediation text (see [`resolve_fix`]), with this
    /// finding's placeholder tokens already substituted — SECONDARY context now that
    /// `fix_specific` above is the primary fix. `None` — rendered as a blank cell / JSON
    /// `null`, never fabricated and never `directive` — when the rule's default option has no
    /// authored `remediation` yet.
    fix: Option<String>,
    is_fp: bool,
    /// The auditor's FP reason (`DispositionWire.reason`), populated only when `is_fp`.
    fp_reason: String,
    /// Pass-1 W7 (C5): every evidence site a merge ABSORBED into this row
    /// (`Finding::also_locations`, verbatim). Before W7 this field reached NO artifact, so a
    /// row representing three detected sites exported as one line at one `path:line` — an
    /// over-merge was observationally identical to a detection miss (tracker C5: the client
    /// was told there is one site; there were three; the other two reached no artifact).
    /// Serialized into `findings.json` and rendered in the workbook's "Also found at"
    /// column, so every absorbed site is observable in the detail artifacts.
    also_locations: Vec<crate::onboard::MergedLocation>,
    /// Pass-1 W7 (C5): the total number of distinct evidence sites this row represents —
    /// its own `path:line` plus every absorbed `also_locations` entry. `1` for an un-merged
    /// row. The single number that makes "one row, N sites" impossible to misread as "one
    /// row, one site".
    site_count: usize,
    /// Pass-1 W3 (A5.6): what this row IS — flattened
    /// [`camerata_api_types::audit::KindOnWire`], so `findings.json` (which serializes this
    /// struct verbatim) carries a flat `"kind"` discriminator plus the variant's payload
    /// fields (a Deviation's numerator/denominator/convention) on EVERY row; an absent kind
    /// renders the explicit `"unspecified"` tag, never a silent `"defect"`. The grader reads
    /// findings.json — a kind that reached only the PDF would be the exact B2 defect class
    /// that produced the hearth 0/12 result.
    #[serde(flatten)]
    kind: camerata_api_types::audit::KindOnWire,
}

/// One row of the `Dependencies` sheet — the `DEP_AUDIT_RULE_ID` carve-out, same as the
/// PDF's own dependency-snapshot section.
#[derive(Clone, Serialize)]
struct DepRow {
    package: String,
    advisory: String,
    severity: String,
    repo: String,
    /// W33: the patched version OSV reports, if any. `None` ⇒ no fix is available yet.
    fixed_version: Option<String>,
    /// W33 ownership tag: `client_owned` when a patch exists (upgrade is the fix) or
    /// `external_dependency` when none does (not-yet-fixable — the "outside your control" class).
    ownership: String,
    /// W33: the realistic action when the CVE is not yet fixable; empty when it is (upgrade).
    action: String,
}

/// One row of the `Coverage` sheet: an audited rule id, uncapped (the PDF's what's-healthy
/// section caps at 10; the workbook doesn't need to).
#[derive(Serialize)]
struct CoverageRow {
    rule_id: String,
    title: String,
    category: String,
    citation_kind: String,
    citation_label: String,
    findings_count: usize,
}

/// One row of the Coverage sheet's "Rules applied" appendix (P7 — evidence-based alternative
/// selection): a client-visible audit trail of every multi-option rule this scan decided an
/// alternative for, so a WRONG pick costs a glance at this table rather than a false finding
/// buried in the report. Built from `ScanReport::recommendations` — see
/// [`build_rules_applied_rows`].
#[derive(Serialize)]
struct RulesAppliedRow {
    rule_id: String,
    /// The human label of the chosen option (resolved against the corpus when available,
    /// else the bare option id) — or an explicit "Not applicable" note when the rule's
    /// concern was judged not to apply to this codebase at all (P7: `applicable == false`),
    /// in which case NO option was actually checked against.
    chosen: String,
    /// The `file:line` evidence backing the pick, or a blank cell when none was recorded
    /// (an operator-forced pick, a not-applicable rule, or a pre-P7 recommendation).
    evidence: String,
    applicable: bool,
}

/// Build the Coverage sheet's "Rules applied" appendix rows from every recommendation this
/// scan recorded (`ScanReport::recommendations` — one entry per multi-option rule the audit
/// decided an alternative for). Sorted by rule id for a deterministic, diffable sheet.
fn build_rules_applied_rows(
    report: &ScanReport,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<RulesAppliedRow> {
    let mut rows: Vec<RulesAppliedRow> = report
        .recommendations
        .values()
        .map(|rec| {
            let chosen = if !rec.applicable {
                "Not applicable — no evidence this concern applies here".to_string()
            } else {
                corpus
                    .and_then(|c| c.get_by_id(&rec.rule_id))
                    .and_then(|r| {
                        r.options
                            .iter()
                            .find(|o| o.id == rec.recommended_option_id)
                            .map(|o| o.label.clone())
                    })
                    .unwrap_or_else(|| rec.recommended_option_id.clone())
            };
            RulesAppliedRow {
                rule_id: rec.rule_id.clone(),
                chosen,
                evidence: rec.evidence.clone().unwrap_or_default(),
                applicable: rec.applicable,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
    rows
}

/// One row of the Index sheet's hyperlinked table of contents.
struct SheetSummary {
    display_name: String,
    sheet_name: String,
    count: usize,
    worst_severity: String,
}

// ── Partition (mirrors `build_report_json`'s partition loop, sharing its primitives) ──

/// Partition `report.findings` into per-finding rows (everything except the dependency
/// carve-out) + dependency rows, reusing the exact same classification/citation/fix helpers
/// `report_export::build_report_json` calls. False positives are KEPT here (with their
/// reason) rather than dropped — the workbook's False Positives sheet is where they surface.
///
/// Rows are returned PRE-SORTED (severity desc, then provenance tier, then repo/path/line —
/// C3-6 added the provenance tie-break so a security/deterministic finding sorts ahead of an
/// architecture/advisory one of the same severity, matching the within-bucket order
/// `report_export::build_report_json`'s matrix now uses) — the one true sort order both
/// `build_workbook` and `build_findings_export` (`findings.json`) consume as-is, rather than
/// each sorting its own copy and risking two orderings silently drifting apart.
fn partition_rows(
    report: &ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_options: &HashMap<String, String>,
) -> (Vec<FindingRow>, Vec<DepRow>) {
    // C4-R3: same zero-width/BOM sanitization `build_report_json` applies to the PDF's JSON —
    // see `sanitize_report_findings`'s doc comment for why this is the one shared ingestion
    // point rather than a per-artifact patch.
    let report = &sanitize_report_findings(report);
    let mut rows = Vec::new();
    let mut dep_rows = Vec::new();

    for f in &report.findings {
        if f.rule_id == DEP_AUDIT_RULE_ID {
            let ownership = crate::ownership::ownership_of(&f.rule_id, &f.captures);
            dep_rows.push(DepRow {
                package: f.snippet.clone(),
                advisory: f.detail.clone(),
                severity: normalize_severity(&f.severity),
                repo: f.repo.clone(),
                fixed_version: f.captures.get("osv_fixed_version").cloned(),
                ownership: ownership.tag().to_string(),
                action: crate::ownership::ownership_action(ownership)
                    .unwrap_or("")
                    .to_string(),
            });
            continue;
        }

        let wire = dispositions.get(&finding_key(f));
        let is_fp = wire.map(|d| d.state.as_str()) == Some("FalsePositive");
        // E2 / W8 (W5-Q2) / A5.2: every corpus-authored severity bound (the `min_severity`
        // FLOOR first, then the `max_severity` and deviation ceilings) — applied here too so
        // the xlsx/findings.json artifacts can never disagree with the PDF on a bounded rule
        // (`report_export::build_report_json` applies the SAME bounds, in the SAME relative
        // order, over the SAME corpus), nor with the live scan, which applies them to the
        // findings themselves (`severity_bounds::apply_corpus_severity_bounds`). All three
        // share `bounded_severity`, which includes the C4-R1 unlocated downgrade between the
        // floor and the ceilings; see its doc comment and `raise_to_rule_min_severity`'s for
        // the kind/location preconditions.
        let severity = crate::severity_bounds::bounded_severity(f, corpus);
        // C4-P2: `category_for_finding`/`title_for_finding` (not the bare rule-id-only
        // `category_for` / a raw corpus-or-rule_id join) — an AI-tier finding with no corpus
        // entry of its own gets its real semantic category and, when the P3 class fallback
        // grounds its citation, a human-readable title too — see those functions' doc comments.
        let category = category_for_finding(f, corpus);
        let title = title_for_finding(f, corpus);
        let chosen_option_for_rule = chosen_options
            .get(&f.rule_id.to_ascii_uppercase())
            .map(String::as_str);
        // W3 (2026-09-30), extended C4-P2: the SAME plain-language rewrite the PDF uses for its
        // curated-site headline AND detail — never the raw gate/rule-detail derivation
        // `defect_headline`/`f.detail` alone would give. Both the headline and the detail
        // columns below come from this ONE call, so a floor rule's authored `finding_detail`
        // template rewrites identically in the PDF and the product export — the raw
        // `finding.detail` (gate enforcement prose) is never exported verbatim.
        let (headline, detail) =
            client_headline_and_detail(f, corpus, &title, chosen_option_for_rule);
        // C4-P2: `citation_for_finding` (not the plain `resolve_citation`) — an AI-tier finding
        // with no corpus entry of its own still gets the P3 class fallback's grounded citation
        // here, the SAME citation `build_report_json`'s curated findings/PDF show for it, rather
        // than falling through to "AI-advisory, model-inferred." in the product export only.
        let citation = citation_for_finding(f, corpus);
        let citation_kind = citation.kind.clone();
        // Grading-cycle fix (2026-10-07): provenance is the tier of THIS ROW'S OWN SOURCE —
        // who/what produced it — which is NOT the same axis as `citation.kind` (whether a
        // real external standard backs its citation). A commodity-taint/preview row can
        // legitimately resolve to a "grounded" citation via the P3 grounding map (it cites
        // the SAME CWE/OWASP standard a deterministic floor rule for the same defect class
        // would), while still having been produced by an external tool this run that may
        // have found zero results on other repos/files. The OLD code derived `provenance`
        // straight from `citation.kind`, so such a row rendered "Deterministic" — a word this
        // export otherwise reserves for Camerata's own proven floor check — even though the
        // ledger's own per-family count can show the tool found nothing. Checking
        // `f.preview_tool` FIRST (before falling back to citation kind) fixes this: ANY row
        // this scan's preview/external-tool pass produced always says "Preview: {tool}",
        // never "Deterministic", regardless of how well-grounded its citation is.
        let provenance = if let Some(tool) = f.preview_tool.as_deref() {
            format!("Preview: {tool}")
        } else {
            match citation.kind.as_str() {
                "grounded" => "Deterministic".to_string(),
                _ => "AI-advisory".to_string(),
            }
        };
        let citation_urls = citation
            .sources
            .iter()
            .map(|s| s.url.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let fix = resolve_fix(&f.rule_id, corpus, f, chosen_option_for_rule);
        // C4-P3: calibration's effort, else the rule's authored band (see
        // `report_export::resolve_effort`'s doc comment) — never `f.effort` alone, so a
        // deterministic/RLS-replay floor finding with an authored band doesn't render "not
        // estimated this run" here while the PDF's curated findings show a real number.
        let resolved_effort = resolve_effort(f, &f.rule_id, corpus, chosen_option_for_rule);
        let (_, est_hours) = effort_hours_bounds(resolved_effort.as_deref());

        let (bucket, disposition_kind, disposition_label_str, fp_reason) = if is_fp {
            (
                "",
                None,
                "False positive (excluded from every working sheet — see the False \
                 Positives sheet)"
                    .to_string(),
                wire.map(|d| d.reason.clone()).unwrap_or_default(),
            )
        } else {
            let disposition = classify(f, wire);
            let reason = wire.map(|d| d.reason.clone()).unwrap_or_default();
            // C3-6: THE single bucket computation, shared verbatim with `build_report_json`'s
            // matrix pass and curated-findings label — never this module's own
            // severity-only `matrix_bucket` call (that was the root cause of the xlsx's
            // bucket counts disagreeing with the PDF/JSON: a hedged or uncited-AI row had no
            // gate here to hold it out of an action tier). C5-8 extends this same shared call
            // with a "has a fix" check, so `chosen_option_for_rule` (already computed above for
            // `fix`/`headline`/`detail`) is threaded through here too.
            let bucket = effective_bucket(
                f,
                disposition,
                &severity,
                corpus,
                report.test_file_count,
                chosen_option_for_rule,
            );
            let confirmed = wire.map(|d| d.confirmed_by_client).unwrap_or(false);
            let label = disposition_label(disposition, &reason, bucket, confirmed);
            (bucket, Some(disposition), label, String::new())
        };

        rows.push(FindingRow {
            severity,
            headline,
            repo: f.repo.clone(),
            path: f.path.clone(),
            line: f.line,
            rule_id: f.rule_id.clone(),
            category,
            bucket,
            disposition_kind,
            disposition_label: disposition_label_str,
            effort: resolved_effort.unwrap_or_default(),
            est_hours,
            // C4-P2: the canonical reconciled hedge state (see `is_hedged`/`hedge_confidence`'s
            // doc comment) — never `f.confidence`/`f.needs_review` read directly, so a row
            // `bucket` placed "informational" for a non-confidence reason (an `info`-severity
            // note, a testing-style deviation, an absence-type stance rule) still exports
            // `confidence == "needs-review"` / `needs_review == true` here, matching the
            // executive summary's "held for review" framing — and the two fields can never
            // disagree with each other, since both derive from the same `bucket`+`f` call.
            confidence: hedge_confidence(f, bucket).unwrap_or_default(),
            needs_review: is_hedged(f, bucket),
            in_test: f.in_test,
            provenance,
            citation_kind,
            citation_label: citation.label,
            citation_urls,
            also_matches: crate::report_export::also_matches_titles(&f.also_matches, corpus)
                .join(", "),
            status: f.status.clone(),
            snippet: cap_snippet_for_workbook(&f.snippet),
            detail,
            fix_specific: f.fix_specific.clone(),
            fix,
            is_fp,
            fp_reason,
            also_locations: f.also_locations.clone(),
            site_count: 1 + f.also_locations.len(),
            kind: camerata_api_types::audit::KindOnWire(f.kind.clone()),
        });
    }

    rows.sort_by(|a, b| {
        (
            severity_rank(&a.severity),
            provenance_tier_rank(&a.citation_kind),
            &a.repo,
            &a.path,
            a.line,
        )
            .cmp(&(
                severity_rank(&b.severity),
                provenance_tier_rank(&b.citation_kind),
                &b.repo,
                &b.path,
                b.line,
            ))
    });

    (rows, dep_rows)
}

/// Cap a snippet defensively against Excel's 32,767-character cell limit — NOT the PDF's
/// tight ~800-char/~12-line cap (the workbook has room; §3 of the design doc). ~2,000 chars,
/// with an honest truncation marker (never a silent cut).
const WORKBOOK_SNIPPET_MAX_CHARS: usize = 2000;

fn cap_snippet_for_workbook(snippet: &str) -> String {
    if snippet.chars().count() <= WORKBOOK_SNIPPET_MAX_CHARS {
        return snippet.to_string();
    }
    let mut out: String = snippet.chars().take(WORKBOOK_SNIPPET_MAX_CHARS).collect();
    out.push_str("\n... (truncated)");
    out
}

/// `Confirmed` is open here for exactly the reason it is open in the PDF (see
/// `report_export::Disposition::Confirmed`): the auditor established the finding is real and
/// nobody has fixed it. The workbook's disposition summary is required to reconcile with the
/// PDF's own numbers, so the two "still open" predicates must agree variant for variant.
fn is_still_open(d: Disposition) -> bool {
    matches!(
        d,
        Disposition::Unresolved
            | Disposition::Confirmed
            | Disposition::TechDebtNow
            | Disposition::TechDebtLater
    )
}

/// Delegates to `report_export::severity_rank` (C3-6) rather than keeping its own copy of the
/// mapping — this module's severity-based sorts/ranks (row order, worst-severity labels, tab
/// colors) must use the exact same rank as the matrix's within-bucket sort, never a second
/// hand-maintained copy that could silently drift from it.
fn severity_rank(sev: &str) -> u8 {
    crate::report_export::severity_rank(sev)
}

fn title_case(sev: &str) -> String {
    let mut chars = sev.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
    }
}

fn worst_severity_label(rows: &[&FindingRow]) -> String {
    rows.iter()
        .map(|r| severity_rank(&r.severity))
        .min()
        .map(|rank| match rank {
            0 => "Critical",
            1 => "High",
            2 => "Medium",
            _ => "Low",
        })
        .unwrap_or("Clean")
        .to_string()
}

fn worst_dep_severity_label(rows: &[DepRow]) -> String {
    rows.iter()
        .map(|r| severity_rank(&r.severity))
        .min()
        .map(|rank| match rank {
            0 => "Critical",
            1 => "High",
            2 => "Medium",
            _ => "Low",
        })
        .unwrap_or("Clean")
        .to_string()
}

/// A category sheet's rank (0 = worst, ascending) + tab color, by worst STILL-OPEN severity
/// present (mirrors `report_export`'s scorecard "Action-needed" > "Attention" > "Clean"
/// ordering, extended one notch to distinguish a critical-open category from a
/// merely-high-open one for the tab color).
fn category_status(rows: &[&FindingRow]) -> (u8, &'static str) {
    let mut open_critical = false;
    let mut open_high = false;
    let mut open_medium = false;
    for r in rows {
        if let Some(d) = r.disposition_kind {
            if is_still_open(d) {
                match r.severity.as_str() {
                    "critical" => open_critical = true,
                    "high" => open_high = true,
                    "medium" => open_medium = true,
                    _ => {}
                }
            }
        }
    }
    if open_critical {
        (0, "#C0392B")
    } else if open_high {
        (1, "#EA580C")
    } else if open_medium {
        (2, "#E0B400")
    } else {
        (3, "#27AE60")
    }
}

/// Sheet-name sanitizer: strip Excel's reserved `[ ] : * ? / \` characters, truncate to 31
/// chars, and dedupe with a `" (2)"`/`" (3)"`/... suffix (never a silent name collision that
/// would otherwise get auto-renamed unpredictably by `rust_xlsxwriter`/Excel).
fn sanitize_sheet_name(name: &str, used: &mut HashSet<String>) -> String {
    let cleaned: String = name.chars().filter(|c| !"[]:*?/\\".contains(*c)).collect();
    let cleaned = if cleaned.trim().is_empty() {
        "Sheet".to_string()
    } else {
        cleaned
    };
    let base: String = cleaned.chars().take(31).collect();
    if !used.contains(&base) {
        used.insert(base.clone());
        return base;
    }
    let mut n = 2;
    loop {
        let suffix = format!(" ({n})");
        let max_base_len = 31usize.saturating_sub(suffix.chars().count());
        let truncated_base: String = cleaned.chars().take(max_base_len).collect();
        let candidate = format!("{truncated_base}{suffix}");
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

// ── Pass 3: the kind partition (which sheet a row's own columns make sense on) ─────────

/// Which worksheet a row belongs on, decided by its KIND and nothing else. See the module
/// doc comment's table for the rationale per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindSheet {
    /// Something is wrong at a site: `Defect`, `Deviation`, and — deliberately — a row whose
    /// producer assigned NO kind at all.
    Findings,
    /// `ConventionObserved` / `NoConvention`: what the codebase does, with nothing wrong.
    Conventions,
    /// `NotAssessed`: armed but not evaluable. A coverage disclosure, so it renders in the
    /// Coverage sheet's own "could not complete" section.
    Coverage,
}

/// The ONE kind→sheet decision, so no sheet writer re-derives it and two of them can never
/// disagree (which would either duplicate a row or lose it entirely).
fn kind_sheet(row: &FindingRow) -> KindSheet {
    match &row.kind.0 {
        // An absent kind is NOT a synonym for `Defect` on the wire (A5.6), but for ROUTING it
        // must land with the accusing rows: the honest failure mode for an unmapped emission
        // path is "visible in the wrong tab", never "quietly filed as an observation".
        None | Some(FindingKind::Defect) | Some(FindingKind::Deviation(_)) => KindSheet::Findings,
        Some(FindingKind::ConventionObserved(_)) | Some(FindingKind::NoConvention(_)) => {
            KindSheet::Conventions
        }
        Some(FindingKind::NotAssessed(_)) => KindSheet::Coverage,
    }
}

/// A rule's human-readable title, or its id when the corpus has no entry (an AI-tier rule id,
/// a synthetic/test rule). The ONE place that fallback is decided, shared by the Coverage,
/// Deviations and Conventions sheets.
fn rule_title(rule_id: &str, corpus: Option<&camerata_rules::RuleSet>) -> String {
    corpus
        .and_then(|c| c.get_by_id(rule_id))
        .map(|r| r.title.clone())
        .unwrap_or_else(|| rule_id.to_string())
}

/// `path:line` for a row's own site plus every merge-absorbed site, with a consequence site
/// labeled exactly as the findings sheet's "Also found at" column labels it. Client-facing:
/// no raw rule ids (each absorbed site's own rule identity travels in `findings.json`'s
/// `also_locations` entries, which is where a machine consumer reads it).
fn site_locations(row: &FindingRow) -> Vec<String> {
    let mut out = Vec::with_capacity(1 + row.also_locations.len());
    if !row.path.is_empty() {
        out.push(format!("{}:{}", row.path, row.line));
    }
    for l in &row.also_locations {
        if l.consequence {
            out.push(format!("{}:{} (also affected)", l.path, l.line));
        } else {
            out.push(format!("{}:{}", l.path, l.line));
        }
    }
    out
}

/// One row of the `Deviations` sheet — ONE row per RULE, not per site.
///
/// The deviation payload is group-level by construction ([`camerata_api_types::audit::
/// DeviationKind`]: "All counts are GROUP-level (per rule, across the whole scan), repeated
/// verbatim on every deviating row for that rule"). Rendering it per site repeats the same
/// two numbers down N rows and still leaves the reader to work out that they are the same
/// group. At rule grain the owner's canonical sentence — "2 of 47 sites deviate from
/// URI-versioned endpoints" — is three adjacent columns, with no prose to parse.
#[derive(Serialize)]
struct DeviationGroupRow {
    category: String,
    rule_title: String,
    rule_id: String,
    /// The convention the codebase itself predominantly follows, verbatim from the payload
    /// (authored corpus text, never model prose).
    convention_label: String,
    /// NUMERATOR. The structured `deviating_sites` from the payload, floored at the number of
    /// sites this workbook actually lists for the group — a group that is ON this sheet
    /// because sites deviate can never render "0 deviating" (the C6 discipline: a check that
    /// fired must never render clean).
    deviating_sites: usize,
    /// DENOMINATOR, or `None` when the producer could not count honestly. Rendered as an
    /// explicit "not counted", never as 0 and never fabricated.
    assessed_sites: Option<usize>,
    worst_severity: String,
    /// Every deviating `path:line` the workbook knows about, absorbed merge sites included.
    locations: Vec<String>,
    /// The rule's authored, client-facing remediation — one convention, one fix. Blank when
    /// the rule has no authored remediation (never fabricated).
    recommended_fix: String,
}

/// Group the Deviation rows by rule into the `Deviations` sheet's rows. Sorted worst-severity
/// first, then most-deviating-sites, then rule id, so the sheet is deterministic and diffable.
fn build_deviation_groups(
    rows: &[&FindingRow],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<DeviationGroupRow> {
    let mut by_rule: BTreeMap<String, Vec<&FindingRow>> = BTreeMap::new();
    for row in rows {
        if matches!(row.kind.0, Some(FindingKind::Deviation(_))) {
            by_rule.entry(row.rule_id.clone()).or_default().push(row);
        }
    }

    let mut out: Vec<DeviationGroupRow> = by_rule
        .into_iter()
        .map(|(rule_id, group)| {
            let mut declared_numerator = 0usize;
            let mut assessed_sites: Option<usize> = None;
            let mut convention_label = String::new();
            for row in &group {
                if let Some(FindingKind::Deviation(d)) = &row.kind.0 {
                    declared_numerator = declared_numerator.max(d.deviating_sites);
                    if assessed_sites.is_none() {
                        assessed_sites = d.assessed_sites;
                    }
                    if convention_label.is_empty() {
                        convention_label = d.convention_label.clone();
                    }
                }
            }
            let locations: Vec<String> = group.iter().flat_map(|r| site_locations(r)).collect();
            // Sites this workbook can actually point at (own site + absorbed merge sites).
            let sites_represented: usize = group.iter().map(|r| r.site_count).sum();
            let worst_severity = worst_severity_label(&group);
            let recommended_fix = group.iter().find_map(|r| r.fix.clone()).unwrap_or_default();
            DeviationGroupRow {
                category: group[0].category.clone(),
                rule_title: rule_title(&rule_id, corpus),
                rule_id,
                convention_label,
                deviating_sites: declared_numerator.max(sites_represented),
                assessed_sites,
                worst_severity,
                locations,
                recommended_fix,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        (
            severity_rank(&a.worst_severity.to_ascii_lowercase()),
            std::cmp::Reverse(a.deviating_sites),
            &a.rule_id,
        )
            .cmp(&(
                severity_rank(&b.worst_severity.to_ascii_lowercase()),
                std::cmp::Reverse(b.deviating_sites),
                &b.rule_id,
            ))
    });
    out
}

/// One row of the `Conventions` sheet — the derived decision set (A5.5). A shipped
/// deliverable: the audit derives the client's conventions and this is the sheet those
/// conventions are read off, so it answers "what does our codebase already do well and
/// consistently" and "where have we never decided" in the same table.
#[derive(Serialize)]
struct ConventionRow {
    /// 0 = a convention IS followed consistently, 1 = none adopted. Sorts the "doing well"
    /// block first, because that is this sheet's headline job.
    #[serde(skip)]
    rank: u8,
    category: String,
    rule_title: String,
    rule_id: String,
    /// Plain-language state, never a wire tag (`convention_observed` is our taxonomy, not the
    /// client's vocabulary).
    status: String,
    /// The convention followed, or blank when none has been adopted.
    convention_label: String,
    /// For "no convention adopted": the authored options the codebase was actually seen
    /// using, with per-option site counts where the producer counted them. Blank when the
    /// producer established the split without enumerating it (the Detail cell explains).
    patterns_in_use: String,
    /// The row's own client-facing headline, verbatim. Load-bearing for artifact parity: the
    /// self-measurement harness requires every exported row's headline to appear in the
    /// workbook, so moving these rows off the findings sheet obliges this column to exist.
    headline: String,
    detail: String,
    sites: usize,
    also_found_at: String,
}

fn build_convention_rows(
    rows: &[&FindingRow],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<ConventionRow> {
    let mut out: Vec<ConventionRow> = rows
        .iter()
        .filter_map(|row| {
            let (rank, status, convention_label, patterns_in_use) = match &row.kind.0 {
                Some(FindingKind::ConventionObserved(c)) => (
                    0u8,
                    "Followed consistently — no exceptions found".to_string(),
                    c.convention_label.clone(),
                    String::new(),
                ),
                Some(FindingKind::NoConvention(n)) => (
                    1u8,
                    "No single convention adopted".to_string(),
                    String::new(),
                    n.observed_options
                        .iter()
                        .map(|o| match o.sites {
                            Some(1) => format!("{} (1 site)", o.label),
                            Some(n) => format!("{} ({n} sites)", o.label),
                            None => o.label.clone(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => return None,
            };
            Some(ConventionRow {
                rank,
                category: row.category.clone(),
                rule_title: rule_title(&row.rule_id, corpus),
                rule_id: row.rule_id.clone(),
                status,
                convention_label,
                patterns_in_use,
                headline: row.headline.clone(),
                detail: row.detail.clone(),
                sites: row.site_count,
                also_found_at: site_locations(row)
                    .into_iter()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (a.rank, &a.category, &a.rule_title, &a.rule_id).cmp(&(
            b.rank,
            &b.category,
            &b.rule_title,
            &b.rule_id,
        ))
    });
    out
}

/// One row of the Coverage sheet's "could not complete" section — a [`FindingKind::
/// NotAssessed`] row. Honesty, not a finding: a rule that was armed and could not be
/// evaluated must be distinguishable from one that was assessed and found clean.
#[derive(Serialize)]
struct NotAssessedRow {
    rule_title: String,
    rule_id: String,
    /// The structured reason in client language. The payload's `reason_detail` is the
    /// PRODUCER's verbatim diagnostic ("eslint exited 2") and is deliberately NOT rendered —
    /// G3: a client-facing coverage statement is a concise sentence, never an internal
    /// diagnostic dump. The machine-readable reason and detail both reach `findings.json`.
    why: String,
    /// The row's own client-facing headline, verbatim (parity — see [`ConventionRow`]).
    headline: String,
    detail: String,
}

/// The client-facing sentence for each structured non-assessment reason. Never the wire tag:
/// `pass_failed` names an internal pipeline stage, which is exactly the vocabulary that must
/// not reach a client's cell.
fn not_assessed_reason_label(reason: NotAssessedReason) -> &'static str {
    match reason {
        NotAssessedReason::PassFailed => "This check did not complete during the inspection",
        NotAssessedReason::NoFilesMatched => "No files in this codebase were in scope for it",
        NotAssessedReason::ToolFailed => "An external analysis tool it depends on could not run",
        NotAssessedReason::ModelDeclined => "The inspection reached no judgement on it",
    }
}

fn build_not_assessed_rows(
    rows: &[&FindingRow],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<NotAssessedRow> {
    let mut out: Vec<NotAssessedRow> = rows
        .iter()
        .filter_map(|row| match &row.kind.0 {
            Some(FindingKind::NotAssessed(na)) => Some(NotAssessedRow {
                rule_title: rule_title(&row.rule_id, corpus),
                rule_id: row.rule_id.clone(),
                why: not_assessed_reason_label(na.reason).to_string(),
                headline: row.headline.clone(),
                detail: row.detail.clone(),
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| (&a.rule_title, &a.rule_id).cmp(&(&b.rule_title, &b.rule_id)));
    out
}

/// The client-facing label for how a rule was checked, from its citation kind. The raw kinds
/// (`grounded` / `preview` / `advisory`) are OUR taxonomy; the words here are the ones the
/// Index sheet's own Provenance legend already defines, so the workbook speaks one vocabulary
/// and no cell leaks an internal token.
fn how_checked_label(citation_kind: &str) -> &'static str {
    match citation_kind {
        "grounded" => "Deterministic",
        "preview" => "Preview",
        _ => "AI-advisory",
    }
}

fn build_coverage_rows(
    report: &ScanReport,
    rows: &[FindingRow],
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<CoverageRow> {
    report
        .provenance
        .audited_rule_ids
        .iter()
        .map(|rid| {
            let title = rule_title(rid, corpus);
            let category = category_for(rid, corpus);
            let citation = resolve_citation(rid, None, corpus);
            // W7 (C6): count every way this rule's hits are represented, not PRIMARIES only.
            // Before W7 this counted surviving primary rows alone, so a rule whose every row
            // was absorbed by a merge (its id demoted to another row's `also_matches`, its
            // site preserved in `also_locations`) rendered 0 — green, "clean" — and a rule
            // that found something and lost it was indistinguishable from a rule that found
            // nothing. Sites first (one per primary row + one per absorbed `also_locations`
            // entry under this rule); when the rule left no site record at all, same-location
            // `also_matches` demotions still count, so a fired rule can never read clean.
            let primary_rows = rows
                .iter()
                .filter(|r| !r.is_fp && &r.rule_id == rid)
                .count();
            let absorbed_sites = report
                .findings
                .iter()
                .flat_map(|f| f.also_locations.iter())
                .filter(|l| &l.rule_id == rid)
                .count();
            let mut findings_count = primary_rows + absorbed_sites;
            if findings_count == 0 {
                findings_count = report
                    .findings
                    .iter()
                    .filter(|f| f.also_matches.iter().any(|m| m == rid))
                    .count();
            }
            CoverageRow {
                rule_id: rid.clone(),
                title,
                category,
                citation_kind: citation.kind,
                citation_label: citation.label,
                findings_count,
            }
        })
        .collect()
}

// ── Formats (built once, reused everywhere — no ad-hoc `Format::new()` at call sites) ──

/// Every [`Format`] this module produces goes through one of these methods, so the theme
/// (colors, fonts, alignment) stays consistent across every sheet instead of drifting
/// call-site by call-site.
struct Formats {
    header: Format,
    title: Format,
    section_title: Format,
    bold: Format,
    needs_review_flag: Format,
}

impl Formats {
    fn new() -> Self {
        Formats {
            header: Format::new()
                .set_bold()
                .set_font_color("#FFFFFF")
                .set_background_color("#1F2937")
                .set_font_size(11)
                .set_align(FormatAlign::VerticalCenter),
            title: Format::new().set_bold().set_font_size(14),
            section_title: Format::new()
                .set_bold()
                .set_font_size(11)
                .set_font_color("#1F2937"),
            bold: Format::new().set_bold(),
            needs_review_flag: Format::new().set_italic().set_font_color("#B45309"),
        }
    }

    /// The Severity cell's own direct color (critical/high/medium/low), applied at write
    /// time rather than as a conditional-formatting rule — the value set is closed, so a
    /// direct format survives every viewer identically (design doc §3 rationale).
    fn severity_cell(&self, sev: &str) -> Format {
        let (bg, fg): (&str, &str) = match sev {
            "critical" => ("#B91C1C", "#FFFFFF"),
            "high" => ("#EA580C", "#FFFFFF"),
            "medium" => ("#FCD34D", "#000000"),
            _ => ("#E5E7EB", "#000000"),
        };
        Format::new()
            .set_background_color(bg)
            .set_font_color(fg)
            .set_bold()
            .set_align(FormatAlign::Center)
    }

    /// A generic data cell: optional row-tint background, optional wrap, optional monospace
    /// (the snippet column). Always vertically top-aligned per the design spec.
    fn cell(&self, bg: Option<&str>, wrap: bool, mono: bool) -> Format {
        let mut f = Format::new().set_align(FormatAlign::Top);
        if let Some(bg) = bg {
            f = f.set_background_color(bg);
        }
        if wrap {
            f = f.set_text_wrap();
        }
        if mono {
            f = f.set_font_name("Courier New").set_font_size(9);
        }
        f
    }
}

// ── The ~21(+2)-column findings schema (All Findings / category sheets / False Positives) ──

const HEADERS: [&str; 26] = [
    "Severity",
    "Headline",
    "Repo",
    "File",
    "Line",
    "Rule ID",
    "Category",
    "Action",
    "Disposition",
    "Effort",
    "Est. hours",
    "Confidence",
    "Needs review",
    "In test scope",
    "Provenance",
    "Citation",
    "Citation URLs",
    "Also matches",
    "Status",
    "Snippet",
    "Detail",
    // P2: the PRIMARY fix — codebase-specific, shown BEFORE the rule's generic remediation
    // (matches the PDF's fix_for_this_finding-first ordering).
    "Fix (specific to this finding)",
    "Recommended Fix",
    // Pass-1 W3 (A5.6): the row's kind tag (defect / deviation / convention_observed /
    // no_convention / not_assessed / unspecified). Appended LAST deliberately — a minimal,
    // correct carry of the new field; the real worksheet redesign (per-kind sheets etc.) is
    // Pass 3 work and must not be pre-empted here.
    "Kind",
    // Pass-1 W7 (C5): how many distinct evidence sites this row represents (its own
    // path:line plus every absorbed merge site), and the absorbed sites themselves — the
    // columns that make a merged row say "3 sites" instead of silently reading as one.
    "Sites",
    "Also found at",
];

const WIDTHS: [f64; 26] = [
    10.0, 50.0, 18.0, 40.0, 7.0, 28.0, 16.0, 10.0, 30.0, 9.0, 16.0, 12.0, 12.0, 16.0, 16.0, 34.0,
    30.0, 24.0, 18.0, 55.0, 70.0, 45.0, 45.0, 20.0, 8.0, 44.0,
];

/// Columns that wrap (matches the design's wrap-column list, plus the Fix-specific and
/// Recommended-Fix columns at the end, and W7's "Also found at" site list).
const WRAP_COLS: [u16; 8] = [1, 8, 15, 16, 20, 21, 22, 25];
const SNIPPET_COL: u16 = 19;
const NEEDS_REVIEW_COL: u16 = 12;

fn write_finding_row(
    ws: &mut Worksheet,
    r: u32,
    row: &FindingRow,
    fmts: &Formats,
    zebra_on: bool,
) -> Result<(), XlsxError> {
    let bg: Option<&str> = if row.severity == "critical" {
        Some("#FEF2F2")
    } else if zebra_on {
        Some("#F9FAFB")
    } else {
        None
    };

    ws.write_string_with_format(
        r,
        0,
        title_case(&row.severity),
        &fmts.severity_cell(&row.severity),
    )?;
    ws.write_string_with_format(r, 1, &row.headline, &fmts.cell(bg, true, false))?;
    ws.write_string_with_format(r, 2, &row.repo, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 3, &row.path, &fmts.cell(bg, false, false))?;
    ws.write_number_with_format(r, 4, row.line as f64, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 5, &row.rule_id, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 6, &row.category, &fmts.cell(bg, false, false))?;
    let action = if row.is_fp {
        String::new()
    } else {
        bucket_title(row.bucket).to_string()
    };
    ws.write_string_with_format(r, 7, &action, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 8, &row.disposition_label, &fmts.cell(bg, true, false))?;
    ws.write_string_with_format(r, 9, &row.effort, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 10, &row.est_hours, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 11, &row.confidence, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(
        r,
        NEEDS_REVIEW_COL,
        if row.needs_review { "Yes" } else { "" },
        &fmts.cell(bg, false, false),
    )?;
    ws.write_string_with_format(
        r,
        13,
        if row.in_test { "Yes" } else { "" },
        &fmts.cell(bg, false, false),
    )?;
    ws.write_string_with_format(r, 14, &row.provenance, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 15, &row.citation_label, &fmts.cell(bg, true, false))?;
    ws.write_string_with_format(r, 16, &row.citation_urls, &fmts.cell(bg, true, false))?;
    ws.write_string_with_format(r, 17, &row.also_matches, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, 18, &row.status, &fmts.cell(bg, false, false))?;
    ws.write_string_with_format(r, SNIPPET_COL, &row.snippet, &fmts.cell(bg, true, true))?;
    ws.write_string_with_format(r, 20, &row.detail, &fmts.cell(bg, true, false))?;
    // P2: fix_specific (the PRIMARY, codebase-specific fix) renders in the column BEFORE the
    // rule's generic Recommended Fix — same first-vs-secondary ordering as the PDF template.
    ws.write_string_with_format(
        r,
        21,
        row.fix_specific.as_deref().unwrap_or(""),
        &fmts.cell(bg, true, false),
    )?;
    ws.write_string_with_format(
        r,
        22,
        row.fix.as_deref().unwrap_or(""),
        &fmts.cell(bg, true, false),
    )?;
    // A5.6: the row's kind tag — the SAME wire discriminator findings.json serializes
    // (`KindOnWire::tag`, "unspecified" when the producer never assigned one), never a
    // re-derivation.
    ws.write_string_with_format(r, 23, row.kind.tag(), &fmts.cell(bg, false, false))?;
    // W7 (C5): the site count plus every absorbed site, rendered as `path:line` (one per
    // line; a consequence site — no independently-fixable code of its own — is labeled). No
    // raw rule ids here: this cell is client-facing, and each absorbed site's own rule
    // identity already travels in findings.json's `also_locations` entries.
    ws.write_number_with_format(r, 24, row.site_count as f64, &fmts.cell(bg, false, false))?;
    let also_found_at = row
        .also_locations
        .iter()
        .map(|l| {
            if l.consequence {
                format!("{}:{} (also affected)", l.path, l.line)
            } else {
                format!("{}:{}", l.path, l.line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    ws.write_string_with_format(r, 25, &also_found_at, &fmts.cell(bg, true, false))?;

    Ok(())
}

/// Write one findings-style sheet (All Findings / a category sheet / False Positives).
/// `fp_reason_col`: appends the "FP Reason" column (col X) — only for the False Positives
/// sheet. `tab_color`: applied when present.
fn write_findings_sheet(
    wb: &mut Workbook,
    name: &str,
    rows: &[&FindingRow],
    fmts: &Formats,
    fp_reason_col: bool,
    tab_color: Option<&str>,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    if let Some(color) = tab_color {
        let _ = ws.set_tab_color(Color::from(color));
    }

    for (c, h) in HEADERS.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &fmts.header)?;
    }
    if fp_reason_col {
        ws.write_string_with_format(0, 26, "FP Reason", &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;

    for (c, w) in WIDTHS.iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }
    if fp_reason_col {
        ws.set_column_width(26, 40.0)?;
    }

    for (i, row) in rows.iter().enumerate() {
        let r = (i + 1) as u32;
        let zebra_on = i % 2 == 1;
        write_finding_row(ws, r, row, fmts, zebra_on)?;
        if fp_reason_col {
            let bg: Option<&str> = if row.severity == "critical" {
                Some("#FEF2F2")
            } else if zebra_on {
                Some("#F9FAFB")
            } else {
                None
            };
            ws.write_string_with_format(r, 26, &row.fp_reason, &fmts.cell(bg, true, false))?;
        }
    }

    let last_row = rows.len() as u32;
    let last_col: u16 = if fp_reason_col { 26 } else { 25 };
    ws.autofilter(0, 0, last_row, last_col)?;
    ws.set_freeze_panes(1, 0)?;

    // Needs-review flag: italic amber whenever the column reads "Yes", so it survives
    // re-sorting by the user (design §3's "Additionally" conditional-format rule).
    let cf_last_row = last_row.max(1);
    let cf = ConditionalFormatText::new()
        .set_rule(ConditionalFormatTextRule::Contains("Yes".to_string()))
        .set_format(fmts.needs_review_flag.clone());
    ws.add_conditional_format(1, NEEDS_REVIEW_COL, cf_last_row, NEEDS_REVIEW_COL, &cf)?;

    // Wrap columns get their format applied above per-cell already; this is a defensive
    // no-op sanity list kept in sync with `WRAP_COLS` (a future column addition that forgets
    // to wrap its own cells is still caught by the regression test iterating this constant).
    let _ = WRAP_COLS;

    Ok(())
}

fn write_dependencies_sheet(
    wb: &mut Workbook,
    name: &str,
    dep_rows: &[DepRow],
    coverage_notes: &[String],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    let _ = ws.set_tab_color(Color::from("#6B7280"));

    let headers = [
        "Package",
        "Advisory",
        "Severity",
        "Repo",
        "Fixed in",
        "Recommended action",
    ];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    for (c, w) in [30.0, 60.0, 12.0, 18.0, 14.0, 48.0].iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }

    let mut sorted: Vec<DepRow> = dep_rows.to_vec();
    sorted.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| a.package.cmp(&b.package))
    });

    for (i, row) in sorted.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_string_with_format(r, 0, &row.package, &fmts.cell(None, false, false))?;
        ws.write_string_with_format(r, 1, &row.advisory, &fmts.cell(None, true, false))?;
        ws.write_string_with_format(
            r,
            2,
            title_case(&row.severity),
            &fmts.severity_cell(&row.severity),
        )?;
        ws.write_string_with_format(r, 3, &row.repo, &fmts.cell(None, false, false))?;
        // W33: "Fixed in" is the upgrade target when a patch exists; the action spells out what
        // to do when it does not (monitor / accept / replace) — the ownership split, made visible.
        let fixed = row.fixed_version.as_deref().unwrap_or("(no patch yet)");
        ws.write_string_with_format(r, 4, fixed, &fmts.cell(None, false, false))?;
        let action = if row.action.is_empty() {
            row.fixed_version
                .as_deref()
                .map(|v| format!("Upgrade to >= {v}."))
                .unwrap_or_default()
        } else {
            row.action.clone()
        };
        ws.write_string_with_format(r, 5, &action, &fmts.cell(None, true, false))?;
    }

    let last_row = sorted.len() as u32;
    ws.autofilter(0, 0, last_row, 5)?;
    ws.set_freeze_panes(1, 0)?;

    let mut r = last_row + 2;
    if !coverage_notes.is_empty() {
        ws.write_string_with_format(r, 0, "Dependency-audit coverage notes", &fmts.section_title)?;
        r += 1;
        for note in coverage_notes {
            ws.write_string(r, 0, note)?;
            r += 1;
        }
    }

    Ok(())
}

/// The `Deviations` sheet — one row per RULE, stating the consistency sentence from columns.
///
/// This is the sheet the owner's canonical example has to be readable off: "2 of 47 sites
/// deviate from *URI-versioned endpoints*". Numerator, denominator and convention are three
/// adjacent columns; "Where" lists the sites, absorbed merge sites included, so an over-merge
/// cannot hide behind a single `path:line` (C5).
fn write_deviations_sheet(
    wb: &mut Workbook,
    name: &str,
    groups: &[DeviationGroupRow],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    let _ = ws.set_tab_color(Color::from("#E0B400"));

    let headers = [
        "Area",
        "What was checked",
        "Convention your codebase follows",
        "Sites deviating",
        "Sites assessed",
        "Highest severity",
        "Where",
        "Recommended fix",
        "Rule ID",
    ];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    for (c, w) in [16.0, 46.0, 40.0, 15.0, 15.0, 16.0, 44.0, 50.0, 28.0]
        .iter()
        .enumerate()
    {
        ws.set_column_width(c as u16, *w)?;
    }

    if groups.is_empty() {
        // An empty STRUCTURAL tab says "we looked and found none"; a missing tab says "we
        // never looked". Never let the absence of a section read as the absence of the check.
        ws.write_string(
            1,
            0,
            "No consistency deviations were identified in this inspection.",
        )?;
        ws.set_freeze_panes(1, 0)?;
        return Ok(());
    }

    for (i, g) in groups.iter().enumerate() {
        let r = (i + 1) as u32;
        let bg: Option<&str> = if i % 2 == 1 { Some("#F9FAFB") } else { None };
        ws.write_string_with_format(r, 0, &g.category, &fmts.cell(bg, false, false))?;
        ws.write_string_with_format(r, 1, &g.rule_title, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 2, &g.convention_label, &fmts.cell(bg, true, false))?;
        ws.write_number_with_format(r, 3, g.deviating_sites as f64, &fmts.cell(bg, false, false))?;
        // An unknown denominator renders as an explicit words-not-number cell. Writing 0 (or
        // repeating the numerator) would fabricate a denominator the producer refused to
        // claim, which is the one thing `assessed_sites: None` exists to prevent.
        match g.assessed_sites {
            Some(n) => ws.write_number_with_format(r, 4, n as f64, &fmts.cell(bg, false, false))?,
            None => {
                ws.write_string_with_format(r, 4, "not counted", &fmts.cell(bg, false, false))?
            }
        };
        ws.write_string_with_format(
            r,
            5,
            &g.worst_severity,
            &fmts.severity_cell(&g.worst_severity.to_ascii_lowercase()),
        )?;
        ws.write_string_with_format(r, 6, g.locations.join("\n"), &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 7, &g.recommended_fix, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 8, &g.rule_id, &fmts.cell(bg, false, false))?;
    }

    ws.autofilter(0, 0, groups.len() as u32, 8)?;
    ws.set_freeze_panes(1, 0)?;
    Ok(())
}

/// The `Conventions` sheet — the derived decision set (A5.5), which is a shipped feature and
/// not filler: these are the conventions that become the client's `AGENTS.md`.
///
/// Two statements share the sheet because a client reads them together: "this is what you do
/// consistently" and "here you have never decided". The followed-consistently block sorts
/// first, since answering "what does our codebase already do well" is this sheet's headline
/// job.
fn write_conventions_sheet(
    wb: &mut Workbook,
    name: &str,
    rows: &[ConventionRow],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    let _ = ws.set_tab_color(Color::from("#27AE60"));

    let headers = [
        "Area",
        "What was checked",
        "Status",
        "Convention your codebase follows",
        "Patterns in use",
        "What we observed",
        "Detail",
        "Sites",
        "Also found at",
        "Rule ID",
    ];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    for (c, w) in [16.0, 46.0, 36.0, 40.0, 34.0, 50.0, 60.0, 8.0, 36.0, 28.0]
        .iter()
        .enumerate()
    {
        ws.set_column_width(c as u16, *w)?;
    }

    if rows.is_empty() {
        ws.write_string(
            1,
            0,
            "This inspection derived no codebase conventions. A convention is reported here \
             only when the codebase follows one consistently, or when it demonstrably has none.",
        )?;
        ws.set_freeze_panes(1, 0)?;
        return Ok(());
    }

    for (i, row) in rows.iter().enumerate() {
        let r = (i + 1) as u32;
        let bg: Option<&str> = if i % 2 == 1 { Some("#F9FAFB") } else { None };
        ws.write_string_with_format(r, 0, &row.category, &fmts.cell(bg, false, false))?;
        ws.write_string_with_format(r, 1, &row.rule_title, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 2, &row.status, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 3, &row.convention_label, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 4, &row.patterns_in_use, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 5, &row.headline, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 6, &row.detail, &fmts.cell(bg, true, false))?;
        ws.write_number_with_format(r, 7, row.sites as f64, &fmts.cell(bg, false, false))?;
        ws.write_string_with_format(r, 8, &row.also_found_at, &fmts.cell(bg, true, false))?;
        ws.write_string_with_format(r, 9, &row.rule_id, &fmts.cell(bg, false, false))?;
    }

    ws.autofilter(0, 0, rows.len() as u32, 9)?;
    ws.set_freeze_panes(1, 0)?;
    Ok(())
}

/// One concise, client-facing line summarising what this inspection actually checked (G3:
/// "coverage IS client-facing, but concise and useful, not exhaustive. A one-line summary,
/// written for a client, never an internal diagnostic dump"). The per-rule table below it is
/// the audit trail — the xlsx is the detail surface, where a missing row is a fault — but a
/// client should not have to count 400 rows to learn what happened.
fn coverage_summary_line(coverage_rows: &[CoverageRow], not_assessed: usize) -> String {
    let total = coverage_rows.len();
    let with_findings = coverage_rows
        .iter()
        .filter(|r| r.findings_count > 0)
        .count();
    let clean = total.saturating_sub(with_findings);
    let rules = if total == 1 { "rule" } else { "rules" };
    // Branched rather than one template, because the degenerate cases are the ones a client
    // actually sees on a healthy codebase, and "0 found something to look at; the other 3
    // were checked and found clean" is the sentence a template produces and a human never
    // would. Caught by reading the rendered sheet, not by a test.
    let mut line = if total == 0 {
        "No rules were recorded as checked for this inspection.".to_string()
    } else if with_findings == 0 {
        format!("We checked {total} {rules} against this codebase; every one came back clean.")
    } else if clean == 0 {
        format!(
            "We checked {total} {rules} against this codebase, and every one found something \
             to look at."
        )
    } else {
        format!(
            "We checked {total} {rules} against this codebase. {with_findings} found something \
             to look at; the other {clean} {} checked and found clean.",
            if clean == 1 { "was" } else { "were" }
        )
    };
    if not_assessed > 0 {
        line.push_str(&format!(
            " A further {not_assessed} could not be evaluated this scan and {} named below, \
             with the reason.",
            if not_assessed == 1 { "is" } else { "are" }
        ));
    }
    line
}

#[allow(clippy::too_many_arguments)]
fn write_coverage_sheet(
    wb: &mut Workbook,
    name: &str,
    coverage_rows: &[CoverageRow],
    excluded_mechanical_rules: &[String],
    rules_applied: &[RulesAppliedRow],
    tool_coverage_notes: &[String],
    rules_not_run: &[(String, String)],
    not_assessed: &[NotAssessedRow],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    let _ = ws.set_tab_color(Color::from("#6B7280"));

    // The one-line client summary comes FIRST, above the table it summarises.
    ws.write_string_with_format(0, 0, "Coverage", &fmts.section_title)?;
    ws.write_string(
        1,
        0,
        coverage_summary_line(coverage_rows, not_assessed.len()),
    )?;
    const HEADER_ROW: u32 = 3;

    let headers = [
        "Area",
        "What was checked",
        "Result",
        "How we checked it",
        "Standard cited",
        "Rule ID",
    ];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(HEADER_ROW, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(HEADER_ROW, 28)?;
    for (c, w) in [16.0, 60.0, 20.0, 18.0, 40.0, 28.0].iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }

    for (i, row) in coverage_rows.iter().enumerate() {
        let r = HEADER_ROW + 1 + i as u32;
        // A "Result" cell in words, not a bare integer: a client reading "0" has to guess
        // whether the rule was clean or never ran, and those are different statements — the
        // never-ran case is disclosed in its own section below, by name.
        let (result, result_fmt) = if row.findings_count == 0 {
            (
                "Clean — nothing found".to_string(),
                fmts.cell(Some("#ECFDF5"), false, false),
            )
        } else {
            (
                format!(
                    "{} site{} found",
                    row.findings_count,
                    if row.findings_count == 1 { "" } else { "s" }
                ),
                fmts.cell(None, false, false),
            )
        };
        ws.write_string_with_format(r, 0, &row.category, &fmts.cell(None, false, false))?;
        ws.write_string_with_format(r, 1, &row.title, &fmts.cell(None, true, false))?;
        ws.write_string_with_format(r, 2, &result, &result_fmt)?;
        ws.write_string_with_format(
            r,
            3,
            how_checked_label(&row.citation_kind),
            &fmts.cell(None, false, false),
        )?;
        ws.write_string_with_format(r, 4, &row.citation_label, &fmts.cell(None, true, false))?;
        ws.write_string_with_format(r, 5, &row.rule_id, &fmts.cell(None, false, false))?;
    }

    let last_row = HEADER_ROW + coverage_rows.len() as u32;
    ws.autofilter(HEADER_ROW, 0, last_row, 5)?;
    ws.set_freeze_panes(HEADER_ROW + 1, 0)?;

    let mut next_r = last_row + 2;

    // VESTIGIAL in practice, kept deliberately. `split_scannable_rules_from` has returned a
    // hard-coded empty `excluded_mechanical` since W4 dropped CI-tier exclusion from the
    // semantic pass, so for any report produced by today's pipeline this block cannot render.
    // It stays because `excluded_mechanical_rules` is persisted on `ScanReport` and a pre-W4
    // report re-exported from disk can still carry real content — and because the honest
    // reading of a live-looking block that can only ever be empty is "dead", which is exactly
    // the thing worth writing down rather than leaving the next reader to rediscover. The
    // modern equivalent is the `rules_not_run` section below, sourced from the scan ledger.
    if !excluded_mechanical_rules.is_empty() {
        ws.write_string_with_format(
            next_r,
            0,
            "Excluded from this code-only audit (mechanical, CI-enforced instead)",
            &fmts.section_title,
        )?;
        next_r += 1;
        for rid in excluded_mechanical_rules {
            ws.write_string(next_r, 0, rid)?;
            next_r += 1;
        }
        next_r += 1;
    }

    // P7: "Rules applied" appendix — every multi-option rule this scan decided an
    // alternative for, with the chosen option, the evidence backing it, and whether the
    // rule's concern was even judged applicable. A wrong pick costs a glance at this table
    // instead of surfacing as a false finding in the report body.
    if !rules_applied.is_empty() {
        ws.write_string_with_format(
            next_r,
            0,
            "Rules applied (alternative selection)",
            &fmts.section_title,
        )?;
        next_r += 1;
        let ra_headers = ["Rule ID", "Chosen alternative", "Evidence", "Applicable"];
        for (c, h) in ra_headers.iter().enumerate() {
            ws.write_string_with_format(next_r, c as u16, *h, &fmts.header)?;
        }
        next_r += 1;
        let ra_header_row = next_r - 1;
        for row in rules_applied {
            ws.write_string_with_format(next_r, 0, &row.rule_id, &fmts.cell(None, false, false))?;
            ws.write_string_with_format(next_r, 1, &row.chosen, &fmts.cell(None, true, false))?;
            ws.write_string_with_format(next_r, 2, &row.evidence, &fmts.cell(None, true, false))?;
            ws.write_string_with_format(
                next_r,
                3,
                if row.applicable { "Yes" } else { "No" },
                &fmts.cell(None, false, false),
            )?;
            next_r += 1;
        }
        let ra_last_row = next_r.saturating_sub(1);
        if ra_last_row > ra_header_row {
            ws.autofilter(ra_header_row, 0, ra_last_row, 3)?;
        }
        next_r += 1;
    }

    // W7 (G1/G3): non-dependency tool-coverage gaps — the SAME concise client-facing lines
    // the PDF/JSON carry in `failed_passes` (see `report_export::tool_coverage_disclosures`).
    // Before W7 only osv-scanner notes reached any artifact (on the Dependencies sheet), so
    // a failed semgrep/eslint/ruff/clippy tier was invisible to a client here too.
    if !tool_coverage_notes.is_empty() {
        ws.write_string_with_format(next_r, 0, "Tool coverage notes", &fmts.section_title)?;
        next_r += 1;
        for note in tool_coverage_notes {
            ws.write_string(next_r, 0, note)?;
            next_r += 1;
        }
        next_r += 1;
    }

    // Pass 3: `NotAssessed` rows — armed, but not evaluable. They belong HERE and not on the
    // findings sheet: "we could not check this" is a coverage statement, and giving it equal
    // billing with a defect both overstates it and buries the defects. It is still an
    // EXPORTED ROW (it reaches findings.json), so this section renders each row's own
    // headline verbatim — that is what keeps artifact parity true after the kind partition.
    if !not_assessed.is_empty() {
        ws.write_string_with_format(
            next_r,
            0,
            "Checks we could not complete this scan",
            &fmts.section_title,
        )?;
        next_r += 1;
        for (c, h) in [
            "What was checked",
            "Why",
            "What we observed",
            "Detail",
            "Rule ID",
        ]
        .iter()
        .enumerate()
        {
            ws.write_string_with_format(next_r, c as u16, *h, &fmts.header)?;
        }
        next_r += 1;
        for row in not_assessed {
            ws.write_string_with_format(next_r, 0, &row.rule_title, &fmts.cell(None, true, false))?;
            ws.write_string_with_format(next_r, 1, &row.why, &fmts.cell(None, true, false))?;
            ws.write_string_with_format(next_r, 2, &row.headline, &fmts.cell(None, true, false))?;
            ws.write_string_with_format(next_r, 3, &row.detail, &fmts.cell(None, true, false))?;
            ws.write_string_with_format(next_r, 4, &row.rule_id, &fmts.cell(None, false, false))?;
            next_r += 1;
        }
        next_r += 1;
    }

    // W7 (G2): rules the ledger confirms did NOT run this scan, with the recorded reason —
    // the detail-surface sibling of the report JSON's `rules_not_run` section, so the
    // workbook (the meat-and-potatoes artifact) never implies a never-run rule was
    // checked-and-clean.
    if !rules_not_run.is_empty() {
        ws.write_string_with_format(next_r, 0, "Rules not run this scan", &fmts.section_title)?;
        next_r += 1;
        for (c, h) in ["Rule ID", "Reason"].iter().enumerate() {
            ws.write_string_with_format(next_r, c as u16, *h, &fmts.header)?;
        }
        next_r += 1;
        for (rid, reason) in rules_not_run {
            ws.write_string_with_format(next_r, 0, rid, &fmts.cell(None, false, false))?;
            ws.write_string_with_format(next_r, 1, reason, &fmts.cell(None, true, false))?;
            next_r += 1;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_index_sheet(
    wb: &mut Workbook,
    report: &ScanReport,
    opts: &ReportOptions,
    live_rows: &[&FindingRow],
    fp_count: usize,
    dep_rows: &[DepRow],
    sheet_plan: &[SheetSummary],
    corpus: Option<&camerata_rules::RuleSet>,
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name("Index")?;
    let _ = ws.set_tab_color(Color::from("#111827"));
    ws.set_column_width(0, 34.0)?;
    ws.set_column_width(1, 60.0)?;
    for c in 2..6u16 {
        ws.set_column_width(c, 11.0)?;
    }

    let mut r = 0u32;
    // C4-P4 (residual defect 3): finish the P6 "Codebase Inspection" rename on the Index sheet —
    // this title row, "Repos audited", and "Audit model" below were left over from before that
    // pass. Wording now matches the PDF's own neutral "Codebase Inspection"/"Repos inspected"/
    // "Inspection model" language exactly (see `audit_report.typ`'s cover + methodology
    // sections and `NEUTRAL_COVER_TITLE`).
    ws.write_string_with_format(
        r,
        0,
        "Camerata Codebase Inspection — Product Export Index",
        &fmts.title,
    )?;
    r += 2;
    if !opts.project_title.is_empty() {
        ws.write_string(r, 0, &opts.project_title)?;
        r += 1;
    }
    if !opts.client_name.is_empty() {
        ws.write_string(r, 0, format!("Prepared for {}", opts.client_name))?;
        r += 1;
    }
    if !opts.prepared_by.is_empty() {
        ws.write_string(r, 0, format!("Prepared by {}", opts.prepared_by))?;
        r += 1;
    }
    r += 1;

    // ── Provenance ──────────────────────────────────────────────────────────
    ws.write_string_with_format(r, 0, "Provenance", &fmts.section_title)?;
    r += 1;
    ws.write_string(r, 0, "Repos inspected")?;
    ws.write_string(r, 1, report.repos.join(", "))?;
    r += 1;
    for aref in &report.provenance.audited_refs {
        let sha = aref
            .sha
            .as_deref()
            .map(|s| s.chars().take(7).collect::<String>())
            .unwrap_or_else(|| "no commit recorded".to_string());
        let branch = aref.branch.as_deref().unwrap_or("unknown branch");
        let dirty = if aref.dirty { " (dirty)" } else { "" };
        ws.write_string(r, 0, &aref.repo)?;
        ws.write_string(r, 1, format!("{sha} on {branch}{dirty}"))?;
        r += 1;
    }
    ws.write_string(r, 0, "Inspection model")?;
    ws.write_string(
        r,
        1,
        report.provenance.audit_model.as_deref().unwrap_or("n/a"),
    )?;
    r += 1;
    ws.write_string(r, 0, "Calibration model")?;
    ws.write_string(
        r,
        1,
        report
            .provenance
            .calibration_model
            .as_deref()
            .unwrap_or("n/a"),
    )?;
    r += 1;
    ws.write_string(r, 0, "Camerata version")?;
    ws.write_string(r, 1, &report.provenance.camerata_version)?;
    r += 1;
    ws.write_string(r, 0, "Generated")?;
    // The SCAN time, the SAME string the PDF cover prints — not the wall clock at
    // serialization. See `report_export::generated_at_label`: a workbook re-exported months
    // later used to claim it was generated months after the audit it describes, and the cell
    // straddled a minute boundary under load (source 2 in
    // `crates/cli/tests/inspect_export_equivalence.rs`'s normalization).
    ws.write_string(
        r,
        1,
        crate::report_export::generated_at_label(&report.provenance),
    )?;
    r += 1;
    ws.write_string(r, 0, "Files scanned")?;
    ws.write_number(r, 1, report.files_scanned as f64)?;
    r += 1;
    ws.write_string(r, 0, "Files excluded as noise")?;
    ws.write_number(r, 1, report.files_excluded as f64)?;
    r += 1;
    ws.write_string(r, 0, "Code volume (characters)")?;
    ws.write_number(r, 1, report.code_chars as f64)?;
    r += 2;

    // ── Counts matrix: category x severity ─────────────────────────────────
    ws.write_string_with_format(r, 0, "Findings by category", &fmts.section_title)?;
    r += 1;
    let matrix_header_row = r;
    for (c, h) in ["Category", "Critical", "High", "Medium", "Low", "Total"]
        .iter()
        .enumerate()
    {
        ws.write_string_with_format(matrix_header_row, c as u16, *h, &fmts.header)?;
    }
    r += 1;

    let mut by_cat: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    for row in live_rows {
        let counts = by_cat.entry(row.category.clone()).or_insert([0; 4]);
        match row.severity.as_str() {
            "critical" => counts[0] += 1,
            "high" => counts[1] += 1,
            "medium" => counts[2] += 1,
            _ => counts[3] += 1,
        }
    }
    // Audited-but-empty categories still get a (zero) row, matching the PDF scorecard's own
    // "clean row" behavior (see report_export's `scorecard_includes_a_clean_row_...` test).
    for rid in &report.provenance.audited_rule_ids {
        by_cat.entry(category_for(rid, corpus)).or_insert([0; 4]);
    }

    let mut totals = [0usize; 4];
    for (cat, counts) in &by_cat {
        ws.write_string(r, 0, cat)?;
        for (i, count) in counts.iter().enumerate() {
            ws.write_number(r, (i + 1) as u16, *count as f64)?;
            totals[i] += *count;
        }
        let total: usize = counts.iter().sum();
        ws.write_number(r, 5, total as f64)?;
        r += 1;
    }
    ws.write_string_with_format(r, 0, "Total", &fmts.bold)?;
    for (i, total) in totals.iter().enumerate() {
        ws.write_number_with_format(r, (i + 1) as u16, *total as f64, &fmts.bold)?;
    }
    ws.write_number_with_format(r, 5, totals.iter().sum::<usize>() as f64, &fmts.bold)?;
    r += 2;

    // ── Statement types: which of the five kinds this inspection returned ──────────
    // Pass 3. An inspection no longer returns one kind of statement, and a client opening a
    // multi-tab workbook needs to know, in one glance, which tab holds what and how much is
    // in it. The counts are over the SAME `live_rows` set the category matrix above uses, so
    // this block and that matrix always total the same number.
    ws.write_string_with_format(r, 0, "Statement types", &fmts.section_title)?;
    r += 1;
    let mut defects = 0usize;
    let mut deviations = 0usize;
    let mut conventions_followed = 0usize;
    let mut no_convention = 0usize;
    let mut not_assessed = 0usize;
    let mut unclassified = 0usize;
    for row in live_rows {
        match &row.kind.0 {
            Some(FindingKind::Defect) => defects += 1,
            Some(FindingKind::Deviation(_)) => deviations += 1,
            Some(FindingKind::ConventionObserved(_)) => conventions_followed += 1,
            Some(FindingKind::NoConvention(_)) => no_convention += 1,
            Some(FindingKind::NotAssessed(_)) => not_assessed += 1,
            None => unclassified += 1,
        }
    }
    for (label, count, where_to_look) in [
        ("Defects", defects, "All Findings"),
        (
            "Deviations from your own conventions",
            deviations,
            "Deviations (one row per convention), All Findings (one row per site)",
        ),
        (
            "Conventions followed consistently",
            conventions_followed,
            "Conventions",
        ),
        ("No convention adopted", no_convention, "Conventions"),
        ("Not assessed this scan", not_assessed, "Coverage"),
    ] {
        ws.write_string(r, 0, label)?;
        ws.write_number(r, 1, count as f64)?;
        ws.write_string(r, 2, where_to_look)?;
        r += 1;
    }
    // Shown ONLY when non-zero, so that when it appears it means something: a row whose
    // producer assigned no kind is an unmapped emission path, and A5.6's backfill rule is
    // that such a row is surfaced as a gap rather than silently counted as a defect.
    if unclassified > 0 {
        ws.write_string_with_format(r, 0, "Unclassified", &fmts.bold)?;
        ws.write_number_with_format(r, 1, unclassified as f64, &fmts.bold)?;
        ws.write_string(r, 2, "All Findings")?;
        r += 1;
    }
    r += 1;

    // ── Disposition summary (must reconcile with the PDF's own numbers exactly) ─────
    // C3-6: `informational` is counted here too (it wasn't before — the old `_ => {}` arm
    // silently dropped a held-for-review row from every count) and `open` mirrors
    // `report_export::build_report_json`'s own formula exactly: every `Unresolved` row MINUS
    // the held-for-review ones (an informational/held row is always `Unresolved` by
    // construction — see `is_informational`/`is_held_for_review` — so this keeps `open` an
    // ACTION count, not an appendix one), never just "unresolved rows not routed
    // informational/held" counted directly, so the two artifacts can't drift if a future gate
    // ever routes a non-`Unresolved` row that way.
    //
    // C5-4: `held` is counted SEPARATELY from `informational` (same split as
    // `report_export::effective_bucket`'s two buckets) but both feed the SAME
    // `held_for_review_total` shown in this one disposition-summary line, matching the PDF's
    // `ExecutiveSummaryJson::held_for_review` reconciliation exactly — a high/critical
    // calibration hedge lives in `held`, not `informational`, but it still counts here.
    let mut do_now = 0usize;
    let mut do_next = 0usize;
    let mut plan = 0usize;
    let mut accepted = 0usize;
    let mut informational = 0usize;
    let mut held = 0usize;
    let mut unresolved = 0usize;
    for row in live_rows {
        match row.bucket {
            "do_now" => do_now += 1,
            "do_next" => do_next += 1,
            "plan" => plan += 1,
            "accepted" => accepted += 1,
            "informational" => informational += 1,
            "held" => held += 1,
            _ => {}
        }
        // Mirrors `report_export`'s own `open` count, which also sums Unresolved + Confirmed
        // before subtracting the held/informational rows — this line is the workbook half of
        // the reconciliation the comment above this block demands.
        if matches!(
            row.disposition_kind,
            Some(Disposition::Unresolved) | Some(Disposition::Confirmed)
        ) {
            unresolved += 1;
        }
    }
    let held_for_review_total = informational + held;
    let open = unresolved.saturating_sub(held_for_review_total);
    ws.write_string(
        r,
        0,
        format!(
            "Disposition summary: {open} open, {do_now} do now, {do_next} do next, {plan} \
             planned, {accepted} accepted, {held_for_review_total} held for review, {fp_count} \
             excluded as false positives, {} dependency {}.",
            dep_rows.len(),
            if dep_rows.len() == 1 {
                "advisory"
            } else {
                "advisories"
            }
        ),
    )?;
    r += 2;

    // ── Legend ──────────────────────────────────────────────────────────────
    ws.write_string_with_format(r, 0, "Legend", &fmts.section_title)?;
    r += 1;
    ws.write_string(r, 0, "Severity")?;
    r += 1;
    for sev in ["critical", "high", "medium", "low"] {
        ws.write_string_with_format(r, 0, title_case(sev), &fmts.severity_cell(sev))?;
        r += 1;
    }
    r += 1;
    ws.write_string_with_format(r, 0, "Disposition vocabulary", &fmts.bold)?;
    r += 1;
    for line in [
        "Open (recommended: ...) — no triage decision yet this session",
        "Confirmed by the reviewer (recommended: ...) — the auditor read this finding and \
         judged it real; still open work, the recommendation is unchanged",
        "Accepted risk / Needs client confirmation — an Ignored disposition (see \
         confirmed_by_client)",
        "Tech debt, resolve now / planned (resolve later) — a TechDebt disposition",
        "Pre-existing accepted debt (baseline suppression) — accepted in a PRIOR run",
        "False positive — auditor-dispositioned, excluded from every working sheet (see \
         the False Positives sheet)",
    ] {
        ws.write_string(r, 0, line)?;
        r += 1;
    }
    r += 1;
    ws.write_string_with_format(r, 0, "Provenance vocabulary", &fmts.bold)?;
    r += 1;
    for line in [
        "Deterministic — corpus-grounded, cites an authoritative external source",
        "Preview: {tool} — scan-time deterministic tool pass, not yet wired into the gate",
        "AI-advisory — model-inferred, no corpus citation",
    ] {
        ws.write_string(r, 0, line)?;
        r += 1;
    }
    r += 1;
    ws.write_string_with_format(r, 0, "Confidence", &fmts.bold)?;
    r += 1;
    for line in [
        "high — a clear, concrete violation",
        "needs-review — the calibration pass flagged it as debatable/theoretical/under-evidenced",
    ] {
        ws.write_string(r, 0, line)?;
        r += 1;
    }
    r += 2;

    // ── Hyperlinked table of contents ───────────────────────────────────────
    ws.write_string_with_format(r, 0, "Contents", &fmts.section_title)?;
    r += 1;
    let toc_header_row = r;
    for (c, h) in ["Sheet", "Findings", "Worst severity"].iter().enumerate() {
        ws.write_string_with_format(toc_header_row, c as u16, *h, &fmts.header)?;
    }
    r += 1;
    for sheet in sheet_plan {
        let link = format!("internal:'{}'!A1", sheet.sheet_name);
        ws.write_url_with_text(r, 0, link.as_str(), &sheet.display_name)?;
        ws.write_number(r, 1, sheet.count as f64)?;
        ws.write_string(r, 2, &sheet.worst_severity)?;
        r += 1;
    }

    Ok(())
}

// ── Public entry point ──────────────────────────────────────────────────────────────

/// Build the complete Excel workbook from a completed scan + the client's triage
/// dispositions + the (optional, best-effort) rule corpus — the Excel sibling of
/// [`crate::report_export::build_report_json`]. Pure aside from the in-memory
/// `rust_xlsxwriter` buffer; fully unit-testable.
pub fn build_workbook(
    report: &crate::onboard::SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> anyhow::Result<Vec<u8>> {
    let (rows, dep_rows) = partition_rows(report, dispositions, corpus, &opts.chosen_options);

    let live_rows: Vec<&FindingRow> = rows.iter().filter(|r| !r.is_fp).collect();
    let fp_rows: Vec<&FindingRow> = rows.iter().filter(|r| r.is_fp).collect();

    // Pass 3: the kind partition. `live_rows` stays the WHOLE non-FP set — the Index's
    // category matrix and disposition summary reconcile against the PDF's executive summary
    // and must keep counting every row. It is the SHEETS that partition.
    let findings_rows: Vec<&FindingRow> = live_rows
        .iter()
        .copied()
        .filter(|r| kind_sheet(r) == KindSheet::Findings)
        .collect();
    let convention_sheet_rows: Vec<&FindingRow> = live_rows
        .iter()
        .copied()
        .filter(|r| kind_sheet(r) == KindSheet::Conventions)
        .collect();
    let coverage_kind_rows: Vec<&FindingRow> = live_rows
        .iter()
        .copied()
        .filter(|r| kind_sheet(r) == KindSheet::Coverage)
        .collect();
    let deviation_groups = build_deviation_groups(&findings_rows, corpus);
    let convention_rows = build_convention_rows(&convention_sheet_rows, corpus);
    let not_assessed_rows = build_not_assessed_rows(&coverage_kind_rows, corpus);

    // Per-area views are views of the FINDINGS set, not of every statement: an area tab that
    // mixed "you consistently use cursor pagination" into the same list as a critical
    // injection is the flattening Pass 3 exists to undo.
    let mut by_category: BTreeMap<String, Vec<&FindingRow>> = BTreeMap::new();
    for r in &findings_rows {
        by_category.entry(r.category.clone()).or_default().push(*r);
    }
    let mut category_plan: Vec<(String, Vec<&FindingRow>)> = by_category.into_iter().collect();
    category_plan.sort_by(|a, b| {
        let (rank_a, _) = category_status(&a.1);
        let (rank_b, _) = category_status(&b.1);
        (rank_a, &a.0).cmp(&(rank_b, &b.0))
    });

    let mut used_names: HashSet<String> = HashSet::new();
    used_names.insert("Index".to_string());

    let mut sheet_plan: Vec<SheetSummary> = Vec::new();
    let all_findings_name = sanitize_sheet_name("All Findings", &mut used_names);
    sheet_plan.push(SheetSummary {
        display_name: "All Findings".to_string(),
        sheet_name: all_findings_name.clone(),
        count: findings_rows.len(),
        worst_severity: worst_severity_label(&findings_rows),
    });

    let deviations_name = sanitize_sheet_name("Deviations", &mut used_names);
    sheet_plan.push(SheetSummary {
        display_name: "Deviations".to_string(),
        sheet_name: deviations_name.clone(),
        count: deviation_groups.len(),
        worst_severity: deviation_groups
            .first()
            .map(|g| g.worst_severity.clone())
            .unwrap_or_else(|| "Clean".to_string()),
    });

    let conventions_name = sanitize_sheet_name("Conventions", &mut used_names);
    sheet_plan.push(SheetSummary {
        display_name: "Conventions".to_string(),
        sheet_name: conventions_name.clone(),
        count: convention_rows.len(),
        // A convention observation is informational by construction (it ships at `info` and
        // nothing is wrong at a site), so a severity column here would be a column of noise.
        worst_severity: String::new(),
    });

    let mut category_sheets: Vec<(String, Vec<&FindingRow>, String)> = Vec::new();
    for (cat, rows_for_cat) in category_plan {
        let sanitized = sanitize_sheet_name(&cat, &mut used_names);
        let (_, color) = category_status(&rows_for_cat);
        sheet_plan.push(SheetSummary {
            display_name: cat.clone(),
            sheet_name: sanitized.clone(),
            count: rows_for_cat.len(),
            worst_severity: worst_severity_label(&rows_for_cat),
        });
        category_sheets.push((sanitized, rows_for_cat, color.to_string()));
    }

    let dep_name = sanitize_sheet_name("Dependencies", &mut used_names);
    sheet_plan.push(SheetSummary {
        display_name: "Dependencies".to_string(),
        sheet_name: dep_name.clone(),
        count: dep_rows.len(),
        worst_severity: worst_dep_severity_label(&dep_rows),
    });

    let fp_name = sanitize_sheet_name("False Positives", &mut used_names);
    sheet_plan.push(SheetSummary {
        display_name: "False Positives".to_string(),
        sheet_name: fp_name.clone(),
        count: fp_rows.len(),
        worst_severity: if fp_rows.is_empty() {
            "n/a".to_string()
        } else {
            worst_severity_label(&fp_rows)
        },
    });

    let coverage_rows = build_coverage_rows(report, &rows, corpus);
    let coverage_name = sanitize_sheet_name("Coverage", &mut used_names);
    sheet_plan.push(SheetSummary {
        display_name: "Coverage".to_string(),
        sheet_name: coverage_name.clone(),
        count: coverage_rows.len(),
        worst_severity: String::new(),
    });

    let dep_coverage_notes: Vec<String> = report
        .coverage_notes
        .iter()
        .filter(|n| n.tool == "osv-scanner")
        .map(|n| n.message.clone())
        .collect();

    let mut wb = Workbook::new();
    // Reproducibility: pin the workbook's document properties to the SCAN time.
    // `rust_xlsxwriter`'s `DocProperties::default` stamps `ExcelDateTime::utc_now()` at second
    // precision and writes it to `docProps/core.xml` as BOTH `dcterms:created` and
    // `dcterms:modified`, so with nothing set, two exports of one report differed there — the
    // workbook half of the same "cannot prove two deliverables are the same report" problem as
    // the PDF's creation timestamp (see `report_export::compile_pdf`). It is also the more
    // honest metadata: a client inspecting the file's properties is asking when the audit ran.
    //
    // `ExcelDateTime::from_timestamp` rather than handing over the `chrono` value directly,
    // because `rust_xlsxwriter`'s optional `chrono` feature is not enabled for this crate and
    // enabling it to save one conversion is not worth a dependency-surface change.
    //
    // Left at the library default when provenance carries no parseable instant: that path is
    // non-reproducible by construction, exactly as `generated_at_label` documents, and
    // stamping an invented constant on a client deliverable would be worse than admitting it.
    if let Some(scan_time) = crate::report_export::scan_instant(&report.provenance) {
        if let Ok(created) = rust_xlsxwriter::ExcelDateTime::from_timestamp(scan_time.timestamp()) {
            wb.set_properties(
                &rust_xlsxwriter::DocProperties::new().set_creation_datetime(&created),
            );
        }
    }
    let fmts = Formats::new();

    write_index_sheet(
        &mut wb,
        report,
        opts,
        &live_rows,
        fp_rows.len(),
        &dep_rows,
        &sheet_plan,
        corpus,
        &fmts,
    )
    .context("writing the Index sheet")?;
    // ORDER MATTERS: "All Findings" must stay the SECOND worksheet added (sheet2.xml) —
    // several cross-artifact equality tests read specific cells out of that entry by name.
    write_findings_sheet(
        &mut wb,
        &all_findings_name,
        &findings_rows,
        &fmts,
        false,
        None,
    )
    .context("writing the All Findings sheet")?;
    write_deviations_sheet(&mut wb, &deviations_name, &deviation_groups, &fmts)
        .context("writing the Deviations sheet")?;
    write_conventions_sheet(&mut wb, &conventions_name, &convention_rows, &fmts)
        .context("writing the Conventions sheet")?;
    for (sanitized, rows_for_cat, color) in &category_sheets {
        write_findings_sheet(
            &mut wb,
            sanitized,
            rows_for_cat,
            &fmts,
            false,
            Some(color.as_str()),
        )
        .context("writing a category sheet")?;
    }
    write_dependencies_sheet(&mut wb, &dep_name, &dep_rows, &dep_coverage_notes, &fmts)
        .context("writing the Dependencies sheet")?;
    write_findings_sheet(&mut wb, &fp_name, &fp_rows, &fmts, true, Some("#6B7280"))
        .context("writing the False Positives sheet")?;
    let rules_applied_rows = build_rules_applied_rows(report, corpus);
    // W7 (G1/G3): the same concise per-tool coverage lines the PDF/JSON disclose through
    // `failed_passes` — never the notes' raw internal diagnostics.
    let tool_coverage_notes =
        crate::report_export::tool_coverage_disclosures(&report.coverage_notes);
    // W7 (G2): every rule the ledger confirms did not run, sorted for a stable sheet.
    let mut rules_not_run: Vec<(String, String)> = report
        .ledger
        .excluded_rules()
        .into_iter()
        .map(|(id, reason)| (id.to_string(), reason.to_string()))
        .collect();
    rules_not_run.sort();
    write_coverage_sheet(
        &mut wb,
        &coverage_name,
        &coverage_rows,
        &report.excluded_mechanical_rules,
        &rules_applied_rows,
        &tool_coverage_notes,
        &rules_not_run,
        &not_assessed_rows,
        &fmts,
    )
    .context("writing the Coverage sheet")?;

    wb.save_to_buffer()
        .context("serializing the xlsx workbook to bytes")
}

// ── `findings.json` (product export, machine-readable sibling of the workbook) ────────

/// The `findings.json` payload bundled into the product-export zip: the machine-readable
/// version of the Excel data. `findings` is the WHOLE non-FP (severity-desc-then-repo/path/
/// line-sorted) [`FindingRow`] set — the same `partition_rows` output the workbook is built
/// from, so the two can never disagree on a field value.
///
/// It is NOT the same set as the workbook's "All Findings" sheet, and this comment claimed it
/// was until 2026-10-12. Since the Pass-3 restructure the workbook PARTITIONS that set across
/// three destinations by [`FindingKind`] (see `kind_sheet`): "All Findings" takes only
/// Defect/Deviation/absent-kind rows, Conventions takes the informational ones, and Coverage's
/// "could not complete" section takes the not-assessed ones. `findings.json` is deliberately
/// unpartitioned — a machine-readable export exists to be filtered by its consumer, and
/// dropping the informational rows from it would make the JSON a lossy subset of the workbook
/// it is supposed to be the complete sibling of.
///
/// That distinction is the whole point of writing it down: the code is right and the comment
/// was wrong, and someone trusting the comment would "restore parity" by DELETING rows from
/// the JSON. `req_pass3_every_exported_row_lands_on_exactly_one_sheet` pins the real
/// invariant — every row here lands on exactly one of the three sheets.
///
/// `provenance` and `summary` are reused VERBATIM from the already-built
/// [`crate::report_export::AuditReportJson`] (`cover` and `executive_summary` respectively)
/// rather than re-derived here — same discipline as the row data: one computation, three
/// consumers (PDF, xlsx, JSON).
#[derive(Serialize)]
pub struct FindingsExport {
    pub provenance: crate::report_export::CoverJson,
    pub summary: crate::report_export::ExecutiveSummaryJson,
    pub findings: Vec<FindingRow>,
}

/// Build the `findings.json` payload — the JSON sibling of [`build_workbook`]. Takes the
/// already-built `report_json` (the SAME [`crate::report_export::AuditReportJson`] the PDF
/// was compiled from and the caller also passes to `build_workbook`'s call site) so
/// `provenance`/`summary` are reused, never recomputed a second time from `report` directly.
/// `pub` (not `pub(crate)`): needed both by `lib.rs`'s product-export route and by the
/// integration test `tests/generate_sample_report.rs`, which links `camerata-server` as an
/// external crate.
pub fn build_findings_export(
    report: &crate::onboard::SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    report_json: &crate::report_export::AuditReportJson,
    chosen_options: &HashMap<String, String>,
) -> FindingsExport {
    let (rows, _dep_rows) = partition_rows(report, dispositions, corpus, chosen_options);
    let findings: Vec<FindingRow> = rows.into_iter().filter(|r| !r.is_fp).collect();
    FindingsExport {
        provenance: report_json.cover.clone(),
        summary: report_json.executive_summary.clone(),
        findings,
    }
}

/// Build the per-dataset JSON exports that ride in the product ZIP under `json/`. Each file is
/// a second SERIALIZER over the SAME typed row structs the xlsx sheets are built from (the exact
/// `partition_rows` + `build_*` outputs [`build_workbook`] uses, called here identically), never
/// an independent walk of the scan - so `json/`, the xlsx, and `findings.json` can never
/// disagree. Returns `(archive_path, pretty-JSON bytes)` pairs.
///
/// The eight machine-readable datasets every product package ships, as `csv/{name}.csv` and
/// `json/{name}.json`. The ONE definition: the release gate's dataset-set check and the export
/// tests both read it, so a dataset added here is demanded of every package.
pub const DATASETS: [&str; 8] = [
    "all-findings",
    "deviations",
    "conventions",
    "not-assessed",
    "dependencies",
    "false-positives",
    "coverage",
    "rules-applied",
];

/// `json/all-findings.json` is the flat sibling of `findings.json`'s `findings` array (the whole
/// non-FP set); the other files are the partitioned sheet tables, each its own one-table file.
pub fn build_json_exports(
    report: &crate::onboard::SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let (rows, dep_rows) = partition_rows(report, dispositions, corpus, &opts.chosen_options);
    let live_rows: Vec<&FindingRow> = rows.iter().filter(|r| !r.is_fp).collect();
    let fp_rows: Vec<&FindingRow> = rows.iter().filter(|r| r.is_fp).collect();
    let findings_rows: Vec<&FindingRow> = live_rows
        .iter()
        .copied()
        .filter(|r| kind_sheet(r) == KindSheet::Findings)
        .collect();
    let convention_sheet_rows: Vec<&FindingRow> = live_rows
        .iter()
        .copied()
        .filter(|r| kind_sheet(r) == KindSheet::Conventions)
        .collect();
    let coverage_kind_rows: Vec<&FindingRow> = live_rows
        .iter()
        .copied()
        .filter(|r| kind_sheet(r) == KindSheet::Coverage)
        .collect();

    let deviation_groups = build_deviation_groups(&findings_rows, corpus);
    let convention_rows = build_convention_rows(&convention_sheet_rows, corpus);
    let not_assessed_rows = build_not_assessed_rows(&coverage_kind_rows, corpus);
    let coverage_rows = build_coverage_rows(report, &rows, corpus);
    let rules_applied_rows = build_rules_applied_rows(report, corpus);

    // One (path, bytes) per table; pretty-printed for a diffable, human-skimmable file.
    Ok(vec![
        (
            "json/all-findings.json".to_string(),
            serde_json::to_vec_pretty(&live_rows)?,
        ),
        (
            "json/deviations.json".to_string(),
            serde_json::to_vec_pretty(&deviation_groups)?,
        ),
        (
            "json/conventions.json".to_string(),
            serde_json::to_vec_pretty(&convention_rows)?,
        ),
        (
            "json/not-assessed.json".to_string(),
            serde_json::to_vec_pretty(&not_assessed_rows)?,
        ),
        (
            "json/dependencies.json".to_string(),
            serde_json::to_vec_pretty(&dep_rows)?,
        ),
        (
            "json/false-positives.json".to_string(),
            serde_json::to_vec_pretty(&fp_rows)?,
        ),
        (
            "json/coverage.json".to_string(),
            serde_json::to_vec_pretty(&coverage_rows)?,
        ),
        (
            "json/rules-applied.json".to_string(),
            serde_json::to_vec_pretty(&rules_applied_rows)?,
        ),
    ])
}

/// Convert a JSON array-of-objects (one row per object) into RFC 4180 CSV bytes, via the `csv`
/// crate (never hand-rolled quoting). Header = the union of object keys in first-seen order, so
/// the column set is stable and complete even if a later row carries a key an earlier one
/// omitted. A scalar cell is written as its text; `null` as empty; a nested array/object (e.g.
/// a finding's flattened `kind` payload or its `also_locations`) as a COMPACT JSON string in the
/// one cell - CSV is flat, so a nested value is preserved losslessly as JSON rather than
/// dropped. Deriving CSV from the SAME JSON the `json/` files carry makes the two formats agree
/// by construction.
fn json_array_to_csv(value: &serde_json::Value) -> anyhow::Result<Vec<u8>> {
    let rows = value.as_array().cloned().unwrap_or_default();

    // Header: union of keys, first-seen order.
    let mut headers: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in &rows {
        if let Some(obj) = row.as_object() {
            for k in obj.keys() {
                if seen.insert(k.clone()) {
                    headers.push(k.clone());
                }
            }
        }
    }

    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(&headers)?;
    for row in &rows {
        let obj = row.as_object();
        let record: Vec<String> = headers
            .iter()
            .map(|key| match obj.and_then(|o| o.get(key)) {
                None | Some(serde_json::Value::Null) => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Bool(b)) => b.to_string(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                // Flat format: a nested value is kept losslessly as compact JSON in its cell.
                Some(other) => serde_json::to_string(other).unwrap_or_default(),
            })
            .collect();
        wtr.write_record(&record)?;
    }
    Ok(wtr.into_inner()?)
}

/// Build the per-dataset CSV exports that ride in the product ZIP under `csv/` - the plain-table
/// sibling of [`build_json_exports`]. Each CSV is derived from the EXACT JSON that file's `json/`
/// sibling carries (via [`json_array_to_csv`]), so `csv/`, `json/`, the xlsx, and `findings.json`
/// all trace to the one `partition_rows` pass and cannot disagree. Returns `(csv/<name>.csv,
/// bytes)` pairs mirroring the eight JSON tables.
pub fn build_csv_exports(
    report: &crate::onboard::SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let json = build_json_exports(report, dispositions, corpus, opts)?;
    json.into_iter()
        .map(|(path, bytes)| {
            // json/foo.json -> csv/foo.csv
            let name = path
                .strip_prefix("json/")
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap_or(&path);
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            Ok((format!("csv/{name}.csv"), json_array_to_csv(&value)?))
        })
        .collect()
}

/// Killing tests for the cargo-mutants survivors of 2026-10-28 (docs/MUTATION_TESTING.md).
#[cfg(test)]
mod mutation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::{AuditedRef, Finding, ScanProvenance};
    use std::io::Read;

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
            code_lines_by_language: Vec::new(),
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
                    ..AuditedRef::default()
                }],
                audit_model: Some("test-model".to_string()),
                calibration_model: Some("test-cal-model".to_string()),
                clone_judge_model: None,
                mode: "parallel".to_string(),
                thorough: false,
                deep: false,
                rules_fingerprint: "fp".to_string(),
                audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
                camerata_version: "0.0.0-test".to_string(),
                osv_scanner_version: Some("1.9.0".to_string()),
                started_at: "2026-07-23T00:00:00Z".to_string(),
                finished_at: "2026-07-23T00:05:00Z".to_string(),
                model_sessions: Vec::new(),
            },
            recommendations: std::collections::HashMap::new(),
            failed_passes: Vec::new(),
            ledger: crate::scan_ledger::ScanLedger::new(),
        }
    }

    fn wire(state: &str, reason: &str) -> DispositionWire {
        DispositionWire {
            state: state.to_string(),
            reason: reason.to_string(),
            bucket: String::new(),
            confirmed_by_client: false,
        }
    }

    fn empty_opts() -> ReportOptions {
        ReportOptions::default()
    }

    fn read_zip_entry(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .expect("workbook must be a valid zip");
        let mut file = archive
            .by_name(name)
            .unwrap_or_else(|_| panic!("missing zip entry {name}"));
        let mut s = String::new();
        file.read_to_string(&mut s)
            .expect("entry must be UTF-8 text");
        s
    }

    fn any_worksheet_xml_containing(bytes: &[u8], needle: &str) -> bool {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .expect("workbook must be a valid zip");
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).expect("zip entry");
            let name = file.name().to_string();
            if !name.starts_with("xl/worksheets/") {
                continue;
            }
            let mut s = String::new();
            if file.read_to_string(&mut s).is_ok() && s.contains(needle) {
                return true;
            }
        }
        false
    }

    /// Resolve one cell's ACTUAL text, following the shared-string pool `rust_xlsxwriter` uses
    /// for every repeated/non-numeric cell (`<c r="B2" t="s"><v>N</v></c>` — `N` indexes into
    /// `xl/sharedStrings.xml`, it is never the literal text). Needed whenever a test must pin
    /// which COLUMN a string landed in (e.g. "Headline", not "Detail") rather than merely
    /// whether the string appears anywhere in the workbook.
    fn cell_text(bytes: &[u8], sheet_name: &str, cell_ref: &str) -> String {
        let sheet_xml = read_zip_entry(bytes, sheet_name);
        let marker = format!("r=\"{cell_ref}\"");
        let cell_start = sheet_xml
            .find(&marker)
            .unwrap_or_else(|| panic!("cell {cell_ref} not found in {sheet_name}: {sheet_xml}"));
        let tail = &sheet_xml[cell_start..];
        let cell_end = tail
            .find("</c>")
            .unwrap_or_else(|| panic!("cell {cell_ref} has no closing </c>: {tail}"));
        let cell_xml = &tail[..cell_end];
        let v_start = cell_xml
            .find("<v>")
            .unwrap_or_else(|| panic!("cell {cell_ref} has no <v> value: {cell_xml}"))
            + 3;
        let v_end = cell_xml[v_start..]
            .find("</v>")
            .unwrap_or_else(|| panic!("cell {cell_ref}'s <v> has no closing tag: {cell_xml}"))
            + v_start;
        let idx: usize = cell_xml[v_start..v_end].parse().unwrap_or_else(|_| {
            panic!(
                "cell {cell_ref} value {:?} is not a shared-string index",
                &cell_xml[v_start..v_end]
            )
        });

        let shared_xml = read_zip_entry(bytes, "xl/sharedStrings.xml");
        let entry = shared_xml
            .split("<si>")
            .skip(1) // first chunk is the <sst ...> header, before the first <si>
            .nth(idx)
            .unwrap_or_else(|| {
                panic!("shared-string index {idx} out of range for cell {cell_ref}")
            });
        let t_open = entry.find('>').map(|i| i + 1).unwrap_or(0);
        let after_t_open = &entry[t_open..];
        let t_close = after_t_open
            .find("</t>")
            .unwrap_or_else(|| panic!("malformed <t> in shared string {idx}: {entry}"));
        after_t_open[..t_close].to_string()
    }

    /// The `xl/worksheets/sheetN.xml` entry backing a worksheet NAME. `rust_xlsxwriter`
    /// writes sheets in add order and `xl/workbook.xml` declares them in that same order, so
    /// the Nth declared name is `sheetN.xml`. Resolving by NAME rather than hard-coding an
    /// index means a sheet inserted ahead of the one under test cannot silently repoint an
    /// assertion at the wrong sheet — which is precisely the failure mode a multi-sheet
    /// restructure introduces.
    fn sheet_entry_for(bytes: &[u8], sheet_name: &str) -> String {
        let wb = read_zip_entry(bytes, "xl/workbook.xml");
        let names: Vec<String> = wb
            .split("<sheet name=\"")
            .skip(1)
            .map(|c| c[..c.find('"').expect("a declared sheet name is quoted")].to_string())
            .collect();
        let idx = names
            .iter()
            .position(|n| n == sheet_name)
            .unwrap_or_else(|| panic!("no worksheet named {sheet_name:?}; have {names:?}"));
        format!("xl/worksheets/sheet{}.xml", idx + 1)
    }

    /// Every shared-string VALUE actually referenced by cells on ONE worksheet.
    ///
    /// The shared-strings pool is workbook-wide, so a pool-level `contains` cannot tell which
    /// SHEET a string landed on. "This row must be on Conventions and NOT among the findings"
    /// and "no internal vocabulary may reach a client-facing cell on this sheet" are both
    /// per-sheet claims, and a pool-level check would pass vacuously on either.
    fn sheet_cell_strings(bytes: &[u8], sheet_name: &str) -> Vec<String> {
        let sheet_xml = read_zip_entry(bytes, &sheet_entry_for(bytes, sheet_name));
        let shared_xml = read_zip_entry(bytes, "xl/sharedStrings.xml");
        let unescape = |s: &str| {
            s.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&amp;", "&")
        };
        let pool: Vec<String> = shared_xml
            .split("<si>")
            .skip(1)
            .filter_map(|si| {
                // Mirrors `cell_text`: within an `<si>` chunk the first `>` closes the
                // `<t ...>` open tag (which may carry `xml:space="preserve"`).
                let start = si.find('>')? + 1;
                let end = si[start..].find("</t>")? + start;
                Some(unescape(&si[start..end]))
            })
            .collect();
        let mut out = Vec::new();
        for chunk in sheet_xml.split("t=\"s\"").skip(1) {
            let Some(v_at) = chunk.find("<v>") else {
                continue;
            };
            let v_start = v_at + 3;
            let Some(v_len) = chunk[v_start..].find("</v>") else {
                continue;
            };
            if let Ok(i) = chunk[v_start..v_start + v_len].parse::<usize>() {
                if let Some(s) = pool.get(i) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    /// One NUMERIC cell's value on a named worksheet. `cell_text` resolves shared strings and
    /// panics on a number; a numerator/denominator column is written with
    /// `write_number_with_format` precisely so it stays a number in Excel, so it needs its own
    /// reader. Asserts the cell is NOT a shared string, which is itself the claim for a
    /// count column.
    fn cell_number(bytes: &[u8], sheet_name: &str, cell_ref: &str) -> f64 {
        let sheet_xml = read_zip_entry(bytes, &sheet_entry_for(bytes, sheet_name));
        let marker = format!("r=\"{cell_ref}\"");
        let start = sheet_xml
            .find(&marker)
            .unwrap_or_else(|| panic!("cell {cell_ref} not found on {sheet_name}: {sheet_xml}"));
        let tail = &sheet_xml[start..];
        let end = tail
            .find("</c>")
            .unwrap_or_else(|| panic!("cell {cell_ref} has no closing </c>: {tail}"));
        let cell = &tail[..end];
        assert!(
            !cell.contains("t=\"s\""),
            "cell {cell_ref} on {sheet_name} is a shared string, not a number: {cell}"
        );
        let v_start = cell
            .find("<v>")
            .unwrap_or_else(|| panic!("cell {cell_ref} has no <v>: {cell}"))
            + 3;
        let v_end = cell[v_start..]
            .find("</v>")
            .unwrap_or_else(|| panic!("cell {cell_ref}'s <v> is unclosed: {cell}"))
            + v_start;
        cell[v_start..v_end]
            .parse()
            .unwrap_or_else(|e| panic!("cell {cell_ref} is not numeric ({e}): {cell}"))
    }

    /// The number of DATA rows on a named worksheet (every `<row ` minus its header row).
    fn sheet_data_row_count(bytes: &[u8], sheet_name: &str) -> usize {
        let sheet_xml = read_zip_entry(bytes, &sheet_entry_for(bytes, sheet_name));
        sheet_xml.matches("<row ").count().saturating_sub(1)
    }

    // ── Header schema is stable and in order ───────────────────────────────────

    #[test]
    fn header_schema_matches_the_design_column_order() {
        // Pass-1 W3 (A5.6): 23 -> 24 — the Kind column is appended LAST (a deliberate
        // data-structure change this test adapts to; tracker test-discipline exception (a)).
        // Pass-1 W7 (C5): 24 -> 26 — the Sites + "Also found at" columns append after Kind
        // (same exception (a): absorbed merge sites must be observable in the workbook).
        assert_eq!(HEADERS.len(), 26);
        assert_eq!(HEADERS[0], "Severity");
        // P2: the specific fix comes BEFORE the generic Recommended Fix — same
        // primary-then-secondary ordering as the PDF template.
        assert_eq!(HEADERS[21], "Fix (specific to this finding)");
        assert_eq!(HEADERS[22], "Recommended Fix");
        assert_eq!(HEADERS[23], "Kind");
        assert_eq!(HEADERS[24], "Sites");
        assert_eq!(HEADERS[25], "Also found at");
        assert_eq!(HEADERS.len(), WIDTHS.len());
    }

    // ── Workbook magic + basic shape ────────────────────────────────────────────

    #[test]
    fn build_workbook_returns_a_valid_xlsx_with_pk_magic() {
        let f = finding("SEC-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec!["SEC-1"]);
        let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
            .expect("build_workbook must succeed");
        assert!(!bytes.is_empty());
        assert_eq!(
            &bytes[0..2],
            b"PK",
            "xlsx must start with the zip magic bytes"
        );
    }

    #[test]
    fn json_exports_cover_every_table_and_parse() {
        let critical = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        let mut dep = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
        dep.snippet = "foo@1.0.0".to_string();
        let report = report_with(vec![critical, dep], vec!["SUPABASE-RLS-ENABLED-1"]);
        let sealed = report.clone().seal();

        let exports = build_json_exports(&sealed, &HashMap::new(), None, &empty_opts()).unwrap();
        let names: Vec<&str> = exports.iter().map(|(n, _)| n.as_str()).collect();
        for expected in [
            "json/all-findings.json",
            "json/deviations.json",
            "json/conventions.json",
            "json/not-assessed.json",
            "json/dependencies.json",
            "json/false-positives.json",
            "json/coverage.json",
            "json/rules-applied.json",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        assert_eq!(exports.len(), 8, "exactly the eight data tables");
        // Every file is valid, parseable JSON (an array).
        for (name, bytes) in &exports {
            let v: serde_json::Value = serde_json::from_slice(bytes)
                .unwrap_or_else(|e| panic!("{name} is not valid JSON: {e}"));
            assert!(v.is_array(), "{name} must be a JSON array of rows");
        }
    }

    #[test]
    fn csv_exports_cover_every_table_and_survive_nasty_cells() {
        // A detail with a comma, a double-quote and a newline - the exact RFC 4180 escaping
        // case a hand-rolled writer fails. It must round-trip as ONE cell, text intact.
        let mut f = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.rs", 1, "high");
        f.detail = "line one, with a comma and a \"quote\"\nand a second line".to_string();
        let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
        let sealed = report.clone().seal();

        let exports = build_csv_exports(&sealed, &HashMap::new(), None, &empty_opts()).unwrap();
        let names: Vec<&str> = exports.iter().map(|(n, _)| n.as_str()).collect();
        for expected in [
            "csv/all-findings.csv",
            "csv/deviations.csv",
            "csv/conventions.csv",
            "csv/not-assessed.csv",
            "csv/dependencies.csv",
            "csv/false-positives.csv",
            "csv/coverage.csv",
            "csv/rules-applied.csv",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        assert_eq!(exports.len(), 8);

        // Parse all-findings.csv back and confirm the nasty detail survived as one field.
        let all = &exports
            .iter()
            .find(|(n, _)| n == "csv/all-findings.csv")
            .unwrap()
            .1;
        let mut rdr = csv::Reader::from_reader(all.as_slice());
        let headers = rdr.headers().unwrap().clone();
        let detail_col = headers
            .iter()
            .position(|h| h == "detail")
            .expect("detail column");
        let rec = rdr.records().next().expect("one data row").unwrap();
        assert_eq!(
            &rec[detail_col], "line one, with a comma and a \"quote\"\nand a second line",
            "the comma/quote/newline cell must round-trip intact"
        );
    }

    #[tokio::test]
    async fn json_all_findings_matches_findings_json_rows() {
        // The load-bearing parity gate: json/all-findings.json is byte-for-byte the same data
        // as findings.json's `findings` array, because both serialize the SAME non-FP FindingRow
        // set from the SAME partition_rows pass. If they ever diverge, the artifacts disagree.
        let f = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
        let sealed = report.clone().seal();

        let report_json =
            crate::report_export::build_report_json(&sealed, &HashMap::new(), None, &empty_opts());
        let fe = build_findings_export(
            &sealed,
            &HashMap::new(),
            None,
            &report_json,
            &HashMap::new(),
        );
        let expected = serde_json::to_value(&fe.findings).unwrap();

        let exports = build_json_exports(&sealed, &HashMap::new(), None, &empty_opts()).unwrap();
        let all_findings = &exports
            .iter()
            .find(|(n, _)| n == "json/all-findings.json")
            .expect("all-findings present")
            .1;
        let got: serde_json::Value = serde_json::from_slice(all_findings).unwrap();

        assert_eq!(
            got, expected,
            "json/all-findings.json must equal findings.json's findings array"
        );
    }

    #[test]
    fn workbook_contains_the_expected_sheet_names_in_order() {
        let critical = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        let mut dep = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
        dep.snippet = "foo@1.0.0".to_string();
        let report = report_with(vec![critical, dep], vec!["SUPABASE-RLS-ENABLED-1"]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        for expected in [
            "name=\"Index\"",
            "name=\"All Findings\"",
            "name=\"Dependencies\"",
            "name=\"False Positives\"",
            "name=\"Coverage\"",
            // A9: the per-area tab is named from the CATEGORY. Without a corpus,
            // `SUPABASE-RLS-ENABLED-1` now resolves through the closed taxonomy to
            // "RLS Policy" rather than through its own first two tokens to "Supabase RLS".
            "name=\"RLS Policy\"",
        ] {
            assert!(
                workbook_xml.contains(expected),
                "workbook.xml missing {expected}: {workbook_xml}"
            );
        }
    }

    // ── C4-P4 residual defect 3: Index sheet "Audit" wording finishes the P6 rename ─────

    /// The Index sheet was left behind when the rest of the report moved from "Audit"/"audit"
    /// to the neutral "Codebase Inspection" product wording (P6, 2026-09-29) — its title row
    /// said "Camerata Audit", and its provenance rows said "Repos audited" / "Audit model"
    /// where the PDF cover/methodology already say "Repos inspected" / "Inspection model".
    /// Asserts the renamed strings are present and the word "audit" (any casing) never appears
    /// in the sheet's own label cells (column A) — "Camerata" itself contains no such
    /// substring, so this is a clean word-boundary check, not a scrub-then-check dance like the
    /// PDF template's equivalent test (which has to carve out internal field-accessor tokens).
    #[test]
    fn index_sheet_says_inspection_never_audit() {
        let f = finding("SEC-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec!["SEC-1"]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        // Cell text in this workbook is stored by reference into the shared-strings table, not
        // inline in the worksheet XML (see `cell_text`'s own `t="s"` + `<v>index</v>` lookup) —
        // the Index sheet's actual label text lives in `xl/sharedStrings.xml`. That table is
        // workbook-wide (every sheet's strings share it), and a DIFFERENT, out-of-scope sheet
        // legitimately carries "auditor" (the False Positives sheet's "auditor-dispositioned"
        // reason text — about the human reviewer, not report branding) — so this checks the
        // three EXACT renamed/removed Index-sheet strings rather than a blanket "no audit
        // anywhere in the workbook" scan, which would false-positive on that unrelated string.
        let strings_xml = read_zip_entry(&bytes, "xl/sharedStrings.xml");

        assert!(
            strings_xml.contains("Camerata Codebase Inspection"),
            "Index sheet title must use the renamed 'Codebase Inspection' wording: {strings_xml}"
        );
        assert!(
            strings_xml.contains("Repos inspected"),
            "Index sheet must say 'Repos inspected', matching the PDF cover: {strings_xml}"
        );
        assert!(
            strings_xml.contains("Inspection model"),
            "Index sheet must say 'Inspection model', matching the PDF methodology section: \
             {strings_xml}"
        );
        assert!(
            !strings_xml.contains("Camerata Audit "),
            "the old 'Camerata Audit' title wording must be gone: {strings_xml}"
        );
        assert!(
            !strings_xml.contains("Repos audited"),
            "the old 'Repos audited' wording must be gone: {strings_xml}"
        );
        assert!(
            !strings_xml.contains("Audit model"),
            "the old 'Audit model' wording must be gone: {strings_xml}"
        );
    }

    // ── P7: Coverage sheet's "Rules applied" appendix ────────────────────────────

    /// The Coverage sheet must carry a "Rules applied" appendix listing rule id, chosen
    /// alternative, evidence, and applicable yes/no for every recorded recommendation — so a
    /// wrong pick costs a glance at the workbook rather than surfacing as a false finding.
    #[test]
    fn coverage_sheet_lists_rules_applied_with_chosen_evidence_and_applicability() {
        let mut report = report_with(vec![], vec!["MULTI-RULE-1", "MULTI-RULE-2"]);
        report.recommendations.insert(
            "MULTI-RULE-1".to_string(),
            crate::ai_audit::RuleRecommendation {
                rule_id: "MULTI-RULE-1".to_string(),
                recommended_option_id: "opt-b".to_string(),
                recommendation_reasoning: "the repo already does it the B way".to_string(),
                hallucinated: false,
                operator_chosen: false,
                evidence: Some("src/api/list_users.rs:42 — cursor already threaded".to_string()),
                applicable: true,
                ..Default::default()
            },
        );
        report.recommendations.insert(
            "MULTI-RULE-2".to_string(),
            crate::ai_audit::RuleRecommendation {
                rule_id: "MULTI-RULE-2".to_string(),
                recommended_option_id: String::new(),
                recommendation_reasoning: "no evidence of this anywhere in the codebase"
                    .to_string(),
                hallucinated: false,
                operator_chosen: false,
                evidence: None,
                applicable: false,
                ..Default::default()
            },
        );
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();

        let shared_strings = read_zip_entry(&bytes, "xl/sharedStrings.xml");
        let contains = |needle: &str| {
            shared_strings.contains(needle) || any_worksheet_xml_containing(&bytes, needle)
        };

        assert!(
            contains("Rules applied (alternative selection)"),
            "appendix section header"
        );
        assert!(contains("MULTI-RULE-1"), "applicable rule id");
        assert!(
            contains("src/api/list_users.rs:42"),
            "the evidence line must be recoverable from the workbook"
        );
        assert!(contains("MULTI-RULE-2"), "not-applicable rule id");
        assert!(
            contains("Not applicable"),
            "a not-applicable rule's chosen column must say so plainly"
        );
        assert!(
            contains("Yes"),
            "the applicable column for the evidenced rule"
        );
        assert!(
            contains("No"),
            "the applicable column for the not-applicable rule"
        );
    }

    /// No recommendations recorded (a scan with no multi-option rules, or one predating P7) —
    /// the appendix must simply be absent, never an empty/broken header.
    #[test]
    fn coverage_sheet_omits_rules_applied_appendix_when_there_are_no_recommendations() {
        let report = report_with(vec![], vec!["SEC-1"]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        assert!(
            !any_worksheet_xml_containing(&bytes, "Rules applied (alternative selection)")
                && !read_zip_entry(&bytes, "xl/sharedStrings.xml")
                    .contains("Rules applied (alternative selection)"),
            "no recommendations recorded — the appendix must not render"
        );
    }

    // ── Category sheets match `category_for` (the PDF's own scorecard grouping) ──

    #[test]
    fn category_sheets_match_the_pdf_scorecard_categories() {
        let rls = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        let arch = finding("ARCH-1", "b.rs", 2, "medium");
        let report = report_with(vec![rls, arch], vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        // A9: both tabs are named from the CATEGORY, which now comes from the closed
        // taxonomy rather than the rule id's own first two tokens — "RLS Policy" not
        // "Supabase RLS", "Architecture" not "Arch".
        assert!(
            workbook_xml.contains("name=\"RLS Policy\""),
            "{workbook_xml}"
        );
        assert!(
            workbook_xml.contains("name=\"Architecture\""),
            "{workbook_xml}"
        );
    }

    // ── A9: a worksheet TAB NAME is never a slugified rule id ─────────────────

    /// req_a9: the per-area worksheets are named from the category, so a mangled category is
    /// not merely an ugly cell — it is the tab the client clicks on. This is the client-facing
    /// consequence the A9 scorecard fix exists to close, asserted at the surface a client
    /// actually sees.
    ///
    /// FAILS BEFORE THE CHANGE: `xl/workbook.xml` contained `name="Arch No"` and
    /// `name="Legacy Unmapped"` — the first two tokens of `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1`
    /// and `LEGACY-UNMAPPED-1`, title-cased. Red output was
    /// `a worksheet tab is named "Arch No" — the rule id ARCH-NO-CROSS-BOUNDARY-IMPORTS-1
    /// slugified`.
    #[test]
    fn req_a9_no_worksheet_tab_is_named_after_a_slugified_rule_id() {
        let report = report_with(
            vec![
                finding("ARCH-NO-CROSS-BOUNDARY-IMPORTS-1", "src/a.rs", 1, "high"),
                finding("LEGACY-UNMAPPED-1", "src/b.rs", 2, "high"),
            ],
            vec![],
        );
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        for slug in ["Arch No", "Legacy Unmapped"] {
            assert!(
                !workbook_xml.contains(&format!("name=\"{slug}\"")),
                "a worksheet tab is named {slug:?} — a rule id slugified. A client opening the \
                 deliverable must never see internal vocabulary on a tab: {workbook_xml}"
            );
        }
        // Positive half: the real categories DID get their tabs, so this is not green merely
        // because the sheets vanished.
        assert!(
            workbook_xml.contains("name=\"Architecture\""),
            "{workbook_xml}"
        );
        assert!(workbook_xml.contains("name=\"Other\""), "{workbook_xml}");
    }

    // ── req_det: two workbook builds of ONE report are byte-identical ─────────

    /// req_det: building the SAME report twice, across a second boundary, must produce
    /// byte-identical workbooks.
    ///
    /// FAILS BEFORE THE CHANGE: `rust_xlsxwriter`'s `DocProperties` default stamps
    /// `ExcelDateTime::utc_now()` at second precision into `docProps/core.xml` as BOTH
    /// `dcterms:created` and `dcterms:modified`, and nothing pinned it. Red output was
    /// `two builds of ONE report must be byte-identical; … first difference at byte 1247`,
    /// the two windows reading `2026-10-12T14:02:58Z` and `2026-10-12T14:03:00Z`.
    ///
    /// The sleep is what makes this test honest — two builds in the same second passed under
    /// the OLD code, which is exactly why the export-equivalence test flaked about one run in
    /// four instead of failing outright.
    #[test]
    fn req_det_two_workbook_builds_of_one_report_are_byte_identical() {
        let report = report_with(
            vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
            vec!["SEC-NO-HARDCODED-SECRETS-1"],
        );
        let sealed = report.clone().seal();
        let first = build_workbook(&sealed, &HashMap::new(), None, &empty_opts()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let second = build_workbook(&sealed, &HashMap::new(), None, &empty_opts()).unwrap();

        let first_diff = first
            .iter()
            .zip(&second)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| first.len().min(second.len()));
        assert_eq!(
            first,
            second,
            "two builds of ONE report must be byte-identical; lengths {} vs {}, first \
             difference at byte {first_diff}",
            first.len(),
            second.len()
        );
    }

    /// req_det: the workbook's document properties state when the AUDIT ran, not when the
    /// bytes were written — the same correctness argument as the PDF's creation timestamp.
    /// `rust_xlsxwriter` writes `dcterms:created` and `dcterms:modified` from this one value,
    /// so both are checked.
    ///
    /// FAILS BEFORE THE CHANGE: both carried the wall clock at build time.
    #[test]
    fn req_det_workbook_doc_properties_carry_the_scan_time() {
        let report = report_with(
            vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
            vec!["SEC-NO-HARDCODED-SECRETS-1"],
        );
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let core = read_zip_entry(&bytes, "docProps/core.xml");
        // `report_with` stamps finished_at = 2026-07-23T00:05:00Z.
        assert!(
            core.contains("<dcterms:created xsi:type=\"dcterms:W3CDTF\">2026-07-23T00:05:00Z"),
            "dcterms:created must be the scan instant: {core}"
        );
        assert!(
            core.contains("<dcterms:modified xsi:type=\"dcterms:W3CDTF\">2026-07-23T00:05:00Z"),
            "dcterms:modified must be the scan instant too: {core}"
        );
    }

    /// req_det: the Index sheet's human-readable "Generated" row states the scan time, so the
    /// workbook a client reads agrees with the PDF cover rather than contradicting it.
    ///
    /// FAILS BEFORE THE CHANGE: that cell was `chrono::Utc::now()`, so a workbook re-exported
    /// months later claimed to have been generated months after the audit it describes — and
    /// it straddled a minute boundary under load, which is source 2 in
    /// `crates/cli/tests/inspect_export_equivalence.rs`'s normalization.
    #[test]
    fn req_det_index_generated_row_is_the_scan_time() {
        let report = report_with(
            vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
            vec!["SEC-NO-HARDCODED-SECRETS-1"],
        );
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let shared = read_zip_entry(&bytes, "xl/sharedStrings.xml");
        assert!(
            shared.contains("2026-07-23 00:05 UTC"),
            "the Index 'Generated' row must state the scan time: {shared}"
        );
    }

    #[test]
    fn categories_with_zero_findings_get_no_sheet() {
        // SUPABASE-RLS-ENABLED-1 is audited but never fires — it must show up in
        // Coverage/Index only, never as its own (empty, broken-looking) sheet.
        //
        // A9: the audited id is a real one now. The old fixture used `ZZZ-1` and asserted no
        // `name="Zzz"` tab; since `ZZZ-1` resolves to the generic "Other" bucket, that
        // assertion would have passed without the sheet-suppression logic doing anything —
        // a guard that cannot fail. The id below has a category ("RLS Policy") that WOULD
        // be a tab if the suppression broke.
        let report = report_with(vec![], vec!["SUPABASE-RLS-ENABLED-1"]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        assert!(
            !workbook_xml.contains("name=\"RLS Policy\""),
            "an audited-but-empty category must not get its own sheet: {workbook_xml}"
        );
    }

    // ── False Positives sheet carries reasons ──────────────────────────────────

    #[test]
    fn false_positives_sheet_carries_the_auditor_reason() {
        let f = finding("SEC-1", "a.rs", 1, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&f),
            wire(
                "FalsePositive",
                "test fixture constant, not a live credential",
            ),
        );
        let report = report_with(vec![f], vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &dispositions, None, &empty_opts()).unwrap();
        // `rust_xlsxwriter` pools repeated strings into `xl/sharedStrings.xml` rather than
        // inlining them per-cell, so the reason text is recoverable there (or, for a
        // constant-memory mode, inline in the sheet XML) — check both.
        let shared_strings = read_zip_entry(&bytes, "xl/sharedStrings.xml");
        assert!(
            shared_strings.contains("not a live credential")
                || any_worksheet_xml_containing(&bytes, "not a live credential"),
            "the FP reason must be recoverable from the workbook"
        );
    }

    // ── Recommended-Fix: binds to authored `remediation`, NEVER `directive` (Fix 1) ─────

    #[tokio::test]
    async fn recommended_fix_is_populated_from_authored_remediation() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let mut f = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            "critical",
        );
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), Some(&corpus), &HashMap::new());
        assert!(
            rows[0]
                .fix
                .as_deref()
                .is_some_and(|s| s.contains("profiles")),
            "the Recommended Fix column must be populated from authored remediation, got: {:?}",
            rows[0].fix
        );
    }

    // ── Provenance: a preview row never renders "Deterministic" (defect 3, 2026-10-07) ──

    /// A preview/external-tool row whose rule id resolves through the P3/commodity-taint
    /// grounding map to a GROUNDED corpus citation (the exact shape
    /// `report_export::taint_sqli_tool_rule_id_resolves_through_the_grounding_map` proves for
    /// `citation.kind`) must still show its OWN tool in the `provenance` column, never
    /// "Deterministic" — that word is reserved for a row Camerata's own floor actually
    /// produced. Provenance (who produced this row) and citation kind (whether a real
    /// external standard backs it) are different axes; the citation itself is allowed to
    /// resolve "grounded" here, the provenance column is not.
    #[tokio::test]
    async fn a_grounded_preview_row_never_renders_deterministic_provenance() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        let mut f = finding(
            "camerata.security.taint-sql-injection-go",
            "internal/db.go",
            88,
            "high",
        );
        f.preview = true;
        f.preview_tool = Some("semgrep".to_string());
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), Some(&corpus), &HashMap::new());

        assert_eq!(
            rows[0].citation_kind, "grounded",
            "sanity: the citation itself resolves grounded via the tool-rule-id mapping"
        );
        assert_eq!(
            rows[0].provenance, "Preview: semgrep",
            "provenance must name the tool, never borrow 'Deterministic' from the grounded \
             citation: {:?}",
            rows[0].provenance
        );
    }

    /// The flip side: a NON-preview, genuinely deterministic floor finding with a grounded
    /// citation still renders "Deterministic" provenance — this fix must not have flipped
    /// the common case.
    #[tokio::test]
    async fn a_non_preview_grounded_finding_still_renders_deterministic_provenance() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let f = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.py", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), Some(&corpus), &HashMap::new());
        assert_eq!(rows[0].provenance, "Deterministic");
    }

    #[test]
    fn recommended_fix_is_none_not_fabricated_without_a_corpus() {
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        assert_eq!(
            rows[0].fix, None,
            "must not fabricate a fix when the corpus is absent"
        );
    }

    #[test]
    fn recommended_fix_is_none_when_remediation_is_unauthored() {
        // Every real corpus rule's default option now has authored `remediation` (the corpus-wide
        // authoring passes closed that gap), so hardcoding a real rule id here would stop proving
        // anything the moment that rule got authored. `ruleset_with_unauthored_rule` builds a
        // synthetic single-rule corpus with a real `directive` but `remediation: None`, pinning the
        // fail-safe permanently and independent of corpus authoring state.
        let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
        let f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), Some(&corpus), &HashMap::new());
        assert_eq!(
            rows[0].fix, None,
            "must omit the fix, never fall back to directive, when remediation is unauthored"
        );
    }

    // ── P2: fix_specific column — primary fix, populated from Finding.fix_specific ──────

    #[test]
    fn fix_specific_column_is_populated_from_the_finding() {
        let mut f = finding("ARCH-1", "app/auth/signout/route.ts", 12, "high");
        f.fix_specific = Some(
            "Use `safeInternalPath` from lib/redirect.ts, as app/auth/signin/route.ts does."
                .to_string(),
        );
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        assert_eq!(
            rows[0].fix_specific.as_deref(),
            Some("Use `safeInternalPath` from lib/redirect.ts, as app/auth/signin/route.ts does.")
        );
    }

    #[test]
    fn fix_specific_column_is_none_when_never_generated() {
        let f = finding("ARCH-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        assert_eq!(rows[0].fix_specific, None);
    }

    /// Both fixes land in the workbook — the specific one FIRST (col V), the generic one
    /// second (col W) — same ordering the PDF template renders.
    #[tokio::test]
    async fn fix_specific_and_recommended_fix_both_populate_in_the_workbook() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let mut f = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            "critical",
        );
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        f.fix_specific = Some("Enable RLS directly on `profiles`.".to_string());
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), Some(&corpus), &HashMap::new());
        assert_eq!(
            rows[0].fix_specific.as_deref(),
            Some("Enable RLS directly on `profiles`.")
        );
        assert!(rows[0]
            .fix
            .as_deref()
            .is_some_and(|s| s.contains("profiles")));
    }

    // ── `findings.json` (product export, machine-readable sibling of the workbook) ─────

    // ── Pass-1 W3 (A5.6/A5.2): the kind must survive to findings.json, losslessly ────────

    fn kinded(mut f: Finding, kind: camerata_api_types::audit::FindingKind) -> Finding {
        f.kind = Some(kind);
        f
    }

    /// req A5.6 — THE W3 end-to-end guarantee: every one of the five kinds (plus the
    /// legacy/unspecified case) reaches `findings.json` INTACT — flat `"kind"` discriminator
    /// and, for a Deviation, the full structured sentence (numerator / denominator /
    /// observed-majority convention) — because findings.json is the grader's input ("the
    /// JSON is just the data structure"); a kind that reached only the PDF would be the
    /// exact B2 defect class behind the hearth 0/12 result.
    ///
    /// FAILS BEFORE THE FIX: `FindingRow` had no kind field, so no `"kind"` key existed in
    /// findings.json at all (verified by the pre-W3 tree: `grep FindingKind crates/` was
    /// empty), and every assertion below had nothing to read.
    #[test]
    fn req_a5_6_every_kind_survives_to_findings_json_losslessly() {
        use camerata_api_types::audit::{
            ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind, NotAssessedKind,
            NotAssessedReason, ObservedOptionUsage,
        };
        let rows = vec![
            kinded(finding("SEC-1", "a.rs", 1, "critical"), FindingKind::Defect),
            kinded(
                finding("ARCH-API-VERSIONING-1", "b.rs", 2, "medium"),
                FindingKind::Deviation(DeviationKind {
                    deviating_sites: 2,
                    assessed_sites: Some(47),
                    convention_option_id: Some("versioned".to_string()),
                    convention_label: "URI-versioned endpoints".to_string(),
                }),
            ),
            kinded(
                finding("ARCH-PAGINATION-1", "c.rs", 3, "info"),
                FindingKind::ConventionObserved(ConventionObservedKind {
                    convention_option_id: "cursor".to_string(),
                    convention_label: "Cursor pagination".to_string(),
                }),
            ),
            kinded(
                finding("ARCH-ERROR-SHAPE-1", "", 0, "info"),
                FindingKind::NoConvention(NoConventionKind {
                    observed_options: vec![
                        ObservedOptionUsage {
                            option_id: "rfc7807".to_string(),
                            label: "RFC 7807 problem+json".to_string(),
                            sites: Some(3),
                        },
                        ObservedOptionUsage {
                            option_id: "ad-hoc".to_string(),
                            label: "Ad-hoc error bodies".to_string(),
                            sites: Some(2),
                        },
                    ],
                }),
            ),
            kinded(
                finding("RUST-NO-UNWRAP-1", "", 0, "info"),
                FindingKind::NotAssessed(NotAssessedKind {
                    reason: NotAssessedReason::NoFilesMatched,
                    reason_detail: "no rust files matched this scan's file set".to_string(),
                }),
            ),
            // A legacy row whose producer predates the taxonomy: kind stays None.
            finding("LEGACY-1", "z.rs", 9, "high"),
        ];
        let report = report_with(rows, vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        let v = serde_json::to_value(&export).expect("findings.json serializes");
        let rows = v["findings"].as_array().expect("findings array");
        assert_eq!(rows.len(), 6, "all six rows ship");
        let row_for = |rule: &str| -> &serde_json::Value {
            rows.iter()
                .find(|r| r["rule_id"] == rule)
                .unwrap_or_else(|| panic!("no findings.json row for {rule}"))
        };

        assert_eq!(row_for("SEC-1")["kind"], "defect");

        let dev = row_for("ARCH-API-VERSIONING-1");
        assert_eq!(dev["kind"], "deviation");
        assert_eq!(
            dev["deviating_sites"], 2,
            "numerator is structured, on the row"
        );
        assert_eq!(
            dev["assessed_sites"], 47,
            "denominator is structured, on the row"
        );
        assert_eq!(dev["convention_option_id"], "versioned");
        assert_eq!(
            dev["convention_label"], "URI-versioned endpoints",
            "the observed majority convention is renderable without parsing prose"
        );

        let conv = row_for("ARCH-PAGINATION-1");
        assert_eq!(conv["kind"], "convention_observed");
        assert_eq!(conv["convention_option_id"], "cursor");

        let nc = row_for("ARCH-ERROR-SHAPE-1");
        assert_eq!(nc["kind"], "no_convention");
        let observed = nc["observed_options"].as_array().expect("observed_options");
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0]["option_id"], "rfc7807");
        assert_eq!(observed[0]["sites"], 3);

        let na = row_for("RUST-NO-UNWRAP-1");
        assert_eq!(na["kind"], "not_assessed");
        assert_eq!(na["reason"], "no_files_matched");

        let legacy = row_for("LEGACY-1");
        assert_eq!(
            legacy["kind"], "unspecified",
            "an absent kind must surface explicitly in findings.json"
        );
        assert_ne!(legacy["kind"], "defect", "never silently a defect");
    }

    /// req A5.2 — the xlsx/findings.json twin of the PDF-side deviation ceiling: a deviation
    /// arriving at `high` exports at `medium` here too (same clamp, same corpus), while a
    /// defect under the same rule keeps `high`. FAILS BEFORE THE FIX: with
    /// `clamp_deviation_severity` absent from `partition_rows`, both exported `high`.
    #[test]
    fn req_a5_2_findings_json_caps_a_deviation_at_medium_but_not_a_defect() {
        use camerata_api_types::audit::{DeviationKind, FindingKind};
        let mut rule = camerata_rules::bare_rule("DEV-RULE-1", "test-fixture");
        rule.default_option = Some("default".to_string());
        rule.options = vec![camerata_rules::RuleOption {
            id: "default".to_string(),
            label: "Default".to_string(),
            directive: "do the right thing".to_string(),
            why: "because".to_string(),
            remediation: Some("Fix the thing.".to_string()),
            finding_headline: None,
            finding_detail: None,
            escalation: None,
            effort: None,
            violates: false,
        }];
        let mut corpus = camerata_rules::RuleSet::default();
        corpus.push(rule);

        let dev = kinded(
            finding("DEV-RULE-1", "a.rs", 1, "high"),
            FindingKind::Deviation(DeviationKind {
                deviating_sites: 1,
                assessed_sites: None,
                convention_option_id: Some("default".to_string()),
                convention_label: "Default".to_string(),
            }),
        );
        let defect = kinded(
            finding("DEV-RULE-1", "b.rs", 2, "high"),
            FindingKind::Defect,
        );
        let report = report_with(vec![dev, defect], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let v = serde_json::to_value(&export).unwrap();
        let sev_of = |path: &str| -> String {
            v["findings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["path"] == path)
                .unwrap_or_else(|| panic!("no row for {path}"))["severity"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            sev_of("a.rs"),
            "medium",
            "deviation capped at the default ceiling"
        );
        assert_eq!(
            sev_of("b.rs"),
            "high",
            "defect under the same rule untouched"
        );
    }

    /// req A5.6 — minimal workbook render: the All Findings sheet carries a `Kind` column
    /// whose cell is the same stable wire tag findings.json serializes (no layout redesign —
    /// that is Pass 3 work).
    #[test]
    fn req_a5_6_kind_column_renders_in_the_all_findings_sheet() {
        use camerata_api_types::audit::{DeviationKind, FindingKind};
        let dev = kinded(
            finding("DEV-RULE-1", "a.rs", 1, "medium"),
            FindingKind::Deviation(DeviationKind {
                deviating_sites: 2,
                assessed_sites: Some(5),
                convention_option_id: None,
                convention_label: "observed majority".to_string(),
            }),
        );
        let legacy = finding("LEGACY-1", "b.rs", 2, "high");
        let report = report_with(vec![dev, legacy], vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        // Cell text is written via the shared-strings table, so that's where a rendered
        // header/cell string is observable in the raw xlsx.
        let shared = read_zip_entry(&bytes, "xl/sharedStrings.xml");
        assert!(shared.contains(">Kind<"), "the Kind header must render");
        assert!(
            shared.contains(">deviation<"),
            "the deviation tag must render in a cell"
        );
        assert!(
            shared.contains(">unspecified<"),
            "a legacy row renders the explicit unspecified tag"
        );
        // And the schema itself pins the column: last column, stable position.
        assert_eq!(HEADERS[23], "Kind");
    }

    /// `findings_export.findings` must have the same length as the "All Findings" sheet
    /// (sheet2.xml — Index is sheet1) actually written into the xlsx: both are built from
    /// the SAME `partition_rows` call, filtered the same way (`!is_fp`), so this is the
    /// consistency guarantee, not a coincidence. An FP is excluded from both.
    #[test]
    fn findings_export_length_matches_the_all_findings_sheet_row_count() {
        let a = finding("SEC-1", "a.rs", 1, "critical");
        let b = finding("SEC-2", "b.rs", 2, "high");
        let fp = finding("SEC-3", "c.rs", 3, "medium");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&fp), wire("FalsePositive", "fixture"));
        let report = report_with(vec![a, b, fp], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            None,
            &empty_opts(),
        );
        let xlsx_bytes =
            build_workbook(&report.clone().seal(), &dispositions, None, &empty_opts()).unwrap();
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            None,
            &json,
            &HashMap::new(),
        );

        assert_eq!(
            findings_export.findings.len(),
            2,
            "the FP-dispositioned finding must be excluded, same as the All Findings sheet"
        );

        // "All Findings" is the second worksheet added (after Index) -> sheet2.xml.
        // `<row ` appears once per row, including the header — subtract 1 for it.
        let sheet_xml = read_zip_entry(&xlsx_bytes, "xl/worksheets/sheet2.xml");
        let row_count = sheet_xml.matches("<row ").count();
        assert_eq!(
            row_count.saturating_sub(1),
            findings_export.findings.len(),
            "findings.json row count must match the All Findings sheet's actual data rows"
        );
    }

    /// A finding's `fix` field in `findings.json` is exactly `resolve_fix`'s output for the
    /// same rule id + corpus + finding — the same guarantee the xlsx's Recommended Fix column
    /// has, and `null` (never a string), never `directive`, for a rule with no `fix` at all.
    #[tokio::test]
    async fn findings_export_fix_field_matches_resolve_fix() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let mut f = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            "critical",
        );
        f.captures
            .insert("table".to_string(), "profiles".to_string());
        let report = report_with(vec![f.clone()], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let expected =
            crate::report_export::resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f, None);
        assert!(
            expected.is_some(),
            "fixture rule must have authored remediation"
        );
        assert_eq!(findings_export.findings[0].fix, expected);
    }

    #[test]
    fn findings_export_fix_field_is_null_when_remediation_is_unauthored() {
        // Same rationale as `recommended_fix_is_none_when_remediation_is_unauthored` above: every
        // real corpus rule is now authored, so a synthetic single-rule corpus (via
        // `ruleset_with_unauthored_rule`) is the only way to permanently pin the "unauthored
        // remediation serializes as JSON null" fail-safe, independent of corpus authoring state.
        let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
        let f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
        let report = report_with(vec![f], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        assert_eq!(findings_export.findings[0].fix, None);
        let serialized = serde_json::to_value(&findings_export.findings[0]).unwrap();
        assert_eq!(
            serialized.get("fix"),
            Some(&serde_json::Value::Null),
            "an unauthored fix must serialize as JSON null, not be dropped from the object"
        );
    }

    /// P2: `findings.json` must carry `fix_specific` and place it BEFORE `fix` in the
    /// serialized object — the same primary-then-secondary ordering as the xlsx columns and
    /// the PDF template. `serde_json::Value`'s map does not preserve field order (it's a
    /// `BTreeMap` without the `preserve_order` feature), so this asserts on the serialized
    /// STRING, which a derived `Serialize` always emits in struct declaration order.
    #[test]
    fn findings_export_carries_fix_specific_before_fix() {
        let mut f = finding("ARCH-1", "app/auth/signout/route.ts", 12, "high");
        f.fix_specific = Some("Use `safeInternalPath` from lib/redirect.ts.".to_string());
        let report = report_with(vec![f], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        assert_eq!(
            findings_export.findings[0].fix_specific.as_deref(),
            Some("Use `safeInternalPath` from lib/redirect.ts.")
        );
        let serialized = serde_json::to_string(&findings_export.findings[0]).unwrap();
        let fix_specific_idx = serialized
            .find("\"fix_specific\":")
            .expect("fix_specific key must be present");
        let fix_idx = serialized
            .find("\"fix\":")
            .expect("fix key must be present");
        assert!(
            fix_specific_idx < fix_idx,
            "fix_specific must be serialized BEFORE fix, got: {serialized}"
        );
    }

    /// The top-level JSON object has exactly the documented shape: `provenance`, `summary`,
    /// `findings`.
    #[test]
    fn findings_export_json_shape_has_provenance_summary_and_findings_keys() {
        let f = finding("SEC-1", "a.rs", 1, "high");
        let report = report_with(vec![f], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        let v = serde_json::to_value(&findings_export).expect("must serialize");
        assert!(v.get("provenance").is_some(), "{v:?}");
        assert!(v.get("summary").is_some(), "{v:?}");
        assert!(
            v.get("findings").and_then(|f| f.as_array()).is_some(),
            "{v:?}"
        );
    }

    // ── Degenerate inputs must never panic ─────────────────────────────────────

    #[test]
    fn findings_export_zero_findings_is_empty_and_serializes_cleanly() {
        let report = report_with(vec![], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        assert!(findings_export.findings.is_empty());
        let bytes = serde_json::to_vec_pretty(&findings_export)
            .expect("zero findings must still serialize to valid JSON");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn findings_export_all_false_positive_produces_an_empty_findings_array() {
        let f1 = finding("SEC-1", "a.rs", 1, "critical");
        let f2 = finding("SEC-2", "b.rs", 2, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f1), wire("FalsePositive", "fixture"));
        dispositions.insert(finding_key(&f2), wire("FalsePositive", "fixture"));
        let report = report_with(vec![f1, f2], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            None,
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            None,
            &json,
            &HashMap::new(),
        );
        assert!(
            findings_export.findings.is_empty(),
            "all-FP input must yield an empty findings array, not panic"
        );
        serde_json::to_vec_pretty(&findings_export)
            .expect("all-FP input must still serialize to valid JSON");
    }

    // ── W3 (2026-09-30): the plain-language headline must reach JSON + xlsx, not just the
    // PDF ──────────────────────────────────────────────────────────────────────────────

    /// A single-rule synthetic corpus whose default option carries an AUTHORED, plain-language
    /// `finding_headline`/`finding_detail` pair — the same P4 mechanism `resolve_floor_finding_
    /// text` reads. Built via `bare_rule` + `RuleSet::push` (the general escape hatch, per its
    /// own doc comment) rather than `ruleset_with_unauthored_rule`, which deliberately leaves
    /// both fields `None`.
    fn ruleset_with_authored_floor_headline(
        rule_id: &str,
        headline: &str,
        detail: &str,
    ) -> camerata_rules::RuleSet {
        let mut rule = camerata_rules::bare_rule(rule_id, "test-fixture");
        rule.options = vec![camerata_rules::RuleOption {
            id: "default".to_string(),
            label: "Default".to_string(),
            directive: format!("Test-fixture detection directive for {rule_id}."),
            why: "Test-fixture rationale.".to_string(),
            remediation: None,
            finding_headline: Some(headline.to_string()),
            finding_detail: Some(detail.to_string()),
            escalation: None,
            effort: None,
            violates: false,
        }];
        rule.default_option = Some("default".to_string());
        let mut set = camerata_rules::RuleSet::default();
        set.push(rule);
        set
    }

    /// W3: a floor finding whose raw `Finding::detail` is internal, imperative gate prose
    /// ("Deny …") must export its AUTHORED plain-language headline in BOTH `findings.json`
    /// and the xlsx's "Headline" column — the exact same rewrite the PDF already applies via
    /// `client_headline_and_detail` — never the raw gate directive. The raw text must still be
    /// recoverable (completeness): it stays verbatim in `FindingRow::detail`/the "Detail"
    /// column.
    #[test]
    fn findings_export_and_xlsx_headline_use_the_plain_language_rewrite_not_raw_rule_text() {
        const AUTHORED_HEADLINE: &str = "A live secret key is committed to this repository.";
        const AUTHORED_DETAIL: &str =
            "A live secret key is committed to this repository and must be rotated.";
        const RAW_GATE_TEXT: &str = "Deny writing a file whose path marks it as secret-bearing.";

        let corpus = ruleset_with_authored_floor_headline(
            "SEC-TEST-HEADLINE-1",
            AUTHORED_HEADLINE,
            AUTHORED_DETAIL,
        );
        let mut f = finding("SEC-TEST-HEADLINE-1", "a.env", 1, "critical");
        f.detail = RAW_GATE_TEXT.to_string();
        let report = report_with(vec![f], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let xlsx_bytes = build_workbook(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        )
        .unwrap();

        assert_eq!(
            findings_export.findings.len(),
            1,
            "fixture must produce exactly one exported finding"
        );
        let exported_headline = &findings_export.findings[0].headline;
        assert_eq!(
            exported_headline, AUTHORED_HEADLINE,
            "findings.json headline must be the authored plain-language rewrite, not the raw \
             gate directive, got: {exported_headline:?}"
        );
        assert!(
            !exported_headline.starts_with("Deny"),
            "findings.json headline must never be the raw gate directive verbatim"
        );
        // C4-P2: the DETAIL column goes through the SAME plain-language rewrite as the
        // headline — before this fix, `findings.json`'s `detail` carried `RAW_GATE_TEXT`
        // verbatim (the PDF's `CuratedSiteJson::detail` already rewrote it; only the headline
        // was fixed in a prior cycle), so a floor rule's authored `finding_detail` template
        // rewrote cleanly in the PDF while the product export still printed internal gate
        // prose for the IDENTICAL finding. `FindingRow::detail` is no longer an
        // "internal-completeness, raw on purpose" field — the raw `finding.detail` lives only
        // on the underlying `Finding`, never re-exported verbatim here.
        assert_eq!(findings_export.findings[0].detail, AUTHORED_DETAIL);
        assert!(
            !findings_export.findings[0].detail.starts_with("Deny"),
            "findings.json detail must never be the raw gate directive verbatim"
        );

        // "All Findings" is the second worksheet added (after Index) -> sheet2.xml; row 1 is
        // the header, so the finding lands on row 2. Column B is "Headline" (index 1), column
        // U is "Detail" (index 20) per the `HEADERS` array. Resolve through the shared-string
        // pool (`cell_text`) rather than a whole-workbook substring search, so this pins which
        // COLUMN the rewrite landed in.
        let headline_cell = cell_text(&xlsx_bytes, "xl/worksheets/sheet2.xml", "B2");
        assert_eq!(
            headline_cell, AUTHORED_HEADLINE,
            "xlsx Headline column must carry the same plain-language rewrite, got: {headline_cell:?}"
        );
        assert!(
            !headline_cell.starts_with("Deny"),
            "xlsx Headline column must never be the raw gate directive verbatim"
        );
        let detail_cell = cell_text(&xlsx_bytes, "xl/worksheets/sheet2.xml", "U2");
        assert_eq!(
            detail_cell, AUTHORED_DETAIL,
            "xlsx Detail column must carry the same plain-language rewrite as the PDF, got: \
             {detail_cell:?}"
        );
        assert!(
            !detail_cell.starts_with("Deny"),
            "xlsx Detail column must never be the raw gate directive verbatim"
        );
    }

    /// General guard (not fixture-specific): across a MIX of findings — one whose rule has an
    /// authored plain-language template and one whose rule the corpus doesn't know at all (so
    /// it falls back to the pre-P4 `defect_headline`-over-`detail` derivation) — no exported
    /// headline in `findings.json` may begin with an internal imperative rule-text pattern.
    /// This pins the CLASS of bug (any floor rule's raw directive leaking as a client-facing
    /// headline), not one specific rule id or wording.
    #[test]
    fn no_findings_export_headline_matches_the_imperative_rule_text_pattern() {
        let corpus = ruleset_with_authored_floor_headline(
            "SEC-TEST-HEADLINE-2",
            "A vendor API token is exposed in this file.",
            "Rotate the token and move it to a secret manager.",
        );
        let mut authored = finding("SEC-TEST-HEADLINE-2", "a.py", 3, "high");
        authored.detail =
            "Require that secret values never be committed to source control.".to_string();
        // No corpus entry at all for this rule id — exercises the pre-P4 fallback path, whose
        // own `detail` (via the shared `finding()` helper) is not itself imperative, same as
        // any real AI-tier finding's house-convention prose.
        let unauthored = finding("SEC-TEST-NOT-IN-CORPUS-1", "b.py", 4, "medium");

        let report = report_with(vec![authored, unauthored], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );

        const IMPERATIVE_PREFIXES: &[&str] =
            &["Deny ", "Require ", "Disallow ", "Forbid ", "Enforce "];
        for row in &findings_export.findings {
            assert!(
                !IMPERATIVE_PREFIXES
                    .iter()
                    .any(|p| row.headline.starts_with(p)),
                "findings.json headline must never start with an internal imperative rule-text \
                 pattern, got: {:?}",
                row.headline
            );
        }
    }

    // ── Conditional formatting / autofilter / freeze panes are actually applied ────

    #[test]
    fn findings_sheets_carry_autofilter_freeze_and_conditional_formatting() {
        let mut f = finding("SEC-1", "a.rs", 1, "high");
        f.needs_review = true;
        let report = report_with(vec![f], vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        assert!(
            any_worksheet_xml_containing(&bytes, "<autoFilter"),
            "at least one findings sheet must carry an autofilter"
        );
        assert!(
            any_worksheet_xml_containing(&bytes, "state=\"frozen\""),
            "at least one findings sheet must freeze its header row"
        );
        assert!(
            any_worksheet_xml_containing(&bytes, "<conditionalFormatting"),
            "the needs-review conditional format must be present"
        );
    }

    // ── Degenerate inputs must never panic ─────────────────────────────────────

    #[test]
    fn zero_findings_produces_a_sane_workbook_without_panicking() {
        let report = report_with(vec![], vec![]);
        let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
            .expect("zero findings must not fail the export");
        assert_eq!(&bytes[0..2], b"PK");
    }

    #[test]
    fn all_false_positive_findings_produce_a_sane_workbook() {
        let f1 = finding("SEC-1", "a.rs", 1, "critical");
        let f2 = finding("SEC-2", "b.rs", 2, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&f1), wire("FalsePositive", "fixture"));
        dispositions.insert(finding_key(&f2), wire("FalsePositive", "fixture"));
        let report = report_with(vec![f1, f2], vec![]);
        let bytes = build_workbook(&report.clone().seal(), &dispositions, None, &empty_opts())
            .expect("all-FP input must not fail the export");
        assert_eq!(&bytes[0..2], b"PK");
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        // No real findings survive -> no category sheet, but All Findings/False Positives/
        // Dependencies/Coverage/Index must still exist.
        assert!(workbook_xml.contains("name=\"All Findings\""));
        assert!(workbook_xml.contains("name=\"False Positives\""));
    }

    #[test]
    fn no_dependency_findings_produces_a_sane_workbook() {
        let f = finding("SEC-1", "a.rs", 1, "low");
        let report = report_with(vec![f], vec![]);
        let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
            .expect("no-dependency-findings input must not fail the export");
        assert_eq!(&bytes[0..2], b"PK");
    }

    #[test]
    fn missing_directive_leaves_fix_empty_across_a_full_workbook_build() {
        // No corpus at all -> every rule's fix is empty; must not fail or fabricate.
        let f = finding("SEC-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
            .expect("missing-directive input must not fail the export");
        assert_eq!(&bytes[0..2], b"PK");
    }

    // ── Sheet-name sanitization ─────────────────────────────────────────────────

    #[test]
    fn sanitize_sheet_name_strips_reserved_characters_and_truncates() {
        let mut used = HashSet::new();
        let name = sanitize_sheet_name("Supa/base:RLS*[test]?", &mut used);
        assert!(!name.chars().any(|c| "[]:*?/\\".contains(c)));
    }

    #[test]
    fn sanitize_sheet_name_dedupes_collisions() {
        let mut used = HashSet::new();
        let a = sanitize_sheet_name("Security", &mut used);
        let b = sanitize_sheet_name("Security", &mut used);
        assert_ne!(a, b);
        assert!(b.ends_with(" (2)"));
    }

    // ── C3-6: one bucket computation, shared by every artifact ───────────────────────

    /// The three export artifacts (PDF, xlsx, `findings.json`) each used to bucket a finding
    /// independently: `build_report_json`'s matrix pass applied `is_informational`/
    /// `is_uncited_ai_finding` BEFORE `matrix_bucket`, but this module's `partition_rows`
    /// called `matrix_bucket` directly with no such gate at all — a hedged (`needs-review`)
    /// row the PDF/JSON correctly held out of do_now/do_next/plan could still land in an
    /// action tier in the xlsx, and the three artifacts' bucket counts could disagree (a real
    /// run showed a JSON 5/10/26 split plus 6 held for review against an xlsx 7/8/32 split —
    /// exactly the 6 hedged rows the xlsx had no gate to hold back). `effective_bucket` (C3-6)
    /// is now the ONE function every artifact calls: this pins the fix — every row's bucket
    /// label is identical across the JSON matrix and the xlsx `FindingRow`, the aggregate
    /// counts per bucket match exactly, and the hedged row never lands in an action bucket in
    /// EITHER artifact. No finding is dropped by the fix (over-tell, never under-tell) — the
    /// hedged row still ships, just consistently routed to `held` (C5-4) everywhere.
    #[test]
    fn bucket_labels_and_counts_are_identical_across_json_and_xlsx() {
        let critical = finding("SEC-CRIT-1", "src/a.rs", 1, "critical");
        let high_no_effort = finding("SEC-HIGH-1", "src/b.rs", 2, "high");
        let mut high_low_effort = finding("SEC-HIGH-2", "src/c.rs", 3, "high");
        high_low_effort.effort = Some("low".to_string());
        let medium = finding("SEC-MED-1", "src/d.rs", 4, "medium");
        let mut hedged_low = finding("SEC-HEDGED-1", "src/e.rs", 5, "low");
        hedged_low.confidence = Some("needs-review".to_string());

        let findings = vec![
            critical,
            high_no_effort,
            high_low_effort,
            medium,
            hedged_low,
        ];
        let rule_ids = vec![
            "SEC-CRIT-1",
            "SEC-HIGH-1",
            "SEC-HIGH-2",
            "SEC-MED-1",
            "SEC-HEDGED-1",
        ];
        let report = report_with(findings, rule_ids);
        let dispositions = HashMap::new();
        let opts = empty_opts();

        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            None,
            &opts,
        );
        let (rows, _dep) = partition_rows(&report, &dispositions, None, &opts.chosen_options);
        let live_rows: Vec<&FindingRow> = rows.iter().filter(|r| !r.is_fp).collect();

        assert_eq!(
            live_rows.len(),
            5,
            "no finding may be dropped from the xlsx partition"
        );

        // The JSON side's (rule_id, repo, path, line) -> bucket map, read off `matrix`.
        let mut json_bucket: HashMap<(String, String, String, usize), String> = HashMap::new();
        for f in json
            .matrix
            .do_now
            .iter()
            .chain(json.matrix.do_next.iter())
            .chain(json.matrix.plan.iter())
            .chain(json.matrix.accepted.iter())
            .chain(json.matrix.informational.iter())
            .chain(json.matrix.held.iter())
        {
            json_bucket.insert(
                (f.rule_id.clone(), f.repo.clone(), f.path.clone(), f.line),
                f.bucket.clone(),
            );
        }
        assert_eq!(
            json_bucket.len(),
            5,
            "no finding may be dropped from the JSON matrix either"
        );

        for row in &live_rows {
            let key = (
                row.rule_id.clone(),
                row.repo.clone(),
                row.path.clone(),
                row.line,
            );
            let expected = json_bucket
                .get(&key)
                .unwrap_or_else(|| panic!("row {key:?} is missing from the JSON matrix"));
            assert_eq!(
                &row.bucket, expected,
                "xlsx and JSON must compute the SAME bucket for {key:?}"
            );
        }

        // Aggregate counts must match exactly, bucket by bucket — this is the fix for the
        // three artifacts disagreeing on bucket COUNTS, not just individual labels.
        for (bucket, expected_count) in [
            ("do_now", json.matrix.do_now.len()),
            ("do_next", json.matrix.do_next.len()),
            ("plan", json.matrix.plan.len()),
            ("accepted", json.matrix.accepted.len()),
            ("informational", json.matrix.informational.len()),
            ("held", json.matrix.held.len()),
        ] {
            let actual = live_rows.iter().filter(|r| r.bucket == bucket).count();
            assert_eq!(
                actual, expected_count,
                "xlsx and JSON must agree on the {bucket} count"
            );
        }

        // Expected shape for THIS fixture: 2 do_now (critical + high/low-effort), 1 do_next
        // (high/no-effort), 1 plan (medium), 1 held (C5-4: the hedged low), 0 accepted, 0
        // informational (a calibration hedge never lands there any more).
        assert_eq!(json.matrix.do_now.len(), 2);
        assert_eq!(json.matrix.do_next.len(), 1);
        assert_eq!(json.matrix.plan.len(), 1);
        assert_eq!(json.matrix.accepted.len(), 0);
        assert_eq!(json.matrix.informational.len(), 0);
        assert_eq!(json.matrix.held.len(), 1);

        // The hedged row is NEVER in an action bucket, in EITHER artifact.
        let hedged_key = (
            "SEC-HEDGED-1".to_string(),
            "owner/repo".to_string(),
            "src/e.rs".to_string(),
            5,
        );
        assert_eq!(
            json_bucket.get(&hedged_key).map(String::as_str),
            Some("held"),
            "the hedged finding must route to held (C5-4) in the JSON matrix"
        );
        let hedged_row = live_rows
            .iter()
            .find(|r| r.rule_id == "SEC-HEDGED-1")
            .expect("the hedged row must still be present — never dropped, only re-bucketed");
        assert_eq!(hedged_row.bucket, "held");
        assert_ne!(hedged_row.bucket, "do_now");
        assert_ne!(hedged_row.bucket, "do_next");

        // The PDF template reads these SAME precomputed fields — `d.priority_grid` (built from
        // this exact `matrix` by `build_priority_grid`) and each curated site's own
        // `disposition` label — never a second, independent severity-to-bucket pass of its own.
        let template = include_str!("../templates/audit_report.typ");
        assert!(
            template.contains("d.priority_grid"),
            "the template must render the grid derived from `matrix`, not recompute buckets"
        );
        assert!(
            template.contains("site.disposition"),
            "the template must render each curated site's own precomputed disposition label"
        );
    }

    /// Within a bucket, ordering is now severity THEN provenance tier (C3-6 item 3): a
    /// deterministic/security-grounded finding ranks ahead of an ungrounded advisory one of
    /// the SAME severity, so a security-floor medium never prints after an architecture/
    /// advisory medium just because of scan/insertion order. Checked in BOTH the JSON
    /// matrix's `plan` vector and the xlsx row order — same comparator, same answer. The two
    /// findings are deliberately given paths that sort the WRONG way alphabetically (the
    /// architecture finding's path sorts first) and are inserted architecture-before-security,
    /// so a passing test can only mean the provenance-tier sort actually fired, not an
    /// accidental path or insertion-order match.
    #[tokio::test]
    async fn security_deterministic_finding_orders_before_architecture_advisory_at_the_same_severity(
    ) {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        // Not in the corpus at all -> resolves to an "advisory" citation (architecture/stance
        // observation, no grounded standard behind it).
        let architecture = finding("ARCH-STANCE-DEMO-1", "src/a-arch.rs", 9, "medium");
        // A real, grounded corpus rule (used elsewhere as the RLS class's own grounding rule)
        // -> resolves to a "grounded" citation.
        let security = finding("SUPABASE-RLS-ENABLED-1", "src/z-sec.rs", 1, "medium");
        let report = report_with(
            vec![architecture, security],
            vec!["ARCH-STANCE-DEMO-1", "SUPABASE-RLS-ENABLED-1"],
        );
        let dispositions = HashMap::new();
        let opts = empty_opts();

        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &opts,
        );
        assert_eq!(
            json.matrix.plan.len(),
            2,
            "both medium findings land in plan"
        );
        assert_eq!(
            json.matrix.plan[0].rule_id, "SUPABASE-RLS-ENABLED-1",
            "the grounded/security finding must sort before the ungrounded/architecture one"
        );
        assert_eq!(json.matrix.plan[1].rule_id, "ARCH-STANCE-DEMO-1");

        let (rows, _dep) =
            partition_rows(&report, &dispositions, Some(&corpus), &opts.chosen_options);
        let plan_rows: Vec<&FindingRow> = rows.iter().filter(|r| r.bucket == "plan").collect();
        assert_eq!(plan_rows.len(), 2);
        assert_eq!(
            plan_rows[0].rule_id, "SUPABASE-RLS-ENABLED-1",
            "the xlsx row order must match the JSON matrix's within-bucket order"
        );
        assert_eq!(plan_rows[1].rule_id, "ARCH-STANCE-DEMO-1");
    }

    // ── C4-P2: artifact consistency (hedge reconciliation, AI-tier citation/category/title,
    // detail rewrite, cross-artifact equality gate) ──────────────────────────────────────

    /// Whether `typst` is on PATH — mirrors `report_export`'s own `which_typst` gate (that
    /// helper is private to its own test module, so this is a small local copy) so the
    /// cross-artifact test below can exercise a REAL PDF compile where possible without
    /// failing in an environment that never installed the binary.
    fn typst_available() -> bool {
        std::process::Command::new("typst")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// C4-P2: an AI-tier finding with no corpus entry of its own, classified via the P3
    /// fallback (`classify_ai_finding`) into a known defect class, must carry the IDENTICAL
    /// grounded citation, a human-readable title (never the bare invented rule id), and its
    /// real semantic category (never a mangled rule-id-token key) across `findings.json`, the
    /// xlsx workbook, and the PDF's curated-findings data — and the category scorecard must
    /// count it as a checked rule. This is the general class behind the "AI-tier critical is
    /// uncited in json/xlsx while the PDF prints a different rule's citation under a mangled
    /// category with 0 rules checked" bug.
    #[tokio::test]
    async fn ai_tier_finding_citation_title_and_category_are_consistent_and_grounded() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        let mut f = finding(
            "AI-RLS-GAP-CUSTOM-7",
            "supabase/functions/orders/index.ts",
            42,
            "critical",
        );
        f.category = Some("rls-policy".to_string());
        f.detail = "Cross-tenant row level security gap: the orders table has no RLS policy, \
                     so any authenticated user can read every tenant's rows."
            .to_string();
        let report = report_with(vec![f], vec![]);
        let dispositions = HashMap::new();
        let opts = empty_opts();

        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &opts,
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let xlsx_bytes =
            build_workbook(&report.clone().seal(), &dispositions, Some(&corpus), &opts).unwrap();

        // The known grounding rule's own citation/title — the SAME real corpus entry every
        // artifact must end up citing for this finding.
        let grounding_citation =
            crate::report_export::resolve_citation("SUPABASE-RLS-ENABLED-1", None, Some(&corpus));
        assert_eq!(grounding_citation.kind, "grounded");
        let grounding_title = corpus
            .get_by_id("SUPABASE-RLS-ENABLED-1")
            .expect("fixture assumes SUPABASE-RLS-ENABLED-1 exists in the real corpus")
            .title
            .clone();

        assert_eq!(
            json.curated_findings.len(),
            1,
            "the AI-tier finding must survive the P3 citation gate into the curated set"
        );
        let group = &json.curated_findings[0];
        assert_eq!(
            group.citation.label, grounding_citation.label,
            "the group header's citation must be the P3 class's grounded citation, not advisory"
        );
        assert_ne!(
            group.title, "AI-RLS-GAP-CUSTOM-7",
            "an AI-tier group's title must never be the model's own invented rule id repeated"
        );
        assert_eq!(
            group.title, grounding_title,
            "an AI-tier group grounded via the P3 class fallback should borrow that class's own \
             human-readable corpus title"
        );
        assert_eq!(group.sites.len(), 1);
        assert_eq!(
            group.sites[0].citation.label, group.citation.label,
            "the site's OWN citation must match the group header rendered next to it — never a \
             different (category-neighbor) citation"
        );

        assert_eq!(findings_export.findings.len(), 1);
        let row = &findings_export.findings[0];
        assert_eq!(
            row.citation_label, group.citation.label,
            "findings.json citation_label must be byte-identical to the PDF's curated-site citation"
        );
        assert_eq!(
            row.category, "RLS Policy",
            "an AI-tier finding's category must come from its semantic `category` field, not a \
             mangled rule-id-derived key"
        );

        let cat_row = json
            .scorecard
            .rows
            .iter()
            .find(|r| r.category == "RLS Policy")
            .expect("the RLS Policy category must appear in the scorecard");
        assert_eq!(cat_row.critical, 1);
        assert!(
            cat_row.audited_rules >= 1,
            "a category with >= 1 finding must show >= 1 rules checked, got {}",
            cat_row.audited_rules
        );

        // The xlsx row must carry the SAME citation label and category, in the columns the
        // design doc assigns them (Category = col G, index 6; Citation = col P, index 15).
        let category_cell = cell_text(&xlsx_bytes, "xl/worksheets/sheet2.xml", "G2");
        assert_eq!(category_cell, "RLS Policy");
        let citation_cell = cell_text(&xlsx_bytes, "xl/worksheets/sheet2.xml", "P2");
        assert_eq!(citation_cell, group.citation.label);
    }

    /// C4-P2: the three "how many of these findings are hedged" signals — the executive
    /// summary's `held_for_review` count, the number of exported rows with `needs_review ==
    /// true`, and the number with `confidence == "needs-review"` — must all agree on a built
    /// report. Before this fix they could legitimately disagree: `held_for_review` (the
    /// informational-bucket count) folds in THREE signals besides calibration doubt (an
    /// `info`-severity note, a testing-style deviation with no corpus, an absence-type stance
    /// rule) that never touched the raw `needs_review`/`confidence` fields, so a row held for
    /// one of those OTHER reasons exported `needs_review: false` / `confidence: "high"` even
    /// while sitting in the bucket the narrative calls "held for a human reviewer's judgment
    /// call".
    #[test]
    fn hedge_counts_held_needs_review_and_confidence_agree_on_a_built_report() {
        // f1: an ordinary curated critical — never hedged, never informational.
        let f1 = finding("SEC-CRITICAL-1", "a.rs", 1, "critical");

        // f2: calibration itself flagged doubt (§2c) — the one signal that already touched the
        // raw fields before this fix.
        let mut f2 = finding("AI-DOUBTFUL-1", "b.rs", 2, "medium");
        f2.confidence = Some("needs-review".to_string());
        f2.needs_review = true;

        // f3: a testing-style deviation in a repo below the test-corpus threshold (§2d) — NOT
        // a confidence signal; `report_with` defaults `test_file_count` to 0.
        let mut f3 = finding("STYLE-TEST-NAMING-1", "c_test.rs", 3, "medium");
        f3.category = Some("testing-style".to_string());

        // f4: an `info`-severity note — also NOT a confidence signal.
        let f4 = finding("INFO-NOTE-1", "d.rs", 4, "info");

        let report = report_with(vec![f1, f2, f3, f4], vec![]);
        let dispositions = HashMap::new();
        let opts = empty_opts();
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            None,
            &opts,
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            None,
            &json,
            &HashMap::new(),
        );

        assert_eq!(
            json.executive_summary.held_for_review, 3,
            "f2 (calibration doubt), f3 (testing-style, no corpus) and f4 (info) must all be held"
        );
        let needs_review_count = findings_export
            .findings
            .iter()
            .filter(|r| r.needs_review)
            .count();
        let confidence_needs_review_count = findings_export
            .findings
            .iter()
            .filter(|r| r.confidence == "needs-review")
            .count();
        assert_eq!(
            needs_review_count, 3,
            "exported needs_review==true count must match held_for_review"
        );
        assert_eq!(
            confidence_needs_review_count, 3,
            "exported confidence==\"needs-review\" count must match held_for_review"
        );
        assert_eq!(json.executive_summary.held_for_review, needs_review_count);
        assert_eq!(needs_review_count, confidence_needs_review_count);

        // The reconciled biconditional: a row with confidence == "needs-review" and
        // needs_review == false (or vice versa) is impossible.
        for row in &findings_export.findings {
            assert_eq!(
                row.confidence == "needs-review",
                row.needs_review,
                "confidence/needs_review must never disagree, got confidence={:?} \
                 needs_review={} for {}",
                row.confidence,
                row.needs_review,
                row.rule_id
            );
        }

        // f3/f4 specifically: informational for a NON-confidence reason, yet must still export
        // hedged — the exact "two informational rows are confidence=high/unhedged" bug.
        let style_row = findings_export
            .findings
            .iter()
            .find(|r| r.rule_id == "STYLE-TEST-NAMING-1")
            .expect("f3 must be exported");
        assert!(
            style_row.needs_review && style_row.confidence == "needs-review",
            "a testing-style-held row must export hedged even though calibration never set its \
             raw confidence"
        );
        let info_row = findings_export
            .findings
            .iter()
            .find(|r| r.rule_id == "INFO-NOTE-1")
            .expect("f4 must be exported");
        assert!(
            info_row.needs_review && info_row.confidence == "needs-review",
            "an info-severity-held row must export hedged even though calibration never set its \
             raw confidence"
        );
    }

    /// General guard (not fixture-specific), mirroring
    /// `no_findings_export_headline_matches_the_imperative_rule_text_pattern` above for the
    /// `detail` column: across a mix of findings, no exported `detail` in `findings.json` may
    /// begin with an internal imperative gate verb. Before C4-P2, `detail` was NEVER rewritten
    /// (only `headline` was, in a prior cycle) — this pins the fix at the same generality as
    /// the existing headline test, using the task's own anchor pattern.
    #[test]
    fn no_findings_export_detail_matches_the_imperative_rule_text_pattern() {
        let corpus = ruleset_with_authored_floor_headline(
            "SEC-TEST-HEADLINE-3",
            "A vendor API token is exposed in this file.",
            "Rotate the token and move it to a secret manager.",
        );
        let mut authored = finding("SEC-TEST-HEADLINE-3", "a.py", 3, "high");
        authored.detail =
            "Require that secret values never be committed to source control.".to_string();
        // No corpus entry at all — exercises the pre-P4 fallback path, whose own `detail` (via
        // the shared `finding()` helper) is not itself imperative, same as any real AI-tier
        // finding's house-convention prose.
        let unauthored = finding("SEC-TEST-NOT-IN-CORPUS-2", "b.py", 4, "medium");

        let report = report_with(vec![authored, unauthored], vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );

        let imperative = regex::Regex::new(r"^\s*(Deny|Require|Disallow|Forbid|Enforce)\b")
            .expect("imperative-gate-verb pattern must compile");
        for row in &findings_export.findings {
            assert!(
                !imperative.is_match(&row.headline),
                "findings.json headline must never start with an internal imperative rule-text \
                 pattern, got: {:?}",
                row.headline
            );
            assert!(
                !imperative.is_match(&row.detail),
                "findings.json detail must never start with an internal imperative rule-text \
                 pattern, got: {:?}",
                row.detail
            );
        }
    }

    /// C4-P2's durable fix: the cross-artifact EQUALITY GATE. For the SAME finding, citation
    /// label, detail, severity, bucket, and hedge state must be byte-identical across
    /// `findings.json` and the xlsx workbook — and the PDF's curated-findings data (what the
    /// template actually renders) must carry those same values too, never a value the
    /// template or a renderer recomputed independently. This is the test that stops the whole
    /// "three renderers, three answers" bug family from recurring: any future change that
    /// makes one artifact drift from the other two fails HERE, not in a client's hands.
    #[tokio::test]
    async fn cross_artifact_equality_gate_citation_detail_severity_bucket_and_hedge_match() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        // f1: a plain grounded finding sharing SUPABASE-RLS-ENABLED-1, not hedged -> do_now.
        let f1 = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            1,
            "critical",
        );
        // f2: the SAME rule id (same curated-findings GROUP as f1) but calibration-hedged at
        // medium severity -> routes to "held" (C5-4) while f1 stays "do_now" — proving
        // bucket is computed PER SITE, never inherited from the group's first member.
        let mut f2 = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/2.sql",
            5,
            "medium",
        );
        f2.confidence = Some("needs-review".to_string());
        f2.needs_review = true;

        let report = report_with(vec![f1, f2], vec!["SUPABASE-RLS-ENABLED-1"]);
        let dispositions = HashMap::new();
        let opts = empty_opts();

        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &opts,
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let xlsx_bytes =
            build_workbook(&report.clone().seal(), &dispositions, Some(&corpus), &opts).unwrap();

        assert_eq!(
            json.curated_findings.len(),
            1,
            "both sites share one rule-id group"
        );
        let group = &json.curated_findings[0];
        assert_eq!(group.sites.len(), 2);
        assert_eq!(findings_export.findings.len(), 2);

        for site in &group.sites {
            let row = findings_export
                .findings
                .iter()
                .find(|r| r.path == site.path && r.line == site.line)
                .unwrap_or_else(|| panic!("findings.json must carry a row for {}", site.path));

            assert_eq!(
                row.citation_label, site.citation.label,
                "citation must be byte-identical between findings.json and the PDF's curated \
                 site, for {}",
                site.path
            );
            assert_eq!(
                row.detail, site.detail,
                "detail must be byte-identical between findings.json and the PDF's curated \
                 site, for {}",
                site.path
            );
            assert_eq!(
                row.severity, site.severity,
                "severity must be byte-identical between findings.json and the PDF's curated \
                 site, for {}",
                site.path
            );
            assert_eq!(
                row.bucket, site.bucket,
                "bucket must be byte-identical between findings.json and the PDF's curated \
                 site, for {}",
                site.path
            );
            let row_hedged = row.needs_review;
            let site_hedged = site.confidence.as_deref() == Some("needs-review");
            assert_eq!(
                row_hedged, site_hedged,
                "hedge state must be byte-identical between findings.json and the PDF's curated \
                 site, for {}",
                site.path
            );

            // Now the ACTUAL xlsx bytes, not just the Rust FindingRow value — the "All
            // Findings" sheet is sheet2.xml; rows sort severity-desc so f1 (critical) is row 2
            // and f2 (medium) is row 3.
            let xlsx_row = if site.severity == "critical" { 2 } else { 3 };
            let citation_cell = cell_text(
                &xlsx_bytes,
                "xl/worksheets/sheet2.xml",
                &format!("P{xlsx_row}"),
            );
            assert_eq!(
                citation_cell, site.citation.label,
                "xlsx Citation cell must be byte-identical to the PDF's curated-site citation"
            );
            let detail_cell = cell_text(
                &xlsx_bytes,
                "xl/worksheets/sheet2.xml",
                &format!("U{xlsx_row}"),
            );
            assert_eq!(
                detail_cell, site.detail,
                "xlsx Detail cell must be byte-identical to the PDF's curated-site detail"
            );
            let needs_review_cell = cell_text(
                &xlsx_bytes,
                "xl/worksheets/sheet2.xml",
                &format!("M{xlsx_row}"),
            );
            assert_eq!(
                needs_review_cell == "Yes",
                site_hedged,
                "xlsx Needs review cell must be byte-identical to the PDF's curated-site hedge \
                 state"
            );
        }

        // The group header's citation (what the template actually renders next to the FIRST
        // site) must be that site's own citation — never a different member's.
        assert_eq!(group.citation.label, group.sites[0].citation.label);

        // Real end-to-end check where the toolchain allows it: the template must compile
        // against this exact two-site, mixed-bucket, mixed-hedge data shape — proving the PDF
        // reads these same serialized fields rather than recomputing anything of its own.
        if typst_available() {
            let pdf = crate::report_export::compile_pdf(&json)
                .await
                .expect("compile_pdf must succeed for a mixed-bucket, mixed-hedge group");
            assert!(pdf.starts_with(b"%PDF"));
        } else {
            eprintln!(
                "skipping the compile_pdf leg of cross_artifact_equality_gate_...: typst not on \
                 PATH"
            );
        }
    }

    // ── C4-P4 residual defect 2: no raw rule id in client-facing "also matches" prose ──────

    /// `findings.json`'s `also_matches` field (and the xlsx "Also matches" column it mirrors
    /// verbatim) used to be `f.also_matches.join(", ")` — the raw internal rule ids, exactly
    /// the kind of token a client has no way to decode. Both must now render the OTHER rule's
    /// human-readable TITLE instead, never the bare id.
    #[tokio::test]
    async fn also_matches_column_and_findings_json_render_titles_never_raw_rule_ids() {
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

        let other_rule_id = "SEC-NO-HARDCODED-SECRETS-1";
        let other_title = corpus
            .get_by_id(other_rule_id)
            .expect("fixture assumes this rule exists in the real corpus")
            .title
            .clone();

        let mut f = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        f.also_matches = vec![other_rule_id.to_string()];

        let report = report_with(vec![f], vec![]);
        let dispositions = HashMap::new();
        let opts = empty_opts();
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &opts,
        );
        let findings_export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            Some(&corpus),
            &json,
            &HashMap::new(),
        );

        assert_eq!(findings_export.findings.len(), 1);
        let row = &findings_export.findings[0];
        assert!(
            row.also_matches.contains(&other_title),
            "findings.json also_matches must render the rule TITLE, got: {:?}",
            row.also_matches
        );
        assert!(
            !row.also_matches.contains(other_rule_id),
            "findings.json also_matches must never contain the raw rule id, got: {:?}",
            row.also_matches
        );

        let xlsx_bytes =
            build_workbook(&report.clone().seal(), &dispositions, Some(&corpus), &opts).unwrap();
        let also_matches_cell = cell_text(&xlsx_bytes, "xl/worksheets/sheet2.xml", "R2");
        assert_eq!(
            also_matches_cell, row.also_matches,
            "the xlsx Also matches column must be byte-identical to findings.json"
        );
        assert!(
            !also_matches_cell.contains(other_rule_id),
            "the xlsx Also matches column must never contain the raw rule id, got: {also_matches_cell:?}"
        );

        // Also exercise the PDF's own curated-site field directly: the group header is on the
        // FIRST (and only) site here.
        assert_eq!(json.curated_findings.len(), 1);
        let site = &json.curated_findings[0].sites[0];
        assert!(site.also_matches_titles.contains(&other_title));
        assert!(!site.also_matches_titles.iter().any(|t| t == other_rule_id));
    }

    /// An `also_matches` id with no resolvable corpus title must be DROPPED, never fall back to
    /// the bare id (that fallback is fine for a finding's own PRIMARY title — see
    /// `title_for_finding` — but would reintroduce the raw-id leak here).
    #[test]
    fn also_matches_with_no_corpus_entry_is_dropped_not_shown_as_a_raw_id() {
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        f.also_matches = vec!["SOME-UNKNOWN-RULE-ID-1".to_string()];
        let titles = crate::report_export::also_matches_titles(&f.also_matches, None);
        assert!(
            titles.is_empty(),
            "an unresolvable also_matches id must be dropped, not shown raw: {titles:?}"
        );
    }

    /// req_c5: a merged row's absorbed evidence sites (`Finding::also_locations`) reach
    /// `findings.json` verbatim, together with an explicit `site_count` — before W7 neither
    /// field was exported anywhere, so a row representing three detected sites was
    /// observationally identical to a single-site row (tracker C5: an over-merge was
    /// indistinguishable from a detection miss in every client artifact).
    #[test]
    fn req_c5_absorbed_sites_are_observable_in_findings_json() {
        let mut f = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/orders.rs", 10, "high");
        f.also_locations = vec![
            crate::onboard::MergedLocation {
                repo: "owner/repo".to_string(),
                path: "src/orders.rs".to_string(),
                line: 24,
                rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
                snippet: "let q = format!(\"SELECT ... {}\", id);".to_string(),
                consequence: false,
            },
            crate::onboard::MergedLocation {
                repo: "owner/repo".to_string(),
                path: "src/orders.rs".to_string(),
                line: 41,
                rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
                snippet: "let q = \"DELETE ...\".to_string() + &id;".to_string(),
                consequence: true,
            },
        ];
        let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        let row = serde_json::to_value(&export.findings[0]).unwrap();
        assert_eq!(
            row["site_count"], 3,
            "one own site + two absorbed sites must read as 3, got: {row}"
        );
        let locs = row["also_locations"]
            .as_array()
            .expect("findings.json rows must carry an also_locations array");
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0]["path"], "src/orders.rs");
        assert_eq!(locs[0]["line"], 24);
        assert_eq!(locs[1]["line"], 41);
        assert_eq!(locs[1]["consequence"], true);

        // The workbook renders the same sites in the "Also found at" column (path:line,
        // client-facing — no raw rule ids), with the consequence site labeled.
        let xlsx =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let shared = read_zip_entry(&xlsx, "xl/sharedStrings.xml");
        assert!(
            shared.contains("src/orders.rs:24"),
            "the workbook must name the absorbed site src/orders.rs:24"
        );
        assert!(
            shared.contains("src/orders.rs:41 (also affected)"),
            "the workbook must label the consequence site"
        );

        // And the PDF's own data (the report JSON) carries the same multiplicity on the
        // curated site, so no artifact under-reports the site count relative to another.
        let site = &json.curated_findings[0].sites[0];
        assert_eq!(site.site_count, 3);
        assert_eq!(site.also_locations.len(), 2);
    }

    /// req_c6: a rule that FIRED and was merged away as a group member must never render
    /// "0 / clean" on the Coverage sheet. Before W7, `build_coverage_rows` counted PRIMARY
    /// rows only, so a rule whose every row was absorbed into another rule's primary (its id
    /// demoted to `also_matches`, its site preserved in `also_locations`) read as green,
    /// zero-findings coverage — a rule that found something and lost it rendered exactly
    /// like a rule that found nothing.
    #[test]
    fn req_c6_a_rule_merged_away_as_a_member_never_renders_clean_on_the_coverage_sheet() {
        let mut primary = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/orders.rs", 10, "high");
        primary.also_matches = vec!["SEC-NO-HARDCODED-SECRETS-1".to_string()];
        primary.also_locations = vec![crate::onboard::MergedLocation {
            repo: "owner/repo".to_string(),
            path: "src/orders.rs".to_string(),
            line: 12,
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            snippet: "let key = \"sk_live_...\";".to_string(),
            consequence: false,
        }];
        let report = report_with(
            vec![primary],
            vec!["SEC-NO-RAW-SQL-CONCAT-1", "SEC-NO-HARDCODED-SECRETS-1"],
        );
        let (rows, _deps) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        let coverage = build_coverage_rows(&report, &rows, None);
        let absorbed_rule = coverage
            .iter()
            .find(|c| c.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
            .expect("the absorbed rule must still have a coverage row");
        assert!(
            absorbed_rule.findings_count > 0,
            "a rule that fired and was absorbed by a merge must never read 0/clean — the C6 \
             shape: {}",
            absorbed_rule.findings_count
        );
        // The surviving primary's own count covers its own site plus its absorbed same-rule
        // sites (none here), unchanged.
        let primary_rule = coverage
            .iter()
            .find(|c| c.rule_id == "SEC-NO-RAW-SQL-CONCAT-1")
            .unwrap();
        assert_eq!(primary_rule.findings_count, 1);
    }

    /// req_c6: a same-location demotion (an `also_matches` entry with NO site record of its
    /// own) still counts — the weaker of the two absorption shapes must also never render
    /// clean.
    #[test]
    fn req_c6_a_same_location_demotion_also_never_renders_clean() {
        let mut primary = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/orders.rs", 10, "high");
        primary.also_matches = vec!["ARCH-NO-DIRECT-DB-1".to_string()];
        let report = report_with(
            vec![primary],
            vec!["SEC-NO-RAW-SQL-CONCAT-1", "ARCH-NO-DIRECT-DB-1"],
        );
        let (rows, _deps) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        let coverage = build_coverage_rows(&report, &rows, None);
        let demoted = coverage
            .iter()
            .find(|c| c.rule_id == "ARCH-NO-DIRECT-DB-1")
            .unwrap();
        assert!(
            demoted.findings_count > 0,
            "a same-location demotion must still count as the rule having fired"
        );
    }

    /// req_g1/req_g2 (W7): the workbook's Coverage sheet discloses non-dependency tool
    /// coverage gaps (one concise client-facing line per tool — G3) and names every rule the
    /// ledger confirms did not run. Before W7 the workbook carried neither: only osv-scanner
    /// notes reached it (Dependencies sheet), and ledger-excluded rules reached the report
    /// JSON only.
    #[test]
    fn req_g1_g2_coverage_sheet_discloses_tool_gaps_and_rules_not_run() {
        let f = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 3, "high");
        let mut report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
        report.coverage_notes = vec![crate::onboard::CoverageNote {
            tool: "eslint".to_string(),
            message: "Could not preview 12 rule(s) with eslint: spawn ENOENT.".to_string(),
        }];
        report.ledger.record_rule(
            "JS-SOME-NEVER-RUN-RULE-1",
            crate::scan_ledger::RuleTier::ExternalTool,
            false,
            Some("preview tool failed to start".to_string()),
            0,
            0,
        );
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let shared = read_zip_entry(&bytes, "xl/sharedStrings.xml");
        assert!(
            shared.contains("Tool coverage notes"),
            "the Coverage sheet must carry the tool-coverage section"
        );
        assert!(
            shared.contains("The eslint static-analysis checks did not fully run this scan"),
            "the disclosure must name the tool in client language"
        );
        assert!(
            !shared.contains("ENOENT"),
            "the raw internal diagnostic must not reach the workbook (G3)"
        );
        assert!(
            shared.contains("Rules not run this scan")
                && shared.contains("JS-SOME-NEVER-RUN-RULE-1"),
            "a ledger-excluded rule must be named on the Coverage sheet"
        );
    }

    /// req_c5: an un-merged row reads `site_count == 1` with an empty `also_locations` —
    /// the C5 export must never inflate a single-site row into a multi-site claim.
    #[test]
    fn req_c5_unmerged_row_site_count_is_exactly_one() {
        let f = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/orders.rs", 10, "high");
        let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        let row = serde_json::to_value(&export.findings[0]).unwrap();
        assert_eq!(row["site_count"], 1);
        assert_eq!(row["also_locations"].as_array().unwrap().len(), 0);
    }

    // ── Pass 3 (CLIENT DELIVERABLES): the workbook is partitioned by KIND ─────────────────
    //
    // Zach: "Excel = the meat and potatoes. Needs rethinking: separate worksheets for
    // findings, conventions, deviations, coverage. This is where the specifics live."
    //
    // Every test below asserts on the WORKBOOK BYTES, per sheet, not on a Rust row vector —
    // a sheet writer that early-returns, or a row routed to no sheet at all, must fail here
    // and not in a client's hands.

    /// A fixture carrying ALL FIVE kinds plus a legacy (kind-less) row — the only shape that
    /// can exercise the partition. Headlines are distinct per rule so a per-row presence
    /// check is meaningful.
    fn all_five_kinds() -> Vec<Finding> {
        use camerata_api_types::audit::{
            ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind, NotAssessedKind,
            NotAssessedReason, ObservedOptionUsage,
        };
        vec![
            kinded(
                finding("SEC-INJECTION-1", "src/orders.rs", 10, "critical"),
                FindingKind::Defect,
            ),
            kinded(
                finding("ARCH-API-VERSIONING-1", "src/api/users.rs", 4, "medium"),
                FindingKind::Deviation(DeviationKind {
                    deviating_sites: 2,
                    assessed_sites: Some(47),
                    convention_option_id: Some("versioned".to_string()),
                    convention_label: "URI-versioned endpoints".to_string(),
                }),
            ),
            kinded(
                finding("ARCH-PAGINATION-1", "src/api/list.rs", 7, "info"),
                FindingKind::ConventionObserved(ConventionObservedKind {
                    convention_option_id: "cursor".to_string(),
                    convention_label: "Cursor pagination".to_string(),
                }),
            ),
            kinded(
                finding("ARCH-ERROR-SHAPE-1", "src/api/errors.rs", 1, "info"),
                FindingKind::NoConvention(NoConventionKind {
                    observed_options: vec![
                        ObservedOptionUsage {
                            option_id: "rfc7807".to_string(),
                            label: "RFC 7807 problem+json".to_string(),
                            sites: Some(3),
                        },
                        ObservedOptionUsage {
                            option_id: "ad-hoc".to_string(),
                            label: "Ad-hoc error bodies".to_string(),
                            sites: Some(1),
                        },
                    ],
                }),
            ),
            kinded(
                finding("JS-NO-EVAL-1", "src/legacy.js", 2, "info"),
                FindingKind::NotAssessed(NotAssessedKind {
                    reason: NotAssessedReason::ToolFailed,
                    reason_detail: "eslint exited 2: spawn ENOENT in the scanned repo".to_string(),
                }),
            ),
            // Producer predates the taxonomy / was missed by the backfill.
            finding("LEGACY-UNMAPPED-1", "src/old.rs", 9, "high"),
        ]
    }

    /// req_pass3: EVERY row `findings.json` exports lands on exactly ONE of the three kind
    /// destinations — All Findings, Conventions, or Coverage's "could not complete" section.
    ///
    /// The restructure's entire risk lives here. Before Pass 3 every non-FP row went to one
    /// sheet, so conservation was trivially true; routing five kinds to three destinations
    /// makes both "lands nowhere" and "lands twice" reachable, and either is a PARITY FAULT
    /// by name (the grader's own verdict, and the exact blindness behind the hearth 0/12
    /// result) rather than a cosmetic slip. Keyed on each row's HEADLINE, which is row-specific
    /// prose — the same identity the self-measurement parity harness uses.
    ///
    /// FAILS BEFORE THE CHANGE: there was no Conventions sheet and no Coverage not-assessed
    /// section, so `sheet_entry_for("Conventions")` had no sheet to resolve. Re-verified
    /// against the flattened behaviour by routing every kind to `KindSheet::Findings`: the
    /// convention/not-assessed rows then land on All Findings as well, and the "exactly one
    /// destination" count reads 1 for rows that should be on Conventions — caught as
    /// `found on 0 of the three` for the Conventions rows under the empty-sheet note.
    #[test]
    fn req_pass3_every_exported_row_lands_on_exactly_one_sheet() {
        let mut rows = all_five_kinds();
        let fp = finding("SEC-FP-1", "src/fixture.rs", 3, "high");
        let mut dispositions = HashMap::new();
        dispositions.insert(finding_key(&fp), wire("FalsePositive", "test fixture code"));
        rows.push(fp);

        let report = report_with(rows, vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &dispositions,
            None,
            &empty_opts(),
        );
        let export = build_findings_export(
            &report.clone().seal(),
            &dispositions,
            None,
            &json,
            &HashMap::new(),
        );
        let bytes =
            build_workbook(&report.clone().seal(), &dispositions, None, &empty_opts()).unwrap();

        let headlines: Vec<String> = export.findings.iter().map(|r| r.headline.clone()).collect();
        assert_eq!(
            headlines.len(),
            6,
            "the FP is excluded; all six kinded rows must still export"
        );
        let distinct: HashSet<&String> = headlines.iter().collect();
        assert_eq!(
            distinct.len(),
            headlines.len(),
            "the fixture's headlines must be distinct or a per-row presence check is vacuous: \
             {headlines:#?}"
        );

        let destinations = ["All Findings", "Conventions", "Coverage"];
        let cells: Vec<Vec<String>> = destinations
            .iter()
            .map(|s| sheet_cell_strings(&bytes, s))
            .collect();
        for headline in &headlines {
            let hits: Vec<&str> = destinations
                .iter()
                .zip(cells.iter())
                .filter(|(_, c)| c.iter().any(|s| s == headline))
                .map(|(name, _)| *name)
                .collect();
            assert_eq!(
                hits.len(),
                1,
                "row {headline:?} must appear on EXACTLY ONE of {destinations:?}, found on \
                 {hits:?}. Zero means the row reached no artifact surface (a parity fault); \
                 two means the kind partition double-counts it."
            );
        }

        // And the partition's arithmetic, independently: the two row-shaped sheets plus the
        // not-assessed section must account for every exported row, with nothing left over.
        let findings_sheet = sheet_data_row_count(&bytes, "All Findings");
        let conventions_sheet = sheet_data_row_count(&bytes, "Conventions");
        assert_eq!(findings_sheet, 3, "defect + deviation + unmapped");
        assert_eq!(conventions_sheet, 2, "convention observed + no convention");
        assert_eq!(
            findings_sheet + conventions_sheet + 1,
            headlines.len(),
            "the one not-assessed row is the remainder; if this drifts, a kind is being \
             dropped or duplicated"
        );
    }

    /// req_pass3: the Deviations sheet states its NUMERATOR and DENOMINATOR as numbers in
    /// their own columns, beside the convention label — so "2 of 47 sites deviate from
    /// URI-versioned endpoints" is readable off three adjacent cells with no prose parsing.
    ///
    /// FAILS BEFORE THE CHANGE: there was no Deviations sheet. The structured numbers reached
    /// `findings.json` (W3) and the workbook carried neither them nor the convention label in
    /// any column — a reader had to find the two numbers inside the `Detail` sentence, which
    /// is exactly the prose-parsing the structured payload exists to abolish. Re-verified by
    /// reverting the two count columns to a single rendered sentence: `cell_number` then
    /// fails its own "this cell is a number, not a shared string" assertion.
    #[test]
    fn req_pass3_deviation_sheet_states_numerator_and_denominator() {
        let report = report_with(all_five_kinds(), vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();

        let entry = sheet_entry_for(&bytes, "Deviations");
        assert_eq!(
            cell_text(&bytes, &entry, "C2"),
            "URI-versioned endpoints",
            "the convention the codebase follows is its own column, verbatim from the \
             authored corpus option"
        );
        assert_eq!(
            cell_number(&bytes, "Deviations", "D2"),
            2.0,
            "numerator: sites deviating"
        );
        assert_eq!(
            cell_number(&bytes, "Deviations", "E2"),
            47.0,
            "denominator: sites assessed"
        );

        let headers = sheet_cell_strings(&bytes, "Deviations");
        for h in [
            "Convention your codebase follows",
            "Sites deviating",
            "Sites assessed",
        ] {
            assert!(
                headers.iter().any(|s| s == h),
                "the Deviations sheet must carry the {h:?} column: {headers:#?}"
            );
        }

        // One row per RULE, not per site: the payload's counts are group-level, so a per-site
        // sheet would repeat 2 and 47 down N rows and still leave the grouping implicit.
        assert_eq!(
            sheet_data_row_count(&bytes, "Deviations"),
            1,
            "the fixture has one deviating rule, so the sheet has one row"
        );
    }

    /// req_pass3: a deviation whose producer could not count the denominator renders an
    /// explicit "not counted" — never 0, and never the numerator repeated.
    ///
    /// `assessed_sites: None` exists to mean "the producer could not count honestly"
    /// ([`camerata_api_types::audit::DeviationKind`]); writing 0 there would fabricate the one
    /// number it refuses to claim, and "2 of 0" is worse than no denominator at all.
    ///
    /// FAILS BEFORE THE CHANGE: no sheet rendered a denominator in any form.
    #[test]
    fn req_pass3_deviation_without_a_denominator_says_not_counted_never_zero() {
        use camerata_api_types::audit::{DeviationKind, FindingKind};
        let dev = kinded(
            finding("ARCH-API-VERSIONING-1", "src/api/users.rs", 4, "medium"),
            FindingKind::Deviation(DeviationKind {
                deviating_sites: 2,
                assessed_sites: None,
                convention_option_id: None,
                convention_label: "the pattern used elsewhere in this codebase".to_string(),
            }),
        );
        let report = report_with(vec![dev], vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();

        let entry = sheet_entry_for(&bytes, "Deviations");
        assert_eq!(
            cell_text(&bytes, &entry, "E2"),
            "not counted",
            "an uncountable denominator must say so in words"
        );
        assert_eq!(
            cell_number(&bytes, "Deviations", "D2"),
            2.0,
            "the numerator is still a real number"
        );
    }

    /// req_pass3 (the C6 discipline, extended to the new sheet): a deviation group that is ON
    /// this sheet because sites deviate can never render "0 deviating".
    ///
    /// C6 was a real shipped bug of exactly this shape — a rule that found something and lost
    /// it in a merge rendered green. The same hole exists for a producer that fills the
    /// payload's `deviating_sites` with 0 (or never fills it) while the row set plainly holds
    /// deviating sites, so the numerator is floored at the sites the workbook actually lists.
    ///
    /// FAILS BEFORE THE CHANGE: taking the payload's number verbatim renders 0, and the
    /// sheet tells a client there are no deviating sites on a row that exists because there
    /// are. Verified red by removing the `.max(sites_represented)` floor in
    /// `build_deviation_groups`.
    #[test]
    fn req_pass3_deviation_group_can_never_render_zero_deviating_sites() {
        use camerata_api_types::audit::{DeviationKind, FindingKind};
        let dev = kinded(
            finding("ARCH-API-VERSIONING-1", "src/api/users.rs", 4, "medium"),
            FindingKind::Deviation(DeviationKind {
                // The producer never counted — the structural numerator is absent/zero.
                deviating_sites: 0,
                assessed_sites: Some(47),
                convention_option_id: None,
                convention_label: "URI-versioned endpoints".to_string(),
            }),
        );
        let report = report_with(vec![dev], vec![]);
        let (rows, _dep) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        let live: Vec<&FindingRow> = rows.iter().filter(|r| !r.is_fp).collect();
        let groups = build_deviation_groups(&live, None);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].deviating_sites, 1,
            "a group with one listed deviating site must never report 0"
        );

        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        assert_eq!(cell_number(&bytes, "Deviations", "D2"), 1.0);
    }

    /// req_pass3: the Conventions sheet reports the two convention statements DISTINCTLY and
    /// in client language — "followed consistently" with the convention named, and "no single
    /// convention adopted" with the patterns actually in use and their site counts — and
    /// neither statement appears among the findings.
    ///
    /// This sheet is a shipped feature, not filler (A5.5): the audit derives the client's
    /// conventions and these become their `AGENTS.md`. A client must be able to read it and
    /// see what their codebase already does well and consistently.
    ///
    /// FAILS BEFORE THE CHANGE: these rows sat on the one findings sheet, interleaved by
    /// severity with real defects, with the raw wire tag `convention_observed` in a `Kind`
    /// cell as the only indication of what they were, and the convention label reachable
    /// nowhere in the workbook at all (it lived only in `findings.json`'s payload).
    #[test]
    fn req_pass3_conventions_sheet_reports_followed_and_no_convention_distinctly() {
        let report = report_with(all_five_kinds(), vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();

        let conventions = sheet_cell_strings(&bytes, "Conventions");
        let has = |needle: &str| conventions.iter().any(|s| s == needle);
        assert!(
            has("Followed consistently — no exceptions found"),
            "the zero-violation statement must read as plain language: {conventions:#?}"
        );
        assert!(
            has("Cursor pagination"),
            "the convention the codebase follows must be named: {conventions:#?}"
        );
        assert!(
            has("No single convention adopted"),
            "the no-convention statement must be distinct from the followed one"
        );
        // The observed split is ONE cell (one line per option), so this is a containment
        // check on that cell rather than a whole-cell equality like the others.
        assert!(
            conventions
                .iter()
                .any(|s| s.contains("RFC 7807 problem+json (3 sites)")
                    && s.contains("Ad-hoc error bodies (1 site)")),
            "the observed split must be enumerated with per-option site counts, one option \
             per line in the 'Patterns in use' cell: {conventions:#?}"
        );
        assert!(
            !conventions
                .iter()
                .any(|s| s.contains("convention_observed") || s.contains("no_convention")),
            "no raw wire tag may reach a client-facing cell: {conventions:#?}"
        );

        // And they are NOT among the findings: a client scanning "what is wrong" must not
        // read "you consistently use cursor pagination" in the same list as an injection.
        let findings = sheet_cell_strings(&bytes, "All Findings");
        assert!(
            !findings.iter().any(|s| s == "Cursor pagination"),
            "a convention observation must not render on the findings sheet"
        );
    }

    /// req_pass3: a `NotAssessed` row renders on COVERAGE, with its reason in client
    /// language — and never among the findings, and never carrying the producer's own
    /// diagnostic.
    ///
    /// "Armed but not evaluable" is honesty about coverage, not an accusation: giving it equal
    /// billing with a defect both overstates it and buries the defects. It is still an
    /// exported row, so the section renders the row's own headline verbatim, which is what
    /// keeps artifact parity true across the partition.
    ///
    /// G3 is the other half: the payload's `reason_detail` is the producer's verbatim
    /// diagnostic ("eslint exited 2: spawn ENOENT…") and must not reach the client — the same
    /// discipline W7 applied to the tool-coverage disclosures.
    ///
    /// FAILS BEFORE THE CHANGE: the row sat on the findings sheet with the tag
    /// `not_assessed`, and the Coverage sheet had no section for it.
    #[test]
    fn req_pass3_not_assessed_renders_on_coverage_not_among_the_findings() {
        let report = report_with(all_five_kinds(), vec![]);
        let json = crate::report_export::build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &empty_opts(),
        );
        let export = build_findings_export(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &json,
            &HashMap::new(),
        );
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();

        let na_headline = export
            .findings
            .iter()
            .find(|r| r.rule_id == "JS-NO-EVAL-1")
            .map(|r| r.headline.clone())
            .expect("the not-assessed row must still be exported");

        let coverage = sheet_cell_strings(&bytes, "Coverage");
        assert!(
            coverage
                .iter()
                .any(|s| s == "Checks we could not complete this scan"),
            "Coverage must carry the not-assessed section: {coverage:#?}"
        );
        assert!(
            coverage
                .iter()
                .any(|s| s == "An external analysis tool it depends on could not run"),
            "the structured reason must render in client language, not as `tool_failed`: \
             {coverage:#?}"
        );
        assert!(
            coverage.contains(&na_headline),
            "the row's own headline must render verbatim or artifact parity breaks: \
             {na_headline:?} not in {coverage:#?}"
        );
        assert!(
            !coverage
                .iter()
                .any(|s| s.contains("not_assessed") || s.contains("tool_failed")),
            "no raw wire tag in a client-facing cell: {coverage:#?}"
        );

        let findings = sheet_cell_strings(&bytes, "All Findings");
        assert!(
            !findings.contains(&na_headline),
            "a not-assessed row must not take equal billing with the defects"
        );

        // The producer's own diagnostic reaches `findings.json` (machine-readable) and NO
        // cell of the workbook.
        let shared = read_zip_entry(&bytes, "xl/sharedStrings.xml");
        assert!(
            !shared.contains("ENOENT") && !shared.contains("eslint exited 2"),
            "the producer's raw diagnostic must not reach the client-facing workbook (G3)"
        );
        let na_json = serde_json::to_value(
            export
                .findings
                .iter()
                .find(|r| r.rule_id == "JS-NO-EVAL-1")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(na_json["reason"], "tool_failed");
        assert!(
            na_json["reason_detail"]
                .as_str()
                .unwrap_or_default()
                .contains("ENOENT"),
            "the diagnostic is not lost — it stays in the machine-readable export: {na_json}"
        );
    }

    /// req_pass3 / req_g3: Coverage opens with ONE concise, client-facing summary line, and
    /// its "how we checked" cells speak the Index legend's vocabulary rather than our own
    /// internal citation-kind tokens.
    ///
    /// Zach's G3 ruling: "coverage IS client-facing, but concise and useful, not exhaustive.
    /// A one-line summary, written for a client, never an internal diagnostic dump."
    ///
    /// FAILS BEFORE THE CHANGE, two ways, both verified: (1) the sheet opened directly on a
    /// header row with no summary of any kind — `We checked …` appeared nowhere; (2) the
    /// `Citation Kind` column wrote `row.citation_kind` verbatim, so `grounded` — a token from
    /// our own taxonomy that appears in no legend and in no other cell of this workbook — was
    /// rendered straight into a client-facing cell.
    #[tokio::test]
    async fn req_pass3_coverage_opens_with_a_concise_client_summary_and_no_internal_taxonomy() {
        // The REAL corpus, because "no internal taxonomy leaks" is only a meaningful claim
        // over rules that actually resolve a grounded citation — the leak was the raw
        // `grounded` token, and a corpus-less fixture can never produce one.
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let f = finding(
            "SUPABASE-RLS-ENABLED-1",
            "supabase/migrations/1.sql",
            3,
            "critical",
        );
        let report = report_with(
            vec![f],
            vec!["SUPABASE-RLS-ENABLED-1", "SEC-NO-HARDCODED-SECRETS-1"],
        );
        let bytes = build_workbook(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        )
        .unwrap();

        let coverage = sheet_cell_strings(&bytes, "Coverage");
        let summary = coverage
            .iter()
            .find(|s| s.starts_with("We checked "))
            .unwrap_or_else(|| {
                panic!("Coverage must open with a one-line client summary: {coverage:#?}")
            });
        assert!(
            summary.contains("2 rules") && summary.contains("found clean"),
            "the summary must actually say what was checked and what came back clean: \
             {summary:?}"
        );
        assert_eq!(
            coverage
                .iter()
                .filter(|s| s.starts_with("We checked "))
                .count(),
            1,
            "ONE line, not a paragraph per rule — G3's whole point"
        );

        // A result cell reads as a statement, not a bare integer a client has to interpret.
        assert!(
            coverage.iter().any(|s| s == "Clean — nothing found"),
            "a clean rule must say so in words: {coverage:#?}"
        );
        assert!(
            coverage.iter().any(|s| s == "1 site found"),
            "a rule that fired must say how many sites: {coverage:#?}"
        );

        // No internal taxonomy: `grounded`/`advisory` are our citation kinds, not a client's
        // vocabulary, and the Index legend defines these words instead.
        assert!(
            coverage.iter().any(|s| s == "Deterministic"),
            "how-we-checked must use the legend's own vocabulary: {coverage:#?}"
        );
        for leak in ["grounded", "advisory", "Citation Kind"] {
            assert!(
                !coverage.iter().any(|s| s == leak),
                "{leak:?} is internal vocabulary and must not reach a Coverage cell: \
                 {coverage:#?}"
            );
        }
    }

    /// req_pass3 / req_g3: the Coverage summary reads like a sentence a human wrote, in every
    /// case a client can actually hit — including the two that a single format template gets
    /// wrong and that a healthy codebase sees most often.
    ///
    /// Found by RENDERING the sheet and reading it, not by a failing assertion: a template
    /// produced "0 found something to look at; the other 3 were checked and found clean",
    /// which is the exact register G3's ruling rules out ("a one-line summary, WRITTEN FOR A
    /// CLIENT"). Pinned here so the branches cannot silently collapse back into one template.
    ///
    /// FAILS BEFORE THE CHANGE: `coverage_summary_line` did not exist — the Coverage sheet
    /// had no summary of any kind.
    #[test]
    fn req_pass3_coverage_summary_reads_as_a_sentence_in_every_degenerate_case() {
        let row = |findings_count: usize| CoverageRow {
            rule_id: "R-1".to_string(),
            title: "t".to_string(),
            category: "c".to_string(),
            citation_kind: "grounded".to_string(),
            citation_label: "l".to_string(),
            findings_count,
        };

        // Nothing checked at all: never a sentence with a 0 in it pretending to be a result.
        assert_eq!(
            coverage_summary_line(&[], 0),
            "No rules were recorded as checked for this inspection."
        );

        // The healthy case: no "0 found something to look at".
        let all_clean = coverage_summary_line(&[row(0), row(0), row(0)], 0);
        assert_eq!(
            all_clean,
            "We checked 3 rules against this codebase; every one came back clean."
        );
        assert!(
            !all_clean.contains('0'),
            "no bare zero in the clean case: {all_clean:?}"
        );

        // Everything fired: no "the other 0 were checked and found clean".
        let none_clean = coverage_summary_line(&[row(2), row(1)], 0);
        assert_eq!(
            none_clean,
            "We checked 2 rules against this codebase, and every one found something to look at."
        );

        // Singulars agree, and the not-assessed clause is appended rather than replacing the
        // summary — a rule that could not be evaluated is extra honesty, not a substitute for
        // saying what WAS checked.
        let one = coverage_summary_line(&[row(1)], 1);
        assert!(
            one.starts_with("We checked 1 rule against this codebase")
                && one.contains("A further 1 could not be evaluated this scan and is named below"),
            "{one:?}"
        );
        let mixed = coverage_summary_line(&[row(1), row(0), row(0)], 2);
        assert_eq!(
            mixed,
            "We checked 3 rules against this codebase. 1 found something to look at; the other \
             2 were checked and found clean. A further 2 could not be evaluated this scan and \
             are named below, with the reason."
        );
    }

    /// req_pass3 (C5 across the restructure): absorbed merge sites stay visible on the NEW
    /// sheets too.
    ///
    /// C5 was a shipped defect — an absorbed site reached no artifact, so an over-merge was
    /// observationally identical to a detection miss. W7 fixed it for the findings sheet with
    /// the "Sites" and "Also found at" columns. Moving rows to new sheets re-opens exactly
    /// that hole for every row that moved, so the Deviations sheet's "Where" column and the
    /// Conventions sheet's "Sites"/"Also found at" columns are asserted here directly.
    ///
    /// FAILS BEFORE THE CHANGE: neither sheet existed, so an absorbed site under a deviation
    /// or a convention row had no column on any sheet that could show it.
    #[test]
    fn req_pass3_absorbed_merge_sites_stay_visible_on_the_new_sheets() {
        use camerata_api_types::audit::{ConventionObservedKind, DeviationKind, FindingKind};
        let loc = |line: usize, consequence: bool| crate::onboard::MergedLocation {
            repo: "owner/repo".to_string(),
            path: "src/api/users.rs".to_string(),
            line,
            rule_id: "ARCH-API-VERSIONING-1".to_string(),
            snippet: "router.get(\"/users\", …)".to_string(),
            consequence,
        };
        let mut dev = kinded(
            finding("ARCH-API-VERSIONING-1", "src/api/users.rs", 4, "medium"),
            FindingKind::Deviation(DeviationKind {
                deviating_sites: 3,
                assessed_sites: Some(47),
                convention_option_id: None,
                convention_label: "URI-versioned endpoints".to_string(),
            }),
        );
        dev.also_locations = vec![loc(18, false), loc(31, true)];

        let mut conv = kinded(
            finding("ARCH-PAGINATION-1", "src/api/list.rs", 7, "info"),
            FindingKind::ConventionObserved(ConventionObservedKind {
                convention_option_id: "cursor".to_string(),
                convention_label: "Cursor pagination".to_string(),
            }),
        );
        conv.also_locations = vec![crate::onboard::MergedLocation {
            repo: "owner/repo".to_string(),
            path: "src/api/search.rs".to_string(),
            line: 22,
            rule_id: "ARCH-PAGINATION-1".to_string(),
            snippet: "cursor".to_string(),
            consequence: false,
        }];

        let report = report_with(vec![dev, conv], vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();

        let where_cell = cell_text(&bytes, &sheet_entry_for(&bytes, "Deviations"), "G2");
        for site in ["src/api/users.rs:4", "src/api/users.rs:18"] {
            assert!(
                where_cell.contains(site),
                "the Deviations sheet must name every deviating site, missing {site}: \
                 {where_cell:?}"
            );
        }
        assert!(
            where_cell.contains("src/api/users.rs:31 (also affected)"),
            "a consequence site must be labeled, same wording as the findings sheet: \
             {where_cell:?}"
        );
        assert!(
            !where_cell.contains("ARCH-API-VERSIONING-1"),
            "this cell is client-facing: no raw rule ids (C4-P4): {where_cell:?}"
        );
        assert_eq!(
            cell_number(&bytes, "Deviations", "D2"),
            3.0,
            "all three sites count toward the numerator, absorbed ones included"
        );

        assert_eq!(
            cell_number(&bytes, "Conventions", "H2"),
            2.0,
            "the Conventions sheet's Sites column counts the absorbed site too"
        );
        let also = cell_text(&bytes, &sheet_entry_for(&bytes, "Conventions"), "I2");
        assert_eq!(
            also, "src/api/search.rs:22",
            "the Conventions sheet must keep the 'Also found at' column C5 added"
        );
    }

    /// req_pass3: the four named worksheets are STRUCTURAL — they exist even when a scan
    /// produced nothing for them, each saying so in a line a client can read.
    ///
    /// A vanishing tab reads as "we never looked"; an empty tab with a sentence reads as "we
    /// looked and there was none". The latter is the only honest one, and it is the same
    /// reason Dependencies and False Positives already render when empty.
    ///
    /// FAILS BEFORE THE CHANGE: `name="Deviations"` and `name="Conventions"` did not exist in
    /// any workbook, empty or not.
    #[test]
    fn req_pass3_the_four_named_sheets_exist_even_when_a_scan_finds_nothing() {
        let report = report_with(vec![], vec![]);
        let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
            .expect("a zero-finding scan must still export");
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        for name in [
            "Index",
            "All Findings",
            "Deviations",
            "Conventions",
            "Coverage",
        ] {
            assert!(
                workbook_xml.contains(&format!("name=\"{name}\"")),
                "the {name} sheet must exist even on an empty scan: {workbook_xml}"
            );
        }
        assert!(
            sheet_cell_strings(&bytes, "Deviations")
                .iter()
                .any(|s| s == "No consistency deviations were identified in this inspection."),
            "an empty Deviations sheet must say so rather than look broken"
        );
        assert!(
            sheet_cell_strings(&bytes, "Conventions")
                .iter()
                .any(|s| s.starts_with("This inspection derived no codebase conventions.")),
            "an empty Conventions sheet must say so rather than look broken"
        );
    }

    /// req_pass3: no internal pipeline vocabulary reaches a cell on any of the new
    /// client-facing sheets.
    ///
    /// Merge, dedup, ledger, pass names and bucket identifiers are how WE talk about the
    /// engine. A client reads this workbook. Checked per SHEET (the shared-strings pool is
    /// workbook-wide, so a pool-level check could not make this claim about a sheet).
    ///
    /// FAILS BEFORE THE CHANGE for `grounded` on the Coverage sheet (the `Citation Kind`
    /// column wrote the raw citation kind), which is why that token is in the list.
    #[tokio::test]
    async fn req_pass3_no_internal_pipeline_vocabulary_on_the_client_facing_sheets() {
        // The REAL corpus and real audited rule ids, deliberately: a corpus-less fixture
        // resolves every citation to "advisory", so the `grounded` leak the Coverage sheet
        // actually had would be UNREACHABLE and this test would pass vacuously on the
        // unfixed code. Verified: with the raw citation-kind cell restored, this test goes
        // red on `grounded` only because of this corpus.
        let corpus_path = camerata_rules::corpus_path();
        let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
        let report = report_with(
            all_five_kinds(),
            vec!["SUPABASE-RLS-ENABLED-1", "SEC-NO-HARDCODED-SECRETS-1"],
        );
        let bytes = build_workbook(
            &report.clone().seal(),
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        )
        .unwrap();

        const FORBIDDEN: [&str; 12] = [
            "merge",
            "merged",
            "dedup",
            "ledger",
            "do_now",
            "do_next",
            "grounded",
            "convention_observed",
            "no_convention",
            "not_assessed",
            "pass_failed",
            "also_locations",
        ];
        for sheet in ["Deviations", "Conventions", "Coverage"] {
            for cell in sheet_cell_strings(&bytes, sheet) {
                let lower = cell.to_ascii_lowercase();
                for token in FORBIDDEN {
                    assert!(
                        !lower.contains(token),
                        "the {sheet} sheet leaks internal vocabulary {token:?} into a \
                         client-facing cell: {cell:?}"
                    );
                }
            }
        }
    }

    /// req_pass3: the Index orients a client across the new tabs — it counts each of the five
    /// kinds and names the sheet each lives on, and it surfaces an UNCLASSIFIED row as its own
    /// visible line rather than folding it into the defect count.
    ///
    /// A5.6's backfill rule is that an absent kind is surfaced as a gap, never silently
    /// counted as a defect. With five kinds across four tabs, the place that rule has to hold
    /// is the one sheet a client opens first.
    ///
    /// FAILS BEFORE THE CHANGE: the Index had no statement-type block at all, so a client
    /// opening the workbook had no way to learn that an inspection now returns five different
    /// kinds of statement, let alone which tab held which.
    #[test]
    fn req_pass3_index_counts_the_statement_kinds_and_surfaces_unclassified_rows() {
        let report = report_with(all_five_kinds(), vec![]);
        let bytes =
            build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        let index = sheet_cell_strings(&bytes, "Index");
        for label in [
            "Statement types",
            "Defects",
            "Deviations from your own conventions",
            "Conventions followed consistently",
            "No convention adopted",
            "Not assessed this scan",
            "Unclassified",
        ] {
            assert!(
                index.iter().any(|s| s == label),
                "the Index must carry the {label:?} line: {index:#?}"
            );
        }

        // And `Unclassified` appears ONLY when there is one, so that its presence means
        // something rather than being a permanent zero a reader learns to skip.
        let clean = report_with(
            vec![kinded(
                finding("SEC-INJECTION-1", "src/orders.rs", 10, "critical"),
                camerata_api_types::audit::FindingKind::Defect,
            )],
            vec![],
        );
        let clean_bytes =
            build_workbook(&clean.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
        assert!(
            !sheet_cell_strings(&clean_bytes, "Index")
                .iter()
                .any(|s| s == "Unclassified"),
            "with every row kinded, the Unclassified line must not render"
        );
    }
}
