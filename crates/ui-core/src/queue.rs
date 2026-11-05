//! The escalation queue lane's judgments (review UI spec sections 6 and 11; design section 7).
//!
//! Every judgment the queue screen makes is a pure function here, with a unit test (spec rule 2):
//! the order of the rows, where the line falls and what it says, the sentences above the table,
//! which rows get a Needed me box, what a key means, and what the view-only "show below the line"
//! does (nothing but change what is drawn: no request, no write, U5). The view in `camerata-ui`
//! chooses words and pixels and calls these.

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review::EscalationReason;
use camerata_api_types::review_view::{
    EscalationBudgetOutcome, NeededMeSummary, QueueRow, QueueView,
};

use crate::edits::{DetailContext, PoolSite};
use crate::review_settings::effect_sentence;
use crate::tree::export::Door;
use crate::tree::thousands;
use crate::triage::{FocusMove, KeyMods, PaneCommand, PaneFocus};

/// How many escalated rows the table draws before "Show 100 more". The spec's 30,000-site queue
/// loads its 347 escalated rows at once, but the table never draws all of them in one go.
pub const ROWS_PER_PAGE: usize = 100;

// ── The three bands ───────────────────────────────────────────────────────────────────────

/// Where a row stands against the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    /// Inside the budget, above the line.
    WithinBudget,
    /// Past the budget, kept for a reason that may exceed it.
    OverBudget,
    /// A candidate below the line.
    BelowLine,
}

/// The band of a row.
pub fn band_of(row: &QueueRow) -> Band {
    if !row.candidate.escalated {
        Band::BelowLine
    } else if row.candidate.over_budget_reason.is_some() {
        Band::OverBudget
    } else {
        Band::WithinBudget
    }
}

/// Sort rows by band (above the line, over budget, below), then by [`EscalationReason`] rank,
/// then by global rank (spec section 6: `queue::sort`; design test
/// `queue_sorts_by_reason_rank_then_global_rank`). The sort is stable, so equal keys keep the
/// order the server sent them in.
pub fn sort_rows(rows: &mut [&QueueRow]) {
    rows.sort_by_key(|r| {
        (
            band_of(r),
            r.candidate.reason.rank(),
            r.candidate.global_rank,
        )
    });
}

/// The rows of a view in the order the table draws them: escalated rows (inside the budget, then
/// over it) and, when they are loaded, the candidates below the line.
pub fn ordered(view: &QueueView) -> Vec<&QueueRow> {
    let mut rows: Vec<&QueueRow> = view.escalated.iter().chain(&view.below).collect();
    sort_rows(&mut rows);
    rows
}

// ── What the view is showing ──────────────────────────────────────────────────────────────

/// The queue's own view state. "Show below the line" is a flag here and nothing more: it changes
/// what is drawn and sends no request and writes nothing (spec U5). Changing the rate or floor is
/// the Settings lane's write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueState {
    /// Draw the candidates below the line.
    pub show_below: bool,
    /// How many escalated rows the table draws.
    pub shown: usize,
    /// The site key of the row the cursor is on.
    pub cursor: Option<String>,
}

impl Default for QueueState {
    fn default() -> Self {
        Self {
            show_below: false,
            shown: ROWS_PER_PAGE,
            cursor: None,
        }
    }
}

impl QueueState {
    /// Show or hide the candidates below the line. Nothing else changes.
    pub fn toggle_below(&mut self) {
        self.show_below = !self.show_below;
    }

    /// "Show 100 more" escalated rows.
    pub fn show_more(&mut self) {
        self.shown += ROWS_PER_PAGE;
    }

    /// Put the cursor on `key`.
    pub fn focus(&mut self, key: &str) {
        self.cursor = Some(key.to_string());
    }

    /// Move the cursor one row in the drawn order (`keys`). With no cursor, `Down` takes the
    /// first row and `Up` the last. Stops at the ends.
    pub fn move_cursor(&mut self, keys: &[&str], dir: FocusMove) {
        if keys.is_empty() {
            return;
        }
        let at = self
            .cursor
            .as_deref()
            .and_then(|c| keys.iter().position(|k| *k == c));
        let next = match (at, dir) {
            (None, FocusMove::Down) => 0,
            (None, FocusMove::Up) => keys.len() - 1,
            (Some(i), FocusMove::Down) => (i + 1).min(keys.len() - 1),
            (Some(i), FocusMove::Up) => i.saturating_sub(1),
        };
        self.cursor = Some(keys[next].to_string());
    }
}

