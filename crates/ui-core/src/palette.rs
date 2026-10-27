//! The review command palette (`WORK_ORDER_REVIEW_SPEED.md` B.8, code viewer step 7d), as pure
//! logic.
//!
//! One list of the review actions that already exist, reachable from the keyboard and filtered as
//! the reviewer types. The palette adds no action of its own. Each entry names an action that
//! something else already performs, and the Dioxus layer runs it through the same function that
//! thing calls:
//!
//! - A pane entry (decide the focused row, move the cursor, open the code viewer, stack the
//!   group) carries the [`PaneCommand`] its hotkey produces. [`pane_entries`] asks
//!   [`pane_command`] for it with the hotkey, so the entry and the key are the same value by
//!   construction, and a test pins that no hotkey is left unresolved.
//! - The rest (undo, the pane's rule and detail buttons, the Order select, the timer's Pause /
//!   Resume button, the strip's stats button) are [`PaletteAction`] variants naming that control.
//!
//! What this module owns: which entries exist for the state of the review ([`entries`]), how a
//! typed query narrows them ([`filter`]), the cursor in the narrowed list ([`PaletteState`]),
//! what a key means inside the palette ([`palette_key`]), the key that opens it
//! ([`palette_open_key`]) and the gate that refuses it ([`palette_gate`]). It writes nothing and
//! decides nothing about a finding.
//!
//! # The gate
//!
//! Like the stacked view's `stack_focus`, the palette is refused while a reason prompt is open.
//! A reason prompt owns the keyboard and its row set is a promise the heading has made; a palette
//! that ran a decision, moved the cursor or switched the order from beside it would break that
//! promise. It is refused over the code viewer and the stacked view too: they are layers that own
//! the keyboard, and the palette's entries act on the Queue underneath.

use crate::order_preset::OrderPreset;
use crate::triage::{
    pane_command, state_hotkey, FocusMove, KeyMods, PaneCommand, PaneFocus, TriageState,
};

// ── The gate ─────────────────────────────────────────────────────────────────────────────

/// Whether the palette may open now, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteGate {
    /// Nothing owns the keyboard: open it.
    Open,
    /// A reason prompt (the pane's or the toolbar's) is open and owns the keyboard.
    ReasonPrompt,
    /// The code viewer or the stacked view is open over the Queue.
    Overlay,
}

/// THE PALETTE'S GATE, as a pure function of what is open. A reason prompt is named before an
/// overlay, so the reviewer is told about the one that has something half-written in it.
pub fn palette_gate(reason_prompt_open: bool, overlay_open: bool) -> PaletteGate {
    if reason_prompt_open {
        PaletteGate::ReasonPrompt
    } else if overlay_open {
        PaletteGate::Overlay
    } else {
        PaletteGate::Open
    }
}

impl PaletteGate {
    /// Whether the palette may open.
    pub fn is_open(self) -> bool {
        self == Self::Open
    }

    /// What to tell the reviewer when the palette was asked for and refused. `None` when open.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::ReasonPrompt => {
                Some("Finish or cancel the reason first, then open the command palette.")
            }
            Self::Overlay => Some(
                "Close the code viewer or the stacked view first, then open the command palette.",
            ),
        }
    }
}

// ── The keys ─────────────────────────────────────────────────────────────────────────────

/// Whether this press asks for the palette: Ctrl+K or Cmd+K (a letter either case, so Caps Lock
/// does not matter), with no Alt. Plain letters are the pane's and Ctrl / Cmd chords are not
/// (`pane_command` refuses them), so the chord can never be read as a disposition.
pub fn palette_open_key(key: &str, mods: KeyMods) -> bool {
    (mods.ctrl || mods.meta) && !mods.alt && key.eq_ignore_ascii_case("k")
}

/// What a press inside the palette's field asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKey {
    /// Close it and give the keyboard back to where it was.
    Close,
    /// Move the highlight.
    Move(FocusMove),
    /// Run the highlighted entry.
    Run,
}

