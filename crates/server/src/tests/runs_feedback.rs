//! Run lifecycle, feedback, governance events, decisions, investigation and sign-off
//! handler tests.

use super::*;

#[tokio::test]
async fn health_returns_ok() {
    let app = router(AppState::seeded());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "ok");
}

/// `/api/usage` returns the cumulative ledger snapshot in the documented shape, and
/// reflects calls recorded on the shared ledger (proving the endpoint reads the same
/// state the chokepoint writes). Provider-agnostic: records an Anthropic-shaped (cost
/// present) and a Gemini-shaped (tokens only, derived cost) call.
#[tokio::test]
async fn usage_endpoint_shape_and_accumulation() {
    let state = AppState::seeded();
    // Anthropic-shaped: cost reported directly.
    state.usage_ledger.record(
        "claude-opus-5",
        &crate::llm::LlmResponse {
            reported_model: None,
            text: String::new(),
            model: "claude-opus-5".to_string(),
            backend: "cli".to_string(),
            cost_usd: Some(0.05),
            input_tokens: Some(1000),
            output_tokens: Some(500),
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 20,
            or_cache_discount: None,
            truncated: false,
        },
    );
    // Gemini-shaped: no cost field, known model id -> derived cost (sonnet 3/15 per Mtok).
    state.usage_ledger.record(
        "claude-sonnet-5",
        &crate::llm::LlmResponse {
            reported_model: None,
            text: String::new(),
            model: "claude-sonnet-5".to_string(),
            backend: "api".to_string(),
            cost_usd: None,
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        },
    );

    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    // Documented payload shape.
    assert_eq!(json["input_tokens"], 1_001_000);
    assert_eq!(json["output_tokens"], 1_000_500);
    assert_eq!(json["cache_read"], 10);
    assert_eq!(json["cache_creation"], 20);
    assert_eq!(json["calls"], 2);
    assert_eq!(json["rate_limited"], false);
    assert!(json["last_rate_limit"].is_null());
    // 0.05 (reported) + 12.0 (derived sonnet, $2/$10) = 12.05.
    let cost = json["total_cost_usd"].as_f64().unwrap();
    assert!((cost - 12.05).abs() < 1e-6, "got {cost}");
    // by_model array of {model,tokens,cost,calls}, sorted by descending cost (sonnet first).
    let by_model = json["by_model"].as_array().unwrap();
    assert_eq!(by_model.len(), 2);
    assert_eq!(by_model[0]["model"], "claude-sonnet-5");
    assert_eq!(by_model[0]["calls"], 1);
    assert_eq!(by_model[0]["tokens"], 2_000_000);
}

#[tokio::test]
async fn rules_endpoint_excludes_gov1_and_returns_real_rules() {
    let app = router(AppState::seeded());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/rules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let arr = json.as_array().unwrap();
    // The twelve substantive rules, GOV-1 (the synthetic test rule) filtered out.
    // Count updated when SEC-NO-GIT-STATE-MUTATION-1 was added (2026-06-27).
    assert_eq!(arr.len(), 12);
    let ids: Vec<&str> = arr.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"SEC-NO-HARDCODED-SECRETS-1"));
    assert!(ids.contains(&"SEC-NO-PATH-ESCAPE-1"));
    assert!(ids.contains(&"SEC-NO-SECRET-FILES-1"));
    assert!(ids.contains(&"SEC-NO-PRIVATE-KEY-1"));
    assert!(ids.contains(&"SEC-NO-VENDOR-TOKEN-1"));
    assert!(ids.contains(&"SEC-NO-SECRET-FILE-1"));
    assert!(ids.contains(&"SEC-NO-DISABLED-TLS-1"));
    assert!(ids.contains(&"SEC-NO-CAMERATA-CONFIG-1"));
    assert!(ids.contains(&"SEC-NO-UNSAFE-DESERIALIZATION-1"));
    assert!(ids.contains(&"SEC-NO-GIT-STATE-MUTATION-1"));
    assert!(!ids.contains(&"GOV-1"));
}

#[tokio::test]
async fn per_uow_path_decodes_slash_and_hash_in_story_id() {
    // GitHub UoW ids are `owner/repo#num` — the `/` and `#` must survive routing.
    // The UI percent-encodes the id into one path segment; axum's Path extractor
    // decodes it. Proves the dev-status (and every per-UoW path endpoint) works for
    // a GitHub-sourced id, not just a slash-free demo id.
    let state = AppState::new(std::sync::Arc::new(
        camerata_worktracker::InMemoryStoryStore::new(),
    ));
    let app = router(state.clone());
    let encoded = "owner%2Frepo%23123"; // owner/repo#123
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{encoded}/status"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"in_progress"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "encoded story id must route to the dev-status handler (not 404)"
    );
    // The status must have landed on the DECODED id, proving axum decoded %2F + %23.
    assert_eq!(
        state.uow.get_or_create("owner/repo#123").dev_status,
        crate::uow::DevStatus::InProgress
    );
}

#[tokio::test]
async fn three_phase_state_endpoints_persist_to_the_uow() {
    // The #105 cockpit-state endpoints (intake context/repos, per-phase chat, contract,
    // meta) round-trip onto the UoW so the refinement-session state survives sessions.
    let state = AppState::new(std::sync::Arc::new(
        camerata_worktracker::InMemoryStoryStore::new(),
    ));
    let app = router(state.clone());
    let sid = "S-105";

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

    // Intake context.
    let r = post(
        format!("/api/uow/{sid}/intake/context"),
        r#"{"context":"the next agent should know X"}"#,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    // Intake repo/branch scope (R6) — both branch-mode variants.
    let r = post(
        format!("/api/uow/{sid}/intake/repos"),
        r#"{"repos":[{"repo":"me/fe","branch":{"mode":"new_from_base","base":"main","new_name":""}},{"repo":"me/be","branch":{"mode":"existing","branch_name":"feature/x"}}]}"#,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    // Investigation + development chat turns.
    let r = post(
        format!("/api/uow/{sid}/investigation/chat"),
        r#"{"role":"user","text":"what about caching?"}"#,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = post(
        format!("/api/uow/{sid}/development/chat"),
        r#"{"role":"user","text":"there is a bug in the loop"}"#,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    // Contract (R3.g).
    let r = post(
        format!("/api/uow/{sid}/contract"),
        r#"{"contract":"GET /users returns [User]","crosses_boundary":true}"#,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    // Meta — patch only the viewed phase + intake_finished.
    let r = post(
        format!("/api/uow/{sid}/meta"),
        r#"{"viewed_phase":"development","intake_finished":true}"#,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    // Everything landed on the stored UoW.
    let uow = state.uow.get_or_create(sid);
    assert_eq!(uow.intake.context, "the next agent should know X");
    assert_eq!(uow.intake.repos.len(), 2);
    assert_eq!(uow.intake.repos[0].repo, "me/fe");
    assert!(matches!(
        uow.intake.repos[1].branch,
        crate::uow::BranchMode::Existing { ref branch_name } if branch_name == "feature/x"
    ));
    assert_eq!(uow.investigation.chat.len(), 1);
    assert_eq!(uow.investigation.contract, "GET /users returns [User]");
    assert!(uow.investigation.crosses_boundary);
    assert_eq!(uow.development.chat.len(), 1);
    assert_eq!(uow.meta.viewed_phase, crate::uow::PhaseTab::Development);
    assert!(uow.meta.intake_finished);
    assert!(
        !uow.meta.investigation_finished,
        "unpatched field unchanged"
    );
}

#[tokio::test]
async fn stories_endpoint_returns_the_active_projects_spine() {
    // ISOLATION (A8): /api/stories is project-scoped. A story is in-scope when one of its
    // build-target repos (or its source container) is in the active project's repos.
    // Stories targeting another repo must NOT leak.
    use camerata_worktracker::{FeatureStatus, RepoTarget};

    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // Active project owns `acme/app`.
    state
        .projects
        .create("Acme", vec!["acme/app".to_string()])
        .unwrap();

    // Two in-scope stories (target acme/app) and one out-of-scope (targets other/repo).
    let mk = |id: &str, repo: &str, status: FeatureStatus| CanonicalStory {
        id: id.to_string(),
        external_ref: None,
        title: format!("story {id}"),
        description: "desc".to_string(),
        status,
        created_by: "architect".to_string(),
        targets: vec![RepoTarget::new(repo)],
    };
    state
        .stories
        .upsert(mk("CAM-1", "acme/app", FeatureStatus::Executing))
        .await
        .unwrap();
    state
        .stories
        .upsert(mk("CAM-2", "acme/app", FeatureStatus::SignedOff))
        .await
        .unwrap();
    state
        .stories
        .upsert(mk("OTHER-1", "other/repo", FeatureStatus::Executing))
        .await
        .unwrap();

    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/stories")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let arr = json.as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "only the active project's stories, got: {json:?}"
    );
    let ids: Vec<&str> = arr.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"CAM-1") && ids.contains(&"CAM-2"));
    assert!(
        !ids.contains(&"OTHER-1"),
        "out-of-project story must not leak, got: {ids:?}"
    );
}

/// #20: POST /api/stories/adopt-issue maps a GitHub issue onto the spine (token-free,
/// fields travel in the request) and persists it in the in-memory StoryStore.
#[tokio::test]
async fn adopt_issue_persists_a_canonical_story_in_the_store() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let stories = state.stories.clone();
    let app = router(state);

    let body = serde_json::json!({
        "repo": "zernst3/camerata-orchestrator",
        "number": 20,
        "title": "Story intake from GitHub Issues",
        "body": "Adopt a repo's issues into the spine.",
    })
    .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/stories/adopt-issue")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["id"], "zernst3/camerata-orchestrator#20");
    assert_eq!(json["title"], "Story intake from GitHub Issues");
    assert_eq!(json["status"], "intake");
    assert_eq!(json["external_ref"]["provider"], "github");
    assert_eq!(json["external_ref"]["external_id"], "20");
    assert_eq!(
        json["external_ref"]["container"],
        "zernst3/camerata-orchestrator"
    );

    // The story is actually on the spine now (adopt persisted it).
    let spine = stories.list().await.unwrap();
    assert_eq!(spine.len(), 1);
    assert_eq!(spine[0].id, "zernst3/camerata-orchestrator#20");
}

/// #20: a malformed repo (not `owner/name`) is rejected, not silently adopted.
///
/// ROUTES-7: invalid REQUEST INPUT is a 400 Bad Request, not a 500. (Previously every
/// AppError mapped to 500; this asserts the corrected status for the bad-input class.)
#[tokio::test]
async fn adopt_issue_rejects_a_malformed_repo() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let body = serde_json::json!({ "repo": "not-a-repo", "number": 1, "title": "x", "body": "" })
        .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/stories/adopt-issue")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // The body shape is unchanged: UI parses `{error}`.
    let json = body_json(resp).await;
    assert!(json["error"].is_string(), "body keeps the {{error}} shape");
}