/// One line of the table.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry<'a> {
    /// A row.
    Row(&'a QueueRow),
    /// The line between the budgeted rows and what follows: `the line: 300 of a budget of 300`.
    Line(String),
    /// The over-budget band's heading: `47 over budget: all unauthenticated reach or ...`.
    OverBand(String),
    /// The heading above the candidates below the line.
    BelowHeading(String),
}

/// What the table draws for a view and a state.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout<'a> {
    pub entries: Vec<Entry<'a>>,
    /// Escalated rows not drawn yet ("Show 100 more").
    pub hidden_escalated: usize,
    /// The footer under the table: the hidden candidates and what to do about them.
    pub footer: Option<String>,
    /// Candidates below the line the server has not sent yet (a read, not a write).
    pub below_unloaded: usize,
}

impl Layout<'_> {
    /// The site keys of the rows drawn, in order: what Up and Down walk.
    pub fn keys(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Row(r) => Some(r.candidate.key.as_str()),
                _ => None,
            })
            .collect()
    }

    /// How many rows are drawn.
    pub fn row_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, Entry::Row(_)))
            .count()
    }
}

/// The rows, the line and the headings for `view` under `state`. The escalated rows load at once
/// but only `state.shown` of them are drawn; the line is drawn once every row above it is. The
/// candidates below the line are drawn only when `state.show_below`.
pub fn layout<'a>(view: &'a QueueView, state: &QueueState) -> Layout<'a> {
    let mut escalated: Vec<&QueueRow> = view.escalated.iter().collect();
    sort_rows(&mut escalated);
    let within = escalated
        .iter()
        .take_while(|r| band_of(r) == Band::WithinBudget)
        .count();
    let over = escalated.len() - within;
    let shown = state.shown.min(escalated.len());

    let mut entries: Vec<Entry<'a>> = Vec::new();
    for (i, row) in escalated.iter().take(shown).enumerate() {
        if i == within {
            entries.push(Entry::Line(line_text(view, within)));
            if over > 0 {
                entries.push(Entry::OverBand(over_band_text(&escalated[within..])));
            }
        }
        entries.push(Entry::Row(row));
    }
    // Every row above the line is drawn and no row below it is yet: the line closes the list.
    if shown > 0 && shown == within {
        entries.push(Entry::Line(line_text(view, within)));
    }
    let hidden_escalated = escalated.len() - shown;

    let mut below: Vec<&QueueRow> = view.below.iter().collect();
    sort_rows(&mut below);
    let below_unloaded = view.below_total.saturating_sub(below.len());
    if state.show_below && !below.is_empty() {
        entries.push(Entry::BelowHeading(below_heading(view, below.len())));
        entries.extend(below.iter().map(|r| Entry::Row(r)));
    }
    Layout {
        entries,
        hidden_escalated,
        footer: footer_text(view, state, below.len()),
        below_unloaded,
    }
}

// ── Words ─────────────────────────────────────────────────────────────────────────────────

/// `Budget 300 of 300,000 lines at 1.0 per 1,000 (floor 20)`.
pub fn budget_line(view: &QueueView) -> String {
    let b = &view.budget;
    format!(
        "Budget {} of {} lines at {:?} per 1,000 (floor {})",
        thousands(b.budget),
        thousands(b.counted_lines),
        b.rate_per_kloc,
        thousands(b.floor)
    )
}

/// What the rows over budget have in common, in the design's words (4.4 step 5): `all
/// unauthenticated reach or secret exposure at critical`, and `unverified evidence at any
/// severity` where that is why a row is over. The severities are read from the rows, never
/// assumed from the reason: the server lets a reason go over budget only at critical, so on real
/// data the sentence says critical, but a row of another severity is named as it is.
pub fn over_budget_reasons_text(rows: &[&QueueRow]) -> String {
    // Each reason with the severities its rows have, worst first.
    let mut by_reason: Vec<(EscalationReason, Vec<String>)> = Vec::new();
    for row in rows {
        let Some(reason) = row.candidate.over_budget_reason else {
            continue;
        };
        let severity = row.candidate.severity.to_ascii_lowercase();
        match by_reason.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, severities)) => {
                if !severities.contains(&severity) {
                    severities.push(severity);
                }
            }
            None => by_reason.push((reason, vec![severity])),
        }
    }
    by_reason.sort_by_key(|(r, _)| r.rank());
    for (_, severities) in &mut by_reason {
        severities.sort_by_key(|s| crate::viewer::severity_rank(s));
    }
    let evidence = by_reason
        .iter()
        .any(|(r, _)| *r == EscalationReason::EvidenceUnverified);
    // Reasons that share the same severities share one clause.
    let mut clauses: Vec<(Vec<&str>, &Vec<String>)> = Vec::new();
    for (reason, severities) in by_reason
        .iter()
        .filter(|(r, _)| *r != EscalationReason::EvidenceUnverified)
    {
        match clauses.iter_mut().find(|(_, s)| *s == severities) {
            Some((phrases, _)) => phrases.push(reason.phrase()),
            None => clauses.push((vec![reason.phrase()], severities)),
        }
    }
    let mut parts: Vec<String> = clauses
        .iter()
        .map(|(phrases, severities)| {
            let words: Vec<&str> = severities.iter().map(String::as_str).collect();
            format!("{} at {}", join_or(phrases), join_or(&words))
        })
        .collect();
    if evidence {
        parts.push("unverified evidence at any severity".to_string());
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("all {}", parts.join(", and "))
}

