//! Settings, provider policy, project and rescan/accept alternatives, memory and
//! hierarchy handler tests.

use super::*;

/// The GLOBAL chat assistant resolves its model with precedence:
/// req.model (UI override) > app-level chat_model > DEFAULT_MODEL.
#[test]
fn resolve_chat_model_precedence() {
    // 1. Explicit request model wins over everything.
    assert_eq!(
        resolve_chat_model(Some("req-model"), Some("app-model")),
        "req-model"
    );
    // 2. No (or blank) request model -> app-level chat model.
    assert_eq!(resolve_chat_model(None, Some("app-model")), "app-model");
    assert_eq!(
        resolve_chat_model(Some("   "), Some("app-model")),
        "app-model",
        "blank request model is ignored"
    );
    // 3. Neither set -> DEFAULT_MODEL floor.
    assert_eq!(resolve_chat_model(None, None), crate::llm::DEFAULT_MODEL);
    assert_eq!(
        resolve_chat_model(Some("  "), Some("   ")),
        crate::llm::DEFAULT_MODEL,
        "blank request AND blank app model -> default floor"
    );
    // 4. Request model still wins even when the app model is blank.
    assert_eq!(
        resolve_chat_model(Some("req-model"), Some("   ")),
        "req-model"
    );
}

/// Scan-type selector flags default to TRUE when absent (today's behaviour: both scans run).
#[test]
fn audit_req_scan_type_flags_default_true() {
    let req: AuditReq = serde_json::from_str(r#"{"repos":["me/api"]}"#).unwrap();
    assert!(req.run_ai_review, "run_ai_review defaults true");
    assert!(req.run_deterministic, "run_deterministic defaults true");
    // Explicit false is honored.
    let req: AuditReq =
        serde_json::from_str(r#"{"repos":["me/api"],"run_ai_review":false}"#).unwrap();
    assert!(!req.run_ai_review);
    assert!(req.run_deterministic, "the other flag stays true");
}

/// B3: the synchronous desktop endpoint has no job to record a batch id on, so a resolved
/// Batch (the API default, or a forced one) runs in real time and says so ONCE in the notes.
/// Every other mode, and no note, otherwise.
#[test]
fn the_synchronous_endpoint_never_runs_a_batch() {
    use crate::ai_audit::{resolve_scan_mode, ScanMode};
    let mut notes = Vec::new();
    // The API-default resolution is Batch; the synchronous endpoint turns it into real time.
    let resolved = resolve_scan_mode(
        None,
        &crate::llm::BackendResolution::Api,
        crate::efficiency::EfficiencyConfig::default(),
    )
    .mode;
    assert_eq!(
        resolved,
        ScanMode::Batch,
        "the premise: batch is the default"
    );
    assert_eq!(
        synchronous_scan_mode(resolved, &mut notes),
        ScanMode::Parallel
    );
    assert_eq!(notes, vec![SYNCHRONOUS_SCAN_REALTIME_NOTICE.to_string()]);
    assert!(!SYNCHRONOUS_SCAN_REALTIME_NOTICE.contains('\u{2014}'));
    // A forced batch is real time here too.
    let mut notes = Vec::new();
    assert_eq!(
        synchronous_scan_mode(ScanMode::Batch, &mut notes),
        ScanMode::Parallel
    );
    assert_eq!(notes.len(), 1);
    // Other modes pass through with no note.
    for mode in [ScanMode::Parallel, ScanMode::Sequential] {
        let mut notes = Vec::new();
        assert_eq!(synchronous_scan_mode(mode, &mut notes), mode);
        assert!(notes.is_empty());
    }
}

/// `effective_scan_modes`: both-false is never a no-op — it coerces back to both-true and
/// flags the coercion. Any other combination passes through untouched.
#[test]
fn effective_scan_modes_never_a_no_op() {
    assert_eq!(effective_scan_modes(true, true), (true, true, false));
    assert_eq!(effective_scan_modes(true, false), (true, false, false));
    assert_eq!(effective_scan_modes(false, true), (false, true, false));
    // Both off -> forced back on, coercion flagged.
    assert_eq!(effective_scan_modes(false, false), (true, true, true));
}

/// `GET /api/settings` includes the global `chat_backend` and `api_key_present`. The
/// stored value is the ENTIRE input — no env fallback of any kind
/// (`docs/design/2026-09-22_per-project-backend.md`).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn get_settings_includes_chat_backend_and_api_key_present() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["chat_backend"], "api",
        "stored chat backend surfaces verbatim"
    );
    assert!(
        json["api_key_present"].is_boolean(),
        "api_key_present is present and boolean"
    );
}

/// `POST /api/settings/chat-backend` persists a valid backend and echoes it plus
/// `api_key_present`; the stored setting reflects the new value.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn post_chat_backend_persists_valid_value() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/chat-backend")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"api"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["backend"], "api");
    assert!(json["api_key_present"].is_boolean());
    // Persisted on the settings store.
    assert_eq!(
        state.settings.chat_backend(),
        crate::llm::ProjectBackend::Api
    );
}

/// `POST /api/settings/chat-backend` persists to the settings store WITHOUT touching
/// process env at all — there is no `CAMERATA_LLM_BACKEND` env var to mutate anymore.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn post_chat_backend_does_not_mutate_process_env() {
    // Sentinel value the handler must NOT touch (proving no env write happens at all).
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_LLM_BACKEND", "sentinel-untouched");

    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/chat-backend")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"api"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The setting is persisted to the store...
    assert_eq!(
        state.settings.chat_backend(),
        crate::llm::ProjectBackend::Api
    );
    // ...but the process env was left exactly as it was (no per-request set_var).
    assert_eq!(
        std::env::var("CAMERATA_LLM_BACKEND").as_deref(),
        Ok("sentinel-untouched"),
        "the handler must not mutate CAMERATA_LLM_BACKEND"
    );
}

// ── Pass 2 (provider-safety UI): settings routes + the picker's data route ──────

/// `GET /api/settings` surfaces the provider-safety policy's default (safe_mode=true,
/// no pin) when nothing has ever been set — the same fail-safe guarantee
/// `SettingsStore::provider_policy` gives at the store layer, now visible over HTTP.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn get_settings_includes_default_safe_mode_and_no_pin() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["safe_mode"], true);
    assert!(json["pinned_provider"].is_null());
}

/// `POST /api/settings/provider-policy` persists both fields and echoes them; the
/// store reflects the new value afterward.
#[tokio::test]
async fn post_provider_policy_persists_safe_mode_and_pin() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/provider-policy")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"safe_mode":false,"pinned_provider":"deepinfra"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["safe_mode"], false);
    assert_eq!(json["pinned_provider"], "deepinfra");

    let stored = state.settings.provider_policy();
    assert!(!stored.safe_mode);
    assert_eq!(stored.pinned_provider.as_deref(), Some("deepinfra"));
}

/// A blank/whitespace `pinned_provider` collapses to `None` — same convention as
/// `set_chat_model` elsewhere in this file.
#[tokio::test]
async fn post_provider_policy_blank_pin_collapses_to_none() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/provider-policy")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"safe_mode":true,"pinned_provider":"   "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["pinned_provider"].is_null());
    assert_eq!(state.settings.provider_policy().pinned_provider, None);
}

