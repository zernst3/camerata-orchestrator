//! H4 item 3: the optional triage log. Per rule, what the AI pass proposed, what review kept,
//! what it held or set aside as information only, what it threw out, and why.
//!
//! Off by default ([`ReportOptions::triage_log`]). When asked for it rides in the product
//! package as a workbook sheet ([`TRIAGE_LOG_SHEET`]) and a CSV ([`TRIAGE_LOG_CSV`]); the
//! release gate's secrets check scans both like every other member, and its dataset-set check
//! admits the CSV only as this one optional file.
//!
//! # What counts
//!
//! "Proposed by the AI pass" is every AI-tier row ([`crate::report_export::is_ai_tier`]) that
//! reached review in the sealed report, the same rows the workbook and PDF are built from.
//! Each lands in exactly one outcome, decided by the SAME predicates the PDF and the workbook
//! use (`effective_bucket` and its gates), so the log cannot disagree with either:
//!
//! - thrown out: the reviewer marked it a false positive (their reason is quoted);
//! - held for review: the calibration hedge, no grounded citation, or no fix to show;
//! - information only: a convention to consider rather than an action item;
//! - kept: everything else (an action tier, or accepted by the reviewer).
//!
//! Rows the scan merged into another row at the same location appear on the surviving row's
//! "Also violates" column, not here: the log is about review outcomes, not de-duplication.
//!
//! # The caption
//!
//! "Proposed by the AI pass" counts rows that REACHED review. Repeats the scan dropped before
//! review (same path, line and rule) cannot be counted by anything downstream, so the log says
//! how many there were: [`caption`], with the count from the scan ledger
//! ([`crate::scan_ledger::ScanLedger::deduped_total`]). The sheet prints it under the title. The
//! CSV is a plain table with a fixed header and carries no caption row (a text row would break
//! every consumer that reads it as data), so this module doc and [`triage_log_csv`] are where
//! the CSV's counting basis is stated.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::onboard::Finding;
use crate::report_export::{
    classify, effective_bucket, finding_key, is_action_row_missing_a_fix, is_ai_tier,
    is_held_for_review, is_informational, is_uncited_ai_finding, title_for_finding,
    DispositionWire, ReportOptions,
};

/// The workbook sheet's name.
pub const TRIAGE_LOG_SHEET: &str = "Triage Log";
/// The CSV's archive path in the product package.
pub const TRIAGE_LOG_CSV: &str = "csv/triage-log.csv";

/// The CSV header and the sheet's column titles, in order.
const COLUMNS: [&str; 8] = [
    "Rule ID",
    "Rule",
    "Proposed by the AI pass",
    "Kept",
    "Held for review",
    "Information only",
    "Thrown out",
    "Why",
];

/// One rule's triage outcome. `proposed == kept + held_for_review + information_only +
/// thrown_out` always.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageLogRow {
    pub rule_id: String,
    pub rule: String,
    pub proposed: usize,
    pub kept: usize,
    pub held_for_review: usize,
    pub information_only: usize,
    pub thrown_out: usize,
    /// Every reason a proposal was not kept, with its count, most frequent first:
    /// `"2 held for review: calibration flagged it as needs review; 1 thrown out: ..."`.
    pub why: String,
}

/// The triage log for `report`, one row per rule the AI pass proposed anything under, sorted
/// by rule id. Pure.
pub fn build_triage_log(
    report: &crate::onboard::ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    chosen_options: &HashMap<String, String>,
) -> Vec<TriageLogRow> {
    #[derive(Default)]
    struct Tally {
        rule: String,
        proposed: usize,
        kept: usize,
        held: usize,
        info: usize,
        thrown: usize,
        reasons: BTreeMap<String, usize>,
    }
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for f in report
        .findings
        .iter()
        .filter(|f| f.rule_id != crate::dep_audit::DEP_AUDIT_RULE_ID && is_ai_tier(f))
    {
        let tally = tallies.entry(f.rule_id.clone()).or_default();
        if tally.rule.is_empty() {
            tally.rule = title_for_finding(f, corpus);
        }
        tally.proposed += 1;
        let wire = dispositions.get(&finding_key(f));
        if let Some(w) = wire.filter(|w| w.state == "FalsePositive") {
            tally.thrown += 1;
            let reason = if w.reason.trim().is_empty() {
                "thrown out: marked a false positive by the reviewer".to_string()
            } else {
                format!(
                    "thrown out: marked a false positive by the reviewer ({})",
                    w.reason.trim()
                )
            };
            *tally.reasons.entry(reason).or_insert(0) += 1;
            continue;
        }
        let chosen = chosen_options
            .get(&f.rule_id.to_ascii_uppercase())
            .map(String::as_str);
        let disposition = classify(f, wire);
        let severity = crate::severity_bounds::bounded_severity(f, corpus);
        let bucket = effective_bucket(
            f,
            disposition,
            &severity,
            corpus,
            report.test_file_count,
            chosen,
        );
        let why = |outcome: &str| {
            format!(
                "{outcome}: {}",
                set_aside_reason(
                    f,
                    disposition,
                    &severity,
                    corpus,
                    report.test_file_count,
                    chosen
                )
            )
        };
        match bucket {
            "held" => {
                tally.held += 1;
                *tally.reasons.entry(why("held for review")).or_insert(0) += 1;
            }
            "informational" => {
                tally.info += 1;
                *tally.reasons.entry(why("information only")).or_insert(0) += 1;
            }
            _ => tally.kept += 1,
        }
    }
    tallies
        .into_iter()
        .map(|(rule_id, t)| {
            let mut reasons: Vec<(String, usize)> = t.reasons.into_iter().collect();
            // Most frequent first; ties in text order (the map already sorted them).
            reasons.sort_by_key(|r| std::cmp::Reverse(r.1));
            let why = reasons
                .iter()
                .map(|(reason, n)| format!("{n} {reason}"))
                .collect::<Vec<_>>()
                .join("; ");
            TriageLogRow {
                rule_id,
                rule: t.rule,
                proposed: t.proposed,
                kept: t.kept,
                held_for_review: t.held,
                information_only: t.info,
                thrown_out: t.thrown,
                why,
            }
        })
        .collect()
}

