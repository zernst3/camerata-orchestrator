//! `gate-probe` — the #14 end-to-end gate-loop GO/NO-GO on a story, deterministic (no `claude`).
//!
//! This is the thesis-validating probe: it runs ONE story through BOTH gate layers with the REAL
//! engine pieces and NO model call, then prints a single GO / NO-GO verdict.
//!
//!   - LAYER 1 (deny-before-execute): the real [`camerata_gateway::GovernedGateway`] evaluates the
//!     write the agent attempts. A forbidden write MUST be DENIED before it can touch disk; a
//!     clean write MUST be ALLOWED (the gate is not deny-everything).
//!   - LAYER 2 (bounce-and-revise): the real [`camerata_core::FleetCoordinator`] runs the governed
//!     stage; the post-task check flags a planted violation on the first pass, the stage BOUNCES,
//!     and the revise pass resolves it. Proves the "run → deny/flag → bounce → fix → clean" loop
//!     actually closes.
//!
//! Where the `acceptance` command proves Layer 1 in isolation and the coordinator unit tests prove
//! Layer 2, THIS probe runs a story through the whole loop and reports one verdict. The LIVE proof
//! (a real `claude -p` subprocess through the MCP gateway) is `live-demo`; this is its
//! deterministic, CI-able stand-in — the engine-level go/no-go.

use std::path::Path;
use std::sync::Mutex;

use camerata_core::{
    AgentDriver, AgentOutcome, CheckRunner, Decision, FleetCoordinator, FleetStage,
    GovernanceGateway, Role, RuleId, SessionId, ToolCall,
};
use camerata_gateway::{enforced_gate_rules, gov1_rule, GovernedGateway};

/// A governed-agent stand-in whose FIRST pass leaves a layer-2 violation and whose revise pass is
/// clean — so the coordinator bounces exactly once and resolves. No model call (hermetic).
struct BounceThenCleanDriver {
    calls: Mutex<usize>,
}

impl BounceThenCleanDriver {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl AgentDriver for BounceThenCleanDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        Ok(AgentOutcome {
            session_id: format!("gate-probe-{}", role.name.to_lowercase()),
            result: format!("pass {n}: {task}"),
            cost_usd: Some(0.0),
            denials: vec![],
        })
    }
}

/// A layer-2 check runner that flags a violation on the FIRST check (dirty), then reports clean —
/// so the stage bounces once and the revise pass resolves it. (`gov1_rule()` stands in for the
/// violated rule id; the coordinator only cares that the set is non-empty then empty.)
struct DirtyThenCleanChecks {
    checks: Mutex<usize>,
}

