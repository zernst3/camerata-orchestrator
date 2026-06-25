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

use serde::{Deserialize, Serialize};

use camerata_core::{Decision, RuleId, ToolCall};
use camerata_gateway::{enforced_gate_rules, evaluate_call};
use camerata_liveness::LivenessTracker;

use crate::transcript::{generated_prompt, AgentTranscript, TranscriptStore};

/// Whether a run is interactive (watched by the architect) or autonomous (walk-away/routine).
/// Determines which stall threshold and policy apply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Watched,
    Autonomous,
}

impl Default for RunKind {
    fn default() -> Self {
        RunKind::Watched
    }
}

/// What the server does when a run stalls (exceeds its idle threshold).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StallPolicy {
    /// Surface a stall alert to the UI; do NOT cancel the run.
    Alert,
    /// Automatically cancel the run when the stall threshold is exceeded.
    Cancel,
}

impl Default for StallPolicy {
    fn default() -> Self {
        StallPolicy::Alert
    }
}

/// The lifecycle status of a run, in Camerata's vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Planned,
    Executing,
    Gating,
    /// Phase 3b: the run is PAUSED on a structured clarifying question the gated agent
    /// raised mid-run. The open clarification (in the 3a clarify store) is the pause
    /// point, auto-saved; the run resumes when a human answers it. A run in this state is
    /// not `done`: it is parked, waiting on the human.
    AwaitingClarification,
    AwaitingQa,
    /// The run failed with a human-readable reason (e.g. stall timeout, infra error).
    Failed { reason: String },
    /// The run was explicitly cancelled (by the architect or by automatic stall policy).
    Cancelled,
}

/// One real gate verdict recorded during a run.
///
/// Reused, by design, for ALL of the dev-cycle observability layers (not just the
/// layer-1 gate): the `layer` field discriminates the source ("layer-1" = the
/// deny-before-execute gate; "layer-2" = the post-task lint/test check; "delegate" =
/// delegation dispatch/return; "tier" = the model routing for a spawned agent;
/// "stage"/"fleet" = lifecycle), and `verdict` carries the per-layer outcome
/// ("allow"/"deny" for the gate; "pass"/"fail" for layer-2; "info"/"dispatch" etc.
/// elsewhere). No new field is needed — the UI keys off `layer` + `verdict`.
#[derive(Clone, Serialize)]
pub struct GateEvent {
    pub seq: usize,
    /// Which observability layer produced it (see the struct doc).
    pub layer: String,
    /// The per-layer outcome (see the struct doc).
    pub verdict: String,
    /// The rule id that denied / the rules a layer-2 check flagged, when applicable.
    pub rule: Option<String>,
    /// Human-readable narrative plus the gate's own reason text.
    pub detail: String,
    /// FNV-1a hex hash of the denied write's content (NEVER the raw content).
    /// Present only on layer-1 deny events sourced from the LIVE gateway JSONL sink.
    /// None for scripted runs, allow events, and non-content events (delegate, fleet).
    /// Carried here so run-finalization capture can write it to the enforcement ledger
    /// without re-reading the original denied content.
    #[serde(default)]
    pub content_hash: Option<String>,
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
    /// "scripted" (token-free, real-gate verdicts) or "live" (a real claude -p fleet).
    pub mode: String,
    /// Liveness tracker: replaces the previous `last_activity_ms: u128` field. Thread-safe
    /// (`Arc<AtomicU64>`), cheap to clone. `#[serde(skip)]` — not sent on the wire directly;
    /// callers read the computed `idle_ms` / `stalled` from `RunStatusResponse` instead.
    #[serde(skip)]
    pub tracker: LivenessTracker,
    /// A short human-readable label of the most recent progress point (the kind/summary of
    /// the last gate event, or `"agent: <last line truncated>"` from a heartbeat). For
    /// operator diagnosis when a run stalls. Mirrors `tracker.last_label()` but kept as a
    /// dedicated field so it serializes on the wire without an extra method call.
    pub last_progress_label: String,
    /// Whether this run is interactive (Watched) or autonomous (Autonomous).
    pub kind: RunKind,
    /// What the server does when this run stalls.
    pub stall_policy: StallPolicy,
    /// Human-readable reason for a `Failed` status (mirrors `RunStatus::Failed.reason`
    /// for convenience — the UI reads this field without matching the enum variant).
    pub failure_reason: Option<String>,
}

