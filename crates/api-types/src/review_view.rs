//! The review read model: every typed view the review screens read from the server (review UI
//! spec `2026-11-02_review-ui-spec.md`, sections 4 to 10; work order WO9c).
//!
//! The server composes each view from the reviewed report (the sealed report with the reviewer's
//! overlay applied) and the project's stores, with no model call. The UI deserializes the same
//! structs, so a route's real JSON output is the UI's input by construction. Every struct here
//! denies unknown fields: a field the server adds without a home in this file fails the round-trip
//! test and the wire-shape snapshot, instead of being dropped on the way to the screen.
//!
//! Which spec section each struct serves is named on the struct.
//!
//! # What a view never does
//!
//! - [`TreeView`] carries nodes, counts and markers only. It never carries a site, so its size
//!   follows the number of nodes and not the number of findings (spec U1).
//! - A site is read one node at a time, [`SITES_PAGE_SIZE`] at a time ([`SitesPage`]).
//! - "Show below the line" is a view flag on the queue request and writes nothing (spec U5).
//!   Changing the rate or floor is a separate write ([`EscalationBudgetReq`]).

use serde::{Deserialize, Serialize};

use crate::audit::FindingKind;
use crate::decisions::{ClientAck, DecisionState, Scope};
use crate::edits::EditField;
use crate::review::{
    EscalationBudget, EscalationCandidate, EscalationReason, EscalationState, OutlierReason,
    ReviewLevel, Stage2Disagreement,
};

/// How many sites one [`SitesPage`] carries at most (spec U1).
pub const SITES_PAGE_SIZE: usize = 500;
/// How many candidates below the line one queue request returns at most (spec section 6).
pub const QUEUE_BELOW_PAGE_SIZE: usize = 100;

/// The largest escalation rate the settings accept, per 1,000 lines (spec section 9).
pub const ESCALATION_RATE_MAX: f32 = 10.0;
/// The largest escalation floor the settings accept.
pub const ESCALATION_FLOOR_MAX: usize = 1_000;
/// The largest Engine Check sample the settings accept.
pub const SAMPLE_SIZE_MAX: usize = 1_000;
/// The Engine Check sample the settings suggest for an early engagement (design A2b).
pub const SAMPLE_SIZE_SUGGESTED: usize = 50;
/// A category sampled fewer times than this has a rate that is indicative only (spec section 7).
pub const SMALL_CATEGORY_DRAWN: usize = 10;

// ── Shared pieces ─────────────────────────────────────────────────────────────────────────

/// A chip on a site row (spec section 2). A row lists every marker it has, in the precedence the
/// screens draw them in: the first two show in the Markers column and the rest fold into "+N".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Marker {
    /// The release gate refuses the row until it is decided.
    Held,
    /// In the architect's queue, above the line.
    Escalated,
    /// A candidate below the line.
    Candidate,
    /// Escalated past the budget.
    OverBudget,
    /// The group reviewer or the engine says it does not belong with its sub-group.
    Outlier,
    /// A member of a decided node that the node decision did not cover.
    NewSinceDecision,
    /// A decision or an edit made on an earlier run that no longer applies.
    Stale,
    /// The code moved; the row has a new key.
    Moved,
    /// Another row may describe the same thing.
    PossibleDuplicate,
    /// The reviewer changed a field of the row.
    Edited,
}

impl Marker {
    /// Every marker, in precedence order (spec section 2, "noise rule").
    pub const PRECEDENCE: [Marker; 10] = [
        Self::Held,
        Self::Escalated,
        Self::Candidate,
        Self::OverBudget,
        Self::Outlier,
        Self::NewSinceDecision,
        Self::Stale,
        Self::Moved,
        Self::PossibleDuplicate,
        Self::Edited,
    ];
}

