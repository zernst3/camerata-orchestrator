//! The opt-in LIVE run path (#4 seam): run a real governed fleet for a story.
//!
//! Enabled only when `CAMERATA_LIVE_BUILD=1`. It builds a minimal `Plan` from the
//! story and runs the SAME real path `po-demo` exercises (`camerata_fleet::build_from_plan`):
//! scaffold a worktree, spawn a governed `claude -p` agent per task behind the gateway,
//! run cargo. BuildEvents are recorded as the run's gate activity.
//!
//! This is wired and compiled; its runtime proof is the operator's, because it needs
//! the gateway binary built, the `claude` CLI available, and tokens. The default
//! (scripted) path stays token-free and is the one verified in CI.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camerata_fleet::tier::TierMap;
use camerata_fleet::{
    build_from_plan_with_model_iterations_and_layer2, build_from_plan_with_tier_map_and_layer2,
    locate_gateway_bin, BuildEvent,
};
use camerata_intake::{Plan, PlanTask, TaskKind};

use crate::run::{GateEvent, RunStatus, RunStore};

/// Env the live executor sets so EVERY gateway subprocess in the fleet (the per-stage
/// agents and any delegate children, which inherit the server's process env) appends its
/// structured gate-decision records to ONE shared JSONL sink. The server tails that file
/// and folds each record into the run's event stream. Must match the gateway's
/// `GATE_EVENTS_FILE_ENV`. Observability only — it routes where decisions are RECORDED,
/// never what is decided.
const GATE_EVENTS_FILE_ENV: &str = "CAMERATA_GATE_EVENTS_FILE";

/// A structured gate-decision record as the gateway writes it to the JSONL sink. This
/// MIRRORS the gateway's `GateDecisionRecord` (the gateway is a binary crate, so its
/// type is not importable); the field names + `#[serde(default)]` on `kind` keep it
/// wire-compatible. Parsing-only: it carries no decision logic.
#[derive(Debug, Clone, serde::Deserialize)]
struct GateDecisionRecord {
    #[serde(default)]
    kind: String,
    verdict: String,
    target: String,
    rule: Option<String>,
    reason: String,
    #[allow(dead_code)]
    #[serde(default)]
    ts_ms: u128,
    /// FNV-1a hex hash of the denied write's content (NEVER the raw content).
    /// Set on DENY records only; `None` for allow / delegation records.
    /// Carried through to `GateEvent` for capture at run finalization.
    #[serde(default)]
    content_hash: Option<String>,
}

/// Map ONE gateway gate-decision JSONL record to a run [`GateEvent`]. PURE +
/// unit-testable: a faithful translation of a decision the gateway already made into the
/// run's event vocabulary. `seq` is supplied by the caller's shared counter.
///
/// - `kind == "gate"` (or empty, legacy): a layer-1 allow/deny on a write target.
/// - `kind == "delegate-dispatch"` / `"delegate-return"`: a delegation event (layer
///   "delegate"), where `target` is the tier.
fn gate_record_to_event(seq: usize, rec: &GateDecisionRecord) -> GateEvent {
    match rec.kind.as_str() {
        "delegate-dispatch" => GateEvent {
            seq,
            layer: "delegate".to_string(),
            verdict: "dispatch".to_string(),
            rule: None,
            detail: rec.reason.clone(),
            content_hash: None,
        },
        "delegate-return" => GateEvent {
            seq,
            layer: "delegate".to_string(),
            // "incomplete" (escalation) or "returned" — straight from the record.
            verdict: rec.verdict.clone(),
            rule: None,
            detail: rec.reason.clone(),
            content_hash: None,
        },
        // "gate" or empty (legacy): a layer-1 write decision.
        _ => GateEvent {
            seq,
            layer: "layer-1".to_string(),
            verdict: rec.verdict.clone(),
            rule: rec.rule.clone(),
            detail: if rec.verdict == "deny" {
                format!("Write to {} denied. {}", rec.target, rec.reason)
            } else {
                format!("Write to {} allowed.", rec.target)
            },
            // Carry the content hash through so run-finalization capture can write
            // it to the enforcement ledger without re-reading the denied content.
            content_hash: rec.content_hash.clone(),
        },
    }
}

