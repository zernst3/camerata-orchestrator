//! The Engine Check lane (review UI spec section 7; design A2b, A9 and A10).
//!
//! The architect's own quality control of the engine, internal only: one card per sampled row,
//! one question, Yes or No, and a No opens the correction in place. Two layers, as every lane:
//!
//! - [`EngineScreen`] and the views under it are props-only. Every judgment they draw (what a
//!   card stands in, the header sentences, the honest label on each rate, which keys are armed)
//!   comes from `camerata_ui_core::engine_check`; they choose words and pixels.
//! - [`EngineHost`] owns the signals and the I/O: it reads the lane through `ReviewApi`, posts
//!   the correction (a decision or an edit) and then the verdict that names it, and takes an
//!   export-panel door to a card through the shell's `DoorRequest`.
//!
//! A rate here is the error rate "among reported findings" and the thrown-out rate is "real
//! problems among thrown-out findings" (design A10 item 6). The word that claims the check sees
//! what the engine missed appears nowhere in this file (a test scans for it).

use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::{
    EngineCheckCard, EngineCheckError, EngineCheckLane, SampleStratum,
};
use camerata_ui::review_io::ReviewApi;
use camerata_ui_core::engine_check::{
    all_verdicts_in, allocation_note, card_position, card_state, card_state_text, collapsed_text,
    correction_lines, correction_ref, correction_request, duplicate_text, engine_command,
    engine_keys_armed, error_digit, error_label, escalation_text, field_label, flags_text,
    keys_hint, merge_targets, no_request, outlier_text, placed_text, question, rate_lines,
    sample_line, stage1_text, stratum_label, verdicts_line, yes_request, CardState, Correction,
    CorrectionKind, EngineCommand, KeyPhase, LaneState, Phase, NO_CARDS, OFF_TEXT, SEVERITIES,
    SUBTITLE, TEXT_FIELDS,
};
use camerata_ui_core::tree::export::Door;
use camerata_ui_core::tree::shell::{lane_for_key, Lane};
use camerata_ui_core::tree::{
    dom_token, file_line, kind_tag_of, marker_chips, severity_badge, severity_label,
};
use camerata_ui_core::triage::{FocusMove, KeyMods, PaneFocus};
use dioxus::prelude::*;

use super::scan::kind_chip_parts;
use super::tree::{DoorRequest, ShellRefresh};

/// The DOM id of the lane's focusable region. Tabbing into it arms the keys.
pub(crate) const ENGINE_REGION_ID: &str = "camerata-engine-region";

/// How the lane stands (spec section 7, "States").
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum EngineStatus {
    Loading,
    /// The request failed, with the server's reason. A lane that had loaded stays.
    Failed(String),
    Ready,
}

// ── The screen ────────────────────────────────────────────────────────────────────────────

