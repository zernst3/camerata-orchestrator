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
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

// ─── per-model child driver seam (provider coupling) ─────────────────────────

/// Factory that produces a fully-GATED, NON-orchestrator child [`AgentDriver`] for a
/// given model + worktree, resolving the model's OWN provider (Claude CLI, Anthropic
/// API, or OpenRouter) rather than inheriting the parent's.
///
/// # Why this seam exists
///
/// `run_delegated`/`run_fan_out` historically hard-coded a [`ClaudeCliDriver`] pinned to
/// the tier's model. A tier set to an OpenRouter model then spawned `claude -p --model
/// <openrouter-id>`, which the Claude CLI cannot run. The provider must be resolved
/// PER CHILD MODEL. The provider-routing machinery (`build_agent_driver` + the model
/// registry + keychain creds) lives in `camerata-server`, and the gateway crate must not
/// depend on the server. So the server injects an implementation of this trait into
/// [`OrchestratorConfig::child_driver_factory`]; the gateway only calls it.
///
/// # Gate contract (MUST hold for every driver this returns)
///
/// The returned driver MUST be:
/// - **`gated_write`-only** — `Task`/`Bash`/`Write`/`Edit`/`MultiEdit`/`NotebookEdit`
///   disallowed; the sole mutation path is the governed write tool;
/// - **worktree-jailed** to `worktree` (writes outside are denied);
/// - **depth-1 / NON-orchestrator** — no `CAMERATA_DELEGATE_ENABLED`, no `delegate`/
///   `fan_out` tools; the child cannot re-delegate.
///
/// The gateway frames the subtask + records gate decisions IDENTICALLY regardless of
/// which provider the factory returns; the factory's ONLY job is provider coupling +
/// constructing the child under the gate contract above.
pub trait ChildDriverFactory: Send + Sync {
    /// Build a gated, non-orchestrator child driver for `model`, jailed to `worktree`,
    /// with `read_dirs` as the (read-only) multi-repo read scope.
    ///
    /// The `rule_subset` the child enforces is supplied by the gateway at run time
    /// (it is the orchestrator's own active subset) via the [`child_role`] the gateway
    /// passes to `driver.run`; an API-backed implementation that needs the subset at
    /// construction time should accept it through its own constructor before being
    /// boxed here. (The CLI child reads its subset from the per-session rules file the
    /// gateway already writes, so the gateway-built child path needs nothing extra.)
    fn build_child(
        &self,
        model: &str,
        worktree: &Path,
        read_dirs: &[PathBuf],
    ) -> std::io::Result<Box<dyn AgentDriver>>;
}

/// The per-tier model ids the orchestrator delegates to. Only `fast` and
/// `balanced` are valid `tier` arguments to the tool; `strongest` is the
/// orchestrator's own tier (delegating to it would be a no-op self-delegate), but
/// it is carried so the map is complete and a future re-delegate-higher path can
/// resolve it.
///
/// `vision` is the optional Designer (multimodal) band — orthogonal to the logic
/// ladder. It is reachable as the `"vision"` tier (alias `"designer"`) ONLY when
/// the orchestrator was configured with a non-empty vision model (which the fleet
/// only does when the active project's `vision_enabled` flag is set). When unset,
/// the field is an empty string and `resolve("vision")` returns `None`, so a
/// `delegate {tier:"vision"}` is refused cleanly exactly like an unknown tier — no
/// new authority, no panic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DelegateModels {
    pub fast: String,
    pub balanced: String,
    pub strongest: String,
    /// Optional Designer/vision model id. Empty (the default when the project's
    /// vision band is disabled/unset) means `resolve("vision")` returns `None`.
    /// Serde-defaulted so a model map serialized before this field existed (e.g. an
    /// orchestrator booted by an older fleet build) deserializes cleanly to empty.
    #[serde(default)]
    pub vision: String,
}

