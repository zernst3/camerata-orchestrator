//! The review settings lane (review UI spec section 9; work order WO11b).
//!
//! A props-only screen, [`ReviewSettingsView`], and one host, [`ReviewSettingsHost`], that owns
//! the signals and reads and writes through [`ReviewSource`]. The screen never fetches. Every
//! judgment (validation, hints, whether Apply is allowed, what Enter and Esc do, the budget
//! preview) is a pure function in `camerata_ui_core::review_settings`; this file chooses words
//! and pixels.
//!
//! Nothing here calls a model. Changing the rate or floor re-marks the line from the stored
//! candidates on the server (design A4), so Apply sends exactly one request: the escalation
//! budget write.
//!
//! The lane is mounted by the review shell (WO10). This module only exposes the screen and its
//! host; it does not wire a lane switcher.

use std::rc::Rc;

use camerata_api_types::review_view::{
    EscalationBudgetOutcome, ReviewSettingsView as SettingsData,
};
use camerata_ui_core::models::ModelsResp;
use camerata_ui_core::review_settings::{
    budget_line, effect_sentence, field_action, field_hint, formula_apply_state, model_groups,
    preview_is_current, preview_request, reason_choices, reason_label, reason_locked, reasons_note,
    revert_field, sample_apply_state, sample_hint, sample_size, toggle_reason, ApplyState, Field,
    FieldAction, SettingsDraft, SettingsEvent, Stage,
};
use dioxus::prelude::*;

use camerata_ui::review_source::ReviewSource;

/// The sentence under the stage models: where a model may come from. It says where models do
/// NOT come from; the settings lane never suggests any other source.
pub(crate) const MODELS_NOTE: &str = "Models come from this project or a CLI flag, never the environment. The model per stage is recorded in the ledger and disclosed in the methodology section.";

/// The line beside Apply: what applying does.
pub(crate) const APPLY_NOTE: &str =
    "Changing this re-marks the line from the stored candidates. No model call.";

/// The note shown while a run is in flight.
pub(crate) const RUN_IN_FLIGHT_NOTE: &str =
    "A run is in flight, so these settings are locked until it finishes.";

// ── The props-only screen ───────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
pub struct ReviewSettingsViewProps {
    /// The settings as saved.
    pub view: SettingsData,
    /// The fields as typed.
    pub draft: SettingsDraft,
    /// What applying the draft would do (the server's preview), when it asked.
    #[props(default)]
    pub preview: Option<EscalationBudgetOutcome>,
    /// A write is in flight.
    #[props(default)]
    pub saving: bool,
    /// The server's reason for the last refusal, shown in a banner. The fields keep their values.
    #[props(default)]
    pub save_error: Option<String>,
    /// A run is in flight: the controls are disabled.
    #[props(default)]
    pub run_in_flight: bool,
    /// The model registry, for the stage selects. `None` while it loads or when it failed: each
    /// select then offers its current model alone.
    #[props(default)]
    pub models: Option<ModelsResp>,
    pub on_event: EventHandler<SettingsEvent>,
}

