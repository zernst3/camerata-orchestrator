//! camerata-server: the Axum BFF the Dioxus cockpit talks to.
//!
//! This is the seam that turns the in-process monolith into a cloud-hostable
//! system: every UI-facing contract is an HTTP endpoint here, so the same server
//! runs locally behind the desktop shell today and in the cloud later. The UI
//! stops calling the backend crates directly and calls this instead.
//!
//! HISTORICAL NOTE, corrected 2026-10-11: this header used to describe a "Phase 1"
//! read-only router (`GET /api/health` / `/api/rules` / `/api/stories`) and promise
//! execution endpoints "in later phases". Those phases landed long ago. The crate is
//! now the composition root for most of the backend, and the module list below is the
//! accurate map. The subsystems a reader most often comes here for:
//!
//! - **The inspection pipeline.** [`onboard`] (deterministic floor, stack detection,
//!   rule proposal, the `Finding` / `ScanReport` / `SealedReport` data model),
//!   [`ai_audit`] (the semantic/LLM tier, the alternatives pass, the finding-kind
//!   assignment passes), [`scan_tools`] (the external-tool preview tier),
//!   [`dep_audit`], `mechanical_gate` (private) and [`suppression`].
//! - **The client deliverable.** [`report_export`] builds the PDF's JSON and owns the
//!   severity choke-point; [`xlsx_export`] builds the kind-partitioned workbook and
//!   `findings.json`. Both take `&onboard::SealedReport`, never `&ScanReport`, so the
//!   row set cannot be mutated after sealing. See `docs/TECHNICAL.md` section 6a.
//! - **Self-measurement.** [`scan_ledger`] records what actually ran;
//!   [`integrity_gate`] turns that into a verdict with an exit code. See
//!   `docs/TECHNICAL.md` section 6b.
//! - **The governed dev loop.** [`uow`], [`dev_implement_run`] (which owns the
//!   cross-agent integration gate bundle), [`jobs`], [`test_tamper`],
//!   [`enforcement_ledger`].
//!
//! Every UI-facing contract is still an HTTP endpoint registered in this module's
//! router, which is the property the first paragraph above describes and which has
//! not changed.

pub mod ai_audit;
pub mod api_agent_driver;
pub mod app_dirs;
pub mod arm;
pub mod auto_fire;
pub mod checkpoint;
pub mod clarify;
pub mod clarify_resume;
pub mod client_lock;
pub mod client_provenance;
pub mod client_source;
pub mod connections;
/// H5: the three-pass consensus option (off by default).
pub mod consensus;
mod corpus_freshness;
/// `POST /api/apps` (Part 2 of the Rust-fullstack scaffolder): drives
/// `camerata-scaffold`, creates + pushes the GitHub repo, and registers the
/// resulting Camerata project. See the module doc comment for the full flow.
pub mod create_app;
pub mod decisions;
pub mod decompose;
pub mod dep_audit;
pub mod draft;
pub mod edits;
pub mod enforcement_ledger;
pub mod engine_check;
pub mod escalation;
pub mod eval;
pub mod evidence;
pub mod feature_flags;
pub mod fix;
pub mod github_issues;
mod governed_rules;
pub mod grounding;
pub mod inspections;
pub mod investigation_run;
pub mod jobs;
// `lifecycle` (UowStage state machine + transitions) is now the framework-agnostic
// `camerata_app_core::lifecycle` (RUST-HEADLESS-CORE-1); re-exported so `crate::lifecycle::*` call
// sites (uow, lib) are unchanged.
pub use camerata_app_core::lifecycle;
pub mod cost_cap;
pub mod failure_report;
/// The failure classes and typed transport failure, re-exported for callers of the server.
pub use camerata_llm::failure;
pub mod dev_implement_run;
pub mod efficiency;
pub mod live_fleet;
pub mod llm;
mod native_units;
pub mod notify;
pub mod onboard;
/// `POST /api/orchestrator/message` — the architect-orchestrator's first
/// functional turn (real LLM + real governed execution seam). See the module doc.
pub mod orchestrator_message;
pub mod ownership;
pub mod pr;
pub mod pr_resolve_run;
pub mod preflight;
pub mod project;
/// Delimiting client content for a model with a per-run random token (prompt-injection defense).
pub mod prompt_frame;
pub mod provider;
pub mod reconcile;
/// Inspection findings become units of work (roadmap 2c): the plan and create routes, the
/// implementer's findings grounding and the delta retest evidence.
pub mod remediation;
/// PDF audit-report export (brownfield audit hardening Pass B): `AuditReportJson` +
/// `build_report_json` (pure serializer) + `compile_pdf` (Typst compile). See the
/// module doc comment and `docs/design/2026-07-23_brownfield-audit-report.md`.
pub mod remediation_plan;
/// H5: compare two runs of the same scan (`camerata repeatability`, the consensus option).
pub mod repeatability;
pub mod report_export;
pub mod review_agent;
/// The review tree's cross-run site matching (`match_runs`); the stable site key it matches on is
/// `camerata_api_types::site_key`.
pub mod review_tree;
pub mod review_view;
pub mod routine;
pub mod run;
pub mod run_health;
pub mod run_store;
mod run_target;
pub mod runtime_audit;
pub mod scan_cache;
pub mod scan_routing;
pub mod scan_tools;
pub mod scope_registration;
pub mod severity_bounds;
pub mod stage_telemetry;
pub mod stall_sweep;
/// Excel workbook export (product-export, Pass C): `build_workbook`, the Excel sibling of
/// `report_export::build_report_json`. See the module doc comment and
/// `docs/design/2026-07-27_product-export.md`.
pub mod xlsx_export;
// `schedule` (routine string parsing + is_due/next_fire) is now the framework-agnostic
// `camerata_app_core::schedule` (RUST-HEADLESS-CORE-1); re-exported so `crate::schedule::*` call
// sites (auto_fire, lib) are unchanged.
pub use camerata_app_core::schedule;
/// Layer-3 CI workflow generator — produces `.github/workflows/camerata-gates.yml`
/// from the built-in language gate commands + manifest checks. See
/// `docs/decisions/2026-06-22_check_manifest_single_source_of_truth.md`.
pub mod credentials;
/// A10: a finding the engine throws out is kept, marked and left out of the client export, never
/// deleted. See the module doc comment.
pub mod engine_discard;
/// D3: the standard JavaScript, Web and Node API names and the ESLint rule names a fix may use
/// without them being in the finding's evidence.
pub(crate) mod fix_standard_names;
/// W6 / I2: the PRE-FLIGHT INTEGRITY GATE — turns the `scan_ledger` summary a human used to
/// eyeball into a verdict with an exit code, so a silently degraded scan is caught before a
/// graded run is paid for rather than after. See the module doc comment.
pub mod integrity_gate;
/// D1: the content test for a junk model finding (placeholder text, no evidence, a discard
/// verdict in its own explanation), shared by scan parsing, the post-calibration filter and the
/// release gate. See the module doc comment.
pub mod junk_finding;
/// W1 item 4: corpus-level build gate — every rule declaring `enforcement = "mechanical"`
/// must resolve to a real wired detector. See the module doc comment.
mod mechanical_gate;
pub mod model_profile_cascade;
pub mod model_registry;
pub mod model_tier;
pub mod provider_policy;
pub mod rate_limit;
pub mod release_gate;
/// M4: wording problems recorded as review flags on the row (one list in one capture), never as a
/// failure that locks a run or an export. See the module doc comment.
pub mod review_flags;
/// W1: the pipeline-integrity ledger — per-rule "did it actually run" tracking and
/// per-stage row-accounting, so a rule that never ran can never be reported as
/// "verified clean" and a row that vanishes between pipeline stages can never go
/// unnoticed. See the module doc comment and the hardening-cycle notes for the defects this closes.
pub mod scan_ledger;
pub mod settings;
pub mod story_docs_gate;
pub mod suppression;
pub mod terminal;
/// Real-git fixtures shared by unit and integration tests; nothing in the product calls it.
#[doc(hidden)]
pub mod test_git;
pub mod test_tamper;
pub mod tool_provisioning;
pub mod transcript;
pub mod uow;
pub mod update_branch_run;
pub mod usage_ledger;
/// GAP-2: the shared commit / PR / branch chokepoint gate. Every server-side VCS action
/// funnels through `vcs_choke` so a project's configured process rules are actually enforced
/// (HARD-BLOCK on violation), not merely configurable via the bypass endpoint.
pub mod vcs_choke;
pub mod vuln_intel;
pub mod warm_schedule;
pub mod workflow_gen;
pub mod workitems;
pub mod workspace;

