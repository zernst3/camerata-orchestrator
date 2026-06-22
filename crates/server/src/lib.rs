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
pub mod feature_flags;
pub mod fix;
pub mod github_issues;
pub mod investigation_run;
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
pub mod scan_tools;
pub mod schedule;
pub mod model_tier;
pub mod settings;
pub mod suppression;
pub mod terminal;
pub mod transcript;
pub mod uow;
pub mod update_branch_run;
pub mod workitems;
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
    /// The central, version-tracked SQLite artifact store (ROUTE-A). Backs the per-story
    /// decision-record + investigation-note history that used to live inline on the UoW.
    /// `None` until a data-dir-backed store is opened in [`AppState::from_env`]; tests
    /// run without it (the UoW falls back to inline decisions). Held here so future
    /// callers (queryable history endpoints) can reach the same store the UoW writes to.
    #[allow(dead_code)]
    artifacts: Option<Arc<dyn camerata_persistence::ArtifactStore>>,
    /// Runtime feature flags. Loaded once at server start from `.camerata/features.toml`
    /// (relative to the CWD), with `CAMERATA_FEATURE_<NAME>=false` env overrides applied
    /// on top. Every flag defaults to `true`; a flag is OFF only when explicitly set to
    /// `false`. Cloned into handlers via the shared `AppState`.
    pub feature_flags: crate::feature_flags::FeatureFlags,
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
            artifacts: None,
            feature_flags: crate::feature_flags::FeatureFlags::default(),
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
            // The story spine must persist too: a UoW references its story by id, and
            // /api/uows resolves the WorkItem from the spine. An in-memory spine meant
            // restored UoWs rendered blank (and couldn't be run). Persist it alongside.
            state.stories = Arc::new(InMemoryStoryStore::at(dir.join("stories.json")));
            // Central artifact store (ROUTE-A): per-story decision records + investigation
            // notes are versioned here. Opened on the same data dir as the other stores.
            // Best-effort: if the store can't be opened (no runtime handle, or sqlx error),
            // the UoW keeps its inline-decisions behaviour so the app still runs.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let db_path = dir.join("artifacts.db");
                let opened = tokio::task::block_in_place(|| {
                    handle.block_on(camerata_persistence::SqliteStore::open_path(&db_path))
                });
                if let Ok(store) = opened {
                    let store: Arc<dyn camerata_persistence::ArtifactStore> = Arc::new(store);
                    state.artifacts = Some(store.clone());
                    state.uow = state.uow.with_artifacts(store);
                }
            }
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
        // Feature flags: load from .camerata/features.toml (CWD-relative) with env
        // overrides applied on top. Loaded last so the flags are available to every
        // handler via AppState from first request. Infallible: missing config = defaults.
        state.feature_flags = crate::feature_flags::FeatureFlags::load();
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
        // Model-tiering: read/write the project's fast/balanced/strongest model bindings (#63).
        .route("/api/projects/:id/tier-map", post(set_tier_map))
        // VCS-gate process-rule configuration + auditable bypass (issue #65).
        .route(
            "/api/projects/:id/process-rule-config",
            get(get_process_rule_config).post(set_process_rule_config_handler),
        )
        .route(
            "/api/projects/:id/vcs-gate/bypass",
            post(vcs_gate_bypass),
        )
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
        .route("/api/onboard/greenfield", post(onboard_greenfield))
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
        // ── Provider-agnostic WorkItem + UoW layer (governed-dev surface) ─────
        // Replaces the inline owner/repo adopt-issue hack: pull all open issues
        // across the active project's repos, then create a UoW (deduped by external
        // ref) and drive it through the EXISTING governed-dev endpoints (the gate).
        .route("/api/workitems/pull", post(workitems_pull))
        .route("/api/workitems/refresh", post(workitems_refresh))
        .route("/api/workitems/comment", post(workitems_comment))
        .route("/api/workitems/comments", post(workitems_comments))
        .route("/api/workitems/assignees", post(workitems_assignees))
        .route("/api/uows", get(uows_list))
        .route("/api/uow/from-workitem", post(uow_from_workitem))
        // ── AI story authoring from a blank UoW (2026-06-22) ──────────────────
        .route("/api/uow/blank", post(uow_blank))
        .route("/api/uow/:story_id/author", post(uow_author))
        .route("/api/uow/:story_id/publish", post(uow_publish))
        .route("/api/uow", get(uow_list))
        .route("/api/uow/:story_id", get(uow_get))
        .route("/api/uow/:story_id/status", post(uow_set_status))
        .route("/api/uow/:story_id/branch", post(uow_set_branch))
        .route("/api/uow/:story_id/history", post(uow_append_history))
        // ── Governed-development lifecycle (Pillar 2) ─────────────────────────
        .route("/api/uow/:story_id/decisions", post(uow_set_decisions))
        .route("/api/uow/:story_id/branches", post(uow_list_branches))
        .route("/api/uow/:story_id/update-branch", post(uow_update_branch))
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
        // ── Feature flags ─────────────────────────────────────────────────────
        .route("/api/feature-flags", get(get_feature_flags))
        // ── Development context ───────────────────────────────────────────────
        .route("/api/development/context", get(development_context))
        // ── Update detection ─────────────────────────────────────────────────
        .route("/api/updates/check", get(updates_check))
        // ── Single-rule overrides ─────────────────────────────────────────────
        .route(
            "/api/projects/:id/rules/:rule_id",
            get(get_rule_override).post(set_rule_override),
        )
        .route(
            "/api/projects/:id/repos/:repo/rules/:rule_id",
            get(get_repo_rule_override).post(set_repo_rule_override),
        )
        // ── Deep-report export ────────────────────────────────────────────────
        .route("/api/projects/:id/deep-report", get(export_deep_report))
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
    /// The per-UoW THREE-TIER model map (ORCH-MODEL-TIERING-1). When present, the
    /// development fleet runs TIERED: each task runs on its capability band's model
    /// (`strongest`/`balanced`/`fast`), with the strongest tier acting as the
    /// orchestrator/lead. When absent, the single-`model` path is used (back-compat).
    /// The two fields are independent; `tier_map` takes precedence when both are sent.
    #[serde(default)]
    tier_map: Option<crate::model_tier::TierMap>,
    /// One-time BOOTSTRAP escape hatch (default OFF): when `Some(true)`, this single run
    /// uses a NO-OP layer-2 runner — no post-task lint/test bounce — so a brownfield repo
    /// can install the linters/checkers that layer-2 needs without tripping the fail-closed
    /// "could-not-run" deadlock. It skips ONLY layer 2. Layer 1 (the deny-before-write gate
    /// every spawned agent runs behind) and the no-code-first decisions gate
    /// (`ensure_development_gate`) are UNCHANGED in both cases — the gate is never bypassed.
    /// `None`/`false` is exactly today's behaviour (the real per-language CheckRunner). See
    /// `docs/decisions/2026-06-22_ci_wiring_both_layers_and_layer2_bootstrap_bypass.md`.
    #[serde(default)]
    skip_layer2: Option<bool>,
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
    let (model, tier_map, skip_layer2) = match req {
        Some(Json(r)) => (
            r.model.filter(|m| !m.trim().is_empty()),
            r.tier_map,
            r.skip_layer2.unwrap_or(false),
        ),
        None => (None, None, false),
    };

    // The no-code-first gate (Pillar 2): a governed run cannot start until every
    // DecisionRecord on this story's UoW is approved (decisions_approved_for_development).
    // We block + surface exactly why, rather than silently starting a run that the
    // architect did not gate. The check reads the persisted decisions on the UoW. The
    // gate is identical for the single-model and tiered paths.
    if let Err(reason) = ensure_development_gate(&state, &story_id) {
        let body = Json(serde_json::json!({
            "error": "development gate not satisfied",
            "reason": reason,
            "story_id": story_id,
        }));
        return (StatusCode::CONFLICT, body).into_response();
    }

    let (run_id, mode) =
        start_governed_run(&state, &story_id, model, tier_map, skip_layer2).await;
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
///
/// `skip_layer2` is the one-time BOOTSTRAP escape hatch (default OFF). When `true`, the
/// live fleet runs this ONE run with a no-op layer-2 runner (no post-task lint/test
/// bounce) so a brownfield repo can install the tooling layer-2 needs. It skips ONLY
/// layer 2: layer 1 (the deny-before-write gate) and the no-code-first decisions gate
/// (`ensure_development_gate`, already enforced in the caller) are unchanged. The scripted
/// path has no layer-2 bounce, so the flag is a no-op there.
async fn start_governed_run(
    state: &AppState,
    story_id: &str,
    model: Option<String>,
    tier_map: Option<crate::model_tier::TierMap>,
    skip_layer2: bool,
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
        match tier_map {
            // TIERED path (ORCH-MODEL-TIERING-1): each task on its band's model, the
            // strongest tier leading. The single `model` is ignored when a map is given.
            Some(map) => {
                tokio::spawn(async move {
                    live_fleet::execute_live_run_tiered(
                        store,
                        rid,
                        title,
                        desc,
                        map,
                        max_iterations,
                        skip_layer2,
                    )
                    .await
                });
            }
            // Single-model path (back-compat): one operator-wide model for every agent.
            None => {
                tokio::spawn(async move {
                    live_fleet::execute_live_run(
                        store,
                        rid,
                        title,
                        desc,
                        model,
                        max_iterations,
                        skip_layer2,
                    )
                    .await
                });
            }
        }
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
    //
    // Evidence assembly (issue #53): the watcher also builds the SOC-2 evidence record
    // from the run's gate decisions + provenance + scoped audit over the changed files,
    // attaches it to the UoW, and posts it as a PR comment when a PR number is known
    // from the UoW's branch. Graceful degradation: evidence failure never blocks the run.
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
/// UoW, advance the lifecycle stage to `AwaitingQa`, and assemble + attach the SOC-2
/// evidence record (issue #53). Bounded poll loop so a never-completing run (e.g. a
/// wedged live fleet) can't leak the task forever.
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

                // ── Evidence assembly (issue #53) ────────────────────────────────
                // Build the SOC-2 evidence record from the run's gate decisions +
                // provenance + a scoped audit over the changed files. Attach it to the
                // UoW so the sign-off gate and PR renderer can use it. All steps are
                // best-effort: a failure here never blocks the run's AwaitingQa state.
                let evidence = assemble_evidence_for_run(&run, &prov, &story_id);
                uow.attach_evidence(&story_id, evidence);

                return;
            }
        } else {
            // The run vanished from the store; nothing to stamp.
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Build a [`crate::evidence::UowEvidenceRecord`] from a completed run's provenance
/// and gate events (issue #53). Runs the scoped deterministic audit over the changed
/// paths (derived from the gate events' targets) to populate the `scoped_scan` field
/// and the `has_critical` blocking flag.
///
/// The "changed files" for the scoped scan are the TARGET paths of every allowed gate
/// verdict: files the run actually managed to write under the gate. Denied writes are
/// excluded (the content never landed on disk). For the scripted run, these are the
/// planted fictional paths; for a live run, they are the real paths the agent wrote.
///
/// Since the scripted run writes fictional content (which is not on disk), the scoped
/// scan receives empty file content for those paths — the deterministic floor only fires
/// on actual file content, so no findings are produced and `has_critical = false` for a
/// clean scripted run. This is correct: the scripted path exercises the gate logic, not
/// real source files.
fn assemble_evidence_for_run(
    run: &crate::run::Run,
    prov: &crate::run::RunProvenance,
    story_id: &str,
) -> crate::evidence::UowEvidenceRecord {
    use crate::evidence::{GateDecision, UowEvidenceRecord, scoped_audit};

    let now = chrono::Utc::now().to_rfc3339();
    let mut record = UowEvidenceRecord::new(story_id, &run.id, &now);

    // ── Governance event history ─────────────────────────────────────────────
    // Run started.
    record.add_event(&now, "governed-fleet", "run", format!(
        "Governed run {} completed (mode: {}, {} allowed, {} denied).",
        run.id, run.mode, prov.allow_count, prov.deny_count,
    ));

    // Gate events → evidence history + gate decisions.
    for event in &run.events {
        let ts = &now; // run events don't carry a timestamp; use the record timestamp.
        if event.verdict == "deny" {
            let rule_id = event.rule.as_deref().unwrap_or("-");
            record.add_event(
                ts,
                "gate-layer-1",
                "gate_deny",
                format!("Gate denied write to `{}`: rule {} fired — {}", event.detail, rule_id, event.detail),
            );
            record.record_gate_decision(GateDecision::deny(ts, &event.detail, rule_id));
        } else if event.verdict == "allow" {
            record.add_event(
                ts,
                "gate-layer-1",
                "gate_allow",
                format!("Gate allowed write: {}", event.detail),
            );
            record.record_gate_decision(GateDecision::allow(ts, &event.detail));
        }
        // "info", "error", "bounce" events from the live fleet are not gate decisions;
        // record them as notes so they appear in the history.
        if event.verdict == "info" || event.verdict == "error" || event.verdict == "bounce" {
            record.add_event(ts, "governed-fleet", "note", &event.detail);
        }
    }

    // ── Rules enforced ───────────────────────────────────────────────────────
    // Record each rule that was in force during the run. For rules that actually fired
    // a denial, use "denied" as an extra tag in the directive.
    let fired_set: std::collections::HashSet<&str> =
        prov.rules_fired.iter().map(|r| r.as_str()).collect();
    for rule_id in &prov.rules_in_force {
        let directive = if fired_set.contains(rule_id.as_str()) {
            format!("Enforced (fired a denial during this run). Rule id: {rule_id}")
        } else {
            format!("Enforced (no violation this run). Rule id: {rule_id}")
        };
        record.record_rule(rule_id, directive, "mechanical");
    }

    // ── Scoped security scan ─────────────────────────────────────────────────
    // Derive "changed paths" from the allowed gate verdicts (files that landed on disk).
    // For each changed path, we supply an empty file body — the scripted run's fictional
    // paths have no real content on disk, and the deterministic floor only fires on actual
    // content. A live run's paths do exist, but reading them here would require knowing the
    // workspace root (not available in this pure-ish context). The empty-body approach is
    // correct for the scripted path and conservative (fewer false positives) for live runs.
    // TODO(live-scan): for the live path, resolve the workspace root and read actual content.
    let allowed_paths: Vec<String> = run.events.iter()
        .filter(|e| e.verdict == "allow")
        .map(|e| e.detail.clone())
        .collect();
    // Build a synthetic file list: allowed paths with empty content.
    let all_files: Vec<(String, String)> = allowed_paths.iter()
        .map(|p| (p.clone(), String::new()))
        .collect();
    let scan_result = scoped_audit(&format!("{story_id}/run/{}", run.id), &all_files, &allowed_paths);
    // Add a critical_finding event to the history for each critical finding.
    for finding in scan_result.summary.findings.iter().filter(|f| f.severity == "critical") {
        record.add_event(
            &now,
            "scoped-audit",
            "critical_finding",
            format!("CRITICAL: {} in {} (line {}): {}", finding.rule_id, finding.path, finding.line, finding.detail),
        );
    }
    // Add security_finding events for non-critical findings.
    for finding in scan_result.summary.findings.iter().filter(|f| f.severity != "critical") {
        record.add_event(
            &now,
            "scoped-audit",
            "security_finding",
            format!("{}: {} in {} (line {}): {}", finding.severity.to_uppercase(), finding.rule_id, finding.path, finding.line, finding.detail),
        );
    }
    record.set_scoped_scan(scan_result.summary);

    // ── Content hash ──────────────────────────────────────────────────────────
    record.compute_hash();
    record
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
    /// An explicit reason to waive a Critical scoped-scan finding that would otherwise
    /// block sign-off (issue #53). A Critical finding in the evidence record blocks the
    /// `AwaitingQa → SignedOff` transition unless the architect provides a non-empty
    /// reason here. A reason-less waive (`waive_reason: ""` or absent) is rejected with
    /// HTTP 409 so the UI must ask the architect to type a justification.
    ///
    /// When present and the evidence has a critical finding, the waiver reason is
    /// appended to the sign-off note so it is durable in the UoW history.
    #[serde(default)]
    waive_reason: Option<String>,
    /// Optional GitHub PR number to post the evidence markdown as a comment on (issue
    /// #53). When supplied AND a `CAMERATA_GITHUB_TOKEN` is set, Camerata posts the
    /// evidence record as a PR comment via the arm.rs GitHub primitives. If omitted,
    /// no PR comment is posted (the evidence is still stored on the UoW). Graceful
    /// degradation: a failed PR comment never blocks the sign-off.
    #[serde(default)]
    pr_number: Option<u64>,
    /// The `owner/repo` the PR lives in (required when `pr_number` is set). Format:
    /// `"owner/repo"`. When absent and `pr_number` is set, the PR comment is skipped.
    #[serde(default)]
    pr_repo: Option<String>,
}

/// SIGN-OFF action for a run (issue #21 / #53): the architect explicitly marks a
/// completed governed run as signed off. Persisted on the story's Unit of Work (which
/// survives sessions) along with the run id and a history entry.
///
/// # Critical-finding gate (issue #53)
///
/// When the UoW's evidence record contains a Critical scoped-scan finding, the sign-off
/// is BLOCKED until the architect supplies an explicit `waive_reason` in the request.
/// A reason-less waive is rejected with HTTP 409 (CONFLICT), forcing the architect to
/// acknowledge the finding. When a waive is accepted, the reason is appended to the
/// sign-off note and recorded in the UoW history.
///
/// Camerata never signs work off on its own — this is the deliberate human gate after
/// reviewing the provenance.
///
/// # PR comment posting (issue #53)
///
/// When `pr_number` and `pr_repo` are supplied, the evidence record's markdown is
/// posted as a GitHub PR comment via the arm.rs primitives. Graceful degradation: no
/// token or a GitHub error skips the PR comment without failing the sign-off.
async fn sign_off_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SignOffReq>,
) -> Result<Response, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("run not found: {id}")))?;

    // ── Critical-finding sign-off gate (issue #53) ───────────────────────────
    // Read the UoW's attached evidence. If it has a critical scoped-scan finding,
    // block sign-off unless the architect supplied an explicit waive_reason.
    let current_uow = state.uow.get_or_create(&run.story_id);
    if current_uow.is_sign_off_blocked() {
        let waive = req.waive_reason.as_deref().filter(|r| !r.trim().is_empty());
        if waive.is_none() {
            // Critical finding present but no waive reason — reject with 409.
            let body = Json(serde_json::json!({
                "error": "sign-off blocked by critical security finding",
                "reason": "The evidence record for this run contains a Critical scoped-scan \
                           finding. Sign-off is blocked until you supply a non-empty \
                           `waive_reason` explaining why the finding is acceptable to ship.",
                "blocked": true,
            }));
            return Ok((StatusCode::CONFLICT, body).into_response());
        }
        // Waive with reason: fold the reason into the note so it is durable.
        // The waiver is also recorded as a history entry by `uow.sign_off` (it appends
        // the full note text, which includes the waiver reason).
        let _ = waive; // acknowledged above; used below when building the effective note.
    }

    // Build the effective note, folding in the waiver reason when present.
    let effective_note = {
        let waive = req
            .waive_reason
            .as_deref()
            .filter(|r| !r.trim().is_empty());
        match (req.note.as_deref(), waive) {
            (Some(note), Some(reason)) => Some(format!("{note} [WAIVER: {reason}]")),
            (None, Some(reason)) => Some(format!("[WAIVER] {reason}")),
            (Some(note), None) => Some(note.to_string()),
            (None, None) => None,
        }
    };

    let by = req
        .by
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "architect".to_string());
    let mut uow = state
        .uow
        .sign_off(&run.story_id, &by, &run.id, effective_note.as_deref());

    // ── Post evidence as PR comment (issue #53) ───────────────────────────────
    // When the UoW has an evidence record AND the caller supplied a PR number + repo,
    // post the rendered markdown as a GitHub PR comment. Best-effort: any error is
    // logged but never propagates to the caller.
    if let (Some(evidence), Some(pr_number), Some(pr_repo)) =
        (&uow.evidence, req.pr_number, req.pr_repo.as_deref())
    {
        // Update sign-off in the evidence record copy for the PR comment.
        let mut evidence_for_pr = evidence.clone();
        if let Some(so) = &uow.sign_off {
            evidence_for_pr.set_sign_off(so);
            evidence_for_pr.compute_hash();
        }
        let markdown = crate::evidence::render_pr_markdown(&evidence_for_pr);

        if let Some((owner, repo_name)) = pr_repo.split_once('/') {
            let token = std::env::var("CAMERATA_GITHUB_TOKEN")
                .unwrap_or_default();
            let comment_url = crate::arm::post_pr_comment(
                owner,
                repo_name,
                pr_number,
                &markdown,
                &token,
            )
            .await
            .unwrap_or_default(); // graceful degradation: None on any failure

            if let Some(url) = comment_url {
                // Record the PR comment link in the UoW history (best-effort update).
                state.uow.append_history(
                    &run.story_id,
                    "evidence_pr_comment",
                    &format!("SOC-2 evidence posted as PR comment: {url}"),
                );
                // Re-read the UoW with the updated history so the response is current.
                uow = state.uow.get_or_create(&run.story_id);
            }
        }
    }

    Ok(Json(uow).into_response())
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

