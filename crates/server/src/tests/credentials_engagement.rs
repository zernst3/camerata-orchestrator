//! Model registry, credential, client-engagement and backend resolution tests.

use super::*;

/// `use_live_anthropic_models`: the pure decision `get_model_registry` /
/// `refresh_model_registry` / `refresh_anthropic_registry` all consult. False on a
/// fresh CLI-default install (no backend setting, no key).
#[test]
fn use_live_anthropic_models_false_by_default() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(!use_live_anthropic_models(&state));
}

/// True only once BOTH conditions hold: effective backend is `api` AND a key is
/// present. Neither alone is enough — this is the exact two-input gate the design doc
/// describes for Feature A ("the picker's available Claude models become a function
/// of the active backend").
#[test]
#[serial_test::serial(process_env)]
fn use_live_anthropic_models_requires_both_api_backend_and_key() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    // API backend selected, but no key yet.
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    assert!(
        !use_live_anthropic_models(&state),
        "api backend alone, no key, must stay false"
    );

    // Key present, but backend still `api` — now both hold.
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test-key")
        .unwrap();
    assert!(
        use_live_anthropic_models(&state),
        "api backend + key present must be true"
    );

    // Flip back to CLI: even with the key present, must be false.
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Cli);
    assert!(
        !use_live_anthropic_models(&state),
        "cli backend must stay false even with a key"
    );
}

/// `GET /api/models/registry` on the CLI backend (the default) serves the hardcoded
/// Claude catalog, even when a live Anthropic cache happens to be populated (e.g. left
/// over from a prior API-backend session) — the CLI backend must never see the live
/// list.
#[tokio::test]
async fn get_model_registry_serves_hardcoded_claude_on_cli_backend() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .model_registry
        .seed_anthropic_entries(vec![crate::model_registry::RegistryEntry {
            provider: "claude".to_string(),
            display: "Live-Only Model".to_string(),
            id: "claude-live-only".to_string(),
            free: false,
            tool_use: true,
            context: 0,
            coding: 1.0,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: true,
            vision: true,
            price_known: false,
        }]);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/models/registry")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let models = json["models"].as_array().expect("models array");
    assert!(
        models.iter().any(|m| m["id"] == "claude-opus-5"),
        "CLI backend must serve the hardcoded catalog: {models:?}"
    );
    assert!(
        !models.iter().any(|m| m["id"] == "claude-live-only"),
        "CLI backend must NOT leak the live cache even when populated: {models:?}"
    );
}

/// `GET /api/models/registry` on the API backend with a key present AND a populated
/// live cache serves the LIVE list instead of the hardcoded catalog — this is Feature
/// A's whole point (a model like `claude-fable-5` shows up in the picker iff the
/// account is actually served it, which the live fetch — not the hardcoded list —
/// determines).
#[tokio::test]
async fn get_model_registry_serves_live_list_on_api_backend_with_key_and_cache() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test-key")
        .unwrap();
    state
        .model_registry
        .seed_anthropic_entries(vec![crate::model_registry::RegistryEntry {
            provider: "claude".to_string(),
            display: "Live-Only Model".to_string(),
            id: "claude-live-only".to_string(),
            free: false,
            tool_use: true,
            context: 0,
            coding: 1.0,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: true,
            vision: true,
            price_known: false,
        }]);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/models/registry")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let models = json["models"].as_array().expect("models array");
    assert!(
        models.iter().any(|m| m["id"] == "claude-live-only"),
        "API backend with key + populated cache must serve the LIVE list: {models:?}"
    );
    assert!(
        !models.iter().any(|m| m["id"] == "claude-opus-5"),
        "the live list REPLACES the hardcoded catalog, it doesn't union with it: {models:?}"
    );
    assert!(json["anthropic_fetched"].as_bool().unwrap_or(false));
}

/// `GET /api/models/registry` on the API backend WITH a key but a cache that was never
/// populated (fresh install, or the live fetch failed) must still serve the hardcoded
/// catalog — the fail-soft floor from `ModelRegistry::claude_entries_for_backend`
/// surfaced all the way through the HTTP handler.
#[tokio::test]
async fn get_model_registry_falls_back_to_hardcoded_when_api_backend_but_no_live_cache() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test-key")
        .unwrap();
    // No seed_anthropic_entries call: cache stays unpopulated.
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/models/registry")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let models = json["models"].as_array().expect("models array");
    assert!(
        models.iter().any(|m| m["id"] == "claude-opus-5"),
        "no live cache yet: must fall back to the hardcoded catalog: {models:?}"
    );
    assert!(!json["anthropic_fetched"].as_bool().unwrap_or(true));
}

/// `POST /api/models/registry/refresh/anthropic` with no Anthropic key present
/// returns 200 with `attempted: false` (never a 5xx for a missing key) and the
/// registry it echoes back still serves the hardcoded catalog.
#[tokio::test]
async fn refresh_anthropic_registry_endpoint_no_key_is_a_graceful_noop() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models/registry/refresh/anthropic")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["attempted"], false);
    let models = json["models"].as_array().expect("models array");
    assert!(models.iter().any(|m| m["id"] == "claude-opus-5"));
}

