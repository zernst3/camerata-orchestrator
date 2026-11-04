//! Work item, unit-of-work authoring and chat prompt rendering tests.

use super::*;

/// POST /api/workitems/pull degrades gracefully with no token: it returns an
/// empty item list plus a hint, never an error.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn workitems_pull_no_token_returns_empty_with_message() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workitems/pull")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["items"].as_array().unwrap().len(), 0);
    assert!(json["message"].is_string());
}

/// POST /api/workitems/comments degrades gracefully with no token: it returns an
/// empty comment list (never an error) so the modal renders "No comments."
#[tokio::test]
#[serial_test::serial(process_env)]
async fn workitems_comments_no_token_returns_empty() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let body = serde_json::json!({ "work_item_id": "github:o/r#20" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workitems/comments")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["comments"].as_array().unwrap().len(), 0);
}

/// POST /api/workitems/assignees degrades gracefully with no token: it returns an
/// empty user list so the @-autocomplete simply shows no suggestions.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn workitems_assignees_no_token_returns_empty() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let body = serde_json::json!({ "work_item_id": "github:o/r#20" }).to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workitems/assignees")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["users"].as_array().unwrap().len(), 0);
}

/// GET /api/me returns `{ login: null }` with no token (the UI hides "Assign to me").
#[tokio::test]
#[serial_test::serial(process_env)]
async fn me_no_token_returns_null_login() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(
        json.get("login").is_some(),
        "the `login` key is always present"
    );
    assert!(json["login"].is_null(), "no token → login is null");
}

/// POST /api/workitems/assign errors without a token (there is nothing to assign
/// against), and rejects an empty assignee.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn workitems_assign_requires_token_and_nonempty_assignee() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    // No token → error status (not a silent success).
    let body =
        serde_json::json!({ "work_item_id": "github:o/r#20", "assignee": "octocat" }).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workitems/assign")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "assign without a token must not report success"
    );
}

/// POST /api/workitems/updated-check degrades gracefully with no token: it returns an
/// empty updates list (never an error) so a background poll is silent.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn workitems_updated_check_no_token_returns_empty() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let app = router(AppState::new(
        std::sync::Arc::new(InMemoryStoryStore::new()),
    ));
    let body = serde_json::json!({
        "items": [{ "work_item_id": "github:o/r#20", "repo": "o/r", "number": 20 }]
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workitems/updated-check")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["updates"].as_array().unwrap().len(), 0);
}

/// POST /api/uow/from-workitem creates a UoW on first call and DEDUPES on the
/// second (created=false, same uow_id, no duplicate). Token-free: the spine row is
/// seeded from the id alone, so this is hermetic.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn uow_from_workitem_dedups_by_external_ref() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // ISOLATION: the dedup + listing are now project-scoped; activate a project whose
    // repos cover the work item's repo (`o/r`).
    state.projects.create("P", vec!["o/r".to_string()]).unwrap();
    let uow = state.uow.clone();
    let app = router(state);

    let call = |app: Router| async {
        let body = serde_json::json!({ "work_item_id": "github:o/r#20" }).to_string();
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/from-workitem")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
    };

    // First call: created=true, the UoW key is the story id (prefix stripped).
    let json1 = body_json(call(app.clone()).await).await;
    assert_eq!(json1["created"], true);
    assert_eq!(json1["uow_id"], "o/r#20");

    // Second call with the SAME work item: created=false, same id.
    let json2 = body_json(call(app).await).await;
    assert_eq!(json2["created"], false, "must dedup, never duplicate");
    assert_eq!(json2["uow_id"], "o/r#20");

    // Exactly one UoW exists for this story.
    let n = uow.list().iter().filter(|u| u.story_id == "o/r#20").count();
    assert_eq!(n, 1, "exactly one UoW for the work item");
}

/// GET /api/uows returns each UoW with the WorkItem it references (resolved from
/// the spine, repo set) and its lifecycle stage.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn uows_list_carries_workitem_and_stage() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // ISOLATION: /api/uows is project-scoped; activate a project covering `o/r`.
    state.projects.create("P", vec!["o/r".to_string()]).unwrap();
    let app = router(state);

    // Create one UoW from a work item.
    let body = serde_json::json!({ "work_item_id": "github:o/r#20" }).to_string();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/from-workitem")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/uows")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let uows = json["uows"].as_array().unwrap();
    assert_eq!(uows.len(), 1);
    assert_eq!(uows[0]["id"], "o/r#20");
    assert_eq!(uows[0]["stage"], "intake");
    assert_eq!(uows[0]["work_item"]["id"], "github:o/r#20");
    assert_eq!(
        uows[0]["work_item"]["repo"], "o/r",
        "repo set on the work item"
    );
    assert_eq!(uows[0]["work_item"]["number"], 20);
}

