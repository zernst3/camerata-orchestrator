//! A batch decision at a node, and the summary edit (spec section 5; design U3 and U4).
//!
//! A batch decision writes one decision with node scope. The server writes the node record and
//! one per-site record for each member it covers. What the decision leaves out is decided here,
//! from the sites of the node: held rows are always left out (U3), outliers are left out unless
//! the reviewer ticked them, and a member that already holds this decision is left as it is.

use std::collections::BTreeSet;

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{EditField, NewEditReq};
use camerata_api_types::review_view::{SiteRow, TreeNodeView};

use crate::triage::{TriageState, UNDO_WINDOW_MS};

use super::{is_outlier, thousands};

/// The decision a triage state stands for at a node. Tech debt at a node is the "later" bucket,
/// the Queue's default.
pub fn decision_state_for(state: TriageState) -> Option<DecisionState> {
    match state {
        TriageState::Confirmed => Some(DecisionState::Confirmed),
        TriageState::Ignored => Some(DecisionState::Ignored),
        TriageState::FalsePositive => Some(DecisionState::FalsePositive),
        TriageState::TechDebt => Some(DecisionState::TechDebtLater),
        // A merge needs a second site and has no node form; Unresolved is not a decision.
        TriageState::MergedInto | TriageState::Unresolved => None,
    }
}

/// What a batch decision on a node will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    pub node: String,
    pub state: DecisionState,
    /// The sites the decision will write a record for.
    pub keys: Vec<String>,
    /// Outliers the reviewer did not tick: left out.
    pub excluded_outliers: Vec<String>,
    /// Held rows: always left out, because the gate needs a ruling on each by name.
    pub excluded_held: Vec<String>,
    /// Members that already hold this decision: left as they are.
    pub already_recorded: Vec<String>,
    /// Outliers the reviewer ticked, so they are in `keys`.
    pub ticked_included: usize,
}