/// The whole Engine Check lane.
#[component]
pub(crate) fn EngineScreen(
    lane: Option<EngineCheckLane>,
    status: EngineStatus,
    state: LaneState,
    focus: PaneFocus,
    /// The rows a merge correction can point at: `(site key, label)`.
    #[props(default)]
    merge_options: Vec<(String, String)>,
    #[props(default)] on_key: Callback<(String, KeyMods)>,
    #[props(default)] on_focus: Callback<PaneFocus>,
    #[props(default)] on_prev: Callback<()>,
    #[props(default)] on_next: Callback<()>,
    #[props(default)] on_yes: Callback<()>,
    #[props(default)] on_no: Callback<()>,
    #[props(default)] on_pick: Callback<EngineCheckError>,
    #[props(default)] on_correct: Callback<()>,
    #[props(default)] on_field: Callback<EditField>,
    #[props(default)] on_value: Callback<String>,
    #[props(default)] on_reason: Callback<String>,
    #[props(default)] on_commit: Callback<()>,
    #[props(default)] on_cancel: Callback<()>,
    #[props(default)] on_settings: Callback<()>,
    #[props(default)] on_retry: Callback<()>,
) -> Element {
    let armed = engine_keys_armed(focus);
    let phase = KeyPhase::from(&state.phase);
    let card: Option<EngineCheckCard> =
        lane.as_ref().and_then(|l| l.cards.get(state.card)).cloned();
    let count = lane.as_ref().map_or(0, |l| l.cards.len());
    rsx! {
        div { class: "engine-lane",
            if let EngineStatus::Failed(message) = &status {
                div { class: "review-banner", role: "alert",
                    span { class: "review-banner-msg", "{message}" }
                    button { class: "btn-edit-sm", onclick: move |_| on_retry.call(()), "Retry" }
                }
            }
            if status == EngineStatus::Loading && lane.is_none() {
                p { class: "review-loading", "Loading the Engine Check" }
                div { class: "tree-skel",
                    for n in 0..5 {
                        div { key: "{n}", class: "tree-skel-row" }
                    }
                }
            }
            if let Some(l) = lane.as_ref() {
                EngineHead { lane: l.clone(), on_settings }
                if l.off {
                    div { class: "engine-off",
                        p { class: "pane-empty", "{OFF_TEXT}" }
                        button { class: "btn-edit-sm", onclick: move |_| on_settings.call(()), "Open Settings" }
                    }
                } else if count == 0 {
                    p { class: "pane-empty", "{NO_CARDS}" }
                }
            }
            if let Some(card) = card {
                div {
                    class: "engine-region",
                    id: "{ENGINE_REGION_ID}",
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
                        if engine_command(focus, &key, mods, phase).is_some() {
                            e.prevent_default();
                            e.stop_propagation();
                        }
                        on_key.call((key, mods));
                    },
                    EngineCardView {
                        card,
                        index: state.card,
                        count,
                        state: state.clone(),
                        merge_options,
                        on_prev,
                        on_next,
                        on_yes,
                        on_no,
                        on_pick,
                        on_correct,
                        on_field,
                        on_value,
                        on_reason,
                        on_commit,
                        on_cancel,
                    }
                }
                div { class: "pane-actions tree-keys",
                    span {
                        class: if armed { "pane-keys armed" } else { "pane-keys off" },
                        "{keys_hint(armed, phase)}"
                    }
                }
            }
        }
    }
}

// ── The head ──────────────────────────────────────────────────────────────────────────────

/// The title, the sample sentence, the allocation and the rates.
#[component]
fn EngineHead(lane: EngineCheckLane, on_settings: Callback<()>) -> Element {
    let lines = rate_lines(&lane);
    let done = all_verdicts_in(&lane);
    rsx! {
        div { class: "engine-head",
            div { class: "engine-title",
                h3 { class: "node-title", "Engine Check" }
                span { class: "mark-qc", title: "Quality control of the engine", "QC" }
                span { class: "engine-sub", "{SUBTITLE}" }
            }
            if !lane.off {
                p { class: "engine-sample",
                    "{sample_line(&lane)}"
                    if let Some(note) = allocation_note(&lane) {
                        span { class: "engine-note", " \u{b7} {note}" }
                    }
                }
                p { class: "engine-alloc",
                    for (i, a) in lane.allocation.iter().filter(|a| a.drawn > 0).enumerate() {
                        span { key: "{a.category}", class: "engine-cat", title: "{camerata_ui_core::engine_check::category_tip(a)}",
                            if i > 0 { "\u{b7} " }
                            "{a.category} {a.drawn}"
                            if a.verdicts > 0 && (done || a.indicative_only) {
                                span { class: "engine-cat-rate",
                                    " ({camerata_ui_core::engine_check::rate_text(a.rate_percent, a.margin_percent, a.indicative_only)})"
                                }
                            }
                        }
                    }
                }
                p { class: "engine-rates",
                    span { class: "engine-progress", "{verdicts_line(&lane)}" }
                    for line in lines {
                        span { key: "{line.label}", class: "engine-rate",
                            " \u{b7} {line.label}: {line.text}"
                        }
                    }
                }
                if done {
                    p { class: "engine-done",
                        span { class: "engine-done-msg", "All verdicts are in." }
                        button {
                            class: "btn-edit-sm",
                            disabled: true,
                            title: "Operator only, never in a client export. The report has no download in this build yet.",
                            "Engine Check report (operator only)"
                        }
                    }
                }
            }
        }
    }
}

// ── One card ──────────────────────────────────────────────────────────────────────────────

