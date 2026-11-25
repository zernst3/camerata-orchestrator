//! The cost checkpoint at a stage boundary, and on the job state the UI polls.

use super::*;
use crate::cost_cap::RunControls;
use crate::cost_checkpoint::{CheckpointPlan, CheckpointSettings, CheckpointSink, CAUSE_PROJECTED};

fn meter(limit: f64, later: f64, assumed: Option<usize>, spent: f64) -> UsageMeter {
    let controls = RunControls {
        scope: Default::default(),
        max_cost_usd: Some(limit),
        checkpoint: CheckpointSettings {
            plan: Some(CheckpointPlan {
                per_piece_usd: 1.0,
                later_stages_usd: later,
                assumed_problems: assumed,
                ..CheckpointPlan::default()
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
    // Three pieces left. One finished piece weighs 4 percent against the $1.00 estimate:
    // 0.04 x 2.00 + 0.96 x 1.00 = $1.04 each, so 2 + 3 x 1.04 = 5.12.
    assert!(
        (cp.projected_usd - 5.12).abs() < 1e-9,
        "{}",
        cp.projected_usd
    );
    assert_eq!((cp.pieces_done, cp.pieces_left), (1, 3));
    assert!(!cp.stopped);
    let json = serde_json::to_value(jobs.get(&id).unwrap()).unwrap();
    assert!(json.get("cost_checkpoint").is_some());
}

// ── Fable round 1 ──────────────────────────────────────────────────────────────────────────

use crate::cost_checkpoint::{CostCheckpoint, CHECKPOINT_FILE};
use crate::llm::LlmResponse;
use crate::stage_telemetry::Stage;

fn pay(m: &UsageMeter, usd: f64) {
    m.record_stage(
        Stage::Finder,
        &LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text: String::new(),
            model: "m".into(),
            backend: "api".into(),
            cost_usd: Some(usd),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        },
    );
}

fn plain(limit: f64, plan: CheckpointPlan, folder: Option<&std::path::Path>) -> RunControls {
    RunControls {
        scope: Default::default(),
        max_cost_usd: Some(limit),
        checkpoint: CheckpointSettings {
            plan: Some(plan),
            run_folder: folder.map(std::path::Path::to_path_buf),
            ..CheckpointSettings::default()
        },
        ..RunControls::default()
    }
}

#[test]
fn a_piece_that_finishes_after_the_pause_leaves_the_pause_as_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let texts = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut controls = plain(5.0, CheckpointPlan::default(), Some(dir.path()));
    let sink_texts = texts.clone();
    controls.checkpoint.sink = CheckpointSink::new(move |cp| {
        sink_texts.lock().unwrap().push(cp.operator_text());
    });
    let m = UsageMeter::with_controls(controls);
    m.begin_pieces(4, 0);
    pay(&m, 3.0);
    m.piece_finished(Some(3.0), 0);
    assert!(
        m.projection_stops_piece(),
        "3 spent + 3 pieces at 3 is over 5"
    );
    // A piece already in flight finishes after the pause.
    pay(&m, 3.0);
    m.piece_finished(Some(3.0), 0);
    let latest = m.latest_checkpoint().unwrap();
    assert!(latest.stopped, "{latest:?}");
    assert!((latest.spent_usd - 6.0).abs() < 1e-9);
    assert_eq!(m.cost_cap_record().unwrap().cause(), CAUSE_PROJECTED);
    let on_disk: CostCheckpoint =
        serde_json::from_slice(&std::fs::read(dir.path().join(CHECKPOINT_FILE)).unwrap()).unwrap();
    assert!(on_disk.stopped);
    assert!((on_disk.spent_usd - 6.0).abs() < 1e-9);
    assert!(texts
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .starts_with("Paused before"));
}

#[test]
fn the_stage_boundary_prices_the_pieces_of_repositories_not_begun() {
    // The run plans 4 pieces; the first repository's 2 are done at $1.50 each.
    let plan = CheckpointPlan {
        pieces: 4,
        ..CheckpointPlan::default()
    };
    let m = UsageMeter::with_controls(plain(5.0, plan, None));
    m.begin_pieces(2, 0);
    pay(&m, 3.0);
    m.piece_finished(Some(1.5), 0);
    m.piece_finished(Some(1.5), 0);
    // 3 spent + 2 pieces of the next repository at 1.50 = 6, over 5.
    assert!(m.stage_blocked("acme/app", STAGE_FIX));
    let stop = m.checkpoint_stop().unwrap();
    assert_eq!(stop.pieces_left, 2);
    assert!((stop.projected_usd - 6.0).abs() < 1e-9, "{stop:?}");
}

#[test]
fn the_joints_figure_is_not_counted_again_at_a_stage_boundary() {
    let plan = CheckpointPlan {
        joints_usd: 2.0,
        ..CheckpointPlan::default()
    };
    let m = meter_with_plan(4.0, plan, 3.0);
    // 3 spent of 4: the joints are over by now, so nothing more is projected.
    assert!(!m.stage_blocked("acme/app", STAGE_FIX));
}

fn meter_with_plan(limit: f64, plan: CheckpointPlan, spent: f64) -> UsageMeter {
    UsageMeter::continuing(
        plain(limit, plan, None),
        &ActualUsage {
            cost_usd: spent,
            calls: 1,
            cost_complete: true,
            ..ActualUsage::default()
        },
    )
}

#[test]
fn a_resume_projects_from_the_stored_checkpoint_before_its_first_piece_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let stored = CostCheckpoint {
        spent_usd: 3.0,
        pieces_done: 1,
        pieces_left: 3,
        ..CostCheckpoint::default()
    };
    std::fs::write(
        dir.path().join(CHECKPOINT_FILE),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let plan = CheckpointPlan::from_stored(dir.path()).expect("seeded from the earlier attempt");
    let mut controls = plain(11.0, plan, None);
    controls.stored_spend_usd = 3.0;
    let m = UsageMeter::with_controls(controls);
    m.begin_pieces(4, 1);
    assert!(
        m.projection_stops_piece(),
        "3 spent + 3 pieces at 3 is 12, over 11"
    );
    let stop = m.checkpoint_stop().unwrap();
    assert!((stop.projected_usd - 12.0).abs() < 1e-9, "{stop:?}");
    assert_eq!(stop.pieces_left, 3);
}

#[test]
fn a_limit_typed_from_the_rounded_estimate_does_not_pause() {
    // Two pieces at $3.002 project $6.004: shown as $6.00, so a $6.00 limit must hold.
    let plan = CheckpointPlan {
        per_piece_usd: 3.002,
        ..CheckpointPlan::default()
    };
    let m = UsageMeter::with_controls(plain(6.0, plan, None));
    m.begin_pieces(2, 0);
    assert!(!m.projection_stops_piece());
}

#[test]
fn the_pause_records_the_hard_limits_reserve_for_the_resume_advice() {
    let m = UsageMeter::with_controls(plain(5.0, CheckpointPlan::default(), None));
    m.begin_pieces(4, 0);
    pay(&m, 3.0);
    m.piece_finished(Some(3.0), 0);
    assert!(m.projection_stops_piece());
    let stop = m.checkpoint_stop().unwrap();
    // The largest call so far cost $3.00: the hard limit holds that back for a piece in flight.
    assert!((stop.reserve_usd - 3.0).abs() < 1e-9, "{stop:?}");
    assert!(
        stop.pause_message().contains("at least $15.00."),
        "{}",
        stop.pause_message()
    );
}

// ── a piece cut in two on a resume is still one planned piece ───────────────────────────────

fn two_repo_meter(stored: f64) -> UsageMeter {
    let controls = plain(
        100.0,
        CheckpointPlan {
            per_piece_usd: 1.0,
            pieces: 4,
            ..CheckpointPlan::default()
        },
        None,
    );
    UsageMeter::continuing(
        controls,
        &ActualUsage {
            cost_usd: stored,
            calls: 1,
            cost_complete: true,
            ..ActualUsage::default()
        },
    )
}

fn projected(m: &UsageMeter) -> f64 {
    m.latest_checkpoint().expect("a checkpoint").projected_usd
}

/// Two repositories of two planned pieces each, each piece costing 1.00: the projection after
/// each finished piece when nothing splits.
fn unsplit_projections() -> Vec<f64> {
    let m = two_repo_meter(0.0);
    let mut seen = Vec::new();
    m.begin_pieces(2, 0);
    for _ in 0..2 {
        pay(&m, 1.0);
        m.piece_finished(Some(1.0), 0);
        seen.push(projected(&m));
    }
    m.begin_pieces(2, 0);
    pay(&m, 1.0);
    m.piece_finished(Some(1.0), 0);
    seen.push(projected(&m));
    seen
}

#[test]
fn a_piece_that_splits_in_the_first_of_two_repositories_projects_as_if_it_never_split() {
    let baseline = unsplit_projections();
    let m = two_repo_meter(0.0);
    // Repository one: piece one whole, piece two as two halves (3 works, 2 of them halves).
    m.begin_pieces(3, 0);
    m.begin_halves(2, 0);
    pay(&m, 1.0);
    m.piece_finished(Some(1.0), 0);
    assert!(
        (projected(&m) - baseline[0]).abs() < 1e-9,
        "{}",
        projected(&m)
    );
    // One half done: the parent is still one piece left (half a piece of it paid for).
    pay(&m, 0.5);
    m.half_finished();
    m.piece_finished(Some(0.5), 0);
    assert!(
        (projected(&m) - baseline[0]).abs() < 1e-9,
        "{}",
        projected(&m)
    );
    // Both halves done: exactly the unsplit projection after piece two.
    pay(&m, 0.5);
    m.half_finished();
    m.piece_finished(Some(0.5), 0);
    assert!(
        (projected(&m) - baseline[1]).abs() < 1e-9,
        "{}",
        projected(&m)
    );
    // Repository two, whole: the same as before.
    m.begin_pieces(2, 0);
    pay(&m, 1.0);
    m.piece_finished(Some(1.0), 0);
    assert!(
        (projected(&m) - baseline[2]).abs() < 1e-9,
        "{}",
        projected(&m)
    );
}

#[test]
fn a_resume_holding_one_stored_half_projects_as_if_the_piece_never_split() {
    let baseline = unsplit_projections();
    // Stored: piece one (1.00) and one half (0.50). Both are held; the other half runs.
    let m = two_repo_meter(1.5);
    m.begin_pieces(3, 2);
    m.begin_halves(2, 1);
    pay(&m, 0.5);
    m.half_finished();
    m.piece_finished(Some(0.5), 0);
    assert!(
        (projected(&m) - baseline[1]).abs() < 1e-9,
        "{}",
        projected(&m)
    );
    assert_eq!(m.latest_checkpoint().unwrap().pieces_left, 2);
}
