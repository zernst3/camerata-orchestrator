//! The export-blocked line and the rows behind it (spec section 10, work order E).
//!
//! The panel mirrors the release gate: it counts what [`ExportGateView`] lists, which the server
//! derives from the gate's own refusal functions, and adds nothing of its own. The held rows are
//! marked in the triage table itself (red status cell "Needs a decision", the reason as its
//! title); the panel keeps one line and a button. A row the table does not hold is only in Review,
//! and the line says so and offers a door there.

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
    /// The row's site key (what the triage table's rows are keyed by); empty for a group decision.
    pub key: String,
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
    for (r, kind) in gate.held.iter().map(|r| (r, GateRowKind::Held)).chain(
        gate.merged_without_decision
            .iter()
            .map(|r| (r, GateRowKind::Merged)),
    ) {
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
                key: first.key.clone(),
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
        key: String::new(),
        place: g.node_id.clone(),
        rule_id: String::new(),
        reason: format!(
            "the decision has no record for {} {}",
            thousands(g.missing.len()),
            if g.missing.len() == 1 {
                "place"
            } else {
                "places"
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

/// The site key of every row that needs a decision, once each, in the gate's order. The triage
/// table marks exactly these rows.
pub fn held_keys(gate: &ExportGateView) -> Vec<String> {
    held_rows(gate).into_iter().map(|r| r.key).collect()
}

/// Why each held row is held, by site key: the words the table shows as the status cell's title.
pub fn held_reasons(gate: &ExportGateView) -> std::collections::HashMap<String, String> {
    held_rows(gate)
        .into_iter()
        .map(|r| (r.key, r.reason))
        .collect()
}

/// The panel's one line and where its buttons go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedLine {
    /// `Export blocked: 8 rows need a decision (3 are in Review).`
    pub text: String,
    /// How many held rows the triage table holds: what "Show them" filters to.
    pub in_table: usize,
    /// How many held rows are only in Review, plus group decisions that miss records.
    pub in_review: usize,
    /// Where "Open Review" goes: the first thing the table does not hold. `None` when the table
    /// holds every row.
    pub review_door: Option<Door>,
}

/// The sentence about rules split with no decision (Zach's ruling on A3): the count and the rule
/// ids, and the two ways to decide. Empty when none is held. The set is the gate's own
/// (`split_rules_without_choice` on the server), never recomputed here.
fn split_rules_sentence(split: &[camerata_api_types::review_view::SplitRuleHold]) -> String {
    if split.is_empty() {
        return String::new();
    }
    // One entry for each (repo, rule), whatever order they come in. With several repos the repo
    // is named, since the same rule can be split in more than one and the count is per repo.
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for s in split {
        let entry = (s.repo.as_str(), s.rule_id.as_str());
        if !seen.contains(&entry) {
            seen.push(entry);
        }
    }
    let mut repos: Vec<&str> = seen.iter().map(|(r, _)| *r).collect();
    repos.sort_unstable();
    repos.dedup();
    let ids: Vec<String> = seen
        .iter()
        .map(|(repo, rule)| {
            if repos.len() > 1 {
                format!("{rule} in {repo}")
            } else {
                (*rule).to_string()
            }
        })
        .collect();
    let n = seen.len();
    let (noun, verb, each) = if n == 1 {
        ("split rule", "needs", "for it")
    } else {
        ("split rules", "need", "for each")
    };
    format!(
        " {n} {noun} {verb} a decision: {}. Accept an option {each}, or leave it to the client.",
        ids.join(", ")
    )
}

/// The panel's line, or `None` when the gate is open and the export button stays. `table_keys` is
/// the site keys the triage table holds (`None`: no table is on screen, so every row is in
/// Review). The count and the set of held rows are the gate's alone; the table only says which of
/// them it can show.
pub fn blocked_line(
    gate: &ExportGateView,
    table_keys: Option<&std::collections::HashSet<String>>,
) -> Option<BlockedLine> {
    if !gate.is_blocked() {
        return None;
    }
    let rows = gate.rows_needing_a_decision();
    let held = held_rows(gate);
    let in_table = held
        .iter()
        .filter(|r| table_keys.is_some_and(|keys| keys.contains(&r.key)))
        .count();
    let in_review_rows = rows.saturating_sub(in_table);
    let gap_sites: usize = gate
        .node_decisions_missing_members
        .iter()
        .map(|g| g.missing.len())
        .sum();
    let review_door = held
        .iter()
        .find(|r| !table_keys.is_some_and(|keys| keys.contains(&r.key)))
        .map(|r| r.door.clone())
        .or_else(|| {
            gate.node_decisions_missing_members
                .first()
                .map(|g| gap_row(g).door)
        });
    let mut text = crate::export_panel::blocked_line(
        rows,
        in_review_rows,
        gate.node_decisions_missing_members.len(),
        gap_sites,
    );
    text.push_str(&split_rules_sentence(&gate.split_rules));
    Some(BlockedLine {
        text,
        in_table,
        in_review: in_review_rows + gate.node_decisions_missing_members.len(),
        review_door,
    })
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
            both.reason
                .contains(HoldReason::RestoredWithoutDecision.phrase()),
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
    fn the_line_counts_the_rows_that_need_a_decision() {
        let gate = &data().gate;
        let line = blocked_line(gate, None).unwrap();
        assert!(
            line.text.starts_with(&format!(
                "Export blocked: {} held rows need a decision",
                gate.rows_needing_a_decision()
            )),
            "{}",
            line.text
        );
        assert!(line.text.contains("group decision"), "{}", line.text);
        assert!(!line.text.contains('\u{2014}'));
    }

    #[test]
    fn rows_the_table_holds_are_counted_and_the_rest_are_in_review_with_a_door() {
        let gate = &data().gate;
        let keys = held_keys(gate);
        assert!(keys.len() >= 2, "the base fixture holds several rows");
        let table: std::collections::HashSet<String> =
            keys.iter().take(keys.len() - 1).cloned().collect();
        let line = blocked_line(gate, Some(&table)).unwrap();
        assert_eq!(line.in_table, keys.len() - 1);
        assert!(line.text.contains("(1 is in Review)"), "{}", line.text);
        let door = line
            .review_door
            .expect("a door to the row only Review holds");
        let last = keys.last().unwrap();
        assert!(
            gate_rows(gate)
                .iter()
                .any(|r| &r.key == last && r.door == door),
            "the door goes to the row the table does not hold"
        );
        // The table holds every row: no door, nothing is in Review but the group decisions.
        let all: std::collections::HashSet<String> = keys.iter().cloned().collect();
        let line = blocked_line(gate, Some(&all)).unwrap();
        assert_eq!(line.in_table, keys.len());
        assert!(!line.text.contains("in Review)"), "{}", line.text);
    }

    #[test]
    fn with_no_table_every_row_is_in_review() {
        let gate = &data().gate;
        let line = blocked_line(gate, None).unwrap();
        assert_eq!(line.in_table, 0);
        assert!(line.text.contains("(all are in Review)"), "{}", line.text);
        assert!(line.review_door.is_some());
    }

    #[test]
    fn the_held_keys_and_reasons_are_the_gates_rows_once_each() {
        let gate = &data().gate;
        let keys = held_keys(gate);
        let distinct: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(keys.len(), distinct.len(), "once each");
        let held_only = gate.rows_needing_a_decision();
        assert_eq!(keys.len(), held_only);
        let reasons = held_reasons(gate);
        assert_eq!(reasons.len(), keys.len());
        assert!(reasons.values().all(|r| !r.is_empty()));
    }

    #[test]
    fn a_clear_gate_has_no_line_and_one_row_reads_in_the_singular() {
        assert_eq!(blocked_line(&ExportGateView::default(), None), None);
        assert!(gate_rows(&ExportGateView::default()).is_empty());
        let mut one = ExportGateView::default();
        one.held.push(data().gate.held[0].clone());
        let table: std::collections::HashSet<String> = held_keys(&one).into_iter().collect();
        assert_eq!(
            blocked_line(&one, Some(&table)).unwrap().text,
            "Export blocked: 1 held row needs a decision."
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

    /// Zach's ruling on amendment A3 (2026-11-20): the export dialog shows how many rules are split
    /// with no decision and names them, and the gate is blocked by them alone.
    #[test]
    fn the_line_counts_and_names_the_split_rules_with_no_decision() {
        use camerata_api_types::review_view::SplitRuleHold;
        let split = |rule: &str| SplitRuleHold {
            repo: "acme/app".into(),
            rule_id: rule.into(),
        };
        let mut gate = ExportGateView {
            split_rules: vec![split("ARCH-A-1"), split("ARCH-B-1")],
            ..ExportGateView::default()
        };
        let line = blocked_line(&gate, None).expect("split rules block the export");
        assert_eq!(
            line.text,
            "Export blocked: 2 split rules need a decision: ARCH-A-1, ARCH-B-1. Accept an option for each, or leave it to the client."
        );
        gate.split_rules.truncate(1);
        assert_eq!(
            blocked_line(&gate, None).unwrap().text,
            "Export blocked: 1 split rule needs a decision: ARCH-A-1. Accept an option for it, or leave it to the client."
        );
        // Beside held rows the sentence is added to the line.
        gate.held.push(data().gate.held[0].clone());
        let text = blocked_line(&gate, None).unwrap().text;
        assert!(
            text.contains("held row") && text.ends_with("or leave it to the client."),
            "{text}"
        );
    }

    /// Review item 13: the same rule split in two repos is two splits, named by repo; a repeated
    /// entry is counted once, whatever its place in the list.
    #[test]
    fn the_split_sentence_names_the_repo_when_there_are_several_and_counts_each_once() {
        use camerata_api_types::review_view::SplitRuleHold;
        let split = |repo: &str, rule: &str| SplitRuleHold {
            repo: repo.into(),
            rule_id: rule.into(),
        };
        let gate = ExportGateView {
            split_rules: vec![
                split("acme/a", "ARCH-X-1"),
                split("acme/b", "ARCH-X-1"),
                split("acme/a", "ARCH-X-1"),
            ],
            ..ExportGateView::default()
        };
        let text = blocked_line(&gate, None).unwrap().text;
        assert!(
            text.contains("2 split rules need a decision: ARCH-X-1 in acme/a, ARCH-X-1 in acme/b."),
            "{text}"
        );
        // One repo: the ids alone, each once.
        let gate = ExportGateView {
            split_rules: vec![split("acme/a", "ARCH-A-1"), split("acme/a", "ARCH-B-1")],
            ..ExportGateView::default()
        };
        let text = blocked_line(&gate, None).unwrap().text;
        assert!(
            text.contains("2 split rules need a decision: ARCH-A-1, ARCH-B-1."),
            "{text}"
        );
    }
}
