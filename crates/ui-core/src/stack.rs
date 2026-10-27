//! The stacked group view, "same fix, many places" (`WORK_ORDER_REVIEW_SPEED.md` B.2), as pure
//! logic.
//!
//! A rule group in the Queue is often one question asked of many places: is this convention
//! real, is this pattern a bug. The stack puts every row of the group on one screen as a card
//! (`path:line`, severity, the stored line with three lines of code either side, the row's
//! status and a checkbox), so the curator reads the pattern once and decides the checked cards
//! together. Everything that decides WHICH rows are in the stack, WHICH cards are checked, WHAT a
//! key means and WHERE the cursor is lives here, with unit tests and no rendering framework:
//!
//! - [`rows_of_group`] and [`is_stackable`]: the group's rows in the table's order.
//! - [`StackState`]: the snapshot of the group, the page, the card cursor and the checkboxes.
//!   Whether a card is checked is derived, never stored: an explicit choice of the curator made
//!   under the state the row is in now, else "checked while undecided". A card that is decided
//!   (here, in the viewer, or by an undo) is therefore unchecked or checked again by the
//!   disposition map alone, with nothing to keep in step.
//! - [`stack_command`]: the stack's key map, a whitelist like `pane_command` and `viewer_command`.
//!   It asks `pane_command` for the disposition keys and maps the answer, so `C T I F` and the
//!   arrows mean what they mean everywhere else by construction.
//!
//! The decisions themselves are NOT here. They go through `TriageModel::dispose_with_undo`, the
//! entry point the pane's keys, the viewer and the Queue's bulk buttons all use, so the
//! required-reason invariant and the undo are not re-implemented for this door.

use std::collections::HashMap;

use crate::triage::{
    finding_key, finding_state, pane_command, rule_group_label, Disposition, FindingView,
    FocusMove, KeyMods, PaneCommand, PaneFocus, TriageState,
};

/// The most cards one page holds, which is the most items the batch context endpoint accepts in
/// one request (`MAX_BATCH_ITEMS` on the server). A bigger group pages.
pub const STACK_PAGE: usize = 200;

/// The reason a key that decides cards did nothing because none is checked. Said in a toast,
/// never silently: an `F` that does nothing looks like a broken key.
pub const NONE_CHECKED_NOTE: &str = "No cards are checked. Press Space to check the focused card.";

// ── Which rows are in the stack ──────────────────────────────────────────────────────────

/// Whether a row belongs to a group the stack can show: a deviation or a defect. A row with no
/// kind is an alarm about the pipeline and an informational row is not a Queue row; neither is a
/// "same fix, many places" group.
pub fn is_stackable(f: &FindingView) -> bool {
    use camerata_api_types::audit::FindingKind;
    matches!(
        f.kind,
        Some(FindingKind::Defect) | Some(FindingKind::Deviation(_))
    )
}

/// The rows of the group `label` among `ordered`, in the order given. `label` is
/// [`rule_group_label`], the value the table groups by, so a stack holds exactly the rows a
/// group header holds. `ordered` is the table's current rows in the order the table shows them.
pub fn rows_of_group(ordered: &[FindingView], label: &str) -> Vec<FindingView> {
    ordered
        .iter()
        .filter(|f| is_stackable(f) && rule_group_label(f) == label)
        .cloned()
        .collect()
}

// ── Where focus goes back to ─────────────────────────────────────────────────────────────

/// Where the stack was opened from, so `Esc` can give the keyboard back there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackOrigin {
    /// The detail pane, by `B` (or its button).
    Pane,
    /// A "Review all N stacked" button of the deviation bulk bar, by its position in the bar.
    BulkBar(usize),
}

// ── The key map ──────────────────────────────────────────────────────────────────────────

/// Where keyboard focus is inside the stack. Only [`StackFocus::Cards`] arms commands; the
/// reason prompt owns every key while it is open, so a letter typed into a reason is text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackFocus {
    Cards,
    ReasonPrompt,
}

/// What a press in the stack asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackCommand {
    /// Close the stack, back to where it was opened from.
    Close,
    /// Next or previous card.
    Move(FocusMove),
    /// Check or uncheck the focused card.
    Toggle,
    /// Open the viewer on the focused card's finding.
    Open,
    /// Apply this disposition to every checked card now (the same path as the pane).
    Dispose(TriageState),
    /// Ask for the one reason, then apply this disposition to every checked card.
    PromptFor(TriageState),
    /// Next (`Down`) or previous (`Up`) page of cards, for a group of more than [`STACK_PAGE`].
    Page(FocusMove),
}