/// `GET /api/models/providers?model_id=...` returns the joined provider records from
/// the (test-seeded) registry cache — no live network call. Seeds via
/// `ModelRegistry::seed_provider_endpoints`, the same test-only seam
/// `crates/llm/src/model_registry.rs` uses.
#[tokio::test]
async fn get_model_providers_returns_seeded_endpoint_records() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.model_registry.seed_provider_endpoints(
        "deepseek/deepseek-chat",
        vec![crate::model_registry::ProviderEndpointInfo {
            provider_slug: "deepinfra".to_string(),
            provider_name: "DeepInfra".to_string(),
            region: Some("US".to_string()),
            training: false,
            retains_prompts: false,
            price_in: 0.14,
            price_out: 0.28,
        }],
    );
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/models/providers?model_id=deepseek%2Fdeepseek-chat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["model_id"], "deepseek/deepseek-chat");
    let providers = json["providers"].as_array().expect("providers array");
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0]["slug"], "deepinfra");
    assert_eq!(providers[0]["name"], "DeepInfra");
    assert_eq!(providers[0]["region"], "US");
    assert_eq!(providers[0]["training"], false);
    assert_eq!(providers[0]["retains_prompts"], false);
    assert!((providers[0]["price_in"].as_f64().unwrap() - 0.14).abs() < 1e-9);
}

/// A model with no cached provider data returns an empty `providers` array (not an
/// error) — the UI's "provider data unavailable, fall back to Auto" path reads this as
/// the degrade-gracefully signal, per the design doc's §5 requirement. Seeds an EMPTY
/// endpoint list directly (the same shape a failed/empty live fetch would cache) so
/// this stays a no-network unit test rather than hitting the real OpenRouter API.
#[tokio::test]
async fn get_model_providers_empty_for_unknown_model_does_not_error() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .model_registry
        .seed_provider_endpoints("not-a-real-model", vec![]);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/models/providers?model_id=not-a-real-model")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["model_id"], "not-a-real-model");
    assert_eq!(json["providers"].as_array().unwrap().len(), 0);
}

// ── `effective_workspace_root` (workspace-folder-derivation fix) ─────────────

/// An explicit `workspace_root` setting always wins, even when a repo override is
/// also set — the architect's explicit choice is never second-guessed.
#[test]
fn effective_workspace_root_prefers_explicit_setting() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_workspace_root(Some("/explicit/root".to_string()));
    state
        .settings
        .set_repo_path("me/api", Some("/other/place/me-api".to_string()));
    let project = state
        .projects
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    assert_eq!(
        effective_workspace_root(&state, &project).as_deref(),
        Some("/explicit/root")
    );
}

/// With no explicit workspace root, the parent of the first in-scope repo's linked
/// local path is used as the effective root.
#[test]
fn effective_workspace_root_derives_from_first_repo_override_parent() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_repo_path("me/api", Some("/Users/auditor/Repos/api".to_string()));
    let project = state
        .projects
        .create("P", vec!["me/api".to_string(), "me/other".to_string()])
        .unwrap();
    assert_eq!(
        effective_workspace_root(&state, &project).as_deref(),
        Some("/Users/auditor/Repos")
    );
}

/// A repo with no override is skipped in favor of a later repo that has one.
#[test]
fn effective_workspace_root_skips_repos_with_no_override() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_repo_path("me/second", Some("/Users/auditor/Repos/second".to_string()));
    let project = state
        .projects
        .create("P", vec!["me/first".to_string(), "me/second".to_string()])
        .unwrap();
    assert_eq!(
        effective_workspace_root(&state, &project).as_deref(),
        Some("/Users/auditor/Repos")
    );
}

/// Neither an explicit root nor any repo override → None (the caller reports a
/// specific "pick a workspace folder" error rather than guessing).
#[test]
fn effective_workspace_root_none_when_nothing_is_set() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    assert!(effective_workspace_root(&state, &project).is_none());
}

/// `POST /api/settings/chat-backend` rejects a bogus backend with 400 and does not
/// mutate the stored setting.
#[tokio::test]
async fn post_chat_backend_rejects_bogus_value() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/chat-backend")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"gemini"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // The store was left untouched (still the Cli default).
    assert_eq!(
        state.settings.chat_backend(),
        crate::llm::ProjectBackend::Cli
    );
}

/// `POST /api/projects/:id/backend` — the per-project backend setting
/// (`docs/design/2026-09-22_per-project-backend.md`). A fresh project defaults to `Cli`;
/// the endpoint switches it and echoes the updated project. Mirrors
/// `post_chat_backend_persists_valid_value`'s shape.
#[tokio::test]
async fn post_project_backend_persists_valid_value() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("Compliance", vec![]).unwrap();
    assert_eq!(
        p.backend,
        crate::project::ProjectBackend::Api,
        "fresh project defaults to Api"
    );
    let app = router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/backend", p.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"cli"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);
    assert_eq!(json["project"]["backend"], "cli");
    // Persisted on the store.
    assert_eq!(
        state.projects.get(&p.id).unwrap().backend,
        crate::project::ProjectBackend::Cli
    );

    // Switching it back to api round-trips too.
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/backend", p.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"api"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let json2 = body_json(resp2).await;
    assert_eq!(json2["project"]["backend"], "api");
}

/// `POST /api/projects/:id/backend` rejects a bogus backend with `ok: false` (no
/// HTTP-status signaling, mirrors `set_step_model`'s shape) and does not mutate the
/// project.
#[tokio::test]
async fn post_project_backend_rejects_bogus_value() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("Compliance", vec![]).unwrap();
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/backend", p.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"gemini"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    assert_eq!(
        state.projects.get(&p.id).unwrap().backend,
        crate::project::ProjectBackend::Api
    );
}

/// `POST /api/projects/:id/backend` against an unknown project id: no HTTP-status
/// signaling (mirrors `set_step_model`'s shape) — a 200 response with `ok: false` and a
/// human-readable `message` the UI can display.
#[tokio::test]
async fn post_project_backend_reports_error_for_unknown_project() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/does-not-exist/backend")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"backend":"api"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    assert!(
        json["message"].as_str().is_some(),
        "message field present for the UI"
    );
}

// ── Audit-integrated alternative recommendation endpoints ─────────────────────────
// (docs/design/2026-09-22_audit-integrated-alternatives.md)

/// `rescan-alternatives` validates every override AGAINST the corpus BEFORE any model
/// call (and before the compliance gate — validation is cheaper and more specific than
/// the gate, so it runs first): an unknown rule id and a real rule with a bogus option id
/// are both rejected, and since NEITHER override is valid, the whole request is refused
/// with `ok: false` naming both problems. This assertion holds regardless of the test
/// environment's key/backend state, since validation never reaches that far.
#[tokio::test]
async fn rescan_alternatives_rejects_invalid_overrides_before_any_model_call() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/rescan-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"overrides":[
                            {"rule_id":"NO-SUCH-RULE-EVER-1","chosen_option_id":"whatever"},
                            {"rule_id":"RUST-DOMAIN-7","chosen_option_id":"not-a-real-option-id"}
                        ]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    let message = json["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("NO-SUCH-RULE-EVER-1"),
        "message: {message}"
    );
    assert!(message.contains("RUST-DOMAIN-7"), "message: {message}");
}

/// A rescan request with at least one VALID override reaches the compliance-safety gate,
/// and a `Blocked` resolution (this project is explicitly switched to `Api`, and this
/// test guarantees no Anthropic key is present) refuses the whole batch with the gate's
/// own message — no model call, no findings. Mirrors the main audit's
/// `blocked_resolution_aborts_the_whole_scan_before_any_work` guard, at the endpoint layer.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_alternatives_blocked_when_project_is_api_backend_with_no_key() {
    // Defensive against the known cross-test env-var race (see
    // `resolve_backend_for_project_gathers_the_three_inputs_correctly`'s doc comment).
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    assert_eq!(
        p.backend,
        crate::project::ProjectBackend::Api,
        "fresh project defaults to Api"
    );
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Api);
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/rescan-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"overrides":[{"rule_id":"RUST-DOMAIN-7","chosen_option_id":"explicit-unitofwork-parameter-on-transactional-repository-methods"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    assert!(
        json["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Anthropic API"),
        "message: {:?}",
        json["message"]
    );
}

/// H2: the same rescan on a client engagement's cli project is refused before any
/// repository is read.
#[tokio::test]
async fn client_engagement_rescan_refuses_the_cli_backend() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Cli);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/rescan-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"overrides":[{"rule_id":"RUST-DOMAIN-7","chosen_option_id":"explicit-unitofwork-parameter-on-transactional-repository-methods"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false, "{json:?}");
    assert!(
        json["message"].as_str().unwrap().contains("`cli` backend"),
        "{json:?}"
    );
}

