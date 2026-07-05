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
use camerata_gateway::{enforced_gate_rules, evaluate_call, gov1_rule, GovernedGateway};

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
            let mut c = self.calls.lock().expect("gate-probe mutex poisoned");
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
            let mut c = self.checks.lock().expect("gate-probe mutex poisoned");
            *c += 1;
            *c
        };
        Ok(if n == 1 { vec![gov1_rule()] } else { vec![] })
    }
}

/// One layer-1 planted-violation check: which floor rule it targets, and whether the gate denied it.
#[derive(Debug, Clone)]
pub struct Layer1Check {
    /// The floor rule id this violation is planted to exercise (e.g. "GOV-1").
    pub rule: String,
    /// Human label for the planted violation (e.g. "forbidden path", "hardcoded secret").
    pub label: String,
    /// Whether the FULL enforced gate denied it (every planted violation MUST be denied).
    pub denied: bool,
    /// Whether the TARGETED arm, evaluated in isolation, denied with exactly `rule`.
    ///
    /// The full gate returns the FIRST matching rule, so a redundant earlier rule can mask a
    /// later one (e.g. the broad hardcoded-secrets arm also catches a PEM header, masking the
    /// dedicated private-key arm). Evaluating the single targeted rule proves that arm itself
    /// fires — so a silently-broken arm cannot hide behind a redundant neighbour.
    pub isolated_denied: bool,
    /// The denial's "[RULE] reason", or the unexpected-allow note.
    pub detail: String,
}

/// The probe's verdict across both gate layers.
#[derive(Debug)]
pub struct GateProbeResult {
    pub story: String,
    /// LAYER 1 — one entry per enforced floor rule; ALL must be `denied` and `isolated_denied`.
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
    /// How many targeted arms fired in isolation (proving no arm is silently broken/masked).
    pub fn layer1_isolated_count(&self) -> usize {
        self.layer1.iter().filter(|c| c.isolated_denied).count()
    }

