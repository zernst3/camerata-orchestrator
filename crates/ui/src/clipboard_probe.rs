//! Self-driving runtime clipboard probe (macOS).
//!
//! Cmd-C/V/X/A in the desktop webview has regressed repeatedly (commits
//! db45d36, 8bccb18) and can only be judged at RUNTIME — a `cargo check`
//! proves nothing about AppKit event dispatch. This module drives the clipboard
//! mechanically inside a LIVE app process:
//!
//!   1. Synthesizes native Cmd-key / mouse `NSEvent`s and posts them to the
//!      probe's OWN process via `-[NSApplication postEvent:atStart:]`.
//!      In-process posting needs no Accessibility permission and enters
//!      `sendEvent:` exactly like a real keystroke (same menu / responder-chain
//!      / webview dispatch).
//!   2. Observes outcomes through `document::eval` (field values, selection)
//!      and the system pasteboard (`pbcopy` / `pbpaste`).
//!
//! Two entry points:
//!   - `examples/clipboard_probe.rs` — a minimal window with the same chrome as
//!     the shipping app (isolates the dispatch chain from app content).
//!   - `CAMERATA_CLIPBOARD_PROBE=1 cargo run -p camerata-ui` — runs the SAME
//!     sequence inside the full cockpit (temp inputs injected into the live
//!     DOM), so app-specific interference (listeners, overlays, CSS) is
//!     included. See `arm_if_requested`.
//!
//! Exit code 0 = all channels work; 1 = at least one failed; 2 = watchdog
//! timeout. A `PROBE RESULT` line is printed to stdout either way.
//!
//! Only SAFE objc2 APIs are used (event synthesis, posting, menu
//! introspection); the workspace forbids `unsafe_code`.

/// If `CAMERATA_CLIPBOARD_PROBE=1`, schedule the probe inside the running app.
/// Call from a component body (it is a hook). No-op otherwise / off-macOS.
pub fn arm_if_requested() {
    #[cfg(target_os = "macos")]
    {
        use dioxus::prelude::{spawn, use_hook};
        let armed = std::env::var("CAMERATA_CLIPBOARD_PROBE").map(|v| v == "1").unwrap_or(false);
        use_hook(move || {
            if armed {
                println!("[probe] CAMERATA_CLIPBOARD_PROBE=1 — probing the LIVE app in ~4s");
                install_watchdog();
                spawn(async move {
                    // Extra settling time: the real app loads fonts/CSS and
                    // spawns the BFF; none of that should matter, but the probe
                    // must observe the app in its steady state.
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    drive(None).await;
                });
            }
        });
    }
}

/// Kill switch so a wedged webview cannot hang a probe run forever.
#[cfg(target_os = "macos")]
pub fn install_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(90));
        println!("PROBE RESULT: TIMEOUT — webview/event loop never completed the probe");
        std::process::exit(2);
    });
}

/// Marker strings: alphanumeric + underscore only, so they survive shell
/// interpolation into pbcopy without quoting concerns.
#[cfg(target_os = "macos")]
const PASTE_MARKER: &str = "CAMERATA_PASTE_MARKER_7391";
#[cfg(target_os = "macos")]
const COPY_MARKER: &str = "CAMERATA_COPY_MARKER_7392";
#[cfg(target_os = "macos")]
const CUT_MARKER: &str = "CAMERATA_CUT_MARKER_7393";
#[cfg(target_os = "macos")]
const SELECTALL_MARKER: &str = "CAMERATA_SELECTALL_7394";
#[cfg(target_os = "macos")]
const CLIPBOARD_SENTINEL: &str = "CAMERATA_SENTINEL_UNTOUCHED";