/// The provenance summary for a run (issue #21): which rules were in force, the
/// gate deny/allow tallies, and the total bounces (denials). This is the durable
/// record an architect reads before signing a run off — the honest accounting of
/// what the gate actually did, derived from the run's REAL verdicts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunProvenance {
    /// The run this provenance describes.
    pub run_id: String,
    /// The story the run governed.
    pub story_id: String,
    /// "scripted" (token-free, real-gate verdicts) or "live".
    pub mode: String,
    /// The run's terminal/current status, snake_case.
    pub status: RunStatus,
    /// The rule ids that were IN FORCE for the run (the gate's enforced set).
    pub rules_in_force: Vec<String>,
    /// How many gate verdicts denied a write.
    pub deny_count: usize,
    /// How many gate verdicts allowed a write.
    pub allow_count: usize,
    /// Total bounces: the count of denied writes the gate sent back (== `deny_count`,
    /// surfaced as its own field because "bounces" is the architect-facing vocabulary).
    pub total_bounces: usize,
    /// The distinct rule ids that actually FIRED a denial, in first-seen order.
    pub rules_fired: Vec<String>,
}

/// Compute the provenance summary for a run. PURE: derived entirely from the run's
/// recorded verdicts plus the supplied enforced-rule set, so it is unit-testable
/// without a gate or a clock. `rules_in_force` is passed in (rather than read from
/// the gateway here) so the caller controls the source of truth and tests stay pure.
pub fn run_provenance(run: &Run, rules_in_force: &[RuleId]) -> RunProvenance {
    let deny_count = run.events.iter().filter(|e| e.verdict == "deny").count();
    let allow_count = run.events.iter().filter(|e| e.verdict == "allow").count();

    // Distinct denying rule ids, in the order the gate first fired them.
    let mut rules_fired: Vec<String> = Vec::new();
    for ev in &run.events {
        if ev.verdict == "deny" {
            if let Some(rule) = &ev.rule {
                if !rules_fired.contains(rule) {
                    rules_fired.push(rule.clone());
                }
            }
        }
    }

    RunProvenance {
        run_id: run.id.clone(),
        story_id: run.story_id.clone(),
        mode: run.mode.clone(),
        status: run.status.clone(),
        rules_in_force: rules_in_force.iter().map(|r| r.0.clone()).collect(),
        deny_count,
        allow_count,
        total_bounces: deny_count,
        rules_fired,
    }
}

/// Render a run's provenance as a Markdown block suitable for a PR body. Camerata
/// never auto-opens PRs; when the architect explicitly opens one, this is folded in
/// so the PR carries the honest accounting of what the gate enforced and bounced.
pub fn provenance_markdown(p: &RunProvenance) -> String {
    let mut out = String::new();
    out.push_str("## Camerata governance provenance\n\n");
    out.push_str(&format!("- Run: `{}` (mode: {})\n", p.run_id, p.mode));
    out.push_str(&format!("- Story: `{}`\n", p.story_id));
    out.push_str(&format!(
        "- Gate verdicts: {} allowed, {} denied ({} total bounces)\n",
        p.allow_count, p.deny_count, p.total_bounces
    ));
    if p.rules_fired.is_empty() {
        out.push_str("- Rules that bounced a write: none\n");
    } else {
        out.push_str(&format!(
            "- Rules that bounced a write: {}\n",
            p.rules_fired.join(", ")
        ));
    }
    out.push_str(&format!(
        "- Rules in force ({}): {}\n",
        p.rules_in_force.len(),
        p.rules_in_force.join(", ")
    ));
    out
}

