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

/// Upper bound on the `diagnostics` text a [`CheckOutcome`] carries, in bytes.
///
/// The captured toolchain output (clippy / tsc / pytest / go vet stdout+stderr)
/// can be arbitrarily large; feeding all of it into the Layer-2 bounce prompt
/// would blow the context budget and, worse, evict the warm prefix cache. We cap
/// at 16 KiB and keep the TAIL (see [`CheckOutcome::push_diagnostics`]) because
/// the most recent / most-relevant errors a tool prints come last (summaries,
/// final error counts, the failing assertion). 16 KiB is ~4k tokens — enough for
/// several distinct failures without dominating the prompt.
pub const DIAGNOSTICS_CAP_BYTES: usize = 16 * 1024;

/// Result of one Layer-2 [`CheckRunner::check`] pass.
///
/// `violated` is the structural verdict the coordinator bounces on (unchanged
/// behaviour from when `check` returned a bare `Vec<RuleId>`). `diagnostics` is
/// the RAW toolchain stdout/stderr the runner captured while producing that
/// verdict — the "strict stack trace" a literal open-weight model needs in order
/// to self-correct, rather than just the rule id. It is capped at
/// [`DIAGNOSTICS_CAP_BYTES`] (most-relevant-last) so it can be forwarded straight
/// into the bounce prompt tail without unbounded growth. Where a runner has no
/// meaningful stdout, `diagnostics` is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    /// The rule ids the pass flagged as violated.
    pub violated: Vec<RuleId>,
    /// Captured, truncation-bounded toolchain output for the violated checks.
    pub diagnostics: String,
}

impl CheckOutcome {
    /// A clean outcome: no violations, no diagnostics.
    pub fn clean() -> Self {
        Self::default()
    }

    /// Build from an already-assembled `violated` list and raw diagnostics text.
    /// The diagnostics are truncated to [`DIAGNOSTICS_CAP_BYTES`] (tail kept).
    pub fn new(violated: Vec<RuleId>, diagnostics: impl Into<String>) -> Self {
        let mut out = Self {
            violated,
            diagnostics: String::new(),
        };
        out.push_diagnostics(diagnostics.into().as_str());
        out
    }

    /// Append `text` to the accumulated diagnostics, keeping the total under
    /// [`DIAGNOSTICS_CAP_BYTES`]. When the running total would exceed the cap we
    /// drop the OLDEST bytes (the head), because the tail — the tool's final
    /// summary / failing assertion — is what the revise pass needs most. A char
    /// boundary is respected so the string stays valid UTF-8.
    pub fn push_diagnostics(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.diagnostics.is_empty() {
            self.diagnostics.push('\n');
        }
        self.diagnostics.push_str(text);
        if self.diagnostics.len() > DIAGNOSTICS_CAP_BYTES {
            // Keep the last DIAGNOSTICS_CAP_BYTES bytes; snap forward to a char
            // boundary so we never split a multibyte sequence.
            let mut cut = self.diagnostics.len() - DIAGNOSTICS_CAP_BYTES;
            while cut < self.diagnostics.len() && !self.diagnostics.is_char_boundary(cut) {
                cut += 1;
            }
            let tail = self.diagnostics.split_off(cut);
            self.diagnostics = format!("[... earlier diagnostics truncated ...]\n{tail}");
        }
    }
}

/// LAYER-2 SEAM — post-task structural check.
///
/// Run after an agent finishes; returns the rule ids it violated structurally
/// PLUS the raw toolchain diagnostics captured while checking (see
/// [`CheckOutcome`]). The coordinator bounces each violated rule back to the
/// agent for revision, and forwards the diagnostics so the agent sees the actual
/// error text, not just the rule id.
#[async_trait::async_trait]
pub trait CheckRunner: Send + Sync {
    async fn check(
        &self,
        role: &Role,
        worktree: &std::path::Path,
    ) -> anyhow::Result<CheckOutcome>;
}

pub mod coordinator;
pub use coordinator::{Coordinator, CoordinatorError, RunReport};

pub mod fleet;
pub use fleet::{FleetCoordinator, FleetReport, FleetStage, StageReport};

// LivenessTracker moved to the `camerata-liveness` leaf crate (Phase 1b).
// Import it from `camerata_liveness::LivenessTracker` directly.

#[cfg(test)]
mod check_outcome_tests {
    use super::*;

    #[test]
    fn clean_outcome_has_no_violations_or_diagnostics() {
        let out = CheckOutcome::clean();
        assert!(out.violated.is_empty());
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn new_carries_violations_and_diagnostics() {
        let out = CheckOutcome::new(
            vec![RuleId("RUST-CLIPPY".into())],
            "error: unused variable `x`",
        );
        assert_eq!(out.violated, vec![RuleId("RUST-CLIPPY".into())]);
        assert!(out.diagnostics.contains("unused variable"));
    }

    #[test]
    fn push_diagnostics_joins_pieces_with_newlines_in_order() {
        let mut out = CheckOutcome::clean();
        out.push_diagnostics("first");
        out.push_diagnostics("second");
        assert_eq!(out.diagnostics, "first\nsecond");
    }

    #[test]
    fn push_diagnostics_ignores_empty_pieces() {
        let mut out = CheckOutcome::clean();
        out.push_diagnostics("");
        out.push_diagnostics("only");
        out.push_diagnostics("");
        assert_eq!(out.diagnostics, "only");
    }

    #[test]
    fn push_diagnostics_truncates_past_the_cap_keeping_the_tail() {
        let mut out = CheckOutcome::clean();
        // A head marker that must be evicted, then a large filler, then a tail
        // marker (the failing assertion) that MUST survive.
        out.push_diagnostics("HEAD_MARKER_SHOULD_BE_DROPPED");
        out.push_diagnostics(&"x".repeat(DIAGNOSTICS_CAP_BYTES));
        out.push_diagnostics("TAIL_MARKER_MUST_SURVIVE");

        assert!(
            out.diagnostics.len() <= DIAGNOSTICS_CAP_BYTES + 64,
            "diagnostics must be bounded near the cap, got {} bytes",
            out.diagnostics.len()
        );
        assert!(
            out.diagnostics.contains("TAIL_MARKER_MUST_SURVIVE"),
            "the most-relevant tail must be preserved"
        );
        assert!(
            !out.diagnostics.contains("HEAD_MARKER_SHOULD_BE_DROPPED"),
            "the oldest head bytes must be dropped"
        );
        assert!(
            out.diagnostics.contains("earlier diagnostics truncated"),
            "truncation must be signposted for the reader"
        );
    }

    #[test]
    fn push_diagnostics_truncation_respects_utf8_boundaries() {
        let mut out = CheckOutcome::clean();
        // Multibyte chars straddling the cut point must not be split (no panic,
        // valid UTF-8 result).
        out.push_diagnostics(&"日本語テスト".repeat(DIAGNOSTICS_CAP_BYTES / 3));
        // Simply constructing + reading the string proves it stayed valid UTF-8.
        assert!(out.diagnostics.len() <= DIAGNOSTICS_CAP_BYTES + 64);
    }
}