/// ROUTES-7: a GET for a missing run returns 404 Not Found (not 500), with the
/// unchanged `{error}` body shape the UI parses.
#[tokio::test]
async fn get_run_missing_returns_404_not_500() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runs/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let json = body_json(resp).await;
    assert!(json["error"].is_string(), "body keeps the {{error}} shape");
}

/// Phase H3: `GET /api/runs/:id/events` returns an empty array (200), never an
/// error, when no governance log is open this session — the fail-soft contract
/// (see `AppState::record_governance`) must extend to the read path.
#[tokio::test]
async fn get_run_events_returns_empty_array_when_no_governance_log() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runs/some-run/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json, serde_json::json!([]));
}

/// Phase H3: `GET /api/governance/events` also returns an empty array (200) with no
/// governance log open, same fail-soft contract as the per-run route.
#[tokio::test]
async fn recent_governance_events_returns_empty_array_when_no_governance_log() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/governance/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json, serde_json::json!([]));
}

/// Phase H3: with a real (in-memory) governance log attached, `GET
/// /api/runs/:id/events` returns exactly that run's recorded events, in insertion
/// order, and `GET /api/governance/events?limit=N` returns the N most recent across
/// all runs, newest first — the read path over the Phase H1/H2 write path.
#[tokio::test]
async fn governance_event_routes_read_back_recorded_events() {
    let log = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");
    log.record(camerata_persistence::GovernanceEvent::info(
        "run-a",
        "run_started",
        "system",
    ))
    .await
    .expect("record run-a event");
    log.record(
        camerata_persistence::GovernanceEvent::error("run-b", "gate_deny", "agent")
            .with_rule_id("SEC-1")
            .with_reason("denied"),
    )
    .await
    .expect("record run-b event");

    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.governance_log = Some(std::sync::Arc::new(log));
    let app = router(state);

    // Per-run: only run-a's event comes back.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runs/run-a/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["run_id"], "run-a");
    assert_eq!(json[0]["kind"], "run_started");

    // Cross-run recent: both events come back, newest (run-b) first.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/governance/events?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 2);
    assert_eq!(json[0]["run_id"], "run-b");
    assert_eq!(json[0]["rule_id"], "SEC-1");
    assert_eq!(json[1]["run_id"], "run-a");
}

// ── Product-Owner feedback loop: defect-report ingest + read + status update ───

/// Fail-soft: `GET /api/projects/:id/feedback` returns an empty array (200), never
/// an error, when no feedback store is open this session.
#[tokio::test]
async fn project_feedback_returns_empty_array_when_no_feedback_store() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-1/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json, serde_json::json!([]));
}

/// Fail-soft: `GET /api/feedback/recent` also returns an empty array (200) with no
/// feedback store open, same contract as the per-project route.
#[tokio::test]
async fn recent_feedback_returns_empty_array_when_no_feedback_store() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/feedback/recent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json, serde_json::json!([]));
}

/// Fail-soft: `POST /api/feedback` responds `{ ok: false, message }` (200, never a
/// 500) when no feedback store is open this session.
#[tokio::test]
async fn submit_feedback_reports_ok_false_when_no_feedback_store() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "project_id": "proj-1",
                        "title": "it broke",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    assert!(json["message"].is_string());
}

/// FOLD E (chat secret-interceptor, applied at ingest): a report whose
/// `context.console`/`context.stack` contains a fake Stripe live key must come
/// back scrubbed — the raw secret must never reach the feedback store OR the
/// paired governance-audit row's `detail` JSON. Exercises the real
/// `submit_feedback` handler end to end (not just `camerata_orchestrator_core::
/// secrets::scrub` in isolation, which already has its own unit tests).
#[tokio::test]
async fn submit_feedback_scrubs_a_secret_in_console_and_stack_before_storing() {
    let feedback = camerata_persistence::FeedbackStore::open_in_memory()
        .await
        .expect("open in-memory feedback store");
    let governance = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");

    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.feedback_store = Some(std::sync::Arc::new(feedback));
    state.governance_log = Some(std::sync::Arc::new(governance));
    let app = router(state);

    let leaked_key = "sk_live_51H8xyzABCDEFGHIJKLMNOP";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "project_id": "proj-secret",
                        "source": "auto",
                        "kind": "runtime_error",
                        "title": "Failed request: /api/charge (500)",
                        "context": {
                            "route": "/checkout",
                            "console": format!("Stripe init failed with key {leaked_key}"),
                            "stack": format!("Error: bad key {leaked_key}\n    at charge (app.js:1:1)"),
                        },
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let submit_json = body_json(resp).await;
    assert_eq!(submit_json["ok"], true);
    let id = submit_json["id"].as_i64().expect("id must be an integer");

    // The stored report (read back via the list route) never carries the raw key.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-secret/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["id"], id);
    let console = json[0]["context"]["console"].as_str().unwrap();
    let stack = json[0]["context"]["stack"].as_str().unwrap();
    assert!(
        !console.contains(leaked_key),
        "console still carries the raw key: {console}"
    );
    assert!(
        !stack.contains(leaked_key),
        "stack still carries the raw key: {stack}"
    );
    assert!(console.contains("{{SECRET:stripe_key_1}}"));
    assert!(stack.contains("{{SECRET:stripe_key_1}}"));
    // Untouched context fields (route) still come through unscrubbed.
    assert_eq!(json[0]["context"]["route"], "/checkout");

    // The paired governance-audit row's `detail` (the full stamped report as
    // JSON) is likewise scrubbed — the raw key must not land in the audit trail
    // either.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runs/proj-secret/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 1);
    let detail = json[0]["detail"].as_str().expect("detail must be a string");
    assert!(
        !detail.contains(leaked_key),
        "governance detail still carries the raw key: {detail}"
    );
    assert!(detail.contains("{{SECRET:stripe_key_1}}"));
}

/// The full ingest round trip with a REAL (in-memory) feedback store attached:
/// `POST /api/feedback` stores the report AND writes a paired `defect_reported`
/// governance row, `GET /api/projects/:id/feedback` lists it back, `GET
/// /api/feedback/recent` surfaces it cross-project, and `POST
/// /api/feedback/:id/status` updates its status.
#[tokio::test]
async fn feedback_routes_submit_store_and_list_with_paired_governance_row() {
    let feedback = camerata_persistence::FeedbackStore::open_in_memory()
        .await
        .expect("open in-memory feedback store");
    let governance = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");

    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.feedback_store = Some(std::sync::Arc::new(feedback));
    state.governance_log = Some(std::sync::Arc::new(governance));
    let app = router(state);

    // Submit an auto-captured report. Client-supplied id/status are ignored (the
    // server stamps its own); severity "critical" -> governance "error" severity;
    // source "auto" -> governance actor "system".
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "id": 999,
                        "project_id": "proj-a",
                        "source": "auto",
                        "kind": "runtime_error",
                        "title": "TypeError: x is undefined",
                        "description": "thrown in render()",
                        "severity": "critical",
                        "status": "resolved",
                        "context": { "route": "/dashboard", "stack": "at foo (app.js:1:1)" },
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let submit_json = body_json(resp).await;
    assert_eq!(submit_json["ok"], true);
    let id = submit_json["id"].as_i64().expect("id must be an integer");

    // A second report for a different project, so cross-project `recent` has
    // something to distinguish from the per-project list.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "project_id": "proj-b",
                        "source": "user",
                        "kind": "user_report",
                        "title": "button does nothing",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["ok"], true);

    // Per-project list: only proj-a's report, server-stamped id/status (NOT the
    // client-supplied 999 / "resolved").
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-a/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["id"], id);
    assert_eq!(json[0]["title"], "TypeError: x is undefined");
    assert_eq!(
        json[0]["status"], "open",
        "server stamps status Open regardless of client input"
    );
    assert_eq!(json[0]["context"]["route"], "/dashboard");

    // Cross-project recent: both reports, newest (proj-b) first.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/feedback/recent?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 2);
    assert_eq!(json[0]["project_id"], "proj-b");
    assert_eq!(json[1]["project_id"], "proj-a");

    // The paired governance row landed: kind defect_reported, actor system
    // (source=auto), severity error (from "critical"), reason = title, run_id =
    // project_id, detail decodes back to the stamped report.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runs/proj-a/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["kind"], "defect_reported");
    assert_eq!(json[0]["actor"], "system");
    assert_eq!(json[0]["severity"], "error");
    assert_eq!(json[0]["reason"], "TypeError: x is undefined");
    let detail: serde_json::Value =
        serde_json::from_str(json[0]["detail"].as_str().expect("detail must be a string"))
            .expect("detail must decode as JSON");
    assert_eq!(detail["id"], id);
    assert_eq!(detail["status"], "open");

    // Status update: Open -> Acknowledged.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/feedback/{id}/status"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "status": "acknowledged" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["ok"], true);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-a/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json[0]["status"], "acknowledged");
}

