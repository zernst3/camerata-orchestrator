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

use camerata_ui_core::decision_memory::shared_prefill;
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

/// The DOM id of the stacked view's reason field (the Queue's has its own).
const STACK_REASON_ID: &str = "camerata-stack-reason";

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
    /// Every Queue row, for decision memory (B.5): the stack's reason prompt looks for what the
    /// draft already decided about the checked cards' shapes among these.
    pub all_findings: Arc<Vec<FindingView>>,
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
            && Arc::ptr_eq(&self.all_findings, &other.all_findings)
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
        div {
            class: "rule-modal-overlay cv-overlay sk-overlay",
            // A review surface: keys and clicks here are the reviewer's, and count for the timer.
            "data-review-surface": "true",
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
                    // The review timer's control (internal), the same component the strip draws.
                    super::review::OverlayTimerControl {
                        id: "camerata-review-timer-toggle-stack",
                        return_focus: STACK_ID,
                        focus_back: prompt.is_none(),
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
                    QueueReasonPrompt {
                        prompt: p,
                        reason,
                        on_reason,
                        on_commit,
                        on_cancel,
                        input_id: STACK_REASON_ID.to_string(),
                    }
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
    // A signal only so the closures below stay `Copy` (an `Arc` captured by value would not be).
    let all_findings = use_signal(|| link.all_findings.clone());
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
        //
        // Decision memory (B.5): the field opens holding the reason this draft already gave for
        // these cards' shapes, but only when EVERY checked card is offered the same one; any
        // card without a prior, or with a different one, leaves it empty. It is a starting text:
        // nothing is decided until the reviewer presses Enter, and what is stored is what the
        // field holds then.
        reason.set(shared_prefill(
            &rows,
            &all_findings.peek(),
            &dispositions_sig.peek(),
            decided,
        ));
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
    use camerata_ui_core::triage::{TechDebtBucket, TriageModel};
    use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
    use dioxus::html::{
        Code, Key, Location, Modifiers, SerializedFocusData, SerializedFormData,
        SerializedKeyboardData, SerializedMouseData,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::super::palette::{
        option_id, route_chord, PaletteRoot, PaletteSlot, PALETTE_BUTTON_ID, PALETTE_INPUT_ID,
        RESTORE_LOG_MARK,
    };
    use super::super::scan::FindingsTable;
    use super::super::scan::{RuleHubRequest, PANE_REASON_ID};
    use super::super::viewer::ViewerHubOpen;
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
        /// The finding the full-detail dialog is open on (the results root hosts the dialog).
        detail: Signal<Option<FindingView>>,
        /// The Rule Hub request the results root would be showing.
        hub: Signal<Option<RuleHubRequest>>,
        /// Where the Queue registers the palette (the review bridge routes the chord through it).
        palette: PaletteSlot,
    }

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
        static ROWS: RefCell<Vec<FindingView>> = const { RefCell::new(Vec::new()) };
        static PRESET: std::cell::Cell<OrderPreset> =
            const { std::cell::Cell::new(OrderPreset::Curated) };
        static VIEW: std::cell::Cell<TriageState> =
            const { std::cell::Cell::new(TriageState::Unresolved) };
        static INITIAL: RefCell<HashMap<String, Disposition>> = RefCell::new(HashMap::new());
        /// The review timer the harness mounts the table with (step 7a).
        static REVIEW: RefCell<Option<super::super::review::ReviewHandle>> =
            const { RefCell::new(None) };
        /// How many times the strip's Review time button's callback was called (the command
        /// palette's stats entry calls the same one).
        static STATS_OPENED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        /// The Rule Hub requests the Queue raised (`on_open_hub`), as `(repo, rule id)`.
        static HUB_REQUESTS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    }

    fn review() -> super::super::review::ReviewHandle {
        REVIEW.with(|r| r.borrow().clone().expect("the harness stored its timer"))
    }

    fn app() -> Element {
        let review = super::super::review::use_review_handle();
        REVIEW.with(|r| *r.borrow_mut() = Some(review.clone()));
        use_context_provider(|| review.clone());
        let toasts = use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        let detail = use_context_provider(|| Signal::new(None::<FindingView>));
        use_context_provider(|| Signal::new(ViewerCache::new()));
        use_context_provider(|| ViewerSlot(Signal::new(None)));
        use_context_provider(|| StackSlot(Signal::new(None)));
        let palette = use_context_provider(|| PaletteSlot(Signal::new(None)));
        let hub = Signal::new(None::<RuleHubRequest>);
        use_context_provider(|| ViewerHubOpen(hub));
        let dispositions = use_signal(|| INITIAL.with(|i| i.borrow().clone()));
        let order_preset = use_signal(|| PRESET.with(std::cell::Cell::get));
        PROBE.with(|p| {
            *p.borrow_mut() = Some(Probe {
                dispositions,
                toasts,
                detail,
                hub,
                palette,
            })
        });
        let findings = ROWS.with(|r| r.borrow().clone());
        let mut shas = HashMap::new();
        shas.insert("me/api".to_string(), SHA.to_string());
        // What the results view hands the table: the tally over every Queue row.
        let counts = TriageModel {
            dispositions: dispositions.read().clone(),
            triage_view: VIEW.with(std::cell::Cell::get),
        }
        .counts(&findings);
        rsx! {
            FindingsTable {
                counts: Some(counts),
                findings,
                repos: vec!["me/api".to_string()],
                descriptions: HashMap::new(),
                dispositions,
                order_preset,
                triage_view: VIEW.with(std::cell::Cell::get),
                audited_shas: shas,
                review: Some(review),
                on_open_stats: Callback::new(|()| STATS_OPENED.with(|n| n.set(n.get() + 1))),
                on_open_hub: Callback::new(|req: RuleHubRequest| {
                    HUB_REQUESTS.with(|r| r.borrow_mut().push((req.repo, req.rule_id)));
                }),
            }
            StackRoot {}
            ViewerRoot {}
            PaletteRoot {}
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
            "ArrowUp" => Key::ArrowUp,
            "Tab" => Key::Tab,
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
        // controls), in the order they were created. The first two are the review timer's
        // control and Close, both allowed under the prompt and left alone.
        let cards: Vec<ElementId> = seen.listeners[opened..after_open]
            .iter()
            .filter(|(n, _)| n == "click")
            .skip(2)
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

    // ── Decision memory (B.5) and the progress strip (B.7), mounted ──────────────────────

    const CHIP: &str = "Previously on this shape";

    /// A defect on `line` whose offending snippet is `snippet`, under one rule.
    fn shaped(line: usize, snippet: &str) -> FindingView {
        let mut f = defect(line);
        f.snippet = snippet.to_string();
        f
    }

    /// The same, as a deviation (so a bulk-bar group and a stack exist for it).
    fn shaped_deviation(line: usize, snippet: &str) -> FindingView {
        let mut f = deviation(line);
        f.snippet = snippet.to_string();
        f
    }

    fn seeded(
        rows: &[&FindingView],
        state: TriageState,
        reason: &str,
    ) -> HashMap<String, Disposition> {
        rows.iter()
            .map(|f| {
                (
                    finding_key(f),
                    Disposition {
                        state,
                        reason: reason.into(),
                        bucket: TechDebtBucket::Later,
                    },
                )
            })
            .collect()
    }

    /// The page with the HTML entities SSR writes for `'`, `<`, `>` and `&` undone, so a chip
    /// reads as written.
    fn plain(page: &str) -> String {
        page.replace("&#39;", "'")
            .replace("&#60;", "<")
            .replace("&#62;", ">")
            .replace("&#38;", "&")
    }

    /// What the reason field holds, read off the page: the `value` of the textarea whose class
    /// is `class`.
    fn field_text(page: &str, class: &str) -> String {
        let at = page
            .find(&format!("{class}\""))
            .unwrap_or_else(|| panic!("no {class} in the page"));
        let rest = &page[at..];
        let tag_end = rest.find('>').expect("the tag closes");
        let tag = &rest[..tag_end];
        let Some(v) = tag.find("value=\"") else {
            return String::new();
        };
        let v = v + "value=\"".len();
        plain(&tag[v..v + tag[v..].find('"').expect("the value closes")])
    }

    async fn arm_pane(vdom: &mut VirtualDom, seen: &mut Seen) -> ElementId {
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        send(
            vdom,
            "focusin",
            pane,
            Box::new(SerializedFocusData::default()),
        );
        settle(vdom, seen).await;
        pane
    }

    /// B.5 check 1: two rows with the same rule and fingerprint. Deciding the first puts the
    /// chip on the second, whose own disposition stays Unresolved until a key is pressed; `I`
    /// pre-fills the reason; editing it before Enter stores the edited text; a row with a
    /// different fingerprint has no chip.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_decided_row_shows_the_chip_on_its_twin_and_i_prefills_but_never_decides() {
        let rows = vec![
            shaped(7, "same line"),
            shaped(8, "same line"),
            shaped(9, "a different line"),
        ];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows.clone(), OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains(CHIP), "nothing is decided yet");

        // Decide the first row: I, a reason, Enter.
        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            field_text(&html(&vdom), "pane-reason-input"),
            "",
            "no memory yet, so the field starts empty"
        );
        typed(
            &vdom,
            seen.first_after(mark, "input"),
            "test fixture, not shipped",
        );
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Ignored), 1);

        // The pane is now on the twin (line 8): the chip is there, and the twin is NOT decided.
        let page = plain(&html(&vdom));
        assert!(
            page.contains("Previously on this shape: Ignored, 'test fixture, not shipped' (1 row)"),
            "{page}"
        );
        assert!(page.contains("1 of 2"), "{page}");
        assert!(
            !probe()
                .dispositions
                .peek()
                .contains_key(&finding_key(&rows[1])),
            "the chip decided nothing"
        );
        assert_eq!(probe().dispositions.peek().len(), 1);

        // I opens the prompt with the remembered reason in it. Still nothing decided.
        let pane = arm_pane(&mut vdom, &mut seen).await;
        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(
            field_text(&page, "pane-reason-input"),
            "test fixture, not shipped",
            "{page}"
        );
        assert_eq!(
            probe().dispositions.peek().len(),
            1,
            "pre-filled, not applied"
        );

        // Edit before Enter: the edited text is what is stored.
        typed(
            &vdom,
            seen.first_after(mark, "input"),
            "test fixture, not shipped in the demo either",
        );
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        let stored = probe()
            .dispositions
            .peek()
            .get(&finding_key(&rows[1]))
            .cloned();
        assert_eq!(
            stored.map(|d| (d.state, d.reason)),
            Some((
                TriageState::Ignored,
                "test fixture, not shipped in the demo either".to_string()
            ))
        );

        // The last row has another fingerprint: no chip, though two rows of its rule are decided.
        let page = html(&vdom);
        assert!(page.contains("1 of 1"), "{page}");
        assert!(
            !page.contains(CHIP),
            "a different fingerprint has no chip: {page}"
        );
    }

    /// Enter on a pre-filled prompt with no edit stores the remembered text as the reviewer's own
    /// act, and Esc stores nothing.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_prefilled_prompt_decides_only_on_enter_and_escape_decides_nothing() {
        let a = shaped(7, "same line");
        let b = shaped(8, "same line");
        let initial = seeded(&[&a], TriageState::FalsePositive, "parser misread");
        let (mut vdom, mut seen, _server, _env) = mount_in(
            vec![a.clone(), b.clone()],
            OrderPreset::CriticalFirst,
            "ok",
            TriageState::Unresolved,
            initial,
        )
        .await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        assert!(plain(&html(&vdom))
            .contains("Previously on this shape: False positive, 'parser misread' (1 row)"));

        // F with the memory: the field holds it. Esc: nothing decided, nothing stored.
        let mark = seen.mark();
        key(&vdom, pane, "f");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            field_text(&html(&vdom), "pane-reason-input"),
            "parser misread"
        );
        key(&vdom, seen.first_after(mark, "keydown"), "Escape");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(probe().dispositions.peek().len(), 1, "Esc decided nothing");

        // I on a shape that was only ever marked false positive: the chip still says so, but a
        // "tool was wrong" reason is not offered as an "accepted risk" one, so the field is empty.
        let pane = arm_pane(&mut vdom, &mut seen).await;
        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(field_text(&html(&vdom), "pane-reason-input"), "");
        assert!(plain(&html(&vdom)).contains("Previously on this shape: False positive"));
        key(&vdom, seen.first_after(mark, "keydown"), "Escape");
        settle(&mut vdom, &mut seen).await;

        // F again, and Enter with no edit: the reviewer's Enter stores exactly the remembered text.
        let pane = arm_pane(&mut vdom, &mut seen).await;
        let mark = seen.mark();
        key(&vdom, pane, "f");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            field_text(&html(&vdom), "pane-reason-input"),
            "parser misread"
        );
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        let stored = probe().dispositions.peek().get(&finding_key(&b)).cloned();
        assert_eq!(
            stored.map(|d| (d.state, d.reason)),
            Some((TriageState::FalsePositive, "parser misread".to_string()))
        );
    }

    /// The chip never appears for a shape that was only Confirmed or saved as Tech debt.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn confirmed_and_tech_debt_twins_show_no_chip() {
        for state in [TriageState::Confirmed, TriageState::TechDebt] {
            let a = shaped(7, "same line");
            let b = shaped(8, "same line");
            let initial = seeded(&[&a], state, "");
            let (mut vdom, mut seen, _server, _env) = mount_in(
                vec![a, b],
                OrderPreset::CriticalFirst,
                "ok",
                TriageState::Unresolved,
                initial,
            )
            .await;
            let pane = arm_pane(&mut vdom, &mut seen).await;
            assert!(
                !html(&vdom).contains(CHIP),
                "{state:?} gives no chip: {}",
                html(&vdom)
            );
            let mark = seen.mark();
            key(&vdom, pane, "i");
            settle(&mut vdom, &mut seen).await;
            assert_eq!(field_text(&html(&vdom), "pane-reason-input"), "");
            key(&vdom, seen.first_after(mark, "keydown"), "Escape");
            settle(&mut vdom, &mut seen).await;
        }
    }

    /// An undo removes the memory with the decision it came from.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_undo_takes_the_chip_back_off_the_twin() {
        let rows = vec![shaped(7, "same line"), shaped(8, "same line")];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        typed(&vdom, seen.first_after(mark, "input"), "fixture");
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains(CHIP), "the twin shows it");

        click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(
            probe().dispositions.peek().is_empty(),
            "the decision is undone"
        );
        // Both rows are back, the cursor on the first; walk to the twin: no chip.
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "ArrowDown");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("2 of 2"), "{page}");
        assert!(
            !page.contains(CHIP),
            "the undone decision is not remembered: {page}"
        );
    }

    /// The chip's reason is text, not markup.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_chip_renders_the_reason_as_text() {
        let a = shaped(7, "same line");
        let b = shaped(8, "same line");
        let initial = seeded(&[&a], TriageState::Ignored, "<b>bold</b> & more");
        let (mut vdom, mut seen, _server, _env) = mount_in(
            vec![a, b],
            OrderPreset::CriticalFirst,
            "ok",
            TriageState::Unresolved,
            initial,
        )
        .await;
        let _ = arm_pane(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("&#60;b&#62;bold&#60;/b&#62; &#38; more"),
            "escaped: {page}"
        );
        assert!(!page.contains("<b>bold</b>"), "{page}");
    }

    /// The viewer's `I` starts its prompt with the remembered reason too, and decides nothing.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_viewers_prompt_starts_with_the_remembered_reason() {
        let a = shaped(7, "same line");
        let b = shaped(8, "same line");
        let initial = seeded(&[&a], TriageState::Ignored, "test fixture");
        let (mut vdom, mut seen, _server, _env) = mount_in(
            vec![a, b],
            OrderPreset::CriticalFirst,
            "ok",
            TriageState::Unresolved,
            initial,
        )
        .await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "o");
        settle(&mut vdom, &mut seen).await;
        let viewer = seen.element(VIEWER_ID);
        let mark = seen.mark();
        key(&vdom, viewer, "i");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(
            field_text(&page, "pane-reason-input"),
            "test fixture",
            "{page}"
        );
        assert_eq!(probe().dispositions.peek().len(), 1, "nothing was applied");
        key(&vdom, seen.first_after(mark, "keydown"), "Escape");
        settle(&mut vdom, &mut seen).await;
    }

    /// The stack's prompt pre-fills only when every checked card is offered the same reason.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_stack_prompt_prefills_only_when_every_checked_card_shares_the_reason() {
        // Two rows already ignored on the shape "alpha"; three undecided in the stack: two on
        // "alpha" and one on a shape nobody has decided.
        let p1 = shaped_deviation(1, "alpha");
        let p2 = shaped_deviation(2, "alpha");
        let rows = vec![
            p1.clone(),
            p2.clone(),
            shaped_deviation(3, "alpha"),
            shaped_deviation(4, "alpha"),
            shaped_deviation(5, "beta"),
        ];
        let initial = seeded(&[&p1, &p2], TriageState::Ignored, "test fixture");
        let (mut vdom, mut seen, _server, _env) = mount_in(
            rows,
            OrderPreset::Curated,
            "ok",
            TriageState::Unresolved,
            initial,
        )
        .await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        assert_eq!(checked_cards(&html(&vdom)), 3);

        // All three checked, one of them with no prior: the field starts empty.
        let mark = seen.mark();
        key(&vdom, panel, "i");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("3 findings (required)"), "{page}");
        assert_eq!(field_text(&page, "queue-reason-input"), "", "{page}");
        key(&vdom, seen.first_after(mark, "keydown"), "Escape");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("queue-reason-prompt"));

        // Uncheck the "beta" card (the third): the two left share the prior reason.
        key(&vdom, panel, "j");
        key(&vdom, panel, "j");
        key(&vdom, panel, " ");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(checked_cards(&html(&vdom)), 2);
        let mark = seen.mark();
        key(&vdom, panel, "i");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("2 findings (required)"), "{page}");
        assert_eq!(
            field_text(&page, "queue-reason-input"),
            "test fixture",
            "{page}"
        );
        assert_eq!(
            state_count(TriageState::Ignored),
            2,
            "pre-filled, not applied"
        );

        // Enter is the reviewer's: both cards get the text the field holds.
        key(&vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Ignored), 4);
    }

    /// Two checked cards whose priors differ get an empty field, not a guess.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_stack_prompt_stays_empty_when_the_priors_differ() {
        let pa = shaped_deviation(1, "alpha");
        let pb = shaped_deviation(2, "beta");
        let rows = vec![
            pa.clone(),
            pb.clone(),
            shaped_deviation(3, "alpha"),
            shaped_deviation(4, "beta"),
        ];
        let mut initial = seeded(&[&pa], TriageState::Ignored, "test fixture");
        initial.extend(seeded(&[&pb], TriageState::Ignored, "legacy path"));
        let (mut vdom, mut seen, _server, _env) = mount_in(
            rows,
            OrderPreset::Curated,
            "ok",
            TriageState::Unresolved,
            initial,
        )
        .await;
        let panel = open_from_bar(&mut vdom, &mut seen).await;
        key(&vdom, panel, "f");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("2 findings (required)"), "{page}");
        assert_eq!(field_text(&page, "queue-reason-input"), "", "{page}");
    }

    // ── The review timer (step 7a, INTERNAL): it observes, it never decides ──────────────

    /// A decision made through `apply_rows` (the pane's key) is recorded as reviewed by the
    /// operator, in a group the cursor opened; the timer wrote no disposition of its own (the
    /// map holds exactly the one the key wrote); and the undo takes the reviewed record away.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_pane_decision_is_recorded_as_reviewed_and_an_undo_unrecords_it() {
        let rows: Vec<FindingView> = (1..=3).map(defect).collect();
        let (mut vdom, mut seen, _server, _env) =
            mount(rows.clone(), OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        // The cursor landed on a row: its rule group is open and the timer is running.
        let group = camerata_ui_core::review_timer::group_key("me/api", "SQL-1");
        assert!(
            review().log().review_groups.contains_key(&group),
            "opening a group is the cursor entering it: {:?}",
            review().log()
        );
        assert!(review().log().review_rows.is_empty(), "no decision yet");

        key(&vdom, pane, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 1, "one key, one write");
        let log = review().log();
        assert_eq!(log.review_rows.len(), 1);
        let (row_key, record) = log.review_rows.iter().next().unwrap();
        assert!(
            rows.iter().any(|f| &finding_key(f) == row_key),
            "the record is keyed by the decided finding"
        );
        assert_eq!(
            record.reviewed_by,
            camerata_ui_core::review_timer::LOCAL_OPERATOR
        );
        assert!(record.reviewed_at_ms > 0);

        click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(probe().dispositions.peek().is_empty());
        assert!(
            review().log().review_rows.is_empty(),
            "an undone row is not reviewed"
        );
    }

    /// Every toolbar door is `apply_rows`, so the batch is recorded too, split across its rows.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_toolbar_batch_is_recorded_row_by_row() {
        let rows: Vec<FindingView> = (1..=3).map(defect).collect();
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        select_and_press(&mut vdom, &mut seen, &[0, 1], TriageState::Confirmed).await;
        assert_eq!(state_count(TriageState::Confirmed), 2);
        assert_eq!(review().log().review_rows.len(), 2);
    }

    /// Opening a group's stacked view is opening the group.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn opening_the_stacked_view_opens_the_group_for_the_timer() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(4), OrderPreset::Curated, "ok").await;
        // Nothing is open yet: all groups start collapsed and the cursor has no visible row.
        assert!(review().log().review_groups.is_empty());
        open_from_bar(&mut vdom, &mut seen).await;
        let log = review().log();
        assert_eq!(log.review_groups.len(), 1, "{log:?}");
        assert!(log.review_groups.keys().next().unwrap().ends_with("API-1"));
    }

    /// The strip draws the timer's state and an always-present button; pressing it pauses, and the
    /// button then offers Resume. The pane gets the keyboard back.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_strip_button_pauses_and_resumes_and_hands_the_keys_back_to_the_pane() {
        let rows: Vec<FindingView> = (1..=2).map(defect).collect();
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        let _pane = arm_pane(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("Review timer running"), "{page}");
        assert!(page.contains("camerata-review-timer-toggle"), "{page}");
        assert!(page.contains("data-review-no-activity"), "{page}");

        let button = seen.element(super::super::review::REVIEW_TIMER_BUTTON_ID);
        click(&vdom, button);
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(page.contains("Review timer paused"), "{page}");
        assert!(!page.contains("Review timer running"), "{page}");
        assert_eq!(
            last_focus(),
            Some(super::super::scan::DETAIL_PANE_ID.to_string()),
            "the pane's keys stay armed"
        );
        // A decision while manually paused is still recorded, and does not resume the timer.
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        key(&vdom, pane, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(review().log().review_rows.len(), 1);
        assert!(html(&vdom).contains("Review timer paused"));

        click(&vdom, button);
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("Review timer running"));
    }

    /// The timer's control is always on screen: the stacked view and the code viewer cover the
    /// strip, so each draws the same control. Pressing any of them toggles the ONE timer (the
    /// strip's text moves with it), hands the keyboard back to the overlay it was pressed in, and
    /// takes only the Enter and Space that activate it: Esc still reaches the overlay.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_timer_control_is_in_the_stack_and_the_viewer_and_toggles_the_one_timer() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(5), OrderPreset::Curated, "ok").await;
        let page = html(&vdom);
        assert!(!page.contains("camerata-review-timer-toggle-stack"));
        let panel = open_from_bar(&mut vdom, &mut seen).await;

        // The stack draws it, as the one shared component (same label, same button).
        let page = html(&vdom);
        assert!(
            page.contains("id=\"camerata-review-timer-toggle-stack\""),
            "{page}"
        );
        assert_eq!(
            page.matches("data-review-no-activity").count(),
            2,
            "strip and stack"
        );
        assert_eq!(page.matches("Review timer running").count(), 2, "{page}");
        let stack_button = seen.element("camerata-review-timer-toggle-stack");
        click(&vdom, stack_button);
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(
            page.matches("Review timer paused").count(),
            2,
            "one timer: the strip behind it moved too: {page}"
        );
        assert_eq!(page.matches(">Resume<").count(), 2);
        assert_eq!(
            last_focus(),
            Some(STACK_ID.to_string()),
            "back to the stack"
        );
        click(&vdom, stack_button);
        settle(&mut vdom, &mut seen).await;
        assert_eq!(html(&vdom).matches("Review timer running").count(), 2);

        // Enter on the focused button does not reach the stack (where Enter opens the viewer);
        // Escape on it does (and closes the stack).
        key(&vdom, stack_button, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(
            !html(&vdom).contains("camerata-code-viewer"),
            "Enter on the button must not open the viewer under it"
        );

        // The viewer, opened on a card, draws it too.
        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("id=\"camerata-review-timer-toggle-viewer\""),
            "{page}"
        );
        assert_eq!(
            page.matches("data-review-no-activity").count(),
            3,
            "strip, stack, viewer"
        );
        let viewer_button = seen.element("camerata-review-timer-toggle-viewer");
        click(&vdom, viewer_button);
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert_eq!(page.matches("Review timer paused").count(), 3, "{page}");
        assert_eq!(
            last_focus(),
            Some(VIEWER_ID.to_string()),
            "back to the viewer"
        );
        // Esc on the focused button still closes the viewer.
        key(&vdom, viewer_button, "Escape");
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("camerata-code-viewer"));
        assert!(html(&vdom).contains("sk-panel"), "back on the stack");
    }

    /// Key and click activity counts for the timer only inside a review surface: the Queue table
    /// with its strip and pane, the viewer and the stack each carry the marker the bridge looks
    /// for, and the panels outside the Queue do not.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_review_surfaces_carry_the_marker_the_bridge_counts() {
        let rows: Vec<FindingView> = (1..=2).map(deviation).collect();
        let (mut vdom, mut seen, _server, _env) = mount(rows, OrderPreset::Curated, "ok").await;
        let page = html(&vdom);
        assert_eq!(
            page.matches("data-review-surface").count(),
            1,
            "the Queue: {page}"
        );
        let queue_at = page.find("data-review-surface").unwrap();
        assert!(
            page.find("progress-strip").unwrap() > queue_at
                && page.find("camerata-finding-pane").unwrap() > queue_at,
            "the strip and the pane are inside it"
        );
        open_from_bar(&mut vdom, &mut seen).await;
        assert_eq!(
            html(&vdom).matches("data-review-surface").count(),
            2,
            "plus the stack"
        );
        let panel = seen.element(STACK_ID);
        key(&vdom, panel, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(
            html(&vdom).matches("data-review-surface").count(),
            3,
            "plus the viewer"
        );
    }

    /// B.7: the strip is above the table, equals `TriageCounts` for every table, and moves with a
    /// decision and an undo.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_strip_equals_triage_counts_for_every_table_and_moves_with_decisions_and_undo() {
        use camerata_ui_core::progress::progress_text;
        let rows: Vec<FindingView> = (1..=6).map(|n| shaped(n, &format!("line {n}"))).collect();
        let mut initial = seeded(&[&rows[0]], TriageState::Confirmed, "");
        initial.extend(seeded(
            &[&rows[1], &rows[2]],
            TriageState::Ignored,
            "fixture",
        ));
        initial.extend(seeded(&[&rows[3]], TriageState::FalsePositive, "misread"));

        let expected = |dispositions: &HashMap<String, Disposition>, view: TriageState| {
            let model = TriageModel {
                dispositions: dispositions.clone(),
                triage_view: view,
            };
            progress_text(model.counts(&rows))
        };

        for view in TriageState::all() {
            let (vdom, _seen, _server, _env) = mount_in(
                rows.clone(),
                OrderPreset::CriticalFirst,
                "ok",
                view,
                initial.clone(),
            )
            .await;
            let page = html(&vdom);
            let want = expected(&initial, view);
            assert!(page.contains(&want), "{view:?}: wanted {want} in {page}");
            assert_eq!(
                want,
                "Undecided 2 of 6 \u{b7} Confirmed 1 \u{b7} Ignored 2 \u{b7} FP 1 \u{b7} Tech debt 0"
            );
            // Above the table, a labelled group of plain text; not a live region.
            let strip = page.find("progress-strip").expect("the strip");
            assert!(strip < page.find("findings-toolbar").unwrap());
            assert!(page.contains("aria-label=\"Review progress\""));
            assert!(
                !page.contains("aria-live"),
                "the strip does not announce itself"
            );
            // Fewer than 20 measured decisions: no time-left estimate is drawn.
            assert!(!page.contains("progress-eta"), "{page}");
        }

        // Live: decide the focused row, then undo.
        let (mut vdom, mut seen, _server, _env) =
            mount(rows.clone(), OrderPreset::CriticalFirst, "ok").await;
        let none = HashMap::new();
        assert!(html(&vdom).contains(&expected(&none, TriageState::Unresolved)));
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "c");
        settle(&mut vdom, &mut seen).await;
        let after = probe().dispositions.peek().clone();
        let page = html(&vdom);
        assert!(
            page.contains(&expected(&after, TriageState::Unresolved)),
            "{page}"
        );
        assert!(
            page.contains("Undecided 5 of 6 \u{b7} Confirmed 1"),
            "{page}"
        );
        click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            page.contains("Undecided 6 of 6 \u{b7} Confirmed 0"),
            "{page}"
        );
    }

    // ── Fix round: undo under an open prompt (M2) and the toolbar's pre-fill (L2) ─────────

    /// Decide the first row with `fixture`, so an undo is armed and the cursor sits on its twin.
    async fn decide_first_then_stand_on_the_twin(vdom: &mut VirtualDom, seen: &mut Seen) {
        let pane = arm_pane(vdom, seen).await;
        let mark = seen.mark();
        key(vdom, pane, "i");
        settle(vdom, seen).await;
        typed(vdom, seen.first_after(mark, "input"), "fixture");
        settle(vdom, seen).await;
        key(vdom, seen.first_after(mark, "keydown"), "Enter");
        settle(vdom, seen).await;
        assert_eq!(state_count(TriageState::Ignored), 1);
    }

    /// M2: an undo while the pane's prompt is open (pre-filled by memory) closes the prompt before
    /// the cursor moves to the restored row, so no Enter can write the reason onto it.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_undo_closes_the_panes_open_prompt_so_nothing_is_written_to_the_restored_row() {
        let rows = vec![
            shaped(7, "same line"),
            shaped(8, "same line"),
            shaped(9, "another line"),
        ];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows.clone(), OrderPreset::CriticalFirst, "ok").await;
        decide_first_then_stand_on_the_twin(&mut vdom, &mut seen).await;

        // On the twin, I opens the prompt holding the remembered reason.
        let pane = arm_pane(&mut vdom, &mut seen).await;
        let mark = seen.mark();
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(field_text(&html(&vdom), "pane-reason-input"), "fixture");
        let field_keys = seen.first_after(mark, "keydown");

        // Undo while it is open: the decision is reverted AND the prompt is gone.
        click(&vdom, seen.element(super::super::scan::PANE_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        let page = html(&vdom);
        assert!(
            !page.contains("pane-reason-input"),
            "the prompt closed: {page}"
        );
        assert!(page.contains("1 of 3"), "the cursor is on the restored row");
        // A late Enter at the old field reaches nothing.
        key(&vdom, field_keys, "Enter");
        settle(&mut vdom, &mut seen).await;
        assert!(
            probe().dispositions.peek().is_empty(),
            "nothing was written: {:?}",
            probe().dispositions.peek().keys().collect::<Vec<_>>()
        );
    }

    /// M2, viewer side: the viewer's own prompt closes on an undo too.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_undo_closes_the_viewers_open_prompt() {
        let rows = vec![shaped(7, "same line"), shaped(8, "same line")];
        let (mut vdom, mut seen, _server, _env) =
            mount(rows, OrderPreset::CriticalFirst, "ok").await;
        decide_first_then_stand_on_the_twin(&mut vdom, &mut seen).await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "o");
        settle(&mut vdom, &mut seen).await;
        key(&vdom, seen.element(VIEWER_ID), "i");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(field_text(&html(&vdom), "pane-reason-input"), "fixture");

        click(&vdom, seen.element(super::super::viewer::VIEWER_UNDO_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(
            !html(&vdom).contains("pane-reason-input"),
            "the prompt closed: {}",
            html(&vdom)
        );
        assert!(probe().dispositions.peek().is_empty(), "the undo landed");
    }

    /// L2: the toolbar's Ignore and False positive prompts start with a remembered reason by the
    /// stack's rule: only when every selected row is offered the same one.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_toolbar_prompt_prefills_by_the_stacks_rule() {
        let prior = shaped(1, "same line");
        let rows = vec![
            prior.clone(),
            shaped(8, "same line"),
            shaped(9, "same line"),
            shaped(10, "a shape nobody decided"),
        ];
        let initial = seeded(&[&prior], TriageState::Ignored, "test fixture");
        let (mut vdom, mut seen, _server, _env) = mount_in(
            rows,
            OrderPreset::CriticalFirst,
            "ok",
            TriageState::Unresolved,
            initial,
        )
        .await;
        // The two twins (table rows 0 and 1): both offered the reason.
        let mark = seen.mark();
        select_and_press(&mut vdom, &mut seen, &[0, 1], TriageState::Ignored).await;
        let page = html(&vdom);
        assert!(page.contains("2 findings (required)"), "{page}");
        assert_eq!(field_text(&page, "queue-reason-input"), "test fixture");
        assert_eq!(state_count(TriageState::Ignored), 1, "nothing applied yet");

        // Add the row with no prior: the prompt is raised again for all three and starts empty.
        key(&vdom, seen.first_after(mark, "keydown"), "Escape");
        settle(&mut vdom, &mut seen).await;
        select_and_press(&mut vdom, &mut seen, &[2], TriageState::Ignored).await;
        let page = html(&vdom);
        assert!(page.contains("3 findings (required)"), "{page}");
        assert_eq!(field_text(&page, "queue-reason-input"), "", "{page}");
    }

    // ── The command palette (work order B.8, step 7d) ───────────────────────────────────
    //
    // These live here because they drive the Queue through the harness above. The palette's own
    // rules are unit-tested in `camerata_ui_core::palette`; what is pinned here is that each
    // entry reaches the function its button or key reaches, and the gate, the focus and the
    // timer behaviour that only a mounted Queue can show.

    /// A Ctrl or Cmd chord as the review bridge reports it, routed the way the bridge routes it:
    /// `route_chord` through the palette slot the Queue registered. `dom_modal` is what the page
    /// says about dialogs at the press (any `.rule-modal-overlay`).
    async fn press_chord(
        vdom: &mut VirtualDom,
        seen: &mut Seen,
        pressed: &str,
        mods: KeyMods,
        dom_modal: bool,
    ) {
        let slot = probe().palette;
        vdom.in_runtime(|| route_chord(Some(slot), pressed, mods, dom_modal));
        settle(vdom, seen).await;
    }

    fn ctrl() -> KeyMods {
        KeyMods {
            ctrl: true,
            ..KeyMods::none()
        }
    }

    /// Ctrl+K, anywhere on the results screen (the focused element does not matter, so `_at` is
    /// only kept so the call sites read as before).
    async fn ctrl_k(vdom: &mut VirtualDom, seen: &mut Seen, _at: ElementId) {
        press_chord(vdom, seen, "k", ctrl(), false).await;
    }

    /// Whether the app prevented the default action of a keydown in the palette's field.
    fn default_prevented(vdom: &VirtualDom, id: ElementId, named: Key, shift: bool) -> bool {
        let mods = if shift {
            Modifiers::SHIFT
        } else {
            Modifiers::empty()
        };
        let data = SerializedKeyboardData::new(
            named,
            Code::Unidentified,
            Location::Standard,
            false,
            mods,
            false,
        );
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(data)));
        let event = dioxus::dioxus_core::Event::new(platform, true);
        vdom.runtime().handle_event("keydown", event.clone(), id);
        !event.default_action_enabled()
    }

    /// Type `text` into the palette's field.
    async fn type_in_palette(vdom: &mut VirtualDom, seen: &mut Seen, text: &str) {
        typed(vdom, seen.element(PALETTE_INPUT_ID), text);
        settle(vdom, seen).await;
    }

    /// Press `pressed` in the palette's field.
    async fn press_in_palette(vdom: &mut VirtualDom, seen: &mut Seen, pressed: &str) {
        key(vdom, seen.element(PALETTE_INPUT_ID), pressed);
        settle(vdom, seen).await;
    }

    /// Open the palette over the focused pane, type `query` and press Enter.
    async fn run_entry(vdom: &mut VirtualDom, seen: &mut Seen, pane: ElementId, query: &str) {
        ctrl_k(vdom, seen, pane).await;
        assert!(html(vdom).contains("pal-panel"), "the palette opened");
        type_in_palette(vdom, seen, query).await;
        press_in_palette(vdom, seen, "Enter").await;
    }

    fn toast_said(needle: &str) -> bool {
        probe()
            .toasts
            .peek()
            .iter()
            .any(|t| t.message.contains(needle))
    }

    /// The pane's "N of M" text.
    fn pane_position(page: &str) -> String {
        let at = page.find("pane-pos").expect("the pane's position");
        let tail = &page[at..];
        let from = tail.find('>').unwrap() + 1;
        let to = tail.find("</span>").unwrap();
        tail[from..to].to_string()
    }

    /// How many lines the open palette lists.
    fn palette_lines(page: &str) -> usize {
        page.matches("role=\"option\"").count()
    }

    fn dispositions_of(probe: Probe) -> Vec<(TriageState, String)> {
        let mut out: Vec<_> = probe
            .dispositions
            .peek()
            .values()
            .map(|d| (d.state, d.reason.clone()))
            .collect();
        out.sort_by_key(|(s, _)| s.label());
        out
    }

    /// Ctrl+K opens it with the review's entries listed, and the toolbar button and Cmd+K open
    /// it too.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn ctrl_k_and_the_toolbar_button_open_the_palette() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        assert!(!html(&vdom).contains("pal-panel"), "closed until asked for");
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        let page = html(&vdom);
        assert!(page.contains("pal-panel"), "{page}");
        assert!(page.contains("Confirm this finding"));
        assert!(page.contains("Mark this finding a false positive"));
        assert!(page.contains("Next finding"));
        assert!(page.contains("Open in the code viewer"));
        assert!(page.contains("Pause the review timer"));
        assert!(page.contains("Show the review time"));
        assert!(
            !page.contains("Undo the last decision"),
            "no undo is live, so no such line"
        );

        press_in_palette(&mut vdom, &mut seen, "Escape").await;
        assert!(!html(&vdom).contains("pal-panel"));
        click(&vdom, seen.element(PALETTE_BUTTON_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("pal-panel"), "the Commands button");
        press_in_palette(&mut vdom, &mut seen, "Escape").await;
        let cmd = KeyMods {
            meta: true,
            ..KeyMods::none()
        };
        press_chord(&mut vdom, &mut seen, "k", cmd, false).await;
        assert!(html(&vdom).contains("pal-panel"), "Cmd+K");
        // Only the palette's chord: Ctrl+J is nothing.
        press_in_palette(&mut vdom, &mut seen, "Escape").await;
        press_chord(&mut vdom, &mut seen, "j", ctrl(), false).await;
        assert!(!html(&vdom).contains("pal-panel"));
    }

    /// Typing narrows the list as it goes, and a query nothing matches says so.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn typing_filters_the_list_as_you_go() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        let all = palette_lines(&html(&vdom));
        type_in_palette(&mut vdom, &mut seen, "false").await;
        let page = html(&vdom);
        assert!(page.contains("false positive"));
        assert!(!page.contains("Confirm this finding"), "narrowed: {page}");
        let some = palette_lines(&page);
        assert!(some < all && some >= 1, "{some} of {all}");
        type_in_palette(&mut vdom, &mut seen, "zzzz").await;
        let page = html(&vdom);
        assert!(page.contains("No command matches"), "{page}");
        assert_eq!(palette_lines(&page), 0, "{page}");
        // Enter with nothing shown runs nothing and the palette stays.
        press_in_palette(&mut vdom, &mut seen, "Enter").await;
        assert!(html(&vdom).contains("pal-panel"));
        assert!(probe().dispositions.peek().is_empty());
    }

    /// Letters typed into the field are text. They are never the pane's hotkeys.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn letters_typed_in_the_palette_are_not_dispositions() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        for letter in ["c", "i", "t", "f", "o", "b"] {
            press_in_palette(&mut vdom, &mut seen, letter).await;
        }
        assert!(probe().dispositions.peek().is_empty());
        assert!(!html(&vdom).contains("sk-panel"));
        assert!(html(&vdom).contains("pal-panel"));
    }

    /// `Confirm` from the palette is `C`: the same write, the same five-second undo. `Tech debt`
    /// applies at once too.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_confirm_and_tech_debt_entries_do_what_their_keys_do() {
        // By the key first, for the reference result.
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "c");
        settle(&mut vdom, &mut seen).await;
        let by_key = dispositions_of(probe());
        assert!(html(&vdom).contains("Confirmed 1 finding."));
        drop((vdom, seen, _server, _env));

        // Then by the palette, on a fresh Queue.
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        run_entry(&mut vdom, &mut seen, pane, "confirm").await;
        assert_eq!(dispositions_of(probe()), by_key, "the same write");
        assert!(
            html(&vdom).contains("Confirmed 1 finding."),
            "the same undo"
        );
        assert!(!html(&vdom).contains("pal-panel"), "and it closed");

        run_entry(&mut vdom, &mut seen, pane, "tech debt").await;
        assert_eq!(state_count(TriageState::TechDebt), 1);
    }

    /// `Ignore` and `False positive` from the palette raise the reason prompt, as `I` and `F`
    /// do, and write nothing yet.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_reasoned_entries_prompt_like_their_keys() {
        for (query, hotkey, heading) in [
            ("ignore", "i", "Reason for \u{201c}Ignored\u{201d}"),
            (
                "false positive",
                "f",
                "Reason for \u{201c}False positive\u{201d}",
            ),
        ] {
            let (mut vdom, mut seen, _server, _env) =
                mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
            let pane = arm_pane(&mut vdom, &mut seen).await;
            key(&vdom, pane, hotkey);
            settle(&mut vdom, &mut seen).await;
            assert!(html(&vdom).contains(heading), "the key: {}", html(&vdom));
            drop((vdom, seen, _server, _env));

            let (mut vdom, mut seen, _server, _env) =
                mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
            let pane = arm_pane(&mut vdom, &mut seen).await;
            run_entry(&mut vdom, &mut seen, pane, query).await;
            let page = html(&vdom);
            assert!(page.contains(heading), "the palette: {page}");
            assert!(
                probe().dispositions.peek().is_empty(),
                "nothing written yet"
            );
            assert!(!page.contains("pal-panel"));
        }
    }

    /// `Next finding` is the Down arrow, `Review this group stacked` is `B`, and `Open in the
    /// code viewer` is `O`.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_move_stack_and_open_entries_do_what_their_keys_do() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        assert!(pane_position(&html(&vdom)).starts_with("1 of"));
        run_entry(&mut vdom, &mut seen, pane, "next").await;
        assert!(pane_position(&html(&vdom)).starts_with("2 of"));
        run_entry(&mut vdom, &mut seen, pane, "previous").await;
        assert!(pane_position(&html(&vdom)).starts_with("1 of"));

        run_entry(&mut vdom, &mut seen, pane, "stacked").await;
        let page = html(&vdom);
        assert!(page.contains("sk-panel"), "B's stacked view: {page}");
        assert!(page.contains("data-stack-label=\"SQL-1\""));
        // Close it, and open the viewer the same way.
        let panel = seen.element(STACK_ID);
        key(&vdom, panel, "Escape");
        settle(&mut vdom, &mut seen).await;
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        run_entry(&mut vdom, &mut seen, pane, "code viewer").await;
        assert!(html(&vdom).contains(VIEWER_ID), "O's viewer");
        assert!(
            !FOCUS_LOG.with(|l| l.borrow().contains(&RESTORE_LOG_MARK.to_string())),
            "the viewer has the keyboard and the palette did not take it back"
        );
    }

    /// The order entries set the cut the Order select sets; the current cut is not listed.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_order_entry_sets_the_queues_cut() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::Curated, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(!html(&vdom).contains("Order the Queue: Curated"));
        type_in_palette(&mut vdom, &mut seen, "by file").await;
        press_in_palette(&mut vdom, &mut seen, "Enter").await;
        let page = html(&vdom);
        let at = page.find("queue-order-select").expect("the select");
        assert!(
            page[at..].contains("aria-label=\"Order the Queue\" value=\"by_file\""),
            "the select now shows the By file cut: {}",
            &page[at..at + 400]
        );
        // Reopened, the cut it left is listed and the one it is in is not.
        ctrl_k(&mut vdom, &mut seen, pane).await;
        let page = html(&vdom);
        assert!(page.contains("Order the Queue: Curated"), "{page}");
        assert!(!page.contains("Order the Queue: By file"));
    }

    /// `Undo` appears only while the undo is live, and it is the pane's undo.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_undo_entry_is_the_panes_undo() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 1);
        run_entry(&mut vdom, &mut seen, pane, "undo").await;
        assert_eq!(state_count(TriageState::Confirmed), 0, "reverted");
        assert!(toast_said("Undone: 1 finding(s) put back."));
    }

    /// The timer entry is the strip's Pause / Resume button, and the stats entry is the strip's
    /// Review time button.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_timer_and_stats_entries_are_the_strips_controls() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        assert!(
            html(&vdom).contains(">Pause<"),
            "the strip's button says Pause"
        );
        run_entry(&mut vdom, &mut seen, pane, "pause").await;
        let page = html(&vdom);
        assert!(
            page.contains(">Resume<"),
            "the strip's button now says Resume: {page}"
        );
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(
            html(&vdom).contains("Resume the review timer"),
            "and the palette says it too"
        );
        press_in_palette(&mut vdom, &mut seen, "Escape").await;

        let before = STATS_OPENED.with(std::cell::Cell::get);
        run_entry(&mut vdom, &mut seen, pane, "show the review time").await;
        assert_eq!(STATS_OPENED.with(std::cell::Cell::get), before + 1);
    }

    /// The pane's rule and detail buttons have palette lines. (The hub and the modal are hosted at
    /// the results root, outside this harness, so what is pinned here is that the lines exist for
    /// a focused row and run without writing.)
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_hub_and_detail_entries_exist_for_a_focused_row_and_write_nothing() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        let page = html(&vdom);
        assert!(
            page.contains("Show the full detail of this finding"),
            "{page}"
        );
        assert!(page.contains("Open the Rule Hub for this finding"));
        press_in_palette(&mut vdom, &mut seen, "Escape").await;
        assert!(probe().detail.peek().is_none());
        run_entry(&mut vdom, &mut seen, pane, "full detail").await;
        assert_eq!(
            probe()
                .detail
                .peek()
                .as_ref()
                .map(|f| (f.rule_id.clone(), f.line)),
            Some(("SQL-1".to_string(), 7)),
            "the pane's Full detail callback was called with the focused finding"
        );
        // The dialog is up now, so the palette is (rightly) refused until it is closed.
        let mut detail = probe().detail;
        vdom.in_runtime(|| detail.set(None));
        settle(&mut vdom, &mut seen).await;
        let before = HUB_REQUESTS.with(|r| r.borrow().len());
        run_entry(&mut vdom, &mut seen, pane, "rule hub").await;
        assert_eq!(
            HUB_REQUESTS.with(|r| r.borrow()[before..].to_vec()),
            vec![("me/api".to_string(), "SQL-1".to_string())],
            "the pane's rule button callback was called for the focused rule"
        );
        assert!(probe().dispositions.peek().is_empty());
    }

    /// A line of the list can be clicked, and runs that entry.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn clicking_a_line_runs_it() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        // Line 0 is the first decision entry: Confirm.
        click(&vdom, seen.element(&option_id(0)));
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 1);
        assert!(!html(&vdom).contains("pal-panel"));
    }

    /// Down moves the highlight, and Enter runs the highlighted line.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_arrows_move_the_highlight_and_enter_runs_it() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        // Down once: the second decision line (Ignore) is highlighted; Enter raises its prompt.
        press_in_palette(&mut vdom, &mut seen, "ArrowDown").await;
        press_in_palette(&mut vdom, &mut seen, "Enter").await;
        assert!(html(&vdom).contains("pane-reason-prompt"));
        assert!(probe().dispositions.peek().is_empty());
    }

    /// REFUSED while the pane's reason prompt is open, by both doors, with the reason said.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_palette_is_refused_under_the_panes_reason_prompt() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("pane-reason-prompt"));

        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(!html(&vdom).contains("pal-panel"), "Ctrl+K refused");
        assert!(toast_said("Finish or cancel the reason first"));

        probe().toasts.clone().write().clear();
        click(&vdom, seen.element(PALETTE_BUTTON_ID));
        settle(&mut vdom, &mut seen).await;
        assert!(!html(&vdom).contains("pal-panel"), "the button refused");
        assert!(toast_said("Finish or cancel the reason first"));
        assert!(
            html(&vdom).contains("pane-reason-prompt"),
            "and the prompt is still open"
        );
    }

    /// REFUSED while the toolbar's reason prompt is open too (it is a reason prompt as well).
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_palette_is_refused_under_the_toolbars_reason_prompt() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        select_and_press(&mut vdom, &mut seen, &[0], TriageState::Ignored).await;
        assert!(html(&vdom).contains("queue-reason-prompt"));
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(!html(&vdom).contains("pal-panel"));
        assert!(toast_said("Finish or cancel the reason first"));
    }

    /// REFUSED while the stacked view is up over the Queue.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_palette_is_refused_over_the_stacked_view() {
        let (mut vdom, mut seen, _server, _env) =
            mount(deviations(3), OrderPreset::Curated, "ok").await;
        let pane = seen.element(super::super::scan::DETAIL_PANE_ID);
        open_from_bar(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("sk-panel"));
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(!html(&vdom).contains("pal-panel"));
        assert!(toast_said(
            "Close the code viewer or the stacked view first"
        ));
    }

    /// REFUSED under every kind of dialog, with the reason said and nothing decided. The full
    /// detail view and the Rule Hub are known by their state; the review-time view (and any dialog
    /// not listed anywhere) is known by what the page reports from the DOM at the press.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_palette_is_refused_under_the_detail_the_hub_and_any_other_dialog() {
        for dialog in ["detail", "hub", "dom"] {
            let (mut vdom, mut seen, _server, _env) =
                mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
            let pane = arm_pane(&mut vdom, &mut seen).await;
            let (mut detail, mut hub) = (probe().detail, probe().hub);
            let dom_modal = dialog == "dom";
            match dialog {
                "detail" => vdom.in_runtime(|| detail.set(Some(defect(7)))),
                "hub" => vdom.in_runtime(|| {
                    hub.set(Some(RuleHubRequest {
                        repo: "me/api".to_string(),
                        rule_id: "SQL-1".to_string(),
                    }))
                }),
                _ => {}
            }
            settle(&mut vdom, &mut seen).await;
            press_chord(&mut vdom, &mut seen, "k", ctrl(), dom_modal).await;
            assert!(!html(&vdom).contains("pal-panel"), "{dialog}: refused");
            assert!(
                toast_said("Close the open dialog first"),
                "{dialog}: and says why"
            );
            // Enter would have been Confirm, the first line. Nothing can have run.
            key(&vdom, pane, "Enter");
            settle(&mut vdom, &mut seen).await;
            assert!(probe().dispositions.peek().is_empty(), "{dialog}");

            // Closed again, it opens.
            vdom.in_runtime(|| {
                detail.set(None);
                hub.set(None);
            });
            settle(&mut vdom, &mut seen).await;
            ctrl_k(&mut vdom, &mut seen, pane).await;
            assert!(
                html(&vdom).contains("pal-panel"),
                "{dialog}: open once closed"
            );
        }
    }

    /// The gate is checked again when an entry runs, not only when the palette opens: a reason
    /// prompt that opens under an open palette (here by the pane's own key) stops the entry, and
    /// the keyboard goes to that prompt, not to the pane (whose next key would drop the reason).
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_entry_is_refused_if_a_reason_prompt_opened_under_the_palette() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        key(&vdom, pane, "i");
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("pane-reason-prompt"));
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        press_in_palette(&mut vdom, &mut seen, "Enter").await;
        assert!(
            probe().dispositions.peek().is_empty(),
            "Confirm did not run"
        );
        assert!(toast_said("Finish or cancel the reason first"));
        assert!(html(&vdom).contains("pane-reason-prompt"));
        assert!(!html(&vdom).contains("pal-panel"), "the palette closed");
        assert_eq!(
            last_focus(),
            Some(PANE_REASON_ID.to_string()),
            "the keyboard is in the open prompt"
        );
        assert!(
            !FOCUS_LOG.with(|l| l.borrow().contains(&RESTORE_LOG_MARK.to_string())),
            "and was not handed to the pane"
        );
    }

    /// Tab and Shift+Tab stay in the palette (their default is prevented), and ordinary typing
    /// is left alone.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn tab_and_shift_tab_are_trapped_in_the_palette() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        let field = seen.element(PALETTE_INPUT_ID);
        assert!(default_prevented(&vdom, field, Key::Tab, false), "Tab");
        assert!(default_prevented(&vdom, field, Key::Tab, true), "Shift+Tab");
        assert!(
            !default_prevented(&vdom, field, Key::Character("a".to_string()), false),
            "a letter is still typed"
        );
        settle(&mut vdom, &mut seen).await;
        assert!(html(&vdom).contains("pal-panel"), "and it is still open");
        assert!(probe().dispositions.peek().is_empty());
    }

    /// A line is checked again when it runs. Here the focused row is decided from under the open
    /// palette: "Confirm this finding" has nothing to act on, so it says so and runs nothing.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_line_whose_row_is_gone_says_so_and_runs_nothing() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        // The one row is decided by its key; the table is empty and the palette still lists Confirm.
        key(&vdom, pane, "c");
        settle(&mut vdom, &mut seen).await;
        assert_eq!(state_count(TriageState::Confirmed), 1);
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        press_in_palette(&mut vdom, &mut seen, "Enter").await;
        assert_eq!(
            state_count(TriageState::Confirmed),
            1,
            "nothing further was decided"
        );
        assert!(
            toast_said("is no longer available"),
            "{:?}",
            probe()
                .toasts
                .peek()
                .iter()
                .map(|t| t.message.clone())
                .collect::<Vec<_>>()
        );
        assert!(!html(&vdom).contains("pal-panel"));
        assert_eq!(
            last_focus(),
            Some(RESTORE_LOG_MARK.to_string()),
            "the keyboard goes back to where it was"
        );
    }

    /// The Pause line is not run as Resume. The timer is paused from under the open palette (as
    /// the idle rule would), so the line saying "Pause" is stale: it says what it became.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_pause_line_is_not_run_as_resume_when_the_timer_flipped() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(html(&vdom).contains("Pause the review timer"));
        let handle = review();
        vdom.in_runtime(|| handle.toggle());
        settle(&mut vdom, &mut seen).await;
        assert!(
            html(&vdom).contains(">Resume<"),
            "the strip says Resume now"
        );
        type_in_palette(&mut vdom, &mut seen, "pause").await;
        press_in_palette(&mut vdom, &mut seen, "Enter").await;
        assert!(toast_said("changed while the palette was open"));
        assert!(
            html(&vdom).contains(">Resume<"),
            "the timer was not resumed by the stale line"
        );
        // Reopened, the line says what is true.
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(html(&vdom).contains("Resume the review timer"));
    }

    /// Esc closes it, changes nothing, and gives the keyboard back.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn escape_closes_the_palette_and_restores_focus() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        ctrl_k(&mut vdom, &mut seen, pane).await;
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        press_in_palette(&mut vdom, &mut seen, "Escape").await;
        assert!(!html(&vdom).contains("pal-panel"));
        assert!(probe().dispositions.peek().is_empty());
        assert_eq!(
            last_focus(),
            Some(RESTORE_LOG_MARK.to_string()),
            "the keyboard was given back"
        );
        // The chord closes it too, and a second Ctrl+K opens a fresh one.
        ctrl_k(&mut vdom, &mut seen, pane).await;
        type_in_palette(&mut vdom, &mut seen, "false").await;
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(!html(&vdom).contains("pal-panel"), "the chord closes it");
        assert_eq!(
            last_focus(),
            Some(RESTORE_LOG_MARK.to_string()),
            "and gives the keyboard back"
        );
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert!(
            html(&vdom).contains("Confirm this finding"),
            "a clean field"
        );
    }

    /// An action that opens something with its own keyboard (the viewer, the stack, a prompt)
    /// does not hand focus back over it; one that does not, does.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn focus_goes_back_after_a_plain_action_and_not_after_an_opening_one() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        run_entry(&mut vdom, &mut seen, pane, "next").await;
        assert_eq!(last_focus(), Some(RESTORE_LOG_MARK.to_string()));
        FOCUS_LOG.with(|l| l.borrow_mut().clear());
        run_entry(&mut vdom, &mut seen, pane, "ignore").await;
        assert!(
            !FOCUS_LOG.with(|l| l.borrow().contains(&RESTORE_LOG_MARK.to_string())),
            "the reason prompt keeps the keyboard it took"
        );
    }

    /// Palette activity is review activity: the overlay is a `data-review-surface`, which is what
    /// the timer's bridge counts keys and clicks inside.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn the_palette_counts_as_a_review_surface_for_the_timer() {
        let (mut vdom, mut seen, _server, _env) =
            mount(vec![defect(7), defect(8)], OrderPreset::CriticalFirst, "ok").await;
        let pane = arm_pane(&mut vdom, &mut seen).await;
        let closed = html(&vdom).matches("data-review-surface").count();
        ctrl_k(&mut vdom, &mut seen, pane).await;
        assert_eq!(
            html(&vdom).matches("data-review-surface").count(),
            closed + 1,
            "the open palette is one more review surface"
        );
        let open = html(&vdom);
        let at = open.find("pal-overlay").unwrap();
        assert!(
            open[at..(at + 60).min(open.len())].contains("data-review-surface"),
            "on the palette's own overlay: {open}"
        );
        press_in_palette(&mut vdom, &mut seen, "Escape").await;
        assert_eq!(html(&vdom).matches("data-review-surface").count(), closed);
    }
}