mod routes;
pub use routes::*;

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
use crate::orchestrator_message::orchestrator_message;
use crate::provider::ProviderHandle;
use crate::routine::{CreateRoutineReq, Routine, RoutineStore, SetEnabledReq};
use crate::run::{live_mode_enabled, run_provenance, Run, RunProvenance, RunStore};

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
    provider: crate::provider::ProviderCell,
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
    /// Persisted, resumable checkpoints for governed dev runs paused on a review-needed denial.
    checkpoints: crate::checkpoint::CheckpointStore,
    /// Per-project incremental-scan cache (file fingerprints + last AI findings) so a
    /// re-scan only pays the AI bill for files that changed. Best-effort; losing it just
    /// means the next scan is a full scan.
    scan_cache: crate::scan_cache::ScanCacheStore,
    /// Persisted per-inspection entries (the sealed report plus its provenance), one file per
    /// project. The durable twin of `last_scan`: written whenever a scan completes, read back
    /// by [`AppState::get_last_scan`] when the in-memory map is empty (after a restart), and the
    /// ONLY source the exports and the review screen have once client source is deleted.
    inspections: crate::inspections::InspectionStore,
    /// The reviewer's decisions on sites, one file per project beside the inspections. Never
    /// part of the sealed report; see [`crate::decisions`].
    decisions: crate::decisions::DecisionStore,
    /// The reviewer's edits, added findings, merges and splits, one file per project beside the
    /// decisions. An overlay on the sealed report; see [`crate::edits`].
    edits: crate::edits::EditStore,
    /// The Engine Check's verdicts, one file per project beside the edits. Internal quality
    /// control: never part of a client output; see [`crate::engine_check`].
    engine_check: crate::engine_check::EngineCheckStore,
    /// Machine-local deletion records (the proof handed to a client) and re-fetch records.
    /// Not part of the project export. See [`crate::client_source`].
    source_records: crate::client_source::records::SourceRecordStore,
    /// Operations in flight on a repo's source (delete, re-fetch, rescan) and scans running in a
    /// request: what the deletion guard's "scan in progress" refusal reads.
    source_ops: crate::client_source::ops::SourceOps,
    /// Where network git steps (re-fetch clone, probe, commit fetch) go. GitHub in production;
    /// tests map a repo to a local bare repository so nothing touches the network.
    git_remotes: crate::client_source::remote::GitRemotes,
    /// The locations source deletion may touch besides the checkout. EMPTY by default so a
    /// state built by `new()` can touch nothing outside a checkout; only `from_env()` installs
    /// the real ones. See [`crate::client_source::storage`].
    storage_roots: Arc<crate::client_source::storage::StorageRoots>,
    /// Per-project last completed scan report. Written the instant any scan handler finishes
    /// (both the synchronous `onboard_audit` path and the async job path). Read by
    /// `active_project_context` as the authoritative source for grounding, with the
    /// UI-round-tripped draft as a first-priority fallback. In-memory only (v1); does not
    /// survive a process restart (a v2 concern).
    last_scan: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, crate::onboard::ScanReport>>,
    >,
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
    /// The readable governance-event audit trail (Phase H1 foundation; SQLite). Unlike
    /// `enforcement_ledger`, this store is meant to be READ BACK (e.g. a per-run activity
    /// timeline). Opened best-effort in [`AppState::from_env`], same fail-soft pattern as
    /// `enforcement_ledger`: `None` in tests and on open failure. NOT YET instrumented at
    /// any lifecycle site as of Phase H1 — that instrumentation is a later phase. Use
    /// [`AppState::record_governance`] as the single call-site helper when it lands.
    pub governance_log: Option<Arc<camerata_persistence::GovernanceLog>>,
    /// The Product-Owner feedback loop's defect-report store (SQLite). Backs
    /// `POST /api/feedback`, `GET /api/projects/:id/feedback`, and
    /// `GET /api/feedback/recent`. Opened best-effort in [`AppState::from_env`], same
    /// fail-soft pattern as `governance_log`: `None` in tests and on open failure — the
    /// endpoints return `{ ok: false, ... }` / an empty list rather than a 500. Use
    /// [`AppState::record_defect`] as the single call-site helper.
    pub feedback_store: Option<Arc<camerata_persistence::FeedbackStore>>,
    /// The confidence engine's decision + outcome + calibration store (SQLite). Backs
    /// the Architect orchestrator's `orchestrator_decision` recording (the "measured
    /// override rate at max dial" moat metric — see
    /// the product-owner vibe-mode design notes,
    /// "Architect-orchestrator design"). Opened best-effort in [`AppState::from_env`],
    /// same fail-soft pattern as `governance_log`/`feedback_store`: `None` in tests and
    /// on open failure. NOT YET instrumented at any lifecycle site as of this phase —
    /// that is the orchestrator LOOP, a later phase; this wires the recording
    /// substrate. Use [`AppState::record_orchestrator_decision`] as the single
    /// call-site helper when it lands.
    pub orchestrator_decisions: Option<Arc<camerata_persistence::OrchestratorDecisionLog>>,
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
    /// Cached authenticated GitHub login for the configured token (the "who am I" behind
    /// the UI's "Assign to me"). `None` = not yet resolved (or resolvable). Fetched ONCE
    /// via `GET /user` on first `/api/me` and memoized here, so it is never re-fetched per
    /// request. Only a successful login is cached; a token-less / failed lookup leaves it
    /// `None` so a later request (after a token is set) can still resolve it.
    github_login_cache: Arc<std::sync::Mutex<Option<String>>>,
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
            provider: crate::provider::ProviderCell::new(ProviderHandle::native()),
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
            checkpoints: crate::checkpoint::CheckpointStore::new(),
            scan_cache: crate::scan_cache::ScanCacheStore::new(),
            inspections: crate::inspections::InspectionStore::new(),
            decisions: crate::decisions::DecisionStore::new(),
            edits: crate::edits::EditStore::new(),
            engine_check: crate::engine_check::EngineCheckStore::new(),
            source_records: crate::client_source::records::SourceRecordStore::new(),
            source_ops: crate::client_source::ops::SourceOps::new(),
            git_remotes: crate::client_source::remote::GitRemotes::default(),
            storage_roots: Arc::new(crate::client_source::storage::StorageRoots::default()),
            last_scan: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            recent_scan: std::sync::Arc::new(std::sync::Mutex::new(None)),
            artifacts: None,
            feature_flags: crate::feature_flags::FeatureFlags::default(),
            usage_ledger: Arc::new(crate::usage_ledger::UsageLedger::new()),
            // Tests and in-process runs start without a ledger (no-op; fail-soft).
            enforcement_ledger: crate::enforcement_ledger::EnforcementLedger::none(),
            // Tests and in-process runs start without a governance log (no-op; fail-soft).
            governance_log: None,
            // Tests and in-process runs start without a feedback store (no-op; fail-soft).
            feedback_store: None,
            // Tests and in-process runs start without an orchestrator-decision store (no-op; fail-soft).
            orchestrator_decisions: None,
            // Tests use an in-memory credential store; production uses the OS keychain.
            credential_store: Arc::new(crate::credentials::MemoryCredentialStore::new()),
            // The registry always has the static Claude entries; OpenRouter is empty until
            // refreshed (key not set in tests anyway).
            model_registry: crate::model_registry::ModelRegistry::new(),
            // Default limiter: 20 RPM for "openrouter", unlimited for all others.
            rate_limiter: Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
            // The authenticated login is resolved lazily on the first `/api/me`.
            github_login_cache: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// The keyless CLI-transport FLOOR `Llm` (kept for hermetic tests and label-only probes).
    /// NO production model step uses it: project steps go through [`AppState::step_llm`] and
    /// the chat through [`AppState::chat_llm`], so the backend a user selects is the backend
    /// that runs. A clone of the shared `Llm` seam WITH the process-global usage ledger attached, so any
    /// model call made through it is recorded into the cumulative cockpit meter. Reads
    /// vendor/model from the environment; the TRANSPORT is fixed to `Backend::Cli`
    /// (`Llm::from_env`'s default per `docs/design/2026-09-22_per-project-backend.md` — no
    /// `CAMERATA_LLM_BACKEND` env read exists anymore). The `claude` CLI transport uses no
    /// API key, so this carries none; anything that may run on the `api` backend builds its
    /// `Llm` with `Llm::for_backend*` and a key from `credentials::anthropic_api_key`.
    ///
    /// This is the FLOOR constructor for call sites that have not (yet) been threaded with an
    /// explicit per-project or chat backend — today: the non-fleet AI steps driven purely by
    /// `state.projects.active()`'s per-step model (decomposition, clarification, story
    /// authoring, escalation), the L3/integration-gate advisory calls inside the governed dev
    /// loop, and misc content generation (PR title/body, HTML report rendering). The five
    /// NAMED seams (brownfield scan, alternative recommendation, rescan, the gov-dev agent
    /// driver, and the chat assistant) do NOT use this — they resolve `project.backend` /
    /// `settings.chat_backend` explicitly and call `Llm::for_backend_and_ledger`
    /// themselves.
    pub fn llm(&self) -> crate::llm::Llm {
        crate::llm::Llm::from_env_with_ledger(self.usage_ledger.clone())
    }

    /// The `Llm` for a model step, on the transport the SETTING says, with the key resolved RIGHT
    /// NOW (`credentials::anthropic_api_key`: the key saved in the app first, env as fallback).
    ///
    /// - With a `project`, the project's own `backend` decides, exactly as the audit does.
    /// - With no project, the global `settings.chat_backend` decides (the chat assistant, and any
    ///   project-less step).
    ///
    /// `Api` calls the Anthropic Messages API with that key; `Cli` drives the logged-in Claude
    /// Code CLI. `Api` with no key is `Err(message)`, never a silent downgrade to the CLI (the
    /// same compliance-safety rule as every other seam).
    pub fn llm_for_project(
        &self,
        project: Option<&crate::project::Project>,
    ) -> Result<crate::llm::Llm, String> {
        // A client engagement never runs a model step on the subscription. The models are
        // chosen per call (and this transport only reaches Anthropic), so only the backend is
        // checked here.
        if let Some(p) = project {
            if let Some(why) = crate::client_lock::refusal(p.client_engagement, p.backend, &[]) {
                return Err(format!("{why} {APP_CLIENT_LOCK_FIX}"));
            }
        }
        let key = crate::credentials::anthropic_api_key(self.credential_store.as_ref());
        let setting = project
            .map(|p| p.backend)
            .unwrap_or_else(|| self.settings.chat_backend());
        let backend = match crate::llm::resolve_backend(setting, key.is_some()) {
            crate::llm::BackendResolution::Blocked { message } => return Err(message),
            crate::llm::BackendResolution::Api => crate::llm::Backend::Api,
            crate::llm::BackendResolution::Cli => crate::llm::Backend::Cli,
        };
        Ok(crate::llm::Llm::for_backend_and_ledger(
            backend,
            key,
            self.usage_ledger.clone(),
        ))
    }

    /// The client-engagement lock for a step whose model id comes from the request (the routine
    /// prompt draft, the escalation review chat, the chat), checked against the ACTIVE project.
    /// [`AppState::llm_for_project`] is handed no model and so cannot check one: without this a
    /// non-Anthropic id would be routed to OpenRouter by `build_completer`. A blank id is the
    /// server default (a Claude model) and passes; with no active project there is no client
    /// code in the step's context and nothing is refused.
    pub fn client_lock_step_model_refusal(&self, model: &str) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        let p = self.projects.active()?;
        crate::client_lock::refusal(
            p.client_engagement,
            p.backend,
            &[("this step".to_string(), model.to_string())],
        )
        .map(|why| format!("{why} {APP_CLIENT_LOCK_FIX}"))
    }

    /// The `Llm` for the project-less CHAT assistant: [`AppState::llm_for_project`] with no
    /// project, so `settings.chat_backend` decides.
    ///
    /// When the ACTIVE project is a client engagement, the chat backend setting must pass the
    /// client-engagement lock too (a `cli` chat is refused with the lock's message): the chat can
    /// carry that project's context, and the setting is global, not the project's own.
    pub fn chat_llm(&self) -> Result<crate::llm::Llm, String> {
        if let Some(p) = self.projects.active() {
            let backend = self.settings.chat_backend();
            if let Some(why) = crate::client_lock::refusal(p.client_engagement, backend, &[]) {
                return Err(format!(
                    "{why} The chat assistant follows the Chat backend setting, which is `cli`. \
                     Set the Chat backend to API in Settings, or turn off Client engagement in \
                     this project's Settings if this is your own development."
                ));
            }
        }
        self.llm_for_project(None)
    }

    /// The `Llm` for a NON-FLEET step of the ACTIVE project (decomposition, clarification,
    /// story authoring, escalation translation, diagrams, mockups): the active project's
    /// backend decides; with no active project, the chat backend setting does.
    pub fn step_llm(&self) -> Result<crate::llm::Llm, String> {
        self.llm_for_project(self.projects.active().as_ref())
    }

    /// [`AppState::step_llm`] as a port, for seams that cannot return an error to the caller
    /// (the L3 review and integration advisories). A refused backend becomes an `LlmPort` that
    /// fails every call with the refusal message, so the step reports it instead of running on
    /// the wrong transport.
    pub fn step_llm_port(&self) -> Arc<dyn crate::llm::LlmPort> {
        match self.step_llm() {
            Ok(llm) => Arc::new(llm),
            Err(message) => Arc::new(crate::llm::RefusedLlm::new(message)),
        }
    }

    /// Record one governance event to the audit trail (Phase H1 foundation).
    ///
    /// The single call-site helper future lifecycle-instrumentation phases (H2+) should
    /// use, e.g.:
    /// `state.record_governance(GovernanceEvent::info(run_id, "run_started", "system")).await;`
    ///
    /// Fail-soft by construction: a `None` log (tests, or an open failure at startup) is
    /// a silent no-op, and a write error is logged via `tracing::warn!` and swallowed —
    /// callers never need to match on a `Result`. Async (not spawned) so a caller that
    /// wants the write to have landed before proceeding (e.g. before returning an HTTP
    /// response) can simply `.await` it; callers that don't care about ordering can
    /// `tokio::spawn` the call themselves.
    pub async fn record_governance(&self, event: camerata_persistence::GovernanceEvent) {
        let Some(log) = self.governance_log.as_ref() else {
            return;
        };
        if let Err(e) = log.record(event).await {
            tracing::warn!(error = %e, "failed to record governance event");
        }
    }

    /// Record one defect report to the feedback store (Product-Owner feedback loop).
    /// Fail-soft: `None` when no feedback store is open this session (tests, or an
    /// open failure at startup) — mirrors [`Self::record_governance`]'s contract.
    /// Returns the assigned row id on success, logging (and swallowing) a write error.
    ///
    /// Does NOT also write a governance-audit row — [`submit_feedback`] does that
    /// itself (kind `"defect_reported"`) so the pairing stays visible at the one call
    /// site that owns both writes, rather than being hidden inside this helper.
    pub async fn record_defect(
        &self,
        report: camerata_api_types::feedback::DefectReport,
    ) -> Option<i64> {
        let store = self.feedback_store.as_ref()?;
        match store.record(report).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(error = %e, "failed to record defect report");
                None
            }
        }
    }

    /// Record one autonomous decision to the confidence engine's decision store, AND
    /// write a matching `governance_events` row (kind `"orchestrator_decision"`) so
    /// the unified audit trail carries it alongside every other governed-run event.
    /// Unlike [`Self::record_defect`] (whose paired governance write lives at the
    /// `submit_feedback` call site), this helper owns BOTH writes itself — there is no
    /// separate HTTP ingest endpoint for decisions yet; the orchestrator loop that
    /// will call this directly is a later phase.
    ///
    /// Fail-soft: `None` when no decision store is open this session (tests, or an
    /// open failure at startup) — mirrors [`Self::record_defect`]'s contract. The
    /// governance-event write is best-effort on top (via [`Self::record_governance`]),
    /// so a governance-log outage never blocks the decision itself from being
    /// recorded.
    pub async fn record_orchestrator_decision(
        &self,
        decision: camerata_persistence::OrchestratorDecision,
    ) -> Option<i64> {
        let store = self.orchestrator_decisions.as_ref()?;
        match store.record_decision(decision.clone()).await {
            Ok(id) => {
                // Class C (irreversible) decisions are ALWAYS supposed to be
                // human-gated before they execute (the dial can never override this —
                // see the plan doc), so one showing up here at all is notable; every
                // other class is routine.
                let severity = if decision.class == "irreversible" {
                    "warn"
                } else {
                    "info"
                };
                let detail = serde_json::to_string(&decision).unwrap_or_default();
                self.record_governance(
                    camerata_persistence::GovernanceEvent::new(
                        decision.run_id.clone(),
                        "orchestrator_decision",
                        severity,
                        "system",
                    )
                    .with_reason(format!(
                        "{} (class {}, confidence {})",
                        decision.chosen, decision.class, decision.confidence
                    ))
                    .with_detail(detail),
                )
                .await;
                Some(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to record orchestrator decision");
                None
            }
        }
    }

    /// Read-only accessor for the project store. Exposed so integration tests can seed a
    /// project via the REAL [`crate::project::ProjectStore`] write path (e.g.
    /// `set_step_model`) and then assert that step/tier resolution reads it back — the same
    /// store the handlers mutate. The field itself stays private (handlers use `state.projects`
    /// in-crate); this accessor is the out-of-crate test seam.
    pub fn projects(&self) -> &crate::project::ProjectStore {
        &self.projects
    }

    /// The persisted inspection store. Out-of-crate test seam (mirrors [`Self::projects`]): the
    /// end-to-end tests read the entry a scan wrote and the one a restart reloads.
    pub fn inspections(&self) -> &crate::inspections::InspectionStore {
        &self.inspections
    }

    /// The reviewer's decision store. Out-of-crate test seam (mirrors [`Self::inspections`]).
    pub fn decisions(&self) -> &crate::decisions::DecisionStore {
        &self.decisions
    }

    /// The reviewer's edit store. Out-of-crate test seam (mirrors [`Self::decisions`]).
    pub fn edits(&self) -> &crate::edits::EditStore {
        &self.edits
    }

    /// The Engine Check's verdict store. Out-of-crate test seam (mirrors [`Self::edits`]).
    pub fn engine_check(&self) -> &crate::engine_check::EngineCheckStore {
        &self.engine_check
    }

    /// The deletion and re-fetch record store. Out-of-crate test seam (mirrors
    /// [`Self::projects`]).
    pub fn source_records(&self) -> &crate::client_source::records::SourceRecordStore {
        &self.source_records
    }

    /// The app-level (cross-project) settings store. Out-of-crate test seam (mirrors
    /// [`Self::projects`]): tests set the app-level `chat_model` via this store and then assert
    /// the chat model resolves back. The field itself stays private (handlers use
    /// `state.settings` in-crate).
    pub fn settings(&self) -> &crate::settings::SettingsStore {
        &self.settings
    }

    /// The Unit-of-Work store. Out-of-crate test seam (mirrors [`Self::projects`]): the
    /// governed-dev lifecycle E2E tests drive the UoW state machine through this store
    /// (the same store the handlers mutate) and assert stage transitions + gating. The
    /// field itself stays private (handlers use `state.uow` in-crate).
    pub fn uow(&self) -> &crate::uow::UowStore {
        &self.uow
    }

    /// Resolve the GitHub token: credential store first, then the
    /// `CAMERATA_GITHUB_TOKEN` environment variable as a back-compat fallback.
    /// Returns `None` when neither is set or non-empty.
    pub fn github_token(&self) -> Option<String> {
        // Keychain first, then CAMERATA_GITHUB_TOKEN, via the one credential resolver (which
        // warns, not swallows, on a keychain read error before falling back to env).
        crate::credentials::github_token(self.credential_store.as_ref())
    }

    /// Resolve the AUTHENTICATED GitHub login for the configured token, memoized so the
    /// `GET /user` call happens at most once per process (not per request). Returns `None`
    /// gracefully when there is no token or the lookup fails; only a successful login is
    /// cached, so a later call (after a token is configured) can still resolve it.
    pub async fn github_login(&self) -> Option<String> {
        // Fast path: already resolved.
        if let Some(login) = self
            .github_login_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Some(login);
        }
        // Not cached, so resolve via the token. No token, no identity.
        let token = self.github_token()?;
        match crate::github_issues::get_authenticated_login(&token).await {
            Ok(login) if !login.is_empty() => {
                *self
                    .github_login_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(login.clone());
                Some(login)
            }
            _ => None,
        }
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

        // ── Soft context (#112): the product brief + the enabled operating principles. The brief
        // is the "why/for-whom/quality-bar" that lets an agent make a judgment call the spec did
        // not anticipate; the principles are how-we-work conduct. Woven in ABOVE the rules so the
        // agent reads the why before the what. Both travel with the project export.
        let mut soft = String::new();
        if !project.product_brief.trim().is_empty() {
            soft.push_str("## Product context\n");
            soft.push_str(project.product_brief.trim());
            soft.push_str("\n\n");
        }
        let enabled: Vec<&crate::project::OperatingPrinciple> = project
            .operating_principles
            .iter()
            .filter(|p| p.enabled && !p.text.trim().is_empty())
            .collect();
        if !enabled.is_empty() {
            soft.push_str("## How to work here\n");
            for p in enabled {
                soft.push_str(&format!("- {}\n", p.text.trim()));
            }
            soft.push('\n');
        }
        // Project memory (Layer 3): the APPROVED, curated learnings, capped to the most recent so
        // they inform without crowding out the task. Proposed/archived entries are excluded.
        let approved: Vec<&crate::project::MemoryEntry> = project
            .memory
            .iter()
            .filter(|m| {
                m.status == crate::project::MemoryStatus::Approved && !m.text.trim().is_empty()
            })
            .collect();
        if !approved.is_empty() {
            const MEM_CAP: usize = 15;
            let start = approved.len().saturating_sub(MEM_CAP);
            soft.push_str("## What we have learned on this project\n");
            for m in &approved[start..] {
                let k = match m.kind {
                    crate::project::MemoryKind::Decision => "decision",
                    crate::project::MemoryKind::Pattern => "pattern",
                    crate::project::MemoryKind::Gotcha => "gotcha",
                    crate::project::MemoryKind::Constraint => "constraint",
                };
                soft.push_str(&format!("- [{k}] {}\n", m.text.trim()));
            }
            soft.push('\n');
        }
        let soft = if soft.trim().is_empty() {
            None
        } else {
            Some(soft)
        };

        let base = crate::grounding::assemble(rule_section, &repo_digests, &unresolved);
        match (soft, base) {
            (Some(s), Some(b)) => Some(format!("{s}\n{b}")),
            (Some(s), None) => Some(s),
            (None, b) => b,
        }
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

    /// The EFFECTIVE work-hierarchy schema to use for design-mode child proposal +
    /// materialization validation. Resolves the active project's stored schema, but falls
    /// back to the seeded default ladder ([`crate::project::default_hierarchy_schema`]) when
    /// there is NO active project OR the stored schema is empty (no types or no relations).
    ///
    /// An empty schema is a bug source, not a valid configuration: it makes the design-author
    /// prompt emit `ALLOWED_CHILD_TYPES: []` (so the model proposes nothing, and any proposals
    /// are stripped) and makes materialize validation reject every child. A project can end up
    /// empty via a bare import that omitted a schema. Resolving empty → default here keeps the
    /// design-author handler, the materialize handler, and validation in agreement. This does
    /// NOT persist the default onto the project; it only affects the in-flight resolution.
    pub(crate) fn effective_hierarchy_schema(&self) -> crate::project::HierarchySchema {
        self.projects
            .active()
            .map(|p| p.hierarchy_schema)
            .unwrap_or_default()
            .resolve_effective()
    }

    /// Store the last completed scan report for the given project. Called the instant a
    /// scan handler finishes on either the synchronous or async path. Fail-soft on lock
    /// poisoning: recovers the inner value rather than panicking the handler.
    ///
    /// Also updates [`Self::recent_scan`] so the project-agnostic fallback always reflects
    /// the most recently completed scan, regardless of which project was active.
    ///
    /// A REAL inspection (the report carries at least one audited repo and is neither the
    /// connect-GitHub gate nor a compliance block) is also appended to the persisted
    /// [`crate::inspections::InspectionStore`], so it survives a restart and outlives the
    /// client's source on disk. A persistence failure is logged, never fatal: the in-memory
    /// copy still serves this session.
    pub(crate) fn set_last_scan(&self, project_id: String, report: crate::onboard::ScanReport) {
        // The patterns across services are derived from the ledger and the findings; rebuilt
        // here so the stored report (and the cockpit tab that reads it) never lags them.
        let mut report = report;
        // A finding the reviewer put back after the engine threw it out stays put when the scan
        // runs again (A12 item 2): its restore is a decision in the store, and the new run's
        // report gets the row back before it is recorded.
        report.reinstate_restored(&crate::decisions::restored_keys_of(
            &self.decisions.active(&project_id),
        ));
        report.refresh_patterns_across_services();
        self.record_inspection(
            &project_id,
            &report,
            crate::inspections::InspectionKind::Full,
        );
        self.set_last_scan_in_memory(project_id, report);
    }

    /// Replace a project's in-memory last scan and the project-agnostic recent scan WITHOUT
    /// creating an inspection entry. The part of [`Self::set_last_scan`] shared with callers
    /// that persist the entry themselves (a delta retest stamps its own
    /// [`crate::inspections::InspectionKind`]) or amend the existing one.
    pub(crate) fn set_last_scan_in_memory(
        &self,
        project_id: String,
        report: crate::onboard::ScanReport,
    ) {
        let mut report = report;
        report.refresh_patterns_across_services();
        report.assign_site_keys();
        let mut guard = self.last_scan.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(project_id, report.clone());
        drop(guard); // release per-project lock before acquiring recent_scan lock
        self.set_recent_scan(report);
    }

    /// Replace the project-agnostic most-recent scan (see [`Self::recent_scan`]).
    pub(crate) fn set_recent_scan(&self, report: crate::onboard::ScanReport) {
        let mut recent = self.recent_scan.lock().unwrap_or_else(|e| e.into_inner());
        *recent = Some(report);
    }

    /// Append `report` to the persisted inspection store when it is a real inspection.
    /// See [`Self::set_last_scan`].
    pub(crate) fn record_inspection(
        &self,
        project_id: &str,
        report: &crate::onboard::ScanReport,
        kind: crate::inspections::InspectionKind,
    ) -> Option<crate::inspections::InspectionEntry> {
        if report.gated || report.blocked || report.provenance.audited_refs.is_empty() {
            return None;
        }
        // The stored report is the sealed artifact every export is built from, so it carries
        // only redacted text: sweep a CLONE (the caller's copy is left as handed in) before
        // anything reaches the file.
        let mut sealed = report.clone();
        sealed.sweep_secrets_before_seal();
        let entry = crate::inspections::InspectionEntry::from_report(project_id, sealed, kind);
        match self.inspections.append(entry) {
            Ok(stored) => Some(stored),
            Err(e) => {
                tracing::warn!(
                    project = project_id,
                    error = %e,
                    "could not persist the completed inspection; exports will not survive a restart"
                );
                // The in-memory copy was updated before the write failed, so it is the newest.
                self.inspections.latest(project_id)
            }
        }
    }

    /// Remember Claude Code sessions that model calls made AFTER the inspection finished (the
    /// alternatives rescan) on the project's current inspection entry, so source deletion
    /// removes their session files too. A no-op without sessions or without an entry.
    pub(crate) fn record_model_sessions(
        &self,
        project_id: &str,
        sessions: Vec<crate::llm::ModelSession>,
    ) {
        for session in sessions {
            if let Err(e) = self.inspections.record_model_session(project_id, session) {
                tracing::warn!(
                    project = project_id,
                    error = %e,
                    "could not persist a recorded model session"
                );
            }
        }
    }

    /// Merge a changed report into the project's CURRENT inspection instead of starting a new
    /// one (the alternatives rescan re-evaluates a few rules of the same inspection). Updates
    /// the in-memory copy and the latest persisted entry in place.
    pub(crate) fn amend_last_scan(&self, project_id: String, report: crate::onboard::ScanReport) {
        let mut report = report;
        report.refresh_patterns_across_services();
        // Same sweep as `record_inspection`: the stored report carries only redacted text.
        let mut sealed = report.clone();
        sealed.sweep_secrets_before_seal();
        match self.inspections.update_latest_report(&project_id, sealed) {
            Ok(_) => {}
            Err(e) => tracing::warn!(
                project = project_id.as_str(),
                error = %e,
                "could not persist the amended inspection"
            ),
        }
        self.set_last_scan_in_memory(project_id, report);
    }

    /// Retrieve the last completed scan report for the given project, if any.
    ///
    /// The in-memory map answers first (it is the freshest within a session); when it has
    /// nothing for the project (a restart emptied it) the latest persisted inspection entry
    /// answers instead. That single fallback is what makes every export, the chat grounding and
    /// the alternatives rescan keep working after a restart and after source deletion.
    /// Returns a clone so callers do not hold the lock across await points.
    /// Fail-soft on lock poisoning: recovers the inner value rather than panicking.
    pub(crate) fn get_last_scan(&self, project_id: &str) -> Option<crate::onboard::ScanReport> {
        let guard = self.last_scan.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(report) = guard.get(project_id) {
            return Some(report.clone());
        }
        drop(guard);
        self.inspections.latest(project_id).map(|e| e.report)
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
        let guard = self.recent_scan.lock().unwrap_or_else(|e| e.into_inner());
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

    /// A clean-slate state whose file-backed stores all live under `dir` (the `camerata`
    /// directory itself, i.e. what `<data_dir>/camerata` is in production). Opens ONLY the JSON
    /// stores, with an in-memory credential store and no SQLite logs, so it needs no async
    /// runtime and never touches the keychain or the real per-user directory. This is the
    /// "restart" seam for end-to-end tests: build a state on a temp dir, drop it, build another
    /// on the same dir, and everything persisted is what the second one sees.
    pub fn from_data_dir(dir: &std::path::Path) -> Self {
        let mut state = Self::new(Arc::new(InMemoryStoryStore::new()));
        state.open_json_stores(dir);
        // Only the data dir is known: nothing outside it (no CLI cache dir, no Claude Code
        // folder) can be touched unless the caller installs roots with `with_storage_roots`.
        state.storage_roots = Arc::new(crate::client_source::storage::StorageRoots {
            data_dir: Some(dir.to_path_buf()),
            ..Default::default()
        });
        state
    }

    /// Replace where network git steps go. Test seam (see [`crate::client_source::remote`]).
    #[must_use]
    pub fn with_git_remotes(mut self, remotes: crate::client_source::remote::GitRemotes) -> Self {
        self.git_remotes = remotes;
        self
    }

    /// Replace the locations source deletion may touch besides the checkout. The test seam that
    /// confines every deletion to a temp directory; production installs the real roots in
    /// [`AppState::from_env`] and never calls this.
    #[must_use]
    pub fn with_storage_roots(
        mut self,
        roots: crate::client_source::storage::StorageRoots,
    ) -> Self {
        self.storage_roots = Arc::new(roots);
        self
    }

    /// Open every JSON-file-backed store under `dir`. The single list of those stores: both
    /// [`AppState::from_env`] and [`AppState::from_data_dir`] go through here, and the
    /// purge-completeness test reads this function's source to prove every store it opens is
    /// accounted for in `client_source::KNOWN_STORAGE_LOCATIONS`.
    fn open_json_stores(&mut self, dir: &std::path::Path) {
        self.projects = crate::project::ProjectStore::load_or_new(dir.join("projects.json"));
        self.settings = crate::settings::SettingsStore::load_or_new(dir.join("settings.json"));
        // Provider-safety session-scoped reset (Pass 2 §3): `safe_mode` must never
        // survive a restart as OFF, even though it's otherwise persisted alongside the
        // rest of `Settings`. See `SettingsStore::reset_provider_policy_to_safe_on_startup`
        // for the full mechanism note — this is the one call site.
        self.settings.reset_provider_policy_to_safe_on_startup();
        // No backend env hydration anymore (`docs/design/2026-09-22_per-project-backend.md`):
        // there is no `CAMERATA_LLM_BACKEND` env var to hydrate. Every project-scoped call
        // resolves its `Llm` from `project.backend` and the chat path from
        // `state.settings.chat_backend()`, both read directly at the call seam.
        self.draft = crate::draft::DraftStore::at(dir.join("onboarding-draft.json"));
        self.uow = crate::uow::UowStore::at(dir.join("uow.json"));
        // Clarifications persist too: every open structured question is a resumable
        // pause point, so the user can leave and come back to any unanswered question
        // (and answered ones stay on the record). Survives restarts; safe to delete.
        self.clarifications =
            crate::clarify::ClarificationStore::at(dir.join("clarifications.json"));
        // Phase 3b: resume contexts for runs paused on a clarification persist next to
        // the clarify store, so a parked run can still resume after a restart.
        self.clarify_resume =
            crate::clarify_resume::ClarifyResumeStore::at(dir.join("clarify-resume.json"));
        // The story spine must persist too: a UoW references its story by id, and
        // /api/uows resolves the WorkItem from the spine. An in-memory spine meant
        // restored UoWs rendered blank (and couldn't be run). Persist it alongside.
        self.stories = Arc::new(InMemoryStoryStore::at(dir.join("stories.json")));
        // Routines persist too, so a scheduled governed run an architect set up
        // survives a restart instead of being lost on every launch.
        self.routines = RoutineStore::at(dir.join("routines.json"));
        // Open human-review escalations survive a restart so a blocked routine isn't
        // silently un-blocked by quitting the app.
        self.escalations = crate::escalation::EscalationStore::at(dir.join("escalations.json"));
        self.checkpoints = crate::checkpoint::CheckpointStore::at(dir.join("checkpoints.json"));
        // Incremental-scan cache: per-project file fingerprints + last AI findings, so a
        // re-scan only re-audits changed files. Survives restarts; safe to delete.
        self.scan_cache =
            crate::scan_cache::ScanCacheStore::load_or_new(dir.join("scan-cache.json"));
        // Completed inspections (sealed report + provenance), one file per project. Unlike the
        // scan cache this is NOT disposable: it is what exports and the review screen are
        // built from once the client's source has been deleted.
        self.inspections = crate::inspections::InspectionStore::at(dir.join("inspections"));
        // The reviewer's decisions, one file per project beside the inspections. Not disposable:
        // a decision is the reviewer's work, and the release gate reads it.
        self.decisions = crate::decisions::DecisionStore::at(dir.join("decisions"));
        // The reviewer's edits, beside the decisions. Not disposable either.
        self.edits = crate::edits::EditStore::at(dir.join("edits"));
        // The Engine Check's verdicts, beside the edits. Not disposable either.
        self.engine_check = crate::engine_check::EngineCheckStore::at(dir.join("engine-check"));
        // The proof of what source deletion removed, and when source was fetched back.
        self.source_records = crate::client_source::records::SourceRecordStore::load_or_new(
            dir.join("source-records.json"),
        );
    }

    /// The REAL runtime state: a CLEAN SLATE (no seeded stories, clarifications,
    /// or routines) plus the provider selected from the environment (GitHub when
    /// `CAMERATA_GITHUB_TOKEN` is set, native otherwise). This is what `serve`
    /// uses, so the running app starts empty and fills only from real activity
    /// (adopting a tracker story, onboarding a repo) — nothing fake to mislead a
    /// connection test. `seeded()` remains for tests and the canned demos.
    pub fn from_env() -> Self {
        let mut state = Self::new(Arc::new(InMemoryStoryStore::new()));
        // The real credential store is installed FIRST so the provider is selected from the
        // GitHub token saved in the app (env only as the fallback), not from env alone.
        state.credential_store = Arc::new(crate::credentials::KeyringCredentialStore);
        state.provider = crate::provider::ProviderCell::new(ProviderHandle::from_credentials(
            state.credential_store.as_ref(),
        ));
        // Projects (their configs + pointers, NOT repo contents) persist across
        // launches in the per-user data dir, so an architect's projects survive a
        // restart. Falls back to an in-memory store if the data dir can't be
        // resolved (the app still runs; it just won't persist that session).
        if let Some(dir) = crate::app_dirs::camerata_dir() {
            state.open_json_stores(&dir);
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
                    handle.block_on(crate::enforcement_ledger::EnforcementLedger::open(
                        &ledger_path,
                    ))
                });

                // Governance-event audit trail (Phase H1 foundation): a READABLE SQLite
                // log of a governed run's lifecycle (run started, agent steps, gate
                // verdicts, escalations, sign-off, commit/PR gates, how it finished).
                // Opened best-effort, same fail-soft pattern as the enforcement ledger
                // above: on open failure, log a warning and leave `governance_log` as
                // `None` so startup and every run/scan path is unaffected. No lifecycle
                // site calls `record_governance` yet in this phase — that instrumentation
                // is a later phase; this just wires the plumbing.
                let governance_path = dir.join("governance_events.db");
                state.governance_log = match tokio::task::block_in_place(|| {
                    handle.block_on(camerata_persistence::GovernanceLog::open(&governance_path))
                }) {
                    Ok(log) => {
                        tracing::info!(
                            path = %governance_path.display(),
                            "governance-event log opened"
                        );
                        Some(Arc::new(log))
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %governance_path.display(),
                            error = %e,
                            "governance-event log could not open; governance events will \
                             not be recorded this session"
                        );
                        None
                    }
                };

                // Product-Owner feedback loop: the defect-report store (auto-capture +
                // click-to-report ingest). Opened best-effort, same fail-soft pattern as
                // the governance log above: on open failure, log a warning and leave
                // `feedback_store` as `None` so startup and every request path is
                // unaffected (the endpoints degrade to `{ ok: false, ... }` / empty
                // lists rather than a 500).
                let feedback_path = dir.join("feedback.db");
                state.feedback_store = match tokio::task::block_in_place(|| {
                    handle.block_on(camerata_persistence::FeedbackStore::open(&feedback_path))
                }) {
                    Ok(store) => {
                        tracing::info!(
                            path = %feedback_path.display(),
                            "feedback store opened"
                        );
                        Some(Arc::new(store))
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %feedback_path.display(),
                            error = %e,
                            "feedback store could not open; defect reports will not be \
                             recorded this session"
                        );
                        None
                    }
                };

                // The confidence engine's decision + calibration store. Opened
                // best-effort, same fail-soft pattern as the stores above: on open
                // failure, log a warning and leave `orchestrator_decisions` as `None`
                // so startup is unaffected (no lifecycle site calls
                // `record_orchestrator_decision` yet — that instrumentation is the
                // orchestrator-loop phase; this just wires the plumbing).
                let orchestrator_decisions_path = dir.join("orchestrator_decisions.db");
                state.orchestrator_decisions = match tokio::task::block_in_place(|| {
                    handle.block_on(camerata_persistence::OrchestratorDecisionLog::open(
                        &orchestrator_decisions_path,
                    ))
                }) {
                    Ok(log) => {
                        tracing::info!(
                            path = %orchestrator_decisions_path.display(),
                            "orchestrator-decision log opened"
                        );
                        Some(Arc::new(log))
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %orchestrator_decisions_path.display(),
                            error = %e,
                            "orchestrator-decision log could not open; decisions will not \
                             be recorded this session"
                        );
                        None
                    }
                };
            }
        }
        // Feature flags: load from .camerata/features.toml (CWD-relative) with env
        // overrides applied on top. Loaded last so the flags are available to every
        // handler via AppState from first request. Infallible: missing config = defaults.
        state.feature_flags = crate::feature_flags::FeatureFlags::load();
        // The ONE place the real home, cache and Claude Code locations are installed: source
        // deletion removes copies of client code from exactly these.
        state.storage_roots =
            Arc::new(crate::client_source::storage::StorageRoots::from_environment());
        // (The production credential store, the OS keychain, was installed at the top of this
        // function so the provider could be chosen from the GitHub token saved in the app.)
        // NOTE: the Anthropic API key is deliberately NOT copied into process env here (or
        // anywhere). Every model client reads it through `credentials::anthropic_api_key`
        // (credential store first, `ANTHROPIC_API_KEY` env as the fallback) at the moment it
        // is built, so a key saved in the app takes effect on the next call with no restart
        // and no process-global env write.
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
    // The operations that scan or write into a repo's source answer `409 source_deleted` when a
    // repo in their scope had its source deleted, before any handler tries to read the folder.
    let source_gate = axum::middleware::from_fn_with_state(
        state.clone(),
        crate::client_source::resolution::scope_gate,
    );
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
        // Governance-event audit trail read path (Phase H3): per-run + cross-run recent.
        .route("/api/runs/:id/events", get(get_run_events))
        .route("/api/governance/events", get(recent_governance_events))
        // Product-Owner feedback loop: defect-report ingest + read + status update.
        .route("/api/feedback", post(submit_feedback))
        .route("/api/projects/:id/feedback", get(project_feedback))
        .route("/api/feedback/recent", get(recent_feedback))
        .route("/api/feedback/:id/status", post(update_feedback_status))
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
        .route("/api/apps", post(create_app))
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
        // Writes into every repo's local clone (and clones a missing one): turned away with
        // `409 source_deleted` when any repo of the project had its source deleted.
        .route(
            "/api/projects/:id/emit-local",
            post(emit_project_local).layer(source_gate.clone()),
        )
        .route("/api/projects/:id/custom", post(add_custom_rule))
        .route("/api/projects/:id/custom/delete", post(delete_custom_rule))
        .route("/api/projects/:id/max-iterations", post(set_max_iterations))
        // Model-tiering: read/write the project's fast/balanced/strongest/vision model bindings (#63).
        .route("/api/projects/:id/tier-map", post(set_tier_map))
        // Designer (vision) band toggle: enable/disable the vision band for this project.
        .route("/api/projects/:id/vision-enabled", post(set_vision_enabled))
        .route("/api/projects/:id/product-brief", post(set_product_brief))
        .route("/api/projects/:id/client-name", post(set_client_name))
        .route(
            "/api/projects/:id/operating-principles",
            post(set_operating_principles),
        )
        .route("/api/projects/:id/memory", post(add_memory))
        .route(
            "/api/projects/:id/memory/:eid",
            post(patch_memory).delete(delete_memory),
        )
        // Per-step model config: set the model for one NON-FLEET AI step on this project.
        .route("/api/projects/:id/step-models", post(set_step_model))
        // Per-project backend (docs/design/2026-09-22_per-project-backend.md): Cli | Api.
        .route("/api/projects/:id/backend", post(set_backend_handler))
        .route(
            "/api/projects/:id/client-engagement",
            post(set_client_engagement_handler),
        )
        // Stall-detection thresholds: per-project idle timeout config.
        .route(
            "/api/projects/:id/stall-thresholds",
            post(set_stall_thresholds_handler),
        )
        // Audit-integrated alternative recommendation (docs/design/2026-09-22_audit-
        // integrated-alternatives.md): batched targeted rescan of operator-overridden
        // multi-option rules, and persisting the current per-rule selection.
        .route(
            "/api/projects/:id/rescan-alternatives",
            post(rescan_alternatives_handler).layer(source_gate.clone()),
        )
        .route(
            "/api/projects/:id/accept-alternatives",
            post(accept_alternatives_handler),
        )
        // L3 agentic code-review gate: per-project opt-in (R7).
        .route("/api/projects/:id/l3-review", post(set_l3_review_handler))
        // Work hierarchy schema (design-page work-type graph): read (GET) + replace (POST).
        .route(
            "/api/projects/:id/hierarchy",
            get(get_hierarchy_handler).post(set_hierarchy_handler),
        )
        // Model Efficiency Profile: preview (GET) and apply (POST).
        .route(
            "/api/projects/:id/model-profile/preview",
            get(preview_model_profile),
        )
        .route("/api/projects/:id/model-profile", post(apply_model_profile))
        // VCS-gate process-rule configuration + auditable bypass (issue #65).
        .route(
            "/api/projects/:id/process-rule-config",
            get(get_process_rule_config).post(set_process_rule_config_handler),
        )
        .route("/api/projects/:id/vcs-gate/bypass", post(vcs_gate_bypass))
        .route("/api/provider", get(provider_info))
        .route("/api/connections", get(connections_status))
        .route("/api/notifications", get(notifications_feed))
        .route("/api/stories/adopt", post(adopt_story))
        // GitHub Issue intake (#20): list a repo's open issues, then adopt one onto the spine.
        .route("/api/github/issues", get(github_issues_list))
        .route("/api/stories/adopt-issue", post(adopt_issue))
        .route(
            "/api/onboard/scan",
            post(onboard_scan).layer(source_gate.clone()),
        )
        .route(
            "/api/onboard/audit",
            post(onboard_audit).layer(source_gate.clone()),
        )
        .route(
            "/api/onboard/audit/start",
            post(onboard_audit_start).layer(source_gate.clone()),
        )
        .route(
            "/api/onboard/preflight",
            post(preflight::onboard_preflight).layer(source_gate.clone()),
        )
        .route("/api/onboard/audit/job/:id", get(onboard_audit_job))
        .route("/api/onboard/audit/job/:id/cancel", post(cancel_audit_job))
        // Saved inspections: one namespace, distinct from the agent runs under /api/runs. The two
        // `/api/runs/:id/plan|resume` paths the first cockpit build called stay as aliases of the
        // same handlers for one release; the cockpit itself now calls /api/inspections.
        .route("/api/inspections", get(list_saved_inspections))
        .route(
            "/api/projects/:id/decisions",
            get(crate::decisions::routes::get_decisions)
                .post(crate::decisions::routes::post_decisions),
        )
        .route(
            "/api/projects/:id/decisions/:decision_id/revoke",
            post(crate::decisions::routes::revoke_decision),
        )
        // The reviewer's edits, added findings, merges and splits, and the client's
        // acknowledgement of an accepted risk (design section 5).
        .route(
            "/api/projects/:id/decisions/:decision_id/client-ack",
            post(crate::edits::routes::client_ack),
        )
        .route(
            "/api/projects/:id/edits",
            get(crate::edits::routes::get_edits).post(crate::edits::routes::post_edit),
        )
        .route(
            "/api/projects/:id/edits/split",
            post(crate::edits::routes::post_split),
        )
        .route(
            "/api/projects/:id/edits/:edit_id/reapply",
            post(crate::edits::routes::reapply_edit),
        )
        .route(
            "/api/projects/:id/edits/:edit_id/discard",
            post(crate::edits::routes::discard_edit),
        )
        .route(
            "/api/projects/:id/added-findings",
            post(crate::edits::routes::post_added_finding),
        )
        .route(
            "/api/projects/:id/merge",
            post(crate::edits::routes::post_merge),
        )
        .route(
            "/api/projects/:id/reviewed-report",
            get(crate::edits::routes::get_reviewed_report),
        )
        .route("/api/inspections/:id/plan", get(run_resume_plan))
        .route("/api/inspections/:id/resume", post(run_resume_start))
        .route("/api/runs/:id/plan", get(run_resume_plan))
        .route("/api/runs/:id/resume", post(run_resume_start))
        .route("/api/onboard/finding-context", get(onboard_finding_context))
        .route(
            "/api/onboard/finding-context/batch",
            post(onboard_finding_context_batch),
        )
        .route("/api/onboard/source-file", get(onboard_source_file))
        .route("/api/onboard/symbol-refs", get(onboard_symbol_refs))
        .route("/api/onboard/finding-fix", get(onboard_finding_fix))
        .route("/api/git/detect-repo", post(detect_repo))
        .route("/api/gate-probe", post(gate_probe))
        .route("/api/onboard/ticket", post(onboard_ticket))
        .route(
            "/api/onboard/arm",
            post(onboard_arm).layer(source_gate.clone()),
        )
        .route(
            "/api/onboard/apply",
            post(onboard_apply).layer(source_gate.clone()),
        )
        .route(
            "/api/onboard/apply/preflight",
            post(onboard_apply_preflight).layer(source_gate.clone()),
        )
        .route(
            "/api/onboard/open-pr",
            post(onboard_open_pr).layer(source_gate.clone()),
        )
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
        .route(
            "/api/routines/templates/:id/instantiate",
            post(instantiate_routine_from_template),
        )
        .route("/api/routines/draft-prompt", post(draft_routine_prompt))
        .route(
            "/api/routines/:id",
            axum::routing::put(update_routine).delete(delete_routine),
        )
        .route("/api/routines/:id/enable", post(set_routine_enabled))
        .route("/api/routines/:id/provision", post(provision_routine))
        .route("/api/routines/:id/run", post(run_routine_now))
        .route("/api/routines/:id/runs", get(routine_runs))
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
        // The architect-orchestrator's first functional turn: a change request
        // against an EXISTING scaffolded project, driven end-to-end (real LLM +
        // real governed execution seam). See `crate::orchestrator_message`.
        .route("/api/orchestrator/message", post(orchestrator_message))
        .route("/api/usage", get(usage))
        .route("/api/models", get(list_models))
        .route("/api/models/registry", get(get_model_registry))
        .route("/api/models/registry/refresh", post(refresh_model_registry))
        // Feature A (live Anthropic `/v1/models`) manual refresh — the Anthropic sibling
        // of the OpenRouter refresh route above. See `refresh_anthropic_registry`.
        .route(
            "/api/models/registry/refresh/anthropic",
            post(refresh_anthropic_registry),
        )
        // Pass 2 (provider-safety UI): the picker's data source — per-provider
        // pricing + data-policy for one model, lazily fetched on first request.
        .route("/api/models/providers", get(get_model_providers))
        .route("/api/settings", get(get_settings))
        .route("/api/settings/workspace", post(set_workspace_root))
        .route("/api/settings/chat-model", post(set_chat_model))
        .route("/api/settings/chat-backend", post(set_chat_backend_handler))
        // Pass 2 (provider-safety UI): the settings-panel "Data safety" toggle + pin.
        .route(
            "/api/settings/provider-policy",
            post(set_provider_policy_handler),
        )
        .route(
            "/api/projects/:id/checkout",
            get(checkout_status).post(checkout_project),
        )
        // Derived readiness gate + "link existing local clone" (readiness-gate ADR).
        .route("/api/projects/:id/readiness", get(project_readiness))
        // Client source lifecycle (delete / re-fetch): docs/design/2026-10-24_delete-and-refetch-
        // client-source.md, section 3.
        .route(
            "/api/projects/:id/source",
            get(crate::client_source::handlers::get_source),
        )
        .route(
            "/api/projects/:id/source/records",
            get(crate::client_source::handlers::get_records),
        )
        .route(
            "/api/projects/:id/source/records/:record_id/text",
            get(crate::client_source::handlers::get_record_text),
        )
        .route(
            "/api/projects/:id/repos/:repo/source/remote",
            get(crate::client_source::refetch::remote_probe),
        )
        .route(
            "/api/projects/:id/repos/:repo/source/refetch",
            post(crate::client_source::refetch::refetch),
        )
        .route(
            "/api/projects/:id/repos/:repo/rescan-changes",
            post(crate::client_source::rescan::rescan_changes_handler),
        )
        .route(
            "/api/projects/:id/repos/:repo/source/delete-preview",
            post(crate::client_source::delete::delete_preview),
        )
        .route(
            "/api/projects/:id/repos/:repo/source/delete",
            post(crate::client_source::delete::delete),
        )
        .route("/api/projects/:id/repos/:repo/link", post(link_repo))
        .route("/api/projects/:id/reset-onboarding", post(reset_onboarding))
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
        .route("/api/projects/:id/git/fetch", post(git_fetch))
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
        .route("/api/workitems/assign", post(workitems_assign))
        .route(
            "/api/workitems/updated-check",
            post(workitems_updated_check),
        )
        .route("/api/workitems/set-parent", post(workitems_set_parent))
        .route("/api/me", get(me))
        .route("/api/uows", get(uows_list))
        .route("/api/uow/from-workitem", post(uow_from_workitem))
        // ── AI story authoring from a blank UoW (2026-06-22) ──────────────────
        .route("/api/uow/blank", post(uow_blank))
        .route("/api/uow/native", post(native_units::uow_native_create))
        .route("/api/uow/:story_id/author", post(uow_author))
        // ── UoW file attachments (2026-07-01) ─────────────────────────────────
        .route(
            "/api/uow/:story_id/attachments",
            get(uow_list_attachments).post(uow_add_attachment),
        )
        .route(
            "/api/uow/:story_id/attachments/:name",
            get(uow_get_attachment).delete(uow_remove_attachment),
        )
        // ── UoW Mermaid diagram (2026-07-01) ──────────────────────────────────
        .route(
            "/api/uow/:story_id/diagram",
            get(uow_get_diagram)
                .post(uow_generate_diagram)
                .delete(uow_clear_diagram),
        )
        // ── UoW mockup window (2026-07-01) ────────────────────────────────────
        .route("/api/uow/:story_id/mockup", post(uow_generate_mockup))
        // ── Design draft-tree (2026-07-01) ────────────────────────────────────
        .route("/api/designs/blank", post(design_blank))
        .route("/api/designs/:id/author", post(design_author))
        .route(
            "/api/designs/:id/nodes",
            get(design_list_nodes).post(design_materialize_nodes),
        )
        .route(
            "/api/designs/:id/nodes/:node_id",
            axum::routing::delete(design_delete_node),
        )
        .route("/api/designs/:id/publish", post(design_publish))
        .route("/api/designs/:id/status", post(design_set_status))
        .route("/api/designs/:id", axum::routing::delete(design_delete))
        .route("/api/projects/:id/designs", get(list_project_designs))
        .route(
            "/api/uow/:story_id/set-draft-parent",
            post(uow_set_draft_parent),
        )
        .route("/api/uow/:story_id/publish", post(uow_publish))
        .route("/api/uow", get(uow_list))
        .route("/api/uow/:story_id", get(uow_get).delete(uow_delete))
        .route("/api/uow/:story_id/status", post(uow_set_status))
        .route("/api/uow/:story_id/branch", post(uow_set_branch))
        .route(
            "/api/uow/:story_id/publish-repos",
            post(uow_set_publish_repos),
        )
        .route("/api/uow/:story_id/history", post(uow_append_history))
        // ── 3-phase cockpit state (#104 / #105) ───────────────────────────────
        .route(
            "/api/uow/:story_id/intake/context",
            post(uow_set_intake_context),
        )
        .route(
            "/api/uow/:story_id/intake/repos",
            post(uow_set_intake_repos),
        )
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
            "/api/uow/:story_id/review-findings/:index/accept",
            post(uow_accept_review_finding),
        )
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
        // BUG B: re-run investigation for a story ALREADY past Intake, without repeating
        // the Intake → Investigating transition (which would 409 a second call).
        .route(
            "/api/uow/:story_id/rerun-investigation",
            post(uow_rerun_investigation),
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
        // ── Applied-rule drift (project-scoped) ───────────────────────────────
        .route("/api/projects/:id/rule-drift", get(project_rule_drift))
        .route(
            "/api/projects/:id/rule-drift/:rule_id/accept",
            post(accept_rule_drift),
        )
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
        // ── Audit-report (PDF) export ─────────────────────────────────────────
        .route("/api/projects/:id/audit-report", post(export_audit_report))
        // ── Product export (PDF + Excel workbook, zipped) ─────────────────────
        .route("/api/projects/:id/product-export", post(export_product))
        // ── Remediation work: inspection findings become units of work (roadmap 2c) ──
        .route(
            "/api/projects/:id/remediation/plan",
            post(remediation::remediation_plan_preview),
        )
        .route(
            "/api/projects/:id/remediation/create",
            post(remediation::remediation_create),
        )
        // ── App-wide credential manager ───────────────────────────────────────
        // POST /api/credentials/:name  — store a credential (body: { "value": "…" })
        // DELETE /api/credentials/:name — remove a saved credential
        // GET  /api/credentials        — list all known credentials with masked values
        .route("/api/credentials", get(list_credentials))
        .route(
            "/api/credentials/:name",
            post(set_credential).delete(delete_credential),
        )
        // A request about an unknown native-shaped (`uow-...`) unit is a 404, never a write
        // that creates a phantom unit.
        // The review read model (WO9c): the routes live in `routes/review_routes.rs`.
        .merge(crate::routes::review_routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::native_units::unknown_native_unit_gate,
        ))
        .with_state(state)
}