    /// GO iff: EVERY enforced floor rule denied its planted violation (both under the full gate
    /// AND in isolation), the clean write was allowed, AND the layer-2 loop bounced once and
    /// resolved. Anything else is NO-GO (the gate isn't fully wired).
    pub fn go(&self) -> bool {
        !self.layer1.is_empty()
            && self.layer1.iter().all(|c| c.denied && c.isolated_denied)
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
    //
    // The planted set is keyed by rule id and MUST cover every arm in `RULE_REGISTRY`
    // (asserted below) so the probe can never silently drift to a subset as arms are added.
    // Each violation is minimal on every dimension except the one it targets, and lives on a
    // non-test path so the test-scope Waive policy cannot suppress it. Content-redundant rules
    // (e.g. the broad hardcoded-secrets arm also matches a PEM header, and SEC-NO-SECRET-FILE-1
    // is a subset of SEC-NO-SECRET-FILES-1) are still fully proven via the per-arm isolation
    // check, even though the full gate masks them behind an earlier redundant rule.
    let gateway = GovernedGateway::new().with_session(session.clone(), role.clone());
    let planted: Vec<(&str, &str, ToolCall)> = vec![
        (
            "GOV-1",
            "forbidden path",
            write_call("crates/forbidden/leak.rs", "// agent tried to write here"),
        ),
        (
            "SEC-NO-HARDCODED-SECRETS-1",
            "hardcoded GitHub token",
            write_call(
                "crates/core/src/config.rs",
                "let t = \"ghp_ABCDEFGHIJ1234567890abcdefghij12\";",
            ),
        ),
        (
            "SEC-NO-RAW-SQL-CONCAT-1",
            "raw SQL concat",
            write_call(
                "crates/core/src/db.rs",
                "let q = \"SELECT * FROM users WHERE id = {}\";",
            ),
        ),
        (
            "ARCH-NO-SECRETS-IN-URL-1",
            "secret in URL",
            write_call(
                "crates/core/src/api.rs",
                "let e = \"https://api.example.com/data?access_token=abc123def456ghi789\";",
            ),
        ),
        (
            "SEC-NO-PATH-ESCAPE-1",
            "path escape",
            write_call("crates/../../etc/cron.d/payload", "harmless content"),
        ),
        (
            "SEC-NO-SECRET-FILES-1",
            "secret file (.env)",
            write_call("crates/core/.env", "API_SECRET=supersecretvalue"),
        ),
        (
            "SEC-NO-PRIVATE-KEY-1",
            "PEM private key block",
            write_call(
                "crates/core/src/keys.rs",
                "const K: &str = \"-----BEGIN RSA PRIVATE KEY-----\";",
            ),
        ),
        (
            "SEC-NO-VENDOR-TOKEN-1",
            "vendor credential token",
            write_call(
                "crates/core/src/vendor.rs",
                "let k = \"sk-ant-api03-ABCDEFGHIJKLMNOPQRST\";",
            ),
        ),
        (
            "SEC-NO-SECRET-FILE-1",
            "secret file type (.p12)",
            write_call("crates/core/tls/prod.p12", "binary keystore bytes"),
        ),
        (
            "SEC-NO-DISABLED-TLS-1",
            "disabled TLS verification",
            write_call("crates/core/src/http.rs", "requests.get(url, verify=False)"),
        ),
        (
            "SEC-NO-UNSAFE-DESERIALIZATION-1",
            "unsafe deserialization",
            write_call("crates/core/src/loader.rs", "data = pickle.loads(untrusted)"),
        ),
        (
            "SEC-NO-CAMERATA-CONFIG-1",
            "governance config write",
            write_call("crates/core/.camerata/checks.toml", "[gate]\ndisabled = true"),
        ),
        (
            "SEC-NO-GIT-STATE-MUTATION-1",
            "git state mutation",
            write_call("crates/core/scripts/reset.sh", "git reset --hard HEAD"),
        ),
    ];
    let mut layer1 = Vec::with_capacity(planted.len());
    for (rule_id, label, call) in &planted {
        let (denied, detail) = match gateway.evaluate(&session, call).await {
            Decision::Deny { rule, reason } => (true, format!("[{}] {reason}", rule.0)),
            Decision::Allow => (
                false,
                "ALLOWED — this floor rule is not wired on writes".to_string(),
            ),
        };
        // Per-arm isolation: evaluate ONLY the targeted rule, so a redundant earlier rule
        // cannot mask a broken later arm. The single rule must deny with its own id.
        let isolated_denied = matches!(
            evaluate_call(&[RuleId(rule_id.to_string())], call),
            Decision::Deny { rule, .. } if rule.0 == *rule_id
        );
        layer1.push(Layer1Check {
            rule: rule_id.to_string(),
            label: label.to_string(),
            denied,
            isolated_denied,
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
    let agent_passes = *driver.calls.lock().expect("gate-probe mutex poisoned");

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
        // LAYER 1: the probe covers EVERY enforced floor rule — one planted violation per
        // arm in RULE_REGISTRY, so it can never silently drift to a subset.
        assert_eq!(
            r.layer1_total(),
            camerata_gateway::RULE_REGISTRY.len(),
            "the probe must plant one violation per enforced floor rule"
        );
        let planted_rules: std::collections::BTreeSet<&str> =
            r.layer1.iter().map(|c| c.rule.as_str()).collect();
        for entry in camerata_gateway::RULE_REGISTRY {
            assert!(
                planted_rules.contains(entry.id),
                "no planted violation for enforced rule {}",
                entry.id
            );
        }
        // The whole floor denied every planted violation, both under the full gate and in
        // isolation (so a redundant earlier rule cannot mask a silently-broken later arm).
        for c in &r.layer1 {
            assert!(
                c.denied,
                "planted violation must be denied: {} ({}) — {}",
                c.label, c.rule, c.detail
            );
            assert!(
                c.isolated_denied,
                "arm {} must deny its planted violation in isolation ({})",
                c.rule, c.label
            );
        }
        assert_eq!(
            r.layer1_denied_count(),
            r.layer1_total(),
            "every planted floor violation must be denied by the full gate"
        );
        assert_eq!(
            r.layer1_isolated_count(),
            r.layer1_total(),
            "every targeted arm must fire in isolation"
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
