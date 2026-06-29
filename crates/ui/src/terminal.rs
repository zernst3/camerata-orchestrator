//! In-app terminal popup (issue #38).
//!
//! A floating FAB (`.term-fab`) to the LEFT of the chat FAB toggles a panel
//! (`.term-panel`) that supports multiple terminal tabs. Each tab is one PTY
//! session backed by `GET /api/terminal/ws?cwd=<abs-path>` on the embedded BFF.
//!
//! ## How the terminal renders
//!
//! Each tab renders its terminal via **xterm.js** loaded from a CDN (`jsdelivr`).
//! The JS is injected once (on first open) via `document::eval`; each new session
//! evaluates a per-session script that:
//!
//!   1. Creates `new Terminal()` and opens it in a stable `<div id="xterm-N">`.
//!   2. Opens `new WebSocket("ws://…/api/terminal/ws?cwd=…")`.
//!   3. Pipes `term.onData → ws.send` and `ws.onmessage.data → term.write`.
//!   4. Sends `{"resize":{"cols":N,"rows":M}}` on terminal resize (FitAddon).
//!
//! ## RUNTIME-TODO items (require live desktop / network)
//!
//! - **CDN availability**: xterm.js is loaded from jsdelivr. An offline machine or
//!   a strict Chromium/wry CSP will block it. Robust follow-up: vendor xterm.js as
//!   a local asset and serve it from the BFF or embed it as a `include_str!`.
//!
//! - **ws:// URL derivation**: the BFF URL is `http://127.0.0.1:8787`; we replace
//!   `http://` with `ws://` to form the WebSocket URL. This is correct for the
//!   embedded BFF but would need `https://` → `wss://` in a cloud-hosted scenario.
//!
//! - **FitAddon resize**: the `FitAddon.fit()` call resizes the terminal to fill
//!   its container. It fires on load and on a `ResizeObserver` callback. Tested
//!   only at runtime.
//!
//! - **PTY bridge**: the actual PTY is on the server side; keyboard input travels
//!   over WebSocket as plain text. Verify that arrow keys / special sequences
//!   (Ctrl-C, Tab completion) round-trip correctly in wry's webview.
//!
//! - **wry CSP**: wry 0.x may enforce a Content-Security-Policy that blocks
//!   inline `<script>` eval and CDN fetches. If xterm.js fails to load, check the
//!   webview console. The vendored-asset path (see above) sidesteps CDN CSP issues.

use dioxus::prelude::*;

/// One terminal tab (one ws session).
#[derive(Clone, PartialEq)]
struct TermTab {
    /// Stable numeric id — used as the DOM element id `xterm-N`.
    id: usize,
    title: String,
}

