//! Planted-violation acceptance run — the in-process proof that the engine is
//! wired together end-to-end with NO network and NO live `claude` call.
//!
//! It constructs the real layer-1 gate ([`camerata_gateway::GovernedGateway`]),
//! a fake/echo [`AgentDriver`], and a checks runner, then asserts the gate denies
//! three planted violations across three different enforced rules, while allowing
//! a clean control write. The role carries the FULL enforced rule set
//! ([`enforced_gate_rules`], the same set the live fleet rides along with):
//!   - GOV-1: a write to a path containing "forbidden" (the verified slice's deny).
//!   - SEC-NO-PATH-ESCAPE-1: a `..` traversal that climbs out of the workspace.
//!   - SEC-NO-HARDCODED-SECRETS-1: a hardcoded credential literal in the content.
//!
//! This is the engine-wiring acceptance test, runnable in CI with no model. The
//! live end-to-end run (a real `claude -p` subprocess through the MCP gateway) is
//! covered separately by the `live-demo` binary; everything up to that boundary is
//! exercised here in-process.

use camerata_agent::allowed_tools_for_role;
use camerata_core::{
    AgentDriver, AgentOutcome, CheckRunner, Decision, GovernanceGateway, Role, RuleId, SessionId,
    ToolCall,
};
use camerata_gateway::{enforced_gate_rules, GovernedGateway};
use std::path::Path;

/// A fake agent driver that echoes the task back as its result and makes NO
/// model call. Stands in for `ClaudeCliDriver` so the acceptance run is
/// hermetic.
pub struct EchoDriver;

#[async_trait::async_trait]
impl AgentDriver for EchoDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        Ok(AgentOutcome {
            session_id: format!("echo-{}", role.name.to_lowercase()),
            result: format!("echo: {task}"),
            cost_usd: Some(0.0),
            denials: vec![],
        })
    }
}

/// A checks runner that reports no structural violations — the acceptance run
/// exercises the LAYER-1 gate deny, not the layer-2 check path (the coordinator
/// tests cover that). Keeps the engine wiring honest without spawning cargo.
pub struct NoopChecks;

#[async_trait::async_trait]
impl CheckRunner for NoopChecks {
    async fn check(&self, _role: &Role, _worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        Ok(vec![])
    }
}

/// The Backend role used in the acceptance run: its rule-subset is the FULL set
/// of enforced gate rules (the same set the live fleet rides along with), so the
/// gateway enforces every layer-1 rule against it, not just GOV-1.
pub fn backend_role() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: enforced_gate_rules(),
        allowed_paths: vec!["crates/".to_string()],
    }
}

/// Result of the acceptance run: the verdict on the planted violation and the
/// verdict on a clean control write, so the caller can assert BOTH directions.
#[derive(Debug)]
pub struct AcceptanceResult {
    pub planted_violation_decision: Decision,
    /// Verdict on a planted `..` traversal write (SEC-NO-PATH-ESCAPE-1).
    pub planted_path_escape_decision: Decision,
    /// Verdict on a planted hardcoded-secret write (SEC-NO-HARDCODED-SECRETS-1).
    pub planted_secret_decision: Decision,
    pub clean_control_decision: Decision,
    /// The fake agent's echoed outcome, proving the driver was wired in.
    pub agent_session_id: String,
    /// The allowedTools the role would run under (proving role → tools wiring).
    pub allowed_tools: Vec<String>,
}

impl AcceptanceResult {
    /// The acceptance criterion: every planted violation DENIED, control ALLOWED.
    pub fn passed(&self) -> bool {
        matches!(self.planted_violation_decision, Decision::Deny { .. })
            && matches!(self.planted_path_escape_decision, Decision::Deny { .. })
            && matches!(self.planted_secret_decision, Decision::Deny { .. })
            && matches!(self.clean_control_decision, Decision::Allow)
    }
}

