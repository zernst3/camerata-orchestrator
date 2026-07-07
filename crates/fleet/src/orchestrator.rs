//! Orchestrator-mode wiring for the governed fleet (UoW delegate, Increment 2).
//!
//! The fleet's strongest stage is the LEAD: it runs on the strongest tier AND is
//! the only stage given the governed `delegate` tool. This module owns:
//!
//! - [`lead_stage_index`] — which stage (if any) is the orchestrator.
//! - [`prepare_orchestrator_session`] — writes the lead's per-session rules +
//!   an mcp-config whose gateway env enables orchestrator mode (delegate ON,
//!   the per-tier model ids, the gateway bin, depth=0, the worktree jail).
//! - [`orchestrator_prompt_suffix`] — the lead's delegation instruction.
//!
//! Every non-lead stage uses the ordinary [`camerata_agent::prepare_session`]
//! (no delegate env), so its gateway never registers `delegate` and its driver
//! never offers it. That is the depth-1 guarantee, end to end.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_agent::{render_rules_file, HeartbeatFn, GATE_EVENTS_FILE_ENV, MCP_SERVER_KEY, RULES_FILE_ENV, WORKTREE_ROOT_ENV};
use camerata_core::{AgentDriver, Role};

use crate::tier::{CapabilityBand, TierMap};
use camerata_intake::PlanTask;

/// Context the [`OrchestratorDriverFactory`] needs to build the LEAD driver on the
/// strongest model's OWN provider.
///
/// This is the orchestrator analogue of the gateway's `ChildDriverFactory`: it lets the
/// LEAD/orchestrator stage run via whichever provider the strongest tier resolves to
/// (Claude CLI today; the native `ApiAgentDriver` when the strongest tier is an
/// OpenRouter model), while every gate invariant is preserved by the factory impl.
///
/// All fields are borrows valid for the duration of the `build_lead` call; the returned
/// driver is owned (`Box<dyn AgentDriver>`).
pub struct LeadBuildContext<'a> {
    /// The strongest-tier model id (what the lead runs on). The factory routes THIS to
    /// its own provider.
    pub strongest_model: &'a str,
    /// The prepared orchestrator session (rules + orchestrator mcp-config with delegate
    /// ON). The CLI path uses `session.mcp_config`; the native path uses the tier map +
    /// worktree to build its `OrchestratorConfig` (the session env is CLI-only).
    pub session: &'a OrchestratorSession,
    /// The shared worktree all agents are jailed to.
    pub worktree: &'a Path,
    /// The full tier map, so the native path can build its per-tier `DelegateModels` (the
    /// CLI path already has them baked into the session's mcp-config env).
    pub tier_map: &'a TierMap,
    /// Whether the Designer (vision) band is reachable for delegation this run.
    pub vision_enabled: bool,
    /// Optional activity heartbeat; the CLI path wires it into the driver so streamed
    /// output keeps `last_activity_ms` fresh for the parent tracked run.
    pub on_activity: Option<HeartbeatFn>,
}