/// A project with a repo that has no local folder is not rescanned for the repos that do:
/// the request is refused and names the repo, unless it says it accepts a partial run.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_alternatives_refuses_a_project_with_a_repo_that_has_no_local_folder() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string(), "me/web".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Cli);
    state.projects.set_client_engagement(&p.id, false, false);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/rescan-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"overrides":[{"rule_id":"RUST-DOMAIN-7","chosen_option_id":"explicit-unitofwork-parameter-on-transactional-repository-methods"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false, "{json:?}");
    let message = json["message"].as_str().unwrap();
    assert!(message.contains("2 of 2 repositories"), "{message}");
    assert!(
        message.contains("me/api") && message.contains("me/web"),
        "{message}"
    );
}

/// A `Cli`-backend project's rescan is never blocked, regardless of key presence — no
/// silent downgrade path exists anymore (the removed `CliFallbackWarn`), just a plain
/// `Cli` resolution that proceeds straight to the (in this test env, failing) model call.
/// Asserts the gate itself never refuses the request; it does not assert findings content
/// (no live model runs in this test suite).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_alternatives_proceeds_on_cli_backend_project() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Cli);
    // The maintainer's own project: a client engagement refuses the cli backend (see
    // `client_engagement_rescan_refuses_the_cli_backend` below).
    state.projects.set_client_engagement(&p.id, false, false);
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/rescan-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"allow_partial":true,"overrides":[{"rule_id":"RUST-DOMAIN-7","chosen_option_id":"explicit-unitofwork-parameter-on-transactional-repository-methods"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    // Not Blocked: the request proceeds (ok: true) rather than being refused outright.
    assert_eq!(
        json["ok"], true,
        "a Cli-backend project must never be blocked: {json:?}"
    );
}

/// `rescan-alternatives` against an unknown project id reports the same `{ ok: false,
/// message }` shape as every other per-project mutator (no HTTP-status signaling).
#[tokio::test]
async fn rescan_alternatives_reports_error_for_unknown_project() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/does-not-exist/rescan-alternatives")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"overrides":[{"rule_id":"RUST-DOMAIN-7","chosen_option_id":"x"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
}

fn seeded_scan_report_with_recommendations(
    repo: &str,
    recs: Vec<(&str, &str)>,
) -> crate::onboard::ScanReport {
    let mut report = crate::onboard::ScanReport::gated(&[repo.to_string()]);
    report.gated = false;
    report.recommendations = recs
        .into_iter()
        .map(|(rule_id, option_id)| {
            (
                rule_id.to_string(),
                crate::ai_audit::RuleRecommendation {
                    repo: String::new(),
                    rule_id: rule_id.to_string(),
                    recommended_option_id: option_id.to_string(),
                    recommendation_reasoning: "test reasoning".to_string(),
                    hallucinated: false,
                    operator_chosen: false,
                    evidence: Some("src/lib.rs:1 — test evidence".to_string()),
                    applicable: true,
                    ..Default::default()
                },
            )
        })
        .collect();
    report
}

/// `accept-alternatives` with no scan on record refuses (never fabricates a ruleset).
#[tokio::test]
async fn accept_alternatives_reports_error_without_a_scan() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/accept-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
}

/// `accept-alternatives` with an empty body (`{}`) accepts EVERY rule in the last scan's
/// `recommendations` map, writing each `recommended_option_id` into that rule's
/// `RuleSelection.chosen_option`.
#[tokio::test]
async fn accept_alternatives_persists_every_recommendation_by_default() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: "TESTING-PYRAMID-1".to_string(),
            ..Default::default()
        });
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: "RUST-DOMAIN-7".to_string(),
            ..Default::default()
        });
    });
    state.set_last_scan(
        p.id.clone(),
        seeded_scan_report_with_recommendations(
            "me/api",
            vec![
                (
                    "TESTING-PYRAMID-1",
                    "pyramid-many-unit-fewer-integration-few-e2e",
                ),
                (
                    "RUST-DOMAIN-7",
                    "explicit-unitofwork-parameter-on-transactional-repository-methods",
                ),
            ],
        ),
    );
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/accept-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);
    assert_eq!(json["accepted"], 2);

    let updated = state.projects.get(&p.id).unwrap();
    let sel = |rid: &str| {
        updated
            .ruleset
            .selections
            .iter()
            .find(|s| s.rule_id == rid)
            .unwrap()
            .chosen_option
            .clone()
    };
    assert_eq!(
        sel("TESTING-PYRAMID-1"),
        Some("pyramid-many-unit-fewer-integration-few-e2e".to_string())
    );
    assert_eq!(
        sel("RUST-DOMAIN-7"),
        Some("explicit-unitofwork-parameter-on-transactional-repository-methods".to_string())
    );
}

/// `accept-alternatives` with `{ "rule_ids": [...] }` accepts ONLY the named subset,
/// leaving every other rule's recommendation un-persisted.
#[tokio::test]
async fn accept_alternatives_scopes_to_the_requested_rule_ids() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .unwrap();
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: "TESTING-PYRAMID-1".to_string(),
            ..Default::default()
        });
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: "RUST-DOMAIN-7".to_string(),
            ..Default::default()
        });
    });
    state.set_last_scan(
        p.id.clone(),
        seeded_scan_report_with_recommendations(
            "me/api",
            vec![
                (
                    "TESTING-PYRAMID-1",
                    "pyramid-many-unit-fewer-integration-few-e2e",
                ),
                (
                    "RUST-DOMAIN-7",
                    "explicit-unitofwork-parameter-on-transactional-repository-methods",
                ),
            ],
        ),
    );
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/accept-alternatives", p.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"rule_ids":["TESTING-PYRAMID-1"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);
    assert_eq!(json["accepted"], 1);

    let updated = state.projects.get(&p.id).unwrap();
    let sel = |rid: &str| {
        updated
            .ruleset
            .selections
            .iter()
            .find(|s| s.rule_id == rid)
            .unwrap()
            .chosen_option
            .clone()
    };
    assert_eq!(
        sel("TESTING-PYRAMID-1"),
        Some("pyramid-many-unit-fewer-integration-few-e2e".to_string())
    );
    assert_eq!(
        sel("RUST-DOMAIN-7"),
        None,
        "un-requested rule must be left untouched"
    );
}

/// GAP-2: the git-commit chokepoint HARD-BLOCKS a commit whose message violates the
/// project's process rules BEFORE any git action is taken (no repo dir is even resolved).
/// The default project config enforces the conventional-commit + commit-doc baseline.
#[tokio::test]
async fn git_commit_chokepoint_blocks_a_message_violating_process_rules() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Gate", vec!["me/api".to_string()])
        .unwrap();
    let app = router(state);

    // "just some junk" is not a conventional-commit subject and has no doc body/story-id.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/git/commit", p.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"repo":"me/api","message":"just some junk"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        json["ok"], false,
        "violating commit must be refused: {json}"
    );
    assert_eq!(
        json["blocked"], true,
        "the response must mark it as gate-blocked: {json}"
    );
    assert!(
        json["message"]
            .as_str()
            .unwrap_or_default()
            .contains("VCS-action gate"),
        "the block message must name the VCS-action gate: {json}"
    );
}