/// ROUTES-9: saving the Anthropic key via `POST /api/credentials/anthropic_api_key`
/// stores it in the credential store WITHOUT mutating the `ANTHROPIC_API_KEY` process-env
/// var. The old handler mirrored the value into env via `std::env::set_var` from the
/// request thread — POSIX UB against worker-thread `getenv`, and the concurrent writer made
/// the credential env-fallback test flaky. The driver path now reads the key store-first, so
/// no env mutation is needed for a freshly-saved key to take effect.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn set_anthropic_credential_does_not_mutate_process_env() {
    // Sentinel value the handler must NOT overwrite.
    let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-sentinel");

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/credentials/anthropic_api_key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value":"sk-ant-freshly-saved"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The key is in the store...
    assert_eq!(
        state
            .credential_store
            .get(crate::credentials::ANTHROPIC_API_KEY)
            .unwrap()
            .as_deref(),
        Some("sk-ant-freshly-saved"),
    );
    // ...but the process env was left exactly as it was (no per-request set_var).
    assert_eq!(
        std::env::var("ANTHROPIC_API_KEY").as_deref(),
        Ok("sk-ant-sentinel"),
        "the handler must not mutate ANTHROPIC_API_KEY"
    );
}

/// The defect this guards: a key saved in the running app passed the gate but the audit /
/// rescan `Llm` (which read ONLY env) failed with "key unset" until a restart. Saving through
/// the real endpoint, with env unset, must make the gate say present AND the very next `Llm`
/// the server builds carry that key; replacing the key must change the next `Llm` too.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn key_saved_through_the_endpoint_reaches_the_gate_and_the_next_llm() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let llm_key = |state: &AppState| {
        crate::onboard::build_llm(
            state.credential_store.as_ref(),
            crate::llm::Backend::Api,
            Some(state.usage_ledger.clone()),
        )
        .api_key()
        .map(str::to_string)
    };
    assert!(!anthropic_api_key_present(state.credential_store.as_ref()));
    assert_eq!(llm_key(&state), None);

    for key in ["sk-ant-first", "sk-ant-replacement"] {
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/credentials/anthropic_api_key")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"value":"{key}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(anthropic_api_key_present(state.credential_store.as_ref()));
        assert_eq!(llm_key(&state).as_deref(), Some(key));
    }
}

/// Send one credentials request through the real router and return (status, body).
async fn credential_request(
    state: &AppState,
    method: &str,
    name: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(format!("/api/credentials/{name}"));
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    let resp = router(state.clone())
        .oneshot(b.body(Body::from(body.unwrap_or("").to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

/// A key saved in the app beats the env var, so there must be a way to REMOVE it: DELETE
/// clears the store and the env fallback takes over again; deleting what is not set is fine.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn delete_credential_removes_the_saved_key_and_hands_back_to_env() {
    let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let (status, _) = credential_request(
        &state,
        "POST",
        "anthropic_api_key",
        Some(r#"{"value":"sk-ant-saved"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        crate::credentials::anthropic_api_key(state.credential_store.as_ref()).as_deref(),
        Some("sk-ant-saved")
    );

    let (status, json) = credential_request(&state, "DELETE", "anthropic_api_key", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], true);
    assert!(!state.credential_store.is_set("anthropic_api_key").unwrap());
    assert_eq!(
        crate::credentials::anthropic_api_key(state.credential_store.as_ref()).as_deref(),
        Some("sk-ant-env"),
        "with the saved key removed the env fallback applies again"
    );

    // Removing a credential that is not set succeeds; an unknown name is refused.
    let (status, _) = credential_request(&state, "DELETE", "anthropic_api_key", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, json) = credential_request(&state, "DELETE", "not_a_credential", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["ok"], false);
}

/// A whitespace-only value is not a credential: the resolver would treat it as absent, so
/// saving one must be refused instead of showing "set" in the UI.
#[tokio::test]
async fn set_credential_rejects_whitespace_only_values() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    for body in [r#"{"value":"   "}"#, "{\"value\":\"\\n\\t \"}"] {
        let (status, json) =
            credential_request(&state, "POST", "anthropic_api_key", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(json["ok"], false);
    }
    assert!(!state.credential_store.is_set("anthropic_api_key").unwrap());
}

/// The same defect class as the Anthropic key: the work-tracker provider carries the GitHub
/// token, so a token saved (or removed) in the app must re-select it with no restart.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn saving_or_removing_the_github_token_reselects_the_provider_without_a_restart() {
    let _t = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let _r = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_REPO");
    let _o = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_OWNER");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(!state.provider.current().live, "no token: native provider");

    let (status, _) = credential_request(
        &state,
        "POST",
        "github_token",
        Some(r#"{"value":"ghp_saved_after_launch"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let live = state.provider.current();
    assert!(
        live.live,
        "a token saved now must wire the GitHub provider now"
    );
    assert!(live.label.contains("github"), "{}", live.label);

    let (status, _) = credential_request(&state, "DELETE", "github_token", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !state.provider.current().live,
        "removing the token (no env fallback) goes back to the native provider"
    );

    // A credential that is not the GitHub token never touches the provider.
    let before = state.provider.current().label;
    credential_request(
        &state,
        "POST",
        "openrouter_api_key",
        Some(r#"{"value":"sk-or-x"}"#),
    )
    .await;
    assert_eq!(state.provider.current().label, before);
}

/// A project whose backend is `backend`, made active.
fn state_with_active_project(backend: crate::project::ProjectBackend) -> AppState {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("StepLlm", vec![]).unwrap();
    state.projects.set_backend(&p.id, backend).unwrap();
    // These tests are about backend resolution, not the client lock: internal development.
    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    state
}

/// H2 review: with no active project there is no setup to say the work is not client work, so
/// the cli-only runs (investigation, branch update, PR feedback) are refused like a scan.
#[tokio::test]
async fn the_cli_only_runs_are_refused_with_no_active_project() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(state.projects.active().is_none());
    for run in [
        "the investigation",
        "the branch update",
        "PR feedback resolution",
    ] {
        let why = cli_only_run_client_lock_refusal(&state, run)
            .unwrap_or_else(|| panic!("{run} with no project must be refused"));
        assert!(why.contains("no active project"), "{why}");
    }
    let run_id = spawn_investigation_agent_run(&state, "story-1", None).await;
    let run = state.runs.get(&run_id).expect("the run exists");
    assert!(run.done);
    assert!(
        run.failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("no active project"),
        "{:?}",
        run.failure_reason
    );
}

/// H2 review: the chat assistant follows the chat backend setting, which may be `cli`. When
/// the active project is a client engagement, a chat that would run on a backend the lock
/// forbids is refused with the lock's message; the maintainer's own project is not locked,
/// and neither is a chat with no active project (no client context).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_engagement_refuses_a_chat_on_the_cli_backend() {
    let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-test-not-real");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    // No project: the chat is not part of any engagement.
    assert!(state.chat_llm().is_ok());

    let p = state.projects.create("Client", vec![]).unwrap();
    assert!(p.client_engagement);
    let why = state
        .chat_llm_for(Some(&p))
        .expect_err("cli chat refused on client work");
    assert!(why.contains("`cli` backend"), "{why}");

    // Over HTTP the chat answers 409 with that message.
    let resp = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"prompt":"hi","project_id":"{}"}}"#,
                    p.id
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert!(body_json(resp).await["error"]
        .as_str()
        .unwrap()
        .contains("`cli` backend"));

    // On the API backend the lock passes; off client work the cli chat passes.
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Api);
    assert!(state.chat_llm_for(Some(&p)).is_ok());
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    let p = state.projects.get(&p.id).unwrap();
    assert!(state.chat_llm_for(Some(&p)).is_ok());
}

