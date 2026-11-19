//! The six `settings/*` gallery states (review UI spec section 12), rendered from the fixtures
//! through the real [`ReviewSettingsView`]. Each page is the lane alone, under a heading, in the
//! cockpit's card; the shell above it belongs to the work order that builds the shell. The stage
//! models are the Inspection settings component's (`inspection_settings_states`).

use camerata_api_types::review_view::EscalationBudgetOutcome;
use camerata_ui_core::review_settings::SettingsDraft;
use camerata_ui_core::test_support::review_fixtures::{fixture, Body, Loaded, SettingsBody};
use dioxus::prelude::*;

use crate::cockpit::review_settings::ReviewSettingsView;
use crate::test_harness;

/// What one page draws: the fixture body.
#[derive(Clone, PartialEq, Props)]
pub(crate) struct SettingsPageProps {
    pub body: SettingsBody,
}

/// The props of the state `id` (`settings/rate-changed`), or `None` for any other id.
pub(crate) fn page_props(id: &str) -> Option<SettingsPageProps> {
    match fixture(id)?.body {
        Body::Settings(body) => Some(SettingsPageProps { body }),
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
            "The escalation line and the Engine Check sample size for this project."
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

macro_rules! states {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(fn $name() -> String { render($id) })*
        /// Every state this file renders, with its renderer.
        pub(super) fn states() -> Vec<super::BuiltRenderer> {
            vec![$(($id, $name as fn() -> String)),*]
        }
    };
}

states! {
    defaults => "settings/defaults",
    rate_changed => "settings/rate-changed",
    invalid => "settings/invalid",
    saving => "settings/saving",
    error => "settings/error",
    run_in_flight => "settings/run-in-flight",
}
