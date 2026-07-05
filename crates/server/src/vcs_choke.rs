//! The commit / PR / branch chokepoint gate (GAP-2).
//!
//! The VCS-action gate engine lives in `camerata_checks::vcs_action` (`build_rules`,
//! `evaluate`, `gate`, `gate_or_bypass`). Before GAP-2 its ONLY production caller was the
//! manual bypass endpoint, so a project could configure process rules (conventional-commit
//! shape, a required story-id, an `AB#<id>` link, branch-naming) and have them silently
//! NOT enforced on any real commit or PR the server performed.
//!
//! This module is the single shared choke every server-side VCS action funnels through. It
//! builds the project's live rule set from its [`ProcessRuleConfig`] and runs the
//! deterministic gate BEFORE the git action is taken. On a violation it returns
//! [`ChokeError::Blocked`] (HARD-BLOCK): the caller aborts the action — no commit, no PR, no
//! branch. There is no "warn and continue"; a process gate that warns is a linter, not a gate.
//!
//! ## Two entry points
//!
//! - [`gated_commit`] / [`gated_pr`] / [`gated_branch`] HARD-BLOCK on violation. Use these
//!   at the real chokepoints for actions whose metadata a human (or the fleet) authored and
//!   is expected to satisfy the project's conventions.
//! - [`gated_commit_or_bypass`] wraps `vcs_action::gate_or_bypass` for orchestration-internal
//!   actions that legitimately cannot satisfy the rule (e.g. a machine-generated merge commit)
//!   and carry an auditable, non-empty bypass reason. A reason-less bypass is itself rejected.
//!
//! Keying: the caller passes the project's `&ProcessRuleConfig`. When no project is active /
//! resolvable, the caller passes `ProcessRuleConfig::default()`, whose default rule set is
//! the conventional-commit shape only (branch-naming / ADO-link are opt-in, disabled by
//! default), so the gate stays quiet for un-configured projects while still enforcing the
//! baseline.

use camerata_checks::vcs_action::{
    build_rules, gate, gate_or_bypass, BypassRequest, GateOrBypassResult, ProcessRuleConfig,
    ProcessViolation, VcsAction,
};

/// The outcome of a blocked chokepoint check.
#[derive(Debug)]
pub enum ChokeError {
    /// The action violated one or more process rules. The action MUST be aborted.
    Blocked(Vec<ProcessViolation>),
    /// A bypass was requested without a non-empty reason (only from the `_or_bypass` path).
    BypassReasonRequired,
}

impl std::fmt::Display for ChokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChokeError::Blocked(violations) => {
                writeln!(
                    f,
                    "VCS-action gate blocked this action ({} process-rule violation(s)):",
                    violations.len()
                )?;
                for v in violations {
                    writeln!(f, "  - {}", v.detail)?;
                }
                write!(
                    f,
                    "Fix the metadata to satisfy the project's conventions, or use the \
                     bypass endpoint with an auditable reason."
                )
            }
            ChokeError::BypassReasonRequired => write!(
                f,
                "bypass rejected: a non-empty reason is required (mirror of the \
                 suppression-waiver invariant)"
            ),
        }
    }
}

impl std::error::Error for ChokeError {}

/// Gate a COMMIT on its message. HARD-BLOCK: `Err(ChokeError::Blocked)` on any violation.
///
/// Call this immediately before performing the commit; abort the commit on `Err`.
pub fn gated_commit(config: &ProcessRuleConfig, message: &str) -> Result<(), ChokeError> {
    let action = VcsAction::Commit {
        message: message.to_string(),
    };
    run_gate(config, &action)
}

/// Gate a PULL REQUEST on its title + body. HARD-BLOCK on any violation.
pub fn gated_pr(config: &ProcessRuleConfig, title: &str, body: &str) -> Result<(), ChokeError> {
    let action = VcsAction::PullRequest {
        title: title.to_string(),
        body: body.to_string(),
    };
    run_gate(config, &action)
}

/// Gate a BRANCH creation on its name. HARD-BLOCK on any violation.
///
/// Branch-naming is opt-in (disabled by default), so for most projects this is a no-op.
pub fn gated_branch(config: &ProcessRuleConfig, name: &str) -> Result<(), ChokeError> {
    let action = VcsAction::Branch {
        name: name.to_string(),
    };
    run_gate(config, &action)
}

/// Gate a COMMIT, allowing an auditable bypass for orchestration-internal commits that
/// legitimately cannot satisfy the rule (e.g. a machine-generated merge commit).
///
/// - `reason = None` — identical to [`gated_commit`] (HARD-BLOCK on violation).
/// - `reason = Some(non-empty)` — if the gate would fail, the action is allowed and the
///   returned `Some(record_summary)` describes the suppressed rules for the evidence trail.
///   When the action passes, `Ok(None)` is returned (no bypass consumed).
/// - `reason = Some(empty)` — `Err(ChokeError::BypassReasonRequired)`.
pub fn gated_commit_or_bypass(
    config: &ProcessRuleConfig,
    message: &str,
    reason: Option<&str>,
) -> Result<Option<String>, ChokeError> {
    let action = VcsAction::Commit {
        message: message.to_string(),
    };
    let rules = build_rules(config);
    let bypass = reason.map(|r| BypassRequest {
        reason: r.to_string(),
    });
    match gate_or_bypass(&rules, &action, bypass.as_ref()) {
        Ok(GateOrBypassResult::Passed) => Ok(None),
        Ok(GateOrBypassResult::Bypassed(record)) => Ok(Some(format!(
            "bypassed [{}]: {}",
            record.suppressed_rule_ids.join(", "),
            record.reason
        ))),
        Ok(GateOrBypassResult::Failed(violations)) => Err(ChokeError::Blocked(violations)),
        Err(_) => Err(ChokeError::BypassReasonRequired),
    }
}

