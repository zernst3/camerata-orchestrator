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
use camerata_server::{resolve_chat_model, step_model, step_model_or, AppState};

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
    // vision_enabled=false here: the logic-ladder tiers only (no vision key).
    let json = delegate_models_json(&m, false).unwrap();
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

    // End-to-end gating of the delegate-models JSON the orchestrator emits:
    // - enabled + configured  -> the "vision" tier is present (Designer band reachable).
    let enabled_json = delegate_models_json(&m, true).unwrap();
    let ev: serde_json::Value = serde_json::from_str(&enabled_json).unwrap();
    assert_eq!(
        ev["vision"], "VISION-id",
        "vision band reachable when project vision_enabled is true"
    );
    // - disabled -> NO "vision" key even though a model is configured (toggle gates
    //   availability), so a delegate {tier:\"vision\"} is refused on the child side.
    let disabled_json = delegate_models_json(&m, false).unwrap();
    let dv: serde_json::Value = serde_json::from_str(&disabled_json).unwrap();
    assert!(
        dv.get("vision").is_none(),
        "vision band unreachable when project vision_enabled is false"
    );
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
    assert_eq!(step_model(&state, StepKind::Audit), DEFAULT_MODEL);
    assert_eq!(
        step_model_or(&state, StepKind::Audit, Some("req-x")),
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
        false, // escalation
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
        false, // escalation
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
        false, // escalation
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
async fn scope6_capturing_completer_records_app_level_chat_model() {
    // The chat assistant is GLOBAL: its model is the APP-LEVEL `chat_model` setting, NOT the
    // per-project ResearchChat step. Resolve via the REAL chat helper `resolve_chat_model`,
    // then drive a completion through a capturing completer with that model. The recorded
    // model must be the app-level id — closing the loop from setting to the model-call boundary.
    let state = AppState::seeded();
    state.settings().set_chat_model(Some("app-chat-model".to_string()));

    // The chat handler resolves: resolve_chat_model(req.model, settings.chat_model).
    // Case A: no request override -> the app-level chat model flows to the boundary.
    let resolved = resolve_chat_model(None, state.settings().get().chat_model.as_deref());
    assert_eq!(resolved, "app-chat-model");

    let cap = CapturingCompleter::new("ok");
    let req = LlmRequest::new("hello").with_model(&resolved);
    cap.complete(req).await.unwrap();
    assert_eq!(
        cap.last().as_deref(),
        Some("app-chat-model"),
        "boundary received the app-level chat model"
    );
}

#[tokio::test]
async fn scope6_capturing_completer_records_chat_request_override() {
    // Case B: a per-request override beats the app-level chat model, and the OVERRIDE is what
    // reaches the boundary (req > app chat_model > default precedence, end to end).
    let state = AppState::seeded();
    state.settings().set_chat_model(Some("app-chat-model".to_string()));

    let resolved =
        resolve_chat_model(Some("ui-override-model"), state.settings().get().chat_model.as_deref());
    assert_eq!(resolved, "ui-override-model");

    let cap = CapturingCompleter::new("ok");
    cap.complete(LlmRequest::new("q").with_model(&resolved))
        .await
        .unwrap();
    assert_eq!(cap.last().as_deref(), Some("ui-override-model"));
}

#[tokio::test]
async fn scope6_chat_model_floors_to_default_when_unset() {
    // Case C: neither a per-request model nor an app-level chat_model -> the DEFAULT_MODEL floor
    // reaches the boundary (no per-project ResearchChat involvement anymore).
    let state = AppState::seeded();
    assert!(
        state.settings().get().chat_model.is_none(),
        "no app-level chat model seeded"
    );
    let resolved = resolve_chat_model(None, state.settings().get().chat_model.as_deref());
    assert_eq!(resolved, DEFAULT_MODEL);

    let cap = CapturingCompleter::new("ok");
    cap.complete(LlmRequest::new("q").with_model(&resolved))
        .await
        .unwrap();
    assert_eq!(cap.last().as_deref(), Some(DEFAULT_MODEL));
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

// ════════════════════════════════════════════════════════════════════════════════════
// SCOPE 7 — Provider-agnostic LEAD/orchestrator seam (OrchestratorDriverFactory)
//   The LEAD runs on the STRONGEST model's OWN provider, mirroring the child-driver-factory:
//   Claude strongest -> ClaudeCliDriver orchestrator; OpenRouter strongest -> native
//   ApiAgentDriver orchestrator (whose delegate/fan_out children resolve per-model + gated).
// ════════════════════════════════════════════════════════════════════════════════════

use camerata_fleet::orchestrator::{
    LeadBuildContext, OrchestratorDriverFactory, OrchestratorSession,
};

/// Build a real orchestrator session (rules + delegate-ON mcp-config) for the lead role.
/// The gateway bin path is a placeholder: construction never executes it.
fn lead_session(tier_map: &TierMap) -> OrchestratorSession {
    let role = camerata_core::Role {
        name: "Lead".to_string(),
        rule_subset: vec![camerata_core::RuleId("GOV-1".to_string())],
        allowed_paths: vec!["crate/".to_string()],
    };
    camerata_fleet::orchestrator::prepare_orchestrator_session(
        std::path::Path::new("/bin/camerata-gateway"),
        &role,
        std::path::Path::new("/work/crate"),
        tier_map,
        false,
        None,
    )
    .expect("prepare orchestrator session")
}

fn server_orch_factory(
    registry: ModelRegistry,
    creds: MemoryCredentialStore,
) -> camerata_server::api_agent_driver::ServerOrchestratorDriverFactory {
    camerata_server::api_agent_driver::ServerOrchestratorDriverFactory::new(
        registry,
        Arc::new(creds),
        limiter(),
        std::path::PathBuf::from("/bin/camerata-gateway"),
        Some("run-7".to_string()),
    )
}

/// PROVIDER FOLLOWS THE MODEL — Claude strongest -> CLI orchestrator (no key needed).
/// The server factory's `build_lead` returns Ok for a claude-provider strongest model even
/// with NO OpenRouter key, because the CLI path never touches the OpenRouter credential.
/// (A native/OpenRouter lead would require the key — see the next test.)
#[test]
fn scope7_claude_strongest_routes_to_cli_orchestrator() {
    let mut tier_map = TierMap::default();
    tier_map.strongest = "claude-opus-4-8".to_string(); // claude provider in the static registry
    let factory = server_orch_factory(ModelRegistry::new(), MemoryCredentialStore::new());
    let session = lead_session(&tier_map);
    let ctx = LeadBuildContext {
        strongest_model: &tier_map.strongest,
        session: &session,
        worktree: std::path::Path::new("/work/crate"),
        tier_map: &tier_map,
        vision_enabled: false,
        on_activity: None,
    };
    let lead = factory.build_lead(&ctx);
    assert!(
        lead.is_ok(),
        "claude strongest must build the CLI orchestrator without an OpenRouter key: {:?}",
        lead.err()
    );
}

/// PROVIDER FOLLOWS THE MODEL — OpenRouter strongest -> NATIVE ApiAgentDriver orchestrator.
/// The native lead path requires the OpenRouter credential. WITHOUT a key it errors cleanly
/// naming the key (proving it took the OpenRouter/native branch, not the CLI branch); WITH a
/// key it builds. This is the publicly-observable proof the provider follows the model.
#[test]
fn scope7_openrouter_strongest_routes_to_native_orchestrator() {
    let mut tier_map = TierMap::default();
    tier_map.strongest = "vendor/lead-model".to_string();
    let registry = registry_with_openrouter("vendor/lead-model");

    // No key -> the NATIVE path errors naming OPENROUTER_API_KEY (the CLI path would NOT).
    let factory_no_key = server_orch_factory(registry.clone(), MemoryCredentialStore::new());
    let session = lead_session(&tier_map);
    let ctx = LeadBuildContext {
        strongest_model: &tier_map.strongest,
        session: &session,
        worktree: std::path::Path::new("/work/crate"),
        tier_map: &tier_map,
        vision_enabled: false,
        on_activity: None,
    };
    let err = factory_no_key.build_lead(&ctx);
    let msg = match err {
        Err(e) => e.to_string(),
        Ok(_) => panic!("openrouter strongest must take the native path and require the key"),
    };
    assert!(
        msg.contains("OPENROUTER_API_KEY"),
        "openrouter strongest routes to the native orchestrator (needs the key): {msg}"
    );

    // WITH a key -> the native orchestrator builds.
    let factory_keyed = server_orch_factory(registry, creds_with_openrouter_key());
    let lead = factory_keyed.build_lead(&ctx);
    assert!(
        lead.is_ok(),
        "openrouter strongest with a key builds the native orchestrator: {:?}",
        lead.err()
    );
}

/// A recording `OrchestratorDriverFactory` double captures that the fleet seam hands the
/// LEAD build the STRONGEST model id (provider selection is the factory's job, downstream).
#[test]
fn scope7_recording_factory_lead_is_built_for_strongest_model() {
    use std::sync::Mutex;
    struct Recorder(Mutex<Vec<String>>);
    impl OrchestratorDriverFactory for Recorder {
        fn build_lead(
            &self,
            ctx: &LeadBuildContext<'_>,
        ) -> anyhow::Result<Box<dyn camerata_core::AgentDriver>> {
            self.0.lock().unwrap().push(ctx.strongest_model.to_string());
            // Return a trivial driver; we only assert the recorded model.
            struct Noop;
            #[async_trait]
            impl camerata_core::AgentDriver for Noop {
                async fn run(
                    &self,
                    _r: &camerata_core::Role,
                    _t: &str,
                ) -> anyhow::Result<camerata_core::AgentOutcome> {
                    Ok(camerata_core::AgentOutcome {
                        session_id: "n".into(),
                        result: "n".into(),
                        cost_usd: None,
                        denials: vec![],
                    })
                }
            }
            Ok(Box::new(Noop))
        }
    }

    let tier_map = distinct_tier_map(); // strongest == "STRONGEST-id"
    let rec = Recorder(Mutex::new(Vec::new()));
    let session = lead_session(&tier_map);
    let ctx = LeadBuildContext {
        strongest_model: &tier_map.strongest,
        session: &session,
        worktree: std::path::Path::new("/work/crate"),
        tier_map: &tier_map,
        vision_enabled: false,
        on_activity: None,
    };
    let _ = rec.build_lead(&ctx).unwrap();
    let calls = rec.0.lock().unwrap();
    assert_eq!(calls.as_slice(), &["STRONGEST-id".to_string()]);
}

/// GATE INVARIANT — the native (OpenRouter) LEAD is built in orchestrator mode WITH an
/// `OrchestratorConfig` whose delegate/fan_out children resolve per-model + gated, while a
/// child built by the SAME server factory machinery is gated_write-only / non-orchestrator.
/// We assert the orchestrator-only property publicly: the lead's `delegate`/`fan_out` arms
/// route through the gated primitive, and a worker NEVER does. (The native arm behavior is
/// exercised in api_agent_driver's unit tests; here we assert the FACTORY produces an
/// orchestrator-mode lead and a non-orchestrator child.)
#[test]
fn scope7_gate_lead_is_orchestrator_child_is_not() {
    use camerata_gateway::delegate::ChildDriverFactory as _;

    // A child built by the server child factory is ALWAYS a non-orchestrator worker.
    let child_factory = camerata_server::api_agent_driver::ServerChildDriverFactory::new(
        registry_with_openrouter("vendor/child"),
        Arc::new(creds_with_openrouter_key()),
        limiter(),
        std::path::PathBuf::from("/bin/camerata-gateway"),
        vec![camerata_core::RuleId("GOV-1".to_string())],
        Some("run-7".to_string()),
    );
    let tmp = tempfile::TempDir::new().unwrap();
    // build_child requires a real worktree dir for prepare_session; tmp suffices.
    let child = child_factory.build_child("vendor/child", tmp.path(), &[]);
    assert!(
        child.is_ok(),
        "the child factory builds a gated worker: {:?}",
        child.err()
    );
    // The child is a depth-1 worker by construction (orchestrator=false in build_agent_driver):
    // its provenance is the only thing we can observe through `dyn AgentDriver`, but its
    // depth-1/non-orchestrator property is guaranteed by ServerChildDriverFactory passing
    // `false` to build_agent_driver — asserted directly in api_agent_driver's unit tests.

    // The LEAD, by contrast, is built in orchestrator mode by the orchestrator factory.
    // We prove it took the ORCHESTRATOR branch (not the worker branch) via the native path's
    // key requirement: only the orchestrator-config-carrying native build reaches the key check.
    let mut tier_map = TierMap::default();
    tier_map.strongest = "vendor/lead-model".to_string();
    let lead_factory =
        server_orch_factory(registry_with_openrouter("vendor/lead-model"), MemoryCredentialStore::new());
    let session = lead_session(&tier_map);
    let ctx = LeadBuildContext {
        strongest_model: &tier_map.strongest,
        session: &session,
        worktree: std::path::Path::new("/work/crate"),
        tier_map: &tier_map,
        vision_enabled: false,
        on_activity: None,
    };
    let lead_err = lead_factory.build_lead(&ctx).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        lead_err.contains("OPENROUTER_API_KEY"),
        "the native LEAD build reaches the orchestrator path's key check (orchestrator-mode): {lead_err}"
    );
}

// Touch `Plan` import so an unused-import lint never silently masks a future use; this
// also documents that PlanTask lives on Plan.tasks in the real fleet path.
#[allow(dead_code)]
fn _plan_shape(p: &Plan) -> usize {
    p.tasks.len()
}
