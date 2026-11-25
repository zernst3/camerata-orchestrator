//! The `run-figures/*` gallery states (GitHub #293): the operator-only Run figures panel a
//! finished run shows, drawn through the real component from the acme run's two sources (the
//! saved report and the run folder's figures).

use camerata_ui_core::run_figures::{FiguresView, ReportFigures, RunFigures, RunRecordFigures};
use camerata_ui_core::test_support::run_figures::{FIGURES_JSON, REPORT_JSON};
use dioxus::prelude::*;

use crate::cockpit::scan::RunFiguresPanel;
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
struct PageProps {
    figures: RunFigures,
    view: FiguresView,
}

#[component]
fn RunFiguresStatePage(props: PageProps) -> Element {
    rsx! {
        div { class: "card review-settings-page",
            RunFiguresPanel { figures: props.figures, initial_view: props.view }
        }
    }
}

fn acme() -> RunFigures {
    let report: ReportFigures = serde_json::from_str(REPORT_JSON).unwrap_or_default();
    let record: Option<RunRecordFigures> = serde_json::from_str(FIGURES_JSON).ok();
    RunFigures::new(report, record)
}

fn page(figures: RunFigures, view: FiguresView) -> String {
    test_harness::render_props(RunFiguresStatePage, PageProps { figures, view })
}

fn summary() -> String {
    page(acme(), FiguresView::Summary)
}

fn batches() -> String {
    page(acme(), FiguresView::Batches)
}

/// A run saved before any figure was recorded, whose folder could not be read.
fn not_recorded() -> String {
    page(
        RunFigures::new(ReportFigures::default(), None),
        FiguresView::Summary,
    )
}

/// Every state this file renders, with its renderer.
pub(super) fn states() -> Vec<super::BuiltRenderer> {
    vec![
        ("run-figures/summary", summary as fn() -> String),
        ("run-figures/batches", batches),
        ("run-figures/not-recorded", not_recorded),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pages_show_the_figures_of_each_state() {
        let s = summary();
        assert!(s.contains("48,213") && s.contains("1h 02m 03s"), "{s}");
        let b = batches();
        assert!(
            b.contains("data-view=\"batches\"") && b.contains("Processing"),
            "{b}"
        );
        let n = not_recorded();
        assert!(n.contains("not recorded") && !n.contains("48,213"), "{n}");
    }
}
