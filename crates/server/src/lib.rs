//! camerata-server: the Axum BFF the Dioxus cockpit talks to.
//!
//! This is the seam that turns the in-process monolith into a cloud-hostable
//! system: every UI-facing contract is an HTTP endpoint here, so the same server
//! runs locally behind the desktop shell today and in the cloud later. The UI
//! stops calling the backend crates directly and calls this instead.
//!
//! Phase 1 (this module) exposes the cockpit's READ contracts:
//!   - `GET /api/health`  -> liveness.
//!   - `GET /api/rules`   -> the gate's enforced rules (the inspector's data).
//!   - `GET /api/stories` -> the canonical story spine (the left rail).
//!
//! Execution endpoints (run a governed fleet on a story) and a live-status stream
//! land in later phases, behind the same router.

pub mod clarify;
pub mod connections;
pub mod decompose;
pub mod notify;
pub mod onboard;
pub mod live_fleet;
pub mod provider;
pub mod routine;
pub mod run;

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

use camerata_gateway::RULE_REGISTRY;
use camerata_worktracker::{CanonicalStory, ExternalRef, InMemoryStoryStore, StoryStore};

use crate::clarify::{AnswerReq, Clarification, ClarificationStore, PostClarifyReq};
use crate::decompose::{propose, to_story, DecompositionStore, Practice, ProposedChild};
use crate::provider::ProviderHandle;
use crate::routine::{CreateRoutineReq, Routine, RoutineStore, SetEnabledReq};
use crate::run::{execute_run, live_mode_enabled, Run, RunStore};

/// Shared server state. Holds the backend contracts behind trait objects so the
/// in-memory impls used now can be swapped for persistent / cloud impls later
/// without touching the handlers.
#[derive(Clone)]
pub struct AppState {
    stories: Arc<dyn StoryStore>,
    runs: RunStore,
    clarifications: ClarificationStore,
    provider: ProviderHandle,
    decompositions: DecompositionStore,
    routines: RoutineStore,
    notifications: crate::notify::NotificationStore,
}

impl AppState {
    /// Build state from an explicit story store, with the native (in-process)
    /// provider.
    pub fn new(stories: Arc<dyn StoryStore>) -> Self {
        Self {
            stories,
            runs: RunStore::new(),
            clarifications: ClarificationStore::new(),
            provider: ProviderHandle::native(),
            decompositions: DecompositionStore::new(),
            routines: RoutineStore::new(),
            notifications: crate::notify::NotificationStore::new(),
        }
    }

    /// Build state seeded with the representative spine + seeded open clarifications,
    /// native provider. Used by tests and the creds-free demo default.
    pub fn seeded() -> Self {
        let mut state = Self::new(Arc::new(InMemoryStoryStore::seeded()));
        state.clarifications = ClarificationStore::seeded();
        state.routines = RoutineStore::seeded();
        state
    }

    /// The REAL runtime state: a CLEAN SLATE (no seeded stories, clarifications,
    /// or routines) plus the provider selected from the environment (GitHub when
    /// `CAMERATA_GITHUB_TOKEN` is set, native otherwise). This is what `serve`
    /// uses, so the running app starts empty and fills only from real activity
    /// (adopting a tracker story, onboarding a repo) — nothing fake to mislead a
    /// connection test. `seeded()` remains for tests and the canned demos.
    pub fn from_env() -> Self {
        let mut state = Self::new(Arc::new(InMemoryStoryStore::new()));
        state.provider = ProviderHandle::from_env();
        state
    }
}

/// One enforced gate rule, as the cockpit inspector renders it.
#[derive(Debug, Serialize)]
pub struct RuleDto {
    /// The rule id (e.g. `SEC-NO-HARDCODED-SECRETS-1`).
    pub id: String,
    /// The human-readable statement of what the rule denies.
    pub statement: String,
}

