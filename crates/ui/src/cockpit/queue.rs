//! The escalation queue lane (review UI spec section 6; design section 7).
//!
//! Two layers, per spec rule 1, the same as the tree lane:
//!
//! - [`QueueScreen`] and the small views under it are props-only. Every judgment they draw (the
//!   order, where the line falls and what it says, the sentences, which rows get a Needed me
//!   box, which keys are armed) comes from `camerata_ui_core::queue`; they choose words and
//!   pixels. The gallery renders them from fixtures and the tests render them from props.
//! - [`QueueHost`] owns the signals and the I/O. It reads through `ReviewApi` (never a request
//!   inside a view), writes decisions and the Needed me flag through the same handle, and hands
//!   a row's site or node to the tree lane through the shell's `DoorRequest`.
//!
//! # Why the rows are not a `CamerataTable`
//!
//! The spec draws a flat `CamerataTable`. The tree lane's reasons hold here too (see the note at
//! the top of `tree.rs`): a chorale cell renderer is `Fn + Send + Sync` and cannot hold a Dioxus
//! `Callback`, so the Needed me checkbox and a row's focus cannot be cells; and the keyboard
//! cursor walks the order the pure `queue::layout` produced, with the line drawn between two
//! rows, which a sortable table would re-order under it. So the rows are a plain table in the
//! tree's own markup and classes.
//!
//! # Show below the line (spec U5)
//!
//! The host asks for the first page of candidates below the line with every read of the queue.
//! "Show below the line" is then a flag on the view state: it sends no request and writes
//! nothing. Only "Show 100 more candidates" reads another page.

use camerata_api_types::review_view::{EscalationBudgetOutcome, QueueRow, QueueView, SiteRow};
use camerata_ui::review_io::ReviewApi;
use camerata_ui_core::edits::DetailContext;
use camerata_ui_core::queue::{
    band_of, budget_line, counts_line, decision_text, detail_context, empty_text,
    has_needed_me_box, keys_hint, layout, needed_me_checked, needed_me_enabled, needed_me_line,
    needed_me_write, node_cell, node_door, node_title, queue_command, queue_keys_armed,
    rate_changed_text, reason_cell, site_door, Band, Entry, Layout, QueueCommand, QueueState,
    NEEDED_ME_HINT, ROWS_PER_PAGE,
};
use camerata_ui_core::tree::batch::{decision_state_for, site_decision_request, TreeUndo};
use camerata_ui_core::tree::export::Door;
use camerata_ui_core::tree::shell::{lane_for_key, Lane};
use camerata_ui_core::tree::{
    dom_token, file_line, severity_badge, severity_label, stage1_cell, thousands, DecisionTone,
};
use camerata_ui_core::triage::{FocusMove, KeyMods, PaneCommand, PaneFocus, TriageState};
use dioxus::prelude::*;

use super::review_shell::LaneControl;
use super::site_detail::code_bridge::use_code_bridge;
use super::site_detail::{use_detail, DetailEvent, DetailPanel, SiteDetailView};
use super::tree::{DoorRequest, ShellRefresh, UndoBanner};

/// The DOM id of the queue's focusable region. Tabbing into it arms the keys.
pub(crate) const QUEUE_REGION_ID: &str = "camerata-queue-region";

/// The row the cursor is on, for the follow-scroll.
pub(crate) const FOCUSED_ROW_SELECTOR: &str = "#camerata-queue-region .queue-row.focused";

/// The most recent budget change the Settings lane applied, so the queue can say what it did the
/// next time it is shown ("Budget is now 450. ..."). Provided by the shell host; the queue
/// clears it when the reviewer dismisses the banner.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct AppliedBudget(pub Signal<Option<EscalationBudgetOutcome>>);

// ── What the screen shows ─────────────────────────────────────────────────────────────────

/// How the lane stands (spec section 6, "States").
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum QueueStatus {
    /// The queue request is in flight.
    Loading,
    /// The request failed, with the server's reason. A queue that had loaded stays.
    Failed(String),
    /// The queue is there.
    Ready,
}

/// The reason prompt for one site's Ignore or False positive.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct QueuePrompt {
    pub key: String,
    pub state: TriageState,
    pub reason: String,
    pub error: Option<String>,
}

/// Rows drawn for the loading state.
const SKELETON_ROWS: usize = 5;

// ── The screen ────────────────────────────────────────────────────────────────────────────