/// THE PALETTE'S KEY MAP, a whitelist like the pane's, the viewer's and the stack's: any key not
/// named here is `None` and is typed into the field (or ignored). The open chord closes it again.
pub fn palette_key(key: &str, mods: KeyMods) -> Option<PaletteKey> {
    if palette_open_key(key, mods) {
        return Some(PaletteKey::Close);
    }
    if mods.ctrl || mods.alt || mods.meta {
        return None;
    }
    match key {
        "Escape" => Some(PaletteKey::Close),
        "Enter" => Some(PaletteKey::Run),
        "ArrowDown" => Some(PaletteKey::Move(FocusMove::Down)),
        "ArrowUp" => Some(PaletteKey::Move(FocusMove::Up)),
        _ => None,
    }
}

// ── The entries ──────────────────────────────────────────────────────────────────────────

/// What running an entry does. Every variant names something that already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    /// A pane command: what the pane does for the hotkey that produces the same value.
    Pane(PaneCommand),
    /// The pane's Undo button.
    Undo,
    /// The pane's rule id button: the Rule Hub on the focused finding's rule.
    OpenHub,
    /// The pane's Full detail button.
    FullDetail,
    /// The Order select, set to this cut.
    SetOrder(OrderPreset),
    /// The strip's Pause / Resume button.
    ToggleTimer,
    /// The strip's Review time button.
    OpenStats,
}

impl PaletteAction {
    /// Whether the keyboard should go back to where it was after this action ran.
    ///
    /// False for an action that opens something which takes the keyboard itself (the code
    /// viewer, the stacked view, a reason prompt, the hub, the detail modal, the stats view):
    /// handing focus back after those would take it from the thing just opened.
    pub fn returns_focus(self) -> bool {
        match self {
            Self::Pane(PaneCommand::Dispose(_) | PaneCommand::MoveFocus(_)) => true,
            Self::Pane(
                PaneCommand::PromptFor(_) | PaneCommand::OpenCode | PaneCommand::OpenStack,
            ) => false,
            Self::Undo | Self::SetOrder(_) | Self::ToggleTimer => true,
            Self::OpenHub | Self::FullDetail | Self::OpenStats => false,
        }
    }
}

/// One line of the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    pub action: PaletteAction,
    /// What the line says.
    pub label: String,
    /// The key that does the same thing elsewhere, shown at the right (`C`, `Down`), or empty.
    pub hint: String,
    /// Extra words the filter matches besides the label (`fp`, `next`, `debt`).
    pub keywords: &'static str,
}

/// What the entries depend on: only the state of the review, never a finding's content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteContext {
    /// A row is focused: the entries that act on it exist.
    pub has_focus: bool,
    /// The focused row belongs to a group the stacked view can show.
    pub stackable: bool,
    /// The five-second undo is live (the pane is showing its Undo button).
    pub can_undo: bool,
    /// The cut the Queue is in now, left out of the Order entries.
    pub order: OrderPreset,
    /// The timer's button word (`Pause` or `Resume`), or `None` when this surface has no timer.
    pub timer_button: Option<String>,
    /// The strip's stats button exists.
    pub has_stats: bool,
}

/// A pane hotkey, the line it gets, and the words it also answers to. The action is not written
/// here: it is whatever [`pane_command`] returns for the key.
struct PaneSpec {
    key: &'static str,
    label: &'static str,
    keywords: &'static str,
    /// Only when the focused row can be stacked.
    needs_stackable: bool,
}