/// LIFECYCLE-7: `GET /api/runs/:id` must NOT report `stalled` for a `done` run or a
/// PARKED run (AwaitingReview / AwaitingClarification), no matter how long idle. Only a
/// live, in-flight run can stall. A freshly-created (active, just-touched) run is also not
/// stalled. Exercises the handler's done/parked short-circuit end-to-end.
#[tokio::test]
async fn get_run_does_not_report_stalled_for_done_or_parked_runs() {
    use crate::run::{RunKind, RunStatus};
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));

    // Helper: GET a run and return its `stalled` flag.
    async fn stalled_flag(state: &AppState, id: &str) -> bool {
        let app = router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/runs/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await["stalled"].as_bool().unwrap()
    }

    // A fresh, active dev-implement run (just touched activity) is not stalled.
    let active = state.runs.create("CAM-live", "live", RunKind::Watched);
    state.runs.set_status(&active, RunStatus::Executing, false);
    state.runs.touch_activity(&active, None);
    assert!(
        !stalled_flag(&state, &active).await,
        "fresh active run not stalled"
    );

    // A DONE run is never stalled (even though it will sit idle forever).
    let done = state.runs.create("CAM-done", "live", RunKind::Watched);
    state.runs.set_status(&done, RunStatus::AwaitingQa, true);
    assert!(!stalled_flag(&state, &done).await, "done run never stalled");

    // A run PARKED on human review is never stalled.
    let parked = state.runs.create("CAM-parked", "live", RunKind::Watched);
    state
        .runs
        .set_status(&parked, RunStatus::AwaitingReview, false);
    assert!(
        !stalled_flag(&state, &parked).await,
        "parked run never stalled"
    );
}

#[test]
fn step_model_routes_to_active_projects_configured_model() {
    // (e) CALL-SITE ROUTING: the resolution helper every non-fleet call site uses must
    // return the ACTIVE project's per-step model — that is what gets put on the LlmRequest.
    use crate::project::StepKind;
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // No active project yet: the only floor is DEFAULT_MODEL (project-less edge).
    assert_eq!(
        step_model(&state, StepKind::StoryAuthoring),
        crate::llm::DEFAULT_MODEL,
        "project-less edge falls back to DEFAULT_MODEL"
    );

    // Create a project (becomes active) and pin its story-authoring model.
    let p = state.projects.create("Routing", vec![]).unwrap();
    state
        .projects
        .set_step_model(&p.id, StepKind::StoryAuthoring, "claude-opus-5".to_string())
        .unwrap();

    // The fallback-step resolver now returns the project's configured model (no env floor).
    let model = step_model(&state, StepKind::StoryAuthoring);
    assert_eq!(model, "claude-opus-5");
    // And it is exactly what a fallback-step call site puts on the LlmRequest.
    let req = crate::llm::LlmRequest::new("draft").with_model(model);
    assert_eq!(req.model, "claude-opus-5");

    // A different step on the same project is still the default (per-step isolation).
    assert_eq!(
        step_model(&state, StepKind::Decomposition),
        crate::llm::DEFAULT_MODEL
    );
}

#[test]
fn ui_picked_step_lets_request_model_override_project_default() {
    // UI-picked steps (audit / calibration / research chat): an explicit request model
    // wins; a blank/None request falls back to the project's per-step default.
    use crate::project::StepKind;
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("UiPick", vec![]).unwrap();
    state
        .projects
        .set_step_model(&p.id, StepKind::Audit, "claude-sonnet-5".to_string())
        .unwrap();

    // Explicit pick overrides the project default.
    assert_eq!(
        step_model_or(&state, StepKind::Audit, Some("claude-opus-5")),
        "claude-opus-5"
    );
    // Blank / None falls back to the project's per-step model.
    assert_eq!(
        step_model_or(&state, StepKind::Audit, Some("   ")),
        "claude-sonnet-5"
    );
    assert_eq!(
        step_model_or(&state, StepKind::Audit, None),
        "claude-sonnet-5"
    );
}

#[test]
fn parse_step_kind_accepts_known_keys_and_separators() {
    use crate::project::StepKind;
    assert_eq!(parse_step_kind("audit"), Some(StepKind::Audit));
    assert_eq!(
        parse_step_kind("research_chat"),
        Some(StepKind::ResearchChat)
    );
    // Tolerant of dashes/spaces/case.
    assert_eq!(
        parse_step_kind("Research-Chat"),
        Some(StepKind::ResearchChat)
    );
    assert_eq!(
        parse_step_kind("story authoring"),
        Some(StepKind::StoryAuthoring)
    );
    assert_eq!(parse_step_kind("nope"), None);
}

#[test]
fn save_armed_classifies_by_scope_and_creates_a_project() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    assert!(state.projects.active().is_none(), "clean slate, no project");
    save_armed_to_project(
        &state,
        &[
            arm_rule("REPO-1", "repo-local", &["me/api"]),
            arm_rule("XREPO-1", "cross-repo", &["me/api", "me/web"]),
            arm_rule("PROC-1", "process", &["me/api"]),
        ],
        &[],
        None,
    );
    let p = state.projects.active().expect("a project was created");
    assert_eq!(p.ruleset.selections.len(), 1, "repo-local -> selections");
    assert_eq!(p.ruleset.selections[0].rule_id, "REPO-1");
    assert_eq!(p.ruleset.cross_repo.len(), 1, "cross-repo -> cross_repo");
    assert_eq!(p.ruleset.process.len(), 1, "process -> process");
    assert!(
        p.repos.contains(&"me/web".to_string()),
        "repos absorbed into the project"
    );
}

#[test]
fn save_armed_never_persists_the_single_repo_sentinel() {
    let sentinel = "\u{0}__single_repo__".to_string();
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // One rule carries the leaked UI sentinel alongside a real repo; another is
    // sentinel-only. Neither must write a NUL into the persisted project.
    save_armed_to_project(
        &state,
        &[
            arm_rule("REPO-1", "repo-local", &[&sentinel, "me/api"]),
            arm_rule("XREPO-1", "cross-repo", &[&sentinel]),
        ],
        &[],
        None,
    );
    let p = state.projects.active().expect("a project was created");
    assert!(
        p.repos.iter().all(|r| !r.contains('\0')),
        "no sentinel in project.repos: {:?}",
        p.repos
    );
    for sel in p
        .ruleset
        .selections
        .iter()
        .chain(p.ruleset.cross_repo.iter())
    {
        assert!(
            sel.repos.iter().all(|r| !r.contains('\0')),
            "no sentinel in selection {}: {:?}",
            sel.rule_id,
            sel.repos
        );
    }
    // The sentinel on REPO-1 expanded to the real repo discovered in the same arm.
    let repo1 = p
        .ruleset
        .selections
        .iter()
        .find(|s| s.rule_id == "REPO-1")
        .unwrap();
    assert!(repo1.repos.contains(&"me/api".to_string()));
}

#[test]
fn re_arm_preserves_custom_in_the_project() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // Seed a project with a custom rule.
    let p = state
        .projects
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    state.projects.update(&p.id, |pr| {
        pr.merge_custom(&[crate::project::CustomRule {
            name: "house".into(),
            body: "Prefer X.".into(),
            domain: "*".into(),
            repos: Vec::new(),
        }]);
    });
    // Arming (saving base rules) must keep the custom rule.
    save_armed_to_project(
        &state,
        &[arm_rule("REPO-1", "repo-local", &["me/api"])],
        &[],
        None,
    );
    let after = state.projects.get(&p.id).unwrap();
    assert_eq!(after.ruleset.selections.len(), 1);
    assert_eq!(
        after.ruleset.custom.len(),
        1,
        "custom survived the re-arm upsert"
    );
}