/// Build the router for a given state. Separated from [`serve`] so it can be
/// exercised in tests without binding a socket.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/rules", get(rules))
        .route("/api/stories", get(stories))
        .route("/api/stories/:id/run", post(start_run))
        .route("/api/runs/:id", get(get_run))
        .route(
            "/api/stories/:id/clarifications",
            get(list_clarifications).post(post_clarification),
        )
        .route("/api/clarifications/:cid/answer", post(answer_clarification))
        .route("/api/clarifications", get(list_open_clarifications))
        .route("/api/provider", get(provider_info))
        .route("/api/connections", get(connections_status))
        .route("/api/notifications", get(notifications_feed))
        .route("/api/stories/adopt", post(adopt_story))
        .route("/api/onboard/scan", post(onboard_scan))
        .route("/api/stories/:id/decompose", post(decompose_propose))
        .route("/api/stories/:id/decompose/commit", post(decompose_commit))
        .route("/api/stories/:id/children", get(list_children))
        .route("/api/routines", get(list_routines).post(create_routine))
        .route("/api/routines/draft-prompt", post(draft_routine_prompt))
        .route("/api/routines/:id/enable", post(set_routine_enabled))
        .route("/api/routines/:id/run", post(run_routine_now))
        .with_state(state)
}

/// Bind `addr` and serve. The same entry point runs locally and in the cloud. The
/// provider is selected from the environment, so setting the GitHub vars switches the
/// whole BFF onto a real repo with no code change.
pub async fn serve(addr: &str) -> anyhow::Result<()> {
    let state = AppState::from_env();

    // Background event-ingest pollers (tracker events -> notification feed -> UI
    // toasts). Cadences are env-configurable; see crate::notify. Spawned here, not
    // in `router`, so unit tests that build the router don't start background work.
    crate::notify::spawn_tracker_poller(
        state.provider.provider.clone(),
        state.notifications.clone(),
    );
    crate::notify::spawn_deploy_poller(state.notifications.clone());

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("camerata-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

// ── handlers ────────────────────────────────────────────────────────────────

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "camerata-server" }))
}

/// The gate's enforced rules, minus the GOV-1 verification fixture (kept in the
/// registry and the test suite, deliberately not surfaced to the cockpit).
async fn rules() -> Json<Vec<RuleDto>> {
    let dtos = RULE_REGISTRY
        .iter()
        .filter(|e| e.id != "GOV-1")
        .map(|e| RuleDto {
            id: e.id.to_string(),
            statement: e.description.to_string(),
        })
        .collect();
    Json(dtos)
}

/// The canonical story spine.
async fn stories(State(state): State<AppState>) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let list = state.stories.list().await.map_err(AppError)?;
    Ok(Json(list))
}

/// Start a governed run for a story. Returns the run id immediately; the run walks
/// to completion on a background task, driving planted tool calls through the REAL
/// gate (deterministic, token-free). Poll `GET /api/runs/:id` for status + verdicts.
async fn start_run(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    let live = live_mode_enabled();
    let mode = if live { "live" } else { "scripted" };
    let run_id = state.runs.create(&story_id, mode);
    let store = state.runs.clone();
    let rid = run_id.clone();

    if live {
        // Real governed fleet (needs the gateway binary + claude + tokens). Pass the
        // story so the live executor can build a plan from it.
        let (title, desc) = match state.stories.get(&story_id).await {
            Ok(Some(s)) => (s.title, s.description),
            _ => (story_id.clone(), String::new()),
        };
        tokio::spawn(async move { live_fleet::execute_live_run(store, rid, title, desc).await });
    } else {
        // Token-free scripted path: real gate verdicts over planted calls.
        tokio::spawn(async move { execute_run(store, rid).await });
    }

    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id, "mode": mode }))
}

/// The current state of a run: its status plus the real gate verdicts so far.
async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Run>, AppError> {
    state
        .runs
        .get(&id)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("run not found: {id}")))
}

/// All OPEN clarifications across every story (the NEEDS YOU queue).
async fn list_open_clarifications(State(state): State<AppState>) -> Json<Vec<Clarification>> {
    Json(state.clarifications.all_open())
}

