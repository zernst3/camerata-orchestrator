//! Settings → Credentials panel.
//!
//! App-wide, not per-project. Lets the user enter their OpenRouter API key and
//! GitHub token once; the backend writes them to the OS keychain. The full value
//! is never echoed back — only the masked form (first 4 chars + `••••`) is
//! displayed after saving.
//!
//! Rendered as a tab inside [`crate::cockpit::CockpitApp`] (see `CockpitView::Credentials`).

use dioxus::prelude::*;

use camerata_ui_core::llm_backend::{show_api_key_warning, LlmBackend};

use crate::loading::{BombeEnabled, BombePreview};
use crate::toast::{push_toast, ToastKind};

// ── localStorage key for the bombe animation toggle ──────────────────────────
const BOMBE_ENABLED_KEY: &str = "camerata.bombe.enabled";

// ── Known credential names (mirrors crate::credentials consts) ───────────────

const OPENROUTER_API_KEY: &str = "openrouter_api_key";
const GITHUB_TOKEN: &str = "github_token";
/// The Anthropic API key credential. NOT in the always-shown [`ALL_CREDENTIALS`] list —
/// it's revealed contextually inside [`ModelBackendSettings`] only when the `api` Claude
/// backend is selected.
const ANTHROPIC_API_KEY: &str = "anthropic_api_key";

const ALL_CREDENTIALS: &[(&str, &str)] = &[
    (OPENROUTER_API_KEY, "OpenRouter API Key"),
    (GITHUB_TOKEN, "GitHub Token"),
];

// ── API types ─────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, serde::Deserialize)]
struct CredentialListItem {
    name: String,
    is_set: bool,
    masked: Option<String>,
}

// ── Fetch ─────────────────────────────────────────────────────────────────────

async fn fetch_credentials() -> Option<Vec<CredentialListItem>> {
    reqwest::get(format!("{}/api/credentials", crate::bff_base()))
        .await
        .ok()?
        .json::<Vec<CredentialListItem>>()
        .await
        .ok()
}

// ── LLM backend (Model backend) settings ────────────────────────────────────

/// The subset of `GET /api/settings` the Model-backend control reads: the EFFECTIVE backend
/// (stored setting > env > default `cli`) and whether an Anthropic API key is present.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct BackendSettingsView {
    #[serde(default)]
    llm_backend: Option<String>,
    #[serde(default)]
    api_key_present: bool,
}

/// Fetch the effective LLM backend + api-key presence from `GET /api/settings`.
async fn fetch_backend_settings() -> Option<BackendSettingsView> {
    reqwest::get(format!("{}/api/settings", crate::bff_base()))
        .await
        .ok()?
        .json::<BackendSettingsView>()
        .await
        .ok()
}

/// Persist the LLM backend via `POST /api/settings/llm-backend`. Returns the server-echoed
/// (backend, api_key_present) on success. `None` on any transport/parse error or non-2xx.
async fn set_backend(backend: LlmBackend) -> Option<(LlmBackend, bool)> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/settings/llm-backend", crate::bff_base()))
        .json(&serde_json::json!({ "backend": backend.as_wire() }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let backend = LlmBackend::parse(v.get("backend").and_then(|b| b.as_str()).unwrap_or("cli"));
    let api_key_present = v
        .get("api_key_present")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    Some((backend, api_key_present))
}

async fn post_credential(name: &str, value: &str) -> Result<String, String> {
    let body = serde_json::json!({ "value": value });
    let resp = reqwest::Client::new()
        .post(format!("{}/api/credentials/{name}", crate::bff_base()))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        let masked = json["masked"]
            .as_str()
            .unwrap_or("(saved)")
            .to_string();
        Ok(masked)
    } else {
        let msg = json["error"]
            .as_str()
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    }
}

// ── Component ─────────────────────────────────────────────────────────────────