/// One card: the question, the two panels and whatever step of the flow is open under them.
#[component]
fn EngineCardView(
    card: EngineCheckCard,
    index: usize,
    count: usize,
    state: LaneState,
    merge_options: Vec<(String, String)>,
    on_prev: Callback<()>,
    on_next: Callback<()>,
    on_yes: Callback<()>,
    on_no: Callback<()>,
    on_pick: Callback<EngineCheckError>,
    on_correct: Callback<()>,
    on_field: Callback<EditField>,
    on_value: Callback<String>,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    let card_status = card_state(&card);
    let status_text = card_state_text(&card_status);
    let token = dom_token(&card.site.site_key);
    // "Correct it" is offered while a No still has nothing behind it.
    let correctable = matches!(card_status, CardState::Held { .. } | CardState::ThrownOut);
    let corrected = matches!(card_status, CardState::Corrected { .. });
    let (state_class, state_chip) = match &card_status {
        CardState::Pending => ("engine-state pending", None),
        CardState::Yes => ("engine-state yes", Some(("mark-edited", "\u{2713} yes"))),
        CardState::Held { .. } => (
            "engine-state held",
            Some(("mark-held", "\u{1f512} held from export until corrected")),
        ),
        CardState::ThrownOut => ("engine-state held", Some(("mark-held", "still thrown out"))),
        CardState::Restored => (
            "engine-state held",
            Some(("mark-held", "\u{1f512} put back, held until you decide it")),
        ),
        CardState::Corrected { .. } => (
            "engine-state corrected",
            Some(("mark-edited", "\u{2713} corrected")),
        ),
    };
    let summary_lines = correction_lines(&card);
    let edited_chips: Vec<_> = marker_chips(&card.site)
        .into_iter()
        .filter(|c| c.class == "mark-edited")
        .collect();
    let thrown_out = card.stratum == SampleStratum::ThrownOut;
    let q = question(&card);
    let site = &card.site;
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(kind_tag_of(site));
    let working = state.busy;
    rsx! {
        div { class: "engine-card", id: "engine-card-{token}", "data-card": "{token}",
            div { class: "engine-card-nav",
                span { class: "engine-card-pos", "{card_position(index, count)}" }
                if let Some(label) = stratum_label(card.stratum) {
                    span { class: "mark-qc", "{label}" }
                }
                span { class: "engine-card-keys",
                    button {
                        class: "btn-edit-sm",
                        disabled: index == 0 || working,
                        onclick: move |_| on_prev.call(()),
                        "Prev"
                    }
                    button {
                        class: "btn-edit-sm",
                        disabled: index + 1 >= count || working,
                        onclick: move |_| on_next.call(()),
                        "Next"
                    }
                }
            }
            div { class: "engine-question",
                span { class: "engine-q", "{q}" }
                span { class: "engine-answers",
                    button {
                        class: "btn-run engine-yes",
                        disabled: working,
                        title: "Press Y while the card has focus",
                        onclick: move |_| on_yes.call(()),
                        "Yes (Y)"
                    }
                    button {
                        class: "btn-edit-sm engine-no",
                        disabled: working,
                        title: "Press N while the card has focus",
                        onclick: move |_| on_no.call(()),
                        "No (N)"
                    }
                }
            }
            div { class: "{state_class}",
                if let Some((chip, label)) = state_chip {
                    span { class: "{chip}", "{label}" }
                }
                span { class: "engine-state-text", "{status_text}" }
                if correctable && state.phase == Phase::Viewing {
                    button { class: "btn-edit-sm", onclick: move |_| on_correct.call(()), "Correct it" }
                }
            }
            if corrected {
                div { class: "engine-fix-summary",
                    for line in summary_lines {
                        span { key: "{line}", class: "engine-fix-line", "{line}" }
                    }
                    for chip in edited_chips {
                        span { key: "{chip.class}", class: "{chip.class}", title: "{chip.tip}", "{chip.label}" }
                    }
                }
            }
            if let Some(error) = state.error.clone() {
                p { class: "batch-error", role: "alert", "{error}" }
            }
            div { class: "engine-panels",
                div { class: "engine-panel engine-client",
                    h4 { class: "engine-panel-head", "As the client would see it" }
                    div { class: "engine-line",
                        span { class: "pane-rule", "{site.rule_id}" }
                        span { class: "tree-badge tree-badge-{severity_badge(&site.severity)}",
                            "{severity_label(&site.severity)}"
                        }
                        span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
                    }
                    p { class: "site-pane-code", "{file_line(site)}" }
                    p { class: "engine-headline", "{site.headline}" }
                    if !site.detail.is_empty() {
                        p { class: "site-pane-line", span { class: "site-pane-label", "Detail" } "{site.detail}" }
                    }
                    if !site.fix.is_empty() {
                        p { class: "site-pane-line", span { class: "site-pane-label", "Fix" } "{site.fix}" }
                    }
                    if !site.snippet.is_empty() {
                        pre { class: "pane-snippet", "{site.snippet}" }
                    }
                }
                div { class: "engine-panel engine-engine",
                    h4 { class: "engine-panel-head", "What the engine did" }
                    dl { class: "engine-facts",
                        dt { "Stage 1" }
                        dd {
                            "{stage1_text(&card)}"
                            if !card.stage1.reason.is_empty() {
                                span { class: "engine-quote", " \u{201c}{card.stage1.reason}\u{201d}" }
                            }
                        }
                        dt { "Placed" }
                        dd {
                            "{placed_text(&card)}"
                            if let Some(summary) = card.placement.summary.clone() {
                                span { class: "engine-quote", " \u{b7} group summary: \u{201c}{summary}\u{201d}" }
                            }
                            if !card.placement.cause.is_empty() {
                                span { class: "engine-quote", " \u{b7} cause: {card.placement.cause}" }
                            }
                        }
                        dt { "Escalation" }
                        dd { "{escalation_text(&card.escalation)}" }
                        dt { "Collapsed from" }
                        dd { "{collapsed_text(&card)}" }
                        dt { "Duplicate link" }
                        dd { "{duplicate_text(&card)}" }
                        dt { "Outlier" }
                        dd { "{outlier_text(&card)}" }
                        dt { "Review flags" }
                        dd { "{flags_text(&card)}" }
                        if let Some(why) = site.thrown_out_reason.clone() {
                            dt { "Thrown out" }
                            dd { "{why}" }
                        }
                    }
                }
            }
            {
                match state.phase.clone() {
                    Phase::Picking { highlighted } => rsx! {
                        WrongList { highlighted, on_pick, on_cancel }
                    },
                    Phase::Correcting(correction) => rsx! {
                        CorrectionForm {
                            correction,
                            card: card.clone(),
                            merge_options,
                            busy: working,
                            on_field,
                            on_value,
                            on_reason,
                            on_commit,
                            on_cancel,
                        }
                    },
                    Phase::Viewing => rsx! {},
                }
            }
            if corrected {
                p { class: "engine-corrected-note",
                    "The corrected version is what ships. The Engine Check never appears in a client export."
                }
            }
            if thrown_out && matches!(card_status, CardState::Pending) {
                p { class: "engine-corrected-note",
                    "A No on a thrown-out finding puts it back into review and holds it from export until you decide it."
                }
            }
        }
    }
}

