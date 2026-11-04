//! The `shell/*` gallery states (review UI spec section 12): the lane switcher and the progress
//! strip alone, in each state the shell can be in.

use camerata_ui_core::test_support::review_fixtures::fixture;
use camerata_ui_core::tree::shell::Lane;
use dioxus::prelude::*;

use crate::cockpit::review_shell::ReviewShell;
use crate::test_harness;

/// The fixture a page is built from, by state id.
#[derive(Clone, PartialEq, Props)]
pub(super) struct PageProps {
    pub id: String,
}

/// One shell state, rendered.
#[component]
pub(super) fn ShellPage(props: PageProps) -> Element {
    let Some(fx) = fixture(&props.id) else {
        return rsx! { p { "no fixture for {props.id}" } };
    };
    rsx! {
        ReviewShell { status: (&fx.shell).into(), active: Lane::Tree, div {} }
    }
}

pub(super) fn render(id: &str) -> String {
    test_harness::render_props(ShellPage, PageProps { id: id.to_string() })
}

macro_rules! states {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(fn $name() -> String { render($id) })*
        /// Every state this file renders, with its renderer.
        pub(super) fn states() -> Vec<(&'static str, fn() -> String)> {
            vec![$(($id, $name as fn() -> String)),*]
        }
    };
}

states! {
    empty => "shell/empty",
    loading => "shell/loading",
    error => "shell/error",
    held_count => "shell/held-count",
}

#[cfg(test)]
mod tests;
