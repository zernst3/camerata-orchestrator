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
pub mod auto_fire;
pub mod clarify;
pub mod connections;
pub mod decompose;
pub mod draft;
pub mod escalation;
pub mod eval;
pub mod evidence;
pub mod fix;
pub mod github_issues;
pub mod jobs;
pub mod lifecycle;
pub mod live_fleet;
pub mod llm;
pub mod notify;
pub mod onboard;
pub mod project;
pub mod provider;
pub mod reconcile;
pub mod routine;
pub mod run;
pub mod scan_cache;
pub mod scan_routing;
pub mod schedule;
pub mod model_tier;
pub mod settings;
pub mod suppression;
pub mod terminal;
pub mod transcript;
pub mod uow;
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
use camerata_worktracker::{CanonicalStory, ExternalRef, InMemoryStoryStore, StoryStore};

use crate::clarify::{AnswerReq, Clarification, ClarificationStore, PostClarifyReq};
use crate::decompose::{to_story, DecompositionStore, Practice, ProposedChild};
use crate::provider::ProviderHandle;
use crate::routine::{CreateRoutineReq, Routine, RoutineStore, SetEnabledReq};
use crate::run::{execute_run, live_mode_enabled, run_provenance, Run, RunProvenance, RunStore};

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
    uow: crate::uow::UowStore,
    escalations: crate::escalation::EscalationStore,
    /// Per-project incremental-scan cache (file fingerprints + last AI findings) so a
    /// re-scan only pays the AI bill for files that changed. Best-effort; losing it just
    /// means the next scan is a full scan.
    scan_cache: crate::scan_cache::ScanCacheStore,
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
            uow: crate::uow::UowStore::new(),
            escalations: crate::escalation::EscalationStore::new(),
            scan_cache: crate::scan_cache::ScanCacheStore::new(),
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
            state.uow = crate::uow::UowStore::at(dir.join("uow.json"));
            // Routines persist too, so a scheduled governed run an architect set up
            // survives a restart instead of being lost on every launch.
            state.routines = RoutineStore::at(dir.join("routines.json"));
            // Open human-review escalations survive a restart so a blocked routine isn't
            // silently un-blocked by quitting the app.
            state.escalations =
                crate::escalation::EscalationStore::at(dir.join("escalations.json"));
            // Incremental-scan cache: per-project file fingerprints + last AI findings, so a
            // re-scan only re-audits changed files. Survives restarts; safe to delete.
            state.scan_cache =
                crate::scan_cache::ScanCacheStore::load_or_new(dir.join("scan-cache.json"));
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
        .route("/api/corpus-rules", get(corpus_rules))
        .route("/api/stories", get(stories))
        .route("/api/stories/:id/run", post(start_run))
        .route("/api/runs/:id", get(get_run))
        .route("/api/runs/:id/agents", get(get_run_agents))
        .route("/api/runs/:id/provenance", get(get_run_provenance))
        .route("/api/runs/:id/sign-off", post(sign_off_run))
        .route(
            "/api/stories/:id/clarifications",
            get(list_clarifications).post(post_clarification),
        )
        .route(
            "/api/clarifications/:cid/answer",
            post(answer_clarification),
        )
        .route("/api/clarifications", get(list_open_clarifications))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/import", post(import_project))
        .route(
            "/api/projects/active",
            get(active_project).post(set_active_project),
        )
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
        .route("/api/projects/:id/max-iterations", post(set_max_iterations))
        .route("/api/provider", get(provider_info))
        .route("/api/connections", get(connections_status))
        .route("/api/notifications", get(notifications_feed))
        .route("/api/stories/adopt", post(adopt_story))
        // GitHub Issue intake (#20): list a repo's open issues, then adopt one onto the spine.
        .route("/api/github/issues", get(github_issues_list))
        .route("/api/stories/adopt-issue", post(adopt_issue))
        .route("/api/onboard/scan", post(onboard_scan))
        .route("/api/onboard/audit", post(onboard_audit))
        .route("/api/onboard/audit/start", post(onboard_audit_start))
        .route("/api/onboard/audit/job/:id", get(onboard_audit_job))
        .route("/api/git/detect-repo", post(detect_repo))
        .route("/api/gate-probe", post(gate_probe))
        .route("/api/onboard/ticket", post(onboard_ticket))
        .route("/api/onboard/arm", post(onboard_arm))
        .route("/api/onboard/apply", post(onboard_apply))
        .route(
            "/api/onboard/apply/preflight",
            post(onboard_apply_preflight),
        )
        .route("/api/onboard/open-pr", post(onboard_open_pr))
        .route(
            "/api/onboard/draft",
            get(onboard_draft_get).post(onboard_draft_save),
        )
        .route("/api/onboard/draft/clear", post(onboard_draft_clear))
        .route("/api/projects/:id/repo-health", get(project_repo_health))
        .route("/api/repo-path", post(set_repo_path))
        .route("/api/onboard/ci-rules", post(onboard_ci_rules))
        .route("/api/onboard/complete", post(onboard_complete))
        .route("/api/projects/:id/suppressions", get(project_suppressions))
        .route("/api/onboard/ignore", post(onboard_ignore))
        .route(
            "/api/stories/:id/clarify/suggest",
            post(suggest_clarifications),
        )
        .route("/api/stories/:id/decompose", post(decompose_propose))
        .route("/api/stories/:id/decompose/commit", post(decompose_commit))
        .route("/api/stories/:id/children", get(list_children))
        .route("/api/routines", get(list_routines).post(create_routine))
        .route("/api/routines/templates", get(list_routine_templates))
        .route("/api/routines/templates/:id/instantiate", post(instantiate_routine_from_template))
        .route("/api/routines/draft-prompt", post(draft_routine_prompt))
        .route(
            "/api/routines/:id",
            axum::routing::put(update_routine).delete(delete_routine),
        )
        .route("/api/routines/:id/enable", post(set_routine_enabled))
        .route("/api/routines/:id/provision", post(provision_routine))
        .route("/api/routines/:id/run", post(run_routine_now))
        // Routine escalations: a blocked routine awaiting human review.
        .route(
            "/api/escalations",
            get(list_escalations).post(raise_escalation),
        )
        .route("/api/escalations/:id/chat", post(chat_escalation))
        .route("/api/escalations/:id/answer", post(answer_escalation))
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
        // ── Local git controls (issue #37) ───────────────────────────────────
        .route("/api/projects/:id/git/branches", get(git_branches))
        .route("/api/projects/:id/git/log", get(git_log))
        .route("/api/projects/:id/git/status", get(git_status_endpoint))
        .route("/api/projects/:id/git/checkout", post(git_checkout))
        .route("/api/projects/:id/git/commit", post(git_commit))
        .route("/api/projects/:id/git/push", post(git_push))
        .route("/api/projects/:id/git/pull", post(git_pull))
        .route("/api/projects/:id/git/cherry-pick", post(git_cherry_pick))
        // ── Unit of Work (issue #39) ─────────────────────────────────────────
        .route("/api/uow", get(uow_list))
        .route("/api/uow/:story_id", get(uow_get))
        .route("/api/uow/:story_id/status", post(uow_set_status))
        .route("/api/uow/:story_id/branch", post(uow_set_branch))
        .route("/api/uow/:story_id/history", post(uow_append_history))
        // ── Governed-development lifecycle (Pillar 2) ─────────────────────────
        .route("/api/uow/:story_id/decisions", post(uow_set_decisions))
        .route(
            "/api/uow/:story_id/begin-investigation",
            post(uow_begin_investigation),
        )
        .route(
            "/api/uow/:story_id/approve-decisions",
            post(uow_approve_decisions),
        )
        // ── In-app terminal (issue #38) ───────────────────────────────────────
        // Each connection spawns a PTY-backed shell; multiple tabs = multiple ws
        // connections. No AppState needed — the handler is fully self-contained.
        .route("/api/terminal/ws", get(terminal::ws_handler))
        // ── Project-aware chat grounding (#54) ───────────────────────────────
        // Supplies the live project state (draft / scan report / ruleset summary)
        // that the Project mode chat panel injects as a system-prompt grounding
        // context. Read-only; no model call happens here.
        .route("/api/projects/active/context", get(active_project_context))
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

    // Auto-fire scheduler: runs provisioned + enabled routines when their schedule
    // comes due. Spawned here (not in `router`) so tests that build the router don't
    // start firing routines. Cadence: CAMERATA_ROUTINE_TICK_SECS (default 60).
    crate::auto_fire::spawn_routine_scheduler(state.routines.clone(), state.escalations.clone());

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

/// Optional request body for starting a governed run. All fields are optional
/// so the existing no-body callers remain compatible.
#[derive(serde::Deserialize, Default)]
struct StartRunReq {
    /// The model id (`/api/models`) for every `claude -p` agent in the live
    /// fleet. Ignored for the scripted (token-free) path. `None`/blank falls
    /// back to the CLI default so the live fleet's behaviour is unchanged when
    /// the caller sends no body.
    #[serde(default)]
    model: Option<String>,
}