/// The settings lane: stage models, the escalation formula with its live budget, the Engine
/// Check sample size.
#[component]
pub fn ReviewSettingsView(props: ReviewSettingsViewProps) -> Element {
    let locked = props.run_in_flight;
    let busy = props.saving;
    let formula = formula_apply_state(&props.view, &props.draft, props.saving, props.run_in_flight);
    let sample = sample_apply_state(&props.view, &props.draft, props.saving, props.run_in_flight);
    let limits = props.view.limits;
    let rate_hint = field_hint(Field::Rate, &props.draft, &limits);
    let floor_hint = field_hint(Field::Floor, &props.draft, &limits);
    let sample_bad = field_hint(Field::Sample, &props.draft, &limits);
    let budget = budget_line(props.view.escalation.counted_lines, &props.draft, &limits);
    let effect = props.preview.as_ref().map(effect_sentence);
    let apply_label = if props.saving { "Saving" } else { "Apply" };
    let apply_title = match &formula {
        ApplyState::Invalid(hint) => hint.clone(),
        ApplyState::RunInFlight => RUN_IN_FLIGHT_NOTE.to_string(),
        _ => String::new(),
    };
    let on_event = props.on_event;

    rsx! {
        div { class: "review-settings", "data-run-in-flight": "{locked}",
            if locked {
                p { class: "review-settings-locked", role: "status", "{RUN_IN_FLIGHT_NOTE}" }
            }
            if let Some(message) = props.save_error.as_ref() {
                div { class: "review-settings-banner", role: "alert", "{message}" }
            }

            // ── Stage models ──
            h3 { class: "scan-section-h", "Stage models" }
            for stage in Stage::ALL {
                {
                    let current = stage.current(&props.view.stage_models).to_string();
                    let groups = model_groups(&props.models, &current);
                    let id = format!("rs-model-{}", stage.step_key());
                    rsx! {
                        div { class: "audit-model-row", key: "{id}",
                            label { class: "audit-model-label", r#for: "{id}", "{stage.label()}" }
                            select {
                                id: "{id}",
                                class: "audit-model-select",
                                disabled: locked || busy,
                                onchange: move |e: Event<FormData>| {
                                    on_event.call(SettingsEvent::Model(stage, e.value()));
                                },
                                if stage.can_be_unset() {
                                    option { value: "", selected: current.is_empty(), "(not set)" }
                                }
                                for (group_label , options) in groups.into_iter() {
                                    optgroup { label: "{group_label}",
                                        for opt in options.into_iter() {
                                            option {
                                                key: "{opt.id}",
                                                value: "{opt.id}",
                                                selected: current == opt.id,
                                                "{opt.label}"
                                            }
                                        }
                                    }
                                }
                            }
                            if !stage.hint().is_empty() {
                                span { class: "audit-model-hint", "{stage.hint()}" }
                            }
                        }
                    }
                }
            }
            p { class: "audit-model-hint review-settings-note", "{MODELS_NOTE}" }

            // ── Escalation formula ──
            h3 { class: "scan-section-h", "Escalation formula" }
            div { class: "audit-model-row",
                label { class: "audit-model-label", r#for: "rs-rate", "Rate per 1,000 lines" }
                div { class: "review-settings-inline",
                    input {
                        id: "rs-rate",
                        class: "review-settings-num",
                        r#type: "text",
                        inputmode: "decimal",
                        value: "{props.draft.rate}",
                        disabled: locked || busy,
                        "aria-invalid": "{rate_hint.is_some()}",
                        oninput: move |e: Event<FormData>| {
                            on_event.call(SettingsEvent::Typed(Field::Rate, e.value()));
                        },
                        onkeydown: move |e: Event<KeyboardData>| {
                            on_event.call(SettingsEvent::Key(Field::Rate, e.key().to_string()));
                        },
                    }
                    label { class: "audit-model-label", r#for: "rs-floor", "Floor" }
                    input {
                        id: "rs-floor",
                        class: "review-settings-num",
                        r#type: "text",
                        inputmode: "numeric",
                        value: "{props.draft.floor}",
                        disabled: locked || busy,
                        "aria-invalid": "{floor_hint.is_some()}",
                        oninput: move |e: Event<FormData>| {
                            on_event.call(SettingsEvent::Typed(Field::Floor, e.value()));
                        },
                        onkeydown: move |e: Event<KeyboardData>| {
                            on_event.call(SettingsEvent::Key(Field::Floor, e.key().to_string()));
                        },
                    }
                    span { class: "review-settings-budget", "{budget}" }
                }
                if let Some(hint) = rate_hint.as_ref() {
                    span { class: "review-settings-bad", role: "alert", "{hint}" }
                }
                if let Some(hint) = floor_hint.as_ref() {
                    span { class: "review-settings-bad", role: "alert", "{hint}" }
                }
                if let Some(sentence) = effect.as_ref() {
                    span { class: "review-settings-effect", "{sentence}" }
                }
            }
            div { class: "audit-model-row",
                label { class: "audit-model-label", "May exceed the budget" }
                div { class: "scan-type-selector",
                    for reason in reason_choices(&props.view) {
                        label { class: "audit-thorough-toggle", key: "{reason.as_str()}",
                            input {
                                r#type: "checkbox",
                                checked: reason_locked(reason) || props.draft.reasons.contains(&reason),
                                disabled: locked || busy || reason_locked(reason),
                                onchange: move |_| on_event.call(SettingsEvent::ToggleReason(reason)),
                            }
                            span { "{reason_label(reason)}" }
                        }
                    }
                }
                span { class: "audit-model-hint", "{reasons_note()}" }
            }
            div { class: "audit-model-row",
                span { class: "audit-model-label" }
                div { class: "review-settings-inline",
                    span { class: "audit-model-hint", "{APPLY_NOTE}" }
                    button {
                        class: "btn-run review-settings-apply",
                        r#type: "button",
                        disabled: !formula.enabled(),
                        title: "{apply_title}",
                        onclick: move |_| on_event.call(SettingsEvent::Apply),
                        "{apply_label}"
                    }
                }
            }

            // ── Engine Check ──
            h3 { class: "scan-section-h", "Engine Check" }
            div { class: "audit-model-row",
                label { class: "audit-model-label", r#for: "rs-sample", "Sample size per run" }
                div { class: "review-settings-inline",
                    input {
                        id: "rs-sample",
                        class: "review-settings-num",
                        r#type: "text",
                        inputmode: "numeric",
                        value: "{props.draft.sample}",
                        disabled: locked || busy,
                        "aria-invalid": "{sample_bad.is_some()}",
                        oninput: move |e: Event<FormData>| {
                            on_event.call(SettingsEvent::Typed(Field::Sample, e.value()));
                        },
                        onkeydown: move |e: Event<KeyboardData>| {
                            on_event.call(SettingsEvent::Key(Field::Sample, e.key().to_string()));
                        },
                    }
                    span { class: "audit-model-hint", "{sample_hint(props.view.engine_check.suggested)}" }
                }
                if let Some(hint) = sample_bad.as_ref() {
                    span { class: "review-settings-bad", role: "alert", "{hint}" }
                }
                if matches!(sample, ApplyState::Ready) {
                    span { class: "audit-model-hint", "Not saved yet. Press Enter to save." }
                }
            }
        }
    }
}

// ── The host: signals and I/O, reading and writing through the source ───────────────────

/// What the host needs from the shell that mounts it, by value so the props can be compared.
/// Two handles are equal when they are the same source.
#[derive(Clone)]
pub struct SettingsSource(pub Rc<dyn ReviewSource>);

impl PartialEq for SettingsSource {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// Load the settings.
pub(crate) async fn load(source: &dyn ReviewSource) -> Result<SettingsData, String> {
    source.settings().await.map_err(|e| e.message)
}

/// Apply the formula in `draft`: exactly one escalation-budget write, then a reload of the
/// settings. Nothing else is asked of the source, and no model is involved. Refuses, without a
/// request, a draft whose Apply is not ready (invalid, unchanged, or a run in flight).
pub(crate) async fn apply_formula(
    source: &dyn ReviewSource,
    view: &SettingsData,
    draft: &SettingsDraft,
    run_in_flight: bool,
) -> Result<(EscalationBudgetOutcome, SettingsData), String> {
    match formula_apply_state(view, draft, false, run_in_flight) {
        ApplyState::Ready => {}
        ApplyState::Invalid(hint) => return Err(hint),
        ApplyState::RunInFlight => return Err(RUN_IN_FLIGHT_NOTE.to_string()),
        ApplyState::Unchanged | ApplyState::Saving => {
            return Err("There is nothing to apply.".to_string())
        }
    }
    let req = camerata_ui_core::review_settings::escalation_req(draft, &view.limits)?;
    let outcome = source.apply_budget(req).await.map_err(|e| e.message)?;
    let reloaded = load(source).await?;
    Ok((outcome, reloaded))
}

/// Save the Engine Check sample size, then reload.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the review shell (WO10) mounts the host; delete this attribute then"
    )
)]
pub(crate) async fn save_sample(
    source: &dyn ReviewSource,
    view: &SettingsData,
    draft: &SettingsDraft,
    run_in_flight: bool,
) -> Result<SettingsData, String> {
    match sample_apply_state(view, draft, false, run_in_flight) {
        ApplyState::Ready => {}
        ApplyState::Invalid(hint) => return Err(hint),
        ApplyState::RunInFlight => return Err(RUN_IN_FLIGHT_NOTE.to_string()),
        ApplyState::Unchanged | ApplyState::Saving => {
            return Err("There is nothing to save.".to_string())
        }
    }
    let n = sample_size(draft, &view.limits)?;
    source.set_sample_size(n).await.map_err(|e| e.message)?;
    load(source).await
}

/// Set one stage's model, then reload.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the review shell (WO10) mounts the host; delete this attribute then"
    )
)]
pub(crate) async fn change_stage_model(
    source: &dyn ReviewSource,
    stage: Stage,
    model: &str,
    run_in_flight: bool,
) -> Result<SettingsData, String> {
    if run_in_flight {
        return Err(RUN_IN_FLIGHT_NOTE.to_string());
    }
    source
        .set_stage_model(stage.step_key(), model)
        .await
        .map_err(|e| e.message)?;
    load(source).await
}