// ── The closed list ───────────────────────────────────────────────────────────────────────

/// The closed list of what was wrong (`1` to `8`, or arrows and Enter).
#[component]
fn WrongList(
    highlighted: usize,
    on_pick: Callback<EngineCheckError>,
    on_cancel: Callback<()>,
) -> Element {
    rsx! {
        div { class: "engine-wrong",
            p { class: "rule-modal-label", "What was wrong?" }
            ol { class: "engine-wrong-list",
                for (i, e) in EngineCheckError::ALL.into_iter().enumerate() {
                    li { key: "{i}", class: if i == highlighted { "engine-wrong-item highlighted" } else { "engine-wrong-item" },
                        button {
                            class: "engine-wrong-btn",
                            onclick: move |_| on_pick.call(e),
                            span { class: "engine-wrong-digit", "{error_digit(e).unwrap_or(' ')}" }
                            "{error_label(e)}"
                        }
                    }
                }
            }
            div { class: "queue-prompt-actions",
                button { class: "btn-edit-sm queue-prompt-cancel", onclick: move |_| on_cancel.call(()), "Cancel" }
            }
        }
    }
}

// ── The correction ────────────────────────────────────────────────────────────────────────

/// The correction for the choice made, in place, with the normal decision or edit behind it.
#[component]
fn CorrectionForm(
    correction: Correction,
    card: EngineCheckCard,
    merge_options: Vec<(String, String)>,
    busy: bool,
    on_field: Callback<EditField>,
    on_value: Callback<String>,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    let kind = correction.kind;
    let title = correction.title();
    let needs_reason = correction.needs_reason();
    let wrong = error_label(correction.error);
    let reason_label = if needs_reason {
        "Why (required, the export reads this)"
    } else {
        "Why (optional)"
    };
    let targets = merge_targets(&card, &merge_options);
    rsx! {
        div { class: "queue-reason-prompt engine-correction",
            label { class: "rule-modal-label", "No: {wrong}. {title}" }
            {
                match kind {
                    CorrectionKind::Severity => rsx! {
                        label { class: "engine-field", "Severity"
                            select {
                                class: "addressee-input",
                                value: "{correction.value}",
                                onchange: move |e| on_value.call(e.value()),
                                for s in SEVERITIES {
                                    option { key: "{s}", value: "{s}", selected: correction.value == s, "{severity_label(s)}" }
                                }
                            }
                        }
                    },
                    CorrectionKind::Merge => rsx! {
                        if targets.is_empty() {
                            p { class: "pane-empty", "No other row in this sub-group to merge into. Pick another answer, or press Esc and edit the row in the tree." }
                        } else {
                            label { class: "engine-field", "Merge into"
                                select {
                                    class: "addressee-input",
                                    value: "{correction.value}",
                                    onchange: move |e| on_value.call(e.value()),
                                    option { value: "", selected: correction.value.is_empty(), "Pick the row this one duplicates" }
                                    for (key, label) in targets.iter() {
                                        option { key: "{dom_token(key)}", value: "{key}", selected: &correction.value == key, "{label}" }
                                    }
                                }
                            }
                        }
                    },
                    CorrectionKind::Subgroup => rsx! {
                        label { class: "engine-field", "Move to sub-group"
                            input {
                                class: "addressee-input",
                                r#type: "text",
                                value: "{correction.value}",
                                oninput: move |e| on_value.call(e.value()),
                                onkeydown: move |e: KeyboardEvent| e.stop_propagation(),
                            }
                        }
                    },
                    CorrectionKind::Text => rsx! {
                        label { class: "engine-field", "Field"
                            select {
                                class: "addressee-input",
                                onchange: move |e| {
                                    if let Some(f) = TEXT_FIELDS.iter().find(|f| f.wire_name() == e.value()) {
                                        on_field.call(*f);
                                    }
                                },
                                for f in TEXT_FIELDS {
                                    option { key: "{field_label(f)}", value: "{f.wire_name()}", selected: correction.field == f, "{field_label(f)}" }
                                }
                            }
                        }
                        textarea {
                            class: "addressee-input summary-input",
                            rows: "4",
                            value: "{correction.value}",
                            oninput: move |e| on_value.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| e.stop_propagation(),
                        }
                    },
                    CorrectionKind::Summary => rsx! {
                        label { class: "engine-field", "Group summary"
                            textarea {
                                class: "addressee-input summary-input",
                                rows: "5",
                                value: "{correction.value}",
                                oninput: move |e| on_value.call(e.value()),
                                onkeydown: move |e: KeyboardEvent| e.stop_propagation(),
                            }
                        }
                    },
                    CorrectionKind::FalsePositive
                    | CorrectionKind::Escalate
                    | CorrectionKind::Deescalate
                    | CorrectionKind::Restore => rsx! {},
                }
            }
            label { class: "rule-modal-label", "{reason_label}" }
            textarea {
                class: "addressee-input queue-reason-input",
                rows: "2",
                placeholder: "why",
                value: "{correction.reason}",
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
            div { class: "queue-prompt-actions",
                button {
                    class: "btn-run queue-prompt-commit",
                    disabled: busy,
                    onclick: move |_| on_commit.call(()),
                    "Commit the correction \u{2192}"
                }
                button {
                    class: "btn-edit-sm queue-prompt-cancel",
                    title: "Leave without correcting. The No is kept and the row stays held from export.",
                    onclick: move |_| on_cancel.call(()),
                    "Leave uncorrected"
                }
            }
        }
    }
}

// ── The host ──────────────────────────────────────────────────────────────────────────────

/// Where the lane goes after a pending No is stored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Then {
    /// Stay on the card.
    Stay,
    /// Go to this card.
    Go(usize),
}

