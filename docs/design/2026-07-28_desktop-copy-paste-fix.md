# Desktop copy/paste: the clipboard bridge (and why the menu-only fix kept failing)

**Date:** 2026-07-28
**Status:** Shipped (mechanically verified via runtime probe; one manual smoke test requested below)
**Supersedes:** `docs/decisions/2026-06-24_desktop_clipboard.md` (that note stays as history)

## Symptom

Cmd-C / Cmd-V / Cmd-X / Cmd-A appear to do nothing in text fields of the macOS
desktop cockpit (`crates/ui`, Dioxus desktop = tao 0.34.8 + wry 0.53.5 +
muda 0.17.2 on dioxus-desktop 0.7.9). Recurring: fixed twice before, reported
broken again.

## Prior attempts and why they did not stick

| Commit | Attempt | Outcome |
|---|---|---|
| `db45d36` (2026-06-19) | Explicit App + Window + Edit native menu bar (`PredefinedMenuItem::{cut,copy,paste,select_all}` → standard `copy:`/`paste:`/… selectors) | Reported still broken after ~5 days in every build |
| `8bccb18` (2026-06-24) | JS head shim (`execCommand("copy"/"cut"/"selectAll")` on Cmd-C/X/A keydown) + corrected menu structure for the paste path | Paste left dependent on the native menu path, explicitly "verify interactively" — and the whole set was reported broken again |

Both attempts bet on the **native dispatch chain**: key event → `NSApp.sendEvent:`
→ window `performKeyEquivalent:` → WKWebView → (if unhandled by the page) WebKit
re-dispatch → `mainMenu.performKeyEquivalent:` → `paste:` action → responder
chain → `WKWebView.paste:`. Every hop must cooperate, several are owned by
third parties (tao's `TaoApp.sendEvent:` override, wry's `WryWebViewParent.keyDown:`
which forwards to the menu and **drops** the event — wry#1711, WebKit's
unhandled-key-equivalent resend), and none of it is observable in CI.

## What the investigation actually established

A new **self-driving runtime probe** (`crates/ui/src/clipboard_probe.rs`) posts
synthetic native keystrokes to its own process — `NSEvent::keyEventWithType…`
+ `-[NSApplication postEvent:atStart:]` need **no Accessibility permission**
in-process and enter `sendEvent:` exactly like real keys — then checks outcomes
via `document::eval` and `pbcopy`/`pbpaste`. Findings at the locked versions:

- **The menu IS registered.** `NSApp.mainMenu` = 3 items
  `["Camerata Orchestrator", "Window", "Edit"]` (muda `init_for_nsapp()` via
  `Config::with_menu` works).
- **Cmd-key keydowns DO reach page JS** (capture-listener log sees
  `Cmd+v/c/x/a`).
- **The full chain passes under synthetic events** — paste included — in a
  minimal window with identical chrome, in a field with a Dioxus `onkeydown`
  handler (its synchronous delegated-event XHR does not break the chain), after
  focus-by-synthetic-mouse-click, and **inside the full live cockpit**
  (`CAMERATA_CLIPBOARD_PROBE=1`).

So no static misconfiguration remains, and the manual failure could not be
reproduced with in-process events. Real HID keystrokes can traverse WebKit's
key-equivalent/resend machinery differently than posted `NSEvent`s (they carry
CGEvent backing and arrive via the WindowServer), and generating real HID input
requires an Accessibility grant this environment does not have. Conclusion:
**stop betting on the native chain at all.** The one delivery channel proven
reliable whenever typing works — JS `keydown` in the webview — becomes the only
dependency.

## The fix: clipboard bridge (`desktop_chrome::use_clipboard_bridge`)

macOS-only (`#[cfg(target_os = "macos")]`), installed by `App` in
`crates/ui/src/main.rs`. No unsafe (workspace forbids it), no new dependencies.

- A capture-phase `keydown` listener is installed through `document::eval`, so
  it owns a `dioxus.send` channel to Rust. It intercepts **plain Cmd** C/X/V
  only (never Ctrl/Alt/Shift combos).