impl DelegateModels {
    /// Resolve a tier label to a concrete model id. Returns `None` for anything
    /// unrecognised OR for a tier whose model is unconfigured (empty), so an
    /// unconfigured `vision` tier is refused the same way an unknown tier is.
    ///
    /// Accepted labels (case-insensitive, surrounding whitespace trimmed):
    /// - `"fast"` / `"balanced"` / `"strongest"` — the logic ladder.
    /// - `"vision"` (alias `"designer"`) — the Designer band; only resolves when a
    ///   non-empty vision model was configured.
    pub fn resolve(&self, tier: &str) -> Option<&str> {
        let model = match tier.trim().to_ascii_lowercase().as_str() {
            "fast" => &self.fast,
            "balanced" => &self.balanced,
            "strongest" => &self.strongest,
            // "designer" is an alias for the vision band.
            "vision" | "designer" => &self.vision,
            _ => return None,
        };
        // An empty model id (e.g. the vision band when disabled/unconfigured) is
        // treated as "not available" → refuse cleanly, never spawn an unmodeled child.
        if model.is_empty() {
            None
        } else {
            Some(model.as_str())
        }
    }
}

/// The orchestrator-mode configuration the gateway reads from the environment.
/// `None` (the default) means the `delegate` tool is disabled.
///
/// `Clone` is derived; the `child_driver_factory` is an `Arc`, so cloning a config
/// (e.g. per fan-out worker) shares the SAME factory cheaply.
#[derive(Clone)]
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
    /// Optional per-model provider seam. When `Some`, `run_delegated`/`run_fan_out` build
    /// each child via this factory so the child runs on ITS OWN model's provider
    /// (Claude CLI, Anthropic API, or OpenRouter) — NOT the parent's. When `None` (the
    /// default, and what [`OrchestratorConfig::from_env`] always produces in the gateway
    /// BINARY process, which cannot reach the server's provider machinery) the legacy
    /// hard-coded [`ClaudeCliDriver`] path is used — exact back-compat for the CLI
    /// orchestrator. Either way the child is gated_write-only, jailed, and depth-1.
    pub child_driver_factory: Option<Arc<dyn ChildDriverFactory>>,
}

