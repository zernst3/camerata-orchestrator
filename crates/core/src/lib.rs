//! camerata-core: the orchestrator's domain types and the seams every other
//! crate plugs into. Deterministic, makes ZERO model calls.
//!
//! The three load-bearing seams below are why the whole stack is Rust:
//! - [`GovernanceGateway`] (layer-1 real-time gate) — verified feasible in Rust
//!   via the MCP-gateway binding (see docs/RUST_CORE_VERIFICATION.md).
//! - [`AgentDriver`] (agent runtime) — drives `claude -p` subprocesses.
//! - [`CheckRunner`] (layer-2 post-task gate).

use serde::{Deserialize, Serialize};

/// A rule id from the camerata-ai corpus, e.g. `RUST-DIOXUS-3`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

/// Identifies one agent session, as reported by the agent runtime. The gateway
/// maps this to a role + rule-subset (it assigned the session at spawn).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

/// A scoped agent role (e.g. Backend, Frontend). Determines the rule-subset,
/// the tools, and the path boundaries an agent runs under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub rule_subset: Vec<RuleId>,
    pub allowed_paths: Vec<String>,
}

/// A tool call an agent is attempting, as the gate sees it.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub tool: String,
    pub input: serde_json::Value,
}

/// The layer-1 gate's verdict on a single tool call.
#[derive(Debug, Clone)]
pub enum Decision {
    Allow,
    Deny { rule: RuleId, reason: String },
}

/// LAYER-1 SEAM — provider-neutral real-time governance gate.
///
/// Evaluate a tool call against the session's role + rule-subset and decide
/// allow/deny BEFORE the call executes. The MCP-gateway binding
/// (`camerata-gateway`) is one implementation; gate logic MUST NOT assume
/// Claude `PreToolUse` hooks. Proven in Rust (RUST_CORE_VERIFICATION.md).
#[async_trait::async_trait]
pub trait GovernanceGateway: Send + Sync {
    async fn evaluate(&self, session: &SessionId, call: &ToolCall) -> Decision;
}

/// Result of one agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutcome {
    pub session_id: String,
    pub result: String,
    pub cost_usd: Option<f64>,
    pub denials: Vec<String>,
}

/// AGENT-RUNTIME SEAM — spawn and supervise an agent for a role + task.
///
/// The `claude -p` CLI driver (`camerata-agent`) is the first implementation;
/// provider / tier / model live behind this seam.
#[async_trait::async_trait]
pub trait AgentDriver: Send + Sync {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome>;
}

/// LAYER-2 SEAM — post-task structural check.
///
/// Run after an agent finishes; returns the rule ids it violated structurally.
/// The coordinator bounces each violated rule back to the agent for revision.
#[async_trait::async_trait]
pub trait CheckRunner: Send + Sync {
    async fn check(&self, role: &Role, worktree: &std::path::Path)
        -> anyhow::Result<Vec<RuleId>>;
}
