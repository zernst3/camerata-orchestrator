//! camerata-agent: the agent runtime. Drives `claude -p` as a subprocess and
//! parses its JSON result. This is the [`camerata_core::AgentDriver`] seam's
//! first implementation. The CLI is the agent; provider/tier/model agnosticism
//! lives behind the trait.
//!
//! The flags here are the ones the verification slice proved out: the agent is
//! locked to the gateway's MCP tools and stripped of every built-in writer, so
//! its ONLY way to act is through the governance gate.
//!
//! Phase-0 additions wired in this pass:
//! - **worktree cwd binding** — an optional [`ClaudeCliDriver::worktree`] sets
//!   the child process cwd and passes `--add-dir` so the agent is scoped to its
//!   own worktree (path isolation, the layer-0 boundary).
//! - **per-role allowedTools** — [`allowed_tools_for_role`] derives the
//!   `--allowedTools` list from a [`camerata_core::Role`]: the governed MCP
//!   write tool plus read-only built-ins. The role's path scope is enforced by
//!   the gateway + `--add-dir`; the tool list enforces *which* tools at all.

use std::path::PathBuf;

use camerata_core::{AgentDriver, AgentOutcome, Role};
use thiserror::Error;

pub mod session;
pub use session::{
    prepare_session, render_mcp_config, render_rules_file, SessionError, SessionSpawn,
    MCP_SERVER_KEY, RULES_FILE_ENV, WORKTREE_ROOT_ENV,
};

pub mod generic;
pub use generic::GenericCliDriver;

pub mod post_story_hook;
pub use post_story_hook::{DocConvention, PostStoryHook, StoryCompletion, StoryDocEmitter};

