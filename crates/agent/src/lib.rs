//! camerata-agent: the agent runtime. Drives `claude -p` as a subprocess and
//! parses its JSON result. This is the [`camerata_core::AgentDriver`] seam's
//! first implementation. The CLI is the agent; provider/tier/model agnosticism
//! lives behind the trait.
//!
//! The flags here are the ones the verification slice proved out: the agent is
//! locked to the gateway's MCP tools and stripped of every built-in writer, so
//! its ONLY way to act is through the governance gate.

use camerata_core::{AgentDriver, AgentOutcome, Role};

/// Drives the Claude Code CLI in headless mode against the Camerata gateway.
pub struct ClaudeCliDriver {
    /// Path to the MCP config that points the agent at the Rust gateway.
    pub mcp_config_path: String,
    /// Built-in tools removed so the agent cannot bypass the gate.
    pub disallowed_builtins: Vec<String>,
}

impl ClaudeCliDriver {
    pub fn new(mcp_config_path: impl Into<String>) -> Self {
        Self {
            mcp_config_path: mcp_config_path.into(),
            disallowed_builtins: ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl AgentDriver for ClaudeCliDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        // TODO(phase0): per-role allowedTools + path boundaries from `role`;
        // worktree cwd binding; stream-json for live status instead of buffered.
        let _ = role;

        let mut cmd = tokio::process::Command::new("claude");
        cmd.arg("-p")
            .arg(task)
            .arg("--strict-mcp-config")
            .arg("--mcp-config")
            .arg(&self.mcp_config_path)
            .arg("--disallowedTools")
            .arg(self.disallowed_builtins.join(" "))
            .arg("--dangerously-skip-permissions")
            .arg("--output-format")
            .arg("json");

        let out = cmd.output().await?;
        if !out.status.success() {
            anyhow::bail!(
                "claude -p exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
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