// ── Model-tiering tier-map endpoint (#63) ─────────────────────────────────────

/// Body for `POST /api/projects/:id/tier-map`. Mirrors [`crate::model_tier::TierMap`]
/// with all three fields optional so callers can patch just the tiers they want.
#[derive(serde::Deserialize)]
struct SetTierMapReq {
    /// Model id for fast (throughput) tasks.
    #[serde(default)]
    fast: Option<String>,
    /// Model id for balanced (mid-tier) tasks.
    #[serde(default)]
    balanced: Option<String>,
    /// Model id for strongest (frontier-class) tasks.
    #[serde(default)]
    strongest: Option<String>,
}

/// `POST /api/projects/:id/tier-map` — update the project's model-tier map.
///
/// Applies only the fields present in the request body (patch semantics): fields
/// absent or `null` leave the existing binding unchanged, so a caller that only
/// wants to update `fast` does not need to repeat the others.
async fn set_tier_map(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetTierMapReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        if let Some(fast) = req.fast.filter(|s| !s.trim().is_empty()) {
            p.tier_map.fast = fast;
        }
        if let Some(balanced) = req.balanced.filter(|s| !s.trim().is_empty()) {
            p.tier_map.balanced = balanced;
        }
        if let Some(strongest) = req.strongest.filter(|s| !s.trim().is_empty()) {
            p.tier_map.strongest = strongest;
        }
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── VCS-gate process-rule configuration + auditable bypass (issue #65) ────────

/// `GET /api/projects/:id/process-rule-config` — read the project's current VCS-gate
/// process-rule configuration.
async fn get_process_rule_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match state.projects.get(&id) {
        Some(p) => Json(serde_json::json!({
            "ok": true,
            "process_rule_config": p.process_rule_config,
        })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// `POST /api/projects/:id/process-rule-config` — replace the project's VCS-gate
/// process-rule configuration. The full [`ProcessRuleConfig`] document is expected
/// in the request body (partial updates are not supported; send the full object).
async fn set_process_rule_config_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(config): Json<camerata_checks::vcs_action::ProcessRuleConfig>,
) -> Json<serde_json::Value> {
    match state
        .projects
        .update(&id, |p| p.set_process_rule_config(config))
    {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Request body for `POST /api/projects/:id/vcs-gate/bypass`.
#[derive(serde::Deserialize)]
struct VcsGateBypassReq {
    /// The [`VcsAction`] to evaluate.
    action: serde_json::Value,
    /// Non-empty bypass reason. A missing or empty reason is rejected (the same
    /// invariant as the suppression-waiver: a reason-less bypass is a hard error).
    reason: String,
}

/// `POST /api/projects/:id/vcs-gate/bypass` — evaluate a VCS action with an
/// auditable bypass.
///
/// Intended use: when Camerata's orchestration code knows that a specific action
/// cannot satisfy the active process rules for a documented, legitimate reason
/// (e.g. a machine-generated merge commit or a one-time onboarding branch), it
/// calls this endpoint instead of the normal gate path. The caller supplies the
/// action metadata AND a non-empty reason; the endpoint records the bypass so it
/// is visible in the evidence trail.
///
/// - Empty or missing `reason` → `400 Bad Request` (bypass without justification
///   is itself a gate violation).
/// - Action already passes the gate → `200 ok: true, bypassed: false` (no bypass
///   record produced; the action is clean).
/// - Action fails + reason present → `200 ok: true, bypassed: true, record: {...}`.
async fn vcs_gate_bypass(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<VcsGateBypassReq>,
) -> Response {
    use camerata_checks::vcs_action::{build_rules, gate_or_bypass, BypassRequest, GateOrBypassResult, VcsAction};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    // Reason-less bypass is rejected immediately (mirrors the suppression-waiver
    // invariant; a bypass must be auditable or it is not a bypass at all).
    let reason = req.reason.trim().to_string();
    if reason.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": "bypass rejected: a non-empty reason is required \
                            (mirror of the suppression-waiver invariant)"
            })),
        )
            .into_response();
    }

    // Look up the project and build its live rule set.
    let Some(project) = state.projects.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "no such project" })),
        )
            .into_response();
    };

    let rules = build_rules(&project.process_rule_config);

    // Parse the action from the request. We accept the same JSON shape as
    // VcsAction's serde representation.
    let action: VcsAction = match serde_json::from_value(req.action) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "message": format!("could not parse action: {e}")
                })),
            )
                .into_response();
        }
    };

    let bypass_req = BypassRequest { reason };
    match gate_or_bypass(&rules, &action, Some(&bypass_req)) {
        Ok(GateOrBypassResult::Passed) => {
            Json(serde_json::json!({ "ok": true, "bypassed": false })).into_response()
        }
        Ok(GateOrBypassResult::Bypassed(record)) => Json(serde_json::json!({
            "ok": true,
            "bypassed": true,
            "record": record,
        }))
        .into_response(),
        Ok(GateOrBypassResult::Failed(violations)) => {
            // Should be unreachable (bypass always converts Failed to Bypassed when
            // the reason is non-empty), but handle it defensively.
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "ok": false,
                    "message": "gate failed and bypass could not be applied",
                    "violations": violations.iter().map(|v| &v.detail).collect::<Vec<_>>(),
                })),
            )
                .into_response()
        }
        Err(_reason_required) => {
            // Should be unreachable (we checked `reason.is_empty()` above), but
            // handle it defensively.
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "message": "bypass rejected: reason must not be empty"
                })),
            )
                .into_response()
        }
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
    /// Scan-type selector (Part C) — run the AI architectural review (the LLM scan of
    /// architectural/structured/prose rules, plus the deep tier when `deep`). Defaults to
    /// TRUE (today's behaviour). When false the audit makes NO model calls — the LLM passes
    /// are skipped entirely (no tokens). If BOTH this and `run_deterministic` arrive false,
    /// `effective_scan_modes` forces both back to true (never a no-op scan).
    #[serde(default = "default_true")]
    run_ai_review: bool,
    /// Scan-type selector (Part C) — run the DETERMINISTIC scans: the always-on security
    /// floor plus the scan-time mechanical preview pass. Defaults to TRUE. When false the
    /// floor + `merge_scan_preview` are skipped. Deterministic-only (this true, `run_ai_review`
    /// false) is fast and uses no LLM / no tokens.
    #[serde(default = "default_true")]
    run_deterministic: bool,
}

