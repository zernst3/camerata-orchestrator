//! Root component: mounts the design-system stylesheet and the router. Kept as its
//! own small file (RUST-DIOXUS-1 file-role layout) rather than folded into `lib.rs`.

use dioxus::prelude::*;

use crate::routes::Route;

/// The app's single root component. `document::Stylesheet` renders a `<link>` into
/// the page head — both during server-side rendering and on the client — so the
/// design tokens + component styles (`assets/design/`) are present from first
/// paint, with no dependency on `index.html`'s own `<head>` content.
#[component]
pub fn App() -> Element {
    rsx! {
        document::Stylesheet { href: "/static/styles/index.css" }
        Router::<Route> {}
    }
}