impl BatchPlan {
    /// The keys the request lists as left out.
    pub fn except(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .excluded_outliers
            .iter()
            .chain(&self.excluded_held)
            .chain(&self.already_recorded)
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Nothing would be written.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// The plan for deciding `state` at `node` over `rows` (every site of the node, not only the
/// ones on screen), with the outliers the reviewer ticked.
pub fn batch_plan(
    node: &str,
    state: &DecisionState,
    rows: &[SiteRow],
    ticked: &BTreeSet<String>,
) -> BatchPlan {
    let mut plan = BatchPlan {
        node: node.to_string(),
        state: state.clone(),
        keys: Vec::new(),
        excluded_outliers: Vec::new(),
        excluded_held: Vec::new(),
        already_recorded: Vec::new(),
        ticked_included: 0,
    };
    for row in rows {
        if row.held.is_some() {
            plan.excluded_held.push(row.site_key.clone());
        } else if is_outlier(row) && !ticked.contains(&row.site_key) {
            plan.excluded_outliers.push(row.site_key.clone());
        } else if row.decision.as_ref().is_some_and(|d| d.state == *state) {
            plan.already_recorded.push(row.site_key.clone());
        } else {
            if is_outlier(row) {
                plan.ticked_included += 1;
            }
            plan.keys.push(row.site_key.clone());
        }
    }
    plan
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// The sentence the prompt shows: `This writes 12 site records. 2 outliers are excluded unless
/// you tick them. 1 held row is excluded.` Clauses with nothing to say are left out.
pub fn plan_sentence(plan: &BatchPlan) -> String {
    let n = plan.keys.len();
    let mut s = format!(
        "This writes {} site {}.",
        thousands(n),
        plural(n, "record", "records")
    );
    let o = plan.excluded_outliers.len();
    if o > 0 {
        s.push_str(&format!(
            " {} {} excluded unless you tick {}.",
            thousands(o),
            plural(o, "outlier is", "outliers are"),
            plural(o, "it", "them")
        ));
    }
    if plan.ticked_included > 0 {
        s.push_str(&format!(
            " {} ticked {} included.",
            thousands(plan.ticked_included),
            plural(plan.ticked_included, "outlier is", "outliers are")
        ));
    }
    let h = plan.excluded_held.len();
    if h > 0 {
        s.push_str(&format!(
            " {} held {} excluded.",
            thousands(h),
            plural(h, "row is", "rows are")
        ));
    }
    let a = plan.already_recorded.len();
    if a > 0 {
        s.push_str(&format!(
            " {} already {} this decision and {} left as {}.",
            thousands(a),
            plural(a, "holds", "hold"),
            plural(a, "is", "are"),
            plural(a, "it is", "they are")
        ));
    }
    s
}

/// The heading above the reason field: `Reason for "Confirmed" on 3 of 8 sites`.
pub fn batch_heading(state: TriageState, plan: &BatchPlan, node_label: &str) -> String {
    format!(
        "Reason for \u{201c}{}\u{201d} on {} (required)",
        state.label(),
        node_label_or(node_label, plan)
    )
}

fn node_label_or(label: &str, plan: &BatchPlan) -> String {
    if label.is_empty() {
        plan.node.clone()
    } else {
        label.to_string()
    }
}

/// What a batch decision cannot be made on: a node of rows the engine threw out (putting one
/// back is a different act) and a node with no sites.
pub fn batch_allowed(node: &TreeNodeView) -> bool {
    !node.thrown_out && node.counts.sites > 0
}

/// Why a batch decision cannot be made, in words, for the buttons' tooltip.
pub fn batch_blocked_reason(node: &TreeNodeView) -> Option<&'static str> {
    if node.thrown_out {
        Some("These rows were thrown out by the engine. Put one back from the Engine Check; a batch decision does not apply.")
    } else if node.counts.sites == 0 {
        Some("This node has no sites.")
    } else {
        None
    }
}

/// The request that records the batch decision. `reason` must not be blank (the server refuses a
/// group decision without one, Confirm included).
pub fn decision_request(
    plan: &BatchPlan,
    reason: &str,
    run_id: &str,
) -> Result<NewDecisionReq, BatchError> {
    if reason.trim().is_empty() {
        return Err(BatchError::ReasonRequired);
    }
    if plan.is_empty() {
        return Err(BatchError::NothingToWrite);
    }
    Ok(NewDecisionReq {
        scope: Scope::Node(plan.node.clone()),
        key: String::new(),
        state: plan.state.clone(),
        reason: reason.trim().to_string(),
        decided_by: String::new(),
        needed_me: None,
        sample_verdict: None,
        except: plan.except(),
        run_id: run_id.to_string(),
        decided_at: String::new(),
    })
}

/// The request that records one site's decision from the keyboard or the pane.
pub fn site_decision_request(
    key: &str,
    state: DecisionState,
    reason: &str,
    run_id: &str,
) -> NewDecisionReq {
    NewDecisionReq {
        scope: Scope::Site,
        key: key.to_string(),
        state,
        reason: reason.trim().to_string(),
        decided_by: String::new(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: run_id.to_string(),
        decided_at: String::new(),
    }
}

/// Why a request was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchError {
    ReasonRequired,
    NothingToWrite,
}

impl BatchError {
    pub fn message(self) -> &'static str {
        match self {
            Self::ReasonRequired => "A group decision needs a reason.",
            Self::NothingToWrite => {
                "Nothing to write: every site here is held, an outlier you did not tick, or already recorded."
            }
        }
    }
}

// ── The undo bar ──────────────────────────────────────────────────────────────────────────

/// A decision the reviewer can still take back: the record to revoke and the words on the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeUndo {
    /// The decision record to revoke (the node record revokes its per-site records too).
    pub decision_id: String,
    pub message: String,
    pub armed_at_ms: u64,
}

impl TreeUndo {
    /// The bar after a batch decision: `12 rows confirmed. Undo`.
    pub fn for_batch(decision_id: &str, count: usize, state: TriageState, now_ms: u64) -> Self {
        Self {
            decision_id: decision_id.to_string(),
            message: format!(
                "{} {} {}.",
                thousands(count),
                plural(count, "row", "rows"),
                past_tense(state)
            ),
            armed_at_ms: now_ms,
        }
    }