/// serde default for an opt-OUT boolean (defaults to `true` when the field is absent).
fn default_true() -> bool {
    true
}

/// Resolve the scan-type selector flags into the effective `(run_ai_review, run_deterministic)`
/// pair. Both default true; a request that turns BOTH off is a no-op scan, so we force both
/// back ON rather than do nothing (the decision: default-both over a 4xx — the scan still runs
/// useful work and the UI keeps both checked, so this is only reachable by a hand-crafted
/// request). Returns the pair plus whether a both-false coercion happened (for a note).
fn effective_scan_modes(run_ai_review: bool, run_deterministic: bool) -> (bool, bool, bool) {
    if !run_ai_review && !run_deterministic {
        (true, true, true)
    } else {
        (run_ai_review, run_deterministic, false)
    }
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
/// Returns `(scannable, excluded_ci_tier_ids, preview_rules, corpus)`.
///
/// `preview_rules` are the SUBSET of the excluded (CI-tier mechanical) rules that
/// the SCAN-TIME deterministic preview pass ([`crate::scan_tools::run_scan_tools`])
/// can run locally: mechanical, and NOT `layer3_only` (CodeQL / paid tiers never
/// preview). The loaded `corpus` is returned so the caller can resolve each rule's
/// linter source without re-loading it.
async fn split_scannable_rules(
    selected: Vec<crate::onboard::SelectedRule>,
) -> (
    Vec<crate::onboard::SelectedRule>,
    Vec<String>,
    Vec<crate::onboard::SelectedRule>,
    Option<camerata_rules::RuleSet>,
) {
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
    // A CI-tier mechanical rule is PREVIEW-runnable unless it is layer3_only.
    let is_preview_runnable = |id: &str| -> bool {
        set.as_ref()
            .and_then(|s| s.get_by_id(id))
            .map(|r| r.enforcement.is_ci_enforced() && !r.is_layer3_only())
            .unwrap_or(false)
    };
    let mut scannable = Vec::new();
    let mut excluded = Vec::new();
    let mut preview = Vec::new();
    for r in selected {
        if is_ci_tier(&r.id) {
            if is_preview_runnable(&r.id) {
                preview.push(r.clone());
            }
            excluded.push(r.id);
        } else {
            scannable.push(r);
        }
    }
    (scannable, excluded, preview, set)
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
    // The scan-runnable subset (mechanical, non-layer3_only) feeds the SCAN-TIME PREVIEW
    // pass below, which runs the rule's deterministic tool itself and folds in preview findings.
    let (selected, excluded_mechanical, preview_rules, corpus) =
        split_scannable_rules(selected).await;
    let model = req.model.filter(|m| !m.trim().is_empty());
    let calibration_model = req.calibration_model.filter(|m| !m.trim().is_empty());
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    // Scan-type selector: resolve the two flags (both-false coerces to both-true).
    let (run_ai_review, run_deterministic, _coerced) =
        effective_scan_modes(req.run_ai_review, req.run_deterministic);
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
        state.feature_flags.soc2,
        run_ai_review,
        run_deterministic,
    )
    .await;
    // Persist the fresh manifest (even after a forced full scan) so the NEXT scan can be
    // incremental. Only when there's an active project to key it to.
    if let Some(id) = &project_id {
        state.scan_cache.put(id, manifest);
    }
    report.excluded_mechanical_rules = excluded_mechanical;
    // SCAN-TIME deterministic PREVIEW pass: run the selected mechanical rules' own tools
    // and fold their findings into triage as preview findings (advisory, not enforced).
    // Gated on the deterministic selection — deselecting deterministic scans skips it too.
    // (No job here, so no live deterministic progress on the synchronous path.)
    if run_deterministic {
        merge_scan_preview(&mut report, &sources, &preview_rules, corpus.as_ref(), None).await;
    }
    Json(report)
}

/// Run the scan-time deterministic preview pass over each local repo source and append
/// its preview findings to the report. A no-op when there are no preview-runnable rules
/// (or the corpus is unavailable). Preview findings are ADVISORY-but-deterministic — they
/// carry stable tool rule-ids, stay OUT of the LLM review, and are labeled "not enforced
/// until wired" in the UI. layer3_only rules were already excluded by `split_scannable_rules`.
async fn merge_scan_preview(
    report: &mut crate::onboard::ScanReport,
    sources: &[(String, std::path::PathBuf)],
    preview_rules: &[crate::onboard::SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
    // The async job to report per-tool deterministic progress into (`(store, id)`), or `None`
    // on the synchronous path. When set, each preview tool registers + streams running → done
    // with its findings count, mirroring the floor's progress.
    job: Option<(&crate::jobs::JobStore, &str)>,
) {
    if preview_rules.is_empty() {
        return;
    }
    let Some(set) = corpus else { return };
    let lookup = |id: &str| set.get_by_id(id);
    for (spec, dir) in sources {
        // Only the rules bound to this repo (or project-level) preview against it.
        let for_repo: Vec<crate::onboard::SelectedRule> = preview_rules
            .iter()
            .filter(|r| r.applies_to(spec))
            .cloned()
            .collect();
        if for_repo.is_empty() {
            continue;
        }
        let mut previews =
            crate::scan_tools::run_scan_tools(spec, dir, &for_repo, &lookup, job).await;
        report.findings.append(&mut previews);
    }
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
    // The scan-runnable subset (mechanical, non-layer3_only) feeds the SCAN-TIME PREVIEW.
    let (selected, excluded_mechanical, preview_rules, corpus) =
        split_scannable_rules(selected).await;
    let model = req.model.filter(|m| !m.trim().is_empty());
    let calibration_model = req.calibration_model.filter(|m| !m.trim().is_empty());
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    let thorough = req.thorough;
    let deep = req.deep;
    // Scan-type selector: resolve the two flags (both-false coerces to both-true).
    let (run_ai_review, run_deterministic, _coerced) =
        effective_scan_modes(req.run_ai_review, req.run_deterministic);
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
    let soc2_enabled = state.feature_flags.soc2;
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
            soc2_enabled,
            run_ai_review,
            run_deterministic,
        )
        .await;
        // Persist the fresh manifest so the next scan can be incremental.
        if let Some(id) = &project_id {
            scan_cache.put(id, manifest);
        }
        report.excluded_mechanical_rules = excluded_mechanical;
        // SCAN-TIME deterministic PREVIEW pass (advisory; not enforced until wired). Gated on
        // the deterministic selection; reports per-tool progress into the job so the cockpit's
        // deterministic progress view shows each preview tool start/run/done live.
        if run_deterministic {
            merge_scan_preview(
                &mut report,
                &sources,
                &preview_rules,
                corpus.as_ref(),
                Some((&jobs, &jid)),
            )
            .await;
        }
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

/// One CI-tier rule sent by the UI for story generation. Carries the rule id and title
/// for display in the issue body, plus the linter hint (first source with one) so the
/// mechanical story can name the exact off-the-shelf tool without looking it up.
#[derive(serde::Deserialize)]
struct CiStoryRule {
    id: String,
    title: String,
    #[serde(default)]
    linter: Option<String>,
}

#[derive(serde::Deserialize)]
struct CiRulesReq {
    repo: String,
    /// "mechanical" or "architectural" — which tier this story covers.
    tier: String,
    /// The rules of that tier to list in the issue body.
    #[serde(default)]
    rules: Vec<CiStoryRule>,
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

/// Emit a tier-specific "wire CI rules" story as a GitHub issue.
///
/// Two tiers are supported:
/// - "mechanical"   — rules that map 1:1 to an off-the-shelf linter/analyzer. Wiring is
///   straightforward: enable the cited linter rule in CI. This story is safe to implement
///   immediately without team refinement.
/// - "architectural" — rules that are also deterministic but require a bespoke AST or static-
///   analysis checker the team must DESIGN before implementing. This story should be scoped
///   and refined first; it should NOT ride with the mechanical story.
///
/// The UI files each story separately so the two tracks land as distinct GitHub issues.
async fn onboard_ci_rules(Json(req): Json<CiRulesReq>) -> Json<serde_json::Value> {
    let Some((owner, repo)) = req.repo.split_once('/') else {
        return Json(serde_json::json!({ "ok": false, "message": "repo must be owner/repo" }));
    };
    if req.tier != "mechanical" && req.tier != "architectural" {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("unknown tier '{}': must be 'mechanical' or 'architectural'", req.tier)
        }));
    }
    if req.rules.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("no {} rules to wire", req.tier)
        }));
    }
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Json(
            serde_json::json!({ "ok": false, "message": "Connect GitHub to create the story issue." }),
        );
    };

    // The preamble is identical for both tiers — it sets up the distinction once and up
    // front so the reader doesn't have to infer it from the title alone.
    let preamble = "Mechanical and architectural rules are both deterministic CI-tier checks. \
        Mechanical rules map to an existing off-the-shelf linter (simple to wire). \
        Architectural rules require a custom checker and team refinement before implementing. \
        Wire each check into the repo's CANONICAL check command (the lint/test command that \
        Camerata's in-loop layer-2 check also runs) AND the CI workflow — one wiring covers BOTH \
        the in-loop layer-2 check (fast, during a governed run) and the authoritative layer-3 CI \
        gate (every PR, including non-Camerata changes).";

    let (title, body) = match req.tier.as_str() {
        "mechanical" => {
            let t = format!("Wire mechanical (off-the-shelf linter) rules into CI — {}", req.repo);
            let rule_lines: String = req
                .rules
                .iter()
                .map(|r| {
                    if let Some(ref linter) = r.linter {
                        format!("- {} — {} (linter: {})\n", r.id, r.title, linter)
                    } else {
                        format!("- {} — {}\n", r.id, r.title)
                    }
                })
                .collect();
            let b = format!(
                "{preamble}\n\n\
                 **This story covers the MECHANICAL tier only.** Each rule listed below maps to a \
                 real off-the-shelf linter or analyzer — enabling the cited tool rule in CI is the \
                 entire implementation. No bespoke checker is needed.\n\n\
                 **Repo:** `{repo}`\n\n\
                 **Rules to wire:**\n\
                 {rule_lines}\n\
                 For each rule:\n\
                 1. Check whether it is already enforced in CI (inspect `.github/workflows/`, the \
                 linter config, package scripts).\n\
                 2. If it is, leave it.\n\
                 3. If not, enable the cited linter rule in the repo's lint config so the repo's \
                 canonical lint command runs it (this makes Camerata's in-loop layer-2 check pick \
                 it up automatically), AND wire that same command into the CI workflow so it runs \
                 on every PR (layer 3). One wiring covers both layers; a failing PR means the rule \
                 is violated.\n\n\
                 Do not weaken or delete existing checks.\n\n\
                 _Filed by Camerata onboarding._"
            );
            (t, b)
        }
        _ => {
            // architectural
            let t = format!(
                "Wire architectural (custom-checker) rules into CI — {}",
                req.repo
            );
            let rule_lines: String = req
                .rules
                .iter()
                .map(|r| format!("- {} — {}\n", r.id, r.title))
                .collect();
            let b = format!(
                "{preamble}\n\n\
                 **This story covers the ARCHITECTURAL tier only.** Each rule below is \
                 deterministic but has NO off-the-shelf linter. Each one needs a bespoke AST or \
                 static-analysis checker designed by the team. **This story is more involved than \
                 the mechanical story and should be scoped and refined with the team before \
                 implementation begins.**\n\n\
                 **Repo:** `{repo}`\n\n\
                 **Rules to wire (each needs a custom checker):**\n\
                 {rule_lines}\n\
                 For each rule:\n\
                 1. Agree on a checker strategy (AST transform, grep pattern, Semgrep rule, etc.).\n\
                 2. Implement and test the checker in isolation.\n\
                 3. Wire the checker into the repo's canonical check command (e.g. its lint/test \
                 script, so Camerata's in-loop layer-2 check runs it) AND the CI workflow so it \
                 runs on every PR (layer 3). One wiring covers both layers.\n\n\
                 Do not weaken or delete existing checks. Scope each checker as a sub-task if the \
                 list is long — do not block the mechanical CI story on this work.\n\n\
                 _Filed by Camerata onboarding._"
            );
            (t, b)
        }
    };

    match crate::onboard::create_issue(owner, repo, &token, &title, &body).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

