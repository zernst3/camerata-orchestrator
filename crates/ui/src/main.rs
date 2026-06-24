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
pub mod md;
mod routines;
mod style;
mod terminal;
mod toast;
mod vcs_settings;
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
                // keeps the process alive and the embedded BFF bound on :8787).
                .with_close_behaviour(WindowCloseBehaviour::WindowCloses)
                // Ensure the process exits when the last window closes.  The background
                // BFF thread is NOT detached, so process exit drops it; the :8787 bind
                // is released immediately.  Without this, a stale server shadows the
                // freshly-built one on the next `cargo run`.
                .with_exits_when_last_window_closes(true)
                // JS shim for Cmd-C / Cmd-X / Cmd-A (belt-and-suspenders alongside the
                // native menu).  See docs/decisions/2026-06-24_desktop_clipboard.md.
                .with_custom_head(CLIPBOARD_SHIM_SCRIPT.to_string()),
        )
        .launch(App);
}

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
                    // Check whether the error looks like a port-already-in-use failure.
                    // `AddrInUse` surfaces as an `std::io::Error` whose kind is
                    // `AddrInUse`; the Display string always contains "address already
                    // in use" (Linux) or "Address already in use" (macOS) or the OS
                    // equivalent.  We match on the lowercase string to be portable.
                    let msg = e.to_string();
                    if msg.to_lowercase().contains("address already in use")
                        || msg.to_lowercase().contains("addr in use")
                    {
                        eprintln!(
                            "\n\
                             ╔══════════════════════════════════════════════════════════════╗\n\
                             ║  [camerata-ui] WARNING: :{} ALREADY IN USE               ║\n\
                             ║                                                              ║\n\
                             ║  A stale Camerata server from a previous run is still        ║\n\
                             ║  holding the port.  This build's code is NOT running —       ║\n\
                             ║  the cockpit is talking to the OLD server.                   ║\n\
                             ║                                                              ║\n\
                             ║  Fix: quit ALL Camerata instances, then relaunch.            ║\n\
                             ╚══════════════════════════════════════════════════════════════╝\n",
                            BFF_ADDR
                        );
                    } else {
                        eprintln!("[camerata-ui] embedded BFF exited: {e}");
                    }
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

    // Ask-a-finding (#54): lifted to App so both CockpitApp (writer, inside
    // CockpitShell) and ChatBubble (reader, sibling of CockpitShell) share the
    // same signal. CockpitApp detects this via `try_consume_context` and skips
    // its own `use_context_provider` call when the parent already provides it.
    let ask_finding = use_signal(|| Option::<chat::FindingContext>::None);
    use_context_provider(|| ask_finding);

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