/// Why the release gate refuses a row until the reviewer rules on it (spec section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// The quoted evidence was not found near the cited line.
    EvidenceUnverified,
    /// The row's text is junk (a placeholder, a refusal, an untranslated fragment).
    JunkContent,
    /// The engine threw the row out and the reviewer put it back; a restore is not a ruling.
    RestoredWithoutDecision,
    /// The Engine Check said No and no correction or decision has been recorded since.
    EngineCheckNoWithoutCorrection,
    /// The row exports as merged and no merge decision with a reason stands behind it.
    MergedWithoutDecision,
}

impl HoldReason {
    /// Every reason, in the order the release gate runs its checks.
    pub const ALL: [HoldReason; 5] = [
        Self::JunkContent,
        Self::EvidenceUnverified,
        Self::RestoredWithoutDecision,
        Self::MergedWithoutDecision,
        Self::EngineCheckNoWithoutCorrection,
    ];

    /// The reason in words, for the export-blocked panel.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::EvidenceUnverified => "evidence unverified",
            Self::JunkContent => "junk text",
            Self::RestoredWithoutDecision => "put back after the engine threw it out, no decision",
            Self::EngineCheckNoWithoutCorrection => "Engine Check said No, no correction yet",
            Self::MergedWithoutDecision => "merged with no decision record",
        }
    }
}

/// Where a site stands against the escalation line (spec section 7, the Engine Check card).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    /// The group reviewer did not want to escalate it.
    NotCandidate,
    /// Wanted, past the budget, kept below the line.
    BelowLine { rank: u32 },
    /// In the queue.
    Escalated { reason: EscalationReason, rank: u32 },
    /// In the queue past the budget, for the reason that may exceed it.
    OverBudget { reason: EscalationReason, rank: u32 },
}

/// Stage 1's answer about a site (spec sections 5, 7 and 8: the Stage 1 column and line).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage1View {
    /// Stage 1's severity: the row's own severity, or the value a reviewer's severity edit
    /// replaced.
    pub severity: String,
    /// `yes`, `no` or `unclear`; blank when stage 1 did not read the site.
    pub real: String,
    pub effort: Option<String>,
    pub confidence: Option<String>,
    /// Stage 1's one-line reason; blank when stage 1 did not read the site.
    pub reason: String,
}

/// What the group reviewer said about a site (spec sections 5 and 8: the Stage 2 column and line).
/// Present only when the group reviewer has said something about the site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage2View {
    pub state: EscalationState,
    pub reason: Option<EscalationReason>,
    pub why: String,
    pub stage2_rank: Option<u32>,
    pub global_rank: Option<u32>,
    pub over_budget_reason: Option<EscalationReason>,
    pub outlier_reason: Option<OutlierReason>,
    pub outlier_why: String,
    pub subgroup_label: String,
    pub subgroup_cause: String,
    /// Written beside stage 1's answer and never over it.
    pub disagreements: Vec<Stage2Disagreement>,
}

/// A decision as a screen shows it (spec sections 5 and 8: the Decision column and line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionView {
    pub id: String,
    pub state: DecisionState,
    pub reason: String,
    pub scope: Scope,
    /// The node decision this site's record came from ("inherited" in small text).
    pub inherited_from: Option<String>,
    pub decided_by: String,
    pub decided_at: String,
    pub needed_me: Option<bool>,
    /// The client's acknowledgement of an Ignored row, once delivered.
    pub confirmed_by_client: Option<ClientAck>,
}

/// An edit as a screen shows it (spec section 8: edited fields, stale edits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditView {
    pub id: String,
    pub field: EditField,
    pub old: String,
    pub new: String,
    pub reason: String,
    pub by: String,
    pub at: String,
    /// The row changed since the edit was made, so the edit is shown and not applied.
    pub stale: bool,
    /// The field's text now, so a stale edit can be compared with `old` and `new`.
    pub current_value: String,
    pub reapplied_from: Option<String>,
}

/// A member of a decided node that the node decision did not cover (spec section 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewSinceDecisionView {
    pub decision_id: String,
    pub node_id: String,
    pub decided_at: String,
    /// How many sites the node decision covers ("12 others").
    pub covered: usize,
}

