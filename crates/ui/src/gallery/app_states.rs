//! The `app/*` gallery state: the screen shown instead of the cockpit when the app and its server
//! come from different builds, rendered from pure props through the real [`MismatchScreen`].

use dioxus::prelude::*;

use crate::build_guard::{MismatchScreen, MismatchScreenProps, ServerBuild};
use crate::test_harness;

#[component]
fn MismatchPage(props: MismatchScreenProps) -> Element {
    rsx! { MismatchScreen { ui_build: props.ui_build, server: props.server } }
}

fn build_mismatch() -> String {
    test_harness::render_props(
        MismatchPage,
        MismatchScreenProps {
            ui_build: Some("3f9c2a1b7d4e5f60718293a4b5c6d7e8f9012345".to_string()),
            server: ServerBuild {
                commit: Some("a81d0e4c2b3f46570817263544536271809fedcb".to_string()),
                dirty: false,
            },
        },
    )
}

/// Every state this file renders, with its renderer.
pub(super) fn states() -> Vec<super::BuiltRenderer> {
    vec![("app/build-mismatch", build_mismatch as fn() -> String)]
}
