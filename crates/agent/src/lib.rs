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
    MCP_SERVER_KEY, RULES_FILE_ENV,
};

pub mod generic;
pub use generic::GenericCliDriver;

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

/// Read-only built-ins an agent always needs (they cannot mutate the worktree,
/// so they are safe to allow alongside the governed write path).
pub const READONLY_BUILTINS: &[&str] = &["Read", "Glob", "Grep", "LS"];

/// Derive the `--allowedTools` list for a role.
///
/// Every role gets the read-only built-ins plus the governed MCP write tool —
/// the agent's ONLY mutation path. This is pure so it is unit-testable without
/// spawning a process. Future roles can narrow/extend this from
/// `role.rule_subset`; today the role name is recorded for tracing and all
/// roles share the same tool surface (writes are gated, not tool-gated).
pub fn allowed_tools_for_role(role: &Role) -> Vec<String> {
    // The role's identity is load-bearing for provenance even though the tool
    // surface is currently uniform; reference it so the mapping is obviously
    // role-derived and a future per-role narrowing has an obvious seam.
    let _ = &role.name;
    let mut tools: Vec<String> = READONLY_BUILTINS.iter().map(|s| s.to_string()).collect();
    tools.push(GATED_WRITE_TOOL.to_string());
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
        }
    }

    /// Bind this driver to `worktree`: the agent runs with that directory as
    /// its cwd and `--add-dir` scope. Builder form.
    pub fn with_worktree(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.worktree = Some(worktree.into());
        self
    }

    /// Build the argv for `claude -p` for the given role + task. Pure and
    /// testable — does not spawn anything. The returned vec is everything after
    /// the `claude` program name.
    pub fn build_args(&self, role: &Role, task: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-p".to_string(),
            task.to_string(),
            "--strict-mcp-config".to_string(),
            "--mcp-config".to_string(),
            self.mcp_config_path.clone(),
            // Per-role allowedTools: the governed write tool + read-only builtins.
            "--allowedTools".to_string(),
            allowed_tools_for_role(role).join(" "),
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