/// The emit cascade normalization: each level implies the ones beneath it, so a malformed
/// request (push without branch) is made safe rather than pushing the current branch.
#[test]
fn emit_cascade_normalizes_each_level_to_imply_the_ones_beneath() {
    use super::normalize_emit_cascade as n;
    assert_eq!(n(false, false, false), (false, false, false)); // all off → local-only
    assert_eq!(n(true, false, false), (true, false, false)); // branch only
    assert_eq!(n(false, true, false), (true, true, false)); // push implies branch
    assert_eq!(n(false, false, true), (true, true, true)); // PR implies push + branch
    assert_eq!(n(true, true, true), (true, true, true)); // already consistent
}

/// `GET /api/routines/:id/runs` returns the routine's run history after a run-now, newest
/// first, with the gate summary recorded (Phase 1 of the Routines design: run history).
#[tokio::test]
async fn project_grounding_includes_brief_and_enabled_principles() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    state.projects.update(&p.id, |pr| {
        pr.product_brief = "Acme builds X for Y; never ship without tests.".to_string();
        pr.operating_principles = vec![
            crate::project::OperatingPrinciple {
                id: "a".to_string(),
                text: "Be explicit.".to_string(),
                enabled: true,
            },
            crate::project::OperatingPrinciple {
                id: "b".to_string(),
                text: "SECRET-DISABLED-PRINCIPLE.".to_string(),
                enabled: false,
            },
        ];
    });
    let g = state.project_grounding().await.expect("grounding present");
    assert!(g.contains("## Product context"), "brief heading present");
    assert!(g.contains("Acme builds X for Y"), "brief body present");
    assert!(
        g.contains("## How to work here"),
        "principles heading present"
    );
    assert!(g.contains("Be explicit."), "enabled principle present");
    assert!(
        !g.contains("SECRET-DISABLED-PRINCIPLE"),
        "a DISABLED principle must NOT reach grounding"
    );
}

#[tokio::test]
async fn project_grounding_includes_approved_memory_only() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    state.projects.update(&p.id, |pr| {
        pr.memory = vec![
            crate::project::MemoryEntry {
                id: "mem-1".to_string(),
                kind: crate::project::MemoryKind::Gotcha,
                text: "APPROVED-LEARNING.".to_string(),
                source: "human".to_string(),
                status: crate::project::MemoryStatus::Approved,
                created: String::new(),
            },
            crate::project::MemoryEntry {
                id: "mem-2".to_string(),
                kind: crate::project::MemoryKind::Decision,
                text: "PROPOSED-LEARNING.".to_string(),
                source: "agent:x".to_string(),
                status: crate::project::MemoryStatus::Proposed,
                created: String::new(),
            },
        ];
    });
    let g = state.project_grounding().await.expect("grounding present");
    assert!(g.contains("## What we have learned on this project"));
    assert!(g.contains("APPROVED-LEARNING"));
    assert!(g.contains("[gotcha]"), "the kind tag is rendered");
    assert!(
        !g.contains("PROPOSED-LEARNING"),
        "a PROPOSED (un-curated) entry must NOT reach grounding"
    );
}

#[tokio::test]
async fn memory_add_patch_delete_curation_flow() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    // Human add → an Approved entry with a fresh id.
    let r = add_memory(
        axum::extract::State(state.clone()),
        axum::extract::Path(p.id.clone()),
        axum::Json(AddMemoryReq {
            kind: crate::project::MemoryKind::Pattern,
            text: "  Use the repository pattern for data access.  ".to_string(),
        }),
    )
    .await
    .0;
    assert_eq!(r["ok"], true);
    let proj = state.projects.active().unwrap();
    assert_eq!(proj.memory.len(), 1);
    assert_eq!(
        proj.memory[0].status,
        crate::project::MemoryStatus::Approved
    );
    assert_eq!(
        proj.memory[0].text,
        "Use the repository pattern for data access."
    );
    let eid = proj.memory[0].id.clone();
    // Patch → archive it.
    let _ = patch_memory(
        axum::extract::State(state.clone()),
        axum::extract::Path((p.id.clone(), eid.clone())),
        axum::Json(PatchMemoryReq {
            status: Some(crate::project::MemoryStatus::Archived),
            text: None,
            kind: None,
        }),
    )
    .await;
    assert_eq!(
        state.projects.active().unwrap().memory[0].status,
        crate::project::MemoryStatus::Archived
    );
    // Delete → gone.
    let _ = delete_memory(
        axum::extract::State(state.clone()),
        axum::extract::Path((p.id.clone(), eid)),
    )
    .await;
    assert!(state.projects.active().unwrap().memory.is_empty());
}

/// `POST /api/projects/:id/reset-onboarding` clears the onboarded marker (and drops the
/// in-progress draft) but leaves the project's repos and ruleset (incl. custom rules)
/// exactly as they were — the "Reset onboarding" UI action must never look like project
/// deletion.
#[tokio::test]
async fn reset_onboarding_clears_marker_and_draft_but_preserves_repos_and_ruleset() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["me/api".to_string()])
        .expect("project created");
    state
        .projects
        .update(&p.id, |proj| {
            proj.mark_onboarded(&["me/api".to_string()]);
            proj.merge_custom(&[crate::project::CustomRule {
                name: "house-style".to_string(),
                body: "Prefer X.".to_string(),
                domain: "*".to_string(),
                repos: Vec::new(),
            }]);
        })
        .expect("project updated");
    state
        .draft
        .save(&p.id, serde_json::json!({"scan": "in-progress"}));
    assert!(!state.projects.get(&p.id).unwrap().onboarded.is_empty());
    assert!(state.draft.load(&p.id).is_some());

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/reset-onboarding", p.id))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);

    let after = state.projects.get(&p.id).unwrap();
    assert!(after.onboarded.is_empty(), "onboarded marker is cleared");
    assert_eq!(after.repos, vec!["me/api".to_string()], "repos untouched");
    assert_eq!(
        after.ruleset.custom.len(),
        1,
        "the ruleset (incl. custom rules) survives a reset"
    );
    assert!(
        state.draft.load(&p.id).is_none(),
        "the in-progress onboarding draft is dropped so a fresh scan starts clean"
    );
}

/// Resetting onboarding for an unknown project id is a clean 400, not a panic.
#[tokio::test]
async fn reset_onboarding_unknown_project_is_bad_request() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/does-not-exist/reset-onboarding")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
}

/// The design-page hierarchy endpoints: GET returns the seeded default ladder; POST replaces the
/// whole schema and it persists on the project.
#[tokio::test]
async fn hierarchy_get_default_then_post_replaces() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");

    // GET → the seeded default ladder (includes Epic).
    let got = get_hierarchy_handler(
        axum::extract::State(state.clone()),
        axum::extract::Path(p.id.clone()),
    )
    .await
    .0;
    assert_eq!(got["ok"], true);
    assert!(
        got["hierarchy"]["types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "Epic"),
        "the seeded default schema is returned"
    );

    // POST → replace with a custom schema.
    let custom = crate::project::HierarchySchema {
        types: vec![crate::project::WorkType {
            name: "Spike".to_string(),
            builtin: false,
            is_design_root: true,
        }],
        relations: vec![],
    };
    let posted = set_hierarchy_handler(
        axum::extract::State(state.clone()),
        axum::extract::Path(p.id.clone()),
        axum::Json(custom.clone()),
    )
    .await
    .0;
    assert_eq!(posted["ok"], true);

    // The replacement persisted.
    assert_eq!(
        state.projects.get(&p.id).unwrap().hierarchy_schema,
        custom,
        "POST replaced the whole schema and it persisted on the project"
    );
}

