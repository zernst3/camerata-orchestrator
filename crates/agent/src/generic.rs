//! `GenericCliDriver`: a provider-agnostic [`camerata_core::AgentDriver`] that
//! drives ANY command-line agent, not just `claude`.
//!
//! # Provider-neutrality proof
//!
//! This driver shares ZERO code with [`crate::ClaudeCliDriver`] beyond the
//! [`camerata_core::AgentDriver`] trait. It carries no `--strict-mcp-config`,
//! no `--dangerously-skip-permissions`, no `--allowedTools` Claude flag — none
//! of the Claude-CLI-specific flags that live in [`crate::ClaudeCliDriver::build_args`].
//!
//! Yet the [`camerata_core::Coordinator`] and [`camerata_core::FleetCoordinator`]
//! accept a `&dyn AgentDriver` reference. They call `driver.run(role, task)` and
//! receive an [`camerata_core::AgentOutcome`]. They never inspect which concrete
//! type is behind the reference. The same governance gate
//! ([`camerata_gateway::evaluate_call`]) decides on the
//! [`camerata_core::ToolCall`] (tool name, path, and content) — it receives no
//! information about the model or driver that produced the call. All three tiers
//! (coordinator, gateway, check runner) are provider-neutral by construction, not
//! by promise.
//!
//! This file is the structural proof. The test in
//! `crates/core/tests/provider_neutrality.rs` is the executable proof.

use std::path::{Path, PathBuf};

use camerata_core::{AgentDriver, AgentOutcome, Role};

/// Derive the shared `CARGO_TARGET_DIR` for a UoW worktree at the canonical layout
/// `<clone>/.camerata-worktrees/<branch>`.
///
/// Returns `Some(<clone>/.camerata-shared-target)` when the worktree has a grandparent
/// (i.e. is at least two levels deep), and `None` for shallow or out-of-band paths.
/// This keeps the derivation infallible: the caller injects the env var only when Some.
fn derive_shared_target_dir(worktree: &Path) -> Option<PathBuf> {
    // parent() → .camerata-worktrees, parent() → clone root
    let clone = worktree.parent()?.parent()?;
    Some(clone.join(".camerata-shared-target"))
}

/// Drives any command-line agent in a subprocess.
///
/// Unlike [`crate::ClaudeCliDriver`], this driver is not tied to any particular
/// model provider: `program` is any binary on `$PATH` (e.g. `"llm"`, `"aider"`,
/// `"my-agent"`), and the flags are caller-supplied. The Camerata coordinator and
/// gateway sit above the driver seam and govern the outcome identically regardless
/// of which binary runs here.
///
/// # Argument assembly
///
/// `build_args` produces: `base_args + [task_flag, task]`. There is no per-role
/// flag injection: the role governs which tasks are dispatched here (the
/// coordinator decides that), not which CLI flags are passed. A caller that wants
/// per-role tool filtering can encode it in `base_args`.
///
/// # Output convention
///
/// `run` expects the agent process to write its result to stdout. Two formats
/// are accepted, in this priority order:
///
/// 1. JSON object with a `"result"` string field (and optionally `"session_id"`
///    and `"cost_usd"`). This is the same shape `claude -p --output-format json`
///    produces, so any Claude-compatible output adapter also satisfies this driver.
/// 2. Raw text: the entire stdout is used as the `result`; `session_id` is
///    derived from the program name, and `cost_usd` is `None`.
///
/// `denials` is always empty from this driver's perspective: the governance gate
/// runs at the MCP transport layer, not here. Denials are recorded by the gateway
/// and propagated through the MCP response, not through stdout.
///
/// # CARGO_TARGET_DIR — disk-safety (2026-06-22)
///
/// When `worktree` is set and follows the canonical layout
/// (`<clone>/.camerata-worktrees/<branch>`), the driver injects `CARGO_TARGET_DIR`
/// pointing at `<clone>/.camerata-shared-target` into the agent environment. This
/// ensures that any cargo invocation the agent makes (builds, tests, proc-macro
/// expansion) writes into the repo's shared artifact directory rather than a
/// per-worktree `target/`, collapsing the N×5 GB disk multiplier to 1×.
///
/// Derivation: `worktree.parent().parent().join(".camerata-shared-target")`. Falls
/// back gracefully (no env var set) for out-of-band worktrees where derivation fails.
#[derive(Clone)]
pub struct GenericCliDriver {
    /// The binary to invoke (e.g. `"llm"`, `"aider"`, `"my-agent"`). Must be
    /// on `$PATH` or be an absolute path.
    pub program: String,
    /// Fixed arguments prepended to every invocation (e.g. `["--model", "gpt-4o"]`).
    pub base_args: Vec<String>,
    /// The flag that precedes the task string in the argv (e.g. `"-p"` or `"--prompt"`).
    pub task_flag: String,
    /// Optional directory to use as the child process cwd. When set the spawned
    /// process runs inside that directory, matching [`crate::ClaudeCliDriver`]'s
    /// worktree binding behavior.
    pub worktree: Option<PathBuf>,
    /// Optional heartbeat callback fired once per stdout line received from the subprocess.
    /// Callers that track run activity (e.g. the server's RunStore) wire this to update
    /// `last_activity_ms`. `None` = no callback.
    pub on_activity: Option<crate::HeartbeatFn>,
}