/// The "Settings → Credentials" panel. Renders one row per known credential:
/// a label, a password input, and a Save button. When already set, shows the
/// masked value and grays out the input placeholder.
///
/// Also renders the Bombe animation settings section below the credentials.
#[component]
pub fn CredentialsSettings() -> Element {
    // Toast list from the app-wide context.
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // Fetch the current credential states from the backend.
    let mut creds_res = use_resource(fetch_credentials);

    rsx! {
        div { class: "credentials-panel",
            h2 { class: "credentials-title", "Settings" }
            p { class: "credentials-intro",
                "Keys are stored in the OS keychain — never in files or the repo. "
                "The full value is write-only: only the first 4 characters are shown after saving."
            }
            match &*creds_res.read() {
                None => rsx! {
                    p { class: "ink-soft", "Loading…" }
                },
                Some(None) => rsx! {
                    p { class: "ink-soft warn", "Could not reach the server." }
                },
                Some(Some(items)) => rsx! {
                    for (name, label) in ALL_CREDENTIALS.iter().copied() {
                        {
                            let item = items.iter().find(|i| i.name == name).cloned();
                            let is_set = item.as_ref().map(|i| i.is_set).unwrap_or(false);
                            let current_masked = item.and_then(|i| i.masked);
                            rsx! {
                                CredentialRow {
                                    key: "{name}",
                                    name: name.to_string(),
                                    label: label.to_string(),
                                    is_set,
                                    current_masked,
                                    toasts,
                                    on_saved: move |_| {
                                        creds_res.restart();
                                    },
                                }
                            }
                        }
                    }
                },
            }

            // ── Claude backend (CLI ⟷ API) ────────────────────────────────
            ModelBackendSettings {}

            // ── Bombe animation settings ──────────────────────────────────
            BombeSettings {}
        }
    }
}

// ── ModelBackendSettings ────────────────────────────────────────────────────

/// The "Claude backend" control: a CLI ⟷ API segmented toggle for how Claude runs.
///
/// - **CLI** runs Claude via the logged-in Claude Code subscription (no key).
/// - **API** runs Claude via the Anthropic API and requires an Anthropic API key.
///
/// Reads the current effective backend + key presence from `GET /api/settings`, and writes the
/// choice via `POST /api/settings/llm-backend`. When `api` is selected the Anthropic API key
/// input is revealed inline (reusing [`CredentialRow`], stored in the keychain under
/// `anthropic_api_key`). When `api` is selected with no key present the server silently falls
/// back to CLI, so an inline warning is shown until a key is saved (pure view logic lives in
/// `camerata_ui_core::llm_backend`).
#[component]
fn ModelBackendSettings() -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut settings_res = use_resource(fetch_backend_settings);
    // The credentials list drives the revealed Anthropic key row's is_set/masked state.
    let mut creds_res = use_resource(fetch_credentials);
    let mut saving = use_signal(|| false);

    // Snapshot the resource into an owned value so the read guard is dropped before the rsx
    // body (whose onclick closures call `settings_res.restart()`, which needs a fresh borrow).
    let snapshot = settings_res.read().clone();
    match snapshot {
        None => rsx! {
            div { class: "credentials-field-section",
                div { class: "credentials-field-header",
                    label { class: "credentials-label", "Claude backend" }
                }
                p { class: "ink-soft", "Loading…" }
            }
        },
        Some(None) => rsx! {
            div { class: "credentials-field-section",
                div { class: "credentials-field-header",
                    label { class: "credentials-label", "Claude backend" }
                }
                p { class: "ink-soft warn", "Could not reach the server." }
            }
        },
        Some(Some(view)) => {
            let selected = LlmBackend::parse(view.llm_backend.as_deref().unwrap_or("cli"));
            let api_key_present = view.api_key_present;
            let warn = show_api_key_warning(selected, api_key_present);

            let seg = move |backend: LlmBackend, label: &'static str| {
                let is_active = selected == backend;
                rsx! {
                    button {
                        key: "{label}",
                        class: if is_active {
                            "backend-seg backend-seg-active"
                        } else {
                            "backend-seg"
                        },
                        disabled: saving() || is_active,
                        onclick: move |_| {
                            if is_active { return; }
                            saving.set(true);
                            spawn(async move {
                                match set_backend(backend).await {
                                    Some((b, _)) => {
                                        push_toast(
                                            toasts,
                                            ToastKind::Info,
                                            format!("Claude backend set to {}.", b.label()),
                                        );
                                        settings_res.restart();
                                    }
                                    None => {
                                        push_toast(
                                            toasts,
                                            ToastKind::Error,
                                            "Could not update the Claude backend.".to_string(),
                                        );
                                    }
                                }
                                saving.set(false);
                            });
                        },
                        "{label}"
                    }
                }
            };

            // When API is selected, look up the Anthropic key credential's state from the
            // credentials list so the revealed row shows the correct set/masked badge.
            let anthropic_item = match &*creds_res.read() {
                Some(Some(items)) => items
                    .iter()
                    .find(|i| i.name == ANTHROPIC_API_KEY)
                    .cloned(),
                _ => None,
            };
            let anthropic_is_set = anthropic_item.as_ref().map(|i| i.is_set).unwrap_or(false);
            let anthropic_masked = anthropic_item.and_then(|i| i.masked);
            let show_api = selected == LlmBackend::Api;

            rsx! {
                div { class: "credentials-field-section",
                    div { class: "credentials-field-header",
                        label { class: "credentials-label", "Claude backend" }
                    }
                    p { class: "credentials-intro",
                        "Claude runs via the CLI (your logged-in Claude Code subscription) or the Anthropic API (needs an Anthropic API key)."
                    }
                    div { class: "backend-toggle",
                        {seg(LlmBackend::Cli, "CLI")}
                        {seg(LlmBackend::Api, "API")}
                    }
                    // When API is selected: either reveal the key input (once we know the key
                    // isn't present) or, if a key IS present, show the set/masked row. The
                    // warning shows only while API is selected AND no key is present.
                    if show_api {
                        if warn {
                            p { class: "ink-soft warn backend-key-warning",
                                "API backend needs an ANTHROPIC_API_KEY — the app will fall back to CLI until one is configured."
                            }
                        }
                        CredentialRow {
                            name: ANTHROPIC_API_KEY.to_string(),
                            label: "Anthropic API Key".to_string(),
                            is_set: anthropic_is_set,
                            current_masked: anthropic_masked,
                            toasts,
                            on_saved: move |_| {
                                // Re-fetch both the key state (updates the row's badge) and the
                                // backend settings (clears the `api_key_present` warning).
                                creds_res.restart();
                                settings_res.restart();
                            },
                        }
                    }
                }
            }
        }
    }
}

