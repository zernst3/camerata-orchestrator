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

use camerata_core::{Decision, RuleId, ToolCall};
use camerata_gateway::{enforced_gate_rules, evaluate_call};
use camerata_liveness::LivenessTracker;

// The run-engine DOMAIN types + pure functions live in `camerata-app-core::run` (Phase 3a,
// issue #117); they carry no transport/gateway/tokio coupling. Re-exported here so every
// existing `crate::run::X` call site (handlers, routine store, scheduler) is unchanged.
pub use camerata_app_core::run::{
    idle_ms, is_stalled, live_mode_enabled, provenance_markdown, run_provenance,
    run_stall_threshold_ms, stall_decision, DEFAULT_RUN_STALL_THRESHOLD_MS, GateEvent, Run,
    RunKind, RunProvenance, RunStatus, StallDecision, StallPolicy,
};

use crate::transcript::{generated_prompt, AgentTranscript, TranscriptStore};

/// Parse the monotonic sequence number out of a `run-N` id (see [`RunStore::create`]).
/// Returns `0` for an id that doesn't match the pattern (defensive; should not happen for
/// ids this store minted) so ordering degrades gracefully instead of panicking.
fn run_id_seq(id: &str) -> usize {
    id.strip_prefix("run-").and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// In-memory store of runs, shared into the background executor and the handlers.
#[derive(Clone, Default)]
pub struct RunStore {
    runs: Arc<Mutex<HashMap<String, Run>>>,
    counter: Arc<AtomicUsize>,
    cancel_signals: Arc<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
    /// Abort handles for the `tokio::spawn`ed task driving each run. Aborting the task
    /// drops the agent driver future, and the driver spawns its `claude` subprocess with
    /// `kill_on_drop(true)`, so the child process is reaped when the task is aborted.
    /// This is what lets a Stop request reach a run that is blocked inside a live agent
    /// subprocess (the between-step `is_cancelled` checks only fire when the loop is
    /// running). Registered when the run task is spawned; removed when it finishes.
    abort_handles: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>,
    /// Per-run completion signal (LIFECYCLE-3). Notified the instant a run reaches a
    /// terminal (`done = true`) state — success (`AwaitingQa`), `Failed`, or `Cancelled`.
    /// The provenance-stamping watcher awaits this instead of polling for 5 minutes, so a
    /// live run that legitimately outlives the old poll budget still gets stamped. Created
    /// lazily in [`Self::create`]; `Notify` retains a single permit, so a terminal state
    /// reached BEFORE the watcher starts awaiting is not lost.
    completion: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>>,
}

impl RunStore {
    pub fn new() -> Self {
        Self {
            runs: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicUsize::new(0)),
            cancel_signals: Arc::new(Mutex::new(HashMap::new())),
            abort_handles: Arc::new(Mutex::new(HashMap::new())),
            completion: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a Planned run for `story_id` in the given mode ("scripted" | "live") and
    /// return its id.
    pub fn create(&self, story_id: &str, mode: &str, kind: RunKind) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("run-{n}");
        let stall_policy = match kind {
            RunKind::Autonomous => StallPolicy::Cancel,
            RunKind::Watched => StallPolicy::Alert,
        };
        let run = Run {
            id: id.clone(),
            story_id: story_id.to_string(),
            status: RunStatus::Planned,
            events: Vec::new(),
            done: false,
            mode: mode.to_string(),
            // LivenessTracker::new() initialises to the current wall clock (not stalled).
            tracker: LivenessTracker::new(),
            last_progress_label: "created".to_string(),
            kind: kind.clone(),
            stall_policy,
            failure_reason: None,
        };
        if let Ok(mut guard) = self.runs.lock() {
            guard.insert(id.clone(), run);
        }
        // Register a cancel signal for this run.
        if let Ok(mut signals) = self.cancel_signals.lock() {
            signals.insert(
                id.clone(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
        }
        // Register a completion signal (LIFECYCLE-3): notified when the run goes terminal.
        if let Ok(mut completion) = self.completion.lock() {
            completion.insert(id.clone(), Arc::new(tokio::sync::Notify::new()));
        }
        id
    }

    pub fn get(&self, id: &str) -> Option<Run> {
        self.runs.lock().ok()?.get(id).cloned()
    }

    /// LIFECYCLE-9 (single-flight guard): return the FIRST active (non-`done`) run on
    /// `story_id`, if any. "Active" is simply `!run.done` — a run is done only once it
    /// reaches a terminal state (AwaitingQa success, Failed, Cancelled) or is marked done
    /// (a superseded paused run). A story with an active run must not start a second run
    /// (two runs would share one worktree) and its worktree must not be torn down.
    /// Returns a cloned snapshot so the caller holds no lock.
    pub fn active_run_for_story(&self, story_id: &str) -> Option<Run> {
        let guard = self.runs.lock().ok()?;
        guard
            .values()
            .find(|r| r.story_id == story_id && !r.done)
            .cloned()
    }

    /// BUG B: the MOST RECENT run for `story_id` in the given `mode` (e.g.
    /// `"investigation"`), regardless of whether it is done. Unlike
    /// [`Self::active_run_for_story`] this DOES return a terminal (`Failed`/`AwaitingQa`)
    /// run — it exists so a failed/empty investigation's reason can be surfaced on the
    /// empty-state UI even after the page reloads and the session-only `active_run` signal
    /// is gone. "Most recent" = the highest numeric suffix of the `run-N` id, which is
    /// equivalent to insertion order since ids are minted from a monotonic counter
    /// ([`Self::create`]). Returns a cloned snapshot so the caller holds no lock.
    pub fn latest_run_for_story_and_mode(&self, story_id: &str, mode: &str) -> Option<Run> {
        let guard = self.runs.lock().ok()?;
        guard
            .values()
            .filter(|r| r.story_id == story_id && r.mode == mode)
            .max_by_key(|r| run_id_seq(&r.id))
            .cloned()
    }

    /// LIFECYCLE-6 (stall sweep): snapshot every ACTIVE (non-`done`) run. Returns cloned
    /// snapshots so the caller (the background stall sweep) holds no lock while it evaluates
    /// each run's stall decision and, when needed, cancels it. Done runs are excluded because
    /// a terminal run can never stall.
    pub fn snapshot_active(&self) -> Vec<Run> {
        match self.runs.lock() {
            Ok(guard) => guard.values().filter(|r| !r.done).cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Return `true` when this status is a TERMINAL run status that must never be
    /// overwritten by a late executor (LIFECYCLE-1 / LIFECYCLE-2): an explicit
    /// `Cancelled` or `Failed`. `AwaitingQa` (the success terminal) is intentionally NOT
    /// here — it is still reached via the normal `set_status(.., true)` success path, and
    /// the `done` guard below prevents any further mutation once it lands.
    fn is_terminal_status(status: &RunStatus) -> bool {
        matches!(status, RunStatus::Cancelled | RunStatus::Failed { .. })
    }

    /// Notify the per-run completion signal (LIFECYCLE-3). Called by every terminal setter
    /// so a `wait_until_done` awaiter wakes the instant the run finishes.
    fn signal_completion(&self, id: &str) {
        if let Ok(completion) = self.completion.lock() {
            if let Some(n) = completion.get(id) {
                n.notify_one();
            }
        }
    }

    pub(crate) fn set_status(&self, id: &str, status: RunStatus, done: bool) {
        let mut became_done = false;
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                // TERMINAL GUARD (LIFECYCLE-1): once a run is done, or has been explicitly
                // Cancelled/Failed, refuse any further status mutation. This stops a late
                // executor (one that kept running past a cancel/fail) from resurrecting a
                // terminal run and, e.g., advancing it to AwaitingQa as if it had succeeded.
                if run.done || Self::is_terminal_status(&run.status) {
                    return;
                }
                run.status = status;
                run.done = done;
                became_done = done;
            }
        }
        if became_done {
            self.signal_completion(id);
        }
    }

    /// Mark a run terminal (`done = true`) WITHOUT altering its status — e.g. a paused run
    /// (`AwaitingReview`) whose review has been resolved and is now superseded by a fresh resume
    /// run. Preserving the last status keeps the history honest (it really did stop at review),
    /// while `done = true` stops it lingering as an open run forever.
    pub fn mark_done(&self, id: &str) {
        let mut became_done = false;
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                became_done = !run.done;
                run.done = true;
            }
        }
        if became_done {
            self.signal_completion(id);
        }
    }

    pub(crate) fn push_event(&self, id: &str, event: GateEvent) {
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                let label = format!("{} {}", event.layer, event.verdict);
                // Update the tracker (atomic, lock-free) and the label field together.
                run.tracker.record_progress(&label);
                run.last_progress_label = label;
                run.events.push(event);
            }
        }
    }

    /// Update the run's last-activity timestamp to now and optionally set the progress label.
    /// Called by `push_event` (automatic) and by the agent heartbeat callback (live runs).
    pub(crate) fn touch_activity(&self, id: &str, label: Option<String>) {
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                if let Some(l) = label {
                    run.tracker.record_progress(&l);
                    run.last_progress_label = l;
                } else {
                    run.tracker.tick();
                }
            }
        }
    }

    /// Mark a run as failed with a human-readable reason. Sets `done = true` and a genuine
    /// `Failed` terminal status (LIFECYCLE-2: a failure is NOT a success). Idempotent
    /// terminal: an already-terminal run (done / Cancelled / Failed) is left untouched so a
    /// late failure can't clobber a cancel, and a cancel can't be relabelled a failure.
    pub fn fail_with_reason(&self, id: &str, reason: String) {
        let mut became_done = false;
        {
            let mut runs = self.runs.lock().unwrap();
            if let Some(run) = runs.get_mut(id) {
                if run.done || Self::is_terminal_status(&run.status) {
                    return;
                }
                run.status = RunStatus::Failed { reason: reason.clone() };
                run.failure_reason = Some(reason);
                run.done = true;
                became_done = true;
            }
        }
        if became_done {
            self.signal_completion(id);
        }
    }

    /// Register the abort handle for the task driving `id`. Called right after the run
    /// task is `tokio::spawn`ed so a later [`Self::cancel`] can abort it (reaping any live
    /// agent subprocess via `kill_on_drop`). Replacing an existing handle is fine (a run
    /// is driven by one task).
    pub fn register_abort(&self, id: &str, handle: tokio::task::AbortHandle) {
        if let Ok(mut handles) = self.abort_handles.lock() {
            handles.insert(id.to_string(), handle);
        }
    }

    /// Drop the abort handle for `id` (the task finished on its own). Idempotent.
    pub fn clear_abort(&self, id: &str) {
        if let Ok(mut handles) = self.abort_handles.lock() {
            handles.remove(id);
        }
    }

    /// Cancel a run: set the atomic cancel signal, abort the driving task (which drops the
    /// agent driver future and kills its `claude` subprocess via `kill_on_drop`), update
    /// status to Cancelled, and mark `done = true`. The run loops also check `is_cancelled`
    /// between steps so a cancel between subprocess spawns is honored without an abort.
    /// Idempotent: a run already done/cancelled is left in a terminal state.
    pub fn cancel(&self, id: &str) {
        // Set the atomic cancel signal first (between-step checks read this).
        {
            let signals = self.cancel_signals.lock().unwrap();
            if let Some(sig) = signals.get(id) {
                sig.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        // Abort the driving task so a run blocked inside a live agent subprocess is reaped
        // immediately (the dropped driver future kills the kill_on_drop child).
        {
            let mut handles = self.abort_handles.lock().unwrap();
            if let Some(handle) = handles.remove(id) {
                handle.abort();
            }
        }
        // Update the run record to a terminal Cancelled state so GET /api/runs/:id reports
        // it (the aborted task can no longer set status itself).
        let mut became_done = false;
        {
            let mut runs = self.runs.lock().unwrap();
            if let Some(run) = runs.get_mut(id) {
                // Don't clobber an already-terminal failure/cancel.
                if !run.done {
                    run.status = RunStatus::Cancelled;
                    run.done = true;
                    became_done = true;
                }
            }
        }
        if became_done {
            self.signal_completion(id);
        }
    }

    /// Return `true` when a cancel signal has been set for this run.
    pub fn is_cancelled(&self, id: &str) -> bool {
        let signals = self.cancel_signals.lock().unwrap();
        signals
            .get(id)
            .map(|sig| sig.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Await the run's terminal (`done`) state and return the final [`Run`] snapshot
    /// (LIFECYCLE-3). Completion-driven: the future resolves the instant a terminal setter
    /// (`set_status(.., true)`, `fail_with_reason`, `cancel`, `mark_done`) fires the
    /// per-run completion signal, so a live run that outlives the old 5-minute poll budget
    /// is still stamped. `safety_timeout` is a backstop, NOT the normal path: if it elapses
    /// (the run wedged without ever going terminal, or the run id is unknown) the future
    /// resolves to `None` and the caller stamps nothing. The completion `Notify` retains a
    /// permit, so a run that went terminal BEFORE this is called still wakes immediately.
    pub async fn wait_until_done(&self, id: &str, safety_timeout: Duration) -> Option<Run> {
        // Grab the per-run notifier up front. Missing → unknown run id; nothing to await.
        let notify = {
            let completion = self.completion.lock().ok()?;
            completion.get(id).cloned()?
        };
        loop {
            // Register interest BEFORE the done-check to close the notify/observe race: if
            // the run goes terminal between this and the check below, the retained permit
            // makes the subsequent `notified().await` return immediately.
            let notified = notify.notified();
            if let Some(run) = self.get(id) {
                if run.done {
                    return Some(run);
                }
            } else {
                return None;
            }
            match tokio::time::timeout(safety_timeout, notified).await {
                Ok(()) => {
                    // Woken by a terminal transition; re-check on the next loop turn.
                    continue;
                }
                Err(_) => {
                    // Backstop elapsed. Return the terminal run if it somehow finished
                    // without notifying, else `None` (wedged / unknown).
                    return self.get(id).filter(|r| r.done);
                }
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
            // Scripted runs have no raw content to hash (the content is test fixture data
            // embedded in the call builder). content_hash remains None for scripted events;
            // it is populated only for LIVE gateway events via gate_record_to_event.
            content_hash: None,
        },
        Decision::Allow => GateEvent {
            seq,
            layer: "layer-1".to_string(),
            verdict: "allow".to_string(),
            rule: None,
            detail: narrative.to_string(),
            content_hash: None,
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

/// The agents the scripted run models, and which script events each produced — so the
/// transcript can show the GENERATED prompt + the agent's actions and the REAL gate
/// verdict. Index references into [`run_event_script`].
struct ScriptedAgent {
    session_id: &'static str,
    role: &'static str,
    task: &'static str,
    /// `(event index, action label)` pairs this agent produced.
    steps: &'static [(usize, &'static str)],
}

fn scripted_agents() -> [ScriptedAgent; 2] {
    [
        ScriptedAgent {
            session_id: "frontend-1",
            role: "Frontend engineer",
            task: "Wire the member-export action in the UI and its export config.",
            steps: &[
                (
                    0,
                    "Write a workspace-relative payload file (attempted path-escape)",
                ),
                (
                    1,
                    "Write crates/api/src/export_config.rs (attempted hardcoded token)",
                ),
            ],
        },
        ScriptedAgent {
            session_id: "backend-1",
            role: "Backend engineer",
            task: "Implement the members-repository export method.",
            steps: &[(
                2,
                "Write crates/api/src/members_repo.rs (repository method)",
            )],
        },
    ]
}

/// Walk a run to completion, emitting the real gate verdicts with calm pacing so the
/// cockpit can render the progression live. The verdicts come from
/// [`run_event_script`] (the real gate); only the pacing lives here. As it walks, it
/// also fills the per-agent transcripts (the generated prompt + each agent's actions
/// and the real verdict) so the Agent-activity drawer can show what Camerata told its
/// agents — the otherwise-hidden prompting.
pub async fn execute_run(store: RunStore, transcripts: TranscriptStore, run_id: String) {
    let beat = Duration::from_millis(550);
    let story_id = store.get(&run_id).map(|r| r.story_id).unwrap_or_default();
    let events = run_event_script();
    let agents = scripted_agents();

    // Register each agent with its GENERATED prompt up front (status: running).
    for a in &agents {
        transcripts.register(
            &run_id,
            AgentTranscript {
                session_id: a.session_id.to_string(),
                role: a.role.to_string(),
                prompt: generated_prompt(a.role, &story_id, a.task),
                output: String::new(),
                status: "running".to_string(),
            },
        );
    }

    store.set_status(&run_id, RunStatus::Executing, false);
    tokio::time::sleep(beat).await;

    for (idx, event) in events.iter().enumerate() {
        // Append this event to the agent that produced it, with the real verdict.
        if let Some((agent, action)) = agents.iter().find_map(|a| {
            a.steps
                .iter()
                .find(|(i, _)| *i == idx)
                .map(|(_, act)| (a, *act))
        }) {
            transcripts.append_output(&run_id, agent.session_id, &format!("→ {action}"));
            let line = if event.verdict == "deny" {
                format!(
                    "   ✗ GATE DENIED [{}] — {}",
                    event.rule.clone().unwrap_or_default(),
                    event.detail
                )
            } else {
                format!("   ✓ gate allowed — {}", event.detail)
            };
            transcripts.append_output(&run_id, agent.session_id, &line);
        }
        store.push_event(&run_id, event.clone());
        tokio::time::sleep(beat).await;
    }

    // Final per-agent status: blocked if any of its writes were denied, else done.
    for a in &agents {
        let blocked = a
            .steps
            .iter()
            .any(|(i, _)| events.get(*i).map(|e| e.verdict == "deny").unwrap_or(false));
        transcripts.set_status(
            &run_id,
            a.session_id,
            if blocked { "blocked" } else { "done" },
        );
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
        assert_eq!(
            events[1].rule.as_deref(),
            Some("SEC-NO-HARDCODED-SECRETS-1")
        );

        // The clean write is allowed.
        assert_eq!(events[2].verdict, "allow");
        assert!(events[2].rule.is_none());
    }

    #[test]
    fn provenance_summarizes_rules_tallies_and_bounces() {
        // Build a run whose verdicts are the REAL gate's over the planted script:
        // two denies (path-escape, hardcoded-secret) and one allow.
        let store = RunStore::new();
        let id = store.create("CAM-7", "scripted", RunKind::Watched);
        for ev in run_event_script() {
            store.push_event(&id, ev);
        }
        store.set_status(&id, RunStatus::AwaitingQa, true);
        let run = store.get(&id).expect("run exists");

        let rules = enforced_gate_rules();
        let prov = run_provenance(&run, &rules);

        assert_eq!(prov.run_id, id);
        assert_eq!(prov.story_id, "CAM-7");
        assert_eq!(prov.mode, "scripted");
        assert_eq!(prov.status, RunStatus::AwaitingQa);

        // Tallies: 1 allow, 2 deny; total_bounces mirrors deny_count.
        assert_eq!(prov.allow_count, 1);
        assert_eq!(prov.deny_count, 2);
        assert_eq!(prov.total_bounces, 2);

        // The two distinct rules that bounced a write, in first-seen order.
        assert_eq!(
            prov.rules_fired,
            vec![
                "SEC-NO-PATH-ESCAPE-1".to_string(),
                "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            ]
        );

        // Rules in force is the full enforced set (non-empty, includes the firers).
        assert_eq!(prov.rules_in_force.len(), rules.len());
        assert!(prov
            .rules_in_force
            .iter()
            .any(|r| r == "SEC-NO-PATH-ESCAPE-1"));

        // The PR-body markdown carries the honest accounting.
        let md = provenance_markdown(&prov);
        assert!(md.contains("2 total bounces"));
        assert!(md.contains("SEC-NO-PATH-ESCAPE-1"));
        assert!(md.contains(&id));
    }

    #[test]
    fn provenance_with_no_denies_reports_zero_bounces() {
        let store = RunStore::new();
        let id = store.create("CAM-8", "scripted", RunKind::Watched);
        store.push_event(
            &id,
            GateEvent {
                seq: 1,
                layer: "layer-1".to_string(),
                verdict: "allow".to_string(),
                rule: None,
                detail: "clean write".to_string(),
                content_hash: None,
            },
        );
        let run = store.get(&id).expect("run exists");
        let prov = run_provenance(&run, &enforced_gate_rules());
        assert_eq!(prov.allow_count, 1);
        assert_eq!(prov.deny_count, 0);
        assert_eq!(prov.total_bounces, 0);
        assert!(prov.rules_fired.is_empty());
        assert!(provenance_markdown(&prov).contains("Rules that bounced a write: none"));
    }

    #[test]
    fn run_store_create_and_get() {
        let store = RunStore::new();
        let id = store.create("CAM-1", "scripted", RunKind::Watched);
        let run = store.get(&id).expect("run exists");
        assert_eq!(run.story_id, "CAM-1");
        assert_eq!(run.status, RunStatus::Planned);
        assert!(run.events.is_empty());
        assert!(store.get("nope").is_none());
    }

    // ── stall detection tests ─────────────────────────────────────────────────
    // NOTE: the pure `idle_ms` / `is_stalled` unit tests moved to
    // `camerata_app_core::run` alongside the functions themselves (Phase 3a, #117).
    // The store-based / gate-based tests below exercise the moved types + fns THROUGH
    // the re-export, so integration coverage of the run engine is unchanged here.

    #[test]
    fn push_event_updates_last_activity_ms() {
        let store = RunStore::new();
        let id = store.create("CAM-X", "scripted", RunKind::Watched);
        let before = store.get(&id).unwrap().tracker.last_activity_ms();

        // Tiny sleep to ensure time advances.
        std::thread::sleep(std::time::Duration::from_millis(5));

        store.push_event(&id, GateEvent {
            seq: 1,
            layer: "layer-1".to_string(),
            verdict: "allow".to_string(),
            rule: None,
            detail: "test".to_string(),
            content_hash: None,
        });
        let after = store.get(&id).unwrap().tracker.last_activity_ms();
        assert!(after >= before, "last_activity_ms must advance after push_event");
    }

    #[test]
    fn create_initializes_last_activity_ms() {
        let store = RunStore::new();
        let id = store.create("CAM-Y", "scripted", RunKind::Watched);
        let run = store.get(&id).unwrap();
        assert!(run.tracker.last_activity_ms() > 0, "last_activity_ms must be initialized");
        assert_eq!(run.last_progress_label, "created");
    }

    #[test]
    fn push_event_updates_last_progress_label() {
        let store = RunStore::new();
        let id = store.create("CAM-Z", "scripted", RunKind::Watched);
        store.push_event(&id, GateEvent {
            seq: 1,
            layer: "delegate".to_string(),
            verdict: "dispatch".to_string(),
            rule: None,
            detail: "dispatching".to_string(),
            content_hash: None,
        });
        let run = store.get(&id).unwrap();
        assert!(run.last_progress_label.contains("delegate"));
        assert!(run.last_progress_label.contains("dispatch"));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn execute_run_walks_to_awaiting_qa_with_real_denies() {
        let store = RunStore::new();
        let transcripts = TranscriptStore::new();
        let id = store.create("CAM-1", "scripted", RunKind::Watched);
        // start_paused auto-advances tokio time, so the sleeps resolve instantly.
        execute_run(store.clone(), transcripts.clone(), id.clone()).await;

        let run = store.get(&id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        assert_eq!(run.events.len(), 3);
        let denies = run.events.iter().filter(|e| e.verdict == "deny").count();
        assert_eq!(denies, 2);

        // Two agents got generated prompts + their actions/verdicts; the frontend
        // (which hit two denials) is blocked, the backend (clean) is done.
        let agents = transcripts.get(&id);
        assert_eq!(agents.len(), 2);
        let fe = agents
            .iter()
            .find(|a| a.session_id == "frontend-1")
            .unwrap();
        assert!(fe.prompt.contains("CAM-1"));
        assert!(fe.output.contains("GATE DENIED"));
        assert_eq!(fe.status, "blocked");
        let be = agents.iter().find(|a| a.session_id == "backend-1").unwrap();
        assert_eq!(be.status, "done");
    }

    #[test]
    fn watched_run_gets_alert_stall_policy() {
        let store = RunStore::new();
        let id = store.create("S-1", "live", RunKind::Watched);
        let run = store.get(&id).unwrap();
        assert_eq!(run.stall_policy, StallPolicy::Alert);
        assert_eq!(run.kind, RunKind::Watched);
    }

    #[test]
    fn autonomous_run_gets_cancel_stall_policy() {
        let store = RunStore::new();
        let id = store.create("S-2", "scripted", RunKind::Autonomous);
        let run = store.get(&id).unwrap();
        assert_eq!(run.stall_policy, StallPolicy::Cancel);
        assert_eq!(run.kind, RunKind::Autonomous);
    }

    #[test]
    fn cancel_sets_status_and_done() {
        let store = RunStore::new();
        let id = store.create("S-3", "live", RunKind::Watched);
        store.cancel(&id);
        let run = store.get(&id).unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.done);
        assert!(store.is_cancelled(&id));
    }

    #[tokio::test]
    async fn cancel_aborts_the_registered_task() {
        // A run driven by a long-lived spawned task: registering its abort handle and
        // then cancelling must abort the task (so a live agent subprocess would be reaped
        // via kill_on_drop) AND leave the run terminal.
        let store = RunStore::new();
        let id = store.create("S-ABORT", "live", RunKind::Watched);

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let handle = tokio::spawn(async move {
            // Sleep effectively forever; aborting drops this future.
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            // Only runs if NOT aborted — the test asserts this never fires.
            let _ = tx.send(());
        });
        store.register_abort(&id, handle.abort_handle());

        store.cancel(&id);

        // The task was aborted: awaiting it yields a cancelled JoinError.
        assert!(handle.await.unwrap_err().is_cancelled());
        // The run is terminal.
        let run = store.get(&id).unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.done);
        assert!(store.is_cancelled(&id));
        // The task body never completed.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn cancel_does_not_clobber_already_done_run() {
        // A run that already failed stays failed after a late cancel (idempotent terminal).
        let store = RunStore::new();
        let id = store.create("S-DONE", "live", RunKind::Watched);
        store.fail_with_reason(&id, "boom".to_string());
        store.cancel(&id);
        let run = store.get(&id).unwrap();
        assert_eq!(run.status, RunStatus::Failed { reason: "boom".to_string() });
        assert!(run.done);
        // The cancel signal is still set (between-step checks would see it), which is fine.
        assert!(store.is_cancelled(&id));
    }

    #[test]
    fn set_status_cannot_overwrite_a_terminal_cancelled_run() {
        // LIFECYCLE-1: a late executor calling set_status(AwaitingQa, true) on a run that
        // was already Cancelled must be a no-op — no resurrection to a success terminal.
        let store = RunStore::new();
        let id = store.create("S-GUARD-C", "live", RunKind::Watched);
        store.cancel(&id);
        // A stale executor tries to complete the run as if it succeeded.
        store.set_status(&id, RunStatus::AwaitingQa, true);
        let run = store.get(&id).unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.done);
    }

    #[test]
    fn set_status_cannot_overwrite_a_terminal_failed_run() {
        // LIFECYCLE-2: a failed run stays failed; a late AwaitingQa cannot relabel it.
        let store = RunStore::new();
        let id = store.create("S-GUARD-F", "live", RunKind::Watched);
        store.fail_with_reason(&id, "gateway binary missing".to_string());
        store.set_status(&id, RunStatus::AwaitingQa, true);
        let run = store.get(&id).unwrap();
        assert_eq!(
            run.status,
            RunStatus::Failed { reason: "gateway binary missing".to_string() }
        );
        assert!(run.done);
    }

    #[test]
    fn fail_with_reason_does_not_clobber_a_cancelled_run() {
        // A cancel that raced ahead of a failure wins: the run stays Cancelled.
        let store = RunStore::new();
        let id = store.create("S-GUARD-CF", "live", RunKind::Watched);
        store.cancel(&id);
        store.fail_with_reason(&id, "late failure".to_string());
        let run = store.get(&id).unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
    }

    #[tokio::test]
    async fn wait_until_done_resolves_on_terminal_transition() {
        // LIFECYCLE-3: the completion path resolves when the run goes terminal, WITHOUT a
        // wall-clock poll. A tiny background task flips the run to AwaitingQa; the awaiter
        // wakes on the signal (safety timeout is a generous backstop, never the path).
        let store = RunStore::new();
        let id = store.create("S-WAIT", "live", RunKind::Watched);
        let store2 = store.clone();
        let id2 = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            store2.set_status(&id2, RunStatus::AwaitingQa, true);
        });
        let run = store
            .wait_until_done(&id, Duration::from_secs(30))
            .await
            .expect("terminal run");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
    }

    #[tokio::test]
    async fn wait_until_done_returns_immediately_when_already_terminal() {
        // The Notify retains a permit: a run that went terminal BEFORE the awaiter starts
        // is not lost — wait_until_done returns the terminal snapshot right away.
        let store = RunStore::new();
        let id = store.create("S-WAIT-PRE", "live", RunKind::Watched);
        store.fail_with_reason(&id, "boom".to_string());
        let run = store
            .wait_until_done(&id, Duration::from_secs(30))
            .await
            .expect("terminal run");
        assert!(matches!(run.status, RunStatus::Failed { .. }));
    }

    #[test]
    fn fail_with_reason_sets_status_and_reason() {
        let store = RunStore::new();
        let id = store.create("S-4", "live", RunKind::Watched);
        store.fail_with_reason(&id, "timeout".to_string());
        let run = store.get(&id).unwrap();
        assert_eq!(run.status, RunStatus::Failed { reason: "timeout".to_string() });
        assert_eq!(run.failure_reason.as_deref(), Some("timeout"));
        assert!(run.done);
    }

    #[test]
    fn stall_decision_ok_below_threshold() {
        let store = RunStore::new();
        let id = store.create("S-5", "live", RunKind::Watched);
        let run = store.get(&id).unwrap();
        let now_ms = u128::from(run.tracker.last_activity_ms()) + 50_000;
        assert_eq!(stall_decision(&run, 120_000, now_ms), StallDecision::Ok);
    }

    #[test]
    fn stall_decision_alert_for_watched_run() {
        let store = RunStore::new();
        let id = store.create("S-6", "live", RunKind::Watched);
        let run = store.get(&id).unwrap();
        let now_ms = u128::from(run.tracker.last_activity_ms()) + 200_000;
        assert_eq!(stall_decision(&run, 120_000, now_ms), StallDecision::Alert);
    }

    #[test]
    fn stall_decision_cancel_for_autonomous_run() {
        let store = RunStore::new();
        let id = store.create("S-7", "scripted", RunKind::Autonomous);
        let run = store.get(&id).unwrap();
        let now_ms = u128::from(run.tracker.last_activity_ms()) + 700_000;
        assert_eq!(stall_decision(&run, 600_000, now_ms), StallDecision::Cancel);
    }
}