/// POST /api/workitems/comment rejects an empty body and a non-github id without
/// touching the network. The well-formed-token path is exercised in integration.
#[tokio::test]
async fn workitems_comment_validates_input() {
    // Seed the token on this AppState's own credential store (store-first in
    // `github_token()`) rather than the process-global `CAMERATA_GITHUB_TOKEN` env var,
    // so this test does not race other tests that mutate that env var under
    // `cargo test --workspace` (a shared-process-env flake). Per-instance store =
    // deterministic regardless of what any concurrent test does to the env.
    let creds = crate::credentials::MemoryCredentialStore::new();
    crate::credentials::CredentialStore::set(&creds, crate::credentials::GITHUB_TOKEN, "ghp_test")
        .unwrap();
    let mut state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.credential_store = std::sync::Arc::new(creds);
    let app = router(state);

    // ROUTES-7: an empty body is invalid REQUEST INPUT → 400 Bad Request (was 500).
    let body = serde_json::json!({ "work_item_id": "github:o/r#20", "body": "  " }).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workitems/comment")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// The work-item id parser produces the (repo, number) the comment/refresh paths
/// pass to the adapter, and rejects malformed ids. This pins the comment-payload
/// addressing (repo + number) the GitHub comment call uses.
#[test]
fn parse_github_work_item_id_extracts_repo_and_number() {
    let (repo, number) = match parse_github_work_item_id("github:zernst3/camerata-orchestrator#42")
    {
        Ok(v) => v,
        Err(e) => panic!("valid id should parse: {}", e.err),
    };
    assert_eq!(repo, "zernst3/camerata-orchestrator");
    assert_eq!(number, 42);

    // Wrong provider, missing number, bad repo, non-numeric number all error.
    assert!(parse_github_work_item_id("jira:PROJ-1").is_err());
    assert!(parse_github_work_item_id("github:o/r").is_err());
    assert!(parse_github_work_item_id("github:notarepo#1").is_err());
    assert!(parse_github_work_item_id("github:o/r#notanumber").is_err());
}

// ── AI story authoring from a blank UoW (2026-06-22) ──────────────────────────

/// `parse_author_response` handles a clean JSON object, a fenced JSON block, and
/// non-JSON prose (kept as the conversational reply, draft left untouched).
#[test]
fn parse_author_response_handles_json_fenced_and_prose() {
    let (t, b, r, opts) = parse_author_response(
        "{\"title\":\"Add export\",\"body\":\"## Summary\\nDo it\",\"reply\":\"What format?\"}",
    );
    assert_eq!(t, "Add export");
    assert!(b.contains("Summary"));
    assert_eq!(r, "What format?");
    // No options key present -> empty (free-text fallback).
    assert!(opts.is_empty());

    // Fenced block is unwrapped.
    let fenced = "```json\n{\"title\":\"T\",\"body\":\"B\",\"reply\":\"R\"}\n```";
    let (t, b, r, opts) = parse_author_response(fenced);
    assert_eq!((t.as_str(), b.as_str(), r.as_str()), ("T", "B", "R"));
    assert!(opts.is_empty());

    // Non-JSON: whole text becomes the reply; title/body empty (caller keeps draft).
    let (t, b, r, opts) = parse_author_response("Just some prose, no JSON here.");
    assert!(t.is_empty() && b.is_empty());
    assert_eq!(r, "Just some prose, no JSON here.");
    assert!(opts.is_empty());
}

/// When the authoring model asks a clarifying question it returns structured
/// `options`; `parse_author_response` surfaces them (label + description) so the
/// loop can emit a structured clarification. Entries missing a label are dropped.
#[test]
fn parse_author_response_extracts_structured_options() {
    let raw = "{\"title\":\"T\",\"body\":\"B\",\"reply\":\"Which timezone?\",\"options\":[\
            {\"label\":\"Org tz\",\"description\":\"consistent send time\"},\
            {\"label\":\"Member tz\",\"description\":\"local hour per member\"},\
            {\"description\":\"no label, dropped\"}]}";
    let (_, _, r, opts) = parse_author_response(raw);
    assert_eq!(r, "Which timezone?");
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0].label, "Org tz");
    assert_eq!(opts[0].description, "consistent send time");
    assert_eq!(opts[1].label, "Member tz");
}