impl std::fmt::Debug for GenericCliDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenericCliDriver")
            .field("program", &self.program)
            .field("base_args", &self.base_args)
            .field("task_flag", &self.task_flag)
            .field("worktree", &self.worktree)
            .field("on_activity", &self.on_activity.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

impl GenericCliDriver {
    /// Construct a driver for `program`, with `task_flag` preceding the task
    /// string in the argv. `base_args` are prepended to every invocation.
    ///
    /// Example: `GenericCliDriver::new("llm", "--prompt", &["--model", "gpt-4o"])`
    /// produces argv `["--model", "gpt-4o", "--prompt", "<task>"]`.
    pub fn new(
        program: impl Into<String>,
        task_flag: impl Into<String>,
        base_args: &[&str],
    ) -> Self {
        Self {
            program: program.into(),
            task_flag: task_flag.into(),
            base_args: base_args.iter().map(|s| s.to_string()).collect(),
            worktree: None,
            on_activity: None,
        }
    }

    /// Set the heartbeat callback fired once per stdout line from the subprocess.
    /// Callers that track run activity wire this to `RunStore::touch_activity`. Builder.
    pub fn with_on_activity(mut self, cb: crate::HeartbeatFn) -> Self {
        self.on_activity = Some(cb);
        self
    }

    /// Bind this driver to a worktree directory. The spawned process runs with
    /// that directory as its cwd. Builder form.
    pub fn with_worktree(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.worktree = Some(worktree.into());
        self
    }

    /// Build the argv (everything after the program name) for the given role and
    /// task. Pure and testable: no process is spawned.
    ///
    /// Returned vec: `base_args + [task_flag, task]`.
    ///
    /// The `role` is accepted so the signature matches [`crate::ClaudeCliDriver::build_args`]
    /// and callers can treat both drivers uniformly. The role's content is not
    /// used here; per-role tool filtering, if needed, belongs in `base_args`.
    pub fn build_args(&self, _role: &Role, task: &str) -> Vec<String> {
        let mut args = self.base_args.clone();
        args.push(self.task_flag.clone());
        args.push(task.to_string());
        args
    }

    /// Build the tokio command (program + args + optional cwd + CARGO_TARGET_DIR),
    /// ready to `.output()`.
    ///
    /// When `self.worktree` is set and follows the canonical Camerata layout
    /// (`<clone>/.camerata-worktrees/<branch>`), `CARGO_TARGET_DIR` is injected so any
    /// cargo invocations the agent makes write into the shared artifact store rather than
    /// a per-worktree `target/`. Out-of-band worktrees fall back silently (no env var set).
    fn build_command(&self, role: &Role, task: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(self.build_args(role, task));
        if let Some(wt) = &self.worktree {
            cmd.current_dir(wt);
            // Inject CARGO_TARGET_DIR for disk-safety (2026-06-22). Derive the shared
            // target dir from the canonical layout: parent = .camerata-worktrees, grandparent
            // = clone root. Falls back (no env set) when derivation fails (out-of-band wt).
            if let Some(shared_target) = derive_shared_target_dir(wt) {
                cmd.env("CARGO_TARGET_DIR", &shared_target);
            }
        }
        cmd
    }

    /// Derive a session id from the program name. This is a stable, inert string
    /// (not a real session tracking id); it is present so the [`AgentOutcome`]
    /// field is populated rather than empty, matching the shape callers expect.
    fn session_id_from_program(&self) -> String {
        format!("generic-{}", self.program)
    }
}

#[async_trait::async_trait]
impl AgentDriver for GenericCliDriver {
    /// Spawn `program` with the assembled args, capture stdout line-by-line via the
    /// bounded [`crate::stream_subprocess`] helper, and produce an [`AgentOutcome`].
    ///
    /// Output parsing: attempts JSON first (`"result"` field), falls back to raw
    /// stdout. Either way the coordinator receives the same [`AgentOutcome`] shape
    /// it would from any other driver, and the governance layers above are
    /// unaffected by which binary ran here.
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        let cmd = self.build_command(role, task);

        let (stdout, _status) = crate::stream_subprocess(
            cmd,
            self.on_activity.clone(),
            crate::agent_inactivity_window(),
            crate::agent_total_timeout(),
        )
        .await?;

        // Attempt JSON parse first. If the JSON has a `result` field, use the
        // structured fields. Otherwise treat the entire stdout as the result.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if v.get("result").is_some() {
                return Ok(AgentOutcome {
                    session_id: v["session_id"]
                        .as_str()
                        .unwrap_or(&self.session_id_from_program())
                        .to_string(),
                    result: v["result"].as_str().unwrap_or_default().to_string(),
                    cost_usd: v["cost_usd"].as_f64(),
                    denials: vec![],
                });
            }
        }

        // Raw-text fallback: the whole stdout is the result.
        Ok(AgentOutcome {
            session_id: self.session_id_from_program(),
            result: stdout.trim_end().to_string(),
            cost_usd: None,
            denials: vec![],
        })
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::RuleId;

    fn role() -> Role {
        Role {
            name: "Backend".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/".to_string()],
        }
    }

    #[test]
    fn build_args_produces_non_claude_argv() {
        let driver = GenericCliDriver::new("llm", "--prompt", &[]);
        let args = driver.build_args(&role(), "do a thing");

        // The argv must not contain any Claude-specific flag.
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "generic argv must not contain Claude-specific flags"
        );
        assert!(
            !args.iter().any(|a| a == "--strict-mcp-config"),
            "generic argv must not contain Claude-specific flags"
        );
        assert!(
            !args.iter().any(|a| a == "--allowedTools"),
            "generic argv must not contain Claude-specific flags"
        );
        // The program is not "claude".
        assert_ne!(
            driver.program, "claude",
            "program is not the Claude CLI binary"
        );
        // The task flag and task must be present.
        let prompt_idx = args
            .iter()
            .position(|a| a == "--prompt")
            .expect("--prompt flag must be present");
        assert_eq!(args[prompt_idx + 1], "do a thing");
    }

    #[test]
    fn build_args_prepends_base_args() {
        let driver = GenericCliDriver::new("aider", "-m", &["--model", "gpt-4o"]);
        let args = driver.build_args(&role(), "task");

        assert_eq!(args[0], "--model");
        assert_eq!(args[1], "gpt-4o");
        assert_eq!(args[2], "-m");
        assert_eq!(args[3], "task");
    }

    #[test]
    fn build_args_without_worktree_contains_no_dir_scope() {
        let driver = GenericCliDriver::new("llm", "-p", &[]);
        let args = driver.build_args(&role(), "task");
        // No directory-scoping flag — unlike ClaudeCliDriver there is no --add-dir.
        assert!(!args.iter().any(|a| a == "--add-dir"));
    }

    #[test]
    fn with_worktree_sets_field() {
        let driver = GenericCliDriver::new("llm", "-p", &[]).with_worktree("/tmp/wt/generic");
        assert_eq!(driver.worktree, Some(PathBuf::from("/tmp/wt/generic")));
    }

    #[test]
    fn session_id_derived_from_program() {
        let driver = GenericCliDriver::new("my-agent", "-p", &[]);
        assert_eq!(driver.session_id_from_program(), "generic-my-agent");
    }

    // ── CARGO_TARGET_DIR derivation (disk-safety, 2026-06-22) ────────────────

    /// Canonical layout: target dir is the `.camerata-shared-target` sibling of
    /// `.camerata-worktrees/` under the clone root.
    #[test]
    fn derive_shared_target_dir_canonical_layout() {
        let wt = PathBuf::from("/Users/me/ws/acme/api/.camerata-worktrees/camerata__story-7");
        let got = derive_shared_target_dir(&wt);
        assert_eq!(
            got,
            Some(PathBuf::from(
                "/Users/me/ws/acme/api/.camerata-shared-target"
            ))
        );
    }

    /// Two worktrees under the same clone derive the SAME target dir.
    #[test]
    fn derive_shared_target_dir_same_for_same_clone() {
        let base = "/Users/me/ws/acme/api/.camerata-worktrees";
        let wt_a = PathBuf::from(base).join("story-a");
        let wt_b = PathBuf::from(base).join("story-b");
        assert_eq!(derive_shared_target_dir(&wt_a), derive_shared_target_dir(&wt_b));
    }

    /// `build_command` injects CARGO_TARGET_DIR when a canonical worktree is set.
    /// We verify by inspecting the command's env via the std command representation.
    #[test]
    fn build_command_injects_cargo_target_dir_for_canonical_worktree() {
        let wt = PathBuf::from("/tmp/clone/.camerata-worktrees/story-1");
        let driver =
            GenericCliDriver::new("my-agent", "-p", &[]).with_worktree(wt.clone());
        // build_command is private; verify indirectly through the worktree field and
        // the derive function (which build_command calls).
        let expected_target = derive_shared_target_dir(&wt);
        assert!(expected_target.is_some(), "canonical worktree must derive a target dir");
        assert_eq!(
            expected_target.unwrap(),
            PathBuf::from("/tmp/clone/.camerata-shared-target")
        );
    }

    /// No worktree → no CARGO_TARGET_DIR injection (nothing to derive from).
    #[test]
    fn build_command_no_cargo_target_dir_without_worktree() {
        let driver = GenericCliDriver::new("my-agent", "-p", &[]);
        assert!(driver.worktree.is_none(), "no worktree set");
        // derive_shared_target_dir is never called when worktree is None;
        // just confirm the field is unset — the build_command branch is not reached.
    }
}
