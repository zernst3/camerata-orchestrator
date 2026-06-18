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

pub mod ai_audit;
pub mod arm;
pub mod draft;
pub mod clarify;
pub mod connections;
pub mod decompose;
pub mod fix;
pub mod jobs;
pub mod notify;
pub mod onboard;
pub mod project;
pub mod reconcile;
pub mod live_fleet;
pub mod llm;
pub mod provider;
pub mod routine;
pub mod run;
pub mod settings;
pub mod suppression;
pub mod transcript;
pub mod workspace;

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
use camerata_worktracker::{
    CanonicalStory, ExternalRef, FeatureStatus, InMemoryStoryStore, RepoTarget, StoryStore,
};

use crate::clarify::{AnswerReq, Clarification, ClarificationStore, PostClarifyReq};
use crate::decompose::{to_story, DecompositionStore, Practice, ProposedChild};
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
    projects: crate::project::ProjectStore,
    settings: crate::settings::SettingsStore,
    transcripts: crate::transcript::TranscriptStore,
    jobs: crate::jobs::JobStore,
    draft: crate::draft::DraftStore,
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
            projects: crate::project::ProjectStore::new(),
            settings: crate::settings::SettingsStore::new(),
            transcripts: crate::transcript::TranscriptStore::new(),
            jobs: crate::jobs::JobStore::new(),
            draft: crate::draft::DraftStore::new(),
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
        // Projects (their configs + pointers, NOT repo contents) persist across
        // launches in the per-user data dir, so an architect's projects survive a
        // restart. Falls back to an in-memory store if the data dir can't be
        // resolved (the app still runs; it just won't persist that session).
        if let Some(data) = dirs::data_dir() {
            let dir = data.join("camerata");
            state.projects = crate::project::ProjectStore::load_or_new(dir.join("projects.json"));
            state.settings = crate::settings::SettingsStore::load_or_new(dir.join("settings.json"));
            state.draft = crate::draft::DraftStore::at(dir.join("onboarding-draft.json"));
        }
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
        .route("/api/runs/:id/agents", get(get_run_agents))
        .route(
            "/api/stories/:id/clarifications",
            get(list_clarifications).post(post_clarification),
        )
        .route("/api/clarifications/:cid/answer", post(answer_clarification))
        .route("/api/clarifications", get(list_open_clarifications))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/import", post(import_project))
        .route("/api/projects/active", get(active_project).post(set_active_project))
        .route("/api/projects/:id/export", get(export_project))
        .route("/api/projects/:id", axum::routing::delete(delete_project))
        .route(
            "/api/projects/:id/ruleset",
            get(export_project_ruleset).post(import_project_ruleset),
        )
        .route("/api/projects/:id/reconcile", get(reconcile_project))
        .route("/api/projects/:id/emit", post(emit_project))
        .route("/api/projects/:id/custom", post(add_custom_rule))
        .route("/api/projects/:id/custom/delete", post(delete_custom_rule))
        .route("/api/provider", get(provider_info))
        .route("/api/connections", get(connections_status))
        .route("/api/notifications", get(notifications_feed))
        .route("/api/stories/adopt", post(adopt_story))
        .route("/api/onboard/scan", post(onboard_scan))
        .route("/api/onboard/audit", post(onboard_audit))
        .route("/api/onboard/audit/start", post(onboard_audit_start))
        .route("/api/onboard/audit/job/:id", get(onboard_audit_job))
        .route("/api/git/detect-repo", post(detect_repo))
        .route("/api/onboard/ticket", post(onboard_ticket))
        .route("/api/onboard/arm", post(onboard_arm))
        .route("/api/onboard/apply", post(onboard_apply))
        .route("/api/onboard/open-pr", post(onboard_open_pr))
        .route("/api/onboard/draft", get(onboard_draft_get).post(onboard_draft_save))
        .route("/api/onboard/draft/clear", post(onboard_draft_clear))
        .route("/api/projects/:id/repo-health", get(project_repo_health))
        .route("/api/repo-path", post(set_repo_path))
        .route("/api/onboard/fix", post(onboard_fix))
        .route("/api/onboard/ci-rules", post(onboard_ci_rules))
        .route("/api/projects/:id/suppressions", get(project_suppressions))
        .route("/api/onboard/ignore", post(onboard_ignore))
        .route("/api/stories/:id/clarify/suggest", post(suggest_clarifications))
        .route("/api/stories/:id/decompose", post(decompose_propose))
        .route("/api/stories/:id/decompose/commit", post(decompose_commit))
        .route("/api/stories/:id/children", get(list_children))
        .route("/api/routines", get(list_routines).post(create_routine))
        .route("/api/routines/draft-prompt", post(draft_routine_prompt))
        .route(
            "/api/routines/:id",
            axum::routing::put(update_routine).delete(delete_routine),
        )
        .route("/api/routines/:id/enable", post(set_routine_enabled))
        .route("/api/routines/:id/run", post(run_routine_now))
        // Local workspace: the user picks a visible folder; project repos clone under
        // it, the fleet edits there, the dev runs/tests locally, then ship pushes + PRs.
        // AI: the model provider seam (CLI locally, Anthropic API in production). The
        // research chat and every AI step call models through this.
        .route("/api/chat", post(chat))
        .route("/api/models", get(list_models))
        .route("/api/settings", get(get_settings))
        .route("/api/settings/workspace", post(set_workspace_root))
        .route(
            "/api/projects/:id/checkout",
            get(checkout_status).post(checkout_project),
        )
        .route("/api/projects/:id/branch", post(checkout_branch))
        .route("/api/projects/:id/ship", post(ship_repo))
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

    // Shutdown hook: on Ctrl+C / SIGTERM, reap any in-flight `claude` audit subprocesses
    // before exiting so a signal-driven quit never orphans them (kill_on_drop only covers
    // graceful runtime shutdown). A hard SIGKILL of the app is uncatchable and not covered.
    tokio::spawn(async {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        #[cfg(unix)]
        let terminate = async {
            if let Ok(mut s) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                s.recv().await;
            }
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();
        tokio::select! {
            () = ctrl_c => {},
            () = terminate => {},
        }
        crate::llm::kill_inflight_claude();
        std::process::exit(0);
    });

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
    let (run_id, mode) = start_governed_run(&state, &story_id).await;
    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id, "mode": mode }))
}

