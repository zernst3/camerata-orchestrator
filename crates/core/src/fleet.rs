//! The FleetCoordinator: pure orchestration of a SEQUENCE of roles against a
//! single shared worktree, each role driven as one governed agent run.
//!
//! # Why a fleet, and how it differs from [`Coordinator`]
//!
//! [`crate::Coordinator`] orchestrates ONE role + task (with a single
//! bounce-and-revise pass). The fleet generalises that to a *pipeline*: an
//! ordered list of `(Role, task)` stages run back-to-back against the SAME
//! worktree, so the filesystem output of an earlier agent (role A, the
//! "implementer") is visible to a later one (role B, the "tester"). That shared
//! worktree IS the inter-agent channel — no message passing, no shared memory;
//! the governed writes one agent lands become the substrate the next reads.
//!
//! Like the single-role coordinator this module makes ZERO model calls and runs
//! ZERO checks itself. Both seams are injected:
//!
//! - the [`AgentDriver`] is supplied PER STAGE (each governed agent gets its own
//!   driver, since each is a distinct `claude -p` session locked to its role's
//!   per-session rules file + mcp-config), and
//! - the [`CheckRunner`] is shared across the fleet (the layer-2 gate is a
//!   property of the worktree, not the role).
//!
//! That keeps the brain deterministic and unit-testable with fakes (see the
//! tests below), exactly as the single-role coordinator is.
//!
//! # Governance
//!
//! The fleet does not weaken governance: every stage's agent run still goes
//! through whatever gated driver the caller injected (in the live path, that is
//! a `claude -p` session locked to the Rust gateway via a per-session
//! mcp-config). The fleet simply sequences those governed runs and threads the
//! shared worktree through them. Each stage also bounces-and-revises once on its
//! own, reusing the same single-stage contract.

use std::path::{Path, PathBuf};

use crate::{AgentDriver, CheckRunner, Role, RunReport};
use crate::coordinator::{build_revise_task, CoordinatorError};

/// One stage of a fleet pipeline: a role, its task, and the governed driver that
/// runs it. The driver is per-stage because each governed agent is a distinct
/// session (its own rules file + mcp-config); the role carries the rule-subset.
pub struct FleetStage<'a> {
    /// The role this stage runs under (carries the rule-subset + path scope).
    pub role: Role,
    /// The task this stage's agent is asked to perform.
    pub task: String,
    /// The governed driver for THIS stage's agent run. Injected, so core never
    /// names a concrete model; in the live path this is a `claude -p` session
    /// locked to the gateway.
    pub driver: &'a dyn AgentDriver,
}

impl<'a> FleetStage<'a> {
    /// Construct a stage from its parts.
    pub fn new(role: Role, task: impl Into<String>, driver: &'a dyn AgentDriver) -> Self {
        Self {
            role,
            task: task.into(),
            driver,
        }
    }
}

/// The per-stage result inside a fleet run: which role ran, and the full
/// single-stage [`RunReport`] (initial + optional bounce-and-revise).
#[derive(Debug, Clone)]
pub struct StageReport {
    /// The role that ran this stage.
    pub role_name: String,
    /// The full single-stage report (agent → check → bounce-once).
    pub report: RunReport,
}

impl StageReport {
    /// Whether this stage ended clean (no residual layer-2 violations).
    pub fn is_clean(&self) -> bool {
        self.report.is_clean()
    }
}

/// The outcome of a whole fleet run: one [`StageReport`] per stage, in order.
#[derive(Debug, Clone)]
pub struct FleetReport {
    /// Per-stage reports, in execution order.
    pub stages: Vec<StageReport>,
}

impl FleetReport {
    /// Whether EVERY stage ended clean. A fleet is only clean if no stage left a
    /// residual layer-2 violation.
    pub fn is_clean(&self) -> bool {
        self.stages.iter().all(StageReport::is_clean)
    }

    /// Total number of bounce-and-revise passes across all stages.
    pub fn total_bounces(&self) -> usize {
        self.stages.iter().filter(|s| s.report.bounced).count()
    }
}

/// Orchestrates a SEQUENCE of governed roles against one shared worktree.
///
/// Holds only the shared check runner + worktree; each stage brings its own
/// governed driver. Pure orchestration: zero model calls, zero checks of its
/// own (both seams injected).
pub struct FleetCoordinator<'a> {
    checks: &'a dyn CheckRunner,
    worktree: PathBuf,
}

