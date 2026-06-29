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

use std::path::{Path, PathBuf};

use camerata_core::{AgentDriver, AgentOutcome, Role};
use thiserror::Error;

pub mod session;
pub use session::{
    prepare_session, render_mcp_config, render_rules_file, SessionError, SessionSpawn,
    MCP_SERVER_KEY, RULES_FILE_ENV, WORKTREE_ROOT_ENV,
};

pub mod generic;
pub use generic::GenericCliDriver;

pub mod liveness;
pub use liveness::{spawn_mtime_probe, newest_mtime, HeartbeatFn, MTIME_PROBE_INTERVAL};

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

    #[error("agent subprocess stalled: no output for {idle_secs}s (last line: {last_line:?})")]
    Stalled { idle_secs: u64, last_line: Option<String> },
}

// ─── heartbeat + timeout constants ───────────────────────────────────────────

/// Inactivity window: if the subprocess emits no stdout line for this duration, it is
/// considered stalled and killed. Overridable via `CAMERATA_AGENT_INACTIVITY_SECS`.
pub const DEFAULT_AGENT_INACTIVITY_SECS: u64 = 120;

/// Hard-ceiling: absolute maximum wall-clock time a subprocess may run. A backstop
/// against runaway processes that keep trickling output. Overridable via
/// `CAMERATA_AGENT_TOTAL_TIMEOUT_SECS`.
pub const DEFAULT_AGENT_TOTAL_TIMEOUT_SECS: u64 = 3600; // 1 hour