/// THE STACK'S SCOPED KEY HANDLER, as a pure function.
///
/// A whitelist, like `pane_command` and `viewer_command`: any key not named here returns `None`,
/// and so does every key under any focus but [`StackFocus::Cards`]. Ctrl, Alt and Meta presses
/// are refused so Cmd-C still copies a snippet; shift is how a capital letter is typed.
///
/// The disposition keys and the arrows are asked of `pane_command` and mapped, so a new
/// disposition hotkey reaches the stack by being added once. `Enter` opens the viewer here
/// (there is no code selection to protect in a list of cards).
pub fn stack_command(focus: StackFocus, key: &str, mods: KeyMods) -> Option<StackCommand> {
    if focus != StackFocus::Cards || mods.ctrl || mods.alt || mods.meta {
        return None;
    }
    match key {
        "Escape" => return Some(StackCommand::Close),
        "Enter" => return Some(StackCommand::Open),
        " " => return Some(StackCommand::Toggle),
        ">" => return Some(StackCommand::Page(FocusMove::Down)),
        "<" => return Some(StackCommand::Page(FocusMove::Up)),
        _ => {}
    }
    if let Some(cmd) = pane_command(PaneFocus::Cursor, key, mods) {
        return match cmd {
            PaneCommand::Dispose(s) => Some(StackCommand::Dispose(s)),
            PaneCommand::PromptFor(s) => Some(StackCommand::PromptFor(s)),
            PaneCommand::MoveFocus(d) => Some(StackCommand::Move(d)),
            // `O` and `B` open surfaces from the pane. Inside the stack the viewer is `Enter`
            // and there is no stack above a stack.
            PaneCommand::OpenCode | PaneCommand::OpenStack => None,
        };
    }
    if key.chars().count() != 1 {
        return None;
    }
    match key.to_ascii_lowercase().as_str() {
        "j" => Some(StackCommand::Move(FocusMove::Down)),
        "k" => Some(StackCommand::Move(FocusMove::Up)),
        _ => None,
    }
}

/// The stack's focus, from the one fact the host knows: whether a reason prompt is open. THE
/// derivation: the key handler, the view's keystroke claim and every mouse door's gate
/// ([`stack_keys_armed`] of this) read it, so a click cannot do what the same press would be
/// refused.
pub fn stack_focus(prompt_open: bool) -> StackFocus {
    if prompt_open {
        StackFocus::ReasonPrompt
    } else {
        StackFocus::Cards
    }
}

/// Whether the stack answers keys under this focus: defined as "would `Esc` produce a command",
/// so what the strip tells the curator and what the handler does cannot drift apart.
pub fn stack_keys_armed(focus: StackFocus) -> bool {
    stack_command(focus, "Escape", KeyMods::none()).is_some()
}

/// One row of the stack's key list: what is shown, what it does, and the presses that prove it.
pub struct StackKeyRow {
    pub label: &'static str,
    pub action: &'static str,
    pub probes: &'static [&'static str],
}

/// The stack's keys, for its key strip and the user guide. A test checks the probes are exactly
/// the keys the map answers.
pub const STACK_KEY_ROWS: [StackKeyRow; 8] = [
    StackKeyRow {
        label: "J  K",
        action: "Next, previous card",
        probes: &["j", "k", "ArrowDown", "ArrowUp"],
    },
    StackKeyRow {
        label: "Space",
        action: "Check or uncheck the card",
        probes: &[" "],
    },
    StackKeyRow {
        label: "Enter",
        action: "Open the card in the code viewer (Esc comes back here)",
        probes: &["Enter"],
    },
    StackKeyRow {
        label: "C",
        action: "Confirm the checked cards",
        probes: &["c"],
    },
    StackKeyRow {
        label: "T",
        action: "Save the checked cards as tech debt",
        probes: &["t"],
    },
    StackKeyRow {
        label: "F  I",
        action: "False positive, ignore: one reason for all the checked cards",
        probes: &["f", "i"],
    },
    StackKeyRow {
        label: "<  >",
        action: "Previous, next page (a group of more than 200 pages)",
        probes: &["<", ">"],
    },
    StackKeyRow {
        label: "Esc",
        action: "Close, back to where the stack was opened from",
        probes: &["Escape"],
    },
];

