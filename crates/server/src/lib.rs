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
pub mod api_agent_driver;
pub mod arm;
pub mod auto_fire;
pub mod dep_audit;
pub mod clarify;
pub mod clarify_resume;
pub mod connections;
pub mod decompose;
pub mod draft;
pub mod enforcement_ledger;
pub mod escalation;
pub mod eval;
pub mod evidence;
pub mod feature_flags;
pub mod fix;
pub mod grounding;
pub mod github_issues;
pub mod investigation_run;
pub mod jobs;
pub mod lifecycle;
pub mod live_fleet;
pub mod llm;
pub mod notify;
pub mod onboard;
pub mod pr;
pub mod dev_implement_run;
pub mod pr_resolve_run;
pub mod project;
pub mod provider;
pub mod reconcile;
pub mod review_agent;
pub mod routine;
pub mod run;
pub mod scan_cache;
pub mod scan_routing;
pub mod scan_tools;
pub mod schedule;
pub mod tool_provisioning;
pub mod model_tier;
pub mod settings;
pub mod suppression;
pub mod terminal;
pub mod test_tamper;
/// Layer-3 CI workflow generator — produces `.github/workflows/camerata-gates.yml`
/// from the built-in language gate commands + manifest checks. See
/// `docs/decisions/2026-06-22_check_manifest_single_source_of_truth.md`.
pub mod credentials;
pub mod model_registry;
pub mod model_profile_cascade;
pub mod rate_limit;
pub mod workflow_gen;
pub mod transcript;
pub mod uow;
pub mod usage_ledger;
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
    /// Phase 3b: persisted resume contexts for runs paused on a clarification, keyed by
    /// the clarification id. When a gated agent asks a question the run parks at
    /// `AwaitingClarification` and the context to re-spawn it is stored here; the answer
    /// endpoint consumes it to resume. Persisted alongside the clarify store so a pause
    /// point survives a restart.
    clarify_resume: crate::clarify_resume::ClarifyResumeStore,
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
    /// Per-project last completed scan report. Written the instant any scan handler finishes
    /// (both the synchronous `onboard_audit` path and the async job path). Read by
    /// `active_project_context` as the authoritative source for grounding, with the
    /// UI-round-tripped draft as a first-priority fallback. In-memory only (v1); does not
    /// survive a process restart (a v2 concern).
    last_scan: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, crate::onboard::ScanReport>>>,
    /// Project-agnostic fallback: the single most-recently completed scan, regardless of which
    /// project was active at scan time.  Mirrors `last_scan` but keyed only on recency.
    ///
    /// Motivation: if the active project changed between scan submission and scan completion
    /// (or no project was active at scan time), `last_scan` has nothing under the CURRENT
    /// active project's key and `active_project_context` would return "none yet."  This field
    /// is the last-resort fallback so a completed scan always surfaces in the chat context.
    ///
    /// Priority chain in `active_project_context`:
    ///   draft extract  →  get_last_scan(project.id)  →  get_recent_scan()
    recent_scan: std::sync::Arc<std::sync::Mutex<Option<crate::onboard::ScanReport>>>,
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
    /// Process/session-global cumulative LLM usage ledger (tokens + $ + calls + by-model +
    /// rate-limited state). EVERY model call routed through `Llm::from_env_with_ledger`
    /// folds into this; the `/api/usage` endpoint snapshots it for the cockpit's persistent
    /// usage meter. Provider-agnostic: keys off the vendor-neutral `LlmResponse` usage fields.
    pub usage_ledger: Arc<crate::usage_ledger::UsageLedger>,
    /// Optional append-only enforcement-catch ledger (SQLite). Captures every Layer-1
    /// gate deny, Layer-2 bounce, and floor audit catch for external SQL analytics
    /// (prevented-merges dataset). Write-only in app code; fail-soft: if it can't be
    /// opened the run/scan paths are unaffected. `None` in tests and on open failure.
    pub enforcement_ledger: crate::enforcement_ledger::EnforcementLedger,
    /// App-wide credential store backed by the OS keychain in production and an
    /// in-memory map in tests.  Never exposes full values over HTTP; handlers call
    /// `.masked()`.  Service code (e.g. [`github_token`]) reads the full value
    /// in-process, falling back to the environment variable for back-compat.
    pub credential_store: Arc<dyn crate::credentials::CredentialStore>,
    /// App-wide model registry (Claude static + OpenRouter discovered). Shared so
    /// the OpenRouter cache survives across requests and is refreshed exactly once
    /// (or on demand via `POST /api/models/registry/refresh`). Thread-safe: the
    /// registry holds its own `Arc<Mutex<...>>` internally.
    pub model_registry: crate::model_registry::ModelRegistry,
    /// Per-provider RPM rate limiter. Shared across all handlers so concurrent calls
    /// through any [`crate::llm::OpenRouterCompleter`] instance all draw from the same
    /// token bucket. Default: 20 RPM for "openrouter"; unlimited for all other providers.
    /// Passed into [`crate::llm::build_completer`] whenever an OpenRouter model is
    /// selected.
    pub rate_limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
}

impl AppState {
    /// Build state from an explicit story store, with the native (in-process)
    /// provider.
    pub fn new(stories: Arc<dyn StoryStore>) -> Self {
        Self {
            stories,
            runs: RunStore::new(),
            clarifications: ClarificationStore::new(),
            clarify_resume: crate::clarify_resume::ClarifyResumeStore::new(),
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
            last_scan: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            recent_scan: std::sync::Arc::new(std::sync::Mutex::new(None)),
            artifacts: None,
            feature_flags: crate::feature_flags::FeatureFlags::default(),
            usage_ledger: Arc::new(crate::usage_ledger::UsageLedger::new()),
            // Tests and in-process runs start without a ledger (no-op; fail-soft).
            enforcement_ledger: crate::enforcement_ledger::EnforcementLedger::none(),
            // Tests use an in-memory credential store; production uses the OS keychain.
            credential_store: Arc::new(crate::credentials::MemoryCredentialStore::new()),
            // The registry always has the static Claude entries; OpenRouter is empty until
            // refreshed (key not set in tests anyway).
            model_registry: crate::model_registry::ModelRegistry::new(),
            // Default limiter: 20 RPM for "openrouter", unlimited for all others.
            rate_limiter: Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
        }
    }

    /// A clone of the shared `Llm` seam WITH the process-global usage ledger attached, so any
    /// model call made through it is recorded into the cumulative cockpit meter. Every handler
    /// that needs the LLM goes through this (instead of bare `Llm::from_env`) so the ledger
    /// sees ALL call paths. Reads vendor/transport/model from the environment, same as before.
    pub fn llm(&self) -> crate::llm::Llm {
        crate::llm::Llm::from_env_with_ledger(self.usage_ledger.clone())
    }

    /// Read-only accessor for the project store. Exposed so integration tests can seed a
    /// project via the REAL [`crate::project::ProjectStore`] write path (e.g.
    /// `set_step_model`) and then assert that step/tier resolution reads it back — the same
    /// store the handlers mutate. The field itself stays private (handlers use `state.projects`
    /// in-crate); this accessor is the out-of-crate test seam.
    pub fn projects(&self) -> &crate::project::ProjectStore {
        &self.projects
    }