/// The chat "+ Add to learnings" button's SERVER path: GET /api/projects/active to resolve the
/// id, then POST /api/projects/:id/memory with the reply text. Exercised end to end through the
/// router (the Dioxus button + add_chat_learning helper drive exactly these two calls).
#[tokio::test]
async fn chat_add_to_learnings_server_path_resolves_active_then_adds() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    let app = router(state.clone());

    // GET active -> the active project's id (what the chat resolves first).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/projects/active")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let active = body_json(resp).await;
    let id = active["id"].as_str().expect("active project carries an id");
    assert_eq!(id, p.id);

    // POST the reply text as a learning to that id.
    let post = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{id}/memory"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"kind":"decision","text":"Captured from the chat."}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK);

    // The entry is present, human-sourced, and Approved (the human clicked = curated).
    let mem = state.projects.active().unwrap().memory;
    assert_eq!(mem.len(), 1);
    assert_eq!(mem[0].text, "Captured from the chat.");
    assert_eq!(mem[0].status, crate::project::MemoryStatus::Approved);
    assert_eq!(mem[0].source, "human");
}

/// BUG 2 contract regression: `GET /api/development/context` must emit the field
/// names the chat client (`UowSnapshot`) parses — `sign_off`, `updated`, and
/// `gate_provenance` — NOT the old `signed_off` / `last_activity` /
/// `has_gate_provenance` shape the client silently defaulted past. Before the fix
/// every story showed "not signed off" / "(unknown)" / "no run yet" even when true,
/// because the client's `#[serde(default)]` fields never matched the wire keys.
///
/// Drives a UoW scoped to the active project, records a gate provenance and a
/// sign-off, then asserts the response item carries the client-shaped keys with
/// real values (and none of the stale keys).
#[tokio::test]
async fn development_context_emits_client_expected_field_names() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");

    // A UoW scoped to the active project (project_id match), with a recorded gate
    // provenance and a sign-off so all three client-expected fields are populated.
    let uow = state.uow.create_blank_with_parent(None, Some(p.id.clone()));
    let story_id = uow.story_id.clone();
    state.uow.record_gate_provenance(
        &story_id,
        crate::uow::GateProvenance::new(
            "run-9",
            "scripted",
            4,
            1,
            vec!["SEC-NO-PATH-ESCAPE-1".to_string()],
            "2026-06-21T09:59:00Z",
        ),
    );
    state
        .uow
        .sign_off(&story_id, "zach", "run-9", Some("gate held"));

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/development/context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["ok"], true);
    let items = body["units_of_work"]
        .as_array()
        .expect("units_of_work array");
    let item = items
        .iter()
        .find(|u| u["story_id"] == serde_json::json!(story_id))
        .expect("the seeded UoW appears in the active project's context");

    // The client-expected keys are present with real values.
    assert!(
        item.get("sign_off").is_some() && !item["sign_off"].is_null(),
        "must emit `sign_off` (object) for a signed-off story, got: {item}"
    );
    assert_eq!(item["sign_off"]["by"], "zach");
    assert_eq!(item["sign_off"]["run_id"], "run-9");

    assert!(
        item.get("updated").is_some() && item["updated"].is_string(),
        "must emit `updated` (RFC 3339 string), got: {item}"
    );

    assert!(
        item.get("gate_provenance").is_some() && !item["gate_provenance"].is_null(),
        "must emit `gate_provenance` (object), got: {item}"
    );
    assert_eq!(item["gate_provenance"]["allow_count"], 4);
    assert_eq!(item["gate_provenance"]["deny_count"], 1);
    assert_eq!(item["gate_provenance"]["total_bounces"], 1);
    assert_eq!(item["gate_provenance"]["mode"], "scripted");

    // The stale wire keys the client never read are GONE.
    assert!(
        item.get("signed_off").is_none(),
        "stale boolean `signed_off` must be replaced by `sign_off`"
    );
    assert!(
        item.get("last_activity").is_none(),
        "stale `last_activity` must be replaced by `updated`"
    );
    assert!(
        item.get("has_gate_provenance").is_none(),
        "stale boolean `has_gate_provenance` must be replaced by `gate_provenance`"
    );
}

/// A project export is a backup file Zach moves between machines, not a client deliverable,
/// and a client's name should not travel in a file by default: the export carries every
/// project setting except the client name, which is left blank (and so is blank again on
/// import, where the operator sets it for that engagement).
#[tokio::test]
async fn a_project_export_file_does_not_carry_the_client_name() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project created");
    state
        .projects
        .update(&p.id, |p| p.client_name = "Example Client Ltd".to_string())
        .expect("project updated");
    let app = router(state.clone());

    let exp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{}/export", p.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(exp.status(), StatusCode::OK);
    let doc = body_json(exp).await;
    assert_eq!(doc["client_name"], "", "{doc}");
    assert!(
        !doc.to_string().contains("Example Client Ltd"),
        "the client name must not appear anywhere in the export: {doc}"
    );
    // The rest of the project still travels.
    assert_eq!(doc["name"], "Acme");
    assert_eq!(doc["repos"][0], "acme/portal");
    // The stored project is untouched: only the file is redacted.
    assert_eq!(
        state.projects.get(&p.id).expect("project").client_name,
        "Example Client Ltd"
    );
}

/// End-to-end THROUGH THE HTTP ROUTER: the design-page hierarchy round-trips — GET returns the
/// seeded default ladder, POST replaces it with a custom graph, a second GET reflects the change,
/// and the project export carries the schema (portability). Exercises routing + (de)serialization
/// + persistence in one flow, exactly as the drag-and-drop builder drives it.
#[tokio::test]
async fn hierarchy_schema_http_round_trip_and_export() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    let app = router(state.clone());
    let id = p.id.clone();

    // GET → the seeded default ladder (includes Epic).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{id}/hierarchy"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_json(resp).await;
    assert_eq!(got["ok"], true);
    let names: Vec<&str> = got["hierarchy"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Epic"),
        "seeded default includes Epic; got {names:?}"
    );

    // POST → replace with a custom graph.
    let post = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{id}/hierarchy"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"types":[{"name":"Objective","builtin":false,"is_design_root":true}],"relations":[{"parent":"Objective","child":"KeyResult"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::OK);

    // GET again → the custom schema is reflected.
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{id}/hierarchy"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let got2 = body_json(resp2).await;
    assert_eq!(got2["hierarchy"]["types"][0]["name"], "Objective");
    assert_eq!(got2["hierarchy"]["relations"][0]["child"], "KeyResult");

    // Export carries the schema (portability — travels with the project).
    let exp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{id}/export"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(exp.status(), StatusCode::OK);
    let doc = body_json(exp).await;
    // ProjectExportDoc #[serde(flatten)]s the project, so its fields are at the top level.
    assert_eq!(
        doc["hierarchy_schema"]["types"][0]["name"], "Objective",
        "the schema travels in the project export"
    );
}