/// Where a moved site was before (spec section 8: "moved, was line 212").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovedFromView {
    pub key: String,
    /// The line in the earlier run, when that run is still stored.
    pub line: Option<usize>,
}

/// A source collapsed into a row (spec section 8: "Also flagged by"). It keeps its own location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlaggedByView {
    pub rule_id: String,
    pub tool: Option<String>,
    pub site_key: String,
    pub path: String,
    pub line: usize,
}

/// A possible duplicate and what it points at (spec section 2: the `dup?` chip).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateView {
    pub key: String,
    pub hint: String,
    pub rule_id: String,
    pub path: String,
    pub line: usize,
}

/// A row merged into another (spec section 8: "Merged into web/env.ts:3").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergedIntoView {
    pub key: String,
    /// `path:line` of the target, as the Decision column prints it.
    pub label: String,
}

/// One site row (spec section 5, "Columns of a site row", and section 8, the site detail). The
/// same row serves the tree, the queue, the Engine Check card and the detail pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteRow {
    pub site_key: String,
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    /// What the row is; `None` only for a report older than the field (shown as unspecified).
    pub kind: Option<FindingKind>,
    pub category: Option<String>,
    pub headline: String,
    pub detail: String,
    /// The fix text for this site.
    pub fix: String,
    /// `Tailored fix` or `Standard fix`.
    pub fix_type: String,
    pub snippet: String,
    /// `[category node id, rule node id, sub-group node id]`.
    pub group_path: Vec<String>,
    /// The sub-group's primary row (the one that orders the sub-group).
    pub group_primary: bool,
    pub stage1: Stage1View,
    pub stage2: Option<Stage2View>,
    /// Every marker the row has, in [`Marker::PRECEDENCE`] order.
    pub markers: Vec<Marker>,
    pub decision: Option<DecisionView>,
    /// The edits made to this row, applied or stale, oldest first.
    pub edits: Vec<EditView>,
    /// An earlier decision that no longer applies. The row is Unresolved beside it.
    pub stale_decision: Option<DecisionView>,
    pub new_since_decision: Option<NewSinceDecisionView>,
    pub moved_from: Option<MovedFromView>,
    pub flagged_by: Vec<FlaggedByView>,
    pub possible_duplicate_of: Option<DuplicateView>,
    pub merged_into: Option<MergedIntoView>,
    /// Why the release gate holds the row, when it does.
    pub held: Option<HoldReason>,
    /// How closely the row was reviewed; `None` for a row held for a ruling.
    pub review_level: Option<ReviewLevel>,
    /// The reviewer added this finding.
    pub added_by_reviewer: bool,
    /// Why the engine threw the row out, when it is a thrown-out row.
    pub thrown_out_reason: Option<String>,
    /// Review flags on the row's wording (spec A8); internal.
    pub review_flags: Vec<String>,
}

// ── Section 4: the shell ──────────────────────────────────────────────────────────────────

/// The counts in the progress strip (spec section 4): the triage tally plus the held rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCounts {
    /// Every site of the run, thrown-out rows excluded.
    pub total: usize,
    pub unresolved: usize,
    pub confirmed: usize,
    pub ignored: usize,
    pub false_positive: usize,
    pub tech_debt: usize,
    pub merged: usize,
    /// Rows the release gate refuses without a decision.
    pub held: usize,
}

/// The numbers on the lane switcher (spec section 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneCounts {
    /// "Tree 1,204 undecided".
    pub tree_undecided: usize,
    /// "Escalations 347".
    pub escalations: usize,
    /// "Engine Check 50": the cards drawn for the run.
    pub engine_check: usize,
    /// The sample size setting is 0 ("Engine Check off").
    pub engine_check_off: bool,
    /// "Flat view": the rows of the flat queue.
    pub flat_rows: usize,
}