/// Start a governed run for a story through the ONE pipeline every development task
/// uses — live fleet (worktree → gated MCP write → layer-2 checks → bounce) when opted
/// in, the token-free scripted gate otherwise. Returns `(run_id, mode)`. Shared so a
/// brownfield remediation run is governed EXACTLY like any other dev task, not a
/// special path: fixing the audited items is a development task, the first one.
async fn start_governed_run(state: &AppState, story_id: &str) -> (String, &'static str) {
    let live = live_mode_enabled();
    let mode = if live { "live" } else { "scripted" };
    let run_id = state.runs.create(story_id, mode);
    let store = state.runs.clone();
    let rid = run_id.clone();

    if live {
        // Real governed fleet (needs the gateway binary + claude + tokens). Pass the
        // story so the live executor can build a plan from it.
        let (title, desc) = match state.stories.get(story_id).await {
            Ok(Some(s)) => (s.title, s.description),
            _ => (story_id.to_string(), String::new()),
        };
        tokio::spawn(async move { live_fleet::execute_live_run(store, rid, title, desc).await });
    } else {
        // Token-free scripted path: real gate verdicts over planted calls, with the
        // per-agent transcripts (generated prompt + actions + verdicts) populated.
        let transcripts = state.transcripts.clone();
        tokio::spawn(async move { execute_run(store, transcripts, rid).await });
    }
    (run_id, mode)
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

/// The per-agent transcripts for a run: the GENERATED prompt each agent was handed and
/// its output so far. Powers the Agent-activity drawer (the otherwise-hidden prompting).
async fn get_run_agents(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Vec<crate::transcript::AgentTranscript>> {
    Json(state.transcripts.get(&id))
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

// ── Projects ────────────────────────────────────────────────────────────────

async fn list_projects(State(state): State<AppState>) -> Json<Vec<crate::project::Project>> {
    Json(state.projects.list())
}

#[derive(serde::Deserialize)]
struct CreateProjectReq {
    name: String,
    #[serde(default)]
    repos: Vec<String>,
}

async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Json<serde_json::Value> {
    match state.projects.create(&req.name, req.repos) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "could not create project" })),
    }
}

async fn active_project(State(state): State<AppState>) -> Json<Option<crate::project::Project>> {
    Json(state.projects.active())
}

/// Delete a project.
async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": state.projects.delete(&id) }))
}

/// Export a project as a portable JSON document (the full project: name, repos, ruleset)
/// — for backup or moving a project between machines/installs.
async fn export_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::project::Project>, AppError> {
    state
        .projects
        .get(&id)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))
}

/// A project import document (a prior export). `id` in the JSON is ignored — the import
/// always gets a fresh id (new) or preserves the existing id (overwrite).
#[derive(serde::Deserialize)]
struct ImportProjectReq {
    name: String,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    ruleset: crate::project::ProjectRuleset,
    /// Repos already onboarded in the source project — travels with the export so
    /// onboarding state is preserved across machines.
    #[serde(default)]
    onboarded: Vec<String>,
    /// When `false` (the default) a name collision returns `conflict: true` so the UI
    /// can ask before overwriting. Pass `true` to overwrite in place (same id, same
    /// name, replaced repos/ruleset/onboarded).
    #[serde(default)]
    overwrite: bool,
}

/// Import a project from an exported JSON, make it active, and return it.
///
/// Conflict response (HTTP 200):
/// `{ "ok": false, "conflict": true, "name": "…", "message": "…" }`
///
/// Success response (HTTP 200):
/// `{ "ok": true, "project": {…}, "overwritten": <bool> }`
async fn import_project(
    State(state): State<AppState>,
    Json(req): Json<ImportProjectReq>,
) -> Json<serde_json::Value> {
    use crate::project::ImportOutcome;
    let name = req.name.clone();
    match state
        .projects
        .import_or_overwrite(&req.name, req.repos, req.ruleset, req.onboarded, req.overwrite)
    {
        Some(ImportOutcome::Created(p)) => {
            Json(serde_json::json!({ "ok": true, "project": p, "overwritten": false }))
        }
        Some(ImportOutcome::Overwritten(p)) => {
            Json(serde_json::json!({ "ok": true, "project": p, "overwritten": true }))
        }
        Some(ImportOutcome::Conflict) => Json(serde_json::json!({
            "ok": false,
            "conflict": true,
            "name": name,
            "message": format!("A project named \"{name}\" already exists. Importing will overwrite it."),
        })),
        None => Json(serde_json::json!({ "ok": false, "message": "could not import project" })),
    }
}

#[derive(serde::Deserialize)]
struct SetActiveReq {
    id: String,
}

async fn set_active_project(
    State(state): State<AppState>,
    Json(req): Json<SetActiveReq>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": state.projects.set_active(&req.id) }))
}

/// Export the project's ruleset as JSON (the portable source of truth).
async fn export_project_ruleset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match state.projects.get(&id) {
        Some(p) => Json(serde_json::json!({ "ok": true, "ruleset": p.ruleset })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Import a ruleset: upsert the BASE rules (selections / cross-repo / process)
/// while PRESERVING the project's existing custom rules.
async fn import_project_ruleset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(incoming): Json<crate::project::ProjectRuleset>,
) -> Json<serde_json::Value> {
    let updated = state.projects.update(&id, |p| {
        p.upsert_base_rules(
            incoming.selections.clone(),
            incoming.cross_repo.clone(),
            incoming.process.clone(),
        );
        // Merge imported custom rules by name (imported wins), never dropping
        // existing ones that aren't in the import.
        p.merge_custom(&incoming.custom);
    });
    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Add or edit (by name) a custom rule on a project. An existing name is replaced
/// (an explicit edit); a new name is added. Never drops other custom rules.
#[derive(serde::Deserialize)]
struct CustomRuleReq {
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    domain: String,
}

async fn add_custom_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CustomRuleReq>,
) -> Json<serde_json::Value> {
    if req.name.trim().is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "name is required" }));
    }
    let rule = crate::project::CustomRule {
        name: req.name.trim().to_string(),
        body: req.body,
        domain: if req.domain.trim().is_empty() {
            "*".to_string()
        } else {
            req.domain.trim().to_string()
        },
    };
    match state.projects.update(&id, |p| p.merge_custom(std::slice::from_ref(&rule))) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Explicitly delete a custom rule by name (the ONLY way a custom rule leaves a
/// project).
#[derive(serde::Deserialize)]
struct DeleteCustomReq {
    name: String,
}

async fn delete_custom_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DeleteCustomReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.remove_custom(&req.name);
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Reconcile a project's repos with the rule-bank: read each repo's emitted gate
/// config (ground truth of what's applied) and rehydrate the full source rule
/// (alternatives + context) by id. Gated on the token (reads the repos). Returns
/// the applied rules with their source rehydrated.
async fn reconcile_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to reconcile." }));
    };
    let applied = crate::reconcile::reconcile_repos(&project.repos, &token).await;
    Json(serde_json::json!({ "ok": true, "applied": applied }))
}

/// Connection health for the optional integrations (GitHub, Claude). Probes
/// GitHub reachability when a token is set so a 401/403/5xx surfaces as a real
/// error; integrations being absent is reported as "not configured" (a warning,
/// not an error, in the UI).
async fn connections_status() -> Json<crate::connections::ConnectionsReport> {
    Json(crate::connections::probe().await)
}

/// Request to scan repo(s) for the brownfield audit. Accepts a SET of repos (a
/// brownfield onboarding spans inter-related repos); a single `repo` is also
/// accepted for convenience.
#[derive(serde::Deserialize)]
struct ScanReq {
    /// `owner/repo` entries to scan together.
    #[serde(default)]
    repos: Vec<String>,
    /// Convenience single repo (folded into `repos`).
    #[serde(default)]
    repo: Option<String>,
}