/// Start a governed run for a story. Returns the run id immediately; the run walks
/// to completion on a background task, driving planted tool calls through the REAL
/// gate (deterministic, token-free). Poll `GET /api/runs/:id` for status + verdicts.
///
/// The optional JSON body accepts `{ "model": "<id>" }` to pin the model for the
/// live-fleet path. The scripted (token-free) path ignores it.
async fn start_run(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<StartRunReq>>,
) -> Response {
    let model = req
        .and_then(|Json(r)| r.model)
        .filter(|m| !m.trim().is_empty());

    // The no-code-first gate (Pillar 2): a governed run cannot start until every
    // DecisionRecord on this story's UoW is approved (decisions_approved_for_development).
    // We block + surface exactly why, rather than silently starting a run that the
    // architect did not gate. The check reads the persisted decisions on the UoW.
    if let Err(reason) = ensure_development_gate(&state, &story_id) {
        let body = Json(serde_json::json!({
            "error": "development gate not satisfied",
            "reason": reason,
            "story_id": story_id,
        }));
        return (StatusCode::CONFLICT, body).into_response();
    }

    let (run_id, mode) = start_governed_run(&state, &story_id, model).await;
    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id, "mode": mode }))
        .into_response()
}

/// Enforce the no-code-first gate for a story before a governed run may start.
///
/// Returns `Ok(())` when development is permitted, or `Err(reason)` with a human-
/// readable explanation when it is blocked. The gate is the structural decision check
/// (`decisions_approved_for_development`): at least one decision exists and every
/// decision is `Approved`.
///
/// As a side effect, when the gate IS satisfied it best-effort drives the UoW's
/// lifecycle stage forward to `Development` (Investigating → DecisionsApproved →
/// Development as needed), so the persisted stage reflects that a governed run is now
/// underway. The forward drive is best-effort: a UoW already past these stages is left
/// as-is, never moved backward.
fn ensure_development_gate(state: &AppState, story_id: &str) -> Result<(), String> {
    use camerata_worktracker::investigation::decisions_approved_for_development;
    use crate::lifecycle::UowStage;

    let uow = state.uow.get_or_create(story_id);

    if !decisions_approved_for_development(&uow.decisions) {
        let unapproved = uow.decisions.iter().filter(|d| d.needs_review()).count();
        let reason = if uow.decisions.is_empty() {
            "No decisions have been recorded for this story yet. The investigation \
             must surface at least one decision and the architect must approve it \
             before any code is written."
                .to_string()
        } else {
            format!(
                "{unapproved} of {} decision(s) still need the architect's approval. \
                 Every decision must be approved before a governed run can start.",
                uow.decisions.len()
            )
        };
        return Err(reason);
    }

    // Gate satisfied — drive the lifecycle stage forward to Development, stepping
    // through any intermediate stages. Each step is best-effort: a transition that is
    // illegal from the current stage (because the UoW is already further along, or was
    // never moved off Intake) is simply skipped, never forced.
    match uow.stage {
        UowStage::Intake => {
            let _ = state.uow.begin_investigation(story_id);
            let _ = state.uow.approve_decisions(story_id);
            let _ = state.uow.start_development(story_id);
        }
        UowStage::Investigating => {
            let _ = state.uow.approve_decisions(story_id);
            let _ = state.uow.start_development(story_id);
        }
        UowStage::DecisionsApproved => {
            let _ = state.uow.start_development(story_id);
        }
        // Already at/after Development: leave the stage as-is.
        _ => {}
    }
    Ok(())
}

/// Start a governed run for a story through the ONE pipeline every development task
/// uses — live fleet (worktree → gated MCP write → layer-2 checks → bounce) when opted
/// in, the token-free scripted gate otherwise. Returns `(run_id, mode)`. Shared so a
/// brownfield remediation run is governed EXACTLY like any other dev task, not a
/// special path: fixing the audited items is a development task, the first one.
///
/// `model` is forwarded to every `claude -p` agent in the live-fleet path. It is
/// ignored for the scripted path (which makes no agent calls).
async fn start_governed_run(
    state: &AppState,
    story_id: &str,
    model: Option<String>,
) -> (String, &'static str) {
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
        // The active project's loop-guard ceiling (#29) governs how many times a dirty
        // stage may bounce-and-revise before its residual violations are surfaced.
        let max_iterations = state
            .projects
            .active()
            .map(|p| p.max_iterations)
            .unwrap_or(1);
        tokio::spawn(async move {
            live_fleet::execute_live_run(store, rid, title, desc, model, max_iterations).await
        });
    } else {
        // Token-free scripted path: real gate verdicts over planted calls, with the
        // per-agent transcripts (generated prompt + actions + verdicts) populated.
        // `model` is not relevant here — no agent process is spawned.
        let transcripts = state.transcripts.clone();
        tokio::spawn(async move { execute_run(store, transcripts, rid).await });
    }

    // Provenance-stamping watcher (Pillar 2): once the run reaches its terminal
    // (`done`) state, freeze the gate provenance onto the story's UoW and advance the
    // lifecycle stage Development → AwaitingQa. This persists the honest accounting an
    // architect reviews at QA, and survives the in-memory RunStore being lost. Runs as
    // its own task so the run executor stays unaware of the UoW (keeps the layers thin).
    {
        let runs = state.runs.clone();
        let uow = state.uow.clone();
        let watch_id = run_id.clone();
        let watch_story = story_id.to_string();
        tokio::spawn(async move {
            stamp_provenance_when_done(runs, uow, watch_id, watch_story).await;
        });
    }

    (run_id, mode)
}

/// Poll a run until it reports `done`, then freeze its gate provenance onto the story's
/// UoW and advance the lifecycle stage to `AwaitingQa`. Bounded poll loop so a never-
/// completing run (e.g. a wedged live fleet) can't leak the task forever.
async fn stamp_provenance_when_done(
    runs: RunStore,
    uow: crate::uow::UowStore,
    run_id: String,
    story_id: String,
) {
    // Up to ~5 minutes of 500ms polls. The scripted path finishes in a few seconds;
    // the live path is operator-driven and may legitimately take longer, but we cap to
    // avoid an unbounded task. If it times out, no provenance is stamped (the architect
    // can still read the live run + sign off; the durable copy is best-effort).
    const MAX_POLLS: usize = 600;
    for _ in 0..MAX_POLLS {
        if let Some(run) = runs.get(&run_id) {
            if run.done {
                let rules = camerata_gateway::enforced_gate_rules();
                let prov = run_provenance(&run, &rules);
                let frozen = crate::uow::GateProvenance {
                    run_id: prov.run_id.clone(),
                    mode: prov.mode.clone(),
                    allow_count: prov.allow_count,
                    deny_count: prov.deny_count,
                    total_bounces: prov.total_bounces,
                    rules_fired: prov.rules_fired.clone(),
                    recorded: chrono::Utc::now().to_rfc3339(),
                };
                uow.record_gate_provenance(&story_id, frozen);
                // Advance Development → AwaitingQa (best-effort: only legal from
                // Development; a UoW elsewhere is left as-is, never forced).
                let _ = uow.finish_development(&story_id);
                return;
            }
        } else {
            // The run vanished from the store; nothing to stamp.
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
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

/// The PROVENANCE summary for a run (issue #21): which rules were in force, the gate
/// deny/allow tallies, and the total bounces — the honest accounting an architect
/// reads before signing the run off. Derived from the run's REAL recorded verdicts.
async fn get_run_provenance(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunProvenance>, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("run not found: {id}")))?;
    let rules = camerata_gateway::enforced_gate_rules();
    Ok(Json(run_provenance(&run, &rules)))
}

#[derive(serde::Deserialize)]
struct SignOffReq {
    /// Who is signing off (the architect's handle/name). Defaults to "architect".
    #[serde(default)]
    by: Option<String>,
    /// An optional note attached to the sign-off.
    #[serde(default)]
    note: Option<String>,
}

/// SIGN-OFF action for a run (issue #21): the architect explicitly marks a completed
/// governed run as signed off. Persisted on the story's Unit of Work (which survives
/// sessions) along with the run id and a history entry. Camerata never signs work off
/// on its own — this is the deliberate human gate after reviewing the provenance.
async fn sign_off_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SignOffReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("run not found: {id}")))?;
    let by = req
        .by
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "architect".to_string());
    let uow = state
        .uow
        .sign_off(&run.story_id, &by, &run.id, req.note.as_deref());
    Ok(Json(uow))
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

/// A project export: the full project (name, repos, ruleset, onboarded) PLUS its
/// routines, so the autonomous plane travels with the project. `#[serde(flatten)]` keeps
/// the project fields at the top level, so an older importer that only reads project
/// fields still works (it just ignores `routines`).
#[derive(serde::Serialize)]
struct ProjectExportDoc {
    #[serde(flatten)]
    project: crate::project::Project,
    /// The project's routines. On import they arrive un-provisioned + stopped (the
    /// importer explicitly sets them up). Empty for a project with no routines.
    routines: Vec<Routine>,
}

/// Export a project as a portable JSON document (project + its routines) — for backup or
/// moving a project between machines/installs.
async fn export_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectExportDoc>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("project not found: {id}")))?;
    let routines = state.routines.list_for_project(&id);
    Ok(Json(ProjectExportDoc { project, routines }))
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
    /// The source project's routines, travelling with the export. Imported routines are
    /// created un-provisioned + stopped. Empty (default) leaves the target's routines
    /// untouched; a non-empty list REPLACES the target project's routines.
    #[serde(default)]
    routines: Vec<ImportedRoutine>,
}

/// A routine inside a project import. Deserializes from an exported full `Routine` (extra
/// fields like id / enabled / last_run are ignored); only the authoring fields travel.
#[derive(serde::Deserialize)]
struct ImportedRoutine {
    name: String,
    schedule: String,
    #[serde(default)]
    intent: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    scope: String,
    /// The routine's model travels with it; blank/absent falls back to the server default.
    #[serde(default)]
    model: Option<String>,
}

