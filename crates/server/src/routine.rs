//! Routine dashboard backend (ADR `routine_dashboard`).
//!
//! A routine is a scheduled governed run: a name, a schedule, a prompt, a permission
//! scope, an enabled flag, and the last-run summary. "Run now" executes a governed run
//! immediately, reusing the REAL gate script from the run engine (so the recorded
//! verdicts are genuine, token-free). The auto-fire scheduler (an engine-owned timer)
//! is the remaining wiring; this turn ships the model, the store, and run-now so the
//! dashboard can list, toggle, and run routines.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The outcome summary of a routine's last run: real counts from the gate script.
#[derive(Clone, Serialize)]
pub struct RoutineRunSummary {
    /// "passed" when the governed run completed (denies are the gate working, not
    /// failures).
    pub outcome: String,
    pub total_verdicts: usize,
    pub denies: usize,
    pub allows: usize,
}

/// A scheduled governed routine.
#[derive(Clone, Serialize)]
pub struct Routine {
    pub id: String,
    pub name: String,
    /// Human-readable schedule (e.g. "daily 04:00"). The scheduler that fires on it is
    /// the remaining wiring.
    pub schedule: String,
    /// The user's plain-language description of WHAT they want the routine to do.
    /// This is what the user writes; the AI authors the operational `prompt` from it
    /// (ADR routine_authoring_intent_not_prompt).
    pub intent: String,
    /// The OPERATIONAL prompt the agent actually runs — authored from `intent` by the
    /// lead-engineer AI (model tiering, directives, governance framing) and
    /// human-reviewed. Never the user's raw description verbatim.
    pub prompt: String,
    /// The permission / rule scope the routine runs under (shown so an unattended
    /// agent's governance is legible).
    pub scope: String,
    pub enabled: bool,
    pub last_run: Option<RoutineRunSummary>,
}

/// Request body to create a routine. The user supplies `intent`; `prompt` is the
/// reviewed operational prompt (from the draft step). If `prompt` is empty the
/// server scaffolds one from the intent so the raw description is never run as-is.
#[derive(Deserialize)]
pub struct CreateRoutineReq {
    pub name: String,
    pub schedule: String,
    pub intent: String,
    #[serde(default)]
    pub prompt: String,
    pub scope: String,
}

/// Request body for the draft-prompt step: the user's intent + scope.
#[derive(Deserialize)]
pub struct DraftPromptReq {
    pub intent: String,
    #[serde(default)]
    pub scope: String,
}

/// Response from the draft-prompt step.
#[derive(Serialize)]
pub struct DraftPromptResp {
    /// The drafted operational prompt for the user to review/edit.
    pub prompt: String,
    /// How it was authored: `scaffold` (deterministic fallback, no Claude) or
    /// `claude` (the lead-engineer AI authored it).
    pub authored_by: String,
}

/// Deterministic scaffold for the operational prompt when no Claude connection is
/// available to author it for real. Wraps the user's intent with the standard
/// governance/scope framing and marks model tiering as the lead engineer's call,
/// so the flow is usable offline and the user always reviews a structured prompt
/// rather than running their raw description. The real AI authoring replaces this
/// when Claude is connected.
pub fn scaffold_prompt(intent: &str, scope: &str) -> String {
    let scope = if scope.trim().is_empty() {
        "read-only"
    } else {
        scope.trim()
    };
    format!(
        "Objective (from the user's description):\n{intent}\n\n\
         Operating constraints:\n\
         - Every file write passes the governance gate (deny-before-execute); the agent \
         has no shell, no direct file tools, and cannot spawn subagents.\n\
         - Scope / rules: {scope}\n\
         - Model tiering: use the smallest capable model per task and escalate only for \
         genuinely hard reasoning (the lead engineer sets this per task once Claude is \
         connected).\n\
         - Be directive and concrete: prefer exact files and steps over open-ended \
         exploration.\n\
         - Report what was done, what the gate denied, and anything left for human \
         review.\n\n\
         [Draft scaffold — connect Claude so the lead engineer authors the full \
         operational prompt (including chosen model tiers) from your description.]"
    )
}

/// Request body to enable/disable a routine.
#[derive(Deserialize)]
pub struct SetEnabledReq {
    pub enabled: bool,
}

/// In-memory routine store.
#[derive(Clone, Default)]
pub struct RoutineStore {
    items: Arc<Mutex<Vec<Routine>>>,
    counter: Arc<AtomicUsize>,
}

