//! The stacked group view, "same fix, many places" (`WORK_ORDER_REVIEW_SPEED.md` B.2).
//!
//! A full-window overlay listing every row of one rule group in the current Queue table as a
//! card: `path:line`, a severity dot, the stored line with three lines of code either side, the
//! row's status and a checkbox. The curator reads the pattern once and decides the checked cards
//! together.
//!
//! Split the way the viewer and the pane are:
//!
//! * [`StackView`] is props-only. It chooses words and pixels, so SSR tests render every state
//!   (loading, with code, snippet only, a prompt open, a paged group).
//! * [`StackHost`] owns the signals and the I/O: the one batch request, the prompt, the keys.
//!   Every judgment (which key means what, which cards are checked, which page) is a pure
//!   function in `camerata_ui_core::stack`.
//!
//! ## No new write path, no new endpoint
//!
//! A decision calls the link's `on_dispose`, which is the Queue's `apply_rows`: the one function
//! behind the pane's keys, the viewer's keys and this view. It owns the required-reason refusal,
//! the five-second undo (which names the count) and the removal of the rows from the table. The
//! reason prompt is the Queue's own [`QueueReasonPrompt`]. The code comes from
//! `POST /api/onboard/finding-context/batch`, once per page, through the pane's own gate
//! (`accept`) and cache lane, so a card shows code only when the pane would and otherwise shows
//! its stored snippet.
//!
//! ## Hosted at the results root
//!
//! Like the viewer, the overlay is mounted by [`StackRoot`] at the results root and not inside
//! the table's subtree (an overlay that is a sibling of a chorale table has left a ghost node
//! swallowing the next click in the desktop webview). The table registers a [`StackLink`] in the
//! [`StackSlot`] and opens the stack by writing `link.open`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use camerata_ui_core::pane_context::{ContextTarget, PaneContext};
use camerata_ui_core::stack::{
    stack_command, stack_focus, stack_keys_armed, StackFocus, StackOrigin, StackState,
    NONE_CHECKED_NOTE, STACK_KEY_ROWS,
};
use camerata_ui_core::triage::{
    finding_key, finding_state, Disposition, FindingView, FocusMove, KeyMods, TriageState,
    UndoEntry,
};
use camerata_ui_core::viewer::{severity_class, ViewerCache};
use dioxus::prelude::*;

use super::scan::{context_rows, QueueReasonPrompt, ToolbarReasonPrompt};
use super::viewer::{dom_key, focus_element, follow_selected, ViewerUndoBar};

/// The DOM id of the stack's focusable root. The keydown handler lives here, and the viewer's
/// close hands the keyboard back to it.
pub(super) const STACK_ID: &str = "camerata-stack";

/// DOM ids of the controls a mounted test needs to find (and a screen reader to name).
pub(super) const STACK_UNDO_ID: &str = "camerata-stack-undo";
pub(super) const STACK_CONFIRM_ID: &str = "camerata-stack-act-confirm";
pub(super) const STACK_DEBT_ID: &str = "camerata-stack-act-debt";
pub(super) const STACK_IGNORE_ID: &str = "camerata-stack-act-ignore";
pub(super) const STACK_FP_ID: &str = "camerata-stack-act-fp";
pub(super) const STACK_PREV_ID: &str = "camerata-stack-page-prev";
pub(super) const STACK_NEXT_ID: &str = "camerata-stack-page-next";

/// The selector of the focused card, for the follow-scroll.
const SELECTED_CARD: &str = ".sk-card-sel";

/// The DOM id of a "Review all N stacked" button in the deviation bulk bar, so closing the stack
/// can give the keyboard back to the button that opened it.
pub(super) fn bar_door_id(index: usize) -> String {
    format!("camerata-stack-door-{index}")
}

/// The DOM id focus returns to when the stack closes.
pub(super) fn origin_focus_id(origin: StackOrigin) -> String {
    match origin {
        StackOrigin::Pane => super::scan::DETAIL_PANE_ID.to_string(),
        StackOrigin::BulkBar(index) => bar_door_id(index),
    }
}

/// A request to show a stack. Written by the Queue (the pane's `B`, the bulk bar's button) into
/// the link's `open`, read by [`StackRoot`].
#[derive(Clone, PartialEq)]
pub(super) struct StackOpen {
    /// A new number per opening, so a second opening is a fresh host (clean prompt, checkboxes
    /// and request) and not the previous one with its state.
    pub id: u64,
    /// The group's header line (`rule_group_label`).
    pub label: String,
    /// The group's rows in the table's order.
    pub rows: Vec<FindingView>,
    pub origin: StackOrigin,
    /// The finding the pane was on, so the cursor starts on its card.
    pub anchor: Option<String>,
}

/// Everything the stack needs from the Queue's table, registered by `FindingsTable`.
#[derive(Clone)]
pub(super) struct StackLink {
    /// The open stack, or `None`. The Queue writes it to open; it also reads it (the viewer's
    /// close checks whether to hand the keyboard to the stack).
    pub open: Signal<Option<StackOpen>>,
    pub dispositions: Signal<HashMap<String, Disposition>>,
    /// The inspected commit per repo, from the report's `audited_refs`.
    pub shas: Arc<HashMap<String, String>>,
    pub project_id: String,
    pub undo_entry: Signal<Option<UndoEntry>>,
    pub now_ms: Signal<u64>,
    pub on_undo: Callback<()>,
    /// The Queue's one write path with an undo: these rows to this state with this reason.
    /// Answers `true` only when a disposition was written.
    pub on_dispose: Callback<(Vec<FindingView>, TriageState, String), bool>,
    /// Aim the Queue's cursor at this finding and open the viewer on it. Answers `true` when it
    /// opened (a finding that has left the table cannot be opened, and the Queue says so).
    pub on_open_viewer: Callback<String, bool>,
    /// Close the stack. Carries the key of the card the curator was on, so the Queue can put its
    /// cursor there, and gives the keyboard back to where the stack was opened from.
    pub on_close: Callback<Option<String>>,
}

impl PartialEq for StackLink {
    fn eq(&self, other: &Self) -> bool {
        self.open == other.open
            && self.dispositions == other.dispositions
            && Arc::ptr_eq(&self.shas, &other.shas)
            && self.project_id == other.project_id
            && self.undo_entry == other.undo_entry
            && self.now_ms == other.now_ms
            && self.on_undo == other.on_undo
            && self.on_dispose == other.on_dispose
            && self.on_open_viewer == other.on_open_viewer
            && self.on_close == other.on_close
    }
}

/// Where the Queue registers its [`StackLink`]. Provided by the results view.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct StackSlot(pub Signal<Option<StackLink>>);

// ── What a card shows ────────────────────────────────────────────────────────────────────

/// One card, as data. Built by [`card_views`] from the page's rows, the disposition map and the
/// windows the batch endpoint returned.
#[derive(Clone, PartialEq)]
pub(super) struct StackCardView {
    /// The finding key made safe for an attribute (`dom_key`).
    pub key: String,
    /// `path:line`.
    pub location: String,
    /// `critical` / `high` / `medium` / `low`, for the dot.
    pub severity_class: String,
    pub severity: String,
    /// The row's disposition: Unresolved, Confirmed, Ignored, Tech debt or False positive.
    pub status: String,
    /// The reason recorded with a decision, for the tooltip.
    pub status_title: String,
    pub checked: bool,
    pub focused: bool,
    /// The stored line, shown when there is no window.
    pub snippet: String,
    /// The code around the line, when the batch endpoint answered for this card and the pane's
    /// gate accepted it.
    pub window: Option<PaneContext>,
}

/// The cards of the current page. A card has a window only when `windows` holds one for its key
/// (it was accepted by `accept`, which is the pane's gate), so everything else is the stored
/// snippet: the degradation floor.
pub(super) fn card_views(
    state: &StackState,
    dispositions: &HashMap<String, Disposition>,
    windows: &HashMap<String, PaneContext>,
) -> Vec<StackCardView> {
    state
        .page_rows()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let key = finding_key(f);
            let disposition = finding_state(dispositions, f);
            StackCardView {
                key: dom_key(&key),
                location: format!("{}:{}", f.path, f.line),
                severity_class: severity_class(&f.severity).to_string(),
                severity: f.severity.clone(),
                status: disposition.label().to_string(),
                status_title: dispositions
                    .get(&key)
                    .map(|d| d.reason.clone())
                    .filter(|_| disposition.requires_reason())
                    .unwrap_or_default(),
                checked: state.is_checked(f, dispositions),
                focused: i == state.cursor(),
                snippet: f.snippet.clone(),
                window: windows.get(&key).cloned(),
            }
        })
        .collect()
}

/// Where the code for the page on screen stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CodeLoad {
    /// The request has not finished (the cards show their stored lines meanwhile).
    Pending,
    /// The server did not answer: the code was never read, so nothing is known about it.
    Failed,
    /// The server answered. `shown` of `total` cards have the code around them.
    Done { shown: usize, total: usize },
}

/// The sentence under the header about the code on the cards, or `None` while there is nothing to
/// say. Cards without the code show the stored line only, and the curator is told why, rather
/// than left to wonder whether the code was missing, refused or merely slow. A server that did
/// not answer is a different sentence from a server that answered "not at that commit": the
/// first says nothing about the code.
pub(super) fn context_note(load: CodeLoad) -> Option<String> {
    match load {
        CodeLoad::Pending => None,
        CodeLoad::Failed => Some(
            "The server did not answer the request for the code around these findings, so cards \
             without it show the stored line only. Reopen the stack to ask again."
                .to_string(),
        ),
        CodeLoad::Done { shown, total } if shown >= total => None,
        CodeLoad::Done { shown: 0, total } => Some(format!(
            "The code around these {total} findings could not be read at the inspected commit. \
             Each card shows its stored line only."
        )),
        CodeLoad::Done { shown, total } => Some(format!(
            "{} of {total} cards show the stored line only: the code around them could \
             not be read at the inspected commit.",
            total - shown
        )),
    }
}