/// Create the imported routines under `project_id` (un-provisioned + stopped), replacing
/// any the target project already had. No-op when the import carried none, so importing a
/// routine-less export never wipes routines the importer added locally.
fn import_project_routines(state: &AppState, project_id: &str, routines: &[ImportedRoutine]) {
    if routines.is_empty() {
        return;
    }
    let reqs: Vec<crate::routine::CreateRoutineReq> = routines
        .iter()
        .map(|r| crate::routine::CreateRoutineReq {
            name: r.name.clone(),
            schedule: r.schedule.clone(),
            intent: r.intent.clone(),
            prompt: r.prompt.clone(),
            scope: if r.scope.trim().is_empty() {
                "read-only".to_string()
            } else {
                r.scope.clone()
            },
            project_id: Some(project_id.to_string()),
            model: r.model.clone(),
        })
        .collect();
    state.routines.replace_for_project(project_id, &reqs);
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
    let imported_routines = req.routines;
    match state.projects.import_or_overwrite(
        &req.name,
        req.repos,
        req.ruleset,
        req.onboarded,
        req.overwrite,
    ) {
        Some(ImportOutcome::Created(p)) => {
            import_project_routines(&state, &p.id, &imported_routines);
            Json(serde_json::json!({ "ok": true, "project": p, "overwritten": false }))
        }
        Some(ImportOutcome::Overwritten(p)) => {
            import_project_routines(&state, &p.id, &imported_routines);
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

/// Every corpus rule with its FULL context (title, domain, scope, options, default), regardless
/// of any detected stack — feeds the Rules view's all-rules reference table and the
/// option-switch editor. `propose_corpus_rules` with no repos returns the whole library
/// (un-suggested, repos empty); the Rules view cross-references each against the project's
/// selections to show which repos it's applied to.
async fn corpus_rules() -> Json<Vec<crate::onboard::ProposedRule>> {
    Json(crate::onboard::propose_corpus_rules(&[]).await)
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
    match state
        .projects
        .update(&id, |p| p.merge_custom(std::slice::from_ref(&rule)))
    {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Update a project's loop-guard ceiling (#29): the max developer→checker
/// bounce-and-revise iterations a stage may take before the fleet stops and
/// raises the outstanding violations for human review. Clamped to at least `1`
/// (a found violation always earns one bounce; the guard caps the loop, it never
/// disables it).
#[derive(serde::Deserialize)]
struct MaxIterationsReq {
    max_iterations: usize,
}

async fn set_max_iterations(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MaxIterationsReq>,
) -> Json<serde_json::Value> {
    match state
        .projects
        .update(&id, |p| p.set_max_iterations(req.max_iterations))
    {
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
/// Resolve a set of repos to their LOCAL working-tree dirs for an onboarding read (scan /
/// audit / waiver registry). Local-first: a repo's dir is its per-repo path override, else
/// `<workspace_root>/<owner>/<repo>`. Repos with no local git clone are returned as NOTES
/// (not sources) so the caller can surface "browse to the repo's folder" — onboarding reads
/// code from disk, never from GitHub.
fn resolve_local_sources(
    state: &AppState,
    repos: &[String],
) -> (Vec<(String, std::path::PathBuf)>, Vec<String>) {
    let workspace_root = state.settings.workspace_root();
    let mut sources = Vec::new();
    let mut notes = Vec::new();
    for spec in repos {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let override_path = state.settings.repo_path(spec);
        match crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            spec,
        ) {
            Some(dir) if dir.join(".git").exists() => sources.push((spec.to_string(), dir)),
            Some(dir) => notes.push(format!(
                "{spec}: {} is not a local git clone — browse to the repo's folder",
                dir.display()
            )),
            None => notes.push(format!(
                "{spec}: no local folder set — browse to the repo's folder"
            )),
        }
    }
    (sources, notes)
}

async fn onboard_scan(
    State(state): State<AppState>,
    Json(req): Json<ScanReq>,
) -> Json<crate::onboard::ScanReport> {
    let mut repos = req.repos;
    if let Some(r) = req.repo {
        repos.push(r);
    }
    repos.retain(|r| !r.trim().is_empty());
    if repos.is_empty() {
        let mut r = crate::onboard::ScanReport::gated(&repos);
        r.gated = false;
        r.message = Some("Add at least one repo (browse to its local folder) to scan.".to_string());
        return Json(r);
    }

    // Local-first: scan reads the repos' local working trees, not GitHub. No token needed.
    let (sources, notes) = resolve_local_sources(&state, &repos);
    Json(crate::onboard::scan_repos(&sources, notes).await)
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
    /// Thorough calibration (#51): when true, the calibration pass runs multiple times and takes
    /// a conservative consensus, plus a proportionality signal. Costs more AI; opt-in.
    #[serde(default)]
    thorough: bool,
    /// Incremental scan: when true (the default), the AI audit only re-scans files whose content
    /// changed since the last scan of this project, carrying forward cached findings for unchanged
    /// files. The UI's "Full scan (ignore incremental cache)" checkbox sends `false` to force a
    /// clean pass over every file. The first scan of a project is always full (no cache yet).
    #[serde(default = "default_true")]
    incremental: bool,
    /// OPT-IN deep compliance & security tier (#55, in-MVP per #62). When true, AFTER the
    /// standard audit Camerata runs the three deep lenses — SOC-2 readiness gap analysis, a
    /// deep security audit (beyond the deterministic floor), and a threat model — over each
    /// repo on the selected model, and attaches the result to the report's `deep` field.
    /// Defaults to FALSE: it is the MOST EXPENSIVE tier (three extra whole-repo passes) and
    /// must never run by default. Its output is ADVISORY + model-inferred (#62), not a SOC-2
    /// report and not a penetration test.
    #[serde(default)]
    deep: bool,
}

/// serde default for an opt-OUT boolean (defaults to `true` when the field is absent).
fn default_true() -> bool {
    true
}

/// The transcript key the scan/audit AI activity registers under (the Agent-activity
/// drawer polls `/api/runs/scan-audit/agents`).
const SCAN_AUDIT_KEY: &str = "scan-audit";

/// Phase 2 — audit the repos AGAINST the selected rules (the deterministic security floor
/// plus the AI audit parameterized by the chosen rules). Returns the findings report. The
/// AI activity (prompts and output) registers into the transcript store so the UI can
/// show, live, that the model is actually working.
/// Partition selected audit rules into the ones the code-only AI scan should check
/// (prose / structured) and the CI-tier ones it should NOT (mechanical / architectural).
/// CI-tier rules are enforced in CI from build/runtime/DB context (query-plan, migration audit,
/// AST static analysis), so scanning them from a static code digest only yields weak,
/// low-confidence findings (e.g. "an index probably exists in a migration somewhere"). The
/// corpus is the source of each rule's tier; a rule absent from the corpus (e.g. a custom rule)
/// defaults to scannable.
/// Returns `(scannable, excluded_ci_tier_ids)`.
async fn split_scannable_rules(
    selected: Vec<crate::onboard::SelectedRule>,
) -> (Vec<crate::onboard::SelectedRule>, Vec<String>) {
    let corpus_path = camerata_rules::corpus_path();
    let set = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };
    let is_ci_tier = |id: &str| -> bool {
        set.as_ref()
            .and_then(|s| s.get_by_id(id))
            .map(|r| r.enforcement.is_ci_enforced())
            .unwrap_or(false)
    };
    let mut scannable = Vec::new();
    let mut excluded = Vec::new();
    for r in selected {
        if is_ci_tier(&r.id) {
            excluded.push(r.id);
        } else {
            scannable.push(r);
        }
    }
    (scannable, excluded)
}

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
    // Local-first: the audit reads the repos' local working trees, not GitHub.
    let (sources, notes) = resolve_local_sources(&state, &repos);
    let selected: Vec<crate::onboard::SelectedRule> = req
        .rules
        .into_iter()
        .filter(|r| !r.id.trim().is_empty())
        .map(|r| crate::onboard::SelectedRule {
            id: r.id,
            directive: r.directive,
            repos: r.repos,
        })
        .collect();
    // Mechanical rules are enforced in CI, not by the static code scan — drop them here.
    let (selected, excluded_mechanical) = split_scannable_rules(selected).await;
    let model = req.model.filter(|m| !m.trim().is_empty());
    let calibration_model = req.calibration_model.filter(|m| !m.trim().is_empty());
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    // Fresh transcript for this audit run so the live feedback panel starts clean.
    state.transcripts.clear(SCAN_AUDIT_KEY);
    // Incremental scan: load this project's prior manifest unless the user forced a full scan.
    let project_id = state.projects.active().map(|p| p.id);
    let prior = if req.incremental {
        project_id
            .as_deref()
            .and_then(|id| state.scan_cache.get(id))
    } else {
        None
    };
    let (mut report, manifest) = crate::onboard::audit_repos(
        &sources,
        &selected,
        notes,
        model.as_deref(),
        calibration_model.as_deref(),
        mode,
        req.thorough,
        Some((&state.transcripts, SCAN_AUDIT_KEY)),
        None,
        prior.as_ref(),
        req.deep,
    )
    .await;
    // Persist the fresh manifest (even after a forced full scan) so the NEXT scan can be
    // incremental. Only when there's an active project to key it to.
    if let Some(id) = &project_id {
        state.scan_cache.put(id, manifest);
    }
    report.excluded_mechanical_rules = excluded_mechanical;
    Json(report)
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
        .map(|r| crate::onboard::SelectedRule {
            id: r.id,
            directive: r.directive,
            repos: r.repos,
        })
        .collect();
    // Mechanical rules are enforced in CI, not by the static code scan — drop them here.
    let (selected, excluded_mechanical) = split_scannable_rules(selected).await;
    let model = req.model.filter(|m| !m.trim().is_empty());
    let calibration_model = req.calibration_model.filter(|m| !m.trim().is_empty());
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    let thorough = req.thorough;
    let deep = req.deep;
    // Local-first: resolve each repo's local working tree up front (the spawned job owns them).
    let (sources, notes) = resolve_local_sources(&state, &repos);

    let job_id = state.jobs.create();
    state.transcripts.clear(SCAN_AUDIT_KEY);

    let jobs = state.jobs.clone();
    let transcripts = state.transcripts.clone();
    let jid = job_id.clone();
    // Incremental scan: capture the prior manifest + the cache store for the spawned task.
    let project_id = state.projects.active().map(|p| p.id);
    let prior = if req.incremental {
        project_id
            .as_deref()
            .and_then(|id| state.scan_cache.get(id))
    } else {
        None
    };
    let scan_cache = state.scan_cache.clone();
    tokio::spawn(async move {
        if sources.is_empty() {
            jobs.fail(
                &jid,
                "No local repos to audit — browse to each repo's local folder first.",
            );
            return;
        }
        let (mut report, manifest) = crate::onboard::audit_repos(
            &sources,
            &selected,
            notes,
            model.as_deref(),
            calibration_model.as_deref(),
            mode,
            thorough,
            Some((&transcripts, SCAN_AUDIT_KEY)),
            Some((&jobs, &jid)),
            prior.as_ref(),
            deep,
        )
        .await;
        // Persist the fresh manifest so the next scan can be incremental.
        if let Some(id) = &project_id {
            scan_cache.put(id, manifest);
        }
        report.excluded_mechanical_rules = excluded_mechanical;
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
/// #14 — run the end-to-end gate-loop probe (both layers, deterministic, no model) and return a
/// GO/NO-GO the Governed Development screen surfaces as an in-app gate self-check.
async fn gate_probe() -> Json<serde_json::Value> {
    match camerata_fleet::gate_probe::run_gate_probe().await {
        Ok(r) => {
            let checks: Vec<serde_json::Value> = r
                .layer1
                .iter()
                .map(|c| serde_json::json!({ "label": c.label, "denied": c.denied, "detail": c.detail }))
                .collect();
            Json(serde_json::json!({
                "ok": true,
                "go": r.go(),
                "story": r.story,
                "layer1": checks,
                "layer1_denied": r.layer1_denied_count(),
                "layer1_total": r.layer1_total(),
                "layer1_clean_allowed": r.layer1_clean_allowed,
                "layer2_bounced": r.layer2_bounced,
                "layer2_clean": r.layer2_clean,
                "agent_passes": r.agent_passes,
            }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

async fn detect_repo(
    State(state): State<AppState>,
    Json(req): Json<DetectRepoReq>,
) -> Json<serde_json::Value> {
    match crate::workspace::detect_remote_repo(std::path::Path::new(&req.path)).await {
        Ok(repo) => {
            // #50: onboarding is one-time per repo. Report whether this repo is ALREADY onboarded
            // (in any project's onboarded list) so the UI can block a re-onboard and route the
            // user to the workspace instead of producing a duplicate set of issues + branch.
            let onboarded_in = state
                .projects
                .list()
                .into_iter()
                .find(|p| p.onboarded.iter().any(|r| r == &repo))
                .map(|p| p.name);
            Json(serde_json::json!({
                "ok": true,
                "repo": repo,
                "onboarded": onboarded_in.is_some(),
                "onboarded_project": onboarded_in,
            }))
        }
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
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to file a ticket." }),
        );
    };
    let title = req.title.unwrap_or_else(|| {
        format!(
            "Tech debt: {} audit finding(s) accepted",
            req.findings.len()
        )
    });
    match crate::onboard::create_tech_debt_ticket(owner, repo, &token, &title, &req.findings).await
    {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Request to arm a set of repos with the approved (resolved) rules.
#[derive(serde::Deserialize)]
struct ArmReq {
    /// Fully-resolved rules (each with its chosen directive + which repos it goes to).
    rules: Vec<crate::arm::ArmRule>,
    /// User-authored custom rules (#49) — saved to the project's ruleset.custom and rendered
    /// as CUSTOM-<name> blocks in AGENTS.md. `domain` routes them (`*` = all repos, else a repo).
    #[serde(default)]
    custom: Vec<crate::project::CustomRule>,
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
        by_repo
            .entry(f.repo.clone())
            .or_default()
            .entries
            .push(entry);
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

/// Compute the exact set of files Apply will write for each repo, from a resolved
/// `ArmReq`. This is the SINGLE source of truth for "what files land in each repo",
/// shared by `onboard_apply` (which then writes them) and `onboard_apply_preflight`
/// (which checks which already exist). Keeping both paths on this helper guarantees the
/// overwrite warning lists exactly the files Apply would clobber — no drift.
///
/// Returns `repo -> Vec<(repo_relative_path, content)>`. Repos with no rules/custom for
/// them are omitted (Apply skips them too).
fn apply_files_per_repo(
    req: &ArmReq,
    custom: &[crate::project::CustomRule],
) -> std::collections::BTreeMap<String, Vec<(String, String)>> {
    let repo_local: Vec<&crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();

    let baselines = baselines_from_findings(&req.findings, "architect");

    let mut out: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    for repo in &repos {
        let repo_rules: Vec<&crate::arm::ArmRule> = repo_local
            .iter()
            .copied()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
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
        out.insert(repo.clone(), files);
    }
    out
}

/// Preflight for Apply: for each target repo, resolve its local dir (same resolution Apply
/// uses) and report which governance files Camerata is about to write ALREADY EXIST on disk
/// and would be overwritten. The UI calls this BEFORE firing `onboard_apply` so it can warn
/// the architect and require explicit acknowledgement before clobbering hand-written files.
///
/// Returns `{ ok, repos: [{ repo, existing_files: [path...] }, ...] }`. Only repos with at
/// least one would-be-overwritten file are listed (an empty `repos` means Apply is safe and
/// the UI should proceed without nagging).
async fn onboard_apply_preflight(
    State(state): State<AppState>,
    Json(req): Json<ArmReq>,
) -> Json<serde_json::Value> {
    let workspace_root = state.settings.workspace_root();
    let custom = state
        .projects
        .active()
        .map(|p| p.ruleset.custom)
        .unwrap_or_default();

    let files_per_repo = apply_files_per_repo(&req, &custom);

    let mut repos_out = Vec::new();
    for (repo, files) in &files_per_repo {
        let override_path = state.settings.repo_path(repo);
        let Some(dir) = crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        ) else {
            // No resolvable local dir → nothing to overwrite (Apply will report the
            // unresolved repo itself). Skip from the warning list.
            continue;
        };
        let will_write: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
        let existing = crate::arm::existing_governance_files(&dir, &will_write);
        if !existing.is_empty() {
            repos_out.push(serde_json::json!({
                "repo": repo,
                "existing_files": existing,
            }));
        }
    }

    Json(serde_json::json!({ "ok": true, "repos": repos_out }))
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
    save_armed_to_project(&state, &req.rules, &req.custom);

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
    let custom = state
        .projects
        .active()
        .map(|p| p.ruleset.custom)
        .unwrap_or_default();

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
        if req.custom.is_empty() {
            return Json(
                serde_json::json!({ "ok": false, "message": "No rules selected to apply." }),
            );
        }
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to apply (the branch is pushed to origin)." }),
        );
    };
    // Apply writes into each repo's LOCAL clone. A repo's local dir is resolved per-repo:
    // a per-repo path override (chosen via repo health) wins; otherwise it's cloned under the
    // workspace folder. We no longer hard-require the workspace folder up front — a project
    // whose repos all have explicit local folders can apply without one.
    let workspace_root = state.settings.workspace_root();

    // Source of truth: save the armed ruleset to the active project (create one if none).
    save_armed_to_project(&state, &req.rules, &req.custom);

    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .cloned()
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();

    // Fail fast (with the actionable message) only when NOTHING local is available: no
    // workspace folder AND no per-repo override for any target repo. Otherwise apply
    // per-repo, reporting any individually-unresolved repo in its result row.
    let has_any_local =
        workspace_root.is_some() || repos.iter().any(|r| state.settings.repo_path(r).is_some());
    if !has_any_local {
        return Json(
            serde_json::json!({ "ok": false, "message": "Set a local workspace folder (Settings) or choose each repo's local folder (repo health) — Apply writes into the repo's local clone." }),
        );
    }

    let custom = state
        .projects
        .active()
        .map(|p| p.ruleset.custom)
        .unwrap_or_default();
    let baselines = baselines_from_findings(&req.findings, "architect");

    let mut results = Vec::new();
    let mut applied: Vec<String> = Vec::new();
    for repo in &repos {
        let repo_rules: Vec<&crate::arm::ArmRule> = repo_local
            .iter()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
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
        // Resolve THIS repo's local dir: per-repo override wins, else <workspace_root>/<repo>.
        let override_path = state.settings.repo_path(repo);
        let Some(dir) = crate::workspace::resolve_repo_dir(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        ) else {
            results.push(serde_json::json!({
                "repo": repo, "ok": false,
                "message": "No local path — choose this repo's folder (repo health) or set a workspace folder."
            }));
            continue;
        };
        // Clone into the workspace root only when there's no explicit override (never clone
        // over a folder the architect chose by hand).
        let clone_root = if override_path
            .as_deref()
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false)
        {
            None
        } else {
            workspace_root.as_deref().map(std::path::Path::new)
        };
        let msg = format!("chore(governance): apply Camerata ruleset to {repo}");
        match crate::workspace::apply_local_and_push(
            &dir,
            repo,
            clone_root,
            crate::arm::ARM_BRANCH,
            &files,
            &msg,
            &token,
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
            state
                .projects
                .update(&active.id, |p| p.mark_onboarded(&applied));
        }
    }
    Json(
        serde_json::json!({ "ok": true, "branch": crate::arm::ARM_BRANCH, "onboarded": applied, "results": results }),
    )
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
    let path = if req.path.trim().is_empty() {
        None
    } else {
        Some(req.path.clone())
    };
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
/// The active project's id, or `""` when none is active (drafts are keyed per project so
/// opening another project never clobbers this one's in-progress onboarding).
fn active_project_id(state: &AppState) -> String {
    state.projects.active().map(|p| p.id).unwrap_or_default()
}

async fn onboard_draft_get(State(state): State<AppState>) -> Json<Option<serde_json::Value>> {
    let pid = active_project_id(&state);
    Json(state.draft.load(&pid))
}

/// Save/replace the active project's onboarding draft (opaque blob; the UI owns its shape).
async fn onboard_draft_save(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let pid = active_project_id(&state);
    state.draft.save(&pid, body);
    Json(serde_json::json!({ "ok": true }))
}

/// Drop the active project's onboarding draft (completed, or starting fresh).
async fn onboard_draft_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    let pid = active_project_id(&state);
    state.draft.clear(&pid);
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
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to open the PR." }),
        );
    };
    let mut repos: Vec<String> = req
        .repos
        .into_iter()
        .filter(|r| !r.trim().is_empty())
        .collect();
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
            Err(e) => results
                .push(serde_json::json!({ "repo": repo, "ok": false, "message": format!("{e}") })),
        }
    }
    Json(serde_json::json!({ "ok": true, "results": results }))
}

/// A finding subset the UI sends with a triage action (ignore). `rule_id` + `path` +
/// `snippet` identify the violation for the baseline waiver (extra fields are ignored).
#[derive(serde::Deserialize)]
struct OnboardFinding {
    #[serde(default)]
    rule_id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    snippet: String,
}

#[derive(serde::Deserialize)]
struct IgnoreReq {
    repo: String,
    findings: Vec<OnboardFinding>,
    /// Mandatory justification — a reason-less suppression is rejected (the invariant).
    reason: String,
    #[serde(default)]
    ticket: Option<String>,
}

/// Fetch and parse a repo's committed `.camerata/baseline.json` (default branch), or an
/// empty baseline if absent.
async fn fetch_baseline(owner: &str, repo: &str, token: &str) -> crate::suppression::Baseline {
    use base64::Engine as _;
    use camerata_worktracker::{HttpTransport, ReqwestTransport};
    let Ok(transport) = ReqwestTransport::new(format!("Bearer {token}")) else {
        return crate::suppression::Baseline::default();
    };
    let url =
        format!("https://api.github.com/repos/{owner}/{repo}/contents/.camerata/baseline.json");
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
        return Err(AppError(anyhow::anyhow!(
            "connect GitHub to record an ignore"
        )));
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
        let mut entry =
            crate::suppression::baseline_entry(&fr, "architect", &now, req.reason.trim());
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
    Ok(Json(
        serde_json::json!({ "ok": true, "url": url, "ignored": req.findings.len() }),
    ))
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
    // Local-first: the waiver registry reads each repo's local working tree, not GitHub.
    let (sources, _notes) = resolve_local_sources(&state, &project.repos);
    Ok(Json(crate::onboard::suppression_registry(&sources).await))
}

#[derive(serde::Deserialize)]
struct CiRulesReq {
    repo: String,
}

/// Finish onboarding for the active project. The post-scan steps (audit, triage, Apply,
/// wire-CI) are all optional, so this is the explicit "I'm done" action — and it must
/// PERSIST the onboarding result, not just flip a flag: it rebuilds the project's repos +
/// selected rules from the draft (the repos/rules otherwise only save at Apply), marks the
/// repos onboarded, and clears the draft.
async fn onboard_complete(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(active) = state.projects.active() else {
        return Json(serde_json::json!({ "ok": false, "message": "no active project" }));
    };
    let pid = active.id.clone();

    // The draft is UI-owned JSON; we read only the fields we need to recover the result.
    #[derive(serde::Deserialize)]
    struct DraftRule {
        #[serde(default)]
        id: String,
        #[serde(default)]
        scope: String,
        #[serde(default)]
        default_option: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct DraftScan {
        #[serde(default)]
        repos: Vec<String>,
        #[serde(default)]
        proposed_rules: Vec<DraftRule>,
    }
    #[derive(serde::Deserialize)]
    struct DraftForComplete {
        scan: DraftScan,
        #[serde(default)]
        repo_selection: std::collections::HashMap<String, Vec<String>>,
        #[serde(default)]
        custom: Vec<crate::project::CustomRule>,
    }

    let parsed = state
        .draft
        .load(&pid)
        .and_then(|v| serde_json::from_value::<DraftForComplete>(v).ok());

    let (repos, selections, cross_repo, process, custom) = if let Some(d) = parsed {
        use crate::project::RuleSelection;
        // rule_id -> the repos that selected it.
        let mut by_rule: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (repo, ids) in &d.repo_selection {
            for id in ids {
                by_rule.entry(id.clone()).or_default().push(repo.clone());
            }
        }
        let by_id: std::collections::HashMap<&str, &DraftRule> = d
            .scan
            .proposed_rules
            .iter()
            .map(|r| (r.id.as_str(), r))
            .collect();
        let (mut selections, mut cross_repo, mut process) = (Vec::new(), Vec::new(), Vec::new());
        for (rule_id, mut repos) in by_rule {
            repos.sort();
            repos.dedup();
            let Some(pr) = by_id.get(rule_id.as_str()) else {
                continue;
            };
            let sel = RuleSelection {
                rule_id: rule_id.clone(),
                chosen_option: pr.default_option.clone(),
                repos,
            };
            match pr.scope.as_str() {
                "cross-repo" => cross_repo.push(sel),
                "process" => process.push(sel),
                _ => selections.push(sel),
            }
        }
        (d.scan.repos, selections, cross_repo, process, d.custom)
    } else {
        (
            active.repos.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    };

    let has_rules = !selections.is_empty() || !cross_repo.is_empty() || !process.is_empty();
    state.projects.update(&pid, |p| {
        if has_rules {
            // Replace the base rules from onboarding; custom rules are preserved.
            p.ruleset.selections = selections.clone();
            p.ruleset.cross_repo = cross_repo.clone();
            p.ruleset.process = process.clone();
        }
        // Upsert the draft's custom rules (#49) by name (preserve any pre-existing).
        for c in &custom {
            if let Some(slot) = p.ruleset.custom.iter_mut().find(|x| x.name == c.name) {
                *slot = c.clone();
            } else {
                p.ruleset.custom.push(c.clone());
            }
        }
        // Adds the repos to the project AND marks them onboarded (union, deduped).
        p.mark_onboarded(&repos);
    });
    state.draft.clear(&pid);
    Json(serde_json::json!({ "ok": true, "onboarded": repos }))
}

/// Emit the "wire mechanical rules into CI" task as a STORY on the tracker — a GitHub
/// issue — NOT a governed run launched from onboarding. Onboarding's job is to produce
/// stories; the dev layer (Pillar 2) picks the issue up and does the work (every write
/// gated). Arming already emits `.camerata/ci-checks.json` (the declared mechanical rules);
/// this issue is the development task of turning each declared check into a real CI gate.
async fn onboard_ci_rules(Json(req): Json<CiRulesReq>) -> Json<serde_json::Value> {
    let Some((owner, repo)) = req.repo.split_once('/') else {
        return Json(serde_json::json!({ "ok": false, "message": "repo must be owner/repo" }));
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to create the story issue." }),
        );
    };
    let title = format!("Wire mechanical rules into CI — {}", req.repo);
    let body = format!(
        "Camerata onboarding story: wire the mechanical governance rules into **{repo}**'s CI so \
         a pull request that violates one FAILS the build.\n\n\
         The rules to enforce are declared in `.camerata/ci-checks.json` (id, title, directive, \
         conformance). For EACH rule:\n\
         1. Check whether it is ALREADY enforced in CI (inspect `.github/workflows/`, the linter \
         config, package scripts).\n\
         2. If it is, leave it.\n\
         3. If not, implement the enforcement using this repo's stack — an ESLint rule, a \
         migration/index audit step, or an AST lint — per the rule's `conformance`, wired into \
         the CI workflow so it runs on every PR.\n\n\
         Do not weaken or delete existing checks. When the dev layer (Pillar 2) is wired, this \
         story is picked up and run as a governed development task; for now it's filed as a \
         tracked issue.\n\n_Filed by Camerata onboarding._"
    );
    match crate::onboard::create_issue(owner, repo, &token, &title, &body).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Classify the armed rules by scope and save them to the active project (creating
/// one from the rules' repos if none exists). This is the upsert: it replaces the
/// project's BASE rules (selections / cross-repo / process) and leaves custom rules
/// untouched.
fn save_armed_to_project(
    state: &AppState,
    rules: &[crate::arm::ArmRule],
    custom: &[crate::project::CustomRule],
) {
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
    // Repo-scoped custom rules pull their repo into the project too (covers a custom-only apply).
    for c in custom {
        let d = c.domain.trim();
        if !d.is_empty() && d != "*" {
            all_repos.insert(d.to_string());
        }
    }
    let pid = match state.projects.active() {
        Some(p) => p.id,
        None => match state
            .projects
            .create("My project", all_repos.iter().cloned().collect())
        {
            Some(p) => p.id,
            None => return,
        },
    };
    state.projects.update(&pid, |p| {
        p.upsert_base_rules(selections, cross, process);
        // Upsert custom rules by name (preserve any not in this apply).
        for c in custom {
            if let Some(slot) = p.ruleset.custom.iter_mut().find(|x| x.name == c.name) {
                *slot = c.clone();
            } else {
                p.ruleset.custom.push(c.clone());
            }
        }
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
            results.push(
                serde_json::json!({ "repo": repo, "ok": false, "message": "not owner/repo" }),
            );
            continue;
        };
        let repo_rules: Vec<&crate::arm::ArmRule> = rules
            .iter()
            .filter(|r| r.repos.iter().any(|x| x == repo))
            .collect();
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
            Err(e) => results
                .push(serde_json::json!({ "repo": repo, "ok": false, "message": format!("{e}") })),
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
            let enforcement = rule.enforcement.as_str();
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
        return Json(
            serde_json::json!({ "ok": false, "message": "Nothing to emit — this project has no repo-local rules or custom rules yet." }),
        );
    }
    // Re-emit carries no new baseline (it's a ruleset refresh, not onboarding).
    let no_baselines = std::collections::HashMap::new();
    let results = emit_to_repos(
        &project.repos,
        &rules,
        &project.ruleset.custom,
        &no_baselines,
        &token,
    )
    .await;
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
    state
        .stories
        .upsert(story.clone())
        .await
        .map_err(AppError)?;
    Ok(Json(story))
}

// ── GitHub Issue intake (#20) ─────────────────────────────────────────────────

/// Query for `GET /api/github/issues` — the `owner/repo` whose open issues to list.
#[derive(serde::Deserialize)]
struct GithubIssuesQuery {
    repo: String,
}

/// List a repo's OPEN GitHub issues for the adopt picker. Gated on
/// `CAMERATA_GITHUB_TOKEN`: with no token (or an unreachable API / bad repo) this
/// returns `{ ok: false, issues: [], message }` with an empty list — it NEVER
/// errors out or panics, so the UI degrades to a "Connect GitHub" hint. The token
/// is never echoed back. Pull requests are filtered out by the parser.
async fn github_issues_list(
    axum::extract::Query(q): axum::extract::Query<GithubIssuesQuery>,
) -> Json<serde_json::Value> {
    let repo = q.repo.trim();
    if repo.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "issues": [],
            "message": "Provide a repo as `owner/name`.",
        }));
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(serde_json::json!({
            "ok": false,
            "issues": [],
            "message": "Connect GitHub to list issues.",
        }));
    };
    match crate::github_issues::list_open_issues(repo, &token).await {
        Ok(issues) => Json(serde_json::json!({ "ok": true, "issues": issues })),
        // Surface a redacted error message — never the token, never the raw URL.
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "issues": [],
            "message": format!("Could not list issues for {repo}: {e}"),
        })),
    }
}

/// Request to adopt a specific GitHub issue onto the spine. The title/body are sent
/// from the picker (already fetched in the list call) so adoption needs no second
/// round-trip to GitHub.
#[derive(serde::Deserialize)]
struct AdoptIssueReq {
    /// The source repo as `owner/name`.
    repo: String,
    /// The issue number.
    number: u64,
    /// The issue title.
    #[serde(default)]
    title: String,
    /// The issue body (markdown). May be empty.
    #[serde(default)]
    body: String,
}

/// Adopt a GitHub issue (including an onboarding-emitted one) into the canonical
/// story spine: map it to a `CanonicalStory` with an `ExternalRef` pointing at the
/// issue and upsert it into the `StoryStore`. Upsert is idempotent — re-adopting the
/// same issue refreshes the spine row rather than duplicating it. This path is
/// token-free (the issue fields travel in the request), so it works the same in a
/// test as in production.
async fn adopt_issue(
    State(state): State<AppState>,
    Json(req): Json<AdoptIssueReq>,
) -> Result<Json<CanonicalStory>, AppError> {
    let repo = req.repo.trim();
    if camerata_worktracker::RepoCoord::parse(repo).is_none() {
        return Err(AppError(anyhow::anyhow!(
            "repo must be `owner/name`, got `{repo}`"
        )));
    }
    let story = crate::github_issues::issue_to_story(repo, req.number, &req.title, &req.body);
    state
        .stories
        .upsert(story.clone())
        .await
        .map_err(AppError)?;
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
    let children = crate::decompose::propose_ai(&parent, &Practice::default_feature(), &llm).await;
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
    let user = format!(
        "Story: {}\n\nDescription: {}",
        story.title, story.description
    );
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
        state
            .stories
            .upsert(child.clone())
            .await
            .map_err(AppError)?;
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
        .complete(
            crate::llm::LlmRequest::new(user)
                .with_model(req.model)
                .with_system(system),
        )
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

/// List available routine templates (preset configurations for common automation patterns).
async fn list_routine_templates() -> Json<Vec<crate::routine::RoutineTemplate>> {
    Json(crate::routine::builtin_templates())
}

/// Instantiate a routine from a template: given a template id, return a fresh Routine
/// prefilled with the template's defaults, ready for the architect to review and customize.
/// The routine is NOT yet persisted; the caller (UI form) passes it to the normal create
/// flow if the architect approves.
async fn instantiate_routine_from_template(
    Path(template_id): Path<String>,
) -> Result<Json<crate::routine::Routine>, AppError> {
    let templates = crate::routine::builtin_templates();
    let template = templates
        .iter()
        .find(|t| t.id == template_id)
        .ok_or_else(|| AppError(anyhow::anyhow!("template not found: {template_id}")))?;
    Ok(Json(crate::routine::instantiate_from_template(template)))
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

/// Provision a routine on this backend (the "Set up" action for one that arrived via a
/// project import). Registers it with the scheduler; does not enable it — the architect
/// still presses Start.
async fn provision_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .set_provisioned(&id)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("routine not found: {id}")))
}

/// Run a routine now (a governed run via the real gate; records the summary). If the run
/// is blocked (gate denials), raise a human-review escalation — same hook the scheduler
/// uses, so a blocked routine surfaces a review whether it fired on a timer or by hand.
async fn run_routine_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Routine>, AppError> {
    let routine = state
        .routines
        .run_now(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("routine not found: {id}")))?;
    crate::escalation::raise_if_blocked(&state.escalations, &routine);
    Ok(Json(routine))
}