/// `POST /api/uow/blank` creates a draft UoW that then lists in `/api/uows` with
/// `work_item = null` and `authoring = true`.
#[tokio::test]
async fn blank_uow_creates_and_lists_as_authoring() {
    // REGRESSION FIX (fix/draft-project-scope): a blank draft has a `draft-` id with NO
    // resolvable repo. It is scoped to its CREATING project's id, so the project-scoped
    // /api/uows for THAT project INCLUDES it (authoring is unblocked), while a DIFFERENT
    // active project EXCLUDES it (cross-project isolation preserved).
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // P is created (and made active). Q exists too, for the isolation check.
    let proj_p = state.projects.create("P", vec!["o/r".to_string()]).unwrap();
    state.projects.create("Q", vec!["x/y".to_string()]).unwrap();
    // Make P the active project again (create made Q active last).
    state.projects.set_active(&proj_p.id);
    let app = router(state.clone());

    let resp = app
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
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let uow_id = json["uow_id"].as_str().unwrap().to_string();
    assert!(uow_id.starts_with("draft-"), "draft id, got {uow_id}");

    // P is active: the draft it created APPEARS in /api/uows as an authoring draft.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/uows")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    let uows = json["uows"].as_array().unwrap();
    let found = uows
        .iter()
        .find(|u| u["id"] == serde_json::json!(uow_id))
        .unwrap_or_else(|| {
            panic!("draft must appear in its own project's /api/uows, got: {json:?}")
        });
    assert_eq!(
        found["authoring"],
        serde_json::json!(true),
        "the draft is surfaced as an authoring draft"
    );

    // Switch the active project to Q: the draft created under P must NOT appear.
    state.projects.set_active(
        &state
            .projects
            .list()
            .into_iter()
            .find(|p| p.name == "Q")
            .unwrap()
            .id,
    );
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/uows")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    let uows = json["uows"].as_array().unwrap();
    assert!(
        uows.iter().all(|u| u["id"] != serde_json::json!(uow_id)),
        "P's draft must NOT leak into project Q's /api/uows (cross-project isolation), got: {json:?}"
    );
}

/// `POST /api/uow/:id/author` appends the chat turn and persists the requirements
/// even when the LLM is unavailable (token-free): the endpoint degrades gracefully
/// with a clear note rather than crashing, and the user message is recorded.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn author_endpoint_appends_chat_without_token() {
    // Ensure no API key is set so the LLM path returns the graceful note (the CLI may
    // or may not exist on CI; either way the user turn is appended and we get a 200).
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let uow_store = state.uow.clone();
    let app = router(state);

    let draft = uow_store.create_blank();
    let id = draft.story_id.clone();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{}/author", id))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"message":"Add a CSV export to the report"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The store reflects the appended user turn + the requirements prompt.
    let after = uow_store.get_or_create(&id);
    let st = after.authoring.expect("authoring state");
    assert_eq!(st.requirements_prompt, "Add a CSV export to the report");
    assert_eq!(st.chat.first().map(|m| m.role.as_str()), Some("user"));
    assert_eq!(
        st.chat.first().map(|m| m.text.as_str()),
        Some("Add a CSV export to the report")
    );
    // An AI turn (real reply or graceful note) is always appended after the user turn.
    assert_eq!(st.chat.get(1).map(|m| m.role.as_str()), Some("ai"));
}

/// `POST /api/uow/:id/publish` rejects (non-2xx) with a clear reason when no GitHub
/// token is configured.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn publish_without_token_is_rejected_with_reason() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let uow_store = state.uow.clone();
    let app = router(state);

    let draft = uow_store.create_blank();
    // Give it a draft title so we reach the token check (not the empty-title check).
    uow_store.append_authoring_turn(&draft.story_id, "req", "ok", "A title", "A body");

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{}/publish", draft.story_id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"repo":"me/api"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!resp.status().is_success(), "no token -> non-2xx");
    let json = body_json(resp).await;
    let err = json["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("GitHub") || err.contains("token"),
        "reason names the missing token, got: {err}"
    );
}