// ── The state ────────────────────────────────────────────────────────────────────────────

/// The stack's state: a snapshot of the group's rows taken when it opened, the page, the card
/// cursor and the curator's explicit checkbox choices.
///
/// The rows are a SNAPSHOT: deciding cards updates their status in place and the stack stays
/// open, so a card must not vanish when its row leaves the Queue table. What a card shows about
/// its decision is read from the disposition map each time, never from the snapshot.
#[derive(Clone, PartialEq)]
pub struct StackState {
    label: String,
    rows: Vec<FindingView>,
    page: usize,
    /// Index of the focused card within the page.
    cursor: usize,
    /// Explicit checkbox choices by finding key, each with the state the row was in when the
    /// curator made it. A key not here follows the default, and so does a key whose row has been
    /// decided somewhere else since: the choice was about a different decision.
    overrides: HashMap<String, (bool, TriageState)>,
}

impl StackState {
    /// A stack over `rows` (the group, in the table's order), with the cursor on the row keyed
    /// `anchor` when there is one (the finding the pane was on), else on the first card.
    pub fn new(label: impl Into<String>, rows: Vec<FindingView>, anchor: Option<&str>) -> Self {
        let at = anchor
            .and_then(|a| rows.iter().position(|f| finding_key(f) == a))
            .unwrap_or(0);
        Self {
            label: label.into(),
            rows,
            page: at / STACK_PAGE,
            cursor: at % STACK_PAGE,
            overrides: HashMap::new(),
        }
    }

    /// The group's header line ([`rule_group_label`]).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Every row of the group, on every page.
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// The current page, from 0.
    pub fn page(&self) -> usize {
        self.page
    }

    /// How many pages the group has (at least 1).
    pub fn page_count(&self) -> usize {
        self.rows.len().div_ceil(STACK_PAGE).max(1)
    }

    /// Whether the group is bigger than one page.
    pub fn is_paged(&self) -> bool {
        self.rows.len() > STACK_PAGE
    }

    /// The half-open range of `rows` the current page shows.
    fn page_range(&self) -> std::ops::Range<usize> {
        let from = (self.page * STACK_PAGE).min(self.rows.len());
        from..(from + STACK_PAGE).min(self.rows.len())
    }

    /// The rows of the current page: the cards on screen.
    pub fn page_rows(&self) -> &[FindingView] {
        &self.rows[self.page_range()]
    }

    /// What a group of more than one page tells the curator, or `None` for a single page. It
    /// says the group is bigger than what is on screen and that the keys act on this page only.
    pub fn page_note(&self) -> Option<String> {
        if !self.is_paged() {
            return None;
        }
        let range = self.page_range();
        Some(format!(
            "Showing cards {} to {} of {}. The keys act on the cards of this page. \
             Press > for the next page, < for the previous.",
            range.start + 1,
            range.end,
            self.rows.len()
        ))
    }

    /// Index of the focused card within the page.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The focused card's row.
    pub fn cursor_row(&self) -> Option<&FindingView> {
        self.page_rows().get(self.cursor)
    }

    /// The focused card's finding key.
    pub fn cursor_key(&self) -> Option<String> {
        self.cursor_row().map(finding_key)
    }

    /// Move the card cursor one card, clamped at both ends of the page (no wrap-around, for the
    /// reason `RowCursor::moved` gives: a cursor that silently restarts looks like one that
    /// stopped).
    pub fn move_cursor(&mut self, dir: FocusMove) {
        let last = self.page_rows().len().saturating_sub(1);
        self.cursor = match dir {
            FocusMove::Up => self.cursor.saturating_sub(1),
            FocusMove::Down => (self.cursor + 1).min(last),
        };
    }

    /// Put the cursor on the card keyed `key` (on whichever page holds it). `false` when the
    /// group has no such row.
    pub fn focus_key(&mut self, key: &str) -> bool {
        match self.rows.iter().position(|f| finding_key(f) == key) {
            Some(at) => {
                self.page = at / STACK_PAGE;
                self.cursor = at % STACK_PAGE;
                true
            }
            None => false,
        }
    }

    /// Go to the next or previous page, cursor on its first card. `false` at either end.
    pub fn turn_page(&mut self, dir: FocusMove) -> bool {
        let next = match dir {
            FocusMove::Down if self.page + 1 < self.page_count() => self.page + 1,
            FocusMove::Up if self.page > 0 => self.page - 1,
            _ => return false,
        };
        self.page = next;
        self.cursor = 0;
        true
    }

