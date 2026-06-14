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

/// Env var the orchestrator points at the per-session rules JSON file.
pub const RULES_FILE_ENV: &str = "CAMERATA_RULES_FILE";

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Absolute path to write.
    pub path: String,
    /// File content.
    pub content: String,
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
}

impl Gateway {
    /// Construct the gateway with the rule-subset read from the environment.
    pub fn new() -> Self {
        Self::with_rules(load_rule_subset())
    }

    /// Construct the gateway with an explicit rule-subset (used by `new` and by
    /// tests). Keeps the evaluation seam injectable.
    pub fn with_rules(rule_subset: Vec<RuleId>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            rule_subset: Arc::new(rule_subset),
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

        let decision = match self.evaluate(&path, &content) {
            Err(rule) => format!("DENIED [{rule}] path={path}"),
            Ok(()) => match std::fs::write(&path, content.as_bytes()) {
                Ok(()) => format!("ALLOWED: wrote {} bytes to {path}", content.len()),
                Err(e) => format!("ALLOWED but IO error on {path}: {e}"),
            },
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
