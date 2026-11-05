//! The site detail (review UI spec section 8): the right pane for a focused site, in every lane.
//!
//! Two layers, per spec rule 1:
//!
//! - [`SiteDetailView`] is props-only. Every judgment it draws (the chips, which fields carry an
//!   edited or stale mark, the decision line, the client line, what a form refuses) comes from
//!   `camerata_ui_core::edits`; it chooses words and pixels. The gallery renders it from fixtures
//!   and the tests render it from props.
//! - [`DetailCtl`] (from [`use_detail`]) owns the signals and the I/O: the open form, the undo
//!   bar, the notice, and the posts. A lane calls `use_detail` once, hands the panel it reads to
//!   the view and forwards the view's [`DetailEvent`]s back to `handle`. The detail never reads
//!   a site itself: the row comes with the lane.
//!
//! # What it keeps from the tree's old read-only pane
//!
//! The decision buttons, the outlier tick and the reason prompt of a site decision are the lane's
//! (the lane owns the decision keys and the undo of a decision); the detail draws the buttons,
//! reports a press as [`DetailEvent::Decide`] and draws the lane's prompt as its `children`.

use camerata_api_types::decisions::Decision;
use camerata_api_types::edits::{Edit, EditField, FindingAdded};
use camerata_api_types::review_view::SiteRow;
use camerata_ui_core::edits::{
    ack_detail, ack_line, ack_state, added_message, added_outcome, chord_hint, chord_letter,
    commit, control_for, decision_line, duplicate_line, edit_tip, field_label, field_state,
    flagged_by_line, head_chips, held_line, is_thrown_out, merge_candidate_total, merge_candidates,
    merge_shown_line, reapply_decision_request, reapply_done_message, reduce, review_level_text,
    severity_note, snippet_at_line, split_choice_from_value, split_option_value, stage1_line,
    stage2_line, stale_edit_line, stale_edit_tip, AckState, AddDraft, AddField, AddedOutcome,
    Control, DetailAction, DetailContext, DetailMode, DetailUndo, DetailWrite, FieldState,
    LineAction, LineTone, Snippet, SplitChoice, ThrowOutChoice, UndoAction, SPLIT_NEW_VALUE,
};
use camerata_ui_core::tree::{file_line, is_outlier, kind_tag_of, severity_badge, severity_label};
use camerata_ui_core::triage::TriageState;
use dioxus::prelude::*;

use super::scan::kind_chip_parts;
use camerata_ui::review_io::{ReviewApi, ReviewWrites};

/// The DOM id of the open form, for a test or a screen reader to find it.
pub(crate) const DETAIL_FORM_ID: &str = "site-detail-form";

// ── What the view reports ─────────────────────────────────────────────────────────────────

/// Something the reviewer did in the detail. The lane forwards these to [`DetailCtl::handle`],
/// except the ones that are the lane's own (a decision, a tick, the viewer, the stack).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DetailEvent {
    /// Something typed or picked in the open form, or a form opened.
    Form(DetailAction),
    /// Enter in the form, or its Commit button.
    Commit,
    /// Esc in the form, or its Cancel button.
    Cancel,
    /// A decision button: the lane decides.
    Decide(TriageState),
    /// The outlier checkbox: the lane ticks.
    Tick,
    /// Reapply or Discard beside a stale edit, by the edit's id.
    StaleEdit(String, LineAction),
    /// Reapply or Discard beside a stale decision.
    StaleDecision(LineAction),
    /// The undo bar's button.
    Undo,
    /// The Code button (`O`): the lane opens the viewer.
    OpenCode,
    /// The Stack button (`B`): the lane opens the stacked view.
    OpenStack,
    /// The add form's "Load code" button.
    LoadSnippet,
}

/// The undo bar as drawn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DetailBanner {
    pub message: String,
    pub button: &'static str,
    pub seconds_left: u64,
}

/// A line under the head saying what just happened or what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailNotice {
    /// Drawn as a warning (the evidence check held an added finding; a write was refused).
    pub warn: bool,
    pub text: String,
}

/// What the pane shows besides the site: the open form, the undo bar and the notice.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct DetailPanel {
    pub mode: DetailMode,
    pub undo: Option<DetailBanner>,
    pub notice: Option<DetailNotice>,
}

// ── The empty state ───────────────────────────────────────────────────────────────────────

/// Nothing is focused.
#[component]
pub(crate) fn SiteDetailEmpty() -> Element {
    rsx! {
        div { class: "site-detail",
            p { class: "pane-empty", "Select a site" }
        }
    }
}

// ── The view ──────────────────────────────────────────────────────────────────────────────

