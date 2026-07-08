//! The background stall sweep (LIFECYCLE-6): a tokio task that periodically evaluates every
//! ACTIVE run and AUTO-CANCELS the autonomous ones that have stalled.
//!
//! # Why a sweep
//!
//! `stall_decision` / `StallPolicy::Cancel` / `RunKind::Autonomous` already existed but were
//! dead code: nothing ever called them, so a wedged walk-away (routine-driven) run would sit
//! forever. `GET /api/runs/:id` reports `stalled` for an interactive run so the architect can
//! act, but an autonomous run has NO architect watching. This sweep is the actor: it applies
//! the per-project stall threshold and, for Autonomous runs only, cancels a stalled run.
//!
//! # Autonomous-only auto-cancel
//!
//! Watched (interactive) runs are ALERT-ONLY: the sweep never cancels them (the architect is
//! watching and decides). Only Autonomous runs whose [`StallPolicy`] is `Cancel` are
//! auto-cancelled. Done runs and human-parked runs (AwaitingReview / AwaitingClarification)
//! are never touched — [`stall_decision`] short-circuits them to `Ok`.
//!
//! # Threshold
//!
//! The threshold is the ACTIVE project's `stall_threshold_ms(autonomous)` (the generous routine
//! band for autonomous runs), falling back to the env/default when no project is active. This is
//! the SAME threshold `get_run` reports against, so the banner and the auto-cancel agree.

use std::sync::Arc;
use std::time::Duration;

use crate::project::ProjectStore;
use crate::run::{stall_decision, RunStore, StallDecision};

