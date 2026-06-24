//! Orchestrator-mode `delegate` support for the governance gateway.
//!
//! Increment 2 of the UoW governed-dev redesign. The lead/orchestrator agent
//! (on the strongest tier) gets ONE extra governed capability beyond
//! `gated_write`: a `delegate(subtask, tier)` MCP tool. Calling it spawns a
//! SINGLE **gated** `claude -p` child on the chosen tier's model, in the SAME
//! worktree, wired to its OWN gateway instance that exposes `gated_write` ONLY
//! (delegate DISABLED), runs the subtask synchronously, and returns the child's
//! full output to the orchestrator.
//!
//! # Gate preservation
//!
//! The raw CLI `Task` tool stays on the disallowed list for EVERY agent. The
//! ONLY spawn path is this tool, and the gateway — not the agent — performs the
//! spawn, gating the child by construction:
//!
//! - the child's `--allowedTools` is `allowed_tools_for_role` (NO delegate), so
//!   it cannot re-delegate → **depth is inherently 1**;
//! - the child's gateway boots WITHOUT the orchestrator-mode env, so it never
//!   even registers `delegate`;
//! - the child inherits the SAME rule subset + worktree jail as the orchestrator.
//!
//! # Depth guard (belt-and-suspenders)
//!
//! Beyond the structural depth-1 guarantee, the handler refuses to spawn once
//! [`OrchestratorConfig::depth`] reaches [`OrchestratorConfig::max_depth`], and
//! it threads `depth + 1` into the child's gateway env. So even a misconfiguration
//! that re-enabled orchestrator mode on a child cannot recurse past the cap.

use std::collections::BTreeMap;
use std::path::PathBuf;

use camerata_agent::{
    ClaudeCliDriver, GATED_WRITE_TOOL, MCP_SERVER_KEY, RULES_FILE_ENV, WORKTREE_ROOT_ENV,
};
use camerata_core::{AgentDriver, Role, RuleId};

/// Env flag that puts the gateway in ORCHESTRATOR mode. When unset (the default,
/// for every non-orchestrator agent including every delegate child) the
/// `delegate` tool refuses.
pub const DELEGATE_ENABLED_ENV: &str = "CAMERATA_DELEGATE_ENABLED";

/// Env carrying the per-tier model ids as a JSON object, e.g.
/// `{"fast":"claude-haiku-...","balanced":"claude-sonnet-...","strongest":"claude-opus-..."}`.
pub const DELEGATE_MODELS_ENV: &str = "CAMERATA_DELEGATE_MODELS";

/// Env carrying the absolute path to the built `camerata-gateway` binary, used to
/// wire each delegate child's OWN gateway instance.
pub const GATEWAY_BIN_ENV: &str = "CAMERATA_GATEWAY_BIN";

/// Env carrying the CURRENT delegation depth (default `0`). The handler refuses
/// once it reaches [`DELEGATE_MAX_DEPTH_ENV`]; the spawned child gets `depth + 1`.
pub const DELEGATE_DEPTH_ENV: &str = "CAMERATA_DELEGATE_DEPTH";

/// Env carrying the MAX delegation depth (default `1`). With the structural
/// depth-1 guarantee this is a redundant safety net.
pub const DELEGATE_MAX_DEPTH_ENV: &str = "CAMERATA_DELEGATE_MAX_DEPTH";

/// The default maximum delegation depth when [`DELEGATE_MAX_DEPTH_ENV`] is unset.
pub const DEFAULT_MAX_DEPTH: u32 = 1;

/// The per-tier model ids the orchestrator delegates to. Only `fast` and
/// `balanced` are valid `tier` arguments to the tool; `strongest` is the
/// orchestrator's own tier (delegating to it would be a no-op self-delegate), but
/// it is carried so the map is complete and a future re-delegate-higher path can
/// resolve it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DelegateModels {
    pub fast: String,
    pub balanced: String,
    pub strongest: String,
}

impl DelegateModels {
    /// Resolve a tier label (`"fast"` | `"balanced"` | `"strongest"`,
    /// case-insensitive) to a concrete model id. Returns `None` for anything else.
    pub fn resolve(&self, tier: &str) -> Option<&str> {
        match tier.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(&self.fast),
            "balanced" => Some(&self.balanced),
            "strongest" => Some(&self.strongest),
            _ => None,
        }
    }
}