/// The whole queue lane: the table on the left, the focused row on the right.
#[component]
pub(crate) fn QueueScreen(
    view: Option<QueueView>,
    status: QueueStatus,
    state: QueueState,
    focus: PaneFocus,
    prompt: Option<QueuePrompt>,
    undo: Option<UndoBanner>,
    /// What a budget change just did, as the toast says it.
    #[props(default)]
    toast: Option<String>,
    /// More candidates are being read.
    #[props(default)]
    reading_more: bool,
    #[props(default)] on_key: Callback<(String, KeyMods)>,
    #[props(default)] on_focus: Callback<PaneFocus>,
    #[props(default)] on_focus_row: Callback<String>,
    #[props(default)] on_toggle_below: Callback<()>,
    #[props(default)] on_more: Callback<()>,
    #[props(default)] on_more_below: Callback<()>,
    #[props(default)] on_needed_me: Callback<String>,
    #[props(default)] on_open_site: Callback<String>,
    #[props(default)] on_open_node: Callback<String>,
    #[props(default)] on_decide: Callback<TriageState>,
    #[props(default)] on_settings: Callback<()>,
    #[props(default)] on_prompt_reason: Callback<String>,
    #[props(default)] on_prompt_commit: Callback<()>,
    #[props(default)] on_prompt_cancel: Callback<()>,
    #[props(default)] on_undo: Callback<()>,
    #[props(default)] on_dismiss_toast: Callback<()>,
    #[props(default)] on_retry: Callback<()>,
    /// The site detail's open form, undo bar and notice (the detail's own state, from `use_detail`).
    #[props(default)]
    detail: DetailPanel,
    /// Something the reviewer did in the site detail.
    #[props(default)]
    on_detail: Callback<DetailEvent>,
) -> Element {
    let armed = queue_keys_armed(focus);
    let prompt_open = prompt.is_some();
    let drawn: Option<Layout<'_>> = view.as_ref().map(|v| layout(v, &state));
    let cursor_row: Option<QueueRow> = view.as_ref().and_then(|v| {
        let key = state.cursor.as_deref()?;
        v.escalated
            .iter()
            .chain(&v.below)
            .find(|r| r.candidate.key == key)
            .cloned()
    });
    let on_row = cursor_row.is_some();
    use_effect(use_reactive(&state.cursor, move |_| {
        super::viewer::follow_selected(FOCUSED_ROW_SELECTOR);
    }));
    let empty = view.as_ref().is_some_and(|v| v.escalated.is_empty());
    rsx! {
        if let Some(banner) = undo {
            div { class: "tree-undo-dock",
                div { class: "pane-undo",
                    span { class: "pane-undo-msg", "{banner.message}" }
                    button { class: "btn-edit-sm pane-undo-btn", onclick: move |_| on_undo.call(()), "Undo" }
                    span { class: "pane-undo-left", "{banner.seconds_left}s" }
                }
            }
        }
        div { class: "review-queue-lane",
            div { class: "review-queue-left",
                if let Some(text) = toast {
                    div { class: "review-queue-toast", role: "status",
                        span { class: "review-queue-toast-msg", "{text}" }
                        button { class: "btn-edit-sm", onclick: move |_| on_dismiss_toast.call(()), "Dismiss" }
                    }
                }
                if let QueueStatus::Failed(message) = &status {
                    div { class: "review-banner", role: "alert",
                        span { class: "review-banner-msg", "{message}" }
                        button { class: "btn-edit-sm", onclick: move |_| on_retry.call(()), "Retry" }
                    }
                }
                if status == QueueStatus::Loading && view.is_none() {
                    p { class: "review-loading", "Loading the escalations" }
                    div { class: "tree-skel",
                        for n in 0..SKELETON_ROWS {
                            div { key: "{n}", class: "tree-skel-row" }
                        }
                    }
                }
                if let Some(v) = view.as_ref() {
                    QueueHead { view: v.clone(), on_settings }
                    if empty {
                        p { class: "pane-empty review-queue-empty", "{empty_text(v)}" }
                    }
                    if let Some(l) = drawn.as_ref() {
                        if !l.entries.is_empty() {
                            div {
                                class: "queue-region",
                                id: "{QUEUE_REGION_ID}",
                                // A focusable REGION: tabbing in arms the keys, tabbing out disarms them.
                                tabindex: "0",
                                onfocusin: move |_| on_focus.call(PaneFocus::Cursor),
                                onfocusout: move |_| on_focus.call(PaneFocus::Elsewhere),
                                onkeydown: move |e: KeyboardEvent| {
                                    let m = e.modifiers();
                                    let mods = KeyMods {
                                        ctrl: m.contains(Modifiers::CONTROL),
                                        alt: m.contains(Modifiers::ALT),
                                        meta: m.contains(Modifiers::META),
                                        shift: m.contains(Modifiers::SHIFT),
                                    };
                                    let key = e.key().to_string();
                                    // Swallow only the presses the queue claims, so Tab still tabs
                                    // and Cmd-C still copies.
                                    if queue_command(focus, &key, mods, on_row).is_some() {
                                        e.prevent_default();
                                        e.stop_propagation();
                                    }
                                    on_key.call((key, mods));
                                },
                                QueueTable {
                                    entries: l.entries.iter().map(entry_owned).collect::<Vec<_>>(),
                                    cursor: state.cursor.clone(),
                                    on_focus_row,
                                    on_needed_me,
                                    on_open_node,
                                }
                            }
                        }
                        QueueFooter {
                            hidden_escalated: l.hidden_escalated,
                            footer: l.footer.clone(),
                            show_below: state.show_below,
                            below_unloaded: l.below_unloaded,
                            reading_more,
                            on_more,
                            on_more_below,
                            on_toggle_below,
                        }
                    }
                    div { class: "pane-actions tree-keys",
                        span {
                            class: if armed { "pane-keys armed" } else { "pane-keys off" },
                            "{keys_hint(armed, prompt_open)}"
                        }
                    }
                }
            }
            div { class: "review-queue-right",
                if let Some(row) = cursor_row {
                    QueueSide {
                        ctx: view.as_ref().map(|v| detail_context(v, &row)).unwrap_or_default(),
                        row,
                        prompt: prompt.clone(),
                        detail: detail.clone(),
                        on_detail,
                        on_needed_me,
                        on_open_site,
                        on_open_node,
                        on_prompt_reason,
                        on_prompt_commit,
                        on_prompt_cancel,
                    }
                } else if view.as_ref().is_some_and(|v| !v.escalated.is_empty()) {
                    p { class: "pane-empty", "Select a row." }
                }
            }
        }
    }
}

