//! The Run figures panel (GitHub #293, with #61's batch status): after an inspection run, the
//! operator sees a metrics-only summary of it and can copy it as plain text. One table; the
//! buttons above it switch views (Summary, Stages, Agent roles, Models, Batches).
//!
//! Two sources, read separately: the report the screen already holds (scope, and the stage
//! figures on its ledger) and the run folder's figures (`GET /api/inspections/:id/figures`: wall
//! clock and batch status). The figures themselves, and the copied text, are worked out in
//! `camerata_ui_core::run_figures`. Operator-only: nothing here is ever part of the client
//! package, and no path, code or finding is shown.

use super::*;
use camerata_ui_core::run_figures::{FiguresView, ReportFigures, RunFigures, RunRecordFigures};

/// `GET /api/inspections/:id/figures`. `None` when the run has no saved folder or the server did
/// not answer: the figures only the folder holds then read "not recorded".
pub(in crate::cockpit) async fn fetch_run_record_figures(run_id: &str) -> Option<RunRecordFigures> {
    let resp = reqwest::get(format!(
        "{}/api/inspections/{run_id}/figures",
        crate::bff_base()
    ))
    .await
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

/// Where the last Copy stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Copied {
    Yes,
    No,
}

/// The panel, from figures already worked out. `copier` takes the copied text in place of the
/// system clipboard (a test, the gallery); without it the platform clipboard is used.
#[component]
pub(crate) fn RunFiguresPanel(
    figures: RunFigures,
    #[props(default)] copier: Option<EventHandler<String>>,
    // The view the panel opens on (Summary unless a gallery page shows another).
    #[props(default)] initial_view: FiguresView,
) -> Element {
    let mut view = use_signal(|| initial_view);
    let mut copied = use_signal(|| None::<Copied>);
    let table = figures.table(view());
    let text = figures.copy_text();
    // Kept in variables so they are dynamic attributes a test can find the element by.
    let copy_mark = "run-figures-copy";
    rsx! {
        div { class: "det-progress run-figures", "data-test": "run-figures",
            div { class: "det-progress-head",
                span { class: "det-progress-title", "Run figures" }
                span { class: "det-progress-count", "Operator only. Never part of the client package." }
            }
            div { class: "triage-switch run-figures-views",
                for v in FiguresView::ALL {
                    button {
                        key: "{v.key()}",
                        r#type: "button",
                        class: if view() == v { "triage-tab active" } else { "triage-tab" },
                        "data-test": "run-figures-view-{v.key()}",
                        onclick: move |_| view.set(v),
                        "{v.label()}"
                    }
                }
            }
            table { class: "review-stats-table run-figures-table", "data-view": "{view().key()}",
                thead {
                    tr {
                        for h in table.headers.iter() {
                            th { key: "{h}", "{h}" }
                        }
                    }
                }
                tbody {
                    if table.rows.is_empty() {
                        tr { class: "run-figures-empty",
                            td { colspan: "{table.headers.len()}", "{table.empty}" }
                        }
                    }
                    for (i, row) in table.rows.iter().enumerate() {
                        tr { key: "{i}",
                            for (j, cell) in row.iter().enumerate() {
                                td { key: "{j}", "{cell}" }
                            }
                        }
                    }
                }
            }
            div { class: "cost-panel-actions",
                button {
                    class: "btn-secondary run-figures-copy",
                    r#type: "button",
                    "data-test": copy_mark,
                    title: "Copies every figure as plain text, one per line.",
                    onclick: move |_| {
                        let text = text.clone();
                        match copier {
                            Some(handler) => {
                                handler.call(text);
                                copied.set(Some(Copied::Yes));
                            }
                            None => {
                                spawn(async move {
                                    let ok = camerata_ui::platform::current().copy_text(&text).await;
                                    copied.set(Some(if ok { Copied::Yes } else { Copied::No }));
                                });
                            }
                        }
                    },
                    "Copy"
                }
                match copied() {
                    Some(Copied::Yes) => rsx! {
                        span { class: "onboard-gate-hint run-figures-copied", "Copied the run figures." }
                    },
                    Some(Copied::No) => rsx! {
                        span { class: "onboard-gate-hint run-figures-copied", "Could not copy the run figures." }
                    },
                    None => rsx! {},
                }
            }
        }
    }
}

/// The panel for a finished run: the report's figures as given, the run folder's fetched by the
/// run's id. A run that was not saved (no id) has no folder, so its time and batches read "not
/// recorded".
#[component]
pub(crate) fn RunFiguresSection(
    report: ReportFigures,
    run_id: Option<String>,
    #[props(default)] copier: Option<EventHandler<String>>,
) -> Element {
    let record = use_resource(use_reactive!(|run_id| async move {
        match run_id {
            Some(id) => fetch_run_record_figures(&id).await,
            None => None,
        }
    }));
    let Some(record) = record.read().clone() else {
        return rsx! {
            div { class: "det-progress run-figures", "data-test": "run-figures-loading",
                span { class: "det-progress-title", "Run figures" }
                span { class: "det-progress-count", "Reading the run's figures." }
            }
        };
    };
    rsx! {
        RunFiguresPanel { figures: RunFigures::new(report, record), copier }
    }
}

#[cfg(test)]
#[path = "run_figures_panel_tests.rs"]
mod tests;