    /// Whether `f`'s card is checked: the curator's explicit choice, else checked while the row
    /// is undecided. A decided row defaults to unchecked, so the default never sweeps a decision
    /// the curator already made back into the next bulk action.
    ///
    /// An explicit choice counts only while the row is in the state it was made under. Decide the
    /// card somewhere else (the viewer, the pane, an undo) and the choice is discarded, so a
    /// "checked" made on an Unresolved card cannot carry over to the same card once Confirmed and
    /// sweep it into the next action. No bookkeeping call keeps this in step: it is derived.
    pub fn is_checked(&self, f: &FindingView, dispositions: &HashMap<String, Disposition>) -> bool {
        let now = finding_state(dispositions, f);
        match self.overrides.get(&finding_key(f)) {
            Some((checked, made_under)) if *made_under == now => *checked,
            _ => now == TriageState::Unresolved,
        }
    }

    /// Check or uncheck the focused card. `false` when there is no card.
    pub fn toggle_cursor(&mut self, dispositions: &HashMap<String, Disposition>) -> bool {
        let Some(f) = self.cursor_row().cloned() else {
            return false;
        };
        let now = self.is_checked(&f, dispositions);
        let under = finding_state(dispositions, &f);
        self.overrides.insert(finding_key(&f), (!now, under));
        true
    }

    /// The checked cards of the current page, in card order. Decisions act on these and only
    /// these: a card on another page is not on screen, and a key must not decide what the
    /// curator cannot see.
    pub fn checked_rows(&self, dispositions: &HashMap<String, Disposition>) -> Vec<FindingView> {
        self.page_rows()
            .iter()
            .filter(|f| self.is_checked(f, dispositions))
            .cloned()
            .collect()
    }

    /// The summary line under the header: how many cards, how many checked.
    pub fn summary(&self, dispositions: &HashMap<String, Disposition>) -> String {
        let cards = self.page_rows().len();
        let checked = self.checked_rows(dispositions).len();
        let noun = if cards == 1 { "card" } else { "cards" };
        format!("{cards} {noun} on screen, {checked} checked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::deviation_groups;

    fn finding(path: &str, line: usize, kind: serde_json::Value) -> FindingView {
        let mut v = serde_json::json!({
            "repo": "me/api", "path": path, "line": line, "rule_id": "R-1",
            "severity": "high", "snippet": format!("line {line}"), "detail": "d",
            "status": "active"
        });
        for (k, val) in kind.as_object().expect("kind object") {
            v[k] = val.clone();
        }
        serde_json::from_value(v).expect("fixture")
    }

    fn defect(path: &str, line: usize) -> FindingView {
        finding(path, line, serde_json::json!({"kind": "defect"}))
    }

    fn deviation(path: &str, line: usize) -> FindingView {
        finding(
            path,
            line,
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 3, "assessed_sites": 40,
                "convention_label": "URI versioning"
            }),
        )
    }

    fn defects(n: usize) -> Vec<FindingView> {
        (1..=n).map(|i| defect("src/a.rs", i)).collect()
    }

    fn keys(rows: &[FindingView]) -> Vec<String> {
        rows.iter().map(finding_key).collect()
    }

    fn none() -> KeyMods {
        KeyMods::none()
    }

    fn map() -> HashMap<String, Disposition> {
        HashMap::new()
    }

    fn decide(m: &mut HashMap<String, Disposition>, f: &FindingView, state: TriageState) {
        m.entry(finding_key(f)).or_default().state = state;
    }

    // ── rows ──

    #[test]
    fn a_group_holds_the_rows_the_header_holds_in_the_tables_order() {
        let a = deviation("src/z.rs", 9);
        let b = defect("src/y.rs", 5);
        let c = deviation("src/a.rs", 1);
        let label = rule_group_label(&a);
        assert_eq!(rule_group_label(&c), label);
        let got = rows_of_group(&[a.clone(), b.clone(), c.clone()], &label);
        assert_eq!(
            keys(&got),
            keys(&[a, c]),
            "the order given, the other kind left out"
        );
        // A defect group is keyed by the bare rule id.
        assert_eq!(
            keys(&rows_of_group(std::slice::from_ref(&b), "R-1")),
            keys(&[b])
        );
        // The label is what the bulk bar's groups carry too.
        let groups = deviation_groups(&[deviation("x.rs", 1)]);
        assert_eq!(groups[0].label, rule_group_label(&groups[0].rows[0]));
    }