/// An owned copy of a layout entry, so a row can be a component prop.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum OwnedEntry {
    Row(Box<QueueRow>),
    Line(String),
    OverBand(String),
    BelowHeading(String),
}

fn entry_owned(e: &Entry<'_>) -> OwnedEntry {
    match e {
        Entry::Row(r) => OwnedEntry::Row(Box::new((*r).clone())),
        Entry::Line(t) => OwnedEntry::Line(t.clone()),
        Entry::OverBand(t) => OwnedEntry::OverBand(t.clone()),
        Entry::BelowHeading(t) => OwnedEntry::BelowHeading(t.clone()),
    }
}

// ── The head ──────────────────────────────────────────────────────────────────────────────

/// The title, the budget sentence, the counts and Needed me.
#[component]
fn QueueHead(view: QueueView, on_settings: Callback<()>) -> Element {
    rsx! {
        div { class: "review-queue-head",
            div { class: "review-queue-title",
                h3 { class: "node-title", "Escalations" }
                span { class: "review-queue-budget", "{budget_line(&view)}" }
                button {
                    class: "btn-edit-sm",
                    title: "Open Settings to change the rate or the floor. Press S while the queue has focus.",
                    onclick: move |_| on_settings.call(()),
                    "Change"
                }
            }
            p { class: "review-queue-counts", "{counts_line(&view)}" }
            p { class: "review-queue-needed", "{needed_me_line(&view.needed_me)}" }
        }
    }
}

// ── The table ─────────────────────────────────────────────────────────────────────────────

