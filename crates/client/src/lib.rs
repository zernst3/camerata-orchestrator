//! `camerata-client` — a typed HTTP client over the running BFF's (`camerata-server`)
//! `/api/*` routes, typed against `camerata-api-types` DTOs (Phase D of the DTO
//! extraction, see `docs/plans/2026-07-01_backend-headless-core.md`).
//!
//! This is the shared adapter both the MCP server (Phase E) and the HTTP CLI (Phase F)
//! call — it depends ONLY on `camerata-api-types` (the pure-serde wire leaf) plus
//! generic HTTP/serde/error crates, never on `camerata-server` or any other
//! behavior-carrying camerata crate. Callers never talk to the backend crates
//! in-process; every verb here is a real HTTP round trip to the BFF, same as the
//! Dioxus cockpit (`crates/ui`) already does.
//!
//! # Base URL resolution
//!
//! Mirrors `crates/ui/src/main.rs`'s `bff_base()` idiom: [`bff_base`] reads
//! `CAMERATA_BFF_URL`, falling back to the embedded BFF's default local address. Tests
//! should prefer [`Client::with_base`] (pointed at a `wiremock` mock server) over the
//! process-global env var, since the env var is shared process-wide and racy under
//! parallel test execution.
//!
//! # First-rung verbs
//!
//! Exactly five, matching the routes registered in `crates/server/src/lib.rs::router`:
//!
//! | Method | [`Client`] fn | Route |
//! |---|---|---|
//! | GET | [`Client::list_stories`] | `/api/stories` |
//! | GET | [`Client::get_run`] | `/api/runs/:id` |
//! | GET | [`Client::list_uows`] | `/api/uows` |
//! | POST | [`Client::assign_work_item`] | `/api/workitems/assign` |
//! | POST | [`Client::start_run`] | `/api/stories/:id/run` |
//!
//! Every non-2xx response maps to [`ClientError::Api`] (status + raw body), never a
//! panic; a transport-level failure (connection refused, TLS, JSON decode) maps to
//! [`ClientError::Request`].
//!
//! # Governance-event read path (Phase H3)
//!
//! Two more verbs over the governance-event audit trail (Phase H1/H2's write-only
//! ledger, exposed for reading in Phase H3):
//!
//! | Method | [`Client`] fn | Route |
//! |---|---|---|
//! | GET | [`Client::run_events`] | `/api/runs/:id/events` |
//! | GET | [`Client::recent_events`] | `/api/governance/events` |
//!
//! # Product-Owner feedback-loop verbs
//!
//! Two verbs over the defect-report ingest surface (auto-capture + click-to-report):
//!
//! | Method | [`Client`] fn | Route |
//! |---|---|---|
//! | POST | [`Client::report_defect`] | `/api/feedback` |
//! | GET | [`Client::project_feedback`] | `/api/projects/:id/feedback` |

use camerata_api_types::feedback::DefectReport;
use camerata_api_types::governance::GovernanceEventDto;
use camerata_api_types::run::{RunStatusResponse, StartRunRequest, StartRunResponse};
use camerata_api_types::stories::CanonicalStory;
use camerata_api_types::uow::UowListResponse;
use camerata_api_types::workitems::{AssignWorkItemRequest, AssignWorkItemResponse};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// The BFF's default local address, matching `camerata_ui::BFF_URL`
/// (`crates/ui/src/main.rs`) — the embedded desktop BFF binds `127.0.0.1:8787`.
pub const DEFAULT_BFF_URL: &str = "http://127.0.0.1:8787";

/// Resolve the BFF base URL: `CAMERATA_BFF_URL` when set, else [`DEFAULT_BFF_URL`].
/// Mirrors `crates/ui/src/main.rs`'s `bff_base()` (~lines 34-43) so every adapter
/// (cockpit, MCP server, HTTP CLI) resolves the BFF the same way.
pub fn bff_base() -> String {
    std::env::var("CAMERATA_BFF_URL").unwrap_or_else(|_| DEFAULT_BFF_URL.to_string())
}

/// A typed error from a [`Client`] call. Non-2xx BFF responses map to [`Self::Api`]
/// (never a panic); transport-level failures (connection refused, TLS, JSON
/// encode/decode) map to [`Self::Request`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The BFF responded with a non-2xx status. `body` is the raw response body
    /// verbatim — the BFF conventionally shapes it as `{"error": "..."}` (see
    /// `crates/server/src/lib.rs`'s `AppError::into_response`), but callers that need
    /// the structured reason should parse `body` themselves rather than rely on that
    /// shape being guaranteed for every route (e.g. `start_run`'s 409 body carries
    /// additional `reason`/`active_run_id` fields — see its doc comment).
    #[error("BFF returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
    /// A transport-level failure: connection refused, timeout, TLS, or a response body
    /// that failed to decode as the expected JSON shape.
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
}

