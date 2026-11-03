//! The workbook sheets that state the review (WO9, design 3B): the Tree sheet (the category, rule
//! and sub-group nodes with their summaries and counts) and the Decisions sheet (every decision
//! record the report was reviewed with). Both are renders of the one [`ReviewedReport`]; the
//! Remediation plan sheet is in [`super::plan_sheet`].

use rust_xlsxwriter::{Workbook, XlsxError};

use super::subgroups::SubgroupFacts;
use super::{CappedSheet, Formats};
use crate::report_export::decisions_file::DecisionRowJson;
use crate::report_export::tree_view::TreeNodeView;

/// The Remediation plan sheet's name.
pub const PLAN_SHEET: &str = "Remediation plan";
/// The Tree sheet's name.
pub const TREE_SHEET: &str = "Tree";
/// The Decisions sheet's name.
pub const DECISIONS_SHEET: &str = "Decisions";

/// The review levels a Tree row counts, in the order its columns are written.
const LEVEL_COLUMNS: [(&str, &str); 3] = [
    ("Personally reviewed", "personally_reviewed"),
    ("Reviewed as pattern", "reviewed_as_pattern:"),
    ("Engine verified", "engine_verified"),
];

/// Sites under a node at a review level: the wire name, or every pattern review for the prefix.
fn level_count(node: &TreeNodeView, wire: &str) -> usize {
    node.review_levels
        .iter()
        .filter(|(name, _)| {
            if wire.ends_with(':') {
                name.starts_with(wire)
            } else {
                name.as_str() == wire
            }
        })
        .map(|(_, n)| *n)
        .sum()
}

const TREE_HEADERS: [&str; 11] = [
    "Node id",
    "Level",
    "Parent",
    "Label",
    "Cause",
    "Summary",
    "Sites",
    "False positives",
    "Personally reviewed",
    "Reviewed as pattern",
    "Engine verified",
];

const INTERNAL_TREE_HEADERS: [&str; 6] = [
    "Summary source",
    "Escalated",
    "Below the line",
    "Outliers",
    "Unplaced",
    "Review flags",
];

pub(super) fn write_tree_sheet(
    wb: &mut Workbook,
    nodes: &[TreeNodeView],
    internal: bool,
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let mut ws = CappedSheet::new(wb.add_worksheet());
    ws.set_name(TREE_SHEET)?;
    let _ = ws.set_tab_color(rust_xlsxwriter::Color::from("#6B7280"));
    let mut headers: Vec<&str> = TREE_HEADERS.to_vec();
    if internal {
        headers.extend(INTERNAL_TREE_HEADERS);
    }
    for (c, h) in headers.iter().enumerate() {
        ws.write_text_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    let widths = [
        34.0, 10.0, 34.0, 40.0, 40.0, 70.0, 8.0, 10.0, 12.0, 12.0, 12.0, 12.0, 10.0, 10.0, 10.0,
        10.0, 50.0,
    ];
    for (c, _) in headers.iter().enumerate() {
        ws.set_column_width(c as u16, widths.get(c).copied().unwrap_or(14.0))?;
    }
    let text = fmts.cell(None, false, false);
    let wrap = fmts.cell(None, true, false);
    for (i, n) in nodes.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_text_with_format(r, 0, &n.id, &text)?;
        ws.write_text_with_format(r, 1, &n.level, &text)?;
        ws.write_text_with_format(r, 2, &n.parent, &text)?;
        ws.write_text_with_format(r, 3, &n.label, &wrap)?;
        ws.write_text_with_format(r, 4, &n.cause, &wrap)?;
        ws.write_text_with_format(r, 5, &n.summary, &wrap)?;
        ws.write_number_with_format(r, 6, n.sites as f64, &text)?;
        ws.write_number_with_format(r, 7, n.false_positives as f64, &text)?;
        for (k, (_, wire)) in LEVEL_COLUMNS.iter().enumerate() {
            ws.write_number_with_format(r, 8 + k as u16, level_count(n, wire) as f64, &text)?;
        }
        if let Some(t) = &n.internal {
            ws.write_text_with_format(r, 11, &t.summary_source, &text)?;
            ws.write_number_with_format(r, 12, t.escalated as f64, &text)?;
            ws.write_number_with_format(r, 13, t.below_the_line as f64, &text)?;
            ws.write_number_with_format(r, 14, t.outliers as f64, &text)?;
            ws.write_number_with_format(r, 15, t.unplaced as f64, &text)?;
            ws.write_text_with_format(r, 16, t.flags.join("; "), &wrap)?;
        }
    }
    ws.autofilter(0, 0, nodes.len() as u32, (headers.len() - 1) as u16)?;
    ws.set_freeze_panes(1, 0)?;
    Ok(())
}

const DECISION_HEADERS: [&str; 11] = [
    "Decision id",
    "Scope",
    "Site id",
    "Group",
    "State",
    "Merged into",
    "Sites covered",
    "Inherited from",
    "Reason recorded",
    "Reason",
    "Revoked",
];