fn chat_req(project_id: Option<&str>, system: Option<&str>) -> ChatReq {
    ChatReq {
        prompt: "What model is this?".to_string(),
        model: String::new(),
        system: system.map(str::to_string),
        project_id: project_id.map(str::to_string),
        history: Vec::new(),
    }
}

/// Part 3: the chat's lock follows the project the REQUEST names, never the server's stale
/// active project. Home screen (no project id, no project layer) on the CLI is allowed even
/// while a client engagement is the server's last active project.
#[test]
#[serial_test::serial(process_env)]
fn chat_with_no_project_runs_on_cli_even_when_a_stale_client_project_is_active() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    let stale = state.projects.create("Stale client", vec![]).unwrap();
    assert!(stale.client_engagement);
    assert!(state.projects.set_active(&stale.id));
    assert!(
        state.projects.active().is_some(),
        "the stale active project"
    );
    let req = chat_req(
        None,
        Some("=== LAYER 1: CAMERATA TECHNICAL REFERENCE ===\n"),
    );
    assert!(chat_lock(&state, &req, "").is_ok());
}

/// The phase chats (investigation and development) post a system prompt headed `=== STORY ===`
/// or `=== INVESTIGATION NOTE ===`. Posted with NO project id while a client engagement is the
/// server's active project on the cli (the old "bare body" shape), they are refused by the
/// header allowlist, over HTTP too (refused before any model is called).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_phase_chat_without_a_project_id_is_refused_while_a_client_project_is_active() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    let p = state.projects.create("Client", vec![]).unwrap();
    assert!(state.projects.set_active(&p.id));
    for system in [
        "You are the development agent.\n=== STORY ===\nS-1\n",
        "You are the investigation agent.\n=== INVESTIGATION NOTE ===\nsecret client note\n",
    ] {
        let req = chat_req(None, Some(system));
        let err = chat_lock(&state, &req, "").expect_err(system);
        assert_eq!(err.status, StatusCode::CONFLICT, "{system}");
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "prompt": "hi", "system": system }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{system}");
    }
    // The same phase chat NAMING the client project is refused on the cli by the lock itself.
    let named = chat_req(Some(&p.id), Some("=== STORY ===\nS-1\n"));
    let err = chat_lock(&state, &named, "").expect_err("client on cli");
    assert!(err.err.to_string().contains("`cli` backend"));
}

/// With no project id, ONLY the static layer headers (LAYER 1, 1b, 2) are allowed; any other
/// `=== ` header line is refused, including ones no layer defines today.
#[test]
fn chat_with_no_project_allows_only_the_static_layer_headers() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    let static_only = "intro\n=== LAYER 1: CAMERATA TECHNICAL REFERENCE ===\ndoc\n\
                       === LAYER 1b: CAMERATA USER GUIDE ===\nguide\n\
                       === LAYER 2: GOVERNANCE RULES CATALOG (every rule) ===\nR-1\n";
    assert!(chat_lock(&state, &chat_req(None, Some(static_only)), "").is_ok());
    // A bare prompt with no system is allowed too.
    assert!(chat_lock(&state, &chat_req(None, None), "").is_ok());
    for header in [
        "=== STORY ===",
        "=== INVESTIGATION NOTE ===",
        "=== APPROVED DECISIONS (x) ===",
        "=== SOMETHING NEW ===",
        "   === INDENTED ===",
    ] {
        let sys = format!("{static_only}{header}\nbody\n");
        let err = chat_lock(&state, &chat_req(None, Some(&sys)), "").expect_err(header);
        assert_eq!(err.status, StatusCode::CONFLICT, "{header}");
    }
}