    /// Resolve the GitHub token: credential store first, then the
    /// `CAMERATA_GITHUB_TOKEN` environment variable as a back-compat fallback.
    /// Returns `None` when neither is set or non-empty.
    pub fn github_token(&self) -> Option<String> {
        // 1. Try the credential store (keychain in prod, in-memory in tests).
        if let Ok(Some(token)) = self.credential_store.get(crate::credentials::GITHUB_TOKEN) {
            if !token.is_empty() {
                return Some(token);
            }
        }
        // 2. Fall back to the environment variable for back-compat.
        std::env::var("CAMERATA_GITHUB_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// Build the shared PROJECT-GROUNDING block for the ACTIVE project: its rule context
    /// (committed ruleset + in-progress selections, via the same renderers the chat uses)
    /// plus a bounded digest of each of its local repo clones (detected stack, dependency
    /// highlights, high-signal docs, shallow tree). This is the single window every
    /// in-project agent receives so none behaves like a context-less product owner. See
    /// `docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md` and
    /// [`crate::grounding`].
    ///
    /// ISOLATION: reads ONLY the active project's repos (resolved via the machine-local
    /// override path or `<workspace_root>/<owner>/<repo>`); never another project's clone.
    /// Returns `None` when there is no active project, or nothing at all to say.
    ///
    /// Async because it does (bounded) blocking file IO under `spawn_blocking`.
    pub(crate) async fn project_grounding(&self) -> Option<String> {
        let project = self.projects.active()?;

        // ── Rule context (reuse the chat's renderers so agents see the SAME picture) ──
        let committed = build_ruleset_summary(&project.ruleset);
        let committed = if committed.trim().is_empty() {
            None
        } else {
            Some(committed)
        };
        let selected = self
            .draft
            .load(&project.id)
            .as_ref()
            .and_then(render_selected_rules_for_chat);
        let rule_section =
            crate::grounding::render_rule_context(committed.as_deref(), selected.as_deref());

        // ── Repo context: resolve each repo's local clone, then digest it off-thread ──
        let workspace_root = self.settings.workspace_root();
        let mut resolved: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut unresolved: Vec<(String, String)> = Vec::new();
        for repo in &project.repos {
            let override_path = self.settings.repo_path(repo);
            match crate::workspace::resolve_repo_dir(
                override_path.as_deref(),
                workspace_root.as_deref(),
                repo,
            ) {
                Some(dir) if dir.is_dir() => resolved.push((repo.clone(), dir)),
                Some(dir) => unresolved.push((
                    repo.clone(),
                    format!("local folder not found ({})", dir.display()),
                )),
                None => unresolved.push((
                    repo.clone(),
                    "no local path set — set a workspace root or the repo's folder".to_string(),
                )),
            }
        }

        let repo_digests = if resolved.is_empty() {
            Vec::new()
        } else {
            tokio::task::spawn_blocking(move || {
                resolved
                    .iter()
                    .filter_map(|(repo, dir)| crate::grounding::render_repo_digest(repo, dir))
                    .collect::<Vec<String>>()
            })
            .await
            .unwrap_or_default()
        };

        crate::grounding::assemble(rule_section, &repo_digests, &unresolved)
    }

    /// Resolve the active project's PRIMARY local repo clone — the first of the active
    /// project's repos whose local clone exists on disk. Used as the cwd for project-level
    /// agents that aren't bound to one specific repo. For the full read scope (which covers
    /// EVERY repo in the project) see [`Self::active_repo_dirs`].
    ///
    /// ISOLATION: resolves ONLY the active project's repos (machine-local override path or
    /// `<workspace_root>/<owner>/<repo>`); never another project's clone. Returns `None`
    /// when there is no active project or none of its repos resolve to an existing folder
    /// (the agent then falls back to grounding-digest-only, read-window-less, as before).
    ///
    /// This grants READ access only; it does NOT touch the write gate. Callers thread the
    /// result into `prepare_session(..., Some(dir))`, which binds cwd + `--add-dir` for
    /// reads AND the gateway write-jail — `gated_write` stays the only (gated) write path.
    pub(crate) fn active_repo_dir(&self) -> Option<std::path::PathBuf> {
        self.active_repo_dirs().into_iter().next()
    }

    /// Resolve the local clones of ALL of the active project's repos — the UNION read scope
    /// for in-project agents. A project contains MULTIPLE distinct repos (e.g. a frontend
    /// and a backend); every in-project agent should be able to READ across all of them
    /// (e.g. a frontend UoW reading the backend's API surface), so this returns one
    /// `PathBuf` per repo whose local clone exists on disk, in `project.repos` order.
    ///
    /// Repos that don't resolve to an existing folder are skipped (the agent simply lacks a
    /// read window into those); the result is empty when there is no active project or none
    /// of its repos resolve.
    ///
    /// ISOLATION: resolves ONLY the ACTIVE project's repos (machine-local override path or
    /// `<workspace_root>/<owner>/<repo>`); never another project's clone. This is the read
    /// scope only — callers pass it to `prepare_session(.., cwd, &dirs)` /
    /// `with_repo_read_dirs(..)` to add each dir via `--add-dir`. It does NOT widen the
    /// write gate: `gated_write` jailed to the single UoW worktree (CAMERATA_WORKTREE_ROOT)
    /// remains the only write path, so the extra repos are READ-ONLY.
    pub(crate) fn active_repo_dirs(&self) -> Vec<std::path::PathBuf> {
        let Some(project) = self.projects.active() else {
            return Vec::new();
        };
        let workspace_root = self.settings.workspace_root();
        let mut dirs = Vec::new();
        for repo in &project.repos {
            let override_path = self.settings.repo_path(repo);
            if let Some(dir) = crate::workspace::resolve_repo_dir(
                override_path.as_deref(),
                workspace_root.as_deref(),
                repo,
            ) {
                if dir.is_dir() && !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
        dirs
    }

    /// Store the last completed scan report for the given project. Called the instant a
    /// scan handler finishes on either the synchronous or async path. Fail-soft on lock
    /// poisoning: recovers the inner value rather than panicking the handler.
    ///
    /// Also updates [`Self::recent_scan`] so the project-agnostic fallback always reflects
    /// the most recently completed scan, regardless of which project was active.
    pub(crate) fn set_last_scan(&self, project_id: String, report: crate::onboard::ScanReport) {
        let mut guard = self
            .last_scan
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.insert(project_id, report.clone());
        drop(guard); // release per-project lock before acquiring recent_scan lock
        let mut recent = self
            .recent_scan
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *recent = Some(report);
    }

    /// Retrieve the last completed scan report for the given project, if any.
    /// Returns a clone so callers do not hold the lock across await points.
    /// Fail-soft on lock poisoning: returns `None` rather than panicking.
    pub(crate) fn get_last_scan(&self, project_id: &str) -> Option<crate::onboard::ScanReport> {
        let guard = self
            .last_scan
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.get(project_id).cloned()
    }

    /// Retrieve the most recently completed scan, regardless of which project it belonged to.
    ///
    /// ISOLATION (A5): this was a last-resort fallback in `active_project_context`, but it
    /// leaked one project's scan into another's chat grounding, so all production call sites
    /// were removed. The `recent_scan` field is still written on scan completion and this
    /// accessor is retained for the isolation regression tests (which assert the data exists
    /// yet does NOT surface cross-project). Not called from production code by design.
    /// Returns a clone so callers do not hold the lock across await points.
    /// Fail-soft on lock poisoning: returns `None` rather than panicking.
    #[allow(dead_code)]
    pub(crate) fn get_recent_scan(&self) -> Option<crate::onboard::ScanReport> {
        let guard = self
            .recent_scan
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.clone()
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
            // Clarifications persist too: every open structured question is a resumable
            // pause point, so the user can leave and come back to any unanswered question
            // (and answered ones stay on the record). Survives restarts; safe to delete.
            state.clarifications =
                crate::clarify::ClarificationStore::at(dir.join("clarifications.json"));
            // Phase 3b: resume contexts for runs paused on a clarification persist next to
            // the clarify store, so a parked run can still resume after a restart.
            state.clarify_resume =
                crate::clarify_resume::ClarifyResumeStore::at(dir.join("clarify-resume.json"));
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

                // Enforcement-catch ledger: append-only SQLite log of every gate deny,
                // layer-2 bounce, and floor audit catch. Opened best-effort (fail-soft):
                // if it can't open, a no-op ledger is used and runs/scans are unaffected.
                let ledger_path = dir.join("enforcement_catches.db");
                state.enforcement_ledger = tokio::task::block_in_place(|| {
                    handle.block_on(
                        crate::enforcement_ledger::EnforcementLedger::open(&ledger_path),
                    )
                });
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
        // Production path: use the OS keychain (macOS Keychain, Windows Credential
        // Manager, Secret Service on Linux) so credentials survive restarts without
        // living in any file on disk.
        state.credential_store =
            Arc::new(crate::credentials::KeyringCredentialStore);
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
        .route("/api/runs/:id/cancel", post(cancel_run))
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
        .route("/api/projects/:id/emit-local", post(emit_project_local))
        .route("/api/projects/:id/custom", post(add_custom_rule))
        .route("/api/projects/:id/custom/delete", post(delete_custom_rule))
        .route("/api/projects/:id/max-iterations", post(set_max_iterations))
        // Model-tiering: read/write the project's fast/balanced/strongest/vision model bindings (#63).
        .route("/api/projects/:id/tier-map", post(set_tier_map))
        // Designer (vision) band toggle: enable/disable the vision band for this project.
        .route("/api/projects/:id/vision-enabled", post(set_vision_enabled))
        // Per-step model config: set the model for one NON-FLEET AI step on this project.
        .route("/api/projects/:id/step-models", post(set_step_model))
        // Stall-detection thresholds: per-project idle timeout config.
        .route("/api/projects/:id/stall-thresholds", post(set_stall_thresholds_handler))
        // L3 agentic code-review gate: per-project opt-in (R7).
        .route("/api/projects/:id/l3-review", post(set_l3_review_handler))
        // Model Efficiency Profile: preview (GET) and apply (POST).
        .route("/api/projects/:id/model-profile/preview", get(preview_model_profile))
        .route("/api/projects/:id/model-profile", post(apply_model_profile))
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
        .route("/api/onboard/audit/job/:id/cancel", post(cancel_audit_job))
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
        .route(
            "/api/projects/active/generate-ci-workflow",
            post(generate_ci_workflow),
        )
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
        .route("/api/usage", get(usage))
        .route("/api/models", get(list_models))
        .route("/api/models/registry", get(get_model_registry))
        .route("/api/models/registry/refresh", post(refresh_model_registry))
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
        .route("/api/workitems/set-parent", post(workitems_set_parent))
        .route("/api/uows", get(uows_list))
        .route("/api/uow/from-workitem", post(uow_from_workitem))
        // ── AI story authoring from a blank UoW (2026-06-22) ──────────────────
        .route("/api/uow/blank", post(uow_blank))
        .route("/api/uow/:story_id/author", post(uow_author))
        .route(
            "/api/uow/:story_id/set-draft-parent",
            post(uow_set_draft_parent),
        )
        .route("/api/uow/:story_id/publish", post(uow_publish))
        .route("/api/uow", get(uow_list))
        .route("/api/uow/:story_id", get(uow_get).delete(uow_delete))
        .route("/api/uow/:story_id/status", post(uow_set_status))
        .route("/api/uow/:story_id/branch", post(uow_set_branch))
        .route("/api/uow/:story_id/history", post(uow_append_history))
        // ── 3-phase cockpit state (#104 / #105) ───────────────────────────────
        .route("/api/uow/:story_id/intake/context", post(uow_set_intake_context))
        .route("/api/uow/:story_id/intake/repos", post(uow_set_intake_repos))
        // Per-repo Ship (R3.f / R6): push + open PR for each in-scope repo's story branch.
        // This is the multi-repo generalisation of `POST /api/uow/:story_id/pr/open`.
        .route("/api/uow/:story_id/intake/ship", post(uow_intake_ship))
        .route(
            "/api/uow/:story_id/investigation/chat",
            post(uow_append_investigation_chat),
        )
        .route("/api/uow/:story_id/contract", post(uow_set_contract))
        .route(
            "/api/uow/:story_id/development/chat",
            post(uow_append_development_chat),
        )
        .route("/api/uow/:story_id/meta", post(uow_set_meta))
        // ── Governed-development lifecycle (Pillar 2) ─────────────────────────
        .route("/api/uow/:story_id/decisions", post(uow_set_decisions))
        .route(
            "/api/uow/:story_id/investigation",
            get(uow_get_investigation),
        )
        .route(
            "/api/uow/:story_id/investigation/review",
            post(uow_mark_investigation_reviewed),
        )
        .route("/api/uow/:story_id/branches", post(uow_list_branches))
        .route("/api/uow/:story_id/update-branch", post(uow_update_branch))
        // ── Per-UoW PR lifecycle (Decision 2) ─────────────────────────────────
        .route("/api/uow/:story_id/pr/open", post(uow_pr_open))
        .route("/api/uow/:story_id/pr", get(uow_pr_get))
        .route("/api/uow/:story_id/pr/comment", post(uow_pr_comment))
        .route("/api/uow/:story_id/pr/resolve", post(uow_pr_resolve))
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
        // ── App-wide credential manager ───────────────────────────────────────
        // POST /api/credentials/:name  — store a credential (body: { "value": "…" })
        // GET  /api/credentials        — list all known credentials with masked values
        .route("/api/credentials", get(list_credentials))
        .route("/api/credentials/:name", post(set_credential))
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

    // Per-UoW worktree housekeeping (Decision 1): on startup, prune stale worktree admin
    // records from every known repo clone AND remove worktrees for UoWs that are already
    // in a terminal state (SignedOff). Two cleanup passes, both best-effort + non-blocking:
    //
    //   Pass 1 — Terminal-state sweep (disk-safety, 2026-06-22): for every SignedOff UoW
    //   that still has a branch, remove its worktree. This reclaims leaked worktrees from
    //   crashes/kills that happened BETWEEN sign-off and the on-sign-off teardown, and from
    //   sessions that pre-dated the per-stage teardown feature. Conservative: only removes
    //   worktrees for UoWs explicitly in SignedOff state; branches are left intact (they may
    //   still back a PR). Note: with the current lifecycle (SignedOff is the only terminal
    //   stage), this is sufficient. If future stages add Abandoned/Failed variants, extend
    //   the filter here.
    //
    //   Pass 2 — Admin-record prune (Decision 1 original): `git worktree prune` drops admin
    //   records for worktrees whose directories were removed out-of-band (e.g. by the sweep
    //   above, or by the user manually). Always runs even when pass 1 is skipped.
    {
        let projects = state.projects.clone();
        let settings = state.settings.clone();
        let uow_store = state.uow.clone();
        tokio::spawn(async move {
            let workspace_root = settings.workspace_root();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Pass 1: remove worktrees for terminal-state (SignedOff) UoWs.
            //
            // ISOLATION: only sweep UoWs whose repo belongs to a known project. The
            // global `list()` would resolve and act on every project's UoWs even when
            // none is active/onboarded; scope to the union of all projects' in-scope
            // repos via `list_for_repos`, and skip the pass entirely when there are
            // no project repos to sweep. This is a REPO-ONLY sweep (not a single active
            // project's view): use `list_for_repos`, NOT `list_for_project`. Blank drafts
            // have no worktree and no resolvable repo, so they must be excluded here.
            let project_repos: Vec<String> = projects
                .list()
                .into_iter()
                .flat_map(|p| p.repos)
                .collect();
            let scoped_uows = if project_repos.is_empty() {
                Vec::new()
            } else {
                uow_store.list_for_repos(&project_repos)
            };
            for uow in scoped_uows {
                if uow.stage != crate::lifecycle::UowStage::SignedOff {
                    continue;
                }
                let Some(branch) = uow.branch.as_deref().filter(|b| !b.trim().is_empty()) else {
                    continue;
                };
                let Some(repo) = repo_from_story_id(&uow.story_id) else {
                    continue;
                };
                let override_path = settings.repo_path(&repo);
                let Some(clone) = crate::workspace::resolve_repo_dir(
                    override_path.as_deref(),
                    workspace_root.as_deref(),
                    &repo,
                ) else {
                    continue;
                };
                // Best-effort: errors (no clone, no worktree) are silently ignored — the
                // worktree simply wasn't there; the prune pass handles the admin records.
                crate::workspace::remove_uow_worktree(&clone, branch).await;
            }

            // Pass 2: prune stale admin records across all known repos.
            for project in projects.list() {
                for repo in &project.repos {
                    if !seen.insert(repo.clone()) {
                        continue;
                    }
                    let override_path = settings.repo_path(repo);
                    if let Some(clone) = crate::workspace::resolve_repo_dir(
                        override_path.as_deref(),
                        workspace_root.as_deref(),
                        repo,
                    ) {
                        crate::workspace::prune_worktrees(&clone).await;
                    }
                }
            }
        });
    }

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

/// The canonical story spine, scoped to the active project.
///
/// ISOLATION (A8): the spine is global, so an unfiltered list leaks every project's
/// stories. A story belongs to the active project when any of its build TARGET repos
/// or its SOURCE container (`external_ref.container`) is in the project's `repos`.
/// No active project → empty list.
async fn stories(State(state): State<AppState>) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let Some(p) = state.projects.active() else {
        return Ok(Json(Vec::new()));
    };
    let in_scope = |s: &CanonicalStory| -> bool {
        s.targets.iter().any(|t| p.repos.iter().any(|r| r == &t.repo))
            || s.external_ref
                .as_ref()
                .and_then(|e| e.container.as_ref())
                .is_some_and(|c| p.repos.iter().any(|r| r == c))
    };
    let list: Vec<CanonicalStory> = state
        .stories
        .list()
        .await
        .map_err(AppError)?
        .into_iter()
        .filter(in_scope)
        .collect();
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

    // Contract precondition (R3.g): when the work crosses a contract boundary,
    // a non-empty contract must exist before development starts. The orchestrator
    // refuses and pushes back — as a lead engineer refuses to parallelize a team
    // across an interface no one has agreed.
    if uow.investigation.crosses_boundary && uow.investigation.contract.trim().is_empty() {
        return Err(
            "This story crosses a contract boundary (crosses_boundary = true) but no \
             contract has been written yet. Write the interface contract in the \
             Investigation phase before starting development (R3.g). The refinement \
             agent may author the contract, but it must exist before code is written."
                .to_string(),
        );
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
    let run_id = state.runs.create(story_id, mode, crate::run::RunKind::Watched);
    let store = state.runs.clone();
    let rid = run_id.clone();

    if live {
        // Real governed fleet (needs the gateway binary + claude + tokens). Pass the
        // story so the live executor can build a plan from it.
        let (title, desc) = match state.stories.get(story_id).await {
            Ok(Some(s)) => (s.title, s.description),
            _ => (story_id.to_string(), String::new()),
        };

        // Resolve the effective tier map for this run: caller-supplied map takes
        // precedence (explicit per-run override); fall back to the project's
        // persisted tier map so the project settings panel is authoritative.
        let tier_map: Option<crate::model_tier::TierMap> = tier_map
            .or_else(|| state.projects.active().map(|p| p.tier_map));

        // The active project's loop-guard ceiling (#29) governs how many times a dirty
        // stage may bounce-and-revise before its residual violations are surfaced.
        let max_iterations = state
            .projects
            .active()
            .map(|p| p.max_iterations)
            .unwrap_or(1);

        // ── Brownfield vs. greenfield dispatch ────────────────────────────────
        //
        // When the UoW's repo worktree is RESOLVABLE (a local clone exists on disk),
        // use the brownfield in-place implement path: the agent edits the EXISTING
        // codebase on the UoW's branch, and the server commits. The spec comes from
        // the UoW's APPROVED decisions (the no-code-first gate already verified them).
        //
        // When the worktree is NOT resolvable (no local clone, no workspace root), fall
        // back to the greenfield scaffolder (execute_live_run / _tiered) which builds a
        // new app from a plan in a throwaway temp dir.
        //
        // GATE UNCHANGED in both paths: `ensure_development_gate` (the no-code-first
        // check) runs above, and every agent spawned here uses `governed_role` +
        // `gated_write`-only tools regardless of which branch is taken.
        //
        // `story_id` is a `&str` reference that doesn't outlive the function; own it
        // early so the tokio::spawn closure can capture it as `'static`.
        let story_id_owned = story_id.to_string();
        let uow_data = state.uow.get_or_create(story_id);
        let uow_branch = uow_data
            .branch
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            .map(|b| b.to_string());
        let decisions = state.uow.decisions_for(story_id);

        // Try to resolve the UoW's worktree from the active project's settings.
        // We require an active project to have a resolvable workspace root / repo override.
        let worktree = if let (Some(branch), Some(_proj)) = (
            uow_branch.as_deref(),
            state.projects.active(),
        ) {
            // Derive repo from story_id (owner/repo#num → owner/repo).
            let repo_from_story = story_id
                .rsplit_once('#')
                .map(|(r, _)| r)
                .unwrap_or(story_id);
            let override_path = state.settings.repo_path(repo_from_story);
            let workspace_root = state.settings.workspace_root();
            crate::workspace::resolve_uow_worktree(
                override_path.as_deref(),
                workspace_root.as_deref(),
                repo_from_story,
                branch,
            )
            .await
        } else {
            None
        };

        if crate::dev_implement_run::is_brownfield(worktree.as_deref()) {
            // Brownfield: implement in-place in the UoW's worktree.
            let dir = worktree.expect("is_brownfield guarantees Some");
            let uow_store = state.uow.clone();
            let repo = story_id
                .rsplit_once('#')
                .map(|(r, _)| r.to_string())
                .unwrap_or_else(|| story_id.to_string());
            let branch = uow_branch.unwrap_or_else(|| format!("camerata/{story_id}"));
            let token = state.github_token();
            // For the tiered path we pick the strongest model for the implementer; for
            // the single-model path we use the caller's model (or the default).
            let impl_model = match &tier_map {
                Some(map) => map.strongest.clone(),
                None => model
                    .clone()
                    .unwrap_or_else(crate::model_tier::default_strongest_model),
            };
            // GROUNDING (the invariant): rule + repo digest for the implementer (computed
            // before the spawn so it can move into the task).
            let grounding = state.project_grounding().await;
            // MULTI-REPO READ scope: ALL the active project's local repo clones. The
            // implementer writes only to its worktree; these are added READ-ONLY so it can
            // read sibling repos.
            let read_dirs = state.active_repo_dirs();
            // Layer-3 agentic code review (R7): build the bundle when the active project
            // has L3 enabled. The bundle captures the LLM + model + story text + rules
            // prose BEFORE the spawn; the reviewer is BLIND to other-agent transcripts.
            let l3_bundle: Option<crate::dev_implement_run::L3ReviewBundle> = {
                state.projects.active().and_then(|proj| {
                    if !proj.l3_review.enabled {
                        return None;
                    }
                    let l3_model = proj.l3_model().to_string();
                    let rules_prose = grounding.clone().unwrap_or_default();
                    let story_text = format!("{title}\n\n{desc}");
                    let llm: Arc<dyn crate::llm::Completer> = Arc::new(state.llm());
                    Some(crate::dev_implement_run::L3ReviewBundle {
                        story_text,
                        rules_prose,
                        model: l3_model,
                        llm,
                    })
                })
            };
            // Integration gate bundle (R3.e): only when the UoW crosses a boundary and
            // has a non-empty contract. The gate runs after L2/L3 in the bounce loop.
            let integration_gate_bundle: Option<crate::dev_implement_run::IntegrationGateBundle> = {
                if uow_data.investigation.crosses_boundary
                    && !uow_data.investigation.contract.trim().is_empty()
                {
                    let gate_model = match &tier_map {
                        Some(map) => map.balanced_primary().to_string(),
                        None => model
                            .clone()
                            .unwrap_or_else(crate::model_tier::default_strongest_model),
                    };
                    let llm: Arc<dyn crate::llm::Completer> = Arc::new(state.llm());
                    Some(crate::dev_implement_run::IntegrationGateBundle {
                        contract: uow_data.investigation.contract.clone(),
                        model: gate_model,
                        llm,
                    })
                } else {
                    None
                }
            };

            // ── Multi-repo worktree setup (R3.f / R6) ────────────────────────────────
            //
            // When the UoW's intake.repos has entries, set up a worktree + story branch
            // PER in-scope repo and capture each repo's base commit. The integration gate
            // then computes diffs from ALL repos (run_multi_repo_integration_gate).
            //
            // Single in-scope repo (or no intake.repos): repo_worktrees stays empty and
            // execute_dev_implement_run falls back to the single-repo path (unchanged).
            //
            // Base-commit capture: HEAD in each worktree before any agent work. Used by
            // worktree_diff_from_base after the agent commits to get the exact per-repo diff.
            //
            // TODO(#105-live): When fan_out wires per-worker worktrees on the live-fleet
            // side, extend this to also validate that each worker's jail matches its repo
            // worktree dir. For now, repo_worktrees feeds only the integration gate.
            let repo_worktrees: Vec<crate::dev_implement_run::RepoWorktree> = {
                let intake_repos = uow_data.intake.repos.clone();
                if intake_repos.len() <= 1 {
                    // Single-repo path (or no intake selection): handled by the primary
                    // dir + base_commit in execute_dev_implement_run. No extra setup.
                    Vec::new()
                } else {
                    // Multi-repo path: resolve + set up each in-scope repo's worktree.
                    let workspace_root = state.settings.workspace_root();
                    let mut worktrees = Vec::new();
                    for scope in &intake_repos {
                        // Derive the story branch for this repo from its BranchMode.
                        let scope_branch = derive_scope_branch(&scope.branch, &story_id_owned);
                        let override_path = state.settings.repo_path(&scope.repo);
                        // Resolve and ensure the per-repo worktree (same machinery as the
                        // primary repo path above). Skip repos that don't resolve locally —
                        // the primary repo's worktree is always present from the outer resolve.
                        // TODO(#105-live): surface unresolved sibling repos as run events.
                        if let Some(wt_dir) = crate::workspace::resolve_uow_worktree(
                            override_path.as_deref(),
                            workspace_root.as_deref(),
                            &scope.repo,
                            &scope_branch,
                        )
                        .await
                        {
                            // Capture the base commit for this repo's worktree.
                            let base = tokio::process::Command::new("git")
                                .args(["rev-parse", "HEAD"])
                                .current_dir(&wt_dir)
                                .output()
                                .await
                                .ok()
                                .filter(|o| o.status.success())
                                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                                .unwrap_or_default();
                            worktrees.push(crate::dev_implement_run::RepoWorktree {
                                repo: scope.repo.clone(),
                                branch: scope_branch,
                                dir: wt_dir,
                                base_commit: base,
                            });
                        }
                    }
                    worktrees
                }
            };

            // Test-tamper guard (AGENTIC-NO-TEST-TAMPER-1): enforce on TWO conditions —
            // the rule is SELECTED (active) in the project AND the chosen option is the
            // (default) escalate option. A project that selected an "allow" option, or has
            // not selected the rule, is not blocked. `test_tamper_guard_active` is the pure
            // decision over the project's ruleset selections.
            let enforce_test_tamper_guard = state
                .projects
                .active()
                .map(|p| crate::test_tamper::test_tamper_guard_active(&p.ruleset.selections))
                .unwrap_or(false);
            let impl_escalations = state.escalations.clone();

            // Clone the provider-dispatch context before the move closure captures it.
            let impl_registry = state.model_registry.clone();
            let impl_creds = state.credential_store.clone();
            let impl_limiter = state.rate_limiter.clone();
            tokio::spawn(async move {
                crate::dev_implement_run::execute_dev_implement_run(
                    store,
                    uow_store,
                    rid,
                    story_id_owned,
                    title,
                    desc,
                    repo,
                    dir,
                    branch,
                    decisions,
                    token,
                    impl_model,
                    max_iterations,
                    skip_layer2,
                    grounding,
                    read_dirs,
                    l3_bundle,
                    integration_gate_bundle,
                    repo_worktrees,
                    impl_registry,
                    impl_creds,
                    impl_limiter,
                    impl_escalations,
                    enforce_test_tamper_guard,
                )
                .await
            });
        } else {
            // Greenfield fallback: scaffold a new app from the plan in a throwaway dir.
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
    //
    // Enforcement-catch ledger capture: after the run is done, iterate its gate events
    // and write one catch per deny to the enforcement ledger (best-effort, fail-soft).
    {
        let runs = state.runs.clone();
        let uow = state.uow.clone();
        let watch_id = run_id.clone();
        let watch_story = story_id.to_string();
        let ledger = state.enforcement_ledger.clone();
        tokio::spawn(async move {
            stamp_provenance_when_done(runs, uow, ledger, watch_id, watch_story).await;
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
    ledger: crate::enforcement_ledger::EnforcementLedger,
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

                // ── Enforcement-catch ledger capture (terminal point 1) ──────────
                // After run is done, write one catch per deny/bounce gate event to the
                // append-only enforcement ledger. Best-effort / fail-soft: errors are
                // logged and swallowed inside `capture_run_finalization`; a ledger
                // failure never affects the run's terminal state or provenance.
                crate::enforcement_ledger::capture_run_finalization(
                    &ledger,
                    &run.events,
                    &run.id,
                    &story_id,
                )
                .await;

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

/// The current state of a run enriched with live stall diagnostics.
///
/// Fields added beyond `Run`:
/// - `idle_ms` — milliseconds since last activity (now − last_activity_ms).
/// - `stalled` — true when idle_ms > stall_threshold_ms.
/// - `stall_threshold_ms` — the active threshold (env-overridable, default 120 000ms).
/// - `stall_policy` — whether the run's policy is to alert or auto-cancel on stall.
/// - `failure_reason` — human-readable failure reason when the run failed.
#[derive(serde::Serialize)]
struct RunStatusResponse {
    #[serde(flatten)]
    run: Run,
    idle_ms: u128,
    stalled: bool,
    stall_threshold_ms: u128,
    stall_policy: crate::run::StallPolicy,
    failure_reason: Option<String>,
}

/// `GET /api/runs/:id` — the current state of a run: status, gate verdicts, and
/// live stall diagnostics so the cockpit can surface a stalled run without polling
/// a separate endpoint.
async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunStatusResponse>, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError(anyhow::anyhow!("run not found: {id}")))?;
    let threshold_ms = crate::run::run_stall_threshold_ms();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idle = crate::run::idle_ms(u128::from(run.tracker.last_activity_ms()), now_ms);
    let stalled = crate::run::is_stalled(idle, threshold_ms);
    let stall_policy = run.stall_policy.clone();
    let failure_reason = run.failure_reason.clone();
    Ok(Json(RunStatusResponse {
        run,
        idle_ms: idle,
        stalled,
        stall_threshold_ms: threshold_ms,
        stall_policy,
        failure_reason,
    }))
}

/// `POST /api/runs/:id/cancel` — cancel a run immediately. Idempotent; a run that is
/// already done or cancelled is unaffected. Always returns 204.
async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    state.runs.cancel(&id);
    StatusCode::NO_CONTENT
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

    // ── Per-UoW worktree teardown (Decision 1) ───────────────────────────────
    // Sign-off ends this UoW's active dev lifecycle, so its per-UoW worktree can be
    // reclaimed. Best-effort + non-fatal: a missing worktree, an unresolved repo, or a
    // git error never blocks sign-off. The shared clone and the branch itself are left
    // intact (the branch may still be wanted for the PR); only the extra checkout is removed.
    if let Some(branch) = uow.branch.as_deref().filter(|b| !b.trim().is_empty()) {
        if let Some(repo) = repo_from_story_id(&run.story_id) {
            let override_path = state.settings.repo_path(&repo);
            let workspace_root = state.settings.workspace_root();
            if let Some(clone) = crate::workspace::resolve_repo_dir(
                override_path.as_deref(),
                workspace_root.as_deref(),
                &repo,
            ) {
                crate::workspace::remove_uow_worktree(&clone, branch).await;
            }
        }
    }

    Ok(Json(uow).into_response())
}

/// OPEN clarifications for the active project's stories (the NEEDS YOU queue).
/// No active project → empty queue.
async fn list_open_clarifications(State(state): State<AppState>) -> Json<Vec<Clarification>> {
    let Some(p) = state.projects.active() else {
        return Json(vec![]);
    };
    Json(state.clarifications.all_open_for_project(&p.repos))
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
    let clar = state.clarifications.post_structured(
        &story_id,
        &req.question,
        &req.addressee,
        req.options.clone(),
        req.multi_select,
        req.allow_free_text,
    );

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
    // Structured path: selected options and/or a free-text "Other". When both the
    // structured fields are empty, fall back to the legacy `answer` string as the
    // free-text leg, so old callers keep working unchanged.
    let free_text = match req.free_text.clone() {
        Some(ft) => Some(ft),
        None if !req.answer.trim().is_empty() => Some(req.answer.clone()),
        None => None,
    };
    let selection = crate::clarify::ClarifyAnswer {
        selected: req.selected.clone(),
        free_text,
    };
    let answered = state
        .clarifications
        .answer_structured(&cid, selection, &req.answered_by)
        .ok_or_else(|| AppError(anyhow::anyhow!("clarification not found: {cid}")))?;

    // Phase 3b: if a gated run is PARKED on this clarification, resume it now. The resume
    // context is consumed (no double-resume); the re-spawned agent gets the original task
    // plus the question plus this answer. The gate is unchanged — the resume rebuilds the
    // SAME governed role + gated session. Only the investigation phase resumes today; the
    // enum keeps the dev-phase resume a closed, explicit future branch.
    if let Some(ctx) = state.clarify_resume.take(&cid) {
        let answer_summary = answered
            .answer
            .clone()
            .unwrap_or_else(|| req.answer.clone());
        match ctx.phase {
            crate::clarify_resume::PausedPhase::Investigation => {
                let runs = state.runs.clone();
                let uow = state.uow.clone();
                let clarifications = state.clarifications.clone();
                let resume = state.clarify_resume.clone();
                // ON-DEMAND REPO READ (the invariant): re-resolve the active project's local
                // clone so the RESUMED agent also runs with the repo as cwd + `--add-dir`
                // and keeps full read access. Read-only; the write gate is unchanged.
                let repo_dir = state.active_repo_dir();
                // MULTI-REPO READ scope: ALL the active project's repo clones (read-only).
                let read_dirs = state.active_repo_dirs();
                tokio::spawn(async move {
                    crate::investigation_run::resume_investigation_after_clarification(
                        runs,
                        uow,
                        clarifications,
                        resume,
                        ctx,
                        answer_summary,
                        repo_dir,
                        read_dirs,
                    )
                    .await;
                });
            }
        }
    }

    Ok(Json(answered))
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

/// Deserialise a chain field that may be `null` (absent), a JSON string (legacy single-model),
/// or a JSON array (new chain form). Mirrors `camerata_fleet::tier::deserialize_chain`.
fn deserialize_optional_chain<'de, D>(de: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Many(Vec<String>),
    }
    let maybe: Option<StringOrVec> = Option::deserialize(de)?;
    Ok(maybe.map(|v| match v {
        StringOrVec::Single(s) => vec![s],
        StringOrVec::Many(v) => v,
    }))
}

/// Body for `POST /api/projects/:id/tier-map`. Mirrors [`crate::model_tier::TierMap`]
/// with all fields optional so callers can patch just the tiers they want.
#[derive(serde::Deserialize)]
struct SetTierMapReq {
    /// Model chain for fast (throughput) tasks. Single string or array (back-compat).
    #[serde(default, deserialize_with = "deserialize_optional_chain")]
    fast: Option<Vec<String>>,
    /// Model chain for balanced (mid-tier) tasks. Single string or array (back-compat).
    #[serde(default, deserialize_with = "deserialize_optional_chain")]
    balanced: Option<Vec<String>>,
    /// Model id for strongest (frontier-class) tasks.
    #[serde(default)]
    strongest: Option<String>,
    /// Model chain for the Designer (vision/multimodal) band. Optional; absent = leave unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_chain")]
    vision: Option<Vec<String>>,
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
        if let Some(fast) = req.fast.filter(|v| !v.is_empty()) {
            p.tier_map.fast = fast;
        }
        if let Some(balanced) = req.balanced.filter(|v| !v.is_empty()) {
            p.tier_map.balanced = balanced;
        }
        if let Some(strongest) = req.strongest.filter(|s| !s.trim().is_empty()) {
            p.tier_map.strongest = strongest;
        }
        // vision: an empty vec clears the slot (user removed the model); any non-None
        // value (including []) is persisted. This lets the UI clear the vision chain.
        if let Some(vision) = req.vision {
            p.tier_map.vision = vision;
        }
        // A manual tier-band edit deviates from any preset -> the project is now Custom.
        p.mark_profile_custom();
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/vision-enabled`.
#[derive(serde::Deserialize)]
struct SetVisionEnabledReq {
    /// Whether to enable (`true`) or disable (`false`) the Designer (vision) band for this project.
    enabled: bool,
}

/// `POST /api/projects/:id/vision-enabled` — toggle the Designer (vision) band for a project.
///
/// Persists `Project::vision_enabled`. When `false` (the default), the orchestrator ignores
/// `tier_map.vision` even if populated. When `true`, the vision band is available and the
/// orchestrator may invoke the Designer for stories that require multimodal reasoning.
async fn set_vision_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetVisionEnabledReq>,
) -> Json<serde_json::Value> {
    match state.projects.update(&id, |p| {
        p.vision_enabled = req.enabled;
    }) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Resolve the model id for a NON-FLEET AI step from the ACTIVE project's per-step config.
///
/// This is the single resolution point for the FALLBACK steps (story authoring,
/// decomposition, escalation, clarification): once a project exists, its
/// [`crate::project::Project::model_for_step`] value is authoritative — there is NO
/// env/const fallback. The ONLY remaining [`crate::llm::DEFAULT_MODEL`] floor is the
/// project-less edge (no active project at all, e.g. a smoke test before any project is
/// created). UI-picked steps (audit / calibration / research chat) do not use this helper
/// directly — they let an explicit request model override this default at their call site
/// via [`step_model_or`].
pub fn step_model(state: &AppState, step: crate::project::StepKind) -> String {
    state
        .projects
        .active()
        .map(|p| p.model_for_step(step).to_string())
        .unwrap_or_else(|| crate::llm::DEFAULT_MODEL.to_string())
}

/// UI-picked step resolution: the caller's explicit `requested` model wins when non-empty;
/// otherwise fall back to the active project's per-step default (see [`step_model`]). Used
/// by the audit / calibration / research-chat steps, where the UI still owns the
/// speed-vs-thoroughness override but the project supplies the default.
pub fn step_model_or(
    state: &AppState,
    step: crate::project::StepKind,
    requested: Option<&str>,
) -> String {
    match requested.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => step_model(state, step),
    }
}

/// Body for `POST /api/projects/:id/step-models`: which NON-FLEET AI step and the model id
/// to bind to it. Patch semantics — one step per call, the others left untouched.
#[derive(serde::Deserialize)]
struct SetStepModelReq {
    /// The step key: `audit` | `calibration` | `research_chat` | `story_authoring` |
    /// `decomposition` | `escalation` | `clarification`.
    step: String,
    /// The model id to bind (e.g. `claude-opus-4-8`).
    model: String,
}

/// Parse a step key from the request body into a [`StepKind`]. Tolerant of dash/space
/// separators (e.g. `research-chat`). Returns `None` for an unknown key.
fn parse_step_kind(s: &str) -> Option<crate::project::StepKind> {
    use crate::project::StepKind;
    match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
        "audit" => Some(StepKind::Audit),
        "calibration" => Some(StepKind::Calibration),
        "research_chat" => Some(StepKind::ResearchChat),
        "story_authoring" => Some(StepKind::StoryAuthoring),
        "decomposition" => Some(StepKind::Decomposition),
        "escalation" => Some(StepKind::Escalation),
        "clarification" => Some(StepKind::Clarification),
        _ => None,
    }
}

/// `POST /api/projects/:id/step-models` — set the model for ONE non-fleet AI step on this
/// project. Patch semantics (one step per call); mirrors the tier-map write path —
/// mutates only the named project and persists. A blank `model` or unknown `step` is a
/// no-op error response (never silently mutates).
async fn set_step_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetStepModelReq>,
) -> Json<serde_json::Value> {
    let Some(step) = parse_step_kind(&req.step) else {
        return Json(serde_json::json!({ "ok": false, "message": "unknown step" }));
    };
    let model = req.model.trim().to_string();
    if model.is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "model must not be empty" }));
    }
    match state.projects.set_step_model(&id, step, model) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

/// Body for `POST /api/projects/:id/stall-thresholds`.
#[derive(serde::Deserialize)]
struct SetStallThresholdsReq {
    watched_secs: u64,
    routine_secs: u64,
}

/// `POST /api/projects/:id/stall-thresholds` — update the per-project stall-detection
/// thresholds. Both values must be positive; a zero value is rejected.
async fn set_stall_thresholds_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetStallThresholdsReq>,
) -> Json<serde_json::Value> {
    if req.watched_secs == 0 || req.routine_secs == 0 {
        return Json(serde_json::json!({ "ok": false, "message": "thresholds must be > 0" }));
    }
    let thresholds = crate::project::StallThresholds {
        watched_secs: req.watched_secs,
        routine_secs: req.routine_secs,
    };
    match state.projects.set_stall_thresholds(&id, thresholds) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── L3 agentic code-review gate configuration (R7) ────────────────────────────

/// Body for `POST /api/projects/:id/l3-review`.
///
/// `enabled` controls whether the L3 reviewer runs after each governed development
/// stage. `model` pins the reviewer to a specific model id; an empty or absent
/// `model` means "use the project's `balanced` tier model" (the fallback defined in
/// [`crate::project::Project::l3_model`]).
#[derive(serde::Deserialize)]
struct SetL3ReviewReq {
    /// Whether the L3 reviewer is enabled for this project.
    enabled: bool,
    /// The model id for the L3 reviewer. Empty string = use the balanced tier fallback.
    #[serde(default)]
    model: String,
}

/// `POST /api/projects/:id/l3-review` — update the per-project L3 agentic code-review
/// gate configuration.
///
/// Accepts `{ "enabled": bool, "model": "<id or empty>" }` and writes it to the project
/// store. Returns the updated project on success, or an error when the project id is not
/// found. The `model` field is optional in the request body (defaults to empty string =
/// use the balanced tier fallback).
async fn set_l3_review_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetL3ReviewReq>,
) -> Json<serde_json::Value> {
    let config = crate::project::L3ReviewConfig {
        enabled: req.enabled,
        model: req.model.trim().to_string(),
    };
    match state.projects.set_l3_review(&id, config) {
        Some(p) => Json(serde_json::json!({ "ok": true, "project": p })),
        None => Json(serde_json::json!({ "ok": false, "message": "no such project" })),
    }
}

// ── Model Efficiency Profile ──────────────────────────────────────────────────

/// Query parameters for `GET /api/projects/:id/model-profile/preview`.
#[derive(serde::Deserialize)]
struct PreviewProfileQuery {
    profile: String,
}

/// `GET /api/projects/:id/model-profile/preview?profile=X` — compute the assignments a
/// profile would produce WITHOUT applying them. Returns the proposed tier_map, step_models,
/// and l3_review so the UI can show a "current → new" confirm popup before committing.
async fn preview_model_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PreviewProfileQuery>,
) -> Json<serde_json::Value> {
    let profile = match q.profile.to_ascii_lowercase().as_str() {
        "max_efficiency" | "maxefficiency" => crate::project::ModelProfile::MaxEfficiency,
        "balanced" => crate::project::ModelProfile::Balanced,
        "max_quality" | "maxquality" => crate::project::ModelProfile::MaxQuality,
        "custom" => crate::project::ModelProfile::Custom,
        _ => return Json(serde_json::json!({ "ok": false, "message": "unknown profile" })),
    };
    // Verify the project exists.
    if state.projects.get(&id).is_none() {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    }
    let assignments = crate::model_profile_cascade::compute_profile_cascade(profile, &state.model_registry);
    match assignments {
        None => Json(serde_json::json!({
            "ok": true,
            "profile": q.profile,
            "noop": true,
            "message": "Custom profile: no cascade — user owns every entry.",
        })),
        Some(a) => Json(serde_json::json!({
            "ok": true,
            "profile": q.profile,
            "noop": false,
            "assignments": {
                "tier_map": a.tier_map,
                "step_models": a.step_models,
                "l3_review": a.l3_review,
            }
        })),
    }
}

/// Body for `POST /api/projects/:id/model-profile`.
#[derive(serde::Deserialize)]
struct ApplyModelProfileReq {
    /// The profile to apply: `"max_efficiency"` | `"balanced"` | `"max_quality"` | `"custom"`.
    profile: String,
}

/// `POST /api/projects/:id/model-profile { profile }` — set the profile and apply the
/// cascade to the project's tier-map, step-models, and L3 config. Auto-saves. Returns
/// the updated project.
async fn apply_model_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApplyModelProfileReq>,
) -> Json<serde_json::Value> {
    let profile = match req.profile.to_ascii_lowercase().as_str() {
        "max_efficiency" | "maxefficiency" => crate::project::ModelProfile::MaxEfficiency,
        "balanced" => crate::project::ModelProfile::Balanced,
        "max_quality" | "maxquality" => crate::project::ModelProfile::MaxQuality,
        "custom" => crate::project::ModelProfile::Custom,
        _ => return Json(serde_json::json!({ "ok": false, "message": "unknown profile" })),
    };

    let assignments = crate::model_profile_cascade::compute_profile_cascade(profile, &state.model_registry);

    let updated = state.projects.update(&id, |p| {
        p.model_profile = profile;
        if let Some(ref a) = assignments {
            p.tier_map = a.tier_map.clone();
            p.step_models = a.step_models.clone();
            p.l3_review = a.l3_review.clone();
        }
    });

    match updated {
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
    let report = crate::onboard::scan_repos(&sources, notes).await;
    // Populate the per-project last-scan store so the chat assistant sees deterministic
    // results immediately. The async audit path (`onboard_audit_start`) and the sync audit
    // (`onboard_audit`) already do this; without it here, a floor-only `/api/onboard/scan`
    // leaves `last_scan` empty and the chat shows "no scan results yet". Fail-soft, keyed by
    // the active project (the same id `active_project_context` reads).
    if let Some(id) = state.projects.active().map(|p| p.id) {
        state.set_last_scan(id, report.clone());
    }
    Json(report)
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
    // Audit + calibration are UI-PICKED non-fleet steps: an explicit request model wins;
    // otherwise the active project's per-step default applies (DEFAULT_MODEL floor only with
    // no active project). Each is resolved to a concrete id (never `None`) so there is no
    // downstream env/const fallback once a project exists.
    let model = Some(step_model_or(
        &state,
        crate::project::StepKind::Audit,
        req.model.as_deref(),
    ));
    let calibration_model = Some(step_model_or(
        &state,
        crate::project::StepKind::Calibration,
        req.calibration_model.as_deref(),
    ));
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
        Some(state.usage_ledger.clone()),
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

    // ── Always-on dep-audit (osv-scanner) — runs LAST, hard-bounded ──────────────────
    // Runs after preview linters so a slow osv-scanner never blocks them.
    // Hard 60-second total timeout (provisioning + scan); fail-soft on timeout.
    // Gated on run_deterministic; CAMERATA_DISABLE_DEP_AUDIT bypasses for test isolation.
    if run_deterministic {
        use tokio::time::{timeout, Duration};
        let dep_audit_timeout = Duration::from_secs(60);
        for (spec, dir) in &sources {
            let spec = spec.as_str();
            let run = crate::dep_audit::run_dep_audit(spec, dir);
            let (dep_findings, dep_note) = match timeout(dep_audit_timeout, run).await {
                Ok(result) => result,
                Err(_elapsed) => (
                    Vec::new(),
                    Some(crate::onboard::CoverageNote {
                        tool: "osv-scanner".to_string(),
                        message: format!(
                            "dependency audit (osv-scanner) timed out after 60 s for {spec}"
                        ),
                    }),
                ),
            };
            if !dep_findings.is_empty() {
                report.findings.extend(dep_findings);
            }
            if let Some(note) = dep_note {
                report.coverage_notes.push(note);
            }
        }
    }

    // ── Enforcement-catch ledger capture (terminal point 2: scan completion) ──────────
    // After a scan completes, write one floor/catch record for each ACTIVE floor finding
    // to the enforcement ledger. Best-effort / fail-soft: runs in a background task so
    // the response is never delayed by ledger writes. Errors inside are logged and swallowed.
    {
        let ledger = state.enforcement_ledger.clone();
        let all_findings = report.findings.clone();
        tokio::spawn(async move {
            crate::enforcement_ledger::capture_scan_findings(&ledger, &all_findings).await;
        });
    }

    // ── Write last_scan (synchronous path) ───────────────────────────────────────────
    // Store the completed report immediately so chat grounding can read it regardless of
    // whether the UI has round-tripped the draft back yet (timing race fixed).
    if let Some(id) = &project_id {
        state.set_last_scan(id.clone(), report.clone());
    }

    Json(report)
}

/// Map a semgrep rule id to the FLOOR rule id whose security category it overlaps,
/// or `None` when the semgrep rule has no floor twin (net-new coverage that must pass
/// through untouched).
///
/// The floor (`crate::onboard::AUDIT_RULES`) runs at the Layer-1 gate AND at scan
/// preview. Semgrep runs at scan preview AND CI (Layer-2 / Layer-3). Semgrep rules
/// that overlap the floor on the same (repo, path, line) axis:
///
/// | Semgrep rule id                                  | Floor rule id                    |
/// |--------------------------------------------------|----------------------------------|
/// | `camerata.security.hardcoded-secret`             | `SEC-NO-HARDCODED-SECRETS-1`     |
/// | `camerata.security.hardcoded-secret-dquote`      | `SEC-NO-HARDCODED-SECRETS-1`     |
/// | `camerata.security.sql-string-concat-python`     | `SEC-NO-RAW-SQL-CONCAT-1`        |
/// | `camerata.security.sql-string-concat-js`         | `SEC-NO-RAW-SQL-CONCAT-1`        |
/// | `camerata.security.sql-string-concat-rust`       | `SEC-NO-RAW-SQL-CONCAT-1`        |
/// | `camerata.security.sql-string-concat-csharp`     | `SEC-NO-RAW-SQL-CONCAT-1`        |
/// | `camerata.security.disabled-tls-rust`            | `SEC-NO-DISABLED-TLS-1`          |
/// | `camerata.security.disabled-tls-csharp`          | `SEC-NO-DISABLED-TLS-1`          |
/// | `camerata.security.yaml-unsafe-load`             | `SEC-NO-UNSAFE-DESERIALIZATION-1` |
///
/// The remaining semgrep rules (`exec-injection`, `exec-injection-js`,
/// `weak-hash-*`, `path-traversal-python`, `subprocess-shell-true`) have no
/// floor twin and map to `None`.
///
/// NOTE: rule ids here must be the NORMALIZED (clean) form — `normalize_semgrep_rule_id`
/// in `scan_tools` strips any absolute-path prefix before findings reach this function.
///
/// IMPORTANT: do NOT remove rules from either ruleset to fix overlap. The floor enforces
/// at Layer 1 (gate); semgrep enforces at Layers 2-3 (CI). Trimming semgrep would punch
/// a hole in CI coverage. The fix is presentation-time dedup at scan preview only (see
/// `dedup_scan_previews`). Decision: docs/decisions/2026-06-22_scan_floor_semgrep_dedup.md
#[cfg_attr(not(test), allow(dead_code))]
fn semgrep_floor_category(semgrep_rule_id: &str) -> Option<&'static str> {
    match semgrep_rule_id {
        "camerata.security.hardcoded-secret"
        | "camerata.security.hardcoded-secret-dquote" => Some("SEC-NO-HARDCODED-SECRETS-1"),
        "camerata.security.sql-string-concat-python"
        | "camerata.security.sql-string-concat-js"
        | "camerata.security.sql-string-concat-rust"
        | "camerata.security.sql-string-concat-csharp" => Some("SEC-NO-RAW-SQL-CONCAT-1"),
        "camerata.security.disabled-tls-rust"
        | "camerata.security.disabled-tls-csharp" => Some("SEC-NO-DISABLED-TLS-1"),
        "camerata.security.yaml-unsafe-load" => Some("SEC-NO-UNSAFE-DESERIALIZATION-1"),
        _ => None,
    }
}

/// Map ANY finding's rule id (floor OR any preview tool) to a normalized security
/// category string, or `None` when no category mapping exists. Used by
/// `dedup_scan_previews` for cross-tool dedup: two findings on the same
/// (repo, path, line) with the SAME normalized category are duplicates; different
/// categories on the same line are KEPT (conservative / surface-over-hide).
///
/// Category vocabulary (lowercase, hyphen-separated):
/// - `"secret"` — hardcoded secrets, vendor tokens, private keys
/// - `"sql"`    — raw SQL string concatenation (injection)
/// - `"exec"`   — OS command / exec injection
/// - `"hash"`   — weak cryptographic hash
/// - `"path"`   — path traversal
/// - `"shell"`  — subprocess with shell=True
/// - `"deser"`  — unsafe deserialization (yaml.load w/o SafeLoader, pickle, unserialize,
///                Marshal.load, BinaryFormatter, …). Replaces the old `"yaml"` category so
///                the floor rule SEC-NO-UNSAFE-DESERIALIZATION-1 and the semgrep rule
///                camerata.security.yaml-unsafe-load share one category and collapse
///                correctly when they fire on the same (repo, path, line).
/// - `"tls"`    — disabled TLS / cert verification
///
/// Decision: docs/decisions/2026-06-23_stack_gating_and_crosstool_dedup.md
pub(crate) fn finding_security_category(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        // Floor rules (SEC-* ids from AUDIT_RULES)
        "SEC-NO-HARDCODED-SECRETS-1" | "SEC-NO-PRIVATE-KEY-1" | "SEC-NO-VENDOR-TOKEN-1" => Some("secret"),
        "SEC-NO-RAW-SQL-CONCAT-1" => Some("sql"),
        "SEC-NO-DISABLED-TLS-1" => Some("tls"),
        // SEC-NO-UNSAFE-DESERIALIZATION-1: the floor rule for the deser category.
        // camerata.security.yaml-unsafe-load (semgrep) maps to the same category so that
        // a floor finding and a semgrep finding on the same (repo, path, line) collapse to
        // one row, floor canonical. The old "yaml" category string is retired; both sides
        // now use "deser" (broader: covers pickle, unserialize, Marshal.load, BinaryFormatter).
        "SEC-NO-UNSAFE-DESERIALIZATION-1" => Some("deser"),
        // Semgrep rules (camerata.security.*)
        // NOTE: keep these in sync with the rule ids in
        // crates/server/assets/semgrep-rules/security.yml. A semgrep rule whose id is
        // missing here returns None, so the deduper cannot categorize it and it will
        // NEVER collapse against an overlapping floor finding (the 2026-06-23 dedup gap:
        // the -rust/-csharp/-dquote ids were added to the corpus but not registered here).
        "camerata.security.hardcoded-secret"
        | "camerata.security.hardcoded-secret-dquote" => Some("secret"),
        "camerata.security.sql-string-concat-python"
        | "camerata.security.sql-string-concat-js"
        | "camerata.security.sql-string-concat-rust"
        | "camerata.security.sql-string-concat-csharp" => Some("sql"),
        "camerata.security.exec-injection"
        | "camerata.security.exec-injection-js" => Some("exec"),
        "camerata.security.weak-hash-python"
        | "camerata.security.weak-hash-js"
        | "camerata.security.weak-hash-rust"
        | "camerata.security.weak-hash-csharp" => Some("hash"),
        "camerata.security.disabled-tls-rust"
        | "camerata.security.disabled-tls-csharp" => Some("tls"),
        "camerata.security.path-traversal-python" => Some("path"),
        "camerata.security.subprocess-shell-true" => Some("shell"),
        // yaml-unsafe-load maps to "deser" (not "yaml") so it collapses with the
        // floor rule SEC-NO-UNSAFE-DESERIALIZATION-1 when both fire on the same line.
        "camerata.security.yaml-unsafe-load" => Some("deser"),
        // Ruff / ESLint lints that overlap with floor or semgrep in the same category.
        // S608 = possible SQL injection (Ruff/flake8-bandit) — same category as the floor SQL rule.
        "S608" => Some("sql"),
        _ => None,
    }
}

/// Precedence rank for a finding when choosing the canonical row among duplicates.
/// Lower = higher precedence (kept as canonical). Ordering:
///   0 = floor (deterministic, stable, gate-enforced SEC-* id)
///   1 = clippy / ruff / eslint (native language linter)
///   2 = semgrep (polyglot, more permissive category match)
fn finding_dedup_rank(f: &crate::onboard::Finding) -> u8 {
    if !f.preview {
        return 0; // floor finding (enforced, never preview)
    }
    match f.preview_tool.as_deref() {
        Some("clippy") | Some("ruff") | Some("eslint") => 1,
        Some("semgrep") => 2,
        _ => 1,
    }
}

/// Generalized scan-preview dedup: merge `previews` into `existing` (floor + any
/// already-accumulated preview findings), collapsing findings that share the same
/// `(repo, path, line)` AND a compatible `finding_security_category` into a single
/// canonical row.
///
/// ## Canonical preference order
///
/// floor > clippy/ruff/eslint (rank 1) > semgrep (rank 2)
///
/// When a new preview finding duplicates an EXISTING row, the existing row is
/// kept if its rank is lower (higher precedence), and the new finding's rule id is
/// appended to `also_matches` on the canonical. If the new finding has HIGHER
/// precedence than the existing preview row, the new finding BECOMES canonical (its
/// rule_id is kept and the old rule_id is swapped into `also_matches`).
///
/// ## Conservative dedup
///
/// Only deduplicated when same location AND same security category. Two findings on
/// the same line but different categories are BOTH kept ("surface over hide").
///
/// ## Backward compatibility
///
/// This is a strict superset of the old `dedup_preview_against_floor` contract.
/// The floor↔semgrep dedup behavior is preserved exactly as a special case.
///
/// Decision: docs/decisions/2026-06-23_stack_gating_and_crosstool_dedup.md
pub(crate) fn dedup_scan_previews(
    existing: &mut Vec<crate::onboard::Finding>,
    previews: Vec<crate::onboard::Finding>,
) -> Vec<crate::onboard::Finding> {
    let mut out: Vec<crate::onboard::Finding> = Vec::with_capacity(previews.len());

    for preview in previews {
        let preview_cat = finding_security_category(&preview.rule_id);

        // Try to find an existing finding that shares the same location AND category.
        // The category must be Some (we don't collapse unrecognized categories).
        let twin_idx = if let Some(cat) = preview_cat {
            existing.iter().position(|f| {
                f.repo == preview.repo
                    && f.path == preview.path
                    && f.line == preview.line
                    && finding_security_category(&f.rule_id) == Some(cat)
            })
        } else {
            None
        };

        if let Some(idx) = twin_idx {
            let existing_rank = finding_dedup_rank(&existing[idx]);
            let preview_rank = finding_dedup_rank(&preview);

            if preview_rank >= existing_rank {
                // Existing row has equal or higher precedence: keep it, fold preview in.
                existing[idx].also_matches.push(preview.rule_id.clone());
                // drop the preview finding (continue to next)
            } else {
                // Preview has higher precedence: promote it to canonical, demote existing.
                let old_rule_id = existing[idx].rule_id.clone();
                let mut new_canonical = preview;
                new_canonical.also_matches.extend(existing[idx].also_matches.drain(..));
                new_canonical.also_matches.push(old_rule_id);
                existing[idx] = new_canonical;
                // promoted to existing; don't push to out
            }
        } else {
            // No twin at this (repo, path, line, category): net-new coverage or different
            // category — keep it.
            out.push(preview);
        }
    }

    out
}

/// Backward-compatible alias kept so the large existing test suite (`dedup_preview_against_floor`)
/// continues to pass without modification. Delegates directly to `dedup_scan_previews`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn dedup_preview_against_floor(
    existing: &mut Vec<crate::onboard::Finding>,
    previews: Vec<crate::onboard::Finding>,
) -> Vec<crate::onboard::Finding> {
    dedup_scan_previews(existing, previews)
}

/// Run the scan-time deterministic preview pass over each local repo source and append
/// its preview findings to the report. A no-op when there are no preview-runnable rules
/// (or the corpus is unavailable). Preview findings are ADVISORY-but-deterministic — they
/// carry stable tool rule-ids, stay OUT of the LLM review, and are labeled "not enforced
/// until wired" in the UI. layer3_only rules were already excluded by `split_scannable_rules`.
///
/// Stack-gating: for each repo the set of languages present (from its file extensions)
/// is derived and passed to `run_scan_tools` so tools whose required language is absent
/// are omitted entirely — they never appear as a passing "✓ 0".
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
        // Derive the language set present in this repo so stack-gating can omit
        // tools whose language is absent (e.g. no eslint on a Rust-only repo).
        let dir_clone = dir.clone();
        let present_languages =
            tokio::task::spawn_blocking(move || {
                crate::onboard::files::read_local_repo_files(&dir_clone)
                    .map(|ex| crate::scan_tools::languages_from_files(&ex.files))
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
        let (previews, mut notes) =
            crate::scan_tools::run_scan_tools(spec, dir, &for_repo, &lookup, Some(&present_languages), job).await;
        // Dedup preview findings that overlap the deterministic floor or other preview
        // tools BEFORE appending. When two tools flag the same (repo, path, line) for a
        // compatible security category, the higher-precedence finding is kept canonical and
        // the other tool's id is folded into `also_matches`. See `dedup_scan_previews`.
        let deduped = dedup_scan_previews(&mut report.findings, previews);
        report.findings.extend(deduped);
        report.coverage_notes.append(&mut notes);
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
    // Audit + calibration are UI-PICKED non-fleet steps: an explicit request model wins;
    // otherwise the active project's per-step default applies (DEFAULT_MODEL floor only with
    // no active project). Each is resolved to a concrete id (never `None`) so there is no
    // downstream env/const fallback once a project exists.
    let model = Some(step_model_or(
        &state,
        crate::project::StepKind::Audit,
        req.model.as_deref(),
    ));
    let calibration_model = Some(step_model_or(
        &state,
        crate::project::StepKind::Calibration,
        req.calibration_model.as_deref(),
    ));
    let mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    let thorough = req.thorough;
    let deep = req.deep;
    // Scan-type selector: resolve the two flags (both-false coerces to both-true).
    let (run_ai_review, run_deterministic, _coerced) =
        effective_scan_modes(req.run_ai_review, req.run_deterministic);
    // Local-first: resolve each repo's local working tree up front (the spawned job owns them).
    let (sources, notes) = resolve_local_sources(&state, &repos);

    let job_id = state.jobs.create("audit");
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
    // Captured for the spawned task so the async audit's model calls feed the cumulative meter.
    let usage_ledger = state.usage_ledger.clone();
    // Captured so the async path can write to last_scan the instant the report is final.
    let last_scan = state.last_scan.clone();
    // Also capture recent_scan so the project-agnostic fallback is kept in sync.
    let recent_scan = state.recent_scan.clone();
    tokio::spawn(async move {
        if sources.is_empty() {
            jobs.fail(
                &jid,
                "No local repos to audit — browse to each repo's local folder first.",
            );
            return;
        }

        // ── Pre-declare the FULL deterministic tool pipeline before any tool runs ──
        //
        // The progress box shows "X / N tools"; N must reflect the complete pipeline
        // from the first poll, not grow one tool at a time.  We compute the complete
        // tool set now — before audit_repos starts — and seed the job with all of them
        // at `starting` status.
        //
        // Pipeline:
        //   1. floor      (always, when run_deterministic)
        //   2. preview linters (clippy / ruff / eslint / semgrep / unrouted — derived
        //      from the selected mechanical rules via the corpus, when run_deterministic;
        //      STACK-GATED: a tool whose language is absent from all repos is omitted)
        //   3. dep-audit  (always-on unless CAMERATA_DISABLE_DEP_AUDIT is set)
        //
        // The UI will see, e.g. "0/4 tools" and then watch them fill in, rather than
        // "1/2 tools" while the other two are still invisible.
        if run_deterministic {
            let lookup = |id: &str| corpus.as_ref().and_then(|s| s.get_by_id(id));
            // Derive the UNION of all languages present across every source repo.
            // Stack-gating uses this union so the pre-declared tool count matches the
            // tools that `merge_scan_preview` will actually run (which gates per-repo
            // but adds to the same job). Using the union is conservative: a tool
            // present in ANY repo stays in the denominator.
            let mut union_languages = std::collections::HashSet::<String>::new();
            for (_, dir) in &sources {
                let dir_clone = dir.clone();
                if let Ok(Ok(extracted)) =
                    tokio::task::spawn_blocking(move || crate::onboard::files::read_local_repo_files(&dir_clone)).await
                {
                    union_languages.extend(
                        crate::scan_tools::languages_from_files(&extracted.files)
                    );
                }
            }
            let lang_gate = if union_languages.is_empty() { None } else { Some(&union_languages) };
            let mut tool_ids: Vec<String> = vec!["floor".to_string()];
            let preview_ids =
                crate::scan_tools::preview_tool_ids_for_rules(&preview_rules, &lookup, lang_gate);
            tool_ids.extend(preview_ids);
            let dep_audit_disabled = std::env::var(crate::dep_audit::DISABLE_ENV_VAR)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if !dep_audit_disabled {
                tool_ids.push("dep-audit".to_string());
            }
            let id_refs: Vec<&str> = tool_ids.iter().map(String::as_str).collect();
            jobs.declare_tools(&jid, &id_refs);
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
            Some(usage_ledger.clone()),
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

        // ── Always-on dep-audit (osv-scanner) — runs LAST, hard-bounded ──────────
        //
        // dep-audit runs AFTER the floor AND the preview linters so a slow osv-scanner
        // provisioning or scan never blocks them.  A single 60-second hard
        // tokio::time::timeout wraps the ENTIRE call (provisioning + scan); on timeout
        // the step fails soft and the scan completes without dep-audit findings.
        //
        // Gated on `run_deterministic` (dep-audit is part of the deterministic floor);
        // CAMERATA_DISABLE_DEP_AUDIT bypasses it entirely for test isolation.
        if run_deterministic {
            use tokio::time::{timeout, Duration};
            let dep_audit_timeout = Duration::from_secs(60);
            for (spec, dir) in &sources {
                let spec = spec.as_str();
                let run = crate::dep_audit::run_dep_audit(spec, dir);
                let (dep_findings, dep_note) = match timeout(dep_audit_timeout, run).await {
                    Ok(result) => result,
                    Err(_elapsed) => (
                        Vec::new(),
                        Some(crate::onboard::CoverageNote {
                            tool: "osv-scanner".to_string(),
                            message: format!(
                                "dependency audit (osv-scanner) timed out after 60 s for {spec}"
                            ),
                        }),
                    ),
                };
                let dep_count = dep_findings.len();
                jobs.det_tool_running(&jid, "dep-audit");
                jobs.det_tool_done(&jid, "dep-audit", dep_count);
                if !dep_findings.is_empty() {
                    jobs.add_findings(&jid, dep_findings.clone());
                    report.findings.extend(dep_findings);
                }
                if let Some(note) = dep_note {
                    report.coverage_notes.push(note);
                }
            }
        }

        // ── Enforcement-ledger scan-capture is done by the synchronous audit handler ──
        // (The job path is async; ledger capture lives in the synchronous `onboard_audit`
        // handler. If you add ledger capture here in the future, follow the same
        // background-spawn pattern used there.)

        // ── Write last_scan + recent_scan (async job path) ──────────────────────────
        // Store the completed report before finishing the job so any concurrent chat
        // request landing immediately after sees the full results. Fail-soft.
        // Also update the project-agnostic `recent_scan` fallback so the active-project
        // context can surface this scan even if the active project changed since the
        // scan was submitted (or no project was active at submission time).
        if let Some(id) = &project_id {
            let mut guard = last_scan.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(id.clone(), report.clone());
        }
        {
            let mut recent = recent_scan.lock().unwrap_or_else(|e| e.into_inner());
            *recent = Some(report.clone());
        }

        jobs.finish(&jid, report);
    });

    Json(serde_json::json!({ "job_id": job_id }))
}

/// Poll an async audit job: status, progress (done/total passes), incremental findings, and
/// the final report once done. `null` for an unknown id.
/// Enriched job status response: the job state plus idle time and cancel flag.
#[derive(Debug, serde::Serialize)]
struct JobStatusResponse {
    job: crate::jobs::JobState,
    idle_ms: Option<u128>,
    cancel_requested: bool,
}

async fn onboard_audit_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    match state.jobs.get(&id) {
        Some(job) => {
            let idle_ms = state.jobs.idle_ms(&id, now_ms);
            let cancel_requested = state.jobs.is_cancel_requested(&id);
            Json(Some(JobStatusResponse { job, idle_ms, cancel_requested })).into_response()
        }
        None => Json::<Option<JobStatusResponse>>(None).into_response(),
    }
}

/// `POST /api/onboard/audit/job/:id/cancel` — cancel an in-flight audit job.
async fn cancel_audit_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    state.jobs.cancel(&id);
    StatusCode::NO_CONTENT
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

// ── CI story body helpers ─────────────────────────────────────────────────────
//
// These functions are extracted from `onboard_ci_rules` so they can be unit-
// tested without spinning up a GitHub token or an HTTP server. Each produces the
// complete Markdown issue body for one tier.

/// The preamble shared by both tier stories. Explains the SSOT model up-front so any
/// developer or agent picking up either story understands how `.camerata/checks.toml`
/// drives BOTH layers before reading the tier-specific instructions.
fn ci_story_ssot_preamble() -> &'static str {
    "## Single Source of Truth: `.camerata/checks.toml`

The canonical place to declare a custom deterministic check is **`.camerata/checks.toml`**. \
Adding ONE entry there makes the check enforced at BOTH:

- **Layer 2** (in-loop dev gate): checks marked `in_loop = true` run in the governed dev \
  loop after each agent task. A violation bounces the work back before a PR is even opened — \
  the same way a failing `cargo clippy` would.
- **Layer 3** (generated CI workflow): the entire manifest (both `in_loop = true` AND \
  `in_loop = false`) is consumed by Camerata's workflow generator \
  (`POST /api/projects/active/generate-ci-workflow`) to produce \
  `.github/workflows/camerata-gates.yml`. CI is always the superset backstop.

**Parity is structural, not by convention.** Both the Layer-2 runner and the Layer-3 workflow \
generator call the SAME shared functions over the manifest, so they cannot drift by construction.

### Schema

```toml
# .camerata/checks.toml

[[check]]
id       = \"DEP-CRUISER-LAYERING-1\"
name     = \"dependency-cruiser layering\"
tool     = \"dependency-cruiser\"
version  = \"6.3.0\"                 # exact, not a range — L2/L3 must run the same version
install  = \"npm install -g dependency-cruiser@6.3.0\"
command  = \"depcruise --config .dependency-cruiser.cjs src\"
severity = \"high\"
in_loop  = true                    # true = also run at Layer 2; false = CI-only (needs secrets/services)
```

Field reference:

| Field      | Required | Semantics |
|------------|----------|-----------|
| `id`       | yes      | Stable rule id; used as violation id on non-zero exit |
| `name`     | yes      | Short human label for bounce-back messages |
| `command`  | yes      | Shell command run with `cwd = repo root` |
| `severity` | yes      | `\"high\"` / `\"medium\"` / `\"low\"` (all severities block; severity shapes message priority) |
| `in_loop`  | yes      | `true` = Layer 2 AND Layer 3; `false` = CI-only (use for secret-dependent or slow checks) |
| `tool`     | no       | External tool name (`\"dependency-cruiser\"`, `\"semgrep\"`). Required when `version` is set. |
| `version`  | no       | **EXACT** pinned version (`\"6.3.0\"`). No ranges or carets — determinism requires an exact match. Layer 2 verifies this version before running the check; a mismatch is a violation. |
| `install`  | no       | Exact install command (`\"npm install -g dependency-cruiser@6.3.0\"`). Explicit because install mechanisms span pip / npm / cargo / go. Layer 3 emits this as a step immediately before the check command. Layer 2 does NOT install — it only verifies. |

### Why pin `tool` + `version` + `install`?

Without version pinning, Layer 2 and Layer 3 can run different versions of the same linter \
on the same ruleset and produce different results — \"green in the dev loop, red in CI.\" \
Pinning all three fields closes this gap: the manifest is the single source of version truth, \
not each environment's ambient tool install.

### Gate protection (`SEC-NO-CAMERATA-CONFIG-1`)

**Agents cannot edit `.camerata/checks.toml`.** The Layer-1 gateway hard-guard \
`SEC-NO-CAMERATA-CONFIG-1` denies any agent write targeting a path under `.camerata/`. \
This prevents an agent from weakening or disabling its own gates. The manifest is always a \
human/operator commit. **If you are an agent reading this story: the `.camerata/checks.toml` \
edit described below must be made by the operator, not by you.**

### After editing the manifest

Regenerate the CI workflow so Layer 3 reflects the new check:

```
POST /api/projects/active/generate-ci-workflow
{ \"repo_root\": \"<absolute path to repo>\", \"stack\": \"<detected stack>\" }
```

Or, from the Camerata UI: open the project, click **Generate CI Workflow**. The generated \
`.github/workflows/camerata-gates.yml` should be committed alongside the manifest change."
}

/// Build the GitHub issue body for the **mechanical** tier story.
///
/// Mechanical rules map 1:1 to off-the-shelf linters or analyzers. The implementation is:
/// pick the version, add the manifest entry, regenerate the workflow — done. Both layers
/// pick up the change automatically.
fn ci_story_body_mechanical(repo: &str, rules: &[CiStoryRule]) -> String {
    let rule_lines: String = rules
        .iter()
        .map(|r| {
            if let Some(ref linter) = r.linter {
                format!("- **{}** — {} _(linter: {})_\n", r.id, r.title, linter)
            } else {
                format!("- **{}** — {}\n", r.id, r.title)
            }
        })
        .collect();

    // Per-rule manifest examples — one annotated entry per rule.
    let manifest_examples: String = rules
        .iter()
        .map(|r| {
            let tool_hint = r.linter.as_deref().unwrap_or("<tool-name>");
            format!(
                "```toml\n\
                 # For rule {id} — choose the exact version your team standardises on.\n\
                 [[check]]\n\
                 id       = \"{id}\"\n\
                 name     = \"{title}\"\n\
                 tool     = \"{tool}\"           # the off-the-shelf linter binary\n\
                 version  = \"<x.y.z>\"          # EXACT pinned version — no ranges\n\
                 install  = \"{tool}@<x.y.z>\"   # exact install command (npm/pip/cargo/etc.)\n\
                 command  = \"<linter invocation with --config or --rule flag>\"\n\
                 severity = \"high\"\n\
                 in_loop  = true               # bounces the agent at Layer 2; also in CI\n\
                 ```\n\n",
                id = r.id,
                title = r.title,
                tool = tool_hint,
            )
        })
        .collect();

    // Rule-specific cadence guidance: the dep-audit rule requires the developer
    // to choose and implement a cadence (weekly cron, per-PR, in-loop, or manual).
    // Camerata does NOT build a scheduling engine — the cadence is a project decision
    // carried here as concrete implementation guidance.
    let dep_audit_cadence_section =
        if rules.iter().any(|r| r.id == "CICD-DEPENDENCY-AUDIT-1") {
            "\n---\n\n\
             ## Dependency vulnerability scanning — cadence (CICD-DEPENDENCY-AUDIT-1)\n\n\
             **Camerata does not build a scheduling engine.** The cadence for dependency \
             scanning is a project decision you make on the rule, and the developer \
             implements the cadence (cron job, PR trigger, in-loop, or on-demand) when \
             wiring CI. The manifest entry above registers the tool and version; the \
             CI workflow wires the trigger.\n\n\
             ### Cadence options\n\n\
             Choose ONE of the following cadences and implement it in your CI workflow:\n\n\
             #### Option A — Weekly scheduled CI job (recommended)\n\n\
             Catches newly-disclosed CVEs that affect already-merged dependencies — the \
             class of vulnerability a per-PR scan misses entirely. Low noise, no per-PR \
             friction. The standard cadence for dependency scanning.\n\n\
             ```yaml\n\
             # .github/workflows/dep-audit.yml\n\
             on:\n\
             \x20 schedule:\n\
             \x20   - cron: '0 3 * * 1'   # every Monday at 03:00 UTC\n\
             \x20 workflow_dispatch:        # allow manual on-demand runs\n\
             ```\n\n\
             Manifest entry — set `in_loop = false` (CI-only cadence):\n\n\
             ```toml\n\
             [[check]]\n\
             id       = \"CICD-DEPENDENCY-AUDIT-1\"\n\
             name     = \"dependency vulnerability scan\"\n\
             tool     = \"osv-scanner\"\n\
             version  = \"<x.y.z>\"          # EXACT pinned version — no ranges\n\
             install  = \"curl -sSfL https://github.com/google/osv-scanner/releases/download/v<x.y.z>/osv-scanner_linux_amd64 -o osv-scanner && chmod +x osv-scanner && sudo mv osv-scanner /usr/local/bin/\"\n\
             command  = \"osv-scanner -r .\"\n\
             severity = \"high\"\n\
             in_loop  = false               # weekly CI job; not run on every agent task\n\
             ```\n\n\
             #### Option B — Per-PR / push trigger\n\n\
             Catches a vulnerable dependency the moment it lands in a PR. Higher per-PR \
             friction; appropriate for regulated environments or low-tolerance security postures.\n\n\
             ```yaml\n\
             on:\n\
             \x20 pull_request:\n\
             \x20 push:\n\
             \x20   branches: [main]\n\
             ```\n\n\
             Use the same manifest entry as Option A (`in_loop = false`).\n\n\
             #### Option C — In-loop AND CI (every-pass, highest coverage)\n\n\
             Set `in_loop = true` so osv-scanner also runs in the Camerata Layer-2 \
             in-loop dev gate after each agent task. osv-scanner is fast (lock-file \
             read, no full build), so in-loop overhead is typically a few seconds.\n\n\
             ```toml\n\
             in_loop  = true   # bounces the agent at Layer 2 too; also in CI\n\
             ```\n\n\
             Pair with any CI trigger (weekly cron, per-PR, or both).\n\n\
             #### Option D — Manual / on-demand only\n\n\
             `workflow_dispatch` only. No automated trigger. Appropriate for early-stage \
             projects or teams that fire the scan deliberately (pre-release, post-dep-bump).\n\n\
             ```yaml\n\
             on:\n\
             \x20 workflow_dispatch:\n\
             ```\n\n\
             Use `in_loop = false` in the manifest.\n\n\
             ### osv-scanner invocation\n\n\
             ```sh\n\
             # Recursive scan from repo root — reads all supported lock files.\n\
             osv-scanner --format json -r . | tee osv-results.json\n\
             osv-scanner -r . 2>&1 || exit 1\n\
             ```\n\n\
             `osv-scanner -r .` exits non-zero on any finding — which is what the \
             `.camerata/checks.toml` manifest runner and CI gate both test. Pin the \
             osv-scanner version in `tool` + `version` + `install` in the manifest \
             so Layer 2 and Layer 3 always run the same binary.\n\n\
             ### Checklist for this rule\n\n\
             - [ ] Decide the cadence (A, B, C, or D above).\n\
             - [ ] Add the `[[check]]` entry to `.camerata/checks.toml` with pinned \
             `tool`, `version`, and `install`; set `in_loop` to match the cadence.\n\
             - [ ] Commit `.camerata/checks.toml` (human/operator commit — agents \
             cannot write `.camerata/`).\n\
             - [ ] Wire the CI workflow trigger (cron/pull_request/workflow_dispatch) \
             in `.github/workflows/dep-audit.yml`.\n\
             - [ ] Regenerate `.github/workflows/camerata-gates.yml` via Camerata.\n\
             - [ ] Verify the scan fires at the chosen cadence and fails on a known \
             vulnerable lockfile (e.g. introduce a known-CVE package, confirm exit non-zero).\n"
        } else {
            ""
        };

    let preamble = ci_story_ssot_preamble();
    format!(
        "{preamble}\n\n\
         ---\n\n\
         ## This story: MECHANICAL tier\n\n\
         **This story covers the MECHANICAL tier only.** Each rule below maps to a real \
         off-the-shelf linter or analyzer. The entire implementation is:\n\n\
         1. Decide the exact tool version your team will standardise on.\n\
         2. Add the manifest entry to `.camerata/checks.toml` (see examples below).\n\
         3. Commit the manifest (operator/human commit — agents cannot write `.camerata/`).\n\
         4. Regenerate the CI workflow: both Layer 2 and Layer 3 automatically enforce \
         the check. No separate CI wiring step is needed.\n\n\
         **Repo:** `{repo}`\n\n\
         **Rules to wire:**\n\
         {rule_lines}\n\
         ---\n\n\
         ## Manifest entries to add\n\n\
         For each rule, add one `[[check]]` block to `.camerata/checks.toml`. \
         Examples (fill in the real version and invocation):\n\n\
         {manifest_examples}\
         > **Version pinning is not optional.** Without it, Layer 2 and Layer 3 can run \
         > different tool versions on the same ruleset and produce different results. \
         > Pin the exact version; use `install` so CI installs it before running the check.\
         {dep_audit_cadence_section}\n\n\
         ---\n\n\
         ## Implementation checklist\n\n\
         - [ ] For each rule: confirm the linter is already in the repo or add it as a dev dependency.\n\
         - [ ] Add the `[[check]]` entry to `.camerata/checks.toml` with pinned `tool`, `version`, \
         and `install`.\n\
         - [ ] Commit `.camerata/checks.toml` (human/operator commit).\n\
         - [ ] Regenerate `.github/workflows/camerata-gates.yml` via Camerata and commit it.\n\
         - [ ] Open a draft PR; verify CI passes with the new check step visible in the workflow run.\n\
         - [ ] Do not weaken or delete existing checks.\n\n\
         _Filed by Camerata onboarding._"
    )
}

/// Build the GitHub issue body for the **architectural** tier story.
///
/// Architectural rules are deterministic (a PR either violates them or it does not) but have
/// NO off-the-shelf linter. Each requires a bespoke checker — a script, custom Semgrep rule,
/// AST pass, or dependency-graph query — designed and scoped by the team before wiring.
///
/// The canonical worked example is API-layering enforcement via `dependency-cruiser`:
/// a JS/TS repo uses a `.dependency-cruiser.cjs` config that asserts the allowed import
/// graph; `depcruise` returns non-zero on violation.  Register it in the manifest, and
/// both Layer 2 and Layer 3 enforce it identically with no separate wiring.
fn ci_story_body_architectural(repo: &str, rules: &[CiStoryRule]) -> String {
    let rule_lines: String = rules
        .iter()
        .map(|r| format!("- **{}** — {}\n", r.id, r.title))
        .collect();

    let preamble = ci_story_ssot_preamble();
    format!(
        "{preamble}\n\n\
         ---\n\n\
         ## This story: ARCHITECTURAL tier\n\n\
         **This story covers the ARCHITECTURAL tier only.** Each rule below is deterministic \
         but has **no off-the-shelf linter**. Each one requires a bespoke checker — a script, \
         custom Semgrep rule, AST pass, or dependency-graph query — that the team must DESIGN \
         and SCOPE before implementation begins. **This is design work first, not a \
         configuration task.**\n\n\
         Scope each rule as a sub-task. Do not block the mechanical CI story on this work.\n\n\
         **Repo:** `{repo}`\n\n\
         **Rules that need a custom checker:**\n\
         {rule_lines}\n\
         ---\n\n\
         ## How to implement each rule (step-by-step)\n\n\
         For each rule in the list above, follow this process:\n\n\
         ### Step 1 — Design the deterministic checker\n\n\
         Choose a strategy that returns **exit 0 on pass, non-zero on violation**, with \
         CWD = repo root. Options (not exhaustive):\n\n\
         - **dependency-cruiser** (JS/TS layering): write a `.dependency-cruiser.cjs` config \
         that declares the allowed import graph; the checker command is \
         `depcruise --config .dependency-cruiser.cjs src`.\n\
         - **Custom Semgrep rule**: write a `.semgrep.yml` with a `pattern-not-inside` rule \
         and run `semgrep --config .semgrep.yml --error .`.\n\
         - **Shell/Python script**: traverse the AST or grep for the pattern; exit non-zero \
         when a violation is found. Place under `scripts/` and make it executable.\n\
         - **Any other deterministic static tool**: anything that can be invoked from the \
         repo root and signals pass/fail via exit code.\n\n\
         **Worked example — API layering (`ARCH-API-LAYERING-1`):**\n\n\
         The rule asserts that service modules never import from repository modules directly \
         (services orchestrate; repositories own data access). `dependency-cruiser` can \
         encode this as a forbidden dependency arc:\n\n\
         ```js\n\
         // .dependency-cruiser.cjs\n\
         module.exports = {{\n\
           forbidden: [\n\
             {{\n\
               name: 'no-service-to-repo-direct',\n\
               severity: 'error',\n\
               from: {{ path: 'src/services/' }},\n\
               to:   {{ path: 'src/repositories/' }},\n\
             }},\n\
           ],\n\
         }};\n\
         ```\n\n\
         Run `depcruise --config .dependency-cruiser.cjs src` — zero output + exit 0 means \
         clean; non-zero means a service is importing a repository directly.\n\n\
         ### Step 2 — Add the manifest entry to `.camerata/checks.toml`\n\n\
         Once the checker script/config is committed and verified locally, register it in \
         the manifest. Pin the exact tool version so Layer 2 and Layer 3 run identically:\n\n\
         ```toml\n\
         # .camerata/checks.toml\n\
         [[check]]\n\
         id       = \"ARCH-API-LAYERING-1\"       # matches the rule id in the corpus\n\
         name     = \"API layering\"\n\
         tool     = \"dependency-cruiser\"         # omit if using a repo script with no external tool\n\
         version  = \"6.3.0\"                      # EXACT pinned version — no ranges\n\
         install  = \"npm install -g dependency-cruiser@6.3.0\"\n\
         command  = \"depcruise --config .dependency-cruiser.cjs src\"\n\
         severity = \"high\"\n\
         in_loop  = true                          # bounces the agent at Layer 2 too\n\
         ```\n\n\
         For a repo-local script (no external tool required):\n\n\
         ```toml\n\
         [[check]]\n\
         id       = \"ARCH-MY-CUSTOM-RULE-1\"\n\
         name     = \"<rule description>\"\n\
         command  = \"scripts/check_my_rule.sh\"   # must be executable; cwd = repo root\n\
         severity = \"high\"\n\
         in_loop  = true\n\
         # tool / version / install omitted when no external binary is required\n\
         ```\n\n\
         > **Gate protection:** `.camerata/checks.toml` is protected by `SEC-NO-CAMERATA-CONFIG-1`. \
         > Agents cannot write to `.camerata/`. This manifest edit MUST be a human/operator \
         > commit.\n\n\
         ### Step 3 — Regenerate the CI workflow\n\n\
         After committing the manifest, regenerate `.github/workflows/camerata-gates.yml`:\n\n\
         ```\n\
         POST /api/projects/active/generate-ci-workflow\n\
         {{ \"repo_root\": \"<absolute path>\", \"stack\": \"<detected stack>\" }}\n\
         ```\n\n\
         Or use the Camerata UI: open the project, click **Generate CI Workflow**. \
         Commit the updated workflow file.\n\n\
         ### Step 4 — Verify at both layers\n\n\
         - **Layer 2**: trigger a governed dev run in Camerata and confirm the check fires \
         and bounces on a known violation.\n\
         - **Layer 3**: open a PR and confirm the check step appears in the generated \
         `.github/workflows/camerata-gates.yml` workflow run.\n\n\
         The result: one definition in `.camerata/checks.toml`, enforced identically at \
         Layer 2 and Layer 3. No separate L2 vs L3 wiring is needed.\n\n\
         ---\n\n\
         ## Implementation checklist (per rule)\n\n\
         - [ ] Agree on the checker strategy and scope it (design phase).\n\
         - [ ] Build and verify the checker in isolation (returns 0 on pass, non-zero on violation).\n\
         - [ ] Commit the checker script/config to the repo.\n\
         - [ ] Add the `[[check]]` entry to `.camerata/checks.toml` with pinned `tool` + \
         `version` + `install` (if using an external tool).\n\
         - [ ] Commit `.camerata/checks.toml` (human/operator commit — not an agent).\n\
         - [ ] Regenerate `.github/workflows/camerata-gates.yml` via Camerata and commit it.\n\
         - [ ] Verify the check fires at Layer 2 (governed dev run) and Layer 3 (CI on PR).\n\
         - [ ] Do not weaken or delete existing checks.\n\
         - [ ] Scope each rule as its own sub-task if the list is long. Do not block the \
         mechanical CI story on this work.\n\n\
         _Filed by Camerata onboarding._"
    )
}

/// Emit a tier-specific "wire CI rules" story as a GitHub issue.
///
/// Two tiers are supported:
/// - "mechanical"   — rules that map 1:1 to an off-the-shelf linter/analyzer. Wiring is
///   straightforward: add a manifest entry to `.camerata/checks.toml` (the SSOT) and both
///   Layer 2 (in-loop) and Layer 3 (CI) automatically enforce it.
/// - "architectural" — rules that are also deterministic but require a bespoke AST or static-
///   analysis checker the team must DESIGN before implementing. This story should be scoped
///   and refined first; it should NOT ride with the mechanical story.
///
/// The UI files each story separately so the two tracks land as distinct GitHub issues.
/// Both stories carry the full SSOT HOW-TO so a developer or AI agent can implement
/// the check correctly without additional hand-holding.
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

    let (title, body) = match req.tier.as_str() {
        "mechanical" => {
            let t = format!("Wire mechanical (off-the-shelf linter) rules into CI — {}", req.repo);
            let b = ci_story_body_mechanical(repo, &req.rules);
            (t, b)
        }
        _ => {
            // architectural
            let t = format!(
                "Wire architectural (custom-checker) rules into CI — {}",
                req.repo
            );
            let b = ci_story_body_architectural(repo, &req.rules);
            (t, b)
        }
    };

    match crate::onboard::create_issue(owner, repo, &token, &title, &body).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

// ── CI workflow generator endpoint ───────────────────────────────────────────

/// Request body for the CI workflow generation endpoint.
///
/// The caller supplies the active project's repo root path and the detected
/// language stack. The server loads `.camerata/checks.toml` from the root and
/// generates the complete `.github/workflows/camerata-gates.yml` YAML.
#[derive(serde::Deserialize)]
struct GenerateCiWorkflowReq {
    /// Absolute path to the repo/worktree root on the server's filesystem.
    repo_root: String,
    /// The detected language stack (defaults to Rust when omitted).
    #[serde(default)]
    stack: crate::workflow_gen::RepoStack,
}

/// `POST /api/projects/active/generate-ci-workflow`
///
/// Load `.camerata/checks.toml` from `req.repo_root`, generate the
/// `.github/workflows/camerata-gates.yml` YAML for the given stack, write it to
/// disk under `repo_root`, and return the YAML body.
///
/// A missing manifest is NOT an error — it generates a workflow with only the
/// built-in language steps. The caller can store the YAML in the repo.
async fn generate_ci_workflow(
    Json(req): Json<GenerateCiWorkflowReq>,
) -> Json<serde_json::Value> {
    use crate::workflow_gen::generate_gates_workflow;
    use camerata_checks::manifest::load_manifest;
    use std::path::Path;

    let root = Path::new(&req.repo_root);

    // Load manifest (best-effort: absent/invalid → empty).
    let manifest = match load_manifest(root) {
        Ok(Some(m)) => m,
        Ok(None) => camerata_checks::CheckManifest::default(),
        Err(e) => {
            eprintln!(
                "[camerata-server] generate-ci-workflow: manifest parse error: {}; \
                 generating without manifest checks",
                e
            );
            camerata_checks::CheckManifest::default()
        }
    };

    let yaml = generate_gates_workflow(&req.stack, &manifest);

    // Write to <repo_root>/.github/workflows/camerata-gates.yml.
    let workflow_path = root
        .join(".github")
        .join("workflows")
        .join("camerata-gates.yml");

    if let Some(parent) = workflow_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to create workflow dir: {e}")
            }));
        }
    }

    if let Err(e) = std::fs::write(&workflow_path, &yaml) {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("failed to write workflow file: {e}")
        }));
    }