/// Run the planted-violation acceptance scenario in-process.
///
/// Wires: GovernedGateway (real layer-1 gate) + EchoDriver (fake agent) +
/// NoopChecks (layer-2 runner). Binds a session to the Backend role, runs the
/// echo agent, then asks the gate to evaluate three planted violations (forbidden
/// path, `..` traversal, hardcoded secret) and one clean control write.
pub async fn run_acceptance() -> anyhow::Result<AcceptanceResult> {
    let role = backend_role();
    let session = SessionId("acceptance-session".to_string());

    // Layer-1 gate with the session bound to the Backend role (this is the
    // residual the verification flagged — the session → role → rule-subset map).
    let gateway = GovernedGateway::new().with_session(session.clone(), role.clone());

    // Fake agent + checks, proving the seams are injectable without a model.
    let driver = EchoDriver;
    let checks = NoopChecks;
    let agent_outcome = driver.run(&role, "implement crates/core feature").await?;
    let _ = checks.check(&role, Path::new("/tmp/acceptance-wt")).await?;

    // Planted violation: a write to a "forbidden" path (mirrors the verified
    // slice's GOV-1 deny).
    let planted = ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/forbidden/secrets.rs",
            "content": "leak"
        }),
    };
    let planted_violation_decision = gateway.evaluate(&session, &planted).await;

    // Planted path-escape: a `..` traversal that climbs out of the workspace
    // (SEC-NO-PATH-ESCAPE-1). Clean of any "forbidden" substring, so it proves a
    // DIFFERENT rule fires through the real gateway path, not just GOV-1.
    let path_escape = ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/../../etc/cron.d/payload",
            "content": "*/1 * * * * root sh -c id"
        }),
    };
    let planted_path_escape_decision = gateway.evaluate(&session, &path_escape).await;

    // Planted secret: a clean path but a hardcoded credential in the content
    // (SEC-NO-HARDCODED-SECRETS-1), proving a content rule fires end-to-end.
    let secret = ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/core/src/config.rs",
            "content": "let token = \"ghp_ABCDEFGHIJ1234567890abcdefghij12\";"
        }),
    };
    let planted_secret_decision = gateway.evaluate(&session, &secret).await;

    // Clean control: a write to a legitimate path with benign content must be
    // allowed even with ALL rules active (the gate is not deny-everything).
    let control = ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/core/src/feature.rs",
            "content": "ok"
        }),
    };
    let clean_control_decision = gateway.evaluate(&session, &control).await;

    Ok(AcceptanceResult {
        planted_violation_decision,
        planted_path_escape_decision,
        planted_secret_decision,
        clean_control_decision,
        agent_session_id: agent_outcome.session_id,
        allowed_tools: allowed_tools_for_role(&role),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_gateway::{gov1_rule, sec_no_hardcoded_secrets_1_rule, sec_no_path_escape_1_rule};

    #[tokio::test]
    async fn acceptance_gate_denies_planted_violation_and_allows_control() {
        let result = run_acceptance()
            .await
            .expect("acceptance run should complete");

        // The core assertion: the gate DENIED the planted forbidden write.
        match &result.planted_violation_decision {
            Decision::Deny { rule, reason } => {
                assert_eq!(*rule, gov1_rule(), "denied by GOV-1");
                assert!(reason.contains("GOV-1"));
            }
            Decision::Allow => panic!("planted violation was NOT denied — gate is not wired"),
        }

        // A DIFFERENT rule fires end-to-end: the `..` traversal is denied by
        // SEC-NO-PATH-ESCAPE-1, proving more than GOV-1 rides the gateway path.
        match &result.planted_path_escape_decision {
            Decision::Deny { rule, reason } => {
                assert_eq!(
                    *rule,
                    sec_no_path_escape_1_rule(),
                    "denied by path-escape rule"
                );
                assert!(reason.contains("SEC-NO-PATH-ESCAPE-1"));
            }
            Decision::Allow => panic!("path-escape violation was NOT denied"),
        }

        // A content rule fires end-to-end: the hardcoded credential is denied by
        // SEC-NO-HARDCODED-SECRETS-1.
        match &result.planted_secret_decision {
            Decision::Deny { rule, reason } => {
                assert_eq!(
                    *rule,
                    sec_no_hardcoded_secrets_1_rule(),
                    "denied by secrets rule"
                );
                assert!(reason.contains("SEC-NO-HARDCODED-SECRETS-1"));
            }
            Decision::Allow => panic!("hardcoded-secret violation was NOT denied"),
        }

        // The control write was allowed (the gate is not deny-everything, even
        // with all rules active).
        assert!(matches!(result.clean_control_decision, Decision::Allow));

        // Whole-scenario predicate.
        assert!(result.passed(), "acceptance run must pass");

        // The fake agent was wired in (driver seam exercised).
        assert_eq!(result.agent_session_id, "echo-backend");

        // The role → allowedTools wiring is present (agent seam exercised).
        assert!(result
            .allowed_tools
            .iter()
            .any(|t| t == camerata_agent::GATED_WRITE_TOOL));
    }
}
