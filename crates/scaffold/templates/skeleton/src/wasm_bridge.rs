//! Wasm-only bridge for the auto-capture error reporter (Camerata feedback loop,
//! `DefectSource::Auto`, `DefectKind::RuntimeError`).
//!
//! Installs a Rust panic hook that forwards the panic message to the JS-side
//! reporter (`assets/error-reporter.js`), which builds the actual
//! `DefectReport`-shaped JSON and POSTs it to `window.CAMERATA_CAPTURE_URL`. The
//! JS side also independently listens for `window.onerror`, `unhandledrejection`,
//! and failed `fetch` calls — this bridge covers the fourth source (a Rust panic
//! that `window.onerror` alone would only see as an opaque "unreachable executed").
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = __camerataReportPanic)]
    fn camerata_report_panic(message: &str, stack: &str);
}

/// Install the panic hook. Call once, before `dioxus::launch`, from the wasm
/// entrypoint in `main.rs`.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        // `PanicHookInfo::to_string()` gives the message + source location; a JS
        // stack isn't available from inside a Rust panic hook, so we pass an empty
        // stack and let the browser devtools (and the console listener installed
        // alongside this in error-reporter.js) supply the rest.
        camerata_report_panic(&info.to_string(), "");
    }));
}