    #[test]
    fn only_defects_and_deviations_are_stackable() {
        assert!(is_stackable(&defect("a.rs", 1)));
        assert!(is_stackable(&deviation("a.rs", 1)));
        let no_kind: FindingView = serde_json::from_value(serde_json::json!({
            "repo": "o/r", "path": "a.rs", "line": 1, "rule_id": "R", "severity": "low",
            "snippet": "s", "detail": "d"
        }))
        .unwrap();
        assert!(!is_stackable(&no_kind), "an unspecified kind is an alarm");
        assert!(
            rows_of_group(std::slice::from_ref(&no_kind), &rule_group_label(&no_kind)).is_empty(),
            "and never in a stack"
        );
    }

    // ── checkboxes ──

    #[test]
    fn undecided_cards_are_checked_by_default_and_decided_ones_are_not() {
        let rows = defects(3);
        let stack = StackState::new("R-1", rows.clone(), None);
        let mut m = map();
        assert!(rows.iter().all(|f| stack.is_checked(f, &m)));
        decide(&mut m, &rows[1], TriageState::Confirmed);
        assert!(stack.is_checked(&rows[0], &m));
        assert!(
            !stack.is_checked(&rows[1], &m),
            "decided: shows state, unchecked"
        );
        assert_eq!(
            keys(&stack.checked_rows(&m)),
            keys(&[rows[0].clone(), rows[2].clone()])
        );
    }

    #[test]
    fn space_flips_the_focused_card_and_only_that_one() {
        let rows = defects(3);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        let m = map();
        assert!(stack.toggle_cursor(&m));
        assert!(!stack.is_checked(&rows[0], &m));
        assert!(stack.is_checked(&rows[1], &m));
        stack.move_cursor(FocusMove::Down);
        assert!(stack.toggle_cursor(&m));
        assert!(stack.toggle_cursor(&m), "twice is back to checked");
        assert_eq!(
            keys(&stack.checked_rows(&m)),
            keys(&[rows[1].clone(), rows[2].clone()])
        );
    }

    #[test]
    fn a_decided_card_can_be_checked_on_purpose() {
        let rows = defects(2);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        let mut m = map();
        decide(&mut m, &rows[0], TriageState::Confirmed);
        assert!(
            !stack.is_checked(&rows[0], &m),
            "decided: unchecked by default"
        );
        assert!(stack.toggle_cursor(&m));
        assert!(stack.is_checked(&rows[0], &m), "an explicit choice wins");
    }

    /// A choice is about the decision the card was in when it was made. Decide the card
    /// somewhere else and the choice is gone, with no call to keep it in step.
    #[test]
    fn an_explicit_choice_does_not_survive_a_decision_made_elsewhere() {
        let rows = defects(2);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        let mut m = map();
        // Checked on purpose while Unresolved is the default; an explicit CHECK is the case at
        // risk, so first uncheck then check again (two explicit choices under Unresolved).
        assert!(stack.toggle_cursor(&m));
        assert!(stack.toggle_cursor(&m));
        assert!(stack.is_checked(&rows[0], &m));
        // The viewer confirms it.
        decide(&mut m, &rows[0], TriageState::Confirmed);
        assert!(
            !stack.is_checked(&rows[0], &m),
            "the old choice no longer applies: decided, unchecked"
        );
        assert!(!keys(&stack.checked_rows(&m)).contains(&finding_key(&rows[0])));
        // An explicit UNcheck survives while the state is the same, and is dropped when it moves.
        let mut other = StackState::new("R-1", rows.clone(), None);
        let mut m2 = map();
        assert!(other.toggle_cursor(&m2));
        assert!(!other.is_checked(&rows[0], &m2));
        decide(&mut m2, &rows[0], TriageState::TechDebt);
        m2.remove(&finding_key(&rows[0])); // an undo
        assert!(
            !other.is_checked(&rows[0], &m2),
            "back under the state the choice was made in: it applies again"
        );
        decide(&mut m2, &rows[0], TriageState::Ignored);
        assert!(!other.is_checked(&rows[0], &m2), "decided: the default");
    }

    // ── the decisions ──

