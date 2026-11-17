//! The cost checkpoint at a stage boundary, and on the job state the UI polls.

use super::*;
use crate::cost_cap::RunControls;
use crate::cost_checkpoint::{CheckpointPlan, CheckpointSettings, CheckpointSink, CAUSE_PROJECTED};

fn meter(limit: f64, later: f64, assumed: Option<usize>, spent: f64) -> UsageMeter {
    let controls = RunControls {
        max_cost_usd: Some(limit),
        checkpoint: CheckpointSettings {
            plan: Some(CheckpointPlan {
                per_piece_usd: 1.0,
                later_stages_usd: later,
                assumed_problems: assumed,
            }),
            ..CheckpointSettings::default()
        },
        ..RunControls::default()
    };
    UsageMeter::continuing(
        controls,
        &ActualUsage {
            cost_usd: spent,
            calls: 1,
            cost_complete: true,
            ..ActualUsage::default()
        },
    )
}

#[test]
fn a_stage_whose_projection_goes_over_the_limit_is_not_started() {
    // $3 spent of $10; the stages after the finder are estimated at $8: $11 projected.
    let m = meter(10.0, 8.0, None, 3.0);
    assert!(m.stage_blocked("acme/app", STAGE_FIX));
    let record = m.cost_cap_record().expect("a limit was set");
    assert!(record.stopped_the_run());
    assert_eq!(record.cause(), CAUSE_PROJECTED);
    assert_eq!(record.not_run_stages.len(), 1);
    let stop = m.checkpoint_stop().expect("the projection stopped the run");
    assert!((stop.projected_usd - 11.0).abs() < 1e-9);
    assert_eq!(stop.pieces_left, 0);
    assert!(stop
        .pause_message()
        .contains("New estimate $11.00 (+$1.00)"));
    // Later stages stay blocked, and no new send is admitted.
    assert!(m.stage_blocked("acme/app", STAGE_DETAIL));
    assert_eq!(m.stop_cause(), CAUSE_PROJECTED);
}

#[test]
fn a_stage_whose_projection_fits_the_limit_starts() {
    let m = meter(10.0, 5.0, None, 3.0);
    assert!(!m.stage_blocked("acme/app", STAGE_FIX));
    assert!(m.checkpoint_stop().is_none());
}

#[test]
fn the_later_stages_scale_with_the_problems_found_against_the_estimates_assumption() {
    // The estimate assumed 10 problems for $4 of later stages; 5 were found: $2.
    let m = meter(10.0, 4.0, Some(10), 3.0);
    m.begin_pieces(1, 0);
    m.piece_finished(Some(0.0), 5);
    assert!(!m.stage_blocked("acme/app", STAGE_FIX), "3 + 2 fits 10");
    // 30 problems were found: $12 of later stages, so $15 projected.
    let m = meter(10.0, 4.0, Some(10), 3.0);
    m.begin_pieces(1, 0);
    m.piece_finished(Some(0.0), 30);
    assert!(m.stage_blocked("acme/app", STAGE_FIX));
}

#[test]
fn a_run_with_no_limit_never_blocks_on_a_projection() {
    let m = UsageMeter::default();
    assert!(!m.stage_blocked("acme/app", STAGE_FIX));
    assert!(!m.projection_stops_piece());
    assert!(m.checkpoint_stop().is_none());
}

#[test]
fn each_checkpoint_reaches_the_job_state_the_ui_polls() {
    let jobs = crate::jobs::JobStore::new();
    let id = jobs.create("scan", None);
    let m = meter(10.0, 0.0, None, 2.0);
    let (store, job) = (jobs.clone(), id.clone());
    m.set_job_sink(CheckpointSink::new(move |cp| {
        store.set_cost_checkpoint(&job, cp.clone());
    }));
    m.begin_pieces(4, 0);
    m.piece_finished(Some(2.0), 1);
    let cp = jobs
        .get(&id)
        .unwrap()
        .cost_checkpoint
        .expect("recorded on the job");
    assert!((cp.approved_usd - 10.0).abs() < 1e-9);
    assert!((cp.spent_usd - 2.0).abs() < 1e-9);
    // Three pieces left at the $2.00 average: 2 + 6 = 8.
    assert!(
        (cp.projected_usd - 8.0).abs() < 1e-9,
        "{}",
        cp.projected_usd
    );
    assert_eq!((cp.pieces_done, cp.pieces_left), (1, 3));
    assert!(!cp.stopped);
    let json = serde_json::to_value(jobs.get(&id).unwrap()).unwrap();
    assert!(json.get("cost_checkpoint").is_some());
}
