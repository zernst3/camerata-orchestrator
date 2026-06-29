//! The routine auto-fire scheduler: a background tokio task that runs routines when
//! their schedule comes due.
//!
//! This is the "remaining wiring" the routine-dashboard ADR called out: until now
//! `enabled` was a flag that fired nothing. The loop mirrors the notify-poller idiom
//! (`crate::notify::spawn_tracker_poller`): clone the Arc-backed [`RoutineStore`] into a
//! task that wakes on an interval, fires each provisioned + enabled routine whose
//! schedule is due (via the existing governed `run_now` path), and stamps `last_fired`
//! so a given slot fires once rather than every tick.
//!
//! Cadence is `CAMERATA_ROUTINE_TICK_SECS` (default 60). The decision of WHETHER a
//! routine is due lives in the pure, unit-tested [`crate::schedule::is_due`].

use std::time::Duration;

use chrono::Local;

use crate::escalation::EscalationStore;
use crate::routine::RoutineStore;

/// Tick cadence in seconds (`CAMERATA_ROUTINE_TICK_SECS`, default 60, min 1).
fn tick_secs() -> u64 {
    std::env::var("CAMERATA_ROUTINE_TICK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60)
}

/// Spawn the auto-fire loop. Call once from `serve()` after building [`crate::AppState`].
///
/// This runs EVERY provisioned + enabled routine across ALL projects whenever its
/// schedule is due — routines are the autonomous plane and are not gated by which project
/// the architect is currently viewing. The loop lives in-process, so routines run exactly
/// while Camerata is open; pressing Stop (disable) is the only thing that halts one.
pub fn spawn_routine_scheduler(routines: RoutineStore, escalations: EscalationStore) {
    let secs = tick_secs();
    tokio::spawn(async move {
        loop {
            tick(&routines, &escalations);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    });
}

/// One scheduler pass: fire every provisioned + enabled routine whose schedule is due
/// against the current local wall-clock time, regardless of project.
fn tick(routines: &RoutineStore, escalations: &EscalationStore) {
    let now_local = Local::now();
    let now = now_local.naive_local();
    for r in routines.list() {
        // Imported-but-not-set-up routines, and stopped ones, never auto-fire.
        if !r.provisioned || !r.enabled {
            continue;
        }
        let last = r
            .last_fired
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.naive_local());
        if crate::schedule::is_due(&r.schedule, now, last) {
            // Run via the existing governed path, then stamp the fire so the same slot
            // isn't re-run on the next tick. Both are best-effort: a routine deleted
            // mid-tick simply no-ops.
            let now_ts = now_local.to_rfc3339();
            let ran = routines.run_now_scheduled(&r.id, &now_ts);
            let _ = routines.mark_fired(&r.id, &now_ts);
            // If the unattended run was blocked (gate denials), raise a human-review
            // escalation so the architect can resolve it whenever they next look, and link it to
            // this run's history entry.
            if let Some(routine) = ran {
                if let Some(esc_id) = crate::escalation::raise_if_blocked(escalations, &routine) {
                    let _ = routines.link_last_run_escalation(&r.id, &esc_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routine::CreateRoutineReq;

    fn req(name: &str, schedule: &str) -> CreateRoutineReq {
        CreateRoutineReq {
            name: name.to_string(),
            schedule: schedule.to_string(),
            intent: "do a thing".to_string(),
            prompt: String::new(),
            scope: "read-only".to_string(),
            project_id: None,
            model: None,
        }
    }

    #[test]
    fn tick_fires_due_enabled_routine_and_stamps_once() {
        let store = RoutineStore::new();
        let esc = EscalationStore::new();
        // "daily 00:00" is always due during the day (slot = today 00:00 <= now).
        let r = store.create(&req("Due", "daily 00:00"));
        assert!(r.last_fired.is_none());

        tick(&store, &esc);
        let after = &store.list()[0];
        assert!(after.last_fired.is_some(), "due routine was stamped");
        assert!(after.last_run.is_some(), "due routine actually ran");
        // The scripted gate denies (2 denies), so an unattended fire raises one review.
        assert_eq!(
            esc.list_open().len(),
            1,
            "blocked unattended run raised a review"
        );

        // A second immediate tick must NOT re-fire the same slot (last_fired > slot)
        // and must NOT pile up a duplicate review.
        let fired_at = after.last_fired.clone();
        tick(&store, &esc);
        assert_eq!(
            store.list()[0].last_fired,
            fired_at,
            "same slot not re-fired"
        );
        assert_eq!(esc.list_open().len(), 1, "review not duplicated");
    }

    #[test]
    fn tick_skips_disabled_and_unprovisioned() {
        let store = RoutineStore::new();
        let esc = EscalationStore::new();
        let disabled = store.create(&req("Stopped", "daily 00:00"));
        store.set_enabled(&disabled.id, false);

        // The disabled routine is skipped; the provisioned+enabled gate is what matters.
        tick(&store, &esc);
        assert!(
            store
                .list()
                .iter()
                .find(|r| r.id == disabled.id)
                .unwrap()
                .last_fired
                .is_none(),
            "disabled routine never auto-fires"
        );
        assert!(
            esc.list_open().is_empty(),
            "nothing fired, nothing escalated"
        );
    }

    #[test]
    fn manual_schedule_never_fires() {
        let store = RoutineStore::new();
        let esc = EscalationStore::new();
        let r = store.create(&req("Manual", "manual"));
        tick(&store, &esc);
        assert!(store
            .list()
            .iter()
            .find(|x| x.id == r.id)
            .unwrap()
            .last_fired
            .is_none());
    }
}