/// Query for `GET /api/escalations`: `?open=true` returns only open reviews.
#[derive(serde::Deserialize)]
struct EscalationListQuery {
    #[serde(default)]
    open: bool,
}

/// List escalations (all, or only open ones with `?open=true`).
async fn list_escalations(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EscalationListQuery>,
) -> Json<Vec<crate::escalation::Escalation>> {
    if q.open {
        Json(state.escalations.list_open())
    } else {
        Json(state.escalations.list())
    }
}

/// Raise an escalation against a routine (deduped per routine). The routine's display name
/// is denormalized in from the routine store.
async fn raise_escalation(
    State(state): State<AppState>,
    Json(req): Json<crate::escalation::RaiseEscalationReq>,
) -> Result<Json<crate::escalation::Escalation>, AppError> {
    let name = state
        .routines
        .list()
        .into_iter()
        .find(|r| r.id == req.routine_id)
        .map(|r| r.name)
        .ok_or_else(|| AppError(anyhow::anyhow!("routine not found: {}", req.routine_id)))?;
    Ok(Json(state.escalations.raise_deduped(req, &name)))
}

/// Body for a turn in the escalation review conversation: the human's message + the model
/// the lead-engineer agent should answer on (blank -> server default).
#[derive(serde::Deserialize)]
struct ChatEscalationReq {
    message: String,
    #[serde(default)]
    model: String,
}