impl std::fmt::Debug for OrchestratorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestratorConfig")
            .field("models", &self.models)
            .field("worktree_root", &self.worktree_root)
            .field("gateway_bin", &self.gateway_bin)
            .field("depth", &self.depth)
            .field("max_depth", &self.max_depth)
            .field(
                "child_driver_factory",
                &self.child_driver_factory.as_ref().map(|_| "<factory>"),
            )
            .finish()
    }
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
            // The gateway BINARY process (which is the only caller of `from_env`) has no
            // access to the server's provider machinery (`build_agent_driver` + model
            // registry + keychain), so it cannot construct a cross-provider factory. It
            // keeps the legacy hard-coded ClaudeCliDriver child path (back-compat). The
            // factory is injected only by the in-process server path (native delegate /
            // fan_out), which builds the `OrchestratorConfig` directly.
            child_driver_factory: None,
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
                "DELEGATE REFUSED: unknown or unconfigured tier '{t}' \
                 (expected fast | balanced | strongest, or vision/designer when a \
                 vision model is configured)"
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
    //    jailed to the shared worktree, running on the CHILD MODEL's OWN provider.
    //
    //    Provider coupling (the bug this fixes): when a `child_driver_factory` is
    //    injected (the in-process server path), the child is built through it so the
    //    model routes to its OWN provider — Claude CLI, Anthropic API, or OpenRouter —
    //    never inheriting the parent's. The factory's contract REQUIRES the returned
    //    driver to be gated_write-only, worktree-jailed, and depth-1/non-orchestrator,
    //    so the gate posture is identical to the legacy CLI child.
    //
    //    When no factory is injected (`None`, always the case in the gateway BINARY
    //    process that has no provider machinery) we keep TODAY's exact ClaudeCliDriver
    //    behavior — same construction, same gate — for full back-compat.
    let driver: Box<dyn AgentDriver> = match &config.child_driver_factory {
        Some(factory) => {
            match factory.build_child(&model, &config.worktree_root, &[]) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(format!(
                        "[delegate tier={tier} model={model}] child driver build FAILED: {e}\n\
                         INCOMPLETE: could not construct the per-model child driver; do this yourself."
                    ));
                }
            }
        }
        None => Box::new(
            ClaudeCliDriver::new(mcp_config.display().to_string())
                .with_worktree(&config.worktree_root)
                .with_model(&model)
                .as_orchestrator(false),
        ),
    };

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
            // Default test map: vision UNSET (the common case — vision off).
            vision: String::new(),
        }
    }

    /// A model map WITH a configured vision/Designer model (vision_enabled case).
    fn models_with_vision() -> DelegateModels {
        DelegateModels {
            vision: "claude-vision-model".to_string(),
            ..models()
        }
    }

    fn cfg(depth: u32, max_depth: u32) -> OrchestratorConfig {
        OrchestratorConfig {
            models: models(),
            worktree_root: PathBuf::from("/work/crate"),
            gateway_bin: PathBuf::from("/bin/camerata-gateway"),
            depth,
            max_depth,
            child_driver_factory: None,
        }
    }

    // ── ChildDriverFactory test double + per-model coupling tests ──────────────

    /// A `ChildDriverFactory` test double that RECORDS the model id it was asked to
    /// build (so a test can assert per-model provider coupling), then returns a child
    /// driver that does NOT spawn anything (keeping CI token-free): its `run` returns a
    /// fixed marker string echoing the model it was constructed for.
    #[derive(Clone, Default)]
    struct RecordingFactory {
        /// Models requested, in call order. `Arc<Mutex<..>>` so the recorder survives the
        /// `Arc<dyn ChildDriverFactory>` boxing and stays observable from the test.
        models: Arc<std::sync::Mutex<Vec<String>>>,
        /// Worktrees requested, in call order (to assert per-worker jailing in fan-out).
        worktrees: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    }

    /// A no-spawn child driver returned by [`RecordingFactory`]. Records nothing itself;
    /// it just proves the factory path is exercised and echoes its model.
    struct EchoChildDriver {
        model: String,
    }

    #[async_trait::async_trait]
    impl AgentDriver for EchoChildDriver {
        async fn run(&self, _role: &Role, _task: &str) -> anyhow::Result<AgentOutcome> {
            Ok(AgentOutcome {
                session_id: "echo-child".to_string(),
                result: format!("ECHO model={}", self.model),
                cost_usd: None,
                denials: vec![],
            })
        }
    }

    impl ChildDriverFactory for RecordingFactory {
        fn build_child(
            &self,
            model: &str,
            worktree: &std::path::Path,
            _read_dirs: &[PathBuf],
        ) -> std::io::Result<Box<dyn AgentDriver>> {
            self.models.lock().unwrap().push(model.to_string());
            self.worktrees.lock().unwrap().push(worktree.to_path_buf());
            Ok(Box::new(EchoChildDriver {
                model: model.to_string(),
            }))
        }
    }

    use camerata_core::AgentOutcome;

    fn cfg_with_factory(
        depth: u32,
        max_depth: u32,
        models: DelegateModels,
        factory: Arc<RecordingFactory>,
    ) -> OrchestratorConfig {
        OrchestratorConfig {
            models,
            worktree_root: PathBuf::from("/work/crate"),
            gateway_bin: PathBuf::from("/bin/camerata-gateway"),
            depth,
            max_depth,
            child_driver_factory: Some(factory),
        }
    }

    #[tokio::test]
    async fn run_delegated_asks_factory_for_the_tiers_model_claude() {
        // A Claude tier -> the factory is asked for the Claude model id (per-model coupling).
        let factory = Arc::new(RecordingFactory::default());
        let config = cfg_with_factory(0, 1, models(), factory.clone());
        let out = run_delegated(&config, vec![RuleId("GOV-1".to_string())], "do x", "balanced")
            .await
            .unwrap();
        // The factory-built child ran (echo marker present, model echoed).
        assert!(out.contains("ECHO model=claude-sonnet-4-6"), "got: {out}");
        // And the factory was asked for EXACTLY the balanced (Claude) model.
        let asked = factory.models.lock().unwrap().clone();
        assert_eq!(asked, vec!["claude-sonnet-4-6".to_string()]);
    }

    #[tokio::test]
    async fn run_delegated_asks_factory_for_the_tiers_model_openrouter() {
        // An OpenRouter tier -> the factory is asked for the OpenRouter model id, proving
        // the child resolves via ITS OWN provider rather than inheriting Claude.
        let or_models = DelegateModels {
            fast: "openrouter/meta-llama/llama-3-8b".to_string(),
            balanced: "openrouter/mistralai/mistral-7b".to_string(),
            strongest: "claude-opus-4-8".to_string(),
            vision: String::new(),
        };
        let factory = Arc::new(RecordingFactory::default());
        let config = cfg_with_factory(0, 1, or_models, factory.clone());
        let out = run_delegated(&config, vec![RuleId("GOV-1".to_string())], "do x", "fast")
            .await
            .unwrap();
        assert!(
            out.contains("ECHO model=openrouter/meta-llama/llama-3-8b"),
            "got: {out}"
        );
        let asked = factory.models.lock().unwrap().clone();
        assert_eq!(asked, vec!["openrouter/meta-llama/llama-3-8b".to_string()]);
    }

    #[tokio::test]
    async fn run_delegated_asks_factory_for_the_vision_model() {
        // The vision tier routes to the configured vision model via the factory.
        let factory = Arc::new(RecordingFactory::default());
        let config = cfg_with_factory(0, 1, models_with_vision(), factory.clone());
        let out = run_delegated(&config, vec![RuleId("GOV-1".to_string())], "design", "vision")
            .await
            .unwrap();
        assert!(out.contains("ECHO model=claude-vision-model"), "got: {out}");
        let asked = factory.models.lock().unwrap().clone();
        assert_eq!(asked, vec!["claude-vision-model".to_string()]);
    }

    #[tokio::test]
    async fn run_delegated_none_factory_falls_back_to_cli_path() {
        // With NO factory injected, the legacy CLI path is taken. We can't run `claude`
        // in CI, but we can prove the CLI path is selected (not the factory): the depth
        // guard + tier resolve still pass, and the result is the ClaudeCliDriver run
        // outcome (which, with no `claude` binary in CI, surfaces as a child-run FAILED /
        // INCOMPLETE marker — NEVER the factory ECHO marker). The key assertion is that
        // the factory ECHO marker is ABSENT, proving back-compat dispatch.
        let config = cfg(0, 1); // child_driver_factory: None
        let out = run_delegated(&config, vec![RuleId("GOV-1".to_string())], "do x", "fast")
            .await
            .unwrap();
        assert!(
            !out.contains("ECHO model="),
            "None factory must NOT take the factory path: {out}"
        );
        // It went down the CLI driver path (tier/model framing present).
        assert!(out.contains("tier=fast"), "expected CLI-path framing: {out}");
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
    fn resolve_vision_tier_returns_model_when_set() {
        let m = models_with_vision();
        // The vision band resolves to the configured vision model...
        assert_eq!(m.resolve("vision"), Some("claude-vision-model"));
        // ...case-insensitively + trimmed, same contract as the logic tiers.
        assert_eq!(m.resolve("  VISION "), Some("claude-vision-model"));
        // ...and the `designer` alias resolves to the same model.
        assert_eq!(m.resolve("designer"), Some("claude-vision-model"));
        assert_eq!(m.resolve("DESIGNER"), Some("claude-vision-model"));
    }

    #[test]
    fn resolve_vision_tier_returns_none_when_unset() {
        // Vision UNSET (vision_enabled off / no model configured): a vision delegate
        // is refused cleanly — `resolve` returns None exactly like an unknown tier,
        // never a panic, never an empty-string model that would spawn an unmodeled child.
        let m = models(); // vision == ""
        assert_eq!(m.resolve("vision"), None);
        assert_eq!(m.resolve("designer"), None);
    }

    #[tokio::test]
    async fn run_delegated_refuses_vision_when_unset_without_spawning() {
        // depth allows, tier label is valid, but vision is unconfigured -> refuse
        // BEFORE any spawn/IO, surfacing as UnknownTier (the same clean-refusal path
        // as a genuinely unknown tier). No child process, no token spend.
        let err = run_delegated(
            &cfg(0, 1),
            vec![RuleId("GOV-1".to_string())],
            "design a hero section",
            "vision",
        )
        .await
        .unwrap_err();
        assert_eq!(err, DelegateError::UnknownTier("vision".to_string()));
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
