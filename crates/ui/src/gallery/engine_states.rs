//! The `engine/*` gallery states (review UI spec section 12): the Engine Check lane under the
//! shell, from the review fixtures, with the app's real stylesheet.

use camerata_ui_core::engine_check::{LaneState, Phase};
use camerata_ui_core::test_support::review_fixtures::{fixture, Body, Loaded, StateFixture};
use camerata_ui_core::tree::shell::Lane;
use camerata_ui_core::triage::PaneFocus;
use dioxus::prelude::*;

use crate::cockpit::engine_check::{EngineScreen, EngineStatus};
use crate::cockpit::review_shell::ReviewShell;
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
pub(super) struct PageProps {
    pub id: String,
}

#[component]
pub(super) fn EnginePage(props: PageProps) -> Element {
    let Some(StateFixture {
        shell,
        body: Body::Engine(body),
    }) = fixture(&props.id)
    else {
        return rsx! { p { "no engine fixture for {props.id}" } };
    };
    let (lane, status) = match &body.lane {
        Loaded::Loading => (None, EngineStatus::Loading),
        Loaded::Failed(m) => (None, EngineStatus::Failed(m.clone())),
        Loaded::Ready(l) => (Some(l.clone()), EngineStatus::Ready),
    };
    let state = LaneState {
        card: body.selected_card.unwrap_or(0),
        phase: if body.wrong_list_open {
            Phase::Picking { highlighted: 0 }
        } else {
            Phase::Viewing
        },
        ..LaneState::default()
    };
    rsx! {
        ReviewShell { status: (&shell).into(), active: Lane::EngineCheck,
            EngineScreen {
                lane,
                status,
                state,
                focus: PaneFocus::Cursor,
            }
        }
    }
}

pub(super) fn render(id: &str) -> String {
    test_harness::render_props(EnginePage, PageProps { id: id.to_string() })
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
    lane_off => "engine/lane-off",
    loading => "engine/loading",
    card_pending => "engine/card-pending",
    no_list_open => "engine/no-list-open",
    no_pending_correction => "engine/no-pending-correction",
    no_corrected => "engine/no-corrected",
    all_verdicts => "engine/all-verdicts",
    small_category => "engine/small-category",
}

#[cfg(test)]
mod tests;