/// Decisions and edits that no longer sit well on this run (spec section 8: stale, moved, new).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaleSummary {
    pub stale_decisions: usize,
    pub stale_edits: usize,
    pub moved: usize,
    pub new_since_decision: usize,
}

/// `GET /api/projects/:id/review`: the review shell (spec section 4). Loaded once by the shell host
/// and refreshed after every write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewShellView {
    pub run_id: String,
    pub engine_version: String,
    pub counts: ReviewCounts,
    pub budget: EscalationBudget,
    pub lanes: LaneCounts,
    pub export_gate: ExportGateView,
    pub stale: StaleSummary,
    /// False for an old inspection no group reviewer has read (spec section 5: "Not reviewed by
    /// the group reviewer").
    pub group_reviewed: bool,
}

// ── Section 5: the tree and its sites ─────────────────────────────────────────────────────

/// Which level of the tree a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TreeLevel {
    Category,
    Rule,
    Subgroup,
}

/// Whether a node is read site by site or decided as a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TreeNodeKind {
    Escalation,
    Bulk,
}

/// Who wrote a node's summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummarySource {
    Model,
    Reviewer,
}

/// The counts on a node's header (spec section 2, "Counts line"). Every one is read from the tree
/// and the rows, never from the group reviewer's prose (design A9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCountsView {
    pub sites: usize,
    pub escalated: usize,
    pub below_the_line: usize,
    pub outliers: usize,
    pub new_since_decision: usize,
    pub stale: usize,
    pub held: usize,
    pub decided: usize,
    /// Sites the group reviewer did not place in a sub-group.
    pub unplaced: usize,
}

/// A group decision standing on a node (spec section 5: the node head's decided state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDecisionSummary {
    pub id: String,
    pub state: DecisionState,
    pub reason: String,
    pub decided_at: String,
    /// How many sites the decision covers.
    pub covered: usize,
    /// How many members it deliberately left out (unticked outliers, held rows).
    pub left_out: usize,
}

/// One node of the tree (spec section 5, `TreeNodeView`). Never lists its sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeNodeView {
    /// `cat:authorization`, `rule:SEC-RLS-1`, `sub:SEC-RLS-1/3`.
    pub id: String,
    pub level: TreeLevel,
    pub parent: Option<String>,
    pub label: String,
    pub kind: TreeNodeKind,
    /// Why the sites of a sub-group belong together.
    pub cause: Option<String>,
    /// What the reviewer reads at this node.
    pub summary: Option<String>,
    pub summary_source: SummarySource,
    /// Review flags on the summary (a count the data contradicts, a differing member not named).
    pub flags: Vec<String>,
    /// A node of rows the engine threw out (spec design A12).
    pub thrown_out: bool,
    pub counts: NodeCountsView,
    pub decision: Option<NodeDecisionSummary>,
    /// The reviewer's edit of the summary (spec U4); the engine's summary is replaced by it.
    pub summary_edit: Option<EditView>,
}

/// `GET /api/projects/:id/review/tree`: nodes, counts and markers only; never sites (spec U1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeView {
    pub run_id: String,
    pub group_reviewed: bool,
    /// Every level, parents before children.
    pub nodes: Vec<TreeNodeView>,
}

/// `GET /api/projects/:id/review/sites?node=&cursor=&limit=`: one node's sites, at most
/// [`SITES_PAGE_SIZE`] at a time (spec U1). The order is fixed (severity, then path, line and key),
/// and the cursor names the last row returned, so a decision or an edit made between two pages
/// never moves a row from one page to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SitesPage {
    pub run_id: String,
    pub node: String,
    /// Sites under the node in all.
    pub total: usize,
    /// Where this page starts, counted in sites.
    pub offset: usize,
    pub rows: Vec<SiteRow>,
    /// Pass back as `cursor` for the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

// ── Section 6: the escalation queue ───────────────────────────────────────────────────────

/// One step of a breadcrumb (a node id and its label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreadcrumbItem {
    pub id: String,
    pub label: String,
}

