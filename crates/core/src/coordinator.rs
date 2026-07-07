//! The Coordinator: pure orchestration of one role + task through the
//! agent-runtime and layer-2 check seams, with a single bounce-and-revise pass.
//!
//! # Contract
//!
//! Given a [`Role`] and a task string, the coordinator:
//! 1. runs the injected [`AgentDriver`] (the model call — NOT made here; the
//!    driver is injected, so core stays model-free per the seam design),
//! 2. runs the injected [`CheckRunner`] against the worktree,
//! 3. if any [`RuleId`] is reported violated, performs ONE bounce-and-revise
//!    pass: it re-runs the agent with the violated rule ids appended to the
//!    task, then re-checks.
//!
//! It bounces AT MOST once. A rule still violated after the revise pass is
//! reported as a residual in [`RunReport::final_violations`]; escalation /
//! human-in-the-loop is the caller's policy, not the coordinator's.
//!
//! This module makes ZERO model calls itself — every model interaction goes
//! through the injected `AgentDriver`. That is what keeps the brain
//! deterministic and unit-testable with a fake driver (see the CLI acceptance
//! scaffold and the tests below).

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{AgentDriver, AgentOutcome, CheckRunner, Role, RuleId};

/// Errors the coordinator surfaces. The driver and check runner return
/// `anyhow::Error`; we wrap them so a caller can tell which seam failed.
#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("agent driver failed on the {pass} pass: {source}")]
    Driver {
        pass: &'static str,
        #[source]
        source: anyhow::Error,
    },

    #[error("check runner failed on the {pass} pass: {source}")]
    Check {
        pass: &'static str,
        #[source]
        source: anyhow::Error,
    },
}

/// The outcome of a coordinated run: every agent pass it took, the violations
/// found at each stage, and whether the bounce-and-revise pass ran.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// Outcome of the initial agent run.
    pub initial_outcome: AgentOutcome,
    /// Violations the check runner found after the initial run.
    pub initial_violations: Vec<RuleId>,
    /// Outcome of the revise run, if a bounce occurred.
    pub revised_outcome: Option<AgentOutcome>,
    /// Violations remaining after all passes. Empty == clean.
    pub final_violations: Vec<RuleId>,
    /// Whether the single bounce-and-revise pass was performed.
    pub bounced: bool,
}

impl RunReport {
    /// Whether the run ended clean (no residual violations).
    pub fn is_clean(&self) -> bool {
        self.final_violations.is_empty()
    }
}

/// Orchestrates one role + task end-to-end. Holds borrowed seam implementations
/// so the same coordinator can drive many tasks; the driver and check runner
/// are injected (dependency inversion — core never names a concrete model).
pub struct Coordinator<'a> {
    driver: &'a dyn AgentDriver,
    checks: &'a dyn CheckRunner,
    worktree: PathBuf,
}

impl<'a> Coordinator<'a> {
    /// Build a coordinator over an injected driver + check runner, scoped to
    /// `worktree` (the directory the agent and checks operate on).
    pub fn new(
        driver: &'a dyn AgentDriver,
        checks: &'a dyn CheckRunner,
        worktree: impl Into<PathBuf>,
    ) -> Self {
        Self {
            driver,
            checks,
            worktree: worktree.into(),
        }
    }

    /// The worktree this coordinator operates on.
    pub fn worktree(&self) -> &Path {
        &self.worktree
    }