/// Sweep cadence in seconds (`CAMERATA_STALL_SWEEP_SECS`, default 30, min 1).
///
/// The sweep is cheap (an in-memory scan), so a short cadence keeps auto-cancel responsive
/// without meaningful cost. It is independent of the (generous) stall threshold itself.
fn sweep_secs() -> u64 {
    std::env::var("CAMERATA_STALL_SWEEP_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30)
}

/// Spawn the stall sweep loop. Call once from `serve()` after building [`crate::AppState`].
///
/// Spawned there (not in `router`) so unit tests that only build the router don't start
/// background cancellation. Lives in-process, so it sweeps exactly while Camerata is open.
pub fn spawn_stall_sweep(
    runs: RunStore,
    projects: ProjectStore,
    // Phase H2: the governance-event audit trail, so an auto-cancelled autonomous run gets a
    // durable `stall_cancel` row (not just the in-memory `failure_reason`). `None` in tests /
    // on open failure (fail-soft — see `AppState::record_governance`).
    governance_log: Option<Arc<camerata_persistence::GovernanceLog>>,
) {
    let secs = sweep_secs();
    tokio::spawn(async move {
        loop {
            sweep_once(&runs, &projects, governance_log.as_ref()).await;
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    });
}

/// One sweep pass: for every active run, apply the per-project stall threshold and auto-cancel
/// the AUTONOMOUS ones that have stalled.
///
/// Returns the ids that were auto-cancelled this pass (for logging / tests). Watched runs that
/// are stalled are intentionally left running (alert-only): the reported `stalled` flag in
/// `get_run` is their signal, and the architect decides.
///
/// Pure-ish: reads a clock and mutates the store only through the public `cancel` /
/// `fail_with_reason` setters, which are themselves idempotent-terminal. Split from the spawn
/// wrapper so it is directly unit-testable without a running tokio interval.
pub async fn sweep_once(
    runs: &RunStore,
    projects: &ProjectStore,
    governance_log: Option<&Arc<camerata_persistence::GovernanceLog>>,
) -> Vec<String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    sweep_at(runs, projects, now_ms, governance_log).await
}

/// [`sweep_once`] with an explicit `now_ms` clock, so tests can drive a run past its threshold
/// deterministically without waiting on wall-clock idle. The spawn path always uses the real
/// clock via [`sweep_once`].
pub async fn sweep_at(
    runs: &RunStore,
    projects: &ProjectStore,
    now_ms: u128,
    governance_log: Option<&Arc<camerata_persistence::GovernanceLog>>,
) -> Vec<String> {
    let mut cancelled = Vec::new();
    for run in runs.snapshot_active() {
        // Per-kind threshold from the active project (generous routine band for autonomous),
        // falling back to the env/default when no project is active.
        let threshold_ms = projects
            .active()
            .map(|p| p.stall_threshold_ms(run.kind.is_autonomous()))
            .unwrap_or_else(crate::run::run_stall_threshold_ms);

        match stall_decision(&run, threshold_ms, now_ms) {
            // AUTONOMOUS-ONLY auto-cancel. `StallDecision::Cancel` is only ever returned for a
            // run whose policy is `Cancel`, which is set only for `RunKind::Autonomous` runs
            // (see `RunStore::create`). Fail the run with an honest stall reason so the terminal
            // state carries WHY it ended; `fail_with_reason` is idempotent-terminal, so a run
            // that finished on its own between the snapshot and here is left untouched.
            StallDecision::Cancel => {
                let idle_secs = crate::run::idle_ms(
                    u128::from(run.tracker.last_activity_ms()),
                    now_ms,
                ) / 1_000;
                let reason = format!(
                    "Auto-cancelled: autonomous run stalled (idle {idle_secs}s, threshold \
                     {}s) with no activity heartbeat.",
                    threshold_ms / 1_000
                );
                runs.fail_with_reason(&run.id, reason.clone());
                // Phase H2: durable record of the auto-cancel, independent of the in-memory
                // `failure_reason` (which does not survive a process restart).
                if let Some(log) = governance_log {
                    let event = camerata_persistence::GovernanceEvent::warn(
                        run.id.clone(),
                        "stall_cancel",
                        "system",
                    )
                    .with_story_id(run.story_id.clone())
                    .with_reason(reason);
                    if let Err(e) = log.record(event).await {
                        tracing::warn!(error = %e, run_id = %run.id, "failed to record stall_cancel governance event");
                    }
                }
                cancelled.push(run.id.clone());
            }
            // Watched runs: alert-only. The stall is surfaced in `get_run`; the sweep does not
            // cancel it. `Ok` runs are healthy (or done/parked).
            StallDecision::Alert | StallDecision::Ok => {}
        }
    }
    cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{RunKind, RunStatus};

    /// Rewind a run's last-activity clock so it reads as idle by `back_secs` seconds. The
    /// tracker is initialised to "now" on `create`; we can't set it directly, so we assert
    /// against a `now_ms` far in the future instead. This helper computes that future clock.
    fn now_plus(secs: u64) -> u128 {
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        base + u128::from(secs) * 1_000
    }

    /// An AUTONOMOUS run idle past its threshold is auto-cancelled by the sweep: the run goes
    /// terminal (`done`) with a `Failed` status carrying the stall reason. Driven with an
    /// explicit future clock so the idle time is deterministic.
    #[tokio::test]
    async fn autonomous_stalled_run_is_auto_cancelled() {
        let runs = RunStore::new();
        let projects = ProjectStore::new(); // no active project → env/default threshold
        let id = runs.create("CAM-auto", "live", RunKind::Autonomous);
        runs.set_status(&id, RunStatus::Executing, false);

        // Idle ~1_000_000s in the future: far past even the generous routine default.
        let cancelled = sweep_at(&runs, &projects, now_plus(1_000_000), None).await;
        assert_eq!(cancelled, vec![id.clone()], "the stalled run was cancelled");

        let r = runs.get(&id).unwrap();
        assert!(r.done, "auto-cancelled run is terminal");
        assert!(
            matches!(r.status, RunStatus::Failed { .. }),
            "auto-cancel marks the run Failed with a reason, got {:?}",
            r.status
        );
        assert!(
            r.failure_reason
                .as_deref()
                .unwrap_or_default()
                .contains("stalled"),
            "failure reason names the stall"
        );
    }

    /// A WATCHED run past its threshold is only FLAGGED stalled, never auto-cancelled by the
    /// sweep, while an AUTONOMOUS run in the same pass IS cancelled. Drives the full `sweep_at`
    /// path with both runs present to prove the policy split end-to-end.
    #[tokio::test]
    async fn watched_stall_is_alert_only_autonomous_is_cancel() {
        let runs = RunStore::new();
        let projects = ProjectStore::new();
        let watched = runs.create("CAM-w", "live", RunKind::Watched);
        let autonomous = runs.create("CAM-a", "live", RunKind::Autonomous);
        runs.set_status(&watched, RunStatus::Executing, false);
        runs.set_status(&autonomous, RunStatus::Executing, false);

        let far_future = now_plus(1_000_000); // idle ~1_000_000s, past any threshold
        let cancelled = sweep_at(&runs, &projects, far_future, None).await;

        // Only the autonomous run was cancelled.
        assert_eq!(cancelled, vec![autonomous.clone()]);

        // Watched: still active (alert-only), status unchanged.
        let w = runs.get(&watched).unwrap();
        assert!(!w.done, "stalled watched run is NOT auto-cancelled");
        assert_eq!(w.status, RunStatus::Executing);

        // Autonomous: terminal Failed.
        let a = runs.get(&autonomous).unwrap();
        assert!(a.done);
        assert!(matches!(a.status, RunStatus::Failed { .. }));

        // And the underlying decision split is what drives it.
        let threshold = 120_000u128;
        assert_eq!(
            stall_decision(&runs.get(&watched).unwrap(), threshold, far_future),
            StallDecision::Alert
        );
    }

    /// A fresh/active autonomous run (activity within threshold) is untouched by the sweep.
    #[tokio::test]
    async fn fresh_autonomous_run_is_untouched() {
        let runs = RunStore::new();
        let projects = ProjectStore::new();
        let id = runs.create("CAM-fresh", "live", RunKind::Autonomous);
        runs.set_status(&id, RunStatus::Executing, false);
        // Just touched activity → not stalled.
        runs.touch_activity(&id, None);

        let cancelled = sweep_once(&runs, &projects, None).await;
        assert!(cancelled.is_empty());
        let r = runs.get(&id).unwrap();
        assert!(!r.done, "fresh autonomous run stays active");
        assert_eq!(r.status, RunStatus::Executing);
    }

    /// A DONE autonomous run is never swept (terminal runs cannot stall).
    #[tokio::test]
    async fn done_autonomous_run_is_never_swept() {
        let runs = RunStore::new();
        let projects = ProjectStore::new();
        let id = runs.create("CAM-done", "live", RunKind::Autonomous);
        runs.set_status(&id, RunStatus::AwaitingQa, true); // terminal success

        // Even with a zero cadence / future clock, snapshot_active excludes done runs, so the
        // sweep can't touch it and its terminal status is preserved.
        let cancelled = sweep_once(&runs, &projects, None).await;
        assert!(cancelled.is_empty());
        let r = runs.get(&id).unwrap();
        assert!(r.done);
        assert_eq!(r.status, RunStatus::AwaitingQa, "success terminal preserved");
    }
}