    /// The bar after one site's decision.
    pub fn for_site(decision_id: &str, state: TriageState, now_ms: u64) -> Self {
        Self {
            decision_id: decision_id.to_string(),
            message: format!("1 row {}.", past_tense(state)),
            armed_at_ms: now_ms,
        }
    }

    /// Whole seconds left in the five second window at `now_ms`, or `None` once it has closed.
    pub fn seconds_left(&self, now_ms: u64) -> Option<u64> {
        let spent = now_ms.saturating_sub(self.armed_at_ms);
        (spent < UNDO_WINDOW_MS).then(|| (UNDO_WINDOW_MS - spent).div_ceil(1000))
    }
}

fn past_tense(state: TriageState) -> &'static str {
    match state {
        TriageState::Confirmed => "confirmed",
        TriageState::Ignored => "ignored",
        TriageState::FalsePositive => "marked false positive",
        TriageState::TechDebt => "saved as tech debt",
        TriageState::MergedInto => "merged",
        TriageState::Unresolved => "left unresolved",
    }
}

// ── The summary edit (U4) ─────────────────────────────────────────────────────────────────

/// Why a summary edit was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryEditError {
    ReasonRequired,
    Empty,
    Unchanged,
}

impl SummaryEditError {
    pub fn message(self) -> &'static str {
        match self {
            Self::ReasonRequired => {
                "An edit needs a reason: say why the model's summary was wrong."
            }
            Self::Empty => "The summary cannot be empty.",
            Self::Unchanged => "That is the summary already.",
        }
    }
}

/// The summary a node shows now: the reviewer's edit when it applies, else the model's.
pub fn summary_text(node: &TreeNodeView) -> &str {
    node.summary.as_deref().unwrap_or_default()
}

/// The edit that replaces a node's summary: keyed by the node id (the server works out the old
/// value and the fingerprint), with a required reason (spec U4).
pub fn summary_edit_request(
    node: &TreeNodeView,
    new_text: &str,
    reason: &str,
    run_id: &str,
) -> Result<NewEditReq, SummaryEditError> {
    let new_text = new_text.trim();
    if new_text.is_empty() {
        return Err(SummaryEditError::Empty);
    }
    if new_text == summary_text(node).trim() {
        return Err(SummaryEditError::Unchanged);
    }
    if reason.trim().is_empty() {
        return Err(SummaryEditError::ReasonRequired);
    }
    Ok(NewEditReq {
        key: node.id.clone(),
        field: EditField::Summary,
        new: new_text.to_string(),
        reason: reason.trim().to_string(),
        by: String::new(),
        run_id: run_id.to_string(),
    })
}

