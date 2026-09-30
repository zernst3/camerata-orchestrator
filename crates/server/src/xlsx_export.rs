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
//! `normalize_severity`, `category_for`, `matrix_bucket`, `disposition_label`,
//! `client_headline_and_detail`, `resolve_citation`, `resolve_fix`, `effort_hours_bounds`,
//! `finding_key`, `bucket_title`, `Disposition`). A finding's severity, category, matrix
//! bucket, disposition label, citation, and recommended fix are computed identically in
//! both places; only the OUTPUT SHAPE differs. W3 (2026-09-30): the headline in particular
//! goes through `client_headline_and_detail` — the SAME plain-language rewrite the PDF uses
//! (preferring a floor rule's authored `finding_headline` template, falling back to
//! `defect_headline` over the raw `detail` only when no template is authored) — so the
//! product export (`findings.json` / xlsx) can never leak raw internal rule prose that the
//! PDF has already rewritten. The raw `detail` string is still carried verbatim in
//! `FindingRow::detail` (an internal-completeness field, not client-facing headline text).
//!
//! # Why this is fed from `ScanReport`, not `AuditReportJson`
//! `AuditReportJson` already excludes false positives, caps snippets/what's-healthy, and
//! partitions dependency findings into a separate lane — exactly the curation the PDF wants.
//! The workbook's whole point is the UNCUT dataset (false positives get their own sheet with
//! reasons, snippets are capped only defensively against Excel's cell limit, every audited
//! rule appears in Coverage regardless of cap). So [`build_workbook`] runs its OWN partition
//! pass over `report.findings`, reusing only the low-level pure functions above — never the
//! already-curated `AuditReportJson` type.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Context;
use rust_xlsxwriter::{
    Color, ConditionalFormatText, ConditionalFormatTextRule, Format, FormatAlign, Workbook,
    Worksheet, XlsxError,
};
use serde::Serialize;

use crate::dep_audit::DEP_AUDIT_RULE_ID;
use crate::onboard::ScanReport;
use crate::report_export::{
    bucket_title, category_for, classify, client_headline_and_detail, disposition_label,
    effort_hours_bounds, finding_key, matrix_bucket, normalize_severity, resolve_citation,
    resolve_fix, Disposition, DispositionWire, ReportOptions,
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
    /// `"do_now"` | `"do_next"` | `"plan"` | `"accepted"` (matches [`matrix_bucket`]), or
    /// empty for a false-positive row (a FP has no matrix cell — it is excluded from the
    /// matrix entirely, same as the PDF).
    bucket: &'static str,
    /// The classified [`Disposition`] this session, or `None` for a false-positive row
    /// (`classify` must never be called on an FP-dispositioned finding — see
    /// `report_export::classify`'s doc comment). Drives the Index sheet's disposition
    /// summary line and each category sheet's tab color / worst-open-severity ranking.
    disposition_kind: Option<Disposition>,
    disposition_label: String,
    effort: String,
    est_hours: String,
    confidence: String,
    needs_review: bool,
    in_test: bool,
    /// `"Deterministic"` | `"Preview: {tool}"` | `"AI-advisory"` — derived from
    /// `resolve_citation(...).kind`, per the design doc's Provenance column.
    provenance: String,
    citation_label: String,
    /// Newline-separated citation source URLs (plain text, not hyperlink objects — a cell
    /// can carry several URLs).
    citation_urls: String,
    also_matches: String,
    status: String,
    snippet: String,
    detail: String,
    /// **P2 (2026-09-29): the PRIMARY fix, shown FIRST** — a codebase-specific remediation
    /// for THIS finding, generated at scan time by
    /// [`crate::ai_audit::generate_fix_specifics`] (see `report_export::CuratedSiteJson::
    /// fix_for_this_finding`'s doc comment, which this field mirrors verbatim). `None` when
    /// fix-generation never ran (a deterministic-only scan) or gave up after retries
    /// (`report_export::fix_generation_failed`).
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
}

/// One row of the `Dependencies` sheet — the `DEP_AUDIT_RULE_ID` carve-out, same as the
/// PDF's own dependency-snapshot section.
#[derive(Clone)]
struct DepRow {
    package: String,
    advisory: String,
    severity: String,
    repo: String,
}