/// The empty-hierarchy-schema bug fix (design canvas "planner proposes zero children"):
/// `effective_hierarchy_schema` must resolve to the seeded DEFAULT ladder — so an `Epic`
/// resolves allowed children `["Feature"]`, NOT `[]` — in every degenerate case:
///   (a) no active project at all,
///   (b) an active project whose stored schema was seeded EMPTY (e.g. a bare import).
/// An empty schema is what made design-author emit `ALLOWED_CHILD_TYPES: []` and strip every
/// proposal, and made materialize reject every child.
#[test]
fn effective_hierarchy_schema_falls_back_to_default_ladder() {
    let allowed_children =
        |schema: &crate::project::HierarchySchema, parent: &str| -> Vec<String> {
            schema
                .relations
                .iter()
                .filter(|r| r.parent == parent)
                .map(|r| r.child.clone())
                .collect::<Vec<_>>()
        };

    // (a) No active project → default ladder.
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    assert!(state.projects.active().is_none(), "no project yet");
    let schema = state.effective_hierarchy_schema();
    assert!(!schema.types.is_empty(), "must not be the empty default");
    assert_eq!(
        allowed_children(&schema, "Epic"),
        vec!["Feature".to_string()],
        "Epic's allowed children resolve to the default ladder, not []",
    );

    // (b) Active project with an EMPTY stored schema (bare-import shape) → default ladder.
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    state
        .projects
        .set_hierarchy_schema(&p.id, crate::project::HierarchySchema::default())
        .expect("schema set");
    assert!(
        state
            .projects
            .active()
            .unwrap()
            .hierarchy_schema
            .types
            .is_empty(),
        "stored schema is empty",
    );
    let schema = state.effective_hierarchy_schema();
    assert_eq!(
        allowed_children(&schema, "Epic"),
        vec!["Feature".to_string()],
        "an empty stored schema still resolves to the default ladder",
    );

    // (c) Active project with a real custom schema → used verbatim (no clobbering).
    state
        .projects
        .set_hierarchy_schema(
            &p.id,
            crate::project::HierarchySchema {
                types: vec![crate::project::WorkType {
                    name: "Objective".into(),
                    builtin: false,
                    is_design_root: true,
                }],
                relations: vec![crate::project::TypeRelation {
                    parent: "Objective".into(),
                    child: "KeyResult".into(),
                }],
            },
        )
        .expect("schema set");
    let schema = state.effective_hierarchy_schema();
    assert_eq!(
        allowed_children(&schema, "Objective"),
        vec!["KeyResult".to_string()],
        "a usable custom schema is used verbatim",
    );
    assert!(
        allowed_children(&schema, "Epic").is_empty(),
        "the custom schema does not carry the default ladder's Epic relation",
    );
}

/// The view/edit modal's Save POSTs {text, kind} to /memory/:eid. Confirm patch edits both.
#[tokio::test]
async fn memory_edit_modal_patches_text_and_kind() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    let added = add_memory(
        axum::extract::State(state.clone()),
        axum::extract::Path(p.id.clone()),
        axum::Json(AddMemoryReq {
            kind: crate::project::MemoryKind::Decision,
            text: "original".to_string(),
        }),
    )
    .await;
    assert_eq!(
        added.0["ok"], true,
        "add_memory reports success: {}",
        added.0
    );
    let eid = state.projects.active().unwrap().memory[0].id.clone();
    // Edit text + kind (what the modal Save does).
    let patched = patch_memory(
        axum::extract::State(state.clone()),
        axum::extract::Path((p.id.clone(), eid)),
        axum::Json(PatchMemoryReq {
            status: None,
            text: Some("  edited learning  ".to_string()),
            kind: Some(crate::project::MemoryKind::Gotcha),
        }),
    )
    .await;
    assert_eq!(
        patched.0["ok"], true,
        "patch_memory reports success: {}",
        patched.0
    );
    let m = state.projects.active().unwrap().memory[0].clone();
    assert_eq!(m.text, "edited learning", "text edited + trimmed");
    assert_eq!(m.kind, crate::project::MemoryKind::Gotcha, "kind edited");
    assert_eq!(
        m.status,
        crate::project::MemoryStatus::Approved,
        "status untouched when not in the patch"
    );
}

#[tokio::test]
async fn routine_runs_endpoint_returns_history_after_run_now() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let r = state.routines.create(&crate::routine::CreateRoutineReq {
        name: "Nightly".to_string(),
        schedule: "manual".to_string(),
        intent: "scan".to_string(),
        prompt: "P".to_string(),
        scope: "read-only".to_string(),
        model: None,
        project_id: None,
    });
    let app = router(state);
    // No runs yet.
    let empty = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/routines/{}/runs", r.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::OK);
    assert!(body_json(empty).await.as_array().unwrap().is_empty());
    // Run it, then the history has one entry.
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/routines/{}/run", r.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let runs = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/routines/{}/runs", r.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(runs.status(), StatusCode::OK);
    let arr = body_json(runs).await;
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one run recorded after run-now");
    assert_eq!(arr[0]["trigger"], "manual");
    assert_eq!(arr[0]["summary"]["denies"], 2); // scripted gate
}

/// Helper: simulate a PAUSED governed run (what Engine C does at the test-tamper site) by
/// raising a UoW review escalation + creating + linking its checkpoint. Returns (esc_id,
/// ckpt_id).
fn seed_paused_uow_run(state: &AppState, story_id: &str) -> (String, String) {
    let esc = state.escalations.raise(
        crate::escalation::RaiseEscalationReq {
            subject_kind: crate::escalation::SubjectKind::Uow,
            checkpoint_id: None,
            routine_id: story_id.to_string(),
            reason: "AGENTIC-NO-TEST-TAMPER-1 — agent modified an existing test".to_string(),
            stopped_for: "Confirm the test edit is a legit refactor.".to_string(),
            suggestions: vec![],
            raw_context: String::new(),
        },
        "Add login",
    );
    let ckpt = state.checkpoints.create(crate::checkpoint::NewCheckpoint {
        story_id: story_id.to_string(),
        run_id: "run-orig".to_string(),
        escalation_id: esc.id.clone(),
        pause_reason: "test-tamper".to_string(),
        repo: "me/api".to_string(),
        branch: "camerata/me-api-7".to_string(),
        worktree_dir: "/camerata/nonexistent-wt".to_string(),
        base_commit: "abc123".to_string(),
        iteration: 0,
        max_iterations: 1,
        model: "claude-opus-5".to_string(),
        project_id: None,
    });
    state.escalations.set_checkpoint(&esc.id, &ckpt.id);
    (esc.id, ckpt.id)
}

