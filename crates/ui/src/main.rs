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
                .with_menu(app_menu_bar())
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

/// Google Fonts <link> tags injected into <head> so that "Courier Prime"
/// (title + monospace) and "Inter" (sans body) are available via the CDN.
/// These are loaded before the clipboard shim and before the global stylesheet
/// so the fonts are already resolving when layout paint fires.
///
/// Both families are variable-weight subsets served from fonts.googleapis.com.
/// Courier Prime (400/700 roman only — it has no variable axis) is loaded
/// alongside Inter's variable range (100..900 wght).
const GOOGLE_FONTS_LINK: &str = r#"<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Courier+Prime:wght@400;700&family=Inter:wght@100..900&display=swap">"#;

/// JavaScript injected into <head> on every page load.
///
/// WHY THIS EXISTS
/// ---------------
/// wry 0.53.5 ships `WryWebViewParent::keyDown:` which unconditionally forwards
/// every key-down event to `NSApp.mainMenu().performKeyEquivalent()` and then drops
/// the event — it never calls `interpretKeyEvents:` or forwards unhandled events to
/// the WKWebView.  For Cmd-C/X/A, WKWebView handles these natively when it is the
/// first responder *and* the responder chain finds `copy:`/`cut:`/`selectAll:` on it.
/// In practice, with the bare-binary launch (`cargo run`), the first-responder focus
/// and responder-chain delivery are fragile enough that WKWebView's built-in path
/// fires inconsistently.
///
/// The JS `keydown` path is a separate, always-reliable channel: WKWebView delivers
/// `keydown` events to JavaScript regardless of native responder-chain state.
/// `document.execCommand('copy'/'cut'/'selectAll')` works from a JS event handler
/// without any user-gesture restriction because the event itself is the gesture.
///
/// PASTE IS EXCLUDED
/// -----------------
/// `document.execCommand('paste')` is intentionally blocked by WebKit security
/// policy: a page cannot read the clipboard programmatically without a native
/// permission prompt.  `navigator.clipboard.readText()` requires the Clipboard
/// Permission API, which WKWebView does not grant to injected scripts.  Paste
/// therefore relies entirely on the native menu path: `NSApp.mainMenu()
/// .performKeyEquivalent()` → `paste:` selector → WKWebView responder.  If that
/// path remains broken in a future wry release, the correct fix is a one-line patch
/// to `WryWebViewParent::keyDown:` to call `interpretKeyEvents:` after forwarding to
/// the menu (tracked in wry#1711).
///
/// CROSS-PLATFORM SAFETY
/// ----------------------
/// The script guards on `navigator.platform` / `e.metaKey` (macOS Command key).  On
/// Windows/Linux where Ctrl is the modifier, `e.ctrlKey` is also handled so the shim
/// remains useful if the app is ever built there.  `execCommand` is a no-op when no
/// text is selected, so there is no visible side-effect from the listener firing in
/// neutral state.
const CLIPBOARD_SHIM_SCRIPT: &str = r#"<script>
(function () {
  "use strict";
  // Guard: only run once even if the head is injected multiple times.
  if (window.__camerataClipboardShimInstalled) return;
  window.__camerataClipboardShimInstalled = true;

  document.addEventListener("keydown", function (e) {
    // macOS Command key OR Windows/Linux Ctrl key.
    var mod = e.metaKey || e.ctrlKey;
    if (!mod) return;

    switch (e.key) {
      case "c":
        // Cmd/Ctrl-C: copy selected text.
        document.execCommand("copy");
        break;
      case "x":
        // Cmd/Ctrl-X: cut selected text.
        document.execCommand("cut");
        break;
      case "a":
        // Cmd/Ctrl-A: select all content in the focused editable element.
        // We do NOT call e.preventDefault() here so that the browser's
        // default select-all (which works across a wider set of elements)
        // also runs; execCommand fires first for elements that support it.
        document.execCommand("selectAll");
        break;
      // "v" (paste) intentionally omitted — cannot be done safely from JS.
      // See the PASTE IS EXCLUDED comment above.
    }
  }, /* useCapture = */ true);
}());
</script>"#;

/// Build the application menu bar.
///
/// A complete, correctly-structured macOS menu bar is required for the native
/// paste path (Cmd-V).  The flow is:
///
///   1. User presses Cmd-V
///   2. tao's TaoApp.sendEvent: → [super sendEvent:] (standard NSApp)
///   3. NSApp checks mainMenu.performKeyEquivalent: → finds Cmd-V "Paste" item
///   4. Fires paste: selector → responder chain → WKWebView.paste: → pastes
///
/// wry's WryWebViewParent.keyDown: is a parallel path that also calls
/// mainMenu.performKeyEquivalent: for any key events that bubble past the webview.
/// Both paths require this menu to be registered as NSApp's main menu, which
/// Dioxus does via muda::Menu::init_for_nsapp() inside Config::with_menu().
///
/// STRUCTURE
/// ---------
/// On macOS the first submenu is the "application menu" (shown bold in the menu
/// bar as the app name).  We name it "Camerata Orchestrator" so it displays
/// correctly when running as a bare binary without an app bundle.  Then a Window
/// submenu (matching what dioxus_desktop::menubar::default_menu_bar() emits, and
/// registered via set_as_windows_menu_for_nsapp() as AppKit expects), then a full
/// Edit submenu with every standard text-editing predefined item.
///
/// CROSS-PLATFORM
/// --------------
/// PredefinedMenuItem items are the same on Windows/Linux (they use whatever the
/// platform's default for cut/copy/paste is), so this is safe to ship as-is.
/// The set_as_windows_menu_for_nsapp() call is guarded by #[cfg(target_os = "macos")].
fn app_menu_bar() -> dioxus::desktop::muda::Menu {
    use dioxus::desktop::muda::{AboutMetadata, Menu, PredefinedMenuItem, Submenu};

    let menu = Menu::new();

    // --- App menu (FIRST submenu = the bold app-named slot on macOS) ---
    let app = Submenu::new("Camerata Orchestrator", true);
    let _ = app.append_items(&[
        &PredefinedMenuItem::about(
            None,
            Some(AboutMetadata {
                name: Some("Camerata Orchestrator".to_string()),
                ..Default::default()
            }),
        ),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ]);

    // --- Window menu (second submenu, registered with AppKit as the Window menu) ---
    let window = Submenu::new("Window", true);
    let _ = window.append_items(&[
        &PredefinedMenuItem::fullscreen(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::maximize(None),
        &PredefinedMenuItem::minimize(None),
        &PredefinedMenuItem::close_window(None),
    ]);

    // --- Edit menu (the part that drives the native paste: responder-chain action) ---
    let edit = Submenu::new("Edit", true);
    let _ = edit.append_items(&[
        &PredefinedMenuItem::undo(None),
        &PredefinedMenuItem::redo(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::select_all(None),
    ]);

    let _ = menu.append_items(&[&app, &window, &edit]);

    // Tell AppKit which submenu is the Window menu.  Required for AppKit to
    // auto-populate it with "Bring All to Front" and open-window entries.
    // Must be called after the submenu is appended to the menu AND after
    // init_for_nsapp() has been called (Dioxus calls that in Config::with_menu).
    // We call it here on the submenu object; muda resolves the actual NSMenu
    // instance via the MudaMenuDelegate id at call-time.  If init_for_nsapp()
    // hasn't fired yet, resolve_ns_menu_for_nsapp() returns None and this is a
    // harmless no-op — the call will happen again internally when needed.
    #[cfg(target_os = "macos")]
    window.set_as_windows_menu_for_nsapp();

    menu
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