/// One turn of the human <-> lead-engineer review conversation. The agent is grounded on
/// the escalation and is instructed NOT to act — only `answer` (authorization) unblocks.
/// Persists both the human message and the reply, and returns the updated escalation.
async fn chat_escalation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ChatEscalationReq>,
) -> Result<Json<crate::escalation::Escalation>, AppError> {
    let esc = state
        .escalations
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("escalation not found: {id}")))?;
    let system = crate::escalation::chat_system_prompt(&esc);
    let user = crate::escalation::chat_user_prompt(&esc, &req.message);
    let llm = crate::llm::Llm::from_env();
    let reply = match llm
        .complete(
            crate::llm::LlmRequest::new(user)
                .with_model(req.model)
                .with_system(system),
        )
        .await
    {
        Ok(r) if !r.text.trim().is_empty() => r.text,
        _ => "I couldn't reach the model just now. You can still authorize a decision \
              below, or try again."
            .to_string(),
    };
    state
        .escalations
        .append_turn(&id, &req.message, &reply)
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("escalation not found: {id}")))
}

/// Resolve an escalation with the human's answer. The answer is run through the
/// AI-translation step (issue #43) — the lead-engineer agent restates it as a precise,
/// structured resume payload on the model the caller selects — and stored on the (now
/// resolved) escalation. On any model failure it falls back to the deterministic scaffold,
/// so resolving never dead-ends. The blocked routine is returned to `Idle` so its next slot
/// can run.
async fn answer_escalation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<crate::escalation::AnswerEscalationReq>,
) -> Result<Json<crate::escalation::Escalation>, AppError> {
    let esc = state
        .escalations
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("escalation not found: {id}")))?;
    if esc.status != crate::escalation::EscalationStatus::Open {
        return Err(AppError(anyhow::anyhow!("no open escalation: {id}")));
    }
    // Translate the human answer into a structured resume payload via the real LLM seam
    // (model-selectable, with a deterministic fallback inside translate_answer_ai).
    let driver = crate::escalation::LlmTranslator {
        llm: crate::llm::Llm::from_env(),
    };
    let model = state
        .routines
        .list()
        .into_iter()
        .find(|r| r.id == esc.routine_id)
        .map(|r| r.model)
        .unwrap_or_default();
    let payload = crate::escalation::translate_answer_ai(&driver, &esc, &req.answer, &model).await;
    let resolved = state
        .escalations
        .resolve_with_payload(&id, &req.answer, &payload)
        .ok_or_else(|| AppError(anyhow::anyhow!("no open escalation: {id}")))?;
    // The block is cleared: return the routine to Idle so the scheduler can run its next
    // slot (the directive is recorded on the resolved escalation for the resumed run to
    // consult).
    let _ = state
        .routines
        .set_status(&resolved.routine_id, crate::routine::RoutineStatus::Idle);
    Ok(Json(resolved))
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
    Ok(Json(
        crate::workspace::checkout_status(&root, &req.repo).await,
    ))
}