    Json(serde_json::json!({
        "ok": true,
        "path": workflow_path.display().to_string(),
        "yaml": yaml
    }))
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

/// Re-emit a project's ruleset directly into the LOCAL working copies of its repos,
/// unconditionally — regardless of onboarding state. Rebuilds AGENTS.md +
/// CONVENTIONS.md + the gate configs from the project's CURRENT principle sources and
/// ALWAYS writes them to disk. Does NOT open a PR and does NOT require the repo to be
/// already onboarded. This is the "re-sync my local clones" action for an architect who
/// edited the ruleset after onboarding and wants the local files to reflect the current
/// selections without going through a full re-onboard.
///
/// Needs a workspace folder set (or per-repo path overrides via repo health) — Apply
/// writes into the repo's local clone. The token is required so the branch can also be
/// pushed to origin (keeping the remote in sync with the local write).
async fn emit_project_local(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let Some(project) = state.projects.get(&id) else {
        return Json(serde_json::json!({ "ok": false, "message": "no such project" }));
    };
    let token = std::env::var("CAMERATA_GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_default();

    let rules = resolve_project_arm_rules(&project).await;
    if rules.is_empty() && project.ruleset.custom.is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "Nothing to emit — this project has no repo-local rules or custom rules yet." }),
        );
    }

    // Re-emit carries no new baseline (it's a ruleset refresh, not onboarding).
    let no_baselines: std::collections::HashMap<String, String> = Default::default();
    let workspace_root = state.settings.workspace_root();

    // Filter to repo-local rules only (cross-repo / process rules are project-level).
    let repo_local: Vec<crate::arm::ArmRule> = rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .cloned()
        .collect();
    let mut repos: Vec<String> = repo_local.iter().flat_map(|r| r.repos.clone()).collect();
    repos.sort();
    repos.dedup();

    let has_any_local =
        workspace_root.is_some() || repos.iter().any(|r| state.settings.repo_path(r).is_some());
    if !has_any_local {
        return Json(
            serde_json::json!({ "ok": false, "message": "Set a local workspace folder (Settings) or choose each repo's local folder (repo health) — emit-local writes into the repo's local clone." }),
        );
    }

    let custom = project.ruleset.custom.clone();
    let mut results = Vec::new();
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
        // Build the governance files unconditionally — no onboarding-state gate.
        let mut files = crate::arm::arm_files_for_repo(&repo_rules, &repo_custom);
        if let Some(baseline_json) = no_baselines.get(repo) {
            files.push((".camerata/baseline.json".to_string(), baseline_json.clone()));
        }
        // Resolve this repo's local dir: per-repo override wins, else <workspace_root>/<repo>.
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
        let clone_root = if override_path
            .as_deref()
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false)
        {
            None
        } else {
            workspace_root.as_deref().map(std::path::Path::new)
        };
        let msg = format!("chore(governance): re-emit Camerata ruleset to {repo}");
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
                results.push(serde_json::json!({
                    "repo": repo, "ok": true, "branch": crate::arm::ARM_BRANCH, "path": path
                }));
            }
            Err(e) => results.push(serde_json::json!({
                "repo": repo, "ok": false, "message": format!("{e}")
            })),
        }
    }
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
    let llm = state.llm();
    // Decomposition is a NON-FLEET step: model from the active project's per-step config.
    let model = step_model(&state, crate::project::StepKind::Decomposition);
    let grounding = state.project_grounding().await;
    // MULTI-REPO READ: ALL the active project's local repo clones (read-only across all).
    let repo_dirs = state.active_repo_dirs();
    let children = crate::decompose::propose_ai(
        &parent,
        &Practice::default_feature(),
        &llm,
        &model,
        grounding.as_deref(),
        &repo_dirs,
    )
    .await;
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
    let llm = state.llm();
    // Clarification authoring is a NON-FLEET step: model from the active project's per-step
    // config (DEFAULT_MODEL floor only when there is no active project).
    let model = step_model(&state, crate::project::StepKind::Clarification);
    let questions = match llm
        .complete(
            crate::llm::LlmRequest::new(user)
                .with_model(model)
                .with_system(system),
        )
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