// ── fingerprint + dedupe fold (PART C) ──────────────────────────────────

/// Two reports that share the SAME logical error (same kind, same top stack
/// frame, same route) fold into ONE row with `count` incremented, and only the
/// FIRST occurrence writes a `defect_reported` governance row — the second is a
/// silent dedupe, not a second audit-trail entry.
#[tokio::test]
async fn submit_feedback_dedupes_same_fingerprint_reports_by_incrementing_count() {
    let feedback = camerata_persistence::FeedbackStore::open_in_memory()
        .await
        .expect("open in-memory feedback store");
    let governance = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");

    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.feedback_store = Some(std::sync::Arc::new(feedback));
    state.governance_log = Some(std::sync::Arc::new(governance));
    let app = router(state);

    let body = serde_json::json!({
        "project_id": "proj-storm",
        "source": "auto",
        "kind": "runtime_error",
        "title": "TypeError: x is undefined",
        "context": { "route": "/dashboard", "stack": "at render (app.js:42:7)\nat foo (app.js:1:1)" },
    })
    .to_string();

    // First occurrence: inserted fresh.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback")
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let first_json = body_json(resp).await;
    assert_eq!(first_json["ok"], true);
    assert!(
        first_json.get("deduped").is_none(),
        "first occurrence must not be flagged as deduped"
    );
    let id = first_json["id"].as_i64().expect("id must be an integer");

    // Second occurrence: SAME kind/stack-top-frame/route (the render loop repeats
    // the error). Must fold into the SAME row, not insert a second one.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let second_json = body_json(resp).await;
    assert_eq!(second_json["ok"], true);
    assert_eq!(second_json["id"], id, "dedupe returns the SAME row id");
    assert_eq!(second_json["deduped"], true);

    // Exactly one row for this project, with count folded to 2.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-storm/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    let reports = json.as_array().unwrap();
    assert_eq!(
        reports.len(),
        1,
        "the second occurrence must NOT insert a new row"
    );
    assert_eq!(reports[0]["id"], id);
    assert_eq!(reports[0]["count"], 2);

    // Exactly ONE governance row — the duplicate did not write a second one.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runs/proj-storm/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    let events = json.as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "only the FIRST occurrence writes a defect_reported governance row"
    );
    assert_eq!(events[0]["kind"], "defect_reported");
}

/// Reports with DIFFERENT fingerprints (different route) do NOT fold together —
/// each gets its own row.
#[tokio::test]
async fn submit_feedback_does_not_dedupe_across_different_routes() {
    let feedback = camerata_persistence::FeedbackStore::open_in_memory()
        .await
        .expect("open in-memory feedback store");
    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.feedback_store = Some(std::sync::Arc::new(feedback));
    let app = router(state);

    for route in ["/dashboard", "/settings"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/feedback")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "project_id": "proj-multi",
                            "source": "auto",
                            "kind": "runtime_error",
                            "title": "TypeError: x is undefined",
                            "context": { "route": route },
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["ok"], true);
    }

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-multi/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        json.as_array().unwrap().len(),
        2,
        "different routes must not fold into one row"
    );
}

/// A client-supplied `fingerprint` is honored as-is (the server does not
/// recompute it), so a client with its OWN dedupe key still folds correctly.
#[tokio::test]
async fn submit_feedback_honors_client_provided_fingerprint() {
    let feedback = camerata_persistence::FeedbackStore::open_in_memory()
        .await
        .expect("open in-memory feedback store");
    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.feedback_store = Some(std::sync::Arc::new(feedback));
    let app = router(state);

    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/feedback")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "project_id": "proj-client-fp",
                            "source": "auto",
                            "kind": "runtime_error",
                            "title": "boom",
                            "fingerprint": "client-computed-fp",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["ok"], true);
    }

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects/proj-client-fp/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    let reports = json.as_array().unwrap();
    assert_eq!(
        reports.len(),
        1,
        "client-provided fingerprint must dedupe too"
    );
    assert_eq!(reports[0]["fingerprint"], "client-computed-fp");
    assert_eq!(reports[0]["count"], 2);
}

// ── record_orchestrator_decision: the confidence engine's recording substrate ──

/// `record_orchestrator_decision` records the decision AND a paired
/// `orchestrator_decision` governance row, readable back via the existing
/// `/api/runs/:id/events` endpoint. Severity is "info" for a routine class.
#[tokio::test]
async fn record_orchestrator_decision_stores_decision_and_paired_governance_row() {
    let decisions = camerata_persistence::OrchestratorDecisionLog::open_in_memory()
        .await
        .expect("open in-memory decision store");
    let governance = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");

    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.orchestrator_decisions = Some(std::sync::Arc::new(decisions));
    state.governance_log = Some(std::sync::Arc::new(governance));

    let decision = camerata_persistence::OrchestratorDecision::new(
        "run-1",
        "mechanically_verified",
        "high",
        "picked the vetted default",
    )
    .with_alternatives(vec!["ask the human".to_string()])
    .with_assumption("assumed the default rule option applies");

    let id = state
        .record_orchestrator_decision(decision)
        .await
        .expect("decision store is open, so an id must come back");
    assert!(id > 0);

    let events = state
        .governance_log
        .as_ref()
        .unwrap()
        .by_run("run-1")
        .await
        .expect("by_run");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "orchestrator_decision");
    assert_eq!(events[0].severity, "info");
    assert_eq!(events[0].actor, "system");
    assert!(events[0]
        .reason
        .as_deref()
        .unwrap()
        .contains("picked the vetted default"));

    let calibration = state
        .orchestrator_decisions
        .as_ref()
        .unwrap()
        .calibration()
        .await
        .expect("calibration");
    assert_eq!(calibration.len(), 1);
    assert_eq!(calibration[0].class, "mechanically_verified");
    assert_eq!(calibration[0].total, 1);
}

/// An Irreversible-class decision is notable (Class C is supposed to always be
/// human-gated before it executes), so its paired governance row is "warn", not
/// "info" — the one severity-mapping branch that differs from the routine case.
#[tokio::test]
async fn record_orchestrator_decision_uses_warn_severity_for_irreversible_class() {
    let decisions = camerata_persistence::OrchestratorDecisionLog::open_in_memory()
        .await
        .expect("open in-memory decision store");
    let governance = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");

    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.orchestrator_decisions = Some(std::sync::Arc::new(decisions));
    state.governance_log = Some(std::sync::Arc::new(governance));

    let decision =
        camerata_persistence::OrchestratorDecision::new("run-2", "irreversible", "low", "z");
    state
        .record_orchestrator_decision(decision)
        .await
        .expect("decision store is open");

    let events = state
        .governance_log
        .as_ref()
        .unwrap()
        .by_run("run-2")
        .await
        .expect("by_run");
    assert_eq!(events[0].severity, "warn");
}

/// Fail-soft: `None` when no decision store is open this session (mirrors
/// `record_defect`'s contract) — no governance row is written either.
#[tokio::test]
async fn record_orchestrator_decision_returns_none_when_no_decision_store() {
    let governance = camerata_persistence::GovernanceLog::open_in_memory()
        .await
        .expect("open in-memory governance log");
    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.governance_log = Some(std::sync::Arc::new(governance));

    let decision =
        camerata_persistence::OrchestratorDecision::new("run-3", "preview_reversible", "high", "x");
    let result = state.record_orchestrator_decision(decision).await;
    assert!(result.is_none());

    let events = state
        .governance_log
        .as_ref()
        .unwrap()
        .by_run("run-3")
        .await
        .expect("by_run");
    assert!(events.is_empty());
}

/// Fail-soft: `POST /api/feedback/:id/status` responds `{ ok: false, message }`
/// (200, never a 500) when no feedback store is open this session.
#[tokio::test]
async fn update_feedback_status_reports_ok_false_when_no_feedback_store() {
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/feedback/1/status")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "status": "resolved" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    assert!(json["message"].is_string());
}