/// One row of the queue (spec section 6, "Columns").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueRow {
    pub candidate: EscalationCandidate,
    /// Category, rule and sub-group, for the Node column and its jump to the tree.
    pub node_path: Vec<BreadcrumbItem>,
    pub site: SiteRow,
}

/// "Needed me: 120 of 140 decided (86%)" (spec section 6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeededMeSummary {
    /// Escalated rows the reviewer has decided.
    pub decided: usize,
    /// Of those, how many the reviewer marked as needing them.
    pub needed_me: usize,
    /// `needed_me` as a whole percent of `decided`; 0 when nothing is decided.
    pub percent: u8,
}

/// `GET /api/projects/:id/review/queue?show_below=&below_offset=&below_limit=` (spec section 6).
///
/// `show_below` is a VIEW flag (spec U5): it asks for candidates below the line to be listed and
/// writes nothing. The rate and floor change only through [`EscalationBudgetReq`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueView {
    pub run_id: String,
    pub budget: EscalationBudget,
    /// The reasons that may exceed the budget, for the over-budget sentence.
    pub over_budget_reasons: Vec<EscalationReason>,
    /// Escalated candidates: the budgeted ones then the ones over budget, in rank order.
    pub escalated: Vec<QueueRow>,
    pub show_below: bool,
    /// Candidates below the line, [`QUEUE_BELOW_PAGE_SIZE`] at a time; empty unless `show_below`.
    pub below: Vec<QueueRow>,
    pub below_total: usize,
    pub below_offset: usize,
    pub needed_me: NeededMeSummary,
    /// False when no group reviewer has read the run (the empty state explains why).
    pub group_reviewed: bool,
}

// ── Section 7: the Engine Check lane ──────────────────────────────────────────────────────

/// The closed list of what was wrong (design A2b and A9 item 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineCheckError {
    NotARealIssue,
    WrongSeverity,
    DuplicateOfAnotherRow,
    ShouldHaveBeenEscalated,
    ShouldNotHaveBeenEscalated,
    WrongGrouping,
    WordingOrFixWrong,
    /// Design A9 item 3.
    GroupSummaryDoesNotDescribe,
}

impl EngineCheckError {
    /// Every choice, in the order the list offers them.
    pub const ALL: [EngineCheckError; 8] = [
        Self::NotARealIssue,
        Self::WrongSeverity,
        Self::DuplicateOfAnotherRow,
        Self::ShouldHaveBeenEscalated,
        Self::ShouldNotHaveBeenEscalated,
        Self::WrongGrouping,
        Self::WordingOrFixWrong,
        Self::GroupSummaryDoesNotDescribe,
    ];
}

/// The decision or edit that corrects a row an Engine Check No named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionRef {
    Decision(String),
    Edit(String),
}

/// Which stratum a sampled row was drawn from (design A10 item 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleStratum {
    /// An unread kept row.
    Kept,
    /// A row the engine threw out; sampled apart and its rate never pooled with the kept rows.
    ThrownOut,
}

/// One Yes or No on one sampled row, as stored (spec section 7). Internal: it never appears in
/// any client output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCheckVerdict {
    /// The site key.
    pub key: String,
    pub run_id: String,
    /// "Did the engine get this right?" Yes is `true`.
    pub right: bool,
    /// What was wrong; required when `right` is false.
    pub wrong: Option<EngineCheckError>,
    /// The correction, once the reviewer has made one. A No with `None` here holds the row from
    /// the client export.
    pub correction: Option<CorrectionRef>,
    pub by: String,
    pub at: String,
}

/// `POST /api/projects/:id/review/engine-check/:key` (spec section 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCheckVerdictReq {
    pub right: bool,
    #[serde(default)]
    pub wrong: Option<EngineCheckError>,
    #[serde(default)]
    pub correction: Option<CorrectionRef>,
    #[serde(default)]
    pub by: String,
}