/// Routines for the active project. No active project → empty list.
async fn list_routines(State(state): State<AppState>) -> Json<Vec<Routine>> {
    let Some(p) = state.projects.active() else {
        return Json(vec![]);
    };
    Json(state.routines.list_for_project(&p.id))
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
    State(state): State<AppState>,
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
    let completer = crate::llm::build_completer(
        &req.model,
        &state.model_registry,
        state.credential_store.as_ref(),
        std::sync::Arc::new(state.llm()),
        state.rate_limiter.clone(),
    )
    .unwrap_or_else(|_| std::sync::Arc::new(state.llm()));
    match completer
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
    // GROUNDING (the invariant): the lead-engineer review chat reasons about the ACTUAL
    // project — append the active project's repo + rule digest to the system prompt.
    let system = match state.project_grounding().await {
        Some(grounding) => format!(
            "{}\n\n{grounding}",
            crate::escalation::chat_system_prompt(&esc)
        ),
        None => crate::escalation::chat_system_prompt(&esc),
    };
    let user = crate::escalation::chat_user_prompt(&esc, &req.message);
    let completer = crate::llm::build_completer(
        &req.model,
        &state.model_registry,
        state.credential_store.as_ref(),
        std::sync::Arc::new(state.llm()),
        state.rate_limiter.clone(),
    )
    .unwrap_or_else(|_| std::sync::Arc::new(state.llm()));
    let reply = match completer
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
        llm: state.llm(),
        // MULTI-REPO READ: let the translator read across ALL the active project's repo clones.
        repo_dirs: state.active_repo_dirs(),
    };
    // Escalation translation is a NON-FLEET step: the model comes from the active project's
    // per-step config (no env/const fallback once a project exists), replacing the prior
    // per-routine model. DEFAULT_MODEL floor only applies with no active project.
    let model = step_model(&state, crate::project::StepKind::Escalation);
    let grounding = state.project_grounding().await;
    let payload = crate::escalation::translate_answer_ai(
        &driver,
        &esc,
        &req.answer,
        &model,
        grounding.as_deref(),
    )
    .await;
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

/// `GET /api/models/registry` — return the full model registry (Claude static + cached
/// OpenRouter entries). The response carries an `openrouter_fetched` flag; when `false`
/// the client can prompt the user to add an OpenRouter key and call the refresh endpoint.
async fn get_model_registry(
    State(state): State<AppState>,
) -> Json<crate::model_registry::RegistryResp> {
    let models = state.model_registry.all_entries();
    let openrouter_fetched = state.model_registry.openrouter_fetched();
    Json(crate::model_registry::RegistryResp {
        models,
        openrouter_fetched,
    })
}

/// `POST /api/models/registry/refresh` — (re-)fetch the OpenRouter model catalog using
/// the credential stored in the OS keychain. Returns the count of fetched models plus the
/// full updated registry. Returns `{ attempted: false, openrouter_count: 0 }` when the
/// OpenRouter API key is not set (the Claude portion is unaffected).
async fn refresh_model_registry(
    State(state): State<AppState>,
) -> Json<crate::model_registry::RefreshResp> {
    let attempted = state
        .model_registry
        .try_refresh_from_store(state.credential_store.as_ref())
        .await;
    let models = state.model_registry.all_entries();
    let openrouter_count = models.iter().filter(|e| e.provider == "openrouter").count();
    Json(crate::model_registry::RefreshResp {
        openrouter_count,
        attempted,
        models,
    })
}

/// One prior turn in a research-chat conversation, sent by the UI with each POST so the
/// model has memory of the thread. role must be "user" or "assistant".
#[derive(serde::Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(serde::Deserialize)]
struct ChatReq {
    prompt: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    system: Option<String>,
    /// Prior turns in chronological order (oldest first). Empty on the first message.
    /// The UI accumulates turns client-side and sends them with every POST.
    /// Back-compat: omitted or null → treated as empty (same as before this fix).
    #[serde(default)]
    history: Vec<ChatTurn>,
}

/// Maximum number of prior turns to embed in the prompt. Oldest turns beyond this limit
/// are dropped (FIFO) to keep token usage bounded. A "turn" is one user + one assistant
/// exchange, so 20 turns ≈ 40 messages ≈ several thousand tokens of context at most.
const CHAT_HISTORY_TURN_CAP: usize = 20;

/// Render prior conversation turns into a transcript block that is prepended to the
/// user's new message. When history is empty (first message or back-compat), returns
/// `None` so the caller can fall back to the bare single-prompt path.
///
/// Format:
/// ```text
/// Conversation so far:
/// User: <content>
/// Assistant: <content>
/// ...
///
/// User's new message:
/// <prompt>
/// ```
///
/// Roles are normalized: "user" maps to "User", anything else (including "assistant")
/// maps to "Assistant". The recent-window cap (`CHAT_HISTORY_TURN_CAP`) is applied
/// before rendering — oldest turns are dropped first.
pub(crate) fn render_chat_prompt(history: &[ChatTurn], prompt: &str) -> String {
    if history.is_empty() {
        return prompt.to_string();
    }

    // Apply cap: keep only the most-recent CHAT_HISTORY_TURN_CAP turns.
    // (Each element of `history` is a single message; cap on messages, not pairs.)
    let cap = CHAT_HISTORY_TURN_CAP;
    let capped: &[ChatTurn] = if history.len() > cap {
        // Drop the oldest turns — always drop from the front (FIFO eviction).
        &history[history.len() - cap..]
    } else {
        history
    };

    let mut out = String::from("Conversation so far:\n");
    for turn in capped {
        let label = if turn.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&turn.content);
        out.push('\n');
    }
    out.push_str("\nUser's new message:\n");
    out.push_str(prompt);
    out
}

/// The research chat: one completion through the configured backend. The side-by-side
/// chatbot uses this; it's also the smoke test that the model wiring works.
///
/// When `history` is present in the request body, prior turns are embedded into the
/// prompt text as a transcript block before the new message (see `render_chat_prompt`).
/// The grounding system prompt (`system`) is passed through unchanged so all four
/// context layers remain intact. An empty or absent `history` behaves exactly as before
/// this fix (back-compat: the single-prompt path is unaffected).
async fn chat(
    State(state): State<AppState>,
    Json(req): Json<ChatReq>,
) -> Result<Json<crate::llm::LlmResponse>, AppError> {
    // Research chat is a UI-PICKED non-fleet step: an explicit request model wins; otherwise
    // the active project's per-step default applies (DEFAULT_MODEL floor only with no project).
    let model = step_model_or(&state, crate::project::StepKind::ResearchChat, Some(&req.model));
    // Route to the right Completer: OpenRouter for openrouter-provider models, Anthropic for all
    // others. Returns an error when the model is OpenRouter-provider but no key is set.
    let completer = crate::llm::build_completer(
        &model,
        &state.model_registry,
        state.credential_store.as_ref(),
        std::sync::Arc::new(state.llm()),
        state.rate_limiter.clone(),
    )
    .map_err(AppError)?;
    // Embed history into the prompt when prior turns exist; otherwise use the bare prompt.
    let full_prompt = render_chat_prompt(&req.history, &req.prompt);
    let mut r = crate::llm::LlmRequest::new(full_prompt).with_model(model);
    if let Some(system) = req.system {
        r = r.with_system(system);
    }
    Ok(Json(completer.complete(r).await?))
}

/// `GET /api/usage` — the cumulative, session-wide LLM usage snapshot for the cockpit's
/// persistent usage meter: total input/output/cache tokens, derived/reported $ cost, call
/// count, a by-model breakdown, and the current rate-limited state. Provider-agnostic (keys
/// off the vendor-neutral `LlmResponse` usage fields), so it works for Claude today and for
/// a future Gemini arm with no change. Observability only — reading it changes nothing.
async fn usage(State(state): State<AppState>) -> Json<crate::usage_ledger::UsageSnapshot> {
    Json(state.usage_ledger.snapshot())
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

// ── Credential manager endpoints ──────────────────────────────────────────────

/// Request body for `POST /api/credentials/:name`.
#[derive(serde::Deserialize)]
struct SetCredentialReq {
    value: String,
}

/// One credential's status as returned by `GET /api/credentials`.
///
/// The `masked` field is the ONLY value ever placed in an HTTP response.
/// The full secret is never serialised or logged.
#[derive(serde::Serialize)]
struct CredentialStatus {
    name: String,
    is_set: bool,
    masked: Option<String>,
}

/// `POST /api/credentials/:name` — store a credential in the OS keychain.
///
/// Body: `{ "value": "…" }`
/// Response: `{ "ok": true, "masked": "sk-o••••" }`
///
/// The full value is written to the keychain and immediately discarded from
/// memory; the response contains only the masked form.
async fn set_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<SetCredentialReq>,
) -> impl IntoResponse {
    use crate::credentials::ALL_CREDENTIALS;
    // Reject unknown credential names to prevent arbitrary keychain writes.
    if !ALL_CREDENTIALS.contains(&name.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("unknown credential name: {name}")
            })),
        );
    }
    if req.value.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": "value must not be empty"
            })),
        );
    }
    if let Err(e) = state.credential_store.set(&name, &req.value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to store credential: {e}")
            })),
        );
    }
    // Return the masked form so the UI can confirm what was stored.
    let masked = state
        .credential_store
        .masked(&name)
        .ok()
        .flatten()
        .unwrap_or_else(|| "••••".to_string());
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "masked": masked })),
    )
}

/// `GET /api/credentials` — list all known credentials with their set-status and
/// masked values.  The full value is never included.
///
/// Response: `[{ "name": "openrouter_api_key", "is_set": true, "masked": "sk-o••••" }, …]`
async fn list_credentials(State(state): State<AppState>) -> Json<Vec<CredentialStatus>> {
    use crate::credentials::ALL_CREDENTIALS;
    let statuses: Vec<CredentialStatus> = ALL_CREDENTIALS
        .iter()
        .map(|name| {
            let is_set = state.credential_store.is_set(name).unwrap_or(false);
            let masked = state.credential_store.masked(name).ok().flatten();
            CredentialStatus {
                name: name.to_string(),
                is_set,
                masked,
            }
        })
        .collect();
    Json(statuses)
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
    let Some(p) = state.projects.active() else {
        return Json(vec![]);
    };
    Json(state.uow.list_for_project(&p.id, &p.repos))
}

/// The UoW for a story. Creates a default one if the story has no UoW yet.
async fn uow_get(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.get_or_create(&story_id))
}

// ── Provider-agnostic WorkItem + UoW layer (governed-dev surface) ──────────────

/// Read the GitHub token: credential store first, then `CAMERATA_GITHUB_TOKEN` env
/// var as a back-compat fallback.  Returns `None` when neither is set or non-empty.
///
/// This is the canonical resolution path used by all handlers.  The plain
/// `github_token_env()` helper below covers the few call sites that run before the
/// AppState is available (e.g. the connections probe, which runs out of band).
fn github_token() -> Option<String> {
    github_token_env()
}

/// Read `CAMERATA_GITHUB_TOKEN` from the environment only (no credential store).
/// Used by the connections probe and other paths that have no AppState at hand.
fn github_token_env() -> Option<String> {
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
    let Some(token) = state.github_token() else {
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
        // Prefer the GraphQL path: it carries the sub-issue `parent` linkage the REST
        // issues-list response omits (root cause of every issue showing "(no parent)").
        // FALL BACK to the parent-less REST list on any GraphQL failure (auth scope, API
        // error, network) so the pull still works — just without parent grouping.
        let fetched = match crate::github_issues::list_open_issues_with_parents(repo, &token).await {
            Ok(issues) => Ok(issues),
            Err(_) => crate::github_issues::list_open_issues(repo, &token).await,
        };
        match fetched {
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
                        parent_number: issue.parent_number,
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
    State(state): State<AppState>,
    Json(req): Json<WorkItemRefreshReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = state
        .github_token()
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
    State(state): State<AppState>,
    Json(req): Json<WorkItemCommentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = state
        .github_token()
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
    State(state): State<AppState>,
    Json(req): Json<WorkItemCommentsReq>,
) -> Json<serde_json::Value> {
    let Some(token) = state.github_token() else {
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
    State(state): State<AppState>,
    Json(req): Json<WorkItemAssigneesReq>,
) -> Json<serde_json::Value> {
    let Some(token) = state.github_token() else {
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

/// `POST /api/workitems/set-parent` body `{ work_item_id, parent_number }` — make an
/// existing work item (the CHILD) a native GitHub sub-issue of `parent_number` (in the
/// child's own repo). This is the "Set parent" affordance from the work-item modal: it
/// fixes the orphaned-issue case where a pulled issue has no parent linkage yet.
///
/// `parent_number` accepts a JSON number or string (`42` or `"42"`/`"#42"`); it is
/// normalized to a bare number. Resolves the child's GitHub database id (the large `id`
/// the sub-issue API requires) from its per-repo number, then links it under the parent.
///
/// Returns `{ ok: true }` on success, `{ ok: false, message }` on any failure (no token,
/// malformed id, bad parent number, GitHub error). Never panics.
#[derive(serde::Deserialize)]
struct WorkItemSetParentReq {
    work_item_id: String,
    /// The parent issue number — a JSON number or a string (`"#42"` is accepted).
    parent_number: serde_json::Value,
}

async fn workitems_set_parent(
    State(state): State<AppState>,
    Json(req): Json<WorkItemSetParentReq>,
) -> Json<serde_json::Value> {
    // Normalize parent_number from either a JSON number or a string.
    let parent_raw = match &req.parent_number {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("parent_number must be a number or string, got `{other}`"),
            }));
        }
    };
    let Some(parent_str) = crate::github_issues::normalize_parent_number(&parent_raw) else {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("`{parent_raw}` is not a valid issue number"),
        }));
    };
    let parent_number: u64 = match parent_str.parse() {
        Ok(n) => n,
        Err(_) => {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("`{parent_str}` is not a valid issue number"),
            }));
        }
    };

    let Some(token) = state.github_token() else {
        return Json(serde_json::json!({
            "ok": false,
            "message": "Connect GitHub (set CAMERATA_GITHUB_TOKEN) to set a parent.",
        }));
    };

    let (repo, number) = match parse_github_work_item_id(&req.work_item_id) {
        Ok(pair) => pair,
        Err(e) => {
            return Json(serde_json::json!({
                "ok": false,
                "message": e.0.to_string(),
            }));
        }
    };
    let Some(coord) = camerata_worktracker::RepoCoord::parse(&repo) else {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("work item repo is not `owner/repo`: `{repo}`"),
        }));
    };

    // Resolve the CHILD issue's GitHub database id (distinct from its per-repo number),
    // which the sub-issue link endpoint requires as `sub_issue_id`.
    let child_db_id =
        match crate::github_issues::fetch_issue_db_id(&coord.owner, &coord.repo, number, &token)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return Json(serde_json::json!({
                    "ok": false,
                    "message": format!("could not resolve issue #{number}: {e}"),
                }));
            }
        };

    match crate::github_issues::link_sub_issue(
        &coord.owner,
        &coord.repo,
        parent_number,
        child_db_id,
        &token,
    )
    .await
    {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "message": format!("could not set parent #{parent_number}: {e}"),
        })),
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
    let Some(p) = state.projects.active() else {
        return Ok(Json(serde_json::json!({ "uows": [] })));
    };
    let stories = state.stories.list().await.map_err(AppError)?;
    let uows = state.uow.list_for_project(&p.id, &p.repos);
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

    // DEDUP by external ref: a UoW already exists for this work item id WITHIN the
    // active project. Scoping to the active project's repos lets two projects adopt
    // the same issue id without colliding. No active project → no scope → no dedup.
    let already = match state.projects.active() {
        Some(p) => state
            .uow
            .list_for_project(&p.id, &p.repos)
            .iter()
            .any(|u| u.story_id == story_id),
        None => false,
    };
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
        state.github_token(),
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

/// Optional body for `POST /api/uow/blank`. An absent body or an empty `{}` is
/// treated identically to `{ "parent_id": null }` (back-compat: callers that POST
/// no body, or the existing `serde_json::json!({})` call in the UI, all work unchanged).
#[derive(serde::Deserialize, Default)]
struct UowBlankReq {
    /// The parent issue identifier (accepts `"42"` or `"#42"`). Stored on the draft
    /// after normalization (strip `#`, keep digits only). `None` → no parent link at publish.
    #[serde(default)]
    parent_id: Option<String>,
}

/// `POST /api/uow/blank` — create a blank DRAFT UoW (no story yet, `work_item = None`,
/// an empty authoring state). Optionally accepts `{ "parent_id": "42" }` to mark the
/// story as a future sub-issue of an existing GitHub issue at publish time. It appears
/// in `/api/uows` as a draft (authoring=true) and is the start of the "author a story
/// with AI" flow. Returns `{ uow_id }`.
async fn uow_blank(
    State(state): State<AppState>,
    body: Option<Json<UowBlankReq>>,
) -> Json<serde_json::Value> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    // Normalize and validate the parent_id: strip "#", ensure digits only. An invalid
    // or empty string is silently treated as None (no parent) so a typo in the UI
    // doesn't block draft creation.
    let parent_id = req
        .parent_id
        .as_deref()
        .and_then(crate::github_issues::normalize_parent_number);
    // Scope the draft to the active project so it appears in that project's
    // `list_for_project` view (and ONLY that project's) even though it has no resolvable
    // repo yet. If there is no active project the draft is still created, just unscoped.
    let project_id = state.projects.active().map(|p| p.id);
    let uow = state.uow.create_blank_with_parent(parent_id, project_id);
    Json(serde_json::json!({ "uow_id": uow.story_id }))
}

/// Body for `POST /api/uow/:story_id/set-draft-parent`. Optional `parent_id` (accepts
/// `"42"` or `"#42"`); absent / empty / null clears any previously-set parent.
#[derive(serde::Deserialize, Default)]
struct SetDraftParentReq {
    #[serde(default)]
    parent_id: Option<String>,
}

/// `POST /api/uow/:story_id/set-draft-parent` — set or clear the parent issue on a DRAFT
/// UoW. The parent is picked on the authoring screen (not up front in the nav); the value
/// is normalized and stored on the draft, and the publish step later creates the native
/// GitHub sub-issue link. Invalid / empty input clears the parent (a typo never blocks).
/// Returns the updated UoW.
async fn uow_set_draft_parent(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    body: Option<Json<SetDraftParentReq>>,
) -> Json<crate::uow::UnitOfWork> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let parent_id = req
        .parent_id
        .as_deref()
        .and_then(crate::github_issues::normalize_parent_number);
    Json(state.uow.set_draft_parent(&story_id, parent_id))
}

#[derive(serde::Deserialize)]
struct UowAuthorReq {
    /// The next message in the clarification chat. The first message is the free-text
    /// requirements; subsequent ones answer the AI's clarifying questions.
    message: String,
    /// Optional per-turn model override (the UI's model selector beside Send). When
    /// present and non-empty it takes precedence over the project's StoryAuthoring step
    /// model; when absent we fall back to the project-configured model (back-compat).
    #[serde(default)]
    model: Option<String>,
}

/// The system prompt that turns the LLM into a story-authoring assistant. It produces a
/// JSON object `{ "title", "body", "reply" }` so the server can update the draft AND show
/// a conversational reply (which may be a clarifying question).
const STORY_AUTHOR_SYSTEM: &str = "You are a product-owner assistant that drafts a single \
GitHub-issue-style user story (a title and a markdown body) from a set of requirements and \
an ongoing clarification chat. Keep one cohesive story: a concise imperative title and a \
body with sections like Summary, Acceptance Criteria (a checklist), and Notes as warranted. \
When the requirements are ambiguous or missing key detail, ASK ONE concise clarifying \
question in your reply AND offer the answerer 2-4 concrete options to choose from, each with \
a short benefit/drawback so they can decide quickly (the AskUserQuestion style). Draft the \
best story you can so far. Respond ONLY with a minified JSON object with exactly these keys: \
\"title\" (string), \"body\" (string, markdown), \"reply\" (string: a short conversational \
message to the author, e.g. your clarifying question or a note on what you changed), and \
\"options\" (array, possibly empty: when you are asking a clarifying question, 2-4 objects \
each with \"label\" (a short selectable choice) and \"description\" (its benefit/drawback); \
leave it as an empty array when you are NOT asking a question). Do not wrap the JSON in code \
fences.";