- **Copy/Cut:** JS reads the selection itself (input/textarea selection range,
  else `window.getSelection()`), sends the text to Rust, Rust writes the system
  pasteboard via **`pbcopy`** (stdin, exact round-trip). Cut additionally
  deletes the selection with `execCommand("delete")` (undo-able; fires `input`
  so Dioxus `oninput` signals stay in sync). Nothing selected → the bridge
  stands aside completely (no `preventDefault`).
- **Paste:** Rust reads the pasteboard via **`pbpaste`** and inserts with
  `execCommand("insertText", false, <json-escaped text>)` — works in `input`,
  `textarea`, `contenteditable`; participates in undo; fires `input`.
- When the bridge acts it calls `preventDefault()`, so a (sometimes-)working
  native path can never double-paste/double-copy; the head shim defers via the
  shared `window.__camerataClipboardBridgeInstalled` flag (a double CUT would
  otherwise delete twice).
- **Resilience:** the persistent listener calls the refreshable
  `window.__camerataClipboardBridgeSend` binding; the Rust loop reinstalls the
  eval if the channel dies (reload), so the bridge cannot silently die.
- Cmd-A stays with the head shim (`execCommand("selectAll")` + browser default)
  — pure JS, no native dependency to remove.
- The native menu bar **stays**: clickable Edit>Copy/Paste menu items dispatch
  actions directly (no key-equivalent hop) and the menu is required macOS
  chrome anyway. Windows/Linux are untouched: the bridge is a no-op there and
  the shim only adds `execCommand` on Ctrl-C/X/A as before.

## Files

- `crates/ui/src/desktop_chrome.rs` — menu + shim (moved from `main.rs`), the
  bridge, `js_insert_text_stmt` (unit-tested escaping), coupling tests.
- `crates/ui/src/clipboard_probe.rs` — the runtime probe (safe objc2 only).
- `crates/ui/src/lib.rs` — minimal lib target so the probe example and the bin
  share the EXACT shipping code.
- `crates/ui/examples/clipboard_probe.rs` — minimal-window probe harness.
- `crates/ui/src/main.rs` — installs the bridge; arms the in-app probe.
- `crates/ui/Cargo.toml` — macOS-only deps `objc2 0.6` / `objc2-foundation 0.3`
  / `objc2-app-kit 0.3` (already in-tree via tao/wry/muda; zero new transitive
  deps).

## Verification

Mechanical (all green, exit 0):

```sh
cargo test  -p camerata-ui --lib                      # bridge/shim unit tests
cargo run   -p camerata-ui --example clipboard_probe  # minimal window, fix ON
CAMERATA_PROBE_FIX=0 cargo run -p camerata-ui --example clipboard_probe  # baseline
CAMERATA_CLIPBOARD_PROBE=1 cargo run -p camerata-ui   # probe inside the LIVE cockpit
```

Probe matrix covered: paste into a plain field, paste into a field with a
Dioxus `onkeydown` handler, paste after focus-by-mouse-click, copy, cut,
select-all — each asserted against the real system pasteboard / DOM state, with
exactly-once semantics (no double insert/delete).

**NOT verified (requires a human):** a physical keystroke travels HID →
WindowServer → app; the probe cannot synthesize that without an Accessibility
grant. **Manual smoke test:** `cargo run -p camerata-ui`, click a text field
(e.g. the chat box), type, select (Cmd-A), Cmd-C, then Cmd-V — expect the text
to duplicate; also Cmd-V of text copied from another app.

## Residual uncertainty

- If the historical manual failures were caused by Cmd-key `keydown`s not
  reaching page JS under real HID input (never observed in any probe run, and
  incompatible with typing working), the bridge would not fire either. In that
  case the recorded keydown log from `CAMERATA_CLIPBOARD_PROBE=1` plus one
  manual keypress is the next diagnostic step.
- Copying text selected inside the xterm.js terminal panel uses xterm's own
  selection model (no DOM selection under the canvas renderer); the bridge
  stands aside there and the old behavior applies. Paste INTO the terminal goes
  through `insertText` → xterm's input pipeline. If terminal copy matters, wire
  `term.getSelection()` into the bridge as a follow-up.
- Undo/redo (Cmd-Z/Shift-Cmd-Z) still ride the native path; out of scope here.
