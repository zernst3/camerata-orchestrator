//! The `queue/*` gallery states (review UI spec section 12): the escalation queue lane under the
//! shell, from the review fixtures, with the app's real stylesheet.

use camerata_ui_core::queue::{rate_changed_text, QueueState};
use camerata_ui_core::test_support::review_fixtures::{fixture, Body, Loaded, StateFixture};
use camerata_ui_core::tree::shell::Lane;
use camerata_ui_core::triage::PaneFocus;
use dioxus::prelude::*;

use crate::cockpit::queue::{QueueScreen, QueueStatus};
use crate::cockpit::review_shell::ReviewShell;
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
pub(super) struct PageProps {
    pub id: String,
}

#[component]
pub(super) fn QueuePage(props: PageProps) -> Element {
    let Some(StateFixture {
        shell,
        body: Body::Queue(body),
    }) = fixture(&props.id)
    else {
        return rsx! { p { "no queue fixture for {props.id}" } };
    };
    let (view, status) = match &body.queue {
        Loaded::Loading => (None, QueueStatus::Loading),
        Loaded::Failed(m) => (None, QueueStatus::Failed(m.clone())),
        Loaded::Ready(v) => (Some(v.clone()), QueueStatus::Ready),
    };
    let first = view
        .as_ref()
        .and_then(|v| v.escalated.first())
        .map(|r| r.candidate.key.clone());
    let state = QueueState {
        show_below: props.id == "queue/line-lowered",
        cursor: body.selected_site.clone().or(first),
        ..QueueState::default()
    };
    let toast = body.rate_toast.as_ref().map(rate_changed_text);
    rsx! {
        ReviewShell { status: (&shell).into(), active: Lane::Escalations,
            QueueScreen {
                view,
                status,
                state,
                focus: PaneFocus::Cursor,
                prompt: None,
                undo: None,
                toast,
            }
        }
    }
}

pub(super) fn render(id: &str) -> String {
    test_harness::render_props(QueuePage, PageProps { id: id.to_string() })
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
    empty => "queue/empty",
    loading => "queue/loading",
    error => "queue/error",
    at_budget => "queue/at-budget",
    over_budget => "queue/over-budget",
    line_lowered => "queue/line-lowered",
    rate_changed_toast => "queue/rate-changed-toast",
    thirty_k => "queue/30k",
}

#[cfg(test)]
mod tests;
