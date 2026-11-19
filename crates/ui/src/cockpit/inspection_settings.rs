//! The Inspection settings component (settings overhaul, work order A).
//!
//! ONE component holds every setting an inspection reads, and it reads the PROJECT (the active
//! project fetch), never `GET /api/projects/:id/review/settings`, which refuses until an
//! inspection result exists. It is mounted on the inspection start screen (before any run), in
//! Settings > This project and in Review > Settings, so the three places cannot disagree.
//!
//! A props-only screen, [`InspectionSettingsView`], draws the rows; [`InspectionSettingsHost`]
//! owns the signals and does the I/O through [`InspectionIo`] (a fake in tests, HTTP in the app).
//! Every judgment (which rows show, labels, hints, "(not set: ...)", the conflict line) is a pure
//! function in `camerata_ui_core::inspection_settings`; this file chooses words and pixels.
//!
//! Each model row saves on change through the existing `POST /api/projects/:id/step-models` route
//! and shows "Saved" inline. Backend and Client engagement are the existing editors, embedded as
//! they are; Client name is the existing field. A run in flight locks every control.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use camerata_api_types::project::{ProjectBackend, ProjectFinder};
use camerata_ui_core::inspection_settings::{
    conflict_line, each_run_lines, engagement_model_conflicts, model_groups, rows,
    unused_rows_button, InspectionValues, RowSpec, Stage, FINDER_CHOICES, MODELS_NOTE,
    RUN_IN_FLIGHT_NOTE,
};
use camerata_ui_core::models::ModelsResp;
use dioxus::prelude::*;

use super::ProjectView;

/// The html id of the "Show the N rows" button.
pub(crate) const REVEAL_ID: &str = "is-reveal-unused";

type IoFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// What the component asks of the server. The app uses [`HttpInspectionIo`]; a test hands in a
/// fake.
pub(crate) trait InspectionIo {
    /// The active project, when it is `id` (a blank `id` accepts whichever is active).
    fn project<'a>(&'a self, id: &'a str) -> IoFuture<'a, Option<ProjectView>>;
    /// One write on `POST /api/projects/:id/step-models`: the updated project, or the server's
    /// reason it refused.
    fn set_step<'a>(
        &'a self,
        id: &'a str,
        step: &'a str,
        model: &'a str,
    ) -> IoFuture<'a, Result<ProjectView, String>>;
    /// The model registry for the selects.
    fn models(&self) -> IoFuture<'_, Option<ModelsResp>>;
}

/// A shared handle on an [`InspectionIo`]. Two handles are equal when they share the same source.
#[derive(Clone)]
pub(crate) struct InspectionApi(pub Rc<dyn InspectionIo>);

impl PartialEq for InspectionApi {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(Rc::as_ptr(&self.0), Rc::as_ptr(&other.0))
    }
}

impl Default for InspectionApi {
    /// The real thing.
    fn default() -> Self {
        Self(Rc::new(HttpInspectionIo))
    }
}

/// The routes, over HTTP, through the same helpers the rest of the cockpit uses.
pub(crate) struct HttpInspectionIo;

impl InspectionIo for HttpInspectionIo {
    fn project<'a>(&'a self, id: &'a str) -> IoFuture<'a, Option<ProjectView>> {
        Box::pin(async move {
            super::fetch_active_project()
                .await
                .filter(|p| id.is_empty() || p.id == id)
        })
    }

    fn set_step<'a>(
        &'a self,
        id: &'a str,
        step: &'a str,
        model: &'a str,
    ) -> IoFuture<'a, Result<ProjectView, String>> {
        Box::pin(async move {
            let resp = reqwest::Client::new()
                .post(format!(
                    "{}/api/projects/{}/step-models",
                    crate::bff_base(),
                    id
                ))
                .json(&serde_json::json!({ "step": step, "model": model }))
                .send()
                .await
                .map_err(|e| format!("Could not reach the local server ({e})."))?;
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|_| "The server sent an answer the app could not read.".to_string())?;
            if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(body
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("The server refused the change.")
                    .to_string());
            }
            body.get("project")
                .and_then(|p| serde_json::from_value(p.clone()).ok())
                .ok_or_else(|| "The server did not send the updated project.".to_string())
        })
    }

    fn models(&self) -> IoFuture<'_, Option<ModelsResp>> {
        Box::pin(crate::chat::fetch_models())
    }
}

// ── The props-only screen ───────────────────────────────────────────────────────────────

/// What a person can do in the component, as the screen reports it to its host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InspectionEvent {
    /// A select changed: the row and the value picked (a model id, the finder word, or empty).
    Pick(Stage, String),
    /// The "Show the N rows the X finder uses" button.
    ShowUnused,
    /// The embedded Backend control saved.
    BackendSaved(ProjectBackend),
    /// The embedded Client engagement control saved: engagement and the client tooling opt-in.
    EngagementSaved(bool, bool),
}