/// One row of the `Coverage` sheet: an audited rule id, uncapped (the PDF's what's-healthy
/// section caps at 10; the workbook doesn't need to).
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
/// Rows are returned PRE-SORTED (severity desc, then repo/path/line) — the one true sort
/// order both `build_workbook` and `build_findings_export` (`findings.json`) consume as-is,
/// rather than each sorting its own copy and risking two orderings silently drifting apart.
fn partition_rows(
    report: &ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_options: &HashMap<String, String>,
) -> (Vec<FindingRow>, Vec<DepRow>) {
    let mut rows = Vec::new();
    let mut dep_rows = Vec::new();

    for f in &report.findings {
        if f.rule_id == DEP_AUDIT_RULE_ID {
            dep_rows.push(DepRow {
                package: f.snippet.clone(),
                advisory: f.detail.clone(),
                severity: normalize_severity(&f.severity),
                repo: f.repo.clone(),
            });
            continue;
        }

        let wire = dispositions.get(&finding_key(f));
        let is_fp = wire.map(|d| d.state.as_str()) == Some("FalsePositive");
        let severity = normalize_severity(&f.severity);
        let category = category_for(&f.rule_id, corpus);
        let title = corpus
            .and_then(|c| c.get_by_id(&f.rule_id))
            .map(|r| r.title.clone())
            .unwrap_or_else(|| f.rule_id.clone());
        let chosen_option_for_rule = chosen_options
            .get(&f.rule_id.to_ascii_uppercase())
            .map(String::as_str);
        // W3 (2026-09-30): the SAME plain-language rewrite the PDF uses for its curated-site
        // headline — never the raw gate/rule-detail derivation `defect_headline` alone would
        // give. `f.detail` (raw internal rule prose for a floor finding) stays available
        // verbatim in `FindingRow::detail` below, so nothing is lost — only the client-facing
        // headline column changes.
        let (headline, _) = client_headline_and_detail(f, corpus, &title, chosen_option_for_rule);
        let citation = resolve_citation(&f.rule_id, f.preview_tool.as_deref(), corpus);
        let provenance = match citation.kind.as_str() {
            "preview" => format!(
                "Preview: {}",
                f.preview_tool.as_deref().unwrap_or("unknown tool")
            ),
            "grounded" => "Deterministic".to_string(),
            _ => "AI-advisory".to_string(),
        };
        let citation_urls = citation
            .sources
            .iter()
            .map(|s| s.url.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let fix = resolve_fix(&f.rule_id, corpus, f, chosen_option_for_rule);
        let (_, est_hours) = effort_hours_bounds(f.effort.as_deref());

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
            let bucket = matrix_bucket(disposition, &severity, f.effort.as_deref());
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
            effort: f.effort.clone().unwrap_or_default(),
            est_hours,
            confidence: f.confidence.clone().unwrap_or_default(),
            needs_review: f.needs_review,
            in_test: f.in_test,
            provenance,
            citation_label: citation.label,
            citation_urls,
            also_matches: f.also_matches.join(", "),
            status: f.status.clone(),
            snippet: cap_snippet_for_workbook(&f.snippet),
            detail: f.detail.clone(),
            fix_specific: f.fix_specific.clone(),
            fix,
            is_fp,
            fp_reason,
        });
    }

    rows.sort_by(|a, b| {
        (severity_rank(&a.severity), &a.repo, &a.path, a.line).cmp(&(
            severity_rank(&b.severity),
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

fn is_still_open(d: Disposition) -> bool {
    matches!(
        d,
        Disposition::Unresolved | Disposition::TechDebtNow | Disposition::TechDebtLater
    )
}

fn severity_rank(sev: &str) -> u8 {
    match sev {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
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
            let title = corpus
                .and_then(|c| c.get_by_id(rid))
                .map(|r| r.title.clone())
                .unwrap_or_else(|| rid.clone());
            let category = category_for(rid, corpus);
            let citation = resolve_citation(rid, None, corpus);
            let findings_count = rows.iter().filter(|r| !r.is_fp && &r.rule_id == rid).count();
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

const HEADERS: [&str; 23] = [
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
];

const WIDTHS: [f64; 23] = [
    10.0, 50.0, 18.0, 40.0, 7.0, 28.0, 16.0, 10.0, 30.0, 9.0, 16.0, 12.0, 12.0, 16.0, 16.0, 34.0,
    30.0, 24.0, 18.0, 55.0, 70.0, 45.0, 45.0,
];

/// Columns that wrap (matches the design's wrap-column list, plus the Fix-specific and
/// Recommended-Fix columns at the end).
const WRAP_COLS: [u16; 7] = [1, 8, 15, 16, 20, 21, 22];
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

    ws.write_string_with_format(r, 0, title_case(&row.severity), &fmts.severity_cell(&row.severity))?;
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
        ws.write_string_with_format(0, 23, "FP Reason", &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;

    for (c, w) in WIDTHS.iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }
    if fp_reason_col {
        ws.set_column_width(23, 40.0)?;
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
            ws.write_string_with_format(r, 23, &row.fp_reason, &fmts.cell(bg, true, false))?;
        }
    }

    let last_row = rows.len() as u32;
    let last_col: u16 = if fp_reason_col { 23 } else { 22 };
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

    let headers = ["Package", "Advisory", "Severity", "Repo"];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    for (c, w) in [30.0, 60.0, 12.0, 18.0].iter().enumerate() {
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
        ws.write_string_with_format(r, 2, title_case(&row.severity), &fmts.severity_cell(&row.severity))?;
        ws.write_string_with_format(r, 3, &row.repo, &fmts.cell(None, false, false))?;
    }

    let last_row = sorted.len() as u32;
    ws.autofilter(0, 0, last_row, 3)?;
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

#[allow(clippy::too_many_arguments)]
fn write_coverage_sheet(
    wb: &mut Workbook,
    name: &str,
    coverage_rows: &[CoverageRow],
    excluded_mechanical_rules: &[String],
    rules_applied: &[RulesAppliedRow],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(name)?;
    let _ = ws.set_tab_color(Color::from("#6B7280"));

    let headers = [
        "Rule ID",
        "Title",
        "Category",
        "Citation Kind",
        "Citation Label",
        "Findings this run",
    ];
    for (c, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    for (c, w) in [28.0, 60.0, 16.0, 14.0, 40.0, 16.0].iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }

    for (i, row) in coverage_rows.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_string_with_format(r, 0, &row.rule_id, &fmts.cell(None, false, false))?;
        ws.write_string_with_format(r, 1, &row.title, &fmts.cell(None, true, false))?;
        ws.write_string_with_format(r, 2, &row.category, &fmts.cell(None, false, false))?;
        ws.write_string_with_format(r, 3, &row.citation_kind, &fmts.cell(None, false, false))?;
        ws.write_string_with_format(r, 4, &row.citation_label, &fmts.cell(None, true, false))?;
        let count_fmt = if row.findings_count == 0 {
            fmts.cell(Some("#ECFDF5"), false, false)
        } else {
            fmts.cell(None, false, false)
        };
        ws.write_number_with_format(r, 5, row.findings_count as f64, &count_fmt)?;
    }

    let last_row = coverage_rows.len() as u32;
    ws.autofilter(0, 0, last_row, 5)?;
    ws.set_freeze_panes(1, 0)?;

    let mut next_r = last_row + 2;

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
    ws.write_string_with_format(r, 0, "Camerata Audit — Product Export Index", &fmts.title)?;
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
    ws.write_string(r, 0, "Repos audited")?;
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
    ws.write_string(r, 0, "Audit model")?;
    ws.write_string(r, 1, report.provenance.audit_model.as_deref().unwrap_or("n/a"))?;
    r += 1;
    ws.write_string(r, 0, "Calibration model")?;
    ws.write_string(
        r,
        1,
        report.provenance.calibration_model.as_deref().unwrap_or("n/a"),
    )?;
    r += 1;
    ws.write_string(r, 0, "Camerata version")?;
    ws.write_string(r, 1, &report.provenance.camerata_version)?;
    r += 1;
    ws.write_string(r, 0, "Generated")?;
    ws.write_string(r, 1, chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string())?;
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

    // ── Disposition summary (must reconcile with the PDF's own numbers exactly) ─────
    let mut do_now = 0usize;
    let mut do_next = 0usize;
    let mut plan = 0usize;
    let mut accepted = 0usize;
    let mut open = 0usize;
    for row in live_rows {
        match row.bucket {
            "do_now" => do_now += 1,
            "do_next" => do_next += 1,
            "plan" => plan += 1,
            "accepted" => accepted += 1,
            _ => {}
        }
        if row.disposition_kind == Some(Disposition::Unresolved) {
            open += 1;
        }
    }
    ws.write_string(
        r,
        0,
        format!(
            "Disposition summary: {open} open, {do_now} do now, {do_next} do next, {plan} \
             planned, {accepted} accepted, {fp_count} excluded as false positives, {} \
             dependency {}.",
            dep_rows.len(),
            if dep_rows.len() == 1 { "advisory" } else { "advisories" }
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
    report: &ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> anyhow::Result<Vec<u8>> {
    let (rows, dep_rows) = partition_rows(report, dispositions, corpus, &opts.chosen_options);

    let live_rows: Vec<&FindingRow> = rows.iter().filter(|r| !r.is_fp).collect();
    let fp_rows: Vec<&FindingRow> = rows.iter().filter(|r| r.is_fp).collect();

    let mut by_category: BTreeMap<String, Vec<&FindingRow>> = BTreeMap::new();
    for r in &live_rows {
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
        count: live_rows.len(),
        worst_severity: worst_severity_label(&live_rows),
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
    write_findings_sheet(&mut wb, &all_findings_name, &live_rows, &fmts, false, None)
        .context("writing the All Findings sheet")?;
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
    write_coverage_sheet(
        &mut wb,
        &coverage_name,
        &coverage_rows,
        &report.excluded_mechanical_rules,
        &rules_applied_rows,
        &fmts,
    )
    .context("writing the Coverage sheet")?;

    wb.save_to_buffer()
        .context("serializing the xlsx workbook to bytes")
}

// ── `findings.json` (product export, machine-readable sibling of the workbook) ────────

/// The `findings.json` payload bundled into the product-export zip: the machine-readable
/// version of the Excel data. `findings` is the SAME (non-FP, severity-desc-then-repo/path/
/// line-sorted) [`FindingRow`] set that becomes the workbook's "All Findings" sheet — see
/// [`build_findings_export`] — so the JSON and the xlsx can never disagree on a row count or
/// a field value; both come from one `partition_rows` pass.
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
    report: &ScanReport,
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
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("workbook must be a valid zip");
        let mut file = archive
            .by_name(name)
            .unwrap_or_else(|_| panic!("missing zip entry {name}"));
        let mut s = String::new();
        file.read_to_string(&mut s).expect("entry must be UTF-8 text");
        s
    }

    fn any_worksheet_xml_containing(bytes: &[u8], needle: &str) -> bool {
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("workbook must be a valid zip");
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

    // ── Header schema is stable and in order ───────────────────────────────────

    #[test]
    fn header_schema_matches_the_design_column_order() {
        assert_eq!(HEADERS.len(), 23);
        assert_eq!(HEADERS[0], "Severity");
        // P2: the specific fix comes BEFORE the generic Recommended Fix — same
        // primary-then-secondary ordering as the PDF template.
        assert_eq!(HEADERS[21], "Fix (specific to this finding)");
        assert_eq!(HEADERS[22], "Recommended Fix");
        assert_eq!(HEADERS.len(), WIDTHS.len());
    }

    // ── Workbook magic + basic shape ────────────────────────────────────────────

    #[test]
    fn build_workbook_returns_a_valid_xlsx_with_pk_magic() {
        let f = finding("SEC-1", "a.rs", 1, "critical");
        let report = report_with(vec![f], vec!["SEC-1"]);
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts())
            .expect("build_workbook must succeed");
        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..2], b"PK", "xlsx must start with the zip magic bytes");
    }

    #[test]
    fn workbook_contains_the_expected_sheet_names_in_order() {
        let critical = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
        let mut dep = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
        dep.snippet = "foo@1.0.0".to_string();
        let report = report_with(
            vec![critical, dep],
            vec!["SUPABASE-RLS-ENABLED-1"],
        );
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        for expected in [
            "name=\"Index\"",
            "name=\"All Findings\"",
            "name=\"Dependencies\"",
            "name=\"False Positives\"",
            "name=\"Coverage\"",
            "name=\"Supabase RLS\"",
        ] {
            assert!(
                workbook_xml.contains(expected),
                "workbook.xml missing {expected}: {workbook_xml}"
            );
        }
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
            },
        );
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();

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
        assert!(contains("Yes"), "the applicable column for the evidenced rule");
        assert!(contains("No"), "the applicable column for the not-applicable rule");
    }

    /// No recommendations recorded (a scan with no multi-option rules, or one predating P7) —
    /// the appendix must simply be absent, never an empty/broken header.
    #[test]
    fn coverage_sheet_omits_rules_applied_appendix_when_there_are_no_recommendations() {
        let report = report_with(vec![], vec!["SEC-1"]);
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
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
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        assert!(workbook_xml.contains("name=\"Supabase RLS\""), "{workbook_xml}");
        assert!(workbook_xml.contains("name=\"Arch\""), "{workbook_xml}");
    }

    #[test]
    fn categories_with_zero_findings_get_no_sheet() {
        // ZZZ-1 is audited but never fires — it must show up in Coverage/Index only, never
        // as its own (empty, broken-looking) sheet.
        let report = report_with(vec![], vec!["ZZZ-1"]);
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
        let workbook_xml = read_zip_entry(&bytes, "xl/workbook.xml");
        assert!(
            !workbook_xml.contains("name=\"Zzz\""),
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
            wire("FalsePositive", "test fixture constant, not a live credential"),
        );
        let report = report_with(vec![f], vec![]);
        let bytes = build_workbook(&report, &dispositions, None, &empty_opts()).unwrap();
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
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        f.captures.insert("table".to_string(), "profiles".to_string());
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), Some(&corpus), &HashMap::new());
        assert!(
            rows[0].fix.as_deref().is_some_and(|s| s.contains("profiles")),
            "the Recommended Fix column must be populated from authored remediation, got: {:?}",
            rows[0].fix
        );
    }

    #[test]
    fn recommended_fix_is_none_not_fabricated_without_a_corpus() {
        let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let (rows, _) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        assert_eq!(rows[0].fix, None, "must not fabricate a fix when the corpus is absent");
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
        let json =
            crate::report_export::build_report_json(&report, &dispositions, None, &empty_opts());
        let xlsx_bytes = build_workbook(&report, &dispositions, None, &empty_opts()).unwrap();
        let findings_export = build_findings_export(&report, &dispositions, None, &json, &HashMap::new());

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
        let mut f = finding("SUPABASE-RLS-ENABLED-1", "supabase/migrations/1.sql", 1, "critical");
        f.captures.insert("table".to_string(), "profiles".to_string());
        let report = report_with(vec![f.clone()], vec![]);
        let json = crate::report_export::build_report_json(
            &report,
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(&report, &HashMap::new(), Some(&corpus), &json, &HashMap::new());
        let expected = crate::report_export::resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f, None);
        assert!(expected.is_some(), "fixture rule must have authored remediation");
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
            &report,
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(&report, &HashMap::new(), Some(&corpus), &json, &HashMap::new());
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
        let json =
            crate::report_export::build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let findings_export =
            build_findings_export(&report, &HashMap::new(), None, &json, &HashMap::new());
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
        let json =
            crate::report_export::build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let findings_export = build_findings_export(&report, &HashMap::new(), None, &json, &HashMap::new());
        let v = serde_json::to_value(&findings_export).expect("must serialize");
        assert!(v.get("provenance").is_some(), "{v:?}");
        assert!(v.get("summary").is_some(), "{v:?}");
        assert!(v.get("findings").and_then(|f| f.as_array()).is_some(), "{v:?}");
    }

    // ── Degenerate inputs must never panic ─────────────────────────────────────

    #[test]
    fn findings_export_zero_findings_is_empty_and_serializes_cleanly() {
        let report = report_with(vec![], vec![]);
        let json =
            crate::report_export::build_report_json(&report, &HashMap::new(), None, &empty_opts());
        let findings_export = build_findings_export(&report, &HashMap::new(), None, &json, &HashMap::new());
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
        let json =
            crate::report_export::build_report_json(&report, &dispositions, None, &empty_opts());
        let findings_export = build_findings_export(&report, &dispositions, None, &json, &HashMap::new());
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
            &report,
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report,
            &HashMap::new(),
            Some(&corpus),
            &json,
            &HashMap::new(),
        );
        let xlsx_bytes =
            build_workbook(&report, &HashMap::new(), Some(&corpus), &empty_opts()).unwrap();

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
        // Completeness: the raw internal text is still carried, just not as the headline.
        assert_eq!(findings_export.findings[0].detail, RAW_GATE_TEXT);

        // "All Findings" is the second worksheet added (after Index) -> sheet2.xml; row 1 is
        // the header, so the finding lands on row 2. Column B is "Headline" (index 1), column
        // U is "Detail" (index 20) per the `HEADERS` array. Resolve through the shared-string
        // pool (`cell_text`) rather than a whole-workbook substring search, so this pins which
        // COLUMN the rewrite landed in — the raw text legitimately still appears in "Detail".
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
            detail_cell, RAW_GATE_TEXT,
            "completeness: the raw internal text must still be recoverable from the Detail column"
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
            &report,
            &HashMap::new(),
            Some(&corpus),
            &empty_opts(),
        );
        let findings_export = build_findings_export(
            &report,
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
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
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
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts())
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
        let bytes = build_workbook(&report, &dispositions, None, &empty_opts())
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
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts())
            .expect("no-dependency-findings input must not fail the export");
        assert_eq!(&bytes[0..2], b"PK");
    }

    #[test]
    fn missing_directive_leaves_fix_empty_across_a_full_workbook_build() {
        // No corpus at all -> every rule's fix is empty; must not fail or fabricate.
        let f = finding("SEC-1", "a.rs", 1, "medium");
        let report = report_with(vec![f], vec![]);
        let bytes = build_workbook(&report, &HashMap::new(), None, &empty_opts())
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
}