/// Where the engine placed a sampled row (spec section 7: "Placed").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementView {
    pub node_path: Vec<BreadcrumbItem>,
    pub subgroup_label: String,
    pub cause: String,
    /// The group summary the row sits under.
    pub summary: Option<String>,
}

/// An outlier mark on a sampled row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlierView {
    pub reason: OutlierReason,
    pub why: String,
}

/// One Engine Check card: the row as the client would see it, and everything the engine did with
/// it (spec section 7; design A2b "engine_check_card_shows_every_engine_decision_for_the_row").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCheckCard {
    pub site: SiteRow,
    pub stratum: SampleStratum,
    pub stage1: Stage1View,
    pub placement: PlacementView,
    pub escalation: EscalationStatus,
    pub flagged_by: Vec<FlaggedByView>,
    pub duplicate_link: Option<DuplicateView>,
    pub outliers: Vec<OutlierView>,
    pub review_flags: Vec<String>,
    pub verdict: Option<EngineCheckVerdict>,
}

/// One row of the allocation line and the rates (spec section 7, "Authorization 22 ...").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryAllocation {
    pub category: String,
    /// Cards drawn from this category.
    pub drawn: usize,
    /// Unread rows the category had to draw from.
    pub unread: usize,
    pub verdicts: usize,
    pub wrong: usize,
    /// Wrong as a percent of verdicts; `None` before the first verdict.
    pub rate_percent: Option<f64>,
    /// The 95 percent margin of that rate, in percentage points.
    pub margin_percent: Option<f64>,
    /// Drawn fewer than [`SMALL_CATEGORY_DRAWN`]: the rate is indicative only.
    pub indicative_only: bool,
}

/// The rate over one stratum (design A10 item 2: the thrown-out stratum apart from the kept rows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StratumRate {
    pub stratum: SampleStratum,
    pub drawn: usize,
    pub verdicts: usize,
    pub wrong: usize,
    pub rate_percent: Option<f64>,
    pub margin_percent: Option<f64>,
}

/// `GET /api/projects/:id/review/engine-check` (spec section 7). Absent from every client export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCheckLane {
    pub run_id: String,
    /// The `sample_size_per_run` setting.
    pub setting: usize,
    /// The setting is 0: the lane is off and has no cards.
    pub off: bool,
    pub drawn: usize,
    /// Unread kept rows in the run.
    pub unread_total: usize,
    /// Thrown-out rows in the run.
    pub thrown_out_total: usize,
    pub allocation: Vec<CategoryAllocation>,
    /// The kept stratum then the thrown-out stratum; never pooled.
    pub strata: Vec<StratumRate>,
    /// Cards in category order, then thrown-out; the order is fixed by the run.
    pub cards: Vec<EngineCheckCard>,
}

// ── Section 10: the export-blocked state ──────────────────────────────────────────────────

/// A row the release gate refuses (spec section 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRow {
    pub key: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub reason: HoldReason,
    /// Extra words for the panel line (the merge target, for a merge).
    pub detail: String,
}

/// A group decision that has no record of its own for some site it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDecisionGap {
    pub decision_id: String,
    pub node_id: String,
    /// The site keys the decision covers that hold no record.
    pub missing: Vec<String>,
}

/// `GET /api/projects/:id/review/export-gate`, and the shell's `export_gate` (spec section 10).
/// Derived from the release gate's own refusal functions, so it lists exactly what the gate
/// refuses for the same reviewed report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportGateView {
    pub held: Vec<HeldRow>,
    pub merged_without_decision: Vec<HeldRow>,
    pub node_decisions_missing_members: Vec<NodeDecisionGap>,
}

impl ExportGateView {
    /// The gate refuses the package.
    pub fn is_blocked(&self) -> bool {
        !self.held.is_empty()
            || !self.merged_without_decision.is_empty()
            || !self.node_decisions_missing_members.is_empty()
    }

    /// How many rows need a decision ("3 rows need a decision before the package can be built").
    pub fn rows_needing_a_decision(&self) -> usize {
        self.held.len() + self.merged_without_decision.len()
    }
}