/// Brownfield scan: audit a SET of existing repos against the content rules and
/// propose a starter ruleset, aggregating across all of them. Gated on the GitHub
/// token — without it, returns a gated report (no scan) so the UI shows "connect
/// GitHub". The audit reuses the gate's own arms, so it reports exactly what the
/// gate would deny on a new write.
async fn onboard_scan(Json(req): Json<ScanReq>) -> Json<crate::onboard::ScanReport> {
    let mut repos = req.repos;
    if let Some(r) = req.repo {
        repos.push(r);
    }
    repos.retain(|r| !r.trim().is_empty());
    if repos.is_empty() {
        let mut r = crate::onboard::ScanReport::gated(&repos);
        r.gated = false;
        r.message = Some("Name at least one `owner/repo` to scan.".to_string());
        return Json(r);
    }

    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(crate::onboard::ScanReport::gated(&repos));
    };
    Json(crate::onboard::scan_repos(&repos, &token).await)
}

/// One selected rule the Phase-2 audit runs against, with its per-repo binding. An empty
/// `repos` means PROJECT-LEVEL (scanned against every repo); a non-empty `repos` scopes the
/// rule to just those repos. The UI sends each repo's selections plus the project-level set,
/// and the audit scopes each repo to the rules that apply to it.
#[derive(serde::Deserialize)]
struct AuditRuleReq {
    #[serde(default)]
    id: String,
    #[serde(default)]
    directive: String,
    /// Repos this rule applies to. Empty = project-level (all repos). Omitted by older
    /// single-repo callers, which correctly reads as "applies to the one repo scanned".
    #[serde(default)]
    repos: Vec<String>,
}

#[derive(serde::Deserialize)]
struct AuditReq {
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    rules: Vec<AuditRuleReq>,
    /// The model the USER picked for this audit (company-agnostic id, e.g.
    /// `claude-sonnet-4-6`). None / empty → the server default. The user owns the
    /// thoroughness-vs-speed trade-off by choosing here.
    #[serde(default)]
    model: Option<String>,
    /// The model the user picked for the CALIBRATION pass (severity recalibration +
    /// confidence tagging). Its own knob so a customer can run a cheap scan with a stronger
    /// verify (or keep it end-to-end). None / empty → falls back to the scan model.
    #[serde(default)]
    calibration_model: Option<String>,
    /// The execution mode: `parallel` (default) or `sequential`. Speed/scale knob,
    /// orthogonal to model + rules.
    #[serde(default)]
    mode: Option<String>,
}

/// The transcript key the scan/audit AI activity registers under (the Agent-activity
/// drawer polls `/api/runs/scan-audit/agents`).
const SCAN_AUDIT_KEY: &str = "scan-audit";

/// Phase 2 — audit the repos AGAINST the selected rules (the deterministic security floor
/// plus the AI audit parameterized by the chosen rules). Returns the findings report. The
/// AI activity (prompts and output) registers into the transcript store so the UI can
/// show, live, that the model is actually working.
async fn onboard_audit(
    State(state): State<AppState>,
    Json(req): Json<AuditReq>,
) -> Json<crate::onboard::ScanReport> {
    let repos: Vec<String> = req
        .repos
        .into_iter()
        .filter(|r| !r.trim().is_empty())
        .collect();
    if repos.is_empty() {
        let mut r = crate::onboard::ScanReport::gated(&repos);
        r.gated = false;
        r.message = Some("No repos to audit.".to_string());
        return Json(r);
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(crate::onboard::ScanReport::gated(&repos));
    };
    let selected: Vec<crate::onboard::SelectedRule> = req
        .rules
        .into_iter()
        .filter(|r| !r.id.trim().is_empty())
        .map(|r| crate::onboard::SelectedRule { id: r.id, directive: r.directive, repos: r.repos })
        .collect();
    let model = req.model.filter(|m| !m.trim().is_empty());
    let calibration_model = req.calibration_model.filter(|m| !m.trim().is_empty());
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    // Fresh transcript for this audit run so the live feedback panel starts clean.
    state.transcripts.clear(SCAN_AUDIT_KEY);
    Json(
        crate::onboard::audit_repos(
            &repos,
            &selected,
            &token,
            model.as_deref(),
            calibration_model.as_deref(),
            mode,
            Some((&state.transcripts, SCAN_AUDIT_KEY)),
            None,
        )
        .await,
    )
}

/// Mode 3 — START an async audit JOB. Spawns the same audit in the background and returns a
/// job id IMMEDIATELY, so the request never blocks for the (possibly many-minute) run. The
/// UI polls `GET /api/onboard/audit/job/:id` for progress + incremental findings, then the
/// final report. The work is decoupled from this request — it survives a dropped poll.
async fn onboard_audit_start(
    State(state): State<AppState>,
    Json(req): Json<AuditReq>,
) -> Json<serde_json::Value> {
    let repos: Vec<String> = req
        .repos
        .into_iter()
        .filter(|r| !r.trim().is_empty())
        .collect();
    let selected: Vec<crate::onboard::SelectedRule> = req
        .rules
        .into_iter()
        .filter(|r| !r.id.trim().is_empty())
        .map(|r| crate::onboard::SelectedRule { id: r.id, directive: r.directive, repos: r.repos })
        .collect();
    let model = req.model.filter(|m| !m.trim().is_empty());
    let calibration_model = req.calibration_model.filter(|m| !m.trim().is_empty());
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());

    let job_id = state.jobs.create();
    state.transcripts.clear(SCAN_AUDIT_KEY);

    let jobs = state.jobs.clone();
    let transcripts = state.transcripts.clone();
    let jid = job_id.clone();
    tokio::spawn(async move {
        let Some(token) = token else {
            jobs.fail(&jid, "No GitHub token connected (set CAMERATA_GITHUB_TOKEN).");
            return;
        };
        if repos.is_empty() {
            jobs.fail(&jid, "No repos to audit.");
            return;
        }
        let report = crate::onboard::audit_repos(
            &repos,
            &selected,
            &token,
            model.as_deref(),
            calibration_model.as_deref(),
            mode,
            Some((&transcripts, SCAN_AUDIT_KEY)),
            Some((&jobs, &jid)),
        )
        .await;
        jobs.finish(&jid, report);
    });

    Json(serde_json::json!({ "job_id": job_id }))
}

/// Poll an async audit job: status, progress (done/total passes), incremental findings, and
/// the final report once done. `null` for an unknown id.
async fn onboard_audit_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Option<crate::jobs::JobState>> {
    Json(state.jobs.get(&id))
}

#[derive(serde::Deserialize)]
struct DetectRepoReq {
    path: String,
}

/// Derive `owner/repo` from a LOCAL git checkout's origin remote — so the UI can let a
/// developer navigate to a repo folder instead of typing the identifier.
async fn detect_repo(Json(req): Json<DetectRepoReq>) -> Json<serde_json::Value> {
    match crate::workspace::detect_remote_repo(std::path::Path::new(&req.path)).await {
        Ok(repo) => Json(serde_json::json!({ "ok": true, "repo": repo })),
        Err(message) => Json(serde_json::json!({ "ok": false, "message": message })),
    }
}