pub(super) fn write_decisions_sheet(
    wb: &mut Workbook,
    rows: &[DecisionRowJson],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let mut ws = CappedSheet::new(wb.add_worksheet());
    ws.set_name(DECISIONS_SHEET)?;
    let _ = ws.set_tab_color(rust_xlsxwriter::Color::from("#6B7280"));
    for (c, h) in DECISION_HEADERS.iter().enumerate() {
        ws.write_text_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    let widths = [
        16.0, 8.0, 18.0, 34.0, 16.0, 18.0, 10.0, 16.0, 10.0, 50.0, 9.0,
    ];
    for (c, w) in widths.iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }
    let text = fmts.cell(None, false, false);
    let wrap = fmts.cell(None, true, false);
    let yes = |b: bool| if b { "Yes" } else { "" };
    for (i, d) in rows.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_text_with_format(r, 0, &d.id, &text)?;
        ws.write_text_with_format(r, 1, &d.scope, &text)?;
        ws.write_text_with_format(r, 2, &d.site_id, &text)?;
        ws.write_text_with_format(r, 3, &d.node_id, &text)?;
        ws.write_text_with_format(r, 4, &d.state, &text)?;
        ws.write_text_with_format(r, 5, &d.merged_into, &text)?;
        ws.write_number_with_format(r, 6, d.applies_to.len() as f64, &text)?;
        ws.write_text_with_format(r, 7, &d.inherited_from, &text)?;
        ws.write_text_with_format(r, 8, yes(d.has_reason), &text)?;
        ws.write_text_with_format(r, 9, &d.reason, &wrap)?;
        ws.write_text_with_format(r, 10, yes(d.revoked), &text)?;
    }
    ws.autofilter(0, 0, rows.len() as u32, (DECISION_HEADERS.len() - 1) as u16)?;
    ws.set_freeze_panes(1, 0)?;
    Ok(())
}

const PLAN_HEADERS: [&str; 12] = [
    "Group id",
    "Rule",
    "Cause",
    "Summary",
    "Severity",
    "Sites",
    "Fix",
    "Worked example",
    "Effort",
    "Top folders",
    "Sequence",
    "Reviewed",
];

/// The Remediation plan sheet: one row per sub-group with something left to fix, in the order to
/// fix them. A false positive or an ignored site is not counted in a row.
pub(super) fn write_plan_sheet(
    wb: &mut Workbook,
    plan: &[SubgroupFacts],
    fmts: &Formats,
) -> Result<(), XlsxError> {
    let mut ws = CappedSheet::new(wb.add_worksheet());
    ws.set_name(PLAN_SHEET)?;
    let _ = ws.set_tab_color(rust_xlsxwriter::Color::from("#2B5580"));
    for (c, h) in PLAN_HEADERS.iter().enumerate() {
        ws.write_text_with_format(0, c as u16, *h, &fmts.header)?;
    }
    ws.set_row_height(0, 28)?;
    let widths = [
        34.0, 40.0, 40.0, 60.0, 10.0, 8.0, 60.0, 50.0, 9.0, 44.0, 12.0, 40.0,
    ];
    for (c, w) in widths.iter().enumerate() {
        ws.set_column_width(c as u16, *w)?;
    }
    let text = fmts.cell(None, false, false);
    let wrap = fmts.cell(None, true, false);
    for (i, g) in plan.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_text_with_format(r, 0, &g.id, &text)?;
        ws.write_text_with_format(r, 1, format!("{} ({})", g.rule_title, g.rule_id), &wrap)?;
        ws.write_text_with_format(r, 2, &g.cause, &wrap)?;
        ws.write_text_with_format(r, 3, &g.summary, &wrap)?;
        ws.write_text_with_format(r, 4, &g.severity, &text)?;
        ws.write_number_with_format(r, 5, g.open.len() as f64, &text)?;
        ws.write_text_with_format(r, 6, &g.fix, &wrap)?;
        ws.write_text_with_format(r, 7, &g.worked_example, &wrap)?;
        ws.write_text_with_format(r, 8, &g.effort, &text)?;
        ws.write_text_with_format(r, 9, g.folders.join("\n"), &wrap)?;
        ws.write_text_with_format(r, 10, g.sequence.label(), &text)?;
        let reviewed: Vec<String> = g
            .review_levels
            .iter()
            .map(|(level, n)| {
                format!(
                    "{n} {}",
                    level.split(':').next().unwrap_or(level).replace('_', " ")
                )
            })
            .collect();
        ws.write_text_with_format(r, 11, reviewed.join(", "), &wrap)?;
    }
    ws.autofilter(0, 0, plan.len() as u32, (PLAN_HEADERS.len() - 1) as u16)?;
    ws.set_freeze_panes(1, 0)?;
    Ok(())
}
