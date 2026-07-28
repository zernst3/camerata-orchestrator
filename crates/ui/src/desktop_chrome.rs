//! Desktop window "chrome": the native menu bar and the injected clipboard shim.
//!
//! This lives in the camerata-ui LIBRARY target (not the binary) so that the
//! runtime clipboard probe (`examples/clipboard_probe.rs`) can exercise the
//! EXACT same menu + head-script configuration the shipping app installs,
//! rather than a hand-copied approximation that could drift.
//!
//! History: Cmd-C/V/X/A in the desktop webview has been broken repeatedly on
//! macOS (commits db45d36, 8bccb18; docs/decisions/2026-06-24_desktop_clipboard.md).
//! The probe exists so any future regression can be reproduced and verified
//! mechanically (`cargo run -p camerata-ui --example clipboard_probe`) instead
//! of by hand-testing a GUI.

/// Google Fonts <link> tags injected into <head> so that "Courier Prime"
/// (title + monospace) and "Inter" (sans body) are available via the CDN.
/// These are loaded before the clipboard shim and before the global stylesheet
/// so the fonts are already resolving when layout paint fires.
///
/// Both families are variable-weight subsets served from fonts.googleapis.com.
/// Courier Prime (400/700 roman only — it has no variable axis) is loaded
/// alongside Inter's variable range (100..900 wght).
pub const GOOGLE_FONTS_LINK: &str = r#"<link rel="preconnect" href="https://fonts.googleapis.com">
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
/// The JS `keydown` path is a separate channel: WKWebView delivers `keydown`
/// events to JavaScript regardless of native responder-chain state.
/// `document.execCommand('copy'/'cut'/'selectAll')` works from a JS event handler
/// without any user-gesture restriction because the event itself is the gesture.
///
/// PASTE IS EXCLUDED
/// -----------------
/// `document.execCommand('paste')` is intentionally blocked by WebKit security
/// policy: a page cannot read the clipboard programmatically without a native
/// permission prompt.  `navigator.clipboard.readText()` requires the Clipboard
/// Permission API, which WKWebView does not grant to injected scripts.  Paste
/// therefore needs a Rust-assisted channel (`use_clipboard_bridge` below), and
/// the same bridge also takes over copy/cut on macOS so no clipboard action
/// depends on the fragile native dispatch chain.  See the design note at
/// docs/design/2026-07-28_desktop-copy-paste-fix.md.
///
/// CROSS-PLATFORM SAFETY
/// ----------------------
/// The script guards on `e.metaKey` (macOS Command key) / `e.ctrlKey`
/// (Windows/Linux), so the shim remains useful if the app is ever built there.
/// `execCommand` is a no-op when no text is selected, so there is no visible
/// side-effect from the listener firing in neutral state.  On macOS the Rust
/// clipboard bridge (when installed) owns Cmd-C/X/V, and this shim defers to it
/// (see the `bridged` guard) so the two channels can never double-act.
pub const CLIPBOARD_SHIM_SCRIPT: &str = r#"<script>
(function () {
  "use strict";
  // Guard: only run once even if the head is injected multiple times.
  if (window.__camerataClipboardShimInstalled) return;
  window.__camerataClipboardShimInstalled = true;

  document.addEventListener("keydown", function (e) {
    // macOS Command key OR Windows/Linux Ctrl key.
    var mod = e.metaKey || e.ctrlKey;
    if (!mod) return;

    // When the Rust clipboard bridge owns copy/cut (macOS, Cmd), defer to it
    // so the two channels can never double-act (a double "cut" would delete a
    // second time after the selection is gone).
    var bridged = window.__camerataClipboardBridgeInstalled === true && e.metaKey;

    switch (e.key) {
      case "c":
        // Cmd/Ctrl-C: copy selected text.
        if (!bridged) document.execCommand("copy");
        break;
      case "x":
        // Cmd/Ctrl-X: cut selected text.
        if (!bridged) document.execCommand("cut");
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
pub fn app_menu_bar() -> dioxus::desktop::muda::Menu {
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

/// The clipboard bridge (macOS): Cmd-C / Cmd-X / Cmd-V via Rust + `pb{copy,paste}`.
///
/// WHY A BRIDGE
/// ------------
/// The native clipboard path (menu-bar key equivalent → `copy:`/`paste:` →
/// responder chain → WKWebView) has repeatedly proven unreliable for REAL
/// keystrokes under the tao/wry/Dioxus bare-binary launch, even though every
/// piece looks correct (menu registered, webview focused — see
/// docs/design/2026-07-28_desktop-copy-paste-fix.md for the dispatch-chain
/// analysis and probe evidence).  And paste is impossible from page JS alone:
/// WebKit blocks `document.execCommand('paste')` and does not grant injected
/// scripts clipboard-read permission.
///
/// The bridge removes the native dispatch chain from the picture for all three
/// clipboard actions.  Its ONLY requirement is that page JS receives `keydown`
/// events — the same delivery that makes plain typing work:
///
///   1. A capture-phase `keydown` listener (installed through `document::eval`
///      so it owns a `dioxus.send` channel) intercepts plain Cmd-C/X/V.
///   2. COPY/CUT: JS reads the selected text itself (input/textarea selection
///      range, or `window.getSelection()` elsewhere) and sends it to Rust,
///      which writes the system pasteboard via `pbcopy`.  CUT also deletes the
///      selection with `execCommand('delete')` (undo-able, fires `input` so
///      Dioxus `oninput` signals stay in sync).  With NOTHING selected the
///      bridge stands aside entirely (no preventDefault), leaving the native
///      path and the head shim to their normal behavior.
///   3. PASTE: Rust reads the pasteboard via `pbpaste` and inserts at the caret
///      with `execCommand('insertText')` — works in `<input>`, `<textarea>`
///      and `contenteditable`, participates in the undo stack, and fires
///      `input` events.
///
/// When the bridge acts it calls `preventDefault()`, so a half-working native
/// path can never double-copy or double-paste; the head shim likewise defers
/// via the `__camerataClipboardBridgeInstalled` flag (a double CUT would
/// otherwise delete twice).
///
/// RESILIENCE
/// ----------
/// The listener is registered ONCE on `document` and calls the current
/// `window.__camerataClipboardBridgeSend` binding; each (re)install of the
/// eval refreshes that binding.  If the eval channel dies (reload, error), the
/// Rust loop reinstalls it and the listener picks up the fresh channel — the
/// bridge cannot silently die after the first page hiccup.  `pbcopy`/`pbpaste`
/// are macOS built-ins: ground-truth pasteboard access with no unsafe FFI and
/// no new dependencies (the workspace forbids `unsafe_code`).
///
/// CROSS-PLATFORM
/// --------------
/// macOS-only by `#[cfg]`: on Windows/Linux the webviews (WebView2/WebKitGTK)
/// handle Ctrl-C/X/V natively and `pbcopy`/`pbpaste` do not exist — installing
/// the bridge there would break working behavior.  The JS also only reacts to
/// plain `metaKey` combinations, never `ctrlKey`.
///
/// Call from a component body (it is a hook).
#[cfg(target_os = "macos")]
pub fn use_clipboard_bridge() {
    use dioxus::document;
    use dioxus::prelude::{spawn, use_hook};

    use_hook(|| {
        spawn(async move {
            loop {
                let mut listener = document::eval(CLIPBOARD_BRIDGE_JS);
                // When recv errors the channel died (navigation / reload /
                // eval error): fall out and reinstall to refresh the binding.
                while let Ok(msg) = listener.recv::<serde_json::Value>().await {
                    handle_bridge_message(&msg).await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        });
    });
}

/// No-op on non-macOS: the platform webviews handle Ctrl-C/X/V natively there.
#[cfg(not(target_os = "macos"))]
pub fn use_clipboard_bridge() {}

/// The page-side half of the bridge.  Kept as a named constant so tests can
/// assert the coupling invariants (install flag shared with the head shim,
/// send-binding refresh, capture phase).
#[cfg(any(target_os = "macos", test))]
const CLIPBOARD_BRIDGE_JS: &str = r#"
    // Refresh the send binding on every (re)install so the persistent listener
    // below always talks to the LIVE eval channel.
    window.__camerataClipboardBridgeSend = function (payload) {
        try { dioxus.send(payload); } catch (err) { /* channel mid-reinstall */ }
    };

    if (!window.__camerataClipboardBridgeInstalled) {
        window.__camerataClipboardBridgeInstalled = true;

        var selectedText = function () {
            var el = document.activeElement;
            if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA")) {
                if (el.selectionStart == null || el.selectionEnd == null) return "";
                return el.value.substring(el.selectionStart, el.selectionEnd);
            }
            var s = window.getSelection();
            return s ? s.toString() : "";
        };

        document.addEventListener("keydown", function (e) {
            // Plain Cmd only — Cmd+Shift/Alt/Ctrl combos are other shortcuts.
            if (!e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;

            if (e.key === "v") {
                e.preventDefault();
                window.__camerataClipboardBridgeSend({ op: "paste" });
            } else if (e.key === "c" || e.key === "x") {
                var text = selectedText();
                if (!text) return; // nothing selected: leave the native path alone
                e.preventDefault();
                if (e.key === "x") {
                    // Undo-able deletion that fires `input` (Dioxus stays in sync).
                    document.execCommand("delete");
                }
                window.__camerataClipboardBridgeSend({ op: "copy", text: text });
            }
        }, /* useCapture = */ true);
    }

    // Keep this eval (and its dioxus.send channel) alive until it is replaced.
    await new Promise(function () {});
"#;

/// Dispatch one message from the page-side bridge.
#[cfg(target_os = "macos")]
async fn handle_bridge_message(msg: &serde_json::Value) {
    match msg.get("op").and_then(|v| v.as_str()) {
        Some("paste") => insert_pasteboard_text().await,
        Some("copy") => {
            let text = msg.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !text.is_empty() {
                write_system_pasteboard(text);
            }
        }
        other => eprintln!("[camerata-ui] clipboard bridge: unknown message op {other:?}"),
    }
}

/// Write text to the system pasteboard via `pbcopy` (stdin, so arbitrary
/// content — quotes, newlines, unicode — round-trips exactly).
#[cfg(target_os = "macos")]
fn write_system_pasteboard(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let child = Command::new("pbcopy").stdin(Stdio::piped()).spawn();
    match child {
        Ok(mut child) => {
            if let Some(stdin) = child.stdin.as_mut() {
                if let Err(e) = stdin.write_all(text.as_bytes()) {
                    eprintln!("[camerata-ui] clipboard bridge: pbcopy write failed: {e}");
                }
            }
            // pbcopy exits as soon as stdin closes; this wait is momentary.
            let _ = child.wait();
        }
        Err(e) => eprintln!("[camerata-ui] clipboard bridge: could not spawn pbcopy: {e}"),
    }
}

/// Read the system pasteboard and insert it at the caret of the focused
/// element.  `pbpaste` is the ground-truth reader (same data a native paste
/// would insert).
#[cfg(target_os = "macos")]
async fn insert_pasteboard_text() {
    use dioxus::document;

    let output = std::process::Command::new("pbpaste")
        .stdout(std::process::Stdio::piped())
        .output();
    let text = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        other => {
            eprintln!("[camerata-ui] paste bridge: pbpaste failed: {other:?}");
            return;
        }
    };
    if text.is_empty() {
        return;
    }
    match js_insert_text_stmt(&text) {
        Some(stmt) => {
            let _ = document::eval(&stmt).await;
        }
        None => eprintln!("[camerata-ui] paste bridge: could not encode clipboard text"),
    }
}

/// Build the `insertText` eval statement for a piece of clipboard text.
/// serde_json turns the text into a correctly-escaped JS string literal
/// (quotes, newlines, unicode), so arbitrary clipboard content is inserted
/// verbatim rather than interpreted as script.  Pure, so it is unit-testable.
#[cfg(any(target_os = "macos", test))]
fn js_insert_text_stmt(text: &str) -> Option<String> {
    serde_json::to_string(text)
        .ok()
        .map(|literal| format!("document.execCommand('insertText', false, {literal}); return true;"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_text_stmt_escapes_quotes_and_newlines() {
        let stmt = js_insert_text_stmt("line1\n\"quoted\" 'single'").expect("encodes");
        // The JSON literal keeps the newline escaped and the double quotes
        // backslash-escaped, so the statement stays a single valid JS string.
        assert!(stmt.contains(r#""line1\n\"quoted\" 'single'""#), "stmt = {stmt}");
        assert!(stmt.starts_with("document.execCommand('insertText', false, "));
    }

    #[test]
    fn insert_text_stmt_neutralizes_script_close_tag() {
        // The eval runs as JS (not parsed as HTML), but the content must still
        // land inside ONE string literal even if it contains angle brackets.
        let stmt = js_insert_text_stmt("</script><b>x</b>").expect("encodes");
        assert!(stmt.contains(r#""</script><b>x</b>""#), "stmt = {stmt}");
    }

    #[test]
    fn shim_defers_copy_and_cut_to_the_bridge() {
        // The head shim and the bridge coordinate through this exact flag; if
        // either side renames it the double-cut protection silently dies.
        assert!(CLIPBOARD_SHIM_SCRIPT.contains("__camerataClipboardBridgeInstalled"));
        assert!(CLIPBOARD_BRIDGE_JS.contains("__camerataClipboardBridgeInstalled"));
    }

    #[test]
    fn bridge_listener_survives_channel_reinstall() {
        // The persistent listener must call the refreshable send BINDING, not
        // a captured `dioxus.send` (which dies with its eval channel).
        assert!(CLIPBOARD_BRIDGE_JS.contains("window.__camerataClipboardBridgeSend = function"));
        assert!(CLIPBOARD_BRIDGE_JS.contains("__camerataClipboardBridgeSend({ op:"));
    }

    #[test]
    fn bridge_only_reacts_to_plain_cmd() {
        // Cmd+Shift-V / Cmd+Alt-anything are OTHER shortcuts; the bridge must
        // not swallow them.
        assert!(CLIPBOARD_BRIDGE_JS.contains("if (!e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;"));
    }
}