/// E2E of the Governed Development review loop, APPROVE path (scripted): a paused run's
/// escalation, resolved with Approve, RESUMES from its checkpoint (consuming it once-only) and
/// records the decision on the UoW trail. Drives the real `answer_escalation` handler.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn uow_escalation_approve_resumes_and_consumes_checkpoint() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let (esc_id, ckpt_id) = seed_paused_uow_run(&state, "me/api#7");
    // The UoW review is discoverable in the NEEDS YOU surface before resolution.
    assert_eq!(
        state.escalations.list_open_uow().len(),
        1,
        "open UoW review -> NEEDS YOU"
    );

    let resolved = answer_escalation(
        axum::extract::State(state.clone()),
        axum::extract::Path(esc_id.clone()),
        axum::Json(crate::escalation::AnswerEscalationReq {
            answer: "Approved: the test change is a legitimate refactor; proceed.".to_string(),
            action: crate::escalation::EscalationAction::Approve,
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("answer_escalation should succeed"))
    .0;
    assert_eq!(resolved["status"], "resolved");
    // A successful resume surfaces the NEW run id so the client can follow it.
    assert!(
        resolved["resume_run_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "approve returns the resumed run id: {resolved}"
    );
    // Approve RESUMES: the checkpoint is consumed (enforcing once-only resume) and no longer
    // shows in NEEDS YOU.
    assert!(
        state.checkpoints.get(&ckpt_id).unwrap().resumed.is_some(),
        "approve resumes -> checkpoint consumed"
    );
    assert_eq!(
        state.escalations.list_open_uow().len(),
        0,
        "resolved -> off NEEDS YOU"
    );
    // The decision is recorded on the UoW's durable history. Without a live agent the resume
    // REFUSES honestly (it never fabricates a continued cycle), so the trail records the
    // refusal rather than a fake success.
    let uow = state.uow.get_or_create("me/api#7");
    assert!(
        uow.history
            .iter()
            .any(|h| h.text.contains("Resume refused")),
        "approve records the honest resume refusal on the UoW trail"
    );
}

/// E2E of the REJECT path (scripted): resolving with Reject does NOT resume — it consumes the
/// checkpoint (so the run can't be re-spawned) and records the abandonment. Drives the real
/// `answer_escalation` handler.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn uow_escalation_reject_stops_and_consumes_checkpoint() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let (esc_id, ckpt_id) = seed_paused_uow_run(&state, "me/api#8");

    let resolved = answer_escalation(
        axum::extract::State(state.clone()),
        axum::extract::Path(esc_id.clone()),
        axum::Json(crate::escalation::AnswerEscalationReq {
            answer: "Reject: the agent weakened a real assertion to make it pass. Fix the code."
                .to_string(),
            action: crate::escalation::EscalationAction::Reject,
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("answer_escalation should succeed"))
    .0;
    assert_eq!(resolved["status"], "resolved");
    // Reject does NOT resume, so no run id is surfaced.
    assert!(
        resolved["resume_run_id"].is_null(),
        "reject surfaces no resumed run id: {resolved}"
    );
    // Reject consumes the checkpoint (stop cleanly; no resume).
    assert!(
        state.checkpoints.get(&ckpt_id).unwrap().resumed.is_some(),
        "reject consumes the checkpoint -> not resumable"
    );
    // The rejection is recorded on the UoW trail.
    let uow = state.uow.get_or_create("me/api#8");
    assert!(
        uow.history.iter().any(|h| h.text.contains("REJECTED")),
        "reject records the abandonment on the UoW trail"
    );
}

// ── `run_id` stamped onto open clarifications (the client's other Bombe-blind-spot fix) ──
//
// A clarification that paused a governed investigation run is resumed via a DETACHED
// `tokio::spawn` (see `answer_clarification`) — the client never learns the resumed run's id
// from the answer response itself, because it's the SAME id the run always had. So the list
// endpoints stamp `run_id` onto each OPEN clarification up front (non-destructively, via
// `with_resume_run_ids`) so the UI already knows it before the human even answers, and can
// re-attach `poll_run_to_done` on it — keeping the Bombe animating through the resume even
// when answered from the app-wide NEEDS YOU queue, where no page was already polling it.

#[test]
fn with_resume_run_ids_stamps_only_the_paused_clarification() {
    let resume = crate::clarify_resume::ClarifyResumeStore::new();
    resume.put(
        "clar-1",
        crate::clarify_resume::ClarifyResumeContext {
            run_id: "run-9".to_string(),
            story_id: "me/api#3".to_string(),
            story_title: "Add export".to_string(),
            story_desc: "desc".to_string(),
            model: "claude-opus-5".to_string(),
            phase: crate::clarify_resume::PausedPhase::Investigation,
            original_task: "Analyze the story.".to_string(),
            asked_question: "Include archived rows?".to_string(),
        },
    );
    let mk = |id: &str, story_id: &str| Clarification {
        id: id.to_string(),
        story_id: story_id.to_string(),
        question: "Q".to_string(),
        addressee: "you".to_string(),
        options: vec![],
        multi_select: false,
        allow_free_text: true,
        answer: None,
        answer_selection: None,
        answered_by: None,
        state: crate::clarify::ClarifyState::Asked,
        run_id: None,
    };
    let clars = vec![mk("clar-1", "me/api#3"), mk("clar-2", "me/api#4")];

    let stamped = with_resume_run_ids(&resume, clars);

    assert_eq!(
        stamped
            .iter()
            .find(|c| c.id == "clar-1")
            .unwrap()
            .run_id
            .as_deref(),
        Some("run-9"),
        "the clarification with a parked resume context gets its run_id"
    );
    assert!(
        stamped
            .iter()
            .find(|c| c.id == "clar-2")
            .unwrap()
            .run_id
            .is_none(),
        "a free-standing clarification (no parked run) stays None"
    );
    // Non-destructive: a SECOND stamp still finds it (list endpoints must never consume the
    // pause point — only actually answering does, via `ClarifyResumeStore::take`).
    assert!(
        resume.get("clar-1").is_some(),
        "with_resume_run_ids used .get, not .take"
    );
}

#[tokio::test]
async fn list_clarifications_handler_stamps_run_id_for_a_paused_investigation() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let posted = state.clarifications.post("me/api#5", "Which flag?", "you");
    state.clarify_resume.put(
        &posted.id,
        crate::clarify_resume::ClarifyResumeContext {
            run_id: "run-55".to_string(),
            story_id: "me/api#5".to_string(),
            story_title: "T".to_string(),
            story_desc: "D".to_string(),
            model: "claude-opus-5".to_string(),
            phase: crate::clarify_resume::PausedPhase::Investigation,
            original_task: "task".to_string(),
            asked_question: "Which flag?".to_string(),
        },
    );

    let Json(out) = list_clarifications(
        axum::extract::State(state.clone()),
        axum::extract::Path("me/api#5".to_string()),
    )
    .await;

    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].run_id.as_deref(),
        Some("run-55"),
        "the handler stamps run_id from the resume-context store onto the list response"
    );
    // Still non-destructive at the HTTP layer too: answering later can still consume it.
    assert!(state.clarify_resume.get(&posted.id).is_some());
}

/// Regression test for issue #106: `POST /api/projects/:id/emit-local` must run
/// unconditionally for an already-onboarded project (no onboarding-state gate).
///
/// This test verifies the route is wired and the handler logic executes regardless
/// of whether the project is onboarded. With no workspace configured the handler
/// reports the expected "no local path" error (not a 404, not a silent no-op, and
/// not the "already onboarded" gate that blocked all re-emits before this fix).
///
/// A full end-to-end local-write test requires a real git repo with a pushable
/// remote (the apply_local_and_push codepath pushes the branch). The pure-function
/// layer (arm_files_for_repo always produces current content) is covered by the
/// `re_emit_for_onboarded_project_always_writes_current_content` test in arm.rs.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn emit_project_local_route_runs_for_onboarded_project_no_workspace_guard() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));

    // Create a project, seed it with a rule, and mark it as ALREADY ONBOARDED.
    let p = state
        .projects
        .create("Onboarded", vec!["me/api".to_string()])
        .unwrap();
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: "RUST-DOMAIN-6".to_string(),
            chosen_option: None,
            repos: vec!["me/api".to_string()],
            ..Default::default()
        });
        proj.mark_onboarded(&["me/api".to_string()]);
    });
    let onboarded = state.projects.get(&p.id).unwrap();
    assert!(
        onboarded.onboarded.contains(&"me/api".to_string()),
        "project must be onboarded before calling emit-local"
    );

    // Call the endpoint. No workspace is set, so the handler reports the "no local
    // path" message — the point is that it RUNS (200 ok:false, not 404 or no-op).
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{}/emit-local", p.id))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // The route MUST exist (not 404) and must return 200.
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "emit-local endpoint must be reachable for an onboarded project"
    );
    let json = body_json(resp).await;
    // The handler reaches the emit logic (no silent no-op, no onboarding-state guard).
    // With no workspace set it returns ok:false + the actionable message.
    assert_eq!(
        json["ok"], false,
        "with no workspace, emit-local reports ok:false (not a silent no-op): {json:?}"
    );
    let msg = json["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("workspace") || msg.contains("local") || msg.contains("emit"),
        "message must explain the missing workspace, got: {msg}"
    );
}