impl<'a> FleetCoordinator<'a> {
    /// Build a fleet coordinator over an injected check runner, scoped to
    /// `worktree` (the shared directory every stage's agent + the checks operate
    /// on).
    pub fn new(checks: &'a dyn CheckRunner, worktree: impl Into<PathBuf>) -> Self {
        Self {
            checks,
            worktree: worktree.into(),
        }
    }

    /// The shared worktree every stage operates on.
    pub fn worktree(&self) -> &Path {
        &self.worktree
    }

    /// Run a fleet of stages in order against the shared worktree.
    ///
    /// Each stage runs the single-role contract: agent → layer-2 check →
    /// (bounce-and-revise once if dirty). Because the worktree is shared and the
    /// stages run in order, the files an earlier stage's agent wrote are present
    /// on disk for a later stage's agent to read.
    ///
    /// Returns a [`FleetReport`] with one [`StageReport`] per stage. Stages run
    /// even if an earlier stage left residual violations — the fleet sequences
    /// every governed agent and reports honestly; escalation / abort policy is
    /// the caller's, not the coordinator's (same stance as the single-role
    /// coordinator's residual handling).
    pub async fn run(&self, stages: &[FleetStage<'_>]) -> Result<FleetReport, CoordinatorError> {
        let mut reports = Vec::with_capacity(stages.len());
        for stage in stages {
            let report = self.run_stage(stage).await?;
            reports.push(StageReport {
                role_name: stage.role.name.clone(),
                report,
            });
        }
        Ok(FleetReport { stages: reports })
    }

    /// Run a single stage: agent → check → (bounce-and-revise once).
    ///
    /// This mirrors [`crate::Coordinator::run`] exactly, but takes the driver
    /// from the stage rather than from a coordinator-held field, so each stage
    /// can be a distinct governed session.
    async fn run_stage(&self, stage: &FleetStage<'_>) -> Result<RunReport, CoordinatorError> {
        let role = &stage.role;
        let task = stage.task.as_str();
        let driver = stage.driver;

        // 1. Initial governed agent pass (model call lives behind the driver).
        let initial_outcome =
            driver
                .run(role, task)
                .await
                .map_err(|source| CoordinatorError::Driver {
                    pass: "initial",
                    source,
                })?;

        // 2. Layer-2 check against the shared worktree.
        let initial_violations = self
            .checks
            .check(role, &self.worktree)
            .await
            .map_err(|source| CoordinatorError::Check {
                pass: "initial",
                source,
            })?;

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

        // 3. ONE bounce-and-revise pass, citing the violated rule ids verbatim.
        let revise_task = build_revise_task(task, &initial_violations);
        let revised_outcome = driver
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
            })?;