// ── OpenRouter model-registry refresh triggers ──────────────────────────────────
//
// `POST /api/models/registry/refresh` (`refresh_model_registry` below) used to have ZERO
// callers: not startup, not credential-save, no UI. A user could save a valid OpenRouter
// key and never see an OpenRouter model in any picker until they manually curled the
// refresh endpoint. Three triggers now cover it:
//   1. Startup — `spawn_startup_openrouter_refresh`, called from `serve()`.
//   2. Credential save — `trigger_openrouter_refresh_if_needed`, called from
//      `set_credential` below, gated on the saved credential being `openrouter_api_key`.
//   3. Manual — the existing `POST /api/models/registry/refresh` endpoint, now also
//      reachable from a "Refresh models" button in the UI (`crates/ui/src/credentials.rs`).

/// Startup trigger (1 of 3). Fire-and-forget a best-effort OpenRouter registry refresh at
/// boot so a previously-saved key populates the picker without a manual POST. Spawned so
/// it never blocks server start; graceful when no key is configured because
/// `ModelRegistry::try_refresh_from_store` itself no-ops (no HTTP call, no error) in that
/// case — see `try_refresh_from_store_is_a_graceful_noop_with_no_key_configured` in
/// `crates/llm/src/model_registry.rs`, which exercises that exact no-key path directly and
/// synchronously (this function only wraps it in `tokio::spawn`, which cannot itself
/// panic, so there is nothing further to unit-test here without racing the task).
fn spawn_startup_openrouter_refresh(state: &AppState) {
    let model_registry = state.model_registry.clone();
    let credential_store = state.credential_store.clone();
    tokio::spawn(async move {
        model_registry
            .try_refresh_from_store(credential_store.as_ref())
            .await;
    });
}