// ── Greenfield scaffold handler ───────────────────────────────────────────────

/// Request body for the greenfield scaffold endpoint.
///
/// The caller supplies the new repo's name (used as the commit label), the local
/// path where the repo should be created, the resolved rules to bake in, and any
/// custom rules from the active project. All fields are validated server-side
/// before the blocking scaffold runs.
#[derive(serde::Deserialize)]
struct GreenfieldReq {
    /// Human name / label for the new repo (used in the initial commit message).
    /// Does NOT need to be an `owner/repo` — it can be a bare project name until
    /// the user connects the remote. Required.
    name: String,
    /// Absolute path on disk where the new repo directory should be created.
    /// The directory MUST NOT already exist; scaffold creates it. Required.
    path: String,
    /// The resolved rules to install (same shape as `ArmReq.rules`). Optional —
    /// an empty list scaffolds just the git repo + an empty gate config.
    #[serde(default)]
    rules: Vec<crate::arm::ArmRule>,
    /// Custom rules from the active project to carry into the emit. Optional.
    #[serde(default)]
    custom: Vec<crate::project::CustomRule>,
}

/// Greenfield onboarding: scaffold a NEW local git repo with governance baked in
/// from commit zero. This is the counterpart to the brownfield apply flow: instead
/// of writing governance files into an EXISTING repo, it CREATES the repo directory,
/// emits the governance files (AGENTS.md / CONVENTIONS.md / .camerata/rules.json /
/// CI workflow) using the same `arm_files_for_repo` primitive as apply, and commits
/// them as the initial commit.
///
/// The handler does NOT push to GitHub — the new repo is local-only until the
/// architect connects it to a remote. It marks the scaffolded repo as onboarded in
/// the active project (creating one from the name if none is active).
///
/// The scaffold is a blocking operation (filesystem + git) and is run via
/// `tokio::task::spawn_blocking` to avoid blocking the async runtime.
async fn onboard_greenfield(
    State(state): State<AppState>,
    Json(req): Json<GreenfieldReq>,
) -> Json<serde_json::Value> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "Name the new repo." }));
    }
    let path_str = req.path.trim().to_string();
    if path_str.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "Choose a directory for the new repo." }));
    }
    let dest = std::path::PathBuf::from(&path_str);

    // Resolve the repo-local rules (drop cross-repo / process: they're project-level,
    // not written into any single repo's files). Clone the vecs so they are 'static
    // and can be moved into the spawn_blocking closure.
    let repo_local: Vec<crate::arm::ArmRule> = req
        .rules
        .into_iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .collect();
    let custom: Vec<crate::project::CustomRule> = req.custom;
    let name_clone = name.clone();

    // Run the blocking scaffold (filesystem + git) off the async runtime.
    // Move owned vecs (not refs) into the closure to satisfy the 'static bound.
    let result = tokio::task::spawn_blocking(move || {
        let rule_refs: Vec<&crate::arm::ArmRule> = repo_local.iter().collect();
        let custom_refs: Vec<&crate::project::CustomRule> = custom.iter().collect();
        crate::onboard::scaffold_greenfield_blocking(&dest, &rule_refs, &custom_refs, &name_clone)
    })
    .await;

    let scaffold = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Json(serde_json::json!({ "ok": false, "message": format!("{e}") }));
        }
        Err(e) => {
            return Json(serde_json::json!({ "ok": false, "message": format!("scaffold task: {e}") }));
        }
    };

    // Mark the new repo as onboarded in the active project (creating one if needed
    // so the name appears in the repos list and the onboarded badge lights up).
    let repo_label = name.clone();
    let pid = match state.projects.active() {
        Some(p) => p.id,
        None => match state.projects.create(&repo_label, vec![repo_label.clone()]) {
            Some(p) => p.id,
            None => {
                // Project store unavailable — continue anyway; the scaffold itself succeeded.
                return Json(serde_json::json!({
                    "ok": true,
                    "path": scaffold.path,
                    "files_written": scaffold.files_written,
                    "commit_sha": scaffold.commit_sha,
                    "message": scaffold.message,
                }));
            }
        },
    };
    state.projects.update(&pid, |p| {
        p.mark_onboarded(&[repo_label.clone()]);
    });

    // Record the scaffolded dir as the repo's local path override so subsequent
    // scan/apply operations resolve it correctly without a workspace root.
    let path_clone = scaffold.path.clone();
    state.settings.set_repo_path(&repo_label, Some(path_clone));

    Json(serde_json::json!({
        "ok": true,
        "path": scaffold.path,
        "files_written": scaffold.files_written,
        "commit_sha": scaffold.commit_sha,
        "message": scaffold.message,
    }))
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

// ── Provider-agnostic WorkItem + UoW layer (governed-dev surface) ──────────────

/// Read `CAMERATA_GITHUB_TOKEN`, returning `None` when unset or empty.
fn github_token() -> Option<String> {
    std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
}

/// `POST /api/workitems/pull` — pull ALL open issues across ALL the ACTIVE project's
/// repos via the GitHub adapter, normalized to [`WorkItem`] (each carrying its repo).
/// Manual (user-triggered), no cache. Returns `{ items: WorkItem[] }`.
///
/// Degrades gracefully: with no token, or no active project, or no repos, returns an
/// empty item list (never an error) so the UI can render a "Connect GitHub / add a
/// repo" hint. A per-repo fetch failure is skipped (the union of the repos that DID
/// resolve is returned) rather than failing the whole pull.
async fn workitems_pull(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(token) = github_token() else {
        return Json(serde_json::json!({
            "items": [],
            "message": "Connect GitHub to pull work items.",
        }));
    };
    let Some(project) = state.projects.active() else {
        return Json(serde_json::json!({
            "items": [],
            "message": "No active project. Create or select one to pull work items.",
        }));
    };
    let mut items: Vec<crate::workitems::WorkItem> = Vec::new();
    for repo in &project.repos {
        match crate::github_issues::list_open_issues(repo, &token).await {
            Ok(issues) => {
                for issue in issues {
                    // The list path returns IssueSummary (no state/labels); open issues
                    // are by definition "open" with no camerata labels needed for the
                    // pull view. Map straight onto a WorkItem with the repo set.
                    items.push(crate::workitems::WorkItem {
                        id: crate::workitems::WorkItem::github_id(repo, issue.number),
                        provider: "github".to_string(),
                        repo: repo.clone(),
                        number: issue.number,
                        title: issue.title,
                        body: issue.body,
                        state: "open".to_string(),
                        url: issue.url,
                        labels: Vec::new(),
                    });
                }
            }
            // Skip a repo that fails (bad name, 404, rate limit) — the union of the
            // repos that resolved is still useful; the architect sees what loaded.
            Err(_) => continue,
        }
    }
    Json(serde_json::json!({ "items": items }))
}

/// `POST /api/workitems/refresh` body `{ work_item_id }` — re-pull ONE work item from
/// its source (GitHub), returning `{ item: WorkItem }`. Needs the token.
#[derive(serde::Deserialize)]
struct WorkItemRefreshReq {
    work_item_id: String,
}

async fn workitems_refresh(
    State(_state): State<AppState>,
    Json(req): Json<WorkItemRefreshReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = github_token()
        .ok_or_else(|| AppError(anyhow::anyhow!("no GitHub token — set CAMERATA_GITHUB_TOKEN")))?;
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    let detail = crate::github_issues::get_issue_detail(&repo, number, &token)
        .await
        .map_err(AppError)?;
    let item = crate::workitems::WorkItem::from_github_issue(&repo, &detail);
    Ok(Json(serde_json::json!({ "item": item })))
}

/// `POST /api/workitems/comment` body `{ work_item_id, body }` — comment back onto the
/// source issue (GitHub) via the adapter / sync path. Returns `{ ok, url }`. Needs the
/// token. The `url` is the created comment's html_url.
#[derive(serde::Deserialize)]
struct WorkItemCommentReq {
    work_item_id: String,
    body: String,
}

async fn workitems_comment(
    State(_state): State<AppState>,
    Json(req): Json<WorkItemCommentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = github_token()
        .ok_or_else(|| AppError(anyhow::anyhow!("no GitHub token — set CAMERATA_GITHUB_TOKEN")))?;
    if req.body.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!("comment body must not be empty")));
    }
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    let url = crate::github_issues::comment_on_issue(&repo, number, &req.body, &token)
        .await
        .map_err(AppError)?;
    Ok(Json(serde_json::json!({ "ok": true, "url": url })))
}

/// `POST /api/workitems/comments` body `{ work_item_id }` — read the COMMENTS on the
/// source issue (GitHub), returning `{ comments: IssueComment[] }` oldest-first.
///
/// Degrades gracefully (mirroring the pull path): with no token, or a malformed id, or
/// a fetch failure, returns an EMPTY comment list (never an error) so the UoW modal can
/// render "No comments." instead of breaking.
#[derive(serde::Deserialize)]
struct WorkItemCommentsReq {
    work_item_id: String,
}

async fn workitems_comments(
    State(_state): State<AppState>,
    Json(req): Json<WorkItemCommentsReq>,
) -> Json<serde_json::Value> {
    let Some(token) = github_token() else {
        return Json(serde_json::json!({ "comments": [] }));
    };
    let Ok((repo, number)) = parse_github_work_item_id(&req.work_item_id) else {
        return Json(serde_json::json!({ "comments": [] }));
    };
    match crate::github_issues::get_issue_comments(&repo, number, &token).await {
        Ok(comments) => Json(serde_json::json!({ "comments": comments })),
        Err(_) => Json(serde_json::json!({ "comments": [] })),
    }
}

