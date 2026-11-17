//! The cost checkpoint, against the scripted stand-in agent (no model is called): a run whose
//! projected total would go past the approved amount stops BEFORE the next piece through the
//! spend limit's own stop, and a resume with a raised limit finishes it, paying nothing twice.

use std::sync::Mutex;

use super::*;
use crate::ai_audit::UsageMeter;
use crate::cost_checkpoint::{
    CheckpointPlan, CheckpointSettings, CheckpointSink, CostCheckpoint, CAUSE_PROJECTED,
    CHECKPOINT_FILE,
};

fn two_pieces() -> FinderOptions {
    let mut o = options();
    o.force_pieces = Some(2);
    o
}

fn part(path: &str, lines: usize, cost: f64) -> ScriptedAgent {
    ScriptedAgent::new(full_script(
        &[(path, lines)],
        vec![problem(1, path, 2, "medium", "fn item_2() { run(2); }")],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", path, 1),
            answer_clean("SEC-TWO-1", path, 2),
        ],
    ))
    .with_usage(usage(cost))
}

fn agents(piece_cost: f64) -> ByPiece {
    ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, piece_cost)),
        ("src/b.rs", part("src/b.rs", 4, piece_cost)),
        (
            "joints",
            ScriptedAgent::new(vec![
                out("problems-1.json", &json!([])),
                out("leads-answers.json", &json!([])),
            ])
            .with_usage(usage(0.5)),
        ),
    ])
}

type Lines = std::sync::Arc<Mutex<Vec<CostCheckpoint>>>;

fn controls(
    limit: f64,
    stored: f64,
    folder: Option<&std::path::Path>,
    seen: &Lines,
) -> RunControls {
    let seen = seen.clone();
    RunControls {
        max_cost_usd: Some(limit),
        stored_spend_usd: stored,
        checkpoint: CheckpointSettings {
            plan: Some(CheckpointPlan {
                per_piece_usd: 1.0,
                later_stages_usd: 0.0,
                assumed_problems: None,
            }),
            run_folder: folder.map(std::path::Path::to_path_buf),
            sink: CheckpointSink::new(move |cp| seen.lock().unwrap().push(cp.clone())),
        },
        ..RunControls::default()
    }
}

async fn go(agent: &dyn PieceAgent, meter: &UsageMeter, controls: &RunControls) -> FinderResult {
    find_repo(
        agent,
        REPO,
        &repo_files(),
        &rules(),
        &corpus(),
        controls,
        &two_pieces(),
        Some(meter),
    )
    .await
    .unwrap()
}

async fn in_run<F: std::future::Future>(dir: &std::path::Path, fut: F) -> F::Output {
    crate::checkpoint::with_run_checkpoints(Some(dir.to_path_buf()), fut).await
}

#[tokio::test]
async fn a_projection_over_the_limit_stops_before_the_next_piece_and_spends_nothing_more() {
    let run_dir = tempfile::tempdir().unwrap();
    let seen: Lines = Lines::default();
    // Limit $5; each piece costs $3. After the first piece: spent $3 + one piece left at $3 =
    // $6, over the limit, though only $3 is spent.
    let c = controls(5.0, 0.0, Some(run_dir.path()), &seen);
    let meter = UsageMeter::with_controls(c.clone());
    let agent = agents(3.0);
    let (_, _, _, failed, _, _) = go(&agent, &meter, &c).await;

    assert_eq!(
        agent.seen.lock().unwrap().clone(),
        vec!["src/a.rs".to_string()],
        "the second piece was never started"
    );
    assert!(
        (meter.spent_usd() - 3.0).abs() < 1e-9,
        "nothing more was paid: {}",
        meter.spent_usd()
    );
    let refused = failed
        .iter()
        .find(|f| f.files == vec!["src/b.rs".to_string()])
        .unwrap_or_else(|| panic!("the unstarted piece is a part a resume retries: {failed:?}"));
    assert_eq!(refused.class, crate::ai_audit::FailureClass::SpendLimit);
    assert!(
        refused.reason.contains(CAUSE_PROJECTED),
        "{}",
        refused.reason
    );
    assert!(!refused.reason.contains('$'), "{}", refused.reason);

    // The record: approved, spent, projected, work done and left, the new estimate and the rise.
    let stop = meter
        .checkpoint_stop()
        .expect("the run was stopped by the projection");
    assert!(stop.stopped);
    assert!((stop.approved_usd - 5.0).abs() < 1e-9);
    assert!((stop.spent_usd - 3.0).abs() < 1e-9);
    assert!((stop.projected_usd - 6.0).abs() < 1e-9);
    assert_eq!((stop.pieces_done, stop.pieces_left), (1, 1));
    assert_eq!(stop.new_estimate_usd, Some(6.0));
    assert_eq!(stop.difference_usd, Some(1.0));
    assert_eq!(
        stop.pause_message(),
        "Paused before going over the approved amount. Approved $5.00, spent $3.00, 1 piece left. \
         New estimate $6.00 (+$1.00). Approve to continue: resume with a spend limit of at least \
         $6.00."
    );
    let record = meter.cost_cap_record().expect("a limit was set");
    assert_eq!(record.cause(), CAUSE_PROJECTED);
    assert!(record.stopped_the_run());
    // One line per finished piece, then the pause.
    let lines: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(CostCheckpoint::operator_text)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], "Spent $3.00 of $5.00 approved; projected $6.00");
    assert_eq!(lines[1], stop.pause_message());
    // Recorded in the run folder.
    let on_disk: CostCheckpoint = serde_json::from_slice(
        &std::fs::read(run_dir.path().join(CHECKPOINT_FILE)).expect("the checkpoint was recorded"),
    )
    .unwrap();
    assert_eq!(on_disk, stop);
}

