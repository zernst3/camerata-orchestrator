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

use camerata_agent::{render_rules_file, MCP_SERVER_KEY, RULES_FILE_ENV, WORKTREE_ROOT_ENV};
use camerata_core::Role;

use crate::tier::{CapabilityBand, TierMap};
use camerata_intake::PlanTask;

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
pub fn delegate_models_json(tier_map: &TierMap) -> Result<String, serde_json::Error> {
    let obj = serde_json::json!({
        "fast": tier_map.model_for(CapabilityBand::Fast),
        "balanced": tier_map.model_for(CapabilityBand::Balanced),
        "strongest": tier_map.model_for(CapabilityBand::Strongest),
    });
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
) -> Result<String, serde_json::Error> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(RULES_FILE_ENV.to_string(), rules_file.display().to_string());
    env.insert(
        WORKTREE_ROOT_ENV.to_string(),
        worktree.display().to_string(),
    );
    env.insert(DELEGATE_ENABLED_ENV.to_string(), "1".to_string());
    env.insert(DELEGATE_MODELS_ENV.to_string(), delegate_models_json(tier_map)?);
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
) -> anyhow::Result<OrchestratorSession> {
    let dir = tempfile::TempDir::new()?;
    let session_dir = dir.path();

    let rules_file = session_dir.join("rules.json");
    std::fs::write(&rules_file, render_rules_file(role)?)?;

    let mcp_config = session_dir.join("gateway.json");
    let cfg = render_orchestrator_mcp_config(gateway_bin, &rules_file, worktree, tier_map)?;
    std::fs::write(&mcp_config, cfg)?;

    Ok(OrchestratorSession {
        rules_file,
        mcp_config,
        _dir: dir,
    })
}

/// The delegation instruction appended to the lead stage's task prompt.
pub fn orchestrator_prompt_suffix() -> &'static str {
    "\n\nYou are the LEAD on the strongest tier. Do the complex, one-way-door work \
     yourself. Delegate well-scoped, simpler subtasks to the balanced or fast tiers \
     via the `delegate` tool (argument: {\"subtask\": \"...\", \"tier\": \"balanced\" | \
     \"fast\"}). The delegate runs ONE gated child and returns its full output. If a \
     delegate returns text starting with `INCOMPLETE:` or otherwise signals the work \
     is above its tier, do it yourself or re-delegate to a higher tier. You cannot be \
     delegated to, and your delegates cannot delegate further."
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
        let json = delegate_models_json(&TierMap::default()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["fast"], "claude-haiku-4-5-20251001");
        assert_eq!(v["balanced"], "claude-sonnet-4-6");
        assert_eq!(v["strongest"], "claude-opus-4-8");
    }

    #[test]
    fn orchestrator_mcp_config_enables_delegate_with_full_env() {
        let cfg = render_orchestrator_mcp_config(
            Path::new("/bin/camerata-gateway"),
            Path::new("/tmp/s/rules.json"),
            Path::new("/work/crate"),
            &TierMap::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let env = &v["mcpServers"][MCP_SERVER_KEY]["env"];
        assert_eq!(env[DELEGATE_ENABLED_ENV], "1");
        assert_eq!(env[DELEGATE_DEPTH_ENV], "0");
        assert_eq!(env[GATEWAY_BIN_ENV], "/bin/camerata-gateway");
        assert_eq!(env[RULES_FILE_ENV], "/tmp/s/rules.json");
        assert_eq!(env[WORKTREE_ROOT_ENV], "/work/crate");
        // The models env is a JSON object string with all three tiers.
        let models: serde_json::Value =
            serde_json::from_str(env[DELEGATE_MODELS_ENV].as_str().unwrap()).unwrap();
        assert_eq!(models["balanced"], "claude-sonnet-4-6");
    }

    #[test]
    fn prepare_orchestrator_session_writes_both_files() {
        // prepare_orchestrator_session now manages its own TempDir (ARCH-RESOURCE-LIFECYCLE-1).
        let s = prepare_orchestrator_session(
            Path::new("/bin/camerata-gateway"),
            &role(),
            Path::new("/work/crate"),
            &TierMap::default(),
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
        let s = orchestrator_prompt_suffix();
        assert!(s.contains("delegate"));
        assert!(s.contains("INCOMPLETE:"));
        assert!(s.contains("strongest"));
    }
}