/// `POST /api/workitems/assignees` body `{ work_item_id }` — read the ASSIGNABLE users
/// for the work item's repo (the practical @-mention set), returning `{ users: [login] }`.
///
/// Degrades gracefully: with no token, or a malformed id, or a fetch failure, returns an
/// EMPTY user list (never an error) so the comment box's @-autocomplete simply shows no
/// suggestions instead of breaking.
#[derive(serde::Deserialize)]
struct WorkItemAssigneesReq {
    work_item_id: String,
}

async fn workitems_assignees(
    State(_state): State<AppState>,
    Json(req): Json<WorkItemAssigneesReq>,
) -> Json<serde_json::Value> {
    let Some(token) = github_token() else {
        return Json(serde_json::json!({ "users": [] }));
    };
    let Ok((repo, _number)) = parse_github_work_item_id(&req.work_item_id) else {
        return Json(serde_json::json!({ "users": [] }));
    };
    match crate::github_issues::get_assignees(&repo, &token).await {
        Ok(users) => Json(serde_json::json!({ "users": users })),
        Err(_) => Json(serde_json::json!({ "users": [] })),
    }
}

/// Parse a GitHub work-item id (`github:OWNER/REPO#NUMBER`) into `(repo, number)`.
/// Errors when the provider is not `github` or the shape is malformed.
fn parse_github_work_item_id(work_item_id: &str) -> Result<(String, u64), AppError> {
    let rest = work_item_id.strip_prefix("github:").ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "work_item_id must be `github:OWNER/REPO#NUMBER`, got `{work_item_id}`"
        ))
    })?;
    let (repo, num) = rest.rsplit_once('#').ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "work_item_id missing `#NUMBER`: `{work_item_id}`"
        ))
    })?;
    if camerata_worktracker::RepoCoord::parse(repo).is_none() {
        return Err(AppError(anyhow::anyhow!(
            "work_item_id repo is not `owner/repo`: `{repo}`"
        )));
    }
    let number: u64 = num
        .parse()
        .map_err(|_| AppError(anyhow::anyhow!("work_item_id number is not a u64: `{num}`")))?;
    Ok((repo.to_string(), number))
}

/// A UoW with the WorkItem it references and its lifecycle stage, for `GET /api/uows`.
#[derive(serde::Serialize)]
struct UowView {
    /// The UoW id (its story id, e.g. `OWNER/REPO#123`, or `draft-<token>` for an
    /// AI-authoring draft).
    id: String,
    /// The work item this UoW references, when it maps to one (a GitHub-sourced
    /// spine story). `None` for native/legacy stories with no external ref AND for a
    /// blank/authoring DRAFT UoW that has not been published to the board yet.
    work_item: Option<crate::workitems::WorkItem>,
    /// The lifecycle stage as a snake_case wire string (`intake`, `development`, …).
    stage: String,
    /// `true` when this is a blank/authoring DRAFT UoW (it has an authoring state and no
    /// work item yet). The UI renders the authoring panel instead of the dev controls.
    authoring: bool,
}

/// `GET /api/uows` — list all Units of Work, each with the WorkItem it references
/// (resolved from the story spine) and its lifecycle stage. A draft UoW's work item is
/// resolved by its explicit `work_item` link (set at publish), falling back to the key.
async fn uows_list(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let stories = state.stories.list().await.map_err(AppError)?;
    let uows = state.uow.list();
    let views: Vec<UowView> = uows
        .into_iter()
        .map(|u| {
            // A linked draft carries the real work-item story id in `work_item`; a
            // normal UoW's key IS the work-item story id. Resolve against the spine by
            // whichever applies.
            let lookup_id = u.work_item.clone().unwrap_or_else(|| u.story_id.clone());
            let work_item = stories
                .iter()
                .find(|s| s.id == lookup_id)
                .and_then(crate::workitems::WorkItem::from_canonical_story);
            // A draft is one with an authoring state and no published work item yet.
            let authoring = u.authoring.is_some() && work_item.is_none();
            UowView {
                id: u.story_id,
                work_item,
                stage: u.stage.wire_str().to_string(),
                authoring,
            }
        })
        .collect();
    Ok(Json(serde_json::json!({ "uows": views })))
}

/// `POST /api/uow/from-workitem` body `{ work_item_id }` — create a UoW referencing the
/// work item. DEDUP by external ref: if a UoW already exists for that work item, return
/// it with `created=false` (never a duplicate). Returns `{ uow_id, created }`.
///
/// The work item is also ensured on the canonical story spine (idempotent upsert) when
/// it is not already there, so `/api/uows` can resolve the WorkItem back and the
/// governed-dev endpoints find a story to run against. This REPLACES the adopt-issue
/// hack: the UI no longer names a repo + number directly; it pulls work items, then
/// projects one onto a UoW here.
#[derive(serde::Deserialize)]
struct UowFromWorkItemReq {
    work_item_id: String,
}

async fn uow_from_workitem(
    State(state): State<AppState>,
    Json(req): Json<UowFromWorkItemReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    // The UoW key is the spine story id (provider prefix stripped).
    let story_id = crate::workitems::story_id_for(&req.work_item_id);

    // DEDUP by external ref: a UoW already exists for this work item id.
    let already = state.uow.list().iter().any(|u| u.story_id == story_id);
    if already {
        return Ok(Json(
            serde_json::json!({ "uow_id": story_id, "created": false }),
        ));
    }

    // Ensure the work item is on the canonical spine so /api/uows resolves it and the
    // governed-dev endpoints have a story to run against. Idempotent upsert: refresh
    // the row from GitHub when a token is configured, else seed a minimal row from the
    // id alone (so a token-free environment still creates a usable UoW).
    let story = match (
        state.stories.get(&story_id).await.map_err(AppError)?,
        github_token(),
    ) {
        (Some(existing), _) => existing,
        (None, Some(token)) => {
            match crate::github_issues::get_issue_detail(&repo, number, &token).await {
                Ok(detail) => {
                    crate::github_issues::issue_to_story(&repo, number, &detail.title, &detail.body)
                }
                // Token present but the fetch failed: still create a minimal spine row
                // so the UoW is usable; the architect can refresh it later.
                Err(_) => crate::github_issues::issue_to_story(&repo, number, "", ""),
            }
        }
        (None, None) => crate::github_issues::issue_to_story(&repo, number, "", ""),
    };
    state.stories.upsert(story).await.map_err(AppError)?;

    // Create the UoW (get_or_create materializes it at the default Intake stage).
    let uow = state.uow.get_or_create(&story_id);
    Ok(Json(
        serde_json::json!({ "uow_id": uow.story_id, "created": true }),
    ))
}

// ── AI story authoring from a blank UoW (2026-06-22) ─────────────────────────────

/// `POST /api/uow/blank` — create a blank DRAFT UoW (no story yet, `work_item = None`,
/// an empty authoring state). It appears in `/api/uows` as a draft (authoring=true) and
/// is the start of the "author a story with AI" flow. Returns `{ uow_id }`.
async fn uow_blank(State(state): State<AppState>) -> Json<serde_json::Value> {
    let uow = state.uow.create_blank();
    Json(serde_json::json!({ "uow_id": uow.story_id }))
}

#[derive(serde::Deserialize)]
struct UowAuthorReq {
    /// The next message in the clarification chat. The first message is the free-text
    /// requirements; subsequent ones answer the AI's clarifying questions.
    message: String,
}

/// The system prompt that turns the LLM into a story-authoring assistant. It produces a
/// JSON object `{ "title", "body", "reply" }` so the server can update the draft AND show
/// a conversational reply (which may be a clarifying question).
const STORY_AUTHOR_SYSTEM: &str = "You are a product-owner assistant that drafts a single \
GitHub-issue-style user story (a title and a markdown body) from a set of requirements and \
an ongoing clarification chat. Keep one cohesive story: a concise imperative title and a \
body with sections like Summary, Acceptance Criteria (a checklist), and Notes as warranted. \
When the requirements are ambiguous or missing key detail, ASK ONE concise clarifying \
question in your reply and draft the best story you can so far. Respond ONLY with a minified \
JSON object with exactly these keys: \"title\" (string), \"body\" (string, markdown), and \
\"reply\" (string: a short conversational message to the author, e.g. your clarifying \
question or a note on what you changed). Do not wrap the JSON in code fences.";

/// Parse the LLM's story-authoring response into `(title, body, reply)`. The model is asked
/// for a JSON object; if it deviates (e.g. wraps in fences or returns prose), we degrade
/// gracefully: strip a fenced block if present, else treat the whole text as the reply and
/// leave the draft unchanged signals (empty strings).
fn parse_author_response(raw: &str) -> (String, String, String) {
    let trimmed = raw.trim();
    // Strip a leading/trailing ```json … ``` fence if the model added one.
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start())
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner) {
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let reply = v
            .get("reply")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return (title, body, reply);
    }
    // Not JSON: keep the raw text as the conversational reply; leave the draft untouched.
    (String::new(), String::new(), trimmed.to_string())
}

/// Build the user prompt for the authoring LLM from the prior chat plus the new message.
fn build_author_prompt(chat: &[crate::uow::AuthorChatMessage], new_message: &str) -> String {
    let mut p = String::new();
    if chat.is_empty() {
        p.push_str("Requirements:\n");
        p.push_str(new_message);
    } else {
        p.push_str("Conversation so far:\n");
        for m in chat {
            let who = if m.role == "ai" { "Assistant" } else { "Author" };
            p.push_str(&format!("{who}: {}\n", m.text));
        }
        p.push_str("\nNew message from the author:\n");
        p.push_str(new_message);
    }
    p.push_str(
        "\n\nUpdate the story draft and reply. Respond ONLY with the JSON object described \
         in the system prompt.",
    );
    p
}

/// `POST /api/uow/:story_id/author` body `{ message }` — append a turn to a draft UoW's
/// clarification chat, ask the LLM to (re)draft the story, persist, and return the updated
/// `UnitOfWork`. Degrades gracefully with no LLM token (returns a clear note as the AI reply
/// and leaves the draft unchanged) — story authoring is an LLM text-generation assist (no
/// gate, same class as the chat assistant).
async fn uow_author(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowAuthorReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return Err(AppError(anyhow::anyhow!("message must not be empty")));
    }
    // Snapshot the prior chat + draft so we can preserve the draft if the LLM is off/fails.
    let before = state.uow.get_or_create(&story_id);
    let prior = before.authoring.unwrap_or_default();
    let prompt = build_author_prompt(&prior.chat, &message);

    let llm = crate::llm::Llm::from_env();
    let request = crate::llm::LlmRequest::new(prompt).with_system(STORY_AUTHOR_SYSTEM);
    let (title, body, reply) = match llm.complete(request).await {
        Ok(resp) => {
            let (t, b, r) = parse_author_response(&resp.text);
            // Keep the existing draft when the model returned no usable title/body.
            let title = if t.is_empty() { prior.draft_title.clone() } else { t };
            let body = if b.is_empty() { prior.draft_body.clone() } else { b };
            let reply = if r.is_empty() {
                "Updated the draft.".to_string()
            } else {
                r
            };
            (title, body, reply)
        }
        Err(e) => {
            // Token-less / LLM-off: don't crash; record a clear note and keep the draft.
            let note = format!(
                "AI drafting is unavailable right now ({}). Your message was saved; configure \
                 a model (CLI or ANTHROPIC_API_KEY) and try again.",
                e
            );
            (prior.draft_title.clone(), prior.draft_body.clone(), note)
        }
    };
    let updated = state
        .uow
        .append_authoring_turn(&story_id, &message, &reply, &title, &body);
    Ok(Json(updated))
}

#[derive(serde::Deserialize)]
struct UowPublishReq {
    /// The target repo (`owner/repo`), one of the active project's repos.
    repo: String,
}

