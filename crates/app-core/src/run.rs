//! Run-engine DOMAIN types + pure functions (issue #117, Phase 3a).
//!
//! A "run" is a governed execution of a story. This module owns the framework-agnostic
//! data shapes (`Run`, `RunStatus`, `GateEvent`, `RunProvenance`, `StallDecision`, …) and
//! the pure functions over them: provenance summarization (`run_provenance` /
//! `provenance_markdown`), stall math (`idle_ms` / `is_stalled` / `stall_decision`), and
//! the env-flag readers (`live_mode_enabled` / `run_stall_threshold_ms`). All are
//! transport-free (RUST-HEADLESS-CORE-1): NO axum, NO rmcp, NO gateway.
//!
//! The `RunStore` (the `Arc<Mutex>` stores + `AbortHandle`), the scripted gate fixtures
//! (`camerata_gateway::evaluate_call`), and the async `execute_run` executor (tokio +
//! transcripts) stay in the `camerata-server` adapter, which re-exports the items here so
//! existing `crate::run::X` call sites are unchanged.

use serde::{Deserialize, Serialize};

use camerata_core::RuleId;
use camerata_liveness::LivenessTracker;

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
    /// The run is PAUSED on a human-review escalation raised mid-run (e.g. the test-tamper
    /// guard). The open UoW escalation (in the escalation store) is the pause point and a
    /// [`crate::checkpoint::Checkpoint`] holds the resumable state; the run RESUMES (re-spawns
    /// from the checkpoint with the human's directive) when the escalation is resolved. Like
    /// `AwaitingClarification`, a run in this state is not `done`: it is parked, waiting on the
    /// human. Distinct from `AwaitingClarification` because the pause point is an escalation
    /// (Governed Development review), not a clarifying question in the clarify store.
    AwaitingReview,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(is_stalled(120_001, threshold)); // strictly greater = stalled
    }

    // ── pure struct-literal coverage for the moved domain logic ───────────────

    /// Build a `Run` by struct literal (no store, no gate) with hand-built GateEvents,
    /// and assert `run_provenance` + `provenance_markdown` over an explicit `&[RuleId]`.
    #[test]
    fn provenance_summarizes_from_struct_literal_run() {
        let run = Run {
            id: "run-42".to_string(),
            story_id: "CAM-42".to_string(),
            status: RunStatus::AwaitingQa,
            events: vec![
                GateEvent {
                    seq: 1,
                    layer: "layer-1".to_string(),
                    verdict: "deny".to_string(),
                    rule: Some("SEC-NO-PATH-ESCAPE-1".to_string()),
                    detail: "path escape".to_string(),
                    content_hash: None,
                },
                GateEvent {
                    seq: 2,
                    layer: "layer-1".to_string(),
                    verdict: "deny".to_string(),
                    rule: Some("SEC-NO-HARDCODED-SECRETS-1".to_string()),
                    detail: "hardcoded secret".to_string(),
                    content_hash: None,
                },
                // A duplicate deny of the first rule: rules_fired must stay distinct.
                GateEvent {
                    seq: 3,
                    layer: "layer-1".to_string(),
                    verdict: "deny".to_string(),
                    rule: Some("SEC-NO-PATH-ESCAPE-1".to_string()),
                    detail: "path escape again".to_string(),
                    content_hash: None,
                },
                GateEvent {
                    seq: 4,
                    layer: "layer-1".to_string(),
                    verdict: "allow".to_string(),
                    rule: None,
                    detail: "clean write".to_string(),
                    content_hash: None,
                },
            ],
            done: true,
            mode: "scripted".to_string(),
            tracker: LivenessTracker::new(),
            last_progress_label: "done".to_string(),
            kind: RunKind::Watched,
            stall_policy: StallPolicy::Alert,
            failure_reason: None,
        };

        let rules_in_force = vec![
            RuleId("SEC-NO-PATH-ESCAPE-1".to_string()),
            RuleId("SEC-NO-HARDCODED-SECRETS-1".to_string()),
            RuleId("ARCH-1".to_string()),
        ];
        let prov = run_provenance(&run, &rules_in_force);

        assert_eq!(prov.run_id, "run-42");
        assert_eq!(prov.story_id, "CAM-42");
        assert_eq!(prov.mode, "scripted");
        assert_eq!(prov.status, RunStatus::AwaitingQa);

        // Tallies: 3 denies, 1 allow; total_bounces mirrors deny_count.
        assert_eq!(prov.deny_count, 3);
        assert_eq!(prov.allow_count, 1);
        assert_eq!(prov.total_bounces, 3);

        // Distinct denying rules, in first-seen order (the duplicate collapses).
        assert_eq!(
            prov.rules_fired,
            vec![
                "SEC-NO-PATH-ESCAPE-1".to_string(),
                "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            ]
        );

        // Rules in force is the exact slice passed in.
        assert_eq!(prov.rules_in_force.len(), 3);
        assert!(prov.rules_in_force.iter().any(|r| r == "ARCH-1"));

        let md = provenance_markdown(&prov);
        assert!(md.contains("3 total bounces"));
        assert!(md.contains("SEC-NO-PATH-ESCAPE-1"));
        assert!(md.contains("run-42"));
        assert!(md.contains("Rules in force (3)"));
    }

    /// Build struct-literal Runs and assert `stall_decision`: below-threshold -> Ok,
    /// Watched-over-threshold -> Alert, Autonomous-over-threshold -> Cancel. Uses
    /// `LivenessTracker::with_initial_ms` so idle is deterministic (no wall clock).
    #[test]
    fn stall_decision_from_struct_literal_runs() {
        // Helper: a Run whose tracker last ticked at `last_ms`, with the given policy.
        let mk = |last_ms: u64, policy: StallPolicy| Run {
            id: "run-1".to_string(),
            story_id: "CAM-1".to_string(),
            status: RunStatus::Executing,
            events: Vec::new(),
            done: false,
            mode: "live".to_string(),
            tracker: LivenessTracker::with_initial_ms(last_ms),
            last_progress_label: "working".to_string(),
            kind: RunKind::Watched,
            stall_policy: policy,
            failure_reason: None,
        };

        let threshold = 120_000u128;

        // Below threshold (idle 50_000 < 120_000): Ok, regardless of policy.
        let watched_ok = mk(1_000, StallPolicy::Alert);
        assert_eq!(
            stall_decision(&watched_ok, threshold, 51_000),
            StallDecision::Ok
        );

        // Watched, over threshold (idle 200_000 > 120_000): Alert.
        let watched = mk(1_000, StallPolicy::Alert);
        assert_eq!(
            stall_decision(&watched, threshold, 201_000),
            StallDecision::Alert
        );

        // Autonomous, over threshold: Cancel.
        let autonomous = mk(1_000, StallPolicy::Cancel);
        assert_eq!(
            stall_decision(&autonomous, threshold, 201_000),
            StallDecision::Cancel
        );
    }
}