    /// The stack decides through `dispose_with_undo`, the entry point every door uses. For a
    /// group it must leave exactly the state deciding each row on its own leaves, and the
    /// required-reason invariant must hold at this door.
    #[test]
    fn deciding_checked_cards_equals_deciding_each_row_and_keeps_the_reason_invariant() {
        use crate::triage::TriageModel;
        let rows: Vec<FindingView> = (1..=4).map(|i| deviation("src/a.rs", i)).collect();
        let group = deviation_groups(&rows).remove(0);

        let mut bulk = TriageModel::new();
        for f in &group.rows {
            bulk.confirm(std::slice::from_ref(f));
        }
        let mut stacked = TriageModel::new();
        let entry = stacked
            .dispose_with_undo(&group.rows, TriageState::Confirmed, "", 0)
            .expect("confirming needs no reason");
        assert_eq!(stacked.dispositions, bulk.dispositions);
        assert_eq!(
            entry.message(),
            "Confirmed 4 findings.",
            "the toast names the count"
        );

        let mut bulk = TriageModel::new();
        for f in &group.rows {
            bulk.mark_false_positive(std::slice::from_ref(f), "parser bug");
        }
        let mut stacked = TriageModel::new();
        for blank in ["", "   "] {
            assert!(
                stacked
                    .dispose_with_undo(&group.rows, TriageState::FalsePositive, blank, 0)
                    .is_none(),
                "a blank reason is refused"
            );
            assert!(stacked.dispositions.is_empty(), "and nothing is written");
        }
        stacked
            .dispose_with_undo(&group.rows, TriageState::FalsePositive, "parser bug", 0)
            .expect("a reason is given");
        assert_eq!(stacked.dispositions, bulk.dispositions);
    }

    #[test]
    fn unchecked_cards_stay_unresolved_when_the_checked_ones_are_confirmed() {
        use crate::triage::TriageModel;
        let rows = defects(5);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        let mut model = TriageModel::new();
        stack.toggle_cursor(&model.dispositions); // uncheck card 1
        let checked = stack.checked_rows(&model.dispositions);
        assert_eq!(checked.len(), 4);
        let entry = model
            .dispose_with_undo(&checked, TriageState::Confirmed, "", 0)
            .unwrap();
        assert_eq!(entry.message(), "Confirmed 4 findings.");
        assert_eq!(model.state_of(&rows[0]), TriageState::Unresolved);
        for f in &rows[1..] {
            assert_eq!(model.state_of(f), TriageState::Confirmed);
        }
        // Decided cards read unchecked; the card left alone is still the curator's choice.
        assert!(stack.checked_rows(&model.dispositions).is_empty());
    }

    // ── cursor, pages ──

    #[test]
    fn the_cursor_starts_on_the_anchor_and_clamps_at_both_ends() {
        let rows = defects(5);
        let anchor = finding_key(&rows[3]);
        let mut stack = StackState::new("R-1", rows.clone(), Some(&anchor));
        assert_eq!(stack.cursor_key(), Some(anchor));
        for _ in 0..10 {
            stack.move_cursor(FocusMove::Down);
        }
        assert_eq!(stack.cursor(), 4, "no wrap past the last card");
        for _ in 0..10 {
            stack.move_cursor(FocusMove::Up);
        }
        assert_eq!(stack.cursor(), 0);
        // An anchor that is not in the group is the first card.
        let unknown = StackState::new("R-1", rows, Some("not a key"));
        assert_eq!(unknown.cursor(), 0);
    }

    #[test]
    fn a_group_over_the_cap_pages_and_says_so() {
        let rows = defects(STACK_PAGE + 112);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        assert!(stack.is_paged());
        assert_eq!(stack.page_count(), 2);
        assert_eq!(stack.page_rows().len(), STACK_PAGE);
        let note = stack.page_note().expect("a paged group says so");
        assert!(
            note.contains("1 to 200 of 312") && note.contains("this page"),
            "{note}"
        );
        assert!(!note.contains('\u{2014}'), "no em-dash in user text");
        assert!(stack.turn_page(FocusMove::Down));
        assert_eq!(stack.page_rows().len(), 112);
        assert_eq!(
            finding_key(&stack.page_rows()[0]),
            finding_key(&rows[STACK_PAGE])
        );
        assert!(stack.page_note().unwrap().contains("201 to 312 of 312"));
        assert!(!stack.turn_page(FocusMove::Down), "no page past the last");
        assert!(stack.turn_page(FocusMove::Up));
        assert!(!stack.turn_page(FocusMove::Up), "none before the first");
        // A group that fits says nothing.
        assert_eq!(
            StackState::new("R-1", defects(STACK_PAGE), None).page_note(),
            None
        );
        assert_eq!(StackState::new("R-1", defects(0), None).page_count(), 1);
    }