/// A client-engagement project named by the request is refused on the CLI, at the server.
#[test]
fn chat_naming_a_client_project_on_cli_is_refused() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    let p = state.projects.create("Client", vec![]).unwrap();
    let req = chat_req(Some(&p.id), None);
    let err = chat_lock(&state, &req, "").expect_err("refused");
    assert_eq!(err.status, StatusCode::CONFLICT);
}

/// A non-client project named by the request on the CLI is allowed.
#[test]
fn chat_naming_a_non_client_project_on_cli_is_allowed() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    let p = state.projects.create("Mine", vec![]).unwrap();
    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    let req = chat_req(Some(&p.id), Some("=== LAYER 3c: SCAN ===\n"));
    assert!(chat_lock(&state, &req, "").is_ok());
}

/// Project data without a project id cannot dodge the lock: every project layer header is
/// refused when the request names no project, and an unknown project id is refused too.
#[test]
fn chat_with_project_layers_but_no_project_is_refused() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Cli);
    for marker in [
        "=== LAYER 3: LIVE DEVELOPMENT STATE",
        "=== LAYER 3b: PULLED ISSUES",
        "=== LAYER 3c: ACTIVE PROJECT SCAN RESULTS",
        "=== LAYER 3d: SELECTED RULES",
        "=== LAYER 3e: PROJECT RULESET",
        "=== LAYER 4: FOCUSED FINDING",
    ] {
        let req = chat_req(None, Some(&format!("intro\n{marker}\nbody")));
        let err = chat_lock(&state, &req, "").expect_err(marker);
        assert_eq!(err.status, StatusCode::CONFLICT, "{marker}");
    }
    let err =
        chat_lock(&state, &chat_req(Some("no-such-project"), None), "").expect_err("unknown id");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
}

/// H2: the investigation run has only the CLI driver, so a client engagement fails it at
/// once with the reason; the maintainer's own project is not locked.
#[tokio::test]
async fn a_client_engagement_refuses_the_cli_only_runs() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("Client", vec![]).unwrap();
    let why = cli_only_run_client_lock_refusal(&state, "the investigation")
        .expect("a new project is a client engagement");
    assert!(why.contains("Claude Code subscription"), "{why}");

    let run_id = spawn_investigation_agent_run(&state, "story-1", None).await;
    let run = state.runs.get(&run_id).expect("the run exists");
    assert!(run.done);
    assert!(
        run.failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("client engagement"),
        "the run failed with the lock's reason: {:?}",
        run.failure_reason
    );

    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    assert_eq!(
        cli_only_run_client_lock_refusal(&state, "the investigation"),
        None
    );
}

/// H2: a client engagement's model steps never run on the subscription.
#[test]
fn a_client_engagement_refuses_model_steps_on_the_cli_backend() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("Client", vec![]).unwrap();
    assert!(p.client_engagement, "a new project is a client engagement");
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Cli)
        .unwrap();
    let refusal = state
        .step_llm()
        .expect_err("cli is refused for client work");
    assert!(refusal.contains("`cli` backend"), "{refusal}");
    assert!(refusal.contains("Client engagement"), "{refusal}");
}

/// H2 review: the model steps that take a model id from the request (the routine prompt
/// draft, the escalation review chat, the chat) check it against the active project's lock.
/// `llm_for_project` cannot (it is handed no model), so on a client engagement a non-Anthropic
/// id would otherwise reach OpenRouter. A blank id means the server default (a Claude model)
/// and passes; a project that is not a client engagement is never refused.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_engagement_refuses_a_non_anthropic_model_on_request_model_steps() {
    let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-test-not-real");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("Client", vec![]).unwrap();
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Api)
        .unwrap();
    assert!(p.client_engagement, "a new project is a client engagement");
    // The chat backend must pass the lock first (see the cli chat test); then the model.
    state
        .settings
        .set_chat_backend(crate::project::ProjectBackend::Api);

    // The shared check.
    let why = state
        .client_lock_step_model_refusal("deepseek/deepseek-chat")
        .expect("an OpenRouter id is refused on a client engagement");
    assert!(why.contains("`deepseek/deepseek-chat`"), "{why}");
    assert_eq!(state.client_lock_step_model_refusal(""), None);
    assert_eq!(state.client_lock_step_model_refusal("  "), None);
    assert_eq!(
        state.client_lock_step_model_refusal("claude-sonnet-5"),
        None
    );

    // Each site.
    let app = router(state.clone());
    let esc = state.escalations.raise(
        crate::escalation::RaiseEscalationReq {
            subject_kind: crate::escalation::SubjectKind::Uow,
            checkpoint_id: None,
            routine_id: "routine-1".to_string(),
            reason: "r".to_string(),
            stopped_for: "s".to_string(),
            suggestions: vec![],
            raw_context: String::new(),
        },
        "Add login",
    );
    let post = |uri: String, body: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    let chat = post(
        "/api/chat".to_string(),
        // The chat names the project whose data it carries; that project's lock applies.
        Box::leak(
            format!(
                r#"{{"prompt":"hi","model":"deepseek/deepseek-chat","project_id":"{}"}}"#,
                p.id
            )
            .into_boxed_str(),
        ),
    )
    .await;
    assert_eq!(chat.status(), StatusCode::CONFLICT, "the chat");
    assert!(body_json(chat).await["error"]
        .as_str()
        .unwrap()
        .contains("`deepseek/deepseek-chat`"));
    let review = post(
        format!("/api/escalations/{}/chat", esc.id),
        r#"{"message":"hi","model":"qwen/qwen3-coder:free"}"#,
    )
    .await;
    assert_eq!(review.status(), StatusCode::CONFLICT, "the review chat");
    assert!(body_json(review).await["error"]
        .as_str()
        .unwrap()
        .contains("`qwen/qwen3-coder:free`"));
    // The draft has no error channel: a refused model gets the deterministic scaffold and no
    // model call is made.
    let draft = post(
        "/api/routines/draft-prompt".to_string(),
        r#"{"intent":"tidy the docs","scope":"read","model":"deepseek/deepseek-chat"}"#,
    )
    .await;
    assert_eq!(draft.status(), StatusCode::OK);
    assert_eq!(body_json(draft).await["authored_by"], "scaffold");

    // Not a client engagement: never refused.
    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    assert_eq!(
        state.client_lock_step_model_refusal("deepseek/deepseek-chat"),
        None
    );
}