/// Why a row left the action tiers, checked in `effective_bucket`'s own order so the reason
/// named is the gate that decided.
fn set_aside_reason(
    f: &Finding,
    disposition: crate::report_export::Disposition,
    severity: &str,
    corpus: Option<&camerata_rules::RuleSet>,
    test_file_count: usize,
    chosen: Option<&str>,
) -> &'static str {
    if is_held_for_review(f, disposition) {
        "calibration flagged it as needs review"
    } else if is_uncited_ai_finding(f, corpus) {
        "no grounded citation for it"
    } else if is_informational(f, disposition, severity, corpus, test_file_count) {
        "a convention to consider, not an action item"
    } else if is_action_row_missing_a_fix(f, disposition, severity, corpus, test_file_count, chosen)
    {
        "no fix to show for it"
    } else {
        "routed out of the action tiers"
    }
}

/// What "Proposed by the AI pass" counts, and what it leaves out: `deduped` is the scan
/// ledger's count of rows merged into another at the same location before review. Printed on
/// the workbook sheet; the CSV states the same basis here because it cannot carry a caption row.
pub fn caption(deduped: usize) -> String {
    format!(
        "Rows that reached review; {deduped} {} merged into another at the same location \
         {} not listed",
        if deduped == 1 { "row" } else { "rows" },
        if deduped == 1 { "is" } else { "are" },
    )
}

/// The CSV bytes: the header, then one record per row. An empty log is the header alone. The
/// counts are rows that reached review; see [`caption`] for the rows merged away before it.
pub fn triage_log_csv(rows: &[TriageLogRow]) -> anyhow::Result<Vec<u8>> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(COLUMNS)?;
    for r in rows {
        wtr.write_record([
            r.rule_id.clone(),
            r.rule.clone(),
            r.proposed.to_string(),
            r.kept.to_string(),
            r.held_for_review.to_string(),
            r.information_only.to_string(),
            r.thrown_out.to_string(),
            r.why.clone(),
        ])?;
    }
    Ok(wtr.into_inner()?)
}

/// Write the workbook sheet.
pub(super) fn write_triage_log_sheet(
    wb: &mut rust_xlsxwriter::Workbook,
    rows: &[TriageLogRow],
    deduped: usize,
    fmts: &super::Formats,
) -> Result<(), rust_xlsxwriter::XlsxError> {
    let ws = wb.add_worksheet();
    ws.set_name(TRIAGE_LOG_SHEET)?;
    ws.write_string_with_format(0, 0, TRIAGE_LOG_SHEET, &fmts.section_title)?;
    ws.write_string(
        1,
        0,
        "Per rule: what the AI pass proposed, what review kept, held or set aside, what it \
         threw out and why.",
    )?;
    ws.write_string(2, 0, caption(deduped))?;
    const HEADER_ROW: u32 = 3;
    for (c, h) in COLUMNS.iter().enumerate() {
        ws.write_string_with_format(HEADER_ROW, c as u16, *h, &fmts.header)?;
    }
    for (c, w) in [28.0, 40.0, 12.0, 8.0, 10.0, 12.0, 10.0, 80.0]
        .iter()
        .enumerate()
    {
        ws.set_column_width(c as u16, *w)?;
    }
    for (i, r) in rows.iter().enumerate() {
        let row = HEADER_ROW + 1 + i as u32;
        let text = fmts.cell(None, false, false);
        let wrap = fmts.cell(None, true, false);
        ws.write_string_with_format(row, 0, &r.rule_id, &text)?;
        ws.write_string_with_format(row, 1, &r.rule, &wrap)?;
        for (c, n) in [
            r.proposed,
            r.kept,
            r.held_for_review,
            r.information_only,
            r.thrown_out,
        ]
        .iter()
        .enumerate()
        {
            ws.write_number(row, 2 + c as u16, *n as f64)?;
        }
        ws.write_string_with_format(row, 7, &r.why, &wrap)?;
    }
    let last = HEADER_ROW + rows.len() as u32;
    ws.autofilter(HEADER_ROW, 0, last, 7)?;
    ws.set_freeze_panes(HEADER_ROW + 1, 0)?;
    Ok(())
}

/// Whether `options` asked for the triage log.
pub(super) fn wanted(options: &ReportOptions) -> bool {
    options.triage_log
}
