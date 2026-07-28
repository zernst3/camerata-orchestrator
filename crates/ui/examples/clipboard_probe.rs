//! Minimal-window clipboard probe: the dispatch chain in isolation.
//!
//! Launches a bare Dioxus desktop window configured with the EXACT same chrome
//! as the shipping app (`camerata_ui::desktop_chrome`: native menu bar +
//! clipboard shim) and runs `camerata_ui::clipboard_probe::drive` against it.
//! Because the window holds nothing but the probe inputs, a failure here
//! isolates the tao/wry/muda/Dioxus dispatch chain itself; to probe the FULL
//! cockpit instead, run `CAMERATA_CLIPBOARD_PROBE=1 cargo run -p camerata-ui`.
//!
//! ```sh
//! cargo run -p camerata-ui --example clipboard_probe        # probe with the fix
//! CAMERATA_PROBE_FIX=0 cargo run -p camerata-ui --example clipboard_probe
//! #   ^ disables the Cmd-V paste bridge, reproducing the pre-fix baseline
//! ```
//!
//! Exit code 0 = all clipboard channels work; 1 = at least one failed;
//! 2 = watchdog timeout (webview never came up).

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("clipboard_probe is macOS-only (the broken dispatch chain under test is AppKit's).");
}

#[cfg(target_os = "macos")]
fn main() {
    use camerata_ui::{clipboard_probe, desktop_chrome};
    use dioxus::desktop::{Config, WindowBuilder};
    use dioxus::prelude::*;

    clipboard_probe::install_watchdog();

    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                // Same chrome as the shipping app: the real menu bar and the
                // real clipboard shim, so the probe exercises the production
                // dispatch path, not an approximation.
                .with_menu(desktop_chrome::app_menu_bar())
                .with_window(WindowBuilder::new().with_title("Camerata Clipboard Probe"))
                .with_custom_head(desktop_chrome::CLIPBOARD_SHIM_SCRIPT.to_string()),
        )
        .launch(ProbeApp);

    #[component]
    fn ProbeApp() -> Element {
        // A/B switch: CAMERATA_PROBE_FIX=0 runs WITHOUT the paste bridge — the
        // pre-fix baseline that reproduces the bug. Default is fix ON.
        let fix_enabled = std::env::var("CAMERATA_PROBE_FIX").map(|v| v != "0").unwrap_or(true);
        println!("[probe] paste-bridge fix enabled: {fix_enabled}");
        if fix_enabled {
            camerata_ui::desktop_chrome::use_clipboard_bridge();
        }
        use_hook(|| {
            spawn(async move {
                // Let the webview finish loading and the window settle.
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                camerata_ui::clipboard_probe::drive(Some("probe2")).await;
            });
        });
        rsx! {
            div { style: "font-family: monospace; padding: 16px;",
                p { "Camerata clipboard probe — do not touch the keyboard/mouse for ~15s." }
                input { id: "probe", style: "width: 90%; font-size: 16px;", autofocus: true }
                // Mimics the real app's chat/design textareas: a field WITH a
                // Dioxus onkeydown handler. Dioxus delegates the event through a
                // SYNCHRONOUS XHR to Rust — this tests whether that round-trip
                // breaks WebKit's unhandled-key-equivalent redispatch.
                input {
                    id: "probe2",
                    style: "width: 90%; font-size: 16px; margin-top: 8px;",
                    onkeydown: move |_| {},
                }
                // Focused by a SYNTHETIC MOUSE CLICK (the real user flow) rather
                // than programmatic focus, to test focus acquisition via click.
                input { id: "probe3", style: "width: 90%; font-size: 16px; margin-top: 8px;" }
            }
        }
    }
}