#[tokio::test]
async fn a_resume_with_a_raised_limit_finishes_and_pays_nothing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let seen: Lines = Lines::default();
    let first_controls = controls(5.0, 0.0, None, &seen);
    let first_meter = UsageMeter::with_controls(first_controls.clone());
    let first = agents(3.0);
    let (_, _, _, failed, _, _) =
        in_run(dir.path(), go(&first, &first_meter, &first_controls)).await;
    assert_eq!(failed.len(), 1, "{failed:?}");

    // Resume: the limit is raised past the new estimate ($6, plus room for the joints call, which
    // the projection does not price here); the stored $3 counts from the start.
    let c = controls(7.0, 3.0, None, &seen);
    let meter = UsageMeter::with_controls(c.clone());
    let second = agents(3.0);
    let (_, _, _, failed2, _, _) = in_run(dir.path(), go(&second, &meter, &c)).await;
    assert!(failed2.is_empty(), "{failed2:?}");
    let seen_pieces = second.seen.lock().unwrap().clone();
    assert!(
        !seen_pieces.contains(&"src/a.rs".to_string()),
        "the finished piece was not paid for again: {seen_pieces:?}"
    );
    assert!(
        seen_pieces.contains(&"src/b.rs".to_string()),
        "{seen_pieces:?}"
    );
    // The whole run: the first piece once ($3), the second ($3) and the joints call ($0.50).
    assert!(
        (meter.limit_spend_usd() - 6.5).abs() < 1e-9,
        "{}",
        meter.limit_spend_usd()
    );
    assert!(meter.checkpoint_stop().is_none());
}

#[tokio::test]
async fn a_run_with_no_limit_has_no_checkpoint_and_runs_as_before() {
    let seen: Lines = Lines::default();
    let mut c = controls(5.0, 0.0, None, &seen);
    c.max_cost_usd = None;
    let meter = UsageMeter::with_controls(c.clone());
    let agent = agents(30.0);
    let (_, _, _, failed, _, _) = go(&agent, &meter, &c).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(agent.seen.lock().unwrap().contains(&"src/b.rs".to_string()));
    assert!(meter.checkpoint_stop().is_none());
    assert!(meter.latest_checkpoint().is_none());
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing printed without a limit"
    );
    assert!(meter.cost_cap_record().is_none());
}

#[tokio::test]
async fn the_pre_flight_figure_stops_a_run_before_its_first_piece_when_it_is_over_the_limit() {
    let seen: Lines = Lines::default();
    let mut c = controls(1.5, 0.0, None, &seen);
    // Two pieces at $1.00 each is $2.00 against a $1.50 limit, before anything is spent.
    c.checkpoint.plan.as_mut().unwrap().per_piece_usd = 1.0;
    let meter = UsageMeter::with_controls(c.clone());
    let agent = agents(0.2);
    let (_, _, _, failed, _, _) = go(&agent, &meter, &c).await;
    assert!(agent.seen.lock().unwrap().is_empty(), "no piece was sent");
    assert!(meter.spent_usd().abs() < 1e-9);
    assert!(
        failed.iter().all(|f| f.reason.contains(CAUSE_PROJECTED)),
        "{failed:?}"
    );
}
