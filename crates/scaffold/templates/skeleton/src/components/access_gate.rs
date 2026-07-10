//! Minimal single-shared-passcode access lock (FOLD D — default-private skeleton
//! lock). Wraps the app's content and shows a passcode prompt instead until
//! unlocked, so a freshly scaffolded data app is never reachable by default (see
//! `AppRequirements::visibility` in the scaffolder; `src/app.rs` wires this in only
//! when `visibility` is `Private`, the default — this file does not exist at all in
//! a `Public` scaffold).
//!
//! Deliberately NOT a user system: one shared passcode for the whole app, no
//! accounts, no per-user credentials, no auth crate, no user table (see
//! `CONVENTIONS.md`). The passcode is verified server-side
//! (`src/server_fns.rs`'s `verify_access_code`, checked against the
//! `APP_ACCESS_CODE` env var); this component only holds the client-side prompt +
//! the "already unlocked this browser" convenience flag.
//!
//! # Unlock persistence, and why the server render always starts locked
//! The unlock flag lives in the browser's `localStorage`, read/written only on the
//! wasm client (`read_unlocked`/`persist_unlocked`'s wasm32 branch below) — never a
//! cookie the server-rendered pass can inspect. That is a deliberate
//! simplification (no cookie-parsing middleware, no new server-side dependency):
//! the native/SSR branch of `read_unlocked` always returns `false`, so the
//! pre-hydration HTML for an unauthenticated request never contains the gated
//! `children` — only the passcode prompt — and a returning, already-unlocked
//! visitor's browser flips the signal to `true` client-side right after hydration.
//! The visible cost is a brief locked-then-unlocked flash for a returning visitor;
//! that trade favors staying dependency-free over a perfectly smooth reload.
use dioxus::prelude::*;

use crate::components::{Button, ButtonVariant, Card, Field};
use crate::server_fns::verify_access_code;

/// Tiny inline-JS bridge to `window.localStorage`, via `wasm_bindgen`'s
/// `inline_js` (already a dependency for the wasm target — see `Cargo.toml`'s
/// `wasm-bindgen` entry) — no `web-sys`/`js-sys` addition needed for two one-line
/// calls, keeping this feature dependency-free.
#[cfg(target_arch = "wasm32")]
mod storage {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(inline_js = "
        export function camerata_read_unlock() {
            try { return window.localStorage.getItem('camerata_unlocked') === 'true'; }
            catch (e) { return false; }
        }
        export function camerata_write_unlock() {
            try { window.localStorage.setItem('camerata_unlocked', 'true'); }
            catch (e) {}
        }
    ")]
    extern "C" {
        pub fn camerata_read_unlock() -> bool;
        pub fn camerata_write_unlock();
    }
}

#[cfg(target_arch = "wasm32")]
fn read_unlocked() -> bool {
    storage::camerata_read_unlock()
}

#[cfg(target_arch = "wasm32")]
fn persist_unlocked() {
    storage::camerata_write_unlock();
}

#[cfg(not(target_arch = "wasm32"))]
fn read_unlocked() -> bool {
    // SSR always starts locked — see the module doc's "why the server render
    // always starts locked".
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_unlocked() {
    // No-op natively: there is no server-side session to persist into. The client
    // (wasm32) branch above is the only place the unlock flag is actually written.
}

#[derive(Props, Clone, PartialEq)]
pub struct AccessGateProps {
    children: Element,
}

/// Wraps `props.children`, rendering a passcode prompt instead until the visitor
/// submits the passcode configured in `APP_ACCESS_CODE` (checked server-side via
/// `verify_access_code`). See the module doc for the unlock-persistence design.
#[component]
pub fn AccessGate(props: AccessGateProps) -> Element {
    let mut unlocked = use_signal(read_unlocked);
    let mut code = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut pending = use_signal(|| false);

    if unlocked() {
        return rsx! { {props.children} };
    }

    rsx! {
        div { class: "access-gate",
            Card {
                h2 { "Enter passcode" }
                p {
                    class: "text-muted",
                    "This app is private — enter the shared passcode to continue."
                }
                Field {
                    label: "Passcode",
                    value: code(),
                    placeholder: "",
                    password: true,
                    oninput: move |v| {
                        error.set(None);
                        code.set(v);
                    },
                }
                Button {
                    variant: ButtonVariant::Primary,
                    disabled: pending(),
                    onclick: move |_| {
                        let submitted = code();
                        pending.set(true);
                        spawn(async move {
                            let result = verify_access_code(submitted).await;
                            pending.set(false);
                            match result {
                                Ok(true) => {
                                    persist_unlocked();
                                    unlocked.set(true);
                                }
                                Ok(false) => error.set(Some("Incorrect passcode.".to_string())),
                                Err(_) => error.set(Some(
                                    "Could not verify passcode — try again.".to_string(),
                                )),
                            }
                        });
                    },
                    if pending() { "Checking..." } else { "Unlock" }
                }
                if let Some(msg) = error() {
                    p { class: "access-gate__error", "{msg}" }
                }
            }
        }
    }
}