/// The full scripted sequence. Must run on the main thread (dioxus desktop
/// drives spawned futures from the tao event loop), which is exactly what
/// `NSApplication` calls require.
///
/// `handler_input`: id of an extra input that carries a Dioxus `onkeydown`
/// handler (only the example can provide one — a JS-created element cannot have
/// a Dioxus listener). When present, paste is additionally tested there, which
/// exercises Dioxus's synchronous delegated-event round-trip.
#[cfg(target_os = "macos")]
pub async fn drive(handler_input: Option<&str>) {
    use dioxus::document;
    use std::time::Duration;

    // Ensure the probe inputs exist. In the example they are part of the rsx;
    // in the live app they are injected here, floated above every app overlay
    // (toast host is z-index 2147483000).
    let _ = document::eval(
        "if (!document.getElementById('probe')) { \
           var wrap = document.createElement('div'); \
           wrap.id = 'camerata-probe-wrap'; \
           wrap.style.cssText = 'position:fixed;top:8px;left:8px;z-index:2147483600;' + \
             'background:#fff;padding:8px;border:2px solid #f00;'; \
           ['probe', 'probe3'].forEach(function (id) { \
             var i = document.createElement('input'); \
             i.id = id; \
             i.style.cssText = 'display:block;width:340px;font-size:14px;margin:4px 0;'; \
             wrap.appendChild(i); \
           }); \
           document.body.appendChild(wrap); \
         } return true;",
    )
    .await;

    // Make sure OUR window is key and the webview is first responder — the
    // same state a user is in after clicking a text field.
    let desktop = dioxus::desktop::window();
    desktop.window.set_focus();
    let _ = desktop.webview.focus();
    let _ = document::eval("document.getElementById('probe').focus(); return true;").await;

    // Diagnostics: is the shim installed, and what does the main menu hold?
    let shim = eval_json("return window.__camerataClipboardShimInstalled === true;").await;
    println!("[probe] clipboard shim installed in page: {shim}");
    report_main_menu();

    // Key-delivery diagnostic: record every keydown the PAGE sees, so we can
    // tell "shortcut dead because JS never saw it" apart from "JS saw it but
    // execCommand failed".
    let _ = document::eval(
        "window.__probeKeys = []; \
         document.addEventListener('keydown', function(e){ \
           window.__probeKeys.push((e.metaKey ? 'Cmd+' : '') + e.key); \
         }, true); return true;",
    )
    .await;

    let mut failures: Vec<&'static str> = Vec::new();

    // ── PASTE (Cmd-V) ───────────────────────────────────────────────────────
    set_pasteboard(PASTE_MARKER);
    let _ = document::eval(
        "var el = document.getElementById('probe'); el.value=''; el.focus(); return true;",
    )
    .await;
    post_cmd_key("v", 9, false).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let value = eval_json("return document.getElementById('probe').value;").await;
    let paste_ok = value.contains(PASTE_MARKER);
    println!("[probe] PASTE: field value after Cmd-V = {value:?}  -> {}", verdict(paste_ok));
    if !paste_ok {
        failures.push("paste");
    }

    // ── PASTE into a field WITH a Dioxus onkeydown handler (Cmd-V) ──────────
    if let Some(handler_id) = handler_input {
        set_pasteboard(PASTE_MARKER);
        let _ = document::eval(&format!(
            "var el = document.getElementById('{handler_id}'); el.value=''; el.focus(); \
             return true;"
        ))
        .await;
        post_cmd_key("v", 9, false).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let value =
            eval_json(&format!("return document.getElementById('{handler_id}').value;")).await;
        let ok = value.contains(PASTE_MARKER);
        println!(
            "[probe] PASTE-with-dioxus-onkeydown: field value after Cmd-V = {value:?}  -> {}",
            verdict(ok)
        );
        if !ok {
            failures.push("paste-with-dioxus-onkeydown");
        }
    }

    // ── PASTE after focusing by synthetic mouse CLICK (Cmd-V) ───────────────
    set_pasteboard(PASTE_MARKER);
    let _ = document::eval(
        "var el = document.getElementById('probe3'); el.value=''; el.blur(); return true;",
    )
    .await;
    let rect = eval_json(
        "var r = document.getElementById('probe3').getBoundingClientRect(); \
         return JSON.stringify({x: r.left + r.width / 2, y: r.top + r.height / 2, \
                                h: window.innerHeight});",
    )
    .await;
    click_at_page_point(&rect).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let focused =
        eval_json("return document.activeElement ? document.activeElement.id : '<none>';").await;
    println!("[probe] CLICK: activeElement after synthetic click = {focused}");
    post_cmd_key("v", 9, false).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let value = eval_json("return document.getElementById('probe3').value;").await;
    let paste3_ok = value.contains(PASTE_MARKER);
    println!(
        "[probe] PASTE-after-mouse-click: field value after Cmd-V = {value:?}  -> {}",
        verdict(paste3_ok)
    );
    if !paste3_ok {
        failures.push("paste-after-mouse-click");
    }

    // ── COPY (Cmd-C) ────────────────────────────────────────────────────────
    let _ = document::eval(&format!(
        "var el = document.getElementById('probe'); el.value='{COPY_MARKER}'; \
         el.focus(); el.select(); return true;"
    ))
    .await;
    set_pasteboard(CLIPBOARD_SENTINEL);
    post_cmd_key("c", 8, false).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let clip = get_pasteboard();
    let copy_ok = clip == COPY_MARKER;
    println!("[probe] COPY: pasteboard after Cmd-C = {clip:?}  -> {}", verdict(copy_ok));
    if !copy_ok {
        failures.push("copy");
    }

    // ── CUT (Cmd-X) ─────────────────────────────────────────────────────────
    let _ = document::eval(&format!(
        "var el = document.getElementById('probe'); el.value='{CUT_MARKER}'; \
         el.focus(); el.select(); return true;"
    ))
    .await;
    set_pasteboard(CLIPBOARD_SENTINEL);
    post_cmd_key("x", 7, false).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let clip = get_pasteboard();
    let left = eval_json("return document.getElementById('probe').value;").await;
    let cut_ok = clip == CUT_MARKER && !left.contains(CUT_MARKER);
    println!("[probe] CUT: pasteboard = {clip:?}, field left = {left:?}  -> {}", verdict(cut_ok));
    if !cut_ok {
        failures.push("cut");
    }

    // ── SELECT ALL (Cmd-A) ──────────────────────────────────────────────────
    let _ = document::eval(&format!(
        "var el = document.getElementById('probe'); el.value='{SELECTALL_MARKER}'; \
         el.focus(); el.setSelectionRange(0, 0); return true;"
    ))
    .await;
    post_cmd_key("a", 0, false).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let sel_len = eval_json(
        "var el = document.getElementById('probe'); \
         return String(el.selectionEnd - el.selectionStart);",
    )
    .await;
    let selectall_ok = sel_len == SELECTALL_MARKER.len().to_string();
    println!(
        "[probe] SELECT-ALL: selected length = {sel_len} (want {})  -> {}",
        SELECTALL_MARKER.len(),
        verdict(selectall_ok)
    );
    if !selectall_ok {
        failures.push("select-all");
    }

    // Key-delivery report.
    let keys = eval_json("return JSON.stringify(window.__probeKeys);").await;
    println!("[probe] keydowns the PAGE saw during the probe: {keys}");

    if failures.is_empty() {
        println!("PROBE RESULT: PASS — copy, paste, cut, select-all all work");
        std::process::exit(0);
    } else {
        println!("PROBE RESULT: FAIL — broken channels: {failures:?}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
fn verdict(ok: bool) -> &'static str {
    if ok {
        "OK"
    } else {
        "FAIL"
    }
}

/// Evaluate JS in the page and render the JSON result as a plain string.
#[cfg(target_os = "macos")]
async fn eval_json(script: &str) -> String {
    match dioxus::document::eval(script).await {
        Ok(v) => match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string(),
        },
        Err(e) => format!("<eval error: {e:?}>"),
    }
}

/// Put a marker on the system pasteboard (pbcopy = ground truth, no unsafe).
#[cfg(target_os = "macos")]
fn set_pasteboard(text: &str) {
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("printf %s {text} | pbcopy"))
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        println!("[probe] WARNING: pbcopy failed: {status:?}");
    }
}

