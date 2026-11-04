//! The six `settings/*` gallery states (review UI spec section 12), rendered from the fixtures
//! through the real [`ReviewSettingsView`]. Each page is the lane alone, under a heading, in the
//! cockpit's card; the shell above it belongs to the work order that builds the shell.

use camerata_api_types::review_view::EscalationBudgetOutcome;
use camerata_ui_core::models::{ModelOption, ModelsResp};
use camerata_ui_core::review_settings::SettingsDraft;
use camerata_ui_core::test_support::review_fixtures::{fixture, Body, Loaded, SettingsBody};
use dioxus::prelude::*;

use crate::cockpit::review_settings::ReviewSettingsView;
use crate::test_harness;

/// What one page draws: the fixture body and the model registry the selects list.
#[derive(Clone, PartialEq, Props)]
pub(crate) struct SettingsPageProps {
    pub body: SettingsBody,
    pub models: Option<ModelsResp>,
}

/// The registry the selects list in the gallery: the models the fixtures name, so every stage's
/// current model is a real option.
fn registry() -> ModelsResp {
    let model = |id: &str, label: &str| ModelOption {
        label: label.to_string(),
        id: id.to_string(),
        provider: "claude".to_string(),
    };
    ModelsResp {
        models: vec![
            model("claude-opus-5-5", "Claude Opus 5.5"),
            model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
            model("claude-fable-5-1", "Claude Fable 5.1"),
            model("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
        ],
        default: "claude-sonnet-5-5".to_string(),
        backend: String::new(),
    }
}

/// The props of the state `id` (`settings/rate-changed`), or `None` for any other id.
pub(crate) fn page_props(id: &str) -> Option<SettingsPageProps> {
    match fixture(id)?.body {
        Body::Settings(body) => Some(SettingsPageProps {
            body,
            models: Some(registry()),
        }),
        _ => None,
    }
}

/// One page: the heading and the lane. A draft with a preview shows the preview as what applying
/// WOULD do: the fixture's outcome is marked applied, which only the saved state may say.
#[component]
pub(crate) fn SettingsStatePage(props: SettingsPageProps) -> Element {
    let body = props.body;
    let heading = rsx! {
        h2 { class: "panel-h", "Review settings" }
        p { class: "panel-sub",
            "The model per stage, the escalation line and the Engine Check sample size for this project."
        }
    };
    match body.view {
        Loaded::Loading => rsx! {
            div { class: "card review-settings-page",
                {heading}
                p { class: "audit-model-hint", role: "status", "Loading settings" }
            }
        },
        Loaded::Failed(message) => rsx! {
            div { class: "card review-settings-page",
                {heading}
                div { class: "review-settings-banner", role: "alert", "{message}" }
            }
        },
        Loaded::Ready(view) => {
            let draft = match body.draft.as_ref() {
                Some(req) => SettingsDraft::with_req(&view, req),
                None => SettingsDraft::from_view(&view),
            };
            let preview = body.preview.map(|p| EscalationBudgetOutcome {
                applied: false,
                ..p
            });
            rsx! {
                div { class: "card review-settings-page",
                    {heading}
                    ReviewSettingsView {
                        view,
                        draft,
                        preview,
                        saving: body.saving,
                        save_error: body.save_error,
                        run_in_flight: body.run_in_flight,
                        models: props.models,
                        on_event: move |_| {},
                    }
                }
            }
        }
    }
}

/// The full HTML document of the state `id`.
pub(crate) fn render(id: &str) -> String {
    let props = page_props(id).unwrap_or_else(|| panic!("{id} is not a settings state"));
    test_harness::render_props(SettingsStatePage, props)
}

/// The renderer of `state` of the settings screen, for the registry.
pub(super) fn renderer(state: &str) -> Option<fn() -> String> {
    Some(match state {
        "defaults" => || render("settings/defaults"),
        "rate-changed" => || render("settings/rate-changed"),
        "invalid" => || render("settings/invalid"),
        "saving" => || render("settings/saving"),
        "error" => || render("settings/error"),
        "run-in-flight" => || render("settings/run-in-flight"),
        _ => return None,
    })
}