/// Request to file accepted findings as a tech-debt ticket.
#[derive(serde::Deserialize)]
struct TicketReq {
    /// `owner/repo` to file the issue in.
    repo: String,
    #[serde(default)]
    title: Option<String>,
    /// The selected findings to record.
    findings: Vec<crate::onboard::Finding>,
}

/// Accept selected findings as tech debt: open a GitHub issue with them. Gated on
/// the token (needs Issues write). Returns `{ ok, url, message }`.
async fn onboard_ticket(Json(req): Json<TicketReq>) -> Json<serde_json::Value> {
    let Some((owner, repo)) = req.repo.split_once('/') else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Target repo must be `owner/repo`." }),
        );
    };
    if req.findings.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No findings selected." }));
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to file a ticket." }));
    };
    let title = req.title.unwrap_or_else(|| {
        format!("Tech debt: {} audit finding(s) accepted", req.findings.len())
    });
    match crate::onboard::create_tech_debt_ticket(owner, repo, &token, &title, &req.findings).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Request to arm a set of repos with the approved (resolved) rules.
#[derive(serde::Deserialize)]
struct ArmReq {
    /// Fully-resolved rules (each with its chosen directive + which repos it goes to).
    rules: Vec<crate::arm::ArmRule>,
    /// The current findings to snapshot as the baseline (accepted pre-existing debt),
    /// so the team is unblocked at onboarding and the gate enforces only on new code.
    #[serde(default)]
    findings: Vec<ArmFinding>,
}

/// A finding the UI sends to arm, to be snapshotted into the per-repo baseline.
#[derive(serde::Deserialize)]
struct ArmFinding {
    #[serde(default)]
    repo: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    rule_id: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    status: String,
}

/// Build the per-repo `.camerata/baseline.json` contents from the armed findings:
/// snapshot the currently-enforced (active) violations as accepted pre-existing debt,
/// fingerprinted so the ratchet works. Returns `repo -> baseline JSON`.
fn baselines_from_findings(
    findings: &[ArmFinding],
    accepted_by: &str,
) -> std::collections::HashMap<String, String> {
    use crate::suppression::{baseline_entry, Baseline, FindingRef};
    let now = chrono::Utc::now().to_rfc3339();
    let mut by_repo: std::collections::HashMap<String, Baseline> = std::collections::HashMap::new();
    for f in findings {
        // Only snapshot real, currently-enforced violations — not the require-reason
        // meta-finding, not already-suppressed ones.
        if f.status != "active" || f.rule_id == crate::suppression::REASONLESS_RULE_ID {
            continue;
        }
        let fr = FindingRef {
            rule_id: f.rule_id.clone(),
            path: f.path.clone(),
            line: 0,
            snippet: f.snippet.clone(),
        };
        let entry = baseline_entry(&fr, accepted_by, &now, "pre-existing at onboarding");
        by_repo.entry(f.repo.clone()).or_default().entries.push(entry);
    }
    by_repo
        .into_iter()
        .filter_map(|(repo, b)| {
            serde_json::to_string_pretty(&b)
                .ok()
                .map(|json| (repo, json))
        })
        .collect()
}

/// Arm: install the approved ruleset into each repo via a governance PR (AGENTS.md
/// / CONVENTIONS.md / gate config), per the camerata-ai emit format. Gated on the
/// token (needs Contents + PR write). Returns a per-repo result list.
async fn onboard_arm(
    State(state): State<AppState>,
    Json(req): Json<ArmReq>,
) -> Json<serde_json::Value> {
    if req.rules.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No rules selected to arm." }));
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to arm." }));
    };

    // Save the armed ruleset to the active project (create one if none) so the
    // project is the source of truth and re-emit works.
    save_armed_to_project(&state, &req.rules);

    // Only repo-local rules emit into repo files; cross-repo + process rules are
    // project-level (the gates read them from the project store).
    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .cloned()
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();
    let custom = state.projects.active().map(|p| p.ruleset.custom).unwrap_or_default();

    // Snapshot the current violations as the baseline (accepted pre-existing debt), so
    // the team is unblocked at onboarding and the gate enforces only on new code.
    let baselines = baselines_from_findings(&req.findings, "architect");

    let results = emit_to_repos(&repos, &repo_local, &custom, &baselines, &token).await;
    Json(serde_json::json!({ "ok": true, "results": results }))
}

/// Apply: write the approved ruleset onto a governance branch in each repo's LOCAL clone AND
/// push that branch to origin — WITHOUT opening a PR. The architect can edit the working copy
/// freely, then open the PR as a separate step (`onboard_open_pr`). Needs a workspace folder
/// set + a token with Contents write. The branch lands BOTH locally and on origin.
async fn onboard_apply(
    State(state): State<AppState>,
    Json(req): Json<ArmReq>,
) -> Json<serde_json::Value> {
    if req.rules.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No rules selected to apply." }));
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to apply (the branch is pushed to origin)." }));
    };
    let Some(root) = state.settings.workspace_root() else {
        return Json(serde_json::json!({ "ok": false, "message": "Set a local workspace folder first (Settings) — Apply writes into the repo's local clone." }));
    };
    let root = std::path::PathBuf::from(root);

    // Source of truth: save the armed ruleset to the active project (create one if none).
    save_armed_to_project(&state, &req.rules);

    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .cloned()
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();
    let custom = state.projects.active().map(|p| p.ruleset.custom).unwrap_or_default();
    let baselines = baselines_from_findings(&req.findings, "architect");

    let mut results = Vec::new();
    let mut applied: Vec<String> = Vec::new();
    for repo in &repos {
        let repo_rules: Vec<&crate::arm::ArmRule> =
            repo_local.iter().filter(|r| r.repos.iter().any(|x| x == repo)).collect();
        let repo_custom: Vec<&crate::project::CustomRule> = custom
            .iter()
            .filter(|c| c.domain.trim().is_empty() || c.domain.trim() == "*" || &c.domain == repo)
            .collect();
        if repo_rules.is_empty() && repo_custom.is_empty() {
            continue;
        }
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        if let Some(baseline_json) = baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        let msg = format!("chore(governance): apply Camerata ruleset to {repo}");
        match crate::workspace::apply_local_and_push(
            &root, repo, crate::arm::ARM_BRANCH, &files, &msg, &token,
        )
        .await
        {
            Ok(path) => {
                applied.push(repo.clone());
                results.push(serde_json::json!({
                    "repo": repo, "ok": true, "branch": crate::arm::ARM_BRANCH, "path": path
                }));
            }
            Err(e) => results.push(serde_json::json!({
                "repo": repo, "ok": false, "message": format!("{e}")
            })),
        }
    }
    // Applying the ruleset IS the completion act: mark the successfully-applied repos
    // onboarded on the active project (no audit required — onboarding never gates on the
    // optional violation scan). save_armed_to_project above guarantees an active project.
    if !applied.is_empty() {
        if let Some(active) = state.projects.active() {
            state.projects.update(&active.id, |p| p.mark_onboarded(&applied));
        }
    }
    Json(serde_json::json!({ "ok": true, "branch": crate::arm::ARM_BRANCH, "onboarded": applied, "results": results }))
}

