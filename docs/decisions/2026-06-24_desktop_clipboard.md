# Desktop clipboard (copy / cut / paste / select-all) on macOS

**Date:** 2026-06-24
**Status:** Accepted (verify interactively)
**Context:** Cmd-C / X / V / A did not work anywhere in the running macOS desktop app, despite an existing App+Edit native menu (`app_menu_bar`, commit db45d36). That menu had been in every build for ~5 days and was ineffective, so the prior fix was confirmed broken, not merely missing.

## Root cause

`wry 0.53.5`'s `WryWebViewParent::keyDown:` forwards every key-down to `NSApp.mainMenu().performKeyEquivalent()` and then **drops** the event — it never calls `interpretKeyEvents:` or forwards unhandled events to the `WKWebView`. So WKWebView's built-in clipboard handling (which fires only when it is first responder *and* the responder chain finds `copy:`/`cut:`/`paste:`/`selectAll:`) is fragile under the bare-binary launch (`cargo run`), and the native shortcuts fire inconsistently. (Upstream: wry#1711.)

## Fix (two channels)

1. **JS clipboard shim** (`CLIPBOARD_SHIM_SCRIPT`, injected via `Config::with_custom_head`): a `keydown` capture-phase listener that runs `document.execCommand("copy"|"cut"|"selectAll")` for Cmd/Ctrl-C/X/A. WKWebView always delivers `keydown` to JS regardless of native responder state, and `execCommand` from inside the event handler needs no user-gesture grant. This makes **copy / cut / select-all reliable**.
2. **Corrected native menu** (`app_menu_bar`): App submenu first (bold app name), then a Window submenu registered via `set_as_windows_menu_for_nsapp()` (macOS-guarded), then a full Edit submenu of predefined items. `Config::with_menu` registers it as `NSApp`'s main menu via `muda::Menu::init_for_nsapp()`, which is the path Cmd-V (paste) must travel.

## Paste is the hard part

`paste` is **deliberately excluded from the JS shim**: WebKit blocks `document.execCommand("paste")` and `navigator.clipboard.readText()` requires a permission the WKWebView does not grant injected scripts. **Paste therefore depends entirely on the native menu path** (`performKeyEquivalent:` → `paste:` selector → WKWebView). The corrected menu structure above is intended to make that path reliable, but because clipboard behavior in a webview cannot be verified headlessly, **paste must be confirmed interactively**. If it still fails on a future wry, the real fix is a one-line patch to `WryWebViewParent::keyDown:` to call `interpretKeyEvents:` after the menu (wry#1711), or a wry clipboard-permission handler.

## Verify (interactive — required)

1. Fully quit + rebuild + relaunch (clean restart so the new binary runs).
2. Select text in a finding row / the chat box → **Cmd-C**, then **Cmd-V** into the filter or chat input. Also test **Cmd-X** and **Cmd-A**.
3. Copy/cut/select-all should be reliable (JS shim). If paste still misbehaves, it's the wry keyDown path — escalate to the wry#1711 workaround.