/// The pane's keys that decide, move and open, in the order the palette lists them. The four
/// dispositions come first and are derived from [`TriageState`] (below), so this list holds the
/// rest.
const PANE_SPECS: [PaneSpec; 4] = [
    PaneSpec {
        key: "ArrowDown",
        label: "Next finding",
        keywords: "down move cursor",
        needs_stackable: false,
    },
    PaneSpec {
        key: "ArrowUp",
        label: "Previous finding",
        keywords: "up move cursor back",
        needs_stackable: false,
    },
    PaneSpec {
        key: "o",
        label: "Open in the code viewer",
        keywords: "source file view",
        needs_stackable: false,
    },
    PaneSpec {
        key: "b",
        label: "Review this group stacked",
        keywords: "bulk cards same fix",
        needs_stackable: true,
    },
];

/// The words each disposition's line also answers to.
fn decision_keywords(state: TriageState) -> &'static str {
    match state {
        TriageState::Confirmed => "accept real yes",
        TriageState::Ignored => "skip reason",
        TriageState::TechDebt => "debt later ticket",
        TriageState::FalsePositive => "fp wrong noise reason",
        TriageState::Unresolved => "",
    }
}

/// The line for a disposition.
fn decision_label(state: TriageState) -> String {
    match state {
        TriageState::Confirmed => "Confirm this finding".to_string(),
        TriageState::Ignored => "Ignore this finding (asks for a reason)".to_string(),
        TriageState::TechDebt => "Save this finding as tech debt".to_string(),
        TriageState::FalsePositive => {
            "Mark this finding a false positive (asks for a reason)".to_string()
        }
        TriageState::Unresolved => "Move this finding to Unresolved".to_string(),
    }
}

/// How a key is shown in a hint: a letter in capitals, an arrow by its direction.
fn key_hint(key: &str) -> String {
    match key.strip_prefix("Arrow") {
        Some(direction) => direction.to_string(),
        None => key.to_uppercase(),
    }
}

/// The pane entries: each is the command the pane's own key map returns for the key, never a
/// second description of what the key does. A key the map no longer knows yields no entry (and a
/// test fails), so an entry can never run something the key would not.
pub fn pane_entries(stackable: bool) -> Vec<PaletteEntry> {
    let decisions = TriageState::all()
        .into_iter()
        .filter(|s| s.is_decided())
        .filter_map(|state| {
            let key = state_hotkey(state)?;
            Some(PaletteEntry {
                action: PaletteAction::Pane(pane_command(PaneFocus::Cursor, key, KeyMods::none())?),
                label: decision_label(state),
                hint: key_hint(key),
                keywords: decision_keywords(state),
            })
        });
    let rest = PANE_SPECS
        .iter()
        .filter(|spec| stackable || !spec.needs_stackable)
        .filter_map(|spec| {
            Some(PaletteEntry {
                action: PaletteAction::Pane(pane_command(
                    PaneFocus::Cursor,
                    spec.key,
                    KeyMods::none(),
                )?),
                label: spec.label.to_string(),
                hint: key_hint(spec.key),
                keywords: spec.keywords,
            })
        });
    decisions.chain(rest).collect()
}

fn entry(action: PaletteAction, label: &str, keywords: &'static str) -> PaletteEntry {
    PaletteEntry {
        action,
        label: label.to_string(),
        hint: String::new(),
        keywords,
    }
}

/// Every entry for this state of the review, in the order they are listed when nothing is typed.
/// An entry whose control is not on screen does not exist: no line the reviewer can pick that
/// would do nothing.
pub fn entries(ctx: &PaletteContext) -> Vec<PaletteEntry> {
    let mut out = Vec::new();
    if ctx.has_focus {
        out.extend(pane_entries(ctx.stackable));
        out.push(entry(
            PaletteAction::FullDetail,
            "Show the full detail of this finding",
            "modal enclosing block",
        ));
        out.push(entry(
            PaletteAction::OpenHub,
            "Open the Rule Hub for this finding's rule",
            "rule convention",
        ));
    }
    if ctx.can_undo {
        out.push(entry(
            PaletteAction::Undo,
            "Undo the last decision",
            "revert back",
        ));
    }
    for preset in OrderPreset::ALL {
        if preset == ctx.order {
            continue;
        }
        out.push(PaletteEntry {
            action: PaletteAction::SetOrder(preset),
            label: format!("Order the Queue: {}", preset.label()),
            hint: String::new(),
            keywords: "sort cut group",
        });
    }
    if let Some(button) = &ctx.timer_button {
        out.push(PaletteEntry {
            action: PaletteAction::ToggleTimer,
            label: format!("{button} the review timer"),
            hint: String::new(),
            keywords: "pause resume stop start clock",
        });
    }
    if ctx.has_stats {
        out.push(entry(
            PaletteAction::OpenStats,
            "Show the review time",
            "stats hours minutes",
        ));
    }
    out
}