/// The focused site, with every line the spec draws and the forms.
#[component]
pub(crate) fn SiteDetailView(
    row: SiteRow,
    ctx: DetailContext,
    /// `7 of 12`.
    position: String,
    panel: DetailPanel,
    /// The outlier is ticked into the next batch decision.
    ticked: bool,
    /// The lane has a batch decision to tick an outlier into. The tree does; the queue decides
    /// one site at a time, so it asks for no tick.
    #[props(default = true)]
    batch_tick: bool,
    /// The lane has a stacked view of the sub-group to open (B). The tree does; the queue's
    /// rows are not a sub-group, so it asks for no Stack button.
    #[props(default = true)]
    stack: bool,
    #[props(default)] on_event: Callback<DetailEvent>,
    /// The lane's reason prompt for a site decision, drawn above the buttons.
    children: Element,
) -> Element {
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(kind_tag_of(&row));
    let chips = head_chips(&row);
    let line = decision_line(&row, &ctx);
    let level = review_level_text(row.review_level.as_ref());
    let held = held_line(&row);
    let crumb = ctx.crumbs.join(" > ");
    let outlier = is_outlier(&row);
    let ack = ack_line(&row);
    let ack_who = ack_detail(&row);
    let needs_ack = matches!(ack_state(&row), AckState::Needed { .. });
    let stage1 = stage1_line(&row);
    let stage2 = stage2_line(&row);
    let flagged = flagged_by_line(&row);
    let duplicate = duplicate_line(&row);
    let sev_note = severity_note(&row);
    let mode = panel.mode.clone();
    let picking = mode == DetailMode::PickField;
    let editing = |field: EditField| matches!(&mode, DetailMode::Edit(d) if d.field == field);
    let headline = field_state(&row, EditField::Headline, &ctx);
    let severity = field_state(&row, EditField::Severity, &ctx);
    let detail = field_state(&row, EditField::Detail, &ctx);
    let fix = field_state(&row, EditField::Fix, &ctx);
    let fix_type = field_state(&row, EditField::FixType, &ctx);
    let meta: Vec<FieldState> = [
        EditField::RuleId,
        EditField::Kind,
        EditField::Category,
        EditField::Subgroup,
    ]
    .into_iter()
    .map(|f| field_state(&row, f, &ctx))
    .collect();
    let has_form = |f: EditField| editing(f);
    // A row the engine threw out is in no package. The server takes only two decisions on it, so
    // the pane offers only those two and none it would refuse.
    let thrown_out = is_thrown_out(&row);
    rsx! {
        div { class: "site-detail",
            div { class: "site-detail-head",
                span { class: "pane-rule", "{row.rule_id}" }
                span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
                for chip in chips {
                    span { key: "{chip.class}-{chip.label}", class: "{chip.class}", title: "{chip.tip}", "{chip.label}" }
                }
                span { class: "pane-pos", "{position}" }
                span { class: "site-detail-doors",
                    button {
                        class: "btn-edit-sm site-detail-code",
                        title: "Open the code viewer (O)",
                        onclick: move |_| on_event.call(DetailEvent::OpenCode),
                        "Code"
                    }
                    if stack {
                        button {
                            class: "btn-edit-sm site-detail-stack",
                            title: "Open the stacked view of this sub-group (B)",
                            onclick: move |_| on_event.call(DetailEvent::OpenStack),
                            "Stack"
                        }
                    }
                }
            }
            div { class: "site-detail-crumb",
                span { "{crumb}" }
                if let Some(level) = level {
                    span { class: "site-detail-level", "{level}" }
                }
            }
            if let Some(held) = held {
                p { class: "site-detail-held", role: "alert", "{held}" }
            }
            if let Some(reason) = row.thrown_out_reason.clone() {
                p { class: "site-detail-line site-detail-thrown",
                    span { class: "site-detail-label", "Thrown out" }
                    span { class: "site-detail-value", "{reason}" }
                }
            }
            if let Some(banner) = panel.undo.clone() {
                div { class: "pane-undo site-detail-undo",
                    span { class: "pane-undo-msg", "{banner.message}" }
                    button {
                        class: "btn-edit-sm pane-undo-btn",
                        onclick: move |_| on_event.call(DetailEvent::Undo),
                        "{banner.button}"
                    }
                    span { class: "pane-undo-left", "{banner.seconds_left}s" }
                }
            }
            if let Some(notice) = panel.notice.clone() {
                p {
                    class: if notice.warn { "site-detail-notice warn" } else { "site-detail-notice" },
                    role: if notice.warn { "alert" } else { "status" },
                    "{notice.text}"
                }
            }

            FieldRow { state: headline, on_event }
            if has_form(EditField::Headline) {
                EditFormFor { field: EditField::Headline, row: row.clone(), ctx: ctx.clone(), mode: mode.clone(), on_event }
            }
            FieldRow { state: severity, note: sev_note, badge: Some(row.severity.clone()), on_event }
            if has_form(EditField::Severity) {
                EditFormFor { field: EditField::Severity, row: row.clone(), ctx: ctx.clone(), mode: mode.clone(), on_event }
            }
            FieldRow { state: detail, multiline: true, on_event }
            if has_form(EditField::Detail) {
                EditFormFor { field: EditField::Detail, row: row.clone(), ctx: ctx.clone(), mode: mode.clone(), on_event }
            }
            FieldRow { state: fix, multiline: true, on_event }
            if has_form(EditField::Fix) {
                EditFormFor { field: EditField::Fix, row: row.clone(), ctx: ctx.clone(), mode: mode.clone(), on_event }
            }
            FieldRow { state: fix_type, on_event }
            if has_form(EditField::FixType) {
                EditFormFor { field: EditField::FixType, row: row.clone(), ctx: ctx.clone(), mode: mode.clone(), on_event }
            }
            div { class: "site-detail-meta",
                for state in meta {
                    FieldRow { key: "{state.label}", state, compact: true, on_event }
                }
            }
            for field in [EditField::RuleId, EditField::Kind, EditField::Category] {
                if has_form(field) {
                    EditFormFor { key: "{field_label(field)}", field, row: row.clone(), ctx: ctx.clone(), mode: mode.clone(), on_event }
                }
            }
            if let DetailMode::Split(draft) = mode.clone() {
                SplitForm { draft, ctx: ctx.clone(), on_event }
            }

            p { class: "site-detail-line site-detail-where",
                span { class: "site-detail-label", "Where" }
                span { class: "site-detail-value site-pane-code", "{row.repo} {file_line(&row)}" }
            }
            if !row.snippet.is_empty() {
                pre { class: "pane-snippet", "{row.snippet}" }
            }
            if let Some(text) = stage1 {
                p { class: "site-detail-line",
                    span { class: "site-detail-label", "Stage 1" }
                    span { class: "site-detail-value", "{text}" }
                }
            }
            if let Some(text) = stage2 {
                p { class: "site-detail-line",
                    span { class: "site-detail-label", "Stage 2" }
                    span { class: "site-detail-value", "{text}" }
                }
            }
            if let Some(text) = flagged {
                p { class: "site-detail-line",
                    span { class: "site-detail-label", "Also flagged by" }
                    span { class: "site-detail-value", "{text}" }
                }
            }
            if let Some((place, hint)) = duplicate {
                p { class: "site-detail-line site-detail-dup",
                    span { class: "site-detail-label", "Possible duplicate of" }
                    span { class: "site-detail-value", title: "{hint}", "{place}" }
                    if !thrown_out {
                    button {
                        class: "btn-edit-sm",
                        title: "Merge this row into the possible duplicate (M)",
                        onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginMerge)),
                        "merge into"
                    }
                    }
                }
            }
            if !row.review_flags.is_empty() {
                p { class: "site-detail-line site-detail-flags",
                    span { class: "site-detail-label", "Wording flags" }
                    span { class: "site-detail-value", "{row.review_flags.join(\", \")}" }
                }
            }

            p { class: "site-detail-line site-detail-decision",
                span { class: "site-detail-label", "Decision" }
                span {
                    class: match line.tone {
                        LineTone::Decided => "dec-decided",
                        LineTone::Unresolved => "dec-unresolved",
                        LineTone::Warning => "dec-stale",
                    },
                    "{line.text}"
                }
                if line.inherited { span { class: "dec-inherited", "inherited from its group" } }
                for action in line.actions.clone() {
                    button {
                        key: "{action.label()}",
                        class: "btn-edit-sm site-detail-line-action",
                        onclick: move |_| on_event.call(DetailEvent::StaleDecision(action)),
                        "{action.label()}"
                    }
                }
            }
            if let DetailMode::Merge(draft) = mode.clone() {
                MergeForm { draft, row: row.clone(), ctx: ctx.clone(), on_event }
            }
            if let Some(text) = ack {
                p { class: "site-detail-line site-detail-client",
                    span { class: "site-detail-label", "Client" }
                    span { class: "site-detail-value", "{text}" }
                    if needs_ack {
                        button {
                            class: "btn-edit-sm",
                            onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginAck)),
                            "record acknowledgement"
                        }
                    }
                }
                if let Some(who) = ack_who {
                    p { class: "site-detail-line site-detail-client-who",
                        span { class: "site-detail-label", "" }
                        span { class: "site-detail-value", "{who}" }
                    }
                }
            }
            if let DetailMode::Ack(draft) = mode.clone() {
                AckForm { draft, on_event }
            }
            if outlier && batch_tick {
                label { class: "site-detail-line",
                    input {
                        r#type: "checkbox",
                        checked: ticked,
                        onchange: move |_| on_event.call(DetailEvent::Tick),
                    }
                    " Include this outlier in the next batch decision"
                }
            }
            if let DetailMode::Add(draft) = mode.clone() {
                AddForm { draft: *draft, ctx: ctx.clone(), on_event }
            }
            if let DetailMode::ThrowOut(draft) = mode.clone() {
                ThrowOutForm { draft, on_event }
            }

            {children}

            if thrown_out {
                p { class: "site-detail-line site-detail-thrown-out", role: "status",
                    span { class: "site-detail-label", "Next" }
                    span { class: "site-detail-value",
                        "It is in no package. Put it back into review to rule on it, or confirm it was rightly thrown out."
                    }
                }
                div { class: "pane-actions site-detail-actions",
                    for choice in [ThrowOutChoice::PutBack, ThrowOutChoice::Confirm] {
                        button {
                            key: "{choice.label()}",
                            class: "btn-edit-sm site-detail-throw-out",
                            onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginThrowOut(choice))),
                            "{choice.label()}"
                        }
                    }
                    button {
                        class: "btn-edit-sm site-detail-add",
                        title: "Add a finding the engine missed (A)",
                        onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginAdd)),
                        "Add finding"
                    }
                }
            } else {
            div { class: "pane-actions site-detail-actions",
                    for state in [TriageState::Confirmed, TriageState::Ignored, TriageState::TechDebt, TriageState::FalsePositive] {
                        button {
                            key: "{state.label()}",
                            class: "btn-edit-sm pane-dispose",
                            onclick: move |_| on_event.call(DetailEvent::Decide(state)),
                            "{state.label()}"
                        }
                    }
                    button {
                        class: "btn-edit-sm site-detail-merge",
                        title: "Merge this row into another site (M)",
                        onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginMerge)),
                        "Merge into"
                    }
                    button {
                        class: "btn-edit-sm site-detail-add",
                        title: "Add a finding the engine missed (A)",
                        onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginAdd)),
                        "Add finding"
                    }
                }
            }
            if picking {
                p { class: "site-detail-chord", role: "status", "{chord_hint()}" }
            }
        }
    }
}