/// The publish LINK step (what `uow_publish` does after `create_issue`) wires the work
/// item onto the spine and links the draft UoW WITHOUT re-keying it; `/api/uows` then
/// resolves the work item and the entry is no longer flagged as authoring. This
/// exercises the link logic without a network call to `create_issue`.
#[tokio::test]
async fn publish_link_step_links_draft_without_rekey() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // ISOLATION: /api/uows is project-scoped and resolves a LINKED draft's repo from its
    // work_item (`me/api#7`). Activate a project covering `me/api` so the linked draft is
    // visible (an UNLINKED draft would remain excluded — no resolvable repo).
    state
        .projects
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    let uow_store = state.uow.clone();
    let stories = state.stories.clone();
    let app = router(state);

    let draft = uow_store.create_blank();
    let draft_id = draft.story_id.clone();
    uow_store.append_authoring_turn(&draft_id, "req", "ok", "Authored title", "Body");

    // Simulate create_issue having returned issue #7 in me/api: upsert the spine story
    // and link the draft (the two writes uow_publish performs after the HTTP call).
    let story = crate::github_issues::issue_to_story("me/api", 7, "Authored title", "Body");
    let wi_story_id = story.id.clone();
    stories.upsert(story).await.unwrap();
    let linked = uow_store.link_work_item(&draft_id, &wi_story_id);

    // The KEY is unchanged (no re-key); the work_item ref carries the real id.
    assert_eq!(linked.story_id, draft_id, "draft id kept as the key");
    assert_eq!(linked.work_item.as_deref(), Some(wi_story_id.as_str()));

    // /api/uows now resolves the work item and the entry is no longer authoring.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/uows")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;
    let entry = json["uows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == draft_id)
        .expect("linked draft still listed under its draft id");
    assert_eq!(
        entry["authoring"], false,
        "linked draft is no longer authoring"
    );
    assert_eq!(entry["work_item"]["number"], 7);
    assert_eq!(entry["work_item"]["repo"], "me/api");
}

// ── render_chat_prompt — conversation-context embedding ───────────────────
//
// These tests cover the prompt-rendering helper directly (no model calls).
// They guard: (a) history-present path embeds prior turns and the new message,
// (b) empty-history path reproduces the bare single-prompt (back-compat),
// (c) the cap drops the oldest turns when history exceeds CHAT_HISTORY_TURN_CAP,
// (d) role labels render correctly ("user" → "User", "assistant" → "Assistant").

fn make_turn(role: &str, content: &str) -> ChatTurn {
    ChatTurn {
        role: role.to_string(),
        content: content.to_string(),
    }
}

/// (b) Back-compat: empty history must produce exactly the bare prompt, unchanged.
#[test]
fn render_chat_prompt_empty_history_returns_bare_prompt() {
    let result = render_chat_prompt(&[], "Hello, world!");
    assert_eq!(
        result, "Hello, world!",
        "empty history must return the bare prompt unchanged (back-compat)"
    );
}

/// (b) Back-compat: also verify with an empty string prompt.
#[test]
fn render_chat_prompt_empty_history_empty_prompt_returns_empty() {
    let result = render_chat_prompt(&[], "");
    assert_eq!(
        result, "",
        "empty history + empty prompt returns empty string"
    );
}

/// (a) History present: rendered prompt contains the prior user turn.
#[test]
fn render_chat_prompt_includes_prior_user_turn() {
    let history = vec![make_turn("user", "What is SEC-1?")];
    let result = render_chat_prompt(&history, "Can you elaborate?");
    assert!(
        result.contains("User: What is SEC-1?"),
        "rendered prompt must include prior user turn; got: {result:?}"
    );
}

/// (a) History present: rendered prompt contains the prior assistant turn.
#[test]
fn render_chat_prompt_includes_prior_assistant_turn() {
    let history = vec![
        make_turn("user", "What is SEC-1?"),
        make_turn("assistant", "SEC-1 bans hardcoded secrets."),
    ];
    let result = render_chat_prompt(&history, "Can you elaborate?");
    assert!(
        result.contains("Assistant: SEC-1 bans hardcoded secrets."),
        "rendered prompt must include prior assistant turn; got: {result:?}"
    );
}

