//! Camerata governance gateway — Rust MCP server (rmcp 1.7), stdio transport.
//!
//! Proves the load-bearing claim the Haiku NO-GO doc said was impossible:
//! a Rust-owned governance gate that dynamically allows/denies an agent's
//! tool calls, in-process, per a data-driven rule-subset. The agent
//! (`claude -p`) is locked to ONLY this server's `gated_write` tool; every
//! write the agent attempts routes through Rust code that applies the active
//! rule-subset BEFORE touching the filesystem.
//!
//! # Per-session rule delivery (the live slice)
//!
//! The rule-subset is NOT hard-coded. At agent spawn the orchestrator:
//!   1. computes the session's rule-subset,
//!   2. writes it to a per-session rules JSON file,
//!   3. generates an mcp-config that launches THIS binary with env
//!      `CAMERATA_RULES_FILE` pointing at that file.
//!
//! On startup this server reads `CAMERATA_RULES_FILE` (a JSON array of rule-id
//! strings, e.g. `["GOV-1"]`) and evaluates every tool call against it via the
//! SHARED [`camerata_gateway::evaluate_call`]. If the env var is unset it falls
//! back to the verified default subset `["GOV-1"]`. This is the stdio binding
//! of the same `GovernanceGateway` logic the in-process `GovernedGateway` uses
//! — byte-for-byte identical evaluation, two transports.
//!
//! # MCP tool namespacing
//!
//! Claude Code namespaces an MCP tool as `mcp__<server-key>__<tool>`, where
//! `<server-key>` is the key under `mcpServers` in the mcp-config. The config
//! generator (`camerata_agent::mcp_config`) uses the key `camerata`, and this
//! server registers the tool `gated_write`, so the agent sees exactly
//! `mcp__camerata__gated_write` — the constant `camerata_agent::GATED_WRITE_TOOL`.

use camerata_core::{Decision, RuleId, ToolCall};
use camerata_gateway::{evaluate_call, gov1_rule};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

mod delegate;
use delegate::OrchestratorConfig;

/// Env var the orchestrator points at the per-session rules JSON file.
pub const RULES_FILE_ENV: &str = "CAMERATA_RULES_FILE";

/// Env var the orchestrator points at the worktree the agent is jailed to. When set,
/// `gated_write` refuses any write whose resolved target is outside this root, in CODE,
/// independent of any rule. This is the structural guard for "guard the guard":
/// `--add-dir` only scopes the agent's (disallowed) built-ins, not the gateway process
/// that performs the actual write, so the jail has to live here, not in a rule.
pub const WORKTREE_ROOT_ENV: &str = "CAMERATA_WORKTREE_ROOT";

