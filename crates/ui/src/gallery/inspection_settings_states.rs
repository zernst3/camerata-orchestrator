//! The `inspection-settings/*` gallery states (settings overhaul, work order A), rendered from a
//! project through the real props-only [`InspectionSettingsView`]. Each page is the component
//! alone, under a heading, in the cockpit's card: the same component the inspection start screen,
//! Settings > This project and Review > Settings mount.

use camerata_api_types::project::{ProjectBackend, ProjectFinder, ReviewSettings, StepModels};
use camerata_ui_core::inspection_settings::{save_error_line, Stage};
use camerata_ui_core::models::{ModelOption, ModelsResp};
use dioxus::prelude::*;

use crate::cockpit::inspection_settings::InspectionSettingsView;
use crate::cockpit::ProjectView;

use crate::test_harness;

/// The registry the selects list: every model the states name, so each current model is a real
/// option.
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
            model("claude-haiku-5-5", "Claude Haiku 5.5"),
            model("claude-fable-5-2", "Claude Fable 5.2"),
        ],
        default: "claude-sonnet-5-5".to_string(),
        backend: String::new(),
    }
}

/// A project that has never been inspected, with the settings `edit` leaves it.
pub(crate) fn project(
    edit: impl FnOnce(&mut ReviewSettings, &mut StepModels) -> (ProjectBackend, bool),
) -> ProjectView {
    let mut review = ReviewSettings::for_new_project();
    let mut steps = StepModels::default();
    let (backend, engagement) = edit(&mut review, &mut steps);
    serde_json::from_value(serde_json::json!({
        "id": "p-1",
        "name": "Hearth",
        "repos": ["acme/hearth"],
        "review": review,
        "step_models": steps,
        "backend": backend,
        "client_engagement": engagement,
        "client_name": "",
    }))
    .expect("a project the view reads")
}

/// The Opus recipe, set the way the acceptance test sets it: Agent finder and group review on
/// Opus 5.5, Fix on Haiku 5.5, Duplicate check on Sonnet 5.5, CLI, Client engagement off.
fn recipe() -> ProjectView {
    project(|review, steps| {
        review.finder = ProjectFinder::Agent;
        review.finder_model = "claude-opus-5-5".to_string();
        review.group_review_model = "claude-opus-5-5".to_string();
        review.dup_check_model = "claude-sonnet-5-5".to_string();
        steps.fix = "claude-haiku-5-5".to_string();
        (ProjectBackend::Cli, false)
    })
}

/// What one page draws.
#[derive(Clone, PartialEq, Props)]
pub(crate) struct PageProps {
    pub project: ProjectView,
    pub models: Option<ModelsResp>,
    pub run_in_flight: bool,
    pub saved: Option<Stage>,
    pub save_error: Option<String>,
    pub show_unused: bool,
}

#[component]
pub(crate) fn InspectionSettingsStatePage(props: PageProps) -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    rsx! {
        div { class: "card review-settings-page",
            h2 { class: "panel-h", "Inspection settings" }
            p { class: "panel-sub",
                "The finder, the models, where it runs and who it is for. Set before the first run."
            }
            InspectionSettingsView {
                project: props.project,
                models: props.models,
                run_in_flight: props.run_in_flight,
                saved: props.saved,
                save_error: props.save_error,
                show_unused: props.show_unused,
                on_event: move |_| {},
            }
        }
    }
}

/// The page of the state `id` (`inspection-settings/engine`), or `None` for any other id.
pub(crate) fn page_props(id: &str) -> Option<PageProps> {
    let base = PageProps {
        project: recipe(),
        models: Some(registry()),
        run_in_flight: false,
        saved: None,
        save_error: None,
        show_unused: false,
    };
    Some(match id {
        "inspection-settings/agent-opus" => PageProps {
            saved: Some(Stage::GroupReview),
            ..base
        },
        "inspection-settings/engine" => PageProps {
            project: project(|review, _| {
                review.finder = ProjectFinder::Engine;
                (ProjectBackend::Api, true)
            }),
            ..base
        },
        "inspection-settings/all-unset" => PageProps {
            project: project(|review, _| {
                review.group_review_model = String::new();
                (ProjectBackend::Api, false)
            }),
            ..base
        },
        "inspection-settings/hidden-rows-shown" => PageProps {
            show_unused: true,
            ..base
        },
        "inspection-settings/cli-client-conflict" => PageProps {
            project: project(|_, _| (ProjectBackend::Cli, true)),
            ..base
        },
        "inspection-settings/run-in-flight" => PageProps {
            run_in_flight: true,
            ..base
        },
        "inspection-settings/save-error" => PageProps {
            save_error: Some(save_error_line(
                Stage::GroupReview,
                "model must not be empty",
            )),
            ..base
        },
        "inspection-settings/non-anthropic-warning" => PageProps {
            project: project(|review, _| {
                review.finder = ProjectFinder::Agent;
                review.finder_model = "claude-opus-5-5".to_string();
                review.group_review_model = "openai/gpt-5".to_string();
                (ProjectBackend::Api, true)
            }),
            ..base
        },
        "inspection-settings/exclusions-listed" => PageProps {
            project: {
                let mut value = serde_json::to_value(recipe()).expect("a project serializes");
                value["scan_exclusions"] =
                    serde_json::json!(["acme-field-data/seeds/**", "acme-api/db/dump.sql"]);
                serde_json::from_value(value).expect("a project with an exclusion list")
            },
            ..base
        },
        "inspection-settings/registry-missing" => PageProps {
            models: None,
            ..base
        },
        _ => return None,
    })
}

/// The full HTML document of the state `id`.
pub(crate) fn render(id: &str) -> String {
    let props =
        page_props(id).unwrap_or_else(|| panic!("{id} is not an inspection-settings state"));
    test_harness::render_props(InspectionSettingsStatePage, props)
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
    agent_opus => "inspection-settings/agent-opus",
    engine => "inspection-settings/engine",
    all_unset => "inspection-settings/all-unset",
    hidden_rows_shown => "inspection-settings/hidden-rows-shown",
    cli_client_conflict => "inspection-settings/cli-client-conflict",
    run_in_flight => "inspection-settings/run-in-flight",
    save_error => "inspection-settings/save-error",
    registry_missing => "inspection-settings/registry-missing",
    non_anthropic_warning => "inspection-settings/non-anthropic-warning",
    exclusions_listed => "inspection-settings/exclusions-listed",
}