/// H2 review: a scan with no active project is not waved through. There is no inspection
/// setup to say it is not client work, so it is a client engagement with no opt-in (the lock
/// is default-on), and it is refused before any repository is read.
#[tokio::test]
async fn a_scan_with_no_active_project_is_refused_as_a_client_engagement() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(state.projects.active().is_none());
    let why = client_lock_refusal_for_scan(
        None,
        None,
        None,
        false,
        &crate::cost_cap::RunControls::default(),
    )
    .expect("no project means a client engagement");
    assert!(why.contains("no active project"), "{why}");

    let req = || AuditReq {
        repos: vec!["acme/never-read".to_string()],
        ..serde_json::from_value(serde_json::json!({ "repos": [] })).unwrap()
    };
    let Json(report) = onboard_audit(State(state.clone()), Json(req())).await;
    assert!(report.blocked, "{:?}", report.message);
    assert!(report
        .message
        .as_deref()
        .unwrap()
        .contains("no active project"));
    assert_eq!(report.files_scanned, 0);

    let job_id = start_audit_job(&state, req(), AuditJobOptions::default()).await;
    let job = state.jobs.get(&job_id).expect("the job exists");
    assert_eq!(job.status, "failed", "{:?}", job.message);
    assert!(job
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("no active project"));
}

/// H2: a client engagement's audit on the cli backend, or with an OpenRouter audit model, is
/// refused before any repository is located or read (the repo here does not even exist, so
/// reaching the scan would produce a "not found" note, not this refusal); on the API with
/// Claude models the lock lets it through.
#[tokio::test]
async fn a_client_engagement_audit_is_refused_before_any_repository_is_read() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Client", vec!["acme/never-read".to_string()])
        .unwrap();
    // The scan model this test checks the lock against is the engine finder's; the agent finder
    // (a new project's default) does not use it.
    state
        .projects
        .set_review_settings(&p.id, camerata_api_types::project::ReviewSettings::legacy())
        .unwrap()
        .unwrap();
    let req = |model: Option<&str>| AuditReq {
        repos: vec!["acme/never-read".to_string()],
        model: model.map(str::to_string),
        ..serde_json::from_value(serde_json::json!({ "repos": [] })).unwrap()
    };

    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Cli)
        .unwrap();
    let Json(report) = onboard_audit(State(state.clone()), Json(req(None))).await;
    assert!(report.blocked, "{:?}", report.message);
    assert!(report.message.as_deref().unwrap().contains("`cli` backend"));
    assert_eq!(report.files_scanned, 0);

    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Api)
        .unwrap();
    let Json(report) = onboard_audit(
        State(state.clone()),
        Json(req(Some("deepseek/deepseek-chat"))),
    )
    .await;
    assert!(report.blocked);
    assert!(
        report
            .message
            .as_deref()
            .unwrap()
            .contains("`deepseek/deepseek-chat`"),
        "{:?}",
        report.message
    );

    // The async job path refuses the same way: the job fails at once with the reason.
    let job_id = start_audit_job(
        &state,
        req(Some("deepseek/deepseek-chat")),
        AuditJobOptions::default(),
    )
    .await;
    let job = state.jobs.get(&job_id).expect("the job exists");
    assert_eq!(job.status, "failed", "{:?}", job.message);
    assert!(
        job.message
            .as_deref()
            .unwrap_or_default()
            .contains("`deepseek/deepseek-chat`"),
        "{:?}",
        job.message
    );

    // API with Claude models: the lock passes.
    assert_eq!(
        client_lock_refusal_for_scan(
            state.projects.active().as_ref(),
            Some("claude-sonnet-5"),
            Some("claude-sonnet-5"),
            false,
            &crate::cost_cap::RunControls::default(),
        ),
        None
    );
}

