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

use camerata_fleet::tier::TierMap;
use camerata_fleet::{
    build_from_plan_with_model_and_iterations, build_from_plan_with_tier_map, locate_gateway_bin,
    BuildEvent,
};
use camerata_intake::{Plan, PlanTask, TaskKind};

use crate::run::{GateEvent, RunStatus, RunStore};

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
                },
            );
            store.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

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

    let root = std::env::temp_dir().join(format!("camerata-live-{}", std::process::id()));

    let store_cb = store.clone();
    let rid_cb = run_id.clone();
    let seq = AtomicUsize::new(0);

    let result = build_from_plan_with_model_and_iterations(
        &plan,
        &root,
        &gateway_bin,
        model.as_deref(),
        max_iterations,
        &move |event| record_build_event(&store_cb, &rid_cb, &seq, event),
    )
    .await;

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
        },
    );

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

    let root = std::env::temp_dir().join(format!("camerata-live-tiered-{}", std::process::id()));

    let store_cb = store.clone();
    let rid_cb = run_id.clone();
    let seq = AtomicUsize::new(0);

    let result = build_from_plan_with_tier_map(
        &plan,
        &root,
        &gateway_bin,
        &tier_map,
        max_iterations,
        &move |event| record_build_event(&store_cb, &rid_cb, &seq, event),
    )
    .await;

    finish_live_run(&store, &run_id, result);
}

/// Record a single [`BuildEvent`] as run gate activity. Shared by the single-model and
/// tiered live paths so they report progress identically.
fn record_build_event(
    store: &RunStore,
    run_id: &str,
    seq: &AtomicUsize,
    event: BuildEvent,
) {
    let n = seq.fetch_add(1, Ordering::SeqCst) + 1;
    match event {
        BuildEvent::Scaffolding => store.push_event(
            run_id,
            GateEvent {
                seq: n,
                layer: "fleet".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: "Scaffolding the governed worktree.".to_string(),
            },
        ),
        BuildEvent::StageStarted {
            index,
            total,
            role,
            kind,
        } => store.push_event(
            run_id,
            GateEvent {
                seq: n,
                layer: "fleet".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: format!(
                    "Stage {}/{}: {role} ({kind}) running under the gate.",
                    index + 1,
                    total
                ),
            },
        ),
        BuildEvent::StageFinished {
            index,
            total,
            clean,
            bounced,
            session_id,
        } => store.push_event(
            run_id,
            GateEvent {
                seq: n,
                layer: "layer-2".to_string(),
                verdict: if bounced { "bounce" } else { "info" }.to_string(),
                rule: None,
                detail: format!(
                    "Stage {}/{} finished (session {session_id}): clean={clean}, bounced={bounced}.",
                    index + 1,
                    total
                ),
            },
        ),
        BuildEvent::Verifying => store.set_status(run_id, RunStatus::Gating, false),
        BuildEvent::Done {
            compiled,
            tests_passed,
        } => store.push_event(
            run_id,
            GateEvent {
                seq: n,
                layer: "checks".to_string(),
                verdict: if compiled && tests_passed { "allow" } else { "deny" }.to_string(),
                rule: None,
                detail: format!("cargo build={compiled}, cargo test={tests_passed}."),
            },
        ),
    }
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
            },
        );
    }
    store.set_status(run_id, RunStatus::AwaitingQa, true);
}
