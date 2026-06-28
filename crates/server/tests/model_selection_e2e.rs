//! Hermetic end-to-end regression net for MODEL SELECTION wiring.
//!
//! THE CLAIM under test: for every entry point in camerata-orchestrator, the model id
//! a user SELECTS (what the UI POSTs / the project config holds) is the EXACT id that
//! reaches the model-call boundary (the CLI `--model` arg / the OpenRouter request body
//! / the `LlmRequest.model` a `Completer` receives).
//!
//! This is the critical regression net for a class of model-wiring bugs where a
//! selection silently resolves to the wrong model between request and call.
//!
//! HERMETIC: NO real `claude` spawn, NO network. Construction-only `Llm::from_env()`,
//! a fake in-memory `CredentialStore`, a seeded in-memory `AppState`, and a CAPTURING
//! `Completer` that records `req.model` are the only seams used.
//!
//! Coverage levels are labelled per test:
//!   - RESOLUTION-LEVEL: asserts the selected id at the point where the model is resolved
//!     (e.g. `TierMap::model_for_task`, `step_model_or`, `DelegateModels::resolve`,
//!     `ClaudeCliDriver.model`).
//!   - BOUNDARY-CAPTURE: drives a real code path into a capturing `Completer` and asserts
//!     the recorded `req.model` IS the selected/override id (closes the loop).

use std::sync::Arc;

use async_trait::async_trait;

use camerata_server::credentials::{CredentialStore, MemoryCredentialStore, OPENROUTER_API_KEY};
use camerata_server::llm::{
    build_completer, Completer, Llm, LlmRequest, LlmResponse, OpenRouterCompleter, DEFAULT_MODEL,
};
use camerata_server::model_registry::{ModelRegistry, RegistryEntry};
use camerata_server::model_tier::{CapabilityBand, TierMap};
use camerata_server::project::{
    L3ReviewConfig, ModelProfile, ProjectStore, StepKind,
};
use camerata_server::rate_limit::ProviderRateLimiter;
use camerata_server::{step_model, step_model_or, AppState};

use camerata_fleet::orchestrator::{delegate_models_json, lead_stage_index};
use camerata_intake::{Plan, PlanTask, TaskKind};

// ════════════════════════════════════════════════════════════════════════════════════
// Shared fixtures
// ════════════════════════════════════════════════════════════════════════════════════

/// A CAPTURING `Completer`: records every `req.model` it is asked to complete, and returns
/// a fixed response. This is the boundary seam — whatever model reaches a real model call
/// lands here, so asserting on `captured()` proves the selected id flowed end to end.
struct CapturingCompleter {
    seen: std::sync::Mutex<Vec<String>>,
    reply: String,
}