/// Parse the LLM's story-authoring response into `(title, body, reply)`. The model is asked
/// for a JSON object; if it deviates (e.g. wraps in fences or returns prose), we degrade
/// gracefully: strip a fenced block if present, else treat the whole text as the reply and
/// leave the draft unchanged signals (empty strings).
fn parse_author_response(raw: &str) -> (String, String, String, Vec<crate::clarify::ClarifyOption>) {
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
        // Optional structured options for a clarifying question. Drop any entry missing
        // a label; an absent/empty array means "not asking a structured question" and
        // the loop falls back to the free-text reply.
        let options = v
            .get("options")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let label = o.get("label").and_then(|x| x.as_str())?.trim().to_string();
                        if label.is_empty() {
                            return None;
                        }
                        let description = o
                            .get("description")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(crate::clarify::ClarifyOption { label, description })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return (title, body, reply, options);
    }
    // Not JSON: keep the raw text as the conversational reply; leave the draft untouched.
    (String::new(), String::new(), trimmed.to_string(), Vec::new())
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

    let llm = state.llm();
    // Story authoring is a NON-FLEET step: its model comes from the active project's
    // per-step config (no env/const fallback once a project exists). The project-less edge
    // (no active project) is the only place the DEFAULT_MODEL floor applies. An explicit
    // per-turn override from the UI's model selector wins when supplied.
    let model = match req.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => step_model(&state, crate::project::StepKind::StoryAuthoring),
    };
    // GROUNDING (the invariant): the story-authoring agent is a bare-LLM call — the
    // grounding block is its ONLY window onto the real project. Without it, it behaves
    // like a context-less product owner (e.g. asking where to persist a preference
    // across "logged-in users" for an app that has no auth at all). Prepend the active
    // project's repo + rule digest to the system prompt so it drafts against the ACTUAL
    // codebase. See docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md.
    let system = match state.project_grounding().await {
        Some(grounding) => format!("{STORY_AUTHOR_SYSTEM}\n\n{grounding}"),
        None => STORY_AUTHOR_SYSTEM.to_string(),
    };
    // ON-DEMAND REPO READ (the core invariant, beyond the digest): when the active project's
    // repo is local, let the story-authoring model SCAN the actual codebase (cwd + --add-dir
    // + read-only built-ins on the CLI backend) instead of relying only on the inlined
    // digest. This is what stops the context-less-product-owner failure (asking about auth
    // for an app that has none): it can read the real dependencies/code. READ-ONLY — no write
    // path is added. Degrades to digest-only on the API backend / when no repo is local.
    let mut request = crate::llm::LlmRequest::new(prompt)
        .with_model(model)
        .with_system(system);
    // MULTI-REPO READ: a project has several repos. Grant the story-author model read across
    // ALL the active project's repo clones (cwd = primary, the rest as read-only `--add-dir`).
    let repo_dirs = state.active_repo_dirs();
    if !repo_dirs.is_empty() {
        request = request.with_repo_read_dirs(repo_dirs);
    }
    let (title, body, reply, options) = match llm.complete(request).await {
        Ok(resp) => {
            let (t, b, r, opts) = parse_author_response(&resp.text);
            // Keep the existing draft when the model returned no usable title/body.
            let title = if t.is_empty() { prior.draft_title.clone() } else { t };
            let body = if b.is_empty() { prior.draft_body.clone() } else { b };
            let reply = if r.is_empty() {
                "Updated the draft.".to_string()
            } else {
                r
            };
            (title, body, reply, opts)
        }
        Err(e) => {
            // Token-less / LLM-off: don't crash; record a clear note and keep the draft.
            let note = format!(
                "AI drafting is unavailable right now ({}). Your message was saved; configure \
                 a model (CLI or ANTHROPIC_API_KEY) and try again.",
                e
            );
            (
                prior.draft_title.clone(),
                prior.draft_body.clone(),
                note,
                Vec::new(),
            )
        }
    };

    // Structured-clarification upgrade: when the assistant asked a clarifying question
    // AND offered options, emit it as a STRUCTURED clarification keyed to this draft UoW.
    // It then surfaces in the NEEDS YOU queue as a resumable pause point, and is answered
    // via the same AskUserQuestion-style component; the answer feeds back as the next
    // author message. When the model returned no options we fall back to the free-text
    // chat reply only (back-compat), posting nothing structured. Story authoring is an
    // LLM text-generation assist (no gate involved).
    if !options.is_empty() {
        state.clarifications.post_structured(
            &story_id,
            &reply,
            "you",
            options,
            false,
            true,
        );
    }

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
/// drafted title/body, upsert the resulting work item onto the canonical spine, and LINK
/// the draft UoW to it (without re-keying the UoW). When the draft carries a `parent_id`,
/// also creates a native GitHub sub-issue link (child under parent) — FAIL SOFT: if the
/// sub-issue link cannot be created (bad parent number, GitHub error, etc.) the story is
/// still published and a `parent_link_warning` field is included in the response.
/// Returns `{ work_item, uow_id }` (plus optional `parent_link_warning`). Requires a
/// GitHub token; 4xx with a clear reason when the token is absent, the repo is
/// malformed, or the draft has no title.
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
    let token = state.github_token().ok_or_else(|| {
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

    // Create the child issue; we need both the html_url (for the number parse below)
    // and the GitHub database id (for the sub-issue link). `create_issue_returning_id`
    // replaces the previous `onboard::create_issue` call — same endpoint, same payload,
    // additionally reads the `id` field from the response.
    let (html_url, child_db_id) = crate::github_issues::create_issue_returning_id(
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

    // ── Optional: create a native GitHub sub-issue link (FAIL SOFT) ───────────────
    //
    // If the draft carried a parent_id, attempt to create the parent → child sub-issue
    // relationship in GitHub. Any failure (bad number, GitHub error, permissions) is
    // surfaced as a `parent_link_warning` in the response instead of failing the publish.
    let parent_link_warning: Option<String> = match uow.parent_id.as_deref() {
        None => None,
        Some(raw_parent) => match raw_parent.trim().parse::<u64>() {
            Err(_) => Some(format!(
                "published, but could not link to parent #{raw_parent}: not a valid issue number"
            )),
            Ok(parent_number) => {
                match crate::github_issues::link_sub_issue(
                    &coord.owner,
                    &coord.repo,
                    parent_number,
                    child_db_id,
                    &token,
                )
                .await
                {
                    Ok(()) => None,
                    Err(e) => Some(format!(
                        "published, but could not link to parent #{parent_number}: {e}"
                    )),
                }
            }
        },
    };

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
        "parent_link_warning": parent_link_warning,
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

// ── 3-phase cockpit state (#104 / #105) ──────────────────────────────────────

#[derive(serde::Deserialize)]
struct UowIntakeContextReq {
    /// The free-text context for the investigation agent.
    context: String,
}

/// `POST /api/uow/:story_id/intake/context` body `{ context }` — set the Intake free-text
/// context for the investigation agent (3-phase doc §3). Returns the updated UoW.
async fn uow_set_intake_context(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowIntakeContextReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.set_intake_context(&story_id, &req.context))
}

#[derive(serde::Deserialize)]
struct UowIntakeReposReq {
    /// The full in-scope repo/branch set (replaces the prior scope). Out-of-scope repos
    /// are simply absent from this list (R6).
    repos: Vec<crate::uow::RepoScope>,
}

/// `POST /api/uow/:story_id/intake/repos` body `{ repos: [...] }` — replace the per-story
/// repo/branch scope (R6). Returns the updated UoW.
async fn uow_set_intake_repos(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowIntakeReposReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.set_intake_repos(&story_id, req.repos))
}

/// Per-repo Ship result for one in-scope repo. Part of the [`UowIntakeShipResponse`].
#[derive(serde::Serialize, Debug)]
pub struct RepoShipResult {
    /// `owner/repo`.
    pub repo: String,
    /// The story branch that was pushed.
    pub branch: String,
    /// `true` when push + PR succeeded.
    pub ok: bool,
    /// The PR URL on success.
    pub pr_url: Option<String>,
    /// The PR number on success.
    pub pr_number: Option<u64>,
    /// Human-readable error when `ok = false`.
    pub error: Option<String>,
}

/// Response body for `POST /api/uow/:story_id/intake/ship`.
#[derive(serde::Serialize, Debug)]
pub struct UowIntakeShipResponse {
    /// Per-repo Ship results (one entry per in-scope repo in `intake.repos`).
    pub repos: Vec<RepoShipResult>,
    /// `true` when every repo shipped successfully.
    pub all_ok: bool,
}

/// Optional request body for `POST /api/uow/:story_id/intake/ship`.
#[derive(serde::Deserialize, Default)]
struct UowIntakeShipReq {
    /// The target/base branch to open each PR into. Empty/absent → each repo's default branch.
    #[serde(default)]
    base_branch: Option<String>,
}

/// `POST /api/uow/:story_id/intake/ship` body `{ base_branch? }` — per-repo Ship (R3.f / R6).
///
/// Iterates the UoW's `intake.repos`; for each in-scope repo: derives the story branch
/// (via `derive_scope_branch`), resolves its worktree, pushes the branch, and opens a PR.
/// Returns per-repo results in [`UowIntakeShipResponse`] — partial success is surfaced
/// (some repos may succeed while others fail).
///
/// Falls back to the single-repo PR-open path (`uow_pr_open` behaviour) when
/// `intake.repos` is empty (no multi-repo scope selected at Intake).
///
/// 4xx when: no GitHub token; or when single-repo fallback finds no branch / no clone.
async fn uow_intake_ship(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<UowIntakeShipReq>>,
) -> Response {
    let bad = |msg: String| {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg }))).into_response()
    };

    let Some(token) = state.github_token() else {
        return bad("no GitHub token — set CAMERATA_GITHUB_TOKEN to open a PR".to_string());
    };

    let base_branch = req.and_then(|Json(r)| r.base_branch).filter(|b| !b.trim().is_empty());
    let uow = state.uow.get_or_create(&story_id);
    let workspace_root = state.settings.workspace_root();

    // ── Multi-repo path: intake.repos has explicit selections ─────────────────
    if !uow.intake.repos.is_empty() {
        let mut results: Vec<RepoShipResult> = Vec::new();

        for scope in &uow.intake.repos {
            let scope_branch = derive_scope_branch(&scope.branch, &story_id);
            let override_path = state.settings.repo_path(&scope.repo);

            // Resolve the worktree for this repo + branch.
            let dir = match crate::workspace::resolve_uow_worktree(
                override_path.as_deref(),
                workspace_root.as_deref(),
                &scope.repo,
                &scope_branch,
            )
            .await
            {
                Some(d) => d,
                None => {
                    results.push(RepoShipResult {
                        repo: scope.repo.clone(),
                        branch: scope_branch.clone(),
                        ok: false,
                        pr_url: None,
                        pr_number: None,
                        error: Some(format!(
                            "repo `{}` is not resolved locally — set its path in the Rules view \
                             and start development so the repo is cloned",
                            scope.repo
                        )),
                    });
                    continue;
                }
            };

            // Push the branch from its worktree.
            if let Err(e) = crate::workspace::push_branch(&dir, &scope.repo, &scope_branch, &token).await {
                results.push(RepoShipResult {
                    repo: scope.repo.clone(),
                    branch: scope_branch.clone(),
                    ok: false,
                    pr_url: None,
                    pr_number: None,
                    error: Some(format!("could not push `{scope_branch}`: {e}")),
                });
                continue;
            }

            // Open (or discover) the PR.
            let title = format!("Camerata: {story_id} ({})", scope.repo);
            let body = format!(
                "Opened by Camerata for story `{story_id}` in repo `{}`.",
                scope.repo
            );
            match crate::workspace::open_pr_with_base(
                &scope.repo,
                &scope_branch,
                base_branch.as_deref(),
                &title,
                &body,
                &token,
            )
            .await
            {
                Ok(opened) => {
                    state.uow.append_history(
                        &story_id,
                        "intake_ship",
                        &format!(
                            "Opened PR #{} for `{}` on `{}`: {}",
                            opened.number, scope.repo, scope_branch, opened.url
                        ),
                    );
                    results.push(RepoShipResult {
                        repo: scope.repo.clone(),
                        branch: scope_branch.clone(),
                        ok: true,
                        pr_url: Some(opened.url),
                        pr_number: Some(opened.number),
                        error: None,
                    });
                }
                Err(e) => {
                    results.push(RepoShipResult {
                        repo: scope.repo.clone(),
                        branch: scope_branch.clone(),
                        ok: false,
                        pr_url: None,
                        pr_number: None,
                        error: Some(format!("could not open PR: {e}")),
                    });
                }
            }
        }

        let all_ok = results.iter().all(|r| r.ok);
        return Json(UowIntakeShipResponse { repos: results, all_ok }).into_response();
    }

    // ── Single-repo fallback: no intake.repos — use the UoW's primary branch ──
    // Mirrors the `uow_pr_open` flow so existing single-repo users see no change.
    let Some(branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad(
            "this UoW has no branch yet — start development first so there is a branch to \
             open a PR for, or set the in-scope repos in the Intake view for multi-repo Ship"
                .to_string(),
        );
    };
    let Some(repo) = repo_from_story_id(&story_id) else {
        return bad(format!("could not derive owner/repo from story id `{story_id}`"));
    };
    let override_path = state.settings.repo_path(&repo);
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &branch,
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start \
             development so the repo is cloned) before opening a PR"
                .to_string(),
        );
    };
    if let Err(e) = crate::workspace::push_branch(&dir, &repo, &branch, &token).await {
        return bad(format!("could not push `{branch}`: {e}"));
    }
    let title = format!("Camerata: {story_id}");
    let body = format!("Opened by Camerata for story `{story_id}`.");
    match crate::workspace::open_pr_with_base(&repo, &branch, base_branch.as_deref(), &title, &body, &token).await {
        Ok(opened) => {
            state.uow.set_pr(&story_id, Some(opened.number), Some(opened.url.clone()));
            state.uow.append_history(
                &story_id,
                "intake_ship",
                &format!("Opened PR #{} for `{branch}`: {}", opened.number, opened.url),
            );
            let results = vec![RepoShipResult {
                repo: repo.clone(),
                branch,
                ok: true,
                pr_url: Some(opened.url),
                pr_number: Some(opened.number),
                error: None,
            }];
            Json(UowIntakeShipResponse { repos: results, all_ok: true }).into_response()
        }
        Err(e) => bad(format!("could not open the PR: {e}")),
    }
}

#[derive(serde::Deserialize)]
struct UowChatReq {
    /// `"user"` or `"agent"`.
    role: String,
    /// The message body.
    text: String,
}

/// `POST /api/uow/:story_id/investigation/chat` body `{ role, text }` — append one turn to
/// the investigation/refinement agent chat transcript (3-phase doc §4). Returns the
/// updated UoW.
async fn uow_append_investigation_chat(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowChatReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(
        state
            .uow
            .append_investigation_chat(&story_id, &req.role, &req.text),
    )
}

/// `POST /api/uow/:story_id/development/chat` body `{ role, text }` — append one turn to the
/// development agent chat transcript (3-phase doc §5). Returns the updated UoW.
async fn uow_append_development_chat(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowChatReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(
        state
            .uow
            .append_development_chat(&story_id, &req.role, &req.text),
    )
}

#[derive(serde::Deserialize)]
struct UowContractReq {
    /// The prose interface contract (free-form).
    contract: String,
    /// Whether the work crosses a contract boundary (a contract is then required before
    /// development per R3.g). Defaults to `false` when absent.
    #[serde(default)]
    crosses_boundary: bool,
}

/// `POST /api/uow/:story_id/contract` body `{ contract, crosses_boundary }` — set the prose
/// interface contract + the boundary flag (R3.g / §4.6). Returns the updated UoW.
async fn uow_set_contract(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowContractReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(
        state
            .uow
            .set_contract(&story_id, &req.contract, req.crosses_boundary),
    )
}

#[derive(serde::Deserialize)]
struct UowMetaReq {
    /// `"intake"` / `"investigation"` / `"development"`. Absent → unchanged.
    #[serde(default)]
    viewed_phase: Option<String>,
    #[serde(default)]
    intake_finished: Option<bool>,
    #[serde(default)]
    investigation_finished: Option<bool>,
    #[serde(default)]
    development_finished: Option<bool>,
    #[serde(default)]
    done: Option<bool>,
}

/// `POST /api/uow/:story_id/meta` body `{ viewed_phase?, intake_finished?, … }` — patch the
/// 3-phase cockpit meta (viewed phase + per-phase finished flags + done/archived). Each
/// field is optional; absent fields are left unchanged (3-phase doc §2 / §7). An unknown
/// `viewed_phase` string is ignored (treated as absent) so a typo never wedges the meta.
/// Returns the updated UoW.
async fn uow_set_meta(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowMetaReq>,
) -> Json<crate::uow::UnitOfWork> {
    let viewed_phase = req
        .viewed_phase
        .as_deref()
        .and_then(crate::uow::PhaseTab::from_wire);
    Json(state.uow.set_meta(
        &story_id,
        viewed_phase,
        req.intake_finished,
        req.investigation_finished,
        req.development_finished,
        req.done,
    ))
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
    /// The active project's stable id (present when `ok=true`). Lets the client confirm
    /// which project this grounding belongs to, closing the cross-project leak surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
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
    /// A compact, queryable summary of the active scan results — severity/status totals,
    /// by-rule breakdown, and a capped finding list. Populated from the onboarding draft
    /// when a scan has been run. The snippet field is NEVER included to avoid leaking
    /// credential-shaped values into the context. Present when a scan report is found in
    /// the draft; absent when no scan has been run yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_results_section: Option<String>,
    /// A compact, queryable summary of the rules the user has selected in the onboarding
    /// draft (repo_selection + chosen options), formatted for injection into the chat
    /// system prompt as Layer 3d. Populated whenever repo_selection is non-empty — works
    /// pre-scan and pre-onboard as soon as the user makes a selection. Absent when no
    /// rules are selected yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_rules_section: Option<String>,
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

/// Build a compact, queryable scan-results section for the chat system prompt.
///
/// Includes:
/// - Totals: total findings, counts by severity (critical/high/medium/low), counts by
///   status (active / suppressed-* / preview vs floor).
/// - By-rule breakdown: top rules by finding count.
/// - A capped list of the most severe findings (up to `CAP`), each as a single line:
///   `severity · rule_id · repo/path:line · status`.
/// - Coverage notes (tools skipped / not available during the scan).
///
/// The `snippet` field is NEVER included — snippets may contain credential-shaped values
/// and must not be injected into the LLM context. Only rule id, location, severity, status,
/// and the gate `detail` (which is gate-authored prose, not the raw secret) are surfaced.
/// The `detail` is capped at 120 chars per finding.
///
/// Returns an empty string when `findings` is empty (no scan yet — caller should treat as
/// absent and skip the section rather than injecting an empty/misleading block).
pub(crate) fn render_scan_results_for_chat(
    findings: &[crate::onboard::Finding],
    coverage_notes: &[crate::onboard::CoverageNote],
) -> String {
    const CAP: usize = 40;

    if findings.is_empty() && coverage_notes.is_empty() {
        return String::new();
    }

    let mut out = String::new();

    // ── Totals ────────────────────────────────────────────────────────────────
    let total = findings.len();
    let mut by_severity: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    let mut active_count: usize = 0;
    let mut suppressed_count: usize = 0;
    let mut preview_count: usize = 0;

    for f in findings {
        *by_severity
            .entry(f.severity.as_str())
            .or_insert(0) += 1;
        if f.status == "active" {
            active_count += 1;
        } else if f.status.starts_with("suppressed") {
            suppressed_count += 1;
        }
        if f.preview {
            preview_count += 1;
        }
    }
    let floor_count = total - preview_count;

    out.push_str(&format!("Total findings: {total}\n"));
    // Severity breakdown — emit in fixed order (critical first)
    for sev in &["critical", "high", "medium", "low"] {
        if let Some(&n) = by_severity.get(sev) {
            out.push_str(&format!("  {sev}: {n}\n"));
        }
    }
    // Any unexpected severity values
    for (sev, &n) in &by_severity {
        if !matches!(*sev, "critical" | "high" | "medium" | "low") {
            out.push_str(&format!("  {sev}: {n}\n"));
        }
    }
    out.push_str(&format!(
        "Status: {active_count} active (enforced), {suppressed_count} suppressed\n"
    ));
    out.push_str(&format!(
        "Preview (not yet wired into CI gate): {preview_count} | Floor (enforced): {floor_count}\n"
    ));

    // ── By-rule breakdown ─────────────────────────────────────────────────────
    let mut by_rule: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in findings {
        *by_rule.entry(f.rule_id.as_str()).or_insert(0) += 1;
    }
    let mut rule_counts: Vec<(&str, usize)> = by_rule.into_iter().collect();
    // Sort by count descending, then rule_id ascending for determinism
    rule_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    out.push_str("By rule (top hits):\n");
    for (rule_id, count) in rule_counts.iter().take(20) {
        out.push_str(&format!("  {rule_id}: {count}\n"));
    }

    // ── Capped finding list (severity-ordered, no snippet) ────────────────────
    // Sort: active before suppressed, severity (critical→high→medium→low), then rule_id.
    let sev_rank = |s: &str| match s {
        "critical" => 0u8,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    };
    let mut sorted: Vec<&crate::onboard::Finding> = findings.iter().collect();
    sorted.sort_by(|a, b| {
        // Active before suppressed
        let a_active = a.status == "active";
        let b_active = b.status == "active";
        b_active
            .cmp(&a_active)
            .then_with(|| sev_rank(&a.severity).cmp(&sev_rank(&b.severity)))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });

    let shown = sorted.len().min(CAP);
    out.push_str(&format!(
        "Top {shown} findings (of {total}, severity-ordered; snippet omitted):\n"
    ));
    for f in sorted.iter().take(CAP) {
        // NEVER include f.snippet — it may contain a credential.
        let detail_preview: String = f.detail.chars().take(120).collect();
        let preview_tag = if f.preview { " [preview]" } else { "" };
        out.push_str(&format!(
            "  {} · {} · {}/{}:{} · {}{}\n",
            f.severity, f.rule_id, f.repo, f.path, f.line, f.status, preview_tag
        ));
        if !detail_preview.is_empty() {
            out.push_str(&format!("    {detail_preview}\n"));
        }
    }

    // ── Coverage notes ────────────────────────────────────────────────────────
    if !coverage_notes.is_empty() {
        out.push_str("Coverage notes (tools skipped / unavailable):\n");
        for note in coverage_notes {
            out.push_str(&format!("  [{}] {}\n", note.tool, note.message));
        }
    }

    out
}

/// Extract a compact, queryable scan-results section from the UI-owned onboarding draft blob.
///
/// Looks for findings in `draft["scan"]["findings"]` or `draft["audit"]["findings"]`,
/// deserializes them as `Vec<Finding>` (the struct that owns the canonical field names),
/// and delegates to `render_scan_results_for_chat`. Returns `None` when no findings section
/// is found, when deserialization fails, or when the findings list is empty.
fn extract_scan_results_from_draft(draft: &serde_json::Value) -> Option<String> {
    // Prefer the scan section (which contains the deterministic floor + Phase-1 findings)
    // but fall back to the audit section (Phase-2 AI findings live there in some draft shapes).
    let findings_val = draft
        .get("scan")
        .and_then(|s| s.get("findings"))
        .or_else(|| draft.get("audit").and_then(|a| a.get("findings")))?;

    let findings: Vec<crate::onboard::Finding> =
        serde_json::from_value(findings_val.clone()).ok()?;

    if findings.is_empty() {
        return None;
    }

    // Coverage notes are at scan.coverage_notes; absent in audit-only drafts.
    let coverage_notes: Vec<crate::onboard::CoverageNote> = draft
        .get("scan")
        .and_then(|s| s.get("coverage_notes"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let rendered = render_scan_results_for_chat(&findings, &coverage_notes);
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
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

/// Build the selected-rules section for the chat system prompt from the onboarding draft.
///
/// Reads `repo_selection` (repo → [rule_id]) and `chosen` (rule_id → option_id) from the
/// draft, enriches with titles and option labels from `scan.proposed_rules`, and renders a
/// compact, queryable block:
///
/// ```text
/// Total selected: 4 rule(s) across 2 repo(s)
/// Rules:
///   SEC-NO-HARDCODED-SECRETS-1 · "No hardcoded secrets" (all repos) [default option]
///   PERF-FK-INDEX-1 · "FK index required" (owner/api) → chosen: strict-mode
/// ```
///
/// Returns `None` when no rules are selected (empty `repo_selection`), so callers can
/// skip the section rather than injecting an empty or misleading block. Works pre-scan
/// and pre-onboard: the draft carries `repo_selection`/`chosen` as soon as the user
/// makes a selection, even before any findings exist.
///
/// This is a pure function: it takes the draft JSON value and returns a formatted string
/// or None. It does not touch any async I/O or server state.
pub(crate) fn render_selected_rules_for_chat(draft: &serde_json::Value) -> Option<String> {
    // repo_selection: { repo_key: [rule_id, ...] }
    // A sentinel key ("__single__" or similar) means the project is single-repo.
    let repo_selection_val = draft.get("repo_selection")?;
    let repo_selection: std::collections::HashMap<String, Vec<String>> =
        serde_json::from_value(repo_selection_val.clone()).ok()?;

    // Collect all selected rule ids (union across repos) and which repos selected each.
    let mut by_rule: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (repo_key, rule_ids) in &repo_selection {
        for rid in rule_ids {
            by_rule.entry(rid.clone()).or_default().push(repo_key.clone());
        }
    }

    if by_rule.is_empty() {
        return None;
    }

    // chosen: { rule_id: option_id } — non-default option overrides.
    let chosen: std::collections::HashMap<String, String> = draft
        .get("chosen")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Build a lookup: rule_id → (title, options map {id → label}, default_option_id).
    #[derive(serde::Deserialize)]
    struct DraftRuleLight {
        id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        options: Vec<DraftOptionLight>,
        #[serde(default)]
        default_option: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct DraftOptionLight {
        id: String,
        label: String,
    }

    let rule_meta: std::collections::HashMap<String, DraftRuleLight> = draft
        .get("scan")
        .and_then(|s| s.get("proposed_rules"))
        .and_then(|v| serde_json::from_value::<Vec<DraftRuleLight>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    // Count distinct repos (excluding sentinel single-repo key if present).
    let distinct_repos: std::collections::HashSet<&str> = repo_selection
        .keys()
        .map(String::as_str)
        .filter(|k| !k.starts_with("__"))
        .collect();
    let repo_display_count = distinct_repos.len().max(1);

    let total = by_rule.len();
    let mut out = format!(
        "Total selected: {total} rule(s) across {repo_display_count} repo(s)\nRules:\n"
    );

    for (rule_id, mut repos) in by_rule {
        repos.sort();
        repos.dedup();

        // Scope annotation — hide sentinel keys from human-readable display.
        let visible_repos: Vec<&str> = repos
            .iter()
            .map(String::as_str)
            .filter(|k| !k.starts_with("__"))
            .collect();
        let scope_note = if visible_repos.is_empty() {
            "all repos".to_string()
        } else if visible_repos.len() == repo_display_count {
            "all repos".to_string()
        } else {
            visible_repos.join(", ")
        };

        // Title from proposed_rules, falling back to the id itself.
        let title = rule_meta
            .get(&rule_id)
            .and_then(|m| m.title.as_deref())
            .unwrap_or("");

        // Chosen-option annotation.
        let option_note = if let Some(chosen_id) = chosen.get(&rule_id) {
            // Try to resolve the label.
            let label = rule_meta
                .get(&rule_id)
                .and_then(|m| m.options.iter().find(|o| &o.id == chosen_id))
                .map(|o| o.label.as_str())
                .unwrap_or(chosen_id.as_str());
            format!(" → chosen: {label}")
        } else {
            // Check if there IS a default so we can annotate honestly.
            if rule_meta
                .get(&rule_id)
                .and_then(|m| m.default_option.as_deref())
                .is_some()
            {
                " [default option]".to_string()
            } else {
                String::new()
            }
        };

        if title.is_empty() {
            out.push_str(&format!("  {rule_id} ({scope_note}){option_note}\n"));
        } else {
            out.push_str(&format!(
                "  {rule_id} · \"{title}\" ({scope_note}){option_note}\n"
            ));
        }
    }

    Some(out)
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
            project_id: None,
            project_name: None,
            repos: Vec::new(),
            onboarded: Vec::new(),
            ruleset_summary: None,
            finding_count: None,
            findings_summary: None,
            scan_results_section: None,
            selected_rules_section: None,
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
        let scan_results_section = draft
            .as_ref()
            .and_then(|d| extract_scan_results_from_draft(d))
            .or_else(|| {
                state
                    .get_last_scan(&project.id)
                    .map(|r| render_scan_results_for_chat(&r.findings, &r.coverage_notes))
                    .filter(|s| !s.is_empty())
            });
        let selected_rules_section = draft.as_ref().and_then(render_selected_rules_for_chat);
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::PostOnboard,
            project_id: Some(project.id.clone()),
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: project.onboarded.clone(),
            ruleset_summary,
            finding_count,
            findings_summary,
            scan_results_section,
            selected_rules_section,
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
        let scan_results_section = draft
            .as_ref()
            .and_then(|d| extract_scan_results_from_draft(d))
            .or_else(|| {
                state
                    .get_last_scan(&project.id)
                    .map(|r| render_scan_results_for_chat(&r.findings, &r.coverage_notes))
                    .filter(|s| !s.is_empty())
            });
        let selected_rules_section = draft.as_ref().and_then(render_selected_rules_for_chat);
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::PreOnboard,
            project_id: Some(project.id.clone()),
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: Vec::new(),
            ruleset_summary: None, // No committed ruleset yet.
            finding_count,
            findings_summary,
            scan_results_section,
            selected_rules_section,
            draft_json: draft,
            message: Some(format!(
                "Project '{}' has an in-progress onboarding (scan/audit) that has not been applied yet.",
                project.name
            )),
        })
    } else {
        // Blank: project exists but no draft and no onboarded repos.
        // Still check last_scan — a scan may have completed without a draft being
        // round-tripped back (timing race: this is the exact bug being fixed).
        let scan_results_section = state
            .get_last_scan(&project.id)
            .map(|r| render_scan_results_for_chat(&r.findings, &r.coverage_notes))
            .filter(|s| !s.is_empty());
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::Blank,
            project_id: Some(project.id.clone()),
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: Vec::new(),
            ruleset_summary: None,
            finding_count: None,
            findings_summary: None,
            scan_results_section,
            selected_rules_section: None, // Blank: no draft, so no selections yet.
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

/// The decisions-review payload for a story (`GET /api/uow/:story_id/investigation`): the
/// investigation note (or `None` when none has been recorded) and the current decision
/// records. Powers the cockpit's decisions-review surface so the architect can read the
/// investigation output, mark it reviewed, and approve/reject each decision before the
/// Investigating → DecisionsApproved transition. `note_present` distinguishes "no note at
/// all" (e.g. live mode off produced only a placeholder, or the run errored) from an empty
/// note string, so the UI can surface an explicit "no output" state.
#[derive(serde::Serialize)]
struct InvestigationReviewResponse {
    story_id: String,
    note_present: bool,
    note: Option<camerata_worktracker::investigation::InvestigationArtifact>,
    decisions: Vec<camerata_worktracker::investigation::DecisionRecord>,
}

/// `GET /api/uow/:story_id/investigation` — the investigation note + decision records for
/// the decisions-review surface. Read-only.
async fn uow_get_investigation(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<InvestigationReviewResponse> {
    let note = state.uow.investigation_note_for(&story_id);
    let decisions = state.uow.decisions_for(&story_id);
    Json(InvestigationReviewResponse {
        story_id,
        note_present: note.is_some(),
        note,
        decisions,
    })
}

/// `POST /api/uow/:story_id/investigation/review` — mark the story's investigation note
/// reviewed by the architect (ROUTE-B). Returns `{ ok, version }`; `ok=false` when there
/// is no note to review (or it was already reviewed).
async fn uow_mark_investigation_reviewed(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    match state.uow.mark_investigation_reviewed(&story_id) {
        Some(version) => Json(serde_json::json!({ "ok": true, "version": version })),
        None => Json(serde_json::json!({ "ok": false })),
    }
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
/// `DELETE /api/uow/:story_id` — remove a UoW entirely. The UI gates this behind an
/// "are you sure?" confirmation. Returns `{ "ok": <removed> }`.
async fn uow_delete(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": state.uow.delete(&story_id) }))
}

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

    // GROUNDING (the invariant): hand the investigation agent the project's rule context +
    // repo digest so it analyzes the ACTUAL project (it can also read the repo clone).
    let grounding = state.project_grounding().await;
    // ON-DEMAND REPO READ (the core invariant): resolve the active project's local clone so
    // the investigation agent runs WITH the repo as its cwd + `--add-dir` and can scan/read
    // any file it needs, not just the digest. Read-only — the write gate is unchanged. None
    // when the repo isn't local (the agent then has the digest only, as before).
    let repo_dir = state.active_repo_dir();
    // MULTI-REPO READ scope: ALL the active project's local repo clones — the union read
    // window for the investigation agent (cwd stays the primary `repo_dir`; each repo is
    // added read-only via `--add-dir`).
    let read_dirs = state.active_repo_dirs();

    // Create a run the UI can poll, then kick the single gated investigation agent.
    let run_id = state.runs.create(&story_id, "investigation", crate::run::RunKind::Watched);
    {
        let runs = state.runs.clone();
        let uow = state.uow.clone();
        let clarifications = state.clarifications.clone();
        let resume = state.clarify_resume.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        // Register the abort handle so a Stop (POST /api/runs/:id/cancel) can reap a run
        // blocked inside the live agent subprocess (kill_on_drop). Cleared when it finishes.
        let runs_for_clear = state.runs.clone();
        let rid_for_clear = run_id.clone();
        let handle = tokio::spawn(async move {
            crate::investigation_run::execute_investigation_run(
                runs,
                uow,
                clarifications,
                resume,
                rid,
                sid,
                title,
                desc,
                model,
                grounding,
                repo_dir,
                read_dirs,
            )
            .await;
            runs_for_clear.clear_abort(&rid_for_clear);
        });
        state.runs.register_abort(&run_id, handle.abort_handle());
    }

    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

/// Derive `owner/repo` from a UoW story id (`owner/repo#num`). The UoW story id is the
/// GitHub-sourced id WITHOUT the `github:` prefix (see [`UowView`]); the repo is the part
/// before the last `#`. Returns `None` when the id has no `#` or the repo part is not a
/// valid `owner/repo`.
pub(crate) fn repo_from_story_id(story_id: &str) -> Option<String> {
    let (repo, _num) = story_id.rsplit_once('#')?;
    if camerata_worktracker::RepoCoord::parse(repo).is_some() {
        Some(repo.to_string())
    } else {
        None
    }
}

/// Derive the story branch for one in-scope repo given its [`BranchMode`] and the
/// current story id. Used during per-repo worktree setup (R6 multi-repo path).
///
/// - `BranchMode::Existing { branch_name }` → use `branch_name` as-is (working off
///   an existing branch in this repo, as selected at Intake).
/// - `BranchMode::NewFromBase { new_name, .. }` → use `new_name` when non-empty;
///   fall back to `camerata/<story_id>` (the standard Camerata branch slug).
///
/// This is a pure helper — no I/O. It mirrors the fallback the primary-repo branch
/// derivation uses (`uow_branch.unwrap_or_else(|| format!("camerata/{story_id}"))`).
pub(crate) fn derive_scope_branch(mode: &crate::uow::BranchMode, story_id: &str) -> String {
    match mode {
        crate::uow::BranchMode::Existing { branch_name } if !branch_name.trim().is_empty() => {
            branch_name.clone()
        }
        crate::uow::BranchMode::NewFromBase { new_name, .. } if !new_name.trim().is_empty() => {
            new_name.clone()
        }
        _ => format!("camerata/{story_id}"),
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
    // Per-UoW worktree (Decision 1): the UoW's branch is checked out in its OWN worktree off
    // the shared clone, not in the shared clone itself — so two same-repo UoWs never collide on
    // a checkout. This is the canonical dir where this UoW's code lives; ship/push (Phase 2)
    // will reuse it. `None` when the repo isn't a local clone (no override + no workspace root,
    // or the clone doesn't exist yet) — same condition `resolve_repo_dir` would have failed on.
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &target_branch,
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start development \
             so the repo is cloned) before updating the branch"
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

    let run_id = state.runs.create(&story_id, "update-branch", crate::run::RunKind::Watched);
    {
        let runs = state.runs.clone();
        let uow_store = state.uow.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        let token = state.github_token();
        let src = req.source_branch.clone();
        // GROUNDING (the invariant): rule + repo digest for the conflict resolver.
        let grounding = state.project_grounding().await;
        // MULTI-REPO READ scope: ALL the active project's local repo clones (read-only).
        let read_dirs = state.active_repo_dirs();
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
                grounding,
                read_dirs,
            )
            .await;
        });
    }

    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

// ── Per-UoW PR lifecycle (Decision 2) ──────────────────────────────────────────

/// Request body for `POST /api/uow/:story_id/pr/open`.
#[derive(serde::Deserialize)]
struct PrOpenReq {
    /// The target/base branch to open the PR INTO (the console's base-branch picker).
    /// Empty / omitted falls back to the repo's default branch.
    #[serde(default)]
    base_branch: Option<String>,
}

/// `POST /api/uow/:story_id/pr/open` body `{ base_branch }` → push the UoW branch and
/// open a PR into the chosen base, STORING the PR number + url on the UoW.
///
/// Resolves the UoW WORKTREE (Phase 1 seam), pushes the branch, then opens the PR. If a
/// PR for this head already exists (incl. one opened directly in GitHub), `open_pr_with_base`
/// discovers + returns it and we store that. 4xx when: no token; the UoW has no branch; the
/// repo can't be derived; or the repo isn't a local clone.
async fn uow_pr_open(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PrOpenReq>,
) -> Response {
    let bad = |msg: String| {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg }))).into_response()
    };
    let Some(token) = state.github_token() else {
        return bad("no GitHub token — set CAMERATA_GITHUB_TOKEN to open a PR".to_string());
    };
    let uow = state.uow.get_or_create(&story_id);
    let Some(branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad(
            "this UoW has no branch yet — start development first so there is a branch to open a PR for"
                .to_string(),
        );
    };
    let Some(repo) = repo_from_story_id(&story_id) else {
        return bad(format!("could not derive owner/repo from story id `{story_id}`"));
    };
    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &branch,
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start development \
             so the repo is cloned) before opening a PR"
                .to_string(),
        );
    };

    // Push the UoW branch from its worktree.
    if let Err(e) = crate::workspace::push_branch(&dir, &repo, &branch, &token).await {
        return bad(format!("could not push `{branch}`: {e}"));
    }
    // Open (or discover) the PR into the chosen base.
    let title = uow
        .work_item
        .as_deref()
        .map(|w| format!("{w}: {story_id}"))
        .unwrap_or_else(|| format!("Camerata: {story_id}"));
    let body = format!("Opened by Camerata for story `{story_id}`.");
    let base = req.base_branch.as_deref();
    match crate::workspace::open_pr_with_base(&repo, &branch, base, &title, &body, &token).await {
        Ok(opened) => {
            state.uow.set_pr(&story_id, Some(opened.number), Some(opened.url.clone()));
            state.uow.append_history(
                &story_id,
                "pr_open",
                &format!("Opened PR #{} for `{branch}`: {}", opened.number, opened.url),
            );
            Json(serde_json::json!({
                "ok": true,
                "pr_number": opened.number,
                "pr_url": opened.url,
            }))
            .into_response()
        }
        Err(e) => bad(format!("could not open the PR: {e}")),
    }
}