#[derive(serde::Deserialize)]
struct ShipReq {
    repo: String,
    branch: String,
    title: String,
    #[serde(default)]
    body: String,
    /// An optional governed-run id (issue #21). When set, that run's provenance
    /// summary is appended to the PR body so the PR carries the honest accounting of
    /// what the gate enforced and bounced. Opening the PR stays an EXPLICIT action.
    #[serde(default)]
    run_id: Option<String>,
}

/// Ship a repo: push its working branch and open a PR. Returns the PR URL. This is the
/// EXPLICIT open-PR action — Camerata never auto-opens PRs. When a `run_id` is supplied,
/// that run's provenance is folded into the PR body (issue #21).
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
    let body = body_with_provenance(&state, &req.body, req.run_id.as_deref());
    let root = std::path::PathBuf::from(root);
    let url =
        crate::workspace::ship(&req.repo, &req.branch, &req.title, &body, &root, &token).await?;
    Ok(Json(serde_json::json!({ "pr_url": url })))
}

/// Append a run's provenance markdown to a PR body when a run id is supplied and the
/// run exists. Returns the original body unchanged otherwise. Keeps the provenance the
/// architect reviewed visible in the PR itself (issue #21).
fn body_with_provenance(state: &AppState, body: &str, run_id: Option<&str>) -> String {
    let Some(rid) = run_id.filter(|r| !r.trim().is_empty()) else {
        return body.to_string();
    };
    let Some(run) = state.runs.get(rid) else {
        return body.to_string();
    };
    let rules = camerata_gateway::enforced_gate_rules();
    let prov = run_provenance(&run, &rules);
    let block = crate::run::provenance_markdown(&prov);
    if body.trim().is_empty() {
        block
    } else {
        format!("{body}\n\n{block}")
    }
}