// ─── crate-local error type (RUST-DOMAIN-4 / RUST-DOMAIN-6) ───────────────────

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("failed to spawn `claude`: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("`claude -p` exited with status {status}: {stderr}")]
    NonZeroExit { status: String, stderr: String },

    #[error("could not parse `claude -p` JSON output: {0}")]
    ParseOutput(#[source] serde_json::Error),
}

// ─── the MCP-exposed governed write tool name ────────────────────────────────

/// The fully-qualified MCP tool name the gateway exposes. Claude Code namespaces
/// MCP tools as `mcp__<server>__<tool>`; our gateway server registers as
/// `camerata` with the single `gated_write` tool.
pub const GATED_WRITE_TOOL: &str = "mcp__camerata__gated_write";

/// The fully-qualified MCP tool name for governed delegation. Same `camerata`
/// server key, the `delegate` tool. It is registered on the gateway ONLY when the
/// gateway boots in orchestrator mode; it is added to `--allowedTools` ONLY for
/// the orchestrator role. Delegate children NEVER get it (depth-1 guarantee).
pub const DELEGATE_TOOL: &str = "mcp__camerata__delegate";

/// The fully-qualified MCP tool name for raising a structured clarifying question
/// (Phase 3b). Same `camerata` server key, the `ask_clarification` tool. It is a
/// READ-CLASS tool: it records a question to the per-session clarify-request sink and
/// does NOT write to the repo, spawn, or escalate — so granting it creates NO new write
/// path and leaves the deny-before-write gate fully intact. It is added to
/// `--allowedTools` ONLY for drivers that opt in (e.g. the investigation agent) via
/// [`ClaudeCliDriver::with_clarification`]; the disallowed-builtins denylist is unchanged.
pub const ASK_CLARIFICATION_TOOL: &str = "mcp__camerata__ask_clarification";

/// Read-only built-ins an agent always needs (they cannot mutate the worktree,
/// so they are safe to allow alongside the governed write path).
pub const READONLY_BUILTINS: &[&str] = &["Read", "Glob", "Grep", "LS"];

/// Derive the `--allowedTools` list for a role (NON-orchestrator agents).
///
/// Every role gets the read-only built-ins plus the governed MCP write tool —
/// the agent's ONLY mutation path. The `delegate` tool is NEVER included here;
/// this is the function every non-orchestrator agent (and every delegate child)
/// uses, so those agents structurally cannot delegate. This is pure so it is
/// unit-testable without spawning a process.
pub fn allowed_tools_for_role(role: &Role) -> Vec<String> {
    allowed_tools_for_role_with_mode(role, false)
}

/// Derive the `--allowedTools` list for a role, optionally in **orchestrator
/// mode**.
///
/// When `orchestrator` is `true`, the governed `delegate` tool
/// ([`DELEGATE_TOOL`]) is added on top of the read-only built-ins and the
/// governed write tool. This is the ONLY place `delegate` is granted, and it is
/// granted ONLY to the lead/orchestrator agent. Combined with the gateway only
/// *registering* the tool in orchestrator mode, this gives the depth-1 guarantee:
/// a spawned delegate child uses `orchestrator = false`, so it can never
/// re-delegate.
pub fn allowed_tools_for_role_with_mode(role: &Role, orchestrator: bool) -> Vec<String> {
    // The role's identity is load-bearing for provenance even though the tool
    // surface is currently uniform; reference it so the mapping is obviously
    // role-derived and a future per-role narrowing has an obvious seam.
    let _ = &role.name;
    let mut tools: Vec<String> = READONLY_BUILTINS.iter().map(|s| s.to_string()).collect();
    tools.push(GATED_WRITE_TOOL.to_string());
    if orchestrator {
        tools.push(DELEGATE_TOOL.to_string());
    }
    tools
}

/// Drives the Claude Code CLI in headless mode against the Camerata gateway.
///
/// `Clone` so a caller that prepares N per-session drivers in a loop (e.g. the
/// PO-mode fleet, one session per plan task) can own each driver independently of
/// the [`SessionSpawn`] it came from. Cloning copies only the config paths +
/// flags; it spawns nothing.
#[derive(Clone)]
pub struct ClaudeCliDriver {
    /// Path to the MCP config that points the agent at the Rust gateway.
    pub mcp_config_path: String,
    /// Built-in tools EXPLICITLY denied so the agent cannot bypass the gate, even if
    /// `--allowedTools` is not strictly exclusive. Covers the direct write/exec tools
    /// AND `Task` (subagent spawning), which could otherwise launch a child agent that
    /// regains Write/Bash. This denylist must be re-audited on every Claude Code CLI
    /// upgrade: a new write/exec/spawn tool added by the CLI must be added here.
    pub disallowed_builtins: Vec<String>,
    /// Optional worktree the agent is bound to. When set it becomes the child
    /// process cwd AND is passed via `--add-dir`, scoping the agent to its
    /// worktree. When `None` the agent inherits the orchestrator's cwd.
    pub worktree: Option<PathBuf>,
    /// Optional model id (e.g. `claude-sonnet-4-6`) passed via `--model`. When `None`
    /// the CLI uses its configured default. Lets a caller (a routine, the fleet) run a
    /// run on a chosen model.
    pub model: Option<String>,
    /// Optional prior session id to RESUME via `--resume`. Used to continue a run that
    /// stopped at a governance escalation, once a human has authorized a directive — the
    /// directive is passed as the task and the agent picks up its prior context.
    pub resume_session_id: Option<String>,
    /// Whether this agent runs in ORCHESTRATOR mode: it additionally gets the
    /// governed [`DELEGATE_TOOL`] in `--allowedTools`. Only the lead/strongest
    /// stage sets this; every other agent (and every delegate child) leaves it
    /// `false`, so they cannot delegate. The default is `false`.
    pub orchestrator: bool,
    /// Whether this agent may raise structured clarifying questions (Phase 3b): when
    /// `true`, the READ-CLASS [`ASK_CLARIFICATION_TOOL`] is added to `--allowedTools`.
    /// Default `false`. Granting it adds NO write path (the tool records a question, it
    /// does not write to the repo), so the deny-before-write gate is unchanged; the
    /// disallowed-builtins denylist (`Task`/`Write`/`Bash`/…) is unchanged either way.
    pub clarification: bool,
}

impl ClaudeCliDriver {
    pub fn new(mcp_config_path: impl Into<String>) -> Self {
        Self {
            mcp_config_path: mcp_config_path.into(),
            disallowed_builtins: ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit", "Task"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            worktree: None,
            model: None,
            resume_session_id: None,
            orchestrator: false,
            clarification: false,
        }
    }

    /// Mark this driver as the orchestrator (the lead). Its `--allowedTools` will
    /// include the governed [`DELEGATE_TOOL`]. Builder form. Use ONLY for the
    /// lead/strongest stage; delegate children must never set this.
    pub fn as_orchestrator(mut self, orchestrator: bool) -> Self {
        self.orchestrator = orchestrator;
        self
    }

    /// Allow this agent to raise structured clarifying questions (Phase 3b): adds the
    /// READ-CLASS [`ASK_CLARIFICATION_TOOL`] to `--allowedTools`. Builder form. Used by
    /// the investigation runner. This does NOT loosen the gate: `ask_clarification`
    /// records a question (no repo write, no spawn), and every write/exec/spawn built-in
    /// stays on the disallowed denylist.
    pub fn with_clarification(mut self, clarification: bool) -> Self {
        self.clarification = clarification;
        self
    }

    /// Bind this driver to `worktree`: the agent runs with that directory as
    /// its cwd and `--add-dir` scope. Builder form.
    pub fn with_worktree(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.worktree = Some(worktree.into());
        self
    }

    /// Run on a specific model (`--model`). A blank id is ignored (CLI default). Builder.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let m = model.into();
        self.model = if m.trim().is_empty() { None } else { Some(m) };
        self
    }

    /// Resume a prior session (`--resume <session_id>`) — continue a run that stopped at
    /// an escalation, with the authorized directive passed as the task. Builder.
    pub fn resuming(mut self, session_id: impl Into<String>) -> Self {
        let s = session_id.into();
        self.resume_session_id = if s.trim().is_empty() { None } else { Some(s) };
        self
    }

    /// Build the argv for `claude -p` for the given role + task. Pure and
    /// testable — does not spawn anything. The returned vec is everything after
    /// the `claude` program name.
    pub fn build_args(&self, role: &Role, task: &str) -> Vec<String> {
        // Per-role allowedTools: the governed write tool + read-only builtins, PLUS the
        // governed `delegate` tool when (and only when) this driver is the orchestrator.
        // The READ-CLASS `ask_clarification` tool is appended when this driver opts in
        // (Phase 3b); it adds no write path, so the gate posture is unchanged.
        let mut allowed = allowed_tools_for_role_with_mode(role, self.orchestrator);
        if self.clarification {
            allowed.push(ASK_CLARIFICATION_TOOL.to_string());
        }
        let mut args: Vec<String> = vec![
            "-p".to_string(),
            task.to_string(),
            "--strict-mcp-config".to_string(),
            "--mcp-config".to_string(),
            self.mcp_config_path.clone(),
            "--allowedTools".to_string(),
            allowed.join(" "),
            "--disallowedTools".to_string(),
            self.disallowed_builtins.join(" "),
            "--dangerously-skip-permissions".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];

        // Worktree cwd binding: scope the agent to its worktree directory.
        if let Some(wt) = &self.worktree {
            args.push("--add-dir".to_string());
            args.push(wt.display().to_string());
        }

        // Run on a chosen model when set (else the CLI default).
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        // Resume a prior session when set (the task carries the authorized directive).
        if let Some(session) = &self.resume_session_id {
            args.push("--resume".to_string());
            args.push(session.clone());
        }

        args
    }

    /// Build the tokio command (program + args + cwd), ready to `.output()`.
    fn build_command(&self, role: &Role, task: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args(self.build_args(role, task));
        if let Some(wt) = &self.worktree {
            cmd.current_dir(wt);
        }
        cmd
    }
}