/// The model steps of a project (decompose, clarify, authoring, escalation, diagrams, L3 ...)
/// run on THAT project's backend, the way the audit does: Cli by default, Api with the key
/// resolved now, and Api with no key REFUSED, never run on the subscription.
#[test]
#[serial_test::serial(process_env)]
fn step_llm_follows_the_active_projects_backend() {
    use crate::project::ProjectBackend;
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");

    // Cli project: a Cli Llm, no key needed.
    let state = state_with_active_project(ProjectBackend::Cli);
    assert_eq!(state.step_llm().unwrap().backend_label(), "anthropic/cli");

    // Api project, no key anywhere: refused (and the port form reports the same refusal).
    let state = state_with_active_project(ProjectBackend::Api);
    let refusal = state
        .step_llm()
        .expect_err("api with no key must be refused");
    assert!(refusal.contains("no API key"), "{refusal}");
    let port = state.step_llm_port();
    let err = futures::executor::block_on(port.complete(crate::llm::LlmRequest::new("x")))
        .expect_err("the refused port fails every call");
    assert_eq!(err.to_string(), refusal);

    // Key saved after startup: the next step Llm is Api and carries exactly that key.
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-step")
        .unwrap();
    let llm = state.step_llm().unwrap();
    assert_eq!(llm.backend_label(), "anthropic/api");
    assert_eq!(llm.api_key(), Some("sk-ant-step"));

    // Switching the PROJECT to Cli switches the step, whatever the chat setting says.
    let id = state.projects.active().unwrap().id;
    state
        .projects
        .set_backend(&id, ProjectBackend::Cli)
        .unwrap();
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    assert_eq!(state.step_llm().unwrap().backend_label(), "anthropic/cli");
}

/// With no project at all, the global chat backend setting decides (like the chat).
#[test]
#[serial_test::serial(process_env)]
fn step_llm_without_a_project_uses_the_chat_backend_setting() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(state.projects.active().is_none());
    assert_eq!(state.step_llm().unwrap().backend_label(), "anthropic/cli");
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    assert!(state.step_llm().is_err(), "api chat setting + no key");
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-nop")
        .unwrap();
    let llm = state.step_llm().unwrap();
    assert_eq!(llm.backend_label(), "anthropic/api");
    assert_eq!(llm.api_key(), Some("sk-ant-nop"));
}

/// Every step endpoint refuses (409, with the reason) when the active project is Api with no
/// key, instead of quietly running on the subscription CLI.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn every_project_step_endpoint_refuses_an_api_project_with_no_key() {
    use crate::project::ProjectBackend;
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = state_with_active_project(ProjectBackend::Api);
    let app = router(state.clone());
    // A UoW (blank) and an escalation to address; a story for the story-keyed steps.
    let blank = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/blank")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let uow_id = body_json(blank).await["uow_id"]
        .as_str()
        .unwrap()
        .to_string();
    let esc = state.escalations.raise(
        crate::escalation::RaiseEscalationReq {
            subject_kind: crate::escalation::SubjectKind::Uow,
            checkpoint_id: None,
            routine_id: uow_id.clone(),
            reason: "r".to_string(),
            stopped_for: "s".to_string(),
            suggestions: vec![],
            raw_context: String::new(),
        },
        "Add login",
    );
    let story = camerata_worktracker::InMemoryStoryStore::seeded()
        .list()
        .await
        .unwrap()
        .remove(0);
    let story_id = story.id.clone();
    state.stories.upsert(story).await.unwrap();

    let cases: Vec<(String, &str)> = vec![
        (format!("/api/stories/{story_id}/decompose"), "{}"),
        (format!("/api/stories/{story_id}/clarify/suggest"), "{}"),
        (format!("/api/uow/{uow_id}/author"), r#"{"message":"hi"}"#),
        (format!("/api/uow/{uow_id}/diagram"), "{}"),
        (
            format!("/api/escalations/{}/chat", esc.id),
            r#"{"message":"hi"}"#,
        ),
    ];
    for (uri, body) in cases {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
    }
}

/// `GET /api/models` reports the backend the chat will actually use (the chat setting and
/// the resolved key), not a hard-coded CLI label.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn models_endpoint_backend_label_follows_the_chat_setting() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let fetch = |state: AppState| async move {
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        body_json(resp).await
    };

    // Default Cli setting: says cli.
    let json = fetch(state.clone()).await;
    assert_eq!(json["backend"], "anthropic/cli");
    assert!(json["backend_error"].is_null());

    // Api setting with a key: says api.
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-label")
        .unwrap();
    let json = fetch(state.clone()).await;
    assert_eq!(json["backend"], "anthropic/api");
    assert!(json["backend_error"].is_null());

    // Api setting, key removed: still names the setting, and says why it cannot run.
    state
        .credential_store
        .delete(crate::credentials::ANTHROPIC_API_KEY)
        .unwrap();
    let json = fetch(state).await;
    assert_eq!(json["backend"], "anthropic/api");
    assert!(json["backend_error"]
        .as_str()
        .unwrap()
        .contains("no API key"));
}

