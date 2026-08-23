//! OpenRouter provider-safety UI (Pass 2) — Dioxus adapter over the pure logic in
//! `camerata_ui_core::provider_safety` and the Pass-1 backend seams:
//! `GET /api/settings` (reads `safe_mode` + `pinned_provider`),
//! `POST /api/settings/provider-policy` (writes them), and
//! `GET /api/models/providers?model_id=<id>` (the picker's per-provider data).
//!
//! Three pieces live here, all sharing one context signal so they can never disagree:
//! - [`TestingModeBanner`] — the always-visible §4 warning, mounted ONCE in `App` (main.rs).
//! - [`DataSafetySettings`] — the §3 settings-panel toggle + confirm dialog, mounted in
//!   `credentials::CredentialsSettings`.
//! - [`ProviderPicker`] — the §5 per-model provider sub-selector, mounted next to a model
//!   picker (wired into `chat::ChatBubble`; see the module docs there for why the other two
//!   pickers — routines.rs, cockpit/scan.rs — are not yet wired).
//!
//! See `docs/design/2026-07-28_openrouter-provider-safety.md` for the full design. The
//! actual safety ENFORCEMENT lives entirely server-side
//! (`camerata_llm::provider_policy::provider_constraint_for_request`); nothing in this
//! module can affect it — it only reads/writes the policy the enforcement layer already
//! reads independently.

use dioxus::prelude::*;

use camerata_ui_core::models::ModelsResp;
use camerata_ui_core::provider_safety::{
    build_provider_rows, provider_data_unavailable, testing_mode_banner_visible,
    ProviderEndpointsResp, SAFE_MODE_OFF_CONFIRM_TEXT, TESTING_MODE_BANNER_TEXT,
    UNSAFE_ROW_DISABLED_HINT,
};

use crate::toast::{push_toast, ToastKind};

// ── shared app-wide policy state ────────────────────────────────────────────────

/// The current OpenRouter provider-safety policy. Deliberately plain data (no `Option`
/// wrapper for "not loaded yet") — it starts at the safe default and is overwritten once
/// the initial `GET /api/settings` fetch resolves, so every reader (the banner, the
/// toggle, every picker instance) always has a well-defined, safe-by-default value to
/// render even before the fetch completes. Mirrors `ProviderPolicy`'s own `Default`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderPolicyView {
    pub safe_mode: bool,
    pub pinned_provider: Option<String>,
}

impl Default for ProviderPolicyView {
    fn default() -> Self {
        Self { safe_mode: true, pinned_provider: None }
    }
}

/// Newtype context wrapper (Dioxus context is keyed by type) around the shared signal.
/// Every writer (the settings toggle, every `ProviderPicker` instance) updates this
/// signal directly right after a successful POST, so all readers re-render on the next
/// frame with no polling and no possibility of the banner/toggle/picker disagreeing —
/// they all read the exact same signal.
#[derive(Clone, Copy)]
pub struct ProviderPolicySignal(pub Signal<ProviderPolicyView>);

/// Provide the shared context signal and kick off the one-time fetch that seeds it from
/// the server's current (post-session-reset) policy. Call ONCE from `App` (main.rs),
/// before mounting [`TestingModeBanner`] or anything else that reads the context.
pub fn provide_provider_policy_context() {
    let mut signal =
        use_context_provider(|| ProviderPolicySignal(Signal::new(ProviderPolicyView::default())));
    use_hook(|| {
        spawn(async move {
            if let Some(policy) = fetch_provider_policy().await {
                signal.0.set(policy);
            }
        });
    });
}

// ── network helpers ───────────────────────────────────────────────────────────

/// The subset of `GET /api/settings` this module reads.
#[derive(serde::Deserialize)]
struct SettingsPolicyWire {
    /// Defaults to the safe posture if the field is somehow absent from a parseable
    /// response (the real server always sends it) — a display-only fallback; the actual
    /// enforcement reads the server's own `SettingsStore` independently of anything here.
    #[serde(default = "default_true")]
    safe_mode: bool,
    #[serde(default)]
    pinned_provider: Option<String>,
}

