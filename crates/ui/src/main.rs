//! Camerata — the Enterprise Cockpit (Dioxus DESKTOP).
//!
//! The architect's control surface: brownfield onboarding (scan → propose a starter
//! ruleset → audit → arm), findings triage, routines, and the local workspace. The
//! desktop shell embeds the Axum BFF (`camerata-server`) and talks to it over localhost
//! HTTP — the same server that runs in the cloud, so the UI never calls the backend
//! crates in-process for cockpit data.
//!
//! Run it with:
//!     cargo run -p camerata-ui
//! (or `dx serve` from crates/ui if you have the Dioxus CLI and prefer hot-reload).

mod agent_activity;
mod bombe;
mod chat;
mod cockpit;
mod routines;
mod style;
mod terminal;
mod toast;
mod workspace;

use dioxus::prelude::*;

/// Where the embedded BFF binds, and the URL the cockpit fetches from. The desktop
/// shell talks to this local server over HTTP (the same server that runs in the
/// cloud later); the UI never calls the backend crates in-process for cockpit data.
pub const BFF_ADDR: &str = "127.0.0.1:8787";
pub const BFF_URL: &str = "http://127.0.0.1:8787";

fn main() {
    // Auto-load the gitignored .env at the repo root (and any parent), so the
    // GitHub token etc. are available to the embedded BFF without exporting them.
    // Run from the repo dir (`cargo run -p camerata-ui`) so `.env` is found.
    let _ = dotenvy::dotenv();
    dioxus::launch(App);
}

/// Root. Injects the global stylesheet, stands up the embedded BFF once, and shows the
/// Enterprise Cockpit.
#[component]
fn App() -> Element {
    // Stand up the BFF once, on its own background Tokio runtime, so the desktop
    // shell talks to the exact same HTTP server that will run in the cloud. If the
    // port is already serving (e.g. a standalone `camerata-server`), this bind fails
    // harmlessly and the cockpit uses the already-running one.
    use_hook(|| {
        std::thread::spawn(|| match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(async {
                if let Err(e) = camerata_server::serve(BFF_ADDR).await {
                    eprintln!("[camerata-ui] embedded BFF exited: {e}");
                }
            }),
            Err(e) => eprintln!("[camerata-ui] could not start BFF runtime: {e}"),
        });
    });

    // App-wide toast stack, shared via context so any component can push
    // notifications/errors. The ConnectionWatcher below seeds it from the
    // integration health probe.
    let toasts = use_signal(Vec::<toast::Toast>::new);
    use_context_provider(|| toasts);

    rsx! {
        // Global stylesheet, injected as a raw <style> so it works identically on
        // desktop without the asset pipeline. Keeps the whole look in one place.
        style { dangerous_inner_html: style::GLOBAL_CSS }

        div { class: "app-root",
            // Watches connection health and pushes warning/error toasts; renders nothing.
            toast::ConnectionWatcher {}
            // Drains the server-side event-ingest feed (tracker/deploy) into toasts.
            toast::NotificationPoller {}
            cockpit::CockpitShell {}
        }
        // The toast stack is a SEPARATE top-layer overlay — a sibling of app-root,
        // position:fixed, pointer-events:none on the layer (so it never blocks the
        // UI behind it) with pointer-events:auto on each toast.
        toast::ToastHost {}
        // The research chat bubble: a floating, always-available AI scratchpad.
        chat::ChatBubble {}
        // The in-app terminal: a floating PTY-backed shell panel with tab support.
        // FAB sits to the LEFT of the chat FAB so both are reachable without overlap.
        terminal::TerminalBubble {}
    }
}