/// Read the system pasteboard.
#[cfg(target_os = "macos")]
fn get_pasteboard() -> String {
    match std::process::Command::new("pbpaste").stdout(std::process::Stdio::piped()).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(e) => format!("<pbpaste error: {e}>"),
    }
}

/// Print what NSApp.mainMenu actually holds at runtime — hard evidence for
/// whether muda's `init_for_nsapp()` registered the menu bar.
#[cfg(target_os = "macos")]
fn report_main_menu() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(mtm) = MainThreadMarker::new() else {
        println!("[probe] WARNING: report_main_menu called off the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    match app.mainMenu() {
        None => println!("[probe] NSApp.mainMenu: NONE — menu bar was never registered"),
        Some(menu) => {
            let n = menu.numberOfItems();
            let mut titles = Vec::new();
            for i in 0..n {
                if let Some(item) = menu.itemAtIndex(i) {
                    titles.push(item.title().to_string());
                }
            }
            println!("[probe] NSApp.mainMenu: {n} items: {titles:?}");
        }
    }
}

/// Synthesize a left mouse click (down + up) at a PAGE-coordinate point.
/// `rect_json` is `{"x": css_px, "y": css_px, "h": window.innerHeight}`;
/// AppKit window coordinates are bottom-left-origin points, and the webview
/// fills the content view, so `y_window = innerHeight - y_page`.
#[cfg(target_os = "macos")]
async fn click_at_page_point(rect_json: &str) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::NSPoint;

    let parsed: serde_json::Value = match serde_json::from_str(rect_json) {
        Ok(v) => v,
        Err(e) => {
            println!("[probe] WARNING: could not parse rect {rect_json:?}: {e}");
            return;
        }
    };
    let (x, y, h) = (
        parsed["x"].as_f64().unwrap_or(0.0),
        parsed["y"].as_f64().unwrap_or(0.0),
        parsed["h"].as_f64().unwrap_or(0.0),
    );
    let point = NSPoint::new(x, h - y);

    let Some(mtm) = MainThreadMarker::new() else {
        println!("[probe] WARNING: click_at_page_point called off the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let window_number = app.keyWindow().map(|w| w.windowNumber()).unwrap_or(0);

    for (event_type, click_count) in [(NSEventType::LeftMouseDown, 1), (NSEventType::LeftMouseUp, 1)]
    {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            event_type,
            point,
            NSEventModifierFlags::empty(),
            0.0,
            window_number,
            None,
            0,
            click_count,
            1.0,
        );
        match event {
            Some(event) => app.postEvent_atStart(&event, false),
            None => println!("[probe] WARNING: could not synthesize {event_type:?}"),
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

/// Synthesize a native Cmd(+Shift)-<key> keystroke and post it to our own
/// process. `postEvent:atStart:` feeds the standard `sendEvent:` pipeline, so
/// menu key-equivalents, the responder chain, and webview delivery all behave
/// exactly as for a physical keystroke.
#[cfg(target_os = "macos")]
async fn post_cmd_key(key: &str, key_code: u16, shift: bool) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::{NSPoint, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        println!("[probe] WARNING: post_cmd_key called off the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let window_number = app.keyWindow().map(|w| w.windowNumber()).unwrap_or_else(|| {
        println!("[probe] WARNING: no key window; posting with windowNumber 0");
        0
    });

    let mut flags = NSEventModifierFlags::Command;
    if shift {
        flags |= NSEventModifierFlags::Shift;
    }
    let chars = NSString::from_str(key);

    for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            event_type,
            NSPoint::new(0.0, 0.0),
            flags,
            0.0,
            window_number,
            None,
            &chars,
            &chars,
            false,
            key_code,
        );
        match event {
            Some(event) => app.postEvent_atStart(&event, false),
            None => println!("[probe] WARNING: could not synthesize {event_type:?} for {key:?}"),
        }
    }
    // Yield so the event loop actually dispatches the posted events.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}
