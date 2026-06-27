//! Settings → Credentials panel.
//!
//! App-wide, not per-project. Lets the user enter their OpenRouter API key and
//! GitHub token once; the backend writes them to the OS keychain. The full value
//! is never echoed back — only the masked form (first 4 chars + `••••`) is
//! displayed after saving.
//!
//! Rendered as a tab inside [`crate::cockpit::CockpitApp`] (see `CockpitView::Credentials`).

use dioxus::prelude::*;

use crate::BFF_URL;
use crate::loading::{BombeEnabled, BombePreview};
use crate::toast::{push_toast, ToastKind};

// ── localStorage key for the bombe animation toggle ──────────────────────────
const BOMBE_ENABLED_KEY: &str = "camerata.bombe.enabled";

// ── Known credential names (mirrors crate::credentials consts) ───────────────

const OPENROUTER_API_KEY: &str = "openrouter_api_key";
const GITHUB_TOKEN: &str = "github_token";

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
    reqwest::get(format!("{BFF_URL}/api/credentials"))
        .await
        .ok()?
        .json::<Vec<CredentialListItem>>()
        .await
        .ok()
}

async fn post_credential(name: &str, value: &str) -> Result<String, String> {
    let body = serde_json::json!({ "value": value });
    let resp = reqwest::Client::new()
        .post(format!("{BFF_URL}/api/credentials/{name}"))
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

            // ── Bombe animation settings ──────────────────────────────────
            BombeSettings {}
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