#[derive(Clone, PartialEq, Props)]
pub struct ReviewSettingsHostProps {
    pub source: SettingsSource,
    /// A run is in flight (the scan panel's flag): the lane's controls lock.
    #[props(default)]
    pub run_in_flight: bool,
    /// The model registry for the stage selects.
    #[props(default)]
    pub models: Option<ModelsResp>,
    /// Called once the line has been re-marked, with the outcome, so the shell can reload the
    /// queue and show "No model call was made".
    pub on_applied: EventHandler<EscalationBudgetOutcome>,
}

/// The host: loads the settings through the source, keeps the draft, previews the line on the
/// server as the formula is typed, and applies.
#[component]
pub fn ReviewSettingsHost(props: ReviewSettingsHostProps) -> Element {
    let mut view = use_signal(|| None::<SettingsData>);
    let mut load_error = use_signal(|| None::<String>);
    let mut draft = use_signal(|| None::<SettingsDraft>);
    let preview = use_signal(|| None::<EscalationBudgetOutcome>);
    let mut saving = use_signal(|| false);
    let mut save_error = use_signal(|| None::<String>);

    let source = props.source.clone();
    use_future(move || {
        let source = source.clone();
        async move {
            match load(source.0.as_ref()).await {
                Ok(v) => {
                    draft.set(Some(SettingsDraft::from_view(&v)));
                    view.set(Some(v));
                }
                Err(message) => load_error.set(Some(message)),
            }
        }
    });

    let run_in_flight = props.run_in_flight;
    let source = props.source.clone();
    let on_applied = props.on_applied;
    let handler = EventHandler::new(move |event: SettingsEvent| {
        let (Some(current_view), Some(current)) = (view(), draft()) else {
            return;
        };
        match event {
            SettingsEvent::Typed(field, text) => {
                let mut next = current;
                match field {
                    Field::Rate => next.rate = text,
                    Field::Floor => next.floor = text,
                    Field::Sample => next.sample = text,
                }
                request_preview(&source, &current_view, &next, view, draft, preview);
                draft.set(Some(next));
            }
            SettingsEvent::ToggleReason(reason) => {
                let next = toggle_reason(&current, reason);
                request_preview(&source, &current_view, &next, view, draft, preview);
                draft.set(Some(next));
            }
            SettingsEvent::Key(field, key) => {
                let state = if field == Field::Sample {
                    sample_apply_state(&current_view, &current, saving(), run_in_flight)
                } else {
                    formula_apply_state(&current_view, &current, saving(), run_in_flight)
                };
                match field_action(&key, &state) {
                    FieldAction::Apply if field == Field::Sample => {
                        let source = source.clone();
                        spawn(async move {
                            saving.set(true);
                            save_error.set(None);
                            match save_sample(
                                source.0.as_ref(),
                                &current_view,
                                &current,
                                run_in_flight,
                            )
                            .await
                            {
                                Ok(reloaded) => {
                                    draft.set(Some(SettingsDraft::from_view(&reloaded)));
                                    view.set(Some(reloaded));
                                }
                                Err(message) => save_error.set(Some(message)),
                            }
                            saving.set(false);
                        });
                    }
                    FieldAction::Apply => {
                        apply(
                            &source,
                            current_view,
                            current,
                            run_in_flight,
                            view,
                            draft,
                            preview,
                            saving,
                            save_error,
                            on_applied,
                        );
                    }
                    FieldAction::Revert => {
                        let next = revert_field(&current, &current_view, field);
                        request_preview(&source, &current_view, &next, view, draft, preview);
                        draft.set(Some(next));
                    }
                    FieldAction::Nothing => {}
                }
            }
            SettingsEvent::Apply => {
                apply(
                    &source,
                    current_view,
                    current,
                    run_in_flight,
                    view,
                    draft,
                    preview,
                    saving,
                    save_error,
                    on_applied,
                );
            }
            SettingsEvent::Model(stage, model) => {
                let source = source.clone();
                spawn(async move {
                    saving.set(true);
                    save_error.set(None);
                    match change_stage_model(source.0.as_ref(), stage, &model, run_in_flight).await
                    {
                        Ok(reloaded) => {
                            // Only the saved side changes; typed fields stay as typed.
                            view.set(Some(reloaded));
                        }
                        Err(message) => save_error.set(Some(message)),
                    }
                    saving.set(false);
                });
            }
        }
    });

    if let Some(message) = load_error() {
        return rsx! {
            div { class: "review-settings",
                div { class: "review-settings-banner", role: "alert", "{message}" }
            }
        };
    }
    let (Some(current_view), Some(current_draft)) = (view(), draft()) else {
        return rsx! {
            div { class: "review-settings",
                p { class: "audit-model-hint", role: "status", "Loading settings" }
            }
        };
    };
    rsx! {
        ReviewSettingsView {
            view: current_view,
            draft: current_draft,
            preview: preview(),
            saving: saving(),
            save_error: save_error(),
            run_in_flight,
            models: props.models.clone(),
            on_event: handler,
        }
    }
}