/// A stale summary edit's line under the node's summary: the reviewer's text is shown and not
/// applied (design 5.2).
pub fn stale_summary_note(node: &TreeNodeView) -> Option<String> {
    let edit = node.summary_edit.as_ref().filter(|e| e.stale)?;
    Some(format!(
        "Your edit (stale, the model's summary changed): {}",
        edit.new
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::tests::data;
    use camerata_api_types::review::OutlierReason;
    use camerata_api_types::review_view::{HoldReason, Marker};

    /// A plain row, copied from the fixture and given a distinct key.
    fn plain(n: usize) -> SiteRow {
        let mut r = data()
            .rows
            .iter()
            .find(|r| r.markers.is_empty() && r.decision.is_none() && r.stage2.is_some())
            .cloned()
            .unwrap();
        r.site_key = format!("repo\0path{n}.ts\0fp{n:014}\0{n}");
        r
    }

    fn outlier(n: usize) -> SiteRow {
        let mut r = plain(n);
        r.markers = vec![Marker::Outlier];
        if let Some(s) = r.stage2.as_mut() {
            s.outlier_reason = Some(OutlierReason::DifferentObject);
        }
        r
    }

    fn held(n: usize) -> SiteRow {
        let mut r = plain(n);
        r.markers = vec![Marker::Held];
        r.held = Some(HoldReason::EvidenceUnverified);
        r
    }

    /// 9 plain rows, 2 outliers and 1 held row: the spec's prompt example less three plain rows.
    fn twelve() -> Vec<SiteRow> {
        let mut rows: Vec<SiteRow> = (0..9).map(plain).collect();
        rows.extend((100..102).map(outlier));
        rows.push(held(200));
        rows
    }

    #[test]
    fn node_batch_excludes_outliers_unless_ticked() {
        let rows = twelve();
        let none = BTreeSet::new();
        let plan = batch_plan("sub:X/1", &DecisionState::Confirmed, &rows, &none);
        assert_eq!(plan.keys.len(), 9, "the plain rows");
        assert_eq!(plan.excluded_outliers.len(), 2);
        assert_eq!(plan.ticked_included, 0);

        let mut ticked = BTreeSet::new();
        ticked.insert(rows[9].site_key.clone());
        let plan = batch_plan("sub:X/1", &DecisionState::Confirmed, &rows, &ticked);
        assert_eq!(plan.keys.len(), 10, "one outlier ticked in");
        assert!(plan.keys.contains(&rows[9].site_key));
        assert_eq!(plan.excluded_outliers, vec![rows[10].site_key.clone()]);
        assert_eq!(plan.ticked_included, 1);
    }

    #[test]
    fn batch_plan_excludes_outliers_unless_ticked_and_always_excludes_held() {
        let mut rows = twelve();
        // A held row that is also an outlier, and ticked: held still wins.
        let mut both = held(300);
        both.markers.push(Marker::Outlier);
        if let Some(s) = both.stage2.as_mut() {
            s.outlier_reason = Some(OutlierReason::InTestCode);
        }
        rows.push(both.clone());
        let ticked: BTreeSet<String> = rows.iter().map(|r| r.site_key.clone()).collect();
        let plan = batch_plan("sub:X/1", &DecisionState::Confirmed, &rows, &ticked);
        assert_eq!(plan.excluded_held.len(), 2, "held rows are always excluded");
        assert!(plan.excluded_held.contains(&both.site_key));
        assert!(!plan.keys.contains(&both.site_key));
        assert_eq!(plan.keys.len(), 11, "9 plain and 2 ticked outliers");
        // Every member is somewhere, once.
        let all = plan.keys.len()
            + plan.excluded_outliers.len()
            + plan.excluded_held.len()
            + plan.already_recorded.len();
        assert_eq!(all, rows.len());
    }

    #[test]
    fn the_prompt_says_how_many_records_and_what_is_left_out() {
        let rows = twelve();
        let plan = batch_plan(
            "sub:X/1",
            &DecisionState::Confirmed,
            &rows,
            &BTreeSet::new(),
        );
        assert_eq!(
            plan_sentence(&plan),
            "This writes 9 site records. 2 outliers are excluded unless you tick them. 1 held row is excluded."
        );
        // The spec's example: 12 records, 2 outliers, 1 held row.
        let mut rows: Vec<SiteRow> = (0..12).map(plain).collect();
        rows.extend((100..102).map(outlier));
        rows.push(held(200));
        let plan = batch_plan(
            "sub:X/1",
            &DecisionState::Confirmed,
            &rows,
            &BTreeSet::new(),
        );
        assert_eq!(
            plan_sentence(&plan),
            "This writes 12 site records. 2 outliers are excluded unless you tick them. 1 held row is excluded."
        );
    }

    #[test]
    fn the_sentence_is_singular_where_it_should_be_and_silent_where_it_has_nothing_to_say() {
        let rows = vec![plain(1)];
        let plan = batch_plan("n", &DecisionState::Ignored, &rows, &BTreeSet::new());
        assert_eq!(plan_sentence(&plan), "This writes 1 site record.");
        let rows = vec![plain(1), outlier(2)];
        let plan = batch_plan("n", &DecisionState::Ignored, &rows, &BTreeSet::new());
        assert_eq!(
            plan_sentence(&plan),
            "This writes 1 site record. 1 outlier is excluded unless you tick it."
        );
        let mut ticked = BTreeSet::new();
        ticked.insert(rows[1].site_key.clone());
        let plan = batch_plan("n", &DecisionState::Ignored, &rows, &ticked);
        assert_eq!(
            plan_sentence(&plan),
            "This writes 2 site records. 1 ticked outlier is included."
        );
        assert!(!plan_sentence(&plan).contains('\u{2014}'));
    }

    #[test]
    fn a_member_that_already_holds_this_decision_is_left_as_it_is() {
        let mut rows: Vec<SiteRow> = (0..5).map(plain).collect();
        let decided = data()
            .rows
            .iter()
            .find_map(|r| {
                r.decision
                    .clone()
                    .filter(|d| d.state == DecisionState::Confirmed)
            })
            .unwrap();
        for r in rows.iter_mut().take(3) {
            r.decision = Some(decided.clone());
        }
        let plan = batch_plan("n", &DecisionState::Confirmed, &rows, &BTreeSet::new());
        assert_eq!(plan.keys.len(), 2, "the new members only");
        assert_eq!(plan.already_recorded.len(), 3);
        assert!(
            plan_sentence(&plan).contains("3 already hold this decision and are left as they are.")
        );
        // A different decision is not "already recorded": the new one is written.
        let other = batch_plan("n", &DecisionState::Ignored, &rows, &BTreeSet::new());
        assert_eq!(other.keys.len(), 5);
    }

    #[test]
    fn stale_chip_shows_the_prior_value_and_applies_nothing() {
        // The row's earlier decision no longer applies: the row is Unresolved, the chip names
        // what it was, and a batch treats the row as undecided.
        let row = data()
            .rows
            .iter()
            .find(|r| r.stale_decision.is_some() && r.decision.is_none())
            .cloned()
            .expect("a stale-decision row");
        let prior = row.stale_decision.as_ref().unwrap().state.clone();
        let chip = crate::tree::marker_chips(&row)
            .into_iter()
            .find(|c| c.class == "mark-stale")
            .expect("the stale chip");
        assert_eq!(
            chip.label,
            format!("stale: was {}", crate::tree::decision_label(&prior))
        );
        let cell = crate::tree::decision_cell(&row);
        assert_eq!(cell.label, "Unresolved", "nothing was applied");
        let plan = batch_plan("n", &prior, std::slice::from_ref(&row), &BTreeSet::new());
        assert!(
            plan.already_recorded.is_empty(),
            "the stale record counts for nothing"
        );
        assert_eq!(plan.keys, vec![row.site_key.clone()]);
    }

    #[test]
    fn the_request_is_a_node_scope_decision_that_lists_what_it_leaves_out() {
        let rows = twelve();
        let plan = batch_plan(
            "sub:X/1",
            &DecisionState::FalsePositive,
            &rows,
            &BTreeSet::new(),
        );
        let req = decision_request(&plan, "  same test fixture  ", "insp-7").unwrap();
        assert_eq!(req.scope, Scope::Node("sub:X/1".to_string()));
        assert_eq!(req.state, DecisionState::FalsePositive);
        assert_eq!(req.reason, "same test fixture");
        assert_eq!(req.run_id, "insp-7");
        let mut want: Vec<String> = vec![
            rows[9].site_key.clone(),
            rows[10].site_key.clone(),
            rows[11].site_key.clone(),
        ];
        want.sort();
        assert_eq!(req.except, want, "the two outliers and the held row");
    }

    #[test]
    fn a_batch_needs_a_reason_and_something_to_write() {
        let rows = twelve();
        let plan = batch_plan("n", &DecisionState::Confirmed, &rows, &BTreeSet::new());
        assert_eq!(
            decision_request(&plan, "   ", "r").unwrap_err(),
            BatchError::ReasonRequired,
            "Confirm included"
        );
        let only_held = vec![held(1), held(2)];
        let empty = batch_plan("n", &DecisionState::Confirmed, &only_held, &BTreeSet::new());
        assert!(empty.is_empty());
        assert_eq!(
            decision_request(&empty, "reason", "r").unwrap_err(),
            BatchError::NothingToWrite
        );
        assert!(!BatchError::NothingToWrite.message().contains('\u{2014}'));
    }

    #[test]
    fn tech_debt_at_a_node_is_the_later_bucket_and_a_merge_has_no_node_form() {
        assert_eq!(
            decision_state_for(TriageState::TechDebt),
            Some(DecisionState::TechDebtLater)
        );
        assert_eq!(decision_state_for(TriageState::MergedInto), None);
        assert_eq!(decision_state_for(TriageState::Unresolved), None);
        assert_eq!(
            decision_state_for(TriageState::Confirmed),
            Some(DecisionState::Confirmed)
        );
    }

    #[test]
    fn a_thrown_out_node_or_an_empty_one_takes_no_batch() {
        let nodes = &data().tree.nodes;
        let thrown = nodes
            .iter()
            .find(|n| n.thrown_out)
            .expect("a thrown-out node");
        assert!(!batch_allowed(thrown));
        assert!(batch_blocked_reason(thrown).is_some());
        let normal = nodes
            .iter()
            .find(|n| !n.thrown_out && n.counts.sites > 0)
            .unwrap();
        assert!(batch_allowed(normal));
        assert_eq!(batch_blocked_reason(normal), None);
    }

    #[test]
    fn the_undo_bar_names_the_count_and_lasts_five_seconds() {
        let undo = TreeUndo::for_batch("dec-000007", 12, TriageState::Confirmed, 1_000);
        assert_eq!(undo.message, "12 rows confirmed.");
        assert_eq!(undo.decision_id, "dec-000007");
        assert_eq!(undo.seconds_left(1_000), Some(5));
        assert_eq!(undo.seconds_left(3_500), Some(3));
        assert_eq!(undo.seconds_left(5_999), Some(1));
        assert_eq!(undo.seconds_left(6_000), None, "closed at five seconds");
        assert_eq!(
            TreeUndo::for_batch("d", 1, TriageState::Ignored, 0).message,
            "1 row ignored."
        );
        assert_eq!(
            TreeUndo::for_site("d", TriageState::FalsePositive, 0).message,
            "1 row marked false positive."
        );
    }

    fn node_with_summary(summary: Option<&str>) -> TreeNodeView {
        let mut n = data().tree.nodes[0].clone();
        n.summary = summary.map(str::to_string);
        n.summary_edit = None;
        n
    }

    #[test]
    fn edit_requires_a_reason_and_posts_to_the_edits_route_keyed_by_the_node() {
        let node = node_with_summary(Some("The model's reading."));
        let req =
            summary_edit_request(&node, "  The reviewer's reading.  ", " why ", "insp-3").unwrap();
        assert_eq!(req.key, node.id, "keyed by the node id (U4)");
        assert_eq!(req.field, EditField::Summary);
        assert_eq!(req.new, "The reviewer's reading.");
        assert_eq!(req.reason, "why");
        assert_eq!(req.run_id, "insp-3");
        assert_eq!(
            summary_edit_request(&node, "A new one", "  ", "r").unwrap_err(),
            SummaryEditError::ReasonRequired
        );
        assert_eq!(
            summary_edit_request(&node, "   ", "why", "r").unwrap_err(),
            SummaryEditError::Empty
        );
        assert_eq!(
            summary_edit_request(&node, " The model's reading. ", "why", "r").unwrap_err(),
            SummaryEditError::Unchanged
        );
    }

    #[test]
    fn a_stale_summary_edit_is_shown_beside_the_summary_and_applies_nothing() {
        let mut node = node_with_summary(Some("The model's new reading."));
        assert_eq!(stale_summary_note(&node), None);
        node.summary_edit = Some(camerata_api_types::review_view::EditView {
            id: "edit-000001".into(),
            field: EditField::Summary,
            old: "The model's old reading.".into(),
            new: "My reading.".into(),
            reason: "r".into(),
            by: "zach".into(),
            at: "2026-11-04T09:00:00Z".into(),
            stale: true,
            current_value: "The model's new reading.".into(),
            reapplied_from: None,
        });
        let note = stale_summary_note(&node).unwrap();
        assert!(note.contains("My reading."), "{note}");
        assert_eq!(
            summary_text(&node),
            "The model's new reading.",
            "the summary is still the model's"
        );
        node.summary_edit.as_mut().unwrap().stale = false;
        assert_eq!(stale_summary_note(&node), None);
    }
}