impl DirtyThenCleanChecks {
    fn new() -> Self {
        Self {
            checks: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl CheckRunner for DirtyThenCleanChecks {
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let n = {
            let mut c = self.checks.lock().unwrap();
            *c += 1;
            *c
        };
        Ok(if n == 1 { vec![gov1_rule()] } else { vec![] })
    }
}

/// The probe's verdict across both gate layers.
#[derive(Debug)]
pub struct GateProbeResult {
    pub story: String,
    /// LAYER 1 — the gate's verdict on the agent's attempted FORBIDDEN write (must be `Deny`).
    pub layer1_forbidden: Decision,
    /// LAYER 1 — the gate's verdict on a CLEAN write (must be `Allow`; the gate isn't deny-all).
    pub layer1_clean: Decision,
    /// LAYER 2 — did the governed stage bounce-and-revise?
    pub layer2_bounced: bool,
    /// LAYER 2 — did the revise pass end clean (no residual violations)?
    pub layer2_clean: bool,
    /// The number of agent passes the driver actually ran (1 initial + 1 revise = 2 on a bounce).
    pub agent_passes: usize,
}

impl GateProbeResult {
    /// GO iff: the forbidden write was denied, the clean write allowed, AND the layer-2 loop
    /// bounced once and resolved. Anything else is NO-GO (the gate is not fully wired).
    pub fn go(&self) -> bool {
        self.layer1_denied()
            && self.layer1_clean_allowed()
            && self.layer2_bounced
            && self.layer2_clean
    }

    /// LAYER 1 — was the agent's forbidden write denied? (Accessors so callers like the server's
    /// JSON endpoint don't need to match on `Decision` directly.)
    pub fn layer1_denied(&self) -> bool {
        matches!(self.layer1_forbidden, Decision::Deny { .. })
    }

    /// LAYER 1 — the denial's "[RULE] reason" (empty when the forbidden write was NOT denied).
    pub fn layer1_reason(&self) -> String {
        match &self.layer1_forbidden {
            Decision::Deny { rule, reason } => format!("[{}] {reason}", rule.0),
            Decision::Allow => String::new(),
        }
    }

    /// LAYER 1 — was the clean control write allowed?
    pub fn layer1_clean_allowed(&self) -> bool {
        matches!(self.layer1_clean, Decision::Allow)
    }
}

/// Run the end-to-end gate-loop probe in-process (no network, no model).
pub async fn run_gate_probe() -> anyhow::Result<GateProbeResult> {
    let story = "Implement a feature in crates/core".to_string();
    // The governed role carries the FULL enforced gate set — the same set the live fleet rides.
    let role = Role {
        name: "Implementer".to_string(),
        rule_subset: enforced_gate_rules(),
        allowed_paths: vec!["crates/".to_string()],
    };
    let session = SessionId("gate-probe-session".to_string());

    // ── LAYER 1: deny-before-execute, via the real gateway bound to the session/role. ──
    let gateway = GovernedGateway::new().with_session(session.clone(), role.clone());
    let forbidden = ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/forbidden/leak.rs",
            "content": "// agent tried to write here"
        }),
    };
    let layer1_forbidden = gateway.evaluate(&session, &forbidden).await;
    let clean = ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/core/src/feature.rs",
            "content": "pub fn feature() {}"
        }),
    };
    let layer1_clean = gateway.evaluate(&session, &clean).await;

    // ── LAYER 2: bounce-and-revise, via the real coordinator (dirty-then-clean ⇒ one bounce). ──
    let driver = BounceThenCleanDriver::new();
    let checks = DirtyThenCleanChecks::new();
    let worktree = std::env::temp_dir().join(format!("camerata-gate-probe-{}", std::process::id()));
    let fleet = FleetCoordinator::new(&checks, &worktree);
    let stage = FleetStage::new(role.clone(), story.clone(), &driver);
    let report = fleet.run(std::slice::from_ref(&stage)).await?;
    let stage0 = &report.stages[0].report;
    let layer2_bounced = stage0.bounced;
    let layer2_clean = stage0.final_violations.is_empty();
    let agent_passes = *driver.calls.lock().unwrap();

    Ok(GateProbeResult {
        story,
        layer1_forbidden,
        layer1_clean,
        layer2_bounced,
        layer2_clean,
        agent_passes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gate_probe_is_go_end_to_end() {
        let r = run_gate_probe().await.expect("probe runs");
        // LAYER 1: forbidden denied, clean allowed.
        assert!(
            matches!(r.layer1_forbidden, Decision::Deny { .. }),
            "forbidden write must be denied: {:?}",
            r.layer1_forbidden
        );
        assert!(
            matches!(r.layer1_clean, Decision::Allow),
            "clean write must be allowed"
        );
        // LAYER 2: bounced once and resolved; the driver ran an initial + a revise pass.
        assert!(r.layer2_bounced, "the stage must bounce on the planted violation");
        assert!(r.layer2_clean, "the revise pass must resolve the violation");
        assert_eq!(r.agent_passes, 2, "initial + one revise pass");
        // Whole-loop predicate.
        assert!(r.go(), "the gate loop must be GO end to end");
    }
}