/// Per-repo local-path resolution for a project (issue #33): for each repo, is there a local
/// folder, is it a git checkout, and does its origin match? Drives the broken-path health
/// banner + per-repo icons + resolve action. Continuous (the UI calls it on load + after
/// import + after a resolve).
async fn project_repo_health(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "unknown project", "repos": [] }));
    };
    let workspace_root = state.settings.workspace_root();
    let mut out = Vec::new();
    for repo in &project.repos {
        let override_path = state.settings.repo_path(repo);
        let res = crate::workspace::repo_resolution(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        )
        .await;
        out.push(res);
    }
    Json(serde_json::json!({ "ok": true, "repos": out }))
}

#[derive(serde::Deserialize)]
struct RepoPathReq {
    repo: String,
    /// The chosen local folder; empty clears the override.
    #[serde(default)]
    path: String,
}

/// Set (or clear) a repo's machine-local path override, then re-validate it (issue #33).
async fn set_repo_path(
    State(state): State<AppState>,
    Json(req): Json<RepoPathReq>,
) -> Json<serde_json::Value> {
    let path = if req.path.trim().is_empty() { None } else { Some(req.path.clone()) };
    state.settings.set_repo_path(&req.repo, path);
    // Re-resolve so the UI gets the post-set status without a second round trip.
    let workspace_root = state.settings.workspace_root();
    let override_path = state.settings.repo_path(&req.repo);
    let res = crate::workspace::repo_resolution(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &req.repo,
    )
    .await;
    Json(serde_json::json!({ "ok": true, "resolution": res }))
}

/// Load the saved onboarding draft (scan + audit + selections + dispositions), or `null`.
async fn onboard_draft_get(State(state): State<AppState>) -> Json<Option<serde_json::Value>> {
    Json(state.draft.load())
}

/// Save/replace the current onboarding draft (opaque blob; the UI owns its shape).
async fn onboard_draft_save(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    state.draft.save(body);
    Json(serde_json::json!({ "ok": true }))
}

/// Drop the onboarding draft (completed, or starting fresh).
async fn onboard_draft_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.draft.clear();
    Json(serde_json::json!({ "ok": true }))
}

#[derive(serde::Deserialize)]
struct OpenPrReq {
    #[serde(default)]
    repos: Vec<String>,
}

/// Open the governance PR from the already-applied + pushed branch (the explicit, separate
/// step after `onboard_apply`). One PR per repo into its default branch.
async fn onboard_open_pr(
    State(_state): State<AppState>,
    Json(req): Json<OpenPrReq>,
) -> Json<serde_json::Value> {
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to open the PR." }));
    };
    let mut repos: Vec<String> = req.repos.into_iter().filter(|r| !r.trim().is_empty()).collect();
    repos.sort();
    repos.dedup();
    if repos.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "No repos to open a PR for." }));
    }
    let title = "Camerata governance: adopt the selected ruleset";
    let body = "Adopts the Camerata-selected ruleset for this repo (AGENTS.md / CONVENTIONS.md / \
        CI gate / baseline). Applied locally and pushed by Camerata onboarding; opened as a PR \
        on request.";
    let mut results = Vec::new();
    for repo in &repos {
        match crate::workspace::open_branch_pr(repo, crate::arm::ARM_BRANCH, title, body, &token)
            .await
        {
            Ok(url) => results.push(serde_json::json!({ "repo": repo, "ok": true, "url": url })),
            Err(e) => {
                results.push(serde_json::json!({ "repo": repo, "ok": false, "message": format!("{e}") }))
            }
        }
    }
    Json(serde_json::json!({ "ok": true, "results": results }))
}

#[derive(serde::Deserialize)]
struct IgnoreReq {
    repo: String,
    findings: Vec<FixFinding>,
    /// Mandatory justification — a reason-less suppression is rejected (the invariant).
    reason: String,
    #[serde(default)]
    ticket: Option<String>,
}

/// Fetch and parse a repo's committed `.camerata/baseline.json` (default branch), or an
/// empty baseline if absent.
async fn fetch_baseline(
    owner: &str,
    repo: &str,
    token: &str,
) -> crate::suppression::Baseline {
    use base64::Engine as _;
    use camerata_worktracker::{HttpTransport, ReqwestTransport};
    let Ok(transport) = ReqwestTransport::new(format!("Bearer {token}")) else {
        return crate::suppression::Baseline::default();
    };
    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/.camerata/baseline.json");
    let Ok(resp) = transport.get(&url).await else {
        return crate::suppression::Baseline::default();
    };
    if !(200..300).contains(&resp.status) {
        return crate::suppression::Baseline::default();
    }
    let decoded = serde_json::from_str::<serde_json::Value>(&resp.body)
        .ok()
        .and_then(|v| v["content"].as_str().map(|s| s.replace('\n', "")))
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());
    decoded
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Durable, auditable IGNORE: append the selected findings to the repo's central
/// baseline as reasoned suppressions (reason mandatory; who/when stamped; optional
/// ticket tie-back), and open a governed PR. NOT a one-time dismissal — it persists,
/// shows in the diff, and rolls up into the audit registry.
async fn onboard_ignore(
    State(_state): State<AppState>,
    Json(req): Json<IgnoreReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.reason.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "a reason is required to ignore a finding"
        )));
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!("connect GitHub to record an ignore")));
    }
    let (owner, name) = req
        .repo
        .split_once('/')
        .ok_or_else(|| AppError(anyhow::anyhow!("repo must be owner/repo")))?;

    let mut baseline = fetch_baseline(owner, name, &token).await;
    let now = chrono::Utc::now().to_rfc3339();
    for f in &req.findings {
        let fr = crate::suppression::FindingRef {
            rule_id: f.rule_id.clone(),
            path: f.path.clone(),
            line: 0,
            snippet: f.snippet.clone(),
        };
        let mut entry = crate::suppression::baseline_entry(&fr, "architect", &now, req.reason.trim());
        entry.kind = "ignore".to_string();
        entry.ticket = req.ticket.clone();
        baseline.entries.push(entry);
    }
    let json = serde_json::to_string_pretty(&baseline)
        .map_err(|e| AppError(anyhow::anyhow!("serialize baseline: {e}")))?;
    let url = crate::arm::arm_repo(
        owner,
        name,
        &token,
        &[(".camerata/baseline.json".to_string(), json)],
    )
    .await?;
    Ok(Json(serde_json::json!({ "ok": true, "url": url, "ignored": req.findings.len() })))
}

/// The central suppression registry for a project: every inline waiver + baseline
/// entry across its repos, with stale flags. The auditable "everything we've waived" view.
async fn project_suppressions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::suppression::SuppressionRecord>>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))?;
    let token = std::env::var("CAMERATA_GITHUB_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Ok(Json(Vec::new()));
    }
    Ok(Json(
        crate::onboard::suppression_registry(&project.repos, &token).await,
    ))
}

