//! `gate-probe` — the #14 end-to-end gate-loop GO/NO-GO on a story, deterministic (no `claude`).
//!
//! The thesis-validating probe, as a single runnable verdict. It runs ONE story through BOTH gate
//! layers with the REAL engine and NO model call:
//!
//!   - LAYER 1 (deny-before-execute): the real [`camerata_gateway::GovernedGateway`] evaluates a
//!     planted violation for EVERY enforced rule (the whole security floor) plus a clean control.
//!     All violations must be DENIED before they can touch disk; the clean write must be ALLOWED.
//!   - LAYER 2 (bounce-and-revise): the real [`camerata_core::FleetCoordinator`] runs the governed
//!     stage; the post-task check flags a planted violation on the first pass, the stage BOUNCES,
//!     and the revise pass resolves it.
//!
//! GO iff the whole floor denied, the control allowed, and the loop bounced-and-resolved. Where
//! `acceptance` proves a few layer-1 rules in isolation and the coordinator unit tests prove
//! layer 2, THIS runs a story through the whole loop and reports one verdict. The LIVE proof (a
//! real `claude -p` through the MCP gateway) is `live-demo`; this is its deterministic, CI-able
//! stand-in — the engine-level go/no-go, surfaced in-app as the Governed Development self-check.

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
            let mut c = self.calls.lock().expect("calls mutex poisoned");
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
/// so the stage bounces once and the revise pass resolves it.
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
            let mut c = self.checks.lock().expect("checks mutex poisoned");
            *c += 1;
            *c
        };
        Ok(if n == 1 { vec![gov1_rule()] } else { vec![] })
    }
}

/// One layer-1 planted-violation check: which floor rule it targets, and whether the gate denied it.
#[derive(Debug, Clone)]
pub struct Layer1Check {
    /// Human label for the planted violation (e.g. "forbidden path", "hardcoded secret").
    pub label: String,
    /// Whether the gate denied it (every planted violation MUST be denied).
    pub denied: bool,
    /// The denial's "[RULE] reason", or the unexpected-allow note.
    pub detail: String,
}

/// The probe's verdict across both gate layers.
#[derive(Debug)]
pub struct GateProbeResult {
    pub story: String,
    /// LAYER 1 — one entry per planted floor violation; ALL must be `denied`.
    pub layer1: Vec<Layer1Check>,
    /// LAYER 1 — the gate's verdict on a CLEAN write (must be allowed; the gate isn't deny-all).
    pub layer1_clean_allowed: bool,
    /// LAYER 2 — did the governed stage bounce-and-revise?
    pub layer2_bounced: bool,
    /// LAYER 2 — did the revise pass end clean (no residual violations)?
    pub layer2_clean: bool,
    /// Agent passes the driver ran (1 initial + 1 revise = 2 on a bounce).
    pub agent_passes: usize,
}

impl GateProbeResult {
    /// How many planted floor violations the gate denied, and how many were planted.
    pub fn layer1_denied_count(&self) -> usize {
        self.layer1.iter().filter(|c| c.denied).count()
    }
    pub fn layer1_total(&self) -> usize {
        self.layer1.len()
    }

    /// GO iff: EVERY planted floor violation was denied, the clean write was allowed, AND the
    /// layer-2 loop bounced once and resolved. Anything else is NO-GO (the gate isn't fully wired).
    pub fn go(&self) -> bool {
        !self.layer1.is_empty()
            && self.layer1.iter().all(|c| c.denied)
            && self.layer1_clean_allowed
            && self.layer2_bounced
            && self.layer2_clean
    }
}