/// Builds the LEAD/orchestrator driver for a tiered run on the strongest model's OWN
/// provider, already in orchestrator mode and gated.
///
/// Mirrors the child-driver-factory seam (`ChildDriverFactory` in the gateway): the fleet
/// owns the build loop and only calls this to obtain the lead driver, keeping the provider
/// dispatch (and the credential / registry machinery it needs) out of the fleet crate.
///
/// # Gate contract
///
/// The returned driver MUST be in orchestrator mode (it is the ONLY stage that may carry
/// `delegate`/`fan_out`) AND every child it can spawn MUST stay gated_write-only, jailed,
/// depth-1, non-orchestrator. The factory upholds this by reusing the existing gated
/// primitives (the orchestrator mcp-config for the CLI path, the
/// `OrchestratorConfig` + per-model `ChildDriverFactory` for the native path). The factory
/// MUST NOT widen the gate for non-lead stages — it is only ever called for the lead.
pub trait OrchestratorDriverFactory: Send + Sync {
    /// Build the lead driver for `ctx.strongest_model`, returning an owned,
    /// orchestrator-mode, gated `Box<dyn AgentDriver>`.
    fn build_lead(&self, ctx: &LeadBuildContext<'_>) -> anyhow::Result<Box<dyn AgentDriver>>;
}

/// Convenience alias for the injected, optional, shared orchestrator-driver factory.
pub type SharedOrchestratorDriverFactory = Arc<dyn OrchestratorDriverFactory>;

/// Env names that the gateway's `delegate` module reads. Kept in sync with
/// `crates/gateway/src/delegate.rs`. Duplicated here (rather than depending on the
/// gateway lib) because the fleet only WRITES these into the child's mcp-config;
/// it never reads them. A divergence is caught by the live-run integration, and
/// the names are asserted in this module's tests.
pub const DELEGATE_ENABLED_ENV: &str = "CAMERATA_DELEGATE_ENABLED";
pub const DELEGATE_MODELS_ENV: &str = "CAMERATA_DELEGATE_MODELS";
pub const GATEWAY_BIN_ENV: &str = "CAMERATA_GATEWAY_BIN";
pub const DELEGATE_DEPTH_ENV: &str = "CAMERATA_DELEGATE_DEPTH";

/// The index of the LEAD stage: the FIRST task classified into the strongest
/// band. Returns `None` when no task is strongest (then no agent is the
/// orchestrator and the run has no delegation — fully back-compatible).
pub fn lead_stage_index(tasks: &[PlanTask]) -> Option<usize> {
    tasks
        .iter()
        .position(|t| crate::tier::classify_task(t) == CapabilityBand::Strongest)
}

/// Serialize the per-tier model ids the orchestrator may delegate to, as the JSON
/// object the gateway's `DelegateModels` expects.
///
/// The Designer (vision) band is included as the `"vision"` key ONLY when
/// `vision_enabled` is `true` AND the project's vision chain holds a non-empty
/// primary model. When the band is disabled or unconfigured, the key is OMITTED
/// entirely, so the gateway's `DelegateModels::vision` deserializes to its empty
/// default and `resolve("vision")` returns `None` — a `delegate {tier:"vision"}` is
/// then refused cleanly, exactly like an unknown tier. This keeps the gating in ONE
/// place (the toggle controls availability, not configuration): a populated vision
/// chain with the toggle off still emits no key, and the band stays unreachable.
pub fn delegate_models_json(
    tier_map: &TierMap,
    vision_enabled: bool,
) -> Result<String, serde_json::Error> {
    let mut obj = serde_json::json!({
        "fast": tier_map.model_for(CapabilityBand::Fast),
        "balanced": tier_map.model_for(CapabilityBand::Balanced),
        "strongest": tier_map.model_for(CapabilityBand::Strongest),
    });
    // Designer/vision band: reachable only when enabled AND a model is configured.
    if vision_enabled {
        if let Some(model) = tier_map.vision.first().filter(|m| !m.trim().is_empty()) {
            obj["vision"] = serde_json::Value::String(model.clone());
        }
    }
    serde_json::to_string(&obj)
}

/// Render the orchestrator's mcp-config JSON.
///
/// Identical in shape to the non-orchestrator config (one `camerata` server →
/// the gateway binary), but its `env` ADDS the orchestrator-mode variables:
/// `CAMERATA_DELEGATE_ENABLED=1`, the per-tier models JSON, the gateway bin, and
/// `CAMERATA_DELEGATE_DEPTH=0`. Only THIS config carries them, so only the lead's
/// gateway boots in orchestrator mode.
pub fn render_orchestrator_mcp_config(
    gateway_bin: &Path,
    rules_file: &Path,
    worktree: &Path,
    tier_map: &TierMap,
    vision_enabled: bool,
    gate_events_file: Option<&Path>,
) -> Result<String, serde_json::Error> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(RULES_FILE_ENV.to_string(), rules_file.display().to_string());
    env.insert(
        WORKTREE_ROOT_ENV.to_string(),
        worktree.display().to_string(),
    );
    // LIFECYCLE-10: the lead's gateway subprocess writes to the run's OWN sink, threaded
    // per-spawn via this config's env (never the shared parent process env).
    if let Some(sink) = gate_events_file {
        env.insert(GATE_EVENTS_FILE_ENV.to_string(), sink.display().to_string());
    }
    env.insert(DELEGATE_ENABLED_ENV.to_string(), "1".to_string());
    env.insert(
        DELEGATE_MODELS_ENV.to_string(),
        delegate_models_json(tier_map, vision_enabled)?,
    );
    env.insert(GATEWAY_BIN_ENV.to_string(), gateway_bin.display().to_string());
    env.insert(DELEGATE_DEPTH_ENV.to_string(), "0".to_string());