/// One audited finding to remediate (the subset the UI sends to the fix run).
#[derive(serde::Deserialize)]
struct FixFinding {
    #[serde(default)]
    rule_id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: usize,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    snippet: String,
}

#[derive(serde::Deserialize)]
struct FixReq {
    repo: String,
    findings: Vec<FixFinding>,
}

#[derive(serde::Deserialize)]
struct CiRulesReq {
    repo: String,
}

/// Wire the mechanical (CI-tier) governance rules into a repo's CI — as a GOVERNED
/// DEVELOPMENT TASK. Arming emits `.camerata/ci-checks.json` (the declared mechanical
/// rules) but a config doesn't enforce itself; turning each declared check into a real
/// CI mechanism (an ESLint rule, a migration/index audit, an AST lint) is development
/// work. So it runs through the SAME governed pipeline as any dev task: the agent reads
/// the declared checks, sees which are already enforced in CI, and implements the rest,
/// every write passing the gate.
async fn onboard_ci_rules(
    State(state): State<AppState>,
    Json(req): Json<CiRulesReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let spec = format!(
        "Wire Camerata's mechanical governance rules into {}'s CI so a pull request that \
         violates one FAILS the build. This is development work, governed: every write \
         passes the deny-before-execute gate.\n\n\
         The rules to enforce are declared in `.camerata/ci-checks.json` (id, title, \
         directive, conformance). For EACH rule:\n\
         1. Check whether it is ALREADY enforced in CI (inspect `.github/workflows/`, the \
         linter config, package scripts).\n\
         2. If it is, leave it.\n\
         3. If not, implement the enforcement using THIS repo's stack — e.g. an ESLint \
         `no-restricted-syntax`/`no-restricted-imports` rule, a migration/index audit \
         step, or an AST lint — following the rule's `conformance` description, and wire it \
         into the `camerata-governance` workflow so it runs on every PR.\n\n\
         Do not weaken or delete existing checks. Add a focused, minimal enforcement per \
         rule.",
        req.repo
    );
    let slug: String = req
        .repo
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let story_id = format!("ci-{slug}");
    let story = CanonicalStory {
        id: story_id.clone(),
        external_ref: None,
        title: format!("Wire CI governance — {}", req.repo),
        description: spec,
        status: FeatureStatus::Intake,
        created_by: "architect".to_string(),
        targets: vec![RepoTarget::new(&req.repo)],
    };
    state.stories.upsert(story).await.map_err(AppError)?;
    let (run_id, mode) = start_governed_run(&state, &story_id).await;
    Ok(Json(serde_json::json!({
        "story_id": story_id,
        "run_id": run_id,
        "mode": mode,
    })))
}

/// Fix the audited items — as a GOVERNED DEVELOPMENT TASK, not a special path.
///
/// This is the architectural point: remediating the violations a brownfield audit
/// found IS a development task (the first one, usually), so it must run through the
/// EXACT same pipeline every other dev task uses — the worktree, the deny-before-execute
/// gate, the layer-2 post-task checks, the bounce-on-fail loop. The only brownfield-
/// specific step is that ARM first installs the rules + CI gate the fix is then held to.
/// Here we turn the findings into a remediation story (its spec) and start a governed
/// run on it via `start_governed_run` — the same call `start_run` makes. It won't earn
/// its keep unless the fix is gated identically to normal development.
async fn onboard_fix(
    State(state): State<AppState>,
    Json(req): Json<FixReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.findings.is_empty() {
        return Err(AppError(anyhow::anyhow!("no findings to fix")));
    }
    // The remediation spec: the audited violations, as the task description the
    // governed fleet works from.
    let mut spec = format!(
        "Remediate the violations Camerata's audit found in {}. Fix each WITHOUT \
         introducing new violations — every write passes the governance gate.\n\n\
         Findings:\n",
        req.repo
    );
    for (i, f) in req.findings.iter().enumerate() {
        spec.push_str(&format!(
            "{}. [{}] {}:{} — {}{}\n",
            i + 1,
            f.rule_id,
            f.path,
            f.line,
            if f.detail.is_empty() { &f.snippet } else { &f.detail },
            if f.snippet.is_empty() || f.detail.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", f.snippet)
            }
        ));
    }

    // A stable-ish remediation story id per repo, so re-running updates the same story.
    let slug: String = req
        .repo
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let story_id = format!("fix-{slug}");
    let story = CanonicalStory {
        id: story_id.clone(),
        external_ref: None,
        title: format!("Remediate audited violations — {}", req.repo),
        description: spec,
        status: FeatureStatus::Intake,
        created_by: "architect".to_string(),
        targets: vec![RepoTarget::new(&req.repo)],
    };
    state.stories.upsert(story).await.map_err(AppError)?;

    // Start a governed run through the SAME pipeline as any development task.
    let (run_id, mode) = start_governed_run(&state, &story_id).await;
    Ok(Json(serde_json::json!({
        "story_id": story_id,
        "run_id": run_id,
        "mode": mode,
        "findings": req.findings.len(),
    })))
}

/// Classify the armed rules by scope and save them to the active project (creating
/// one from the rules' repos if none exists). This is the upsert: it replaces the
/// project's BASE rules (selections / cross-repo / process) and leaves custom rules
/// untouched.
fn save_armed_to_project(state: &AppState, rules: &[crate::arm::ArmRule]) {
    use crate::project::RuleSelection;
    let mut selections = Vec::new();
    let mut cross = Vec::new();
    let mut process = Vec::new();
    let mut all_repos = std::collections::BTreeSet::new();
    for r in rules {
        let s = RuleSelection {
            rule_id: r.id.clone(),
            chosen_option: r.option.clone(),
            repos: r.repos.clone(),
        };
        for repo in &r.repos {
            all_repos.insert(repo.clone());
        }
        match r.scope.as_str() {
            "cross-repo" => cross.push(s),
            "process" => process.push(s),
            _ => selections.push(s),
        }
    }
    let pid = match state.projects.active() {
        Some(p) => p.id,
        None => match state.projects.create("My project", all_repos.iter().cloned().collect()) {
            Some(p) => p.id,
            None => return,
        },
    };
    state.projects.update(&pid, |p| {
        p.upsert_base_rules(selections, cross, process);
        for repo in &all_repos {
            if !p.repos.contains(repo) {
                p.repos.push(repo.clone());
            }
        }
    });
}