/// Read the inactivity window from the environment, falling back to the default.
pub fn agent_inactivity_window() -> std::time::Duration {
    let secs = std::env::var("CAMERATA_AGENT_INACTIVITY_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_AGENT_INACTIVITY_SECS);
    std::time::Duration::from_secs(secs)
}

/// Read the total hard-ceiling from the environment, falling back to the default.
pub fn agent_total_timeout() -> std::time::Duration {
    let secs = std::env::var("CAMERATA_AGENT_TOTAL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_AGENT_TOTAL_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

// HeartbeatFn is re-exported from `camerata-liveness` via the `liveness` module above.
// It is available as `camerata_agent::HeartbeatFn` for backwards-compatibility.

// ─── streaming subprocess helper ─────────────────────────────────────────────

/// Drive a subprocess to completion, reading stdout line-by-line with an inactivity
/// timeout on each line. Fires `on_activity` on every line received (heartbeat).
///
/// Returns the full accumulated stdout on success, or:
/// - `AgentError::NonZeroExit` when the process exits non-zero.
/// - `AgentError::Stalled` when no line arrives within `inactivity_window`.
/// - Propagates `AgentError::Spawn` if the process can't be started.
///
/// The total hard-ceiling (`total_timeout`) is enforced via `tokio::time::timeout`
/// wrapping the whole streaming loop, so a runaway process that keeps trickling
/// output is still killed eventually.
pub(crate) async fn stream_subprocess(
    mut cmd: tokio::process::Command,
    on_activity: Option<HeartbeatFn>,
    inactivity_window: std::time::Duration,
    total_timeout: std::time::Duration,
) -> Result<(String, std::process::ExitStatus), AgentError> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    cmd.kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(AgentError::Spawn)?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr_handle = child.stderr.take().expect("stderr is piped");

    let mut lines = BufReader::new(stdout).lines();
    let mut accumulated = String::new();
    let mut last_line: Option<String> = None;
    let inactivity_secs = inactivity_window.as_secs();

    let stream_result = tokio::time::timeout(total_timeout, async {
        loop {
            match tokio::time::timeout(inactivity_window, lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    // A line arrived: fire the heartbeat and accumulate.
                    if let Some(cb) = &on_activity {
                        cb();
                    }
                    last_line = Some(line.clone());
                    accumulated.push_str(&line);
                    accumulated.push('\n');
                }
                Ok(Ok(None)) => {
                    // EOF: the process closed its stdout.
                    break;
                }
                Ok(Err(e)) => {
                    // I/O error reading the pipe.
                    let _ = child.kill().await;
                    return Err(AgentError::Spawn(e));
                }
                Err(_) => {
                    // Inactivity timeout: no line within the window — stalled.
                    let _ = child.kill().await;
                    return Err(AgentError::Stalled {
                        idle_secs: inactivity_secs,
                        last_line: last_line.clone(),
                    });
                }
            }
        }
        Ok(())
    })
    .await;

    match stream_result {
        Err(_) => {
            // Total hard-ceiling hit: kill and return Stalled with the total secs.
            let _ = child.kill().await;
            return Err(AgentError::Stalled {
                idle_secs: total_timeout.as_secs(),
                last_line: last_line.clone(),
            });
        }
        Ok(Err(e)) => return Err(e),
        Ok(Ok(())) => {}
    }

    // Collect stderr for error reporting.
    let mut stderr_buf = Vec::new();
    {
        use tokio::io::AsyncReadExt;
        let mut stderr_reader = BufReader::new(stderr_handle);
        let _ = stderr_reader.read_to_end(&mut stderr_buf).await;
    }

    let status = child.wait().await.map_err(AgentError::Spawn)?;

    if !status.success() {
        return Err(AgentError::NonZeroExit {
            status: status.to_string(),
            stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
        });
    }

    Ok((accumulated, status))
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

/// The fully-qualified MCP tool name for fan-out (concurrent multi-repo dispatch).
/// Same `camerata` server key, the `fan_out` tool. It is registered on the gateway
/// ONLY when the gateway boots in orchestrator mode; it is added to `--allowedTools`
/// ONLY for the orchestrator role. Fan-out workers are depth-1 children with
/// `gated_write` ONLY — they NEVER get `fan_out` or `delegate`.
pub const FAN_OUT_TOOL: &str = "mcp__camerata__fan_out";

/// The fully-qualified MCP tool name for raising a structured clarifying question
/// (Phase 3b). Same `camerata` server key, the `ask_clarification` tool. It is a
/// READ-CLASS tool: it records a question to the per-session clarify-request sink and
/// does NOT write to the repo, spawn, or escalate — so granting it creates NO new write
/// path and leaves the deny-before-write gate fully intact. It is added to
/// `--allowedTools` ONLY for drivers that opt in (e.g. the investigation agent) via
/// [`ClaudeCliDriver::with_clarification`]; the disallowed-builtins denylist is unchanged.
pub const ASK_CLARIFICATION_TOOL: &str = "mcp__camerata__ask_clarification";

/// The READ-CLASS escalation tool: the agent raises an escalation when its work meets the
/// escalation CONDITION of a selected rule (the rule-agnostic, agent-driven escalation gate). Like
/// [`ASK_CLARIFICATION_TOOL`] it records to a per-session sink and creates NO new write path, so the
/// deny-before-write gate is intact. Added to `--allowedTools` only for drivers that opt in via
/// [`ClaudeCliDriver::with_escalation`].
pub const RAISE_ESCALATION_TOOL: &str = "mcp__camerata__raise_escalation";

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
/// ([`DELEGATE_TOOL`]) and the `fan_out` tool ([`FAN_OUT_TOOL`]) are added on
/// top of the read-only built-ins and the governed write tool. This is the ONLY
/// place `delegate` and `fan_out` are granted, and they are granted ONLY to the
/// lead/orchestrator agent. Combined with the gateway only *registering* those
/// tools in orchestrator mode, this gives the depth-1 guarantee: a spawned
/// worker child uses `orchestrator = false`, so it can never re-delegate or
/// fan-out further.
pub fn allowed_tools_for_role_with_mode(role: &Role, orchestrator: bool) -> Vec<String> {
    // The role's identity is load-bearing for provenance even though the tool
    // surface is currently uniform; reference it so the mapping is obviously
    // role-derived and a future per-role narrowing has an obvious seam.
    let _ = &role.name;
    let mut tools: Vec<String> = READONLY_BUILTINS.iter().map(|s| s.to_string()).collect();
    tools.push(GATED_WRITE_TOOL.to_string());
    if orchestrator {
        tools.push(DELEGATE_TOOL.to_string());
        tools.push(FAN_OUT_TOOL.to_string());
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
    /// Additional directories the agent may READ (each emitted as its own `--add-dir`).
    /// A project contains MULTIPLE repos; these are the OTHER project repo clones so a
    /// worktree-bound (write-class) agent can read across all of them while still only
    /// writing to its single worktree. For project-level agents this carries the union of
    /// all the project's repo clones. READ-ONLY: `--add-dir` widens read scope only; the
    /// write gate (`gated_write` jailed to `CAMERATA_WORKTREE_ROOT`) is untouched, so these
    /// dirs are NOT writable. Deduped against `worktree` when args are built.
    pub extra_read_dirs: Vec<PathBuf>,
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
    /// Whether this agent may raise rule-driven escalations: when `true`, the READ-CLASS
    /// [`RAISE_ESCALATION_TOOL`] is added to `--allowedTools`. Default `false`. Adds NO write path
    /// (the tool records an escalation, it does not write), so the gate posture is unchanged.
    pub escalation: bool,
    /// Optional heartbeat callback fired once per stdout line received from the subprocess.
    /// Callers that track run activity (e.g. the server's RunStore) wire this to update
    /// `last_activity_ms`. `None` = no callback (all existing callers that don't set it).
    pub on_activity: Option<HeartbeatFn>,
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
            extra_read_dirs: Vec::new(),
            model: None,
            resume_session_id: None,
            orchestrator: false,
            clarification: false,
            escalation: false,
            on_activity: None,
        }
    }

    /// Set the heartbeat callback fired once per stdout line from the subprocess.
    /// Callers that track run activity wire this to `RunStore::touch_activity`. Builder.
    pub fn with_on_activity(mut self, cb: HeartbeatFn) -> Self {
        self.on_activity = Some(cb);
        self
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

    /// Allow this agent to raise rule-driven escalations: adds the READ-CLASS
    /// [`RAISE_ESCALATION_TOOL`] to `--allowedTools`. Builder form. Used by the brownfield
    /// implementer. Does NOT loosen the gate: `raise_escalation` records to a per-session sink (no
    /// repo write, no spawn), and every write/exec/spawn built-in stays on the denylist.
    pub fn with_escalation(mut self, escalation: bool) -> Self {
        self.escalation = escalation;
        self
    }

    /// Bind this driver to `worktree`: the agent runs with that directory as
    /// its cwd and `--add-dir` scope. Builder form.
    pub fn with_worktree(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.worktree = Some(worktree.into());
        self
    }

    /// Add additional READ-ONLY directories (the OTHER repos in the active project) to the
    /// agent's scope: each is emitted as its own `--add-dir`. A project has MULTIPLE repos;
    /// this is what lets, e.g., a frontend UoW read the backend repo's API surface. Builder
    /// form. READ-ONLY by construction: `--add-dir` widens reads, never writes — the write
    /// gate (`gated_write` jailed to `CAMERATA_WORKTREE_ROOT`) is unaffected, so these dirs
    /// cannot be written. Safe to pass dirs that overlap `worktree`; they're deduped.
    pub fn with_read_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.extra_read_dirs = dirs.into_iter().collect();
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
        if self.escalation {
            allowed.push(RAISE_ESCALATION_TOOL.to_string());
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

        // Multi-repo READ scope: a project has MULTIPLE repos, so each OTHER project-repo
        // clone gets its own `--add-dir` here, letting this agent READ across all of them
        // (e.g. a frontend UoW reading the backend's API) on top of its worktree. This is
        // READ-only — `--add-dir` widens reads, never writes; `gated_write` stays jailed to
        // the single worktree (CAMERATA_WORKTREE_ROOT), so the extra dirs are not writable.
        // Deduped against `worktree` so we never emit a duplicate `--add-dir` for the cwd.
        for dir in &self.extra_read_dirs {
            if self.worktree.as_deref() == Some(dir.as_path()) {
                continue;
            }
            args.push("--add-dir".to_string());
            args.push(dir.display().to_string());
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

    /// Build the tokio command (program + args + cwd + CARGO_TARGET_DIR), ready to `.output()`.
    ///
    /// When `self.worktree` follows the canonical Camerata layout
    /// (`<clone>/.camerata-worktrees/<branch>`), `CARGO_TARGET_DIR` is injected so any cargo
    /// invocations Claude makes write into the shared artifact store. See the module-level
    /// note in `crates/agent/src/generic.rs` for the full disk-safety design.
    fn build_command(&self, role: &Role, task: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args(self.build_args(role, task));
        if let Some(wt) = &self.worktree {
            cmd.current_dir(wt);
            // Inject CARGO_TARGET_DIR for disk-safety (2026-06-22).
            if let Some(shared_target) = derive_shared_target_dir(wt) {
                cmd.env("CARGO_TARGET_DIR", &shared_target);
            }
        }
        cmd
    }
}

/// Derive the shared `CARGO_TARGET_DIR` path from a canonical UoW worktree.
///
/// Canonical layout: `<clone>/.camerata-worktrees/<branch>`.
/// Returns `Some(<clone>/.camerata-shared-target)`, or `None` for shallow/out-of-band paths.
fn derive_shared_target_dir(worktree: &Path) -> Option<PathBuf> {
    let clone = worktree.parent()?.parent()?;
    Some(clone.join(".camerata-shared-target"))
}

#[async_trait::async_trait]
impl AgentDriver for ClaudeCliDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        let cmd = self.build_command(role, task);
        let (stdout, _status) = stream_subprocess(
            cmd,
            self.on_activity.clone(),
            agent_inactivity_window(),
            agent_total_timeout(),
        )
        .await?;

        let v: serde_json::Value =
            serde_json::from_str(&stdout).map_err(AgentError::ParseOutput)?;
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
    fn fan_out_tool_not_in_non_orchestrator_tools() {
        // fan_out must NEVER appear in the non-orchestrator tool surface.
        // This is the depth-1 / no-recursive-fan-out guarantee.
        let tools = allowed_tools_for_role(&role());
        assert!(
            !tools.iter().any(|t| t == FAN_OUT_TOOL),
            "fan_out must never be in a non-orchestrator agent's allowlist"
        );
        let tools_false = allowed_tools_for_role_with_mode(&role(), false);
        assert!(!tools_false.iter().any(|t| t == FAN_OUT_TOOL));
    }

    #[test]
    fn fan_out_tool_in_orchestrator_tools() {
        // fan_out MUST be present in orchestrator mode alongside delegate.
        let tools = allowed_tools_for_role_with_mode(&role(), true);
        assert!(
            tools.iter().any(|t| t == FAN_OUT_TOOL),
            "orchestrator must get the fan_out tool"
        );
        // Orchestrator still gets delegate and gated_write too.
        assert!(tools.iter().any(|t| t == DELEGATE_TOOL));
        assert!(tools.iter().any(|t| t == GATED_WRITE_TOOL));
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

    // ── stall detection tests ─────────────────────────────────────────────────

    #[test]
    fn stalled_error_formats_correctly() {
        let e = AgentError::Stalled {
            idle_secs: 120,
            last_line: Some("partial output...".to_string()),
        };
        let s = e.to_string();
        assert!(s.contains("120"), "idle_secs in message");
        assert!(s.contains("partial output"), "last_line in message");
    }

    #[test]
    fn stalled_error_with_no_last_line() {
        let e = AgentError::Stalled {
            idle_secs: 60,
            last_line: None,
        };
        assert!(e.to_string().contains("60"));
    }

    #[tokio::test]
    async fn stream_subprocess_returns_full_output_for_normal_program() {
        // A program that immediately prints three lines and exits.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "echo line1; echo line2; echo line3"]);
        let (out, _) = stream_subprocess(
            cmd,
            None,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("should succeed");
        assert!(out.contains("line1"));
        assert!(out.contains("line2"));
        assert!(out.contains("line3"));
    }

    #[tokio::test]
    async fn stream_subprocess_fires_heartbeat_per_line() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = counter.clone();
        let cb: HeartbeatFn = Arc::new(move || {
            counter2.fetch_add(1, Ordering::SeqCst);
        });

        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "echo a; echo b; echo c"]);
        let (out, _) = stream_subprocess(
            cmd,
            Some(cb),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("should succeed");
        // 3 lines → 3 heartbeats.
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert!(out.contains("a"));
    }

    #[tokio::test]
    async fn stream_subprocess_stalls_when_program_goes_silent() {
        // A program that sleeps longer than the inactivity window without producing output.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "echo started; sleep 10"]);
        let result = stream_subprocess(
            cmd,
            None,
            std::time::Duration::from_millis(200), // very short for test
            std::time::Duration::from_secs(30),
        )
        .await;
        match result {
            Err(AgentError::Stalled { idle_secs: _, last_line }) => {
                // last_line is Some("started") because we got that before the sleep.
                assert_eq!(last_line.as_deref(), Some("started"));
            }
            other => panic!("expected Stalled, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn stream_subprocess_total_timeout_kills_trickler() {
        // A program that prints one line per second indefinitely.
        // Total timeout is shorter than how long it would run.
        let mut cmd = tokio::process::Command::new("sh");
        // Each line resets the inactivity window, but the total ceiling fires.
        cmd.args(["-c", "while true; do echo tick; sleep 0.1; done"]);
        let result = stream_subprocess(
            cmd,
            None,
            std::time::Duration::from_secs(10),   // inactivity: long
            std::time::Duration::from_millis(300), // total: short for test
        )
        .await;
        assert!(
            matches!(result, Err(AgentError::Stalled { .. })),
            "total timeout must produce Stalled"
        );
    }
}