// ── BombeSettings ─────────────────────────────────────────────────────────────

/// Settings section for the Bombe background animation.
///
/// Provides two controls:
/// 1. **Animate ON/OFF** — a persisted toggle.  When OFF the bombe never
///    animates even during loading (stays static-dark).  Persisted to
///    `localStorage["camerata.bombe.enabled"]` so the choice survives
///    relaunches.
/// 2. **Play/Pause preview** — a transient button that toggles the animation
///    purely for visual preview, without touching the loading count or the
///    ON/OFF setting.  Useful to see the bombe in action before committing
///    to a setting.
///
/// Both controls read/write the `BombeEnabled` and `BombePreview` signals in
/// the Dioxus context (provided by `loading::provide_loading_context`).
#[component]
fn BombeSettings() -> Element {
    // Consume bombe control signals from context.  The newtypes are unwrapped
    // to their inner Signal<bool> for direct read/write access.
    let mut enabled = use_context::<BombeEnabled>().0;
    let mut preview = use_context::<BombePreview>().0;

    // On mount: read the persisted enabled value from localStorage and
    // initialise the signal.  Runs once (empty deps).
    use_effect(move || {
        spawn(async move {
            // Evaluate JS in the wry webview to read localStorage.
            // Returns "false" if the key is explicitly set to false, else "true".
            let mut ev = document::eval(
                r#"
                var v = localStorage.getItem('camerata.bombe.enabled');
                dioxus.send(v === null ? 'true' : v);
                "#,
            );
            if let Ok(val) = ev.recv::<String>().await {
                enabled.set(val.trim() != "false");
            }
        });
    });

    // Persist enabled to localStorage whenever it changes.
    let enabled_val = enabled();
    use_effect(move || {
        let js = format!(
            "localStorage.setItem('{}', '{}');",
            BOMBE_ENABLED_KEY,
            if enabled_val { "true" } else { "false" }
        );
        let _ = document::eval(&js);
    });

    let preview_val = preview();
    let preview_label = if preview_val { "Pause Preview" } else { "Play Preview" };
    let enabled_label = if enabled_val { "ON" } else { "OFF" };

    rsx! {
        div { class: "credentials-field-section bombe-settings-section",
            div { class: "credentials-field-header",
                label { class: "credentials-label", "Background Animation" }
            }
            p { class: "bombe-settings-hint",
                "Controls the Bombe machine animation behind the interface."
            }
            div { class: "bombe-settings-row",
                // Animate ON/OFF toggle
                div { class: "bombe-settings-item",
                    span { class: "bombe-settings-item-label", "Animate" }
                    button {
                        class: if enabled_val {
                            "bombe-toggle-btn bombe-toggle-btn-on"
                        } else {
                            "bombe-toggle-btn bombe-toggle-btn-off"
                        },
                        onclick: move |_| {
                            enabled.set(!enabled_val);
                            // When turning off, also stop any active preview.
                            if enabled_val {
                                preview.set(false);
                            }
                        },
                        "{enabled_label}"
                    }
                }
                // Play/Pause preview button — only active when enabled is ON.
                div { class: "bombe-settings-item",
                    span { class: "bombe-settings-item-label", "Preview" }
                    button {
                        class: if preview_val {
                            "bombe-preview-btn bombe-preview-btn-active"
                        } else {
                            "bombe-preview-btn"
                        },
                        disabled: !enabled_val,
                        title: if enabled_val {
                            "Toggle a preview of the Bombe animation"
                        } else {
                            "Enable animation first"
                        },
                        onclick: move |_| {
                            if enabled_val {
                                preview.set(!preview_val);
                            }
                        },
                        "{preview_label}"
                    }
                }
            }
        }
    }
}