/// Emit the repo-local `rules` + the `custom` rules into each repo in `repos`, one
/// governance PR per repo. Shared by initial arm and re-emit. Each repo receives the
/// rules bound to it plus the domain-matching custom rules.
async fn emit_to_repos(
    repos: &[String],
    rules: &[crate::arm::ArmRule],
    custom: &[crate::project::CustomRule],
    baselines: &std::collections::HashMap<String, String>,
    token: &str,
) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    for repo in repos {
        let Some((owner, name)) = repo.split_once('/') else {
            results.push(serde_json::json!({ "repo": repo, "ok": false, "message": "not owner/repo" }));
            continue;
        };
        let repo_rules: Vec<&crate::arm::ArmRule> =
            rules.iter().filter(|r| r.repos.iter().any(|x| x == repo)).collect();
        let repo_custom: Vec<&crate::project::CustomRule> = custom
            .iter()
            .filter(|c| c.domain.trim().is_empty() || c.domain.trim() == "*" || &c.domain == repo)
            .collect();
        if repo_rules.is_empty() && repo_custom.is_empty() {
            continue;
        }
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        // Include this repo's baseline (accepted pre-existing debt) in the same PR, so
        // the gate it installs enforces only on new code from day one.
        if let Some(baseline_json) = baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        match crate::arm::arm_repo(owner, name, token, &files).await {
            Ok(url) => results.push(serde_json::json!({ "repo": repo, "ok": true, "url": url })),
            Err(e) => {
                results.push(serde_json::json!({ "repo": repo, "ok": false, "message": format!("{e}") }))
            }
        }
    }
    results
}

/// Resolve a project's base selections into emittable rules: the directive comes
/// from the corpus rule's chosen alternative (or default), or the gateway content
/// rule's description. The rule-bank is the source; the project stores only the
/// selection (id + chosen option + repos).
async fn resolve_project_arm_rules(project: &crate::project::Project) -> Vec<crate::arm::ArmRule> {
    let corpus_path = camerata_rules::corpus_path();
    let set = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };
    let mut out = Vec::new();
    for sel in &project.ruleset.selections {
        if let Some(rule) = set.as_ref().and_then(|s| s.get_by_id(&sel.rule_id)) {
            let directive = sel
                .chosen_option
                .as_ref()
                .and_then(|oid| rule.options.iter().find(|o| &o.id == oid))
                .or_else(|| {
                    rule.default_option
                        .as_ref()
                        .and_then(|d| rule.options.iter().find(|o| &o.id == d))
                })
                .map(|o| o.directive.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| rule.summary.clone());
            let enforcement = match rule.enforcement {
                camerata_rules::EnforcementKind::Prose => "prose",
                camerata_rules::EnforcementKind::Structured => "structured",
                camerata_rules::EnforcementKind::Mechanical => "mechanical",
            };
            out.push(crate::arm::ArmRule {
                id: sel.rule_id.clone(),
                title: rule.title.clone(),
                directive,
                option: sel.chosen_option.clone(),
                enforcement: enforcement.to_string(),
                scope: "repo-local".to_string(),
                conformance: None,
                repos: sel.repos.clone(),
            });
            continue;
        }
        // A gateway content rule (its description is the directive), else a minimal
        // emit using the id (drift — applied but no rich source).
        let (title, directive) = camerata_gateway::RULE_REGISTRY
            .iter()
            .find(|e| e.id == sel.rule_id)
            .map(|e| (e.description.to_string(), e.description.to_string()))
            .unwrap_or_else(|| (sel.rule_id.clone(), sel.rule_id.clone()));
        out.push(crate::arm::ArmRule {
            id: sel.rule_id.clone(),
            title,
            directive,
            option: sel.chosen_option.clone(),
            enforcement: "mechanical".to_string(),
            scope: "repo-local".to_string(),
            conformance: None,
            repos: sel.repos.clone(),
        });
    }
    out
}

/// Re-emit a project's ruleset (the source of truth) into its repos: rebuild the
/// emit from the project's base selections + custom rules and open a PR per repo.
/// Gated on the token. This is "save -> re-emit": editing the ruleset and emitting
/// produces one updated source-of-truth emit, custom rules preserved.
async fn emit_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({ "ok": false, "message": "Connect GitHub to emit." }));
    };
    let rules = resolve_project_arm_rules(&project).await;
    if rules.is_empty() && project.ruleset.custom.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "Nothing to emit — this project has no repo-local rules or custom rules yet." }));
    }
    // Re-emit carries no new baseline (it's a ruleset refresh, not onboarding).
    let no_baselines = std::collections::HashMap::new();
    let results =
        emit_to_repos(&project.repos, &rules, &project.ruleset.custom, &no_baselines, &token).await;
    Json(serde_json::json!({ "ok": true, "results": results }))
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
    // AI decomposition (grounded children), with the deterministic propose as fallback.
    let llm = crate::llm::Llm::from_env();
    let children =
        crate::decompose::propose_ai(&parent, &Practice::default_feature(), &llm).await;
    Ok(Json(children))
}

/// AI-suggested clarifying questions for a story: the questions an engineer genuinely
/// needs answered before building it. The architect reviews/edits before any is posted
/// to the team (the clarify-bridge stays review-then-post). Empty on model failure.
async fn suggest_clarifications(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Result<Json<Vec<String>>, AppError> {
    let story = state
        .stories
        .get(&story_id)
        .await
        .map_err(AppError)?
        .ok_or_else(|| AppError(anyhow::anyhow!("story not found: {story_id}")))?;
    let system = "You are the engineer about to build this story. List the clarifying \
        questions you GENUINELY need answered before writing code: ambiguities, missing \
        decisions, edge cases, unstated constraints. Be specific to this story. Return \
        ONLY a JSON array of question strings, e.g. [\"q1\", \"q2\"]. 0-6 questions.";
    let user = format!("Story: {}\n\nDescription: {}", story.title, story.description);
    let llm = crate::llm::Llm::from_env();
    let questions = match llm
        .complete(crate::llm::LlmRequest::new(user).with_system(system))
        .await
    {
        Ok(resp) => parse_string_array(&resp.text),
        Err(_) => Vec::new(),
    };
    Ok(Json(questions))
}

/// Extract a JSON array of strings from a model response (tolerant of surrounding prose).
fn parse_string_array(raw: &str) -> Vec<String> {
    let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(&raw[start..=end]).unwrap_or_default()
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
/// routine_authoring_intent_not_prompt). The user describes WHAT they want; the
/// lead-engineer AI authors the operational prompt for them to review/edit
/// (`authored_by: claude`). If the model is unreachable it falls back to the
/// deterministic scaffold (`authored_by: scaffold`) so the form never dead-ends.
async fn draft_routine_prompt(
    Json(req): Json<crate::routine::DraftPromptReq>,
) -> Json<crate::routine::DraftPromptResp> {
    let system = "You are Camerata's lead engineer. The user describes WHAT they want a \
        scheduled, governed agent routine to do. Author the OPERATIONAL PROMPT the agent \
        will run: concrete directives, the model tier(s) appropriate to the work, the \
        permission scope, and the governance framing (every write passes the deny-before-\
        execute gate; the agent cannot run git). Return ONLY the operational prompt text, \
        ready to run — no preamble, no markdown headers.";
    let user = format!(
        "Permission scope: {}\n\nWhat the user wants:\n{}",
        req.scope, req.intent
    );
    let llm = crate::llm::Llm::from_env();
    match llm
        .complete(crate::llm::LlmRequest::new(user).with_system(system))
        .await
    {
        Ok(resp) if !resp.text.trim().is_empty() => Json(crate::routine::DraftPromptResp {
            prompt: resp.text,
            authored_by: "claude".to_string(),
        }),
        _ => Json(crate::routine::DraftPromptResp {
            prompt: crate::routine::scaffold_prompt(&req.intent, &req.scope),
            authored_by: "scaffold".to_string(),
        }),
    }
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

/// Edit an existing routine (name / schedule / intent / prompt / scope).
async fn update_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateRoutineReq>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .update(&id, &req)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("routine not found: {id}")))
}

