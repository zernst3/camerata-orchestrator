//! The review command palette, the Dioxus side (code viewer step 7d; work order B.8).
//!
//! Every rule is in `camerata_ui_core::palette`: which entries exist, how a query narrows them,
//! what a key means in the field, the key that opens it, and the gate that refuses it. This file
//! owns what needs a window:
//!
//! * [`PaletteLink`] / [`PaletteSlot`] / [`PaletteRoot`]: the palette is an overlay, so it is
//!   hosted at the results root like the stacked view and the viewer (an overlay mounted beside a
//!   chorale table has left a click-eating ghost in the desktop webview). The Queue registers a
//!   link and writes [`PaletteOpen`] to open it.
//! * [`PaletteHost`]: the field, the filtered list, the keyboard.
//! * Focus: [`remember_focus`] when it opens and [`restore_focus`] when it closes, so `Esc`
//!   leaves the keyboard where it was.
//!
//! It runs nothing itself. Picking an entry hands its [`PaletteAction`] to the link's `on_run`,
//! which is the Queue's dispatcher onto the functions the buttons and keys already call.
//!
//! The overlay carries `data-review-surface`, so typing in it and clicking in it count as review
//! activity for the internal timer, like the viewer and the stacked view.

use camerata_ui_core::palette::{
    filter, palette_key, PaletteAction, PaletteEntry, PaletteKey, PaletteState,
};
use camerata_ui_core::triage::{FocusMove, KeyMods};
use dioxus::prelude::*;

use super::scan::DETAIL_PANE_ID;
use super::viewer::follow_selected;

/// The DOM id of the palette's field.
pub(super) const PALETTE_INPUT_ID: &str = "camerata-palette-input";

/// The DOM id of the toolbar button that opens the palette with the mouse.
pub(super) const PALETTE_BUTTON_ID: &str = "camerata-palette-button";

/// The selector of the highlighted line, for the follow-scroll.
const SELECTED_LINE: &str = ".pal-row-sel";

/// The DOM id of line `index`.
pub(super) fn option_id(index: usize) -> String {
    format!("camerata-palette-opt-{index}")
}

// ── Where the keyboard was ───────────────────────────────────────────────────────────────

/// Remember the focused element, in the page, before the palette's field takes the keyboard.
const REMEMBER_FOCUS_JS: &str = "window.__camerataPaletteReturn = document.activeElement;";

/// Put the keyboard back on the element remembered when the palette opened, or on `fallback` when
/// there was none worth returning to (nothing focused, the page body, or an element the table has
/// since replaced). The remembered element is forgotten either way.
fn restore_focus_js(fallback: &str) -> String {
    format!(
        "var el = window.__camerataPaletteReturn; window.__camerataPaletteReturn = null;\
if (!(el && el !== document.body && el.isConnected && typeof el.focus === 'function')) \
{{ el = document.getElementById('{fallback}'); }}\
if (el) el.focus();"
    )
}

/// Remember where the keyboard is. Call just before the palette is opened.
pub(super) fn remember_focus() {
    dioxus::document::eval(REMEMBER_FOCUS_JS);
}

/// Give the keyboard back to where it was when the palette opened (the detail pane when that is
/// not possible, so the pane's keys work without a click).
pub(super) fn restore_focus() {
    #[cfg(test)]
    super::viewer::FOCUS_LOG.with(|l| l.borrow_mut().push(RESTORE_LOG_MARK.to_string()));
    dioxus::document::eval(&restore_focus_js(DETAIL_PANE_ID));
}

/// What the focus log records when the palette gives the keyboard back (the real restore is a
/// script, which a test cannot observe).
#[cfg(test)]
pub(super) const RESTORE_LOG_MARK: &str = "palette-return";

// ── Opening ──────────────────────────────────────────────────────────────────────────────

/// A request to show the palette. Written by the Queue into the link's `open`, read by
/// [`PaletteRoot`].
#[derive(Clone, PartialEq)]
pub(super) struct PaletteOpen {
    /// A new number per opening, so a second opening is a fresh host (an empty field, the first
    /// line highlighted) and not the previous one with its state.
    pub id: u64,
    /// The entries for the state of the review when it was opened.
    pub entries: Vec<PaletteEntry>,
}

/// Everything the palette needs from the Queue, registered by `FindingsTable`.
#[derive(Clone, PartialEq)]
pub(super) struct PaletteLink {
    /// The open palette, or `None`. The Queue writes it to open; the host writes `None` to close.
    pub open: Signal<Option<PaletteOpen>>,
    /// Run this action. The Queue's one dispatcher: it calls the function the action's button or
    /// key calls, and refuses (the same gate as opening) while a reason prompt is open.
    pub on_run: Callback<PaletteAction>,
}

/// Where the Queue registers its [`PaletteLink`]. Provided by the results view.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct PaletteSlot(pub Signal<Option<PaletteLink>>);

/// The palette's host at the results root. Renders nothing until the Queue has registered a link
/// and opened the palette; then it mounts [`PaletteHost`] on that opening, keyed by its id.
#[component]
pub(super) fn PaletteRoot() -> Element {
    let slot = try_use_context::<PaletteSlot>();
    let Some(link) = slot.and_then(|s| s.0.read().clone()) else {
        return rsx! {};
    };
    let Some(open) = link.open.read().clone() else {
        return rsx! {};
    };
    rsx! { PaletteHost { key: "{open.id}", link, open } }
}