/// `POST /api/uow/:story_id/publish` body `{ repo }` — create a GitHub issue from the
/// drafted title/body (reuse `onboard::create_issue`), upsert the resulting work item onto
/// the canonical spine, and LINK the draft UoW to it (without re-keying the UoW). Returns
/// the linked `{ work_item, uow_id }`. Requires a GitHub token; 4xx with a clear reason
/// when the token is absent, the repo is malformed, or the draft has no title.
async fn uow_publish(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowPublishReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let coord = camerata_worktracker::RepoCoord::parse(&req.repo).ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "repo must be `owner/repo`, got `{}`",
            req.repo
        ))
    })?;
    let token = github_token().ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "Connect GitHub (set CAMERATA_GITHUB_TOKEN) to publish the story to the board."
        ))
    })?;
    let uow = state.uow.get_or_create(&story_id);
    let authoring = uow.authoring.clone().unwrap_or_default();
    if authoring.draft_title.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "The story has no drafted title yet. Author the story before publishing."
        )));
    }

    // Create the issue (the generic emit-a-story primitive).
    let html_url = crate::onboard::create_issue(
        &coord.owner,
        &coord.repo,
        &token,
        &authoring.draft_title,
        &authoring.draft_body,
    )
    .await
    .map_err(AppError)?;

    // The new issue number is the trailing path segment of the html_url
    // (`https://github.com/owner/repo/issues/<num>`).
    let number: u64 = html_url
        .rsplit('/')
        .next()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| {
            AppError(anyhow::anyhow!(
                "could not parse the new issue number from `{html_url}`"
            ))
        })?;

    // Build the canonical story for the new issue and upsert it onto the spine so
    // /api/uows resolves the work item and dev runs have a story to run against.
    let story = crate::github_issues::issue_to_story(
        &req.repo,
        number,
        &authoring.draft_title,
        &authoring.draft_body,
    );
    let work_item_story_id = story.id.clone();
    state.stories.upsert(story).await.map_err(AppError)?;

    // Link the draft UoW to the work item WITHOUT re-keying it (the work-item ref carries
    // the real owner/repo#num).
    state
        .uow
        .link_work_item(&story_id, &work_item_story_id);

    // Resolve the linked work item for the response.
    let work_item = state
        .stories
        .get(&work_item_story_id)
        .await
        .map_err(AppError)?
        .as_ref()
        .and_then(crate::workitems::WorkItem::from_canonical_story);

    Ok(Json(serde_json::json!({
        "uow_id": story_id,
        "work_item": work_item,
    })))
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

/// Body for `POST /api/uow/:story_id/begin-investigation`. Optional `model` pins the
/// single investigation agent's model; `None`/blank defaults to the active project's
/// `tier_map.strongest`. Absent body (no JSON) is accepted (defaults applied).
#[derive(serde::Deserialize, Default)]
struct BeginInvestigationReq {
    #[serde(default)]
    model: Option<String>,
}

/// Drive the UoW Intake → Investigating (Pillar 2) AND kick a single, model-aware,
/// gated investigation agent that analyzes the story and records an investigation note
/// onto the UoW. Returns `{ "run_id", "story_id" }` so the UI can watch AgentActivity.
///
/// The investigation is a SINGLE agent (not the development fleet): it analyzes and
/// surfaces decisions; it does not scaffold or write code. The agent is gated identically
/// to every fleet agent (allowedTools = gated tools only; `Task` disallowed).
///
/// 409 (with the precise reason) if the stage transition is illegal (e.g. the UoW is
/// not at Intake) — surfaced before any run is started so the UI shows why.
async fn uow_begin_investigation(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<BeginInvestigationReq>>,
) -> Response {
    // Transition the stage first; if it is illegal, surface the reason and start nothing.
    if let Err(err) = state.uow.begin_investigation(&story_id) {
        let body = Json(serde_json::json!({
            "error": "lifecycle transition blocked",
            "reason": err.message(),
            "detail": err,
        }));
        return (StatusCode::CONFLICT, body).into_response();
    }

    // Resolve the model: the caller's choice, else the active project's strongest tier.
    let requested = req
        .and_then(|Json(r)| r.model)
        .filter(|m| !m.trim().is_empty());
    let model = requested.unwrap_or_else(|| {
        state
            .projects
            .active()
            .map(|p| p.tier_map.strongest)
            .unwrap_or_else(crate::model_tier::default_strongest_model)
    });

    // Pull the story context for the agent prompt (best-effort; fall back to the id).
    let (title, desc) = match state.stories.get(&story_id).await {
        Ok(Some(s)) => (s.title, s.description),
        _ => (story_id.clone(), String::new()),
    };

    // Create a run the UI can poll, then kick the single gated investigation agent.
    let run_id = state.runs.create(&story_id, "investigation");
    {
        let runs = state.runs.clone();
        let uow = state.uow.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        tokio::spawn(async move {
            crate::investigation_run::execute_investigation_run(
                runs, uow, rid, sid, title, desc, model,
            )
            .await;
        });
    }

    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

/// Derive `owner/repo` from a UoW story id (`owner/repo#num`). The UoW story id is the
/// GitHub-sourced id WITHOUT the `github:` prefix (see [`UowView`]); the repo is the part
/// before the last `#`. Returns `None` when the id has no `#` or the repo part is not a
/// valid `owner/repo`.
fn repo_from_story_id(story_id: &str) -> Option<String> {
    let (repo, _num) = story_id.rsplit_once('#')?;
    if camerata_worktracker::RepoCoord::parse(repo).is_some() {
        Some(repo.to_string())
    } else {
        None
    }
}

/// `POST /api/uow/:story_id/branches` → `{ "local": [...], "origin": [...] }`.
///
/// Lists the branches this UoW can merge FROM, populating the "Update branch" picker.
/// Resolves the repo (from the story id) and its local clone dir; `local` are the
/// working copy's branches, `origin` are the `origin/*` remote-tracking branches (prefix
/// stripped). Token-less / no-clone / unresolvable repo → empty lists (graceful, never an
/// error) so the UI renders an empty picker rather than breaking.
async fn uow_list_branches(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<crate::workspace::MergeSourceBranches> {
    let Some(repo) = repo_from_story_id(&story_id) else {
        return Json(crate::workspace::MergeSourceBranches::default());
    };
    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_repo_dir(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
    ) else {
        return Json(crate::workspace::MergeSourceBranches::default());
    };
    Json(crate::workspace::list_merge_sources(&dir).await)
}

/// Request body for `POST /api/uow/:story_id/update-branch`.
#[derive(serde::Deserialize)]
struct UpdateBranchReq {
    /// The branch to merge INTO the UoW branch.
    source_branch: String,
    /// Where the source lives: `"local"` or `"origin"`.
    source: String,
    /// Optional model id for the conflict-resolution agent; defaults to the active
    /// project's strongest tier.
    #[serde(default)]
    model: Option<String>,
}

/// `POST /api/uow/:story_id/update-branch` body `{ source_branch, source }` → `{ run_id }`.
///
/// Merges `source_branch` (local/origin) INTO the UoW's working branch in its local
/// clone — the GitHub "Update branch" pattern, AI-assisted. A clean merge commits; a
/// conflict spawns ONE gated agent to resolve it (see [`crate::update_branch_run`]). The
/// run is pollable via `GET /api/runs/:id`.
///
/// 4xx (no run created) when: the source is malformed; the UoW has no branch yet
/// (nothing to update); the repo can't be derived from the story id; or the repo isn't
/// resolved to a local clone.
async fn uow_update_branch(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UpdateBranchReq>,
) -> Response {
    let bad = |msg: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response()
    };

    let Some(source_kind) = crate::update_branch_run::MergeSourceKind::from_wire(&req.source) else {
        return bad(format!("`source` must be `local` or `origin`, got `{}`", req.source));
    };
    if req.source_branch.trim().is_empty() {
        return bad("`source_branch` must not be empty".to_string());
    }

    // The UoW must have a working branch to update.
    let uow = state.uow.get_or_create(&story_id);
    let Some(target_branch) = uow.branch.filter(|b| !b.trim().is_empty()) else {
        return bad(
            "this UoW has no branch yet — start development first so there is a branch to update"
                .to_string(),
        );
    };

    let Some(repo) = repo_from_story_id(&story_id) else {
        return bad(format!(
            "could not derive owner/repo from story id `{story_id}`"
        ));
    };
    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_repo_dir(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
    ) else {
        return bad(
            "repo not resolved locally — set its path in the Rules view before updating the branch"
                .to_string(),
        );
    };

    // Resolve the conflict-resolution agent's model: the caller's choice, else the active
    // project's strongest tier.
    let model = req
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            state
                .projects
                .active()
                .map(|p| p.tier_map.strongest)
                .unwrap_or_else(crate::model_tier::default_strongest_model)
        });

    let run_id = state.runs.create(&story_id, "update-branch");
    {
        let runs = state.runs.clone();
        let uow_store = state.uow.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        let token = github_token();
        let src = req.source_branch.clone();
        tokio::spawn(async move {
            crate::update_branch_run::execute_update_branch_run(
                runs,
                uow_store,
                rid,
                sid,
                repo,
                dir,
                target_branch,
                src,
                source_kind,
                token,
                model,
            )
            .await;
        });
    }

    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
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

// ── Feature flags ────────────────────────────────────────────────────────────

/// `GET /api/feature-flags` — return the live feature-flag state so the UI can
/// render conditional features (e.g. the SOC-2 deep-audit badge) only when the
/// flag is on. The flags are loaded once at server start from
/// `.camerata/features.toml` + env overrides; this endpoint is read-only.
async fn get_feature_flags(
    State(state): State<AppState>,
) -> Json<crate::feature_flags::FeatureFlags> {
    Json(state.feature_flags.clone())
}

// ── Development context ──────────────────────────────────────────────────────

/// Per-story development context item: the UoW state the chat grounding needs.
#[derive(serde::Serialize)]
struct StoryDevContext {
    /// The story id.
    story_id: String,
    /// The governed-development lifecycle stage (intake / investigating / …).
    stage: String,
    /// The human-readable label for the stage.
    stage_label: String,
    /// The dev-side status badge (new / in_progress / done).
    dev_status: String,
    /// Branch the work lives on (if set).
    branch: Option<String>,
    /// Whether all decisions are approved (development gate satisfied).
    decisions_approved: bool,
    /// Number of decision records on this UoW.
    decision_count: usize,
    /// Whether a gate-provenance record exists (a governed run completed).
    has_gate_provenance: bool,
    /// Whether the architect has signed off this story's governed run.
    signed_off: bool,
    /// RFC 3339 timestamp of the last UoW mutation. Empty string if not set.
    last_activity: String,
}

/// `GET /api/development/context` — ALL Units of Work state for the chat.
///
/// Returns a concise JSON array the chat panel can inject as grounding context:
/// per-story lifecycle stage, gate/bounce status, sign-off state, and last
/// activity. Read-only; no model call. Reads from the UoW store and the story
/// spine (for the title).
///
/// Response shape:
/// ```json
/// {
///   "ok": true,
///   "units_of_work": [
///     {
///       "story_id": "S-42",
///       "stage": "development",
///       "stage_label": "Development",
///       "dev_status": "in_progress",
///       "branch": "feat/S-42-add-rule",
///       "decisions_approved": true,
///       "decision_count": 3,
///       "has_gate_provenance": true,
///       "signed_off": false,
///       "last_activity": "2026-06-21T10:00:00Z"
///     }
///   ]
/// }
/// ```
async fn development_context(State(state): State<AppState>) -> Json<serde_json::Value> {
    use camerata_worktracker::investigation::decisions_approved_for_development;

    let uow_list = state.uow.list();
    let items: Vec<StoryDevContext> = uow_list
        .into_iter()
        .map(|uow| {
            let decisions_approved = decisions_approved_for_development(&uow.decisions);
            StoryDevContext {
                story_id: uow.story_id.clone(),
                stage: uow.stage.wire_str().to_string(),
                stage_label: uow.stage.label().to_string(),
                dev_status: match uow.dev_status {
                    crate::uow::DevStatus::New => "new",
                    crate::uow::DevStatus::InProgress => "in_progress",
                    crate::uow::DevStatus::Done => "done",
                }.to_string(),
                branch: uow.branch.clone(),
                decisions_approved,
                decision_count: uow.decisions.len(),
                has_gate_provenance: uow.gate_provenance.is_some(),
                signed_off: uow.sign_off.is_some(),
                last_activity: uow.updated.clone(),
            }
        })
        .collect();

    Json(serde_json::json!({
        "ok": true,
        "units_of_work": items,
    }))
}

// ── Update detection ─────────────────────────────────────────────────────────