/// The orchestrator-mode configuration the gateway reads from the environment.
/// `None` (the default) means the `delegate` tool is disabled.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Per-tier model ids.
    pub models: DelegateModels,
    /// The shared worktree all agents (orchestrator + children) are jailed to.
    pub worktree_root: PathBuf,
    /// Path to the built gateway binary, used for each child's own gateway.
    pub gateway_bin: PathBuf,
    /// Current delegation depth (the orchestrator itself is `0`).
    pub depth: u32,
    /// Maximum delegation depth.
    pub max_depth: u32,
}

impl OrchestratorConfig {
    /// Load the orchestrator-mode config from the environment, or `None` when
    /// orchestrator mode is not enabled / is incompletely configured. A missing or
    /// malformed required field fails CLOSED to `None` (delegate stays disabled)
    /// rather than spawning an unconfigured child.
    pub fn from_env() -> Option<Self> {
        // Gate on the explicit enable flag first.
        let enabled = std::env::var(DELEGATE_ENABLED_ENV)
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        if !enabled {
            return None;
        }

        let models_json = std::env::var(DELEGATE_MODELS_ENV).ok()?;
        let models: DelegateModels = serde_json::from_str(&models_json).ok()?;

        let worktree_root = std::env::var_os(WORKTREE_ROOT_ENV).map(PathBuf::from)?;
        let gateway_bin = std::env::var_os(GATEWAY_BIN_ENV).map(PathBuf::from)?;

        let depth = std::env::var(DELEGATE_DEPTH_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let max_depth = std::env::var(DELEGATE_MAX_DEPTH_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_MAX_DEPTH);

        Some(Self {
            models,
            worktree_root,
            gateway_bin,
            depth,
            max_depth,
        })
    }

    /// Whether a further delegation is permitted by the explicit depth guard.
    pub fn may_delegate(&self) -> bool {
        self.depth < self.max_depth
    }
}

/// The result of a `delegate` call, as returned to the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegateError {
    /// The tier label was not one of `fast` / `balanced` / `strongest`.
    UnknownTier(String),
    /// The explicit depth guard refused (already at max depth).
    DepthExceeded { depth: u32, max_depth: u32 },
}

impl std::fmt::Display for DelegateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DelegateError::UnknownTier(t) => write!(
                f,
                "DELEGATE REFUSED: unknown tier '{t}' (expected fast | balanced | strongest)"
            ),
            DelegateError::DepthExceeded { depth, max_depth } => write!(
                f,
                "DELEGATE REFUSED: depth guard tripped (depth={depth} >= max_depth={max_depth}); \
                 do the work yourself"
            ),
        }
    }
}

/// Build the mcp-config JSON for a delegate CHILD's gateway.
///
/// The child gateway is launched with:
/// - [`RULES_FILE_ENV`] → the child's rules file (same subset as the orchestrator),
/// - [`WORKTREE_ROOT_ENV`] → the shared worktree jail,
/// - [`DELEGATE_DEPTH_ENV`] → `depth + 1`,
///
/// and **crucially WITHOUT** [`DELEGATE_ENABLED_ENV`] / [`DELEGATE_MODELS_ENV`] /
/// [`GATEWAY_BIN_ENV`], so the child boots in NON-orchestrator mode and never
/// registers `delegate`. The server key is [`MCP_SERVER_KEY`] so the child's
/// governed write tool is exactly [`GATED_WRITE_TOOL`].
pub fn render_child_mcp_config(
    gateway_bin: &std::path::Path,
    rules_file: &std::path::Path,
    worktree_root: &std::path::Path,
    child_depth: u32,
) -> Result<String, serde_json::Error> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(RULES_FILE_ENV.to_string(), rules_file.display().to_string());
    env.insert(
        WORKTREE_ROOT_ENV.to_string(),
        worktree_root.display().to_string(),
    );
    // Belt-and-suspenders: carry the incremented depth so even a child that were
    // (mis)configured into orchestrator mode would see it and refuse at max_depth.
    env.insert(DELEGATE_DEPTH_ENV.to_string(), child_depth.to_string());

    let server = serde_json::json!({
        "command": gateway_bin.display().to_string(),
        "args": [],
        "env": env,
    });
    let config = serde_json::json!({
        "mcpServers": { MCP_SERVER_KEY: server }
    });
    serde_json::to_string_pretty(&config)
}

/// A minimal governed role for a delegate child: the orchestrator's own rule
/// subset, scoped to the shared worktree. The name is for provenance only; the
/// tool surface is `gated_write` ONLY (delegate is never granted to children).
pub fn child_role(rule_subset: Vec<RuleId>, worktree_root: &std::path::Path) -> Role {
    Role {
        name: "delegate-child".to_string(),
        rule_subset,
        allowed_paths: vec![worktree_root.display().to_string()],
    }
}