/// The rows, the line, the over-budget band and the candidates below the line.
#[component]
fn QueueTable(
    entries: Vec<OwnedEntry>,
    cursor: Option<String>,
    on_focus_row: Callback<String>,
    on_needed_me: Callback<String>,
    on_open_node: Callback<String>,
) -> Element {
    rsx! {
        table { class: "site-table queue-table",
            colgroup {
                col { style: "width: 40px" }
                col { style: "width: 18%" }
                col { style: "width: 9%" }
                col { style: "width: 15%" }
                col { style: "width: 16%" }
                col { style: "width: 11%" }
                col { style: "width: 12%" }
                col { style: "width: 11%" }
                col { style: "width: 64px" }
            }
            thead {
                tr {
                    th { class: "col-rank", "#" }
                    th { "Reason" }
                    th { "Sev" }
                    th { "Node" }
                    th { "Site" }
                    th { "Stage 1" }
                    th { "Stage 2 why" }
                    th { "Decision" }
                    th { class: "col-need", title: "Needed me", "Needed" }
                }
            }
            tbody {
                for (i, entry) in entries.into_iter().enumerate() {
                    {
                        match entry {
                            OwnedEntry::Row(row) => rsx! {
                                QueueRowView {
                                    key: "{dom_token(&row.candidate.key)}",
                                    focused: cursor.as_deref() == Some(row.candidate.key.as_str()),
                                    row: *row,
                                    on_focus_row,
                                    on_needed_me,
                                    on_open_node,
                                }
                            },
                            OwnedEntry::Line(text) => rsx! {
                                tr { key: "line-{i}", class: "queue-line",
                                    td { colspan: "9", span { class: "queue-line-text", "{text}" } }
                                }
                            },
                            OwnedEntry::OverBand(text) => rsx! {
                                tr { key: "over-{i}", class: "queue-over-band",
                                    td { colspan: "9", span { class: "mark-over", "over" } " {text}" }
                                }
                            },
                            OwnedEntry::BelowHeading(text) => rsx! {
                                tr { key: "below-{i}", class: "queue-below-heading",
                                    td { colspan: "9", "{text}" }
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}

/// One row of the queue (spec section 6, "Columns").
#[component]
fn QueueRowView(
    row: QueueRow,
    focused: bool,
    on_focus_row: Callback<String>,
    on_needed_me: Callback<String>,
    on_open_node: Callback<String>,
) -> Element {
    let token = dom_token(&row.candidate.key);
    let band = band_of(&row);
    let site = &row.site;
    let mut class = String::from("site-row queue-row");
    if site.severity == "critical" {
        class.push_str(" finding-row-critical");
    }
    match band {
        Band::WithinBudget => class.push_str(" row-escalated"),
        Band::OverBudget => class.push_str(" row-escalated row-over"),
        Band::BelowLine => class.push_str(" row-candidate"),
    }
    if site.held.is_some() {
        class.push_str(" row-held");
    }
    if focused {
        class.push_str(" focused");
    }
    let decision = camerata_ui_core::tree::decision_cell(site);
    let enabled = needed_me_enabled(&row);
    let boxed = has_needed_me_box(&row);
    let checked = needed_me_checked(&row);
    let key_focus = row.candidate.key.clone();
    let key_need = row.candidate.key.clone();
    let node_id = row.node_path.last().map(|c| c.id.clone());
    let why = site
        .stage2
        .as_ref()
        .map(|s| s.why.clone())
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| row.candidate.why.clone());
    rsx! {
        tr {
            class: "{class}",
            id: "queue-row-{token}",
            "data-site": "{token}",
            onclick: move |_| on_focus_row.call(key_focus.clone()),
            td { class: "col-rank", title: "Global rank", "{thousands(row.candidate.global_rank as usize)}" }
            td { class: "col-reason",
                if band == Band::BelowLine {
                    span { class: "mark-cand", title: "{marker_tip(&row)}", "candidate" }
                    " "
                }
                "{reason_cell(&row)}"
            }
            td { class: "col-sev",
                span { class: "tree-badge tree-badge-{severity_badge(&site.severity)}", "{severity_label(&site.severity)}" }
            }
            td { class: "col-node", title: "{node_title(&row)}",
                if let Some(id) = node_id {
                    a {
                        class: "queue-node-link",
                        href: "#",
                        onclick: move |e| {
                            e.prevent_default();
                            e.stop_propagation();
                            on_open_node.call(id.clone());
                        },
                        "{node_cell(&row)}"
                    }
                } else {
                    "{node_cell(&row)}"
                }
            }
            td { class: "col-loc", title: "{site.repo}", "{file_line(site)}" }
            td { class: "col-s1", title: "{site.stage1.reason}", "{stage1_cell(site)}" }
            td { class: "col-why", title: "{why}", "{why}" }
            td { class: "col-dec",
                span {
                    class: match decision.tone {
                        DecisionTone::Decided => "dec-decided",
                        DecisionTone::Unresolved => "dec-unresolved",
                    },
                    "{decision_text(&row)}"
                }
                if decision.inherited {
                    span { class: "dec-inherited", "inherited" }
                }
            }
            td { class: "col-need",
                if boxed {
                    input {
                        r#type: "checkbox",
                        checked,
                        disabled: !enabled,
                        title: if enabled { "Needed me: I needed to see this row" } else { "{NEEDED_ME_HINT}" },
                        "aria-label": "Needed me",
                        onclick: move |e| e.stop_propagation(),
                        onchange: move |_| on_needed_me.call(key_need.clone()),
                    }
                }
            }
        }
    }
}

fn marker_tip(row: &QueueRow) -> String {
    format!(
        "The group reviewer wanted this escalated; it is below the line (rank {}).",
        row.candidate.global_rank
    )
}

// ── The footer ────────────────────────────────────────────────────────────────────────────

/// "Show 100 more" for the rows, and the candidates below the line.
#[component]
fn QueueFooter(
    hidden_escalated: usize,
    footer: Option<String>,
    show_below: bool,
    below_unloaded: usize,
    reading_more: bool,
    on_more: Callback<()>,
    on_more_below: Callback<()>,
    on_toggle_below: Callback<()>,
) -> Element {
    rsx! {
        if hidden_escalated > 0 {
            div { class: "sites-more",
                button { class: "btn-edit-sm", onclick: move |_| on_more.call(()),
                    "Show {ROWS_PER_PAGE} more"
                }
                span { "{thousands(hidden_escalated)} escalated rows not shown" }
            }
        }
        if let Some(text) = footer {
            div { class: "sites-more review-queue-below",
                span { "{text}" }
                if !show_below {
                    button {
                        class: "btn-edit-sm",
                        title: "A look only. It changes nothing and sends nothing. Change the rate in Settings to move the line.",
                        onclick: move |_| on_toggle_below.call(()),
                        "Show below the line"
                    }
                } else if below_unloaded > 0 {
                    button {
                        class: "btn-edit-sm",
                        disabled: reading_more,
                        onclick: move |_| on_more_below.call(()),
                        "Show {ROWS_PER_PAGE} more"
                    }
                }
            }
        }
        if show_below {
            div { class: "sites-more review-queue-below",
                button {
                    class: "btn-edit-sm",
                    title: "Hide the candidates again. Nothing is written.",
                    onclick: move |_| on_toggle_below.call(()),
                    "Hide the candidates"
                }
            }
        }
    }
}

// ── The right pane ────────────────────────────────────────────────────────────────────────

/// The focused row: why it is in the queue, Needed me and the doors to the tree, then the same
/// site detail the tree opens (the fields and their edits, the decisions, merge, add), with the
/// queue's reason prompt in it.
#[component]
fn QueueSide(
    row: QueueRow,
    ctx: DetailContext,
    prompt: Option<QueuePrompt>,
    detail: DetailPanel,
    on_detail: Callback<DetailEvent>,
    on_needed_me: Callback<String>,
    on_open_site: Callback<String>,
    on_open_node: Callback<String>,
    on_prompt_reason: Callback<String>,
    on_prompt_commit: Callback<()>,
    on_prompt_cancel: Callback<()>,
) -> Element {
    let key = row.candidate.key.clone();
    let key_need = key.clone();
    let key_open = key.clone();
    let node_id = row.node_path.last().map(|c| c.id.clone());
    let here_prompt = prompt.filter(|p| p.key == key);
    let enabled = needed_me_enabled(&row);
    rsx! {
        div { class: "node-head queue-why",
            p { class: "site-pane-line",
                span { class: "site-pane-label", "Escalated for" }
                "{reason_cell(&row)}"
            }
            if !row.candidate.why.is_empty() {
                p { class: "site-pane-line", span { class: "site-pane-label", "Why" } "{row.candidate.why}" }
            }
            if has_needed_me_box(&row) {
                label { class: "site-pane-line", title: if enabled { "" } else { "{NEEDED_ME_HINT}" },
                    input {
                        r#type: "checkbox",
                        checked: needed_me_checked(&row),
                        disabled: !enabled,
                        onchange: move |_| on_needed_me.call(key_need.clone()),
                    }
                    " Needed me: I needed to see this row"
                }
            }
            div { class: "pane-actions",
                button {
                    class: "btn-edit-sm",
                    title: "Open this site in the tree lane. Press Enter while the queue has focus.",
                    onclick: move |_| on_open_site.call(key_open.clone()),
                    "Open in the tree"
                }
                if let Some(id) = node_id {
                    button {
                        class: "btn-edit-sm",
                        title: "Go to this row's sub-group in the tree. Press G while the queue has focus.",
                        onclick: move |_| on_open_node.call(id.clone()),
                        "Go to the node"
                    }
                }
            }
        }
        SiteDetailView {
            position: format!("rank {}", thousands(row.candidate.global_rank as usize)),
            ticked: false,
            batch_tick: false,
            stack: false,
            panel: detail,
            ctx,
            row: row.site.clone(),
            on_event: on_detail,
            if let Some(p) = here_prompt {
                QueuePromptView {
                    prompt: p,
                    on_reason: on_prompt_reason,
                    on_commit: on_prompt_commit,
                    on_cancel: on_prompt_cancel,
                }
            }
        }
    }
}

/// The reason prompt for one site's Ignore or False positive, drawn inside the detail.
#[component]
fn QueuePromptView(
    prompt: QueuePrompt,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    let p = prompt;
    rsx! {
        div { class: "queue-reason-prompt",
            label { class: "rule-modal-label",
                "Reason for \u{201c}{p.state.label()}\u{201d} on this site (required)"
            }
            textarea {
                class: "addressee-input queue-reason-input",
                rows: "2",
                placeholder: "why (the export reads this)",
                value: "{p.reason}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_reason.call(e.value()),
                onkeydown: move |e: KeyboardEvent| {
                    e.stop_propagation();
                    match e.key() {
                        Key::Escape => on_cancel.call(()),
                        Key::Enter if !e.modifiers().contains(Modifiers::SHIFT) => {
                            e.prevent_default();
                            on_commit.call(());
                        }
                        _ => {}
                    }
                },
            }
            if let Some(error) = p.error {
                p { class: "batch-error", role: "alert", "{error}" }
            }
            div { class: "queue-prompt-actions",
                button { class: "btn-run queue-prompt-commit", onclick: move |_| on_commit.call(()), "Commit \u{2192}" }
                button { class: "btn-edit-sm queue-prompt-cancel", onclick: move |_| on_cancel.call(()), "Cancel" }
            }
        }
    }
}

// ── The host ──────────────────────────────────────────────────────────────────────────────

/// Read the first page of the queue: the escalated rows and, so that "show below the line" needs
/// no request, the first page of the candidates below it.
pub(crate) async fn read_queue(
    reads: &dyn camerata_ui::review_source::ReviewSource,
) -> Result<QueueView, String> {
    reads
        .queue(
            true,
            0,
            camerata_api_types::review_view::QUEUE_BELOW_PAGE_SIZE,
        )
        .await
        .map_err(|e| e.message)
}

/// The next page of candidates below the line, appended to `view` (a read, never a write).
pub(crate) async fn read_more_below(
    reads: &dyn camerata_ui::review_source::ReviewSource,
    view: &QueueView,
) -> Result<QueueView, String> {
    let page = reads
        .queue(
            true,
            view.below.len(),
            camerata_api_types::review_view::QUEUE_BELOW_PAGE_SIZE,
        )
        .await
        .map_err(|e| e.message)?;
    if page.run_id != view.run_id {
        // The run changed under the reader: start from the new run's first page.
        return Ok(page);
    }
    let mut merged = view.clone();
    merged.below.extend(page.below);
    merged.below_total = page.below_total;
    Ok(merged)
}

/// The queue lane with its signals and its I/O.
#[component]
pub(crate) fn QueueHost(
    /// The commit each repo of the run was inspected at (the shell's `audited_shas`), for the
    /// code viewer the detail's Code button opens.
    #[props(default)]
    audited_shas: ReadSignal<std::collections::BTreeMap<String, String>>,
) -> Element {
    let api = use_context::<ReviewApi>();
    let refresh = try_use_context::<ShellRefresh>();
    let door = try_use_context::<DoorRequest>();
    let lanes = try_use_context::<LaneControl>();
    let applied = try_use_context::<AppliedBudget>();
    let toasts = try_use_context::<Signal<Vec<crate::toast::Toast>>>();
    let epoch = use_hook(std::time::Instant::now);

    let mut view = use_signal(|| Option::<QueueView>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| true);
    let mut reading_more = use_signal(|| false);
    let mut state = use_signal(QueueState::default);
    let mut focus = use_signal(|| PaneFocus::Elsewhere);
    let mut prompt = use_signal(|| Option::<QueuePrompt>::None);
    let mut undo = use_signal(|| Option::<TreeUndo>::None);
    let mut now_ms = use_signal(|| 0u64);

    let toast = use_callback(move |(kind, message): (crate::toast::ToastKind, String)| {
        if let Some(toasts) = toasts {
            crate::toast::push_toast(toasts, kind, message);
        }
    });

    let reload = use_callback({
        let api = api.clone();
        move |()| {
            let api = api.clone();
            loading.set(true);
            spawn(async move {
                match read_queue(&*api.reads).await {
                    Ok(v) => {
                        view.set(Some(v));
                        error.set(None);
                    }
                    Err(message) => error.set(Some(message)),
                }
                loading.set(false);
            });
        }
    });
    // The shell's palette asked for the first row that needs a decision: put the cursor on it and
    // the keyboard on the queue, once the queue has loaded.
    let needs_decision = try_use_context::<super::review_shell::NeedsDecisionRequest>();
    use_effect(move || {
        let Some(mut request) = needs_decision else {
            return;
        };
        if !*request.0.read() {
            return;
        }
        let key = view
            .read()
            .as_ref()
            .and_then(camerata_ui_core::queue::first_needing_decision);
        if let Some(key) = key {
            state.write().focus(&key);
            super::viewer::focus_element(QUEUE_REGION_ID);
        }
        // With no view yet the request waits for it; with a view and no such row there is
        // nothing to land on.
        if view.read().is_some() {
            request.0.set(false);
        }
    });
    // Read the queue now and whenever any lane writes (the shell's counter).
    use_effect(move || {
        if let Some(r) = refresh {
            let _ = r.0.read();
        }
        reload.call(());
    });

    let after_write = use_callback(move |()| {
        if let Some(mut r) = refresh {
            *r.0.write() += 1;
        }
    });

    // The undo clock: four ticks a second, only while an undo is armed.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if undo.peek().is_some() {
                now_ms.set(epoch.elapsed().as_millis() as u64);
            }
        }
    });

    let run_id = move || {
        view.peek()
            .as_ref()
            .map(|v| v.run_id.clone())
            .unwrap_or_default()
    };

    // The site detail: the open form, its undo bar and its writes. A write reads the queue and
    // the shell again like any other; a closed form gives the keyboard back to the queue.
    let detail = use_detail(
        after_write,
        Callback::new(move |()| super::viewer::focus_element(QUEUE_REGION_ID)),
    );
    // Another row under the cursor: the form that was open was for the one before.
    let cursor_now = use_memo(move || state.read().cursor.clone());
    use_effect(move || {
        let _ = cursor_now();
        detail.reset();
    });
    // The row under the cursor, with what the detail needs to know about its surroundings.
    let cursor_site = move || -> Option<(SiteRow, DetailContext)> {
        let key = state.peek().cursor.clone()?;
        let current = view.peek();
        let current = current.as_ref()?;
        let row = current
            .escalated
            .iter()
            .chain(&current.below)
            .find(|r| r.candidate.key == key)?;
        Some((row.site.clone(), detail_context(current, row)))
    };

    let decide = use_callback({
        let api = api.clone();
        move |(key, st, reason): (String, TriageState, String)| {
            let Some(decision) = decision_state_for(st) else {
                return;
            };
            let req = site_decision_request(&key, decision, &reason, &run_id());
            let api = api.clone();
            spawn(async move {
                match api.writes.post_decision(req).await {
                    Ok(done) => {
                        undo.set(Some(TreeUndo::for_site(
                            &done.decision.id,
                            st,
                            epoch.elapsed().as_millis() as u64,
                        )));
                        prompt.set(None);
                        super::viewer::focus_element(QUEUE_REGION_ID);
                        after_write.call(());
                    }
                    Err(e) => {
                        if let Some(p) = prompt.write().as_mut() {
                            p.error = Some(e.message);
                        } else {
                            toast.call((crate::toast::ToastKind::Error, e.message));
                        }
                    }
                }
            });
        }
    });

    // `C`, `I`, `F`, `T` or a pane button: apply now, or raise the reason prompt.
    let decide_cursor = use_callback(move |st: TriageState| {
        let Some(key) = state.peek().cursor.clone() else {
            return;
        };
        match PaneCommand::decide(st) {
            PaneCommand::PromptFor(st) => prompt.set(Some(QueuePrompt {
                key,
                state: st,
                reason: String::new(),
                error: None,
            })),
            _ => decide.call((key, st, String::new())),
        }
    });

    // `O` and the detail's Code button: the code viewer, reused from the flat Queue. Its
    // decisions are this lane's site decisions. The stacked view is the tree's (it stacks a whole
    // sub-group, and the queue holds only the escalated rows of one), so Stack goes there.
    let loaded_sites = use_memo(move || {
        view.read()
            .as_ref()
            .map(|v| {
                v.escalated
                    .iter()
                    .chain(&v.below)
                    .map(|r| {
                        let node = r.node_path.last().map(|c| c.id.clone()).unwrap_or_default();
                        (node, r.site.clone())
                    })
                    .collect::<Vec<(String, SiteRow)>>()
            })
            .unwrap_or_default()
    });
    let project = try_use_context::<camerata_ui::review_io::ReviewProject>()
        .map(|p| p.0)
        .unwrap_or_default();
    let bridge = use_code_bridge(
        loaded_sites,
        audited_shas,
        project,
        use_callback(
            move |(_node, key, st, reason): (String, String, TriageState, String)| {
                decide.call((key, st, reason));
            },
        ),
        use_callback(move |(_node, key): (String, String)| state.write().focus(&key)),
        QUEUE_REGION_ID,
    );

    let commit = use_callback(move |()| {
        let Some(p) = prompt.peek().clone() else {
            return;
        };
        if p.reason.trim().is_empty() {
            prompt.set(Some(QueuePrompt {
                error: Some(format!(
                    "A reason is required to move 1 site to \u{201c}{}\u{201d}.",
                    p.state.label()
                )),
                ..p
            }));
        } else {
            decide.call((p.key, p.state, p.reason));
        }
    });

    let find_row = move |key: &str| -> Option<QueueRow> {
        view.peek()
            .as_ref()?
            .escalated
            .iter()
            .chain(view.peek().as_ref()?.below.iter())
            .find(|r| r.candidate.key == key)
            .cloned()
    };

    let needed_me = use_callback({
        let api = api.clone();
        move |key: String| {
            let Some(row) = find_row(&key) else {
                return;
            };
            let Some((decision_id, value)) = needed_me_write(&row) else {
                toast.call((crate::toast::ToastKind::Info, NEEDED_ME_HINT.to_string()));
                return;
            };
            let api = api.clone();
            spawn(async move {
                match api.writes.set_needed_me(&decision_id, value).await {
                    Ok(_) => after_write.call(()),
                    Err(e) => toast.call((crate::toast::ToastKind::Error, e.message)),
                }
            });
        }
    });

    // A row's site or node in the tree: ask the shell, which switches the lane.
    let open_door = use_callback(move |d: Door| {
        if let Some(mut door) = door {
            door.0.set(Some(d));
        } else if let Some(mut lanes) = lanes {
            lanes.0.set(Lane::Tree);
        }
    });
    let open_site = use_callback(move |key: String| {
        if let Some(row) = find_row(&key) {
            open_door.call(site_door(&row));
        }
    });
    let open_node = use_callback(move |id: String| {
        open_door.call(Door::Node { id });
    });

    let more_below = use_callback({
        let api = api.clone();
        move |()| {
            let Some(current) = view.peek().clone() else {
                return;
            };
            reading_more.set(true);
            let api = api.clone();
            spawn(async move {
                match read_more_below(&*api.reads, &current).await {
                    Ok(v) => view.set(Some(v)),
                    Err(message) => toast.call((crate::toast::ToastKind::Error, message)),
                }
                reading_more.set(false);
            });
        }
    });

    let on_key = use_callback(move |(key, mods): (String, KeyMods)| {
        // `1` to `5` switch lanes while the queue's own region has focus (no text field does).
        if *focus.peek() == PaneFocus::Cursor {
            if let Some(mut lanes) = lanes {
                let claimed = mods.ctrl || mods.alt || mods.meta;
                if let Some(next) = lane_for_key(&key, claimed, false) {
                    lanes.0.set(next);
                    return;
                }
            }
        }
        let on_row = state.peek().cursor.is_some();
        let Some(cmd) = queue_command(*focus.peek(), &key, mods, on_row) else {
            return;
        };
        let keys: Vec<String> = view
            .peek()
            .as_ref()
            .map(|v| {
                layout(v, &state.peek())
                    .keys()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        match cmd {
            QueueCommand::Move(dir) => state.write().move_cursor(&key_refs, dir),
            QueueCommand::ShowMore => {
                let at_last = state.peek().cursor.as_deref() == key_refs.last().copied();
                if at_last {
                    let more_rows = view
                        .peek()
                        .as_ref()
                        .is_some_and(|v| layout(v, &state.peek()).hidden_escalated > 0);
                    if more_rows {
                        state.write().show_more();
                    } else if state.peek().show_below {
                        more_below.call(());
                    }
                } else {
                    state.write().move_cursor(&key_refs, FocusMove::Down);
                }
            }
            QueueCommand::Open => {
                if let Some(key) = state.peek().cursor.clone() {
                    open_site.call(key);
                }
            }
            QueueCommand::GoToNode => {
                if let Some(key) = state.peek().cursor.clone() {
                    if let Some(row) = find_row(&key) {
                        if let Some(d) = node_door(&row) {
                            open_door.call(d);
                        }
                    }
                }
            }
            QueueCommand::OpenSettings => {
                if let Some(mut lanes) = lanes {
                    lanes.0.set(Lane::Settings);
                }
            }
            QueueCommand::ToggleNeededMe => {
                if let Some(key) = state.peek().cursor.clone() {
                    needed_me.call(key);
                }
            }
            QueueCommand::Site(PaneCommand::Dispose(st) | PaneCommand::PromptFor(st)) => {
                decide_cursor.call(st);
            }
            QueueCommand::Site(_) => {}
        }
    });

    let on_undo = use_callback({
        let api = api.clone();
        move |()| {
            let Some(entry) = undo.peek().clone() else {
                return;
            };
            undo.set(None);
            let api = api.clone();
            spawn(async move {
                match api.writes.revoke_decision(&entry.decision_id).await {
                    Ok(_) => after_write.call(()),
                    Err(e) => toast.call((crate::toast::ToastKind::Error, e.message)),
                }
            });
        }
    });

    let status = if let Some(message) = error.read().clone() {
        QueueStatus::Failed(message)
    } else if *loading.read() && view.read().is_none() {
        QueueStatus::Loading
    } else {
        QueueStatus::Ready
    };
    let banner = undo.read().as_ref().and_then(|u| {
        u.seconds_left(now_ms()).map(|seconds_left| UndoBanner {
            message: u.message.clone(),
            seconds_left,
        })
    });
    let applied_text = applied
        .and_then(|a| a.0.read().clone())
        .map(|outcome| rate_changed_text(&outcome));

    rsx! {
        QueueScreen {
            view: view.read().clone(),
            status,
            state: state.read().clone(),
            focus: focus(),
            prompt: prompt.read().clone(),
            undo: banner,
            toast: applied_text,
            reading_more: reading_more(),
            on_key,
            on_focus: move |f: PaneFocus| {
                if let Some(next) = camerata_ui_core::triage::next_pane_focus(f, prompt.peek().is_some()) {
                    focus.set(next);
                }
            },
            on_focus_row: move |key: String| state.write().focus(&key),
            on_toggle_below: move |()| state.write().toggle_below(),
            on_more: move |()| state.write().show_more(),
            on_more_below: more_below,
            on_needed_me: needed_me,
            on_open_site: open_site,
            on_open_node: open_node,
            on_decide: decide_cursor,
            on_settings: move |()| {
                if let Some(mut lanes) = lanes {
                    lanes.0.set(Lane::Settings);
                }
            },
            on_prompt_reason: move |text: String| {
                if let Some(p) = prompt.write().as_mut() {
                    p.reason = text;
                }
            },
            on_prompt_commit: move |()| commit.call(()),
            on_prompt_cancel: move |()| {
                prompt.set(None);
                super::viewer::focus_element(QUEUE_REGION_ID);
            },
            on_undo,
            on_dismiss_toast: move |()| {
                if let Some(mut a) = applied {
                    a.0.set(None);
                }
            },
            on_retry: move |()| reload.call(()),
            detail: detail.panel(),
            on_detail: move |event: DetailEvent| match event {
                // The lane's own: a decision and the code viewer.
                DetailEvent::Decide(st) => decide_cursor.call(st),
                DetailEvent::OpenCode => {
                    if let Some(key) = state.peek().cursor.clone() {
                        let node = loaded_sites
                            .peek()
                            .iter()
                            .find(|(_, s)| s.site_key == key)
                            .map(|(n, _)| n.clone())
                            .unwrap_or_default();
                        bridge.open_code.call((node, key));
                    }
                }
                // There is no batch in the queue to tick an outlier into.
                DetailEvent::Tick => {}
                other => {
                    if let Some((row, ctx)) = cursor_site() {
                        detail.handle(other, &row, &ctx, &run_id());
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests;