/// All clarifications on a story (open and answered).
async fn list_clarifications(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<Vec<Clarification>> {
    Json(state.clarifications.for_story(&story_id))
}

/// Post a clarifying question on a story, addressed to the chosen recipient. When a
/// real tracker is wired AND the story has an external ref, the question is ALSO
/// posted as a comment on the tracker item via the provider (best-effort; the local
/// record is returned regardless so the cockpit never blocks on a remote failure).
async fn post_clarification(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PostClarifyReq>,
) -> Json<Clarification> {
    let clar = state
        .clarifications
        .post(&story_id, &req.question, &req.addressee);

    if state.provider.live {
        if let Ok(Some(story)) = state.stories.get(&story_id).await {
            if let Some(reference) = story.external_ref.as_ref() {
                let questions = [req.question.clone()];
                if let Err(e) = state
                    .provider
                    .provider
                    .post_clarifying_questions(reference, &questions)
                    .await
                {
                    eprintln!("[camerata-server] clarify write-back to tracker failed: {e}");
                }
            }
        }
    }

    Json(clar)
}

/// Record the answer to a clarification.
async fn answer_clarification(
    State(state): State<AppState>,
    Path(cid): Path<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Json<Clarification>, AppError> {
    state
        .clarifications
        .answer(&cid, &req.answer, &req.answered_by)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("clarification not found: {cid}")))
}

/// Which work-tracker provider is active, and whether it is a live external tracker.
async fn provider_info(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "provider": state.provider.label,
        "live": state.provider.live,
    }))
}

/// Connection health for the optional integrations (GitHub, Claude). Probes
/// GitHub reachability when a token is set so a 401/403/5xx surfaces as a real
/// error; integrations being absent is reported as "not configured" (a warning,
/// not an error, in the UI).
async fn connections_status() -> Json<crate::connections::ConnectionsReport> {
    Json(crate::connections::probe().await)
}

/// Request to scan a repo for the brownfield audit.
#[derive(serde::Deserialize)]
struct ScanReq {
    /// `owner/repo` to scan.
    repo: String,
}

/// Brownfield scan: audit an existing repo against the content rules and propose a
/// starter ruleset. Gated on the GitHub token — without it, returns a gated report
/// (no scan) so the UI shows "connect GitHub". The audit reuses the gate's own
/// arms, so it reports exactly what the gate would deny on a new write.
async fn onboard_scan(Json(req): Json<ScanReq>) -> Json<crate::onboard::ScanReport> {
    let Some((owner, repo)) = req.repo.split_once('/') else {
        let mut r = crate::onboard::ScanReport::gated(&req.repo);
        r.message = Some("Repository must be `owner/repo`.".to_string());
        return Json(r);
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(crate::onboard::ScanReport::gated(&req.repo));
    };
    match crate::onboard::scan_repo(owner, repo, &token).await {
        Ok(report) => Json(report),
        Err(e) => {
            let mut r = crate::onboard::ScanReport::gated(&req.repo);
            r.gated = false;
            r.message = Some(format!("Scan failed: {e}"));
            Json(r)
        }
    }
}

/// Query for draining the notification feed: only items newer than `since`.
#[derive(serde::Deserialize)]
struct NotifyQuery {
    #[serde(default)]
    since: u64,
}

/// The notification feed the UI polls (env-configurable cadence) and turns into
/// toasts. Returns items with id > `since` plus the new cursor to send next time.
async fn notifications_feed(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<NotifyQuery>,
) -> Json<serde_json::Value> {
    let (items, cursor) = state.notifications.since(q.since);
    Json(serde_json::json!({ "notifications": items, "cursor": cursor }))
}

/// Request to adopt an external work item by its tracker id.
#[derive(serde::Deserialize)]
struct AdoptReq {
    external_id: String,
    /// Optional source container coordinate (GitHub `owner/repo`, Jira/ADO
    /// project). Lets the BFF adopt from any repo the connection can reach, not
    /// just a default. Omitted falls back to the provider's default container.
    #[serde(default)]
    container: Option<String>,
}