    let server = serde_json::json!({
        "command": gateway_bin.display().to_string(),
        "args": [],
        "env": env,
    });
    let config = serde_json::json!({ "mcpServers": { MCP_SERVER_KEY: server } });
    serde_json::to_string_pretty(&config)
}

/// What [`prepare_orchestrator_session`] writes to disk for the lead stage.
pub struct OrchestratorSession {
    /// Path to the per-session rules JSON file.
    pub rules_file: PathBuf,
    /// Path to the orchestrator mcp-config (delegate env enabled).
    pub mcp_config: PathBuf,
    /// The lead role's rule subset (`role.rule_subset`). The native (non-CLI) lead path
    /// needs this to evaluate `gated_write`s and to seed the per-model child factory under
    /// the SAME subset; the CLI path reads it from `rules_file` on disk, so it is kept here
    /// only for the in-process native driver.
    pub role_rule_subset: Vec<camerata_core::RuleId>,
    /// RAII handle: the temp dir is deleted when this field is dropped.
    /// ARCH-RESOURCE-LIFECYCLE-1: every temp artifact must be RAII-cleaned.
    pub _dir: tempfile::TempDir,
}

/// Prepare the lead stage's session with the orchestrator mcp-config on disk.
///
/// Creates a fresh `TempDir` (removed automatically when [`OrchestratorSession`]
/// is dropped), writes `rules.json` (the role's subset) and `gateway.json` (the
/// orchestrator-mode mcp-config) into it. Returns the paths; the caller builds a
/// `ClaudeCliDriver::new(mcp_config).as_orchestrator(true)`.
pub fn prepare_orchestrator_session(
    gateway_bin: &Path,
    role: &Role,
    worktree: &Path,
    tier_map: &TierMap,
    vision_enabled: bool,
    gate_events_file: Option<&Path>,
) -> anyhow::Result<OrchestratorSession> {
    let dir = tempfile::TempDir::new()?;
    let session_dir = dir.path();

    let rules_file = session_dir.join("rules.json");
    std::fs::write(&rules_file, render_rules_file(role)?)?;

    let mcp_config = session_dir.join("gateway.json");
    let cfg = render_orchestrator_mcp_config(
        gateway_bin,
        &rules_file,
        worktree,
        tier_map,
        vision_enabled,
        gate_events_file,
    )?;
    std::fs::write(&mcp_config, cfg)?;

    Ok(OrchestratorSession {
        rules_file,
        mcp_config,
        role_rule_subset: role.rule_subset.clone(),
        _dir: dir,
    })
}