/// Gate a PULL REQUEST, allowing an auditable bypass for orchestration-internal PRs whose
/// title/body are machine-generated (e.g. the onboarding governance PR). Same semantics as
/// [`gated_commit_or_bypass`]: `Ok(None)` when the action passes, `Ok(Some(summary))` when a
/// reasoned bypass is recorded, `Err(ChokeError::Blocked)` on a violation with no bypass,
/// `Err(ChokeError::BypassReasonRequired)` on a reason-less bypass.
pub fn gated_pr_or_bypass(
    config: &ProcessRuleConfig,
    title: &str,
    body: &str,
    reason: Option<&str>,
) -> Result<Option<String>, ChokeError> {
    let action = VcsAction::PullRequest {
        title: title.to_string(),
        body: body.to_string(),
    };
    let rules = build_rules(config);
    let bypass = reason.map(|r| BypassRequest {
        reason: r.to_string(),
    });
    match gate_or_bypass(&rules, &action, bypass.as_ref()) {
        Ok(GateOrBypassResult::Passed) => Ok(None),
        Ok(GateOrBypassResult::Bypassed(record)) => Ok(Some(format!(
            "bypassed [{}]: {}",
            record.suppressed_rule_ids.join(", "),
            record.reason
        ))),
        Ok(GateOrBypassResult::Failed(violations)) => Err(ChokeError::Blocked(violations)),
        Err(_) => Err(ChokeError::BypassReasonRequired),
    }
}

/// Shared inner: build the project's rules and run the deterministic gate.
fn run_gate(config: &ProcessRuleConfig, action: &VcsAction) -> Result<(), ChokeError> {
    let rules = build_rules(config);
    gate(&rules, action).map_err(ChokeError::Blocked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_checks::vcs_action::{CommitDocConfig, ConventionalCommitConfig};

    /// A config that enforces conventional-commit shape but NOT the doc/story-id rule, so
    /// tests can exercise the subject-shape gate in isolation.
    fn conventional_only() -> ProcessRuleConfig {
        ProcessRuleConfig {
            commit_doc: CommitDocConfig {
                enabled: false,
                ..CommitDocConfig::default()
            },
            conventional_commit: ConventionalCommitConfig::default(),
            ..ProcessRuleConfig::default()
        }
    }

    #[test]
    fn compliant_commit_passes() {
        let cfg = conventional_only();
        assert!(gated_commit(&cfg, "feat: add the export endpoint").is_ok());
    }

    #[test]
    fn violating_commit_is_hard_blocked() {
        let cfg = conventional_only();
        let err = gated_commit(&cfg, "just did some stuff").expect_err("must block");
        match err {
            ChokeError::Blocked(v) => {
                assert!(
                    v.iter().any(|x| x.rule_id == "PROCESS-CONVENTIONAL-COMMIT-1"),
                    "conventional-commit rule must fire: {v:?}"
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn default_config_enforces_conventional_shape() {
        // The shipped default has conventional_commit enabled + commit_doc enabled, so a
        // bare-junk subject with no body is blocked out of the box.
        let cfg = ProcessRuleConfig::default();
        assert!(gated_commit(&cfg, "nonsense").is_err(), "default config must gate");
    }

    #[test]
    fn doc_rule_default_requires_body_and_story_id() {
        // Default config's PROCESS-COMMIT-DOC-1 requires a substantive body + bare #<id>.
        let cfg = ProcessRuleConfig::default();
        // Conventional shape but subject-only -> blocked (no body / no story id).
        assert!(gated_commit(&cfg, "feat: add export").is_err());
        // Conventional shape + substantive body + story id -> passes.
        let ok_msg = "feat: add export\n\nImplements the CSV export flow end to end. Refs #42.";
        assert!(gated_commit(&cfg, ok_msg).is_ok(), "compliant commit must pass");
    }

    #[test]
    fn pr_gate_blocks_and_passes() {
        let cfg = ProcessRuleConfig::default();
        // Empty body PR -> blocked by the doc rule on PrBody.
        assert!(gated_pr(&cfg, "Add export endpoint", "").is_err());
        // Substantive PR body with a story id -> passes.
        assert!(gated_pr(
            &cfg,
            "Add export endpoint",
            "Implements the CSV export feature end to end. Closes #99."
        )
        .is_ok());
    }

    #[test]
    fn branch_gate_is_noop_by_default() {
        // branch_naming is opt-in (disabled by default): any branch name passes.
        let cfg = ProcessRuleConfig::default();
        assert!(gated_branch(&cfg, "my-random-branch").is_ok());
    }

    #[test]
    fn bypass_with_reason_allows_a_violating_commit() {
        let cfg = conventional_only();
        // Violating message, but an auditable reason bypasses it.
        let record = gated_commit_or_bypass(
            &cfg,
            "machine merge commit",
            Some("machine-generated merge commit from the rebase pipeline"),
        )
        .expect("bypass must succeed with a reason");
        let summary = record.expect("a bypass record is produced for a violating action");
        assert!(summary.contains("PROCESS-CONVENTIONAL-COMMIT-1"), "record names the rule: {summary}");
    }

    #[test]
    fn bypass_without_reason_is_rejected() {
        let cfg = conventional_only();
        let err = gated_commit_or_bypass(&cfg, "bad subject", Some(""))
            .expect_err("empty reason must be rejected");
        assert!(matches!(err, ChokeError::BypassReasonRequired));
    }

    #[test]
    fn bypass_none_on_passing_action_consumes_no_bypass() {
        let cfg = conventional_only();
        let record = gated_commit_or_bypass(&cfg, "feat: fine", None)
            .expect("a compliant commit passes");
        assert!(record.is_none(), "no bypass record for a passing action");
    }
}