/// Delete a routine.
async fn delete_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if state.routines.delete(&id) {
        Ok(Json(serde_json::json!({ "deleted": id })))
    } else {
        Err(AppError(anyhow::anyhow!("routine not found: {id}")))
    }
}

// ── AI: model provider ────────────────────────────────────────────────────────

/// The models the UI offers in its selector.
async fn list_models() -> Json<serde_json::Value> {
    let models: Vec<_> = crate::llm::MODELS
        .iter()
        .map(|m| {
            serde_json::json!({
                "vendor": m.vendor,
                "label": m.label,
                "id": m.id,
                "price_in": m.price_in,
                "price_out": m.price_out,
            })
        })
        .collect();
    Json(serde_json::json!({
        "models": models,
        "default": crate::llm::DEFAULT_MODEL,
        "backend": crate::llm::Llm::from_env().backend_label(),
    }))
}

#[derive(serde::Deserialize)]
struct ChatReq {
    prompt: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    system: Option<String>,
}

/// The research chat: one completion through the configured backend. The side-by-side
/// chatbot uses this; it's also the smoke test that the model wiring works.
async fn chat(Json(req): Json<ChatReq>) -> Result<Json<crate::llm::LlmResponse>, AppError> {
    let llm = crate::llm::Llm::from_env();
    let mut r = crate::llm::LlmRequest::new(req.prompt).with_model(req.model);
    if let Some(system) = req.system {
        r = r.with_system(system);
    }
    Ok(Json(llm.complete(r).await?))
}

// ── local workspace (checkouts) ───────────────────────────────────────────────

/// The current app settings (incl. the workspace root).
async fn get_settings(State(state): State<AppState>) -> Json<crate::settings::Settings> {
    Json(state.settings.get())
}

#[derive(serde::Deserialize)]
struct WorkspaceRootReq {
    path: Option<String>,
}

/// Set the workspace root (the visible folder where repos are cloned).
async fn set_workspace_root(
    State(state): State<AppState>,
    Json(req): Json<WorkspaceRootReq>,
) -> Json<crate::settings::Settings> {
    Json(state.settings.set_workspace_root(req.path))
}

/// Read-only checkout status for every repo in a project (no network).
async fn checkout_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::workspace::RepoCheckout>>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))?;
    let Some(root) = state.settings.workspace_root() else {
        return Err(AppError(anyhow::anyhow!(
            "no workspace folder is set — pick one first"
        )));
    };
    let root = std::path::PathBuf::from(root);
    let mut out = Vec::with_capacity(project.repos.len());
    for repo in &project.repos {
        out.push(crate::workspace::checkout_status(&root, repo).await);
    }
    Ok(Json(out))
}

/// Clone (or fast-forward) every repo in a project into the workspace.
async fn checkout_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::workspace::RepoCheckout>>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))?;
    let Some(root) = state.settings.workspace_root() else {
        return Err(AppError(anyhow::anyhow!(
            "no workspace folder is set — pick one first"
        )));
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN to clone"
        )));
    }
    let root = std::path::PathBuf::from(root);
    let mut out = Vec::with_capacity(project.repos.len());
    for repo in &project.repos {
        out.push(crate::workspace::clone_or_pull(&root, repo, &token).await);
    }
    Ok(Json(out))
}

#[derive(serde::Deserialize)]
struct BranchReq {
    repo: String,
    branch: String,
}

/// Create (or switch to) a working branch in a project's local clone.
async fn checkout_branch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<BranchReq>,
) -> Result<Json<crate::workspace::RepoCheckout>, AppError> {
    let _project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))?;
    let Some(root) = state.settings.workspace_root() else {
        return Err(AppError(anyhow::anyhow!("no workspace folder is set")));
    };
    let root = std::path::PathBuf::from(root);
    crate::workspace::create_branch(&root, &req.repo, &req.branch).await?;
    Ok(Json(crate::workspace::checkout_status(&root, &req.repo).await))
}

#[derive(serde::Deserialize)]
struct ShipReq {
    repo: String,
    branch: String,
    title: String,
    #[serde(default)]
    body: String,
}

/// Ship a repo: push its working branch and open a PR. Returns the PR URL.
async fn ship_repo(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ShipReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))?;
    let Some(root) = state.settings.workspace_root() else {
        return Err(AppError(anyhow::anyhow!("no workspace folder is set")));
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN to push"
        )));
    }
    let root = std::path::PathBuf::from(root);
    let url = crate::workspace::ship(&req.repo, &req.branch, &req.title, &req.body, &root, &token)
        .await?;
    Ok(Json(serde_json::json!({ "pr_url": url })))
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

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError(e)
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

    fn arm_rule(id: &str, scope: &str, repos: &[&str]) -> crate::arm::ArmRule {
        crate::arm::ArmRule {
            id: id.to_string(),
            title: format!("T {id}"),
            directive: format!("D {id}"),
            option: None,
            enforcement: "structured".to_string(),
            scope: scope.to_string(),
            conformance: None,
            repos: repos.iter().map(|s| s.to_string()).collect(),
        }
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
        );
        let p = state.projects.active().expect("a project was created");
        assert_eq!(p.ruleset.selections.len(), 1, "repo-local -> selections");
        assert_eq!(p.ruleset.selections[0].rule_id, "REPO-1");
        assert_eq!(p.ruleset.cross_repo.len(), 1, "cross-repo -> cross_repo");
        assert_eq!(p.ruleset.process.len(), 1, "process -> process");
        assert!(p.repos.contains(&"me/web".to_string()), "repos absorbed into the project");
    }

    #[test]
    fn re_arm_preserves_custom_in_the_project() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        // Seed a project with a custom rule.
        let p = state.projects.create("P", vec!["me/api".to_string()]).unwrap();
        state.projects.update(&p.id, |pr| {
            pr.merge_custom(&[crate::project::CustomRule {
                name: "house".into(),
                body: "Prefer X.".into(),
                domain: "*".into(),
            }]);
        });
        // Arming (saving base rules) must keep the custom rule.
        save_armed_to_project(&state, &[arm_rule("REPO-1", "repo-local", &["me/api"])]);
        let after = state.projects.get(&p.id).unwrap();
        assert_eq!(after.ruleset.selections.len(), 1);
        assert_eq!(after.ruleset.custom.len(), 1, "custom survived the re-arm upsert");
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
        // The five substantive rules, GOV-1 (the synthetic test rule) filtered out.
        assert_eq!(arr.len(), 5);
        let ids: Vec<&str> = arr.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"SEC-NO-HARDCODED-SECRETS-1"));
        assert!(ids.contains(&"SEC-NO-PATH-ESCAPE-1"));
        assert!(ids.contains(&"SEC-NO-SECRET-FILES-1"));
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