#[async_trait::async_trait]
impl AgentDriver for ClaudeCliDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        let mut cmd = self.build_command(role, task);

        let out = cmd.output().await.map_err(AgentError::Spawn)?;
        if !out.status.success() {
            return Err(AgentError::NonZeroExit {
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }
            .into());
        }

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(AgentError::ParseOutput)?;
        Ok(AgentOutcome {
            session_id: v["session_id"].as_str().unwrap_or_default().to_string(),
            result: v["result"].as_str().unwrap_or_default().to_string(),
            cost_usd: v["total_cost_usd"].as_f64(),
            denials: v["permission_denials"]
                .as_array()
                .map(|a| a.iter().map(|x| x.to_string()).collect())
                .unwrap_or_default(),
        })
    }
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::RuleId;

    fn role() -> Role {
        Role {
            name: "Backend".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/core".to_string()],
        }
    }

    #[test]
    fn allowed_tools_includes_governed_write_and_readonly() {
        let tools = allowed_tools_for_role(&role());
        assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
        assert!(tools.iter().any(|t| t == "Read"));
        // The destructive built-ins must NOT appear in the allow list.
        assert!(!tools.iter().any(|t| t == "Write" || t == "Bash"));
    }

    #[test]
    fn delegate_tool_is_absent_for_non_orchestrator_agents() {
        // The default (non-orchestrator) tool surface must NOT include delegate:
        // this is the depth-1 guarantee at the allowlist level.
        let tools = allowed_tools_for_role(&role());
        assert!(
            !tools.iter().any(|t| t == DELEGATE_TOOL),
            "delegate must never be in a non-orchestrator agent's allowlist"
        );
        // Belt: the explicit-mode false path agrees.
        let tools_false = allowed_tools_for_role_with_mode(&role(), false);
        assert!(!tools_false.iter().any(|t| t == DELEGATE_TOOL));
    }

    #[test]
    fn delegate_tool_present_only_in_orchestrator_mode() {
        let tools = allowed_tools_for_role_with_mode(&role(), true);
        assert!(
            tools.iter().any(|t| t == DELEGATE_TOOL),
            "orchestrator must get the delegate tool"
        );
        // It still has the governed write tool and read-only built-ins.
        assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
        assert!(tools.iter().any(|t| t == "Read"));
    }

    #[test]
    fn build_args_includes_delegate_only_for_orchestrator_driver() {
        let normal = ClaudeCliDriver::new("/tmp/mcp.json");
        let normal_args = normal.build_args(&role(), "task");
        let allowed = {
            let i = normal_args
                .iter()
                .position(|a| a == "--allowedTools")
                .unwrap();
            normal_args[i + 1].clone()
        };
        assert!(
            !allowed.split(' ').any(|t| t == DELEGATE_TOOL),
            "non-orchestrator driver must not offer delegate"
        );

        let lead = ClaudeCliDriver::new("/tmp/mcp.json").as_orchestrator(true);
        let lead_args = lead.build_args(&role(), "task");
        let lead_allowed = {
            let i = lead_args.iter().position(|a| a == "--allowedTools").unwrap();
            lead_args[i + 1].clone()
        };
        assert!(
            lead_allowed.split(' ').any(|t| t == DELEGATE_TOOL),
            "orchestrator driver must offer delegate"
        );
        // Task stays disallowed for the orchestrator too — delegate is NOT Task.
        let dis = {
            let i = lead_args
                .iter()
                .position(|a| a == "--disallowedTools")
                .unwrap();
            lead_args[i + 1].clone()
        };
        assert!(dis.split(' ').any(|t| t == "Task"));
    }

    #[test]
    fn build_args_wires_per_role_allowed_tools() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json");
        let args = driver.build_args(&role(), "do a thing");
        let allowed_idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let allowed_val = &args[allowed_idx + 1];
        assert!(allowed_val.contains(GATED_WRITE_TOOL));
        assert!(allowed_val.contains("Read"));
    }

    #[test]
    fn escape_tools_are_explicitly_denied_and_never_allowed() {
        // The cage's integrity must not rest only on --allowedTools being exclusive:
        // every write/exec/spawn tool is on the explicit denylist, AND absent from the
        // allowlist. `Task` matters most (a subagent could otherwise regain Write/Bash).
        let driver = ClaudeCliDriver::new("/tmp/mcp.json");
        let args = driver.build_args(&role(), "task");
        let disallowed = {
            let i = args.iter().position(|a| a == "--disallowedTools").unwrap();
            args[i + 1].clone()
        };
        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        for tool in ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit", "Task"] {
            assert!(
                disallowed.split(' ').any(|t| t == tool),
                "{tool} must be on the explicit denylist"
            );
            assert!(
                !allowed.split(' ').any(|t| t == tool),
                "{tool} must never be on the allowlist"
            );
        }
    }

    #[test]
    fn ask_clarification_absent_by_default_present_only_when_opted_in() {
        // Default driver: the clarification tool is NOT offered.
        let normal = ClaudeCliDriver::new("/tmp/mcp.json");
        let normal_args = normal.build_args(&role(), "task");
        let normal_allowed = {
            let i = normal_args
                .iter()
                .position(|a| a == "--allowedTools")
                .unwrap();
            normal_args[i + 1].clone()
        };
        assert!(
            !normal_allowed.split(' ').any(|t| t == ASK_CLARIFICATION_TOOL),
            "ask_clarification must be absent unless opted in"
        );

        // Opted-in driver (the investigation agent): the tool is offered.
        let clarifier = ClaudeCliDriver::new("/tmp/mcp.json").with_clarification(true);
        let args = clarifier.build_args(&role(), "task");
        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        assert!(
            allowed.split(' ').any(|t| t == ASK_CLARIFICATION_TOOL),
            "clarification driver must offer ask_clarification"
        );
    }

    #[test]
    fn ask_clarification_does_not_weaken_the_gate() {
        // THE GATE NEVER WEAKENS: enabling ask_clarification must leave the write gate
        // intact — gated_write is still the only write tool, and every write/exec/spawn
        // built-in (esp. Task) stays on the disallowed denylist and off the allowlist.
        let clarifier = ClaudeCliDriver::new("/tmp/mcp.json").with_clarification(true);
        let args = clarifier.build_args(&role(), "task");
        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        let disallowed = {
            let i = args.iter().position(|a| a == "--disallowedTools").unwrap();
            args[i + 1].clone()
        };
        // The only write path is still the governed write tool.
        assert!(allowed.split(' ').any(|t| t == GATED_WRITE_TOOL));
        // Every escape tool stays denied and absent from the allowlist, unchanged.
        for tool in ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit", "Task"] {
            assert!(
                disallowed.split(' ').any(|t| t == tool),
                "{tool} must stay on the denylist even with clarification on"
            );
            assert!(
                !allowed.split(' ').any(|t| t == tool),
                "{tool} must never be on the allowlist even with clarification on"
            );
        }
        // ask_clarification is NOT a write tool: it is not gated_write, delegate, or any
        // built-in writer.
        assert_ne!(ASK_CLARIFICATION_TOOL, GATED_WRITE_TOOL);
    }

    #[test]
    fn build_args_without_worktree_has_no_add_dir() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json");
        let args = driver.build_args(&role(), "task");
        assert!(!args.iter().any(|a| a == "--add-dir"));
    }

    #[test]
    fn build_args_with_worktree_adds_dir_scope() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json").with_worktree("/tmp/wt/backend");
        let args = driver.build_args(&role(), "task");
        let idx = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("--add-dir present");
        assert_eq!(args[idx + 1], "/tmp/wt/backend");
    }

    #[test]
    fn build_args_without_model_or_resume_omits_those_flags() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json");
        let args = driver.build_args(&role(), "task");
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(!args.iter().any(|a| a == "--resume"));
    }

    #[test]
    fn build_args_with_model_passes_model_flag() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json").with_model("claude-sonnet-4-6");
        let args = driver.build_args(&role(), "task");
        let i = args
            .iter()
            .position(|a| a == "--model")
            .expect("--model present");
        assert_eq!(args[i + 1], "claude-sonnet-4-6");
        // A blank model id is ignored (CLI default).
        let blank = ClaudeCliDriver::new("/tmp/mcp.json").with_model("   ");
        assert!(!blank
            .build_args(&role(), "task")
            .iter()
            .any(|a| a == "--model"));
    }

    #[test]
    fn build_args_resuming_passes_resume_flag() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json").resuming("sess-abc123");
        let args = driver.build_args(&role(), "the authorized directive");
        let i = args
            .iter()
            .position(|a| a == "--resume")
            .expect("--resume present");
        assert_eq!(args[i + 1], "sess-abc123");
        // The directive still rides as the -p task.
        let p = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[p + 1], "the authorized directive");
    }

    #[test]
    fn build_args_preserves_strict_mcp_and_disallowed() {
        let driver = ClaudeCliDriver::new("/tmp/mcp.json");
        let args = driver.build_args(&role(), "task");
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(args.iter().any(|a| a == "--disallowedTools"));
        let dis_idx = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert!(args[dis_idx + 1].contains("Write"));
        assert!(args[dis_idx + 1].contains("Bash"));
    }
}