    #[test]
    fn decisions_act_on_the_checked_cards_of_the_page_on_screen_only() {
        let rows = defects(STACK_PAGE + 3);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        let m = map();
        assert_eq!(stack.checked_rows(&m).len(), STACK_PAGE);
        stack.turn_page(FocusMove::Down);
        assert_eq!(keys(&stack.checked_rows(&m)), keys(&rows[STACK_PAGE..]));
        // An anchor on the second page opens there.
        let late = finding_key(&rows[STACK_PAGE + 1]);
        let opened = StackState::new("R-1", rows, Some(&late));
        assert_eq!((opened.page(), opened.cursor()), (1, 1));
    }

    #[test]
    fn focus_key_finds_the_card_on_any_page() {
        let rows = defects(STACK_PAGE + 3);
        let mut stack = StackState::new("R-1", rows.clone(), None);
        assert!(stack.focus_key(&finding_key(&rows[STACK_PAGE + 2])));
        assert_eq!((stack.page(), stack.cursor()), (1, 2));
        assert!(!stack.focus_key("nope"));
        assert_eq!(
            (stack.page(), stack.cursor()),
            (1, 2),
            "unchanged on a miss"
        );
    }

    #[test]
    fn the_summary_counts_cards_and_checks() {
        let rows = defects(3);
        let mut stack = StackState::new("R-1", rows, None);
        let m = map();
        assert_eq!(stack.summary(&m), "3 cards on screen, 3 checked");
        stack.toggle_cursor(&m);
        assert_eq!(stack.summary(&m), "3 cards on screen, 2 checked");
        assert_eq!(
            StackState::new("R-1", defects(1), None).summary(&m),
            "1 card on screen, 1 checked"
        );
    }

    // ── the key map ──

    fn mods(ctrl: bool, alt: bool, meta: bool, shift: bool) -> KeyMods {
        KeyMods {
            ctrl,
            alt,
            meta,
            shift,
        }
    }

    #[test]
    fn the_keys_mean_what_the_work_order_says() {
        use StackCommand::*;
        let f = StackFocus::Cards;
        for (key, want) in [
            ("Escape", Close),
            ("Enter", Open),
            (" ", Toggle),
            ("j", Move(FocusMove::Down)),
            ("J", Move(FocusMove::Down)),
            ("k", Move(FocusMove::Up)),
            ("K", Move(FocusMove::Up)),
            ("ArrowDown", Move(FocusMove::Down)),
            ("ArrowUp", Move(FocusMove::Up)),
            ("c", Dispose(TriageState::Confirmed)),
            ("C", Dispose(TriageState::Confirmed)),
            ("t", Dispose(TriageState::TechDebt)),
            ("f", PromptFor(TriageState::FalsePositive)),
            ("i", PromptFor(TriageState::Ignored)),
            (">", Page(FocusMove::Down)),
            ("<", Page(FocusMove::Up)),
        ] {
            assert_eq!(stack_command(f, key, none()), Some(want), "{key}");
        }
    }

    /// The reason-carrying keys PROMPT; they never decide. The invariant is the model's, and
    /// the key map must not offer a way around it.
    #[test]
    fn the_keys_that_need_a_reason_prompt_and_the_rest_apply() {
        for state in TriageState::all() {
            let Some(hotkey) = crate::triage::state_hotkey(state) else {
                continue;
            };
            let cmd = stack_command(StackFocus::Cards, hotkey, none()).expect("a hotkey");
            assert_eq!(
                matches!(cmd, StackCommand::PromptFor(_)),
                state.requires_reason(),
                "{}",
                state.label()
            );
        }
    }

    #[test]
    fn nothing_is_answered_while_the_reason_prompt_owns_the_keyboard() {
        let universe = key_universe();
        for key in &universe {
            assert_eq!(
                stack_command(StackFocus::ReasonPrompt, key, none()),
                None,
                "{key} is text in the prompt"
            );
        }
        assert!(!stack_keys_armed(StackFocus::ReasonPrompt));
        assert!(stack_keys_armed(StackFocus::Cards));
        assert_eq!(stack_focus(true), StackFocus::ReasonPrompt);
        assert_eq!(stack_focus(false), StackFocus::Cards);
    }