// ── Filtering ────────────────────────────────────────────────────────────────────────────

/// The entries that match `query`, in the order given (stable, so the list does not reshuffle
/// while the reviewer types).
///
/// Every word of the query must appear, in either case, in the entry's label, its hint or its
/// keywords. An empty or blank query matches everything.
pub fn filter<'a>(entries: &'a [PaletteEntry], query: &str) -> Vec<&'a PaletteEntry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    entries
        .iter()
        .filter(|e| {
            let haystack = format!("{} {} {}", e.label, e.hint, e.keywords).to_lowercase();
            words.iter().all(|w| haystack.contains(w.as_str()))
        })
        .collect()
}

// ── The cursor ───────────────────────────────────────────────────────────────────────────

/// What is typed and which line of the narrowed list is highlighted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteState {
    query: String,
    cursor: usize,
}

impl PaletteState {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The field changed. The highlight goes back to the first line: the list under it is a
    /// different list.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.cursor = 0;
    }

    /// The highlighted line's position among `shown` lines (0 when there are none).
    pub fn cursor(&self, shown: usize) -> usize {
        self.cursor.min(shown.saturating_sub(1))
    }

    /// Move the highlight within `shown` lines. It stops at either end and does not wrap.
    pub fn move_cursor(&mut self, dir: FocusMove, shown: usize) {
        let at = self.cursor(shown);
        self.cursor = match dir {
            FocusMove::Down => (at + 1).min(shown.saturating_sub(1)),
            FocusMove::Up => at.saturating_sub(1),
        };
    }

    /// The highlighted entry among the narrowed list, or `None` when nothing matches.
    pub fn selected<'a>(&self, shown: &[&'a PaletteEntry]) -> Option<&'a PaletteEntry> {
        shown.get(self.cursor(shown.len())).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> PaletteContext {
        PaletteContext {
            has_focus: true,
            stackable: true,
            can_undo: true,
            order: OrderPreset::Curated,
            timer_button: Some("Pause".to_string()),
            has_stats: true,
        }
    }

    fn mods(ctrl: bool, alt: bool, meta: bool) -> KeyMods {
        KeyMods {
            ctrl,
            alt,
            meta,
            shift: false,
        }
    }

    fn labels(list: &[&PaletteEntry]) -> Vec<String> {
        list.iter().map(|e| e.label.clone()).collect()
    }

    // ── The gate ──

    #[test]
    fn the_palette_is_refused_under_a_reason_prompt_and_over_an_overlay() {
        assert_eq!(palette_gate(false, false), PaletteGate::Open);
        assert_eq!(palette_gate(true, false), PaletteGate::ReasonPrompt);
        assert_eq!(palette_gate(false, true), PaletteGate::Overlay);
        // A prompt inside an overlay is still named as the prompt.
        assert_eq!(palette_gate(true, true), PaletteGate::ReasonPrompt);
        assert!(palette_gate(false, false).is_open());
        assert!(!palette_gate(true, false).is_open());
        assert!(!palette_gate(false, true).is_open());
    }

    #[test]
    fn a_refusal_says_why_and_an_open_gate_says_nothing() {
        assert_eq!(PaletteGate::Open.refusal(), None);
        assert!(PaletteGate::ReasonPrompt
            .refusal()
            .unwrap()
            .contains("reason"));
        assert!(PaletteGate::Overlay.refusal().unwrap().contains("viewer"));
        for gate in [PaletteGate::ReasonPrompt, PaletteGate::Overlay] {
            assert!(!gate.refusal().unwrap().contains('\u{2014}'), "no em-dash");
            assert!(!gate.refusal().unwrap().contains('\u{2013}'), "no en-dash");
        }
    }

    // ── The keys ──

    #[test]
    fn ctrl_k_and_cmd_k_open_it_and_a_plain_k_does_not() {
        assert!(palette_open_key("k", mods(true, false, false)));
        assert!(palette_open_key("k", mods(false, false, true)));
        assert!(palette_open_key("K", mods(true, false, false)), "Caps Lock");
        assert!(!palette_open_key("k", mods(false, false, false)));
        assert!(
            !palette_open_key("k", mods(true, true, false)),
            "Alt claims it"
        );
        assert!(!palette_open_key("j", mods(true, false, false)));
    }

    #[test]
    fn the_open_chord_is_never_a_disposition() {
        // The pane refuses every Ctrl / Cmd press, so the chord cannot be a command there.
        for m in [mods(true, false, false), mods(false, false, true)] {
            assert_eq!(pane_command(PaneFocus::Cursor, "k", m), None);
        }
    }

    #[test]
    fn the_palette_key_map_is_a_whitelist() {
        let none = KeyMods::none();
        assert_eq!(palette_key("Escape", none), Some(PaletteKey::Close));
        assert_eq!(palette_key("Enter", none), Some(PaletteKey::Run));
        assert_eq!(
            palette_key("ArrowDown", none),
            Some(PaletteKey::Move(FocusMove::Down))
        );
        assert_eq!(
            palette_key("ArrowUp", none),
            Some(PaletteKey::Move(FocusMove::Up))
        );
        assert_eq!(
            palette_key("k", mods(true, false, false)),
            Some(PaletteKey::Close),
            "the open chord closes it"
        );
        // Everything else is typing.
        for typed in ["c", "i", "t", "f", "o", "b", "a", " ", "Tab", "Backspace"] {
            assert_eq!(palette_key(typed, none), None, "{typed}");
        }
        // A chord is not Enter.
        assert_eq!(palette_key("Enter", mods(true, false, false)), None);
    }

    // ── The entries ──

    #[test]
    fn every_pane_hotkey_resolves_to_an_entry_so_none_is_silently_dropped() {
        // `pane_entries` skips a key the map no longer knows; this makes that loud.
        let list = pane_entries(true);
        assert_eq!(list.len(), 4 + PANE_SPECS.len(), "{list:?}");
        assert_eq!(
            pane_entries(false).len(),
            list.len() - 1,
            "B needs a stackable row"
        );
    }

    #[test]
    fn each_pane_entry_is_the_command_its_key_produces() {
        // The entry and the key are one value: pressing the key and picking the line cannot
        // differ, because the line's action is read from the key map.
        for e in pane_entries(true) {
            let PaletteAction::Pane(cmd) = e.action else {
                panic!("a pane entry carries a pane command: {e:?}");
            };
            let key = match e.hint.as_str() {
                "Down" => "ArrowDown".to_string(),
                "Up" => "ArrowUp".to_string(),
                letter => letter.to_lowercase(),
            };
            assert_eq!(
                pane_command(PaneFocus::Cursor, &key, KeyMods::none()),
                Some(cmd),
                "{}",
                e.label
            );
        }
    }

    #[test]
    fn a_decision_that_needs_a_reason_prompts_and_one_that_does_not_applies() {
        let list = pane_entries(true);
        let action_for =
            |state: TriageState| {
                list.iter()
                    .find_map(|e| match e.action {
                        PaletteAction::Pane(
                            PaneCommand::Dispose(s) | PaneCommand::PromptFor(s),
                        ) if s == state => Some(e.action),
                        _ => None,
                    })
                    .unwrap()
            };
        assert_eq!(
            action_for(TriageState::Confirmed),
            PaletteAction::Pane(PaneCommand::Dispose(TriageState::Confirmed))
        );
        assert_eq!(
            action_for(TriageState::TechDebt),
            PaletteAction::Pane(PaneCommand::Dispose(TriageState::TechDebt))
        );
        assert_eq!(
            action_for(TriageState::Ignored),
            PaletteAction::Pane(PaneCommand::PromptFor(TriageState::Ignored))
        );
        assert_eq!(
            action_for(TriageState::FalsePositive),
            PaletteAction::Pane(PaneCommand::PromptFor(TriageState::FalsePositive))
        );
    }

    #[test]
    fn decide_is_the_one_split_between_apply_and_prompt() {
        for state in TriageState::all() {
            let cmd = PaneCommand::decide(state);
            assert_eq!(
                matches!(cmd, PaneCommand::PromptFor(_)),
                state.requires_reason(),
                "{state:?}"
            );
            if let Some(key) = state_hotkey(state) {
                assert_eq!(
                    pane_command(PaneFocus::Cursor, key, KeyMods::none()),
                    Some(cmd)
                );
            }
        }
    }

    #[test]
    fn with_no_focused_row_only_the_global_entries_exist() {
        let ctx = PaletteContext {
            has_focus: false,
            ..full()
        };
        for e in entries(&ctx) {
            assert!(
                !matches!(
                    e.action,
                    PaletteAction::Pane(_) | PaletteAction::FullDetail | PaletteAction::OpenHub
                ),
                "{e:?} needs a focused row"
            );
        }
    }

    #[test]
    fn an_entry_exists_only_when_its_control_does() {
        let bare = PaletteContext {
            has_focus: false,
            stackable: false,
            can_undo: false,
            order: OrderPreset::Curated,
            timer_button: None,
            has_stats: false,
        };
        let list = entries(&bare);
        assert!(!list.iter().any(|e| e.action == PaletteAction::Undo));
        assert!(!list.iter().any(|e| e.action == PaletteAction::ToggleTimer));
        assert!(!list.iter().any(|e| e.action == PaletteAction::OpenStats));
        let all = entries(&full());
        assert!(all.iter().any(|e| e.action == PaletteAction::Undo));
        assert!(all.iter().any(|e| e.action == PaletteAction::ToggleTimer));
        assert!(all.iter().any(|e| e.action == PaletteAction::OpenStats));
    }

    #[test]
    fn the_order_entries_leave_out_the_current_cut() {
        let list = entries(&full());
        let orders: Vec<OrderPreset> = list
            .iter()
            .filter_map(|e| match e.action {
                PaletteAction::SetOrder(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(orders.len(), OrderPreset::ALL.len() - 1);
        assert!(!orders.contains(&OrderPreset::Curated));
    }

    #[test]
    fn the_timer_entry_says_the_button_word() {
        let paused = PaletteContext {
            timer_button: Some("Resume".to_string()),
            ..full()
        };
        let list = entries(&paused);
        let timer = list
            .iter()
            .find(|e| e.action == PaletteAction::ToggleTimer)
            .unwrap();
        assert_eq!(timer.label, "Resume the review timer");
    }

    #[test]
    fn the_listing_order_is_stable() {
        assert_eq!(entries(&full()), entries(&full()));
    }

    #[test]
    fn no_entry_text_has_a_dash_of_any_width() {
        for e in entries(&full()) {
            for text in [&e.label, &e.hint] {
                assert!(
                    !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                    "{e:?}"
                );
            }
        }
    }

    #[test]
    fn only_actions_that_take_the_keyboard_themselves_skip_the_focus_return() {
        use PaneCommand::*;
        let t = TriageState::Confirmed;
        for (action, returns) in [
            (PaletteAction::Pane(Dispose(t)), true),
            (PaletteAction::Pane(MoveFocus(FocusMove::Down)), true),
            (PaletteAction::Pane(PromptFor(TriageState::Ignored)), false),
            (PaletteAction::Pane(OpenCode), false),
            (PaletteAction::Pane(OpenStack), false),
            (PaletteAction::Undo, true),
            (PaletteAction::SetOrder(OrderPreset::ByFile), true),
            (PaletteAction::ToggleTimer, true),
            (PaletteAction::OpenHub, false),
            (PaletteAction::FullDetail, false),
            (PaletteAction::OpenStats, false),
        ] {
            assert_eq!(action.returns_focus(), returns, "{action:?}");
        }
    }

    // ── Filtering and the cursor ──

    #[test]
    fn a_blank_query_matches_everything_in_order() {
        let list = entries(&full());
        assert_eq!(filter(&list, "").len(), list.len());
        assert_eq!(filter(&list, "   ").len(), list.len());
        let shown = filter(&list, "");
        assert_eq!(
            labels(&shown),
            list.iter().map(|e| e.label.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_filter_matches_every_word_in_either_case_and_keeps_the_order() {
        let list = entries(&full());
        let shown = filter(&list, "FALSE posit");
        assert_eq!(shown.len(), 1);
        assert!(shown[0].label.contains("false positive"));
        // Words of the keywords match too, in any order.
        let by_keyword = filter(&list, "fp");
        assert_eq!(by_keyword.len(), 1);
        // Order is the listing order, not a score.
        let order = filter(&list, "order");
        assert!(order
            .iter()
            .all(|e| matches!(e.action, PaletteAction::SetOrder(_))));
        assert_eq!(order.len(), 3);
        // A query nothing matches shows nothing.
        assert!(filter(&list, "zzzz").is_empty());
        assert!(filter(&list, "confirm zzzz").is_empty());
    }

    #[test]
    fn the_hint_is_matched_so_the_key_finds_its_entry() {
        let list = entries(&full());
        let shown = filter(&list, "c");
        assert!(shown.iter().any(|e| e.hint == "C"));
    }

    #[test]
    fn typing_resets_the_highlight_and_the_arrows_stop_at_the_ends() {
        let list = entries(&full());
        let mut state = PaletteState::new();
        let shown = filter(&list, "");
        assert_eq!(state.cursor(shown.len()), 0);
        state.move_cursor(FocusMove::Up, shown.len());
        assert_eq!(state.cursor(shown.len()), 0, "no wrap above");
        for _ in 0..100 {
            state.move_cursor(FocusMove::Down, shown.len());
        }
        assert_eq!(state.cursor(shown.len()), shown.len() - 1, "no wrap below");
        state.set_query("order".to_string());
        assert_eq!(state.cursor(3), 0);
        assert_eq!(state.query(), "order");
    }

    #[test]
    fn the_highlight_survives_a_list_that_shrank_under_it() {
        let list = entries(&full());
        let mut state = PaletteState::new();
        let all = filter(&list, "");
        for _ in 0..5 {
            state.move_cursor(FocusMove::Down, all.len());
        }
        let few = filter(&list, "order");
        assert!(state.cursor(few.len()) < few.len());
        assert!(state.selected(&few).is_some());
        assert_eq!(state.selected(&filter(&list, "zzzz")), None);
    }

    #[test]
    fn selected_is_the_entry_at_the_cursor() {
        let list = entries(&full());
        let shown = filter(&list, "");
        let mut state = PaletteState::new();
        assert_eq!(state.selected(&shown), Some(shown[0]));
        state.move_cursor(FocusMove::Down, shown.len());
        assert_eq!(state.selected(&shown), Some(shown[1]));
    }
}