/// The Chat backend setting must DO what the UI says: `api` builds an Api `Llm` carrying the
/// key resolved now (store first); `cli` builds a Cli `Llm`; `api` with no key refuses
/// instead of silently downgrading to the CLI.
#[test]
#[serial_test::serial(process_env)]
fn chat_llm_follows_the_chat_backend_setting_and_the_stored_key() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));

    // Default (Cli): a Cli Llm, no key needed.
    let llm = state.chat_llm().expect("cli chat needs no key");
    assert_eq!(llm.backend_label(), "anthropic/cli");

    // Api with no key anywhere: refused, not downgraded.
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Api);
    assert!(state.chat_llm().is_err());

    // Key saved after startup: the next chat Llm is Api and carries exactly that key.
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-chat")
        .unwrap();
    let llm = state.chat_llm().expect("api chat with a stored key");
    assert_eq!(llm.backend_label(), "anthropic/api");
    assert_eq!(llm.api_key(), Some("sk-ant-chat"));

    // Replaced key: the next one carries the new key.
    state
        .credential_store
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-chat-2")
        .unwrap();
    assert_eq!(state.chat_llm().unwrap().api_key(), Some("sk-ant-chat-2"));

    // Back to Cli: a Cli Llm again (the CLI transport carries the key but never uses it).
    state
        .settings
        .set_chat_backend(crate::llm::ProjectBackend::Cli);
    assert_eq!(state.chat_llm().unwrap().backend_label(), "anthropic/cli");
}

/// `AppState::github_token()` prefers the credential store over the env var.
#[test]
#[serial_test::serial(process_env)]
fn github_token_method_prefers_store_over_env() {
    // Set a value in the store.
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .credential_store
        .set(crate::credentials::GITHUB_TOKEN, "store_token")
        .expect("set must succeed");

    // Ensure the env var is NOT set so we are testing the store path.
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");

    let resolved = state.github_token();
    assert_eq!(
        resolved.as_deref(),
        Some("store_token"),
        "must return the store value when env var is absent"
    );
}

/// `AppState::github_token()` falls back to env var when store is empty.
#[test]
#[serial_test::serial(process_env)]
fn github_token_method_falls_back_to_env_when_store_empty() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    // Store is empty (MemoryCredentialStore starts empty).

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_GITHUB_TOKEN", "env_token");
    let resolved = state.github_token();

    assert_eq!(
        resolved.as_deref(),
        Some("env_token"),
        "must fall back to env var when store is empty"
    );
}

/// `anthropic_api_key_present` returns true when the keychain holds the key, even
/// with no `ANTHROPIC_API_KEY` env var set.
#[test]
fn anthropic_api_key_present_true_when_keychain_has_it() {
    let store = crate::credentials::MemoryCredentialStore::new();
    crate::credentials::CredentialStore::set(
        &store,
        crate::credentials::ANTHROPIC_API_KEY,
        "sk-ant-keychain",
    )
    .expect("set must succeed");
    // The env var is irrelevant here: the keychain path returns true first.
    assert!(anthropic_api_key_present(&store));
}

/// `anthropic_api_key_present` returns true when only the env var is set (empty
/// keychain), and false when neither source has a key.
#[test]
#[serial_test::serial(process_env)]
fn anthropic_api_key_present_reflects_env_when_keychain_empty() {
    let store = crate::credentials::MemoryCredentialStore::new();
    // Neither source set → false.
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    assert!(!anthropic_api_key_present(&store));
    // Env-only → true.
    let _env2 = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
    let present = anthropic_api_key_present(&store);
    assert!(present, "env var alone makes the key present");
}

/// `resolve_backend_for_project` — the per-seam glue
/// (`docs/design/2026-09-22_per-project-backend.md`) — must gather `anthropic_api_key_present`
/// and the PROJECT's OWN `backend` setting (never a global/env preference) into one
/// resolution. Exercises the "no project" floor plus both concrete `backend` values,
/// using the keychain path for the key (not the env var) so this test can't race the
/// env-var-based tests around it.
#[test]
#[serial_test::serial(process_env)]
fn resolve_backend_for_project_gathers_the_two_inputs_correctly() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));

    // No project at all, no key -> Cli, never Blocked. The project-less floor must be
    // exactly as permissive as a freshly-created project's default (backend=Cli), never
    // more restrictive. Defensively clears the env var too, so this assertion can't be
    // poisoned by another test's ANTHROPIC_API_KEY mutation running concurrently (the
    // known env-var race noted on
    // `credentials::tests::resolve_falls_back_to_env_when_store_empty`).
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    assert_eq!(
        resolve_backend_for_project(&state, None),
        crate::llm::BackendResolution::Cli
    );

    // A project explicitly switched to `Api`, no key anywhere -> Blocked.
    let api_project = state.projects.create("ApiOnly", vec![]).unwrap();
    assert_eq!(
        api_project.backend,
        crate::project::ProjectBackend::Api,
        "fresh project defaults to Api"
    );
    // A NEW project (default Api) with no key is blocked outright: no CLI fallback.
    assert!(matches!(
        resolve_backend_for_project(&state, Some(&api_project)),
        crate::llm::BackendResolution::Blocked { .. }
    ));
    let api_project = state
        .projects
        .set_backend(&api_project.id, crate::project::ProjectBackend::Api)
        .unwrap();
    assert_eq!(
        resolve_backend_for_project(&state, Some(&api_project)),
        crate::llm::BackendResolution::Blocked {
            message: "This project is set to the Anthropic API backend, but no API key is \
                          configured. Add an Anthropic API key (or switch the backend to Cli) to run."
                .to_string(),
        }
    );

    // A key in the credential store (keychain path) + backend=Api -> Api.
    crate::credentials::CredentialStore::set(
        state.credential_store.as_ref(),
        crate::credentials::ANTHROPIC_API_KEY,
        "sk-ant-test",
    )
    .expect("set must succeed");
    assert_eq!(
        resolve_backend_for_project(&state, Some(&api_project)),
        crate::llm::BackendResolution::Api
    );

    // backend=Cli (explicitly chosen) -> quiet Cli, regardless of the key already
    // present from the step above.
    let personal_project = state.projects.create("Personal", vec![]).unwrap();
    let personal_project = state
        .projects
        .set_backend(&personal_project.id, crate::project::ProjectBackend::Cli)
        .unwrap();
    assert_eq!(
        resolve_backend_for_project(&state, Some(&personal_project)),
        crate::llm::BackendResolution::Cli
    );
}