/// A field line: the label, the value with its marks, and the edit button. A stale edit of the
/// field is a second line under it with Reapply and Discard.
#[component]
fn FieldRow(
    state: FieldState,
    /// Words after the value (the severity's stage disagreement).
    #[props(default)]
    note: Option<String>,
    /// A severity badge to draw in front of the value.
    #[props(default)]
    badge: Option<String>,
    #[props(default)] multiline: bool,
    /// Drawn as a small item of the meta line.
    #[props(default)]
    compact: bool,
    on_event: Callback<DetailEvent>,
) -> Element {
    let field = state.field;
    let letter = chord_letter(field).map_or(String::new(), |c| format!(" (E then {c})"));
    let shown = if state.value.is_empty() {
        "(none)".to_string()
    } else {
        state.value.clone()
    };
    let class = match (compact, multiline) {
        (true, _) => "site-detail-item",
        (false, true) => "site-detail-line site-detail-multi",
        (false, false) => "site-detail-line",
    };
    let applied = state.applied.clone();
    let stale = state.stale.clone();
    rsx! {
        div { class: "{class}",
            span { class: "site-detail-label", "{state.label}" }
            span { class: "site-detail-value",
                if let Some(badge) = badge {
                    span { class: "tree-badge tree-badge-{severity_badge(&badge)}", "{severity_label(&badge)}" }
                } else {
                    "{shown}"
                }
                if let Some(note) = note {
                    span { class: "site-detail-note", " ({note})" }
                }
            }
            if let Some(edit) = applied {
                span { class: "mark-edited", title: "{edit_tip(&edit)}", "\u{270e} edited" }
            }
            if let Some(edit) = stale.as_ref() {
                span { class: "mark-stale", title: "{stale_edit_tip(edit)}", "stale edit" }
            }
            button {
                class: "btn-edit-sm site-detail-edit",
                title: "Edit {state.label.to_lowercase()}{letter}",
                onclick: move |_| on_event.call(DetailEvent::Form(DetailAction::BeginEdit(field))),
                "edit"
            }
        }
        if let Some(edit) = stale {
            div { class: "site-detail-stale",
                span { class: "site-detail-stale-text", "{stale_edit_line(&edit)}" }
                for action in [LineAction::Reapply, LineAction::Discard] {
                    button {
                        key: "{action.label()}",
                        class: "btn-edit-sm site-detail-line-action",
                        onclick: {
                            let id = edit.id.clone();
                            move |_| on_event.call(DetailEvent::StaleEdit(id.clone(), action))
                        },
                        "{action.label()}"
                    }
                }
            }
        }
    }
}

// ── Forms ─────────────────────────────────────────────────────────────────────────────────