/// Store the No a half-made correction stands for, with no correction: the row is held from the
/// client export until it is corrected. Returns the words of a refusal.
pub(crate) async fn store_pending_no(
    writes: &dyn camerata_ui::review_io::ReviewWrites,
    key: &str,
    error: EngineCheckError,
) -> Result<(), String> {
    writes
        .post_engine_verdict(key, no_request(error, None))
        .await
        .map(|_| ())
        .map_err(|e| e.message)
}

/// Commit a correction: post the decision or the edit, then the verdict that names it. A
/// refusal of the correction stops before the verdict, so a No is never stored claiming a
/// correction that was not made.
pub(crate) async fn commit_correction(
    writes: &dyn camerata_ui::review_io::ReviewWrites,
    correction: &Correction,
    card: &EngineCheckCard,
    run_id: &str,
) -> Result<(), String> {
    let request = correction_request(correction, card, run_id)?;
    let id = match &request {
        camerata_ui_core::engine_check::CorrectionRequest::Decision(d) => {
            writes
                .post_decision(d.clone())
                .await
                .map_err(|e| e.message)?
                .decision
                .id
        }
        camerata_ui_core::engine_check::CorrectionRequest::Edit(e) => {
            writes
                .post_edit(e.clone())
                .await
                .map_err(|e| e.message)?
                .edit
                .id
        }
    };
    writes
        .post_engine_verdict(
            &card.site.site_key,
            no_request(correction.error, Some(correction_ref(&request, &id))),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.message)
}