/// True when saving credential `name` should trigger an OpenRouter model-registry
/// refresh. Extracted as its own pure, synchronous predicate — separate from
/// [`trigger_openrouter_refresh_if_needed`] — so the name-matching decision is testable
/// with a plain `assert!`, with no tokio runtime and no risk of a live network call.
fn is_openrouter_credential(name: &str) -> bool {
    name == crate::credentials::OPENROUTER_API_KEY
}

/// Credential-save trigger (2 of 3). Call after a credential write succeeds in
/// `set_credential`. When `name` is `openrouter_api_key`, fire-and-forget a registry
/// refresh so models populate immediately without a restart; any other credential name is
/// a no-op. Returns whether a refresh was scheduled, purely so tests can assert the
/// dispatch decision synchronously without awaiting (and thereby racing, or risking a live
/// HTTP call from) the spawned task itself — the task is never awaited here or by callers.
fn trigger_openrouter_refresh_if_needed(
    name: &str,
    model_registry: &crate::model_registry::ModelRegistry,
    credential_store: Arc<dyn crate::credentials::CredentialStore>,
) -> bool {
    if !is_openrouter_credential(name) {
        return false;
    }
    let registry = model_registry.clone();
    tokio::spawn(async move {
        registry
            .try_refresh_from_store(credential_store.as_ref())
            .await;
    });
    true
}

// ── Live Anthropic model-registry refresh triggers (Feature A) ──────────────────
//
// Mirrors the OpenRouter trigger trio immediately above, one-for-one, for the live
// Anthropic `/v1/models` catalog. See `docs/design/2026-08-27_backend-safety-and-live-
// models.md`'s Feature A section.
//   1. Startup — `spawn_startup_anthropic_refresh`, called from `serve()`.
//   2. Credential save — `trigger_anthropic_refresh_if_needed`, called from
//      `set_credential` below, gated on the saved credential being `anthropic_api_key`.
//   3. Manual — `POST /api/models/registry/refresh/anthropic`
//      (`refresh_anthropic_registry`), reachable from the "Refresh models" button beside
//      the Anthropic key field in `crates/ui/src/credentials.rs`.

/// Startup trigger (1 of 3). Fire-and-forget a best-effort live-Anthropic registry
/// refresh at boot so a previously-saved key populates the picker without a manual POST.
/// Spawned so it never blocks server start; graceful when no key is configured because
/// `ModelRegistry::try_refresh_anthropic_from_store` itself no-ops (no HTTP call, no
/// error) in that case.
fn spawn_startup_anthropic_refresh(state: &AppState) {
    let model_registry = state.model_registry.clone();
    let credential_store = state.credential_store.clone();
    tokio::spawn(async move {
        model_registry
            .try_refresh_anthropic_from_store(credential_store.as_ref())
            .await;
    });
}

/// True when saving credential `name` should trigger a live-Anthropic model-registry
/// refresh. Mirrors [`is_openrouter_credential`].
fn is_anthropic_credential(name: &str) -> bool {
    name == crate::credentials::ANTHROPIC_API_KEY
}

/// Credential-save trigger (2 of 3). Call after a credential write succeeds in
/// `set_credential`. When `name` is `anthropic_api_key`, fire-and-forget a live-Anthropic
/// registry refresh so the picker's model list populates immediately without a restart;
/// any other credential name is a no-op. Returns whether a refresh was scheduled, mirroring
/// [`trigger_openrouter_refresh_if_needed`]'s synchronous-assert-friendly contract.
fn trigger_anthropic_refresh_if_needed(
    name: &str,
    model_registry: &crate::model_registry::ModelRegistry,
    credential_store: Arc<dyn crate::credentials::CredentialStore>,
) -> bool {
    if !is_anthropic_credential(name) {
        return false;
    }
    let registry = model_registry.clone();
    tokio::spawn(async move {
        registry
            .try_refresh_anthropic_from_store(credential_store.as_ref())
            .await;
    });
    true
}