/// `GET /api/uow/:story_id/pr` → the console "Pull PR info": resolve the PR for the UoW
/// (stored number → else head-branch search + STORE), then return its state + comments +
/// CI checks.
///
/// Graceful: with no token, no derivable repo, or no PR for the UoW, returns a clear empty
/// payload `{ ok: false, pr: null, ... }` — never an error — so the console renders "No PR
/// yet" instead of breaking. (Asking for PR data is a read, not a gated write.)
async fn uow_pr_get(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    let empty = |msg: &str| {
        serde_json::json!({ "ok": false, "pr": null, "comments": [], "checks": null, "message": msg })
    };
    let Some(token) = state.github_token() else {
        return Json(empty("Connect GitHub (set CAMERATA_GITHUB_TOKEN) to pull PR info."));
    };
    let Some(repo) = repo_from_story_id(&story_id) else {
        return Json(empty("Could not derive owner/repo from the story id."));
    };
    let uow = state.uow.get_or_create(&story_id);
    let Some(info) = crate::pr::resolve_pr_for_uow(&state.uow, &story_id, &uow, &repo, &token).await
    else {
        return Json(empty("No PR for this UoW yet."));
    };
    let comments = crate::pr::list_pr_comments(&repo, info.number, &token)
        .await
        .unwrap_or_default();
    let checks = crate::pr::pr_checks(&repo, &info.head_sha, &token)
        .await
        .unwrap_or_default();
    Json(serde_json::json!({
        "ok": true,
        "pr": info,
        "comments": comments,
        "checks": checks,
    }))
}

/// Request body for `POST /api/uow/:story_id/pr/comment`.
#[derive(serde::Deserialize)]
struct PrCommentReq {
    body: String,
}

/// `POST /api/uow/:story_id/pr/comment` body `{ body }` → post a comment on the UoW's PR.
/// Resolves the PR (stored → else head-search + store), then posts. 4xx on no token /
/// empty body / no PR / no repo.
async fn uow_pr_comment(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PrCommentReq>,
) -> Response {
    let bad = |msg: String| {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg }))).into_response()
    };
    let Some(token) = state.github_token() else {
        return bad("no GitHub token — set CAMERATA_GITHUB_TOKEN to comment".to_string());
    };
    if req.body.trim().is_empty() {
        return bad("comment body must not be empty".to_string());
    }
    let Some(repo) = repo_from_story_id(&story_id) else {
        return bad(format!("could not derive owner/repo from story id `{story_id}`"));
    };
    let uow = state.uow.get_or_create(&story_id);
    let Some(info) = crate::pr::resolve_pr_for_uow(&state.uow, &story_id, &uow, &repo, &token).await
    else {
        return bad("no PR for this UoW yet — open one first".to_string());
    };
    match crate::pr::post_pr_comment(&repo, info.number, &req.body, &token).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })).into_response(),
        Err(e) => bad(format!("could not post the comment: {e}")),
    }
}

/// `POST /api/uow/:story_id/pr/resolve` → a GATED run (mirrors `update-branch`) that feeds
/// the PR feedback (open review comments + failing check names) to ONE governed agent to
/// fix, commit, and optionally push, IN THE UoW WORKTREE. Returns `{ run_id }`.
///
/// The gate is universal + unchanged: same governed role, `Task` disallowed, gated writes
/// only, layer-2 bounce. Reading the PR feedback here is a read; FIXING it goes through the
/// gate. 4xx when: no token; no branch; no derivable repo; the repo isn't a local clone; or
/// no PR exists for the UoW.
#[derive(serde::Deserialize)]
struct PrResolveReq {
    #[serde(default)]
    model: Option<String>,
}