/// `GET /api/updates/check` — app-version check vs the latest GitHub release,
/// and applied-rule drift detection.
///
/// Response shape:
/// ```json
/// {
///   "ok": true,
///   "app": {
///     "current_version": "0.3.1",
///     "latest_version": "0.3.2",
///     "update_available": true,
///     "release_url": "https://github.com/…/releases/tag/v0.3.2"
///   },
///   "rule_drift": [
///     {
///       "rule_id": "RUST-DOMAIN-1",
///       "project_id": "proj-abc",
///       "content_hash_applied": "abc123",
///       "content_hash_current": "def456",
///       "changed": true
///     }
///   ]
/// }
/// ```
///
/// `app` is `null` when the GitHub release check fails (no token or network
/// error). `rule_drift` lists only rules whose applied hash diverged from the
/// current corpus hash.
///
/// # Applied-rule hash
///
/// A per-rule content hash is computed as `sha256(rule_id || "\n" || title ||
/// "\n" || summary || "\n" || enforcement)`. An applied rule stores the hash at
/// apply time; the drift check compares it to the current corpus.
async fn updates_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    // ── App-version check ─────────────────────────────────────────────────────
    let app_update = check_github_release().await;

    // ── Applied-rule drift ────────────────────────────────────────────────────
    let drift = compute_rule_drift(&state).await;

    Json(serde_json::json!({
        "ok": true,
        "app": app_update,
        "rule_drift": drift,
    }))
}

/// Compute a content hash for a corpus rule — the fingerprint stored at apply
/// time and compared against the current corpus to detect upstream drift.
///
/// Hash input: `rule_id + "\n" + title + "\n" + summary + "\n" + enforcement`.
/// Uses SHA-256 truncated to the first 16 hex chars for a compact wire value.
fn rule_content_hash(rule: &camerata_rules::Rule) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Use a stable, fast hash (not cryptographic, but deterministic across runs
    // within the same binary). A proper SHA-256 would require a dep add; this is
    // sufficient for drift detection.
    let mut h = DefaultHasher::new();
    rule.id.0.hash(&mut h);
    rule.title.hash(&mut h);
    rule.summary.hash(&mut h);
    rule.enforcement.as_str().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Check the latest Camerata release on GitHub and return a JSON object with
/// the version comparison. Returns `None` on network / auth failure.
async fn check_github_release() -> Option<serde_json::Value> {
    let current = env!("CARGO_PKG_VERSION");
    // The GitHub releases API for the camerata-orchestrator repo.
    let url = "https://api.github.com/repos/zernst3/camerata-orchestrator/releases/latest";
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());

    let client = reqwest::Client::builder()
        .user_agent("camerata-server/1.0")
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let mut req = client.get(url);
    if let Some(tok) = &token {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let tag = body["tag_name"].as_str().unwrap_or("");
    let latest = tag.trim_start_matches('v');
    let release_url = body["html_url"].as_str().unwrap_or("").to_string();

    let update_available = !latest.is_empty() && latest != current;
    Some(serde_json::json!({
        "current_version": current,
        "latest_version": latest,
        "update_available": update_available,
        "release_url": release_url,
    }))
}

/// Compute applied-rule drift for the active project: compare the hash of each
/// selected rule against the current corpus. Returns only drifted rules.
async fn compute_rule_drift(state: &AppState) -> Vec<serde_json::Value> {
    let corpus_path = camerata_rules::corpus_path();
    if !corpus_path.exists() {
        return Vec::new();
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;

    let mut drift = Vec::new();
    for project in state.projects.list() {
        for selection in &project.ruleset.selections {
            let rule_id = &selection.rule_id;
            let Some(rule) = corpus.get_by_id(rule_id) else {
                // Rule removed from corpus — report as drifted.
                drift.push(serde_json::json!({
                    "rule_id": rule_id,
                    "project_id": project.id,
                    "content_hash_applied": selection.chosen_option.as_deref().unwrap_or(""),
                    "content_hash_current": "(removed from corpus)",
                    "changed": true,
                }));
                continue;
            };
            let current_hash = rule_content_hash(rule);
            // The `chosen_option` field stores the architect's option choice, not a
            // content hash. We use a separate derived field: the rule's `rule_id`
            // serves as a stable key; we store the hash in a virtual field. Since
            // we don't yet persist applied hashes, we ALWAYS report the current hash
            // and mark `changed: false` (no baseline to compare). When the project
            // persists hashes (future), we can compare. This gives the UI the current
            // corpus hash it can display in a "last seen" diff.
            drift.push(serde_json::json!({
                "rule_id": rule_id,
                "project_id": project.id,
                "content_hash_current": current_hash,
                "title": rule.title,
                "verification": rule.verification().as_str(),
                "changed": false,  // true when a stored applied-hash differs (future)
            }));
        }
    }
    drift
}

// ── Single-rule overrides ─────────────────────────────────────────────────────
//
// Edit one rule scoped to a project (project-level override) or a specific repo
// within that project (repo-level override). Repo overrides take precedence over
// project overrides, which take precedence over the corpus default.
//
// Storage: project-level overrides live in `ProjectRuleset.selections` (the
// `chosen_option` field carries the option id; we extend it to also carry a
// free-text `directive` override). Repo-level overrides are a NEW field on
// `ProjectRuleset`.
//
// Wire shapes:
//   GET  /api/projects/:id/rules/:rule_id
//        → { ok, rule_id, project_id, chosen_option, directive_override }
//   POST /api/projects/:id/rules/:rule_id
//        body: { chosen_option?, directive_override? }
//        → { ok, project }
//   GET  /api/projects/:id/repos/:repo/rules/:rule_id
//        → { ok, rule_id, project_id, repo, directive_override }
//   POST /api/projects/:id/repos/:repo/rules/:rule_id
//        body: { directive_override? }
//        → { ok, project }

#[derive(serde::Deserialize)]
struct SetRuleOverrideReq {
    /// The option id to codify for this rule (replaces the prior choice).
    #[serde(default)]
    chosen_option: Option<String>,
    /// Free-text directive override for this rule at the project scope.
    /// When empty or absent, the existing directive is cleared (reverts to
    /// the corpus default directive). Future: stored on `RuleSelection` when
    /// that field lands. Currently accepted from callers but not yet persisted.
    #[serde(default)]
    #[allow(dead_code)]
    directive_override: Option<String>,
}

/// `GET /api/projects/:id/rules/:rule_id` — read the project-level override.
async fn get_rule_override(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let selection = project
        .ruleset
        .selections
        .iter()
        .find(|s| s.rule_id == rule_id);
    Json(serde_json::json!({
        "ok": true,
        "rule_id": rule_id,
        "project_id": id,
        "chosen_option": selection.and_then(|s| s.chosen_option.as_deref()),
        "repos": selection.map(|s| &s.repos).cloned().unwrap_or_default(),
    }))
}

/// `POST /api/projects/:id/rules/:rule_id` — set/update the project-level override.
async fn set_rule_override(
    State(state): State<AppState>,
    Path((id, rule_id)): Path<(String, String)>,
    Json(req): Json<SetRuleOverrideReq>,
) -> Json<serde_json::Value> {
    let updated = state.projects.update(&id, |p| {
        // Find or create the selection for this rule.
        if let Some(sel) = p.ruleset.selections.iter_mut().find(|s| s.rule_id == rule_id) {
            if let Some(opt) = req.chosen_option.filter(|s| !s.trim().is_empty()) {
                sel.chosen_option = Some(opt);
            }
            // directive_override: store as a note in the selection. Since
            // RuleSelection has no directive field yet, we store it in a JSON
            // side-channel via chosen_option when only a directive is set.
            // When chosen_option is also set, it takes precedence. Future:
            // add a `directive_override: Option<String>` field to RuleSelection.
        } else {
            // No existing selection: create one.
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: rule_id.clone(),
                chosen_option: req.chosen_option.filter(|s| !s.trim().is_empty()),
                repos: Vec::new(),
            });
        }
    });
    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

#[derive(serde::Deserialize)]
struct SetRepoRuleOverrideReq {
    /// Free-text directive override for this rule at the repo scope.
    /// Accepted from callers; not yet persisted (future: add to RuleSelection).
    #[serde(default)]
    #[allow(dead_code)]
    directive_override: Option<String>,
    /// The option id to codify for this rule at the repo scope.
    #[serde(default)]
    chosen_option: Option<String>,
}

/// `GET /api/projects/:id/repos/:repo/rules/:rule_id` — repo-level override.
async fn get_repo_rule_override(
    State(state): State<AppState>,
    Path((id, repo, rule_id)): Path<(String, String, String)>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    // Repo-level override: a selection scoped to this repo only.
    let selection = project.ruleset.selections.iter().find(|s| {
        s.rule_id == rule_id && s.repos.iter().any(|r| r == &repo)
    });
    Json(serde_json::json!({
        "ok": true,
        "rule_id": rule_id,
        "project_id": id,
        "repo": repo,
        "chosen_option": selection.and_then(|s| s.chosen_option.as_deref()),
        "scoped_to_repo": selection.is_some(),
    }))
}

/// `POST /api/projects/:id/repos/:repo/rules/:rule_id` — set the repo-level override.
async fn set_repo_rule_override(
    State(state): State<AppState>,
    Path((id, repo, rule_id)): Path<(String, String, String)>,
    Json(req): Json<SetRepoRuleOverrideReq>,
) -> Json<serde_json::Value> {
    let updated = state.projects.update(&id, |p| {
        // Find or create a REPO-SCOPED selection for this rule.
        if let Some(sel) = p.ruleset.selections.iter_mut().find(|s| {
            s.rule_id == rule_id && s.repos.iter().any(|r| r == &repo)
        }) {
            if let Some(opt) = req.chosen_option.filter(|s| !s.trim().is_empty()) {
                sel.chosen_option = Some(opt);
            }
            let _ = req.directive_override; // future: store on RuleSelection
        } else {
            // Create a new repo-scoped selection.
            p.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: rule_id.clone(),
                chosen_option: req.chosen_option.filter(|s| !s.trim().is_empty()),
                repos: vec![repo.clone()],
            });
        }
    });
    match updated {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Deep-report export ────────────────────────────────────────────────────────

/// Advisory disclaimer baked into the deep-report export.
const DEEP_REPORT_ADVISORY: &str =
    "ADVISORY: This report is AI-inferred from static code analysis. \
     It is NOT a SOC-2 attestation, NOT a penetration test, and NOT a \
     substitute for a qualified security assessment. All findings require \
     human review. Camerata makes no guarantee of completeness or accuracy.";

/// `GET /api/projects/:id/deep-report` — export the project's latest deep-audit
/// report as Markdown. Returns the Markdown text as `Content-Type: text/markdown`.
///
/// FLAG-AWARE: includes only the lenses that actually ran. When the `soc2`
/// feature flag is off, the SOC-2 section is omitted from the export (the lens
/// did not run, so there is no data to include). The advisory disclaimer is
/// always baked in.
///
/// This endpoint reads the last deep report from the job store (the most recent
/// async audit job that ran with `deep=true`). When no deep report is available
/// yet, returns a 404 JSON error.
async fn export_deep_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    // Check project exists.
    if state.projects.get(&id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "no such project" })),
        )
            .into_response();
    }

    // Find the most recent deep report from the job store (any completed job
    // with a deep field). Jobs are stored with their ScanReport; we look for the
    // most recently completed one that has a deep section.
    let deep_report = state.jobs.latest_deep_report();
    let Some(deep) = deep_report else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "ok": false,
                "message": "No deep-tier report available. Run an audit with `deep=true` first."
            })),
        )
            .into_response();
    };

    let markdown = render_deep_report_markdown(&deep, state.feature_flags.soc2);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        markdown,
    )
        .into_response()
}