    /// Run `task` for `role`: agent → check → (bounce-and-revise once if dirty).
    pub async fn run(&self, role: &Role, task: &str) -> Result<RunReport, CoordinatorError> {
        // 1. Initial agent pass (model call lives behind the driver).
        let initial_outcome =
            self.driver
                .run(role, task)
                .await
                .map_err(|source| CoordinatorError::Driver {
                    pass: "initial",
                    source,
                })?;

        // 2. Layer-2 check.
        let initial_check =
            self.checks
                .check(role, &self.worktree)
                .await
                .map_err(|source| CoordinatorError::Check {
                    pass: "initial",
                    source,
                })?;
        let initial_violations = initial_check.violated;

        // Clean on the first pass — no bounce.
        if initial_violations.is_empty() {
            return Ok(RunReport {
                final_violations: vec![],
                bounced: false,
                revised_outcome: None,
                initial_outcome,
                initial_violations,
            });
        }

        // 3. ONE bounce-and-revise pass: append the violated rule ids AND the
        //    captured toolchain diagnostics to the task so the agent knows exactly
        //    what to fix, then re-run + re-check.
        let revise_task =
            build_revise_task(task, &initial_violations, &initial_check.diagnostics);
        let revised_outcome = self
            .driver
            .run(role, &revise_task)
            .await
            .map_err(|source| CoordinatorError::Driver {
                pass: "revise",
                source,
            })?;

        let final_violations = self
            .checks
            .check(role, &self.worktree)
            .await
            .map_err(|source| CoordinatorError::Check {
                pass: "revise",
                source,
            })?
            .violated;

        Ok(RunReport {
            initial_outcome,
            initial_violations,
            revised_outcome: Some(revised_outcome),
            final_violations,
            bounced: true,
        })
    }
}

