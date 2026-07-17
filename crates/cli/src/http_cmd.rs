//! HTTP-adapter subcommand handlers (Phase F, GAP-7; see
//! `docs/plans/2026-07-01_backend-headless-core.md`).
//!
//! These are the CLI's own first-rung verbs over `camerata-client` — the SAME typed
//! HTTP client the MCP adapter (`crates/mcp`, Phase E) and the Dioxus cockpit
//! (`crates/ui`) use. Every handler here is a thin delegation to a
//! [`camerata_client::Client`] call: a real HTTP round trip to the running BFF, never
//! an in-process call into `camerata-server` or any other behavior-carrying camerata
//! crate.
//!
//! # Shape
//!
//! Each handler is `async fn handle_x(client: &Client, ...) -> Result<String, ClientError>`
//! — the DTO serialized as pretty JSON on success, the [`ClientError`] propagated
//! untouched on failure. `main.rs` owns printing (stdout on `Ok`, stderr + non-zero
//! exit on `Err`) and building the `Client` (respecting `--bff-url` / `CAMERATA_BFF_URL`
//! via [`camerata_client::Client::new`] / [`camerata_client::Client::with_base`]). This
//! split is what makes the handlers directly unit-testable against a `wiremock` mock
//! server without spawning a subprocess.

use camerata_api_types::run::StartRunRequest;
use camerata_api_types::workitems::AssignWorkItemRequest;
use camerata_client::{Client, ClientError};
use serde::Serialize;

/// Render a DTO as pretty JSON. A plain-data api-types DTO cannot realistically fail to
/// serialize, but this never panics over it even so — a failure renders as a JSON
/// error object rather than taking the process down (mirrors `crates/mcp`'s
/// `dto_tool_result` non-panicking fallback for the same edge case).
fn to_json_string(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| {
        format!("{{\"error\": \"response serialization failed: {e}\"}}")
    })
}

/// `stories` — `GET /api/stories` via [`Client::list_stories`].
pub async fn handle_stories(client: &Client) -> Result<String, ClientError> {
    let stories = client.list_stories().await?;
    Ok(to_json_string(&stories))
}

/// `run <RUN_ID>` — `GET /api/runs/:id` via [`Client::get_run`].
pub async fn handle_run(client: &Client, run_id: &str) -> Result<String, ClientError> {
    let run = client.get_run(run_id).await?;
    Ok(to_json_string(&run))
}

/// `uows` — `GET /api/uows` via [`Client::list_uows`].
pub async fn handle_uows(client: &Client) -> Result<String, ClientError> {
    let uows = client.list_uows().await?;
    Ok(to_json_string(&uows))
}

/// `assign --work-item <ID> --assignee <LOGIN>` — `POST /api/workitems/assign` via
/// [`Client::assign_work_item`].
pub async fn handle_assign(
    client: &Client,
    work_item_id: String,
    assignee: String,
) -> Result<String, ClientError> {
    let resp = client
        .assign_work_item(AssignWorkItemRequest {
            work_item_id,
            assignee,
        })
        .await?;
    Ok(to_json_string(&resp))
}

/// `start-run <STORY_ID> [--model <M>] [--skip-layer2]` — `POST /api/stories/:id/run`
/// via [`Client::start_run`].
pub async fn handle_start_run(
    client: &Client,
    story_id: &str,
    model: Option<String>,
    skip_layer2: bool,
) -> Result<String, ClientError> {
    let body = StartRunRequest {
        model,
        tier_map: None,
        skip_layer2: skip_layer2.then_some(true),
    };
    let resp = client.start_run(story_id, body).await?;
    Ok(to_json_string(&resp))
}

/// `events <RUN_ID>` — `GET /api/runs/:id/events` via [`Client::run_events`].
pub async fn handle_events(client: &Client, run_id: &str) -> Result<String, ClientError> {
    let events = client.run_events(run_id).await?;
    Ok(to_json_string(&events))
}

/// `recent-events [--limit N]` — `GET /api/governance/events?limit=N` via
/// [`Client::recent_events`].
pub async fn handle_recent_events(client: &Client, limit: u32) -> Result<String, ClientError> {
    let events = client.recent_events(limit).await?;
    Ok(to_json_string(&events))
}