/// The form of one field edit, when the open draft is for it.
#[component]
fn EditFormFor(
    field: EditField,
    row: SiteRow,
    ctx: DetailContext,
    mode: DetailMode,
    on_event: Callback<DetailEvent>,
) -> Element {
    let DetailMode::Edit(draft) = mode else {
        return rsx! {};
    };
    if draft.field != field {
        return rsx! {};
    }
    let control = control_for(field, &row, &ctx);
    let title = format!("Edit the {}", field_label(field).to_lowercase());
    rsx! {
        div { class: "queue-reason-prompt site-detail-form", id: "{DETAIL_FORM_ID}",
            label { class: "rule-modal-label", "{title}" }
            ValueControl {
                control,
                value: draft.value.clone(),
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Text(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            label { class: "rule-modal-label", "Why the engine's value was wrong (required)" }
            ReasonInput {
                value: draft.reason.clone(),
                placeholder: "why (recorded with the edit)",
                focus: false,
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Reason(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            FormFooter { error: draft.error.clone(), commit_label: "Commit", on_event }
        }
    }
}

/// The control a field takes.
#[component]
fn ValueControl(
    control: Control,
    value: String,
    on_text: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    match control {
        Control::Line => rsx! {
            input {
                class: "addressee-input site-detail-input",
                r#type: "text",
                value: "{value}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_text.call(e.value()),
                onkeydown: move |e: KeyboardEvent| key_in_field(&e, false, on_commit, on_cancel),
            }
        },
        Control::Multi => rsx! {
            textarea {
                class: "addressee-input site-detail-input",
                rows: "4",
                value: "{value}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_text.call(e.value()),
                onkeydown: move |e: KeyboardEvent| key_in_field(&e, true, on_commit, on_cancel),
                // The text is also the textarea's content, so a first paint (and the gallery
                // page) shows it without a script setting the property.
                "{value}"
            }
        },
        Control::Select(options) => rsx! {
            select {
                class: "addressee-input site-detail-input",
                value: "{value}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                onchange: move |e| on_text.call(e.value()),
                onkeydown: move |e: KeyboardEvent| key_in_field(&e, false, on_commit, on_cancel),
                for o in options {
                    option { key: "{o.value}", value: "{o.value}", selected: o.value == value, "{o.label}" }
                }
            }
        },
        Control::Combo(options) => rsx! {
            input {
                class: "addressee-input site-detail-input",
                r#type: "text",
                list: "site-detail-options",
                value: "{value}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_text.call(e.value()),
                onkeydown: move |e: KeyboardEvent| key_in_field(&e, false, on_commit, on_cancel),
            }
            datalist { id: "site-detail-options",
                for o in options {
                    option { key: "{o.value}", value: "{o.value}" }
                }
            }
        },
        Control::Split | Control::NodeSummary => rsx! {},
    }
}

/// Enter commits (a multi-line field needs Cmd or Ctrl with it), Esc cancels, and no other key
/// leaves the field: a letter typed here is never read as a decision.
fn key_in_field(e: &KeyboardEvent, multiline: bool, commit: Callback<()>, cancel: Callback<()>) {
    e.stop_propagation();
    match e.key() {
        Key::Escape => cancel.call(()),
        Key::Enter => {
            let m = e.modifiers();
            let with_command = m.contains(Modifiers::CONTROL) || m.contains(Modifiers::META);
            if !multiline || with_command {
                e.prevent_default();
                commit.call(());
            }
        }
        _ => {}
    }
}

/// The reason field: Enter commits, Esc cancels.
#[component]
fn ReasonInput(
    value: String,
    placeholder: String,
    focus: bool,
    on_text: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    rsx! {
        textarea {
            class: "addressee-input queue-reason-input",
            rows: "2",
            placeholder: "{placeholder}",
            value: "{value}",
            onmounted: move |e| {
                if focus {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                }
            },
            oninput: move |e| on_text.call(e.value()),
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
            "{value}"
        }
    }
}

/// The error line and the Commit and Cancel buttons of a form.
#[component]
fn FormFooter(
    error: Option<String>,
    commit_label: &'static str,
    #[props(default)] commit_disabled: bool,
    on_event: Callback<DetailEvent>,
) -> Element {
    rsx! {
        if let Some(error) = error {
            p { class: "batch-error site-detail-error", role: "alert", "{error}" }
        }
        div { class: "queue-prompt-actions",
            button {
                class: "btn-run queue-prompt-commit",
                disabled: commit_disabled,
                onclick: move |_| on_event.call(DetailEvent::Commit),
                "{commit_label} \u{2192}"
            }
            button {
                class: "btn-edit-sm queue-prompt-cancel",
                onclick: move |_| on_event.call(DetailEvent::Cancel),
                "Cancel"
            }
        }
    }
}

/// The merge picker: the possible duplicate first, then a search over `path:line` and rule.
#[component]
fn MergeForm(
    draft: camerata_ui_core::edits::MergeDraft,
    row: SiteRow,
    ctx: DetailContext,
    on_event: Callback<DetailEvent>,
) -> Element {
    let candidates = merge_candidates(&row, &ctx, &draft.query);
    let cut_short = merge_shown_line(
        candidates.len(),
        merge_candidate_total(&row, &ctx, &draft.query),
    );
    let picked = draft.target.clone();
    rsx! {
        div { class: "queue-reason-prompt site-detail-form site-detail-merge-form", id: "{DETAIL_FORM_ID}",
            label { class: "rule-modal-label", "Merge this site into another (the row stays in the tree)" }
            input {
                class: "addressee-input site-detail-input",
                r#type: "text",
                placeholder: "search by path:line or rule",
                value: "{draft.query}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_event.call(DetailEvent::Form(DetailAction::Text(e.value()))),
                onkeydown: move |e: KeyboardEvent| {
                    e.stop_propagation();
                    if e.key() == Key::Escape {
                        on_event.call(DetailEvent::Cancel);
                    }
                },
            }
            ul { class: "site-detail-candidates", role: "listbox",
                if candidates.is_empty() {
                    li { class: "site-detail-candidate-none", "{ctx.pool_none}" }
                }
                for c in candidates {
                    li {
                        key: "{camerata_ui_core::tree::dom_token(&c.key)}",
                        class: if picked.as_deref() == Some(c.key.as_str()) { "site-detail-candidate picked" } else { "site-detail-candidate" },
                        role: "option",
                        "aria-selected": "{picked.as_deref() == Some(c.key.as_str())}",
                        onclick: {
                            let key = c.key.clone();
                            move |_| on_event.call(DetailEvent::Form(DetailAction::Pick(key.clone())))
                        },
                        span { class: "site-pane-code", "{c.label}" }
                        span { class: "site-detail-candidate-rule", "{c.rule_id}" }
                        if let Some(note) = c.note.clone() {
                            span { class: "mark-dup", "{note}" }
                        }
                    }
                }
            }
            if let Some(line) = cut_short {
                p { class: "site-detail-candidate-more", "{line}" }
            }
            label { class: "rule-modal-label", "Why these are one problem (required)" }
            ReasonInput {
                value: draft.reason.clone(),
                placeholder: "why (the export reads this)",
                focus: false,
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Reason(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            FormFooter { error: draft.error.clone(), commit_label: "Merge", on_event }
        }
    }
}

/// The split picker: a select of the rule's sub-groups plus a new one with its label.
#[component]
fn SplitForm(
    draft: camerata_ui_core::edits::SplitDraft,
    ctx: DetailContext,
    on_event: Callback<DetailEvent>,
) -> Element {
    let current = split_option_value(&ctx.subgroups, &draft.choice);
    let is_new = draft.choice == SplitChoice::New;
    let subgroups = ctx.subgroups.clone();
    rsx! {
        div { class: "queue-reason-prompt site-detail-form site-detail-split-form", id: "{DETAIL_FORM_ID}",
            label { class: "rule-modal-label", "Move this site to a sub-group" }
            select {
                class: "addressee-input site-detail-input",
                value: "{current}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                onchange: move |e| {
                    if let Some(choice) = split_choice_from_value(&subgroups, &e.value()) {
                        on_event.call(DetailEvent::Form(DetailAction::Split(choice)));
                    }
                },
                onkeydown: move |e: KeyboardEvent| {
                    e.stop_propagation();
                    if e.key() == Key::Escape {
                        on_event.call(DetailEvent::Cancel);
                    }
                },
                for (i, label) in ctx.subgroups.clone().into_iter().enumerate() {
                    option { key: "{i}", value: "{i}", selected: i.to_string() == current, "{label}" }
                }
                option { value: "{SPLIT_NEW_VALUE}", selected: is_new, "New sub-group" }
            }
            if is_new {
                label { class: "rule-modal-label", "Label of the new sub-group (it is also its cause)" }
                input {
                    class: "addressee-input site-detail-input",
                    r#type: "text",
                    placeholder: "for example: test fixtures",
                    value: "{draft.new_label}",
                    oninput: move |e| on_event.call(DetailEvent::Form(DetailAction::Text(e.value()))),
                    onkeydown: move |e: KeyboardEvent| {
                        e.stop_propagation();
                        if e.key() == Key::Escape {
                            on_event.call(DetailEvent::Cancel);
                        }
                    },
                }
            }
            label { class: "rule-modal-label", "Why it does not belong with the others (required)" }
            ReasonInput {
                value: draft.reason.clone(),
                placeholder: "why (recorded with the edit)",
                focus: false,
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Reason(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            FormFooter { error: draft.error.clone(), commit_label: "Move", on_event }
        }
    }
}

/// The client acknowledgement form: who, and an optional note.
#[component]
fn AckForm(draft: camerata_ui_core::edits::AckDraft, on_event: Callback<DetailEvent>) -> Element {
    rsx! {
        div { class: "queue-reason-prompt site-detail-form site-detail-ack-form", id: "{DETAIL_FORM_ID}",
            label { class: "rule-modal-label", "Who at the client acknowledged this accepted risk (required)" }
            input {
                class: "addressee-input site-detail-input",
                r#type: "text",
                placeholder: "name or email",
                value: "{draft.by}",
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_event.call(DetailEvent::Form(DetailAction::Text(e.value()))),
                onkeydown: move |e: KeyboardEvent| key_in_field(&e, false, Callback::new(move |()| on_event.call(DetailEvent::Commit)), Callback::new(move |()| on_event.call(DetailEvent::Cancel))),
            }
            label { class: "rule-modal-label", "Note (optional)" }
            ReasonInput {
                value: draft.note.clone(),
                placeholder: "for example: confirmed in the 11-05 call",
                focus: false,
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Reason(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            FormFooter { error: draft.error.clone(), commit_label: "Record", on_event }
        }
    }
}

/// The put-back or confirm form of a thrown-out row: a required reason, then the one decision.
#[component]
fn ThrowOutForm(
    draft: camerata_ui_core::edits::ThrowOutDraft,
    on_event: Callback<DetailEvent>,
) -> Element {
    let (title, placeholder) = match draft.choice {
        ThrowOutChoice::PutBack => (
            "Why this row is a real problem the engine should not have thrown out (required)",
            "why (the row returns to review, held until you decide it)",
        ),
        ThrowOutChoice::Confirm => (
            "Why the engine was right to throw this row out (required)",
            "why (the row stays out of the client package)",
        ),
    };
    rsx! {
        div { class: "queue-reason-prompt site-detail-form site-detail-throw-out-form", id: "{DETAIL_FORM_ID}",
            label { class: "rule-modal-label", "{title}" }
            ReasonInput {
                value: draft.reason.clone(),
                placeholder,
                focus: true,
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Reason(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            FormFooter { error: draft.error.clone(), commit_label: draft.choice.label(), on_event }
        }
    }
}

/// The add form: where, what, why, and the code at that line, read-only.
#[component]
fn AddForm(draft: AddDraft, ctx: DetailContext, on_event: Callback<DetailEvent>) -> Element {
    let text = |field: AddField| {
        move |e: FormEvent| on_event.call(DetailEvent::Form(DetailAction::Add(field, e.value())))
    };
    let snippet = match &draft.snippet {
        Snippet::NotLoaded => rsx! {
            p { class: "site-detail-snippet-note", "The code at this line is read when you press Load code." }
        },
        Snippet::Loading => rsx! {
            p { class: "site-detail-snippet-note", role: "status", "Reading the code at that line" }
        },
        Snippet::Loaded(code) => rsx! {
            pre { class: "pane-snippet site-detail-snippet", "{code}" }
        },
        Snippet::Failed(why) => rsx! {
            p { class: "site-detail-snippet-note warn", role: "alert", "{why}" }
        },
    };
    rsx! {
        div { class: "queue-reason-prompt site-detail-form site-detail-add-form", id: "{DETAIL_FORM_ID}",
            label { class: "rule-modal-label", "Add a finding the engine missed" }
            div { class: "site-detail-grid",
                label { "Repo" }
                input { class: "addressee-input", r#type: "text", value: "{draft.repo}", oninput: text(AddField::Repo) }
                label { "File" }
                input { class: "addressee-input", r#type: "text", value: "{draft.path}", oninput: text(AddField::Path) }
                label { "Line" }
                div { class: "site-detail-line-load",
                    input { class: "addressee-input", r#type: "text", inputmode: "numeric", value: "{draft.line}", oninput: text(AddField::Line) }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| on_event.call(DetailEvent::LoadSnippet),
                        "Load code"
                    }
                }
                label { "Rule" }
                input { class: "addressee-input", r#type: "text", list: "site-detail-add-rules", value: "{draft.rule_id}", oninput: text(AddField::RuleId) }
                datalist { id: "site-detail-add-rules",
                    for id in ctx.rule_ids.clone() {
                        option { key: "{id}", value: "{id}" }
                    }
                }
                label { "Kind" }
                select { class: "addressee-input", value: "{draft.kind}", onchange: text(AddField::Kind),
                    option { value: "defect", selected: true, "defect" }
                }
                label { "Severity" }
                select { class: "addressee-input", value: "{draft.severity}", onchange: text(AddField::Severity),
                    for s in camerata_ui_core::edits::SEVERITIES {
                        option { key: "{s}", value: "{s}", selected: draft.severity == s, "{s}" }
                    }
                }
                label { "Headline" }
                input { class: "addressee-input", r#type: "text", value: "{draft.headline}", oninput: text(AddField::Headline) }
                label { "Detail" }
                textarea { class: "addressee-input", rows: "3", value: "{draft.detail}", oninput: text(AddField::Detail), "{draft.detail}" }
                label { "Fix" }
                textarea { class: "addressee-input", rows: "2", value: "{draft.fix}", oninput: text(AddField::Fix), "{draft.fix}" }
            }
            label { class: "rule-modal-label", "The code at that line (read-only)" }
            {snippet}
            label { class: "rule-modal-label", "Why you are adding it (required)" }
            ReasonInput {
                value: draft.reason.clone(),
                placeholder: "why (recorded with the finding)",
                focus: false,
                on_text: move |t: String| on_event.call(DetailEvent::Form(DetailAction::Reason(t))),
                on_commit: move |()| on_event.call(DetailEvent::Commit),
                on_cancel: move |()| on_event.call(DetailEvent::Cancel),
            }
            FormFooter { error: draft.error.clone(), commit_label: "Add", on_event }
        }
    }
}

// ── The controller ────────────────────────────────────────────────────────────────────────

/// The detail's signals and I/O. It is `Copy`: every field is a signal or a callback, so the
/// handlers of a lane capture it freely. A lane makes one with [`use_detail`].
#[derive(Clone, Copy)]
pub(crate) struct DetailCtl {
    api: Signal<ReviewApi>,
    mode: Signal<DetailMode>,
    undo: Signal<Option<DetailUndo>>,
    notice: Signal<Option<DetailNotice>>,
    now_ms: Signal<u64>,
    epoch: std::time::Instant,
    /// Something was written: the lane reads the tree and the shell again.
    on_changed: Callback<()>,
    /// The form closed: the lane takes the keyboard back.
    on_closed: Callback<()>,
}

/// One controller for the lane that calls this. `on_changed` runs after every write that
/// succeeds; `on_closed` runs when a form closes, so the lane can focus its region again.
pub(crate) fn use_detail(on_changed: Callback<()>, on_closed: Callback<()>) -> DetailCtl {
    let from_context = use_context::<ReviewApi>();
    let api = use_signal(|| from_context.clone());
    let epoch = use_hook(std::time::Instant::now);
    let mode = use_signal(DetailMode::default);
    let undo = use_signal(|| Option::<DetailUndo>::None);
    let notice = use_signal(|| Option::<DetailNotice>::None);
    let mut now_ms = use_signal(|| 0u64);
    // The undo clock: four ticks a second, only while an undo is armed.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if undo.peek().is_some() {
                now_ms.set(epoch.elapsed().as_millis() as u64);
            }
        }
    });
    DetailCtl {
        api,
        mode,
        undo,
        notice,
        now_ms,
        epoch,
        on_changed,
        on_closed,
    }
}

impl DetailCtl {
    /// What the pane shows besides the site, as of now.
    pub(crate) fn panel(&self) -> DetailPanel {
        let now = (self.now_ms)();
        DetailPanel {
            mode: self.mode.read().clone(),
            undo: self.undo.read().as_ref().and_then(|u| {
                u.seconds_left(now).map(|seconds_left| DetailBanner {
                    message: u.message.clone(),
                    button: u.button(),
                    seconds_left,
                })
            }),
            notice: self.notice.read().clone(),
        }
    }

    /// The pane waits for a field letter (so the lane's keys go to [`Self::chord`] first).
    pub(crate) fn is_picking(&self) -> bool {
        *self.mode.peek() == DetailMode::PickField
    }

    /// Forget the open form and the notice: another site was focused.
    pub(crate) fn reset(&self) {
        let mut mode = self.mode;
        let mut notice = self.notice;
        if *mode.peek() != DetailMode::Viewing {
            mode.set(DetailMode::Viewing);
        }
        if notice.peek().is_some() {
            notice.set(None);
        }
    }

    /// `E`, `M`, `A` or `U` on a focused site.
    pub(crate) fn open(
        &self,
        key: camerata_ui_core::edits::DetailKey,
        row: &SiteRow,
        ctx: &DetailContext,
    ) {
        use camerata_ui_core::edits::DetailKey;
        let action = match key {
            DetailKey::Edit => DetailAction::PickField,
            DetailKey::Merge => DetailAction::BeginMerge,
            DetailKey::Add => DetailAction::BeginAdd,
            DetailKey::Split => DetailAction::BeginSplit,
        };
        self.form(action, row, ctx);
    }

    /// A press while the pane waits for a field letter. `true` when it was the pane's (the lane
    /// ignores it): the letter opens that field's edit, Esc cancels, anything else is ignored.
    pub(crate) fn chord(&self, key: &str, row: &SiteRow, ctx: &DetailContext) -> bool {
        use camerata_ui_core::edits::{chord_press, ChordPress};
        if !self.is_picking() {
            return false;
        }
        match chord_press(key) {
            ChordPress::Field(field) => self.form(DetailAction::BeginEdit(field), row, ctx),
            ChordPress::Cancel => {
                let mut mode = self.mode;
                mode.set(DetailMode::Viewing);
            }
            ChordPress::Ignore => {}
        }
        true
    }

    fn form(&self, action: DetailAction, row: &SiteRow, ctx: &DetailContext) {
        let mut mode = self.mode;
        let next = reduce(mode.peek().clone(), &action, row, ctx);
        let opened_snippet = matches!(&next, DetailMode::Add(_));
        mode.set(next);
        let mut notice = self.notice;
        if matches!(
            action,
            DetailAction::BeginAck
                | DetailAction::BeginAdd
                | DetailAction::BeginMerge
                | DetailAction::BeginSplit
                | DetailAction::BeginEdit(_)
        ) {
            notice.set(None);
        }
        if opened_snippet && matches!(action, DetailAction::BeginAdd) {
            self.load_snippet();
        }
    }

    /// Handle one event from the view (the ones that are the detail's own).
    pub(crate) fn handle(
        &self,
        event: DetailEvent,
        row: &SiteRow,
        ctx: &DetailContext,
        run_id: &str,
    ) {
        match event {
            DetailEvent::Form(action) => self.form(action, row, ctx),
            DetailEvent::Cancel => {
                let mut mode = self.mode;
                mode.set(DetailMode::Viewing);
                self.on_closed.call(());
            }
            DetailEvent::Commit => self.commit(row, ctx, run_id),
            DetailEvent::StaleEdit(id, action) => self.stale_edit(&id, action),
            DetailEvent::StaleDecision(action) => self.stale_decision(row, action, run_id),
            DetailEvent::Undo => self.undo_now(),
            DetailEvent::LoadSnippet => self.load_snippet(),
            DetailEvent::Decide(_)
            | DetailEvent::Tick
            | DetailEvent::OpenCode
            | DetailEvent::OpenStack => {}
        }
    }

    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn commit(&self, row: &SiteRow, ctx: &DetailContext, run_id: &str) {
        let mut mode = self.mode;
        let current = mode.peek().clone();
        let write = match commit(&current, row, ctx, run_id) {
            Ok(write) => write,
            Err(message) => {
                mode.set(current.with_error(message));
                return;
            }
        };
        let this = *self;
        spawn(async move {
            let writes = this.api.peek().writes.clone();
            match run_write(&*writes, write.clone()).await {
                Ok(posted) => {
                    let outcome = after_write(&write, &posted);
                    let mut mode = this.mode;
                    let mut undo = this.undo;
                    let mut notice = this.notice;
                    mode.set(DetailMode::Viewing);
                    undo.set(outcome.stamp(this.now()));
                    notice.set(outcome.notice);
                    this.on_changed.call(());
                    this.on_closed.call(());
                }
                Err(message) => {
                    let mut mode = this.mode;
                    let shown = mode.peek().clone();
                    mode.set(shown.with_error(message));
                }
            }
        });
    }

    fn stale_edit(&self, edit_id: &str, action: LineAction) {
        let this = *self;
        let id = edit_id.to_string();
        spawn(async move {
            let writes = this.api.peek().writes.clone();
            let result = match action {
                LineAction::Reapply => reapply_stale_edit(&*writes, &id).await,
                LineAction::Discard => discard_stale_edit(&*writes, &id).await,
            };
            this.finish(result);
        });
    }

    fn stale_decision(&self, row: &SiteRow, action: LineAction, run_id: &str) {
        let this = *self;
        let row = row.clone();
        let run = run_id.to_string();
        spawn(async move {
            let writes = this.api.peek().writes.clone();
            let result = match action {
                LineAction::Reapply => reapply_stale_decision(&*writes, &row, &run).await,
                LineAction::Discard => discard_stale_decision(&*writes, &row).await,
            };
            this.finish(result);
        });
    }

    /// Show the outcome of a stale line's Reapply or Discard, and have the lane read again.
    fn finish(&self, result: Result<Outcome, String>) {
        let mut undo = self.undo;
        let mut notice = self.notice;
        match result {
            Ok(outcome) => {
                undo.set(outcome.stamp(self.now()));
                notice.set(outcome.notice);
                self.on_changed.call(());
            }
            Err(message) => notice.set(Some(DetailNotice {
                warn: true,
                text: message,
            })),
        }
    }

    fn undo_now(&self) {
        let mut undo = self.undo;
        let Some(entry) = undo.peek().clone() else {
            return;
        };
        undo.set(None);
        let this = *self;
        spawn(async move {
            let writes = this.api.peek().writes.clone();
            let result = match entry.action {
                UndoAction::DiscardEdit(id) => writes.discard_edit(&id).await.map(|_| ()),
                UndoAction::RevokeDecision(id) => writes.revoke_decision(&id).await.map(|_| ()),
            };
            match result {
                Ok(()) => this.on_changed.call(()),
                Err(e) => {
                    let mut notice = this.notice;
                    notice.set(Some(DetailNotice {
                        warn: true,
                        text: e.message,
                    }));
                }
            }
        });
    }

    /// Read the code at the add form's line, for its read-only snippet.
    fn load_snippet(&self) {
        let mut mode = self.mode;
        let DetailMode::Add(draft) = mode.peek().clone() else {
            return;
        };
        let Some(line) = draft.line_number() else {
            let mut next = draft;
            next.snippet = Snippet::Failed("Give a line number first.".to_string());
            mode.set(DetailMode::Add(next));
            return;
        };
        let (repo, path) = (draft.repo.trim().to_string(), draft.path.trim().to_string());
        let mut loading = draft;
        loading.snippet = Snippet::Loading;
        mode.set(DetailMode::Add(loading));
        let this = *self;
        spawn(async move {
            let reads = this.api.peek().reads.clone();
            let found = reads.source_window(&repo, &path, line).await;
            let snippet = match found {
                Ok(w) => snippet_at_line(w.start_line, &w.lines, line).map_or_else(
                    || {
                        Snippet::Failed(
                            "That line is blank or past the end of the file.".to_string(),
                        )
                    },
                    Snippet::Loaded,
                ),
                Err(e) => Snippet::Failed(e.message),
            };
            let mut mode = this.mode;
            // The reviewer may have moved on: only the form that asked takes the answer.
            let still_there = matches!(
                mode.peek().clone(),
                DetailMode::Add(d) if d.repo.trim() == repo && d.path.trim() == path && d.line_number() == Some(line)
            );
            if still_there {
                let shown = mode.peek().clone();
                if let DetailMode::Add(mut d) = shown {
                    d.snippet = snippet;
                    mode.set(DetailMode::Add(d));
                }
            }
        });
    }
}

// ── The writes, as plain async functions (tested without a runtime) ───────────────────────

/// Post a site ruling from a key or a button. A ruling the server would refuse on a thrown-out
/// row is refused here in words, and nothing is posted. `row` is the site when the lane has it
/// loaded; without it the server's own refusal stands as the backstop.
pub(crate) async fn post_ruling(
    writes: &dyn ReviewWrites,
    row: Option<&SiteRow>,
    key: &str,
    state: TriageState,
    reason: &str,
    run_id: &str,
) -> Result<Decision, String> {
    let req = match row {
        Some(row) => camerata_ui_core::edits::ruling_request(row, state, reason, run_id)?,
        None => {
            let decision = camerata_ui_core::tree::batch::decision_state_for(state)
                .ok_or_else(|| "That is not a ruling.".to_string())?;
            camerata_ui_core::tree::batch::site_decision_request(key, decision, reason, run_id)
        }
    };
    writes
        .post_decision(req)
        .await
        .map(|r| r.decision)
        .map_err(|e| e.message)
}

/// What a posted write answered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Posted {
    Edit(Edit),
    Decision(Decision),
    Added(FindingAdded),
    Acked,
}

/// Post `write`. A refusal comes back in the server's words.
pub(crate) async fn run_write(
    writes: &dyn ReviewWrites,
    write: DetailWrite,
) -> Result<Posted, String> {
    match write {
        DetailWrite::Edit { req, .. } => writes
            .post_edit(req)
            .await
            .map(|r| Posted::Edit(r.edit))
            .map_err(|e| e.message),
        DetailWrite::Decision { req, .. } => writes
            .post_decision(req)
            .await
            .map(|r| Posted::Decision(r.decision))
            .map_err(|e| e.message),
        DetailWrite::Add(req) => writes
            .add_finding(req)
            .await
            .map(Posted::Added)
            .map_err(|e| e.message),
        DetailWrite::Ack { decision_id, req } => writes
            .client_ack(&decision_id, req)
            .await
            .map(|_| Posted::Acked)
            .map_err(|e| e.message),
    }
}

/// What the pane shows after a write: an undo bar and/or a notice.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Outcome {
    pub undo: Option<(UndoAction, String)>,
    pub notice: Option<DetailNotice>,
}

impl Outcome {
    pub(crate) fn stamp(&self, now_ms: u64) -> Option<DetailUndo> {
        self.undo
            .clone()
            .map(|(action, message)| DetailUndo::new(action, message, now_ms))
    }
}

/// The undo bar and the notice a posted write leaves.
pub(crate) fn after_write(write: &DetailWrite, posted: &Posted) -> Outcome {
    match (write, posted) {
        (DetailWrite::Edit { done, .. }, Posted::Edit(edit)) => Outcome {
            undo: Some((UndoAction::DiscardEdit(edit.id.clone()), done.clone())),
            notice: None,
        },
        (DetailWrite::Decision { done, .. }, Posted::Decision(d)) => Outcome {
            undo: Some((UndoAction::RevokeDecision(d.id.clone()), done.clone())),
            notice: None,
        },
        (DetailWrite::Add(_), Posted::Added(done)) => {
            let outcome = added_outcome(done);
            Outcome {
                undo: None,
                notice: Some(DetailNotice {
                    warn: matches!(outcome, AddedOutcome::Held(_)),
                    text: added_message(&outcome),
                }),
            }
        }
        (DetailWrite::Ack { .. }, Posted::Acked) => Outcome {
            undo: None,
            notice: Some(DetailNotice {
                warn: false,
                text: "The client's acknowledgement is recorded.".to_string(),
            }),
        },
        _ => Outcome {
            undo: None,
            notice: None,
        },
    }
}

/// Reapply a stale edit: a new edit with the site's current fingerprint. Undo discards the new
/// edit.
pub(crate) async fn reapply_stale_edit(
    writes: &dyn ReviewWrites,
    edit_id: &str,
) -> Result<Outcome, String> {
    let done = writes.reapply_edit(edit_id).await.map_err(|e| e.message)?;
    Ok(Outcome {
        undo: Some((
            UndoAction::DiscardEdit(done.edit.id),
            "Edit reapplied.".to_string(),
        )),
        notice: None,
    })
}

/// Discard a stale edit. It stays in the history and never applies.
pub(crate) async fn discard_stale_edit(
    writes: &dyn ReviewWrites,
    edit_id: &str,
) -> Result<Outcome, String> {
    writes.discard_edit(edit_id).await.map_err(|e| e.message)?;
    Ok(Outcome {
        undo: None,
        notice: Some(DetailNotice {
            warn: false,
            text: "Stale edit discarded. The field keeps the engine's text.".to_string(),
        }),
    })
}

/// Reapply a stale decision: a new record with the same state and reason, on the row's own key.
/// Nothing applies until the reviewer asks for it.
pub(crate) async fn reapply_stale_decision(
    writes: &dyn ReviewWrites,
    row: &SiteRow,
    run_id: &str,
) -> Result<Outcome, String> {
    let req = reapply_decision_request(row, run_id).ok_or_else(|| {
        "A stale merge cannot be carried over: its target belongs to the earlier run. Discard it and merge again.".to_string()
    })?;
    let done = writes.post_decision(req).await.map_err(|e| e.message)?;
    Ok(Outcome {
        undo: Some((
            UndoAction::RevokeDecision(done.decision.id),
            reapply_done_message(row),
        )),
        notice: None,
    })
}

/// Discard a stale decision: the old record is revoked and the row stays Unresolved.
pub(crate) async fn discard_stale_decision(
    writes: &dyn ReviewWrites,
    row: &SiteRow,
) -> Result<Outcome, String> {
    let stale = row
        .stale_decision
        .as_ref()
        .ok_or_else(|| "This row has no stale decision.".to_string())?;
    writes
        .revoke_decision(&stale.id)
        .await
        .map_err(|e| e.message)?;
    Ok(Outcome {
        undo: None,
        notice: Some(DetailNotice {
            warn: false,
            text: "Stale decision discarded. The row stays Unresolved.".to_string(),
        }),
    })
}

pub(crate) mod code_bridge;

#[cfg(test)]
mod tests;