/// A typed HTTP client over the BFF's `/api/*` routes.
///
/// Construct with [`Client::new`] (production: resolves the base URL from
/// `CAMERATA_BFF_URL` / [`DEFAULT_BFF_URL`]) or [`Client::with_base`] (tests: point at
/// a `wiremock` mock server, or any explicit override).
pub struct Client {
    http: reqwest::Client,
    base: String,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client pointed at [`bff_base`] — the production default.
    pub fn new() -> Self {
        Self::with_base(bff_base())
    }

    /// A client pointed at an explicit base URL (e.g. a `wiremock` mock server's
    /// `.uri()` in tests). Preferred over setting `CAMERATA_BFF_URL` in tests: the env
    /// var is process-global and racy under parallel test execution, while this is a
    /// plain struct field.
    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Percent-encode one path SEGMENT (never a whole path — this also encodes `/`),
    /// for interpolating an id that may itself contain reserved URL characters into a
    /// `/api/.../:id/...` route. Story/UoW ids are commonly `owner/repo#123`
    /// (`camerata_worktracker::CanonicalStory::id`) — the `#` would otherwise be
    /// parsed as the URL fragment delimiter and silently truncate the path, and the
    /// `/` would otherwise split into an extra path segment. Mirrors
    /// `crates/ui/src/cockpit.rs`'s private `enc_seg` (same escaping, same rationale).
    fn enc_seg(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 8);
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    /// Decode a response as JSON on 2xx; map any other status to
    /// [`ClientError::Api`] with the raw body.
    async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, ClientError> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api { status, body });
        }
        Ok(resp.json::<T>().await?)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let resp = self.http.get(self.url(path)).send().await?;
        Self::decode(resp).await
    }

    async fn post_json<Req: Serialize + ?Sized, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp, ClientError> {
        let resp = self.http.post(self.url(path)).json(body).send().await?;
        Self::decode(resp).await
    }

    /// `GET /api/stories` — the canonical story spine, scoped to the active project
    /// (empty when no project is active). See `crates/server/src/lib.rs::stories`.
    pub async fn list_stories(&self) -> Result<Vec<CanonicalStory>, ClientError> {
        self.get_json("/api/stories").await
    }

    /// `GET /api/runs/:id` — the current state of a run enriched with live stall
    /// diagnostics. See `crates/server/src/lib.rs::get_run`.
    pub async fn get_run(&self, run_id: &str) -> Result<RunStatusResponse, ClientError> {
        self.get_json(&format!("/api/runs/{}", Self::enc_seg(run_id)))
            .await
    }

    /// `GET /api/uows` — every Unit of Work for the active project, each with the
    /// work item it references and its lifecycle stage (empty when no project is
    /// active). See `crates/server/src/lib.rs::uows_list`.
    pub async fn list_uows(&self) -> Result<UowListResponse, ClientError> {
        self.get_json("/api/uows").await
    }

    /// `POST /api/workitems/assign` — assign a tracker work item to a login, returning
    /// its updated assignee logins and `updated_at`. See
    /// `crates/server/src/lib.rs::workitems_assign`.
    pub async fn assign_work_item(
        &self,
        req: AssignWorkItemRequest,
    ) -> Result<AssignWorkItemResponse, ClientError> {
        self.post_json("/api/workitems/assign", &req).await
    }

    /// `POST /api/stories/:id/run` — start a governed run for a story. Returns the run
    /// id immediately; poll [`Client::get_run`] for status. See
    /// `crates/server/src/lib.rs::start_run`.
    ///
    /// Note: the BFF can refuse this with `409 Conflict` in two cases (a run already
    /// active for the story, or the no-code-first development gate not satisfied) —
    /// both surface here as [`ClientError::Api`] with `status == 409` and a JSON body
    /// carrying `{"error", "reason", ...}` (richer than the generic `AppError` shape;
    /// see `start_run`'s doc comment in `crates/server/src/lib.rs`), which callers that
    /// need the structured reason should parse from `body` themselves.
    pub async fn start_run(
        &self,
        story_id: &str,
        body: StartRunRequest,
    ) -> Result<StartRunResponse, ClientError> {
        self.post_json(
            &format!("/api/stories/{}/run", Self::enc_seg(story_id)),
            &body,
        )
        .await
    }

    /// `GET /api/runs/:id/events` — the full governance-event audit trail for one run,
    /// in insertion order. Empty when no governance log is open server-side. See
    /// `crates/server/src/lib.rs::get_run_events`.
    pub async fn run_events(&self, run_id: &str) -> Result<Vec<GovernanceEventDto>, ClientError> {
        self.get_json(&format!("/api/runs/{}/events", Self::enc_seg(run_id)))
            .await
    }

    /// `GET /api/governance/events?limit=N` — the `N` most recently recorded governance
    /// events across all runs, newest first. See
    /// `crates/server/src/lib.rs::recent_governance_events`.
    pub async fn recent_events(&self, limit: u32) -> Result<Vec<GovernanceEventDto>, ClientError> {
        self.get_json(&format!("/api/governance/events?limit={limit}"))
            .await
    }

    /// `POST /api/feedback` — ingest one defect report (Product-Owner feedback loop:
    /// auto-capture or click-to-report). Returns the assigned row id on success. See
    /// `crates/server/src/lib.rs::submit_feedback`.
    ///
    /// The BFF always answers 200 here (a fail-soft ingest endpoint — see the
    /// handler's doc comment), so a logical failure (`{"ok": false, "message": "..."}`,
    /// e.g. no feedback store open server-side) is NOT distinguishable from a
    /// successful 2xx by status code alone. This maps that case to
    /// `ClientError::Api { status: 200, body: message }` so callers still get an `Err`
    /// rather than silently receiving a bogus id.
    pub async fn report_defect(&self, report: DefectReport) -> Result<i64, ClientError> {
        #[derive(serde::Deserialize)]
        struct SubmitFeedbackResponse {
            ok: bool,
            id: Option<i64>,
            message: Option<String>,
        }
        let resp: SubmitFeedbackResponse = self.post_json("/api/feedback", &report).await?;
        if resp.ok {
            resp.id.ok_or_else(|| ClientError::Api {
                status: reqwest::StatusCode::OK,
                body: "server reported ok but omitted the assigned id".to_string(),
            })
        } else {
            Err(ClientError::Api {
                status: reqwest::StatusCode::OK,
                body: resp
                    .message
                    .unwrap_or_else(|| "feedback ingest failed".to_string()),
            })
        }
    }

    /// `GET /api/projects/:id/feedback` — one project's defect reports, newest first.
    /// Empty when no feedback store is open server-side. See
    /// `crates/server/src/lib.rs::project_feedback`.
    pub async fn project_feedback(&self, project_id: &str) -> Result<Vec<DefectReport>, ClientError> {
        self.get_json(&format!(
            "/api/projects/{}/feedback",
            Self::enc_seg(project_id)
        ))
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::stories::FeatureStatus;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_stories_hits_get_and_decodes() {
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
        let stories = client.list_stories().await.expect("must decode 200");

        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].id, "owner/repo#1");
        assert_eq!(stories[0].status, FeatureStatus::Intake);
        assert_eq!(stories[0].targets[0].repo, "owner/repo");
    }

    #[tokio::test]
    async fn get_run_hits_the_id_scoped_path_and_decodes() {
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
        let run = client.get_run("run-1").await.expect("must decode 200");

        assert_eq!(run.id, "run-1");
        assert_eq!(run.mode, "scripted");
        assert!(!run.done);
        assert!(!run.stalled);
    }

    #[tokio::test]
    async fn list_uows_hits_get_and_decodes() {
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
        let resp = client.list_uows().await.expect("must decode 200");

        assert_eq!(resp.uows.len(), 1);
        assert_eq!(resp.uows[0].id, "owner/repo#1");
        assert_eq!(resp.uows[0].stage, "intake");
        assert!(!resp.uows[0].authoring);
    }

    #[tokio::test]
    async fn assign_work_item_posts_the_right_body_and_decodes() {
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
        let resp = client
            .assign_work_item(AssignWorkItemRequest {
                work_item_id: "github:owner/repo#1".to_string(),
                assignee: "octocat".to_string(),
            })
            .await
            .expect("must decode 200");

        assert!(resp.ok);
        assert_eq!(resp.assignees, vec!["octocat".to_string()]);
        assert_eq!(resp.updated_at, "2026-07-01T00:00:00Z");
    }

    #[tokio::test]
    async fn start_run_posts_to_the_id_scoped_path_with_the_right_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/stories/story-1/run"))
            .and(body_json(serde_json::json!({ "model": "claude-sonnet-4-6" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "run_id": "run-42",
                "story_id": "story-1",
                "mode": "scripted",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let resp = client
            .start_run(
                "story-1",
                StartRunRequest {
                    model: Some("claude-sonnet-4-6".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("must decode 200");

        assert_eq!(resp.run_id, "run-42");
        assert_eq!(resp.mode, "scripted");
    }

    /// Story/run ids commonly contain `/` and `#` (`owner/repo#123`); `enc_seg` must
    /// percent-encode both so `format!("/api/stories/{}/run", enc_seg(id))` builds a
    /// single well-formed path segment rather than a URL whose fragment silently
    /// truncates the request (see `enc_seg`'s doc comment). Mirrors
    /// `crates/ui/src/cockpit.rs`'s `enc_seg_encodes_slash_and_hash` test.
    #[test]
    fn enc_seg_encodes_slash_and_hash() {
        assert_eq!(Client::enc_seg("acme/web#42"), "acme%2Fweb%2342");
        assert_eq!(Client::enc_seg("CAM-7_v.1~x"), "CAM-7_v.1~x");
    }

    #[tokio::test]
    async fn run_events_hits_the_id_scoped_path_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/run-1/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 1,
                    "run_id": "run-1",
                    "story_id": "owner/repo#1",
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
        let events = client.run_events("run-1").await.expect("must decode 200");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].run_id, "run-1");
        assert_eq!(events[0].kind, "run_started");
        assert_eq!(events[0].severity, "info");
    }

    #[tokio::test]
    async fn recent_events_hits_get_with_limit_query_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/governance/events"))
            .and(wiremock::matchers::query_param("limit", "50"))
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
        let events = client.recent_events(50).await.expect("must decode 200");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].run_id, "run-2");
        assert_eq!(events[0].kind, "gate_deny");
        assert_eq!(events[0].rule_id.as_deref(), Some("SEC-1"));
    }

    #[tokio::test]
    async fn run_events_non_2xx_maps_to_client_error_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/runs/nope/events"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({ "error": "run not found: nope" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let err = client
            .run_events("nope")
            .await
            .expect_err("a 404 must be an Err, not a panic");

        match err {
            ClientError::Api { status, body } => {
                assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
                assert!(body.contains("run not found"));
            }
            other => panic!("expected ClientError::Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_2xx_response_maps_to_client_error_api_with_status_and_body() {
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
        let err = client
            .get_run("does-not-exist")
            .await
            .expect_err("a 404 must be an Err, not a panic");

        match err {
            ClientError::Api { status, body } => {
                assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
                assert!(
                    body.contains("run not found"),
                    "body must be preserved verbatim; got: {body}"
                );
            }
            other => panic!("expected ClientError::Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn report_defect_posts_the_report_and_decodes_the_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/feedback"))
            .and(body_json(serde_json::json!({
                "id": null,
                "project_id": "proj-1",
                "source": "auto",
                "kind": "runtime_error",
                "title": "TypeError: x is undefined",
                "description": "",
                "context": { "route": null, "element": null, "stack": null, "console": null, "extra": {} },
                "severity": "info",
                "status": "open",
                "ts": "2026-07-08T00:00:00Z",
                "fingerprint": null,
                "count": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "id": 7,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let mut report = DefectReport::auto(
            "proj-1",
            camerata_api_types::feedback::DefectKind::RuntimeError,
            "TypeError: x is undefined",
        );
        // Pin the timestamp so the wiremock body_json matcher is exact.
        report.ts = "2026-07-08T00:00:00Z".to_string();

        let id = client.report_defect(report).await.expect("must decode 200");
        assert_eq!(id, 7);
    }

    #[tokio::test]
    async fn report_defect_ok_false_maps_to_client_error_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/feedback"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "feedback store is not available this session",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let report = DefectReport::user(
            "proj-2",
            camerata_api_types::feedback::DefectKind::UserReport,
            "button does nothing",
        );
        let err = client
            .report_defect(report)
            .await
            .expect_err("ok:false must surface as an Err, not a fabricated id");

        match err {
            ClientError::Api { status, body } => {
                assert_eq!(status, reqwest::StatusCode::OK);
                assert!(body.contains("feedback store is not available"));
            }
            other => panic!("expected ClientError::Api, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn project_feedback_hits_the_id_scoped_path_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/feedback"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 3,
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
        let reports = client
            .project_feedback("proj-1")
            .await
            .expect("must decode 200");

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].project_id, "proj-1");
        assert_eq!(reports[0].title, "button does nothing");
    }

    #[tokio::test]
    async fn project_feedback_non_2xx_maps_to_client_error_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-x/feedback"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({ "error": "boom" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_base(server.uri());
        let err = client
            .project_feedback("proj-x")
            .await
            .expect_err("a 500 must be an Err, not a panic");

        match err {
            ClientError::Api { status, body } => {
                assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
                assert!(body.contains("boom"));
            }
            other => panic!("expected ClientError::Api, got {other:?}"),
        }
    }
}