// ── Section 9: settings ───────────────────────────────────────────────────────────────────

/// The model per stage, as the settings lane shows them (spec section 9). An empty model means
/// not set: the run uses that stage's default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageModelsView {
    /// The scan model.
    pub scan: String,
    /// Calibration, stage 1.
    pub calibration: String,
    /// The group review, stage 2.
    pub group_review: String,
    pub fix: String,
}

/// The escalation formula (spec section 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationSettingsView {
    pub rate_per_kloc: f32,
    pub floor: usize,
    /// Lines the latest run counted; the formula's input.
    pub counted_lines: usize,
    /// The budget those settings give for the latest run.
    pub budget: EscalationBudget,
    pub over_budget_reasons: Vec<EscalationReason>,
}

/// The Engine Check setting (spec section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCheckSettingsView {
    pub sample_size_per_run: usize,
    pub suggested: usize,
}

/// The bounds the settings validate against (spec section 9: "Validation").
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsLimits {
    pub rate_max: f32,
    pub floor_max: usize,
    pub sample_max: usize,
}

/// `GET /api/projects/:id/review/settings` (spec section 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSettingsView {
    pub stage_models: StageModelsView,
    pub escalation: EscalationSettingsView,
    pub engine_check: EngineCheckSettingsView,
    pub limits: SettingsLimits,
}

/// `POST /api/projects/:id/escalation-budget` (applies) and
/// `POST /api/projects/:id/escalation-budget/preview` (writes nothing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationBudgetReq {
    pub rate_per_kloc: f32,
    pub floor: usize,
    pub over_budget_reasons: Vec<EscalationReason>,
}

/// The reply to both budget routes: the line before and after, and how many candidates crossed it
/// (spec section 6, "Rate changed": "103 candidates moved above the line. No model call was
/// made.").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationBudgetOutcome {
    /// False for a preview.
    pub applied: bool,
    pub before: EscalationBudget,
    pub after: EscalationBudget,
    /// Candidates below the line that the new line escalates.
    pub moved_above: usize,
    /// Escalated candidates the new line puts below it.
    pub moved_below: usize,
}

/// `POST /api/projects/:id/sample-size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleSizeReq {
    pub sample_size_per_run: usize,
}

/// The reply to a sample size write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleSizeResp {
    pub sample_size_per_run: usize,
    /// The new size is 0.
    pub lane_off: bool,
}

/// The body of every refusal from the review routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewViewError {
    pub ok: bool,
    /// A short machine-readable reason (`no_inspection`, `unknown_node`, `run_changed`,
    /// `invalid_setting`, ...).
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_precedence_lists_every_marker_once() {
        let mut seen = std::collections::HashSet::new();
        for m in Marker::PRECEDENCE {
            assert!(seen.insert(m), "{m:?} is listed twice");
        }
        assert_eq!(seen.len(), 10);
    }

    #[test]
    fn the_engine_check_list_is_closed_and_ends_with_the_summary_choice() {
        assert_eq!(EngineCheckError::ALL.len(), 8);
        assert_eq!(
            EngineCheckError::ALL[7],
            EngineCheckError::GroupSummaryDoesNotDescribe
        );
    }

    #[test]
    fn an_empty_gate_view_is_not_blocked() {
        let view = ExportGateView::default();
        assert!(!view.is_blocked());
        assert_eq!(view.rows_needing_a_decision(), 0);
    }

    #[test]
    fn a_correction_ref_names_a_decision_or_an_edit() {
        let d = serde_json::to_value(CorrectionRef::Decision("dec-1".into())).unwrap();
        assert_eq!(d, serde_json::json!({ "decision": "dec-1" }));
        let e: CorrectionRef =
            serde_json::from_value(serde_json::json!({ "edit": "e-1" })).unwrap();
        assert_eq!(e, CorrectionRef::Edit("e-1".into()));
    }
}
