//! Camerata governance gateway — minimal Rust MCP server (rmcp 1.7).
//!
//! Proves the load-bearing claim the Haiku NO-GO doc said was impossible:
//! a Rust-owned governance gate that dynamically allows/denies an agent's
//! tool calls, in-process, per a data-driven rule. The agent (claude -p) is
//! locked to ONLY this server's `gated_write` tool; every write the agent
//! attempts routes through Rust code that applies the rule before acting.

use camerata_core::{Decision, ToolCall};
use camerata_gateway::{evaluate_call, gov1_rule};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Absolute path to write.
    pub path: String,
    /// File content.
    pub content: String,
}

#[derive(Clone)]
pub struct Gateway {
    tool_router: ToolRouter<Self>,
}

impl Gateway {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// The data-driven governance rule. In the real orchestrator this is the
    /// per-session role's rule-subset, looked up by session id. Here we apply a
    /// single-rule subset (GOV-1) through the SHARED evaluation function in
    /// `camerata_gateway::evaluate_call`, so this transport and the in-process
    /// `GovernedGateway` enforce byte-for-byte identical logic.
    fn evaluate(path: &str) -> Result<(), String> {
        let call = ToolCall {
            tool: "gated_write".to_string(),
            input: serde_json::json!({ "path": path }),
        };
        match evaluate_call(&[gov1_rule()], &call) {
            Decision::Allow => Ok(()),
            Decision::Deny { reason, .. } => Err(reason),
        }
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

        let decision = match Gateway::evaluate(&path) {
            Err(rule) => format!("DENIED [{rule}] path={path}"),
            Ok(()) => match std::fs::write(&path, content.as_bytes()) {
                Ok(()) => format!("ALLOWED: wrote {} bytes to {path}", content.len()),
                Err(e) => format!("ALLOWED but IO error on {path}: {e}"),
            },
        };

        let micros = t0.elapsed().as_micros();
        let line = format!("gated_write gate_decision={micros}us -> {decision}\n");
        eprint!("[gateway] {line}");
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/camerata-verify/gateway.log")
        {
            let _ = f.write_all(line.as_bytes());
        }
        decision
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
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
    eprintln!("[gateway] Camerata Rust MCP governance gateway up (rmcp 1.7, stdio)");
    let service = Gateway::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