/// Counter for giving each tab a unique numeric id (monotonically increasing;
/// we never reuse an id so DOM div ids stay stable even after closing a tab).
static NEXT_TAB_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn next_tab_id() -> usize {
    NEXT_TAB_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Inject xterm.js + css from jsdelivr once, then resolve a promise so callers
/// can await readiness before instantiating `new Terminal()`.
///
/// RUNTIME-TODO: this uses `eval` which runs inside the wry webview. If the CDN
/// is unreachable or CSP blocks the request the `xterm` global will be absent
/// and the per-session init below will fail silently. Add an `onerror` handler
/// on the script tag to surface errors in the desktop console.
const XTERM_LOAD_SCRIPT: &str = r#"
(function() {
  if (window.__xtermLoaded) { return Promise.resolve(); }
  return new Promise(function(resolve, reject) {
    // xterm.css
    var link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = 'https://cdn.jsdelivr.net/npm/xterm@5.3.0/css/xterm.min.css';
    document.head.appendChild(link);

    // xterm.js
    var s = document.createElement('script');
    s.src = 'https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.min.js';
    s.onload = function() {
      // FitAddon (for resize-to-container)
      var f = document.createElement('script');
      f.src = 'https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.min.js';
      f.onload = function() {
        window.__xtermLoaded = true;
        resolve();
      };
      f.onerror = reject;
      document.head.appendChild(f);
    };
    s.onerror = reject;
    document.head.appendChild(s);
  });
})()
"#;

/// Per-session init: create a Terminal, attach to `#xterm-{id}`, open the ws.
/// `{id}` and `{ws_url}` are filled in by `make_session_script`.
///
/// RUNTIME-TODO: the `ws://127.0.0.1:8787` URL is the embedded BFF. If the BFF
/// is not yet ready when a tab opens, the ws connection will fail (retry not
/// implemented in v1 — close and reopen the tab).
fn make_session_script(id: usize, ws_url: &str) -> String {
    format!(
        r#"
(async function() {{
  var containerId = 'xterm-{id}';
  var el = document.getElementById(containerId);
  if (!el) {{ return; }}

  // Guard against double-init: onmounted AND tab-click both fire this script.
  // If already initialized, just re-focus so keystrokes land.
  if (window['__term_{id}']) {{
    try {{ window['__term_{id}'].term.focus(); }} catch (e) {{}}
    return;
  }}

  // Wait for xterm.js to load; surface a visible error if the CDN is blocked
  // (strict webview CSP / offline) instead of failing to a blank box.
  try {{
    await (window.__xtermLoadPromise || Promise.resolve());
  }} catch (e) {{
    el.textContent = 'Terminal could not load xterm.js (network/CSP blocked the CDN). ' + e;
    return;
  }}
  if (typeof Terminal === 'undefined') {{
    el.textContent = 'Terminal could not load: xterm.js unavailable (CDN blocked?).';
    return;
  }}

  var term = new Terminal({{
    cursorBlink: true,
    fontSize: 13,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    // Slightly translucent so the Bombe shows through (paired with the .term-panel
    // backdrop-blur in style.rs for legibility). allowTransparency lets the rgba background blend.
    allowTransparency: true,
    theme: {{
      background: 'rgba(27,26,24,0.86)',
      foreground: '#faf9f6',
      cursor: '#b35636',
    }},
  }});

  // FitAddon resizes the PTY to the container. The UMD global is `FitAddon`
  // (namespace) with a `FitAddon` class -> `new FitAddon.FitAddon()`. Optional:
  // if the addon didn't load, the terminal still works at a fixed size.
  var fitAddon = null;
  try {{
    if (window.FitAddon && window.FitAddon.FitAddon) {{
      fitAddon = new window.FitAddon.FitAddon();
      term.loadAddon(fitAddon);
    }}
  }} catch (e) {{ fitAddon = null; }}

  term.open(el);
  term.focus();
  if (fitAddon) {{ try {{ fitAddon.fit(); }} catch (e) {{}} }}

  var observer = new ResizeObserver(function() {{
    // Skip fitting while the panel is hidden (display:none collapses el to 0x0).
    // Fitting to a zero-size box corrupts xterm's geometry, leaving the terminal
    // blank/untypeable when the panel is shown again. offsetParent is null when
    // the element (or an ancestor) is display:none.
    if (el.offsetParent === null || el.clientWidth === 0 || el.clientHeight === 0) {{ return; }}
    if (fitAddon) {{ try {{ fitAddon.fit(); }} catch (e) {{}} }}
  }});
  observer.observe(el);

  // Open the WebSocket to the embedded BFF.
  var ws = new WebSocket('{ws_url}');
  ws.binaryType = 'arraybuffer';

  ws.onopen = function() {{
    // Send initial size so the PTY matches the rendered terminal.
    ws.send(JSON.stringify({{ resize: {{ cols: term.cols, rows: term.rows }} }}));
    term.focus();
  }};

  // PTY output -> xterm.js
  ws.onmessage = function(evt) {{
    if (typeof evt.data === 'string') {{
      term.write(evt.data);
    }} else {{
      term.write(new Uint8Array(evt.data));
    }}
  }};

  ws.onclose = function() {{ term.write('\r\n[session closed]\r\n'); }};
  ws.onerror = function() {{ term.write('\r\n[connection error - is the BFF running?]\r\n'); }};

  // xterm.js input -> ws
  term.onData(function(data) {{
    if (ws.readyState === WebSocket.OPEN) {{ ws.send(data); }}
  }});

  // Sync PTY geometry on resize.
  term.onResize(function(size) {{
    if (ws.readyState === WebSocket.OPEN) {{
      ws.send(JSON.stringify({{ resize: {{ cols: size.cols, rows: size.rows }} }}));
    }}
    if (fitAddon) {{ try {{ fitAddon.fit(); }} catch (e) {{}} }}
  }});

  // Store references for cleanup when the tab closes, and for re-fit/focus on reopen.
  window['__term_{id}'] = {{ term: term, ws: ws, observer: observer, fitAddon: fitAddon }};

  // Re-focus after a tick so the hidden xterm textarea reliably grabs keystrokes
  // (the webview can steal focus during mount).
  setTimeout(function() {{ try {{ term.focus(); }} catch (e) {{}} }}, 60);
}})();
"#,
        id = id,
        ws_url = ws_url,
    )
}

/// Tear down one terminal session: close ws + dispose xterm instance.
fn make_cleanup_script(id: usize) -> String {
    format!(
        r#"
(function() {{
  var s = window['__term_{id}'];
  if (!s) {{ return; }}
  try {{ s.ws.close(); }} catch(e) {{}}
  try {{ s.observer.disconnect(); }} catch(e) {{}}
  try {{ s.term.dispose(); }} catch(e) {{}}
  delete window['__term_{id}'];
}})();
"#,
        id = id
    )
}

/// Re-fit + repaint + focus a terminal after the panel is shown again. Closing the panel
/// hides it (`display:none`), which collapses the xterm box to 0x0 and clears its canvas;
/// on reopen we must wait for the panel to actually have a non-zero layout, then re-fit so
/// the geometry is right, force a full `refresh()` so the screen buffer repaints (a plain
/// `fit()` no-ops if the dimensions didn't change, leaving the canvas blank), then re-focus
/// so keystrokes land. We poll a few animation frames because the `display:none` → visible
/// flip and Dioxus's re-render race this eval; a fixed `setTimeout` was flaky.
fn make_reveal_script(id: usize) -> String {
    format!(
        r#"
(function() {{
  var tries = 0;
  function attempt() {{
    var s = window['__term_{id}'];
    if (!s) {{ return; }}
    var el = document.getElementById('xterm-{id}');
    // Wait until the panel is actually laid out (not display:none, non-zero size).
    if (!el || el.offsetParent === null || el.clientWidth === 0 || el.clientHeight === 0) {{
      if (tries++ < 30) {{ requestAnimationFrame(attempt); }}
      return;
    }}
    try {{ if (s.fitAddon) s.fitAddon.fit(); }} catch (e) {{}}
    // Force a full repaint of the viewport — fit() alone won't redraw if rows/cols
    // are unchanged, so the canvas (cleared while hidden) would stay blank.
    try {{ s.term.refresh(0, s.term.rows - 1); }} catch (e) {{}}
    try {{ s.term.focus(); }} catch (e) {{}}
  }}
  requestAnimationFrame(attempt);
}})();
"#,
        id = id
    )
}

/// The terminal FAB + panel.
///
/// The FAB sits to the LEFT of the chat FAB (both bottom-right, staggered so they
/// don't overlap). CSS: `.term-fab` / `.term-panel` / `.term-tabs` in `style.rs`.
#[component]
pub fn TerminalBubble() -> Element {
    let mut open = use_signal(|| false);
    // Once the panel has been opened even once, we keep it MOUNTED for the rest of the
    // session and only toggle its visibility. Unmounting/remounting (the old approach)
    // destroyed the xterm DOM while the JS term/ws state lived on, so a reopen reattached
    // to dead nodes (blank, untypeable) — or, with display:none, collapsed the canvas to
    // 0x0 and cleared it. Keeping it mounted + hiding via `visibility:hidden` (which retains
    // the layout box, unlike display:none) keeps live shells fully rendered across reopen.
    let mut ever_opened = use_signal(|| false);
    let mut tabs = use_signal(Vec::<TermTab>::new);
    let mut active_tab = use_signal(|| 0usize);

    // Derive the WebSocket URL from BFF_URL: replace "http://" with "ws://".
    // RUNTIME-TODO: for a TLS-terminated cloud deployment this must be "wss://".
    let ws_base = crate::BFF_URL.replacen("http://", "ws://", 1);

    // Inject xterm.js from CDN on first open.
    let mut xterm_loaded = use_signal(|| false);

    rsx! {
        // ── FAB ─────────────────────────────────────────────────────────────
        button {
            class: "term-fab",
            title: "Terminal",
            onclick: move |_| {
                let opening = !open();
                open.set(opening);
                if opening { ever_opened.set(true); }
                if opening && tabs.read().is_empty() {
                    // Auto-open the first tab when the panel is first expanded.
                    let id = next_tab_id();
                    tabs.write().push(TermTab { id, title: format!("shell {id}") });
                    active_tab.set(id);
                }
                if opening && !xterm_loaded() {
                    // Inject xterm.js + CSS once into the webview.
                    // RUNTIME-TODO: this eval runs inside wry. CDN must be reachable
                    // and the webview CSP must allow script-src from jsdelivr.net.
                    xterm_loaded.set(true);
                    // We store the load promise on window so per-session scripts can
                    // await it, even if they fire before the CDN load finishes.
                    let load_script = format!(
                        "window.__xtermLoadPromise = {}",
                        XTERM_LOAD_SCRIPT
                    );
                    let _ = document::eval(&load_script);
                }
                if opening && !tabs.read().is_empty() {
                    // Reopening an existing session: the panel was hidden (display:none),
                    // which collapsed the terminal to 0x0. Re-fit + focus the active tab so
                    // it renders and accepts input again. (On the very first open the tab
                    // isn't initialized yet, so this no-ops and onmounted handles it.)
                    let _ = document::eval(&make_reveal_script(active_tab()));
                }
            },
            // Terminal glyph: a simple ">" prompt icon
            if open() { "✕" } else { ">_" }
        }

        // Once opened, the panel stays MOUNTED for the session; closing just hides it via
        // the `term-hidden` class (`visibility:hidden`, which keeps the layout box sized so
        // xterm's canvas never collapses or clears). This preserves live shells — including
        // their scrollback and running processes — across any number of close/reopen cycles.
        if ever_opened() {
            div {
                class: if open() { "term-panel" } else { "term-panel term-hidden" },
                // ── Tab bar ──────────────────────────────────────────────────
                div { class: "term-tabs",
                    for tab in tabs.read().clone() {
                        {
                            let tab_id = tab.id;
                            let is_active = active_tab() == tab_id;
                            let ws_url_for_new = format!("{ws_base}/api/terminal/ws");
                            rsx! {
                                button {
                                    key: "{tab_id}",
                                    class: if is_active { "term-tab active" } else { "term-tab" },
                                    onclick: move |_| {
                                        active_tab.set(tab_id);
                                        // If we're switching TO this tab, init xterm if not yet done.
                                        // (The DOM element exists now; the session script may already
                                        // have run — `window.__term_N` guards against double-init.)
                                        let ws_url_clone = ws_url_for_new.clone();
                                        let script = make_session_script(tab_id, &ws_url_clone);
                                        let _ = document::eval(&script);
                                    },
                                    span { class: "term-tab-label", "{tab.title}" }
                                    // Close button
                                    span {
                                        class: "term-tab-close",
                                        onclick: move |e| {
                                            e.stop_propagation();
                                            // Tear down the xterm instance + ws.
                                            let _ = document::eval(&make_cleanup_script(tab_id));
                                            tabs.write().retain(|t| t.id != tab_id);
                                            // If we closed the active tab, switch to the last remaining.
                                            if active_tab() == tab_id {
                                                let new_active = tabs.read().last().map(|t| t.id).unwrap_or(0);
                                                active_tab.set(new_active);
                                            }
                                        },
                                        "×"
                                    }
                                }
                            }
                        }
                    }
                    // "+" button to open a new tab
                    {
                        let ws_url_new = format!("{ws_base}/api/terminal/ws");
                        rsx! {
                            button {
                                class: "term-tab-add",
                                title: "New terminal",
                                onclick: move |_| {
                                    let id = next_tab_id();
                                    tabs.write().push(TermTab { id, title: format!("shell {id}") });
                                    active_tab.set(id);
                                    // Init the new session immediately.
                                    let script = make_session_script(id, &ws_url_new);
                                    let _ = document::eval(&script);
                                },
                                "+"
                            }
                        }
                    }
                }

                // ── Session panes ─────────────────────────────────────────────
                // All session divs are in the DOM; only the active one is visible
                // (display:block vs display:none). This lets xterm.js keep state
                // without re-initialising on every tab switch.
                div { class: "term-body",
                    for tab in tabs.read().clone() {
                        {
                            let tab_id = tab.id;
                            let is_active = active_tab() == tab_id;
                            let ws_url_for_mount = format!("{ws_base}/api/terminal/ws");
                            rsx! {
                                div {
                                    key: "{tab_id}",
                                    id: "xterm-{tab_id}",
                                    class: "term-session",
                                    style: if is_active { "display:block" } else { "display:none" },
                                    // Fire the xterm init script once this div is in the DOM.
                                    // `onmounted` gives us the "element is in the DOM" hook.
                                    onmounted: move |_| {
                                        let script = make_session_script(tab_id, &ws_url_for_mount);
                                        let _ = document::eval(&script);
                                    },
                                    // Clicking the pane re-focuses the terminal so keystrokes land.
                                    onclick: move |_| {
                                        let _ = document::eval(&format!(
                                            "try {{ window['__term_{tab_id}'].term.focus(); }} catch(e) {{}}"
                                        ));
                                    },
                                }
                            }
                        }
                    }
                    if tabs.read().is_empty() {
                        div { class: "term-empty",
                            "No terminal sessions. Press \"+\" to open one."
                        }
                    }
                }
            }
        }
    }
}