/// Ask the server what `next` would do to the line, when it is a valid unsaved formula, and keep
/// the answer only if the draft (read again when the answer arrives) still describes the same
/// request. A slow earlier reply is dropped, never shown over a newer preview or after the draft
/// turned invalid. A draft that cannot be previewed clears the preview. This writes nothing and
/// calls no model.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the review shell (WO10) mounts the host; delete this attribute then"
    )
)]
fn request_preview(
    source: &SettingsSource,
    view: &SettingsData,
    next: &SettingsDraft,
    current_view: Signal<Option<SettingsData>>,
    current_draft: Signal<Option<SettingsDraft>>,
    mut preview: Signal<Option<EscalationBudgetOutcome>>,
) {
    let Some(req) = preview_request(view, next) else {
        preview.set(None);
        return;
    };
    let source = source.clone();
    spawn(async move {
        if let Ok(outcome) = source.0.preview_budget(req.clone()).await {
            let still_asked = match (current_view(), current_draft()) {
                (Some(v), Some(d)) => preview_is_current(&v, &d, &req),
                _ => false,
            };
            if still_asked {
                preview.set(Some(outcome));
            }
        }
    });
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the review shell (WO10) mounts the host; delete this attribute then"
    )
)]
#[allow(clippy::too_many_arguments)]
fn apply(
    source: &SettingsSource,
    current_view: SettingsData,
    current: SettingsDraft,
    run_in_flight: bool,
    mut view: Signal<Option<SettingsData>>,
    mut draft: Signal<Option<SettingsDraft>>,
    mut preview: Signal<Option<EscalationBudgetOutcome>>,
    mut saving: Signal<bool>,
    mut save_error: Signal<Option<String>>,
    on_applied: EventHandler<EscalationBudgetOutcome>,
) {
    let source = source.clone();
    spawn(async move {
        saving.set(true);
        save_error.set(None);
        match apply_formula(source.0.as_ref(), &current_view, &current, run_in_flight).await {
            Ok((outcome, reloaded)) => {
                // The typed sample size stays as typed; the formula comes back as saved.
                let mut next = SettingsDraft::from_view(&reloaded);
                next.sample = current.sample.clone();
                draft.set(Some(next));
                view.set(Some(reloaded));
                preview.set(None);
                on_applied.call(outcome);
            }
            // The fields keep the typed values; the reason shows in the banner.
            Err(message) => save_error.set(Some(message)),
        }
        saving.set(false);
    });
}

#[cfg(test)]
mod tests;
