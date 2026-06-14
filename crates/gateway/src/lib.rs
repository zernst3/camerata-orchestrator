//! camerata-gateway (library): the layer-1 real-time governance gate.
//!
//! This is the residual the verification slice (RUST_CORE_VERIFICATION.md)
//! flagged: the MCP server in `src/main.rs` proved a Rust-owned gate can
//! deny a tool call in-process, but it hard-coded a single rule and had no
//! session → role → rule-subset map. This module supplies that map and
//! implements [`camerata_core::GovernanceGateway`] over it.
//!
//! # Design
//!
//! - [`GovernedGateway`] owns a `SessionId -> Role` map (the role carries the
//!   rule-subset, assigned at spawn). [`GovernedGateway::evaluate`] looks up
//!   the session's role and runs every rule in its subset against the call.
//! - [`evaluate_call`] is the reusable, pure rule-evaluation function. BOTH
//!   the in-process [`GovernedGateway`] and the MCP server (`src/main.rs`)
//!   call it, so the verified transport and the orchestrator share one
//!   gate implementation — no divergence.
//! - Rules are matched by [`camerata_core::RuleId`]. GOV-1 (the verified
//!   "no writes to forbidden paths" rule) is the first concrete rule; adding
//!   more is a match arm in [`apply_rule`].
//!
//! async all the way down (RUST-DOMAIN-5): the trait method is async even
//! though the current rules are synchronous, so a future rule that needs I/O
//! (e.g. a path-boundary check against the filesystem) drops in without an
//! API break.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_core::{Decision, GovernanceGateway, Role, RuleId, SessionId, ToolCall};
use thiserror::Error;

// ─── error type (RUST-DOMAIN-4 / RUST-DOMAIN-6) ──────────────────────────────

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("no role is bound to session {0:?}")]
    UnknownSession(SessionId),
}

// ─── the GOV-1 rule id (verified slice) ──────────────────────────────────────

/// The id of the verified "no writes to forbidden paths" rule.
///
/// Named constructor so no caller hard-codes the string (mirrors
/// camerata-checks' `fmt_rule()` / `clippy_rule()`).
pub fn gov1_rule() -> RuleId {
    RuleId("GOV-1".to_string())
}

// ─── reusable rule-evaluation (pure) ─────────────────────────────────────────

/// Evaluate one tool call against a role's rule-subset and return a verdict.
///
/// This is the single source of truth for layer-1 governance. It is pure:
/// same `(rule_subset, call)` always yields the same [`Decision`]. The MCP
/// server in `src/main.rs` and [`GovernedGateway::evaluate`] both call it.
///
/// Rules fire in subset order; the FIRST rule that denies wins (fail-closed
/// on the first violation, which is also the cheapest to explain in the
/// bounce-back message).
pub fn evaluate_call(rule_subset: &[RuleId], call: &ToolCall) -> Decision {
    for rule in rule_subset {
        if let Some(deny) = apply_rule(rule, call) {
            return deny;
        }
    }
    Decision::Allow
}

/// Apply a single rule to a call. Returns `Some(Decision::Deny{..})` if the
/// rule is violated, `None` if this rule does not object.
///
/// Adding a rule is one match arm. Unknown rule ids are a no-op (the gate is
/// permissive about rules it does not implement yet, NOT about calls).
fn apply_rule(rule: &RuleId, call: &ToolCall) -> Option<Decision> {
    match rule.0.as_str() {
        "GOV-1" => check_gov1(call),
        _ => None,
    }
}

/// GOV-1: deny any write whose target path contains the substring "forbidden".
///
/// This is the exact rule the verification slice proved (see `src/main.rs`),
/// lifted here so the in-process gate and the MCP transport agree byte-for-byte.
fn check_gov1(call: &ToolCall) -> Option<Decision> {
    if !is_write_tool(&call.tool) {
        return None;
    }
    let path = call
        .input
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if path.contains("forbidden") {
        Some(Decision::Deny {
            rule: gov1_rule(),
            reason: format!("GOV-1: writes to forbidden paths are denied (path={path})"),
        })
    } else {
        None
    }
}

/// Whether `tool` is a write the gate must govern. The MCP transport exposes
/// exactly one write tool, `gated_write`; the abstract gate also recognises a
/// bare `write` so the in-process API is not coupled to the MCP tool name.
fn is_write_tool(tool: &str) -> bool {
    matches!(tool, "gated_write" | "write")
}

// ─── GovernedGateway: the session -> role map + GovernanceGateway impl ────────

/// The layer-1 gate the orchestrator holds in-process.
///
/// Owns the `SessionId -> Role` binding assigned when an agent is spawned.
/// Each [`Role`] carries its `rule_subset`; [`evaluate`](Self::evaluate)
/// runs that subset against an attempted [`ToolCall`].
#[derive(Debug, Default, Clone)]
pub struct GovernedGateway {
    sessions: HashMap<SessionId, Role>,
}