/// Bind `addr` and serve. The same entry point runs locally and in the cloud. The
/// provider is selected from the environment, so setting the GitHub vars switches the
/// whole BFF onto a real repo with no code change.
pub async fn serve(addr: &str) -> anyhow::Result<()> {
    let state = AppState::from_env();

    // OpenRouter model-registry refresh, trigger 1 of 3 (startup). The other two are the
    // post-credential-save trigger in `set_credential` and the manual "Refresh models" UI
    // button (`POST /api/models/registry/refresh`, unchanged). Before this, NOTHING ever
    // called the refresh automatically: a previously-saved key sat unused until someone
    // manually hit the refresh endpoint. Spawned (never blocks server start) and graceful
    // when no key is configured — `try_refresh_from_store`'s own guard returns `false`
    // immediately with no HTTP call in that case, so a fresh install neither errors nor
    // spams logs. See `try_refresh_from_store_is_a_graceful_noop_with_no_key_configured`
    // in `crates/llm/src/model_registry.rs` for the direct, non-racy test of that contract.
    spawn_startup_openrouter_refresh(&state);

    // Live Anthropic model-registry refresh (Feature A), trigger 1 of 3 (startup) — the
    // one-for-one Anthropic sibling of the OpenRouter startup refresh directly above. See
    // the block above `serve()` and `docs/design/2026-08-27_backend-safety-and-live-
    // models.md`. Graceful when no Anthropic key is configured (no-op, no HTTP call).
    spawn_startup_anthropic_refresh(&state);

    // Corpus freshness: ONE non-fatal log line if any rule domain is overdue, due soon, or has
    // never been confirmed by a sweep (design: docs/design/2026-10-20_corpus-staleness-check.md).
    // Spawned so it never delays binding the port; operator-only, never on a scan/export path.
    tokio::spawn(crate::corpus_freshness::log_at_startup());

    // Background event-ingest pollers (tracker events -> notification feed -> UI
    // toasts). Cadences are env-configurable; see crate::notify. Spawned here, not
    // in `router`, so unit tests that build the router don't start background work.
    crate::notify::spawn_tracker_poller(state.provider.clone(), state.notifications.clone());
    crate::notify::spawn_deploy_poller(state.notifications.clone());

    // Auto-fire scheduler: runs provisioned + enabled routines when their schedule
    // comes due. Spawned here (not in `router`) so tests that build the router don't
    // start firing routines. Cadence: CAMERATA_ROUTINE_TICK_SECS (default 60).
    crate::auto_fire::spawn_routine_scheduler(state.routines.clone(), state.escalations.clone());

    // Stall sweep (LIFECYCLE-6): periodically auto-cancel AUTONOMOUS runs that have stalled
    // (no activity past the active project's routine threshold). Watched runs stay alert-only.
    // Spawned here (not in `router`) so tests that build the router don't auto-cancel runs.
    // Cadence: CAMERATA_STALL_SWEEP_SECS (default 30).
    crate::stall_sweep::spawn_stall_sweep(
        state.runs.clone(),
        state.projects.clone(),
        state.governance_log.clone(),
    );

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
            let project_repos: Vec<String> =
                projects.list().into_iter().flat_map(|p| p.repos).collect();
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
                let Some(repo) = uow.repo().map(str::to_string) else {
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

                        // Pass 3 — T3 janitor startup sweep (disk-buildup guardrail,
                        // docs/design/2026-07-27_build-artifact-janitor.md): remove
                        // any `.camerata-worktrees/*` subdir that ISN'T a currently
                        // registered `git worktree` (a true orphan — e.g. leftover
                        // from a crash between admin-record pruning and disk cleanup,
                        // or predating per-stage teardown), then apply the same T2
                        // shared-target cap/age/orphan policy the teardown path uses.
                        // Best-effort + non-blocking-of-startup (this whole block
                        // already runs in a spawned background task); honors
                        // `CAMERATA_JANITOR` via the janitor functions themselves.
                        let mode = camerata_checks::janitor::janitor_mode();
                        if mode != camerata_checks::janitor::JanitorMode::Off {
                            let dry_run = mode == camerata_checks::janitor::JanitorMode::DryRun;
                            let worktrees_root = clone.join(".camerata-worktrees");
                            for orphan in
                                camerata_checks::janitor::list_orphan_worktree_dirs(&clone)
                            {
                                let outcome = camerata_checks::janitor::reclaim_dir(
                                    &orphan,
                                    &worktrees_root,
                                    &[],
                                    true,
                                    "startup-sweep",
                                    dry_run,
                                    |entry| {
                                        tracing::info!(
                                            path = %entry.path.display(),
                                            bytes = entry.bytes,
                                            "janitor: T3 startup sweep reclaimed orphan worktree"
                                        );
                                    },
                                );
                                if let camerata_checks::janitor::ReclaimOutcome::Skipped {
                                    reason,
                                    ..
                                } = outcome
                                {
                                    tracing::debug!(
                                        path = %orphan.display(),
                                        %reason,
                                        "janitor: T3 startup sweep skipped orphan candidate"
                                    );
                                }
                            }
                        }
                        crate::workspace::maybe_prune_shared_target(&clone, "startup-sweep").await;
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

async fn onboard_audit(
    State(state): State<AppState>,
    Json(req): Json<AuditReq>,
) -> Json<crate::onboard::ScanReport> {
    let run_controls = req.run_controls_for(&state);
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
    // An unknown scan mode is refused before anything runs (never a silent fallback).
    let requested_mode = match crate::ai_audit::ScanMode::parse(req.mode.as_deref()) {
        Ok(m) => m,
        Err(e) => {
            let mut r = crate::onboard::ScanReport::gated(&repos);
            r.gated = false;
            r.ai_error = Some(e.clone());
            r.message = Some(e);
            return Json(r);
        }
    };
    // The client-engagement lock, before any repository is located or read.
    if let Some(why) = client_lock_refusal_for_scan(
        state.projects.active().as_ref(),
        Some(&step_model_or(
            &state,
            crate::project::StepKind::Audit,
            req.model.as_deref(),
        )),
        Some(&step_model_or(
            &state,
            crate::project::StepKind::Calibration,
            req.calibration_model.as_deref(),
        )),
        req.deep,
        fix_step_model(&state).as_deref(),
    ) {
        return Json(client_lock_blocked_report(&repos, why));
    }
    // Local-first: the audit reads the repos' local working trees, not GitHub.
    let LocalSources {
        sources,
        mut notes,
        unresolved,
    } = resolve_local_sources(&state, &repos);
    if let Some(why) = partial_run_refusal(&unresolved, repos.len(), req.allow_partial) {
        // The same shape a client-engagement refusal has (`blocked`, with the reason), which the
        // desktop already renders as a refusal banner. A bare message on an otherwise empty
        // report would show as a successful scan that found nothing.
        return Json(client_lock_blocked_report(&repos, why));
    }
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
    // W4: `split_scannable_rules` no longer excludes anything on CI-tier grounds (see its doc
    // comment) — `excluded_mechanical` is always empty now. It still derives `preview_rules`
    // (the scan-runnable mechanical subset that feeds the SCAN-TIME PREVIEW pass below) and
    // loads `corpus` for reuse. `audit_repos` gets the FULL curated `selected` (unchanged from
    // before) — it needs every id, CI-tier included, both to arm the deterministic
    // architectural engine (`repo_selected_ids`) and, as of W4, to build its own semantic/AI
    // prompt (every selected, code-auditable rule now reaches the model).
    // W13: `corpus_load_failures` is the fifth element and MUST be consumed — see
    // `record_corpus_load_failures`'s call below and `split_scannable_rules`'s doc comment.
    let (_ai_scannable_only, excluded_mechanical, preview_rules, corpus, corpus_load_failures) =
        split_scannable_rules(selected.clone()).await;
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
    // Scan-type selector: resolve the two flags (both-false coerces to both-true).
    let (run_ai_review, run_deterministic, _coerced) =
        effective_scan_modes(req.run_ai_review, req.run_deterministic);
    // Fresh transcript for this audit run so the live feedback panel starts clean.
    state.transcripts.clear(SCAN_AUDIT_KEY);
    // Incremental scan: load this project's prior manifest unless the user forced a full scan.
    let active_project = state.projects.active();
    let project_id = active_project.as_ref().map(|p| p.id.clone());
    // This request holds the checkout open for as long as it runs. Register it, so deleting the
    // source is refused (not raced) until it returns; released when the handler does.
    let _scan_claim = project_id
        .as_deref()
        .map(|id| state.source_ops.begin_scan(id));
    let prior = if req.incremental {
        project_id
            .as_deref()
            .and_then(|id| state.scan_cache.get(id))
    } else {
        None
    };
    // The compliance-safety gate: resolve BEFORE the scan runs, from this project's own
    // `backend` setting and whether an Anthropic key is present. See
    // `resolve_backend_for_project`'s doc comment.
    let backend_resolution = resolve_backend_for_project(&state, active_project.as_ref());
    // Rank 2: with no mode forced, the API backend runs the scan as a Message Batch.
    let mode = scan_mode_for_run(
        requested_mode,
        &backend_resolution,
        run_controls.efficiency,
        &mut notes,
    );
    // This request has no job to record a batch id on, and a batch can run for hours: a dropped
    // connection would orphan a running, billed batch. Batch runs on the job path only.
    let mode = synchronous_scan_mode(mode, &mut notes);

    // PANIC GUARD: this handler has no background job to fail into (unlike
    // `onboard_audit_start`'s spawned task) — a bare panic anywhere in the pipeline below
    // would otherwise unwind straight through the handler, dropping the connection and
    // leaving the caller with an opaque network error instead of an explained failure.
    // `catch_unwind` turns that into a normal JSON response carrying `ai_error`, so the
    // SAME banner the UI already renders for an AI-review failure covers a crash too —
    // never a silent, blank result. `AssertUnwindSafe` is required because the block
    // below captures locals (accumulating `report`, loop state) that aren't automatically
    // UnwindSafe; nothing here is observed again after a panic except through `report`,
    // which this function constructs fresh either way (real or synthesized), so there is
    // no stale/partially-mutated state a caller could see.
    use futures::FutureExt;
    // The review tree's stage 2 reads the final report with the run's own controls, and never
    // runs where the compliance gate blocked the AI review.
    let review_controls = run_controls.clone();
    let ai_review_blocked = matches!(
        &backend_resolution,
        crate::llm::BackendResolution::Blocked { .. }
    );
    let review_backend = match &backend_resolution {
        crate::llm::BackendResolution::Api => crate::llm::Backend::Api,
        _ => crate::llm::Backend::Cli,
    };
    let outcome = std::panic::AssertUnwindSafe(async move {
        let (mut report, mut manifest) = crate::onboard::audit_repos_controlled(
            state.credential_store.as_ref(),
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
            backend_resolution,
            corpus.as_ref(),
            &active_project
                .as_ref()
                .map(project_chosen_options)
                .unwrap_or_default(),
            run_controls,
        )
        .await;
        // A repo that could not be read is on the report as not inspected, never just absent,
        // and the manifest is corrected for it before it is stored.
        record_unresolved_repos_before_storing(
            &mut report,
            &mut manifest,
            &unresolved,
            &selected,
            corpus.as_ref(),
        );
        // Persist the fresh manifest (even after a forced full scan) so the NEXT scan can be
        // incremental. Only when there's an active project to key it to.
        if let Some(id) = &project_id {
            state.scan_cache.put(id, manifest);
        }
        report.excluded_mechanical_rules = excluded_mechanical;
        // W13: as soon as a ScanReport (and therefore a ScanLedger) exists, disclose every
        // corpus file that would not parse — into the ledger, the integrity gate, one concise
        // client line and the operator log. Before this, those errors were discarded at the
        // load site and a rule with a typo'd enum value vanished from the scan silently.
        record_corpus_load_failures(&mut report, &corpus_load_failures);
        // The engagement this scan ran under, with the models that actually ran.
        report
            .ledger
            .record_engagement(engagement_record_for_scan(active_project.as_ref(), &report));
        // SCAN-TIME deterministic PREVIEW pass: run the selected mechanical rules' own tools
        // and fold their findings into triage as preview findings (advisory, not enforced).
        // Gated on the deterministic selection — deselecting deterministic scans skips it too.
        // (No job here, so no live deterministic progress on the synchronous path.)
        if run_deterministic {
            merge_scan_preview(
                &mut report,
                &sources,
                &preview_rules,
                corpus.as_ref(),
                None,
                project_client_tooling(active_project.as_ref()),
            )
            .await;
        }

        // ── Always-on dep-audit (osv-scanner) — runs LAST, hard-bounded ──────────────
        // Runs after preview linters so a slow osv-scanner never blocks them.
        // Hard 60-second total timeout (provisioning + scan); fail-soft on timeout.
        // Gated on run_deterministic; CAMERATA_DISABLE_DEP_AUDIT bypasses for test isolation.
        if run_deterministic {
            use tokio::time::{timeout, Duration};
            let dep_audit_timeout = Duration::from_secs(60);
            // Pass-1 W1 seal fix: snapshotted so the whole loop's net additions get ONE ledger
            // stage entry (a no-op when every repo's dep-audit found nothing new) — see
            // `crate::scan_ledger::record_addition_stage`'s doc comment.
            // Runtime and framework end of life and known runtime vulnerabilities: the rows join
            // the enrichment range below, so a known-exploited runtime vulnerability is labelled
            // and floored exactly like a dependency one.
            let runtime_rows_in = report.findings.len();
            crate::runtime_audit::run_runtime_audit(&mut report, &sources).await;
            let dep_audit_rows_in = report.findings.len();
            let mut osv_runs: Vec<crate::vuln_intel::OsvRun> = Vec::new();
            for (spec, dir) in &sources {
                let spec = spec.as_str();
                let run = crate::dep_audit::run_dep_audit(spec, dir);
                let (dep_findings, dep_note) = match timeout(dep_audit_timeout, run).await {
                    Ok(result) => result,
                    Err(_elapsed) => (
                        Vec::new(),
                        Some(crate::onboard::CoverageNote {
                            tool_failed: false,
                            repo: None,
                            tool: "osv-scanner".to_string(),
                            message: format!(
                                "dependency audit (osv-scanner) timed out after 60 s for {spec}"
                            ),
                        }),
                    ),
                };
                let dep_note = dep_note.map(|mut note| {
                    note.repo.get_or_insert_with(|| spec.to_string());
                    note
                });
                osv_runs.push(crate::vuln_intel::OsvRun::from_note(
                    spec,
                    dep_note.as_ref(),
                ));
                if !dep_findings.is_empty() {
                    report.findings.extend(dep_findings);
                }
                if let Some(note) = dep_note {
                    report.coverage_notes.push(note);
                }
            }
            // KEV / EPSS enrichment and the "vulnerability data as of" stamp, once per run over
            // the rows this loop added (the one implementation, shared with the CLI).
            // ORDER: this MUST stay before `apply_scan_severity_bounds` below. The KEV floor to
            // critical is written first and the corpus bounds get the last word; see
            // `vuln_intel::apply` and `tests/vuln_intel_call_order.rs`.
            crate::vuln_intel::enrich_dependency_findings(&mut report, runtime_rows_in, &osv_runs)
                .await;
            crate::scan_ledger::record_addition_stage(
                &mut report.ledger,
                "dependency-audit-merge",
                dep_audit_rows_in,
                report.findings.len(),
                "dep_audit_findings",
            );
        }

        // ── Pass-1 W1 seal fix: evidence-of-violation gate — LAST row-adding pass ────
        // Runs strictly after every other pass above (the floor/AI/architectural merges
        // inside `audit_repos`, the preview-linter merge, the dependency audit) and strictly
        // before `report` is persisted into `last_scan`/`scan_cache` or returned to the
        // caller — this IS the "row set is final" point for this scan. See
        // `report_export::apply_evidence_gate`'s doc comment.
        crate::report_export::apply_evidence_gate(&mut report);
        // Corpus severity bounds over the FINAL row set — see the block comment on the async
        // path below and `severity_bounds`.
        crate::severity_bounds::apply_scan_severity_bounds(&mut report.findings, corpus.as_ref());

        // ── Stage 2, the group reviewer: the architect's first reader of each rule group ──
        // On the final row set, with the run's own controls (see the job path).
        if run_ai_review && !ai_review_blocked {
            let rules: Vec<(String, String)> = selected
                .iter()
                .map(|r| (r.id.clone(), r.directive.clone()))
                .collect();
            let llm = crate::onboard::build_llm(
                state.credential_store.as_ref(),
                review_backend,
                Some(state.usage_ledger.clone()),
            );
            crate::ai_audit::group_review::review_scan_report(
                &mut report,
                &llm,
                &review_controls,
                &rules,
                review_backend == crate::llm::Backend::Cli,
                None,
            )
            .await;
        }

        // ── Enforcement-catch ledger capture (terminal point 2: scan completion) ─────
        // After a scan completes, write one floor/catch record for each ACTIVE floor
        // finding to the enforcement ledger. Best-effort / fail-soft: runs in a
        // background task so the response is never delayed by ledger writes. Errors
        // inside are logged and swallowed.
        {
            let ledger = state.enforcement_ledger.clone();
            let all_findings = report.findings.clone();
            tokio::spawn(async move {
                crate::enforcement_ledger::capture_scan_findings(&ledger, &all_findings).await;
            });
        }

        // ── Write last_scan (synchronous path) ───────────────────────────────────────
        // Store the completed report immediately so chat grounding can read it regardless
        // of whether the UI has round-tripped the draft back yet (timing race fixed).
        report.assign_site_keys();
        if let Some(id) = &project_id {
            state.set_last_scan(id.clone(), report.clone());
        }

        report
    })
    .catch_unwind()
    .await;

    let report = match outcome {
        Ok(report) => report,
        Err(panic) => {
            let msg = panic_message(&panic);
            crate::onboard::ScanReport::crashed(
                &repos,
                format!(
                    "The scan crashed unexpectedly: {msg}. Please retry; if it keeps \
                     happening, this is a bug worth reporting."
                ),
            )
        }
    };

    Json(report)
}

/// Start the audit job for `req` and return its id. The asynchronous body of
/// [`onboard_audit_start`], shared with the delta rescan (`client_source::rescan`) so there is
/// ONE implementation of "run an audit as a job". See [`AuditJobOptions`].
pub(crate) async fn start_audit_job(
    state: &AppState,
    req: AuditReq,
    options: AuditJobOptions,
) -> String {
    let state = state.clone();
    let run_controls = req.run_controls_for(&state);
    // What the run was asked, saved with the run so it can be resumed with the same request.
    let request_json = serde_json::to_value(&req).unwrap_or(serde_json::Value::Null);
    let resume_run = options.resume_run.clone();
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
    // W4: see the sync `onboard_audit` handler's identical comment (a few hundred lines up) —
    // `excluded_mechanical` is always empty now (nothing is excluded on CI-tier grounds);
    // `preview_rules` keeps its original, narrower meaning (the scan-runnable mechanical
    // subset for the SCAN-TIME PREVIEW below). `audit_repos` gets the FULL `selected` — it
    // needs every id both to arm the deterministic architectural engine and to build its own
    // semantic/AI prompt, which now includes every selected, code-auditable rule.
    // W13: `corpus_load_failures` is the fifth element and MUST be consumed — see
    // `record_corpus_load_failures`'s call below and `split_scannable_rules`'s doc comment.
    let (_ai_scannable_only, excluded_mechanical, preview_rules, corpus, corpus_load_failures) =
        split_scannable_rules(selected.clone()).await;
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
    let requested_mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref());
    let thorough = req.thorough;
    let deep = req.deep;
    // Scan-type selector: resolve the two flags (both-false coerces to both-true).
    let (run_ai_review, run_deterministic, _coerced) =
        effective_scan_modes(req.run_ai_review, req.run_deterministic);

    // Capture the project up front (the named one, else the active one): the job is stamped
    // with it (ROUTES-5, so a project's deep-report export finds ITS OWN latest deep report)
    // AND it seeds the incremental-scan manifest lookup below.
    let active_project = options
        .project_id
        .as_deref()
        .and_then(|id| state.projects.get(id))
        .or_else(|| state.projects.active());
    let project_id = active_project.as_ref().map(|p| p.id.clone());
    // The client-engagement lock, before any repository is located or read: the job fails at
    // once with the reason, and nothing is spawned.
    if let Some(why) = client_lock_refusal_for_scan(
        active_project.as_ref(),
        model.as_deref(),
        calibration_model.as_deref(),
        deep,
        fix_step_model(&state).as_deref(),
    ) {
        let job_id = state.jobs.create("audit", project_id);
        state.jobs.fail(&job_id, why);
        return job_id;
    }
    // Local-first: resolve each repo's local working tree up front (the spawned job owns them).
    let LocalSources {
        sources,
        mut notes,
        unresolved,
    } = resolve_local_sources(&state, &repos);
    if let Some(why) = partial_run_refusal(&unresolved, repos.len(), req.allow_partial) {
        let job_id = state.jobs.create("audit", project_id);
        state.jobs.fail(&job_id, why);
        return job_id;
    }
    notes.extend(options.extra_notes);
    let delta = options.delta;
    let client_tooling = project_client_tooling(active_project.as_ref());
    // Moved into the spawned task, which records the engagement in the scan's ledger.
    let engagement_project = active_project.clone();
    let job_id = state.jobs.create("audit", project_id.clone());
    state.transcripts.clear(SCAN_AUDIT_KEY);
    // The compliance-safety gate: resolved up front (same as the synchronous `onboard_audit`
    // handler) from this project's `backend` setting and key presence, then moved into the
    // spawned task below.
    let backend_resolution = resolve_backend_for_project(&state, active_project.as_ref());
    // An unknown scan mode fails the job before anything runs (never a silent fallback); with
    // no mode forced, the API backend runs the scan as a Message Batch (rank 2).
    let mode = match requested_mode {
        Ok(requested) => scan_mode_for_run(
            requested,
            &backend_resolution,
            run_controls.efficiency,
            &mut notes,
        ),
        Err(e) => {
            state.jobs.fail(&job_id, e);
            return job_id;
        }
    };
    // Audit-integrated alternative recommendation: this project's persisted chosen options,
    // computed up front (like `backend_resolution`) and moved into the spawned task below.
    let chosen_options = active_project
        .as_ref()
        .map(project_chosen_options)
        .unwrap_or_default();

    let jobs = state.jobs.clone();
    let transcripts = state.transcripts.clone();
    let jid = job_id.clone();
    let prior = options.prior_override.or_else(|| {
        if req.incremental {
            project_id
                .as_deref()
                .and_then(|id| state.scan_cache.get(id))
        } else {
            None
        }
    });
    let scan_cache = state.scan_cache.clone();
    let soc2_enabled = state.feature_flags.soc2;
    // Captured for the spawned task so the async audit's model calls feed the cumulative meter.
    let usage_ledger = state.usage_ledger.clone();
    // Captured so the async path records the finished inspection (in-memory last_scan, the
    // project-agnostic recent_scan fallback, and the persisted inspection entry) through the
    // ONE shared writer, `AppState::set_last_scan`, the instant the report is final.
    let scan_state = state.clone();
    tokio::spawn(async move {
        // PANIC GUARD (see `run_job_body_with_panic_guard`'s doc comment for the full
        // rationale): everything below runs the entire AI + deterministic pipeline in this
        // detached background task. `body_jobs` / `body_jid` are clones moved into the
        // body itself so it can report progress and its own terminal state; the outer
        // `jobs` / `jid` stay behind for the guard to fail the job on a panic the body
        // never got to handle.
        let body_jobs = jobs.clone();
        let body_jid = jid.clone();
        run_job_body_with_panic_guard(&jobs, &jid, async move {
            if sources.is_empty() {
                body_jobs.fail(
                    &body_jid,
                    "No local repos to audit. Browse to each repo's local folder first.",
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
                let dep_audit_disabled = std::env::var(crate::dep_audit::DISABLE_ENV_VAR)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if sources.len() > 1 {
                    // Several repos: every repo gets its own rows (`owner/repo:tool`), declared
                    // from that repo's own languages and the rules bound to it, so a tool that
                    // will not run on a repo is not left waiting in the denominator.
                    for (spec, dir) in &sources {
                        let dir_clone = dir.clone();
                        let languages = tokio::task::spawn_blocking(move || {
                            crate::onboard::files::read_local_repo_files(&dir_clone)
                                .map(|ex| crate::scan_tools::languages_from_files(&ex.files))
                                .unwrap_or_default()
                        })
                        .await
                        .unwrap_or_default();
                        let for_repo: Vec<crate::onboard::SelectedRule> = preview_rules
                            .iter()
                            .filter(|r| r.applies_to(spec))
                            .cloned()
                            .collect();
                        let mut tool_ids: Vec<String> = vec!["floor".to_string()];
                        tool_ids.extend(crate::scan_tools::preview_tool_ids_for_rules(
                            &for_repo,
                            &lookup,
                            (!languages.is_empty()).then_some(&languages),
                        ));
                        if !dep_audit_disabled {
                            tool_ids.push("dep-audit".to_string());
                        }
                        let id_refs: Vec<&str> = tool_ids.iter().map(String::as_str).collect();
                        body_jobs.declare_repo_tools(&body_jid, spec, &id_refs);
                    }
                } else {
                    // Derive the UNION of all languages present across every source repo.
                    // Stack-gating uses this union so the pre-declared tool count matches the
                    // tools that `merge_scan_preview` will actually run (which gates per-repo
                    // but adds to the same job). Using the union is conservative: a tool
                    // present in ANY repo stays in the denominator.
                    let mut union_languages = std::collections::HashSet::<String>::new();
                    for (_, dir) in &sources {
                        let dir_clone = dir.clone();
                        if let Ok(Ok(extracted)) = tokio::task::spawn_blocking(move || {
                            crate::onboard::files::read_local_repo_files(&dir_clone)
                        })
                        .await
                        {
                            union_languages
                                .extend(crate::scan_tools::languages_from_files(&extracted.files));
                        }
                    }
                    let lang_gate = if union_languages.is_empty() {
                        None
                    } else {
                        Some(&union_languages)
                    };
                    let mut tool_ids: Vec<String> = vec!["floor".to_string()];
                    let preview_ids = crate::scan_tools::preview_tool_ids_for_rules(
                        &preview_rules,
                        &lookup,
                        lang_gate,
                    );
                    tool_ids.extend(preview_ids);
                    if !dep_audit_disabled {
                        tool_ids.push("dep-audit".to_string());
                    }
                    let id_refs: Vec<&str> = tool_ids.iter().map(String::as_str).collect();
                    body_jobs.declare_tools(&body_jid, &id_refs);
                }
            }

            // The run is saved from the start, so a failed part or an interruption costs only
            // that part (see `crate::run_store`); a resume ("Run what remains") opens the saved run
            // through the same function the command line uses.
            use crate::run_store as rs;
            let source_dirs: Vec<std::path::PathBuf> =
                sources.iter().map(|(_, dir)| dir.clone()).collect();
            let project_key = if source_dirs.len() == 1 {
                crate::scan_cache::state_dir::repo_state_key(&source_dirs[0])
            } else {
                crate::scan_cache::state_dir::project_state_key(&source_dirs)
            };
            let audit_model = crate::ai_audit::resolve_audit_model(model.as_deref());
            let ai_scan_key = crate::scan_cache::AiScanKey::new(
                &audit_model,
                calibration_model.as_deref(),
                &selected,
            );
            // The resolved ids (what each stage calls), so a resume notices a changed default.
            let models_record = rs::ModelsRecord::from_picks(
                model.as_deref(),
                calibration_model.as_deref(),
                &run_controls,
                match &backend_resolution {
                    crate::llm::BackendResolution::Api => "api",
                    _ => "cli",
                },
                &format!("{mode:?}").to_ascii_lowercase(),
            );
            let mut prior = prior;
            let run_store = match &resume_run {
                Some(id) => match rs::open_for_resume(id, &sources, &ai_scan_key, &models_record) {
                    Ok((store, _estimate)) => {
                        // The baseline the run started from, not one an attempt since replaced.
                        prior = store.load_prior_manifest();
                        Some(store)
                    }
                    Err(e) => {
                        body_jobs.fail(&body_jid, e.to_string());
                        return;
                    }
                },
                None => match rs::RunStore::create(rs::NewRun {
                    project_key: project_key.clone(),
                    repos: sources.clone(),
                    models: models_record,
                    ai_scan_key,
                    args: request_json,
                    estimate_usd: None,
                    include_alternatives: corpus.is_some(),
                    origin: rs::RunOrigin::Cockpit,
                    project_id: project_id.clone(),
                }) {
                    Ok(store) => {
                        if let Some(p) = &prior {
                            store.save_prior_manifest(p);
                        }
                        let _ = rs::prune_runs(&project_key, rs::DEFAULT_KEEP_RUNS);
                        Some(store)
                    }
                    Err(e) => {
                        eprintln!(
                            "[camerata-server] this scan cannot be saved ({e}); it runs, but it \
                             cannot be resumed"
                        );
                        None
                    }
                },
            };
            // The run's own delimiter seed, carried with the audit future below (never global).
            let token_seed = run_store.as_ref().map(|s| s.nonce_seed());
            let sessions = crate::llm::SessionSink::new();
            let llm_backend = match &backend_resolution {
                crate::llm::BackendResolution::Api => crate::llm::Backend::Api,
                _ => crate::llm::Backend::Cli,
            };
            let real = crate::onboard::build_llm(
                state.credential_store.as_ref(),
                llm_backend,
                Some(usage_ledger.clone()),
            )
            .with_session_sink(sessions.clone());
            let journaled = run_store
                .as_ref()
                .map(|s| rs::journal::wrap(&real, s.clone()));
            let model_seam: &dyn crate::llm::LlmPort = match &journaled {
                Some(j) => j.as_ref(),
                None => &real,
            };
            // The review tree's stage 2 reads the final report with the run's own controls, and
            // never runs where the compliance gate blocked the AI review.
            let review_controls = run_controls.clone();
            let ai_review_blocked = matches!(
                &backend_resolution,
                crate::llm::BackendResolution::Blocked { .. }
            );
            let (mut report, mut manifest) = crate::checkpoint::with_run_checkpoints(
                run_store.as_ref().map(|s| s.checkpoint_dir()),
                crate::prompt_frame::with_seed(
                    token_seed,
                    crate::onboard::audit_repos_with_llm_controlled(
                        model_seam,
                        &sources,
                        &selected,
                        notes,
                        model.as_deref(),
                        calibration_model.as_deref(),
                        mode,
                        thorough,
                        Some((&transcripts, SCAN_AUDIT_KEY)),
                        Some((&body_jobs, &body_jid)),
                        prior.as_ref(),
                        deep,
                        soc2_enabled,
                        run_ai_review,
                        run_deterministic,
                        backend_resolution,
                        corpus.as_ref(),
                        &chosen_options,
                        run_controls,
                    ),
                ),
            )
            .await;
            report.provenance.model_sessions = sessions.snapshot();
            // A repo that could not be read is on the report as not inspected, never just
            // absent, and the manifest is corrected for it before it is stored.
            record_unresolved_repos_before_storing(
                &mut report,
                &mut manifest,
                &unresolved,
                &selected,
                corpus.as_ref(),
            );
            // Persist the fresh manifest so the next scan can be incremental.
            if let Some(id) = &project_id {
                scan_cache.put(id, manifest);
            }
            report.excluded_mechanical_rules = excluded_mechanical;
            // W13: see the sync `onboard_audit` handler's identical call — disclose every
            // corpus file that would not parse into the ledger, the integrity gate, one
            // concise client line and the operator log.
            record_corpus_load_failures(&mut report, &corpus_load_failures);
            report.ledger.record_engagement(engagement_record_for_scan(
                engagement_project.as_ref(),
                &report,
            ));
            // SCAN-TIME deterministic PREVIEW pass (advisory; not enforced until wired). Gated on
            // the deterministic selection; reports per-tool progress into the job so the cockpit's
            // deterministic progress view shows each preview tool start/run/done live.
            if run_deterministic {
                merge_scan_preview(
                    &mut report,
                    &sources,
                    &preview_rules,
                    corpus.as_ref(),
                    Some((&body_jobs, &body_jid)),
                    client_tooling,
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
                // Pass-1 W1 seal fix: see the identical comment in `onboard_audit`'s synchronous
                // path — one ledger stage entry for the whole loop's net additions.
                // Runtime end of life and runtime vulnerabilities (see `onboard_audit`).
                let runtime_rows_in = report.findings.len();
                crate::runtime_audit::run_runtime_audit(&mut report, &sources).await;
                let dep_audit_rows_in = report.findings.len();
                let mut osv_runs: Vec<crate::vuln_intel::OsvRun> = Vec::new();
                for (spec, dir) in &sources {
                    let spec = spec.as_str();
                    let run = crate::dep_audit::run_dep_audit(spec, dir);
                    let (dep_findings, dep_note) = match timeout(dep_audit_timeout, run).await {
                        Ok(result) => result,
                        Err(_elapsed) => (
                            Vec::new(),
                            Some(crate::onboard::CoverageNote {
                                tool_failed: false,
                                repo: None,
                                tool: "osv-scanner".to_string(),
                                message: format!(
                                    "dependency audit (osv-scanner) timed out after 60 s for {spec}"
                                ),
                            }),
                        ),
                    };
                    let dep_note = dep_note.map(|mut note| {
                        note.repo.get_or_insert_with(|| spec.to_string());
                        note
                    });
                    let dep_count = dep_findings.len();
                    let dep_key =
                        crate::jobs::progress_tool_key(spec, "dep-audit", sources.len() > 1);
                    body_jobs.det_tool_running(&body_jid, &dep_key);
                    body_jobs.det_tool_done(&body_jid, &dep_key, dep_count);
                    osv_runs.push(crate::vuln_intel::OsvRun::from_note(
                        spec,
                        dep_note.as_ref(),
                    ));
                    if !dep_findings.is_empty() {
                        report.findings.extend(dep_findings);
                    }
                    if let Some(note) = dep_note {
                        report.coverage_notes.push(note);
                    }
                }
                // KEV / EPSS enrichment and the "vulnerability data as of" stamp, once per run
                // (see the identical call in `onboard_audit`). The job receives the dependency
                // rows only after enrichment, so the live view never shows an unlabelled row. The
                // wait is bounded: both feeds share `vuln_intel::ENRICHMENT_BUDGET` (45 s), after
                // which a feed counts as failed (cache, or not checked) and the rows are released.
                crate::vuln_intel::enrich_dependency_findings(
                    &mut report,
                    runtime_rows_in,
                    &osv_runs,
                )
                .await;
                if report.findings.len() > runtime_rows_in {
                    body_jobs.add_findings(&body_jid, report.findings[runtime_rows_in..].to_vec());
                }
                crate::scan_ledger::record_addition_stage(
                    &mut report.ledger,
                    "dependency-audit-merge",
                    dep_audit_rows_in,
                    report.findings.len(),
                    "dep_audit_findings",
                );
            }

            // ── Pass-1 W1 seal fix: evidence-of-violation gate — LAST row-adding pass ────
            // Mirrors the synchronous `onboard_audit` handler's own call site exactly — see
            // `report_export::apply_evidence_gate`'s doc comment. Must run before `report` is
            // written into `last_scan`/`recent_scan` or handed to `body_jobs.finish` below.
            crate::report_export::apply_evidence_gate(&mut report);
            // Corpus-authored severity bounds (`min_severity` floor, `max_severity` ceiling,
            // `deviation_ceiling`) over the FINAL row set. `audit_repos` already bounded the
            // floor/architectural/AI rows, but the preview-linter merge, the dependency audit
            // and the evidence gate all append rows AFTER it; without this the cockpit would
            // show those at the producer's raw severity while the PDF/xlsx bound them. The
            // call is idempotent, so the rows `audit_repos` already bounded are unchanged.
            crate::severity_bounds::apply_scan_severity_bounds(
                &mut report.findings,
                corpus.as_ref(),
            );

            // ── Stage 2, the group reviewer: the architect's first reader of each rule group ──
            // Runs on the final row set, so it sees every row the earlier passes added. Its
            // failed parts join the report's before the run is judged complete below.
            if run_ai_review && !ai_review_blocked {
                let rules: Vec<(String, String)> = selected
                    .iter()
                    .map(|r| (r.id.clone(), r.directive.clone()))
                    .collect();
                // With the run's own delimiter seed in scope, like the audit above, so a resumed
                // run asks the group reviewer the same questions and its stored replies match.
                crate::checkpoint::with_run_checkpoints(
                    run_store.as_ref().map(|s| s.checkpoint_dir()),
                    crate::prompt_frame::with_seed(
                        token_seed,
                        crate::ai_audit::group_review::review_scan_report(
                            &mut report,
                            model_seam,
                            &review_controls,
                            &rules,
                            llm_backend == crate::llm::Backend::Cli,
                            Some((&body_jobs, &body_jid)),
                        ),
                    ),
                )
                .await;
            }

            // The run is complete when no part failed or was left out; a scan that is not can be
            // resumed with "Run what remains". The export is the operator's own later step.
            if let Some(store) = &run_store {
                report.run_id = Some(store.run_id());
                crate::run_store::finish_run(store, &report, true);
                // What the run wants the operator to know (a model call made with no prompt
                // seed in scope, say) goes on the report the cockpit shows, not run.json alone.
                crate::run_store::note_report(store, &mut report);
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
            report.assign_site_keys();
            match (&project_id, &delta) {
                // A delta retest is recorded as its own kind of inspection, naming the
                // inspection it was computed from and the two commits between.
                (Some(id), Some(d)) => {
                    // The rule fingerprint of the inspection this retest was computed from, so a
                    // unit's evidence can say when the rule selection changed in between.
                    let base_rules_fingerprint = scan_state
                        .inspections
                        .list(id)
                        .into_iter()
                        .find(|e| e.id == d.base_inspection_id)
                        .map(|e| e.rules_fingerprint)
                        .unwrap_or_default();
                    if let Some(entry) = scan_state.record_inspection(
                        id,
                        &report,
                        crate::inspections::InspectionKind::DeltaRetest {
                            base_inspection_id: d.base_inspection_id.clone(),
                            from_sha: d.from_sha.clone(),
                            to_sha: d.to_sha.clone(),
                        },
                    ) {
                        // Units created from findings learn which of their findings are gone.
                        crate::remediation::record_retest_evidence(
                            &scan_state.uow,
                            id,
                            &entry,
                            &base_rules_fingerprint,
                        );
                    }
                    scan_state.set_last_scan_in_memory(id.clone(), report.clone());
                }
                (Some(id), None) => scan_state.set_last_scan(id.clone(), report.clone()),
                // No project: only the project-agnostic fallback can hold it, and there is no
                // project to key a persisted inspection to.
                (None, _) => scan_state.set_recent_scan(report.clone()),
            }

            body_jobs.finish(&body_jid, report);
        })
        .await;
    });

    job_id
}

/// `DELETE /api/credentials/:name` — remove a saved credential from the OS keychain.
///
/// A credential saved in the app takes precedence over its env var, so this is the way to hand
/// a credential back to the env fallback (or to remove it entirely). Removing one that is not
/// set succeeds. Response: `{ "ok": true }`.
async fn delete_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    use crate::credentials::ALL_CREDENTIALS;
    if !ALL_CREDENTIALS.contains(&name.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("unknown credential name: {name}")
            })),
        );
    }
    if let Err(e) = state.credential_store.delete(&name) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to remove credential: {e}")
            })),
        );
    }
    reselect_provider_if_github(&name, &state);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
}