/// `feedback <PROJECT_ID>` — `GET /api/projects/:id/feedback` via
/// [`Client::project_feedback`].
pub async fn handle_feedback(client: &Client, project_id: &str) -> Result<String, ClientError> {
    let reports = client.project_feedback(project_id).await?;
    Ok(to_json_string(&reports))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn handle_stories_hits_the_route_and_returns_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "owner/repo#1",
                    "external_ref": null,
                    "title": "Fix the thing",
                    "description": "It is broken",
                    "status": "intake",
                    "created_by": "agent",
                    "targets": [{ "repo": "owner/repo", "role": null }],
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_stories(&client).await.expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed[0]["id"], "owner/repo#1");
    }

    #[tokio::test]
    async fn handle_run_hits_the_id_scoped_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/run-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "run-1",
                "story_id": "owner/repo#1",
                "status": "executing",
                "events": [],
                "done": false,
                "mode": "scripted",
                "last_progress_label": "starting",
                "kind": "watched",
                "stall_policy": "alert",
                "failure_reason": null,
                "idle_ms": 12,
                "stalled": false,
                "stall_threshold_ms": 120000,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_run(&client, "run-1").await.expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed["id"], "run-1");
        assert_eq!(parsed["mode"], "scripted");
    }

    #[tokio::test]
    async fn handle_uows_hits_the_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/uows"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uows": [
                    { "id": "owner/repo#1", "work_item": null, "stage": "intake", "authoring": false }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_uows(&client).await.expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed["uows"][0]["id"], "owner/repo#1");
    }

    #[tokio::test]
    async fn handle_assign_posts_the_contract_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/workitems/assign"))
            .and(body_json(serde_json::json!({
                "work_item_id": "github:owner/repo#1",
                "assignee": "octocat",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "assignees": ["octocat"],
                "updated_at": "2026-07-01T00:00:00Z",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_assign(&client, "github:owner/repo#1".to_string(), "octocat".to_string())
            .await
            .expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["assignees"][0], "octocat");
    }

    #[tokio::test]
    async fn handle_start_run_posts_to_the_id_scoped_route_with_flags() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stories/story-1/run"))
            .and(body_json(serde_json::json!({
                "model": "claude-sonnet-4-6",
                "skip_layer2": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "run_id": "run-42",
                "story_id": "story-1",
                "mode": "scripted",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_start_run(
            &client,
            "story-1",
            Some("claude-sonnet-4-6".to_string()),
            true,
        )
        .await
        .expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed["run_id"], "run-42");
    }

    #[tokio::test]
    async fn handle_events_hits_the_id_scoped_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/run-1/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1,
                    "run_id": "run-1",
                    "story_id": null,
                    "ts": "2026-07-08T00:00:00Z",
                    "kind": "run_started",
                    "severity": "info",
                    "actor": "system",
                    "rule_id": null,
                    "reason": null,
                    "detail": null,
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_events(&client, "run-1").await.expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed[0]["run_id"], "run-1");
        assert_eq!(parsed[0]["kind"], "run_started");
    }

    #[tokio::test]
    async fn handle_recent_events_hits_the_route_with_limit_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/governance/events"))
            .and(wiremock::matchers::query_param("limit", "25"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 2,
                    "run_id": "run-2",
                    "story_id": null,
                    "ts": "2026-07-08T00:01:00Z",
                    "kind": "gate_deny",
                    "severity": "error",
                    "actor": "agent",
                    "rule_id": "SEC-1",
                    "reason": "denied",
                    "detail": null,
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_recent_events(&client, 25)
            .await
            .expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed[0]["run_id"], "run-2");
        assert_eq!(parsed[0]["kind"], "gate_deny");
    }

    #[tokio::test]
    async fn handle_feedback_hits_the_id_scoped_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/feedback"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1,
                    "project_id": "proj-1",
                    "source": "user",
                    "kind": "user_report",
                    "title": "button does nothing",
                    "description": "",
                    "context": { "route": null, "element": null, "stack": null, "console": null, "extra": {} },
                    "severity": "info",
                    "status": "open",
                    "ts": "2026-07-08T00:00:00Z",
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let json = handle_feedback(&client, "proj-1")
            .await
            .expect("must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be JSON");
        assert_eq!(parsed[0]["project_id"], "proj-1");
        assert_eq!(parsed[0]["title"], "button does nothing");
    }

    /// A `ClientError` (here: BFF 404) must propagate as `Err`, never a panic and never
    /// a silently-swallowed success.
    #[tokio::test]
    async fn client_error_propagates_as_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/does-not-exist"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({ "error": "run not found: does-not-exist" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let err = handle_run(&client, "does-not-exist")
            .await
            .expect_err("a 404 must be an Err, not a panic");

        match err {
            ClientError::Api { status, body } => {
                assert_eq!(status.as_u16(), 404);
                assert!(body.contains("run not found"));
            }
            other => panic!("expected ClientError::Api, got {other:?}"),
        }
    }

    /// A transport-level failure (nothing listening) also propagates as `Err`.
    #[tokio::test]
    async fn transport_failure_propagates_as_err() {
        let client = Client::with_base("http://127.0.0.1:1");
        let err = handle_stories(&client)
            .await
            .expect_err("connection refused must be an Err, not a panic");
        assert!(matches!(err, ClientError::Request(_)));
    }
}
