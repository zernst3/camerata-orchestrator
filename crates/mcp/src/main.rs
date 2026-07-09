//! `camerata-mcp` binary — serve the first-rung MCP adapter over stdio.
//!
//! Mirrors the governance gateway's serve loop (`crates/gateway/src/main.rs::main`)
//! exactly: build the server, `serve(stdio())`, then park on `waiting()` until the
//! MCP host closes the transport.
//!
//! Run it against a live local Camerata BFF (default `http://127.0.0.1:8787`,
//! override via `CAMERATA_BFF_URL`):
//!
//! ```text
//! cargo run -p camerata-mcp
//! ```
//!
//! or register it with an MCP host, e.g. in a Claude Code mcp-config:
//!
//! ```json
//! { "mcpServers": { "camerata-orchestrator": { "command": "camerata-mcp" } } }
//! ```
//!
//! Startup chatter goes to STDERR only — stdout belongs to the MCP protocol.

use rmcp::{transport::stdio, ServiceExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    eprintln!(
        "[camerata-mcp] Camerata first-rung MCP adapter up (rmcp 1.7, stdio); BFF base: {}",
        camerata_client::bff_base()
    );
    let service = camerata_mcp::Camerata::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