/// The key strip, from the key list the tests hold to the map.
pub(super) fn key_strip() -> String {
    STACK_KEY_ROWS
        .iter()
        .map(|r| format!("{}  {}", r.label, r.action))
        .collect::<Vec<_>>()
        .join("   \u{b7}   ")
}

// ── The view ─────────────────────────────────────────────────────────────────────────────

/// The stack, props only.
#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn StackView(
    /// The group's header line.
    label: String,
    /// "30 cards on screen, 29 checked".
    summary: String,
    /// Said when the group is bigger than a page.
    #[props(default)]
    page_note: Option<String>,
    /// Said when some cards could not get their code.
    #[props(default)]
    code_note: Option<String>,
    /// Whether the group is paged (the page buttons show).
    #[props(default)]
    paged: bool,
    cards: Vec<StackCardView>,
    /// Where keyboard focus is (a reason prompt owns the keyboard while it is open).
    focus: StackFocus,
    /// The reason prompt, when one is raised.
    #[props(default)]
    prompt: Option<ToolbarReasonPrompt>,
    #[props(default)] reason: String,
    undo_entry: Signal<Option<UndoEntry>>,
    now_ms: Signal<u64>,
    /// Every keydown on the panel, as `(key, modifiers)`. The host runs it through
    /// [`stack_command`]; this component never decides what a key means.
    on_key: Callback<(String, KeyMods)>,
    /// A click on a card's checkbox.
    on_toggle: Callback<usize>,
    /// A click on a card, to put the cursor there.
    on_focus_card: Callback<usize>,
    /// "Open in viewer" on a card.
    on_open: Callback<usize>,
    /// A disposition chosen with the MOUSE: the keys are an accelerator, not the only door.
    on_dispose: Callback<TriageState>,
    on_page: Callback<FocusMove>,
    on_close: Callback<()>,
    on_undo: Callback<()>,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    let strip = key_strip();
    // One derivation for the keys, the claim test above and every mouse door: while a reason
    // prompt owns the keyboard, the controls that act on cards are disabled (and the host's
    // handlers refuse too, since a disabled button is only a hint to the browser).
    let armed = stack_keys_armed(focus);
    rsx! {
        div { class: "rule-modal-overlay cv-overlay sk-overlay",
            div {
                id: "{STACK_ID}",
                class: "cv-panel sk-panel",
                tabindex: "0",
                "data-stack-label": "{label}",
                onkeydown: move |e: KeyboardEvent| {
                    let m = e.modifiers();
                    let mods = KeyMods {
                        ctrl: m.contains(Modifiers::CONTROL),
                        alt: m.contains(Modifiers::ALT),
                        meta: m.contains(Modifiers::META),
                        shift: m.contains(Modifiers::SHIFT),
                    };
                    let key = e.key().to_string();
                    // Swallow only the presses the stack claims, so Tab still tabs and Cmd-C
                    // still copies a snippet. The claim test is `stack_command` itself, the
                    // call the host makes.
                    if stack_command(focus, &key, mods).is_some() {
                        e.prevent_default();
                        e.stop_propagation();
                    }
                    on_key.call((key, mods));
                },
                div { class: "cv-head sk-head",
                    span { class: "cv-path sk-label", "{label}" }
                    span { class: "cv-pos sk-summary", "{summary}" }
                    if paged {
                        button {
                            class: "btn-edit-sm sk-page-prev",
                            id: STACK_PREV_ID,
                            disabled: !armed,
                            title: "Previous page of cards (<).",
                            onclick: move |_| on_page.call(FocusMove::Up),
                            "Previous page"
                        }
                        button {
                            class: "btn-edit-sm sk-page-next",
                            id: STACK_NEXT_ID,
                            disabled: !armed,
                            title: "Next page of cards (>).",
                            onclick: move |_| on_page.call(FocusMove::Down),
                            "Next page"
                        }
                    }
                    button {
                        class: "btn-edit-sm cv-close",
                        title: "Close the stack and go back to where it was opened from (Esc).",
                        onclick: move |_| on_close.call(()),
                        "Close"
                    }
                }
                if let Some(note) = page_note {
                    div { class: "cv-banner cv-banner-amber sk-page-note", "{note}" }
                }
                if let Some(note) = code_note {
                    div { class: "cv-banner cv-banner-muted sk-code-note", "{note}" }
                }
                div { class: "sk-actions",
                    button {
                        class: "btn-run sk-act-confirm",
                        id: STACK_CONFIRM_ID,
                        disabled: !armed,
                        title: "Confirm every checked card (C). No reason required.",
                        onclick: move |_| on_dispose.call(TriageState::Confirmed),
                        "Confirm checked"
                    }
                    button {
                        class: "btn-run sk-act-debt",
                        id: STACK_DEBT_ID,
                        disabled: !armed,
                        title: "Save every checked card as tech debt (T).",
                        onclick: move |_| on_dispose.call(TriageState::TechDebt),
                        "Tech debt"
                    }
                    button {
                        class: "btn-restart sk-act-ignore",
                        id: STACK_IGNORE_ID,
                        disabled: !armed,
                        title: "Ignore every checked card, with one reason for all of them (I).",
                        onclick: move |_| on_dispose.call(TriageState::Ignored),
                        "Ignore with reason"
                    }
                    button {
                        class: "btn-restart sk-act-fp",
                        id: STACK_FP_ID,
                        disabled: !armed,
                        title: "Mark every checked card a false positive, with one reason for all of them (F).",
                        onclick: move |_| on_dispose.call(TriageState::FalsePositive),
                        "False positive with reason"
                    }
                }
                div { class: "sk-list",
                    for (i, c) in cards.iter().enumerate() {
                        div {
                            key: "{c.key}",
                            class: if c.focused { "sk-card sk-card-sel" } else { "sk-card" },
                            "data-finding-key": "{c.key}",
                            "data-checked": "{c.checked}",
                            onclick: move |_| on_focus_card.call(i),
                            div { class: "sk-card-head",
                                span {
                                    class: if c.checked { "sk-check sk-check-on" } else { "sk-check" },
                                    "data-disabled": "{!armed}",
                                    role: "checkbox",
                                    "aria-checked": "{c.checked}",
                                    title: "Check or uncheck this card (Space when it is focused).",
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        on_focus_card.call(i);
                                        on_toggle.call(i);
                                    },
                                }
                                span {
                                    class: "sk-dot cv-sev-{c.severity_class}",
                                    title: "severity {c.severity}",
                                }
                                span { class: "sk-loc", "{c.location}" }
                                span {
                                    class: "rule-modal-tag sk-status",
                                    title: "{c.status_title}",
                                    "{c.status}"
                                }
                                button {
                                    class: "btn-edit-sm sk-open",
                                    disabled: !armed,
                                    title: "Open this finding in the code viewer (Enter on the focused card).",
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        on_focus_card.call(i);
                                        on_open.call(i);
                                    },
                                    "Code"
                                }
                            }
                            if let Some(w) = c.window.clone() {
                                pre { class: "finding-ctx-pre sk-ctx", "data-stack-context": "ok",
                                    {context_rows(&w)}
                                }
                            } else {
                                pre { class: "pane-snippet sk-snippet", "{c.snippet}" }
                            }
                        }
                    }
                }
                if let Some(p) = prompt {
                    QueueReasonPrompt { prompt: p, reason, on_reason, on_commit, on_cancel }
                }
                ViewerUndoBar { undo_entry, now_ms, on_undo, button_id: STACK_UNDO_ID.to_string() }
                div { class: if armed { "cv-keys armed" } else { "cv-keys" }, "{strip}" }
            }
        }
    }
}

// ── The host ─────────────────────────────────────────────────────────────────────────────

/// The stack's host at the results root. Renders nothing until the Queue has registered a link
/// and opened a stack; then it mounts [`StackHost`] on that opening, keyed by its id.
#[component]
pub(super) fn StackRoot() -> Element {
    let slot = try_use_context::<StackSlot>();
    let Some(link) = slot.and_then(|s| s.0.read().clone()) else {
        return rsx! {};
    };
    let Some(open) = link.open.read().clone() else {
        return rsx! {};
    };
    rsx! { StackHost { key: "{open.id}", link, open } }
}