// ── CredentialRow ─────────────────────────────────────────────────────────────

#[component]
fn CredentialRow(
    name: String,
    label: String,
    is_set: bool,
    current_masked: Option<String>,
    toasts: Signal<Vec<crate::toast::Toast>>,
    on_saved: EventHandler<()>,
) -> Element {
    let mut input_val = use_signal(String::new);
    let mut saving = use_signal(|| false);

    let badge = if is_set {
        let masked_display = current_masked
            .as_deref()
            .unwrap_or("(set)")
            .to_string();
        rsx! {
            span { class: "credentials-badge-set", "Saved: {masked_display}" }
        }
    } else {
        rsx! {
            span { class: "credentials-badge-unset", "Not set" }
        }
    };

    let placeholder = if is_set { "Enter new value to update" } else { "Paste value here…" };

    rsx! {
        div { class: "credentials-field-section",
            div { class: "credentials-field-header",
                label { class: "credentials-label", "{label}" }
                {badge}
            }
            div { class: "credentials-input-row",
                input {
                    r#type: "password",
                    class: "credentials-input",
                    placeholder: "{placeholder}",
                    value: "{input_val}",
                    autocomplete: "off",
                    spellcheck: "false",
                    oninput: move |e| input_val.set(e.value()),
                }
                button {
                    class: "credentials-save-btn btn-primary",
                    disabled: saving() || input_val().trim().is_empty(),
                    onclick: {
                        let name = name.clone();
                        move |_| {
                            let value = input_val().trim().to_string();
                            if value.is_empty() {
                                return;
                            }
                            let name = name.clone();
                            saving.set(true);
                            spawn(async move {
                                match post_credential(&name, &value).await {
                                    Ok(masked) => {
                                        push_toast(
                                            toasts,
                                            ToastKind::Info,
                                            format!("Saved — {masked}"),
                                        );
                                        input_val.set(String::new());
                                        on_saved.call(());
                                    }
                                    Err(e) => {
                                        push_toast(
                                            toasts,
                                            ToastKind::Error,
                                            format!("Failed to save: {e}"),
                                        );
                                    }
                                }
                                saving.set(false);
                            });
                        }
                    },
                    if saving() { "Saving…" } else { "Save" }
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tier 2: network-helper tests (wiremock) ─────────────────────────────────
    // These point the BFF helpers at a fake server via the CAMERATA_BFF_URL seam
    // (crate::bff_base()) and assert the request CONTRACT. The env override is
    // process-global, so these set+remove it and must not run concurrently with a
    // test that reads bff_base() — keep them narrow.

    // GET /api/credentials — asserts the path and that the JSON list is parsed into
    // the expected CredentialListItem vec.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_credentials_gets_list_and_parses_items() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/credentials"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "openrouter_api_key", "is_set": true, "masked": "sk-1••••" },
                { "name": "github_token", "is_set": false, "masked": null },
            ])))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::fetch_credentials().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let items = result.expect("the GET succeeds and the body parses");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "openrouter_api_key");
        assert!(items[0].is_set);
        assert_eq!(items[0].masked.as_deref(), Some("sk-1••••"));
        assert_eq!(items[1].name, "github_token");
        assert!(!items[1].is_set);
        assert_eq!(items[1].masked, None);
    }

    // POST /api/credentials/{name} — asserts the path includes the credential name
    // and the body is exactly {"value": "..."}; on 200 it returns the masked value.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn post_credential_posts_value_body_and_returns_masked_on_success() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/credentials/openrouter_api_key"))
            .and(body_json(serde_json::json!({ "value": "sk-secret-123" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "masked": "sk-s••••" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::post_credential("openrouter_api_key", "sk-secret-123").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(result, Ok("sk-s••••".to_string()));
    }

    // POST with a non-success status — returns Err carrying the server's `error` field.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn post_credential_returns_error_message_on_failure_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/credentials/github_token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid token format" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::post_credential("github_token", "bad").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(result, Err("invalid token format".to_string()));
    }

    // POST /api/settings/llm-backend — asserts the body is exactly {"backend":"api"} and
    // that the echoed (backend, api_key_present) parse back.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_backend_posts_backend_and_parses_response() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/llm-backend"))
            .and(body_json(serde_json::json!({ "backend": "api" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "backend": "api", "api_key_present": true }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::set_backend(LlmBackend::Api).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let (backend, key) = out.expect("backend echo parsed");
        assert_eq!(backend, LlmBackend::Api);
        assert!(key, "api_key_present reflected");
    }

    // A non-2xx from the endpoint collapses to None (the UI toasts an error).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_backend_returns_none_on_error_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/llm-backend"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "ok": false, "message": "invalid backend" })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::set_backend(LlmBackend::Api).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(out.is_none(), "a 400 collapses to None");
    }

    // GET /api/settings — the backend view parses the effective backend + api_key_present.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_backend_settings_parses_backend_and_key_flag() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "llm_backend": "api", "api_key_present": false }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_backend_settings().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let view = out.expect("backend settings parsed");
        assert_eq!(view.llm_backend.as_deref(), Some("api"));
        assert!(!view.api_key_present);
    }

    // ── Tier 1: render tests (dioxus-ssr) ───────────────────────────────────────
    // Render components headlessly to an HTML string and assert KEY static
    // structure. SSR is static (no clicks, no async-loaded data): use_resource is
    // pending on first render, so CredentialsSettings renders its loading branch.

    // BombeSettings consumes BombeEnabled + BombePreview from context. The harness
    // must provide both before rendering it, else use_context panics.
    fn bombe_harness() -> Element {
        use_context_provider(|| BombeEnabled(Signal::new(true)));
        use_context_provider(|| BombePreview(Signal::new(false)));
        rsx! {
            BombeSettings {}
        }
    }

    #[test]
    fn bombe_settings_renders_animate_and_preview_controls() {
        let mut vdom = VirtualDom::new(bombe_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Background Animation"),
            "section label renders; html=\n{html}"
        );
        assert!(html.contains("Animate"), "the Animate control renders; html=\n{html}");
        assert!(html.contains("Preview"), "the Preview control renders; html=\n{html}");
        // enabled defaults to true → the toggle shows "ON" and the preview button is enabled.
        assert!(html.contains("ON"), "enabled toggle shows ON state; html=\n{html}");
        assert!(
            html.contains("Play Preview"),
            "preview button shows Play (not Pause) when inactive; html=\n{html}"
        );
    }

    // CredentialRow is prop-only + use_signal — no context needed. The `toasts`
    // prop is a Signal, constructed via the VirtualDom-runtime harness.
    fn credential_row_set_harness() -> Element {
        let toasts = use_signal(Vec::<crate::toast::Toast>::new);
        rsx! {
            CredentialRow {
                name: "openrouter_api_key".to_string(),
                label: "OpenRouter API Key".to_string(),
                is_set: true,
                current_masked: Some("sk-1••••".to_string()),
                toasts,
                on_saved: move |_| {},
            }
        }
    }

    #[test]
    fn credential_row_renders_label_masked_badge_and_save_button() {
        let mut vdom = VirtualDom::new(credential_row_set_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("OpenRouter API Key"),
            "the credential label renders; html=\n{html}"
        );
        assert!(
            html.contains("sk-1••••"),
            "the masked value shows in the Saved badge; html=\n{html}"
        );
        assert!(
            html.contains("Saved:"),
            "the set-state badge renders; html=\n{html}"
        );
        assert!(html.contains("Save"), "the Save button renders; html=\n{html}");
        assert!(
            html.contains("Enter new value to update"),
            "the is_set placeholder renders; html=\n{html}"
        );
    }

    fn credential_row_unset_harness() -> Element {
        let toasts = use_signal(Vec::<crate::toast::Toast>::new);
        rsx! {
            CredentialRow {
                name: "github_token".to_string(),
                label: "GitHub Token".to_string(),
                is_set: false,
                current_masked: None,
                toasts,
                on_saved: move |_| {},
            }
        }
    }

    #[test]
    fn credential_row_renders_not_set_badge_when_unset() {
        let mut vdom = VirtualDom::new(credential_row_unset_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("GitHub Token"),
            "the credential label renders; html=\n{html}"
        );
        assert!(
            html.contains("Not set"),
            "the unset-state badge renders; html=\n{html}"
        );
        assert!(
            html.contains("Paste value here"),
            "the unset placeholder renders; html=\n{html}"
        );
    }

    // CredentialsSettings consumes three contexts (toasts Signal, BombeEnabled,
    // BombePreview) and a use_resource. On first SSR render the resource is pending,
    // so it renders the "Loading…" branch plus the BombeSettings section.
    fn credentials_settings_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| BombeEnabled(Signal::new(true)));
        use_context_provider(|| BombePreview(Signal::new(false)));
        rsx! {
            CredentialsSettings {}
        }
    }

    #[test]
    fn credentials_settings_renders_title_intro_and_loading_branch() {
        let mut vdom = VirtualDom::new(credentials_settings_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Settings"), "the panel title renders; html=\n{html}");
        assert!(
            html.contains("OS keychain"),
            "the intro copy renders; html=\n{html}"
        );
        // use_resource is pending on first render → loading branch.
        assert!(
            html.contains("Loading"),
            "the pending-resource loading branch renders; html=\n{html}"
        );
        // The Bombe section renders below regardless of the resource state.
        assert!(
            html.contains("Background Animation"),
            "the Bombe settings section renders below; html=\n{html}"
        );
        // The Claude backend control renders below the credentials too.
        assert!(
            html.contains("Claude backend"),
            "the Claude backend control renders; html=\n{html}"
        );
    }

    // ModelBackendSettings consumes the toasts context and a use_resource. On first SSR
    // render the resource is pending, so it renders the loading branch — but the "Claude
    // backend" label is always present. The pure CLI/API + warning logic is unit-tested in
    // camerata_ui_core::llm_backend; here we lock the label + loading scaffold.
    fn model_backend_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        rsx! {
            ModelBackendSettings {}
        }
    }

    #[test]
    fn model_backend_settings_renders_label_and_loading_branch() {
        let mut vdom = VirtualDom::new(model_backend_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Claude backend"),
            "the Claude backend label renders; html=\n{html}"
        );
        // use_resource is pending on first render → loading branch.
        assert!(
            html.contains("Loading"),
            "the pending-resource loading branch renders; html=\n{html}"
        );
    }

    // The Anthropic key input is revealed inside ModelBackendSettings ONLY when the `api`
    // backend is selected. SSR keeps `use_resource` pending, so we can't drive the resolved
    // branch here; instead we render the reveal sub-tree directly — a `CredentialRow` wired
    // exactly as the API branch wires it (name `anthropic_api_key`, label "Anthropic API
    // Key") — and, for the `cli` case, an empty tree. This locks the contract that the
    // Anthropic key input appears for API and is absent for CLI.
    #[component]
    fn AnthropicKeyRevealProbe(backend: LlmBackend) -> Element {
        let toasts = use_signal(Vec::<crate::toast::Toast>::new);
        if backend == LlmBackend::Api {
            rsx! {
                CredentialRow {
                    name: ANTHROPIC_API_KEY.to_string(),
                    label: "Anthropic API Key".to_string(),
                    is_set: false,
                    current_masked: None,
                    toasts,
                    on_saved: move |_| {},
                }
            }
        } else {
            rsx! {}
        }
    }

    fn anthropic_reveal_api_harness() -> Element {
        rsx! {
            AnthropicKeyRevealProbe { backend: LlmBackend::Api }
        }
    }

    fn anthropic_reveal_cli_harness() -> Element {
        rsx! {
            AnthropicKeyRevealProbe { backend: LlmBackend::Cli }
        }
    }

    #[test]
    fn anthropic_key_input_shown_when_backend_is_api() {
        let mut vdom = VirtualDom::new(anthropic_reveal_api_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Anthropic API Key"),
            "the Anthropic key input is revealed for the API backend; html=\n{html}"
        );
        // The revealed row is an actual credential input (password field + Save button).
        assert!(html.contains("Save"), "the key input has a Save button; html=\n{html}");
    }

    #[test]
    fn anthropic_key_input_hidden_when_backend_is_cli() {
        let mut vdom = VirtualDom::new(anthropic_reveal_cli_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("Anthropic API Key"),
            "the Anthropic key input is NOT shown for the CLI backend; html=\n{html}"
        );
    }
}
