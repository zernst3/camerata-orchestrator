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

use camerata_fleet::{build_from_plan_with_model_and_iterations, locate_gateway_bin, BuildEvent};
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

    let result = build_from_plan_with_model_and_iterations(&plan, &root, &gateway_bin, model.as_deref(), max_iterations, &move |event| {
        let n = seq.fetch_add(1, Ordering::SeqCst) + 1;
        match event {
            BuildEvent::Scaffolding => store_cb.push_event(
                &rid_cb,
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
            } => store_cb.push_event(
                &rid_cb,
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
            } => store_cb.push_event(
                &rid_cb,
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
            BuildEvent::Verifying => store_cb.set_status(&rid_cb, RunStatus::Gating, false),
            BuildEvent::Done {
                compiled,
                tests_passed,
            } => store_cb.push_event(
                &rid_cb,
                GateEvent {
                    seq: n,
                    layer: "checks".to_string(),
                    verdict: if compiled && tests_passed { "allow" } else { "deny" }.to_string(),
                    rule: None,
                    detail: format!("cargo build={compiled}, cargo test={tests_passed}."),
                },
            ),
        }
    })
    .await;

    if let Err(e) = result {
        store.push_event(
            &run_id,
            GateEvent {
                seq: 9999,
                layer: "fleet".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: format!("Live fleet run failed: {e}"),
            },
        );
    }
    store.set_status(&run_id, RunStatus::AwaitingQa, true);
}
