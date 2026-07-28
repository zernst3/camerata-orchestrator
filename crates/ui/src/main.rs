//! Camerata — the Enterprise Cockpit (Dioxus DESKTOP).
//!
//! The architect's control surface: brownfield onboarding (scan → propose a starter
//! ruleset → audit → arm), findings triage, routines, and the local workspace. The
//! desktop shell launches the Axum BFF (`camerata-server`) as a SUBPROCESS (or reuses a
//! healthy one already on the port — see `server_process`) and talks to it over
//! localhost HTTP — the same server that runs in the cloud, so the UI has no compile
//! dependency on the backend crates and never calls them in-process for cockpit data.
//!
//! Run it with:
//!     cargo run -p camerata-ui
//! (or `dx serve` from crates/ui if you have the Dioxus CLI and prefer hot-reload).

use camerata_ui::desktop_chrome::{self, CLIPBOARD_SHIM_SCRIPT, GOOGLE_FONTS_LINK};

mod agent_activity;
mod bombe_bg;
mod chat;
mod cockpit;
mod credentials;
pub mod loading;
pub mod md;
mod routines;
mod server_process;
mod style;
mod terminal;
mod toast;
mod vcs_settings;
mod design;
mod readiness_gate;
mod workspace;

use dioxus::prelude::*;

/// Where the BFF subprocess binds, and the URL the cockpit fetches from. The desktop
/// shell talks to this local server over HTTP (the same server that runs in the
/// cloud later); the UI never calls the backend crates in-process for cockpit data.
pub const BFF_ADDR: &str = "127.0.0.1:8787";
pub const BFF_URL: &str = "http://127.0.0.1:8787";

/// The BFF base URL the cockpit's HTTP helpers talk to. Production uses the embedded BFF at
/// [`BFF_URL`]; tests override it via `CAMERATA_BFF_URL` to point a helper at a mock server
/// (wiremock). New/converted network helpers should call this instead of `BFF_URL` directly so they
/// are testable. (The override is process-global env, so mock-server tests that set it should not
/// run concurrently with other helpers that read it — keep such tests narrowly scoped.)
pub fn bff_base() -> String {
    std::env::var("CAMERATA_BFF_URL").unwrap_or_else(|_| BFF_URL.to_string())
}

fn main() {
    // Auto-load the gitignored .env at the repo root (and any parent), so the
    // GitHub token etc. are inherited by the spawned BFF without exporting them.
    // Run from the repo dir (`cargo run -p camerata-ui`) so `.env` is found.
    let _ = dotenvy::dotenv();
    // Set the OS window title, install a native menu bar, and inject a
    // clipboard-shim script.  See the individual items and `app_menu_bar` for
    // rationale.  The decision note is at
    // docs/decisions/2026-06-24_desktop_clipboard.md.
    use dioxus::desktop::{Config, WindowBuilder, WindowCloseBehaviour};
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                .with_menu(desktop_chrome::app_menu_bar())
                .with_window(WindowBuilder::new().with_title("Camerata Orchestrator"))
                // Ensure closing the window CLOSES it (does not hide it — the macOS
                // default for Dioxus when not set explicitly is WindowHides, which
                // keeps the process alive and the BFF subprocess bound on :8787).
                .with_close_behaviour(WindowCloseBehaviour::WindowCloses)
                // Ensure the process exits when the last window closes.  The
                // server_process exit watchdog notices the app is gone and SIGTERMs
                // the spawned BFF, so the :8787 bind is released within ~a second.
                // Without this, a hidden window keeps the process (and the stale
                // server) alive to shadow the freshly-built one on the next `cargo run`.
                .with_exits_when_last_window_closes(true)
                // Google Fonts for "Courier Prime" (title/mono) and "Inter" (sans body),
                // followed by the JS shim for Cmd-C / Cmd-X / Cmd-A.
                // See docs/decisions/2026-06-24_desktop_clipboard.md for the shim rationale.
                .with_custom_head(format!("{GOOGLE_FONTS_LINK}\n{CLIPBOARD_SHIM_SCRIPT}")),
        )
        .launch(App);
}