fn default_true() -> bool {
    true
}

async fn fetch_provider_policy() -> Option<ProviderPolicyView> {
    let v: SettingsPolicyWire = reqwest::get(format!("{}/api/settings", crate::bff_base()))
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    Some(ProviderPolicyView {
        safe_mode: v.safe_mode,
        pinned_provider: v.pinned_provider,
    })
}

/// Persist `{safe_mode, pinned_provider}` via `POST /api/settings/provider-policy`.
/// Returns the server-echoed value on success (the new source of truth to write back into
/// the shared signal), `None` on any transport/parse error or non-2xx.
async fn set_provider_policy(safe_mode: bool, pinned_provider: Option<String>) -> Option<ProviderPolicyView> {
    let body = serde_json::json!({ "safe_mode": safe_mode, "pinned_provider": pinned_provider });
    let resp = reqwest::Client::new()
        .post(format!("{}/api/settings/provider-policy", crate::bff_base()))
        .json(&body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: SettingsPolicyWire = resp.json().await.ok()?;
    Some(ProviderPolicyView {
        safe_mode: v.safe_mode,
        pinned_provider: v.pinned_provider,
    })
}

async fn fetch_model_providers(model_id: &str) -> Option<ProviderEndpointsResp> {
    reqwest::Client::new()
        .get(format!("{}/api/models/providers", crate::bff_base()))
        .query(&[("model_id", model_id)])
        .send()
        .await
        .ok()?
        .json::<ProviderEndpointsResp>()
        .await
        .ok()
}

// ── §4: the always-visible testing-mode banner ──────────────────────────────────

/// Global, non-dismissible warning shown on EVERY screen whenever safe mode is OFF.
/// Mounted ONCE in `App` (main.rs) as a top-level sibling — NOT inside any per-screen
/// component — so it is genuinely always-visible regardless of which cockpit tab/view is
/// active, and fixed-positioned above the rest of the app chrome (z-index in
/// `.testing-mode-banner`, see `style.rs`) so nothing can render over it.
#[component]
pub fn TestingModeBanner() -> Element {
    let policy = use_context::<ProviderPolicySignal>().0;
    if !testing_mode_banner_visible(policy().safe_mode) {
        return rsx! {};
    }
    rsx! {
        div { class: "testing-mode-banner", role: "alert", "{TESTING_MODE_BANNER_TEXT}" }
    }
}

// ── §3: the settings-panel "Data safety" toggle ─────────────────────────────────

/// The credentials-panel "Data safety" section: a SAFE ⟷ TESTING segmented toggle bound
/// to the shared [`ProviderPolicySignal`], with a confirm dialog gating the ON→OFF
/// transition. Mounted in `credentials::CredentialsSettings`.
#[component]
pub fn DataSafetySettings() -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut policy = use_context::<ProviderPolicySignal>().0;
    let mut saving = use_signal(|| false);
    let mut show_confirm = use_signal(|| false);
    let current = policy();

    let turn_safe_on = {
        let pin = current.pinned_provider.clone();
        move |_| {
            let pin = pin.clone();
            saving.set(true);
            spawn(async move {
                match set_provider_policy(true, pin).await {
                    Some(updated) => policy.set(updated),
                    None => push_toast(
                        toasts,
                        ToastKind::Error,
                        "Could not update the data-safety setting.".to_string(),
                    ),
                }
                saving.set(false);
            });
        }
    };

    let confirm_turn_testing_on = {
        let pin = current.pinned_provider.clone();
        move |_| {
            let pin = pin.clone();
            show_confirm.set(false);
            saving.set(true);
            spawn(async move {
                match set_provider_policy(false, pin).await {
                    Some(updated) => policy.set(updated),
                    None => push_toast(
                        toasts,
                        ToastKind::Error,
                        "Could not update the data-safety setting.".to_string(),
                    ),
                }
                saving.set(false);
            });
        }
    };

    rsx! {
        div { class: "credentials-field-section",
            div { class: "credentials-field-header",
                label { class: "credentials-label", "Data safety" }
            }
            p { class: "credentials-intro",
                "Safe mode restricts every OpenRouter request to providers that neither train on "
                "nor retain your prompts. This is the only mode safe for client repositories."
            }
            div { class: "backend-toggle safety-toggle",
                button {
                    class: if current.safe_mode { "backend-seg backend-seg-active" } else { "backend-seg" },
                    disabled: saving() || current.safe_mode,
                    onclick: turn_safe_on,
                    "SAFE MODE"
                }
                button {
                    class: if !current.safe_mode {
                        "backend-seg backend-seg-active backend-seg-danger"
                    } else {
                        "backend-seg"
                    },
                    disabled: saving() || !current.safe_mode,
                    onclick: move |_| show_confirm.set(true),
                    "TESTING MODE"
                }
            }
            if current.safe_mode {
                p { class: "ink-soft", "Safe mode ON — no training, no retention (recommended)." }
            } else {
                p { class: "ink-soft warn",
                    "Testing mode ON — resets to safe mode automatically the next time the app starts."
                }
            }
            if show_confirm() {
                div { class: "safety-confirm-dialog",
                    p { class: "safety-confirm-text", "{SAFE_MODE_OFF_CONFIRM_TEXT}" }
                    div { class: "safety-confirm-actions",
                        button {
                            class: "btn-secondary",
                            onclick: move |_| show_confirm.set(false),
                            "Cancel"
                        }
                        button {
                            class: "btn-delete-sm",
                            onclick: confirm_turn_testing_on,
                            "Turn off (testing mode)"
                        }
                    }
                }
            }
        }
    }
}

