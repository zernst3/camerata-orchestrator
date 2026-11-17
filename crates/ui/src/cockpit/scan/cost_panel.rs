//! The cost panel on the scan screen: spent against the approved amount while a run goes and
//! after it ends, and, when the run paused before going over, the one-click approval that
//! resumes it at the advised spend limit. It reads `JobState.cost_checkpoint` (the engine's
//! checkpoint, one type shared through `camerata_api_types`) and says nothing without one.

use super::*;
use camerata_api_types::cost_checkpoint::CostCheckpoint;

/// The stage name a group review pause carries (the engine's `STAGE_GROUP_REVIEW`).
const GROUP_REVIEW_STAGE: &str = "group review";

/// Where the approval stands. The run resumes only from `Confirm`, after two clicks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::cockpit) enum ApproveStep {
    /// The pause is shown with its buttons; nothing is asked yet.
    #[default]
    Pause,
    /// "Approve and continue" was clicked: the page asks to confirm the limit.
    Confirm,
    /// "Stop here" was clicked: the run stays paused.
    Left,
}

impl ApproveStep {
    /// "Approve and continue": ask to confirm. Nothing is sent.
    pub(in crate::cockpit) fn ask(self) -> Self {
        match self {
            Self::Pause => Self::Confirm,
            other => other,
        }
    }

    /// "Back" from the confirmation, or from "Stop here" (which is not a dead end).
    pub(in crate::cockpit) fn back(self) -> Self {
        match self {
            Self::Confirm | Self::Left => Self::Pause,
            other => other,
        }
    }

    /// "Stop here": the run stays paused and nothing is sent.
    pub(in crate::cockpit) fn stop(self) -> Self {
        Self::Left
    }

    /// The spend limit to resume with: only a confirmed approval of a paused run has one. The
    /// advised figure is the engine's (new estimate plus reserve, rounded up to the cent).
    pub(in crate::cockpit) fn limit_to_resume(self, cp: &CostCheckpoint) -> Option<f64> {
        (self == Self::Confirm && cp.stopped).then(|| cp.resume_limit_usd())
    }
}

/// The resume the approval makes: `start` is called with the run and the advised limit only when
/// `step` is a confirmed approval of this paused run; otherwise nothing is sent and the answer
/// is `None`. `start` is the request, passed in so the flow is testable without a server.
pub(in crate::cockpit) async fn resume_on_approval<S, SF>(
    step: ApproveStep,
    cp: &CostCheckpoint,
    run_id: &str,
    start: S,
) -> Option<Result<String, String>>
where
    S: FnOnce(String, f64) -> SF,
    SF: std::future::Future<Output = Result<String, String>>,
{
    let limit = step.limit_to_resume(cp)?;
    Some(start(run_id.to_string(), limit).await)
}

/// "1 piece" or "4 pieces".
fn pieces(n: usize) -> String {
    if n == 1 {
        "1 piece".to_string()
    } else {
        format!("{n} pieces")
    }
}

/// What the projected figure covers, by the stage the pause stopped before.
fn projected_label(cp: &CostCheckpoint) -> String {
    match cp.stopped_before.as_str() {
        "" | "agent finder" => format!("Projected ${:.2} for the finder stage", cp.projected_usd),
        GROUP_REVIEW_STAGE => format!("Projected ${:.2} with the group review", cp.projected_usd),
        other => format!("Projected ${:.2} with the {other} stage", cp.projected_usd),
    }
}

/// The width of the bar's fill: spent against approved, held to the bar.
fn spent_percent(cp: &CostCheckpoint) -> u32 {
    if cp.approved_usd <= 0.0 {
        return 0;
    }
    ((cp.spent_usd / cp.approved_usd) * 100.0)
        .clamp(0.0, 100.0)
        .round() as u32
}