/// Run a single delegated subtask synchronously and return the child's output.
///
/// This is the spawn-a-gated-child mechanism: it writes a per-child session
/// (rules + mcp-config), builds a [`ClaudeCliDriver`] that is NOT an orchestrator
/// (so `--allowedTools` excludes [`DELEGATE_TOOL`]), pins the child to the chosen
/// tier's model + the shared worktree, runs `claude -p`, and returns the result.
///
/// `rule_subset` is the orchestrator gateway's active subset, so the child is born
/// under identical governance.
pub async fn run_delegated(
    config: &OrchestratorConfig,
    rule_subset: Vec<RuleId>,
    subtask: &str,
    tier: &str,
) -> Result<String, DelegateError> {
    // 1) Explicit depth guard (belt-and-suspenders over the structural depth-1).
    if !config.may_delegate() {
        return Err(DelegateError::DepthExceeded {
            depth: config.depth,
            max_depth: config.max_depth,
        });
    }

    // 2) Resolve tier -> model.
    let model = config
        .models
        .resolve(tier)
        .ok_or_else(|| DelegateError::UnknownTier(tier.to_string()))?
        .to_string();

    let child_depth = config.depth + 1;

    // 3) Materialize a per-child session dir (rules + child mcp-config).
    //    ARCH-RESOURCE-LIFECYCLE-1: use a TempDir so the dir is removed on every exit
    //    path (normal return, early error return, or future panic) without a manual cleanup.
    let session_tmp = match tempfile::TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return Ok(format!(
                "DELEGATE could not start: failed to create session temp dir: {e}"
            ));
        }
    };
    let session_dir = session_tmp.path();

    let rules_file = session_dir.join("rules.json");
    let rules_json = match serde_json::to_string_pretty(&rule_subset) {
        Ok(j) => j,
        Err(e) => return Ok(format!("DELEGATE could not serialize rules: {e}")),
    };
    if let Err(e) = std::fs::write(&rules_file, rules_json) {
        return Ok(format!("DELEGATE could not write rules file: {e}"));
    }

    let mcp_config = session_dir.join("gateway.json");
    let cfg_json = match render_child_mcp_config(
        &config.gateway_bin,
        &rules_file,
        &config.worktree_root,
        child_depth,
    ) {
        Ok(j) => j,
        Err(e) => return Ok(format!("DELEGATE could not render child mcp-config: {e}")),
    };
    if let Err(e) = std::fs::write(&mcp_config, cfg_json) {
        return Ok(format!("DELEGATE could not write child mcp-config: {e}"));
    }

    // 4) Build a GATED child driver: NOT an orchestrator (no delegate tool),
    //    pinned to the tier model, jailed to the shared worktree.
    let driver = ClaudeCliDriver::new(mcp_config.display().to_string())
        .with_worktree(&config.worktree_root)
        .with_model(&model)
        .as_orchestrator(false);

    let role = child_role(rule_subset, &config.worktree_root);

    // 5) Frame the subtask so the child knows how to signal "above my tier".
    let framed = format!(
        "You are a GATED delegate agent on the `{tier}` tier (model: {model}). Your ONLY way to \
         write files is the `{write}` tool. You CANNOT delegate further.\n\n\
         Subtask:\n{subtask}\n\n\
         If you can complete the subtask, do it through `{write}` and report what you did. \
         If the subtask is above your tier, ambiguous, or you cannot complete it, DO NOT guess: \
         return a short message that begins with `INCOMPLETE:` explaining why, and your parent \
         orchestrator will handle it or re-delegate to a higher tier.",
        tier = tier,
        model = model,
        write = GATED_WRITE_TOOL,
        subtask = subtask,
    );

    // 6) Run synchronously and return the child's full output.
    match driver.run(&role, &framed).await {
        Ok(outcome) => {
            let denials = if outcome.denials.is_empty() {
                String::new()
            } else {
                format!("\n[gate denials: {}]", outcome.denials.join("; "))
            };
            Ok(format!(
                "[delegate tier={tier} model={model} depth={child_depth}]\n{}{denials}",
                outcome.result
            ))
        }
        Err(e) => Ok(format!(
            "[delegate tier={tier} model={model}] child run FAILED: {e}\n\
             INCOMPLETE: the delegate child could not run; do this yourself."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_agent::{allowed_tools_for_role, DELEGATE_TOOL};

    fn models() -> DelegateModels {
        DelegateModels {
            fast: "claude-haiku-4-5-20251001".to_string(),
            balanced: "claude-sonnet-4-6".to_string(),
            strongest: "claude-opus-4-8".to_string(),
        }
    }

    fn cfg(depth: u32, max_depth: u32) -> OrchestratorConfig {
        OrchestratorConfig {
            models: models(),
            worktree_root: PathBuf::from("/work/crate"),
            gateway_bin: PathBuf::from("/bin/camerata-gateway"),
            depth,
            max_depth,
        }
    }

    #[test]
    fn resolve_tier_to_model_is_case_insensitive() {
        let m = models();
        assert_eq!(m.resolve("fast"), Some("claude-haiku-4-5-20251001"));
        assert_eq!(m.resolve("BALANCED"), Some("claude-sonnet-4-6"));
        assert_eq!(m.resolve("  Strongest "), Some("claude-opus-4-8"));
        assert_eq!(m.resolve("ultra"), None);
        assert_eq!(m.resolve(""), None);
    }

    #[test]
    fn depth_guard_permits_below_max_and_refuses_at_max() {
        assert!(cfg(0, 1).may_delegate());
        assert!(!cfg(1, 1).may_delegate());
        assert!(!cfg(2, 1).may_delegate());
        assert!(cfg(0, 2).may_delegate());
        assert!(cfg(1, 2).may_delegate());
        assert!(!cfg(2, 2).may_delegate());
    }

    #[tokio::test]
    async fn run_delegated_refuses_unknown_tier_without_spawning() {
        // depth allows, but the tier is bogus -> refuse BEFORE any spawn/IO.
        let err = run_delegated(&cfg(0, 1), vec![RuleId("GOV-1".to_string())], "do x", "ultra")
            .await
            .unwrap_err();
        assert_eq!(err, DelegateError::UnknownTier("ultra".to_string()));
    }

    #[tokio::test]
    async fn run_delegated_refuses_at_depth_cap_without_spawning() {
        // Even a valid tier is refused once the depth guard is tripped. No child
        // process is spawned (no token spend), keeping CI token-free.
        let err = run_delegated(
            &cfg(1, 1),
            vec![RuleId("GOV-1".to_string())],
            "do x",
            "fast",
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            DelegateError::DepthExceeded {
                depth: 1,
                max_depth: 1
            }
        );
    }

    #[test]
    fn child_role_carries_subset_and_worktree_and_is_not_delegate_capable() {
        let role = child_role(
            vec![RuleId("GOV-1".to_string())],
            std::path::Path::new("/work/crate"),
        );
        assert_eq!(role.rule_subset, vec![RuleId("GOV-1".to_string())]);
        assert_eq!(role.allowed_paths, vec!["/work/crate".to_string()]);
        // The child's allowed tools (non-orchestrator) must NOT include delegate.
        let tools = allowed_tools_for_role(&role);
        assert!(!tools.iter().any(|t| t == DELEGATE_TOOL));
        assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
    }

    #[test]
    fn child_mcp_config_disables_delegate_and_increments_depth() {
        let cfg = render_child_mcp_config(
            std::path::Path::new("/bin/camerata-gateway"),
            std::path::Path::new("/tmp/s/rules.json"),
            std::path::Path::new("/work/crate"),
            1,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        let env = &v["mcpServers"][MCP_SERVER_KEY]["env"];
        // The child gets rules + worktree jail + incremented depth...
        assert_eq!(env[RULES_FILE_ENV], "/tmp/s/rules.json");
        assert_eq!(env[WORKTREE_ROOT_ENV], "/work/crate");
        assert_eq!(env[DELEGATE_DEPTH_ENV], "1");
        // ...but NONE of the orchestrator-mode enablers, so it never registers delegate.
        assert!(env.get(DELEGATE_ENABLED_ENV).is_none());
        assert!(env.get(DELEGATE_MODELS_ENV).is_none());
        assert!(env.get(GATEWAY_BIN_ENV).is_none());
    }

    #[test]
    fn from_env_returns_none_when_disabled() {
        // No global env mutation; just exercise the documented default. With the
        // enable flag absent, orchestrator mode is off. (We avoid setting process
        // env in tests to keep them order-independent and token-free.)
        // Sanity: the parsing of the models JSON is covered separately.
        let parsed: DelegateModels =
            serde_json::from_str(r#"{"fast":"f","balanced":"b","strongest":"s"}"#).unwrap();
        assert_eq!(parsed.fast, "f");
        assert_eq!(parsed.balanced, "b");
        assert_eq!(parsed.strongest, "s");
    }
}