    #[test]
    fn a_modifier_press_is_never_a_command() {
        let universe = key_universe();
        for m in [
            mods(true, false, false, false),
            mods(false, true, false, false),
            mods(false, false, true, false),
            mods(true, true, true, true),
        ] {
            for key in &universe {
                assert_eq!(
                    stack_command(StackFocus::Cards, key, m),
                    None,
                    "{key} with {m:?}: Cmd-C must still copy"
                );
            }
        }
        // Shift is how a capital letter is typed.
        assert_eq!(
            stack_command(StackFocus::Cards, "C", mods(false, false, false, true)),
            Some(StackCommand::Dispose(TriageState::Confirmed))
        );
    }

    /// Every key a keyboard can send that this test knows about.
    fn key_universe() -> Vec<String> {
        let mut keys: Vec<String> = (0x20u8..=0x7e).map(|b| (b as char).to_string()).collect();
        for named in [
            "Escape",
            "Enter",
            "Tab",
            "Backspace",
            "Delete",
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "PageUp",
            "PageDown",
            "Home",
            "End",
            "Shift",
            "Control",
            "Alt",
            "Meta",
            "F1",
            "F5",
        ] {
            keys.push(named.to_string());
        }
        keys
    }

    fn answered(f: impl Fn(&str) -> bool) -> Vec<String> {
        let mut out: Vec<String> = key_universe()
            .into_iter()
            .filter(|k| f(k))
            .map(|k| {
                if k.chars().count() == 1 {
                    k.to_ascii_lowercase()
                } else {
                    k
                }
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn the_key_list_is_exactly_the_keys_the_map_answers() {
        let mut listed: Vec<String> = Vec::new();
        for row in &STACK_KEY_ROWS {
            assert!(!row.label.is_empty() && !row.action.is_empty());
            assert!(!row.action.contains('\u{2014}'), "no em-dash in user text");
            for key in row.probes {
                assert!(
                    stack_command(StackFocus::Cards, key, none()).is_some(),
                    "{} lists {key} but the map ignores it",
                    row.label
                );
                listed.push((*key).to_string());
            }
        }
        listed.sort();
        listed.dedup();
        let got = answered(|k| stack_command(StackFocus::Cards, k, none()).is_some());
        assert_eq!(got, listed);
    }

    /// The shared-key assertions, extended to the third surface. The stack shares with the pane
    /// exactly the disposition keys and the arrows; with the viewer, additionally `J K`, `Space`
    /// (a different action there) and `Esc`; and nothing else. The surfaces are never armed at
    /// once (the viewer is a layer above the stack), so a shared key never fires twice.
    #[test]
    fn the_stack_shares_only_the_listed_keys_with_the_pane_and_the_viewer() {
        use crate::viewer::{viewer_command, ViewerFocus, SHARED_KEYS};
        let stack = |k: &str| stack_command(StackFocus::Cards, k, none()).is_some();
        let pane = |k: &str| crate::triage::pane_command(PaneFocus::Cursor, k, none()).is_some();
        let viewer = |k: &str| viewer_command(ViewerFocus::Code, k, none()).is_some();

        let with_pane = answered(|k| stack(k) && pane(k));
        let mut want_pane: Vec<String> = SHARED_KEYS.iter().map(|k| (*k).to_string()).collect();
        want_pane.sort();
        assert_eq!(with_pane, want_pane, "the stack and the pane");

        let with_viewer = answered(|k| stack(k) && viewer(k));
        let mut want_viewer: Vec<String> = SHARED_KEYS.iter().map(|k| (*k).to_string()).collect();
        want_viewer.extend(["j", "k", " ", "Escape"].map(String::from));
        want_viewer.sort();
        assert_eq!(with_viewer, want_viewer, "the stack and the viewer");

        // The pane's `B` and `O` are the pane's alone.
        assert!(pane("b") && !stack("b") && !viewer("b"));
        assert!(pane("o") && !stack("o") && !viewer("o"));
    }

    #[test]
    fn keys_the_work_order_leaves_unbound_stay_unbound() {
        // No U (an undo must not be a stray key), no N/P (ambiguous), no A.
        for key in [
            "u",
            "U",
            "n",
            "N",
            "p",
            "P",
            "a",
            "A",
            "Tab",
            "Backspace",
            "o",
            "b",
        ] {
            assert_eq!(stack_command(StackFocus::Cards, key, none()), None, "{key}");
        }
    }
}