/// Whether the live-fleet run path is enabled (CAMERATA_LIVE_BUILD=1). Off by default,
/// so a run is the token-free scripted path unless explicitly opted in.
pub fn live_mode_enabled() -> bool {
    std::env::var("CAMERATA_LIVE_BUILD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
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
}

impl RunStore {
    pub fn new() -> Self {
        Self {
            runs: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicUsize::new(0)),
            cancel_signals: Arc::new(Mutex::new(HashMap::new())),
            abort_handles: Arc::new(Mutex::new(HashMap::new())),
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
        id
    }

    pub fn get(&self, id: &str) -> Option<Run> {
        self.runs.lock().ok()?.get(id).cloned()
    }

    pub(crate) fn set_status(&self, id: &str, status: RunStatus, done: bool) {
        if let Ok(mut guard) = self.runs.lock() {
            if let Some(run) = guard.get_mut(id) {
                run.status = status;
                run.done = done;
            }
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

    /// Mark a run as failed with a human-readable reason. Sets `done = true`.
    pub fn fail_with_reason(&self, id: &str, reason: String) {
        let mut runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get_mut(id) {
            run.status = RunStatus::Failed { reason: reason.clone() };
            run.failure_reason = Some(reason);
            run.done = true;
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
        let mut runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get_mut(id) {
            // Don't clobber an already-terminal failure/cancel.
            if !run.done {
                run.status = RunStatus::Cancelled;
                run.done = true;
            }
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
}

// ── stall detection pure functions ───────────────────────────────────────────

/// Threshold for declaring a run stalled: how long (in ms) `last_activity_ms` may be
/// idle before `is_stalled` returns `true`. Overridable via
/// `CAMERATA_RUN_STALL_THRESHOLD_SECS` (default: 120s = 120_000ms).
pub const DEFAULT_RUN_STALL_THRESHOLD_MS: u128 = 120_000;

/// Read the run stall threshold from the environment, returning milliseconds.
pub fn run_stall_threshold_ms() -> u128 {
    std::env::var("CAMERATA_RUN_STALL_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| s as u128 * 1_000)
        .unwrap_or(DEFAULT_RUN_STALL_THRESHOLD_MS)
}

/// Compute how many milliseconds have elapsed since `last_activity_ms`. Pure.
pub fn idle_ms(last_activity_ms: u128, now_ms: u128) -> u128 {
    now_ms.saturating_sub(last_activity_ms)
}

/// A run is stalled when it has been idle longer than the threshold. Pure.
pub fn is_stalled(idle_ms: u128, threshold_ms: u128) -> bool {
    idle_ms > threshold_ms
}

/// The outcome of a stall check: no action needed, alert the operator, or cancel the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallDecision {
    /// The run is not stalled; no action needed.
    Ok,
    /// The run is stalled and its policy says to alert (but not cancel).
    Alert,
    /// The run is stalled and its policy says to cancel it automatically.
    Cancel,
}

/// Determine what action to take given a run's current idle time and stall policy. Pure.
pub fn stall_decision(run: &Run, threshold_ms: u128, now_ms: u128) -> StallDecision {
    // Delegate idle computation to the tracker (u64 arithmetic, safe for wall-clock ms).
    let idle = u128::from(run.tracker.idle_ms(now_ms.try_into().unwrap_or(u64::MAX)));
    if idle < threshold_ms {
        StallDecision::Ok
    } else {
        match run.stall_policy {
            StallPolicy::Alert => StallDecision::Alert,
            StallPolicy::Cancel => StallDecision::Cancel,
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

    #[test]
    fn idle_ms_computes_elapsed() {
        assert_eq!(idle_ms(1000, 2500), 1500);
        assert_eq!(idle_ms(1000, 1000), 0); // no time passed
        assert_eq!(idle_ms(2000, 1000), 0); // saturating_sub: no underflow
    }

    #[test]
    fn is_stalled_threshold_boundary() {
        let threshold = 120_000u128;
        assert!(!is_stalled(0, threshold));
        assert!(!is_stalled(120_000, threshold)); // equal is NOT stalled
        assert!(is_stalled(120_001, threshold));  // strictly greater = stalled
    }

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
