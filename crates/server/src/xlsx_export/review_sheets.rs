//! The sheets for the people who review the report (WO9, design 3B): How problems were grouped
//! (the area, rule and fix groups with their summaries and counts) and Reviewer decisions (every
//! decision record the report was reviewed with). Both are renders of the one [`ReviewedReport`],
//! written through the shared table writer like every other sheet.

use std::collections::HashMap;

use super::sheets::RowCells;
use super::tables::{Cell, Col, Table};
use super::vocabulary as v;
use crate::report_export::decisions_file::DecisionRowJson;
use crate::report_export::tree_view::TreeNodeView;

/// Places under a node at a review level: the wire name, or every review-with-others for the
/// prefix.
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

/// What kind of group a node is, in the vocabulary's words.
fn level_word(level: &str) -> &'static str {
    match level {
        "category" => v::LEVEL_AREA,
        "rule" => v::LEVEL_RULE,
        "subgroup" => v::LEVEL_RULE_GROUP,
        _ => v::LEVEL_FIX,
    }
}

fn yes(b: bool) -> Cell {
    if b {
        Cell::text(v::YES)
    } else {
        Cell::Empty
    }
}

/// The How problems were grouped sheet. The plain columns name each group by its label and its
/// parent by label; the ids sit in the technical band.
pub(super) fn grouped_table(nodes: &[TreeNodeView], internal: bool) -> Table {
    let mut cols = vec![
        Col::plain(v::H_GROUP, 44.0),
        Col::plain(v::H_WITHIN, 10.0),
        Col::plain(v::H_UNDER, 44.0),
        Col::plain(v::H_CAUSE, 40.0),
        Col::plain(v::H_SUMMARY, 70.0),
        Col::plain(v::H_PLACES, 8.0),
        Col::plain(v::H_RULED_OUT_COUNT, 10.0),
        Col::plain(v::H_REVIEWED_IN_PERSON, 12.0),
        Col::plain(v::H_REVIEWED_WITH_OTHERS, 14.0),
        Col::plain(v::H_ENGINE_VERIFIED, 12.0),
        Col::technical(v::H_GROUP_ID, 34.0),
        Col::technical(v::H_PARENT_ID, 34.0),
        Col::technical(v::H_FIX_ID, 20.0),
    ];
    if internal {
        for h in [
            "Summary source",
            v::H_ESCALATED,
            "Below the line",
            "Outliers",
            "Unplaced",
            v::H_REVIEW_FLAGS,
        ] {
            cols.push(Col::technical(h, 14.0));
        }
    }
    let mut table = Table::new(v::HOW_PROBLEMS_WERE_GROUPED, cols);
    let label_of: HashMap<&str, &str> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect();
    for n in nodes {
        let mut c = RowCells::default();
        c.put(v::H_GROUP, Cell::wrap(&n.label))
            .put(v::H_WITHIN, Cell::text(level_word(&n.level)))
            .put(
                v::H_UNDER,
                Cell::wrap(label_of.get(n.parent.as_str()).copied().unwrap_or("")),
            )
            .put(v::H_CAUSE, Cell::wrap(&n.cause))
            .put(v::H_SUMMARY, Cell::wrap(&n.summary))
            .put(v::H_PLACES, Cell::Num(n.sites as f64))
            .put(v::H_RULED_OUT_COUNT, Cell::Num(n.false_positives as f64))
            .put(
                v::H_REVIEWED_IN_PERSON,
                Cell::Num(level_count(n, "personally_reviewed") as f64),
            )
            .put(
                v::H_REVIEWED_WITH_OTHERS,
                Cell::Num(level_count(n, "reviewed_as_pattern:") as f64),
            )
            .put(
                v::H_ENGINE_VERIFIED,
                Cell::Num(level_count(n, "engine_verified") as f64),
            )
            .put(v::H_GROUP_ID, Cell::text(&n.id))
            .put(v::H_PARENT_ID, Cell::text(&n.parent))
            .put(v::H_FIX_ID, Cell::text(&n.fix_id));
        if let Some(t) = &n.internal {
            c.put("Summary source", Cell::text(&t.summary_source))
                .put(v::H_ESCALATED, Cell::Num(t.escalated as f64))
                .put("Below the line", Cell::Num(t.below_the_line as f64))
                .put("Outliers", Cell::Num(t.outliers as f64))
                .put("Unplaced", Cell::Num(t.unplaced as f64))
                .put(v::H_REVIEW_FLAGS, Cell::wrap(t.flags.join("; ")));
        }
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note = Some("This report has no review tree.".into());
    table
}

/// The Reviewer decisions sheet.
pub(super) fn decisions_table(rows: &[DecisionRowJson]) -> Table {
    let mut table = Table::new(
        v::REVIEWER_DECISIONS,
        vec![
            Col::plain(v::H_DECISION_ID, 16.0),
            Col::plain(v::H_SCOPE, 8.0),
            Col::plain(v::H_STATE, 16.0),
            Col::plain(v::H_PLACES_COVERED, 10.0),
            Col::plain(v::H_REASON_RECORDED, 10.0),
            Col::plain(v::H_REASON, 50.0),
            Col::plain(v::H_REVOKED, 9.0),
            Col::technical(v::H_PLACE_ID, 18.0),
            Col::technical(v::H_GROUP_ID, 34.0),
            Col::technical(v::H_MERGED_INTO, 18.0),
            Col::technical(v::H_INHERITED_FROM, 16.0),
        ],
    );
    for d in rows {
        let mut c = RowCells::default();
        c.put(v::H_DECISION_ID, Cell::text(&d.id))
            .put(
                v::H_SCOPE,
                Cell::text(if d.scope == "node" { "Group" } else { "Place" }),
            )
            .put(v::H_STATE, Cell::text(&d.state))
            .put(v::H_PLACES_COVERED, Cell::Num(d.applies_to.len() as f64))
            .put(v::H_REASON_RECORDED, yes(d.has_reason))
            .put(v::H_REASON, Cell::wrap(&d.reason))
            .put(v::H_REVOKED, yes(d.revoked))
            .put(v::H_PLACE_ID, Cell::text(&d.site_id))
            .put(v::H_GROUP_ID, Cell::text(&d.node_id))
            .put(v::H_MERGED_INTO, Cell::text(&d.merged_into))
            .put(v::H_INHERITED_FROM, Cell::text(&d.inherited_from));
        table.rows.push(c.assemble(&table.cols));
    }
    table.empty_note = Some("No reviewer decisions were recorded.".into());
    table
}