/// Check the latest Camerata release on GitHub and return a JSON object with
/// the version comparison. Returns `None` on network / auth failure.
async fn check_github_release(
    creds: &dyn crate::credentials::CredentialStore,
) -> Option<serde_json::Value> {
    let current = env!("CARGO_PKG_VERSION");
    // The GitHub releases API for the camerata-orchestrator repo.
    let url = "https://api.github.com/repos/zernst3/camerata-orchestrator/releases/latest";
    let token = crate::credentials::github_token(creds);

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
async fn export_deep_report(State(state): State<AppState>, Path(id): Path<String>) -> Response {
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

    // ROUTES-5: find THIS project's most recent completed deep report (filtered by
    // project_id, newest by completion time). Previously this returned an arbitrary job's
    // deep report regardless of which project it belonged to.
    let deep_report = state.jobs.latest_deep_report(&id);
    let Some(deep) = deep_report else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "ok": false,
                "message": "No deep-tier report available for this project. Run an audit with `deep=true` first."
            })),
        )
            .into_response();
    };

    let markdown = deep_report_markdown(deep, state.feature_flags.soc2);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        markdown,
    )
        .into_response()
}

/// The deep report as the Markdown a client receives: the lens prose is model-written text read
/// back from the job store, so it is redacted here, at the export, whatever path stored it.
fn deep_report_markdown(mut deep: crate::ai_audit::DeepReport, soc2_enabled: bool) -> String {
    crate::onboard::redact::redact_deep_report(&mut deep);
    render_deep_report_markdown(&deep, soc2_enabled)
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

// ── Audit-report (PDF) export ────────────────────────────────────────────────

/// Request body for `POST /api/projects/:id/audit-report`. The reviewer's decisions now live in
/// the server-side [`crate::decisions::DecisionStore`]. `dispositions` is accepted for one more
/// release so an older client still works: it is used only while the store has no records for
/// the project, and ignored (with any disagreement logged) once it does.
#[derive(serde::Deserialize)]
struct AuditReportReq {
    #[serde(default)]
    dispositions: std::collections::HashMap<String, crate::report_export::DispositionWire>,
    #[serde(default)]
    options: crate::report_export::ReportOptions,
}

/// `POST /api/projects/:id/audit-report` — export the project's last scan as a
/// client-facing/board-forwardable PDF. Body: `{ dispositions, options }` (see
/// [`AuditReportReq`]). Re-derives the report from `last_scan` + the POSTed dispositions on
/// every call; nothing is persisted server-side (re-export = re-run this handler).
///
/// 404s when the project doesn't exist or has no completed scan yet. 500s (with a plain-
/// text-ish JSON message) when `typst` isn't installed or the compile otherwise fails —
/// see `report_export::compile_pdf`'s fail-soft contract.
async fn export_audit_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut req): Json<AuditReportReq>,
) -> Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    let Some(project) = state.projects.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "no such project" })),
        )
            .into_response();
    };

    // A damaged decision or edit file means the reviewer's work is unknown: nothing is exported
    // from guesses.
    if state.decisions.is_corrupt(&id) {
        return crate::decisions::routes::corrupt_response();
    }
    if state.edits.is_corrupt(&id) {
        return crate::edits::routes::corrupt_response();
    }

    let Some(report) = state.get_last_scan(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "ok": false,
                "message": "No scan results available for this project. Run an audit first."
            })),
        )
            .into_response();
    };
    // Pass-1 W1 seal fix: `last_scan` is written only AFTER the scan pipeline's own row-adding
    // passes (the external-tool preview merge, the dependency audit, the evidence-of-violation
    // gate) have all already run — see `onboard_audit_start`'s doc comments at its own
    // `apply_evidence_gate` call site. Sealing it here, immediately on retrieval and before any
    // further processing, is therefore the correct "row set is final" point for this handler:
    // every exporter below reads the identical sealed set.
    let report = report.seal_with_restored(&crate::decisions::restored_keys_of(
        &state.decisions.active(&id),
    ));

    // Best-effort corpus load for the citation join (§4.4) — mirrors `split_scannable_rules`'
    // own fallback: a missing/unreadable corpus degrades every citation to "AI-advisory,
    // model-inferred." rather than failing the export.
    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };

    // Branding (2026-09-13 review): fold CAMERATA_REPORT_BRAND/CAMERATA_REPORT_PREPARED_BY into
    // any blank fields the export dialog didn't POST, BEFORE build_report_json — which stays
    // pure/no-I/O itself (see `ReportOptions::apply_env_defaults`'s doc comment).
    req.options.apply_env_defaults();
    // `resolve_fix`'s chosen-option fallback (audit-integrated alternatives) — server-
    // populated, never client-supplied (see `ReportOptions::chosen_options`'s doc comment).
    req.options.chosen_options = project_chosen_options(&project);
    // Confidentiality: the client name comes from the project's inspection setup, and a blank
    // one blocks the export before anything is built.
    req.options.client_name = project.client_name.clone();
    if let Err(e) = crate::report_export::require_client_name(&req.options) {
        return client_name_required_response(&e);
    }
    // And no report for fewer repos than the project holds.
    req.options.expected_repos = project.repos.clone();
    // A project that is not a client engagement is the operator's own review: its exports may
    // come from a run that did not complete, marked as such.
    req.options.internal = !project.client_engagement;
    if let Err(e) = crate::report_export::require_all_repos(&report, &req.options) {
        return repositories_missing_response(&e);
    }
    if let Err(e) = crate::report_export::require_complete_run(&report, &req.options) {
        return run_incomplete_response(&e);
    }

    // The reviewer's overlay (decisions, edits, additions, merges, splits) over the sealed rows:
    // after the seal, before the gate, in every export.
    let reviewed = reviewed_for_export(&state, &id, &report, &req.dispositions);

    let json = crate::report_export::build_report_json(&reviewed, corpus.as_ref(), &req.options);

    let data_json = match crate::report_export::render_data_json(&json) {
        Ok(data_json) => data_json,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
            )
                .into_response();
        }
    };
    // The release gate's secrets check over the PDF's data, BEFORE it is compiled: this route
    // ships a PDF outside any zip, and `data.json` is the only data the template reads.
    let stem = report_filename_stem_for(reviewed.report(), &req.options.project_title);
    if let Err(failure) = crate::release_gate::check_pdf_data(&stem, &data_json) {
        return release_blocked_response(&failure);
    }
    if let Err(failure) = crate::release_gate::check_unsynced(
        &stem,
        &reviewed.report().findings,
        &req.options.unsynced_changes,
    ) {
        return release_blocked_response(&failure);
    }
    let pdf_bytes = match crate::report_export::compile_pdf_from_data(&data_json).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
            )
                .into_response();
        }
    };

    let filename = format!("{stem}.pdf");

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        pdf_bytes,
    )
        .into_response()
}

/// The reviewer's overlay over a sealed report, for an export: the decisions and edits of the
/// project's stores applied with [`crate::report_export::apply_overlay`]. The request's own
/// dispositions (an older client's) are used only while the decision store holds no live record
/// for the project, and any disagreement with the store is logged
/// ([`crate::decisions::effective_dispositions`]).
fn reviewed_for_export(
    state: &AppState,
    project_id: &str,
    sealed: &crate::onboard::SealedReport,
    request_dispositions: &std::collections::HashMap<String, crate::report_export::DispositionWire>,
) -> crate::report_export::ReviewedReport {
    let effective = crate::decisions::effective_dispositions(
        &state.decisions,
        project_id,
        request_dispositions,
    );
    effective.log_disagreements(project_id);
    let reviewed = crate::report_export::apply_overlay(
        sealed,
        &state.decisions.list(project_id),
        &state.edits.list(project_id),
        &state.edits.added(project_id),
    );
    if state.decisions.has_active_records(project_id) {
        reviewed
    } else {
        reviewed.with_legacy_dispositions(effective.map)
    }
}

