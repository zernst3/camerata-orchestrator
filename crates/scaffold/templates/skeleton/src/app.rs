//! Root component: mounts the design-system stylesheet and the router. Kept as its
//! own small file (RUST-DIOXUS-1 file-role layout) rather than folded into `lib.rs`.
//!
//! When `AppRequirements::visibility` is `Private` (the default — the scaffolder's
//! FOLD D default-private skeleton lock), the router is wrapped in `AccessGate`
//! (`src/components/access_gate.rs`): a minimal single-shared-passcode gate, so a
//! freshly deployed data app is never reachable by default. `Public` omits the
//! wrapper entirely — an explicit opt-in (see CONVENTIONS.md and the scaffolder's
//! `Visibility` enum).

use dioxus::prelude::*;

use crate::routes::Route;
{{ACCESS_GATE_IMPORT}}
/// The app's single root component. `document::Stylesheet` renders a `<link>` into
/// the page head — both during server-side rendering and on the client — so the
/// design tokens + component styles (`assets/design/`) are present from first
/// paint, with no dependency on `index.html`'s own `<head>` content.
#[component]
pub fn App() -> Element {
    rsx! {
        document::Stylesheet { href: "/static/styles/index.css" }
        {{ACCESS_GATE_OPEN}}Router::<Route> {}{{ACCESS_GATE_CLOSE}}
    }
}