#[derive(Clone, PartialEq, Props)]
pub(crate) struct InspectionSettingsViewProps {
    /// The project the rows read.
    pub project: ProjectView,
    /// The model registry. `None` while it loads or when it failed: each select then offers its
    /// current model alone.
    #[props(default)]
    pub models: Option<ModelsResp>,
    /// A run is in flight: every control is disabled and the note says why.
    #[props(default)]
    pub run_in_flight: bool,
    /// A write is in flight.
    #[props(default)]
    pub saving: bool,
    /// The row whose write just succeeded ("Saved" shows beside it).
    #[props(default)]
    pub saved: Option<Stage>,
    /// The server's reason for the last refusal. The row keeps its old value.
    #[props(default)]
    pub save_error: Option<String>,
    /// The rows the finder in effect does not use are shown.
    #[props(default)]
    pub show_unused: bool,
    /// Bumped after a refused write so a select that showed the refused pick is built fresh.
    #[props(default)]
    pub epoch: u32,
    pub on_event: EventHandler<InspectionEvent>,
}

/// One model or finder row.
#[component]
fn InspectionRow(
    spec: RowSpec,
    models: Option<ModelsResp>,
    locked: bool,
    saved: bool,
    on_event: EventHandler<InspectionEvent>,
) -> Element {
    let stage = spec.stage;
    let id = format!("is-model-{}", stage.step_key());
    let current = spec.current.clone();
    let is_finder = stage == Stage::Finder;
    let groups = if is_finder {
        Vec::new()
    } else {
        model_groups(&models, &current)
    };
    rsx! {
        div { class: "audit-model-row inspection-settings-row",
            label { class: "audit-model-label", r#for: "{id}", "{spec.label}" }
            select {
                id: "{id}",
                class: "audit-model-select",
                disabled: locked,
                onchange: move |e: Event<FormData>| {
                    on_event.call(InspectionEvent::Pick(stage, e.value()));
                },
                if is_finder {
                    for (word , text) in FINDER_CHOICES {
                        option { key: "{word}", value: "{word}", selected: current == word, "{text}" }
                    }
                } else {
                    if let Some(unset) = spec.unset_option.as_ref() {
                        option { value: "", selected: current.is_empty(), "{unset}" }
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
            }
            span { class: "audit-model-hint", "{spec.hint}" }
            if let Some(note) = spec.unused_note.as_ref() {
                span { class: "audit-model-hint inspection-settings-unused", "{note}" }
            }
            if saved {
                span { class: "audit-model-hint inspection-settings-saved", role: "status", "Saved" }
            }
        }
    }
}

/// Every setting an inspection reads, from the project. Props-only: it never fetches.
#[component]
pub(crate) fn InspectionSettingsView(props: InspectionSettingsViewProps) -> Element {
    let project = props.project.clone();
    let locked = props.run_in_flight || props.saving;
    let values = InspectionValues::of_project(
        &project.review,
        &project.step_models,
        project.backend,
        project.client_engagement,
    );
    let specs = rows(&values, props.show_unused);
    let reveal = unused_rows_button(values.finder, props.show_unused);
    let conflict = conflict_line(values.backend, values.client_engagement);
    let model_conflicts = engagement_model_conflicts(&values);
    let lines = each_run_lines(values.finder, &project.review);
    let on_event = props.on_event;
    let models = props.models.clone();
    let saved = props.saved;
    let epoch = props.epoch;
    let run_locked = props.run_in_flight;
    let finder_is_agent = values.finder == ProjectFinder::Agent;

    rsx! {
        div {
            class: "inspection-settings",
            "data-run-in-flight": "{run_locked}",
            "data-finder": if finder_is_agent { "agent" } else { "engine" },
            if run_locked {
                p { class: "review-settings-locked", role: "status", "{RUN_IN_FLIGHT_NOTE}" }
            }
            if let Some(message) = props.save_error.as_ref() {
                div { class: "review-settings-banner", role: "alert", "{message}" }
            }

            h3 { class: "scan-section-h", "Finder and models" }
            for spec in specs {
                InspectionRow {
                    key: "{spec.stage.step_key()}-{epoch}",
                    saved: saved == Some(spec.stage),
                    spec,
                    models: models.clone(),
                    locked,
                    on_event,
                }
            }
            if let Some(text) = reveal {
                button {
                    id: "{REVEAL_ID}",
                    class: "btn-edit-sm inspection-settings-reveal",
                    r#type: "button",
                    onclick: move |_| on_event.call(InspectionEvent::ShowUnused),
                    "{text}"
                }
            }
            p { class: "audit-model-hint review-settings-note", "{MODELS_NOTE}" }

            h3 { class: "scan-section-h", "Where it runs" }
            super::rules::BackendEditor {
                project: project.clone(),
                disabled: run_locked,
                on_changed: move |backend| on_event.call(InspectionEvent::BackendSaved(backend)),
            }
            super::rules::ClientEngagementEditor {
                project: project.clone(),
                disabled: run_locked,
                on_changed: move |(engagement, opt_in)| {
                    on_event.call(InspectionEvent::EngagementSaved(engagement, opt_in));
                },
            }
            if let Some(line) = conflict {
                p { class: "review-settings-bad inspection-settings-conflict", role: "alert", "{line}" }
            }

            for line in model_conflicts {
                p {
                    class: "review-settings-bad inspection-settings-conflict",
                    role: "alert",
                    key: "{line}",
                    "{line}"
                }
            }

            h3 { class: "scan-section-h", "Engagement" }
            super::scan::ClientNameSetup {
                project_id: project.id.clone(),
                initial: project.client_name.clone(),
            }

            h3 { class: "scan-section-h", "Each run" }
            for line in lines {
                p { class: "audit-model-hint inspection-settings-line", key: "{line}", "{line}" }
            }
        }
    }
}

// ── The host: signals and I/O ───────────────────────────────────────────────────────────

/// Fold the answer of the embedded controls into the project copy, so every line derived from
/// them (the conflict line, the summary) follows the control.
pub(crate) fn with_event(mut project: ProjectView, event: &InspectionEvent) -> ProjectView {
    match event {
        InspectionEvent::BackendSaved(backend) => project.backend = *backend,
        InspectionEvent::EngagementSaved(engagement, opt_in) => {
            project.client_engagement = *engagement;
            project.client_tooling_opt_in = *opt_in;
        }
        InspectionEvent::Pick(..) | InspectionEvent::ShowUnused => {}
    }
    project
}

/// The host. It loads the project through `api`, writes one row per change, and hands each
/// updated project to `on_changed` so the screen around it can refresh anything derived (the
/// estimate, a summary line). Mount it keyed by the project id: a different project is a fresh
/// host. A blank `project_id` means whichever project is active.
#[component]
pub(crate) fn InspectionSettingsHost(
    project_id: String,
    #[props(default)] run_in_flight: bool,
    #[props(default)] on_changed: Option<EventHandler<ProjectView>>,
    #[props(default)] api: InspectionApi,
) -> Element {
    let mut project = use_signal(|| None::<ProjectView>);
    let mut load_failed = use_signal(|| false);
    let mut saving = use_signal(|| false);
    let mut saved = use_signal(|| None::<Stage>);
    let mut save_error = use_signal(|| None::<String>);
    let mut show_unused = use_signal(|| false);
    let mut epoch = use_signal(|| 0u32);

    let load_api = api.clone();
    let load_id = project_id.clone();
    use_future(move || {
        let api = load_api.clone();
        let id = load_id.clone();
        async move {
            match api.0.project(&id).await {
                Some(p) => project.set(Some(p)),
                None => load_failed.set(true),
            }
        }
    });
    let models_api = api.clone();
    let models = use_resource(move || {
        let api = models_api.clone();
        async move { api.0.models().await }
    });
    let models = models.read().clone().flatten();

    let write_api = api.clone();
    let write_id = project_id.clone();
    let handler = EventHandler::new(move |event: InspectionEvent| match event {
        InspectionEvent::ShowUnused => show_unused.set(true),
        InspectionEvent::BackendSaved(_) | InspectionEvent::EngagementSaved(..) => {
            if let Some(current) = project() {
                let next = with_event(current, &event);
                project.set(Some(next.clone()));
                if let Some(on_changed) = on_changed {
                    on_changed.call(next);
                }
            }
        }
        InspectionEvent::Pick(stage, value) => {
            if run_in_flight {
                save_error.set(Some(RUN_IN_FLIGHT_NOTE.to_string()));
                return;
            }
            let Some(current) = project() else {
                return;
            };
            let id = if write_id.is_empty() {
                current.id.clone()
            } else {
                write_id.clone()
            };
            let api = write_api.clone();
            spawn(async move {
                saving.set(true);
                save_error.set(None);
                saved.set(None);
                match api.0.set_step(&id, stage.step_key(), value.trim()).await {
                    Ok(updated) => {
                        project.set(Some(updated.clone()));
                        saved.set(Some(stage));
                        if let Some(on_changed) = on_changed {
                            on_changed.call(updated);
                        }
                    }
                    Err(message) => {
                        save_error.set(Some(message));
                        // The select shows the refused pick; build it fresh so it shows what the
                        // project still holds.
                        epoch += 1;
                    }
                }
                saving.set(false);
            });
        }
    });

    if load_failed() {
        return rsx! {
            div { class: "inspection-settings",
                div { class: "review-settings-banner", role: "alert",
                    "The project's settings could not be loaded."
                }
            }
        };
    }
    let Some(current) = project() else {
        return rsx! {
            div { class: "inspection-settings",
                p { class: "audit-model-hint", role: "status", "Loading settings" }
            }
        };
    };
    rsx! {
        InspectionSettingsView {
            project: current,
            models,
            run_in_flight,
            saving: saving(),
            saved: saved(),
            save_error: save_error(),
            show_unused: show_unused(),
            epoch: epoch(),
            on_event: handler,
        }
    }
}

#[cfg(test)]
mod tests;
