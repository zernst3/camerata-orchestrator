//! Server functions: the ONLY way this app's frontend reaches the server (RUST-DIOXUS-9).
//! The `#[server]` macro generates both sides from one definition — a network call
//! over the wire on the wasm client, an in-process call on the server during SSR —
//! so there is exactly one source of truth per operation and no separate REST/fetch
//! client to keep in sync. Add new server-reachable operations here, never as a
//! direct `fetch`/HTTP call in `components`/`pages`.

use dioxus::prelude::*;

/// A trivial demo call proving the server-function wiring end to end: the `Home`
/// page's button calls this, which runs on the server and returns a greeting.
/// Replace or remove once the app has real server-side operations.
#[server]
pub async fn greet(name: String) -> Result<String, ServerFnError> {
    let name = if name.trim().is_empty() { "there" } else { name.trim() };
    Ok(format!("Hello, {name} — the server said hi back."))
}