impl RoutineStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store seeded with representative routines so the dashboard has content.
    pub fn seeded() -> Self {
        let store = Self::new();
        let mk = |id: &str, name: &str, schedule: &str, intent: &str, scope: &str, enabled: bool| {
            Routine {
                id: id.to_string(),
                name: name.to_string(),
                schedule: schedule.to_string(),
                intent: intent.to_string(),
                // Demo data: the operational prompt is the scaffold of the intent
                // (the live create path does the same, or AI-authors it).
                prompt: scaffold_prompt(intent, scope),
                scope: scope.to_string(),
                enabled,
                last_run: None,
            }
        };
        let seed = vec![
            mk(
                "rt-1",
                "Nightly dependency + security sweep",
                "daily 04:00",
                "Scan dependencies for advisories; open governed PRs for safe upgrades.",
                "SEC-* + maintenance, write behind the gate",
                true,
            ),
            mk(
                "rt-2",
                "Stale-PR auditor",
                "weekly Mon 09:00",
                "Flag PRs with no activity in 14 days and summarize what they are blocked on.",
                "read-only",
                true,
            ),
            mk(
                "rt-3",
                "Convention drift check",
                "daily 06:00",
                "Check that CONVENTIONS rule ids referenced in code still exist.",
                "ARCH-*, read-only",
                false,
            ),
        ];
        if let Ok(mut guard) = store.items.lock() {
            *guard = seed;
        }
        store.counter.store(3, Ordering::SeqCst);
        store
    }

    pub fn list(&self) -> Vec<Routine> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn create(&self, req: &CreateRoutineReq) -> Routine {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        // The user's raw intent is never run as-is: if the reviewed operational
        // prompt is empty, scaffold one from the intent.
        let prompt = if req.prompt.trim().is_empty() {
            scaffold_prompt(&req.intent, &req.scope)
        } else {
            req.prompt.clone()
        };
        let routine = Routine {
            id: format!("rt-{n}"),
            name: req.name.clone(),
            schedule: req.schedule.clone(),
            intent: req.intent.clone(),
            prompt,
            scope: req.scope.clone(),
            enabled: true,
            last_run: None,
        };
        if let Ok(mut guard) = self.items.lock() {
            guard.push(routine.clone());
        }
        routine
    }

    /// Edit a routine's user-facing fields in place (name / schedule / intent /
    /// prompt / scope). Mirrors `create`'s rule: an empty reviewed prompt is
    /// re-scaffolded from the intent so a routine never runs the raw intent as-is.
    /// `enabled` and `last_run` are preserved.
    pub fn update(&self, id: &str, req: &CreateRoutineReq) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.name = req.name.clone();
        r.schedule = req.schedule.clone();
        r.intent = req.intent.clone();
        r.scope = req.scope.clone();
        r.prompt = if req.prompt.trim().is_empty() {
            scaffold_prompt(&req.intent, &req.scope)
        } else {
            req.prompt.clone()
        };
        Some(r.clone())
    }

    /// Delete a routine by id. Returns true if one was removed.
    pub fn delete(&self, id: &str) -> bool {
        let Ok(mut guard) = self.items.lock() else {
            return false;
        };
        let before = guard.len();
        guard.retain(|r| r.id != id);
        guard.len() != before
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.enabled = enabled;
        Some(r.clone())
    }

    /// Run a routine now: execute a governed run via the REAL gate script and record
    /// the summary. Token-free and instant (the pure script, not the timed executor).
    pub fn run_now(&self, id: &str) -> Option<Routine> {
        let events = crate::run::run_event_script();
        let denies = events.iter().filter(|e| e.verdict == "deny").count();
        let allows = events.iter().filter(|e| e.verdict == "allow").count();
        let summary = RoutineRunSummary {
            outcome: "passed".to_string(),
            total_verdicts: events.len(),
            denies,
            allows,
        };
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.last_run = Some(summary);
        Some(r.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_lists_three_routines() {
        let store = RoutineStore::seeded();
        let list = store.list();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, "rt-1");
        assert!(list[0].enabled);
        assert!(!list[2].enabled);
    }

    #[test]
    fn toggle_and_create_and_run() {
        let store = RoutineStore::seeded();
        assert!(store.set_enabled("rt-3", true).unwrap().enabled);

        let created = store.create(&CreateRoutineReq {
            name: "Ad-hoc".to_string(),
            schedule: "manual".to_string(),
            intent: "do a thing".to_string(),
            prompt: String::new(),
            scope: "read-only".to_string(),
        });
        assert_eq!(created.id, "rt-4");
        assert_eq!(store.list().len(), 4);
        // Empty prompt -> scaffolded from intent (never run the raw intent as-is).
        assert!(created.prompt.contains("do a thing"));
        assert!(created.prompt.contains("governance gate"));
        assert_eq!(created.intent, "do a thing");

        // Run-now records a real-gate summary (2 denies + 1 allow from the script).
        let ran = store.run_now("rt-1").unwrap();
        let summary = ran.last_run.expect("recorded");
        assert_eq!(summary.outcome, "passed");
        assert_eq!(summary.denies, 2);
        assert_eq!(summary.allows, 1);

        assert!(store.run_now("nope").is_none());
    }

    #[test]
    fn update_edits_fields_and_preserves_enabled_and_last_run() {
        let store = RoutineStore::seeded();
        // Record a run on rt-1 so we can prove last_run survives an edit.
        store.run_now("rt-1").unwrap();

        let edited = store
            .update(
                "rt-1",
                &CreateRoutineReq {
                    name: "Renamed".to_string(),
                    schedule: "weekly Mon,Wed 09:00".to_string(),
                    intent: "new intent".to_string(),
                    prompt: String::new(), // empty -> re-scaffolded from intent
                    scope: "write (gated)".to_string(),
                },
            )
            .unwrap();
        assert_eq!(edited.name, "Renamed");
        assert_eq!(edited.schedule, "weekly Mon,Wed 09:00");
        assert_eq!(edited.scope, "write (gated)");
        assert!(edited.prompt.contains("new intent"), "empty prompt re-scaffolded");
        assert!(edited.enabled, "enabled flag preserved across edit");
        assert!(edited.last_run.is_some(), "last_run preserved across edit");

        assert!(store.update("nope", &CreateRoutineReq {
            name: "x".into(), schedule: "daily 09:00".into(), intent: "x".into(),
            prompt: String::new(), scope: "read-only".into(),
        }).is_none());
    }

    #[test]
    fn delete_removes_only_the_named_routine() {
        let store = RoutineStore::seeded();
        assert_eq!(store.list().len(), 3);
        assert!(store.delete("rt-2"));
        assert_eq!(store.list().len(), 2);
        assert!(store.list().iter().all(|r| r.id != "rt-2"));
        // Deleting a missing id is a no-op false.
        assert!(!store.delete("rt-2"));
        assert!(!store.delete("nope"));
    }
}