/// The signals and the I/O. It makes no judgment: every key and every checkbox is a function of
/// `camerata_ui_core::stack`, and every decision is the Queue's `on_dispose`.
#[component]
fn StackHost(link: StackLink, open: StackOpen) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let cache = try_use_context::<Signal<ViewerCache>>();
    // The link's Copy parts, taken out so the closures below can each own them.
    let dispositions_sig = link.dispositions;
    let on_dispose_cb = link.on_dispose;
    let on_open_viewer = link.on_open_viewer;
    let on_close_cb = link.on_close;
    let mut state = use_signal({
        let open = open.clone();
        move || {
            StackState::new(
                open.label.clone(),
                open.rows.clone(),
                open.anchor.as_deref(),
            )
        }
    });
    let mut prompt = use_signal(|| Option::<ToolbarReasonPrompt>::None);
    let mut reason = use_signal(String::new);
    let mut windows = use_signal(HashMap::<String, PaneContext>::new);
    // The pages whose code has been asked for, so a page visited twice is asked once.
    let mut asked = use_signal(HashSet::<usize>::new);
    // Where the code for the page on screen stands.
    let mut loaded = use_signal(|| CodeLoad::Pending);
    // THE GATE for every mouse door: the same focus the key handler reads. While the reason
    // prompt is open its row set is a snapshot the heading has promised; a click that moved the
    // cursor, turned the page or decided other rows would make it a lie.
    let armed = move || stack_keys_armed(stack_focus(prompt.peek().is_some()));

    // The keyboard starts in the stack, and the anchor card is brought into view.
    use_effect(move || {
        focus_element(STACK_ID);
        if state.peek().cursor() > 0 {
            follow_selected(SELECTED_CARD);
        }
    });

    // ONE batch request per page the stack shows (at most STACK_PAGE items, the endpoint's cap).
    // A memo, so the request re-runs when the page changes and not on every checkbox.
    let page = use_memo(move || state.read().page());
    let project = Some(link.project_id.clone()).filter(|p| !p.is_empty());
    let shas = link.shas.clone();
    let _fetch = use_resource(move || {
        let page = page();
        let project = project.clone();
        let shas = shas.clone();
        async move {
            let targets: Vec<ContextTarget> = state
                .peek()
                .page_rows()
                .iter()
                .filter_map(|f| {
                    ContextTarget::for_finding(f, shas.get(&f.repo).map(String::as_str))
                })
                .collect();
            let total = state.peek().page_rows().len();
            // Marked AFTER the request, and only when the server answered: a page left before
            // its answer came (which cancels this resource), or one the server never answered,
            // is asked for again when the curator returns to it.
            let mut unanswered = false;
            if !asked.peek().contains(&page) {
                let got =
                    super::pane_context::load_windows(cache, &targets, project.as_deref()).await;
                windows.write().extend(got.found);
                if got.answered {
                    asked.write().insert(page);
                } else {
                    unanswered = true;
                }
            }
            let have = {
                let w = windows.peek();
                state
                    .peek()
                    .page_rows()
                    .iter()
                    .filter(|f| w.contains_key(&finding_key(f)))
                    .count()
            };
            loaded.set(if unanswered {
                CodeLoad::Failed
            } else {
                CodeLoad::Done { shown: have, total }
            });
        }
    });

    let decide = move |decided: TriageState, why: String| -> bool {
        if !armed() {
            return false;
        }
        let rows = state.peek().checked_rows(&dispositions_sig.peek());
        if rows.is_empty() {
            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, NONE_CHECKED_NOTE);
            return false;
        }
        on_dispose_cb.call((rows, decided, why))
    };
    let mut begin_prompt = move |decided: TriageState| {
        if !armed() {
            return;
        }
        let rows = state.peek().checked_rows(&dispositions_sig.peek());
        if rows.is_empty() {
            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, NONE_CHECKED_NOTE);
            return;
        }
        // The row set is snapshotted when the prompt opens, never re-read at commit time: the
        // heading promises a number, and the commit moves those rows.
        reason.set(String::new());
        prompt.set(Some(ToolbarReasonPrompt {
            state: decided,
            rows,
        }));
    };
    let mut toggle_cursor = move || {
        if !armed() {
            return;
        }
        let d = dispositions_sig.peek().clone();
        state.write().toggle_cursor(&d);
    };
    let open_cursor = move || {
        if !armed() {
            return;
        }
        if let Some(key) = state.peek().cursor_key() {
            on_open_viewer.call(key);
        }
    };
    let mut turn_page = move |dir: FocusMove| {
        if !armed() {
            return;
        }
        if state.write().turn_page(dir) {
            loaded.set(CodeLoad::Pending);
            dioxus::document::eval("var l=document.querySelector('.sk-list');if(l)l.scrollTop=0;");
        }
    };
    let close = move || on_close_cb.call(state.peek().cursor_key());

    let on_key = Callback::new(move |(key, mods): (String, KeyMods)| {
        let focus = stack_focus(prompt.peek().is_some());
        use camerata_ui_core::stack::StackCommand as C;
        match stack_command(focus, &key, mods) {
            Some(C::Close) => close(),
            Some(C::Move(dir)) => {
                state.write().move_cursor(dir);
                follow_selected(SELECTED_CARD);
            }
            Some(C::Toggle) => toggle_cursor(),
            Some(C::Open) => open_cursor(),
            Some(C::Dispose(s)) => {
                decide(s, String::new());
            }
            Some(C::PromptFor(s)) => begin_prompt(s),
            Some(C::Page(dir)) => turn_page(dir),
            None => {}
        }
    });
    let on_commit = Callback::new(move |()| {
        let Some(p) = prompt.peek().clone() else {
            return;
        };
        let landed = on_dispose_cb.call((p.rows, p.state, reason.peek().clone()));
        // A refusal (a blank reason) leaves the prompt OPEN with what was typed, and the Queue's
        // own refusal toast has said why.
        if landed {
            prompt.set(None);
            reason.set(String::new());
            focus_element(STACK_ID);
        }
    });
    let on_cancel = Callback::new(move |()| {
        prompt.set(None);
        reason.set(String::new());
        focus_element(STACK_ID);
    });

    let dispositions = dispositions_sig.read().clone();
    let cards = card_views(&state.read(), &dispositions, &windows.read());
    let (label, summary, page_note, paged) = {
        let s = state.read();
        (
            s.label().to_string(),
            s.summary(&dispositions),
            s.page_note(),
            s.is_paged(),
        )
    };
    let focus = stack_focus(prompt.read().is_some());
    rsx! {
        StackView {
            label,
            summary,
            page_note,
            code_note: context_note(loaded()),
            paged,
            cards,
            focus,
            prompt: prompt(),
            reason: reason(),
            undo_entry: link.undo_entry,
            now_ms: link.now_ms,
            on_key,
            on_toggle: Callback::new(move |i: usize| {
                if !armed() {
                    return;
                }
                let key = state.peek().page_rows().get(i).map(finding_key);
                if let Some(key) = key {
                    state.write().focus_key(&key);
                    toggle_cursor();
                }
            }),
            on_focus_card: Callback::new(move |i: usize| {
                // Also keeps the reason field's focus: this call would take it.
                if !armed() {
                    return;
                }
                let key = state.peek().page_rows().get(i).map(finding_key);
                if let Some(key) = key {
                    state.write().focus_key(&key);
                }
                focus_element(STACK_ID);
            }),
            on_open: Callback::new(move |_: usize| open_cursor()),
            on_dispose: Callback::new(move |s: TriageState| {
                if s.requires_reason() {
                    begin_prompt(s);
                } else {
                    decide(s, String::new());
                }
            }),
            on_page: Callback::new(move |dir: FocusMove| turn_page(dir)),
            on_close: Callback::new(move |()| close()),
            on_undo: link.on_undo,
            on_reason: Callback::new(move |r: String| reason.set(r)),
            on_commit,
            on_cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    //! B.2 acceptance. The props-only view is rendered (SSR) in each state, and the real
    //! `FindingsTable` with `StackRoot` and `ViewerRoot` is mounted and driven by real `keydown`,
    //! `input` and `click` events dispatched to the element ids the VirtualDom reports, against
    //! a fake server that counts the batch requests it receives.

    use std::any::Any;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use camerata_ui_core::order_preset::OrderPreset;
    use camerata_ui_core::triage::TechDebtBucket;
    use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
    use dioxus::html::{
        Code, Key, Location, Modifiers, SerializedFocusData, SerializedFormData,
        SerializedKeyboardData, SerializedMouseData,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::super::scan::FindingsTable;
    use super::super::viewer::{ViewerRoot, ViewerSlot, FOCUS_LOG, VIEWER_ID};
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn deviation(line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": line, "rule_id": "API-1",
            "severity": "high", "snippet": format!("code at {line}"), "detail": "d",
            "status": "active", "kind": "deviation", "deviating_sites": 3,
            "assessed_sites": 40, "convention_label": "URI versioning"
        }))
        .expect("fixture")
    }

    fn defect(line: usize) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": line, "rule_id": "SQL-1",
            "severity": "critical", "snippet": format!("code at {line}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    fn deviations(n: usize) -> Vec<FindingView> {
        (1..=n).map(deviation).collect()
    }

    // ── The view, props only ────────────────────────────────────────────────────────────

    fn window(line: usize) -> PaneContext {
        PaneContext {
            start_line: line - 1,
            violation_line: line,
            lines: vec!["above".into(), format!("code at {line}"), "below".into()],
        }
    }

    fn card(line: usize, checked: bool, window: Option<PaneContext>) -> StackCardView {
        StackCardView {
            key: format!("k{line}"),
            location: format!("src/a.rs:{line}"),
            severity_class: "high".into(),
            severity: "high".into(),
            status: "Unresolved".into(),
            status_title: String::new(),
            checked,
            focused: line == 10,
            snippet: format!("stored {line}"),
            window,
        }
    }

    #[component]
    fn ViewHarness(
        cards: Vec<StackCardView>,
        prompt: Option<ToolbarReasonPrompt>,
        page_note: Option<String>,
        code_note: Option<String>,
        paged: bool,
        focus: StackFocus,
    ) -> Element {
        let undo_entry = use_signal(|| Option::<UndoEntry>::None);
        let now_ms = use_signal(|| 0u64);
        rsx! {
            StackView {
                label: "API-1",
                summary: "2 cards on screen, 1 checked",
                page_note,
                code_note,
                paged,
                cards,
                focus,
                prompt,
                undo_entry,
                now_ms,
                on_key: Callback::new(|_: (String, KeyMods)| {}),
                on_toggle: Callback::new(|_: usize| {}),
                on_focus_card: Callback::new(|_: usize| {}),
                on_open: Callback::new(|_: usize| {}),
                on_dispose: Callback::new(|_: TriageState| {}),
                on_page: Callback::new(|_: FocusMove| {}),
                on_close: Callback::new(|()| {}),
                on_undo: Callback::new(|()| {}),
                on_reason: Callback::new(|_: String| {}),
                on_commit: Callback::new(|()| {}),
                on_cancel: Callback::new(|()| {}),
            }
        }
    }

    fn render_view(
        cards: Vec<StackCardView>,
        prompt: Option<ToolbarReasonPrompt>,
        page_note: Option<String>,
        code_note: Option<String>,
        paged: bool,
    ) -> String {
        let focus = if prompt.is_some() {
            StackFocus::ReasonPrompt
        } else {
            StackFocus::Cards
        };
        let mut vdom = VirtualDom::new_with_props(
            ViewHarness,
            ViewHarnessProps {
                cards,
                prompt,
                page_note,
                code_note,
                paged,
                focus,
            },
        );
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    #[test]
    fn a_card_shows_location_dot_status_checkbox_and_the_code_with_the_violation_marked() {
        let html = render_view(
            vec![card(10, true, Some(window(10))), card(20, false, None)],
            None,
            None,
            None,
            false,
        );
        assert!(html.contains("src/a.rs:10") && html.contains("src/a.rs:20"));
        assert!(html.contains("sk-dot cv-sev-high"), "{html}");
        assert!(html.contains("aria-checked=\"true\""));
        assert!(html.contains("aria-checked=\"false\""));
        assert!(html.contains("Unresolved"));
        // Card one has its window: three lines, only the violation line highlighted.
        assert!(html.contains("data-stack-context=\"ok\""));
        assert!(html.contains("above") && html.contains("below"));
        assert_eq!(html.matches("finding-ctx-line-hit").count(), 1, "{html}");
        // Card two has none: the stored snippet, and nothing is invented.
        assert!(html.contains("stored 20"));
        assert!(
            !html.contains("stored 10"),
            "a card with code does not repeat the snippet"
        );
        assert_eq!(html.matches("sk-card-sel").count(), 1, "one focused card");
    }

    #[test]
    fn the_prompt_names_the_count_and_the_page_and_code_notes_show_when_given() {
        let prompt = ToolbarReasonPrompt {
            state: TriageState::FalsePositive,
            rows: deviations(2),
        };
        let html = render_view(
            vec![card(10, true, None)],
            Some(prompt),
            Some("Showing cards 1 to 200 of 312.".into()),
            context_note(CodeLoad::Done { shown: 0, total: 5 }),
            true,
        );
        assert!(html.contains("2 findings (required)"), "{html}");
        assert!(html.contains("Showing cards 1 to 200 of 312."));
        assert!(html.contains("Next page") && html.contains("Previous page"));
        assert!(html.contains("stored line only"));
        assert!(
            !html.contains("cv-keys armed"),
            "the keys are off under the prompt: {html}"
        );
        let plain = render_view(vec![card(10, true, None)], None, None, None, false);
        assert!(!plain.contains("Next page"), "no page buttons for one page");
        assert!(plain.contains("cv-keys armed"));
    }

    #[test]
    fn the_notes_are_honest_and_carry_no_em_dash() {
        assert_eq!(
            context_note(CodeLoad::Pending),
            None,
            "nothing to say while loading"
        );
        assert_eq!(
            context_note(CodeLoad::Done {
                shown: 30,
                total: 30
            }),
            None,
            "all have code"
        );
        let some = context_note(CodeLoad::Done {
            shown: 25,
            total: 30,
        })
        .unwrap();
        assert!(some.starts_with("5 of 30 cards"), "{some}");
        let none = context_note(CodeLoad::Done {
            shown: 0,
            total: 30,
        })
        .unwrap();
        assert!(none.contains("these 30 findings"), "{none}");
        // A server that never answered is not "could not be read at the inspected commit".
        let failed = context_note(CodeLoad::Failed).unwrap();
        assert!(failed.contains("did not answer"), "{failed}");
        assert!(!failed.contains("inspected commit"), "{failed}");
        for text in [
            some,
            none,
            failed,
            key_strip(),
            NONE_CHECKED_NOTE.to_string(),
            super::super::viewer::GONE_PROBLEM.to_string(),
        ] {
            assert!(!text.contains('\u{2014}'), "{text}");
        }
    }

    #[test]
    fn card_views_give_code_only_to_cards_that_have_a_window_and_show_decisions() {
        let rows = deviations(3);
        let stack = StackState::new("API-1", rows.clone(), None);
        let mut dispositions = HashMap::new();
        dispositions.insert(
            finding_key(&rows[1]),
            Disposition {
                state: TriageState::FalsePositive,
                reason: "parser misread".into(),
                bucket: TechDebtBucket::Later,
            },
        );
        let mut windows = HashMap::new();
        windows.insert(finding_key(&rows[0]), window(1));
        let cards = card_views(&stack, &dispositions, &windows);
        assert_eq!(cards.len(), 3);
        assert!(cards[0].window.is_some() && cards[1].window.is_none());
        assert!(cards[0].checked && !cards[1].checked && cards[2].checked);
        assert_eq!(cards[1].status, "False positive");
        assert_eq!(cards[1].status_title, "parser misread");
        assert_eq!(
            cards[0].status_title, "",
            "only a reasoned decision has a reason"
        );
        assert!(cards[0].focused && !cards[1].focused);
        assert_eq!(cards[0].location, "src/a.rs:1");
    }

    #[test]
    fn origin_focus_ids_name_the_pane_and_the_bar_button() {
        assert_eq!(
            origin_focus_id(StackOrigin::Pane),
            super::super::scan::DETAIL_PANE_ID
        );
        assert_eq!(origin_focus_id(StackOrigin::BulkBar(2)), bar_door_id(2));
        assert_ne!(bar_door_id(0), bar_door_id(1));
    }

    // ── The mounted stack ───────────────────────────────────────────────────────────────

    #[derive(Clone, Copy)]
    struct Probe {
        dispositions: Signal<HashMap<String, Disposition>>,
        toasts: Signal<Vec<crate::toast::Toast>>,
    }

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
        static ROWS: RefCell<Vec<FindingView>> = const { RefCell::new(Vec::new()) };
        static PRESET: std::cell::Cell<OrderPreset> =
            const { std::cell::Cell::new(OrderPreset::Curated) };
        static VIEW: std::cell::Cell<TriageState> =
            const { std::cell::Cell::new(TriageState::Unresolved) };
        static INITIAL: RefCell<HashMap<String, Disposition>> = RefCell::new(HashMap::new());
    }

    fn app() -> Element {
        let toasts = use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(None::<FindingView>));
        use_context_provider(|| Signal::new(ViewerCache::new()));
        use_context_provider(|| ViewerSlot(Signal::new(None)));
        use_context_provider(|| StackSlot(Signal::new(None)));
        let dispositions = use_signal(|| INITIAL.with(|i| i.borrow().clone()));
        let order_preset = use_signal(|| PRESET.with(std::cell::Cell::get));
        PROBE.with(|p| {
            *p.borrow_mut() = Some(Probe {
                dispositions,
                toasts,
            })
        });
        let findings = ROWS.with(|r| r.borrow().clone());
        let mut shas = HashMap::new();
        shas.insert("me/api".to_string(), SHA.to_string());
        rsx! {
            FindingsTable {
                findings,
                repos: vec!["me/api".to_string()],
                descriptions: HashMap::new(),
                dispositions,
                order_preset,
                triage_view: VIEW.with(std::cell::Cell::get),
                audited_shas: shas,
            }
            StackRoot {}
            ViewerRoot {}
        }
    }

    /// The batch endpoint, faked: answers every item `ok` with the line and one either side, or
    /// every item with a refusal status.
    struct Answerer {
        status: &'static str,
    }

    impl Respond for Answerer {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            if self.status == "garbage" {
                // The server answers, but not with anything the client can read: no answer.
                return ResponseTemplate::new(200).set_body_string("not json");
            }
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let items: Vec<serde_json::Value> = body["items"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let line = item["line"].as_u64().unwrap();
                    let mut a = serde_json::json!({
                        "status": self.status, "index": index, "key": item["key"],
                        "repo": item["repo"], "path": item["path"], "line": line,
                    });
                    if self.status == "ok" {
                        a["source"] = "git".into();
                        // The server clips the window to the file: line 1 has nothing above it.
                        let code = format!("code at {line}");
                        a["violation_line"] = line.into();
                        if line == 1 {
                            a["start_line"] = 1.into();
                            a["lines"] = serde_json::json!([code, "below"]);
                        } else {
                            a["start_line"] = (line - 1).into();
                            a["lines"] = serde_json::json!(["above", code, "below"]);
                        }
                        a["matches_snippet"] = true.into();
                    }
                    a
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": items }))
        }
    }

    async fn serve(status: &'static str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/finding-context/batch"))
            .respond_with(Answerer { status })
            .mount(&server)
            .await;
        // The code viewer, opened from a card.
        let lines: Vec<serde_json::Value> = (1..=60)
            .map(|n| serde_json::json!([[null, format!("file line {n}")]]))
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/onboard/source-file"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok", "source": "git", "commit": SHA, "head_differs": false,
                "language": "rust", "total_lines": 60,
                "window": {"from": 1, "to": 60, "total_lines": 60},
                "highlight": "ok", "classes": [], "block": null, "lines": lines,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-context"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"status": "file_missing"})),
            )
            .mount(&server)
            .await;
        server
    }

    /// The batch POST bodies the fake server received.
    async fn batches(server: &MockServer) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|q| q.method.as_str() == "POST" && q.url.path().ends_with("/batch"))
            .map(|q| serde_json::from_slice(&q.body).unwrap())
            .collect()
    }

    /// Listeners and element ids the VirtualDom has reported so far.
    #[derive(Default)]
    struct Seen {
        listeners: Vec<(String, ElementId)>,
        ids: HashMap<String, ElementId>,
        /// Each row's selection checkbox: the first `checked` attribute after the row's index.
        boxes: HashMap<usize, ElementId>,
        /// The elements given a `style` after each row's `data-chorale-index`: the row's cells.
        row_styles: HashMap<usize, Vec<ElementId>>,
        current_row: Option<usize>,
    }

    impl Seen {
        fn take(&mut self, m: &Mutations) {
            for e in &m.edits {
                match e {
                    Mutation::NewEventListener { name, id } => {
                        self.listeners.push((name.clone(), *id));
                    }
                    Mutation::SetAttribute {
                        name: "checked",
                        id,
                        ..
                    } => {
                        if let Some(row) = self.current_row {
                            self.boxes.entry(row).or_insert(*id);
                        }
                    }
                    Mutation::SetAttribute {
                        name: "data-chorale-index",
                        value: dioxus::dioxus_core::AttributeValue::Text(text),
                        ..
                    } => {
                        self.current_row = text.parse().ok();
                    }
                    Mutation::SetAttribute {
                        name: "style", id, ..
                    } => {
                        if let Some(row) = self.current_row {
                            self.row_styles.entry(row).or_default().push(*id);
                        }
                    }
                    Mutation::SetAttribute {
                        name: "id",
                        value: dioxus::dioxus_core::AttributeValue::Text(text),
                        id,
                        ..
                    } => {
                        self.ids.insert(text.clone(), *id);
                    }
                    _ => {}
                }
            }
        }
        fn mark(&self) -> usize {
            self.listeners.len()
        }
        /// A cell of table row `row` that has a click listener (what chorale's row click is).
        fn row_cell(&self, row: usize) -> ElementId {
            self.row_styles
                .get(&row)
                .into_iter()
                .flatten()
                .copied()
                .find(|id| self.listeners.iter().any(|(n, l)| n == "click" && l == id))
                .unwrap_or_else(|| panic!("no clickable cell in row {row}"))
        }
        /// The selection checkbox of table row `row`.
        fn row_box(&self, row: usize) -> ElementId {
            self.boxes[&row]
        }
        /// The first listener `name` registered after `mark`.
        fn first_after(&self, mark: usize, name: &str) -> ElementId {
            self.listeners[mark..]
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| panic!("no new {name} listener"))
        }
        fn element(&self, dom_id: &str) -> ElementId {
            *self
                .ids
                .get(dom_id)
                .unwrap_or_else(|| panic!("no element with id {dom_id}: {:?}", self.ids.keys()))
        }
    }

    async fn settle(vdom: &mut VirtualDom, seen: &mut Seen) {
        for _ in 0..12 {
            let _ = tokio::time::timeout(Duration::from_millis(30), vdom.wait_for_work()).await;
            seen.take(&vdom.render_immediate_to_vec());
        }
    }

    fn send(vdom: &VirtualDom, name: &str, id: ElementId, data: Box<dyn Any>) {
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
    }

    fn key(vdom: &VirtualDom, id: ElementId, pressed: &str) {
        let named = match pressed {
            "Enter" => Key::Enter,
            "Escape" => Key::Escape,
            "ArrowDown" => Key::ArrowDown,
            other => Key::Character(other.to_string()),
        };
        let data = SerializedKeyboardData::new(
            named,
            Code::Unidentified,
            Location::Standard,
            false,
            Modifiers::empty(),
            false,
        );
        send(vdom, "keydown", id, Box::new(data));
    }

    fn click(vdom: &VirtualDom, id: ElementId) {
        send(vdom, "click", id, Box::new(SerializedMouseData::default()));
    }

    fn typed(vdom: &VirtualDom, id: ElementId, text: &str) {
        send(
            vdom,
            "input",
            id,
            Box::new(SerializedFormData::new(text.to_string(), Vec::new())),
        );
    }

    /// Mount the Queue over `rows` (with the stack and viewer roots), against a fake server.
    async fn mount(
        rows: Vec<FindingView>,
        preset: OrderPreset,
        status: &'static str,
    ) -> (VirtualDom, Seen, MockServer, camerata_test_env::EnvGuard) {
        mount_in(
            rows,
            preset,
            status,
            TriageState::Unresolved,
            HashMap::new(),
        )
        .await
    }

    /// `mount`, showing the triage table `view` over rows that already carry `initial`.
    async fn mount_in(
        rows: Vec<FindingView>,
        preset: OrderPreset,
        status: &'static str,
        view: TriageState,
        initial: HashMap<String, Disposition>,
    ) -> (VirtualDom, Seen, MockServer, camerata_test_env::EnvGuard) {
        VIEW.with(|v| v.set(view));
        INITIAL.with(|i| *i.borrow_mut() = initial);
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let server = serve(status).await;
        let env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        ROWS.with(|r| *r.borrow_mut() = rows);
        PRESET.with(|p| p.set(preset));
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        let mut vdom = VirtualDom::new(app);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        settle(&mut vdom, &mut seen).await;
        (vdom, seen, server, env)
    }

    fn html(vdom: &VirtualDom) -> String {
        dioxus_ssr::render(vdom)
    }

    fn probe() -> Probe {
        PROBE.with(|p| p.borrow().expect("the harness stored its probe"))
    }

    fn state_count(want: TriageState) -> usize {
        probe()
            .dispositions
            .peek()
            .values()
            .filter(|d| d.state == want)
            .count()
    }

    fn last_focus() -> Option<String> {
        FOCUS_LOG.with(|l| l.borrow().last().cloned())
    }

    /// Open the stack from the first group's bulk-bar button; returns the panel's key target.
    async fn open_from_bar(vdom: &mut VirtualDom, seen: &mut Seen) -> ElementId {
        let before = seen.mark();
        click(vdom, seen.element(&bar_door_id(0)));
        settle(vdom, seen).await;
        seen.first_after(before, "keydown")
    }

    fn checked_cards(page: &str) -> usize {
        page.matches("data-checked=\"true\"").count()
    }

    /// B.2 check 1: opening a 30-row group issues ONE batch request carrying all 30 items, and
    /// renders 30 cards with the code around each line, the violation line highlighted, and every
    /// undecided card checked.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn opening_a_30_row_group_makes_one_batch_request_and_30_checked_cards() {
        let (mut vdom, mut seen, server, _env) =
            mount(deviations(30), OrderPreset::Curated, "ok").await;
        let page = html(&vdom);
        assert!(
            page.contains("Review all 30 stacked"),
            "the bulk bar's door: {page}"
        );
        assert!(!page.contains("sk-panel"), "closed until asked for");

        open_from_bar(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(
            page.matches("data-stack-context=\"ok\"").count(),
            30,
            "{page}"
        );
        assert_eq!(page.matches("finding-ctx-line-hit").count(), 30);
        assert_eq!(checked_cards(&page), 30, "every undecided card is checked");
        assert!(page.contains("30 cards on screen, 30 checked"));
        assert!(page.contains("src/a.rs:1") && page.contains("src/a.rs:30"));
        assert!(page.contains("above") && page.contains("below"));

        let posted = batches(&server).await;
        assert_eq!(posted.len(), 1, "ONE batch request per group open");
        let items = posted[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 30);
        assert_eq!(items[0]["sha"], SHA, "read at the inspected commit");
        assert_eq!(
            items[0]["expect"], "code at 1",
            "with the stored line to match"
        );
    }

    /// B.2 check 2: Space unchecks a card; C confirms exactly the checked cards; the undo names
    /// the count; the unchecked card stays Unresolved.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn space_unchecks_c_confirms_the_checked_ones_and_the_undo_names_the_count() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(30), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;

        key(&vdom, panel, " ");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(checked_cards(&page), 29, "Space unchecked the focused card");
        assert!(page.contains("30 cards on screen, 29 checked"));
        assert_eq!(
            state_count(TriageState::Confirmed),
            0,
            "nothing decided yet"
        );

        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 29);
        assert_eq!(
            probe().dispositions.peek().get(&finding_key(&deviation(1))),
            None,
            "the unchecked card stays Unresolved"
        );
        let page = html(&vdom);
        assert!(
            page.contains("Confirmed 29 findings."),
            "the undo names the count: {page}"
        );
        assert!(page.contains("sk-panel"), "the stack stays open");
        assert_eq!(
            page.matches(">Confirmed<").count(),
            29,
            "decided cards update in place: {page}"
        );
        assert_eq!(page.matches(">Unresolved<").count(), 1);
        assert_eq!(
            checked_cards(&page),
            0,
            "decided cards, and the one left out, are unchecked"
        );
        assert_eq!(probe().toasts.peek().len(), 0, "no refusal toast");
    }

    /// The undo is the Queue's own: one action puts every card back, and they read checked again.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_undo_returns_every_card_to_unresolved_and_checked() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(6), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 6);
        assert!(html(&vdom).contains("Confirmed 6 findings."));
        assert_eq!(checked_cards(&html(&vdom)), 0);

        // The stack's own Undo button (the pane has one too, so it is found by id).
        click(&vdom, seen.element(STACK_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(
            probe().dispositions.peek().is_empty(),
            "absence is restored as absence"
        );
        let page = html(&vdom);
        assert_eq!(page.matches(">Unresolved<").count(), 6, "{page}");
        assert_eq!(checked_cards(&page), 6, "and they read checked again");
        let undone = probe()
            .toasts
            .peek()
            .iter()
            .find(|t| t.message.contains("6 finding(s) put back"))
            .map(|t| t.message.clone())
            .expect("the undo is said");
        assert!(
            !undone.contains('\u{2014}'),
            "no em-dash in user text: {undone}"
        );
    }

    /// B.2 check 3: F with two cards checked opens ONE prompt naming the count; a blank reason is
    /// refused and nothing is written; Enter with a reason marks both false positive with it.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn f_with_two_checked_asks_one_reason_blank_is_refused_enter_marks_both() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(2), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        assert_eq!(checked_cards(&html(&vdom)), 2);

        let mark = seen.mark();
        key(&vdom, panel, "f");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(
            page.matches("queue-reason-prompt").count(),
            1,
            "one prompt: {page}"
        );
        assert!(page.contains("False positive") && page.contains("2 findings (required)"));
        assert!(
            !page.contains("cv-keys armed"),
            "the keys are off while the prompt owns them"
        );
        assert_eq!(state_count(TriageState::FalsePositive), 0);

        // A blank reason is refused: the prompt stays open and nothing is written.
        let field = seen.first_after(mark, "keydown");
        key(&vdom, field, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(
            html(&vdom).contains("queue-reason-prompt"),
            "the prompt stays open"
        );
        assert_eq!(
            state_count(TriageState::FalsePositive),
            0,
            "nothing written"
        );
        assert!(
            probe().toasts.peek().iter().any(|t| t
                .message
                .contains("A reason is required to move 2 findings")),
            "the refusal is said, with the count: {:?}",
            probe()
                .toasts
                .peek()
                .iter()
                .map(|t| t.message.clone())
                .collect::<Vec<_>>()
        );

        // A reason, then Enter: both cards, that reason.
        typed(
            &vdom,
            seen.first_after(mark, "input"),
            "the parser misread both",
        );
        settle(&mut vdom, &mut seen).await;
        key(&vdom, field, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::FalsePositive), 2);
        for d in probe().dispositions.peek().values() {
            assert_eq!(d.reason, "the parser misread both");
        }
        let page = html(&vdom);
        assert!(!page.contains("queue-reason-prompt"), "the prompt closed");
        assert!(
            page.contains("Marked 2 findings as False positive."),
            "{page}"
        );
        assert_eq!(last_focus(), Some(STACK_ID.to_string()));
    }

    /// `I` is the same door with the other reason-carrying disposition; `T` needs no reason, and
    /// a decision key with nothing checked says so instead of doing nothing silently.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn i_asks_one_reason_and_t_saves_the_checked_cards_as_tech_debt() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, " "); // uncheck the first
        let mark = seen.mark();
        key(&vdom, panel, "i");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("Ignored") && page.contains("2 findings (required)"));
        typed(
            &vdom,
            seen.first_after(mark, "input"),
            "test fixture, not shipped",
        );
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Ignored), 2);

        // Nothing is checked now.
        key(&vdom, panel, "t");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::TechDebt), 0);
        assert!(probe()
            .toasts
            .peek()
            .iter()
            .any(|t| t.message == NONE_CHECKED_NOTE));
        // Check the one left and T saves it.
        key(&vdom, panel, " ");
        key(&vdom, panel, "t");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::TechDebt), 1);
    }

    /// B.2 check 4: Enter on a card opens the viewer at that finding; Esc returns to the stack
    /// with the same card focused, and the keyboard goes back to the stack.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn enter_opens_the_viewer_on_the_card_and_escape_returns_to_the_same_card() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(5), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "j");
        key(&vdom, panel, "j");
        settle(&mut vdom, &mut seen).await;
        let focused_card = |page: &str| {
            let at = page.find("sk-card sk-card-sel").expect("a focused card");
            let rest = &page[at..];
            let marker = "data-finding-key=\"";
            let k = rest.find(marker).unwrap() + marker.len();
            rest[k..k + rest[k..].find('"').unwrap()].to_string()
        };
        let before = focused_card(&html(&vdom));
        assert!(before.contains("|3|"), "J J is the third card: {before}");

        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("file line 3") && page.contains("cv-line-hit"),
            "the viewer is open on the file: {page}"
        );
        assert!(
            page.matches(&format!("data-finding-key=\"{before}\""))
                .count()
                >= 2,
            "the viewer is on that finding"
        );

        // Esc in the viewer: it closes, the stack is still there on the same card.
        key(&vdom, seen.element(VIEWER_ID), "Escape");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(!page.contains("file line 3"), "the viewer closed: {page}");
        assert!(page.contains("sk-panel"));
        assert_eq!(focused_card(&page), before, "the same card is focused");
        assert_eq!(
            last_focus(),
            Some(STACK_ID.to_string()),
            "the keyboard went back to the stack"
        );
    }

    /// A card whose row has been decided has left the table: Enter says so rather than opening
    /// the viewer on nothing.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn enter_on_a_decided_card_says_it_cannot_be_opened() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(2), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("file line"));
        // One wording table: only a finding that has left the table is told how to get it back.
        assert!(probe()
            .toasts
            .peek()
            .iter()
            .any(|t| t.message == super::super::viewer::GONE_PROBLEM));
    }

    /// A group whose batch answer is a refusal shows the stored snippets only, and says so.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_refused_batch_leaves_snippets_only_and_the_cards_still_work() {
        let (mut vdom, mut seen, server, _env) =
            mount(deviations(4), OrderPreset::Curated, "commit_unavailable").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(page.matches("sk-snippet").count(), 4, "{page}");
        assert!(!page.contains("data-stack-context"), "no code is invented");
        assert!(page.contains("code at 1"), "the stored line");
        assert!(
            page.contains("stored line only"),
            "and the curator is told: {page}"
        );
        assert_eq!(batches(&server).await.len(), 1);
        // Deciding does not depend on the code.
        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 4);
    }

    /// A group of more than 200 pages, says so, makes one request per page (200 items, then the
    /// rest), and the keys act on the page on screen.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_group_over_200_pages_and_says_so() {
        let (mut vdom, mut seen, server, _env) =
            mount(deviations(212), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("Showing cards 1 to 200 of 212"), "{page}");
        assert_eq!(checked_cards(&page), 200);
        assert_eq!(page.matches("data-stack-context=\"ok\"").count(), 200);
        let posted = batches(&server).await;
        assert_eq!(posted.len(), 1);
        assert_eq!(
            posted[0]["items"].as_array().unwrap().len(),
            200,
            "the endpoint's cap"
        );

        // C acts on the page on screen only.
        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 200);
        assert!(html(&vdom).contains("Confirmed 200 findings."));

        // The next page: its 12 cards, one more request of 12 items.
        key(&vdom, panel, ">");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("Showing cards 201 to 212 of 212"), "{page}");
        assert_eq!(checked_cards(&page), 12);
        let posted = batches(&server).await;
        assert_eq!(posted.len(), 2, "one request per page");
        assert_eq!(posted[1]["items"].as_array().unwrap().len(), 12);
        // And back: no second request for a page already asked for.
        key(&vdom, panel, "<");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(batches(&server).await.len(), 2);
    }

    /// The pane's `B` opens the stack on the focused row's group, and Esc gives the keyboard back
    /// to the pane.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn b_in_the_pane_opens_the_stack_and_escape_returns_focus_to_the_pane() {
        let mut rows = vec![defect(7), defect(8)];
        rows.push(deviation(1));
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        // The pane holds focus: what the DOM reports on `focusin`.
        send(
            &vdom,
            "focusin",
            pane,
            Box::new(SerializedFocusData::default()),
        );
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("pane-stack"),
            "the pane offers its door: {page}"
        );

        let before = seen.mark();
        key(&vdom, pane, "b");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("sk-panel"), "B opened the stack: {page}");
        assert!(
            page.contains("data-stack-label=\"SQL-1\""),
            "on the focused row's group"
        );
        assert_eq!(page.matches("sk-card-sel").count(), 1);
        assert!(
            page.contains("2 cards on screen, 2 checked"),
            "a defect group of two: {page}"
        );
        let panel = seen.first_after(before, "keydown");

        key(&vdom, panel, "Escape");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(!page.contains("sk-panel"), "Esc closed it");
        assert_eq!(
            last_focus(),
            Some(super::super::scan::DETAIL_PANE_ID.to_string()),
            "the keyboard is back on the pane"
        );
    }

    /// Opened from the bulk bar, Esc gives the keyboard back to the button that opened it.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn escape_returns_the_keyboard_to_the_bulk_bar_button() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "Escape");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("sk-panel"));
        assert_eq!(last_focus(), Some(bar_door_id(0)));
    }

    /// The panel can still hear a press that bubbles up from the reason field; it must not act
    /// on it: C is not a confirm while a reason is typed, and Esc does not close the stack under
    /// the prompt.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_keys_do_nothing_while_the_prompt_owns_the_keyboard() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(2), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "f");
        settle(&mut vdom, &mut seen).await;
        key(&vdom, panel, "c");
        key(&vdom, panel, "Escape");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 0);
        assert!(
            html(&vdom).contains("sk-panel"),
            "Esc did not close the stack under a prompt"
        );
    }

    /// The checked rows leave the Queue's table (the burn-down gesture, as every other bulk
    /// decision does), and ONE undo puts them all back on screen, not merely in the map.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn deciding_the_cards_removes_their_rows_from_the_table_and_undo_restores_them() {
        let rows: Vec<FindingView> = (1..=4).map(defect).collect();
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let table_rows = |vdom: &VirtualDom| html(vdom).matches("data-chorale-index=\"").count();
        assert_eq!(table_rows(&vdom), 4, "the flat list shows its four rows");

        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        send(
            &vdom,
            "focusin",
            pane,
            Box::new(SerializedFocusData::default()),
        );
        let before = seen.mark();
        key(&vdom, pane, "b");
        settle(&mut vdom, &mut seen).await;
        let panel = seen.first_after(before, "keydown");
        key(&vdom, panel, " "); // leave the first card alone
        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 3);
        assert_eq!(
            table_rows(&vdom),
            1,
            "three rows left the table, one stayed"
        );

        click(&vdom, seen.element(STACK_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(probe().dispositions.peek().is_empty());
        assert_eq!(
            table_rows(&vdom),
            4,
            "one undo put all three back on screen"
        );
    }

    // ── H1: the mouse doors obey the same gate as the keys ──────────────────────────────

    /// With the reason prompt open the heading has promised a number of cards. A click that turns
    /// the page, then Enter, must not decide rows the curator is no longer looking at, and a
    /// click on "Confirm checked" must not flip the very rows the prompt is about to decide.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_click_on_next_page_under_the_prompt_changes_nothing_and_enter_decides_what_it_named()
    {
        let (mut vdom, mut seen, server, _env) =
            mount(deviations(212), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        let mark = seen.mark();
        key(&vdom, panel, "f");
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("200 findings (required)"));

        // The page buttons are disabled in the markup...
        let page = html(&vdom);
        assert!(
            page.contains("disabled"),
            "the controls say they are off: {page}"
        );
        // ...and, since a browser's `disabled` is only a hint, the handler refuses too.
        click(&vdom, seen.element(STACK_NEXT_ID));
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("Showing cards 1 to 200 of 212"),
            "the page did not turn: {page}"
        );
        assert_eq!(batches(&server).await.len(), 1, "and nothing was asked for");
        // The panel's Enter (open the viewer) is refused under the prompt as well.
        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("file line"));

        // The prompt still decides exactly the 200 it named, and none of the other 12.
        typed(
            &vdom,
            seen.first_after(mark, "input"),
            "the parser misread these",
        );
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::FalsePositive), 200);
        assert_eq!(
            probe()
                .dispositions
                .peek()
                .get(&finding_key(&deviation(201))),
            None,
            "a row of the page that was never on screen is untouched"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_click_on_confirm_checked_under_the_prompt_does_nothing() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "f");
        settle(&mut vdom, &mut seen).await;
        for id in [
            STACK_CONFIRM_ID,
            STACK_DEBT_ID,
            STACK_IGNORE_ID,
            STACK_FP_ID,
        ] {
            click(&vdom, seen.element(id));
        }
        settle(&mut vdom, &mut seen).await;
        assert!(
            probe().dispositions.peek().is_empty(),
            "no button decided anything under the prompt"
        );
        let page = html(&vdom);
        assert_eq!(
            page.matches("queue-reason-prompt").count(),
            1,
            "and the prompt is still the one it was: {page}"
        );
        assert!(
            page.contains("False positive"),
            "still the False positive prompt"
        );
    }

    /// The card-level mouse doors are shut under the prompt too: a checkbox click does not flip
    /// a card the prompt has already counted, and a click on a card does not take the keyboard
    /// out of the reason field.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn checkbox_and_card_clicks_under_the_prompt_change_nothing() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        let opened = seen.mark();
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        let after_open = seen.mark();
        key(&vdom, panel, "f");
        settle(&mut vdom, &mut seen).await;
        // The stack's own click listeners (the page, decision, checkbox, card and Code
        // controls), in the order they were created. The first is Close, which is allowed
        // under the prompt and is left alone.
        let cards: Vec<ElementId> = seen.listeners[opened..after_open]
            .iter()
            .filter(|(n, _)| n == "click")
            .skip(1)
            .map(|(_, id)| *id)
            .collect();
        assert!(cards.len() >= 9, "the controls were found: {}", cards.len());
        let focus_before = FOCUS_LOG.with(|l| l.borrow().len());
        for id in cards {
            click(&vdom, id);
        }
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(checked_cards(&page), 3, "no checkbox flipped: {page}");
        assert_eq!(
            FOCUS_LOG.with(|l| l.borrow().len()),
            focus_before,
            "and nothing took the focus out of the reason field"
        );
        assert!(!page.contains("file line"), "no viewer opened");
    }

    // ── L5: the mouse buttons raise the prompt, they do not decide ──────────────────────

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_mouse_buttons_that_need_a_reason_raise_the_prompt_and_confirm_decides() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        open_from_bar(&mut vdom, &mut seen).await;
        for (id, label) in [
            (STACK_FP_ID, "False positive"),
            (STACK_IGNORE_ID, "Ignored"),
        ] {
            click(&vdom, seen.element(id));
            settle(&mut vdom, &mut seen).await;
            let page = html(&vdom);
            assert_eq!(
                page.matches("queue-reason-prompt").count(),
                1,
                "{id}: {page}"
            );
            assert!(page.contains(label) && page.contains("3 findings (required)"));
            assert!(
                probe().dispositions.peek().is_empty(),
                "{id}: raising the prompt decided nothing"
            );
            // Leave the prompt for the next button: Esc in the field cancels it.
            let field = seen
                .listeners
                .iter()
                .rev()
                .find(|(n, _)| n == "keydown")
                .unwrap()
                .1;
            key(&vdom, field, "Escape");
            settle(&mut vdom, &mut seen).await;
            assert!(!html(&vdom).contains("queue-reason-prompt"));
        }
        click(&vdom, seen.element(STACK_CONFIRM_ID));
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            state_count(TriageState::Confirmed),
            3,
            "Confirm needs no reason"
        );
    }

    // ── M1: a decision made elsewhere ───────────────────────────────────────────────────

    /// Check a card on purpose, decide it in the viewer, come back: it is decided and unchecked,
    /// and Confirm checked does not touch it again.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_card_decided_in_the_viewer_comes_back_decided_and_unchecked() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        // Uncheck then check the first card: two explicit choices made while it is Unresolved.
        key(&vdom, panel, " ");
        key(&vdom, panel, " ");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(checked_cards(&html(&vdom)), 3);

        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("file line"), "the viewer opened");
        // Confirm the finding in the viewer.
        key(&vdom, seen.element(VIEWER_ID), "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 1);
        key(&vdom, seen.element(VIEWER_ID), "Escape");
        settle(&mut vdom, &mut seen).await;

        let page = html(&vdom);
        assert!(page.contains("sk-panel") && !page.contains("file line"));
        assert_eq!(
            page.matches("sk-status\" title=\"\">Confirmed<").count(),
            1,
            "{page}"
        );
        assert_eq!(
            checked_cards(&page),
            2,
            "the decided card is unchecked: its old choice was about the Unresolved card"
        );
        // And Confirm checked acts on the two that are checked, not on the decided one.
        key(&vdom, panel, "c");
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("Confirmed 2 findings."));
        assert_eq!(state_count(TriageState::Confirmed), 3);
    }

    // ── L3: a server that never answered ────────────────────────────────────────────────

    /// A request nobody answered is not "could not be read at the inspected commit", and the
    /// page is not counted as asked: coming back to it asks again.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_unanswered_request_says_so_and_the_page_is_asked_again_on_return() {
        let (mut vdom, mut seen, server, _env) =
            mount(deviations(212), OrderPreset::Curated, "garbage").await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("did not answer"), "{page}");
        assert!(
            !page.contains("could not be read at the inspected commit"),
            "{page}"
        );
        assert!(page.contains("sk-snippet"), "the stored lines are there");
        assert_eq!(batches(&server).await.len(), 1);

        key(&vdom, panel, ">");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(batches(&server).await.len(), 2, "page two is asked for");
        key(&vdom, panel, "<");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            batches(&server).await.len(),
            3,
            "page one was never answered, so it is asked again"
        );
    }

    // ── L5: the whole chain ─────────────────────────────────────────────────────────────

    /// pane `B` -> stack -> `Enter` -> viewer -> `Esc` -> stack -> `Esc` -> pane, in one run, with
    /// the keyboard going where each step says.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_whole_chain_pane_stack_viewer_stack_pane_returns_the_keyboard_each_step() {
        let rows = vec![defect(7), defect(8)];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        send(
            &vdom,
            "focusin",
            pane,
            Box::new(SerializedFocusData::default()),
        );
        settle(&mut vdom, &mut seen).await;

        let before = seen.mark();
        key(&vdom, pane, "b");
        settle(&mut vdom, &mut seen).await;
        let panel = seen.first_after(before, "keydown");
        assert!(html(&vdom).contains("sk-panel"));
        key(&vdom, panel, "j");

        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(
            html(&vdom).contains("file line 8"),
            "the viewer is on the second card"
        );
        key(&vdom, seen.element(VIEWER_ID), "Escape");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(last_focus(), Some(STACK_ID.to_string()), "viewer to stack");
        assert!(html(&vdom).contains("sk-panel"));

        key(&vdom, panel, "Escape");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("sk-panel"));
        assert_eq!(
            last_focus(),
            Some(super::super::scan::DETAIL_PANE_ID.to_string()),
            "stack to pane"
        );
    }

    // ── The view, props only: the disabled state ────────────────────────────────────────

    #[test]
    fn under_the_prompt_the_acting_controls_render_disabled_and_otherwise_do_not() {
        let rows = deviations(2);
        let prompt = ToolbarReasonPrompt {
            state: TriageState::FalsePositive,
            rows,
        };
        let under = render_view(vec![card(10, true, None)], Some(prompt), None, None, true);
        // Four decision buttons, two page buttons and the card's Code button.
        assert_eq!(under.matches("disabled=true").count(), 7, "{under}");
        let idle = render_view(vec![card(10, true, None)], None, None, None, true);
        assert!(!idle.contains("disabled=true"), "{idle}");
    }

    // ── Every Queue decision button is `apply_rows` ─────────────────────────────────────

    fn change(vdom: &VirtualDom, id: ElementId) {
        send(
            vdom,
            "change",
            id,
            Box::new(SerializedFormData::new("true".to_string(), Vec::new())),
        );
    }

    /// Tick the selection checkbox of each of `rows` (of `total`), then press the toolbar button
    /// that moves them to `target`.
    async fn select_and_press(
        vdom: &mut VirtualDom,
        seen: &mut Seen,
        rows: &[usize],
        target: TriageState,
    ) {
        for r in rows {
            change(vdom, seen.row_box(*r));
        }
        settle(vdom, seen).await;
        click(
            vdom,
            seen.element(&super::super::scan::toolbar_button_id(target)),
        );
        settle(vdom, seen).await;
    }

    fn table_rows(vdom: &VirtualDom) -> usize {
        html(vdom).matches("data-chorale-index=\"").count()
    }

    fn preloaded(rows: &[FindingView], state: TriageState) -> HashMap<String, Disposition> {
        rows.iter()
            .map(|f| {
                (
                    finding_key(f),
                    Disposition {
                        state,
                        reason: if state.requires_reason() {
                            "earlier".into()
                        } else {
                            String::new()
                        },
                        bucket: TechDebtBucket::Later,
                    },
                )
            })
            .collect()
    }

    /// "Confirm" and "Save as tech debt" write through `apply_rows`: the pane's undo names the
    /// count, one click on it puts the rows back in the map AND in the table.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn confirm_and_tech_debt_buttons_go_through_apply_rows_with_an_undo() {
        for (target, expect, said) in [
            (
                TriageState::Confirmed,
                TriageState::Confirmed,
                "Confirmed 2 findings.",
            ),
            (
                TriageState::TechDebt,
                TriageState::TechDebt,
                "Marked 2 findings as Tech debt.",
            ),
        ] {
            let rows: Vec<FindingView> = (1..=3).map(defect).collect();
            let (mut vdom, mut seen, _server, _env) =
                mount(rows, OrderPreset::CriticalFirst, "ok").await;
            assert_eq!(table_rows(&vdom), 3);
            select_and_press(&mut vdom, &mut seen, &[0, 1], target).await;
            assert_eq!(state_count(expect), 2, "{target:?}");
            assert!(html(&vdom).contains(said), "{target:?}: {}", html(&vdom));
            assert_eq!(table_rows(&vdom), 1, "the two rows left the table");

            click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
            settle(&mut vdom, &mut seen).await;
            assert!(probe().dispositions.peek().is_empty(), "{target:?}: undone");
            assert_eq!(table_rows(&vdom), 3, "{target:?}: and back on screen");
        }
    }

    /// "Ignore with reason" and "Mark false positive": the button raises the prompt (nothing is
    /// written), a blank reason is refused in the one wording and the prompt stays, a reason
    /// decides the selected rows with an undo.
    async fn prompted_toolbar_case(target: TriageState, said: &str) {
        let rows: Vec<FindingView> = (1..=3).map(defect).collect();
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let mark = seen.mark();
        select_and_press(&mut vdom, &mut seen, &[0, 1], target).await;
        let page = html(&vdom);
        assert!(page.contains("2 findings (required)"), "{target:?}: {page}");
        assert!(
            probe().dispositions.peek().is_empty(),
            "the button wrote nothing"
        );

        let field = seen.first_after(mark, "keydown");
        key(&vdom, field, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(
            probe().dispositions.peek().is_empty(),
            "a blank reason is refused"
        );
        assert!(
            html(&vdom).contains("queue-reason-prompt"),
            "and the prompt stays"
        );
        assert!(probe().toasts.peek().iter().any(|t| t
            .message
            .contains("A reason is required to move 2 findings")));

        typed(&vdom, seen.first_after(mark, "input"), "a real reason");
        settle(&mut vdom, &mut seen).await;
        key(&vdom, field, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(target), 2);
        assert!(probe()
            .dispositions
            .peek()
            .values()
            .all(|d| d.reason == "a real reason"));
        assert!(html(&vdom).contains(said), "{target:?}: {}", html(&vdom));
        click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(probe().dispositions.peek().is_empty(), "{target:?}: undone");
        assert_eq!(table_rows(&vdom), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn ignore_with_reason_goes_through_apply_rows() {
        prompted_toolbar_case(TriageState::Ignored, "Marked 2 findings as Ignored.").await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn mark_false_positive_goes_through_apply_rows() {
        prompted_toolbar_case(
            TriageState::FalsePositive,
            "Marked 2 findings as False positive.",
        )
        .await;
    }

    /// The "Move to ..." buttons of the other four tables, each from a table that holds three
    /// rows already in that state. A target that needs a reason (Ignored) raises the prompt.
    async fn move_case(from: TriageState, to: TriageState, said: &str, reason: Option<&str>) {
        let rows: Vec<FindingView> = (1..=3).map(defect).collect();
        let initial = preloaded(&rows, from);
        let (mut vdom, mut seen, _server, _env) =
            mount_in(rows, OrderPreset::CriticalFirst, "ok", from, initial).await;
        assert_eq!(table_rows(&vdom), 3, "{from:?}: the table holds its rows");
        let mark = seen.mark();
        select_and_press(&mut vdom, &mut seen, &[0, 1], to).await;
        if let Some(reason) = reason {
            assert_eq!(
                state_count(from),
                3,
                "{from:?} -> {to:?}: the button only asked"
            );
            let field = seen.first_after(mark, "keydown");
            key(&vdom, field, "Enter");
            settle(&mut vdom, &mut seen).await;
            assert_eq!(state_count(from), 3, "a blank reason is refused");
            typed(&vdom, seen.first_after(mark, "input"), reason);
            settle(&mut vdom, &mut seen).await;
            key(&vdom, field, "Enter");
            settle(&mut vdom, &mut seen).await;
        }
        assert_eq!(state_count(to), 2, "{from:?} -> {to:?}");
        assert_eq!(state_count(from), 1);
        assert!(
            html(&vdom).contains(said),
            "{from:?} -> {to:?}: {}",
            html(&vdom)
        );
        assert_eq!(table_rows(&vdom), 1);
        click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            state_count(from),
            3,
            "{from:?} -> {to:?}: undo restores the earlier state"
        );
        assert_eq!(table_rows(&vdom), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_move_to_buttons_go_through_apply_rows() {
        move_case(
            TriageState::Confirmed,
            TriageState::Unresolved,
            "Moved 2 findings back to Unresolved.",
            None,
        )
        .await;
        move_case(
            TriageState::Ignored,
            TriageState::TechDebt,
            "Marked 2 findings as Tech debt.",
            None,
        )
        .await;
        move_case(
            TriageState::FalsePositive,
            TriageState::Unresolved,
            "Moved 2 findings back to Unresolved.",
            None,
        )
        .await;
        move_case(
            TriageState::TechDebt,
            TriageState::Unresolved,
            "Moved 2 findings back to Unresolved.",
            None,
        )
        .await;
        // Tech debt -> Ignored needs a reason now; it used to move the rows with none.
        move_case(
            TriageState::TechDebt,
            TriageState::Ignored,
            "Marked 2 findings as Ignored.",
            Some("accepted risk"),
        )
        .await;
    }

    // ── The pane's prompt is bound to the row it was opened for ─────────────────────────

    /// Open the I prompt on row A, click row B, type a reason, Enter: only A is decided. The
    /// click is refused (as the stack's mouse doors are under its prompt), so the reason typed
    /// for A cannot land on B.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_row_click_under_the_panes_prompt_does_not_retarget_the_reason() {
        let rows = vec![defect(7), defect(8)];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        send(
            &vdom,
            "focusin",
            pane,
            Box::new(SerializedFocusData::default()),
        );
        settle(&mut vdom, &mut seen).await;

        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        assert!(
            html(&vdom).contains("pane-reason-prompt"),
            "the prompt is open"
        );

        // Row B (index 1) is clicked while the prompt is open.
        click(&vdom, seen.row_cell(1));
        settle(&mut vdom, &mut seen).await;
        assert!(
            probe()
                .toasts
                .peek()
                .iter()
                .any(|t| t.message.contains("Finish or cancel the reason first")),
            "the click is refused out loud"
        );
        assert!(
            html(&vdom).contains("pane-reason-prompt"),
            "and the prompt is still open"
        );

        typed(
            &vdom,
            seen.first_after(mark, "input"),
            "test fixture, not shipped",
        );
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;

        let decided = probe().dispositions.peek().clone();
        assert_eq!(decided.len(), 1, "exactly one row was decided");
        assert_eq!(
            decided
                .get(&finding_key(&defect(7)))
                .map(|d| (d.state, d.reason.clone())),
            Some((
                TriageState::Ignored,
                "test fixture, not shipped".to_string()
            )),
            "row A, with the reason typed for it"
        );
        assert!(
            !decided.contains_key(&finding_key(&defect(8))),
            "row B is untouched"
        );
    }

    /// Once the prompt is gone a row click moves the cursor again.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn after_the_prompt_closes_a_row_click_moves_the_cursor() {
        let rows = vec![defect(7), defect(8)];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        send(
            &vdom,
            "focusin",
            pane,
            Box::new(SerializedFocusData::default()),
        );
        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Escape");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("pane-reason-prompt"));
        click(&vdom, seen.row_cell(1));
        settle(&mut vdom, &mut seen).await;
        assert!(
            html(&vdom).contains("2 of 2"),
            "the pane is on the second row: {}",
            html(&vdom)
        );
    }
}
