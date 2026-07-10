//! Native-only server wiring: builds the Axum router that serves the Dioxus SSR
//! shell, the client wasm bundle, this app's server functions, and the static
//! assets (design-system CSS, PWA manifest/service-worker/error-reporter, icon —
//! all under `assets/`, served at `/static/*` via an explicit `ServeDir` mount
//! below).
//!
//! `.serve_static_assets()` (Dioxus's own asset-serving route registration) does
//! NOT serve the raw contents of `assets/` unless each file is referenced through
//! the `asset!()` macro — verified empirically: with only `.serve_static_assets()`
//! wired, `/assets/manifest.json` etc. all 404, and Dioxus's own wasm bundle lands
//! under `/wasm/*`, not the configured `asset_dir`. It ALSO internally reserves a
//! wildcard route directly at `/assets/*` (for its own `asset!()`-processed files),
//! so mounting our own `ServeDir` there too panics at router-build time with an
//! axum route conflict (`Insertion failed due to conflict with previously
//! registered route`) — also verified empirically. `/static` is a deliberately
//! different prefix from both of those, and is the same prefix the itinerary-app
//! reference this skeleton is grounded in uses for the same reason.
//!
//! No database wiring here (RUST-DIOXUS-11: single binary, no DB) — this skeleton
//! is DB-on-demand. There is also deliberately no `/api/feedback` route yet: the
//! auto-capture reporter (`assets/error-reporter.js`, `src/wasm_bridge.rs`) POSTs
//! there, but implementing that ingest endpoint is Part 2 of the scaffolder, not
//! this skeleton. Until then those POSTs 404, harmlessly (the reporter's own fetch
//! call swallows failures — see `assets/error-reporter.js`).
#![cfg(not(target_arch = "wasm32"))]

use axum::{routing::get, Router};
use dioxus::server::{render_handler, DioxusRouterExt, FullstackState, ServeConfig};
use tower_http::{compression::CompressionLayer, services::ServeDir};

use crate::App;

/// Build the app's Axum router. Called from `main.rs`'s native entrypoint.
pub fn build_router() -> Router {
    let state = FullstackState::new(ServeConfig::new(), App);

    let router: Router<FullstackState> = Router::new()
        // This skeleton's own static files (design-system CSS, PWA manifest,
        // service worker, error reporter, icon) — explicit, so it never depends on
        // guessing how the CLI's own asset pipeline maps paths (see module doc).
        .nest_service("/static", ServeDir::new("assets"))
        // Dioxus-managed static assets: the wasm bundle + its loader script
        // (served under `/wasm/*`) plus its own internal `/assets/*` route for
        // `asset!()`-processed files (unused by this skeleton, but reserved).
        .serve_static_assets()
        .register_server_functions();

    router
        .fallback(get(render_handler))
        .layer(CompressionLayer::new())
        .with_state(state)
}
