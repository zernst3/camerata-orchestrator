//! The `export/*` gallery states (review UI spec section 12): the export panel with the gate
//! clear (the button is there) and blocked (one line per refused row, each with a door).

use camerata_ui_core::test_support::review_fixtures::{fixture, Body};
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
    rsx! {
        ReviewShell { status: (&fx.shell).into(), active: Lane::Tree,
            div { class: "audit-export-panel",
                p { class: "section-label", "Export product (ZIP: PDF + Excel + JSON)" }
                ExportGate { gate: body.gate.clone(),
                    // The panel's own button, as it stands when the gate lets the package out.
                    button { class: "btn-run", "Export product" }
                }
            }
        }
    }
}

pub(super) fn render(id: &str) -> String {
    test_harness::render_props(ExportPage, PageProps { id: id.to_string() })
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
}

#[cfg(test)]
mod tests;