// ── §5: the provider picker ──────────────────────────────────────────────────────

/// The per-model OpenRouter provider sub-selector. Renders nothing when the currently
/// selected model (`model()`, looked up against `models` for its `provider` field) is not
/// an OpenRouter model — Claude models have no provider concept here.
///
/// Reusable: this is the ONE implementation; `chat::ChatBubble` wires it against its
/// `model` signal + `models` resource. Wiring it into `routines.rs` / `cockpit/scan.rs`
/// is a matter of passing their own `model: Signal<String>` + `models: Option<ModelsResp>`
/// — trivial given this component, but not done in this pass (see the wiring note in
/// `docs/design/2026-07-28_openrouter-provider-safety.md`'s "Pass 2 landed" section).
#[component]
pub fn ProviderPicker(model: Signal<String>, models: Option<ModelsResp>) -> Element {
    let policy_signal = use_context::<ProviderPolicySignal>().0;

    let provider_res = use_resource(move || {
        let id = model();
        async move { fetch_model_providers(&id).await }
    });

    let current_id = model();
    let is_openrouter = models
        .as_ref()
        .and_then(|m| m.models.iter().find(|o| o.id == current_id))
        .map(|o| o.provider == "openrouter")
        .unwrap_or(false);

    if !is_openrouter {
        return rsx! {};
    }

    let providers_opt = provider_res.read().clone().flatten();

    rsx! {
        div { class: "provider-picker",
            span { class: "provider-picker-label", "Provider" }
            match providers_opt {
                None => rsx! {
                    span { class: "ink-soft provider-picker-status", "Loading providers…" }
                },
                Some(resp) if provider_data_unavailable(&resp.providers) => rsx! {
                    span { class: "ink-soft provider-picker-status",
                        "Provider data unavailable — using Auto."
                    }
                },
                Some(resp) => {
                    let policy_now = policy_signal();
                    let rows = build_provider_rows(
                        &resp.providers,
                        policy_now.safe_mode,
                        policy_now.pinned_provider.as_deref(),
                    );
                    let selected_value = rows
                        .iter()
                        .find(|r| r.selected)
                        .map(|r| r.slug.clone().unwrap_or_default())
                        .unwrap_or_default();
                    let safe_mode_now = policy_now.safe_mode;
                    rsx! {
                        select {
                            class: "provider-picker-select",
                            value: "{selected_value}",
                            onchange: move |e| {
                                let v = e.value();
                                let pin = if v.is_empty() { None } else { Some(v) };
                                spawn(async move {
                                    if let Some(updated) = set_provider_policy(safe_mode_now, pin).await {
                                        policy_signal.clone().set(updated);
                                    }
                                });
                            },
                            for row in rows.iter() {
                                {
                                    let value = row.slug.clone().unwrap_or_default();
                                    let badge = if row.is_safe { "✓" } else { "⚠" };
                                    let hint = if !row.selectable {
                                        format!(" ({UNSAFE_ROW_DISABLED_HINT})")
                                    } else {
                                        String::new()
                                    };
                                    let text = if row.detail.is_empty() {
                                        format!("{} {badge}{hint}", row.label)
                                    } else {
                                        format!("{} — {} {badge}{hint}", row.label, row.detail)
                                    };
                                    rsx! {
                                        option {
                                            key: "{value}",
                                            value: "{value}",
                                            disabled: !row.selectable,
                                            "{text}"
                                        }
                                    }
                                }
                            }
                        }
                    }
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

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_provider_policy_parses_safe_mode_and_pin() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "workspace_root": null,
                "repo_paths": {},
                "chat_model": null,
                "llm_backend": "cli",
                "api_key_present": false,
                "safe_mode": false,
                "pinned_provider": "deepinfra",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::fetch_provider_policy().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let policy = result.expect("parses");
        assert!(!policy.safe_mode);
        assert_eq!(policy.pinned_provider.as_deref(), Some("deepinfra"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_provider_policy_posts_body_and_parses_echo() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/provider-policy"))
            .and(body_json(serde_json::json!({
                "safe_mode": false,
                "pinned_provider": "novita",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "safe_mode": false,
                "pinned_provider": "novita",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::set_provider_policy(false, Some("novita".to_string())).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let policy = result.expect("parses");
        assert!(!policy.safe_mode);
        assert_eq!(policy.pinned_provider.as_deref(), Some("novita"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_provider_policy_returns_none_on_error_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/provider-policy"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::set_provider_policy(true, None).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(result.is_none());
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_model_providers_sends_model_id_query_param_and_parses_rows() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/providers"))
            .and(query_param("model_id", "deepseek/deepseek-chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model_id": "deepseek/deepseek-chat",
                "providers": [
                    {"slug":"deepinfra","name":"DeepInfra","region":"US","price_in":0.14,"price_out":0.28,"training":false,"retains_prompts":false},
                ],
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::fetch_model_providers("deepseek/deepseek-chat").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = result.expect("parses");
        assert_eq!(resp.providers.len(), 1);
        assert_eq!(resp.providers[0].slug, "deepinfra");
    }

    // ── Tier 1: render tests (dioxus-ssr) ───────────────────────────────────────

    fn policy_harness(policy: ProviderPolicyView, child: fn() -> Element) -> Element {
        use_context_provider(|| ProviderPolicySignal(Signal::new(policy)));
        child()
    }

    #[test]
    fn banner_renders_when_safe_mode_off() {
        fn harness() -> Element {
            policy_harness(
                ProviderPolicyView { safe_mode: false, pinned_provider: None },
                || rsx! { TestingModeBanner {} },
            )
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("TESTING MODE"), "banner text renders; html=\n{html}");
        assert!(html.contains("testing-mode-banner"), "banner class renders; html=\n{html}");
    }

    #[test]
    fn banner_does_not_render_when_safe_mode_on() {
        fn harness() -> Element {
            policy_harness(
                ProviderPolicyView { safe_mode: true, pinned_provider: None },
                || rsx! { TestingModeBanner {} },
            )
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(!html.contains("TESTING MODE"), "no banner when safe; html=\n{html}");
        assert!(!html.contains("testing-mode-banner"), "no banner class when safe; html=\n{html}");
    }

    fn data_safety_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| ProviderPolicySignal(Signal::new(ProviderPolicyView::default())));
        rsx! { DataSafetySettings {} }
    }

    #[test]
    fn data_safety_settings_renders_label_and_safe_active_by_default() {
        let mut vdom = VirtualDom::new(data_safety_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Data safety"), "section label renders; html=\n{html}");
        assert!(html.contains("SAFE MODE"), "SAFE segment renders; html=\n{html}");
        assert!(html.contains("TESTING MODE"), "TESTING segment renders; html=\n{html}");
        assert!(
            html.contains("Safe mode ON"),
            "the safe-mode status line renders by default; html=\n{html}"
        );
        // The confirm dialog must NOT render before the operator clicks TESTING.
        assert!(
            !html.contains(camerata_ui_core::provider_safety::SAFE_MODE_OFF_CONFIRM_TEXT),
            "confirm dialog is not shown until requested; html=\n{html}"
        );
    }

    fn data_safety_testing_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| {
            ProviderPolicySignal(Signal::new(ProviderPolicyView {
                safe_mode: false,
                pinned_provider: Some("deepinfra".to_string()),
            }))
        });
        rsx! { DataSafetySettings {} }
    }

    #[test]
    fn data_safety_settings_shows_testing_status_when_off() {
        let mut vdom = VirtualDom::new(data_safety_testing_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Testing mode ON"),
            "the testing-mode status line renders when safe_mode is off; html=\n{html}"
        );
    }

    // ProviderPicker: SSR can't drive the async use_resource to a resolved state, but it
    // CAN prove the "not an OpenRouter model" short-circuit (renders nothing) and the
    // loading state (use_resource is pending on first render).
    fn model_option(id: &str, provider: &str) -> camerata_ui_core::models::ModelOption {
        camerata_ui_core::models::ModelOption {
            label: id.to_string(),
            id: id.to_string(),
            provider: provider.to_string(),
        }
    }

    fn picker_harness_claude() -> Element {
        use_context_provider(|| ProviderPolicySignal(Signal::new(ProviderPolicyView::default())));
        let model = use_signal(|| "claude-opus-5".to_string());
        let models = Some(ModelsResp {
            models: vec![model_option("claude-opus-5", "claude")],
            default: "claude-opus-5".to_string(),
            backend: String::new(),
        });
        rsx! { ProviderPicker { model, models } }
    }

    #[test]
    fn picker_renders_nothing_for_a_claude_model() {
        let mut vdom = VirtualDom::new(picker_harness_claude);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("provider-picker"),
            "no picker chrome for a non-OpenRouter model; html=\n{html}"
        );
    }

    fn picker_harness_openrouter() -> Element {
        use_context_provider(|| ProviderPolicySignal(Signal::new(ProviderPolicyView::default())));
        let model = use_signal(|| "deepseek/deepseek-chat".to_string());
        let models = Some(ModelsResp {
            models: vec![model_option("deepseek/deepseek-chat", "openrouter")],
            default: "deepseek/deepseek-chat".to_string(),
            backend: String::new(),
        });
        rsx! { ProviderPicker { model, models } }
    }

    #[test]
    fn picker_renders_loading_state_for_an_openrouter_model() {
        let mut vdom = VirtualDom::new(picker_harness_openrouter);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("provider-picker"), "picker chrome renders; html=\n{html}");
        assert!(
            html.contains("Loading providers"),
            "use_resource is pending on first SSR render; html=\n{html}"
        );
    }
}