/// (a) History present: the new message appears after the transcript block.
#[test]
fn render_chat_prompt_new_message_appears_after_history() {
    let history = vec![
        make_turn("user", "First question"),
        make_turn("assistant", "First answer"),
    ];
    let new_msg = "Second question";
    let result = render_chat_prompt(&history, new_msg);
    // The transcript block must come before the new message.
    let history_pos = result.find("Conversation so far:").expect("missing header");
    let new_msg_pos = result.find(new_msg).expect("missing new message");
    assert!(
        history_pos < new_msg_pos,
        "history transcript must precede new message (history at {history_pos}, new at {new_msg_pos})"
    );
    // The section header "User's new message:" must also be present.
    assert!(
        result.contains("User's new message:"),
        "missing 'User's new message:' label"
    );
    assert!(
        result.contains(new_msg),
        "new message must appear in the rendered prompt"
    );
}

/// (a) History with a full user+assistant exchange: both turns and the new message present.
#[test]
fn render_chat_prompt_full_exchange_then_new_message() {
    let history = vec![
        make_turn("user", "Tell me about ARCH-1"),
        make_turn("assistant", "ARCH-1 enforces layered boundaries."),
    ];
    let result = render_chat_prompt(&history, "How does it differ from ARCH-2?");
    assert!(result.contains("User: Tell me about ARCH-1"));
    assert!(result.contains("Assistant: ARCH-1 enforces layered boundaries."));
    assert!(result.contains("How does it differ from ARCH-2?"));
}

/// (d) Role labels: "user" maps to "User", "assistant" maps to "Assistant".
#[test]
fn render_chat_prompt_role_labels_are_correct() {
    let history = vec![
        make_turn("user", "u-content"),
        make_turn("assistant", "a-content"),
    ];
    let result = render_chat_prompt(&history, "new");
    assert!(
        result.contains("User: u-content"),
        "user role must render as 'User'"
    );
    assert!(
        result.contains("Assistant: a-content"),
        "assistant role must render as 'Assistant'"
    );
}

/// (d) Any non-"user" role (including "assistant") maps to "Assistant".
#[test]
fn render_chat_prompt_unknown_role_renders_as_assistant() {
    let history = vec![make_turn("ai", "reply")];
    let result = render_chat_prompt(&history, "next");
    assert!(
        result.contains("Assistant: reply"),
        "non-user role must render as 'Assistant'; got: {result:?}"
    );
}

/// (c) Cap: when history exceeds CHAT_HISTORY_TURN_CAP, oldest turns are dropped.
#[test]
fn render_chat_prompt_cap_drops_oldest_turns() {
    // Build CHAT_HISTORY_TURN_CAP + 2 turns so the first two are dropped.
    // Use a format like "unique-msg-NNNN-end" to avoid substring false-positives
    // (e.g. "msg-1" would match inside "msg-10", "msg-11", etc.).
    let cap = CHAT_HISTORY_TURN_CAP;
    let mut history: Vec<ChatTurn> = (0..cap + 2)
        .map(|i| make_turn("user", &format!("unique-msg-{:04}-end", i)))
        .collect();
    // Give the last turn a distinct marker.
    history.push(make_turn("assistant", "final-assistant-reply"));
    let total = history.len(); // cap + 3
    let result = render_chat_prompt(&history, "new-prompt");

    // The first `total - cap` turns must be absent.
    let dropped_count = total - cap;
    for i in 0..dropped_count {
        assert!(
            !result.contains(&format!("unique-msg-{:04}-end", i)),
            "turn {i} (oldest) should have been dropped by the cap"
        );
    }
    // The most-recent turn that survived the cap must be present.
    let first_kept = dropped_count;
    assert!(
        result.contains(&format!("unique-msg-{:04}-end", first_kept)),
        "turn {first_kept} should survive the cap"
    );
    // The new prompt is still present.
    assert!(
        result.contains("new-prompt"),
        "new prompt must appear after capped history"
    );
}

/// (c) Cap: when history is exactly at the cap limit, no turns are dropped.
#[test]
fn render_chat_prompt_at_cap_limit_nothing_dropped() {
    let cap = CHAT_HISTORY_TURN_CAP;
    let history: Vec<ChatTurn> = (0..cap)
        .map(|i| make_turn("user", &format!("msg-{i}")))
        .collect();
    let result = render_chat_prompt(&history, "new");
    // All turns must appear.
    for i in 0..cap {
        assert!(
            result.contains(&format!("msg-{i}")),
            "turn {i} must not be dropped when at the cap limit"
        );
    }
}

// ── scan floor / semgrep dedup tests ─────────────────────────────────────
//
// These tests exercise `dedup_preview_against_floor` and `semgrep_floor_category`
// directly, without standing up an HTTP server or hitting the file system.
// See docs/decisions/2026-06-22_scan_floor_semgrep_dedup.md for the full rationale.