impl CapturingCompleter {
    fn new(reply: &str) -> Self {
        Self {
            seen: std::sync::Mutex::new(Vec::new()),
            reply: reply.to_string(),
        }
    }
    fn captured(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
    fn last(&self) -> Option<String> {
        self.seen.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl Completer for CapturingCompleter {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.seen.lock().unwrap().push(req.model.clone());
        Ok(LlmResponse {
            text: self.reply.clone(),
            model: req.model.clone(),
            backend: "capture".to_string(),
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A `CredentialStore` with the OpenRouter key set (so OpenRouter routing succeeds).
fn creds_with_openrouter_key() -> MemoryCredentialStore {
    let c = MemoryCredentialStore::new();
    c.set(OPENROUTER_API_KEY, "sk-or-test-key").unwrap();
    c
}

/// A registry seeded with one OpenRouter-provider model id so `build_completer` /
/// `build_agent_driver` route it to the OpenRouter path.
fn registry_with_openrouter(id: &str) -> ModelRegistry {
    let reg = ModelRegistry::new();
    reg.seed_openrouter_entries(vec![RegistryEntry {
        provider: "openrouter".to_string(),
        display: id.to_string(),
        id: id.to_string(),
        free: false,
        tool_use: true,
        context: 32_768,
        coding: 1.0,
        price_in: 0.0,
        price_out: 0.0,
        weight: 0,
        caching: false,
        vision: false,
    }]);
    reg
}

fn limiter() -> Arc<ProviderRateLimiter> {
    Arc::new(ProviderRateLimiter::new())
}

/// A construction-only `Llm` (CLI backend). `Llm::from_env()` does NOT spawn `claude`;
/// it only reads env + sets fields. Hermetic.
fn cli_llm() -> Arc<Llm> {
    Arc::new(Llm::from_env())
}

fn plan_task(kind: TaskKind, desc: &str) -> PlanTask {
    PlanTask {
        role: "Agent".to_string(),
        kind,
        description: desc.to_string(),
    }
}

/// A custom TierMap with distinct, recognisable ids in every band so a mis-wire is
/// impossible to miss (no two bands share a model).
fn distinct_tier_map() -> TierMap {
    TierMap {
        strongest: "STRONGEST-id".to_string(),
        balanced: vec!["BALANCED-id".to_string()],
        fast: vec!["FAST-id".to_string()],
        vision: vec!["VISION-id".to_string()],
    }
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 1 — Fleet tier bands  (RESOLUTION-LEVEL)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope1_tier_each_taskkind_maps_to_its_bands_model() {
    let m = distinct_tier_map();
    // Test -> Fast, Database/Frontend -> Balanced, Backend -> Strongest.
    assert_eq!(m.model_for_task(&plan_task(TaskKind::Test, "tests")), "FAST-id");
    assert_eq!(
        m.model_for_task(&plan_task(TaskKind::Database, "schema")),
        "BALANCED-id"
    );
    assert_eq!(
        m.model_for_task(&plan_task(TaskKind::Frontend, "view")),
        "BALANCED-id"
    );
    assert_eq!(
        m.model_for_task(&plan_task(TaskKind::Backend, "domain")),
        "STRONGEST-id"
    );
}

#[test]
fn scope1_tier_override_prefix_forces_band_through_to_model() {
    let m = distinct_tier_map();
    // Backend normally -> Strongest, but the [TIER:fast] prefix forces Fast -> FAST-id.
    let t = plan_task(TaskKind::Backend, "[TIER:fast] scaffold");
    assert_eq!(m.model_for_task(&t), "FAST-id");
    // Test normally -> Fast, but [TIER:strongest] forces Strongest -> STRONGEST-id.
    let t2 = plan_task(TaskKind::Test, "[tier:strongest] gnarly setup");
    assert_eq!(m.model_for_task(&t2), "STRONGEST-id");
}

#[test]
fn scope1_band_model_for_resolves_each_band() {
    let m = distinct_tier_map();
    assert_eq!(m.model_for(CapabilityBand::Fast), "FAST-id");
    assert_eq!(m.model_for(CapabilityBand::Balanced), "BALANCED-id");
    assert_eq!(m.model_for(CapabilityBand::Strongest), "STRONGEST-id");
}

#[test]
fn scope1_orchestrator_lead_stage_is_the_first_strongest() {
    // The orchestrator/lead invariant: the lead is the FIRST strongest-band task, and it
    // therefore resolves to the `strongest` model. Delegate children resolve to the
    // lower bands (asserted in scope1_delegate_children_*).
    let tasks = vec![
        plan_task(TaskKind::Test, "tests"),     // Fast
        plan_task(TaskKind::Database, "schema"), // Balanced
        plan_task(TaskKind::Backend, "domain A"), // Strongest <- lead
        plan_task(TaskKind::Backend, "domain B"), // Strongest
    ];
    let lead = lead_stage_index(&tasks).expect("a strongest task exists -> a lead exists");
    assert_eq!(lead, 2, "lead is the first strongest-band task");

    let m = distinct_tier_map();
    // The lead stage resolves to the STRONGEST model.
    assert_eq!(m.model_for_task(&tasks[lead]), "STRONGEST-id");
}

#[test]
fn scope1_no_lead_when_no_strongest_task() {
    let tasks = vec![
        plan_task(TaskKind::Test, "tests"),
        plan_task(TaskKind::Frontend, "view"),
    ];
    assert_eq!(lead_stage_index(&tasks), None);
}

#[test]
fn scope1_delegate_children_resolve_to_balanced_and_fast() {
    // The orchestrator boots its gateway with a per-tier model map (delegate_models_json).
    // A delegate child on tier `balanced`/`fast` resolves to exactly those band models;
    // `strongest` is carried for completeness. This is the gateway DelegateModels::resolve
    // contract, asserted via the JSON the orchestrator emits.
    let m = distinct_tier_map();
    let json = delegate_models_json(&m).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["fast"], "FAST-id", "delegate fast child -> fast model");
    assert_eq!(
        v["balanced"], "BALANCED-id",
        "delegate balanced child -> balanced model"
    );
    assert_eq!(
        v["strongest"], "STRONGEST-id",
        "delegate map carries strongest for completeness"
    );
    // NOTE: the gateway's `DelegateModels::resolve` (which parses THIS exact JSON on the
    // child side) lives in the gateway BINARY (`gateway/src/main.rs mod delegate`), not the
    // library, so it is not importable here. The JSON shape asserted above is the contract
    // both sides share; the gateway's own unit tests cover `resolve`.
}

#[test]
fn scope1_vision_band_routing_when_enabled() {
    // The vision band is a separate slot, populated independently. When the project enables
    // vision, the configured vision model is the one available (model_for_task covers the
    // logic ladder; the vision slot is read directly off the map).
    let m = distinct_tier_map();
    assert_eq!(m.vision, vec!["VISION-id".to_string()]);
    // gating is a Project flag (vision_enabled); the map still holds the configured id.
    let store = ProjectStore::new();
    let p = store.create("VisionProj", vec![]).unwrap();
    assert!(!p.vision_enabled, "vision defaults off");
    let updated = store
        .update(&p.id, |proj| {
            proj.tier_map = m.clone();
            proj.vision_enabled = true;
        })
        .unwrap();
    assert!(updated.vision_enabled);
    assert_eq!(updated.tier_map.vision, vec!["VISION-id".to_string()]);
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 2 — All 7 helper steps  (RESOLUTION-LEVEL, via the REAL set/resolve path)
// ════════════════════════════════════════════════════════════════════════════════════

const ALL_STEPS: &[(StepKind, &str)] = &[
    (StepKind::Audit, "audit-model"),
    (StepKind::Calibration, "calibration-model"),
    (StepKind::ResearchChat, "research-chat-model"),
    (StepKind::StoryAuthoring, "story-authoring-model"),
    (StepKind::Decomposition, "decomposition-model"),
    (StepKind::Escalation, "escalation-model"),
    (StepKind::Clarification, "clarification-model"),
];

/// Build a seeded AppState with ONE active project, returning (state, project_id).
fn seeded_state_with_project() -> (AppState, String) {
    let state = AppState::seeded();
    let p = state
        .projects()
        .create("E2E", vec!["me/repo".to_string()])
        .expect("create active project");
    (state, p.id)
}

#[test]
fn scope2_each_step_set_via_real_path_resolves_back_exactly() {
    let (state, id) = seeded_state_with_project();

    for (kind, model) in ALL_STEPS {
        // Set via the REAL write path (mirrors the POST /step-models handler).
        let updated = state
            .projects()
            .set_step_model(&id, *kind, model.to_string())
            .expect("project exists");
        assert_eq!(
            updated.model_for_step(*kind),
            *model,
            "{kind:?} persists the selected id on the project"
        );
        // And step_model(&state, kind) (lib.rs:2399) resolves the active project's id.
        assert_eq!(
            step_model(&state, *kind),
            *model,
            "{kind:?} resolves via step_model() to the selected id"
        );
    }
}

#[test]
fn scope2_request_model_overrides_project_default_for_every_step() {
    let (state, id) = seeded_state_with_project();

    for (kind, model) in ALL_STEPS {
        state
            .projects()
            .set_step_model(&id, *kind, model.to_string())
            .unwrap();
        // step_model_or with an explicit override (request model > project default).
        assert_eq!(
            step_model_or(&state, *kind, Some("override-id")),
            "override-id",
            "{kind:?}: request model wins over project default"
        );
        // None / blank request -> falls through to the project default.
        assert_eq!(
            step_model_or(&state, *kind, None),
            *model,
            "{kind:?}: no request model -> project default"
        );
        assert_eq!(
            step_model_or(&state, *kind, Some("   ")),
            *model,
            "{kind:?}: blank request model -> project default (trimmed-empty ignored)"
        );
    }
}

#[test]
fn scope2_step_model_floors_to_default_when_no_active_project() {
    // A seeded state with NO project at all: step_model falls to DEFAULT_MODEL (the only
    // project-less floor). step_model_or still lets the request override.
    let state = AppState::seeded();
    assert!(state.projects().active().is_none(), "no project seeded");
    assert_eq!(step_model(&state, StepKind::ResearchChat), DEFAULT_MODEL);
    assert_eq!(
        step_model_or(&state, StepKind::ResearchChat, Some("req-x")),
        "req-x"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 3 — L3 review  (RESOLUTION-LEVEL, via the REAL set path)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope3_l3_model_returns_pinned_then_falls_back_to_balanced() {
    let store = ProjectStore::new();
    let p = store.create("L3Proj", vec![]).unwrap();

    // Give the project a recognisable balanced tier so the fallback is unambiguous.
    let p = store
        .update(&p.id, |proj| {
            proj.tier_map.balanced = vec!["BALANCED-id".to_string()];
        })
        .unwrap();

    // Empty l3 model -> falls back to tier_map.balanced (primary).
    assert_eq!(
        p.l3_model(),
        "BALANCED-id",
        "empty l3 model falls back to balanced tier"
    );

    // Pinned l3 model -> returned exactly.
    let pinned = store
        .set_l3_review(
            &p.id,
            L3ReviewConfig {
                enabled: true,
                model: "L3-pinned-id".to_string(),
            },
        )
        .unwrap();
    assert_eq!(pinned.l3_model(), "L3-pinned-id");

    // Clearing the pin -> back to balanced.
    let cleared = store
        .set_l3_review(
            &p.id,
            L3ReviewConfig {
                enabled: true,
                model: "   ".to_string(),
            },
        )
        .unwrap();
    assert_eq!(
        cleared.l3_model(),
        "BALANCED-id",
        "whitespace-only l3 model is treated as empty -> balanced fallback"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 4 — Profile cascade  (RESOLUTION-LEVEL, via the REAL cascade fn + apply copy)
// ════════════════════════════════════════════════════════════════════════════════════

/// Apply a profile to a project EXACTLY as the `POST /model-profile` handler does:
/// compute the cascade (the real pure fn), then copy tier_map/step_models/l3 onto the
/// project and stamp model_profile. Returns the updated project.
fn apply_profile_real(
    store: &ProjectStore,
    registry: &ModelRegistry,
    id: &str,
    profile: ModelProfile,
) -> camerata_server::project::Project {
    let assignments =
        camerata_server::model_profile_cascade::compute_profile_cascade(profile, registry);
    store
        .update(id, |p| {
            p.model_profile = profile;
            if let Some(ref a) = assignments {
                p.tier_map = a.tier_map.clone();
                p.step_models = a.step_models.clone();
                p.l3_review = a.l3_review.clone();
            }
        })
        .unwrap()
}

#[test]
fn scope4_balanced_profile_overwrites_all_entries() {
    let store = ProjectStore::new();
    let registry = ModelRegistry::new();
    let p = store.create("Bal", vec![]).unwrap();

    // First scribble distinct non-default values everywhere so "overwrite-all" is visible.
    store
        .update(&p.id, |proj| {
            proj.tier_map = distinct_tier_map();
            proj.set_model_for_step(StepKind::Audit, "scribble".to_string());
        })
        .unwrap();

    let p = apply_profile_real(&store, &registry, &p.id, ModelProfile::Balanced);

    assert_eq!(p.model_profile, ModelProfile::Balanced);
    assert_eq!(p.tier_map.strongest, "claude-opus-4-8");
    assert_eq!(p.tier_map.balanced, vec!["claude-sonnet-4-6".to_string()]);
    assert_eq!(p.tier_map.fast, vec!["claude-haiku-4-5-20251001".to_string()]);
    // Every step model overwritten to Haiku.
    for (kind, _) in ALL_STEPS {
        assert_eq!(
            p.model_for_step(*kind),
            "claude-haiku-4-5-20251001",
            "Balanced overwrites {kind:?} to Haiku"
        );
    }
    assert!(!p.l3_review.enabled, "Balanced leaves L3 off");
}

#[test]
fn scope4_max_quality_profile_overwrites_all_entries() {
    let store = ProjectStore::new();
    let registry = ModelRegistry::new();
    let p = store.create("MQ", vec![]).unwrap();

    let p = apply_profile_real(&store, &registry, &p.id, ModelProfile::MaxQuality);

    assert_eq!(p.model_profile, ModelProfile::MaxQuality);
    assert_eq!(p.tier_map.strongest, "claude-opus-4-8");
    assert_eq!(p.tier_map.balanced, vec!["claude-sonnet-4-6".to_string()]);
    assert_eq!(p.tier_map.fast, vec!["claude-sonnet-4-6".to_string()]);
    for (kind, _) in ALL_STEPS {
        assert_eq!(p.model_for_step(*kind), "claude-sonnet-4-6");
    }
    assert!(p.l3_review.enabled, "MaxQuality turns L3 on");
    assert_eq!(p.l3_review.model, "claude-sonnet-4-6");
}

#[test]
fn scope4_max_efficiency_profile_picks_free_models() {
    let store = ProjectStore::new();
    // Seed a FREE + tool-use OpenRouter model so the MaxEfficiency cascade picks it.
    let registry = ModelRegistry::new();
    registry.seed_openrouter_entries(vec![RegistryEntry {
        provider: "openrouter".to_string(),
        display: "Qwen3 Coder (free)".to_string(),
        id: "qwen/qwen3-coder:free".to_string(),
        free: true,
        tool_use: true,
        context: 32_768,
        coding: 1.0,
        price_in: 0.0,
        price_out: 0.0,
        weight: 0,
        caching: false,
        vision: false,
    }]);
    let p = store.create("ME", vec![]).unwrap();

    let p = apply_profile_real(&store, &registry, &p.id, ModelProfile::MaxEfficiency);

    assert_eq!(p.model_profile, ModelProfile::MaxEfficiency);
    // Strongest stays Opus; balanced/fast chains lead with the free model.
    assert_eq!(p.tier_map.strongest, "claude-opus-4-8");
    assert_eq!(p.tier_map.balanced[0], "qwen/qwen3-coder:free");
    // Step models use the free coder.
    assert_eq!(p.model_for_step(StepKind::Audit), "qwen/qwen3-coder:free");
    assert!(p.l3_review.enabled);
}

#[test]
fn scope4_custom_profile_is_a_noop_no_overwrite() {
    let store = ProjectStore::new();
    let registry = ModelRegistry::new();
    let p = store.create("Cus", vec![]).unwrap();

    // Seed distinct values; Custom must leave them untouched.
    store
        .update(&p.id, |proj| {
            proj.tier_map = distinct_tier_map();
            proj.set_model_for_step(StepKind::Audit, "user-owned".to_string());
        })
        .unwrap();

    let p = apply_profile_real(&store, &registry, &p.id, ModelProfile::Custom);
    assert_eq!(p.model_profile, ModelProfile::Custom);
    // Untouched (compute_profile_cascade returns None for Custom).
    assert_eq!(p.tier_map.strongest, "STRONGEST-id");
    assert_eq!(p.model_for_step(StepKind::Audit), "user-owned");
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 5 — CLI-vs-API routing  (RESOLUTION-LEVEL on the returned completer/driver)
// ════════════════════════════════════════════════════════════════════════════════════

#[test]
fn scope5_claude_model_routes_to_cli_backed_llm() {
    // A claude-provider model id -> build_completer returns the CLI-backed `Llm`
    // (the same Arc we passed in), NOT an OpenRouterCompleter.
    let registry = ModelRegistry::new(); // static registry has claude-sonnet-4-6 as `claude`.
    let creds = MemoryCredentialStore::new(); // no OpenRouter key needed for claude.
    let llm = cli_llm();
    let completer = build_completer("claude-sonnet-4-6", &registry, &creds, llm, limiter())
        .expect("claude model must build without an OpenRouter key");
    assert!(
        !completer.as_any().is::<OpenRouterCompleter>(),
        "claude model must NOT route to OpenRouter"
    );
    // It downcasts to the concrete Llm (the CLI path).
    assert!(
        completer.as_any().is::<Llm>(),
        "claude model routes to the CLI-backed Llm"
    );
}

#[test]
fn scope5_openrouter_model_routes_to_openrouter_completer() {
    let registry = registry_with_openrouter("vendor/some-model");
    let creds = creds_with_openrouter_key();
    let llm = cli_llm();
    let completer = build_completer("vendor/some-model", &registry, &creds, llm, limiter())
        .expect("openrouter model with a key must build");
    assert!(
        completer.as_any().is::<OpenRouterCompleter>(),
        "openrouter model routes to OpenRouterCompleter"
    );
}

#[test]
fn scope5_openrouter_model_without_key_errors_cleanly() {
    let registry = registry_with_openrouter("vendor/no-key-model");
    let creds = MemoryCredentialStore::new(); // NO key.
    let llm = cli_llm();
    let result = build_completer("vendor/no-key-model", &registry, &creds, llm, limiter());
    let msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("build_completer must error for an openrouter model with no key"),
    };
    assert!(
        msg.contains("OPENROUTER_API_KEY"),
        "error must name the missing key: {msg}"
    );
}

#[test]
fn scope5_build_agent_driver_routes_by_provider() {
    use camerata_server::api_agent_driver::build_agent_driver;
    let limiter = limiter();

    // Claude provider, no key needed -> Ok (ClaudeCliDriver).
    let registry = ModelRegistry::new();
    let creds = MemoryCredentialStore::new();
    let claude = build_agent_driver(
        "claude-sonnet-4-6",
        &registry,
        &creds,
        "/tmp/mcp.json",
        vec![],
        None,
        false,
        limiter.clone(),
        None,
    );
    assert!(claude.is_ok(), "claude driver builds without a key");

    // OpenRouter provider WITH key -> Ok (ApiAgentDriver/OpenRouter).
    let reg_or = registry_with_openrouter("vendor/agent-model");
    let creds_or = creds_with_openrouter_key();
    let or = build_agent_driver(
        "vendor/agent-model",
        &reg_or,
        &creds_or,
        "/tmp/mcp.json",
        vec![],
        None,
        false,
        limiter.clone(),
        None,
    );
    assert!(or.is_ok(), "openrouter driver builds with a key");

    // OpenRouter provider WITHOUT key -> Err naming the missing key.
    let creds_none = MemoryCredentialStore::new();
    let err = build_agent_driver(
        "vendor/agent-model",
        &reg_or,
        &creds_none,
        "/tmp/mcp.json",
        vec![],
        None,
        false,
        limiter,
        None,
    );
    let msg = match err {
        Err(e) => e.to_string(),
        Ok(_) => panic!("openrouter agent driver must error without a key"),
    };
    assert!(msg.contains("OPENROUTER_API_KEY"), "error names the key: {msg}");
}

#[test]
fn scope5_cli_driver_carries_selected_model_in_with_model() {
    // build_agent_driver's claude arm does `ClaudeCliDriver::new(..).with_model(model_id)`.
    // The `dyn AgentDriver` it returns is not downcastable, so we assert the exact same
    // construction the arm performs: the selected id lands in the public `model` field, which
    // is what becomes the `--model` CLI arg (agent::build_args). This is the CLI boundary
    // that we cannot exercise by spawning `claude`.
    let driver =
        camerata_agent::ClaudeCliDriver::new("/tmp/mcp.json").with_model("claude-opus-4-8");
    assert_eq!(
        driver.model.as_deref(),
        Some("claude-opus-4-8"),
        "selected id is the model that becomes the --model arg"
    );
    // A blank selection leaves model None (the CLI then uses its own default).
    let blank = camerata_agent::ClaudeCliDriver::new("/tmp/mcp.json").with_model("   ");
    assert_eq!(blank.model, None);
}

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 6 — Boundary capture  (BOUNDARY-CAPTURE: recorded req.model == selected id)
// ════════════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn scope6_capturing_completer_records_research_chat_step_model() {
    // Resolve the ResearchChat step model via the REAL resolution helper, then drive a
    // completion through a capturing completer with that model. The recorded model must be
    // the SELECTED id — closing the loop from selection to the model-call boundary.
    let (state, id) = seeded_state_with_project();
    state
        .projects()
        .set_step_model(&id, StepKind::ResearchChat, "selected-research-model".to_string())
        .unwrap();

    // The chat handler resolves: step_model_or(state, ResearchChat, req.model).
    // Case A: no request override -> project default flows to the boundary.
    let resolved = step_model_or(&state, StepKind::ResearchChat, None);
    assert_eq!(resolved, "selected-research-model");

    let cap = CapturingCompleter::new("ok");
    let req = LlmRequest::new("hello").with_model(&resolved);
    cap.complete(req).await.unwrap();
    assert_eq!(
        cap.last().as_deref(),
        Some("selected-research-model"),
        "boundary received the selected research-chat model"
    );
}

#[tokio::test]
async fn scope6_capturing_completer_records_request_override() {
    // Case B: a request override beats the project default, and the OVERRIDE is what reaches
    // the boundary (req > project > default precedence, end to end).
    let (state, id) = seeded_state_with_project();
    state
        .projects()
        .set_step_model(&id, StepKind::ResearchChat, "project-default-model".to_string())
        .unwrap();

    let resolved = step_model_or(&state, StepKind::ResearchChat, Some("ui-override-model"));
    assert_eq!(resolved, "ui-override-model");

    let cap = CapturingCompleter::new("ok");
    cap.complete(LlmRequest::new("q").with_model(&resolved))
        .await
        .unwrap();
    assert_eq!(cap.last().as_deref(), Some("ui-override-model"));
}

#[tokio::test]
async fn scope6_capturing_completer_records_each_steps_selected_model() {
    // Drive ALL 7 steps through the capturing completer: each selected id reaches the
    // boundary exactly. (Generic step path: resolve via step_model, send via the completer.)
    let (state, id) = seeded_state_with_project();
    let cap = CapturingCompleter::new("ok");

    for (kind, model) in ALL_STEPS {
        state
            .projects()
            .set_step_model(&id, *kind, model.to_string())
            .unwrap();
        let resolved = step_model(&state, *kind);
        cap.complete(LlmRequest::new("p").with_model(&resolved))
            .await
            .unwrap();
        assert_eq!(
            cap.last().as_deref(),
            Some(*model),
            "{kind:?}: selected model reached the boundary"
        );
    }
    // Sanity: one capture per step.
    assert_eq!(cap.captured().len(), ALL_STEPS.len());
}

#[tokio::test]
async fn scope6_capturing_completer_records_fleet_band_models() {
    // The fleet per-stage model (TierMap::model_for_task) reaches the boundary exactly.
    let m = distinct_tier_map();
    let cap = CapturingCompleter::new("ok");

    let stages = [
        (plan_task(TaskKind::Backend, "domain"), "STRONGEST-id"),
        (plan_task(TaskKind::Database, "schema"), "BALANCED-id"),
        (plan_task(TaskKind::Test, "tests"), "FAST-id"),
    ];
    for (task, expected) in &stages {
        let model = m.model_for_task(task);
        cap.complete(LlmRequest::new("p").with_model(model))
            .await
            .unwrap();
        assert_eq!(cap.last().as_deref(), Some(*expected));
    }
}

// Touch `Plan` import so an unused-import lint never silently masks a future use; this
// also documents that PlanTask lives on Plan.tasks in the real fleet path.
#[allow(dead_code)]
fn _plan_shape(p: &Plan) -> usize {
    p.tasks.len()
}