/// The signals and the I/O. It makes no judgment: every key is a function of
/// `camerata_ui_core::palette`, and every action is the Queue's `on_run`.
#[component]
fn PaletteHost(link: PaletteLink, open: PaletteOpen) -> Element {
    let mut state = use_signal(PaletteState::new);
    let mut open_sig = link.open;
    let on_run = link.on_run;
    let entries = use_signal({
        let list = open.entries.clone();
        move || list
    });

    // Close, giving the keyboard back unless the action just run has taken it elsewhere.
    let close = use_callback(move |give_back: bool| {
        open_sig.set(None);
        if give_back {
            restore_focus();
        }
    });
    // Run the line at `index` of the narrowed list.
    let run_at = use_callback(move |index: usize| {
        let action = {
            let all = entries.peek();
            let shown = filter(&all, state.peek().query());
            shown.get(index).map(|e| e.action)
        };
        let Some(action) = action else { return };
        on_run.call(action);
        close.call(action.returns_focus());
    });

    let query = state.read().query().to_string();
    let rows: Vec<(usize, String, String)> = filter(&entries.read(), &query)
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.label.clone(), e.hint.clone()))
        .collect();
    let count = rows.len();
    let cursor = state.read().cursor(count);
    let active = (count > 0).then(|| option_id(cursor));

    rsx! {
        div {
            class: "rule-modal-overlay pal-overlay",
            // A review surface: keys and clicks here are the reviewer's, and count for the timer.
            "data-review-surface": "true",
            onclick: move |_| close.call(true),
            div {
                class: "pal-panel",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "Command palette",
                onclick: move |e| e.stop_propagation(),
                input {
                    id: PALETTE_INPUT_ID,
                    class: "addressee-input pal-input",
                    r#type: "text",
                    role: "combobox",
                    "aria-expanded": "true",
                    "aria-controls": "camerata-palette-list",
                    "aria-activedescendant": active.unwrap_or_default(),
                    autocomplete: "off",
                    spellcheck: "false",
                    placeholder: "Type a command",
                    value: "{query}",
                    autofocus: true,
                    onmounted: move |e| {
                        spawn(async move {
                            let _ = e.set_focus(true).await;
                        });
                    },
                    oninput: move |e| state.write().set_query(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        // Nothing typed here is the pane's: its hotkeys must never see a letter
                        // meant for this field.
                        e.stop_propagation();
                        let m = e.modifiers();
                        let mods = KeyMods {
                            ctrl: m.contains(Modifiers::CONTROL),
                            alt: m.contains(Modifiers::ALT),
                            meta: m.contains(Modifiers::META),
                            shift: m.contains(Modifiers::SHIFT),
                        };
                        let key = e.key().to_string();
                        match palette_key(&key, mods) {
                            Some(PaletteKey::Close) => {
                                e.prevent_default();
                                close.call(true);
                            }
                            Some(PaletteKey::Move(dir)) => {
                                e.prevent_default();
                                move_highlight(&mut state, &entries, dir);
                            }
                            Some(PaletteKey::Run) => {
                                e.prevent_default();
                                let at = {
                                    let all = entries.peek();
                                    let shown = filter(&all, state.peek().query());
                                    state.peek().cursor(shown.len())
                                };
                                run_at.call(at);
                            }
                            None => {}
                        }
                    },
                }
                div {
                    id: "camerata-palette-list",
                    class: "pal-list",
                    role: "listbox",
                    for (i, label, hint) in rows {
                        div {
                            key: "{i}",
                            id: option_id(i),
                            class: if i == cursor { "pal-row pal-row-sel" } else { "pal-row" },
                            role: "option",
                            "aria-selected": if i == cursor { "true" } else { "false" },
                            onclick: move |e| {
                                e.stop_propagation();
                                run_at.call(i);
                            },
                            span { class: "pal-label", "{label}" }
                            if !hint.is_empty() {
                                span { class: "pal-hint", "{hint}" }
                            }
                        }
                    }
                    if count == 0 {
                        p { class: "pal-empty", "No command matches \u{201c}{query}\u{201d}." }
                    }
                }
                p { class: "pal-keys",
                    "Type to filter \u{b7} Up Down choose \u{b7} Enter run \u{b7} Esc close"
                }
            }
        }
    }
}

/// Move the highlight within the narrowed list, and keep it on screen.
fn move_highlight(
    state: &mut Signal<PaletteState>,
    entries: &Signal<Vec<PaletteEntry>>,
    dir: FocusMove,
) {
    let shown = {
        let all = entries.peek();
        filter(&all, state.peek().query()).len()
    };
    state.write().move_cursor(dir, shown);
    follow_selected(SELECTED_LINE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restore_script_prefers_the_remembered_element_and_falls_back_to_the_pane() {
        let js = restore_focus_js(DETAIL_PANE_ID);
        // The remembered element, only when it is still a live focusable thing in the page.
        assert!(js.contains("window.__camerataPaletteReturn"));
        assert!(js.contains("el.isConnected"));
        assert!(js.contains("el !== document.body"));
        // The fallback is the detail pane, so its keys work without a click.
        assert!(js.contains(&format!("getElementById('{DETAIL_PANE_ID}')")));
        // Forgotten either way, so a stale element is never returned to on a later opening.
        assert!(js.contains("window.__camerataPaletteReturn = null"));
        assert!(js.ends_with("if (el) el.focus();"));
    }

    #[test]
    fn the_remember_script_and_the_restore_script_use_the_same_slot() {
        assert!(REMEMBER_FOCUS_JS.contains("window.__camerataPaletteReturn"));
        assert!(REMEMBER_FOCUS_JS.contains("document.activeElement"));
    }

    #[test]
    fn line_ids_are_stable_and_distinct() {
        assert_eq!(option_id(0), "camerata-palette-opt-0");
        assert_ne!(option_id(0), option_id(1));
    }
}