impl GovernedGateway {
    /// An empty gateway with no sessions bound.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Bind `session` to `role` (called at agent spawn). Replaces any prior
    /// binding for that session.
    pub fn bind(&mut self, session: SessionId, role: Role) {
        self.sessions.insert(session, role);
    }

    /// Builder form of [`bind`](Self::bind).
    pub fn with_session(mut self, session: SessionId, role: Role) -> Self {
        self.bind(session, role);
        self
    }

    /// The role bound to `session`, if any.
    pub fn role_for(&self, session: &SessionId) -> Option<&Role> {
        self.sessions.get(session)
    }

    /// Number of bound sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Evaluate a call, surfacing the unknown-session case as an error rather
    /// than a silent allow/deny. [`GovernanceGateway::evaluate`] folds the
    /// unknown-session case into a `Deny` (fail-closed) for the trait contract.
    pub fn try_evaluate(
        &self,
        session: &SessionId,
        call: &ToolCall,
    ) -> Result<Decision, GatewayError> {
        let role = self
            .sessions
            .get(session)
            .ok_or_else(|| GatewayError::UnknownSession(session.clone()))?;
        Ok(evaluate_call(&role.rule_subset, call))
    }
}

#[async_trait]
impl GovernanceGateway for GovernedGateway {
    async fn evaluate(&self, session: &SessionId, call: &ToolCall) -> Decision {
        match self.try_evaluate(session, call) {
            Ok(decision) => decision,
            // Fail-closed: an un-bound session means we cannot vouch for the
            // call, so deny it. GOV-1 is the catch-all rule id for "the gate
            // refused".
            Err(GatewayError::UnknownSession(s)) => Decision::Deny {
                rule: gov1_rule(),
                reason: format!("no role bound to session {s:?}; failing closed"),
            },
        }
    }
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_call(path: &str) -> ToolCall {
        ToolCall {
            tool: "gated_write".to_string(),
            input: json!({ "path": path, "content": "x" }),
        }
    }

    fn role_with(rules: &[&str]) -> Role {
        Role {
            name: "Backend".to_string(),
            rule_subset: rules.iter().map(|r| RuleId(r.to_string())).collect(),
            allowed_paths: vec!["crates/".to_string()],
        }
    }

    #[test]
    fn evaluate_call_allows_clean_write() {
        let subset = vec![gov1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/core/src/lib.rs"));
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn evaluate_call_denies_forbidden_write_via_gov1() {
        let subset = vec![gov1_rule()];
        let d = evaluate_call(&subset, &write_call("crates/forbidden/secret.rs"));
        match d {
            Decision::Deny { rule, .. } => assert_eq!(rule, gov1_rule()),
            Decision::Allow => panic!("expected GOV-1 deny"),
        }
    }

    #[test]
    fn evaluate_call_without_gov1_in_subset_allows_forbidden() {
        // If the role's subset does not include GOV-1, the rule does not apply.
        let subset = vec![RuleId("SOME-OTHER-RULE".to_string())];
        let d = evaluate_call(&subset, &write_call("crates/forbidden/x.rs"));
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn evaluate_call_ignores_non_write_tools() {
        let subset = vec![gov1_rule()];
        let call = ToolCall {
            tool: "read".to_string(),
            input: json!({ "path": "crates/forbidden/x.rs" }),
        };
        assert!(matches!(evaluate_call(&subset, &call), Decision::Allow));
    }

    #[tokio::test]
    async fn governed_gateway_denies_planted_violation() {
        let session = SessionId("sess-1".to_string());
        let gw = GovernedGateway::new().with_session(session.clone(), role_with(&["GOV-1"]));

        let denied = gw
            .evaluate(&session, &write_call("crates/forbidden/leak.rs"))
            .await;
        match denied {
            Decision::Deny { rule, reason } => {
                assert_eq!(rule, gov1_rule());
                assert!(reason.contains("GOV-1"));
            }
            Decision::Allow => panic!("planted violation should be denied"),
        }

        let allowed = gw
            .evaluate(&session, &write_call("crates/core/src/ok.rs"))
            .await;
        assert!(matches!(allowed, Decision::Allow));
    }

    #[tokio::test]
    async fn governed_gateway_fails_closed_on_unknown_session() {
        let gw = GovernedGateway::new();
        let unknown = SessionId("ghost".to_string());
        let d = gw.evaluate(&unknown, &write_call("crates/core/ok.rs")).await;
        assert!(matches!(d, Decision::Deny { .. }), "unbound session must fail closed");
    }

    #[test]
    fn try_evaluate_surfaces_unknown_session_error() {
        let gw = GovernedGateway::new();
        let err = gw
            .try_evaluate(&SessionId("ghost".into()), &write_call("x"))
            .unwrap_err();
        assert!(matches!(err, GatewayError::UnknownSession(_)));
    }

    #[test]
    fn bind_and_role_for_roundtrip() {
        let mut gw = GovernedGateway::new();
        let s = SessionId("s".into());
        gw.bind(s.clone(), role_with(&["GOV-1"]));
        assert_eq!(gw.session_count(), 1);
        assert_eq!(gw.role_for(&s).unwrap().name, "Backend");
    }
}