/// `camerata-inspection-{repo-slug}-{short-sha}` (no extension) — the shared filename stem for
/// EVERY inspection-export artifact (the standalone PDF, and the product-export ZIP; the
/// workbook inside the zip gets its own `-findings` infix, see [`export_product`]). Kept as
/// ONE function so the PDF route and the product-export route can never derive two
/// different names for what is, underneath, the exact same scan. P6 (2026-09-29): renamed from
/// `camerata-audit-*` — the product is "Codebase Inspection" and this stem is client-visible
/// (the actual downloaded filename), not an internal-only identifier.
///
/// `pub`: also called directly by `camerata inspect` (the headless CLI export path,
/// `camerata-cli/src/inspect_cmd.rs`) so its zip's filename stem can never drift from this
/// HTTP handler's — same function, not a re-derived copy.
pub fn report_filename_stem(report: &crate::onboard::ScanReport) -> String {
    report_filename_stem_for(report, "")
}

/// [`report_filename_stem`] for a project titled `project_title`. A project of several repos is
/// named for the project and the number of repos (`camerata-inspection-acme-shop-3-repos`), not
/// for its first repo and that repo's commit, which would label three services' report with one
/// of them. A blank title falls back to the owner the repos share, then to `multi`. A project of
/// one repo keeps the repo and commit name it always had.
pub fn report_filename_stem_for(
    report: &crate::onboard::ScanReport,
    project_title: &str,
) -> String {
    if report.repos.len() > 1 {
        let slug = slugify_for_filename(project_title);
        let slug = if slug.is_empty() {
            shared_owner_slug(&report.repos).unwrap_or_else(|| "multi".to_string())
        } else {
            slug
        };
        return format!("camerata-inspection-{slug}-{}-repos", report.repos.len());
    }
    let repo_slug = report
        .repos
        .first()
        .cloned()
        .unwrap_or_else(|| "repo".to_string())
        .replace('/', "-");
    let short_sha = report
        .provenance
        .audited_refs
        .first()
        .and_then(|r| r.sha.as_deref())
        .map(|s| s.chars().take(7).collect::<String>())
        .unwrap_or_else(|| "nosha".to_string());
    format!("camerata-inspection-{repo_slug}-{short_sha}")
}

/// Lower-case letters and digits of `text`, joined by single hyphens: a filename-safe slug.
fn slugify_for_filename(text: &str) -> String {
    let mut out = String::new();
    let mut pending_hyphen = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_hyphen = true;
        }
    }
    out
}

/// The owner every `owner/repo` in `repos` shares, slugified, when there is one.
fn shared_owner_slug(repos: &[String]) -> Option<String> {
    let mut owners = repos.iter().filter_map(|r| r.split('/').next());
    let first = owners.next()?;
    owners
        .all(|o| o == first)
        .then(|| slugify_for_filename(first))
        .filter(|s| !s.is_empty())
}

/// `POST /api/projects/:id/product-export` — the primary export button's target: a ZIP
/// containing the SAME curated PDF `/audit-report` produces, a fully-formatted Excel
/// workbook (the uncut dataset — every finding, false positives on their own sheet,
/// per-category sheets, a full coverage sheet), and a short `README.txt` manifest. Body,
/// 404/500 contract, and re-derivation-on-every-call semantics are identical to
/// [`export_audit_report`] (see [`AuditReportReq`]) — this handler just builds BOTH
/// artifacts from the same `build_report_json` pass instead of one.
///
/// If `compile_pdf` fails (typst missing/erroring), the whole export fails with the same
/// 500 message the PDF-only route uses today — a half-product (xlsx-only zip) is a support
/// headache, and the error message already tells the user how to fix it (install typst).
async fn export_product(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut req): Json<AuditReportReq>,
) -> Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    let Some(project) = state.projects.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "ok": false, "message": "no such project" })),
        )
            .into_response();
    };

    // A damaged decision or edit file means the reviewer's work is unknown: nothing is exported
    // from guesses.
    if state.decisions.is_corrupt(&id) {
        return crate::decisions::routes::corrupt_response();
    }
    if state.edits.is_corrupt(&id) {
        return crate::edits::routes::corrupt_response();
    }

    let Some(report) = state.get_last_scan(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "ok": false,
                "message": "No scan results available for this project. Run an audit first."
            })),
        )
            .into_response();
    };
    // Pass-1 W1 seal fix: see the identical comment in `export_audit_report` — this is the
    // "row set is final" point for every artifact this handler builds (PDF, xlsx,
    // findings.json), all three now reading the SAME sealed set.
    let report = report.seal_with_restored(&crate::decisions::restored_keys_of(
        &state.decisions.active(&id),
    ));

    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };

    // Branding (2026-09-13 review): same env-default fold as `export_audit_report` — the
    // xlsx/findings.json siblings this handler also builds read `opts.prepared_by` directly
    // (`xlsx_export.rs`), so resolving it here once covers all three artifacts.
    req.options.apply_env_defaults();
    // `resolve_fix`'s chosen-option fallback (audit-integrated alternatives) — server-
    // populated, never client-supplied. Covers all three artifacts built below.
    req.options.chosen_options = project_chosen_options(&project);
    // The backend the scan ran on and whether it was a client engagement, recorded in the
    // manifest and README. Server-populated, never client-supplied. The scan's own ledger record
    // wins (the project's settings may have changed since); the project is the fallback for a
    // scan recorded before the ledger carried it.
    req.options.model = Some(match report.ledger.engagement() {
        Some(e) => ModelRecord {
            backend: e.backend.clone(),
            provider: e.provider.clone(),
            models: Vec::new(),
            client_engagement: e.client_engagement,
        },
        None => ModelRecord::anthropic(project.backend)
            .with_client_engagement(project.client_engagement),
    });
    // Confidentiality: the client name comes from the project's inspection setup. The package
    // assembly refuses a blank one (mapped to a 422 below).
    req.options.client_name = project.client_name.clone();
    // The repos the project holds: the package assembly refuses a report that covers fewer.
    req.options.expected_repos = project.repos.clone();
    req.options.internal = !project.client_engagement;

    // ONE assembly (`release_gate::build_product_package`) and ONE path to zip bytes
    // (`release_gate::release`), shared by this handler, `camerata inspect --export` and the
    // sample generator.
    // The reviewer's overlay (decisions, edits, additions, merges, splits) over the sealed rows:
    // after the seal, before the gate, in every export.
    let reviewed = reviewed_for_export(&state, &id, &report, &req.dispositions);
    let package =
        match crate::release_gate::build_product_package(&reviewed, corpus.as_ref(), &req.options)
            .await
        {
            Ok(package) => package,
            Err(e) => {
                // The assembly checks the PDF's data before it compiles it; a refusal there is the
                // same blocked response as one from `release`.
                if let Some(failure) = crate::release_gate::gate_failure_of(&e) {
                    return release_blocked_response(failure);
                }
                if let Some(missing) = e.downcast_ref::<crate::report_export::ClientNameRequired>()
                {
                    return client_name_required_response(missing);
                }
                if let Some(missing) = e.downcast_ref::<crate::report_export::RepositoriesMissing>()
                {
                    return repositories_missing_response(missing);
                }
                if let Some(incomplete) = e.downcast_ref::<crate::report_export::RunIncomplete>() {
                    return run_incomplete_response(incomplete);
                }
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
                )
                    .into_response();
            }
        };
    let released = match if req.options.internal {
        crate::release_gate::release_internal(package)
    } else {
        crate::release_gate::release(package)
    } {
        Ok(released) => released,
        Err(crate::release_gate::ReleaseError::Blocked(failure)) => {
            return release_blocked_response(&failure);
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "ok": false, "message": e.to_string() })),
            )
                .into_response();
        }
    };
    // Proof the gate ran, visible to the cockpit without reading the zip.
    let release_checks = released.report.check_names().join(", ");
    // Remember what was delivered on the inspection it came from (hash and manifest). A failure
    // to save never blocks the export the operator asked for.
    if let Err(e) = state
        .inspections
        .set_package(&id, released.package_record())
    {
        tracing::warn!(project = %id, "could not record the delivered package: {e}");
    }
    let stem = released.stem;
    let zip_bytes = released.bytes;

    let filename = format!("{stem}.zip");

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (
                header::HeaderName::from_static("x-camerata-release-checks"),
                release_checks,
            ),
        ],
        zip_bytes,
    )
        .into_response()
}

/// The 422 the export routes return when the inspection setup has no client name:
/// `{ ok: false, gate: "client_name", message }`, the message saying where to enter it.
fn client_name_required_response(missing: &crate::report_export::ClientNameRequired) -> Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "ok": false,
            "gate": "client_name",
            "message": missing.to_string(),
        })),
    )
        .into_response()
}

/// The 422 the export routes return when the run did not complete and the export is for a client:
/// `{ ok: false, gate: "incomplete_run", message, reasons }`. The message carries the same
/// reasons the CLI prints, so the cockpit shows the operator why nothing was exported.
fn run_incomplete_response(incomplete: &crate::report_export::RunIncomplete) -> Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "ok": false,
            "gate": "incomplete_run",
            "message": incomplete.to_string(),
            "reasons": incomplete.reasons,
        })),
    )
        .into_response()
}

/// The 422 the export routes return when the report covers fewer repos than the project holds:
/// `{ ok: false, gate: "repositories", message, repos }`.
fn repositories_missing_response(missing: &crate::report_export::RepositoriesMissing) -> Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "ok": false,
            "gate": "repositories",
            "message": missing.to_string(),
            "repos": missing.repos,
        })),
    )
        .into_response()
}

/// The 422 the export routes return when the release gate refuses: `{ ok: false, gate:
/// "release", stem, message, problems: [{check, file, location, message, evidence}] }`. The
/// message is the gate's own fail-loud block and every field is already redacted, so the body
/// can be shown verbatim (and copied by the operator) without ever carrying a secret value.
fn release_blocked_response(failure: &crate::release_gate::GateFailure) -> Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "ok": false,
            "gate": "release",
            "stem": failure.stem_for_display(),
            "message": failure.to_string(),
            "problems": failure.report.problems_json(),
        })),
    )
        .into_response()
}

/// The README.txt manifest bundled inside the product-export zip: what each file is, who
/// it's for, the provenance lines, and the advisory disclaimer (verbatim, reused from
/// `report_export::AUDIT_REPORT_DISCLAIMER` — never a second, drifting copy of that
/// paragraph).
///
/// The provenance lines (repos, generated-at, commit, engine version, corpus version, rules
/// last reviewed) come from [`ManifestProvenance::readme_lines`], the human rendering of the
/// SAME struct `manifest.json`'s `inspection` block is rendered from
/// ([`ManifestProvenance::inspection_json`]), so the two cannot drift. Pass the same value to
/// [`build_product_zip`].
///
/// `pub`: reused verbatim by `camerata inspect` (see [`report_filename_stem`]'s doc comment)
/// so the headless export's README text can never diverge from the HTTP product-export's.
pub fn product_export_readme(stem: &str, provenance: &ManifestProvenance) -> String {
    // A project of several repos gets one more paragraph: where the services differ on a rule.
    let patterns = if provenance.repos.len() > 1 {
        "Patterns across services\n\
         \x20 Where the services of this project differ on a rule: a sheet of that name in the\n\
         \x20 workbook, a section in the PDF and a patterns_across_services key in findings.json.\n\
         \x20 An observation, not a finding: it has no severity and is in no count.\n\
         \n"
    } else {
        ""
    };
    format!(
        "Codebase Inspection - Product Export\n\
         =====================================\n\
         \n\
         This ZIP contains the artifacts below, all derived from the SAME inspection scan:\n\
         \n\
         {stem}.pdf\n\
         \x20 The curated NARRATIVE report: cover, executive summary, category scorecard,\n\
         \x20 severity x effort matrix, curated findings with citations and recommended\n\
         \x20 fixes, what's healthy, dependency snapshot, and methodology. Intended for a\n\
         \x20 board / buyer / decision-maker. False positives are excluded throughout and\n\
         \x20 counted once, in the methodology section.\n\
         \n\
         {stem}-findings.xlsx\n\
         \x20 The COMPLETE working dataset: every finding as its own row, sortable and\n\
         \x20 filterable, with a per-category sheet, a Dependencies sheet, a Coverage sheet\n\
         \x20 (every rule inspected this run, found or not), and a False Positives sheet\n\
         \x20 (with the reviewer's exclusion reasons, nothing is silently dropped). Intended\n\
         \x20 for the engineers doing remediation. The Remediation plan sheet comes first: one\n\
         \x20 row for each sub-group, in the order to fix them. The Sites sheet lists every\n\
         \x20 site, and the Tree sheet shows how the sites are grouped.\n\
         \n\
         findings.json\n\
         \x20 The MACHINE-READABLE version of the xlsx's \"All Findings\" data: the same\n\
         \x20 rows (severity, category, repo/path/line, snippet, detail, recommended fix,\n\
         \x20 disposition, effort, confidence, citation, ...), wrapped with the report's\n\
         \x20 provenance and summary counts. Intended for scripting / CI ingestion / a\n\
         \x20 client's own tooling. Never disagrees with the xlsx, since both are built\n\
         \x20 from the same pass over the scan.\n\
         \n\
         decisions.json\n\
         \x20 Every decision record the report was reviewed with: what was ruled on which\n\
         \x20 site or group of sites, and whether a reason was recorded. The Decisions sheet\n\
         \x20 of the workbook is the same list.\n\
         \n\
         csv/tickets.csv\n\
         \x20 One ticket for each sub-group (one kind of violation with one cause) that still\n\
         \x20 has sites to fix. A site marked a false positive or accepted as a risk is on no\n\
         \x20 ticket. Columns: Title, Description, Priority, Labels, Estimate, Group id, Site\n\
         \x20 count and Sites (the first 50 sites, then a pointer to the Sites sheet filtered\n\
         \x20 on the Group id). To import the file:\n\
         \x20 - Jira: map Title to Summary, Description to Description, Priority to Priority\n\
         \x20   (Highest, High, Medium or Low), Labels to Labels (split on commas) and Sites to\n\
         \x20   a text field of your choice. Estimate is text, not a duration.\n\
         \x20 - Linear: Title, Description and Labels map to the fields of the same name.\n\
         \x20   Priority maps as Highest to Urgent, High to High, Medium to Medium and Low to\n\
         \x20   Low.\n\
         \x20 - GitHub Issues: map Title to Title, Description to Body and Labels to Labels\n\
         \x20   (comma separated). Priority and Estimate have no field there, so put them in\n\
         \x20   the body or drop them.\n\
         \x20 Keep Group id in a custom field or in the body: it is the value to filter the\n\
         \x20 Sites sheet on.\n\
         \n\
         \n\
         {}\
         {}\
         \n\
         {}\n",
        patterns,
        provenance.readme_lines(),
        crate::report_export::audit_report_disclaimer(provenance.repos.len()),
    )
}

/// The current wall-clock time as a `zip::DateTime`, for stamping every entry's
/// last-modified field with the actual build time.
///
/// The `zip` crate's `SimpleFileOptions::default()` leaves `last_modified_time` at its own
/// `Default` — the DOS epoch, 1980-01-01 — unless a caller sets one explicitly. `zip`'s own
/// `DateTime::default_for_write()` helper would paper over this, but only when the crate's
/// `time` (or `chrono`) cargo feature is enabled; this workspace builds `zip` with
/// `default-features = false, features = ["deflate"]`, so that helper silently falls back to
/// the same 1980 default. Building the `DateTime` by hand from `chrono` (already a
/// `camerata-server` dependency, independent of `zip`'s own optional features) avoids adding
/// a cargo feature just for this. Falls back to the DOS epoch only if the current date is
/// somehow outside the format's representable range (year 1980..=2107) — never panics.
fn zip_now() -> zip::DateTime {
    use chrono::{Datelike, Timelike};
    let now = chrono::Utc::now();
    zip::DateTime::from_date_and_time(
        now.year() as u16,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    )
    .unwrap_or_default()
}

/// One inspected repository in the manifest's `inspection.repos`: its name and the exact commit
/// the scan read, so a recipient can tie the findings to a state of the code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvenanceRepo {
    /// `owner/repo`, as on the report cover.
    pub name: String,
    /// Full commit SHA (`git rev-parse HEAD`) the scan recorded for this repo. `None` when the
    /// source dir was not a git repo or the scan could not read it: an honest absence, never a
    /// guess.
    pub commit: Option<String>,
    /// True when the working tree had uncommitted changes at scan time, so `commit` is the base
    /// the scanned code was modified from, not an exact description of it. Always `false` while
    /// `commit` is `None` (there is nothing to be dirty relative to).
    pub dirty: bool,
}

/// Which model backend and provider produced an inspection, recorded in the export so a client
/// can see the chain their code went through (`api` / `anthropic` for a client inspection).
///
/// `provider` is an open string, not an enum: a future Bedrock-served run is recorded by
/// constructing `ModelRecord::new(backend, "bedrock")`, with no change to the manifest, the
/// README or the release gate's agreement check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRecord {
    /// `ProjectBackend::provenance_name()`: `api` or `cli`.
    pub backend: String,
    /// The model provider, e.g. [`ModelRecord::ANTHROPIC`].
    pub provider: String,
    /// Every model the inspection actually ran, by role, in a stable order. Filled from the
    /// scan's own provenance by [`ModelRecord::with_models_from`] when the package is built, so
    /// it states what ran, never what was configured. Empty when no model ran.
    pub models: Vec<crate::client_lock::ModelRole>,
    /// The inspection was a client engagement ([`crate::client_lock`]).
    pub client_engagement: bool,
}

impl ModelRecord {
    /// The Anthropic provider (the Messages API, and Claude Code, which is Anthropic's too).
    pub const ANTHROPIC: &'static str = "anthropic";

    pub fn new(backend: crate::llm::ProjectBackend, provider: &str) -> Self {
        Self {
            backend: backend.provenance_name().to_string(),
            provider: provider.to_string(),
            models: Vec::new(),
            client_engagement: false,
        }
    }

    /// `backend` served by Anthropic: today's only provider.
    pub fn anthropic(backend: crate::llm::ProjectBackend) -> Self {
        Self::new(backend, Self::ANTHROPIC)
    }

    /// This record, marked as a client engagement or not.
    pub fn with_client_engagement(mut self, client_engagement: bool) -> Self {
        self.client_engagement = client_engagement;
        self
    }

    /// This record with `models` replaced by the models the scan recorded as having run: the
    /// scan pass, calibration and the clone judge, each only when present.
    pub fn with_models_from(mut self, provenance: &crate::onboard::ScanProvenance) -> Self {
        self.models = [
            ("the scan", &provenance.audit_model),
            ("calibration", &provenance.calibration_model),
            ("the clone judge", &provenance.clone_judge_model),
        ]
        .into_iter()
        .filter_map(|(role, model)| {
            let model = model.as_deref()?.trim();
            (!model.is_empty()).then(|| crate::client_lock::ModelRole {
                role: role.to_string(),
                model: model.to_string(),
            })
        })
        .collect();
        self
    }
}

