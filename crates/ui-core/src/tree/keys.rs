//! What a key press means in the tree lane (spec sections 5 and 11).
//!
//! The same scoping the Queue's detail pane uses ([`crate::triage::pane_command`]): the keys
//! fire only while the lane's focusable region holds focus ([`PaneFocus::Cursor`]), never while
//! a text field does, and never with Ctrl, Alt or Cmd held. Anything else returns `None` for
//! every key there is, so a letter typed into a reason box can never be read as a decision.

use crate::triage::{FocusMove, KeyMods, PaneCommand, PaneFocus, TriageState};

/// What the cursor is on, which decides what `C`, `I`, `F` and `T` mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    /// Nothing is focused yet.
    None,
    /// A node header.
    Node,
    /// A site row.
    Site,
    /// A site row that is an outlier, so Space ticks it into the next batch.
    OutlierSite,
}

/// What a press asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeCommand {
    Move(FocusMove),
    /// `Right`: open the node, or step into an open one.
    Open,
    /// `Left`: close the node, or go up one level.
    Close,
    /// `Enter`: open the node and step into its sites.
    Enter,
    /// `Shift Left`: close everything.
    CollapseAll,
    /// `Space` on an outlier: tick it into the batch.
    TickOutlier,
    /// `C`, `I`, `F` or `T` on a node: open the batch prompt. All four prompt (design D9): a
    /// group decision always carries a reason, Confirm included.
    BatchPrompt(TriageState),
    /// `C`, `I`, `F` or `T` on a site: the Queue's own split (Confirm and Tech debt apply,
    /// Ignore and False positive prompt).
    Site(PaneCommand),
    /// `E` on a node: edit its summary.
    EditSummary,
    /// `O` on a site: the code viewer.
    OpenCode,
    /// `B` on a node or site: the stacked view of its sub-group.
    OpenStack,
}

/// The key map as a pure function. `key` is the DOM key (`ArrowDown`, `c`, `" "`).
pub fn tree_command(
    focus: PaneFocus,
    key: &str,
    mods: KeyMods,
    at: CursorKind,
) -> Option<TreeCommand> {
    // The scope gate: nothing below is reachable from anywhere but the lane's own cursor.
    if focus != PaneFocus::Cursor {
        return None;
    }
    if mods.ctrl || mods.alt || mods.meta {
        return None;
    }
    match key {
        "ArrowDown" => return Some(TreeCommand::Move(FocusMove::Down)),
        "ArrowUp" => return Some(TreeCommand::Move(FocusMove::Up)),
        "ArrowRight" => return Some(TreeCommand::Open),
        "ArrowLeft" => {
            return Some(if mods.shift {
                TreeCommand::CollapseAll
            } else {
                TreeCommand::Close
            });
        }
        "Enter" => return (at == CursorKind::Node).then_some(TreeCommand::Enter),
        " " => return (at == CursorKind::OutlierSite).then_some(TreeCommand::TickOutlier),
        _ => {}
    }
    if key.chars().count() != 1 {
        return None;
    }
    let state = match key.to_ascii_lowercase().as_str() {
        "c" => Some(TriageState::Confirmed),
        "i" => Some(TriageState::Ignored),
        "f" => Some(TriageState::FalsePositive),
        "t" => Some(TriageState::TechDebt),
        _ => None,
    };
    if let Some(state) = state {
        return match at {
            CursorKind::None => None,
            CursorKind::Node => Some(TreeCommand::BatchPrompt(state)),
            CursorKind::Site | CursorKind::OutlierSite => {
                Some(TreeCommand::Site(PaneCommand::decide(state)))
            }
        };
    }
    match (key.to_ascii_lowercase().as_str(), at) {
        ("e", CursorKind::Node) => Some(TreeCommand::EditSummary),
        ("o", CursorKind::Site | CursorKind::OutlierSite) => Some(TreeCommand::OpenCode),
        ("b", CursorKind::Node | CursorKind::Site | CursorKind::OutlierSite) => {
            Some(TreeCommand::OpenStack)
        }
        _ => None,
    }
}

/// Whether the lane's keys will fire under this focus, for the armed or off indicator. Defined as
/// "would `Down` produce a command", so what the reviewer is told and what the handler does
/// cannot drift apart.
pub fn tree_keys_armed(focus: PaneFocus) -> bool {
    tree_command(focus, "ArrowDown", KeyMods::none(), CursorKind::Node).is_some()
}