/// The rows of a card's sub-group as merge targets: `(site key, "path:line headline")`.
pub(crate) async fn read_merge_options(
    reads: &dyn camerata_ui::review_source::ReviewSource,
    card: &EngineCheckCard,
) -> Result<Vec<(String, String)>, String> {
    let Some(node) = card.site.group_path.get(2) else {
        return Ok(Vec::new());
    };
    let page = reads
        .sites_page(node, None, camerata_api_types::review_view::SITES_PAGE_SIZE)
        .await
        .map_err(|e| e.message)?;
    Ok(page
        .rows
        .iter()
        .map(|r| {
            (
                r.site_key.clone(),
                format!("{} {}", file_line(r), r.headline),
            )
        })
        .collect())
}

/// The Engine Check lane with its signals and its I/O.
#[component]
pub(crate) fn EngineHost() -> Element {
    let api = use_context::<ReviewApi>();
    let refresh = try_use_context::<ShellRefresh>();
    let door = try_use_context::<DoorRequest>();
    let lanes = try_use_context::<super::review_shell::LaneControl>();
    let toasts = try_use_context::<Signal<Vec<crate::toast::Toast>>>();

    let mut lane = use_signal(|| Option::<EngineCheckLane>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| true);
    let mut ls = use_signal(LaneState::default);
    let mut focus = use_signal(|| PaneFocus::Elsewhere);
    let mut merge_options = use_signal(Vec::<(String, String)>::new);

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
                match api.reads.engine_check().await {
                    Ok(l) => {
                        lane.set(Some(l));
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.message)),
                }
                loading.set(false);
            });
        }
    });
    // Read the lane now and whenever any lane writes (the shell's counter).
    use_effect(move || {
        if let Some(r) = refresh {
            let _ = r.0.read();
        }
        reload.call(());
    });

    // An export-panel door to a held Engine Check row: focus its card once the lane is read.
    use_effect(move || {
        let Some(door) = door else {
            return;
        };
        let Some(wanted) = door.0.read().clone() else {
            return;
        };
        if wanted.lane() != Lane::EngineCheck {
            return;
        }
        let Some(l) = lane.read().clone() else {
            return;
        };
        let mut slot = door.0;
        if ls.write().focus_door(&l, &wanted) {
            super::viewer::focus_element(ENGINE_REGION_ID);
        } else {
            toast.call((
                crate::toast::ToastKind::Info,
                "That row is not in this run's Engine Check sample.".to_string(),
            ));
        }
        slot.set(None);
    });

    let after_write = use_callback(move |()| {
        if let Some(mut r) = refresh {
            *r.0.write() += 1;
        }
    });

    let run_id = move || {
        lane.peek()
            .as_ref()
            .map(|l| l.run_id.clone())
            .unwrap_or_default()
    };
    let current_card = move || -> Option<EngineCheckCard> {
        lane.peek().as_ref()?.cards.get(ls.peek().card).cloned()
    };

    // Leave the card: a No whose correction was not made is stored first, with no correction.
    let leave = use_callback({
        let api = api.clone();
        move |then: Then| {
            let pending = ls.peek().pending_no();
            let key = current_card().map(|c| c.site.site_key);
            let count = lane.peek().as_ref().map_or(0, |l| l.cards.len());
            let api = api.clone();
            ls.write().busy = true;
            spawn(async move {
                if let (Some(error_kind), Some(key)) = (pending, key) {
                    if let Err(message) = store_pending_no(&*api.writes, &key, error_kind).await {
                        let mut s = ls.write();
                        s.busy = false;
                        s.error = Some(message);
                        return;
                    }
                    after_write.call(());
                }
                let mut s = ls.write();
                s.busy = false;
                let _ = s.cancel();
                if let Then::Go(i) = then {
                    s.go(i, count);
                }
            });
        }
    });

    let load_merge_options = use_callback({
        let api = api.clone();
        move |card: EngineCheckCard| {
            let api = api.clone();
            spawn(async move {
                match read_merge_options(&*api.reads, &card).await {
                    Ok(o) => merge_options.set(o),
                    Err(message) => toast.call((
                        crate::toast::ToastKind::Error,
                        format!(
                            "Could not read the sub-group's rows for the merge picker: {message}"
                        ),
                    )),
                }
            });
        }
    });

    let pick = use_callback(move |error_kind: EngineCheckError| {
        let Some(card) = current_card() else {
            return;
        };
        if error_kind == EngineCheckError::DuplicateOfAnotherRow {
            load_merge_options.call(card.clone());
        }
        ls.write().pick(error_kind, &card);
    });

    let yes = use_callback({
        let api = api.clone();
        move |()| {
            let Some(card) = current_card() else {
                return;
            };
            let count = lane.peek().as_ref().map_or(0, |l| l.cards.len());
            let api = api.clone();
            ls.write().busy = true;
            spawn(async move {
                match api
                    .writes
                    .post_engine_verdict(&card.site.site_key, yes_request())
                    .await
                {
                    Ok(_) => {
                        {
                            let mut s = ls.write();
                            s.busy = false;
                            let _ = s.cancel();
                            // Yes records the verdict and advances.
                            let next = s.card + 1;
                            if next < count {
                                s.go(next, count);
                            }
                        }
                        after_write.call(());
                    }
                    Err(e) => {
                        let mut s = ls.write();
                        s.busy = false;
                        s.error = Some(e.message);
                    }
                }
            });
        }
    });

    let no = use_callback(move |()| {
        let Some(card) = current_card() else {
            return;
        };
        if card.stratum == SampleStratum::ThrownOut {
            // A No on a thrown-out finding has one answer, and puts the row back.
            pick.call(EngineCheckError::ThrownOutRealProblem);
        } else {
            ls.write().open_list();
        }
    });

    // "Correct it" on a held card reopens the correction for the No it already has.
    let correct = use_callback(move |()| {
        let Some(card) = current_card() else {
            return;
        };
        match card.verdict.as_ref().and_then(|v| v.wrong) {
            Some(e) => pick.call(e),
            None => ls.write().open_list(),
        }
    });

    let commit = use_callback({
        let api = api.clone();
        move |()| {
            let (Some(card), Phase::Correcting(correction)) =
                (current_card(), ls.peek().phase.clone())
            else {
                return;
            };
            let run = run_id();
            let api = api.clone();
            {
                let mut s = ls.write();
                s.busy = true;
                s.error = None;
            }
            spawn(async move {
                match commit_correction(&*api.writes, &correction, &card, &run).await {
                    Ok(()) => {
                        {
                            let mut s = ls.write();
                            s.busy = false;
                            s.phase = Phase::Viewing;
                        }
                        after_write.call(());
                    }
                    Err(message) => {
                        let mut s = ls.write();
                        s.busy = false;
                        s.error = Some(message);
                    }
                }
            });
        }
    });

    let go_to_node = use_callback(move |()| {
        let Some(card) = current_card() else {
            return;
        };
        let Some(node) = card.site.group_path.get(2).cloned() else {
            return;
        };
        if let Some(mut door) = door {
            door.0.set(Some(Door::Node { id: node }));
        } else if let Some(mut lanes) = lanes {
            lanes.0.set(Lane::Tree);
        }
    });

    let on_key = use_callback(move |(key, mods): (String, KeyMods)| {
        // `1` to `5` switch lanes while the region has focus and the list is not open to take them.
        let phase = KeyPhase::from(&ls.peek().phase);
        if *focus.peek() == PaneFocus::Cursor && phase == KeyPhase::Viewing {
            if let Some(mut lanes) = lanes {
                let claimed = mods.ctrl || mods.alt || mods.meta;
                if let Some(next) = lane_for_key(&key, claimed, false) {
                    lanes.0.set(next);
                    return;
                }
            }
        }
        let Some(cmd) = engine_command(*focus.peek(), &key, mods, phase) else {
            return;
        };
        let count = lane.peek().as_ref().map_or(0, |l| l.cards.len());
        let at = ls.peek().card;
        match cmd {
            EngineCommand::Next => {
                if at + 1 < count {
                    leave.call(Then::Go(at + 1));
                }
            }
            EngineCommand::Prev => {
                if at > 0 {
                    leave.call(Then::Go(at - 1));
                }
            }
            EngineCommand::Yes => yes.call(()),
            EngineCommand::No => no.call(()),
            EngineCommand::Pick(e) => pick.call(e),
            EngineCommand::Highlight(dir) => {
                let dir: FocusMove = dir;
                ls.write().move_highlight(dir);
            }
            EngineCommand::PickHighlighted => {
                if let Some(card) = current_card() {
                    if let Phase::Picking { highlighted } = ls.peek().phase {
                        if let Some(e) = EngineCheckError::ALL.get(highlighted) {
                            if *e == EngineCheckError::DuplicateOfAnotherRow {
                                load_merge_options.call(card.clone());
                            }
                        }
                    }
                    ls.write().pick_highlighted(&card);
                }
            }
            EngineCommand::Commit => commit.call(()),
            EngineCommand::Cancel => leave.call(Then::Stay),
            EngineCommand::GoToNode => go_to_node.call(()),
            EngineCommand::Merge => pick.call(EngineCheckError::DuplicateOfAnotherRow),
        }
    });

    let status = if let Some(message) = error.read().clone() {
        EngineStatus::Failed(message)
    } else if *loading.read() && lane.read().is_none() {
        EngineStatus::Loading
    } else {
        EngineStatus::Ready
    };
    let lane_now = lane.read().clone();
    let count = lane_now.as_ref().map_or(0, |l| l.cards.len());

    rsx! {
        EngineScreen {
            lane: lane_now,
            status,
            state: ls.read().clone(),
            focus: focus(),
            merge_options: merge_options.read().clone(),
            on_key,
            on_focus: move |f: PaneFocus| {
                // While a correction form is open the text fields hold focus; a blur out of the
                // region must not re-arm the card keys over a half-written reason.
                let typing = matches!(ls.peek().phase, Phase::Correcting(_));
                if let Some(next) = camerata_ui_core::triage::next_pane_focus(f, typing) {
                    focus.set(next);
                }
            },
            on_prev: move |()| {
                let at = ls.peek().card;
                if at > 0 {
                    leave.call(Then::Go(at - 1));
                }
            },
            on_next: move |()| {
                let at = ls.peek().card;
                if at + 1 < count {
                    leave.call(Then::Go(at + 1));
                }
            },
            on_yes: yes,
            on_no: no,
            on_pick: pick,
            on_correct: correct,
            on_field: move |f: EditField| {
                let phase = ls.peek().phase.clone();
                if let (Some(card), Phase::Correcting(c)) = (current_card(), phase) {
                    ls.write().phase = Phase::Correcting(c.with_field(f, &card));
                }
            },
            on_value: move |v: String| {
                if let Phase::Correcting(c) = &mut ls.write().phase {
                    c.value = v;
                }
            },
            on_reason: move |v: String| {
                if let Phase::Correcting(c) = &mut ls.write().phase {
                    c.reason = v;
                }
            },
            on_commit: commit,
            on_cancel: move |()| leave.call(Then::Stay),
            on_settings: move |()| {
                if let Some(mut lanes) = lanes {
                    lanes.0.set(Lane::Settings);
                }
            },
            on_retry: move |()| reload.call(()),
        }
    }
}

#[cfg(test)]
mod tests;
