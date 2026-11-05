//! The export-blocked panel's rows (spec section 10).
//!
//! The panel mirrors the release gate: it lists what [`ExportGateView`] lists, which the server
//! derives from the gate's own refusal functions, and adds nothing of its own. Each row has a door
//! to the thing it names.

use camerata_api_types::review_view::{ExportGateView, HeldRow, HoldReason, NodeDecisionGap};

use super::shell::Lane;
use super::thousands;

/// Where a row's `open` button goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// Focus this site in the tree lane. `node_id` is the sub-group the row sits in, and the
    /// sites route reads that node. The rule id is the fallback for a row with no place of its
    /// own (`rule:{rule_id}`), and is right only for a native rule.
    Site {
        key: String,
        rule_id: String,
        node_id: String,
    },
    /// Open this node in the tree lane.
    Node { id: String },
    /// Focus this site's card in the Engine Check lane (spec section 14, step 10). The row is
    /// held on the Engine Check's account, so its card is where it is corrected.
    EngineCard { key: String },
}

impl Door {
    /// The lane the door opens in. The shell switches to it and that lane's host takes the
    /// request; the tree takes none that is not its own.
    pub fn lane(&self) -> Lane {
        match self {
            Self::Site { .. } | Self::Node { .. } => Lane::Tree,
            Self::EngineCard { .. } => Lane::EngineCheck,
        }
    }
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

/// The words for why one row is held: the server's extra words when it gave some, else the reason.
fn reason_words(r: &HeldRow) -> String {
    if r.detail.is_empty() {
        r.reason.phrase().to_string()
    } else {
        r.detail.clone()
    }
}

/// Where a row's `open` button goes. A row more than one check refuses goes where the check that
/// runs first sends it: a row put back after a throw-out is decided in its node, and that one
/// decision also clears an Engine Check hold on it, so its door is the row, not the card.
fn door_for(rows: &[&HeldRow]) -> Door {
    let first = rows
        .iter()
        .min_by_key(|r| {
            HoldReason::ALL
                .iter()
                .position(|x| *x == r.reason)
                .unwrap_or(usize::MAX)
        })
        .copied()
        .unwrap_or(rows[0]);
    if first.reason == HoldReason::EngineCheckNoWithoutCorrection {
        Door::EngineCard {
            key: first.key.clone(),
        }
    } else {
        Door::Site {
            key: first.key.clone(),
            rule_id: first.rule_id.clone(),
            node_id: first.node_id.clone(),
        }
    }
}

/// One line per row the gate holds. A row two checks refuse is one line that names each reason
/// (a restored row the Engine Check also said No to), so the lines and the headline's count of
/// rows are the same number.
fn held_rows(gate: &ExportGateView) -> Vec<GateRow> {
    let mut order: Vec<&str> = Vec::new();
    let mut by_key: std::collections::HashMap<&str, Vec<(&HeldRow, GateRowKind)>> =
        std::collections::HashMap::new();
    for (r, kind) in gate
        .held
        .iter()
        .map(|r| (r, GateRowKind::Held))
        .chain(
            gate.merged_without_decision
                .iter()
                .map(|r| (r, GateRowKind::Merged)),
        )
    {
        let entry = by_key.entry(r.key.as_str()).or_default();
        if entry.is_empty() {
            order.push(r.key.as_str());
        }
        entry.push((r, kind));
    }
    order
        .into_iter()
        .map(|key| {
            let group = &by_key[key];
            let rows: Vec<&HeldRow> = group.iter().map(|(r, _)| *r).collect();
            let mut reasons: Vec<String> = Vec::new();
            for r in &rows {
                let words = reason_words(r);
                if !reasons.contains(&words) {
                    reasons.push(words);
                }
            }
            let first = rows[0];
            GateRow {
                kind: group[0].1,
                place: format!("{}:{}", first.path, first.line),
                rule_id: first.rule_id.clone(),
                reason: reasons.join("; "),
                door: door_for(&rows),
            }
        })
        .collect()
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

/// Every line of the panel: the held rows (merges with no decision among them), then the group
/// decisions that miss records. One line per row the gate refuses, however many checks refuse it,
/// none added or left out.
pub fn gate_rows(gate: &ExportGateView) -> Vec<GateRow> {
    held_rows(gate)
        .into_iter()
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
        let distinct_rows = gate
            .held
            .iter()
            .chain(&gate.merged_without_decision)
            .map(|h| h.key.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert_eq!(
            rows.len(),
            distinct_rows + gate.node_decisions_missing_members.len(),
            "one line per row the gate refuses (a row two checks refuse is one line), none added or dropped"
        );
        // Every held row has a door to its own place: its site in the tree, or, for a row the
        // Engine Check holds, its card in the Engine Check lane (spec section 14, step 10).
        for h in gate.held.iter().chain(&gate.merged_without_decision) {
            let wanted = if h.reason == HoldReason::EngineCheckNoWithoutCorrection {
                Door::EngineCard { key: h.key.clone() }
            } else {
                Door::Site {
                    key: h.key.clone(),
                    rule_id: h.rule_id.clone(),
                    node_id: h.node_id.clone(),
                }
            };
            let row = rows
                .iter()
                .find(|r| r.door == wanted)
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

    /// Wave 17 soft (a): a restored row the Engine Check also holds drew two lines under a headline
    /// that counts it once. The panel now draws one line per row, naming each reason, so the
    /// headline and the lines count the same thing.
    #[test]
    fn a_row_two_checks_refuse_is_one_line_naming_both_reasons() {
        let base = data().gate.held[0].clone();
        let restored = HeldRow {
            reason: HoldReason::RestoredWithoutDecision,
            detail: String::new(),
            ..base.clone()
        };
        let engine = HeldRow {
            reason: HoldReason::EngineCheckNoWithoutCorrection,
            detail: String::new(),
            ..base.clone()
        };
        let mut other = base.clone();
        other.key = format!("{}-other", base.key);
        other.reason = HoldReason::EvidenceUnverified;
        other.detail = String::new();
        let mut gate = ExportGateView::default();
        // The Engine Check's hold is listed before the restore's, to show the order is the gate's
        // own and not the list's.
        gate.held.extend([engine, restored, other]);
        let rows = gate_rows(&gate);
        assert_eq!(rows.len(), 2, "two rows, not three lines: {rows:?}");
        assert_eq!(
            rows.len(),
            gate.rows_needing_a_decision(),
            "the lines and the headline count the same rows"
        );
        let both = &rows[0];
        assert!(
            both.reason.contains(HoldReason::RestoredWithoutDecision.phrase()),
            "{}",
            both.reason
        );
        assert!(
            both.reason
                .contains(HoldReason::EngineCheckNoWithoutCorrection.phrase()),
            "{}",
            both.reason
        );
        // A decision on the put-back row clears both holds, so the door is the row in its node.
        assert_eq!(
            both.door,
            Door::Site {
                key: base.key.clone(),
                rule_id: base.rule_id.clone(),
                node_id: base.node_id.clone()
            }
        );
        assert!(!both.reason.contains('\u{2014}'));
        // A merge listing and a hold on the same key are one row too.
        let mut gate = ExportGateView::default();
        gate.held.push(HeldRow {
            reason: HoldReason::EvidenceUnverified,
            detail: String::new(),
            ..base.clone()
        });
        gate.merged_without_decision.push(HeldRow {
            reason: HoldReason::MergedWithoutDecision,
            detail: String::new(),
            ..base
        });
        assert_eq!(gate_rows(&gate).len(), gate.rows_needing_a_decision());
        assert_eq!(gate_rows(&gate).len(), 1);
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