/// The indicator's text.
pub fn keys_hint(armed: bool, prompt_open: bool) -> &'static str {
    if armed {
        "Keys: \u{2191}\u{2193} move \u{b7} \u{2192} open \u{b7} \u{2190} close \u{b7} Enter into the sites \u{b7} C I F T decide \u{b7} Space tick an outlier \u{b7} E edit the summary \u{b7} O code \u{b7} B stack"
    } else if prompt_open {
        "Keys are off while you type. Enter commits, Esc cancels."
    } else {
        "Keys are off. Click the tree (or Tab into it) to arm them."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::TriageState;

    const KINDS: [CursorKind; 4] = [
        CursorKind::None,
        CursorKind::Node,
        CursorKind::Site,
        CursorKind::OutlierSite,
    ];
    const KEYS: [&str; 22] = [
        "ArrowDown",
        "ArrowUp",
        "ArrowRight",
        "ArrowLeft",
        "Enter",
        " ",
        "c",
        "i",
        "f",
        "t",
        "e",
        "o",
        "b",
        "C",
        "I",
        "F",
        "T",
        "E",
        "g",
        "x",
        "Tab",
        "Escape",
    ];

    fn press(focus: PaneFocus, key: &str, at: CursorKind) -> Option<TreeCommand> {
        tree_command(focus, key, KeyMods::none(), at)
    }

    #[test]
    fn tree_keys_open_close_and_batch_only_when_armed() {
        // Not armed: no key does anything, wherever the cursor is.
        for focus in [PaneFocus::Elsewhere, PaneFocus::ReasonPrompt] {
            for key in KEYS {
                for at in KINDS {
                    assert_eq!(press(focus, key, at), None, "{focus:?} {key:?} {at:?}");
                }
            }
        }
        assert!(!tree_keys_armed(PaneFocus::Elsewhere));
        assert!(!tree_keys_armed(PaneFocus::ReasonPrompt));
        assert!(tree_keys_armed(PaneFocus::Cursor));

        // Armed, on a node.
        let on = |key| press(PaneFocus::Cursor, key, CursorKind::Node);
        assert_eq!(on("ArrowRight"), Some(TreeCommand::Open));
        assert_eq!(on("ArrowLeft"), Some(TreeCommand::Close));
        assert_eq!(on("Enter"), Some(TreeCommand::Enter));
        assert_eq!(on("ArrowDown"), Some(TreeCommand::Move(FocusMove::Down)));
        assert_eq!(on("ArrowUp"), Some(TreeCommand::Move(FocusMove::Up)));
        // All four letters open the batch prompt on a node (design D9), in either case.
        for (key, state) in [
            ("c", TriageState::Confirmed),
            ("i", TriageState::Ignored),
            ("f", TriageState::FalsePositive),
            ("t", TriageState::TechDebt),
            ("C", TriageState::Confirmed),
        ] {
            assert_eq!(on(key), Some(TreeCommand::BatchPrompt(state)), "{key}");
        }
        assert_eq!(on("e"), Some(TreeCommand::EditSummary));
        assert_eq!(on("b"), Some(TreeCommand::OpenStack));
        assert_eq!(on("o"), None, "the code viewer is for a site");
        assert_eq!(on(" "), None, "Space ticks an outlier only");
        assert_eq!(on("g"), None);
        // Nothing is focused yet: the decision keys do nothing.
        assert_eq!(press(PaneFocus::Cursor, "c", CursorKind::None), None);
    }

    #[test]
    fn shift_left_collapses_everything() {
        let shift = KeyMods {
            shift: true,
            ..KeyMods::none()
        };
        assert_eq!(
            tree_command(PaneFocus::Cursor, "ArrowLeft", shift, CursorKind::Node),
            Some(TreeCommand::CollapseAll)
        );
    }

    #[test]
    fn a_held_modifier_claims_the_press_and_the_tree_leaves_it_alone() {
        for mods in [
            KeyMods {
                ctrl: true,
                ..KeyMods::none()
            },
            KeyMods {
                alt: true,
                ..KeyMods::none()
            },
            KeyMods {
                meta: true,
                ..KeyMods::none()
            },
        ] {
            for key in KEYS {
                assert_eq!(
                    tree_command(PaneFocus::Cursor, key, mods, CursorKind::Node),
                    None,
                    "{mods:?} {key:?}"
                );
            }
        }
    }

    #[test]
    fn on_a_site_the_decision_keys_are_the_queues_own_split() {
        for at in [CursorKind::Site, CursorKind::OutlierSite] {
            let on = |key| press(PaneFocus::Cursor, key, at);
            assert_eq!(
                on("c"),
                Some(TreeCommand::Site(PaneCommand::Dispose(
                    TriageState::Confirmed
                )))
            );
            assert_eq!(
                on("t"),
                Some(TreeCommand::Site(PaneCommand::Dispose(
                    TriageState::TechDebt
                )))
            );
            assert_eq!(
                on("i"),
                Some(TreeCommand::Site(PaneCommand::PromptFor(
                    TriageState::Ignored
                )))
            );
            assert_eq!(
                on("f"),
                Some(TreeCommand::Site(PaneCommand::PromptFor(
                    TriageState::FalsePositive
                )))
            );
            assert_eq!(on("o"), Some(TreeCommand::OpenCode));
            assert_eq!(on("b"), Some(TreeCommand::OpenStack));
            assert_eq!(
                on("e"),
                None,
                "editing a site's fields is the detail pane's"
            );
        }
    }

    #[test]
    fn space_ticks_only_an_outlier() {
        assert_eq!(
            press(PaneFocus::Cursor, " ", CursorKind::OutlierSite),
            Some(TreeCommand::TickOutlier)
        );
        assert_eq!(press(PaneFocus::Cursor, " ", CursorKind::Site), None);
        assert_eq!(press(PaneFocus::Cursor, " ", CursorKind::Node), None);
    }

    #[test]
    fn the_hint_follows_the_focus() {
        assert!(keys_hint(true, false).starts_with("Keys:"));
        assert!(keys_hint(false, true).contains("while you type"));
        assert!(keys_hint(false, false).contains("Click the tree"));
        for text in [
            keys_hint(true, false),
            keys_hint(false, true),
            keys_hint(false, false),
        ] {
            assert!(!text.contains('\u{2014}'));
        }
    }
}