/// Build the `gated_write` tool-call for a planted (path, content).
fn write_call(path: &str, content: &str) -> ToolCall {
    ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({ "path": path, "content": content }),
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

    // ── LAYER 1: deny-before-execute. One planted violation per enforced floor rule. ──
    let gateway = GovernedGateway::new().with_session(session.clone(), role.clone());
    let planted: Vec<(&str, ToolCall)> =
        vec![
        (
            "forbidden path (GOV-1)",
            write_call("crates/forbidden/leak.rs", "// agent tried to write here"),
        ),
        (
            "path escape (SEC-NO-PATH-ESCAPE-1)",
            write_call("crates/../../etc/cron.d/payload", "*/1 * * * * root sh -c id"),
        ),
        (
            "hardcoded secret (SEC-NO-HARDCODED-SECRETS-1)",
            write_call(
                "crates/core/src/config.rs",
                "let token = \"ghp_ABCDEFGHIJ1234567890abcdefghij12\";",
            ),
        ),
        (
            "raw SQL concat (SEC-NO-RAW-SQL-CONCAT-1)",
            write_call(
                "crates/core/src/db.rs",
                "let q = format!(\"SELECT * FROM users WHERE id = {}\", id);",
            ),
        ),
        (
            "secret in URL (ARCH-NO-SECRETS-IN-URL-1)",
            write_call(
                "crates/core/src/api.rs",
                "let endpoint = \"https://api.example.com/data?access_token=abc123def456ghi789\";",
            ),
        ),
        (
            "secret file (SEC-NO-SECRET-FILES-1)",
            write_call("crates/core/.env", "API_SECRET=supersecretvalue"),
        ),
    ];
    let mut layer1 = Vec::with_capacity(planted.len());
    for (label, call) in &planted {
        let (denied, detail) = match gateway.evaluate(&session, call).await {
            Decision::Deny { rule, reason } => (true, format!("[{}] {reason}", rule.0)),
            Decision::Allow => (
                false,
                "ALLOWED — this floor rule is not wired on writes".to_string(),
            ),
        };
        layer1.push(Layer1Check {
            label: label.to_string(),
            denied,
            detail,
        });
    }
    let clean = write_call("crates/core/src/feature.rs", "pub fn feature() {}");
    let layer1_clean_allowed = matches!(gateway.evaluate(&session, &clean).await, Decision::Allow);

    // ── LAYER 2: bounce-and-revise (dirty-then-clean ⇒ exactly one bounce that resolves). ──
    let driver = BounceThenCleanDriver::new();
    let checks = DirtyThenCleanChecks::new();
    let worktree = std::env::temp_dir().join(format!("camerata-gate-probe-{}", std::process::id()));
    let fleet = FleetCoordinator::new(&checks, &worktree);
    let stage = FleetStage::new(role.clone(), story.clone(), &driver);
    let report = fleet.run(std::slice::from_ref(&stage)).await?;
    let stage0 = &report.stages[0].report;
    let layer2_bounced = stage0.bounced;
    let layer2_clean = stage0.final_violations.is_empty();
    let agent_passes = *driver.calls.lock().expect("calls mutex poisoned");

    Ok(GateProbeResult {
        story,
        layer1,
        layer1_clean_allowed,
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
        // LAYER 1: the whole floor denied every planted violation.
        assert!(r.layer1_total() >= 6, "all enforced floor rules are probed");
        for c in &r.layer1 {
            assert!(
                c.denied,
                "planted violation must be denied: {} — {}",
                c.label, c.detail
            );
        }
        assert_eq!(
            r.layer1_denied_count(),
            r.layer1_total(),
            "every planted floor violation must be denied"
        );
        assert!(r.layer1_clean_allowed, "clean write must be allowed");
        // LAYER 2: bounced once and resolved; the driver ran an initial + a revise pass.
        assert!(
            r.layer2_bounced,
            "the stage must bounce on the planted violation"
        );
        assert!(r.layer2_clean, "the revise pass must resolve the violation");
        assert_eq!(r.agent_passes, 2, "initial + one revise pass");
        // Whole-loop predicate.
        assert!(r.go(), "the gate loop must be GO end to end");
    }
}