/// Parse a single JSONL line into a [`GateDecisionRecord`]. Blank lines and malformed
/// JSON return `None` (the tailer skips them); a partially-written trailing line on a
/// concurrently-appended file is simply retried on the next poll.
fn parse_gate_line(line: &str) -> Option<GateDecisionRecord> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<GateDecisionRecord>(trimmed).ok()
}

/// Tail the gateway's shared gate-decision JSONL sink, folding each new record into the
/// run's event stream via `push_event`, until `done` is signalled. Runs as its own task
/// so the fleet build is unaware of it. Reads the whole file each poll and advances a
/// line cursor (the file is small: one line per gate decision); a record only ever
/// appears once. Observability only — it reads decisions the gateway already made.
async fn tail_gate_events(
    store: RunStore,
    run_id: String,
    sink_path: std::path::PathBuf,
    seq: Arc<AtomicUsize>,
    done: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut cursor = 0usize; // number of lines already folded
    loop {
        if let Ok(text) = std::fs::read_to_string(&sink_path) {
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() > cursor {
                for line in &lines[cursor..] {
                    if let Some(rec) = parse_gate_line(line) {
                        let n = seq.fetch_add(1, Ordering::SeqCst) + 1;
                        store.push_event(&run_id, gate_record_to_event(n, &rec));
                    }
                }
                cursor = lines.len();
            }
        }
        if done.load(Ordering::SeqCst) {
            // One final drain pass already happened above; stop.
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// The live-observability scaffolding for one run: a shared monotonic seq (so the
/// build-event callback and the gate-events tailer never collide on ordering), the
/// gate-events sink path the gateways append to, a `done` flag, and the tailer task.
struct LiveObservability {
    seq: Arc<AtomicUsize>,
    done: Arc<std::sync::atomic::AtomicBool>,
    tailer: tokio::task::JoinHandle<()>,
}

/// Point every gateway subprocess at one shared gate-decision JSONL sink (via the
/// process env so the `claude` CLI + the gateway it launches inherit it), then spawn the
/// tailer that folds those records into the run's events. The sink lives under the run's
/// temp root so concurrent runs in the same process don't interleave. Returns the shared
/// seq counter for the build-event callback to continue from.
///
/// Observability only: this routes WHERE decisions are recorded and surfaces them; it
/// changes nothing about what the gate decides.
fn start_gate_observability(store: &RunStore, run_id: &str, root: &std::path::Path) -> LiveObservability {
    let _ = std::fs::create_dir_all(root);
    let sink_path = root.join("gate-events.jsonl");
    // Fresh sink per run.
    let _ = std::fs::remove_file(&sink_path);
    // SAFETY/scope: process-wide env. The fleet runs are serialized per process in
    // practice (one operator), and the sink path is unique per run root, so the last
    // writer wins harmlessly; the tailer reads the path it was given regardless.
    std::env::set_var(GATE_EVENTS_FILE_ENV, &sink_path);

    let seq = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tailer = tokio::spawn(tail_gate_events(
        store.clone(),
        run_id.to_string(),
        sink_path,
        seq.clone(),
        done.clone(),
    ));
    LiveObservability { seq, done, tailer }
}

/// Signal the tailer to stop and wait for its final drain pass. Best-effort.
async fn stop_gate_observability(obs: LiveObservability) {
    obs.done.store(true, Ordering::SeqCst);
    let _ = obs.tailer.await;
}

/// Run a real governed fleet for a story and record its progress into the run.
///
/// `model` pins the model id for every `claude -p` agent in the fleet (`None` =
/// the CLI's default). `max_iterations` is the loop-guard ceiling (#29): the
/// maximum bounce-and-revise passes a dirty stage may take before its residual
/// violations are surfaced. It comes from the active project's setting (default `1`).
pub async fn execute_live_run(
    store: RunStore,
    run_id: String,
    story_title: String,
    story_desc: String,
    model: Option<String>,
    max_iterations: usize,
    skip_layer2: bool,
) {
    store.set_status(&run_id, RunStatus::Executing, false);

    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            store.push_event(
                &run_id,
                GateEvent {
                    seq: 1,
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!(
                        "Live fleet needs the gateway binary: {e}. Build it with \
                         `cargo build -p camerata-gateway`, then retry with CAMERATA_LIVE_BUILD=1."
                    ),
                    content_hash: None,
                },
            );
            store.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    announce_bootstrap_if_skipping(&store, &run_id, skip_layer2);

    // A minimal plan from the story: one backend implementer task.
    let plan = Plan {
        app_name: story_title.clone(),
        summary: story_desc.clone(),
        tasks: vec![PlanTask {
            role: "Implementer".to_string(),
            kind: TaskKind::Backend,
            description: format!("Implement: {story_title}. {story_desc}"),
        }],
    };

    // Per-run scaffold dir. Keyed by BOTH the pid AND the run id so two concurrent dev
    // runs in the SAME process (e.g. two UoWs building at once) never collide on the temp
    // scaffold. (This path builds a fresh app from a plan in a throwaway temp dir; it does
    // NOT touch any repo clone or UoW branch, so it needs no git worktree — just a unique dir.)
    let root =
        std::env::temp_dir().join(format!("camerata-live-{}-{}", std::process::id(), run_id));

    // Start the gate-decision sink + tailer so REAL layer-1 decisions from the gateway
    // subprocesses are folded into the run's events. Shares its seq with the fleet's
    // build-event callback so the two interleave with coherent ordering.
    let obs = start_gate_observability(&store, &run_id, &root);
    let seq = obs.seq.clone();

    let store_cb = store.clone();
    let rid_cb = run_id.clone();

    let result = build_from_plan_with_model_iterations_and_layer2(
        &plan,
        &root,
        &gateway_bin,
        model.as_deref(),
        max_iterations,
        skip_layer2,
        &move |event| record_build_event(&store_cb, &rid_cb, &*seq, event),
    )
    .await;

    stop_gate_observability(obs).await;
    finish_live_run(&store, &run_id, result);
}

/// The TIERED live run path: identical to [`execute_live_run`] except every plan task
/// runs on its capability band's model from the per-UoW [`TierMap`] (ORCH-MODEL-TIERING-1),
/// with the STRONGEST tier acting as the orchestrator/lead.
///
/// The plan is built so the lead (backend / domain) task is classified `Strongest` —
/// it owns the complex, one-way-door work and runs on the strongest model — while the
/// simpler, mechanical tasks (test scaffolding) classify down to `Fast`/`Balanced` and
/// dispatch to the cheaper models. The fleet's [`camerata_fleet::tier::classify_task`]
/// drives the per-stage routing; this function only threads the per-UoW map through.
///
/// The no-code-first gate (enforced in the server before this is ever called) and the
/// universal tool gate (every spawned agent keeps `--allowedTools` = gated tools only,
/// `Task` disallowed) are unchanged: this path reuses the exact same
/// `build_from_plan_*` machinery, only varying which model each stage's driver pins.
pub async fn execute_live_run_tiered(
    store: RunStore,
    run_id: String,
    story_title: String,
    story_desc: String,
    tier_map: TierMap,
    max_iterations: usize,
    skip_layer2: bool,
) {
    store.set_status(&run_id, RunStatus::Executing, false);

    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            store.push_event(
                &run_id,
                GateEvent {
                    seq: 1,
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!(
                        "Live fleet needs the gateway binary: {e}. Build it with \
                         `cargo build -p camerata-gateway`, then retry with CAMERATA_LIVE_BUILD=1."
                    ),
                    content_hash: None,
                },
            );
            store.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    // Announce the tier map so the cockpit shows which model leads vs. which dispatch.
    store.push_event(
        &run_id,
        GateEvent {
            seq: 0,
            layer: "fleet".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Tiered run: lead/orchestrator = {} (strongest); balanced = {}; fast = {}.",
                tier_map.strongest, tier_map.balanced, tier_map.fast
            ),
            content_hash: None,
        },
    );

    announce_bootstrap_if_skipping(&store, &run_id, skip_layer2);

    // A tiered plan from the story: the lead implementer (Backend → Strongest) owns the
    // domain logic and acts as orchestrator, and a follow-on Test task (→ Fast) covers
    // the mechanical verification. Both run behind the gate; only the model differs.
    let plan = Plan {
        app_name: story_title.clone(),
        summary: story_desc.clone(),
        tasks: vec![
            PlanTask {
                role: "Lead".to_string(),
                kind: TaskKind::Backend,
                description: format!("Lead/orchestrate and implement: {story_title}. {story_desc}"),
            },
            PlanTask {
                role: "Tester".to_string(),
                kind: TaskKind::Test,
                description: format!("Cover the implementation of {story_title} with tests."),
            },
        ],
    };

    // Per-run scaffold dir, keyed by pid AND run id (see `execute_live_run`): two concurrent
    // tiered dev runs in the same process must not share the throwaway build scaffold.
    let root = std::env::temp_dir()
        .join(format!("camerata-live-tiered-{}-{}", std::process::id(), run_id));

    let obs = start_gate_observability(&store, &run_id, &root);
    let seq = obs.seq.clone();

    let store_cb = store.clone();
    let rid_cb = run_id.clone();

    let result = build_from_plan_with_tier_map_and_layer2(
        &plan,
        &root,
        &gateway_bin,
        &tier_map,
        max_iterations,
        skip_layer2,
        &move |event| record_build_event(&store_cb, &rid_cb, &*seq, event),
    )
    .await;

    stop_gate_observability(obs).await;
    finish_live_run(&store, &run_id, result);
}

/// Push a visible info event when a run is a layer-2 bootstrap bypass, so the cockpit
/// makes it obvious that the post-task lint/test bounce is skipped for THIS run. Layer 1
/// (the deny-before-write gate) still applies; this only announces the layer-2 skip.
fn announce_bootstrap_if_skipping(store: &RunStore, run_id: &str, skip_layer2: bool) {
    if !skip_layer2 {
        return;
    }
    store.push_event(
        run_id,
        GateEvent {
            seq: 0,
            layer: "fleet".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: "Bootstrap run: layer-2 checks (post-task lint/test bounce) are SKIPPED \
                     for this one run so the linters/checkers can be installed. The security \
                     gate (layer 1) still applies. Turn this off after the tooling lands."
                .to_string(),
            content_hash: None,
        },
    );
}

/// Record a single [`BuildEvent`] as run gate activity. Shared by the single-model and
/// tiered live paths so they report progress identically.
///
/// `Verifying` flips the run status (no event); every other variant maps to a
/// [`GateEvent`] via the pure [`build_event_to_gate_event`] and is pushed.
fn record_build_event(store: &RunStore, run_id: &str, seq: &AtomicUsize, event: BuildEvent) {
    if matches!(event, BuildEvent::Verifying) {
        store.set_status(run_id, RunStatus::Gating, false);
        return;
    }
    let n = seq.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(ev) = build_event_to_gate_event(n, event) {
        store.push_event(run_id, ev);
    }
}

/// PURE mapping of a fleet [`BuildEvent`] to a run [`GateEvent`], for the live path's
/// activity log. `Verifying` returns `None` (it is a status change, handled by the
/// caller). Each layer/verdict is chosen so the UI can label/colour the event distinctly:
///
/// - `AgentTier`     → layer "tier",    verdict "info"  (which model/lead each agent runs on)
/// - `Layer2Result`  → layer "layer-2", verdict "pass"/"fail" (+ violated rules)
/// - `ReviseIteration` → layer "layer-2", verdict "revise" (bounce-and-revise pass)
/// - `StageStarted`/`StageFinished`/`Scaffolding` → layer "stage"/"fleet", verdict "info"
/// - `Done`          → layer "checks",  verdict "allow"/"deny" (cargo build+test)
///
/// Observability only: it translates events the fleet already produced; it changes no
/// decision and no enforcement. Unit-testable without a store.
fn build_event_to_gate_event(seq: usize, event: BuildEvent) -> Option<GateEvent> {
    let ev = match event {
        BuildEvent::Scaffolding => GateEvent {
            seq,
            layer: "fleet".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: "Scaffolding the governed worktree.".to_string(),
            content_hash: None,
        },
        BuildEvent::StageStarted {
            index,
            total,
            role,
            kind,
        } => GateEvent {
            seq,
            layer: "stage".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Stage {}/{}: {role} ({kind}) running under the gate.",
                index + 1,
                total
            ),
            content_hash: None,
        },
        BuildEvent::AgentTier {
            index,
            role,
            model,
            is_lead,
        } => GateEvent {
            seq,
            layer: "tier".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: if is_lead {
                format!("Stage {}: {role} → {model} (lead/orchestrator, may delegate).", index + 1)
            } else {
                format!("Stage {}: {role} → {model}.", index + 1)
            },
            content_hash: None,
        },
        BuildEvent::Layer2Result {
            index,
            total,
            passed,
            violated_rules,
        } => GateEvent {
            seq,
            layer: "layer-2".to_string(),
            verdict: if passed { "pass" } else { "fail" }.to_string(),
            rule: if violated_rules.is_empty() {
                None
            } else {
                Some(violated_rules.join(", "))
            },
            detail: if passed {
                format!("Stage {}/{} passed layer-2 checks.", index + 1, total)
            } else {
                format!(
                    "Stage {}/{} failed layer-2: {}.",
                    index + 1,
                    total,
                    violated_rules.join(", ")
                )
            },
            content_hash: None,
        },
        BuildEvent::ReviseIteration {
            index,
            violated_rules,
        } => GateEvent {
            seq,
            layer: "layer-2".to_string(),
            verdict: "revise".to_string(),
            rule: if violated_rules.is_empty() {
                None
            } else {
                Some(violated_rules.join(", "))
            },
            detail: format!(
                "Stage {}: bounce-and-revise — sent back to the agent to fix {}.",
                index + 1,
                if violated_rules.is_empty() {
                    "the violations".to_string()
                } else {
                    violated_rules.join(", ")
                }
            ),
            content_hash: None,
        },
        BuildEvent::StageFinished {
            index,
            total,
            clean,
            bounced,
            session_id,
        } => GateEvent {
            seq,
            layer: "stage".to_string(),
            verdict: if clean { "info" } else { "fail" }.to_string(),
            rule: None,
            detail: format!(
                "Stage {}/{} finished (session {session_id}): clean={clean}, bounced={bounced}.",
                index + 1,
                total
            ),
            content_hash: None,
        },
        BuildEvent::Done {
            compiled,
            tests_passed,
        } => GateEvent {
            seq,
            layer: "checks".to_string(),
            verdict: if compiled && tests_passed { "allow" } else { "deny" }.to_string(),
            rule: None,
            detail: format!("cargo build={compiled}, cargo test={tests_passed}."),
            content_hash: None,
        },
        BuildEvent::Verifying => return None,
    };
    Some(ev)
}

/// Finalise a live run: record any error and mark it AwaitingQa. Shared terminal step.
fn finish_live_run(
    store: &RunStore,
    run_id: &str,
    result: anyhow::Result<camerata_fleet::BuildOutcome>,
) {
    if let Err(e) = result {
        store.push_event(
            run_id,
            GateEvent {
                seq: 9999,
                layer: "fleet".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: format!("Live fleet run failed: {e}"),
                content_hash: None,
            },
        );
    }
    store.set_status(run_id, RunStatus::AwaitingQa, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_fleet::BuildEvent;

    // ── gateway gate-decision JSONL → GateEvent ──────────────────────────────

    #[test]
    fn parse_gate_line_skips_blank_and_malformed() {
        assert!(parse_gate_line("").is_none());
        assert!(parse_gate_line("   ").is_none());
        assert!(parse_gate_line("not json").is_none());
        let ok = parse_gate_line(
            r#"{"kind":"gate","verdict":"allow","target":"a/b.rs","rule":null,"reason":"ALLOWED: wrote 1 bytes to a/b.rs","ts_ms":1}"#,
        );
        assert!(ok.is_some());
    }

    #[test]
    fn gate_record_maps_allow_and_deny_to_layer1() {
        let allow = parse_gate_line(
            r#"{"kind":"gate","verdict":"allow","target":"crates/api/src/repo.rs","rule":null,"reason":"ALLOWED: wrote 9 bytes to crates/api/src/repo.rs","ts_ms":1}"#,
        )
        .unwrap();
        let ev = gate_record_to_event(1, &allow);
        assert_eq!(ev.layer, "layer-1");
        assert_eq!(ev.verdict, "allow");
        assert!(ev.rule.is_none());
        assert!(ev.detail.contains("crates/api/src/repo.rs"));

        let deny = parse_gate_line(
            r#"{"kind":"gate","verdict":"deny","target":"crates/api/src/export_config.rs","rule":"SEC-NO-HARDCODED-SECRETS-1","reason":"DENIED [SEC-NO-HARDCODED-SECRETS-1] path=crates/api/src/export_config.rs","ts_ms":2}"#,
        )
        .unwrap();
        let ev = gate_record_to_event(2, &deny);
        assert_eq!(ev.layer, "layer-1");
        assert_eq!(ev.verdict, "deny");
        assert_eq!(ev.rule.as_deref(), Some("SEC-NO-HARDCODED-SECRETS-1"));
        assert!(ev.detail.contains("denied"));
    }

    #[test]
    fn gate_record_maps_delegate_dispatch_and_return() {
        let dispatch = parse_gate_line(
            r#"{"kind":"delegate-dispatch","verdict":"dispatch","target":"fast","rule":null,"reason":"Delegated a subtask to the fast tier.","ts_ms":3}"#,
        )
        .unwrap();
        let ev = gate_record_to_event(3, &dispatch);
        assert_eq!(ev.layer, "delegate");
        assert_eq!(ev.verdict, "dispatch");

        let incomplete = parse_gate_line(
            r#"{"kind":"delegate-return","verdict":"incomplete","target":"fast","rule":null,"reason":"Delegate (fast) returned INCOMPLETE — escalating.","ts_ms":4}"#,
        )
        .unwrap();
        let ev = gate_record_to_event(4, &incomplete);
        assert_eq!(ev.layer, "delegate");
        assert_eq!(ev.verdict, "incomplete");
    }

    #[test]
    fn legacy_record_without_kind_defaults_to_gate_layer() {
        // A record missing `kind` (older gateway) must still parse and map to layer-1.
        let rec = parse_gate_line(
            r#"{"verdict":"deny","target":"x.rs","rule":"GOV-1","reason":"DENIED [GOV-1] path=x.rs","ts_ms":0}"#,
        )
        .unwrap();
        assert_eq!(rec.kind, ""); // serde default
        let ev = gate_record_to_event(1, &rec);
        assert_eq!(ev.layer, "layer-1");
        assert_eq!(ev.verdict, "deny");
    }

    // ── BuildEvent → GateEvent ───────────────────────────────────────────────

    #[test]
    fn verifying_maps_to_no_event() {
        assert!(build_event_to_gate_event(1, BuildEvent::Verifying).is_none());
    }

    #[test]
    fn agent_tier_maps_to_tier_layer_and_notes_lead() {
        let ev = build_event_to_gate_event(
            1,
            BuildEvent::AgentTier {
                index: 0,
                role: "Lead-1".to_string(),
                model: "claude-opus-4-8".to_string(),
                is_lead: true,
            },
        )
        .unwrap();
        assert_eq!(ev.layer, "tier");
        assert_eq!(ev.verdict, "info");
        assert!(ev.detail.contains("claude-opus-4-8"));
        assert!(ev.detail.contains("lead"));
    }

    #[test]
    fn layer2_result_maps_pass_and_fail_with_rules() {
        let pass = build_event_to_gate_event(
            1,
            BuildEvent::Layer2Result {
                index: 0,
                total: 2,
                passed: true,
                violated_rules: vec![],
            },
        )
        .unwrap();
        assert_eq!(pass.layer, "layer-2");
        assert_eq!(pass.verdict, "pass");
        assert!(pass.rule.is_none());

        let fail = build_event_to_gate_event(
            2,
            BuildEvent::Layer2Result {
                index: 1,
                total: 2,
                passed: false,
                violated_rules: vec!["RUST-FMT".to_string(), "RUST-CLIPPY".to_string()],
            },
        )
        .unwrap();
        assert_eq!(fail.layer, "layer-2");
        assert_eq!(fail.verdict, "fail");
        assert_eq!(fail.rule.as_deref(), Some("RUST-FMT, RUST-CLIPPY"));
    }

    #[test]
    fn revise_iteration_maps_to_layer2_revise() {
        let ev = build_event_to_gate_event(
            1,
            BuildEvent::ReviseIteration {
                index: 0,
                violated_rules: vec!["RUST-FMT".to_string()],
            },
        )
        .unwrap();
        assert_eq!(ev.layer, "layer-2");
        assert_eq!(ev.verdict, "revise");
        assert_eq!(ev.rule.as_deref(), Some("RUST-FMT"));
        assert!(ev.detail.contains("bounce-and-revise"));
    }

    #[test]
    fn done_maps_compiled_and_tested_to_allow_else_deny() {
        let ok = build_event_to_gate_event(
            1,
            BuildEvent::Done {
                compiled: true,
                tests_passed: true,
            },
        )
        .unwrap();
        assert_eq!(ok.layer, "checks");
        assert_eq!(ok.verdict, "allow");

        let bad = build_event_to_gate_event(
            2,
            BuildEvent::Done {
                compiled: true,
                tests_passed: false,
            },
        )
        .unwrap();
        assert_eq!(bad.verdict, "deny");
    }
}