/// Lexically normalize a path: resolve `.` and `..` WITHOUT touching the filesystem
/// (so it works for not-yet-created files). Symlink resolution is intentionally not
/// done; the agent cannot create symlinks because `Bash` is denied at the cage.
fn normalize_lexical(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `target` resolves to a path inside `root`. Relative targets resolve against
/// `root` (the agent's cwd is the worktree); absolute targets are taken as-is. Both are
/// lexically normalized, then a component-wise prefix check jails the result. An
/// absolute path outside the worktree, or a `..` climb above it, returns false.
fn within_jail(root: &std::path::Path, target: &str) -> bool {
    let t = std::path::Path::new(target);
    let abs = if t.is_absolute() {
        t.to_path_buf()
    } else {
        root.join(t)
    };
    normalize_lexical(&abs).starts_with(normalize_lexical(root))
}

/// Load the worktree jail root from the environment, canonicalized. `None` means no
/// jail is configured (standalone / test runs keep the rule-only behavior).
fn load_jail_root() -> Option<std::path::PathBuf> {
    let raw = std::env::var_os(WORKTREE_ROOT_ENV)?;
    let path = std::path::PathBuf::from(raw);
    match std::fs::canonicalize(&path) {
        Ok(canon) => Some(canon),
        Err(_) => Some(path),
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Absolute path to write.
    pub path: String,
    /// File content.
    pub content: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DelegateArgs {
    /// A clear, well-scoped instruction for the delegate child to carry out.
    pub subtask: String,
    /// The tier to delegate to: `"fast"` or `"balanced"` (you are `strongest`).
    pub tier: String,
}

/// Load the session's rule-subset from `CAMERATA_RULES_FILE` if set, else fall
/// back to the verified default subset `["GOV-1"]`.
///
/// The file is a JSON array of rule-id strings, e.g. `["GOV-1"]`. This is the
/// data-driven delivery channel: the orchestrator's live rule selection arrives
/// as data, not code. A missing/unreadable/unparseable file fails CLOSED onto
/// the GOV-1 default rather than an empty (allow-everything) subset, so a
/// delivery glitch can never silently disable governance.
fn load_rule_subset() -> Vec<RuleId> {
    let Some(path) = std::env::var_os(RULES_FILE_ENV) else {
        eprintln!("[gateway] {RULES_FILE_ENV} unset; using default subset [GOV-1]");
        return vec![gov1_rule()];
    };
    let path = std::path::PathBuf::from(path);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Vec<RuleId>>(&text) {
            Ok(ids) if !ids.is_empty() => {
                eprintln!(
                    "[gateway] loaded {} rule(s) from {}: {}",
                    ids.len(),
                    path.display(),
                    ids.iter()
                        .map(|r| r.0.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                );
                ids
            }
            Ok(_) => {
                eprintln!(
                    "[gateway] {} parsed to an EMPTY subset; failing closed onto [GOV-1]",
                    path.display()
                );
                vec![gov1_rule()]
            }
            Err(e) => {
                eprintln!(
                    "[gateway] could not parse {} ({e}); failing closed onto [GOV-1]",
                    path.display()
                );
                vec![gov1_rule()]
            }
        },
        Err(e) => {
            eprintln!(
                "[gateway] could not read {} ({e}); failing closed onto [GOV-1]",
                path.display()
            );
            vec![gov1_rule()]
        }
    }
}

#[derive(Clone)]
pub struct Gateway {
    tool_router: ToolRouter<Self>,
    /// The session's rule-subset, delivered via `CAMERATA_RULES_FILE` at spawn.
    /// Shared (`Arc`) because rmcp clones the handler per connection.
    rule_subset: Arc<Vec<RuleId>>,
    /// The worktree the agent is jailed to (via `CAMERATA_WORKTREE_ROOT`). When set,
    /// `gated_write` refuses any target outside it, in code, before any rule runs.
    jail_root: Option<Arc<std::path::PathBuf>>,
    /// Orchestrator-mode config (`None` = the `delegate` tool is disabled). Set only
    /// when the gateway is launched for the LEAD agent with the delegate env vars.
    /// When `None`, `delegate` refuses; this is the per-process half of the gate.
    orchestrator: Option<Arc<OrchestratorConfig>>,
}

impl Gateway {
    /// Construct the gateway with the rule-subset + jail root read from the environment.
    pub fn new() -> Self {
        Self::with_rules(load_rule_subset())
    }

    /// Construct the gateway with an explicit rule-subset (used by `new` and by
    /// tests). Keeps the evaluation seam injectable. The jail root is read from the
    /// environment (unset = no jail).
    pub fn with_rules(rule_subset: Vec<RuleId>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            rule_subset: Arc::new(rule_subset),
            jail_root: load_jail_root().map(Arc::new),
            // Orchestrator mode is opt-in via env and OFF by default, so every
            // non-lead agent's gateway refuses `delegate`.
            orchestrator: OrchestratorConfig::from_env().map(Arc::new),
        }
    }

    /// Evaluate a write against the active rule-subset through the SHARED
    /// [`evaluate_call`], so this transport and the in-process `GovernedGateway`
    /// enforce byte-for-byte identical logic.
    ///
    /// BOTH `path` and `content` are forwarded into the `ToolCall.input`: path
    /// rules (GOV-1) key off `path`, and content rules
    /// (SEC-NO-HARDCODED-SECRETS-1, SEC-NO-RAW-SQL-CONCAT-1,
    /// ARCH-NO-SECRETS-IN-URL-1) key off `content`. Omitting `content` here
    /// would silently disable every content rule over the live transport — the
    /// gate would load them, report them, and never enforce them.
    fn evaluate(&self, path: &str, content: &str) -> Result<(), String> {
        let call = ToolCall {
            tool: "gated_write".to_string(),
            input: serde_json::json!({ "path": path, "content": content }),
        };
        match evaluate_call(&self.rule_subset, &call) {
            Decision::Allow => Ok(()),
            Decision::Deny { reason, .. } => Err(reason),
        }
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl Gateway {
    /// Write a file. Governed: the gate runs in Rust before any write happens.
    #[tool(
        name = "gated_write",
        description = "Write a file to disk. Governed by Camerata: the write is evaluated against the active rule-subset BEFORE execution; a denied write never touches the filesystem."
    )]
    pub async fn gated_write(&self, args: Parameters<WriteArgs>) -> String {
        let t0 = Instant::now();
        let WriteArgs { path, content } = args.0;

        // Structural worktree jail FIRST, independent of any rule: the gateway process
        // can write anywhere on the filesystem, so refuse any target outside the
        // worktree before evaluating content rules. This is what keeps the agent from
        // writing its own rules.json / system files via an absolute path.
        let decision = if self
            .jail_root
            .as_ref()
            .is_some_and(|root| !within_jail(root, &path))
        {
            format!("DENIED [JAIL: outside the worktree] path={path}")
        } else {
            match self.evaluate(&path, &content) {
                Err(rule) => format!("DENIED [{rule}] path={path}"),
                Ok(()) => match std::fs::write(&path, content.as_bytes()) {
                    Ok(()) => format!("ALLOWED: wrote {} bytes to {path}", content.len()),
                    Err(e) => format!("ALLOWED but IO error on {path}: {e}"),
                },
            }
        };

        let micros = t0.elapsed().as_micros();
        let line = format!("gated_write gate_decision={micros}us -> {decision}\n");
        eprint!("[gateway] {line}");
        // Best-effort decision log next to the per-session rules file. The dir
        // may not exist in every environment; ignore failures (the stderr line
        // is the authoritative trace).
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/camerata-verify/gateway.log")
        {
            use std::io::Write as _;
            let _ = f.write_all(line.as_bytes());
        }
        decision
    }

    /// Governed delegation. ENABLED only in orchestrator mode (the lead agent's
    /// gateway). Spawns a SINGLE gated `claude -p` child on the requested tier's
    /// model — gated_write ONLY, `delegate` DISABLED, depth+1, same worktree —
    /// runs the subtask synchronously, and returns the child's full output. A
    /// child never calls "up": escalation is parent-driven (the orchestrator reads
    /// the result and decides). This is the ONLY governed spawn path; the raw CLI
    /// `Task` tool stays disallowed for every agent.
    #[tool(
        name = "delegate",
        description = "Delegate a well-scoped subtask to a lower tier. Spawns ONE gated child agent on the chosen tier ('fast' or 'balanced'), runs it in the same worktree, and returns its full output. If the child returns text starting with 'INCOMPLETE:' the work was above its tier — do it yourself or re-delegate higher. You cannot be delegated to; you are the strongest tier."
    )]
    pub async fn delegate(&self, args: Parameters<DelegateArgs>) -> String {
        let DelegateArgs { subtask, tier } = args.0;

        // Per-process gate: if this gateway was not launched in orchestrator mode,
        // refuse. Non-lead agents never set the orchestrator env, so their gateway
        // lands here. (Their --allowedTools also omits the tool, so this is the
        // belt to that suspenders.)
        let Some(config) = self.orchestrator.as_ref() else {
            eprintln!("[gateway] delegate REFUSED: gateway is not in orchestrator mode");
            return "DELEGATE REFUSED: this agent is not the orchestrator; \
                    delegation is not available. Do the work yourself."
                .to_string();
        };

        eprintln!(
            "[gateway] delegate tier={tier} depth={} max_depth={} subtask_len={}",
            config.depth,
            config.max_depth,
            subtask.len()
        );

        match delegate::run_delegated(config, (*self.rule_subset).clone(), &subtask, &tier).await {
            Ok(output) => output,
            Err(e) => e.to_string(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        // Register the server identity as `camerata`. (Claude Code derives the
        // tool's `mcp__<key>__` prefix from the mcp-config KEY, not this field;
        // we still set it so the server self-identifies consistently.)
        info.server_info = Implementation::new("camerata", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Camerata governance gateway. The ONLY way to write files is gated_write; \
             it is subject to governance rules enforced in-process."
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subset = load_rule_subset();
    eprintln!(
        "[gateway] Camerata Rust MCP governance gateway up (rmcp 1.7, stdio); active subset: {}",
        subset
            .iter()
            .map(|r| r.0.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let service = Gateway::with_rules(subset).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod jail_tests {
    use super::within_jail;
    use std::path::Path;

    #[test]
    fn relative_paths_resolve_under_the_worktree() {
        let root = Path::new("/work/crate");
        assert!(within_jail(root, "src/lib.rs"));
        assert!(within_jail(root, "./src/api/members.rs"));
        assert!(within_jail(root, "Cargo.toml"));
    }

    #[test]
    fn absolute_paths_outside_the_worktree_are_jailed() {
        let root = Path::new("/work/crate");
        // The exact attack: the agent's own session config, a sibling of the worktree.
        assert!(!within_jail(root, "/work/session-1/rules.json"));
        assert!(!within_jail(root, "/work/session-1/gateway.json"));
        // System files.
        assert!(!within_jail(root, "/etc/passwd"));
        assert!(!within_jail(root, "/root/.ssh/authorized_keys"));
        // A sibling whose name merely starts with the root name (component-wise check).
        assert!(!within_jail(root, "/work/crate-evil/x.rs"));
    }

    #[test]
    fn dotdot_climbs_above_the_worktree_are_jailed() {
        let root = Path::new("/work/crate");
        assert!(!within_jail(root, "../session-1/rules.json"));
        assert!(!within_jail(root, "src/../../session-1/rules.json"));
        // A `..` that stays inside is fine.
        assert!(within_jail(root, "src/api/../lib.rs"));
    }

    #[test]
    fn absolute_paths_inside_the_worktree_are_allowed() {
        let root = Path::new("/work/crate");
        assert!(within_jail(root, "/work/crate/src/lib.rs"));
    }
}