/// Adopt a story from the active tracker into the spine: ingest the work item by id
/// via the provider and upsert it into the `StoryStore`. With the native provider this
/// only succeeds for a seeded id; with GitHub configured it pulls a real issue.
async fn adopt_story(
    State(state): State<AppState>,
    Json(req): Json<AdoptReq>,
) -> Result<Json<CanonicalStory>, AppError> {
    let reference = ExternalRef {
        provider: state.provider.provider.kind(),
        external_id: req.external_id.clone(),
        container: req.container.clone(),
        url: String::new(),
        revision: None,
    };
    let story = state
        .provider
        .provider
        .ingest_story(&reference)
        .await
        .map_err(AppError)?;
    state.stories.upsert(story.clone()).await.map_err(AppError)?;
    Ok(Json(story))
}

/// Propose the component children for a parent story (not yet created). The architect
/// reviews/edits these, then commits.
async fn decompose_propose(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Result<Json<Vec<ProposedChild>>, AppError> {
    let parent = state
        .stories
        .get(&story_id)
        .await
        .map_err(AppError)?
        .ok_or_else(|| AppError(anyhow::anyhow!("story not found: {story_id}")))?;
    Ok(Json(propose(&parent, &Practice::default_feature())))
}

/// The edited set of children to commit.
#[derive(serde::Deserialize)]
struct CommitChildrenReq {
    children: Vec<ProposedChild>,
}

/// Commit the (edited) children: create each as a real story on the spine, linked to
/// the parent. The tracker write-back (as the right work-item type, with parent/child
/// relationship metadata) is the provider phase.
async fn decompose_commit(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<CommitChildrenReq>,
) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let mut created = Vec::new();
    let mut child_ids = Vec::new();
    for pc in &req.children {
        let child = to_story(&story_id, pc);
        state.stories.upsert(child.clone()).await.map_err(AppError)?;
        child_ids.push(child.id.clone());
        created.push(child);
    }
    state.decompositions.record(&story_id, child_ids);
    Ok(Json(created))
}

/// The committed children of a parent story.
async fn list_children(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let mut children = Vec::new();
    for cid in state.decompositions.children_of(&story_id) {
        if let Some(story) = state.stories.get(&cid).await.map_err(AppError)? {
            children.push(story);
        }
    }
    Ok(Json(children))
}

/// All routines.
async fn list_routines(State(state): State<AppState>) -> Json<Vec<Routine>> {
    Json(state.routines.list())
}

/// Create a routine.
async fn create_routine(
    State(state): State<AppState>,
    Json(req): Json<CreateRoutineReq>,
) -> Json<Routine> {
    Json(state.routines.create(&req))
}

/// Draft the operational prompt from the user's intent (ADR
/// routine_authoring_intent_not_prompt). The user describes WHAT they want; this
/// returns the operational prompt for them to review/edit. Today it returns the
/// deterministic scaffold (`authored_by: scaffold`); when Claude is connected the
/// lead-engineer AI authors it for real (`authored_by: claude`) — that hook lives
/// here.
async fn draft_routine_prompt(
    Json(req): Json<crate::routine::DraftPromptReq>,
) -> Json<crate::routine::DraftPromptResp> {
    let prompt = crate::routine::scaffold_prompt(&req.intent, &req.scope);
    Json(crate::routine::DraftPromptResp {
        prompt,
        authored_by: "scaffold".to_string(),
    })
}

/// Enable or disable a routine.
async fn set_routine_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetEnabledReq>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .set_enabled(&id, req.enabled)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("routine not found: {id}")))
}

/// Run a routine now (a governed run via the real gate; records the summary).
async fn run_routine_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .run_now(&id)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("routine not found: {id}")))
}

// ── error type ──────────────────────────────────────────────────────────────

/// Maps any backend error to a 500 with a JSON body, so handlers can use `?`.
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.0.to_string() }));
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

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
        // The four substantive rules, GOV-1 filtered out.
        assert_eq!(arr.len(), 4);
        let ids: Vec<&str> = arr.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"SEC-NO-HARDCODED-SECRETS-1"));
        assert!(ids.contains(&"SEC-NO-PATH-ESCAPE-1"));
        assert!(!ids.contains(&"GOV-1"));
    }

    #[tokio::test]
    async fn stories_endpoint_returns_the_seeded_spine() {
        let app = router(AppState::seeded());
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
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], "CAM-1");
        // FeatureStatus serializes snake_case.
        assert_eq!(arr[0]["status"], "executing");
    }
}