/// Construct the revise-pass task by appending the violated rule ids to the
/// original task. Pure + testable. The format is deliberately explicit so the
/// agent gets the rule ids verbatim (PROC-CITE-CONVENTION-ID-1 in spirit: the
/// bounce-back cites the exact rule ids).
pub fn build_revise_task(original: &str, violated: &[RuleId], diagnostics: &str) -> String {
    let ids = violated
        .iter()
        .map(|r| r.0.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // Cache-friendly TAIL placement: `original` is the stable, cached prefix;
    // the rule ids + verbatim toolchain diagnostics are the new delta appended at
    // the end. The diagnostics are the raw error text a literal open-weight model
    // needs to self-correct — the rule id alone is not enough. Emit them last so
    // the failing assertion / final error summary is the most recent context.
    let diag = diagnostics.trim();
    if diag.is_empty() {
        format!(
            "{original}\n\n\
             REVISION REQUIRED: your previous output violated these rules: [{ids}].\n\
             Fix every listed violation and produce a compliant result."
        )
    } else {
        format!(
            "{original}\n\n\
             REVISION REQUIRED: your previous output violated these rules: [{ids}].\n\
             Fix every listed violation and produce a compliant result.\n\n\
             Verbatim toolchain diagnostics from the failed checks (authoritative — \
             fix the ROOT cause these describe):\n{diag}"
        )
    }
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentOutcome;
    use std::sync::Mutex;

    fn role() -> Role {
        Role {
            name: "Backend".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/core".to_string()],
        }
    }

    fn outcome(session: &str) -> AgentOutcome {
        AgentOutcome {
            session_id: session.to_string(),
            result: "ok".to_string(),
            cost_usd: Some(0.0),
            denials: vec![],
        }
    }

    /// Records each task it was asked to run; returns a fixed outcome.
    struct RecordingDriver {
        tasks: Mutex<Vec<String>>,
    }
    impl RecordingDriver {
        fn new() -> Self {
            Self {
                tasks: Mutex::new(vec![]),
            }
        }
    }
    #[async_trait::async_trait]
    impl AgentDriver for RecordingDriver {
        async fn run(&self, _role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
            self.tasks.lock().unwrap().push(task.to_string());
            Ok(outcome("sess"))
        }
    }

    /// Returns a scripted sequence of violation-sets, one per check call.
    struct ScriptedChecks {
        scripted: Mutex<std::collections::VecDeque<Vec<RuleId>>>,
    }
    impl ScriptedChecks {
        fn new(seq: Vec<Vec<RuleId>>) -> Self {
            Self {
                scripted: Mutex::new(seq.into_iter().collect()),
            }
        }
    }
    #[async_trait::async_trait]
    impl CheckRunner for ScriptedChecks {
        async fn check(&self, _role: &Role, _wt: &Path) -> anyhow::Result<crate::CheckOutcome> {
            let violated = self
                .scripted
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(crate::CheckOutcome::new(violated, ""))
        }
    }

    #[tokio::test]
    async fn clean_first_pass_does_not_bounce() {
        let driver = RecordingDriver::new();
        let checks = ScriptedChecks::new(vec![vec![]]); // clean immediately
        let coord = Coordinator::new(&driver, &checks, "/tmp/wt");

        let report = coord.run(&role(), "build the thing").await.unwrap();

        assert!(!report.bounced);
        assert!(report.is_clean());
        assert!(report.revised_outcome.is_none());
        assert_eq!(
            driver.tasks.lock().unwrap().len(),
            1,
            "agent ran exactly once"
        );
    }

    #[tokio::test]
    async fn dirty_then_clean_bounces_once_and_resolves() {
        let driver = RecordingDriver::new();
        let checks = ScriptedChecks::new(vec![
            vec![RuleId("RUST-FMT".to_string())], // initial: dirty
            vec![],                               // after revise: clean
        ]);
        let coord = Coordinator::new(&driver, &checks, "/tmp/wt");

        let report = coord.run(&role(), "build it").await.unwrap();

        assert!(report.bounced);
        assert!(report.is_clean());
        assert_eq!(
            report.initial_violations,
            vec![RuleId("RUST-FMT".to_string())]
        );
        assert!(report.revised_outcome.is_some());

        let tasks = driver.tasks.lock().unwrap();
        assert_eq!(tasks.len(), 2, "agent ran twice (initial + one revise)");
        // The revise task must cite the violated rule id.
        assert!(tasks[1].contains("RUST-FMT"));
        assert!(tasks[1].contains("REVISION REQUIRED"));
    }

    #[tokio::test]
    async fn still_dirty_after_revise_reports_residual_and_bounces_only_once() {
        let driver = RecordingDriver::new();
        let checks = ScriptedChecks::new(vec![
            vec![RuleId("RUST-CLIPPY".to_string())], // initial: dirty
            vec![RuleId("RUST-CLIPPY".to_string())], // still dirty after revise
        ]);
        let coord = Coordinator::new(&driver, &checks, "/tmp/wt");

        let report = coord.run(&role(), "build it").await.unwrap();

        assert!(report.bounced);
        assert!(!report.is_clean());
        assert_eq!(
            report.final_violations,
            vec![RuleId("RUST-CLIPPY".to_string())]
        );
        // AT MOST one bounce: exactly two agent runs, no more.
        assert_eq!(driver.tasks.lock().unwrap().len(), 2);
    }

    #[test]
    fn build_revise_task_cites_all_violated_ids() {
        let task = build_revise_task(
            "do x",
            &[
                RuleId("RUST-FMT".to_string()),
                RuleId("RUST-CLIPPY".to_string()),
            ],
            "",
        );
        assert!(task.starts_with("do x"));
        assert!(task.contains("RUST-FMT"));
        assert!(task.contains("RUST-CLIPPY"));
    }

    #[test]
    fn build_revise_task_appends_diagnostics_at_the_tail() {
        let diag = "error[E0308]: mismatched types\n  expected `u32`, found `String`";
        let task = build_revise_task("do x", &[RuleId("RUST-CLIPPY".to_string())], diag);
        assert!(task.starts_with("do x"));
        assert!(task.contains("RUST-CLIPPY"));
        // The verbatim toolchain diagnostics are present AND at the tail (after
        // the rule-id citation), so the cached prefix stays warm.
        assert!(task.contains("error[E0308]: mismatched types"));
        let diag_pos = task.find("error[E0308]").unwrap();
        let ids_pos = task.find("RUST-CLIPPY").unwrap();
        assert!(diag_pos > ids_pos, "diagnostics must come after the rule ids");
    }

    #[tokio::test]
    async fn driver_error_surfaces_as_coordinator_error() {
        struct FailingDriver;
        #[async_trait::async_trait]
        impl AgentDriver for FailingDriver {
            async fn run(&self, _r: &Role, _t: &str) -> anyhow::Result<AgentOutcome> {
                anyhow::bail!("boom")
            }
        }
        let checks = ScriptedChecks::new(vec![vec![]]);
        let driver = FailingDriver;
        let coord = Coordinator::new(&driver, &checks, "/tmp/wt");
        let err = coord.run(&role(), "x").await.unwrap_err();
        assert!(matches!(
            err,
            CoordinatorError::Driver {
                pass: "initial",
                ..
            }
        ));
    }
}