// ── Local git controls (issue #37) ───────────────────────────────────────────

/// Query parameters shared by git read endpoints.
#[derive(serde::Deserialize)]
struct GitRepoQuery {
    repo: String,
}

/// Query parameters for the commit log.
#[derive(serde::Deserialize)]
struct GitLogQuery {
    repo: String,
    #[serde(default = "default_log_limit")]
    limit: usize,
}

fn default_log_limit() -> usize {
    50
}

/// Resolve a repo's local dir from project settings, or return an error response.
fn resolve_git_dir(
    state: &AppState,
    repo: &str,
) -> Result<std::path::PathBuf, Json<serde_json::Value>> {
    let override_path = state.settings.repo_path(repo);
    let workspace_root = state.settings.workspace_root();
    crate::workspace::resolve_repo_dir(override_path.as_deref(), workspace_root.as_deref(), repo)
        .ok_or_else(|| {
            Json(serde_json::json!({
                "ok": false,
                "message": "repo not resolved locally — set its path in the Rules view"
            }))
        })
}

/// List local branches + the current HEAD branch for a repo.
async fn git_branches(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<GitRepoQuery>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &q.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::list_branches(&dir).await {
        Ok(bl) => {
            Json(serde_json::json!({ "ok": true, "current": bl.current, "branches": bl.branches }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Full git status for a repo: branch, dirty flag, ahead/behind counts, and
/// a human-readable detail string. Used by the cockpit's per-repo status bar.
/// No network: ahead/behind reflects what was fetched locally.
async fn git_status_endpoint(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<GitRepoQuery>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &q.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::git_status(&dir).await {
        Ok(st) => Json(serde_json::json!({
            "ok":     true,
            "branch": st.branch,
            "dirty":  st.dirty,
            "ahead":  st.sync.ahead,
            "behind": st.sync.behind,
            "detail": st.detail,
        })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Recent commit log for a repo.
async fn git_log(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<GitLogQuery>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &q.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::git_log(&dir, q.limit).await {
        Ok(commits) => Json(serde_json::json!({ "ok": true, "commits": commits })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
struct GitCheckoutReq {
    repo: String,
    branch: String,
    #[serde(default)]
    create: bool,
}

/// Switch to (or create) a local branch.
async fn git_checkout(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitCheckoutReq>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let result = if req.create {
        crate::workspace::create_branch_at(&dir, &req.branch).await
    } else {
        crate::workspace::switch_branch(&dir, &req.branch).await
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true, "branch": req.branch })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
struct GitCommitReq {
    repo: String,
    message: String,
}

/// Stage all changes and commit them.
async fn git_commit(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitCommitReq>,
) -> Json<serde_json::Value> {
    if req.message.trim().is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "commit message is required" }));
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::commit_all(&dir, &req.message).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
struct GitPushReq {
    repo: String,
    branch: String,
}

/// Push the branch to origin (user-triggered; token required).
async fn git_push(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitPushReq>,
) -> Json<serde_json::Value> {
    let token = std::env::var("CAMERATA_GITHUB_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "no GitHub token — set CAMERATA_GITHUB_TOKEN to push" }),
        );
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::push_branch(&dir, &req.repo, &req.branch, &token).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
struct GitPullReq {
    repo: String,
    branch: String,
}

/// Fast-forward pull from origin.
async fn git_pull(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitPullReq>,
) -> Json<serde_json::Value> {
    let token = std::env::var("CAMERATA_GITHUB_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "no GitHub token — set CAMERATA_GITHUB_TOKEN to pull" }),
        );
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::pull_branch(&dir, &req.repo, &req.branch, &token).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
struct GitCherryPickReq {
    repo: String,
    sha: String,
}

/// Cherry-pick a commit onto the current HEAD branch. On conflict, returns the error
/// message so the UI can display it (the repo stays in conflict state for the user to
/// resolve).
async fn git_cherry_pick(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitCherryPickReq>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::cherry_pick(&dir, &req.sha).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

// ── Unit of Work handlers (issue #39) ────────────────────────────────────────

/// All known UoWs across every story.
async fn uow_list(State(state): State<AppState>) -> Json<Vec<crate::uow::UnitOfWork>> {
    Json(state.uow.list())
}

/// The UoW for a story. Creates a default one if the story has no UoW yet.
async fn uow_get(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.get_or_create(&story_id))
}

#[derive(serde::Deserialize)]
struct UowStatusReq {
    /// Accepted values: `"new"`, `"in_progress"`, `"done"`.
    status: String,
}

/// Set the dev status for a story's UoW.
async fn uow_set_status(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowStatusReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let status = crate::uow::DevStatus::from_wire(&req.status).ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "unknown status {:?}; expected new, in_progress, done",
            req.status
        ))
    })?;
    state.uow.set_status(&story_id, status);
    Ok(Json(state.uow.get_or_create(&story_id)))
}

#[derive(serde::Deserialize)]
struct UowBranchReq {
    /// The branch name, or `null` to clear it.
    branch: Option<String>,
}

/// Set (or clear) the branch for a story's UoW.
async fn uow_set_branch(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowBranchReq>,
) -> Json<crate::uow::UnitOfWork> {
    state.uow.set_branch(&story_id, req.branch);
    Json(state.uow.get_or_create(&story_id))
}

#[derive(serde::Deserialize)]
struct UowHistoryReq {
    kind: String,
    text: String,
}

/// Append an entry to the AI development history for a story's UoW.
async fn uow_append_history(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowHistoryReq>,
) -> Json<crate::uow::UnitOfWork> {
    state.uow.append_history(&story_id, &req.kind, &req.text);
    Json(state.uow.get_or_create(&story_id))
}

// ── Project-aware chat grounding (#54) ───────────────────────────────────────

/// The pre-onboard phase: the active project has a saved onboarding draft but has not yet
/// completed the Apply step for any repo.
///
/// The draft is a UI-owned blob; we surface its raw JSON as the grounding text so the
/// project-aware chat can reference what the architect was doing. Callers should treat the
/// draft as informational context, not canonical state.
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPhase {
    /// Project has no saved draft and no onboarded repos — truly blank.
    Blank,
    /// An onboarding draft exists but Apply has not yet been completed for any repo.
    PreOnboard,
    /// At least one repo is onboarded (Apply completed).
    PostOnboard,
}

/// The grounding payload the project-aware chat mode uses to build its system prompt.
///
/// The UI fetches this once when the Project tab is opened and injects its fields into
/// the system prompt it sends to `POST /api/chat`. This keeps the LLM call on the
/// existing `chat` handler — no separate AI endpoint is needed here.
#[derive(serde::Serialize)]
pub struct ProjectContextResponse {
    /// Whether the project exists (false when there is no active project).
    pub ok: bool,
    /// Onboarding phase determines which grounding fields are populated.
    pub phase: ProjectPhase,
    /// The active project's name (present when `ok=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// The repos in scope for the active project.
    #[serde(default)]
    pub repos: Vec<String>,
    /// The repos that have completed onboarding.
    #[serde(default)]
    pub onboarded: Vec<String>,
    /// A compact, human-readable summary of the project's selected ruleset (post-onboard).
    /// Each line is `<rule_id>: <scope>` for easy citation in the chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruleset_summary: Option<String>,
    /// The number of active findings from the onboarding audit. Populated post-onboard
    /// when the project has findings recorded in the draft (the draft carries the last audit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding_count: Option<usize>,
    /// A compact listing of the most recent findings (up to 50), suitable for injection
    /// into a system prompt. Each entry is a compact one-line summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings_summary: Option<String>,
    /// The raw onboarding draft (pre-onboard: the architect's in-progress session).
    /// This is the UI-owned JSON blob; it is surfaced as-is for the chat to reference.
    /// Only populated in the PreOnboard phase so the Post-onboard prompt is not cluttered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_json: Option<serde_json::Value>,
    /// A human-readable message explaining why the context is limited (e.g., no active
    /// project, or the project has not been onboarded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Build a compact one-line ruleset summary: `<rule_id>: <scope>` per selected rule.
/// Cross-repo and process rules are tagged accordingly so the chat can explain the
/// distinction.
fn build_ruleset_summary(ruleset: &crate::project::ProjectRuleset) -> String {
    let mut lines = Vec::new();
    for sel in &ruleset.selections {
        let note = if sel.repos.is_empty() {
            "(all repos)".to_string()
        } else {
            format!("({})", sel.repos.join(", "))
        };
        lines.push(format!("{}: repo-local {}", sel.rule_id, note));
    }
    for sel in &ruleset.cross_repo {
        lines.push(format!("{}: cross-repo", sel.rule_id));
    }
    for sel in &ruleset.process {
        lines.push(format!("{}: process (VCS workflow)", sel.rule_id));
    }
    for c in &ruleset.custom {
        let dom = if c.domain.is_empty() || c.domain == "*" {
            "all repos".to_string()
        } else {
            c.domain.clone()
        };
        lines.push(format!("CUSTOM-{}: custom rule ({})", c.name, dom));
    }
    lines.join("\n")
}

/// Extract a compact findings summary from a draft JSON blob (the UI-owned onboarding draft).
/// Looks for the `findings` array inside the audit section; returns a one-line-per-finding
/// compact listing, capped at 50 findings to keep the prompt manageable.
fn extract_findings_from_draft(
    draft: &serde_json::Value,
) -> Option<(usize, String)> {
    // The draft shape is UI-owned; we look for `audit.findings` or `scan.findings`
    // (the audit section, which contains the Phase-2 AI audit findings).
    let findings = draft
        .get("audit")
        .and_then(|a| a.get("findings"))
        .or_else(|| draft.get("scan").and_then(|s| s.get("findings")))
        .and_then(|f| f.as_array())?;

    if findings.is_empty() {
        return None;
    }
    let total = findings.len();
    let cap = total.min(50);
    let mut lines = Vec::with_capacity(cap);
    for f in findings.iter().take(cap) {
        let repo = f.get("repo").and_then(|v| v.as_str()).unwrap_or("?");
        let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let line = f.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let rule_id = f.get("rule_id").and_then(|v| v.as_str()).unwrap_or("?");
        let severity = f.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
        let detail = f
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        lines.push(format!(
            "[{severity}] {rule_id} in {repo}/{path}:{line} — {detail}"
        ));
    }
    Some((total, lines.join("\n")))
}

/// `GET /api/projects/active/context` — the grounding payload for the project-aware chat
/// mode. No AI call; purely a read of the active project's current state (draft + ruleset).
///
/// The UI injects the returned fields into the system prompt it sends to `POST /api/chat`.
/// This keeps the AI call on the existing chat handler and this endpoint purely informational.
async fn active_project_context(
    State(state): State<AppState>,
) -> Json<ProjectContextResponse> {
    let Some(project) = state.projects.active() else {
        return Json(ProjectContextResponse {
            ok: false,
            phase: ProjectPhase::Blank,
            project_name: None,
            repos: Vec::new(),
            onboarded: Vec::new(),
            ruleset_summary: None,
            finding_count: None,
            findings_summary: None,
            draft_json: None,
            message: Some(
                "No active project — create one to use project-aware chat.".to_string(),
            ),
        });
    };

    let draft = state.draft.load(&project.id);

    if !project.onboarded.is_empty() {
        // Post-onboard: at least one repo has been fully onboarded. Surface the live ruleset
        // + any findings captured in the draft (the last audit the architect ran).
        let ruleset_summary = build_ruleset_summary(&project.ruleset);
        let ruleset_summary = if ruleset_summary.is_empty() {
            None
        } else {
            Some(ruleset_summary)
        };
        let (finding_count, findings_summary) = draft
            .as_ref()
            .and_then(|d| extract_findings_from_draft(d))
            .map(|(n, s)| (Some(n), Some(s)))
            .unwrap_or((None, None));
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::PostOnboard,
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: project.onboarded.clone(),
            ruleset_summary,
            finding_count,
            findings_summary,
            draft_json: None, // Don't inject the full draft post-onboard (noisy).
            message: None,
        })
    } else if draft.is_some() {
        // Pre-onboard with a saved draft: the architect is mid-onboarding. Surface the draft
        // as-is so the chat can help interpret the in-progress onboarding state.
        let (finding_count, findings_summary) = draft
            .as_ref()
            .and_then(|d| extract_findings_from_draft(d))
            .map(|(n, s)| (Some(n), Some(s)))
            .unwrap_or((None, None));
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::PreOnboard,
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: Vec::new(),
            ruleset_summary: None, // No committed ruleset yet.
            finding_count,
            findings_summary,
            draft_json: draft,
            message: Some(format!(
                "Project '{}' has an in-progress onboarding (scan/audit) that has not been applied yet.",
                project.name
            )),
        })
    } else {
        // Blank: project exists but no draft and no onboarded repos.
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::Blank,
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: Vec::new(),
            ruleset_summary: None,
            finding_count: None,
            findings_summary: None,
            draft_json: None,
            message: Some(format!(
                "Project '{}' has no scan or onboarding data yet — start an onboarding scan to populate the project context.",
                project.name
            )),
        })
    }
}

/// Replace the full set of decision records on a story's UoW. The governed-dev gate
/// reads these to decide whether a run may start; the cockpit posts them when the
/// investigation surfaces (or the architect resolves) decisions. Body is the JSON
/// array of `DecisionRecord`s (the same shape `camerata-worktracker` serializes).
async fn uow_set_decisions(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(decisions): Json<Vec<camerata_worktracker::investigation::DecisionRecord>>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.set_decisions(&story_id, decisions))
}

/// Helper: map a lifecycle [`crate::lifecycle::TransitionError`] to a 409 CONFLICT with
/// its human-readable message, so the cockpit surfaces exactly why a stage move was
/// blocked instead of a generic 500.
fn transition_response(
    result: Result<crate::uow::UnitOfWork, crate::lifecycle::TransitionError>,
) -> Response {
    match result {
        Ok(uow) => Json(uow).into_response(),
        Err(err) => {
            let body = Json(serde_json::json!({
                "error": "lifecycle transition blocked",
                "reason": err.message(),
                "detail": err,
            }));
            (StatusCode::CONFLICT, body).into_response()
        }
    }
}

/// Drive the UoW Intake → Investigating (Pillar 2). 409 if the UoW is not at Intake.
async fn uow_begin_investigation(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Response {
    transition_response(state.uow.begin_investigation(&story_id))
}

/// Drive the UoW Investigating → DecisionsApproved (Pillar 2), gated by the story's
/// decision records. 409 (with the precise reason) if the gate is not satisfied or the
/// UoW is at the wrong stage.
async fn uow_approve_decisions(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Response {
    transition_response(state.uow.approve_decisions(&story_id))
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
            &[],
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
            }]);
        });
        // Arming (saving base rules) must keep the custom rule.
        save_armed_to_project(
            &state,
            &[arm_rule("REPO-1", "repo-local", &["me/api"])],
            &[],
        );
        let after = state.projects.get(&p.id).unwrap();
        assert_eq!(after.ruleset.selections.len(), 1);
        assert_eq!(
            after.ruleset.custom.len(),
            1,
            "custom survived the re-arm upsert"
        );
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
    #[tokio::test]
    async fn adopt_issue_rejects_a_malformed_repo() {
        let app = router(AppState::new(
            std::sync::Arc::new(InMemoryStoryStore::new()),
        ));
        let body =
            serde_json::json!({ "repo": "not-a-repo", "number": 1, "title": "x", "body": "" })
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
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// #20: with no GitHub token the list endpoint degrades gracefully — `ok:false`,
    /// an empty list, and a hint — instead of erroring or panicking.
    #[tokio::test]
    async fn github_issues_list_is_token_optional_and_never_panics() {
        // Ensure no token is visible to this test process.
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
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
        assert_eq!(state.uow.get_or_create(story).stage, lifecycle::UowStage::Intake);
    }

    #[tokio::test]
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
}