fn join_or(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => (*only).to_string(),
        [first, last] => format!("{first} or {last}"),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// `Escalated 347 · 47 over budget (all ... at critical) · 1,212 candidates below the line`.
pub fn counts_line(view: &QueueView) -> String {
    let mut parts = vec![format!("Escalated {}", thousands(view.escalated.len()))];
    let over: Vec<&QueueRow> = view
        .escalated
        .iter()
        .filter(|r| band_of(r) == Band::OverBudget)
        .collect();
    if !over.is_empty() {
        let why = over_budget_reasons_text(&over);
        parts.push(if why.is_empty() {
            format!("{} over budget", thousands(over.len()))
        } else {
            format!("{} over budget ({why})", thousands(over.len()))
        });
    }
    parts.push(format!(
        "{} {} below the line",
        thousands(view.below_total),
        if view.below_total == 1 {
            "candidate"
        } else {
            "candidates"
        }
    ));
    parts.join(" \u{b7} ")
}

/// `Needed me: 120 of 140 decided (86%)`.
pub fn needed_me_line(n: &NeededMeSummary) -> String {
    if n.decided == 0 {
        return "Needed me: nothing decided yet".to_string();
    }
    format!(
        "Needed me: {} of {} decided ({}%)",
        thousands(n.needed_me),
        thousands(n.decided),
        n.percent
    )
}

/// The divider that is the line: `the line: 300 of a budget of 300`.
pub fn line_text(view: &QueueView, within: usize) -> String {
    format!(
        "the line: {} of a budget of {}",
        thousands(within),
        thousands(view.budget.budget)
    )
}

fn over_band_text(over_rows: &[&QueueRow]) -> String {
    let why = over_budget_reasons_text(over_rows);
    if why.is_empty() {
        format!("{} over budget", thousands(over_rows.len()))
    } else {
        format!("{} over budget: {why}", thousands(over_rows.len()))
    }
}

fn below_heading(view: &QueueView, loaded: usize) -> String {
    if loaded < view.below_total {
        format!(
            "below the line: {} of {} candidates",
            thousands(loaded),
            thousands(view.below_total)
        )
    } else {
        format!("below the line: {} candidates", thousands(view.below_total))
    }
}

fn footer_text(view: &QueueView, state: &QueueState, below_loaded: usize) -> Option<String> {
    if view.below_total == 0 {
        return None;
    }
    if state.show_below {
        let more = view.below_total.saturating_sub(below_loaded);
        return (more > 0).then(|| {
            format!(
                "{} more {} below the line",
                thousands(more),
                if more == 1 { "candidate" } else { "candidates" }
            )
        });
    }
    Some(format!(
        "below the line: {} {} (hidden)",
        thousands(view.below_total),
        if view.below_total == 1 {
            "candidate"
        } else {
            "candidates"
        }
    ))
}

/// What the empty state says (spec section 6).
pub const EMPTY: &str =
    "No escalations. The group reviewer found nothing that needs you, or the run has not reached stage 2.";

/// The empty state: the sentence, then the stage 2 status.
pub fn empty_text(view: &QueueView) -> String {
    let status = if view.group_reviewed {
        "The group reviewer read this run."
    } else {
        "The group reviewer has not read this run."
    };
    format!("{EMPTY} {status}")
}

/// The toast after the rate or floor changed: `Budget is now 450. 103 candidates moved above the
/// line. No model call was made.`
pub fn rate_changed_text(outcome: &EscalationBudgetOutcome) -> String {
    format!(
        "Budget is now {}. {}",
        thousands(outcome.after.budget),
        effect_sentence(outcome)
    )
}

/// Whether the "Budget is now ..." banner still describes the queue. It says what a rate change
/// did to the run that was current when it was applied; once the shell reads a different run (a
/// new inspection), the line it counts candidates against is gone and the banner is stale. Dismiss
/// is not the only way out. `seen_run` is the run the shell last read, `None` before the first read.
pub fn applied_budget_stands(seen_run: Option<&str>, now_run: &str) -> bool {
    seen_run.is_none_or(|seen| seen == now_run)
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ── Cells ─────────────────────────────────────────────────────────────────────────────────

/// The Reason column: the escalation reason in words, `(over)` when the row is past the budget.
pub fn reason_cell(row: &QueueRow) -> String {
    let reason = capitalize(row.candidate.reason.phrase());
    if band_of(row) == Band::OverBudget {
        format!("{reason} (over)")
    } else {
        reason
    }
}

/// The Node column: the sub-group the row sits in, on one line; the whole path is the title.
pub fn node_cell(row: &QueueRow) -> String {
    row.node_path
        .last()
        .map(|c| c.label.clone())
        .unwrap_or_default()
}

/// The whole path, for the Node column's tooltip.
pub fn node_title(row: &QueueRow) -> String {
    row.node_path
        .iter()
        .map(|c| c.label.as_str())
        .collect::<Vec<_>>()
        .join(" > ")
}

/// The sub-group node a row sits in, where "go to the node" lands.
pub fn node_id_of(row: &QueueRow) -> Option<&str> {
    row.node_path.last().map(|c| c.id.as_str())
}

/// Whether the row gets a Needed me box at all: only rows in the queue, not the candidates below
/// the line.
pub fn has_needed_me_box(row: &QueueRow) -> bool {
    band_of(row) != Band::BelowLine
}

/// The Needed me box can be changed only once the row is decided, and only when the decision is
/// one record of its own (a revoked or missing decision has nothing to flag).
pub fn needed_me_enabled(row: &QueueRow) -> bool {
    has_needed_me_box(row) && row.site.decision.is_some()
}

/// Whether the box is ticked: the decision says the reviewer needed to see the row.
pub fn needed_me_checked(row: &QueueRow) -> bool {
    row.site
        .decision
        .as_ref()
        .is_some_and(|d| d.needed_me == Some(true))
}

/// The write a Needed me click makes: the decision's id and the new value, or `None` when the
/// box is not enabled for the row.
pub fn needed_me_write(row: &QueueRow) -> Option<(String, bool)> {
    if !needed_me_enabled(row) {
        return None;
    }
    let d = row.site.decision.as_ref()?;
    Some((d.id.clone(), d.needed_me != Some(true)))
}

/// The hint for a disabled box.
pub const NEEDED_ME_HINT: &str =
    "Decide the row first. Needed me records whether you needed to see it.";

/// The decision a row stands under, in words, for the Decision column.
pub fn decision_text(row: &QueueRow) -> String {
    crate::tree::decision_cell(&row.site).label
}

/// Whether a row's decision is `Merged` (the Needed me box still works on it).
pub fn is_merge(row: &QueueRow) -> bool {
    matches!(
        row.site.decision.as_ref().map(|d| &d.state),
        Some(DecisionState::MergedInto(_))
    )
}

// ── The site detail's surroundings ────────────────────────────────────────────────────────

/// The merge picker's empty line in the queue: opening a sub-group in the tree adds nothing to
/// the queue's pool, so the line names the pool and the way out.
pub const POOL_NONE_QUEUE: &str =
    "No escalated row matches. Open in the tree to merge into a site that is not in the queue.";

/// What the site detail needs to know about a queue row's surroundings (the tree gives it the
/// same from its index, [`crate::edits::context_for`]). The queue holds only its own rows, so
/// the rule select, the split's sub-groups and the merge picker's pool are the rows it has
/// loaded: narrower than the tree's, never wrong.
pub fn detail_context(view: &QueueView, row: &QueueRow) -> DetailContext {
    let all = || view.escalated.iter().chain(&view.below);
    let crumbs = row.node_path.iter().map(|c| c.label.clone()).collect();
    let mut rule_ids: Vec<String> = all().map(|r| r.site.rule_id.clone()).collect();
    rule_ids.push(row.site.rule_id.clone());
    rule_ids.sort();
    rule_ids.dedup();
    // The sub-groups under the row's rule: the node path ends category, rule, sub-group.
    let rule_node = row
        .node_path
        .len()
        .checked_sub(2)
        .map(|i| &row.node_path[i]);
    let own = row.node_path.last().map(|c| c.id.as_str());
    let mut subgroups: Vec<String> = Vec::new();
    if let Some(rule_node) = rule_node {
        for other in all() {
            let n = other.node_path.len();
            if n < 2 || other.node_path[n - 2].id != rule_node.id {
                continue;
            }
            let sub = &other.node_path[n - 1];
            if Some(sub.id.as_str()) != own && !subgroups.contains(&sub.label) {
                subgroups.push(sub.label.clone());
            }
        }
    }
    DetailContext {
        crumbs,
        rule_ids,
        subgroups,
        // The row carries the state of the group decision it is new since, so the queue says the
        // verb the tree says without loading the tree.
        node_decision: row
            .site
            .new_since_decision
            .as_ref()
            .and_then(|n| n.node_state.clone()),
        pool: all().map(|r| PoolSite::of(&r.site)).collect(),
        pool_none: POOL_NONE_QUEUE,
    }
}

// ── The door to the tree ──────────────────────────────────────────────────────────────────

/// The door that opens a row's site in the tree lane (Enter on a row).
pub fn site_door(row: &QueueRow) -> Door {
    Door::Site {
        key: row.candidate.key.clone(),
        rule_id: row.site.rule_id.clone(),
        node_id: row.site.group_path.get(2).cloned().unwrap_or_default(),
    }
}

/// The door that opens a row's sub-group in the tree lane (`G`, or a click on the Node cell).
pub fn node_door(row: &QueueRow) -> Option<Door> {
    node_id_of(row).map(|id| Door::Node { id: id.to_string() })
}

// ── Keys ──────────────────────────────────────────────────────────────────────────────────

/// What a key press asks for in the queue lane (spec section 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueCommand {
    Move(FocusMove),
    /// `Enter`: open the row in the tree lane's detail.
    Open,
    /// `C`, `I`, `F`, `T`: a site decision, applied now or after a reason prompt.
    Site(PaneCommand),
    /// `N`: toggle Needed me on a decided row.
    ToggleNeededMe,
    /// `G`: go to the node in the tree.
    GoToNode,
    /// `S`: open the Settings lane.
    OpenSettings,
    /// `Shift Down`: show 100 more rows (the host does it only at the last drawn row).
    ShowMore,
}

/// The queue's key map as a pure function. Same scoping as the tree and the Queue's detail pane:
/// keys fire only while the lane's region holds focus, never in a text field, never with Ctrl,
/// Alt or Cmd held.
pub fn queue_command(
    focus: PaneFocus,
    key: &str,
    mods: KeyMods,
    on_row: bool,
) -> Option<QueueCommand> {
    if focus != PaneFocus::Cursor || mods.ctrl || mods.alt || mods.meta {
        return None;
    }
    match key {
        "ArrowDown" if mods.shift => return Some(QueueCommand::ShowMore),
        "ArrowDown" => return Some(QueueCommand::Move(FocusMove::Down)),
        "ArrowUp" => return Some(QueueCommand::Move(FocusMove::Up)),
        "Enter" => return on_row.then_some(QueueCommand::Open),
        _ => {}
    }
    if key.chars().count() != 1 {
        return None;
    }
    let pressed = key.to_ascii_lowercase();
    match pressed.as_str() {
        "s" => return Some(QueueCommand::OpenSettings),
        "n" => return on_row.then_some(QueueCommand::ToggleNeededMe),
        "g" => return on_row.then_some(QueueCommand::GoToNode),
        _ => {}
    }
    let state = match pressed.as_str() {
        "c" => crate::triage::TriageState::Confirmed,
        "i" => crate::triage::TriageState::Ignored,
        "f" => crate::triage::TriageState::FalsePositive,
        "t" => crate::triage::TriageState::TechDebt,
        _ => return None,
    };
    on_row.then(|| QueueCommand::Site(PaneCommand::decide(state)))
}

/// Whether the lane's keys will fire under this focus (the armed or off indicator).
pub fn queue_keys_armed(focus: PaneFocus) -> bool {
    queue_command(focus, "ArrowDown", KeyMods::none(), true).is_some()
}

/// The indicator's text. It names only keys that act.
pub fn keys_hint(armed: bool, prompt_open: bool) -> &'static str {
    if armed {
        "Keys: \u{2191}\u{2193} move \u{b7} Enter open in the tree \u{b7} C I F T decide \u{b7} N needed me \u{b7} G go to the node \u{b7} S settings \u{b7} Shift \u{2193} show more"
    } else if prompt_open {
        "Keys are off while you type. Enter commits, Esc cancels."
    } else {
        "Keys are off. Click the queue (or Tab into it) to arm them."
    }
}

#[cfg(test)]
mod tests;