        Ok(RunReport {
            initial_outcome,
            initial_violations,
            revised_outcome: Some(revised_outcome),
            final_violations,
            bounced: true,
        })
    }
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentOutcome, RuleId};
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::Mutex;

    fn role(name: &str) -> Role {
        Role {
            name: name.to_string(),
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

    /// Records (role_name, task) for every run; returns a fixed outcome.
    struct RecordingDriver {
        calls: Mutex<Vec<(String, String)>>,
    }
    impl RecordingDriver {
        fn new() -> Self {
            Self {
                calls: Mutex::new(vec![]),
            }
        }
    }
    #[async_trait::async_trait]
    impl AgentDriver for RecordingDriver {
        async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
            self.calls
                .lock()
                .unwrap()
                .push((role.name.clone(), task.to_string()));
            Ok(outcome(&format!("sess-{}", role.name.to_lowercase())))
        }
    }

    /// Returns a scripted sequence of violation-sets, one per check call.
    struct ScriptedChecks {
        scripted: Mutex<VecDeque<Vec<RuleId>>>,
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
        async fn check(&self, _role: &Role, _wt: &Path) -> anyhow::Result<Vec<RuleId>> {
            Ok(self.scripted.lock().unwrap().pop_front().unwrap_or_default())
        }
    }

    #[tokio::test]
    async fn two_clean_stages_run_in_order_no_bounce() {
        let driver_a = RecordingDriver::new();
        let driver_b = RecordingDriver::new();
        // Each stage's first check is clean.
        let checks = ScriptedChecks::new(vec![vec![], vec![]]);
        let fleet = FleetCoordinator::new(&checks, "/tmp/wt");

        let stages = vec![
            FleetStage::new(role("Implementer"), "write lib.rs", &driver_a),
            FleetStage::new(role("Tester"), "write a test", &driver_b),
        ];
        let report = fleet.run(&stages).await.unwrap();

        assert_eq!(report.stages.len(), 2);
        assert!(report.is_clean());
        assert_eq!(report.total_bounces(), 0);
        assert_eq!(report.stages[0].role_name, "Implementer");
        assert_eq!(report.stages[1].role_name, "Tester");

        // Each governed driver ran exactly once (its own stage).
        assert_eq!(driver_a.calls.lock().unwrap().len(), 1);
        assert_eq!(driver_b.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stage_bounces_once_when_dirty_then_resolves() {
        let driver_a = RecordingDriver::new();
        let driver_b = RecordingDriver::new();
        // Stage A: dirty then clean (bounces). Stage B: clean immediately.
        let checks = ScriptedChecks::new(vec![
            vec![RuleId("RUST-FMT".to_string())], // A initial: dirty
            vec![],                               // A revise: clean
            vec![],                               // B initial: clean
        ]);
        let fleet = FleetCoordinator::new(&checks, "/tmp/wt");

        let stages = vec![
            FleetStage::new(role("Implementer"), "write lib.rs", &driver_a),
            FleetStage::new(role("Tester"), "write a test", &driver_b),
        ];
        let report = fleet.run(&stages).await.unwrap();

        assert!(report.is_clean());
        assert_eq!(report.total_bounces(), 1);
        assert!(report.stages[0].report.bounced);
        assert!(!report.stages[1].report.bounced);

        // Stage A's driver ran twice (initial + revise); the bounce cited the id.
        let a_calls = driver_a.calls.lock().unwrap();
        assert_eq!(a_calls.len(), 2);
        assert!(a_calls[1].1.contains("RUST-FMT"));
        assert!(a_calls[1].1.contains("REVISION REQUIRED"));
        // Stage B's driver ran exactly once.
        assert_eq!(driver_b.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn residual_violation_in_one_stage_marks_fleet_dirty_but_runs_all() {
        let driver_a = RecordingDriver::new();
        let driver_b = RecordingDriver::new();
        // Stage A stays dirty after its one bounce; stage B is clean.
        let checks = ScriptedChecks::new(vec![
            vec![RuleId("RUST-CLIPPY".to_string())], // A initial: dirty
            vec![RuleId("RUST-CLIPPY".to_string())], // A revise: still dirty
            vec![],                                  // B initial: clean
        ]);
        let fleet = FleetCoordinator::new(&checks, "/tmp/wt");

        let stages = vec![
            FleetStage::new(role("Implementer"), "write lib.rs", &driver_a),
            FleetStage::new(role("Tester"), "write a test", &driver_b),
        ];
        let report = fleet.run(&stages).await.unwrap();

        // The fleet is NOT clean (stage A has a residual), but BOTH stages ran.
        assert!(!report.is_clean());
        assert_eq!(report.stages.len(), 2);
        assert!(!report.stages[0].is_clean());
        assert!(report.stages[1].is_clean());
        // Stage A bounced once (exactly two agent runs), no more.
        assert_eq!(driver_a.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn driver_error_in_a_stage_surfaces_as_coordinator_error() {
        struct FailingDriver;
        #[async_trait::async_trait]
        impl AgentDriver for FailingDriver {
            async fn run(&self, _r: &Role, _t: &str) -> anyhow::Result<AgentOutcome> {
                anyhow::bail!("boom")
            }
        }
        let failing = FailingDriver;
        let checks = ScriptedChecks::new(vec![vec![]]);
        let fleet = FleetCoordinator::new(&checks, "/tmp/wt");
        let stages = vec![FleetStage::new(role("Implementer"), "x", &failing)];
        let err = fleet.run(&stages).await.unwrap_err();
        assert!(matches!(
            err,
            CoordinatorError::Driver {
                pass: "initial",
                ..
            }
        ));
    }
}