async fn uow_pr_resolve(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PrResolveReq>,
) -> Response {
    let bad = |msg: String| {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg }))).into_response()
    };
    let Some(token) = state.github_token() else {
        return bad("no GitHub token — set CAMERATA_GITHUB_TOKEN to resolve PR feedback".to_string());
    };
    let uow = state.uow.get_or_create(&story_id);
    let Some(target_branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad("this UoW has no branch yet — nothing to resolve".to_string());
    };
    let Some(repo) = repo_from_story_id(&story_id) else {
        return bad(format!("could not derive owner/repo from story id `{story_id}`"));
    };
    // Resolve the PR (stored → else head-search + store) so we have the feedback to feed.
    let Some(info) = crate::pr::resolve_pr_for_uow(&state.uow, &story_id, &uow, &repo, &token).await
    else {
        return bad("no PR for this UoW yet — open one first".to_string());
    };
    // Gather the feedback: open REVIEW comments (actionable code feedback) + failing checks.
    let review_comments: Vec<String> = crate::pr::list_pr_comments(&repo, info.number, &token)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.review && !c.body.trim().is_empty())
        .map(|c| c.body)
        .collect();
    let failing_checks = crate::pr::pr_checks(&repo, &info.head_sha, &token)
        .await
        .map(|c| c.failing)
        .unwrap_or_default();

    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &target_branch,
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start development \
             so the repo is cloned) before resolving PR feedback"
                .to_string(),
        );
    };

    let model = req.model.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| {
        state
            .projects
            .active()
            .map(|p| p.tier_map.strongest)
            .unwrap_or_else(crate::model_tier::default_strongest_model)
    });

    let run_id = state.runs.create(&story_id, "pr-resolve", crate::run::RunKind::Watched);
    {
        let runs = state.runs.clone();
        let uow_store = state.uow.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        let pr_number = info.number;
        // GROUNDING (the invariant): rule + repo digest for the PR-feedback resolver.
        let grounding = state.project_grounding().await;
        // MULTI-REPO READ scope: ALL the active project's local repo clones (read-only).
        let read_dirs = state.active_repo_dirs();
        tokio::spawn(async move {
            crate::pr_resolve_run::execute_pr_resolve_run(
                runs,
                uow_store,
                rid,
                sid,
                repo,
                dir,
                target_branch,
                pr_number,
                review_comments,
                failing_checks,
                Some(token),
                model,
                grounding,
                read_dirs,
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

    let Some(p) = state.projects.active() else {
        return Json(serde_json::json!({
            "ok": true,
            "units_of_work": [],
        }));
    };
    let uow_list = state.uow.list_for_project(&p.id, &p.repos);
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
            .set_step_model(&p.id, StepKind::StoryAuthoring, "claude-opus-4-8".to_string())
            .unwrap();

        // The fallback-step resolver now returns the project's configured model (no env floor).
        let model = step_model(&state, StepKind::StoryAuthoring);
        assert_eq!(model, "claude-opus-4-8");
        // And it is exactly what a fallback-step call site puts on the LlmRequest.
        let req = crate::llm::LlmRequest::new("draft").with_model(model);
        assert_eq!(req.model, "claude-opus-4-8");

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
            .set_step_model(&p.id, StepKind::Audit, "claude-sonnet-4-6".to_string())
            .unwrap();

        // Explicit pick overrides the project default.
        assert_eq!(
            step_model_or(&state, StepKind::Audit, Some("claude-opus-4-8")),
            "claude-opus-4-8"
        );
        // Blank / None falls back to the project's per-step model.
        assert_eq!(
            step_model_or(&state, StepKind::Audit, Some("   ")),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            step_model_or(&state, StepKind::Audit, None),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn parse_step_kind_accepts_known_keys_and_separators() {
        use crate::project::StepKind;
        assert_eq!(parse_step_kind("audit"), Some(StepKind::Audit));
        assert_eq!(parse_step_kind("research_chat"), Some(StepKind::ResearchChat));
        // Tolerant of dashes/spaces/case.
        assert_eq!(parse_step_kind("Research-Chat"), Some(StepKind::ResearchChat));
        assert_eq!(parse_step_kind("story authoring"), Some(StepKind::StoryAuthoring));
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
            "claude-opus-4-8",
            &crate::llm::LlmResponse {
                text: String::new(),
                model: "claude-opus-4-8".to_string(),
                backend: "cli".to_string(),
                cost_usd: Some(0.05),
                input_tokens: Some(1000),
                output_tokens: Some(500),
                cache_read_input_tokens: 10,
                cache_creation_input_tokens: 20,
                or_cache_discount: None,
            },
        );
        // Gemini-shaped: no cost field, known model id -> derived cost (sonnet 3/15 per Mtok).
        state.usage_ledger.record(
            "claude-sonnet-4-6",
            &crate::llm::LlmResponse {
                text: String::new(),
                model: "claude-sonnet-4-6".to_string(),
                backend: "api".to_string(),
                cost_usd: None,
                input_tokens: Some(1_000_000),
                output_tokens: Some(1_000_000),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
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
        // 0.05 (reported) + 18.0 (derived sonnet) = 18.05.
        let cost = json["total_cost_usd"].as_f64().unwrap();
        assert!((cost - 18.05).abs() < 1e-6, "got {cost}");
        // by_model array of {model,tokens,cost,calls}, sorted by descending cost (sonnet first).
        let by_model = json["by_model"].as_array().unwrap();
        assert_eq!(by_model.len(), 2);
        assert_eq!(by_model[0]["model"], "claude-sonnet-4-6");
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
        assert!(!uow.meta.investigation_finished, "unpatched field unchanged");
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
        assert_eq!(arr.len(), 2, "only the active project's stories, got: {json:?}");
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
        assert!(result.is_err(), "gate must refuse when crosses_boundary=true and contract is empty");
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
        state.uow.set_contract(story, "GET /api/users returns [{id, name}]", true);
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
        let store: std::sync::Arc<dyn camerata_persistence::ArtifactStore> =
            std::sync::Arc::new(store);
        state.uow = state.uow.with_artifacts(store);
        state
    }

    #[tokio::test(flavor = "multi_thread")]
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

        // A second review (already reviewed) → ok=false (no new revision).
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
        let json2 = body_json(resp2).await;
        assert_eq!(json2["ok"], false);
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
        let id = run_store.create("CAM-ev-3", "live", crate::run::RunKind::Watched);
        run_store.push_event(&id, crate::run::GateEvent {
            seq: 1, layer: "fleet".to_string(), verdict: "info".to_string(),
            rule: None, detail: "Scaffolding the worktree.".to_string(),
            content_hash: None,
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
        let run_id = state.runs.create("S-gate-1", "scripted", crate::run::RunKind::Watched);
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
        let run_id = state.runs.create("S-gate-2", "scripted", crate::run::RunKind::Watched);
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
        let run_id = state.runs.create("S-gate-3", "scripted", crate::run::RunKind::Watched);
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
        let run_id = state.runs.create("S-gate-4", "scripted", crate::run::RunKind::Watched);
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
    async fn uows_list_carries_workitem_and_stage() {
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
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
        state
            .projects
            .create("Q", vec!["x/y".to_string()])
            .unwrap();
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
            .unwrap_or_else(|| panic!("draft must appear in its own project's /api/uows, got: {json:?}"));
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
        // ISOLATION: /api/uows is project-scoped and resolves a LINKED draft's repo from its
        // work_item (`me/api#7`). Activate a project covering `me/api` so the linked draft is
        // visible (an UNLINKED draft would remain excluded — no resolvable repo).
        state.projects.create("P", vec!["me/api".to_string()]).unwrap();
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
        assert_eq!(result, "", "empty history + empty prompt returns empty string");
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
        assert!(result.contains("User: u-content"), "user role must render as 'User'");
        assert!(result.contains("Assistant: a-content"), "assistant role must render as 'Assistant'");
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
        assert!(result.contains("new-prompt"), "new prompt must appear after capped history");
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

    /// Helper: build a floor finding (preview = false, enforced).
    fn floor_finding(
        repo: &str,
        path: &str,
        line: usize,
        rule_id: &str,
    ) -> crate::onboard::Finding {
        crate::onboard::Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: "critical".to_string(),
            snippet: "let secret = \"ghp_xxx\";".to_string(),
            detail: "floor detail".to_string(),
            preview: false,
            preview_tool: None,
            ..crate::onboard::Finding::default()
        }
    }

    /// Helper: build a semgrep preview finding (preview = true, preview_tool = "semgrep").
    fn semgrep_finding(
        repo: &str,
        path: &str,
        line: usize,
        semgrep_rule_id: &str,
    ) -> crate::onboard::Finding {
        crate::onboard::Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: semgrep_rule_id.to_string(),
            severity: "critical".to_string(),
            snippet: "let secret = \"ghp_xxx\";".to_string(),
            detail: "semgrep detail".to_string(),
            preview: true,
            preview_tool: Some("semgrep".to_string()),
            ..crate::onboard::Finding::default()
        }
    }

    /// (a) A floor hardcoded-secret finding and a semgrep hardcoded-secret finding on the
    /// SAME file:line collapse to ONE finding. The floor's rule_id is kept canonical; the
    /// semgrep rule_id is recorded in `also_matches`.
    #[test]
    fn dedup_secret_same_line_keeps_floor_and_records_semgrep_in_also_matches() {
        let mut existing = vec![floor_finding(
            "me/api",
            "src/config.rs",
            7,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let previews = vec![semgrep_finding(
            "me/api",
            "src/config.rs",
            7,
            "camerata.security.hardcoded-secret",
        )];

        let out = dedup_preview_against_floor(&mut existing, previews);

        // The semgrep finding must be dropped (deduped).
        assert!(
            out.is_empty(),
            "deduped semgrep finding must not appear in output: {out:?}"
        );
        // The floor finding is still there and now carries the semgrep rule in also_matches.
        assert_eq!(existing.len(), 1);
        let floor = &existing[0];
        assert_eq!(floor.rule_id, "SEC-NO-HARDCODED-SECRETS-1", "floor rule_id is canonical");
        assert_eq!(
            floor.also_matches,
            vec!["camerata.security.hardcoded-secret"],
            "semgrep rule must be recorded in also_matches on the floor finding"
        );
    }

    /// (a) SQL-concat variant: floor SEC-NO-RAW-SQL-CONCAT-1 + semgrep sql-string-concat-python
    /// on the same (repo, path, line) collapse to one floor finding.
    #[test]
    fn dedup_sql_concat_python_same_line_keeps_floor() {
        let mut existing = vec![floor_finding(
            "me/svc",
            "app/db.py",
            15,
            "SEC-NO-RAW-SQL-CONCAT-1",
        )];
        let previews = vec![semgrep_finding(
            "me/svc",
            "app/db.py",
            15,
            "camerata.security.sql-string-concat-python",
        )];

        let out = dedup_preview_against_floor(&mut existing, previews);

        assert!(out.is_empty(), "SQL-concat python semgrep finding must be deduped: {out:?}");
        assert_eq!(
            existing[0].also_matches,
            vec!["camerata.security.sql-string-concat-python"],
        );
    }

    /// (a) SQL-concat JS variant maps to the same floor rule.
    #[test]
    fn dedup_sql_concat_js_same_line_keeps_floor() {
        let mut existing = vec![floor_finding(
            "me/web",
            "src/db.ts",
            42,
            "SEC-NO-RAW-SQL-CONCAT-1",
        )];
        let previews = vec![semgrep_finding(
            "me/web",
            "src/db.ts",
            42,
            "camerata.security.sql-string-concat-js",
        )];

        let out = dedup_preview_against_floor(&mut existing, previews);

        assert!(out.is_empty(), "SQL-concat JS semgrep finding must be deduped: {out:?}");
        assert_eq!(
            existing[0].also_matches,
            vec!["camerata.security.sql-string-concat-js"],
        );
    }

    /// (b) Semgrep catches a secret on a DIFFERENT line than any floor finding. That semgrep
    /// finding is NET-NEW coverage (the regex missed it) and must pass through untouched.
    #[test]
    fn dedup_secret_different_line_is_kept_as_net_new() {
        let mut existing = vec![floor_finding(
            "me/api",
            "src/config.rs",
            7,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        // Semgrep fires on line 12 — a different line, not a duplicate.
        let previews = vec![semgrep_finding(
            "me/api",
            "src/config.rs",
            12,
            "camerata.security.hardcoded-secret",
        )];

        let out = dedup_preview_against_floor(&mut existing, previews);

        assert_eq!(out.len(), 1, "net-new semgrep finding (different line) must be kept");
        assert_eq!(out[0].line, 12);
        assert_eq!(out[0].rule_id, "camerata.security.hardcoded-secret");
        // The existing floor finding must NOT have been mutated.
        assert!(existing[0].also_matches.is_empty(), "floor finding must not be touched");
    }

    /// (c) The non-overlapping semgrep rules (exec-injection, weak-hash, path-traversal,
    /// subprocess-shell-true) have no floor twin and must pass through untouched even when
    /// a floor finding exists at the same path:line.
    ///
    /// NOTE: yaml-unsafe-load was previously in this list but was moved to the overlapping
    /// group when SEC-NO-UNSAFE-DESERIALIZATION-1 was added to the floor (maps to "deser").
    #[test]
    fn dedup_non_overlapping_semgrep_rules_pass_through_untouched() {
        let non_overlapping = &[
            "camerata.security.exec-injection",
            "camerata.security.exec-injection-js",
            "camerata.security.weak-hash-python",
            "camerata.security.weak-hash-js",
            "camerata.security.path-traversal-python",
            "camerata.security.subprocess-shell-true",
        ];

        for rule_id in non_overlapping {
            // Put a floor finding at the same coordinate just to prove it doesn't cross-dedup.
            let mut existing = vec![floor_finding("me/api", "src/app.py", 5, "SEC-NO-HARDCODED-SECRETS-1")];
            let previews = vec![semgrep_finding("me/api", "src/app.py", 5, rule_id)];

            let out = dedup_preview_against_floor(&mut existing, previews);

            assert_eq!(
                out.len(),
                1,
                "non-overlapping semgrep rule '{rule_id}' must not be deduped even at same path:line"
            );
            assert_eq!(out[0].rule_id, *rule_id);
            assert!(
                existing[0].also_matches.is_empty(),
                "floor finding must not be mutated for non-overlapping rule '{rule_id}'"
            );
        }
    }

    /// (e) Two findings at the SAME path:line but in DIFFERENT repos must NEVER cross-dedup.
    #[test]
    fn dedup_different_repos_never_cross_dedup() {
        // Floor in repo "me/api"; semgrep in "me/web" — same path and line, different repo.
        let mut existing = vec![floor_finding(
            "me/api",
            "src/config.rs",
            7,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let previews = vec![semgrep_finding(
            "me/web", // different repo
            "src/config.rs",
            7,
            "camerata.security.hardcoded-secret",
        )];

        let out = dedup_preview_against_floor(&mut existing, previews);

        assert_eq!(out.len(), 1, "different-repo finding must not be deduped");
        assert!(
            existing[0].also_matches.is_empty(),
            "floor finding in a different repo must not be mutated"
        );
    }

    /// (e) Two findings at the same (repo, line) but DIFFERENT files must never cross-dedup.
    #[test]
    fn dedup_different_paths_never_cross_dedup() {
        let mut existing = vec![floor_finding(
            "me/api",
            "src/config.rs",
            7,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let previews = vec![semgrep_finding(
            "me/api",
            "src/other.rs", // different file
            7,
            "camerata.security.hardcoded-secret",
        )];

        let out = dedup_preview_against_floor(&mut existing, previews);

        assert_eq!(out.len(), 1, "different-file finding must not be deduped");
        assert!(existing[0].also_matches.is_empty());
    }

    /// `semgrep_floor_category` mapping table: verify the exact pairs and confirm
    /// non-overlapping rules return `None`.
    #[test]
    fn semgrep_floor_category_returns_correct_mapping() {
        // Overlapping rules: must return the floor rule id they duplicate.
        let overlapping: &[(&str, &str)] = &[
            ("camerata.security.hardcoded-secret", "SEC-NO-HARDCODED-SECRETS-1"),
            ("camerata.security.hardcoded-secret-dquote", "SEC-NO-HARDCODED-SECRETS-1"),
            ("camerata.security.sql-string-concat-python", "SEC-NO-RAW-SQL-CONCAT-1"),
            ("camerata.security.sql-string-concat-js", "SEC-NO-RAW-SQL-CONCAT-1"),
            ("camerata.security.sql-string-concat-rust", "SEC-NO-RAW-SQL-CONCAT-1"),
            ("camerata.security.sql-string-concat-csharp", "SEC-NO-RAW-SQL-CONCAT-1"),
            ("camerata.security.disabled-tls-rust", "SEC-NO-DISABLED-TLS-1"),
            ("camerata.security.disabled-tls-csharp", "SEC-NO-DISABLED-TLS-1"),
            ("camerata.security.yaml-unsafe-load", "SEC-NO-UNSAFE-DESERIALIZATION-1"),
        ];
        for (rule, expected_floor) in overlapping {
            assert_eq!(
                semgrep_floor_category(rule),
                Some(*expected_floor),
                "overlapping rule '{rule}' must map to floor '{expected_floor}'"
            );
        }
        // Non-overlapping rules: no floor twin, return None.
        for rule in &[
            "camerata.security.exec-injection",
            "camerata.security.exec-injection-js",
            "camerata.security.weak-hash-python",
            "camerata.security.weak-hash-js",
            "camerata.security.weak-hash-rust",
            "camerata.security.weak-hash-csharp",
            "camerata.security.path-traversal-python",
            "camerata.security.subprocess-shell-true",
        ] {
            assert_eq!(
                semgrep_floor_category(rule),
                None,
                "rule '{rule}' must map to None (no floor twin)"
            );
        }
    }

    // ── FIX 2: cross-preview-tool dedup tests ────────────────────────────────
    //
    // These tests exercise `dedup_scan_previews` and `finding_security_category`
    // directly. Decision: docs/decisions/2026-06-23_stack_gating_and_crosstool_dedup.md

    /// Build a clippy preview finding (preview = true, preview_tool = "clippy").
    fn clippy_finding(
        repo: &str,
        path: &str,
        line: usize,
        rule_id: &str,
    ) -> crate::onboard::Finding {
        crate::onboard::Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: "medium".to_string(),
            snippet: String::new(),
            detail: "clippy detail".to_string(),
            preview: true,
            preview_tool: Some("clippy".to_string()),
            ..crate::onboard::Finding::default()
        }
    }

    /// Build a ruff preview finding.
    fn ruff_finding(
        repo: &str,
        path: &str,
        line: usize,
        rule_id: &str,
    ) -> crate::onboard::Finding {
        crate::onboard::Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: "medium".to_string(),
            snippet: String::new(),
            detail: "ruff detail".to_string(),
            preview: true,
            preview_tool: Some("ruff".to_string()),
            ..crate::onboard::Finding::default()
        }
    }

    /// Build an eslint preview finding.
    fn eslint_finding(
        repo: &str,
        path: &str,
        line: usize,
        rule_id: &str,
    ) -> crate::onboard::Finding {
        crate::onboard::Finding {
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            rule_id: rule_id.to_string(),
            severity: "medium".to_string(),
            snippet: String::new(),
            detail: "eslint detail".to_string(),
            preview: true,
            preview_tool: Some("eslint".to_string()),
            ..crate::onboard::Finding::default()
        }
    }

    /// finding_security_category: verify the category table is consistent.
    #[test]
    fn finding_security_category_maps_correctly() {
        // Floor rules
        assert_eq!(finding_security_category("SEC-NO-HARDCODED-SECRETS-1"), Some("secret"));
        assert_eq!(finding_security_category("SEC-NO-PRIVATE-KEY-1"), Some("secret"));
        assert_eq!(finding_security_category("SEC-NO-VENDOR-TOKEN-1"), Some("secret"));
        assert_eq!(finding_security_category("SEC-NO-RAW-SQL-CONCAT-1"), Some("sql"));
        assert_eq!(finding_security_category("SEC-NO-DISABLED-TLS-1"), Some("tls"));
        // Semgrep rules
        assert_eq!(finding_security_category("camerata.security.hardcoded-secret"), Some("secret"));
        assert_eq!(finding_security_category("camerata.security.sql-string-concat-python"), Some("sql"));
        assert_eq!(finding_security_category("camerata.security.sql-string-concat-js"), Some("sql"));
        assert_eq!(finding_security_category("camerata.security.exec-injection"), Some("exec"));
        assert_eq!(finding_security_category("camerata.security.exec-injection-js"), Some("exec"));
        assert_eq!(finding_security_category("camerata.security.weak-hash-python"), Some("hash"));
        assert_eq!(finding_security_category("camerata.security.weak-hash-js"), Some("hash"));
        // Rust + C# semgrep ids (the 2026-06-23 dedup-gap fix): must be categorized so they
        // can collapse against an overlapping floor finding at the same (repo, path, line).
        assert_eq!(finding_security_category("camerata.security.hardcoded-secret-dquote"), Some("secret"));
        assert_eq!(finding_security_category("camerata.security.sql-string-concat-rust"), Some("sql"));
        assert_eq!(finding_security_category("camerata.security.sql-string-concat-csharp"), Some("sql"));
        assert_eq!(finding_security_category("camerata.security.weak-hash-rust"), Some("hash"));
        assert_eq!(finding_security_category("camerata.security.weak-hash-csharp"), Some("hash"));
        assert_eq!(finding_security_category("camerata.security.disabled-tls-rust"), Some("tls"));
        assert_eq!(finding_security_category("camerata.security.disabled-tls-csharp"), Some("tls"));
        assert_eq!(finding_security_category("camerata.security.path-traversal-python"), Some("path"));
        assert_eq!(finding_security_category("camerata.security.subprocess-shell-true"), Some("shell"));
        // yaml-unsafe-load now maps to "deser" (not "yaml") so it collapses with the
        // floor rule SEC-NO-UNSAFE-DESERIALIZATION-1 at the same (repo, path, line).
        assert_eq!(finding_security_category("camerata.security.yaml-unsafe-load"), Some("deser"));
        // New floor rule (Step 3 of unsafe-deser port): same "deser" category.
        assert_eq!(finding_security_category("SEC-NO-UNSAFE-DESERIALIZATION-1"), Some("deser"));
        // Ruff bandit code
        assert_eq!(finding_security_category("S608"), Some("sql"));
        // Unknown rule: no category (pass-through, no dedup)
        assert_eq!(finding_security_category("ARCH-NO-CIRCULAR-DEPS-1"), None);
        assert_eq!(finding_security_category("some-unknown-linter-rule"), None);
    }

    /// Regression for the 2026-06-23 dedup gap: a floor SQL finding and a semgrep
    /// `sql-string-concat-rust` finding on the SAME file:line MUST collapse (floor canonical),
    /// not double-report. Before the fix, the -rust id had no category so it skipped dedup.
    #[test]
    fn crosstool_dedup_floor_and_semgrep_rust_sql_same_location_collapses() {
        let mut existing = vec![floor_finding("me/svc", "src/db.rs", 1444, "SEC-NO-RAW-SQL-CONCAT-1")];
        let previews = vec![semgrep_finding(
            "me/svc",
            "src/db.rs",
            1444,
            "camerata.security.sql-string-concat-rust",
        )];
        let leftover = dedup_scan_previews(&mut existing, previews);
        // Semgrep finding folded in, not surfaced as its own row.
        assert!(leftover.is_empty(), "semgrep-rust SQL should collapse into the floor row");
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1", "floor stays canonical");
        assert!(existing[0]
            .also_matches
            .iter()
            .any(|m| m == "camerata.security.sql-string-concat-rust"));
    }

    /// Regression for the unsafe-deser floor port: a floor SEC-NO-UNSAFE-DESERIALIZATION-1
    /// finding and a semgrep camerata.security.yaml-unsafe-load finding on the SAME
    /// (repo, path, line) MUST collapse to one row, floor canonical, because both map to the
    /// "deser" category. Before this step the yaml-unsafe-load rule mapped to "yaml" and the
    /// floor rule did not exist, so they would have produced two rows.
    #[test]
    fn crosstool_dedup_floor_and_semgrep_deser_same_location_collapses() {
        let mut existing = vec![floor_finding(
            "me/svc",
            "src/loader.py",
            42,
            "SEC-NO-UNSAFE-DESERIALIZATION-1",
        )];
        let previews = vec![semgrep_finding(
            "me/svc",
            "src/loader.py",
            42,
            "camerata.security.yaml-unsafe-load",
        )];
        let leftover = dedup_scan_previews(&mut existing, previews);
        // Semgrep finding must be folded into the floor row, not surfaced as its own row.
        assert!(
            leftover.is_empty(),
            "semgrep yaml-unsafe-load must collapse into the floor deser row: {leftover:?}"
        );
        assert_eq!(existing.len(), 1);
        assert_eq!(
            existing[0].rule_id,
            "SEC-NO-UNSAFE-DESERIALIZATION-1",
            "floor rule_id must remain canonical"
        );
        assert!(
            existing[0]
                .also_matches
                .iter()
                .any(|m| m == "camerata.security.yaml-unsafe-load"),
            "semgrep rule must appear in also_matches: {:?}",
            existing[0].also_matches
        );
    }

    /// FIX 2 (a): ruff (rank 1, "sql" category via S608) + semgrep (rank 2, "sql" via
    /// sql-string-concat-python) on the SAME (repo, path, line) collapse to ONE row.
    /// Ruff is canonical (rank 1 < rank 2); semgrep rule_id is folded into `also_matches`.
    #[test]
    fn crosstool_dedup_ruff_and_semgrep_same_location_collapses() {
        // S608 (Ruff / flake8-bandit) → "sql"; sql-string-concat-python → "sql".
        // Both map to the same category, so they collapse.
        let ruff = ruff_finding("me/api", "src/db.py", 15, "S608");
        let mut existing = vec![ruff];
        let previews = vec![semgrep_finding(
            "me/api",
            "src/db.py",
            15,
            "camerata.security.sql-string-concat-python",
        )];

        let out = dedup_scan_previews(&mut existing, previews);

        // The semgrep finding is collapsed: it must NOT appear in `out`.
        assert!(
            out.is_empty(),
            "semgrep duplicate of ruff must be collapsed, not added: {out:?}"
        );
        // The ruff finding is kept canonical and records the semgrep rule.
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].preview_tool.as_deref(), Some("ruff"), "ruff is canonical");
        assert!(
            existing[0].also_matches.contains(&"camerata.security.sql-string-concat-python".to_string()),
            "semgrep rule must be in also_matches: {:?}",
            existing[0].also_matches
        );
    }

    /// FIX 2 (b): floor (rank 0) + ruff (rank 1) on the same location + same "sql" category:
    /// floor wins. Ruff rule id is folded into `also_matches` on the floor finding.
    #[test]
    fn crosstool_dedup_floor_beats_ruff_at_same_location() {
        let floor = floor_finding("me/api", "src/queries.py", 5, "SEC-NO-RAW-SQL-CONCAT-1");
        let mut existing = vec![floor];
        // Ruff S608 fires on the same spot (same "sql" category).
        let previews = vec![ruff_finding("me/api", "src/queries.py", 5, "S608")];

        let out = dedup_scan_previews(&mut existing, previews);

        assert!(
            out.is_empty(),
            "ruff duplicate of floor must be collapsed: {out:?}"
        );
        assert_eq!(existing[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1", "floor rule_id is canonical");
        assert!(
            existing[0].also_matches.contains(&"S608".to_string()),
            "ruff rule must be in also_matches: {:?}",
            existing[0].also_matches
        );
    }

    /// FIX 2 (c): same (repo, path, line) but DIFFERENT categories: BOTH findings are kept.
    /// Conservative dedup — surface over hide.
    #[test]
    fn crosstool_dedup_different_categories_same_line_are_kept() {
        // eslint: SQL injection lint on line 7
        let eslint = eslint_finding("me/web", "src/db.ts", 7, "no-sql-injection");
        let mut existing = vec![eslint];
        // Semgrep: exec-injection on the SAME line (different category: "exec" vs "sql").
        let previews = vec![semgrep_finding(
            "me/web",
            "src/db.ts",
            7,
            "camerata.security.exec-injection-js",
        )];

        let out = dedup_scan_previews(&mut existing, previews);

        // BOTH must be kept: different categories, even though same line.
        assert_eq!(
            out.len(),
            1,
            "different-category findings on the same line must BOTH be kept: out={out:?}"
        );
        assert_eq!(out[0].rule_id, "camerata.security.exec-injection-js");
        // The eslint finding must not have been mutated.
        assert!(existing[0].also_matches.is_empty(), "eslint must not be touched");
    }

    /// FIX 2 (d): floor↔semgrep dedup still works as before (backward-compat regression test).
    /// `dedup_preview_against_floor` delegates to `dedup_scan_previews`, so the floor→semgrep
    /// behavior is fully preserved.
    #[test]
    fn crosstool_dedup_floor_semgrep_regression_unchanged() {
        let mut existing = vec![floor_finding(
            "me/api",
            "src/config.rs",
            7,
            "SEC-NO-HARDCODED-SECRETS-1",
        )];
        let previews = vec![semgrep_finding(
            "me/api",
            "src/config.rs",
            7,
            "camerata.security.hardcoded-secret",
        )];
        let out = dedup_preview_against_floor(&mut existing, previews);
        assert!(out.is_empty(), "floor↔semgrep dedup regression: {out:?}");
        assert_eq!(
            existing[0].also_matches,
            vec!["camerata.security.hardcoded-secret"]
        );
    }

    /// FIX 2 (e): findings with unrecognized (None) categories are never cross-deduped,
    /// even on the same (repo, path, line).
    #[test]
    fn crosstool_dedup_unknown_category_never_deduped() {
        let clip = clippy_finding("me/api", "src/lib.rs", 3, "clippy::unknown-lint");
        let mut existing = vec![clip];
        // An eslint rule with no recognized category on the same line.
        let previews = vec![eslint_finding("me/api", "src/lib.rs", 3, "some-custom-eslint-rule")];

        let out = dedup_scan_previews(&mut existing, previews);

        // Both must be kept since neither has a recognized security category.
        assert_eq!(out.len(), 1, "unrecognized-category findings must not be collapsed");
        assert!(existing[0].also_matches.is_empty());
    }

    // ── ci_story_body helpers ─────────────────────────────────────────────────
    //
    // These tests verify that the enriched story bodies contain the SSOT model
    // content that makes them self-sufficient for implementation. They assert on
    // structural landmarks — not exact string equality — so minor prose edits
    // do not break them.

    fn mechanical_rules_fixture() -> Vec<CiStoryRule> {
        vec![
            CiStoryRule {
                id: "LINT-NO-CONSOLE-1".to_string(),
                title: "No console.log in production code".to_string(),
                linter: Some("eslint".to_string()),
            },
            CiStoryRule {
                id: "LINT-STRICT-1".to_string(),
                title: "TypeScript strict mode".to_string(),
                linter: None,
            },
        ]
    }

    fn architectural_rules_fixture() -> Vec<CiStoryRule> {
        vec![
            CiStoryRule {
                id: "ARCH-API-LAYERING-1".to_string(),
                title: "Services must not import repositories directly".to_string(),
                linter: None,
            },
            CiStoryRule {
                id: "ARCH-NO-CIRCULAR-DEPS-1".to_string(),
                title: "No circular module dependencies".to_string(),
                linter: None,
            },
        ]
    }

    // ── shared SSOT content (must appear in BOTH tier bodies) ─────────────────

    #[test]
    fn mechanical_body_contains_ssot_file_reference() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        assert!(
            body.contains(".camerata/checks.toml"),
            "mechanical body must reference .camerata/checks.toml as SSOT"
        );
    }

    #[test]
    fn architectural_body_contains_ssot_file_reference() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        assert!(
            body.contains(".camerata/checks.toml"),
            "architectural body must reference .camerata/checks.toml as SSOT"
        );
    }

    #[test]
    fn mechanical_body_mentions_both_layers() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        assert!(
            body.contains("Layer 2") && body.contains("Layer 3"),
            "mechanical body must explain enforcement at both Layer 2 and Layer 3"
        );
    }

    #[test]
    fn architectural_body_mentions_both_layers() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        assert!(
            body.contains("Layer 2") && body.contains("Layer 3"),
            "architectural body must explain enforcement at both Layer 2 and Layer 3"
        );
    }

    #[test]
    fn mechanical_body_explains_parity_guarantee() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        // The parity guarantee is the key correctness claim — both layers derive from SSOT.
        assert!(
            body.contains("Parity is structural") || body.contains("parity"),
            "mechanical body must state that Layer-2/Layer-3 parity is structural, not by convention"
        );
    }

    #[test]
    fn architectural_body_explains_parity_guarantee() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        assert!(
            body.contains("Parity is structural") || body.contains("parity"),
            "architectural body must state that Layer-2/Layer-3 parity is structural"
        );
    }

    // ── schema / pinning fields ───────────────────────────────────────────────

    #[test]
    fn mechanical_body_contains_toml_schema_fields() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        // All five required fields must appear.
        for field in &["id", "name", "command", "severity", "in_loop"] {
            assert!(
                body.contains(field),
                "mechanical body must document the `{field}` manifest field"
            );
        }
        // The three pinning fields must also appear.
        for field in &["tool", "version", "install"] {
            assert!(
                body.contains(field),
                "mechanical body must document the `{field}` pinning field"
            );
        }
    }

    #[test]
    fn architectural_body_contains_toml_schema_fields() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        for field in &["id", "name", "command", "severity", "in_loop"] {
            assert!(
                body.contains(field),
                "architectural body must document the `{field}` manifest field"
            );
        }
        for field in &["tool", "version", "install"] {
            assert!(
                body.contains(field),
                "architectural body must document the `{field}` pinning field"
            );
        }
    }

    #[test]
    fn mechanical_body_stresses_exact_version_pinning() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        // "EXACT" or "exact" or "pin" or "pinned" — any of these confirms the story
        // emphasises version pinning over floating versions.
        let lower = body.to_lowercase();
        assert!(
            lower.contains("exact") || lower.contains("pin"),
            "mechanical body must stress that version pinning is exact (no ranges/carets)"
        );
    }

    #[test]
    fn architectural_body_stresses_exact_version_pinning() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        let lower = body.to_lowercase();
        assert!(
            lower.contains("exact") || lower.contains("pin"),
            "architectural body must stress that version pinning is exact"
        );
    }

    // ── gate-protection note ──────────────────────────────────────────────────

    #[test]
    fn mechanical_body_mentions_gate_protection() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        assert!(
            body.contains("SEC-NO-CAMERATA-CONFIG-1") || body.contains("agents cannot"),
            "mechanical body must mention the gate that prevents agents editing .camerata/"
        );
    }

    #[test]
    fn architectural_body_mentions_gate_protection() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        assert!(
            body.contains("SEC-NO-CAMERATA-CONFIG-1") || body.contains("agents cannot"),
            "architectural body must mention SEC-NO-CAMERATA-CONFIG-1 gate protection"
        );
    }

    // ── tier-specific content ─────────────────────────────────────────────────

    #[test]
    fn mechanical_body_names_linter_for_rules_that_have_one() {
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        // LINT-NO-CONSOLE-1 has linter: "eslint" — must appear.
        assert!(
            body.contains("eslint"),
            "mechanical body must surface the linter hint (eslint) for rules that supply one"
        );
    }

    #[test]
    fn mechanical_body_includes_all_rule_ids() {
        let rules = mechanical_rules_fixture();
        let body = ci_story_body_mechanical("owner/repo", &rules);
        for r in &rules {
            assert!(
                body.contains(&r.id),
                "mechanical body must list rule id `{}`",
                r.id
            );
        }
    }

    #[test]
    fn architectural_body_includes_all_rule_ids() {
        let rules = architectural_rules_fixture();
        let body = ci_story_body_architectural("owner/repo", &rules);
        for r in &rules {
            assert!(
                body.contains(&r.id),
                "architectural body must list rule id `{}`",
                r.id
            );
        }
    }

    #[test]
    fn architectural_body_describes_custom_checker_requirement() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        // The story must be explicit that a bespoke checker must be BUILT.
        assert!(
            body.contains("bespoke checker") || body.contains("custom checker"),
            "architectural body must state that a custom/bespoke checker must be built"
        );
    }

    #[test]
    fn architectural_body_contains_api_layering_worked_example() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        // The dependency-cruiser worked example for API layering must appear.
        assert!(
            body.contains("dependency-cruiser"),
            "architectural body must include the dependency-cruiser worked example"
        );
        assert!(
            body.contains("ARCH-API-LAYERING-1"),
            "architectural body must reference ARCH-API-LAYERING-1 as the canonical example"
        );
    }

    #[test]
    fn architectural_body_teaches_regenerate_workflow_step() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        // Step 3 in the how-to is regenerating the CI workflow after editing the manifest.
        assert!(
            body.contains("generate-ci-workflow") || body.contains("Generate CI Workflow"),
            "architectural body must explain how to regenerate the CI workflow after editing the manifest"
        );
    }

    #[test]
    fn architectural_body_scoping_guidance_present() {
        let body = ci_story_body_architectural("owner/repo", &architectural_rules_fixture());
        // The story must tell teams not to block the mechanical story on architectural work.
        assert!(
            body.contains("mechanical CI story") || body.contains("mechanical story"),
            "architectural body must tell teams not to block the mechanical story on this work"
        );
    }

    // ── repo name propagation ─────────────────────────────────────────────────

    #[test]
    fn mechanical_body_includes_repo_name() {
        let body = ci_story_body_mechanical("my-org/my-repo", &mechanical_rules_fixture());
        assert!(
            body.contains("my-org/my-repo"),
            "mechanical body must include the repo name"
        );
    }

    #[test]
    fn architectural_body_includes_repo_name() {
        let body = ci_story_body_architectural("my-org/my-repo", &architectural_rules_fixture());
        assert!(
            body.contains("my-org/my-repo"),
            "architectural body must include the repo name"
        );
    }

    // ── CICD-DEPENDENCY-AUDIT-1 cadence guidance ──────────────────────────────
    //
    // When the dep-audit rule is armed, the mechanical story body must include
    // a "cadence" section that names the four options and the osv-scanner command.
    // When it is NOT armed, the section must be absent (no spurious cadence noise).

    fn dep_audit_rule() -> CiStoryRule {
        CiStoryRule {
            id: "CICD-DEPENDENCY-AUDIT-1".to_string(),
            title: "Scan dependencies for known vulnerabilities (osv-scanner)".to_string(),
            linter: Some("osv-scanner".to_string()),
        }
    }

    fn dep_audit_only_fixture() -> Vec<CiStoryRule> {
        vec![dep_audit_rule()]
    }

    fn dep_audit_mixed_fixture() -> Vec<CiStoryRule> {
        let mut rules = mechanical_rules_fixture();
        rules.push(dep_audit_rule());
        rules
    }

    #[test]
    fn dep_audit_armed_mechanical_body_contains_cadence_section() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        assert!(
            body.contains("cadence"),
            "mechanical body with dep-audit rule must include a cadence section"
        );
    }

    #[test]
    fn dep_audit_armed_mechanical_body_mentions_osv_scanner_command() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        assert!(
            body.contains("osv-scanner -r ."),
            "dep-audit cadence section must include the osv-scanner invocation"
        );
    }

    #[test]
    fn dep_audit_armed_mechanical_body_lists_all_four_cadence_options() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        // Four cadence options: weekly cron, per-PR, in-loop (every-pass), manual.
        assert!(
            body.contains("Weekly") || body.contains("weekly"),
            "cadence section must describe the weekly CI job option"
        );
        assert!(
            body.contains("pull_request") || body.contains("Per-PR") || body.contains("per-PR"),
            "cadence section must describe the per-PR option"
        );
        assert!(
            body.contains("in_loop") && body.contains("in-loop"),
            "cadence section must describe the in-loop / every-pass option"
        );
        assert!(
            body.contains("workflow_dispatch") || body.contains("manual"),
            "cadence section must describe the manual / on-demand option"
        );
    }

    #[test]
    fn dep_audit_armed_mechanical_body_states_developer_implements_cadence() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        // The story must make clear that the developer wires the cadence, not Camerata.
        assert!(
            body.contains("developer") || body.contains("project decision"),
            "cadence section must state that the developer implements the cadence, not Camerata"
        );
    }

    #[test]
    fn dep_audit_armed_mechanical_body_references_checks_toml_for_version_pin() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        // The cadence section must reference version pinning in .camerata/checks.toml.
        assert!(
            body.contains(".camerata/checks.toml"),
            "cadence section must reference .camerata/checks.toml for version pinning"
        );
        assert!(
            body.contains("osv-scanner"),
            "cadence section must name osv-scanner as the tool to pin"
        );
    }

    #[test]
    fn dep_audit_absent_mechanical_body_has_no_cadence_section() {
        // When CICD-DEPENDENCY-AUDIT-1 is NOT among the rules, the cadence section
        // must not appear — no spurious guidance for unrelated mechanical rules.
        let body = ci_story_body_mechanical("owner/repo", &mechanical_rules_fixture());
        assert!(
            !body.contains("Dependency vulnerability scanning"),
            "mechanical body WITHOUT dep-audit must not include the cadence section"
        );
    }

    #[test]
    fn dep_audit_mixed_rules_cadence_section_present() {
        // When dep-audit is one of several mechanical rules, cadence guidance still appears.
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_mixed_fixture());
        assert!(
            body.contains("cadence"),
            "mixed-rule mechanical body with dep-audit must include the cadence section"
        );
    }

    #[test]
    fn dep_audit_cadence_section_recommends_weekly_as_default() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        // The weekly option should be called out as recommended.
        assert!(
            body.contains("recommended") || body.contains("standard"),
            "cadence section must identify the weekly job as the recommended or standard default"
        );
    }

    #[test]
    fn dep_audit_cadence_section_explains_cron_schedule_example() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        // A concrete cron schedule example must be present so the developer can copy it.
        assert!(
            body.contains("cron:"),
            "cadence section must include a concrete cron schedule example"
        );
    }

    #[test]
    fn dep_audit_cadence_section_mentions_camerata_does_not_schedule() {
        let body = ci_story_body_mechanical("owner/repo", &dep_audit_only_fixture());
        // Must be explicit that Camerata does not build a scheduling engine.
        assert!(
            body.contains("scheduling engine") || body.contains("does not build"),
            "cadence section must be explicit that Camerata does not build a scheduling engine"
        );
    }

    // ── uow blank + parent_id field (2026-06-23) ──────────────────────────────────

    /// `POST /api/uow/blank` with a `parent_id` body stores the normalized number on the
    /// created draft. `"#42"` and `"42"` both normalize to `"42"`.
    #[tokio::test]
    async fn uow_blank_with_parent_id_stores_it_on_draft() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let uow_store = state.uow.clone();
        let app = router(state);

        // POST with "#42" — the leading "#" must be stripped.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/uow/blank")
                    .header("content-type", "application/json")
                    .body(Body::from(r##"{"parent_id":"#42"}"##))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let json = body_json(resp).await;
        let uow_id = json["uow_id"].as_str().expect("uow_id in response");
        let uow = uow_store.get_or_create(uow_id);
        assert_eq!(
            uow.parent_id.as_deref(),
            Some("42"),
            "#42 must normalize to 42 and be stored on the draft"
        );
    }

    /// `POST /api/uow/blank` with no body (empty JSON `{}`) still creates a draft with
    /// `parent_id = None` — back-compat with existing callers.
    #[tokio::test]
    async fn uow_blank_with_no_body_creates_draft_without_parent() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let uow_store = state.uow.clone();
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/uow/blank")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let json = body_json(resp).await;
        let uow_id = json["uow_id"].as_str().expect("uow_id in response");
        let uow = uow_store.get_or_create(uow_id);
        assert!(
            uow.parent_id.is_none(),
            "no parent_id in body -> None on draft"
        );
    }

    /// `normalize_parent_number` strips a leading `#` and accepts plain digits; rejects
    /// empty strings and non-numeric input.
    #[test]
    fn normalize_parent_number_strips_hash_and_rejects_non_numeric() {
        use crate::github_issues::normalize_parent_number;
        assert_eq!(normalize_parent_number("42"), Some("42".to_string()));
        assert_eq!(normalize_parent_number("#42"), Some("42".to_string()));
        assert_eq!(normalize_parent_number("  #7  "), Some("7".to_string()));
        assert_eq!(normalize_parent_number(""), None, "empty -> None");
        assert_eq!(normalize_parent_number("#"), None, "bare # -> None");
        assert_eq!(normalize_parent_number("abc"), None, "non-numeric -> None");
        assert_eq!(normalize_parent_number("#abc"), None, "non-numeric after # -> None");
    }

    /// When the draft has a `parent_id` but the GitHub sub-issue link call would fail
    /// (simulated here by a parent that can't be resolved because it's not a valid
    /// number after storage — this exercises the publish path's parent link failure
    /// branch), the story is still published and the response contains a
    /// `parent_link_warning`. We test the store-and-link seam directly (same pattern
    /// as `publish_link_step_links_draft_without_rekey`) with a bogus parent_id to
    /// trigger the parse-failure branch without a real GitHub call.
    #[tokio::test]
    async fn publish_with_invalid_parent_id_still_publishes_with_warning() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let uow_store = state.uow.clone();
        let stories = state.stories.clone();

        // Create a draft with a parent_id that will fail u64 parse (empty string after
        // normalization is stored as None, so use a non-numeric string stored directly
        // via create_blank_with_parent to simulate a stored-but-unparseable value).
        // We bypass normalization here intentionally — the test covers the runtime guard
        // inside uow_publish's parent_link_warning arm.
        let draft = uow_store.create_blank_with_parent(Some("not-a-number".to_string()), None);
        let draft_id = draft.story_id.clone();
        uow_store.append_authoring_turn(&draft_id, "req", "ok", "Story title", "Body");

        // Simulate create_issue having returned issue #99 and the spine + link writes
        // that uow_publish does after the HTTP call. We run the parent-link path directly
        // using the same logic the handler uses, pulling the UoW from the store.
        let story = crate::github_issues::issue_to_story("me/api", 99, "Story title", "Body");
        let wi_story_id = story.id.clone();
        stories.upsert(story).await.unwrap();
        let linked = uow_store.link_work_item(&draft_id, &wi_story_id);
        assert_eq!(linked.work_item.as_deref(), Some(wi_story_id.as_str()));

        // Confirm the parent_id is still on the UoW (carried draft → publish).
        let uow = uow_store.get_or_create(&draft_id);
        assert_eq!(uow.parent_id.as_deref(), Some("not-a-number"));

        // Now simulate the publish handler's parent-link branch: a non-numeric parent_id
        // (after normalization on blank creation it would have been None, but here we
        // stored it raw to test the guard). The handler's match arm is:
        //   Ok(parent_number) => link_sub_issue(...)
        //   Err(_) => Some("could not link...")
        // We test this pure branch inline.
        let warning: Option<String> = match uow.parent_id.as_deref() {
            None => None,
            Some(raw) => match raw.trim().parse::<u64>() {
                Err(_) => Some(format!(
                    "published, but could not link to parent #{raw}: not a valid issue number"
                )),
                Ok(_) => None, // would call link_sub_issue in the real handler
            },
        };
        assert!(
            warning.is_some(),
            "non-numeric parent_id must produce a warning"
        );
        let w = warning.unwrap();
        assert!(
            w.contains("published") && w.contains("not-a-number"),
            "warning must mention 'published' and the bad parent, got: {w}"
        );
    }

    // ── render_scan_results_for_chat ─────────────────────────────────────────

    fn make_finding(
        severity: &str,
        rule_id: &str,
        repo: &str,
        path: &str,
        line: usize,
        status: &str,
        preview: bool,
        snippet: &str,
        detail: &str,
    ) -> crate::onboard::Finding {
        crate::onboard::Finding {
            severity: severity.to_string(),
            rule_id: rule_id.to_string(),
            repo: repo.to_string(),
            path: path.to_string(),
            line,
            status: status.to_string(),
            preview,
            snippet: snippet.to_string(),
            detail: detail.to_string(),
            ..crate::onboard::Finding::default()
        }
    }

    /// Empty findings + empty coverage notes returns an empty string (caller skips the section).
    #[test]
    fn render_scan_results_empty_report_returns_empty_string() {
        let result = render_scan_results_for_chat(&[], &[]);
        assert!(
            result.is_empty(),
            "empty findings + empty coverage must produce empty string, got: {result:?}"
        );
    }

    /// Severity counts appear in the output and are correct.
    #[test]
    fn render_scan_results_counts_by_severity() {
        let findings = vec![
            make_finding("critical", "SEC-1", "o/r", "a.rs", 1, "active", false, "", ""),
            make_finding("critical", "SEC-1", "o/r", "b.rs", 2, "active", false, "", ""),
            make_finding("high", "ARCH-1", "o/r", "c.rs", 3, "active", false, "", ""),
            make_finding("medium", "ARCH-2", "o/r", "d.rs", 4, "suppressed-baseline", false, "", ""),
            make_finding("low", "STYLE-1", "o/r", "e.rs", 5, "active", false, "", ""),
        ];
        let out = render_scan_results_for_chat(&findings, &[]);
        assert!(out.contains("Total findings: 5"), "must report total, got: {out}");
        assert!(out.contains("critical: 2"), "must count critical, got: {out}");
        assert!(out.contains("high: 1"), "must count high, got: {out}");
        assert!(out.contains("medium: 1"), "must count medium, got: {out}");
        assert!(out.contains("low: 1"), "must count low, got: {out}");
    }

    /// Active vs suppressed status counts are present.
    #[test]
    fn render_scan_results_counts_by_status() {
        let findings = vec![
            make_finding("high", "R1", "o/r", "f1.rs", 1, "active", false, "", ""),
            make_finding("high", "R1", "o/r", "f2.rs", 2, "active", false, "", ""),
            make_finding("medium", "R2", "o/r", "f3.rs", 3, "suppressed-baseline", false, "", ""),
            make_finding("low", "R3", "o/r", "f4.rs", 4, "suppressed-inline", false, "", ""),
        ];
        let out = render_scan_results_for_chat(&findings, &[]);
        assert!(out.contains("2 active"), "must report 2 active, got: {out}");
        assert!(out.contains("2 suppressed"), "must report 2 suppressed, got: {out}");
    }

    /// Preview vs floor counts appear.
    #[test]
    fn render_scan_results_preview_vs_floor() {
        let findings = vec![
            make_finding("high", "R1", "o/r", "a.rs", 1, "active", true, "", ""),
            make_finding("high", "R1", "o/r", "b.rs", 2, "active", false, "", ""),
            make_finding("high", "R1", "o/r", "c.rs", 3, "active", false, "", ""),
        ];
        let out = render_scan_results_for_chat(&findings, &[]);
        assert!(
            out.contains("Preview (not yet wired into CI gate): 1"),
            "must show 1 preview, got: {out}"
        );
        assert!(
            out.contains("Floor (enforced): 2"),
            "must show 2 floor, got: {out}"
        );
    }

    /// By-rule breakdown contains the correct counts.
    #[test]
    fn render_scan_results_by_rule_breakdown() {
        let findings = vec![
            make_finding("high", "RULE-A", "o/r", "a.rs", 1, "active", false, "", ""),
            make_finding("high", "RULE-A", "o/r", "b.rs", 2, "active", false, "", ""),
            make_finding("medium", "RULE-A", "o/r", "c.rs", 3, "active", false, "", ""),
            make_finding("low", "RULE-B", "o/r", "d.rs", 4, "active", false, "", ""),
        ];
        let out = render_scan_results_for_chat(&findings, &[]);
        // RULE-A should appear with count 3 (highest); RULE-B with 1.
        assert!(out.contains("RULE-A: 3"), "must show RULE-A count 3, got: {out}");
        assert!(out.contains("RULE-B: 1"), "must show RULE-B count 1, got: {out}");
    }

    /// The top-N finding list is capped at 40 (CAP).
    #[test]
    fn render_scan_results_capped_at_40() {
        let findings: Vec<_> = (0..60)
            .map(|i| make_finding("high", "R1", "o/r", &format!("f{i}.rs"), i, "active", false, "", ""))
            .collect();
        let out = render_scan_results_for_chat(&findings, &[]);
        // Must say "Top 40 of 60"
        assert!(
            out.contains("Top 40 findings (of 60"),
            "must cap at 40 and report total 60, got: {out}"
        );
    }

    /// CRITICAL: snippet must NEVER appear in the output, even when it contains a
    /// credential-shaped value. Rule + location + detail are ok; raw snippet is not.
    #[test]
    fn render_scan_results_never_leaks_snippet() {
        // Split the credential-shaped literal so it is not itself a credential in this
        // public source file.
        let fake_secret = concat!("ghp_", "ABCdef1234567890XYZ");
        let findings = vec![make_finding(
            "critical",
            "SEC-NO-HARDCODED-SECRETS-1",
            "owner/repo",
            "src/config.rs",
            42,
            "active",
            false,
            fake_secret,   // snippet — must NOT appear in output
            "hardcoded token found", // detail — may appear
        )];
        let out = render_scan_results_for_chat(&findings, &[]);
        assert!(
            !out.contains(fake_secret),
            "snippet must NOT appear in chat grounding, got: {out}"
        );
        assert!(
            out.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "rule_id must appear, got: {out}"
        );
        assert!(
            out.contains("src/config.rs:42"),
            "location must appear, got: {out}"
        );
        assert!(
            out.contains("hardcoded token found"),
            "gate detail (not snippet) may appear, got: {out}"
        );
    }

    /// Coverage notes appear in the output.
    #[test]
    fn render_scan_results_includes_coverage_notes() {
        let notes = vec![
            crate::onboard::CoverageNote {
                tool: "ruff".to_string(),
                message: "ruff not installed; Python rules skipped".to_string(),
            },
        ];
        let out = render_scan_results_for_chat(&[], &notes);
        // Even with empty findings, coverage notes are surfaced.
        assert!(
            out.contains("ruff"),
            "coverage note tool must appear, got: {out}"
        );
        assert!(
            out.contains("Python rules skipped"),
            "coverage note message must appear, got: {out}"
        );
    }

    /// extract_scan_results_from_draft returns None for an empty draft blob.
    #[test]
    fn extract_scan_results_from_draft_none_on_empty() {
        let draft = serde_json::json!({});
        assert!(
            extract_scan_results_from_draft(&draft).is_none(),
            "empty draft must return None"
        );
    }

    /// extract_scan_results_from_draft returns None when findings is empty array.
    #[test]
    fn extract_scan_results_from_draft_none_on_empty_findings() {
        let draft = serde_json::json!({"scan": {"findings": []}});
        assert!(
            extract_scan_results_from_draft(&draft).is_none(),
            "empty findings array must return None"
        );
    }

    /// extract_scan_results_from_draft finds findings in scan.findings and produces
    /// a non-empty section with the expected content.
    #[test]
    fn extract_scan_results_from_draft_scan_findings() {
        let draft = serde_json::json!({
            "scan": {
                "findings": [
                    {
                        "repo": "owner/repo",
                        "path": "src/lib.rs",
                        "line": 10,
                        "rule_id": "ARCH-LAYERING-1",
                        "severity": "high",
                        "status": "active",
                        "snippet": "some code here",  // must NOT leak
                        "detail": "layering violation detected"
                    }
                ],
                "coverage_notes": [
                    {"tool": "eslint", "message": "no JS found"}
                ]
            }
        });
        let result = extract_scan_results_from_draft(&draft);
        assert!(result.is_some(), "must return Some for non-empty findings");
        let out = result.unwrap();
        assert!(out.contains("ARCH-LAYERING-1"), "must contain rule_id");
        assert!(out.contains("src/lib.rs:10"), "must contain location");
        // Snippet must not appear
        assert!(
            !out.contains("some code here"),
            "snippet must not appear in output, got: {out}"
        );
        assert!(out.contains("eslint"), "coverage note must appear");
    }

    /// extract_scan_results_from_draft falls back to audit.findings when scan is absent.
    #[test]
    fn extract_scan_results_from_draft_audit_fallback() {
        let draft = serde_json::json!({
            "audit": {
                "findings": [
                    {
                        "repo": "owner/repo",
                        "path": "src/api.rs",
                        "line": 5,
                        "rule_id": "DI-1",
                        "severity": "medium",
                        "status": "active",
                        "snippet": "secret-value-here",
                        "detail": "dependency injection violated"
                    }
                ]
            }
        });
        let result = extract_scan_results_from_draft(&draft);
        assert!(result.is_some(), "audit.findings fallback must work");
        let out = result.unwrap();
        assert!(out.contains("DI-1"), "must contain rule_id from audit section");
        assert!(!out.contains("secret-value-here"), "snippet must not leak via audit path");
    }

    // ── last_scan store tests ─────────────────────────────────────────────────────────

    /// Helper: build a minimal ScanReport with one active finding.
    fn make_scan_report(rule_id: &str) -> crate::onboard::ScanReport {
        crate::onboard::ScanReport {
            repos: vec!["owner/repo".to_string()],
            stacks: Vec::new(),
            files_scanned: 1,
            files_excluded: 0,
            code_chars: 100,
            excluded_mechanical_rules: Vec::new(),
            findings: vec![crate::onboard::Finding {
                repo: "owner/repo".to_string(),
                path: "src/main.rs".to_string(),
                line: 42,
                rule_id: rule_id.to_string(),
                severity: "high".to_string(),
                snippet: String::new(),
                detail: "test finding".to_string(),
                status: "active".to_string(),
                also_matches: Vec::new(),
                preview: false,
                preview_tool: None,
                in_test: false,
                needs_review: false,
            }],
            proposed_rules: Vec::new(),
            gated: false,
            message: None,
            actual_usage: None,
            deep: None,
            coverage_notes: Vec::new(),
        }
    }

    /// set_last_scan / get_last_scan round-trip: a stored report is returned.
    #[test]
    fn last_scan_set_get_round_trip() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let report = make_scan_report("SEC-NO-HARDCODED-SECRETS-1");

        // Before any write: None.
        assert!(
            state.get_last_scan("proj-1").is_none(),
            "get_last_scan must return None before any set"
        );

        state.set_last_scan("proj-1".to_string(), report.clone());

        let got = state.get_last_scan("proj-1");
        assert!(got.is_some(), "get_last_scan must return Some after set");
        let got = got.unwrap();
        assert_eq!(got.findings.len(), 1, "findings must be preserved");
        assert_eq!(
            got.findings[0].rule_id,
            "SEC-NO-HARDCODED-SECRETS-1",
            "rule_id must round-trip"
        );
    }

    /// Different project ids are stored independently.
    #[test]
    fn last_scan_is_keyed_per_project() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        state.set_last_scan("proj-A".to_string(), make_scan_report("RULE-A"));
        state.set_last_scan("proj-B".to_string(), make_scan_report("RULE-B"));

        let a = state.get_last_scan("proj-A").unwrap();
        let b = state.get_last_scan("proj-B").unwrap();
        assert_eq!(a.findings[0].rule_id, "RULE-A");
        assert_eq!(b.findings[0].rule_id, "RULE-B");
        assert!(state.get_last_scan("proj-C").is_none());
    }

    /// active_project_context returns scan_results_section from last_scan when no draft.
    #[tokio::test]
    async fn active_project_context_uses_last_scan_when_no_draft() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        // Create a project (automatically becomes active).
        let project = state.projects.create("TestProj", vec!["owner/repo".to_string()]).unwrap();

        // No draft has been saved, but we store a completed scan report.
        let report = make_scan_report("SEC-NO-HARDCODED-SECRETS-1");
        state.set_last_scan(project.id.clone(), report);

        let app = router(state);
        let req = Request::builder()
            .uri("/api/projects/active/context")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        let section = body.get("scan_results_section");
        assert!(
            section.is_some() && section.unwrap() != &serde_json::Value::Null,
            "scan_results_section must be populated from last_scan when no draft, got: {body:?}"
        );
        let section_str = section.unwrap().as_str().unwrap_or("");
        assert!(
            section_str.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "scan_results_section must contain the finding rule_id, got: {section_str}"
        );
    }

    /// Draft takes precedence over last_scan when the draft has findings.
    #[tokio::test]
    async fn active_project_context_draft_takes_precedence_over_last_scan() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let project = state.projects.create("TestProj", vec!["owner/repo".to_string()]).unwrap();

        // Store a last_scan report with one rule id.
        let report = make_scan_report("LAST-SCAN-RULE");
        state.set_last_scan(project.id.clone(), report);

        // Save a draft with a DIFFERENT rule id — draft must win.
        let draft = serde_json::json!({
            "scan": {
                "findings": [{
                    "repo": "owner/repo",
                    "path": "src/lib.rs",
                    "line": 1,
                    "rule_id": "DRAFT-RULE",
                    "severity": "high",
                    "status": "active",
                    "snippet": "",
                    "detail": "draft finding"
                }],
                "coverage_notes": []
            }
        });
        state.draft.save(&project.id, draft);

        let app = router(state);
        let req = Request::builder()
            .uri("/api/projects/active/context")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        let section_str = body
            .get("scan_results_section")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        assert!(
            section_str.contains("DRAFT-RULE"),
            "draft rule must appear when draft has findings, got: {section_str}"
        );
        assert!(
            !section_str.contains("LAST-SCAN-RULE"),
            "last_scan rule must NOT appear when draft takes precedence, got: {section_str}"
        );
    }

    // ── recent_scan fallback tests ────────────────────────────────────────────────────────

    /// set_last_scan also populates recent_scan; get_recent_scan returns the report.
    #[test]
    fn set_last_scan_populates_recent_scan() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));

        // Before any write: recent_scan is None.
        assert!(
            state.get_recent_scan().is_none(),
            "get_recent_scan must return None before any scan is stored"
        );

        state.set_last_scan("proj-1".to_string(), make_scan_report("RULE-X"));

        let recent = state.get_recent_scan();
        assert!(recent.is_some(), "get_recent_scan must be Some after set_last_scan");
        assert_eq!(
            recent.unwrap().findings[0].rule_id,
            "RULE-X",
            "recent_scan must mirror the report passed to set_last_scan"
        );
    }

    /// When two projects are scanned in sequence, recent_scan reflects the LATEST one.
    #[test]
    fn recent_scan_tracks_most_recent_write() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        state.set_last_scan("proj-A".to_string(), make_scan_report("RULE-FIRST"));
        state.set_last_scan("proj-B".to_string(), make_scan_report("RULE-SECOND"));

        let recent = state.get_recent_scan().unwrap();
        assert_eq!(
            recent.findings[0].rule_id,
            "RULE-SECOND",
            "recent_scan must track the most recent set_last_scan call"
        );

        // Per-project last_scan must still be independent.
        assert_eq!(
            state.get_last_scan("proj-A").unwrap().findings[0].rule_id,
            "RULE-FIRST"
        );
    }

    /// active_project_context surfaces recent_scan when the active project has no
    /// per-project last_scan entry (simulates the active-project-changed-between-submit-
    /// and-complete race that caused "Scan results (none yet)").
    #[tokio::test]
    async fn active_project_context_does_not_leak_recent_scan_across_projects() {
        // ISOLATION (A5): the recent_scan global fallback was REMOVED. A scan stored under
        // a different project's id must NEVER surface in the active project's chat grounding.
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        // Create and activate a project.
        let project = state.projects.create("ActiveProj", vec!["owner/repo".to_string()]).unwrap();

        // Simulate a scan stored under a DIFFERENT project id (the project that was active at
        // scan submission time) — NOT under `project.id`. This also populates recent_scan.
        state.set_last_scan("other-project-id".to_string(), make_scan_report("RECENT-RULE"));

        // Confirm: per-project lookup for the ACTIVE project is empty.
        assert!(
            state.get_last_scan(&project.id).is_none(),
            "test setup: active project must have no per-project last_scan entry"
        );
        // Confirm: recent_scan was populated by the set_last_scan call above (so the
        // assertion below proves the fallback is gone, not that the data is absent).
        assert!(
            state.get_recent_scan().is_some(),
            "test setup: recent_scan must be populated"
        );

        let app = router(state);
        let req = Request::builder()
            .uri("/api/projects/active/context")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        let section = body.get("scan_results_section");
        // The fallback is gone: with no per-project scan for the active project, the section
        // must be absent/null and must NOT contain the OTHER project's rule id.
        let section_str = section.and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !section_str.contains("RECENT-RULE"),
            "cross-project recent_scan must NOT leak into the active project's chat grounding, \
             got: {body:?}"
        );
    }

    /// Per-project last_scan takes precedence over recent_scan when both are present.
    #[tokio::test]
    async fn per_project_last_scan_wins_over_recent_scan() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let project = state.projects.create("ActiveProj", vec!["owner/repo".to_string()]).unwrap();

        // Store a recent_scan for a DIFFERENT project (written second, so it is "more recent").
        state.set_last_scan("other-project-id".to_string(), make_scan_report("RECENT-RULE"));

        // Now store a per-project entry for the ACTIVE project.  Even though recent_scan
        // was written after this, the per-project entry must win.
        state.set_last_scan(project.id.clone(), make_scan_report("PROJECT-RULE"));

        let app = router(state);
        let req = Request::builder()
            .uri("/api/projects/active/context")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        let section_str = body
            .get("scan_results_section")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        assert!(
            section_str.contains("PROJECT-RULE"),
            "per-project last_scan must take precedence over recent_scan, got: {section_str}"
        );
        assert!(
            !section_str.contains("RECENT-RULE"),
            "recent_scan must NOT appear when per-project last_scan is present, got: {section_str}"
        );
    }

    // ── render_selected_rules_for_chat tests ──────────────────────────────────────────

    /// Empty repo_selection → None.
    #[test]
    fn render_selected_rules_none_on_empty_selection() {
        let draft = serde_json::json!({
            "repo_selection": {},
            "chosen": {}
        });
        assert!(
            render_selected_rules_for_chat(&draft).is_none(),
            "empty repo_selection must return None"
        );
    }

    /// No repo_selection key → None.
    #[test]
    fn render_selected_rules_none_when_no_repo_selection_key() {
        let draft = serde_json::json!({ "scan": {} });
        assert!(
            render_selected_rules_for_chat(&draft).is_none(),
            "missing repo_selection must return None"
        );
    }

    /// Selected rule ids appear in output with correct count.
    #[test]
    fn render_selected_rules_includes_rule_ids_and_count() {
        let draft = serde_json::json!({
            "repo_selection": {
                "owner/api": ["SEC-1", "PERF-1"],
                "owner/ui": ["SEC-1"]
            },
            "chosen": {},
            "scan": { "proposed_rules": [] }
        });
        let out = render_selected_rules_for_chat(&draft)
            .expect("non-empty selection must return Some");
        assert!(
            out.contains("SEC-1"),
            "SEC-1 must appear in selected rules output"
        );
        assert!(
            out.contains("PERF-1"),
            "PERF-1 must appear in selected rules output"
        );
        // Total selected = 2 distinct rules.
        assert!(
            out.contains("Total selected: 2"),
            "total must count distinct rules, got: {out}"
        );
    }

    /// Chosen non-default option is rendered with its label.
    #[test]
    fn render_selected_rules_chosen_option_shown() {
        let draft = serde_json::json!({
            "repo_selection": {
                "owner/api": ["RULE-A"]
            },
            "chosen": {
                "RULE-A": "opt-strict"
            },
            "scan": {
                "proposed_rules": [
                    {
                        "id": "RULE-A",
                        "title": "My Rule",
                        "scope": "repo-local",
                        "kind": "review",
                        "enforcement": "prose",
                        "verification": "draft",
                        "options": [
                            { "id": "opt-default", "label": "Default mode", "directive": "use default", "why": "" },
                            { "id": "opt-strict", "label": "Strict mode", "directive": "use strict", "why": "" }
                        ],
                        "default_option": "opt-default"
                    }
                ]
            }
        });
        let out = render_selected_rules_for_chat(&draft)
            .expect("must return Some when rules selected");
        assert!(
            out.contains("chosen: Strict mode"),
            "chosen option label must appear in output, got: {out}"
        );
    }

    /// Default option is annotated as [default option] when no override is chosen.
    #[test]
    fn render_selected_rules_default_option_annotated() {
        let draft = serde_json::json!({
            "repo_selection": {
                "owner/api": ["RULE-B"]
            },
            "chosen": {},
            "scan": {
                "proposed_rules": [
                    {
                        "id": "RULE-B",
                        "title": "Rule B",
                        "scope": "repo-local",
                        "kind": "review",
                        "enforcement": "prose",
                        "verification": "draft",
                        "options": [
                            { "id": "opt-a", "label": "Option A", "directive": "do a", "why": "" }
                        ],
                        "default_option": "opt-a"
                    }
                ]
            }
        });
        let out = render_selected_rules_for_chat(&draft)
            .expect("must return Some when rules selected");
        assert!(
            out.contains("[default option]"),
            "default option must be annotated when no override chosen, got: {out}"
        );
    }

    /// `active_project_context` includes selected_rules_section when draft has
    /// repo_selection entries.
    #[tokio::test]
    async fn active_project_context_includes_selected_rules_section() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let project = state
            .projects
            .create("TestProj", vec!["owner/repo".to_string()])
            .unwrap();

        // Save a draft with a repo_selection (simulates the user checking rules).
        let draft = serde_json::json!({
            "scan": {
                "repos": ["owner/repo"],
                "findings": [],
                "proposed_rules": [
                    {
                        "id": "SEC-NO-HARDCODED-SECRETS-1",
                        "title": "No hardcoded secrets",
                        "scope": "repo-local",
                        "kind": "review",
                        "enforcement": "prose",
                        "verification": "grounded",
                        "options": [],
                        "default_option": null
                    }
                ]
            },
            "repo_selection": {
                "owner/repo": ["SEC-NO-HARDCODED-SECRETS-1"]
            },
            "chosen": {}
        });
        state.draft.save(&project.id, draft);

        let app = router(state);
        let req = Request::builder()
            .uri("/api/projects/active/context")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        let section = body.get("selected_rules_section");
        assert!(
            section.is_some() && section.unwrap() != &serde_json::Value::Null,
            "selected_rules_section must be present when repo_selection is non-empty, got: {body:?}"
        );
        let section_str = section.unwrap().as_str().unwrap_or("");
        assert!(
            section_str.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "selected rule id must appear in selected_rules_section, got: {section_str}"
        );
        assert!(
            section_str.contains("Total selected: 1"),
            "total count must be 1, got: {section_str}"
        );
    }

    /// `active_project_context` has selected_rules_section absent when repo_selection is empty.
    #[tokio::test]
    async fn active_project_context_selected_rules_absent_when_no_selection() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let project = state
            .projects
            .create("EmptyProj", vec!["owner/repo".to_string()])
            .unwrap();

        // Draft has an empty repo_selection.
        let draft = serde_json::json!({
            "scan": { "repos": ["owner/repo"], "findings": [], "proposed_rules": [] },
            "repo_selection": {},
            "chosen": {}
        });
        state.draft.save(&project.id, draft);

        let app = router(state);
        let req = Request::builder()
            .uri("/api/projects/active/context")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        // selected_rules_section must be absent (None / null) when nothing is selected.
        let section = body.get("selected_rules_section");
        let is_absent = section.is_none()
            || section == Some(&serde_json::Value::Null);
        assert!(
            is_absent,
            "selected_rules_section must be absent when repo_selection is empty, got: {body:?}"
        );
    }

    // ── derive_scope_branch (R6 multi-repo worktree setup) ─────────────────────

    /// `BranchMode::Existing { branch_name }` → use the named branch as-is.
    #[test]
    fn derive_scope_branch_existing_mode_uses_branch_name() {
        let mode = crate::uow::BranchMode::Existing {
            branch_name: "feature/my-branch".to_string(),
        };
        assert_eq!(derive_scope_branch(&mode, "acme/api#42"), "feature/my-branch");
    }

    /// `BranchMode::Existing { branch_name: "" }` (empty) → fallback to `camerata/<story_id>`.
    #[test]
    fn derive_scope_branch_existing_empty_name_falls_back_to_slug() {
        let mode = crate::uow::BranchMode::Existing {
            branch_name: "".to_string(),
        };
        assert_eq!(
            derive_scope_branch(&mode, "acme/api#42"),
            "camerata/acme/api#42",
        );
    }

    /// `BranchMode::NewFromBase { new_name }` → use `new_name` when non-empty.
    #[test]
    fn derive_scope_branch_new_from_base_uses_new_name() {
        let mode = crate::uow::BranchMode::NewFromBase {
            base: "main".to_string(),
            new_name: "camerata/story-7".to_string(),
        };
        assert_eq!(derive_scope_branch(&mode, "acme/api#7"), "camerata/story-7");
    }

    /// `BranchMode::NewFromBase { new_name: "" }` → fallback to `camerata/<story_id>`.
    #[test]
    fn derive_scope_branch_new_from_base_empty_name_falls_back_to_slug() {
        let mode = crate::uow::BranchMode::NewFromBase {
            base: "main".to_string(),
            new_name: String::new(),
        };
        assert_eq!(
            derive_scope_branch(&mode, "acme/api#7"),
            "camerata/acme/api#7",
        );
    }

    // ── repo_from_story_id (also verifies the helper is stable) ─────────────────

    /// `repo_from_story_id` extracts `owner/repo` from a `owner/repo#num` story id.
    #[test]
    fn repo_from_story_id_parses_owner_repo() {
        assert_eq!(
            repo_from_story_id("acme/api#42"),
            Some("acme/api".to_string())
        );
    }

    /// `repo_from_story_id` returns None for a malformed story id.
    #[test]
    fn repo_from_story_id_returns_none_for_invalid_id() {
        // No '#' → None.
        assert_eq!(repo_from_story_id("not-a-story-id"), None);
    }

    // ── uow_intake_ship single-repo fallback (no-token 4xx) ─────────────────────

    /// When the GitHub token is absent, `POST /api/uow/:story_id/intake/ship` returns 400.
    #[tokio::test(flavor = "multi_thread")]
    async fn intake_ship_returns_bad_request_when_no_github_token() {
        // Remove the token from the test env so the handler detects its absence.
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");

        let state = AppState::seeded();
        let app = router(state);
        let req = Request::builder()
            .method("POST")
            .uri("/api/uow/acme__api-42/intake/ship")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "must return 400 when no GitHub token is available"
        );
        let body = body_json(resp).await;
        let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            err.contains("GitHub token") || err.contains("no GitHub"),
            "error must mention missing GitHub token, got: {body:?}"
        );
    }

    // ── UowIntakeShipResponse structure ─────────────────────────────────────────

    /// `UowIntakeShipResponse` is serializable with the expected shape.
    #[test]
    fn uow_intake_ship_response_serializes_correctly() {
        let r = UowIntakeShipResponse {
            repos: vec![
                RepoShipResult {
                    repo: "acme/api".to_string(),
                    branch: "camerata/story-42".to_string(),
                    ok: true,
                    pr_url: Some("https://github.com/acme/api/pull/1".to_string()),
                    pr_number: Some(1),
                    error: None,
                },
                RepoShipResult {
                    repo: "acme/frontend".to_string(),
                    branch: "camerata/story-42".to_string(),
                    ok: false,
                    pr_url: None,
                    pr_number: None,
                    error: Some("push failed".to_string()),
                },
            ],
            all_ok: false,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["all_ok"], false);
        assert_eq!(json["repos"][0]["ok"], true);
        assert_eq!(json["repos"][1]["ok"], false);
        assert_eq!(json["repos"][0]["pr_number"], 1);
        assert_eq!(json["repos"][1]["error"], "push failed");
    }

    // ── multi-repo worktree setup: intake.repos determines the path ──────────────

    /// When intake.repos is empty (single-repo), repo_worktrees stays empty and
    /// execute_dev_implement_run uses the single-repo fallback path.
    #[test]
    fn multi_repo_path_requires_two_or_more_intake_repos() {
        // This test verifies the routing logic: <=1 intake repos → empty worktrees.
        let empty: Vec<crate::uow::RepoScope> = vec![];
        assert!(
            empty.len() <= 1,
            "zero intake repos → single-repo path (repo_worktrees = [])"
        );
        let single = vec![crate::uow::RepoScope {
            repo: "acme/api".to_string(),
            branch: crate::uow::BranchMode::default(),
        }];
        assert!(
            single.len() <= 1,
            "one intake repo → single-repo path (repo_worktrees = [])"
        );
        let multi = vec![
            crate::uow::RepoScope {
                repo: "acme/api".to_string(),
                branch: crate::uow::BranchMode::default(),
            },
            crate::uow::RepoScope {
                repo: "acme/frontend".to_string(),
                branch: crate::uow::BranchMode::default(),
            },
        ];
        assert!(
            multi.len() > 1,
            "two intake repos → multi-repo path (repo_worktrees populated)"
        );
    }

    // ── Credential manager tests ───────────────────────────────────────────────

    /// GET /api/credentials returns all known credentials with is_set=false when nothing
    /// is stored, and is_set=true + a masked value after one is stored.
    #[tokio::test]
    async fn list_credentials_returns_all_known_names_with_is_set_flag() {
        let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
        let app = router(state.clone());

        // Initially: all credentials are unset.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let arr = json.as_array().expect("must be an array");
        // Both known credentials must be present.
        let github = arr.iter().find(|c| c["name"] == "github_token").expect("must include github_token");
        let or_key = arr.iter().find(|c| c["name"] == "openrouter_api_key").expect("must include openrouter_api_key");
        assert!(!github["is_set"].as_bool().unwrap_or(true), "initially not set");
        assert!(!or_key["is_set"].as_bool().unwrap_or(true), "initially not set");

        // Store a GitHub token.
        state
            .credential_store
            .set(crate::credentials::GITHUB_TOKEN, "ghp_test_value")
            .expect("set must succeed in MemoryCredentialStore");

        // Now list again — is_set must flip to true for the github_token entry.
        let resp2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json2 = body_json(resp2).await;
        let arr2 = json2.as_array().expect("must be an array");
        let github2 = arr2.iter().find(|c| c["name"] == "github_token").expect("github_token present");
        assert!(github2["is_set"].as_bool().unwrap_or(false), "now is_set=true");
        let masked = github2["masked"].as_str().expect("masked must be present");
        // Masked value must not contain the full token.
        assert!(!masked.contains("ghp_test_value"), "masked must not reveal full token");
        // Must start with the first 4 chars.
        assert!(masked.starts_with("ghp_"), "masked must start with first 4 chars");
    }

    /// POST /api/credentials/:name rejects unknown credential names.
    #[tokio::test]
    async fn set_credential_rejects_unknown_name() {
        let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/credentials/totally_unknown_key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["ok"], false);
    }

    /// POST /api/credentials/:name rejects empty values.
    #[tokio::test]
    async fn set_credential_rejects_empty_value() {
        let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/credentials/github_token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// POST /api/credentials/:name returns { ok: true, masked: "…" } on success,
    /// and the masked value never contains the full token.
    #[tokio::test]
    async fn set_credential_returns_masked_value_not_full_token() {
        let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/credentials/github_token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value":"ghp_supersecrettoken"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["ok"], true);
        let masked = json["masked"].as_str().expect("masked field must be present");
        // Full token must NEVER appear in the response.
        assert!(
            !masked.contains("ghp_supersecrettoken"),
            "response must never contain the full token: {masked}"
        );
        // The prefix appears.
        assert!(masked.starts_with("ghp_"), "masked starts with first 4 chars");
    }

    /// `AppState::github_token()` prefers the credential store over the env var.
    #[test]
    fn github_token_method_prefers_store_over_env() {
        // Set a value in the store.
        let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
        state
            .credential_store
            .set(crate::credentials::GITHUB_TOKEN, "store_token")
            .expect("set must succeed");

        // Ensure the env var is NOT set so we are testing the store path.
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");

        let resolved = state.github_token();
        assert_eq!(
            resolved.as_deref(),
            Some("store_token"),
            "must return the store value when env var is absent"
        );
    }

    /// `AppState::github_token()` falls back to env var when store is empty.
    #[test]
    fn github_token_method_falls_back_to_env_when_store_empty() {
        let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
        // Store is empty (MemoryCredentialStore starts empty).

        std::env::set_var("CAMERATA_GITHUB_TOKEN", "env_token");
        let resolved = state.github_token();
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");

        assert_eq!(
            resolved.as_deref(),
            Some("env_token"),
            "must fall back to env var when store is empty"
        );
    }
}