/// ROUTES-7: the AppError constructors carry the right status while keeping the
/// `{error}` body — the single source of truth all handlers route through.
#[tokio::test]
async fn app_error_status_codes_map_correctly() {
    use axum::response::IntoResponse;
    let cases = [
        (
            AppError(anyhow::anyhow!("boom")),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            AppError::not_found(anyhow::anyhow!("gone")),
            StatusCode::NOT_FOUND,
        ),
        (
            AppError::bad_request(anyhow::anyhow!("nope")),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (err, expected) in cases {
        let resp = err.into_response();
        assert_eq!(resp.status(), expected);
    }
}

/// #20: with no GitHub token the list endpoint degrades gracefully — `ok:false`,
/// an empty list, and a hint — instead of erroring or panicking.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn github_issues_list_is_token_optional_and_never_panics() {
    // Ensure no token is visible to this test process.
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/github/issues?repo=zernst3/camerata-orchestrator")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
    assert_eq!(json["issues"].as_array().unwrap().len(), 0);
    assert!(json["message"].is_string());
}

// ── Pillar 2: the no-code-first gate wired into run start ────────────────────

fn approved_decision_json(story: &str) -> serde_json::Value {
    use camerata_worktracker::investigation::DecisionRecord;
    let d = DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/a"),
        "Decision",
        "Question?",
        "Rationale",
        vec![],
        chrono::Utc::now(),
    )
    .approve(chrono::Utc::now());
    serde_json::to_value(vec![d]).unwrap()
}

#[tokio::test]
async fn start_run_is_blocked_until_decisions_are_approved() {
    let state = AppState::seeded();
    let story = "GATE-1";
    let app = router(state.clone());

    // No decisions recorded → the run is blocked with a 409 carrying the reason.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let json = body_json(resp).await;
    assert!(json["reason"].as_str().unwrap().contains("No decisions"));

    // The UoW stage did NOT advance (still Intake — no code was let through).
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Intake
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn start_run_proceeds_once_decisions_are_approved() {
    let state = AppState::seeded();
    let story = "GATE-2";

    // Record an approved decision via the decisions endpoint.
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/decisions"))
                .header("content-type", "application/json")
                .body(Body::from(approved_decision_json(story).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Now the run starts (scripted path, token-free).
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["run_id"].is_string());

    // The gate side-effect drove the lifecycle stage forward to Development.
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Development
    );
}

/// HONESTY (FIX #1): without a live agent, the production run button must REFUSE — the run
/// lands `Failed`, the UoW stage does NOT advance to AwaitingQa, and NO QA evidence is
/// attached. A user must never be able to click "Run" and watch a fabricated dev cycle
/// complete with real-looking compliance evidence while zero code was written.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn start_run_without_live_fails_and_does_not_advance_or_attach() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_LIVE_BUILD"); // ensure live mode is OFF
    let state = AppState::seeded();
    let story = "HONEST-1";

    // Approve a decision so the no-code-first gate is satisfied and the run can start.
    let app = router(state.clone());
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/uow/{story}/decisions"))
            .header("content-type", "application/json")
            .body(Body::from(approved_decision_json(story).to_string()))
            .unwrap(),
    )
    .await
    .unwrap();

    // Start the run (scripted / token-free path).
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let run_id = json["run_id"].as_str().unwrap().to_string();

    // The run FAILS immediately (refusal is synchronous) — never a fabricated success.
    let run = state.runs.get(&run_id).expect("run exists");
    assert!(
        matches!(run.status, crate::run::RunStatus::Failed { .. }),
        "a token-free production run must FAIL, not fabricate a completed cycle"
    );
    assert!(run.done);

    // Give the provenance watcher a moment; it must freeze provenance but NEVER advance the
    // stage to AwaitingQa or attach QA evidence.
    for _ in 0..50 {
        if state.uow.get_or_create(story).gate_provenance.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let uow = state.uow.get_or_create(story);
    assert_ne!(
        uow.stage,
        lifecycle::UowStage::AwaitingQa,
        "a refused run must NOT advance the UoW to AwaitingQa"
    );
    assert!(
        uow.evidence.is_none(),
        "a refused run must NOT attach fabricated QA evidence"
    );
}

// ── LIFECYCLE-9: single-flight guard per story ───────────────────────────────

/// A second start on a story that already has an active (non-done) run returns 409,
/// naming the active run — two runs must never share one worktree.
#[tokio::test]
async fn lifecycle9_second_start_with_active_run_returns_409() {
    let state = AppState::seeded();
    let story = "SF-1";
    // Approve a decision so the development gate would otherwise pass.
    seed_approved_decision(&state, story);
    // Seed an ACTIVE (Planned, not done) run on the story — simulating a run already
    // in flight.
    let active = state
        .runs
        .create(story, "scripted", crate::run::RunKind::Watched);

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a second start with an active run must be rejected with 409"
    );
    let json = body_json(resp).await;
    assert_eq!(json["active_run_id"], active);
    assert!(json["reason"].as_str().unwrap().contains("Only one"));
}

/// Once the active run is done, a fresh start proceeds (the guard only blocks LIVE runs).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn lifecycle9_start_proceeds_once_prior_run_is_done() {
    let state = AppState::seeded();
    let story = "SF-2";
    seed_approved_decision(&state, story);
    // A prior run that has completed (done) must NOT block a new start.
    let prior = state
        .runs
        .create(story, "scripted", crate::run::RunKind::Watched);
    state.runs.mark_done(&prior);

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a done prior run must not block a new start"
    );
}

/// Sign-off on a story whose run is still LIVE (not done) must NOT tear down the
/// worktree — it records a deferral note instead. Once the run is done, sign-off proceeds
/// without the deferral note.
#[tokio::test]
async fn lifecycle9_signoff_defers_teardown_while_run_is_live() {
    let state = AppState::seeded();
    let story = "SF-3";
    // A UoW branch so the teardown path would otherwise be reached.
    state
        .uow
        .set_branch(story, Some("camerata/sf-3".to_string()));
    // A LIVE run (Planned, not done).
    let run_id = state
        .runs
        .create(story, "live", crate::run::RunKind::Watched);

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/sign-off"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "sign-off itself still succeeds"
    );

    // A deferral history entry was recorded (teardown withheld while live).
    let uow = state.uow.get_or_create(story);
    assert!(
        uow.history
            .iter()
            .any(|h| h.kind == "sign_off" && h.text.contains("teardown deferred")),
        "a live run must defer worktree teardown and record why"
    );
}

/// When the run is done, sign-off does NOT record the deferral note (teardown proceeds).
#[tokio::test]
async fn lifecycle9_signoff_tears_down_when_run_is_done() {
    let state = AppState::seeded();
    let story = "SF-4";
    state
        .uow
        .set_branch(story, Some("camerata/sf-4".to_string()));
    let run_id = state
        .runs
        .create(story, "live", crate::run::RunKind::Watched);
    state
        .runs
        .set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/sign-off"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let uow = state.uow.get_or_create(story);
    assert!(
        !uow.history
            .iter()
            .any(|h| h.kind == "sign_off" && h.text.contains("teardown deferred")),
        "a done run must not record a teardown-deferral note"
    );
}

/// `active_run_for_story` returns the non-done run and ignores done runs.
#[test]
fn lifecycle9_active_run_for_story_ignores_done_runs() {
    let runs = crate::run::RunStore::new();
    let done = runs.create("S-A", "scripted", crate::run::RunKind::Watched);
    runs.mark_done(&done);
    assert!(
        runs.active_run_for_story("S-A").is_none(),
        "a story with only done runs has no active run"
    );
    let live = runs.create("S-A", "scripted", crate::run::RunKind::Watched);
    assert_eq!(
        runs.active_run_for_story("S-A").map(|r| r.id),
        Some(live),
        "the live run is surfaced as the active run"
    );
    // A different story is unaffected.
    assert!(runs.active_run_for_story("S-B").is_none());
}

// ── LIFECYCLE-12: reject-after-bounce reverts committed snapshots ─────────────