/// The delegation instruction appended to the lead stage's task prompt. When
/// `vision_enabled` is true (the project's Designer band is on AND has a model), a
/// vision-routing block is appended that teaches the lead to route visual/design work
/// through the `vision` tier using an HTML/Tailwind mockup as the hand-off contract.
pub fn orchestrator_prompt_suffix(vision_enabled: bool) -> String {
    let mut s = String::from(
        "\n\nYou are the LEAD on the strongest tier. Do the complex, one-way-door work \
         yourself. Delegate well-scoped, simpler subtasks to the balanced or fast tiers \
         via the `delegate` tool (argument: {\"subtask\": \"...\", \"tier\": \"balanced\" | \
         \"fast\"}). The delegate runs ONE gated child and returns its full output. If a \
         delegate returns text starting with `INCOMPLETE:` or otherwise signals the work \
         is above its tier, do it yourself or re-delegate to a higher tier. You cannot be \
         delegated to, and your delegates cannot delegate further.",
    );
    if vision_enabled {
        s.push_str(
            "\n\nVISION/DESIGN WORK: For visual or UI/design subtasks (building or restyling \
             a page or component, matching a mockup, laying out a screen), route through the \
             Designer (vision) tier in THREE steps, never directly: (1) FIRST gather the \
             EXISTING in-code layout and shared styling — read the relevant component file(s) \
             and the project's style/theme tokens — so the design matches what already exists; \
             (2) `delegate {\"tier\": \"vision\", \"subtask\": \"Produce an HTML/Tailwind mockup \
             of <X> using these existing tokens/styles: <paste them>. Output ONLY HTML + \
             Tailwind classes, no framework code.\"}` — the vision tier returns an HTML/Tailwind \
             mockup (an intermediate representation), NOT framework code; (3) take that mockup \
             and `delegate {\"tier\": \"balanced\" or \"strongest\", \"subtask\": \"Translate \
             this HTML/Tailwind mockup into <the repo's UI framework> components, consistent \
             with the existing code and tokens: <paste the mockup>.\"}`. The vision model NEVER \
             writes framework code; a logic tier always does the translation.",
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::RuleId;
    use camerata_intake::TaskKind;

    fn task(kind: TaskKind, desc: &str) -> PlanTask {
        PlanTask {
            role: "Agent".to_string(),
            kind,
            description: desc.to_string(),
        }
    }

    fn role() -> Role {
        Role {
            name: "Lead".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/".to_string()],
        }
    }

    #[test]
    fn lead_is_the_first_strongest_task() {
        let tasks = vec![
            task(TaskKind::Test, "tests"),         // Fast
            task(TaskKind::Database, "schema"),    // Balanced
            task(TaskKind::Backend, "domain a"),   // Strongest  <- lead
            task(TaskKind::Backend, "domain b"),   // Strongest
        ];
        assert_eq!(lead_stage_index(&tasks), Some(2));
    }

    #[test]
    fn no_lead_when_no_strongest_task() {
        let tasks = vec![
            task(TaskKind::Test, "tests"),
            task(TaskKind::Database, "schema"),
            task(TaskKind::Frontend, "view"),
        ];
        assert_eq!(lead_stage_index(&tasks), None);
    }

    #[test]
    fn delegate_models_json_has_all_three_tiers() {
        // vision_enabled=false: the three logic tiers, NO vision key.
        let json = delegate_models_json(&TierMap::default(), false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["fast"], "claude-haiku-4-5-20251001");
        assert_eq!(v["balanced"], "claude-sonnet-4-6");
        assert_eq!(v["strongest"], "claude-opus-4-8");
        assert!(v.get("vision").is_none(), "no vision key when disabled");
    }

    #[test]
    fn delegate_models_json_omits_vision_when_disabled_even_if_configured() {
        // Gating is the toggle, not configuration: a populated vision chain with the
        // toggle OFF still emits NO vision key, so the band stays unreachable.
        let mut m = TierMap::default();
        m.vision = vec!["claude-vision-model".to_string()];
        let json = delegate_models_json(&m, false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("vision").is_none(),
            "vision must be omitted when vision_enabled is false"
        );
    }

    #[test]
    fn delegate_models_json_includes_vision_when_enabled_and_configured() {
        let mut m = TierMap::default();
        m.vision = vec!["claude-vision-model".to_string()];
        let json = delegate_models_json(&m, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["vision"], "claude-vision-model",
            "vision key present when enabled + configured (primary of the chain)"
        );
    }

    #[test]
    fn delegate_models_json_omits_vision_when_enabled_but_unconfigured() {
        // Enabled but no model assigned (empty chain): no key, so resolve("vision")
        // returns None on the child side and the delegate is refused cleanly.
        let m = TierMap::default(); // vision == []
        let json = delegate_models_json(&m, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("vision").is_none(),
            "no vision key when enabled but no model configured"
        );
    }

    #[test]
    fn orchestrator_mcp_config_enables_delegate_with_full_env() {
        let cfg = render_orchestrator_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            Path::new("/work/crate"),
            &TierMap::default(),
            false,
            None,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let env = &v["mcpServers"][MCP_SERVER_KEY]["env"];
        assert_eq!(env[DELEGATE_ENABLED_ENV], "1");
        assert_eq!(env[DELEGATE_DEPTH_ENV], "0");
        assert_eq!(env[GATEWAY_BIN_ENV], "/bin/camerata-gateway");
        assert_eq!(env[RULES_FILE_ENV], "/tmp/s/rules.json");
        assert_eq!(env[WORKTREE_ROOT_ENV], "/work/crate");
        // No sink passed -> the gate-events env is absent.
        assert!(env.get(GATE_EVENTS_FILE_ENV).is_none());
    }

    #[test]
    fn orchestrator_mcp_config_sets_gate_events_sink_when_given() {
        // LIFECYCLE-10: the lead's gateway subprocess sink is threaded per-spawn.
        let cfg = render_orchestrator_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            Path::new("/work/crate"),
            &TierMap::default(),
            false,
            Some(Path::new("/runs/run-xyz/gate-events.jsonl")),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let env = &v["mcpServers"][MCP_SERVER_KEY]["env"];
        assert_eq!(env[GATE_EVENTS_FILE_ENV], "/runs/run-xyz/gate-events.jsonl");
        // The models env is a JSON object string with all three tiers.
        let models: serde_json::Value =
            serde_json::from_str(env[DELEGATE_MODELS_ENV].as_str().unwrap()).unwrap();
        assert_eq!(models["balanced"], "claude-sonnet-4-6");
    }

    #[test]
    fn orchestrator_mcp_config_includes_vision_model_when_enabled() {
        // With vision enabled + a configured vision model, the orchestrator's delegate
        // models env carries the vision key, making the Designer band reachable.
        let mut m = TierMap::default();
        m.vision = vec!["claude-vision-model".to_string()];
        let cfg = render_orchestrator_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            Path::new("/work/crate"),
            &m,
            true,
            None,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let env = &v["mcpServers"][MCP_SERVER_KEY]["env"];
        let models: serde_json::Value =
            serde_json::from_str(env[DELEGATE_MODELS_ENV].as_str().unwrap()).unwrap();
        assert_eq!(models["vision"], "claude-vision-model");
    }

    #[test]
    fn prepare_orchestrator_session_writes_both_files() {
        // prepare_orchestrator_session now manages its own TempDir (ARCH-RESOURCE-LIFECYCLE-1).
        let s = prepare_orchestrator_session(
            Path::new("/bin/camerata-gateway"),
            &role(),
            Path::new("/work/crate"),
            &TierMap::default(),
            false,
            None,
        )
        .unwrap();
        assert!(s.rules_file.exists());
        assert!(s.mcp_config.exists());
        let cfg = std::fs::read_to_string(&s.mcp_config).unwrap();
        assert!(cfg.contains(DELEGATE_ENABLED_ENV));
        // s._dir (TempDir) cleans up on drop — no manual remove_dir_all needed.
    }

    #[test]
    fn prompt_suffix_mentions_delegate_and_escalation() {
        let s = orchestrator_prompt_suffix(false);
        assert!(s.contains("delegate"));
        assert!(s.contains("INCOMPLETE:"));
        assert!(s.contains("strongest"));
    }

    #[test]
    fn prompt_suffix_omits_vision_block_when_disabled() {
        let s = orchestrator_prompt_suffix(false);
        assert!(!s.contains("VISION/DESIGN WORK"), "no vision block when disabled");
        assert!(!s.to_lowercase().contains("html/tailwind mockup"));
    }

    #[test]
    fn prompt_suffix_adds_vision_routing_when_enabled() {
        let s = orchestrator_prompt_suffix(true);
        // Keeps the base delegation instruction.
        assert!(s.contains("delegate"));
        // Adds the vision-routing + IR-handoff guidance.
        assert!(s.contains("VISION/DESIGN WORK"));
        assert!(s.contains("\"tier\": \"vision\""));
        assert!(s.contains("HTML/Tailwind mockup"));
        // The contract: vision returns the IR, a logic tier translates it.
        assert!(s.contains("NEVER writes framework code"));
        assert!(s.contains("Translate this HTML/Tailwind mockup"));
    }
}