// ── Attachment tests ──────────────────────────────────────────────────────

/// The client lock checks the models the run will actually call: a client engagement on the API
/// with the agent finder, the duplicate check or the clone judge on a non-Anthropic model is
/// refused before anything is read, naming the role and the model. A run with all of them on
/// `claude-` ids is let through.
#[tokio::test]
async fn the_client_lock_checks_the_finder_duplicate_check_and_clone_judge_models() {
    use camerata_api_types::project::{ProjectFinder, ReviewSettings};
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Client", vec!["acme/never-read".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Api)
        .unwrap();
    let req = || AuditReq {
        repos: vec!["acme/never-read".to_string()],
        model: Some("claude-sonnet-5-5".to_string()),
        calibration_model: Some("claude-sonnet-5-5".to_string()),
        ..serde_json::from_value(serde_json::json!({ "repos": [] })).unwrap()
    };
    let set = |review: ReviewSettings| {
        state
            .projects
            .set_review_settings(&p.id, review)
            .unwrap()
            .unwrap();
    };
    let base = state.projects.get(&p.id).unwrap().review;
    let cases = [
        (
            "the finder",
            ReviewSettings {
                finder: ProjectFinder::Agent,
                finder_model: "other-vendor/x".to_string(),
                ..base.clone()
            },
        ),
        (
            "the duplicate check",
            ReviewSettings {
                dup_check_model: "other-vendor/x".to_string(),
                ..base.clone()
            },
        ),
        (
            "the clone judge",
            ReviewSettings {
                clone_judge_model: "other-vendor/x".to_string(),
                ..base.clone()
            },
        ),
    ];
    for (role, review) in cases {
        set(review);
        let job_id = start_audit_job(&state, req(), AuditJobOptions::default()).await;
        let job = state.jobs.get(&job_id).expect("the job exists");
        assert_eq!(job.status, "failed", "{role}: {:?}", job.message);
        let message = job.message.unwrap_or_default();
        assert!(
            message.contains(role) && message.contains("`other-vendor/x`"),
            "{role}: {message}"
        );
        assert!(message.contains("Nothing was read"), "{role}: {message}");
    }
    // Every model on a claude- id: the lock lets the run through.
    set(ReviewSettings {
        finder: ProjectFinder::Agent,
        finder_model: "claude-opus-5-5".to_string(),
        dup_check_model: "claude-sonnet-5-5".to_string(),
        clone_judge_model: "claude-haiku-5-5".to_string(),
        ..base
    });
    let project = state.projects.get(&p.id).unwrap();
    let controls = req().run_controls_for(&state);
    assert_eq!(
        client_lock_refusal_for_scan(
            Some(&project),
            Some("claude-sonnet-5-5"),
            Some("claude-sonnet-5-5"),
            false,
            &controls,
        ),
        None
    );
}

/// A job started for a NAMED project that is not the active one (the delta rescan does this) runs
/// on that project's own models, and the client lock checks that project's models: the active
/// project here is clean, the named one has its duplicate check on another vendor's model.
#[tokio::test]
async fn a_job_for_a_named_project_is_lock_checked_on_that_projects_models() {
    use camerata_api_types::project::ReviewSettings;
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let named = state
        .projects
        .create("Named", vec!["acme/never-read".to_string()])
        .unwrap();
    let active = state
        .projects
        .create("Active", vec!["acme/never-read".to_string()])
        .unwrap();
    for id in [&named.id, &active.id] {
        state
            .projects
            .set_backend(id, crate::project::ProjectBackend::Api)
            .unwrap();
    }
    state
        .projects
        .set_review_settings(
            &named.id,
            ReviewSettings {
                dup_check_model: "other-vendor/x".to_string(),
                ..state.projects.get(&named.id).unwrap().review
            },
        )
        .unwrap()
        .unwrap();
    state.projects.set_active(&active.id);
    assert_eq!(state.projects.active().unwrap().id, active.id);
    let req = AuditReq {
        repos: vec!["acme/never-read".to_string()],
        model: Some("claude-sonnet-5-5".to_string()),
        calibration_model: Some("claude-sonnet-5-5".to_string()),
        ..serde_json::from_value(serde_json::json!({ "repos": [] })).unwrap()
    };
    let job_id = start_audit_job(
        &state,
        req,
        AuditJobOptions {
            project_id: Some(named.id.clone()),
            ..AuditJobOptions::default()
        },
    )
    .await;
    let job = state.jobs.get(&job_id).expect("the job exists");
    assert_eq!(job.status, "failed", "{:?}", job.message);
    let message = job.message.unwrap_or_default();
    assert!(
        message.contains("the duplicate check") && message.contains("`other-vendor/x`"),
        "{message}"
    );
}