/// Helper: run a git command in `dir`, returning trimmed stdout.
async fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// E2E (real git repo): after per-iteration snapshot COMMITS, a Reject resets the worktree
/// HARD to the checkpoint's base_commit — HEAD returns to base_commit and the snapshot
/// commits are gone. This is the LIFECYCLE-12 fix: without the reset, those committed
/// snapshots would survive a Reject and could be pushed.
#[tokio::test]
async fn lifecycle12_reject_hard_resets_to_base_commit() {
    let dir = std::env::temp_dir().join(format!("cam-l12-reject-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git_in(&dir, &["init"]).await;
    git_in(&dir, &["config", "user.email", "t@example.com"]).await;
    git_in(&dir, &["config", "user.name", "Test"]).await;
    std::fs::write(dir.join("README.md"), "base").unwrap();
    git_in(&dir, &["add", "."]).await;
    git_in(&dir, &["commit", "-m", "base"]).await;
    let base_commit = git_in(&dir, &["rev-parse", "HEAD"]).await;

    // The agent commits two per-iteration `camerata: snapshot` commits.
    std::fs::write(dir.join("work.rs"), "fn a() {}").unwrap();
    git_in(&dir, &["add", "."]).await;
    git_in(&dir, &["commit", "-m", "camerata: snapshot iteration 0"]).await;
    std::fs::write(dir.join("work.rs"), "fn a() {} fn b() {}").unwrap();
    git_in(&dir, &["add", "."]).await;
    git_in(&dir, &["commit", "-m", "camerata: snapshot iteration 1"]).await;
    // Plus an uncommitted change on top.
    std::fs::write(dir.join("untracked.rs"), "fn c() {}").unwrap();

    // HEAD has moved past base and the snapshots exist.
    assert_ne!(git_in(&dir, &["rev-parse", "HEAD"]).await, base_commit);

    // Reject: revert with the base commit → hard reset + clean.
    revert_worktree(&dir.to_string_lossy(), Some(&base_commit)).await;

    // HEAD is back at base_commit; the snapshot commits are gone.
    assert_eq!(
        git_in(&dir, &["rev-parse", "HEAD"]).await,
        base_commit,
        "HEAD must be reset back to the checkpoint base_commit"
    );
    let log = git_in(&dir, &["log", "--oneline"]).await;
    assert!(
        !log.contains("snapshot"),
        "the per-iteration snapshot commits must be gone after Reject: {log}"
    );
    // The agent's committed file and untracked file are both gone.
    assert!(
        !dir.join("work.rs").exists(),
        "committed work file must be reverted"
    );
    assert!(
        !dir.join("untracked.rs").exists(),
        "untracked file must be cleaned"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// H2 review: the dev loop's git calls go through `workspace::git_command`, which switches off
/// `core.fsmonitor`, so a repository whose own git configuration names a program never gets it
/// run by the revert (`reset --hard`, `clean`). Needs a git that honours `core.fsmonitor`: the
/// control step at the end prints SKIPPED when it does not, and the assertion above it then
/// proves nothing on that machine.
#[cfg(unix)]
#[tokio::test]
async fn the_dev_loop_revert_never_runs_the_repositorys_fsmonitor_command() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    let base_commit = crate::test_git::init_repo(&dir, &[("a.txt", "one\n")]);
    std::fs::write(dir.join("b.txt"), "two\n").unwrap();
    crate::test_git::commit_all(&dir, "camerata: snapshot iteration 0");
    std::fs::write(dir.join("untracked.txt"), "x\n").unwrap();
    let marker = tmp.path().join("fsmonitor-ran");
    let hook = tmp.path().join("hook.sh");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::test_git::git(
        &dir,
        &["config", "core.fsmonitor", &hook.display().to_string()],
    );

    revert_worktree(&dir.to_string_lossy(), Some(&base_commit)).await;
    assert!(
        !marker.exists(),
        "the repository's fsmonitor command ran during the revert"
    );
    assert!(!dir.join("untracked.txt").exists(), "the revert still ran");

    // Control: plain `git status` in the same repository does run it.
    let _ = std::process::Command::new("git")
        .current_dir(&dir)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    if !marker.exists() {
        eprintln!("SKIPPED control: this git does not run core.fsmonitor hooks");
    }
}

/// Without a base_commit (older checkpoints), revert_worktree falls back to the
/// uncommitted-only revert: committed work survives, only uncommitted changes are dropped.
#[tokio::test]
async fn lifecycle12_revert_without_base_only_drops_uncommitted() {
    let dir = std::env::temp_dir().join(format!("cam-l12-nobase-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git_in(&dir, &["init"]).await;
    git_in(&dir, &["config", "user.email", "t@example.com"]).await;
    git_in(&dir, &["config", "user.name", "Test"]).await;
    std::fs::write(dir.join("README.md"), "base").unwrap();
    git_in(&dir, &["add", "."]).await;
    git_in(&dir, &["commit", "-m", "base"]).await;
    std::fs::write(dir.join("committed.rs"), "fn a() {}").unwrap();
    git_in(&dir, &["add", "."]).await;
    git_in(&dir, &["commit", "-m", "camerata: snapshot"]).await;
    let head_with_commit = git_in(&dir, &["rev-parse", "HEAD"]).await;
    // Uncommitted edit on top.
    std::fs::write(dir.join("dirty.rs"), "fn d() {}").unwrap();

    revert_worktree(&dir.to_string_lossy(), None).await;

    // The committed snapshot SURVIVES (no base to reset to); only uncommitted is dropped.
    assert_eq!(
        git_in(&dir, &["rev-parse", "HEAD"]).await,
        head_with_commit,
        "without a base_commit, committed work is not reset (documented fallback)"
    );
    assert!(
        dir.join("committed.rs").exists(),
        "committed file survives the fallback"
    );
    assert!(
        !dir.join("dirty.rs").exists(),
        "uncommitted file is cleaned"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Contract precondition (R3.g) ─────────────────────────────────────────────
// Spec: when a story's UoW has crosses_boundary=true and an empty contract, the
// development gate MUST refuse even when all decisions are approved.

/// Helper: record an approved decision on a story via the UoW store directly.
fn seed_approved_decision(state: &AppState, story: &str) {
    use camerata_worktracker::investigation::DecisionRecord;
    let d = DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/r3g"),
        "R3g decision",
        "Q?",
        "R",
        vec![],
        chrono::Utc::now(),
    )
    .approve(chrono::Utc::now());
    state.uow.set_decisions(story, vec![d]);
}

/// When crosses_boundary=true and no contract exists, the gate refuses even when
/// all decisions are approved.
#[test]
fn contract_precondition_blocks_when_crosses_boundary_and_empty_contract() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let story = "R3G-1";
    // Seed an approved decision so the decisions check passes.
    seed_approved_decision(&state, story);
    // Set crosses_boundary=true with an EMPTY contract.
    state.uow.set_contract(story, "", true);
    let result = ensure_development_gate(&state, story);
    assert!(
        result.is_err(),
        "gate must refuse when crosses_boundary=true and contract is empty"
    );
    let reason = result.unwrap_err();
    assert!(
        reason.contains("crosses a contract boundary"),
        "error must mention the contract boundary: {reason}"
    );
    assert!(
        reason.contains("R3.g"),
        "error must reference R3.g: {reason}"
    );
}

/// When crosses_boundary=true but a non-empty contract exists, the gate permits
/// development (decisions check passes + contract is present).
#[test]
fn contract_precondition_permits_when_crosses_boundary_and_contract_present() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let story = "R3G-2";
    seed_approved_decision(&state, story);
    state
        .uow
        .set_contract(story, "GET /api/users returns [{id, name}]", true);
    let result = ensure_development_gate(&state, story);
    assert!(
        result.is_ok(),
        "gate must allow when crosses_boundary=true AND contract is present: {result:?}"
    );
}

/// When crosses_boundary=false, the contract check is skipped entirely — no contract
/// is required for single-repo / non-boundary work.
#[test]
fn contract_precondition_skipped_when_not_crosses_boundary() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let story = "R3G-3";
    seed_approved_decision(&state, story);
    // crosses_boundary = false, contract is empty — this must be allowed.
    state.uow.set_contract(story, "", false);
    let result = ensure_development_gate(&state, story);
    assert!(
        result.is_ok(),
        "gate must allow when crosses_boundary=false even with empty contract: {result:?}"
    );
}

/// Whitespace-only contract is treated the same as empty (R3.g).
#[test]
fn contract_precondition_blocks_whitespace_contract() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let story = "R3G-4";
    seed_approved_decision(&state, story);
    state.uow.set_contract(story, "   \n\t  ", true);
    let result = ensure_development_gate(&state, story);
    assert!(
        result.is_err(),
        "whitespace-only contract is treated as empty — gate must refuse"
    );
}

#[tokio::test]
async fn approve_decisions_endpoint_409s_when_gate_unsatisfied() {
    let state = AppState::seeded();
    let story = "GATE-3";

    // Move to Investigating first.
    state.uow.begin_investigation(story).unwrap();

    // approve-decisions with no decisions on the UoW → 409 with a precise reason.
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/approve-decisions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let json = body_json(resp).await;
    assert!(json["reason"].is_string());
    // Stage unchanged.
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Investigating
    );
}

// ── Decisions-review surface (GET investigation + mark-reviewed) ──────────────

/// A seeded AppState with an in-memory artifact store attached, so investigation
/// notes (which are store-backed only) round-trip in tests.
async fn seeded_with_artifacts() -> AppState {
    let mut state = AppState::seeded();
    let store = camerata_persistence::SqliteStore::open("sqlite::memory:")
        .await
        .expect("in-memory artifact store");
    let store: std::sync::Arc<dyn camerata_persistence::ArtifactStore> = std::sync::Arc::new(store);
    state.uow = state.uow.with_artifacts(store);
    state
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn get_investigation_reports_note_present_and_decisions() {
    let state = seeded_with_artifacts().await;
    let story = "REVIEW-1";

    // No note yet, no decisions → note_present=false, empty decisions.
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{story}/investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["note_present"], false);
    assert!(json["note"].is_null());
    assert_eq!(json["decisions"].as_array().unwrap().len(), 0);

    // Record a real (non-placeholder) note + one approved decision.
    let note = camerata_worktracker::investigation::InvestigationArtifact::ai_authored(
        story,
        "Real analysis: the export must exclude archived members.",
        chrono::Utc::now(),
    );
    state.uow.set_investigation_note(&note);
    let app2 = router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/decisions"))
                .header("content-type", "application/json")
                .body(Body::from(approved_decision_json(story).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);

    // Now GET returns the note + decision.
    let app3 = router(state.clone());
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{story}/investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json3 = body_json(resp3).await;
    assert_eq!(json3["note_present"], true);
    assert!(json3["note"]["note"]
        .as_str()
        .unwrap()
        .contains("archived members"));
    assert_eq!(json3["note"]["reviewed"], false);
    assert_eq!(json3["decisions"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_investigation_reviewed_flips_the_flag() {
    let state = seeded_with_artifacts().await;
    let story = "REVIEW-2";
    let note = camerata_worktracker::investigation::InvestigationArtifact::ai_authored(
        story,
        "Some analysis.",
        chrono::Utc::now(),
    );
    state.uow.set_investigation_note(&note);

    // Mark reviewed → ok=true.
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/investigation/review"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);

    // The persisted note is now reviewed.
    let stored = state.uow.investigation_note_for(story).unwrap();
    assert!(stored.reviewed);

    // A second review is idempotent (already reviewed) → still ok=true, no new
    // revision, and flagged already_reviewed so the caller can tell it apart.
    let app2 = router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/investigation/review"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let json2 = body_json(resp2).await;
    assert_eq!(json2["ok"], true);
    assert_eq!(json2["already_reviewed"], true);

    // Reviewing a story that has no investigation note at all → 404, ok=false.
    let app3 = router(state.clone());
    let resp3 = app3
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/no-such-story/investigation/review")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status(), StatusCode::NOT_FOUND);
    let json3 = body_json(resp3).await;
    assert_eq!(json3["ok"], false);
}

// ── UoW Increment 1: tiered dev run + model-aware investigation ───────────────

#[test]
fn start_run_req_parses_tier_map_when_present() {
    // The frozen contract: { "model": <string|null>, "tier_map": {...} | null }.
    let body = serde_json::json!({
        "model": null,
        "tier_map": { "strongest": "opus-x", "balanced": "sonnet-x", "fast": "haiku-x" }
    });
    let req: StartRunReq = serde_json::from_value(body).unwrap();
    assert!(req.model.is_none());
    let map = req.tier_map.expect("tier_map parsed");
    assert_eq!(map.strongest, "opus-x");
    assert_eq!(map.balanced_primary(), "sonnet-x");
    assert_eq!(map.fast_primary(), "haiku-x");
}

#[test]
fn start_run_req_tier_map_absent_is_back_compat_single_model() {
    // No tier_map, just a model → single-model path (back-compat).
    let req: StartRunReq =
        serde_json::from_value(serde_json::json!({ "model": "claude-opus-5" })).unwrap();
    assert_eq!(req.model.as_deref(), Some("claude-opus-5"));
    assert!(req.tier_map.is_none());

    // Entirely empty body also parses (no-body callers stay compatible).
    let empty: StartRunReq = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(empty.model.is_none());
    assert!(empty.tier_map.is_none());
}

/// `skip_layer2` (the bootstrap escape hatch) parses when present and defaults to
/// absent (None) → off, so existing callers are unchanged.
#[test]
fn start_run_req_parses_skip_layer2_and_defaults_off() {
    // Present + true.
    let req: StartRunReq =
        serde_json::from_value(serde_json::json!({ "skip_layer2": true })).unwrap();
    assert_eq!(req.skip_layer2, Some(true));

    // Present + false.
    let req: StartRunReq =
        serde_json::from_value(serde_json::json!({ "skip_layer2": false })).unwrap();
    assert_eq!(req.skip_layer2, Some(false));

    // Absent → None (the default-off bootstrap behaviour; today's bodies unchanged).
    let req: StartRunReq = serde_json::from_value(serde_json::json!({
        "tier_map": { "strongest": "s", "balanced": "b", "fast": "f" }
    }))
    .unwrap();
    assert!(req.skip_layer2.is_none());

    let empty: StartRunReq = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(empty.skip_layer2.is_none());
}

/// The dev run selects the TIERED path when a map is present and the single-model
/// path otherwise. Asserted on the scripted (token-free) path: both start a run and
/// return the frozen `{run_id, story_id, mode}` shape, with the gate enforced
/// identically. (The live tiered vs. single-model branch is exercised by the
/// live_fleet functions; here we prove the request contract + gate are honored.)
#[tokio::test]
async fn dev_run_accepts_tier_map_and_still_enforces_the_gate() {
    let state = AppState::seeded();
    let story = "TIER-GATE-1";

    // With a tier_map but NO approved decisions, the gate still blocks (409).
    let app = router(state.clone());
    let body = serde_json::json!({
        "tier_map": { "strongest": "s", "balanced": "b", "fast": "f" }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "the gate is universal: a tier_map does not bypass it"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn dev_run_tiered_path_starts_once_decisions_are_approved() {
    let state = AppState::seeded();
    let story = "TIER-GATE-2";

    // Approve a decision so the gate is satisfied.
    let app = router(state.clone());
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/uow/{story}/decisions"))
            .header("content-type", "application/json")
            .body(Body::from(approved_decision_json(story).to_string()))
            .unwrap(),
    )
    .await
    .unwrap();

    // A tier_map run now starts and returns the frozen response shape.
    let app = router(state.clone());
    let body = serde_json::json!({
        "tier_map": { "strongest": "s", "balanced": "b", "fast": "f" }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/stories/{story}/run"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["run_id"].is_string());
    assert_eq!(json["story_id"], story);
    assert!(json["mode"].is_string());
}

/// begin-investigation is model-aware, returns a run id, and transitions the stage
/// Intake → Investigating.
#[tokio::test]
async fn begin_investigation_is_model_aware_returns_run_id_and_transitions_stage() {
    let state = AppState::seeded();
    let story = "INV-1";
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Intake
    );

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/begin-investigation"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "model": "claude-opus-5" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["run_id"].is_string(), "returns a pollable run id");
    assert_eq!(json["story_id"], story);

    // The stage advanced Intake → Investigating.
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Investigating
    );
}

#[tokio::test]
async fn begin_investigation_accepts_absent_model_body() {
    // No body → defaults to the project's strongest tier; still returns a run id
    // and transitions the stage.
    let state = AppState::seeded();
    let story = "INV-2";
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/begin-investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["run_id"].is_string());
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Investigating
    );
}

#[tokio::test]
async fn begin_investigation_409s_when_not_at_intake() {
    // Already past Intake → the transition is illegal, so no run is started.
    let state = AppState::seeded();
    let story = "INV-3";
    state.uow.begin_investigation(story).unwrap(); // now Investigating

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/begin-investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ── BUG B: rerun-investigation (re-run without repeating the Intake transition) ──

/// `rerun-investigation` refuses honestly (409, naming live mode) when live mode is
/// off — the default in tests/CI. Re-running with live mode off would just record the
/// same "live mode is off" placeholder note again, which isn't useful, so this must be
/// a real refusal, not a silent no-op success.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn rerun_investigation_refuses_honestly_when_live_mode_off() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_LIVE_BUILD");
    let state = AppState::seeded();
    let story = "INV-RERUN-1";
    state.uow.begin_investigation(story).unwrap(); // now Investigating

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/rerun-investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let json = body_json(resp).await;
    assert!(
        json["reason"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("live agent"),
        "must name the live agent as the blocker: {json}"
    );

    // A refused re-run must not silently transition anything.
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Investigating
    );
}

/// `rerun-investigation` 409s when the story is still at Intake — investigation has not
/// started, so `begin-investigation` is the right endpoint there, not a re-run.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn rerun_investigation_409s_when_still_at_intake() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
    let state = AppState::seeded();
    let story = "INV-RERUN-2";
    state.uow.get_or_create(story); // stays at Intake

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/rerun-investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let json = body_json(resp).await;
    assert!(json["reason"]
        .as_str()
        .unwrap_or_default()
        .contains("Begin investigation"));
}

/// The core BUG B fix: a story ALREADY past Intake (Investigating) can be re-run
/// WITHOUT a 409 — `begin-investigation` would 409 here (the stage already moved past
/// Intake on the first begin); `rerun-investigation` spawns a fresh pass instead,
/// WITHOUT repeating that transition.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn rerun_investigation_spawns_run_for_investigating_story_without_409() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
    let state = AppState::seeded();
    let story = "INV-RERUN-3";
    state.uow.begin_investigation(story).unwrap(); // now Investigating

    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{story}/rerun-investigation"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "must NOT 409 for an already-Investigating story"
    );
    let json = body_json(resp).await;
    assert!(
        json["run_id"].as_str().is_some_and(|s| !s.is_empty()),
        "returns a pollable run id"
    );
    assert_eq!(json["story_id"], story);

    // The stage is STILL Investigating IMMEDIATELY after the response — the endpoint
    // itself must not repeat the Intake → Investigating transition (that repetition is
    // exactly what would have 409'd on a second `begin-investigation` call). Asserted
    // synchronously, without waiting for the spawned background agent task to finish —
    // this environment happens to have a real `camerata-gateway` binary built, so
    // letting that task run to completion would make a real (slow, token-spending)
    // `claude` call; the test ending here drops the runtime and the task with it.
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Investigating,
        "re-run must not repeat the Intake -> Investigating transition"
    );
}

// ── Evidence assembly (issue #53) ─────────────────────────────────────────

/// Build a synthetic run with `n_denies` deny events and `n_allows` allow events,
/// store it in the given `RunStore`, and return the run id.
fn make_run(store: &RunStore, story: &str, n_allows: usize, n_denies: usize) -> String {
    let id = store.create(story, "scripted", crate::run::RunKind::Watched);
    for i in 0..n_denies {
        store.push_event(
            &id,
            crate::run::GateEvent {
                seq: i + 1,
                layer: "layer-1".to_string(),
                verdict: "deny".to_string(),
                rule: Some(format!("TEST-RULE-{i}")),
                detail: format!("test deny target {i}"),
                content_hash: None,
            },
        );
    }
    for i in 0..n_allows {
        store.push_event(
            &id,
            crate::run::GateEvent {
                seq: n_denies + i + 1,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: format!("src/clean_{i}.rs"),
                content_hash: None,
            },
        );
    }
    store.set_status(&id, crate::run::RunStatus::AwaitingQa, true);
    id
}

// ── LIFECYCLE-2/3/4: completion-driven, success-gated provenance stamping ─────────

/// Drive a fresh in-memory UoW for `story` to the `Development` stage (the stage a
/// governed dev run stamps from). Seeds one APPROVED decision so the no-code-first
/// transitions succeed: Intake → Investigating → DecisionsApproved → Development.
fn uow_at_development(uow: &crate::uow::UowStore, story: &str) {
    use camerata_worktracker::investigation::{
        DecisionOutcome, DecisionRecord, RevisionActor, RevisionProvenance,
    };
    let decision = DecisionRecord {
        artifact_id: format!("{story}/decision/d1"),
        story_id: story.to_string(),
        label: "d1".to_string(),
        question: "Q?".to_string(),
        rationale: "R".to_string(),
        alternatives_considered: vec![],
        outcome: DecisionOutcome::Approved,
        provenance: RevisionProvenance::new(RevisionActor::Ai, chrono::Utc::now()),
        option_descriptions: vec![],
    };
    uow.set_decisions(story, vec![decision]);
    uow.begin_investigation(story).expect("→ Investigating");
    uow.approve_decisions(story).expect("→ DecisionsApproved");
    uow.start_development(story).expect("→ Development");
    assert_eq!(
        uow.get_or_create(story).stage,
        lifecycle::UowStage::Development
    );
}

/// LIFECYCLE-2 (success): a SUCCESSFUL run advances Development → AwaitingQa, freezes
/// gate provenance, AND attaches QA evidence. Driven through the SAME completion-driven
/// stamping path a real run uses (`stamp_provenance_when_done`), not a hand-stamp.
#[tokio::test]
async fn stamp_on_success_advances_stage_and_attaches_evidence() {
    let runs = RunStore::new();
    let uow = crate::uow::UowStore::new();
    let story = "LIFE-SUCCESS";
    uow_at_development(&uow, story);

    // A completed (AwaitingQa) run with real gate events (1 allow, 2 deny).
    let run_id = make_run(&runs, story, 1, 2);

    stamp_provenance_when_done(
        runs.clone(),
        uow.clone(),
        crate::enforcement_ledger::EnforcementLedger::none(),
        None, // governance_log — not exercised in this unit test
        run_id.clone(),
        story.to_string(),
    )
    .await;

    let stamped = uow.get_or_create(story);
    // Stage advanced.
    assert_eq!(stamped.stage, lifecycle::UowStage::AwaitingQa);
    // Provenance frozen with the honest tallies.
    let prov = stamped
        .gate_provenance
        .expect("provenance frozen on success");
    assert_eq!(prov.deny_count, 2);
    assert_eq!(prov.allow_count, 1);
    // QA evidence attached.
    assert!(
        stamped.evidence.is_some(),
        "QA evidence attached on success"
    );
}

/// LIFECYCLE-2 (failure): a FAILED run (e.g. gateway binary missing) does NOT advance
/// to AwaitingQa and attaches NO QA evidence, but its gate provenance IS still frozen
/// (an honest record of what the gate saw). This is the core fix: the old code advanced
/// the stage + attached evidence for work that never completed.
#[tokio::test]
async fn stamp_on_failure_freezes_provenance_but_does_not_advance_or_attach() {
    let runs = RunStore::new();
    let uow = crate::uow::UowStore::new();
    let story = "LIFE-FAIL";
    uow_at_development(&uow, story);

    // A run with gate events that then FAILS (genuine Failed terminal).
    let run_id = make_run(&runs, story, 1, 2);
    // make_run leaves it AwaitingQa; flip to a fresh run that fails instead so the
    // terminal status is Failed (the terminal guard blocks re-setting a done run).
    let run_id = {
        let id = runs.create(story, "live", crate::run::RunKind::Watched);
        for ev in runs.get(&run_id).unwrap().events {
            runs.push_event(&id, ev);
        }
        runs.fail_with_reason(&id, "gateway binary missing".to_string());
        id
    };

    stamp_provenance_when_done(
        runs.clone(),
        uow.clone(),
        crate::enforcement_ledger::EnforcementLedger::none(),
        None, // governance_log — not exercised in this unit test
        run_id.clone(),
        story.to_string(),
    )
    .await;

    let stamped = uow.get_or_create(story);
    // Stage did NOT advance — still Development.
    assert_eq!(stamped.stage, lifecycle::UowStage::Development);
    // Provenance IS frozen (honest record of what the gate saw).
    assert!(
        stamped.gate_provenance.is_some(),
        "provenance frozen even on failure"
    );
    // NO QA evidence for work that never completed.
    assert!(
        stamped.evidence.is_none(),
        "no QA evidence attached on failure"
    );
}

/// LIFECYCLE-2 (cancel): a CANCELLED run freezes nothing, advances nothing, attaches
/// nothing — the run stopped before any completed work, so there is no honest gate
/// accounting to record.
#[tokio::test]
async fn stamp_on_cancel_does_nothing() {
    let runs = RunStore::new();
    let uow = crate::uow::UowStore::new();
    let story = "LIFE-CANCEL";
    uow_at_development(&uow, story);

    let run_id = runs.create(story, "live", crate::run::RunKind::Watched);
    runs.cancel(&run_id);

    stamp_provenance_when_done(
        runs.clone(),
        uow.clone(),
        crate::enforcement_ledger::EnforcementLedger::none(),
        None, // governance_log — not exercised in this unit test
        run_id.clone(),
        story.to_string(),
    )
    .await;

    let stamped = uow.get_or_create(story);
    assert_eq!(stamped.stage, lifecycle::UowStage::Development);
    assert!(
        stamped.gate_provenance.is_none(),
        "cancel freezes no provenance"
    );
    assert!(stamped.evidence.is_none(), "cancel attaches no evidence");
}

/// LIFECYCLE-3: the completion path stamps correctly for a run that reaches its terminal
/// state LATER (would exceed the old 5-minute poll budget). We drive the completion
/// signal directly — a background task flips the run terminal after the watcher is
/// already awaiting — proving the stamp is completion-driven, not wall-clock-driven.
#[tokio::test]
async fn stamp_is_completion_driven_not_polled() {
    let runs = RunStore::new();
    let uow = crate::uow::UowStore::new();
    let story = "LIFE-LATE";
    uow_at_development(&uow, story);

    // Create a run that is NOT yet done and give it gate events.
    let run_id = runs.create(story, "live", crate::run::RunKind::Watched);
    runs.push_event(
        &run_id,
        crate::run::GateEvent {
            seq: 1,
            layer: "layer-1".to_string(),
            verdict: "allow".to_string(),
            rule: None,
            detail: "src/late.rs".to_string(),
            content_hash: None,
        },
    );

    // Start the watcher; it parks on the completion signal (run is not done yet).
    let watch = {
        let runs = runs.clone();
        let uow = uow.clone();
        let rid = run_id.clone();
        let sid = story.to_string();
        tokio::spawn(async move {
            stamp_provenance_when_done(
                runs,
                uow,
                crate::enforcement_ledger::EnforcementLedger::none(),
                None, // governance_log — not exercised in this unit test
                rid,
                sid,
            )
            .await;
        })
    };

    // Complete the run AFTER the watcher is already awaiting — the completion signal (not
    // a poll) wakes it. A short sleep makes the ordering deterministic.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    runs.set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

    watch.await.expect("watcher task completes");

    let stamped = uow.get_or_create(story);
    assert_eq!(stamped.stage, lifecycle::UowStage::AwaitingQa);
    assert!(stamped.gate_provenance.is_some());
    assert!(stamped.evidence.is_some());
}

/// LIFECYCLE-4 (honesty): a RESUMED governed run gets the SAME completion-driven provenance
/// watcher a fresh run does — proving the resume path wires the watcher (previously it spawned
/// none). Exercised through the real `resume_governed_run` WITHOUT a live agent (CI default):
/// the resume now REFUSES rather than fabricating a completed cycle, so the run lands `Failed`;
/// the watcher must still freeze provenance but must NOT advance the UoW stage. The
/// success-branch of the watcher is covered separately by
/// `stamp_on_success_advances_stage_and_attaches_evidence`.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn resumed_run_without_live_refuses_and_freezes_provenance_without_advancing() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let story = "me/api#7";
    uow_at_development(&state.uow, story);

    // Seed a resumable checkpoint (as a test-tamper pause would).
    let esc = state.escalations.raise(
        crate::escalation::RaiseEscalationReq {
            subject_kind: crate::escalation::SubjectKind::Uow,
            checkpoint_id: None,
            routine_id: story.to_string(),
            reason: "AGENTIC-NO-TEST-TAMPER-1".to_string(),
            stopped_for: "review".to_string(),
            suggestions: vec![],
            raw_context: String::new(),
        },
        "story",
    );
    let ckpt = state.checkpoints.create(crate::checkpoint::NewCheckpoint {
        story_id: story.to_string(),
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

    let (run_id, _mode) = resume_governed_run(&state, &ckpt, "Approved: proceed.")
        .await
        .expect("resume call returns Ok even when it refuses to continue");

    // The resumed run REFUSES without a live agent: it is terminal and Failed (never a
    // fabricated success).
    let stamped = state.runs.get(&run_id).expect("resumed run exists");
    assert!(stamped.done, "refused resume run is terminal");
    assert!(
        matches!(stamped.status, crate::run::RunStatus::Failed { .. }),
        "a resume with no live agent must FAIL, not fabricate a completed cycle"
    );

    // Poll briefly for the watcher's provenance freeze (it runs on its own task).
    let mut frozen = false;
    for _ in 0..50 {
        if state.uow.get_or_create(story).gate_provenance.is_some() {
            frozen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        frozen,
        "LIFECYCLE-4: resumed run's watcher froze provenance"
    );
    // Honesty: the stage did NOT advance — the work never continued.
    assert_eq!(
        state.uow.get_or_create(story).stage,
        lifecycle::UowStage::Development,
        "a refused resume must not advance the UoW stage"
    );
    assert!(
        state.uow.get_or_create(story).evidence.is_none(),
        "a refused resume must not attach QA evidence"
    );
}

#[test]
fn assemble_evidence_for_run_builds_valid_record() {
    let run_store = RunStore::new();
    let run_id = make_run(&run_store, "CAM-ev-1", 1, 2);
    let run = run_store.get(&run_id).unwrap();
    let rules = camerata_gateway::enforced_gate_rules();
    let prov = run_provenance(&run, &rules);

    let record = assemble_evidence_for_run(&run, &prov, "CAM-ev-1");

    // Story and run ids are correct.
    assert_eq!(record.story_id, "CAM-ev-1");
    assert_eq!(record.run_id, run_id);

    // History: at least one "run" event and one per gate event.
    assert!(
        record.history.iter().any(|e| e.kind == "run"),
        "evidence must have a 'run' event"
    );
    assert!(
        record.history.iter().any(|e| e.kind == "gate_deny"),
        "evidence must have gate_deny events"
    );
    assert!(
        record.history.iter().any(|e| e.kind == "gate_allow"),
        "evidence must have gate_allow events"
    );

    // Gate decisions recorded.
    let allows: usize = record
        .gate_decisions
        .iter()
        .filter(|d| d.verdict == "allow")
        .count();
    let denies: usize = record
        .gate_decisions
        .iter()
        .filter(|d| d.verdict == "deny")
        .count();
    assert_eq!(allows, 1, "one allow gate decision");
    assert_eq!(denies, 2, "two deny gate decisions");

    // Rules enforced from the enforced set.
    assert!(
        !record.rules_enforced.is_empty(),
        "rules_enforced must be populated"
    );

    // Scoped scan populated.
    assert!(
        record.scoped_scan.is_some(),
        "scoped_scan must be populated"
    );

    // Content hash is set.
    assert!(
        !record.content_hash.is_empty(),
        "content_hash must be computed"
    );
    assert!(record.verify_hash(), "hash must verify after assembly");
}

#[test]
fn assemble_evidence_clean_run_is_not_blocked() {
    let run_store = RunStore::new();
    let run_id = make_run(&run_store, "CAM-ev-2", 3, 0);
    let run = run_store.get(&run_id).unwrap();
    let rules = camerata_gateway::enforced_gate_rules();
    let prov = run_provenance(&run, &rules);

    let record = assemble_evidence_for_run(&run, &prov, "CAM-ev-2");

    // A run with no real-file writes (fictional clean paths) must not trigger
    // the critical-finding blocker.
    assert!(
        !record.is_sign_off_blocked(),
        "clean scripted run must not block sign-off"
    );
}

#[test]
fn assemble_evidence_info_events_recorded_as_notes() {
    let run_store = RunStore::new();
    let id = run_store.create("CAM-ev-3", "live", crate::run::RunKind::Watched);
    run_store.push_event(
        &id,
        crate::run::GateEvent {
            seq: 1,
            layer: "fleet".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: "Scaffolding the worktree.".to_string(),
            content_hash: None,
        },
    );
    run_store.set_status(&id, crate::run::RunStatus::AwaitingQa, true);
    let run = run_store.get(&id).unwrap();
    let rules = camerata_gateway::enforced_gate_rules();
    let prov = run_provenance(&run, &rules);

    let record = assemble_evidence_for_run(&run, &prov, "CAM-ev-3");
    // Info events are recorded as "note" in the history.
    assert!(
        record.history.iter().any(|e| e.kind == "note"),
        "info fleet events must be recorded as 'note'"
    );
}

// ── Sign-off gate: critical-finding block (issue #53) ─────────────────────

#[tokio::test]
async fn sign_off_blocked_by_critical_finding_without_waiver() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // Seed a run at AwaitingQa.
    let run_id = state
        .runs
        .create("S-gate-1", "scripted", crate::run::RunKind::Watched);
    state
        .runs
        .set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

    // Attach an evidence record with a critical finding.
    let mut ev =
        crate::evidence::UowEvidenceRecord::new("S-gate-1", &run_id, "2026-06-20T00:00:00Z");
    ev.set_scoped_scan(crate::evidence::ScopedScanSummary {
        files_scanned: 1,
        total_findings: 1,
        has_critical: true,
        findings: Vec::new(),
    });
    ev.compute_hash();
    state.uow.attach_evidence("S-gate-1", ev);

    // Sign-off WITHOUT a waive_reason must be rejected with 409.
    let app = router(state);
    let body = serde_json::json!({ "by": "zach" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/sign-off"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "critical finding without waiver must return 409"
    );
    let json = body_json(resp).await;
    assert_eq!(json["blocked"], true);
}

#[tokio::test]
async fn sign_off_unblocked_by_waive_with_reason() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let run_id = state
        .runs
        .create("S-gate-2", "scripted", crate::run::RunKind::Watched);
    state
        .runs
        .set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

    // Attach critical evidence.
    let mut ev =
        crate::evidence::UowEvidenceRecord::new("S-gate-2", &run_id, "2026-06-20T00:00:00Z");
    ev.set_scoped_scan(crate::evidence::ScopedScanSummary {
        files_scanned: 1,
        total_findings: 1,
        has_critical: true,
        findings: Vec::new(),
    });
    ev.compute_hash();
    state.uow.attach_evidence("S-gate-2", ev);

    // Sign-off WITH a waive_reason must succeed (200) and record the waiver in the note.
    let app = router(state);
    let body = serde_json::json!({
        "by": "zach",
        "waive_reason": "Accepting pre-existing tech debt; tracked in issue #99.",
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/sign-off"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "waived critical finding must return 200"
    );
    let json = body_json(resp).await;
    // The sign-off is recorded on the UoW.
    assert!(
        json["sign_off"].is_object(),
        "sign_off field must be present"
    );
    // The waiver reason is folded into the note.
    let note = json["sign_off"]["note"].as_str().unwrap_or("");
    assert!(
        note.contains("WAIVER"),
        "waiver reason must appear in the sign-off note"
    );
}

#[tokio::test]
async fn sign_off_not_blocked_when_no_evidence_attached() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let run_id = state
        .runs
        .create("S-gate-3", "scripted", crate::run::RunKind::Watched);
    state
        .runs
        .set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);
    // No evidence attached: sign-off must succeed without a waiver.

    let app = router(state);
    let body = serde_json::json!({ "by": "zach" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/sign-off"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "no evidence: sign-off must not be blocked"
    );
}

#[tokio::test]
async fn sign_off_non_critical_evidence_not_blocked() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let run_id = state
        .runs
        .create("S-gate-4", "scripted", crate::run::RunKind::Watched);
    state
        .runs
        .set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

    // Attach evidence WITHOUT a critical finding.
    let mut ev =
        crate::evidence::UowEvidenceRecord::new("S-gate-4", &run_id, "2026-06-20T00:00:00Z");
    ev.set_scoped_scan(crate::evidence::ScopedScanSummary {
        files_scanned: 1,
        total_findings: 1,
        has_critical: false,
        findings: Vec::new(),
    });
    ev.compute_hash();
    state.uow.attach_evidence("S-gate-4", ev);

    let app = router(state);
    let body = serde_json::json!({ "by": "zach" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/sign-off"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "non-critical evidence must not block sign-off"
    );
}

// ── PR comment posting: post_pr_comment (issue #53) ──────────────────────
// The actual GitHub call is only exercised in integration tests (needs a live token).
// Unit tests verify the graceful-degradation path (empty token → Ok(None)).

#[tokio::test]
async fn post_pr_comment_gracefully_degrades_without_token() {
    // An empty token must not panic or return Err — it returns Ok(None).
    let result = crate::arm::post_pr_comment("owner", "repo", 42, "# Test\nno token", "").await;
    assert!(result.is_ok(), "must not error without a token");
    assert!(
        result.unwrap().is_none(),
        "must return None without a token"
    );
}

// ── WorkItem + UoW layer (governed-dev surface) ──────────────────────────