/// Provenance for the product ZIP's `manifest.json` `inspection` block (design:
/// `docs/design/2026-10-20_machine-readable-exports.md`). The hard fields are `Option` so the
/// manifest's SHAPE is always complete even when a value was not recorded. `engine_version` is
/// not here: the manifest fills it from the server crate's own `CARGO_PKG_VERSION`, so it can
/// never drift from the binary that produced the package.
///
/// Both export entry points (`export_product`, `camerata inspect --export`) build this through
/// [`crate::client_provenance::export_provenance`], the one place that fills every field.
#[derive(Debug, Clone, Default)]
pub struct ManifestProvenance {
    /// Repositories inspected, each with the commit the scan recorded for it.
    pub repos: Vec<ProvenanceRepo>,
    /// ISO/human generated-at, from the report cover.
    pub generated_at: String,
    /// A deterministic content digest of the rule corpus, when computed
    /// ([`camerata_rules::corpus_version`]).
    pub corpus_version: Option<String>,
    /// The client-visible staleness date ("rules last reviewed on …"): the oldest effective
    /// review date across the rules the scan used
    /// (assembled in [`crate::client_provenance`]). `None` while any used rule has never
    /// been reviewed: an honest absence, never a fabricated date.
    pub rules_last_reviewed: Option<String>,
    /// The model backend and provider that ran the inspection. `None` when the caller did not
    /// record one (an honest absence: `null` in the manifest, no README line).
    pub model: Option<ModelRecord>,
    /// True when the operator's spend limit stopped part of this inspection, so it is
    /// INCOMPLETE. States only that fact: no dollar figure is exported (cost numbers are
    /// operator-only). What was not run is listed in the report's failed-pass section.
    pub cost_cap_reached: bool,
    /// True when the spend limit stopped work because a call's cost could not be determined (the
    /// limit could not be enforced), not because it was reached. Meaningful only with
    /// [`Self::cost_cap_reached`]; it selects the README line ([`COST_CAP_UNENFORCED_README_LINE`]
    /// instead of [`COST_CAP_README_LINE`]).
    pub cost_cap_unenforced: bool,
    /// "Vulnerability data as of": per feed (CISA KEV, FIRST EPSS, OSV), the date the data
    /// behind the dependency findings was current as of, or plainly that the feed was not
    /// checked. `None` renders as every feed not checked (never an omitted line). Filled from
    /// `ScanProvenance::vulnerability_data`; rendered by [`crate::vuln_intel`].
    pub vulnerability_data: Option<crate::vuln_intel::VulnDataStamp>,
    /// 22c: who the inspection was prepared for (the inspection setup's client name, the same
    /// name the confidentiality marking prints). `None` only for a caller that set none.
    pub client_name: Option<String>,
    /// 22c: how many corpus rules the inspection applied (the distinct audited rule ids found
    /// in the loaded corpus). `None` when no corpus was loaded.
    pub corpus_rules_applied: Option<usize>,
    /// 22c: the non-blank lines the inspection read, the figure `camerata count` reports and a
    /// quote is priced on (same file filter, same line definition). `None` when the run read
    /// no local files.
    pub counted_lines: Option<usize>,
    /// 22c: the files the noise filter excluded (build output, lockfiles, vendored and
    /// generated code). `None` when the run read no local files.
    pub excluded_noise_files: Option<usize>,
    /// 22c: the non-blank lines in those excluded files. `None` for a run that did not record
    /// it (a report from before 22c).
    pub excluded_noise_lines: Option<usize>,
    /// Why the inspection did not run to completion (see
    /// [`crate::onboard::ScanReport::incomplete_reasons`]); empty for a complete run. Recorded
    /// only on an internal package: the release gate refuses a package that records any, and a
    /// complete run's manifest and README are unchanged.
    pub incomplete: Vec<String>,
}

impl ManifestProvenance {
    /// The report cover's repos and generated-at, with each repo's commit taken from the cover's
    /// `audited_refs` (the git identity the scan recorded), and `corpus_version` and
    /// `rules_last_reviewed` absent. Callers that have the corpus fill those two through
    /// [`crate::client_provenance::export_provenance`].
    pub fn from_cover(cover: &crate::report_export::CoverJson) -> Self {
        let repos = cover
            .repos
            .iter()
            .map(|name| {
                // The first ref for this repo that actually recorded a commit.
                let recorded = cover
                    .audited_refs
                    .iter()
                    .find(|r| r.repo == *name && r.sha.is_some());
                ProvenanceRepo {
                    name: name.clone(),
                    commit: recorded.and_then(|r| r.sha.clone()),
                    dirty: recorded.is_some_and(|r| r.dirty),
                }
            })
            .collect();
        Self {
            repos,
            generated_at: cover.generated_at.clone(),
            ..Self::default()
        }
    }

    /// Camerata's own version as the manifest and README both state it, so a finding can always
    /// be traced to the engine that produced it. Derived from this crate's `CARGO_PKG_VERSION`
    /// and the git commit the binary was built from ([`ENGINE_COMMIT`]), never supplied by a
    /// caller, so it cannot drift from the binary that built the package. A build outside a git
    /// checkout records no commit, and the version then names the package version alone.
    pub fn engine_version(&self) -> String {
        match ENGINE_COMMIT {
            Some(commit) => format!("camerata {} (commit {commit})", env!("CARGO_PKG_VERSION")),
            None => format!("camerata {}", env!("CARGO_PKG_VERSION")),
        }
    }

    /// The `manifest.json` `inspection` block. An absent optional value is an explicit JSON
    /// `null` (the field always exists, so a consumer can tell "not recorded" from "not
    /// supported"). Rendered from the same fields as [`Self::readme_lines`].
    pub fn inspection_json(&self) -> serde_json::Value {
        let repos: Vec<serde_json::Value> = self
            .repos
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "commit": r.commit,
                    "dirty": r.dirty,
                })
            })
            .collect();
        let mut inspection = serde_json::json!({
            "repos": repos,
            "engine_version": self.engine_version(),
            "corpus_version": self.corpus_version,
            "rules_last_reviewed": self.rules_last_reviewed,
            "model_backend": self.model.as_ref().map(|m| &m.backend),
            "model_provider": self.model.as_ref().map(|m| &m.provider),
            "models": self.model.as_ref().map(|m| {
                m.models
                    .iter()
                    .map(|r| serde_json::json!({ "role": r.role, "model": r.model }))
                    .collect::<Vec<_>>()
            }),
            "client_engagement": self.model.as_ref().map(|m| m.client_engagement),
            "cost_cap_reached": self.cost_cap_reached,
            "cost_cap_unenforced": self.cost_cap_reached && self.cost_cap_unenforced,
            "vulnerability_data": crate::vuln_intel::manifest_json(self.vulnerability_data.as_ref()),
            "client_name": self.client_name,
            "corpus_rules_applied": self.corpus_rules_applied,
            "counted_lines": self.counted_lines,
            "excluded_noise_files": self.excluded_noise_files,
            "excluded_noise_lines": self.excluded_noise_lines,
            "release_checks": Self::release_checks(),
        });
        // Present only for an incomplete run, so a complete run's manifest is unchanged.
        if !self.incomplete.is_empty() {
            inspection["run_incomplete"] = serde_json::json!(true);
            inspection["incomplete_reasons"] = serde_json::json!(self.incomplete);
        }
        inspection
    }

    /// The release gate's check names, in run order. The README line and the manifest's
    /// `inspection.release_checks` both render this one list (a package only exists if every
    /// one of these passed, because `release_gate::release` returns no bytes otherwise).
    pub fn release_checks() -> Vec<&'static str> {
        crate::release_gate::CheckId::ALL
            .iter()
            .map(|c| c.label())
            .collect()
    }

    /// The README's provenance block: the human rendering of [`Self::inspection_json`] plus
    /// `generated_at`. Every line ends with a newline. A value that is not recorded is OMITTED,
    /// never printed as a placeholder: a fabricated review date or an empty commit is worse
    /// than no line. Gate 7 (`tests/product_export_manifest_gates.rs`) holds this equal to the
    /// manifest.
    pub fn readme_lines(&self) -> String {
        let names: Vec<&str> = self.repos.iter().map(|r| r.name.as_str()).collect();
        let mut out = String::new();
        if let Some(client) = &self.client_name {
            out.push_str(&format!("Prepared for: {client}\n"));
        }
        out.push_str(&format!(
            "Repos inspected: {}\nGenerated: {}\n",
            names.join(", "),
            self.generated_at
        ));
        for repo in &self.repos {
            if let Some(commit) = &repo.commit {
                let note = if repo.dirty {
                    " (the working tree had uncommitted changes)"
                } else {
                    ""
                };
                out.push_str(&format!("Commit ({}): {commit}{note}\n", repo.name));
            }
        }
        out.push_str(&format!("Engine version: {}\n", self.engine_version()));
        if let Some(corpus) = &self.corpus_version {
            out.push_str(&format!("Corpus version: {corpus}\n"));
        }
        if let Some(reviewed) = &self.rules_last_reviewed {
            out.push_str(&format!("Rules last reviewed on {reviewed}\n"));
        }
        if let Some(applied) = self.corpus_rules_applied {
            out.push_str(&format!("Corpus rules applied: {applied}\n"));
        }
        if let Some(lines) = self.counted_lines {
            out.push_str(&format!("Counted lines: {lines}\n"));
        }
        if let Some(files) = self.excluded_noise_files {
            match self.excluded_noise_lines {
                Some(lines) => out.push_str(&format!(
                    "Excluded as noise: {files} files, {lines} lines\n"
                )),
                None => out.push_str(&format!("Excluded as noise: {files} files\n")),
            }
        }
        if let Some(model) = &self.model {
            out.push_str(&format!("Model backend: {}\n", model.backend));
            out.push_str(&format!("Model provider: {}\n", model.provider));
            for r in &model.models {
                out.push_str(&format!("Model ({}): {}\n", r.role, r.model));
            }
            out.push_str(&format!(
                "Client engagement: {}\n",
                if model.client_engagement { "yes" } else { "no" }
            ));
        }
        if self.cost_cap_reached {
            let line = if self.cost_cap_unenforced {
                COST_CAP_UNENFORCED_README_LINE
            } else {
                COST_CAP_README_LINE
            };
            out.push_str(&format!("{line}\n"));
        }
        if !self.incomplete.is_empty() {
            out.push_str(&format!("{INTERNAL_INCOMPLETE_README_LINE}\n"));
            for reason in &self.incomplete {
                out.push_str(&format!("Not complete: {reason}\n"));
            }
        }
        out.push_str(&crate::vuln_intel::readme_lines(
            self.vulnerability_data.as_ref(),
        ));
        out.push_str(&format!(
            "Release checks: {} (all passed)\n",
            Self::release_checks().join(", ")
        ));
        out
    }
}

/// The git commit this binary was built from, recorded by `build.rs` (`CAMERATA_GIT_COMMIT`).
/// `None` for a build outside a git checkout: an honest absence, never a placeholder. Stated in
/// [`ManifestProvenance::engine_version`] (README and `manifest.json`) and in the report cover
/// (`report_export::CoverJson::engine_commit`, which the PDF and `findings.json` render).
pub const ENGINE_COMMIT: Option<&str> = option_env!("CAMERATA_GIT_COMMIT");

/// The README line of an internal package made from a run that did not complete.
pub const INTERNAL_INCOMPLETE_README_LINE: &str = "INTERNAL INCOMPLETE: this inspection did not \
     run to completion and is not for a client.";

/// The README line that states the inspection was stopped by the operator's spend limit. Printed
/// only when [`ManifestProvenance::cost_cap_reached`] is true; the release gate parses it back.
pub const COST_CAP_README_LINE: &str = "Spend limit: reached. This inspection is INCOMPLETE; the \
     report lists the passes that were not run.";

/// The README line when the spend limit stopped work because a call's cost could not be
/// determined, so the limit could not be enforced (it was not reached). Printed instead of
/// [`COST_CAP_README_LINE`]; the release gate parses both.
pub const COST_CAP_UNENFORCED_README_LINE: &str = "Spend limit: could not be enforced. This \
     inspection is INCOMPLETE; the report lists the passes that were not run.";

/// The one-sentence purpose + per-contract `schema_version` for a file in the product ZIP, keyed
/// by its archive path. `None` schema for formats with no data contract (PDF, xlsx, README).
fn manifest_file_facts(path: &str) -> (String, Option<String>) {
    let base = path.rsplit('/').next().unwrap_or(path);
    let purpose = if path.ends_with(".pdf") {
        "Curated narrative report for a decision-maker."
    } else if path.ends_with(".xlsx") {
        "Complete working dataset across sheets, for engineers."
    } else if path == "findings.json" {
        "All-Findings rows wrapped with provenance and summary counts, for tooling."
    } else if path == "README.txt" {
        "Plain-text manifest of what each file is."
    } else if path == "decisions.json" {
        "Every decision record the report was reviewed with, for tooling."
    } else if path == "csv/tickets.csv" {
        "One ticket for each sub-group, for a Jira, Linear or GitHub Issues importer."
    } else if path.starts_with("csv/") {
        "One xlsx data table as RFC 4180 CSV, for tooling that does not read Excel."
    } else if path.starts_with("json/") {
        "One xlsx data table as a flat JSON array, for structured ingestion."
    } else {
        "Product-export artifact."
    };
    // A data-contract schema id for the machine files; versioned per dataset, bumped on a
    // breaking column/key change.
    let schema = if path.starts_with("csv/")
        || path.starts_with("json/")
        || path == "findings.json"
        || path == "decisions.json"
    {
        let dataset = base.trim_end_matches(".csv").trim_end_matches(".json");
        Some(format!("{dataset}/1.0"))
    } else {
        None
    };
    (purpose.to_string(), schema)
}

/// Build `manifest.json`: the package index (every file's purpose + sha256 + schema version)
/// plus the `inspection` provenance block. Built LAST, over the FINAL bytes of every other file,
/// and never lists itself (a file cannot hash itself). Pure.
pub fn build_manifest(
    stem: &str,
    pdf_bytes: &[u8],
    xlsx_bytes: &[u8],
    findings_json_bytes: &[u8],
    readme: &str,
    extra_files: &[(String, Vec<u8>)],
    provenance: &ManifestProvenance,
) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let sha = |bytes: &[u8]| -> String {
        let digest = Sha256::digest(bytes);
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let entry = |path: String, bytes: &[u8]| {
        let (purpose, schema_version) = manifest_file_facts(&path);
        serde_json::json!({
            "path": path,
            "purpose": purpose,
            "sha256": sha(bytes),
            "schema_version": schema_version,
        })
    };
    let mut files = vec![
        entry(format!("{stem}.pdf"), pdf_bytes),
        entry(format!("{stem}-findings.xlsx"), xlsx_bytes),
        entry("findings.json".to_string(), findings_json_bytes),
        entry("README.txt".to_string(), readme.as_bytes()),
    ];
    for (path, bytes) in extra_files {
        files.push(entry(path.clone(), bytes));
    }

    let manifest = serde_json::json!({
        "manifest_schema_version": "1.0",
        "generated_at": provenance.generated_at,
        "inspection": provenance.inspection_json(),
        "files": files,
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Zip the PDF + xlsx + `findings.json` + README.txt into one in-memory archive (Deflate
/// compression) for the product-export response body. The ONLY I/O here is the in-memory
/// `Cursor<Vec<u8>>` — no temp files, matching `report_export`'s own "tiny, no persistence"
/// contract.
///
/// `pub(crate)`: this is the ONE zip-assembly function, and `release_gate::release` is its ONLY
/// production caller (a source-grep test pins that), so no exporter can obtain zip bytes that
/// did not pass the release gate. Integration tests that need an archive to tamper with go
/// through [`assemble_unreleased_zip`].
pub(crate) fn build_product_zip(
    stem: &str,
    pdf_bytes: &[u8],
    xlsx_bytes: &[u8],
    findings_json_bytes: &[u8],
    readme: &str,
    extra_files: &[(String, Vec<u8>)],
    provenance: &ManifestProvenance,
) -> zip::result::ZipResult<Vec<u8>> {
    use std::io::Write;

    let buf = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buf);
    // Real timestamps (the build time), not the zip crate's 1980-01-01 default — see
    // `zip_now`'s doc comment.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip_now());

    writer.start_file(format!("{stem}.pdf"), options)?;
    writer.write_all(pdf_bytes)?;

    writer.start_file(format!("{stem}-findings.xlsx"), options)?;
    writer.write_all(xlsx_bytes)?;

    writer.start_file("findings.json", options)?;
    writer.write_all(findings_json_bytes)?;

    writer.start_file("README.txt", options)?;
    writer.write_all(readme.as_bytes())?;

    // The per-dataset CSV/JSON tables (csv/*, json/*), written under their folders.
    for (path, bytes) in extra_files {
        writer.start_file(path.clone(), options)?;
        writer.write_all(bytes)?;
    }

    // manifest.json LAST: it hashes every other file above, so it is built over their final
    // bytes and never lists itself.
    let manifest = build_manifest(
        stem,
        pdf_bytes,
        xlsx_bytes,
        findings_json_bytes,
        readme,
        extra_files,
        provenance,
    );
    writer.start_file("manifest.json", options)?;
    writer.write_all(&manifest)?;

    let cursor = writer.finish()?;
    Ok(cursor.into_inner())
}

/// [`build_product_zip`] for TESTS: an archive that did NOT pass the release gate, so the gate's
/// own corruption tests (manifest hashes, provenance) can build a tampered one. Production code
/// must never call this; a source-grep test fails the build if it does.
#[doc(hidden)]
pub fn assemble_unreleased_zip(
    stem: &str,
    pdf_bytes: &[u8],
    xlsx_bytes: &[u8],
    findings_json_bytes: &[u8],
    readme: &str,
    extra_files: &[(String, Vec<u8>)],
    provenance: &ManifestProvenance,
) -> zip::result::ZipResult<Vec<u8>> {
    build_product_zip(
        stem,
        pdf_bytes,
        xlsx_bytes,
        findings_json_bytes,
        readme,
        extra_files,
        provenance,
    )
}

// ── error type ──────────────────────────────────────────────────────────────

/// A handler error carrying an explicit HTTP status + a JSON body, so handlers can use `?`.
///
/// ROUTES-7: this type used to map EVERY failure to 500, including not-found and bad-request
/// conditions that handler docs promise as 4xx. The `status` field lets a handler classify:
/// use [`AppError::not_found`] for a missing resource (404), [`AppError::bad_request`] for
/// invalid input (400), and the bare `AppError(e)` / `?` conversion for a genuine internal
/// fault (500). The response BODY shape is unchanged (`{ "error": "…" }`) — only the status
/// code is corrected, so UI code that parses `{error}` keeps working.
struct AppError {
    status: StatusCode,
    err: anyhow::Error,
}

/// Construct a 500 `AppError` (the default, for a genuine internal fault). The `AppError(e)`
/// call form is preserved via a same-named free function (types and values live in separate
/// namespaces), so every existing call site keeps compiling; use [`AppError::not_found`] /
/// [`AppError::bad_request`] / [`AppError::with_status`] for a 4xx code.
#[allow(non_snake_case)]
fn AppError(err: anyhow::Error) -> AppError {
    AppError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        err,
    }
}

impl AppError {
    /// An `AppError` that renders with a specific HTTP status (e.g. 409 Conflict).
    fn with_status(status: StatusCode, err: anyhow::Error) -> Self {
        AppError { status, err }
    }

    /// ROUTES-7: a 404 for a missing resource (the message becomes the `{error}` body).
    fn not_found(err: anyhow::Error) -> Self {
        AppError {
            status: StatusCode::NOT_FOUND,
            err,
        }
    }

    /// ROUTES-7: a 400 for invalid input / a request the server understood but can't act on
    /// as posed (the message becomes the `{error}` body).
    fn bad_request(err: anyhow::Error) -> Self {
        AppError {
            status: StatusCode::BAD_REQUEST,
            err,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.err.to_string() }));
        (self.status, body).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError(e)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod lib_multirepo_tests;

#[cfg(test)]
mod preview_evidence_tests;

#[cfg(test)]
mod cockpit_resume_tests;

#[cfg(test)]
mod site_key_paths_tests;

#[cfg(test)]
mod tests;
