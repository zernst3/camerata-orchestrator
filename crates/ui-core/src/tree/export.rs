//! The export-blocked panel's rows (spec section 10).
//!
//! The panel mirrors the release gate: it lists what [`ExportGateView`] lists, which the server
//! derives from the gate's own refusal functions, and adds nothing of its own. Each row has a door
//! to the thing it names.

use camerata_api_types::review_view::{ExportGateView, HeldRow, NodeDecisionGap};

use super::thousands;

/// Where a row's `open` button goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// Focus this site in the tree lane.
    Site { key: String },
    /// Open this node in the tree lane.
    Node { id: String },
}

/// What kind of line a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRowKind {
    /// A site the gate holds (`held` chip).
    Held,
    /// A merged site with no decision record behind it.
    Merged,
    /// A group decision missing records for some sites it covers.
    Gap,
}

impl GateRowKind {
    /// The word at the start of the line.
    pub fn word(self) -> &'static str {
        match self {
            Self::Held | Self::Merged => "held",
            Self::Gap => "group",
        }
    }
}

/// One line of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRow {
    pub kind: GateRowKind,
    /// `web/env.ts:3`, or the node id for a group decision.
    pub place: String,
    pub rule_id: String,
    /// Why, in words.
    pub reason: String,
    pub door: Door,
}

fn held_row(kind: GateRowKind, r: &HeldRow) -> GateRow {
    GateRow {
        kind,
        place: format!("{}:{}", r.path, r.line),
        rule_id: r.rule_id.clone(),
        reason: if r.detail.is_empty() {
            r.reason.phrase().to_string()
        } else {
            r.detail.clone()
        },
        door: Door::Site { key: r.key.clone() },
    }
}

fn gap_row(g: &NodeDecisionGap) -> GateRow {
    GateRow {
        kind: GateRowKind::Gap,
        place: g.node_id.clone(),
        rule_id: String::new(),
        reason: format!(
            "the decision has no record for {} {}",
            thousands(g.missing.len()),
            if g.missing.len() == 1 {
                "site"
            } else {
                "sites"
            }
        ),
        door: Door::Node {
            id: g.node_id.clone(),
        },
    }
}

/// Every line of the panel: the held rows, the merges with no decision, then the group decisions
/// that miss records. One line per thing the gate refuses, none added or left out.
pub fn gate_rows(gate: &ExportGateView) -> Vec<GateRow> {
    gate.held
        .iter()
        .map(|r| held_row(GateRowKind::Held, r))
        .chain(
            gate.merged_without_decision
                .iter()
                .map(|r| held_row(GateRowKind::Merged, r)),
        )
        .chain(gate.node_decisions_missing_members.iter().map(gap_row))
        .collect()
}

/// The panel's first line, or `None` when the gate is open and the export button stays.
pub fn blocked_headline(gate: &ExportGateView) -> Option<String> {
    if !gate.is_blocked() {
        return None;
    }
    let rows = gate.rows_needing_a_decision();
    let gap_sites: usize = gate
        .node_decisions_missing_members
        .iter()
        .map(|g| g.missing.len())
        .sum();
    let mut line = String::from("Export blocked:");
    if rows > 0 {
        line.push_str(&format!(
            " {} {} a decision before the package can be built.",
            thousands(rows),
            if rows == 1 { "row needs" } else { "rows need" }
        ));
    }
    if gap_sites > 0 {
        let groups = gate.node_decisions_missing_members.len();
        line.push_str(&format!(
            " {} group {} missing records for {} {}.",
            thousands(groups),
            if groups == 1 {
                "decision is"
            } else {
                "decisions are"
            },
            thousands(gap_sites),
            if gap_sites == 1 { "site" } else { "sites" }
        ));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::tests::data;

    #[test]
    fn export_panel_lists_each_held_row_with_a_door() {
        let gate = &data().gate;
        assert!(gate.is_blocked(), "the base fixture is blocked");
        let rows = gate_rows(gate);
        assert_eq!(
            rows.len(),
            gate.held.len()
                + gate.merged_without_decision.len()
                + gate.node_decisions_missing_members.len(),
            "one line per thing the gate refuses, none added or dropped"
        );
        // Every held row has a door to its own site.
        for h in gate.held.iter().chain(&gate.merged_without_decision) {
            let row = rows
                .iter()
                .find(|r| r.door == Door::Site { key: h.key.clone() })
                .unwrap_or_else(|| panic!("no line with a door to {}", h.path));
            assert_eq!(row.place, format!("{}:{}", h.path, h.line));
            assert_eq!(row.rule_id, h.rule_id);
            assert!(!row.reason.is_empty());
        }
        // A group decision that misses records has a door to its node.
        for g in &gate.node_decisions_missing_members {
            assert!(rows.iter().any(|r| r.kind == GateRowKind::Gap
                && r.door
                    == Door::Node {
                        id: g.node_id.clone()
                    }));
        }
        // Held lines first, then merges, then gaps.
        let kinds: Vec<GateRowKind> = rows.iter().map(|r| r.kind).collect();
        let last_held = kinds.iter().rposition(|k| *k == GateRowKind::Held);
        let first_gap = kinds.iter().position(|k| *k == GateRowKind::Gap);
        if let (Some(h), Some(g)) = (last_held, first_gap) {
            assert!(h < g);
        }
    }

    #[test]
    fn the_headline_counts_the_rows_that_need_a_decision() {
        let gate = &data().gate;
        let line = blocked_headline(gate).unwrap();
        assert!(
            line.starts_with(&format!(
                "Export blocked: {} rows need a decision before the package can be built.",
                gate.rows_needing_a_decision()
            )),
            "{line}"
        );
        assert!(line.contains("group decision"), "{line}");
        assert!(!line.contains('\u{2014}'));
    }

    #[test]
    fn a_clear_gate_has_no_panel_and_one_row_reads_in_the_singular() {
        assert_eq!(blocked_headline(&ExportGateView::default()), None);
        assert!(gate_rows(&ExportGateView::default()).is_empty());
        let mut one = ExportGateView::default();
        one.held.push(data().gate.held[0].clone());
        assert_eq!(
            blocked_headline(&one).unwrap(),
            "Export blocked: 1 row needs a decision before the package can be built."
        );
    }

    #[test]
    fn a_held_line_says_the_reason_in_words_when_the_row_has_no_extra_detail() {
        let mut h = data().gate.held[0].clone();
        h.detail = String::new();
        let mut gate = ExportGateView::default();
        gate.held.push(h.clone());
        let rows = gate_rows(&gate);
        assert_eq!(rows[0].reason, h.reason.phrase());
        assert_eq!(rows[0].kind.word(), "held");
    }
}