#[component]
pub(in crate::cockpit) fn CostCheckpointPanel(
    checkpoint: Option<CostCheckpoint>,
    // The saved run this scan is. The approval resumes it; without it the button stays off.
    #[props(default)] run_id: Option<String>,
    #[props(default)] step: ApproveStep,
    // True while a job runs: a paused run is resumed once its job has ended.
    #[props(default)] busy: bool,
    #[props(default)] on_ask: Option<EventHandler<()>>,
    #[props(default)] on_back: Option<EventHandler<()>>,
    #[props(default)] on_stop: Option<EventHandler<()>>,
    // The confirming click, the only one that resumes the run (at `limit_to_resume`).
    #[props(default)] on_confirm: Option<EventHandler<()>>,
) -> Element {
    let Some(cp) = checkpoint else {
        return rsx! {};
    };
    let pct = spent_percent(&cp);
    let spent = format!(
        "Spent ${:.2} of ${:.2} approved",
        cp.spent_usd, cp.approved_usd
    );
    let projected = projected_label(&cp);
    let progress = if cp.pieces_done + cp.pieces_left > 0 {
        Some(format!(
            "{} done, {} left",
            pieces(cp.pieces_done),
            cp.pieces_left
        ))
    } else {
        None
    };
    let can_approve = run_id.is_some() && !busy;
    let limit = cp.resume_limit_usd();
    rsx! {
        div { class: "det-progress cost-panel",
            div { class: "det-progress-head",
                span { class: "det-progress-title", "Cost" }
                span { class: "det-progress-count", "{spent}" }
            }
            div { class: "job-progress-track cost-bar",
                div { class: "job-progress-fill", style: "width: {pct}%" }
            }
            p { class: "cost-panel-line", "{projected}" }
            if let Some(progress) = progress {
                p { class: "cost-panel-line", "{progress}" }
            }
            if cp.stopped {
                p { class: "onboard-gate-b cost-panel-pause", "{cp.pause_message()}" }
                match step {
                    ApproveStep::Pause => rsx! {
                        div { class: "cost-panel-actions",
                            button {
                                class: "btn-run cost-approve",
                                r#type: "button",
                                disabled: !can_approve,
                                title: if can_approve {
                                    "Asks you to confirm the new spend limit. Nothing is sent yet."
                                } else {
                                    "The run can be approved once its job has ended and it is saved."
                                },
                                onclick: move |_| {
                                    if let Some(handler) = on_ask {
                                        handler.call(());
                                    }
                                },
                                "Approve and continue"
                            }
                            button {
                                class: "btn-secondary cost-stop",
                                r#type: "button",
                                onclick: move |_| {
                                    if let Some(handler) = on_stop {
                                        handler.call(());
                                    }
                                },
                                "Stop here"
                            }
                        }
                    },
                    ApproveStep::Confirm => rsx! {
                        div { class: "cost-panel-confirm",
                            p { class: "onboard-gate-b",
                                "Resume this run with a spend limit of ${limit:.2}? Work already paid for is not paid for again."
                            }
                            div { class: "cost-panel-actions",
                                button {
                                    class: "btn-run cost-confirm",
                                    r#type: "button",
                                    disabled: !can_approve,
                                    onclick: move |_| {
                                        if let Some(handler) = on_confirm {
                                            handler.call(());
                                        }
                                    },
                                    "Confirm and resume"
                                }
                                button {
                                    class: "btn-secondary cost-back",
                                    r#type: "button",
                                    onclick: move |_| {
                                        if let Some(handler) = on_back {
                                            handler.call(());
                                        }
                                    },
                                    "Back"
                                }
                            }
                        }
                    },
                    ApproveStep::Left => rsx! {
                        p { class: "onboard-gate-hint cost-panel-left",
                            "The run stays paused. It can be resumed later."
                        }
                        button {
                            class: "btn-secondary cost-back",
                            r#type: "button",
                            onclick: move |_| {
                                if let Some(handler) = on_back {
                                    handler.call(());
                                }
                            },
                            "Back"
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_harness::render_fragment;

    fn running() -> CostCheckpoint {
        CostCheckpoint {
            approved_usd: 260.0,
            spent_usd: 130.0,
            projected_usd: 200.0,
            pieces_done: 3,
            pieces_left: 4,
            ..CostCheckpoint::default()
        }
    }

    fn paused() -> CostCheckpoint {
        CostCheckpoint {
            approved_usd: 260.0,
            spent_usd: 231.40,
            projected_usd: 318.20,
            pieces_done: 6,
            pieces_left: 4,
            stopped: true,
            new_estimate_usd: Some(318.20),
            difference_usd: Some(58.20),
            stopped_before: "agent finder".into(),
            reserve_usd: 2.5,
        }
    }

    #[derive(Clone, PartialEq, Props)]
    struct Harness {
        checkpoint: Option<CostCheckpoint>,
        run_id: Option<String>,
        step: ApproveStep,
        busy: bool,
    }

    #[component]
    fn PanelHarness(props: Harness) -> Element {
        rsx! {
            CostCheckpointPanel {
                checkpoint: props.checkpoint,
                run_id: props.run_id,
                step: props.step,
                busy: props.busy,
            }
        }
    }

    fn html(checkpoint: Option<CostCheckpoint>, step: ApproveStep) -> String {
        render_fragment(
            PanelHarness,
            Harness {
                checkpoint,
                run_id: Some("run-1".into()),
                step,
                busy: false,
            },
        )
    }

    #[test]
    fn the_panel_shows_spent_approved_projected_and_the_pieces() {
        let page = html(Some(running()), ApproveStep::Pause);
        assert!(page.contains("Spent $130.00 of $260.00 approved"), "{page}");
        assert!(
            page.contains("Projected $200.00 for the finder stage"),
            "{page}"
        );
        assert!(page.contains("3 pieces done, 4 left"), "{page}");
        assert!(page.contains("width: 50%"), "{page}");
        assert!(!page.contains("Approve and continue"), "{page}");
        assert!(!page.contains("Stop here"), "{page}");
    }

    #[test]
    fn a_group_review_pause_names_the_group_review() {
        let cp = CostCheckpoint {
            stopped_before: "group review".into(),
            ..paused()
        };
        let page = html(Some(cp), ApproveStep::Pause);
        assert!(
            page.contains("Projected $318.20 with the group review"),
            "{page}"
        );
    }

    #[test]
    fn no_checkpoint_draws_no_panel() {
        assert!(!html(None, ApproveStep::Pause).contains("cost-panel"));
    }

    #[test]
    fn a_paused_run_shows_the_message_and_both_buttons() {
        let page = html(Some(paused()), ApproveStep::Pause);
        assert!(
            page.contains("Paused before going over the approved amount. Approved $260.00, spent $231.40, 4 pieces left."),
            "{page}"
        );
        assert!(page.contains("New estimate $318.20 (+$58.20)"), "{page}");
        assert!(page.contains("Approve and continue"), "{page}");
        assert!(page.contains("Stop here"), "{page}");
        assert!(!page.contains("Confirm and resume"), "{page}");
        assert!(page.contains("width: 89%"), "{page}");
    }

    #[test]
    fn the_confirmation_names_the_advised_limit_and_stop_here_leaves_the_run_paused() {
        let page = html(Some(paused()), ApproveStep::Confirm);
        assert!(page.contains("spend limit of $320.70"), "{page}");
        assert!(page.contains("Confirm and resume"), "{page}");
        let left = html(Some(paused()), ApproveStep::Left);
        assert!(left.contains("The run stays paused"), "{left}");
        assert!(!left.contains("Confirm and resume"), "{left}");
        assert!(!left.contains("Approve and continue"), "{left}");
        assert!(
            left.contains("cost-back"),
            "the way back is offered: {left}"
        );
        let again = html(Some(paused()), ApproveStep::Left.back());
        assert!(again.contains("Approve and continue"), "{again}");
    }

    #[test]
    fn the_approve_button_is_off_while_a_job_runs_or_without_a_saved_run() {
        let busy = render_fragment(
            PanelHarness,
            Harness {
                checkpoint: Some(paused()),
                run_id: Some("run-1".into()),
                step: ApproveStep::Pause,
                busy: true,
            },
        );
        let button = busy.split("cost-approve").nth(1).unwrap();
        assert!(
            button
                .split("</button>")
                .next()
                .unwrap()
                .contains("disabled"),
            "{busy}"
        );
        let unsaved = render_fragment(
            PanelHarness,
            Harness {
                checkpoint: Some(paused()),
                run_id: None,
                step: ApproveStep::Pause,
                busy: false,
            },
        );
        let button = unsaved.split("cost-approve").nth(1).unwrap();
        assert!(button
            .split("</button>")
            .next()
            .unwrap()
            .contains("disabled"));
    }

    #[test]
    fn the_run_resumes_only_after_the_confirming_click_and_at_the_advised_limit() {
        let cp = paused();
        // Showing the pause, or asking, resumes nothing.
        assert_eq!(ApproveStep::Pause.limit_to_resume(&cp), None);
        let asked = ApproveStep::Pause.ask();
        assert_eq!(asked, ApproveStep::Confirm);
        assert_eq!(asked.back(), ApproveStep::Pause);
        // The confirmation holds the engine's advised figure: estimate plus reserve.
        let limit = asked.limit_to_resume(&cp).expect("a confirmed approval");
        assert!((limit - 320.70).abs() < 1e-9, "{limit}");
        // Stop here leaves the run paused: no limit, from any step, and asking again does nothing.
        assert_eq!(asked.stop().limit_to_resume(&cp), None);
        assert_eq!(ApproveStep::Pause.stop(), ApproveStep::Left);
        assert_eq!(ApproveStep::Left.ask(), ApproveStep::Left);
        // Stop here is not a dead end: Back returns to the pause with its buttons.
        assert_eq!(ApproveStep::Left.back(), ApproveStep::Pause);
        assert_eq!(ApproveStep::Left.limit_to_resume(&cp), None);
        // A checkpoint that did not stop the run has nothing to resume.
        assert_eq!(ApproveStep::Confirm.limit_to_resume(&running()), None);
    }

    #[test]
    fn the_resume_route_carries_the_approved_limit_to_the_cent() {
        assert_eq!(
            resume_url("http://x", "run-1", Some(320.7)),
            "http://x/api/inspections/run-1/resume?max_cost_usd=320.70"
        );
        assert_eq!(
            resume_url("http://x", "run-1", None),
            "http://x/api/inspections/run-1/resume"
        );
    }

    #[tokio::test]
    async fn approving_calls_the_resume_with_the_advised_limit_and_only_after_the_click() {
        let calls = std::cell::RefCell::new(Vec::<(String, f64)>::new());
        let start = |run: String, limit: f64| {
            calls.borrow_mut().push((run, limit));
            async { Ok("job-2".to_string()) }
        };
        // Nothing is sent while the pause is shown, or after Stop here.
        for step in [ApproveStep::Pause, ApproveStep::Left] {
            assert!(resume_on_approval(step, &paused(), "run-1", start)
                .await
                .is_none());
        }
        assert!(calls.borrow().is_empty());
        let started = resume_on_approval(ApproveStep::Confirm, &paused(), "run-1", start).await;
        assert_eq!(started, Some(Ok("job-2".to_string())));
        assert_eq!(calls.borrow().as_slice(), [("run-1".to_string(), 320.70)]);
    }

    #[test]
    fn the_polled_job_carries_the_checkpoint_and_a_job_without_a_limit_has_none() {
        let job: JobStateView = serde_json::from_value(serde_json::json!({
            "status": "running",
            "cost_checkpoint": {
                "approved_usd": 260.0, "spent_usd": 231.4, "projected_usd": 318.2,
                "pieces_done": 6, "pieces_left": 4, "stopped": true,
                "new_estimate_usd": 318.2, "difference_usd": 58.2,
                "stopped_before": "agent finder", "reserve_usd": 2.5
            }
        }))
        .unwrap();
        assert_eq!(job.cost_checkpoint, Some(paused()));
        let plain: JobStateView =
            serde_json::from_value(serde_json::json!({ "status": "running" })).unwrap();
        assert!(plain.cost_checkpoint.is_none());
    }

    #[test]
    fn the_panels_words_have_no_dashes_and_no_serial_comma() {
        for step in [ApproveStep::Pause, ApproveStep::Confirm, ApproveStep::Left] {
            let page = html(Some(paused()), step);
            assert!(
                !page.contains('\u{2014}') && !page.contains('\u{2013}'),
                "{page}"
            );
            assert!(!page.contains(", and"), "{page}");
        }
    }
}
