//! The `export/*` gallery states (review UI spec section 12): the export panel with the gate
//! clear (the button is there) and blocked (one line per refused row, each with a door).

use camerata_ui_core::test_support::review_fixtures::{fixture, Body};
use camerata_ui_core::tree::export::held_keys;
use camerata_ui_core::tree::shell::Lane;
use dioxus::prelude::*;

use crate::cockpit::export_gate::ExportGate;
use crate::cockpit::review_shell::ReviewShell;
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
pub(super) struct PageProps {
    pub id: String,
}

#[component]
pub(super) fn ExportPage(props: PageProps) -> Element {
    let Some(fx) = fixture(&props.id) else {
        return rsx! { p { "no fixture for {props.id}" } };
    };
    let Body::Export(body) = &fx.body else {
        return rsx! { p { "{props.id} is not an export state" } };
    };
    // The triage table holds every held row, except in the many-rows state, where the last three
    // are only in Review (so the line offers both buttons).
    let mut keys = held_keys(&body.gate);
    if props.id == "export/blocked-many" {
        keys.truncate(keys.len().saturating_sub(3));
    }
    let table_keys: std::collections::HashSet<String> = keys.into_iter().collect();
    rsx! {
        ReviewShell { status: (&fx.shell).into(), active: Lane::Tree,
            div { class: "audit-export-panel",
                // The real panel's heading row: the title and the "?" help button.
                div { class: "export-heading",
                    p { class: "section-label", "{camerata_ui_core::export_panel::HEADING}" }
                    crate::cockpit::help::HelpButton {
                        topic: camerata_ui_core::help::export::export_topic(),
                    }
                }
                ExportGate { gate: body.gate.clone(), table_keys: Some(table_keys),
                    // The panel's own button, as it stands when the gate lets the package out.
                    button { class: "btn-run", "Export product" }
                }
            }
        }
    }
}

pub(super) fn render(id: &str) -> String {
    match id {
        "findings/held-rows" => crate::cockpit::scan::held_rows_gallery(false),
        "findings/held-filter-on" => crate::cockpit::scan::held_rows_gallery(true),
        _ => test_harness::render_props(ExportPage, PageProps { id: id.to_string() }),
    }
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
    clear => "export/clear",
    blocked => "export/blocked",
    one_line_two_reasons => "export/one-line-two-reasons",
    blocked_many => "export/blocked-many",
    held_rows => "findings/held-rows",
    held_filter_on => "findings/held-filter-on",
}

#[cfg(test)]
mod tests;