/// Root. Injects the global stylesheet, stands up the BFF subprocess once, and shows the
/// Enterprise Cockpit.
#[component]
fn App() -> Element {
    // Stand up the BFF once, as a SUBPROCESS (Phase G), so the desktop shell talks to
    // the exact same HTTP server that will run in the cloud — without the UI compiling
    // against the server crate.  `ensure_server_running` reuses a healthy BFF already
    // on :8787 (e.g. a standalone `camerata-server`), otherwise spawns the resolved
    // binary, waits for /api/health, and reclaims the port from unhealthy squatters
    // (the old PORT-TAKEOVER retry loop, preserved in `server_process`).  The runtime
    // lives only for the launch sequence; the guard is parked for the app's lifetime
    // and the spawned child is SIGTERMed on app exit by the watchdog subprocess
    // (tao exits via `process::exit`, so Drop alone would leak the server).
    // The macOS clipboard bridge: Cmd-C/X/V handled via page JS + Rust +
    // pbcopy/pbpaste, independent of the (historically unreliable) native
    // menu/responder-chain dispatch. No-op on other platforms. See
    // desktop_chrome::use_clipboard_bridge and
    // docs/design/2026-07-28_desktop-copy-paste-fix.md.
    desktop_chrome::use_clipboard_bridge();

    // Self-driving clipboard diagnostic: `CAMERATA_CLIPBOARD_PROBE=1 cargo run
    // -p camerata-ui` mechanically exercises Cmd-C/V/X/A inside the LIVE
    // cockpit and prints a PROBE RESULT verdict. No-op without the env var.
    camerata_ui::clipboard_probe::arm_if_requested();

    use_hook(|| {
        std::thread::spawn(|| match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(async {
                let cfg = server_process::ServerLaunchConfig::for_addr(BFF_ADDR);
                match server_process::ensure_server_running(&cfg).await {
                    Ok(guard) => server_process::hold_for_app_lifetime(guard),
                    // ensure_server_running already exhausted the takeover retries;
                    // its error carries the loud FATAL banner.  Print and give up —
                    // the cockpit will show its connection-health toasts.
                    Err(e) => eprintln!("{e}"),
                }
            }),
            Err(e) => eprintln!("[camerata-ui] could not start BFF launcher runtime: {e}"),
        });
    });

    // Global ref-counted in-flight loading count.  Any component or async
    // helper that holds a `loading::LoadingGuard` for its duration
    // increments this; the background Bombe machine watches it and activates
    // animations while count > 0.  Provided BEFORE the BombeBg mount so the
    // context is available when BombeBg first renders.
    loading::provide_loading_context();

    // App-wide toast stack, shared via context so any component can push
    // notifications/errors. The ConnectionWatcher below seeds it from the
    // integration health probe.
    let toasts = use_signal(Vec::<toast::Toast>::new);
    use_context_provider(|| toasts);

    // Ask-a-finding (#54): lifted to App so both CockpitApp (writer, inside
    // CockpitShell) and ChatBubble (reader, sibling of CockpitShell) share the
    // same signal. CockpitApp detects this via `try_consume_context` and skips
    // its own `use_context_provider` call when the parent already provides it.
    let ask_finding = use_signal(|| Option::<chat::FindingContext>::None);
    use_context_provider(|| ask_finding);

    // Governance rules catalog (the chat assistant's Layer-2 context) fetched ONCE at app scope
    // and shared via context, so it is available to the chat anywhere in the app and SURVIVES
    // the ChatBubble mounting/unmounting. Previously each ChatBubble fetched its own copy, so it
    // re-loaded on every open and could sit stuck on "Governance rules catalog (loading…)".
    let rules_catalog = use_resource(chat::fetch_rules_catalog);
    use_context_provider(|| rules_catalog);

    rsx! {
        // Global stylesheet, injected as a raw <style> so it works identically on
        // desktop without the asset pipeline. Keeps the whole look in one place.
        style { dangerous_inner_html: style::GLOBAL_CSS }

        // The Bombe machine background — fixed full-viewport layer at z-index 0,
        // pointer-events:none.  Activates .bombe-running (animations, higher opacity)
        // while the global loading count > 0.  Mounted BEFORE app-root so it paints
        // below the app shell (z-index 1).
        bombe_bg::BombeBg {}

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
        // Receives the ask-a-finding signal: when any FindingsTable row's "Ask AI"
        // button fires, it writes a FindingContext here and the panel auto-opens
        // in Project mode focused on that finding.
        chat::ChatBubble {
            finding: ask_finding(),
            pulled_issues_section: cockpit::pulled_issues_chat_section(),
        }
        // The in-app terminal: a floating PTY-backed shell panel with tab support.
        // FAB sits to the LEFT of the chat FAB so both are reachable without overlap.
        terminal::TerminalBubble {}
    }
}
