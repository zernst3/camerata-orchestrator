//! Phase 3: the run engine. A "run" is a governed execution of a story.
//!
//! The honest, token-free default drives a deterministic sequence of planted tool
//! calls through the REAL layer-1 gate (`camerata_gateway::evaluate_call`), so the
//! gate verdicts a run reports are genuine deny/allow decisions from the actual gate,
//! not narration. The "agent" producing the calls is scripted; the GATE is real. A
//! later increment swaps the scripted calls for a real `claude -p` fleet behind an
//! opt-in flag (the same path `po-demo` exercises), which spends tokens.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use camerata_core::{Decision, RuleId, ToolCall};
use camerata_gateway::{enforced_gate_rules, evaluate_call};

/// The lifecycle status of a run, in Camerata's vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Planned,
    Executing,
    Gating,
    AwaitingQa,
}

/// One real gate verdict recorded during a run.
#[derive(Clone, Serialize)]
pub struct GateEvent {
    pub seq: usize,
    /// Which enforcement layer produced it ("layer-1" for the deny-before-execute gate).
    pub layer: String,
    /// "deny" or "allow", straight from the real gate decision.
    pub verdict: String,
    /// The rule id that denied, when the verdict is a deny.
    pub rule: Option<String>,
    /// Human-readable narrative plus the gate's own reason text.
    pub detail: String,
}

/// A run: a story being governed, its current status, and the real gate activity so far.
#[derive(Clone, Serialize)]
pub struct Run {
    pub id: String,
    pub story_id: String,
    pub status: RunStatus,
    pub events: Vec<GateEvent>,
    /// True once the run has walked to AwaitingQa.
    pub done: bool,
}

/// In-memory store of runs, shared into the background executor and the handlers.
#[derive(Clone, Default)]
pub struct RunStore {
    runs: Arc<Mutex<HashMap<String, Run>>>,
    counter: Arc<AtomicUsize>,
}

impl RunStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a Planned run for `story_id` and return its id.
    pub fn create(&self, story_id: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("run-{n}");
        let run = Run {
            id: id.clone(),
            story_id: story_id.to_string(),
            status: RunStatus::Planned,
            events: Vec::new(),
            done: false,
        };
        if let Ok(mut guard) = self.runs.lock() {
            guard.insert(id.clone(), run);
        }
        id
    }

    pub fn get(&self, id: &str) -> Option<Run> {
        self.runs.lock().ok()?.get(id).cloned()
    }

    fn set_status(&self, id: &str, status: RunStatus, done: bool) {
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                run.status = status;
                run.done = done;
            }
        }
    }

    fn push_event(&self, id: &str, event: GateEvent) {
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                run.events.push(event);
            }
        }
    }
}

// ── the deterministic, real-gate run script ─────────────────────────────────

fn path_escape_call() -> ToolCall {
    ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/../../etc/cron.d/payload",
            "content": "*/1 * * * * root sh -c id",
        }),
    }
}

fn secret_call() -> ToolCall {
    ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/api/src/export_config.rs",
            "content": "let token = \"ghp_ABCDEFGHIJ1234567890abcdefghij12\";",
        }),
    }
}

fn clean_call() -> ToolCall {
    ToolCall {
        tool: "gated_write".to_string(),
        input: serde_json::json!({
            "path": "crates/api/src/members_repo.rs",
            "content": "pub fn export_members() -> Vec<Member> { repo.all() }",
        }),
    }
}

/// Build a gate event by running the call through the REAL gate.
fn gate_event(seq: usize, rules: &[RuleId], call: &ToolCall, narrative: &str) -> GateEvent {
    match evaluate_call(rules, call) {
        Decision::Deny { rule, reason } => GateEvent {
            seq,
            layer: "layer-1".to_string(),
            verdict: "deny".to_string(),
            rule: Some(rule.0),
            detail: format!("{narrative} {reason}"),
        },
        Decision::Allow => GateEvent {
            seq,
            layer: "layer-1".to_string(),
            verdict: "allow".to_string(),
            rule: None,
            detail: narrative.to_string(),
        },
    }
}

/// The ordered gate verdicts a run produces. PURE and deterministic: it runs the
/// planted calls through the real gate and records the genuine decisions. Separated
/// from the timed executor so it is unit-testable without sleeping.
pub fn run_event_script() -> Vec<GateEvent> {
    let rules = enforced_gate_rules();
    vec![
        gate_event(
            1,
            &rules,
            &path_escape_call(),
            "Frontend attempted a write that climbs out of the workspace.",
        ),
        gate_event(
            2,
            &rules,
            &secret_call(),
            "Frontend tried to hardcode an API token into the export config.",
        ),
        gate_event(
            3,
            &rules,
            &clean_call(),
            "Backend wrote the repository method; clean.",
        ),
    ]
}

/// Walk a run to completion, emitting the real gate verdicts with calm pacing so the
/// cockpit can render the progression live. The verdicts come from
/// [`run_event_script`] (the real gate); only the pacing lives here.
pub async fn execute_run(store: RunStore, run_id: String) {
    let beat = Duration::from_millis(550);

    store.set_status(&run_id, RunStatus::Executing, false);
    tokio::time::sleep(beat).await;

    for event in run_event_script() {
        store.push_event(&run_id, event);
        tokio::time::sleep(beat).await;
    }

    store.set_status(&run_id, RunStatus::Gating, false);
    tokio::time::sleep(beat).await;

    store.set_status(&run_id, RunStatus::AwaitingQa, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_script_uses_real_gate_verdicts() {
        let events = run_event_script();
        assert_eq!(events.len(), 3);

        // The path-escape write is denied by the real path-escape rule.
        assert_eq!(events[0].verdict, "deny");
        assert_eq!(events[0].rule.as_deref(), Some("SEC-NO-PATH-ESCAPE-1"));

        // The hardcoded secret is denied by the real secrets rule.
        assert_eq!(events[1].verdict, "deny");
        assert_eq!(events[1].rule.as_deref(), Some("SEC-NO-HARDCODED-SECRETS-1"));

        // The clean write is allowed.
        assert_eq!(events[2].verdict, "allow");
        assert!(events[2].rule.is_none());
    }

    #[test]
    fn run_store_create_and_get() {
        let store = RunStore::new();
        let id = store.create("CAM-1");
        let run = store.get(&id).expect("run exists");
        assert_eq!(run.story_id, "CAM-1");
        assert_eq!(run.status, RunStatus::Planned);
        assert!(run.events.is_empty());
        assert!(store.get("nope").is_none());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn execute_run_walks_to_awaiting_qa_with_real_denies() {
        let store = RunStore::new();
        let id = store.create("CAM-1");
        // start_paused auto-advances tokio time, so the sleeps resolve instantly.
        execute_run(store.clone(), id.clone()).await;

        let run = store.get(&id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        assert_eq!(run.events.len(), 3);
        let denies = run.events.iter().filter(|e| e.verdict == "deny").count();
        assert_eq!(denies, 2);
    }
}
