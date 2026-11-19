//! The `inspection/*` gallery states (settings overhaul, work order C): the Inspection screen's
//! "This run" block and its estimate, drawn through the real props-only components the screen
//! itself mounts. Each page is the block alone, in the cockpit's card, under the screen's own
//! step title.

use camerata_api_types::project::{ProjectBackend, ProjectFinder};
use camerata_ui_core::help::inspection as help_text;
use camerata_ui_core::scan::inspection_run::{
    agent_estimate_unavailable, agent_estimate_view, AgentEstimateView, AgentRepoFigure,
    ChecksToRun,
};
use dioxus::prelude::*;

use crate::cockpit::help::HelpDialog;
use crate::cockpit::scan::{EstimateBlock, InspectionEstimate, InspectionRunControls};
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
struct PageProps {
    id: String,
}

fn agent_figure() -> EstimateBlock {
    let view = agent_estimate_view(
        &[AgentRepoFigure {
            repo: "acme/hearth".to_string(),
            pieces: 3,
            joints_calls: 1,
            expected_usd: 4.2,
            low_usd: 2.52,
            high_usd: 7.56,
        }],
        &[],
        "Claude Opus 5.5",
    );
    let AgentEstimateView::Figure(text) = view else {
        unreachable!("one figure was given");
    };
    EstimateBlock::Agent {
        text,
        finder_backend: (ProjectFinder::Agent, ProjectBackend::Cli),
    }
}

fn engine_figure() -> EstimateBlock {
    EstimateBlock::Engine {
        dollars: "about $3.40".to_string(),
        meta: "~120k tokens \u{b7} 4 pass(es) \u{b7} 12 rule(s)".to_string(),
        repo_lines: Vec::new(),
        scope_line: camerata_ui_core::scan::inspection_run::engine_scope_line(true),
        deep_on: false,
        finder_backend: (ProjectFinder::Engine, ProjectBackend::Api),
    }
}

#[component]
fn InspectionStatePage(props: PageProps) -> Element {
    let id = props.id.as_str();
    let finder = if matches!(id, "inspection/start-engine" | "inspection/help-open") {
        ProjectFinder::Engine
    } else {
        ProjectFinder::Agent
    };
    let checks = if id == "inspection/deterministic-only" {
        ChecksToRun::DeterministicOnly
    } else {
        ChecksToRun::All
    };
    let estimate = match id {
        "inspection/start-engine" | "inspection/help-open" => engine_figure(),
        "inspection/estimate-unavailable" => EstimateBlock::AgentUnavailable {
            sentence: agent_estimate_unavailable("The repository sizes could not be read."),
            finder_backend: (ProjectFinder::Agent, ProjectBackend::Cli),
        },
        "inspection/deterministic-only" => EstimateBlock::DeterministicOnly,
        _ => agent_figure(),
    };
    rsx! {
        div { class: "card review-settings-page",
            h2 { class: "panel-h", "Inspection" }
            p { class: "scan-section-h", "2. Inspect" }
            p { class: "scan-section-sub",
                "Tick the rules above, then press \u{201c}Inspect code against selected rules\u{201d}."
            }
            InspectionRunControls {
                finder,
                checks,
                on_checks: move |_| {},
                run_mode: "auto".to_string(),
                recommended_mode: "auto".to_string(),
                on_run_mode: move |_| {},
                full_ticked: false,
                on_full: move |_| {},
                spend_limit: String::new(),
                on_spend_limit: move |_| {},
                deep_available: false,
                deep: false,
                on_deep: move |_| {},
                busy: false,
            }
            InspectionEstimate { block: estimate }
            if id == "inspection/help-open" {
                HelpDialog { topic: help_text::run_mode_topic(), on_close: move |()| {} }
            }
        }
    }
}

fn render(id: &str) -> String {
    test_harness::render_props(InspectionStatePage, PageProps { id: id.to_string() })
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
    start_agent => "inspection/start-agent",
    start_engine => "inspection/start-engine",
    estimate_unavailable => "inspection/estimate-unavailable",
    deterministic_only => "inspection/deterministic-only",
    help_open => "inspection/help-open",
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::GLOBAL_CSS;

    /// The `<select id=...>` chunk of a rendered page.
    fn select_of(html: &str, id: &str) -> String {
        let at = html
            .find(&format!("<select id=\"{id}\""))
            .unwrap_or_else(|| panic!("no select {id}: {html}"));
        let end = html[at..].find("</select>").unwrap();
        html[at..at + end].to_string()
    }

    /// Setting only the select's `value` shows nothing in a static page: the option that matches
    /// must carry `selected`, or the select reads "All checks" whatever the state is.
    #[test]
    fn the_checks_select_marks_the_matching_option_selected() {
        let html = render("inspection/deterministic-only").replace(GLOBAL_CSS, "");
        let select = select_of(&html, "inspection-checks");
        let value = ChecksToRun::DeterministicOnly.value();
        assert!(
            select.contains(&format!("value=\"{value}\" selected")),
            "{select}"
        );
        assert_eq!(select.matches(" selected").count(), 1, "{select}");
        let all = render("inspection/start-agent").replace(GLOBAL_CSS, "");
        let select = select_of(&all, "inspection-checks");
        assert!(
            select.contains(&format!("value=\"{}\" selected", ChecksToRun::All.value())),
            "{select}"
        );
    }

    /// The Run mode select (Engine finder only) marks its current mode too.
    #[test]
    fn the_run_mode_select_marks_the_current_mode_selected() {
        let html = render("inspection/start-engine").replace(GLOBAL_CSS, "");
        let select = select_of(&html, "inspection-run-mode");
        assert_eq!(select.matches(" selected").count(), 1, "{select}");
    }
}