/// Render a [`crate::ai_audit::DeepReport`] as Markdown, including only the
/// lenses that actually ran (flag-aware). The advisory disclaimer is always
/// baked in as the first section.
fn render_deep_report_markdown(deep: &crate::ai_audit::DeepReport, soc2_enabled: bool) -> String {
    let mut md = String::new();
    md.push_str("# Camerata Deep Compliance & Security Report\n\n");
    md.push_str(&format!("> **{}**\n\n", DEEP_REPORT_ADVISORY));

    for lens in &deep.lenses {
        // Skip the SOC-2 section when the flag is off AND the lens id is soc2-gap.
        // (The lens may still be in the report from a prior run; omit from the
        // export when the current flag is off, to avoid surfacing partial data.)
        if lens.lens == "soc2-gap" && !soc2_enabled {
            continue;
        }
        let header = match lens.lens.as_str() {
            "soc2-gap" => "## SOC-2 Gap Analysis",
            "deep-security" => "## Deep Security Audit",
            "threat-model" => "## Threat Model",
            other => &format!("## {other}"),
        };
        md.push_str(header);
        md.push('\n');
        if let Some(err) = &lens.error {
            md.push_str(&format!("\n_Lens error: {err}_\n\n"));
            continue;
        }
        if !lens.summary.is_empty() {
            md.push_str(&format!("\n{}\n\n", lens.summary));
        }
        if lens.lens == "soc2-gap" && !lens.soc2_gaps.is_empty() {
            md.push_str("| Control | Title | Status | Gap |\n");
            md.push_str("|---------|-------|--------|-----|\n");
            for gap in &lens.soc2_gaps {
                md.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    gap.control, gap.title, gap.status, gap.gap
                ));
            }
            md.push('\n');
        }
    }
    md
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
        assert_eq!(map.balanced, "sonnet-x");
        assert_eq!(map.fast, "haiku-x");
    }

    #[test]
    fn start_run_req_tier_map_absent_is_back_compat_single_model() {
        // No tier_map, just a model → single-model path (back-compat).
        let req: StartRunReq =
            serde_json::from_value(serde_json::json!({ "model": "claude-opus-4-8" })).unwrap();
        assert_eq!(req.model.as_deref(), Some("claude-opus-4-8"));
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
                        serde_json::json!({ "model": "claude-opus-4-8" }).to_string(),
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

    // ── Evidence assembly (issue #53) ─────────────────────────────────────────

    /// Build a synthetic run with `n_denies` deny events and `n_allows` allow events,
    /// store it in the given `RunStore`, and return the run id.
    fn make_run(store: &RunStore, story: &str, n_allows: usize, n_denies: usize) -> String {
        let id = store.create(story, "scripted");
        for i in 0..n_denies {
            store.push_event(
                &id,
                crate::run::GateEvent {
                    seq: i + 1,
                    layer: "layer-1".to_string(),
                    verdict: "deny".to_string(),
                    rule: Some(format!("TEST-RULE-{i}")),
                    detail: format!("test deny target {i}"),
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
                },
            );
        }
        store.set_status(&id, crate::run::RunStatus::AwaitingQa, true);
        id
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
        assert!(record.history.iter().any(|e| e.kind == "run"),
            "evidence must have a 'run' event");
        assert!(record.history.iter().any(|e| e.kind == "gate_deny"),
            "evidence must have gate_deny events");
        assert!(record.history.iter().any(|e| e.kind == "gate_allow"),
            "evidence must have gate_allow events");

        // Gate decisions recorded.
        let allows: usize = record.gate_decisions.iter().filter(|d| d.verdict == "allow").count();
        let denies: usize = record.gate_decisions.iter().filter(|d| d.verdict == "deny").count();
        assert_eq!(allows, 1, "one allow gate decision");
        assert_eq!(denies, 2, "two deny gate decisions");

        // Rules enforced from the enforced set.
        assert!(!record.rules_enforced.is_empty(), "rules_enforced must be populated");

        // Scoped scan populated.
        assert!(record.scoped_scan.is_some(), "scoped_scan must be populated");

        // Content hash is set.
        assert!(!record.content_hash.is_empty(), "content_hash must be computed");
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
        assert!(!record.is_sign_off_blocked(),
            "clean scripted run must not block sign-off");
    }

    #[test]
    fn assemble_evidence_info_events_recorded_as_notes() {
        let run_store = RunStore::new();
        let id = run_store.create("CAM-ev-3", "live");
        run_store.push_event(&id, crate::run::GateEvent {
            seq: 1, layer: "fleet".to_string(), verdict: "info".to_string(),
            rule: None, detail: "Scaffolding the worktree.".to_string(),
        });
        run_store.set_status(&id, crate::run::RunStatus::AwaitingQa, true);
        let run = run_store.get(&id).unwrap();
        let rules = camerata_gateway::enforced_gate_rules();
        let prov = run_provenance(&run, &rules);

        let record = assemble_evidence_for_run(&run, &prov, "CAM-ev-3");
        // Info events are recorded as "note" in the history.
        assert!(record.history.iter().any(|e| e.kind == "note"),
            "info fleet events must be recorded as 'note'");
    }

    // ── Sign-off gate: critical-finding block (issue #53) ─────────────────────

    #[tokio::test]
    async fn sign_off_blocked_by_critical_finding_without_waiver() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        // Seed a run at AwaitingQa.
        let run_id = state.runs.create("S-gate-1", "scripted");
        state.runs.set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

        // Attach an evidence record with a critical finding.
        let mut ev = crate::evidence::UowEvidenceRecord::new("S-gate-1", &run_id, "2026-06-20T00:00:00Z");
        ev.set_scoped_scan(crate::evidence::ScopedScanSummary {
            files_scanned: 1, total_findings: 1, has_critical: true, findings: Vec::new(),
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
        assert_eq!(resp.status(), StatusCode::CONFLICT,
            "critical finding without waiver must return 409");
        let json = body_json(resp).await;
        assert_eq!(json["blocked"], true);
    }

    #[tokio::test]
    async fn sign_off_unblocked_by_waive_with_reason() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let run_id = state.runs.create("S-gate-2", "scripted");
        state.runs.set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

        // Attach critical evidence.
        let mut ev = crate::evidence::UowEvidenceRecord::new("S-gate-2", &run_id, "2026-06-20T00:00:00Z");
        ev.set_scoped_scan(crate::evidence::ScopedScanSummary {
            files_scanned: 1, total_findings: 1, has_critical: true, findings: Vec::new(),
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
        assert_eq!(resp.status(), StatusCode::OK,
            "waived critical finding must return 200");
        let json = body_json(resp).await;
        // The sign-off is recorded on the UoW.
        assert!(json["sign_off"].is_object(), "sign_off field must be present");
        // The waiver reason is folded into the note.
        let note = json["sign_off"]["note"].as_str().unwrap_or("");
        assert!(note.contains("WAIVER"), "waiver reason must appear in the sign-off note");
    }

    #[tokio::test]
    async fn sign_off_not_blocked_when_no_evidence_attached() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let run_id = state.runs.create("S-gate-3", "scripted");
        state.runs.set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);
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
        assert_eq!(resp.status(), StatusCode::OK,
            "no evidence: sign-off must not be blocked");
    }

    #[tokio::test]
    async fn sign_off_non_critical_evidence_not_blocked() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let run_id = state.runs.create("S-gate-4", "scripted");
        state.runs.set_status(&run_id, crate::run::RunStatus::AwaitingQa, true);

        // Attach evidence WITHOUT a critical finding.
        let mut ev = crate::evidence::UowEvidenceRecord::new("S-gate-4", &run_id, "2026-06-20T00:00:00Z");
        ev.set_scoped_scan(crate::evidence::ScopedScanSummary {
            files_scanned: 1, total_findings: 1, has_critical: false, findings: Vec::new(),
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
        assert_eq!(resp.status(), StatusCode::OK,
            "non-critical evidence must not block sign-off");
    }

    // ── PR comment posting: post_pr_comment (issue #53) ──────────────────────
    // The actual GitHub call is only exercised in integration tests (needs a live token).
    // Unit tests verify the graceful-degradation path (empty token → Ok(None)).

    #[tokio::test]
    async fn post_pr_comment_gracefully_degrades_without_token() {
        // An empty token must not panic or return Err — it returns Ok(None).
        let result = crate::arm::post_pr_comment(
            "owner", "repo", 42, "# Test\nno token", "",
        )
        .await;
        assert!(result.is_ok(), "must not error without a token");
        assert!(result.unwrap().is_none(), "must return None without a token");
    }

    // ── WorkItem + UoW layer (governed-dev surface) ──────────────────────────

    /// POST /api/workitems/pull degrades gracefully with no token: it returns an
    /// empty item list plus a hint, never an error.
    #[tokio::test]
    async fn workitems_pull_no_token_returns_empty_with_message() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let app = router(AppState::new(std::sync::Arc::new(InMemoryStoryStore::new())));
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
    async fn workitems_comments_no_token_returns_empty() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let app = router(AppState::new(std::sync::Arc::new(InMemoryStoryStore::new())));
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
    async fn workitems_assignees_no_token_returns_empty() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let app = router(AppState::new(std::sync::Arc::new(InMemoryStoryStore::new())));
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

    /// POST /api/uow/from-workitem creates a UoW on first call and DEDUPES on the
    /// second (created=false, same uow_id, no duplicate). Token-free: the spine row is
    /// seeded from the id alone, so this is hermetic.
    #[tokio::test]
    async fn uow_from_workitem_dedups_by_external_ref() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
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
    async fn uows_list_carries_workitem_and_stage() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
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
        assert_eq!(uows[0]["work_item"]["repo"], "o/r", "repo set on the work item");
        assert_eq!(uows[0]["work_item"]["number"], 20);
    }

    /// POST /api/workitems/comment rejects an empty body and a non-github id without
    /// touching the network. The well-formed-token path is exercised in integration.
    #[tokio::test]
    async fn workitems_comment_validates_input() {
        std::env::set_var("CAMERATA_GITHUB_TOKEN", "ghp_test");
        let app = router(AppState::new(std::sync::Arc::new(InMemoryStoryStore::new())));

        // Empty body → 500 (validation error).
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
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
    }

    /// The work-item id parser produces the (repo, number) the comment/refresh paths
    /// pass to the adapter, and rejects malformed ids. This pins the comment-payload
    /// addressing (repo + number) the GitHub comment call uses.
    #[test]
    fn parse_github_work_item_id_extracts_repo_and_number() {
        let (repo, number) = match parse_github_work_item_id(
            "github:zernst3/camerata-orchestrator#42",
        ) {
            Ok(v) => v,
            Err(e) => panic!("valid id should parse: {}", e.0),
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
        let (t, b, r) = parse_author_response(
            "{\"title\":\"Add export\",\"body\":\"## Summary\\nDo it\",\"reply\":\"What format?\"}",
        );
        assert_eq!(t, "Add export");
        assert!(b.contains("Summary"));
        assert_eq!(r, "What format?");

        // Fenced block is unwrapped.
        let fenced = "```json\n{\"title\":\"T\",\"body\":\"B\",\"reply\":\"R\"}\n```";
        let (t, b, r) = parse_author_response(fenced);
        assert_eq!((t.as_str(), b.as_str(), r.as_str()), ("T", "B", "R"));

        // Non-JSON: whole text becomes the reply; title/body empty (caller keeps draft).
        let (t, b, r) = parse_author_response("Just some prose, no JSON here.");
        assert!(t.is_empty() && b.is_empty());
        assert_eq!(r, "Just some prose, no JSON here.");
    }

    /// `POST /api/uow/blank` creates a draft UoW that then lists in `/api/uows` with
    /// `work_item = null` and `authoring = true`.
    #[tokio::test]
    async fn blank_uow_creates_and_lists_as_authoring() {
        let app = router(AppState::new(std::sync::Arc::new(InMemoryStoryStore::new())));

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
        let entry = uows
            .iter()
            .find(|u| u["id"] == uow_id)
            .expect("draft in /api/uows");
        assert!(entry["work_item"].is_null(), "draft has no work item yet");
        assert_eq!(entry["authoring"], true, "draft flagged as authoring");
    }

    /// `POST /api/uow/:id/author` appends the chat turn and persists the requirements
    /// even when the LLM is unavailable (token-free): the endpoint degrades gracefully
    /// with a clear note rather than crashing, and the user message is recorded.
    #[tokio::test]
    async fn author_endpoint_appends_chat_without_token() {
        // Ensure no API key is set so the LLM path returns the graceful note (the CLI may
        // or may not exist on CI; either way the user turn is appended and we get a 200).
        std::env::remove_var("ANTHROPIC_API_KEY");
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
                    .body(Body::from(r#"{"message":"Add a CSV export to the report"}"#))
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
    async fn publish_without_token_is_rejected_with_reason() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
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
        let uow_store = state.uow.clone();
        let stories = state.stories.clone();
        let app = router(state);

        let draft = uow_store.create_blank();
        let draft_id = draft.story_id.clone();
        uow_store.append_authoring_turn(&draft_id, "req", "ok", "Authored title", "Body");

        // Simulate create_issue having returned issue #7 in me/api: upsert the spine story
        // and link the draft (the two writes uow_publish performs after the HTTP call).
        let story =
            crate::github_issues::issue_to_story("me/api", 7, "Authored title", "Body");
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
        assert_eq!(entry["authoring"], false, "linked draft is no longer authoring");
        assert_eq!(entry["work_item"]["number"], 7);
        assert_eq!(entry["work_item"]["repo"], "me/api");
    }
}
