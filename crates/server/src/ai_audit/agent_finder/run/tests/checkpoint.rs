//! The cost checkpoint, against the scripted stand-in agent (no model is called): a run whose
//! projected total would go past the approved amount stops BEFORE the next piece through the
//! spend limit's own stop, and a resume with a raised limit finishes it, paying nothing twice.

use std::sync::Mutex;

use super::super::FinderExtras;
use super::*;
use crate::ai_audit::{FailedPass, UsageMeter};
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
        scope: Default::default(),
        max_cost_usd: Some(limit),
        stored_spend_usd: stored,
        checkpoint: CheckpointSettings {
            plan: Some(CheckpointPlan {
                per_piece_usd: 1.0,
                later_stages_usd: 0.0,
                assumed_problems: None,
                ..CheckpointPlan::default()
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
         $9.00."
    );
    // The advised limit holds back the hard limit's reserve: the largest call so far, $3.00.
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
    assert_eq!(
        lines[0],
        "Spent $3.00 of $5.00 approved; projected $6.00 for the finder stage"
    );
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

#[tokio::test]
async fn a_two_repository_run_pauses_in_the_first_with_the_whole_runs_pieces_priced() {
    let seen: Lines = Lines::default();
    // The run plans 4 pieces (2 for each repository) at $1.00; each really costs $3.00.
    let mut c = controls(10.0, 0.0, None, &seen);
    c.checkpoint.plan.as_mut().unwrap().pieces = 4;
    let meter = UsageMeter::with_controls(c.clone());
    let agent = agents(3.0);
    let (_, _, _, failed, _, _) = go(&agent, &meter, &c).await;
    // After the first piece: 3 spent + 3 pieces of the run left at 3 = 12, over 10.
    let stop = meter
        .checkpoint_stop()
        .expect("paused in the first repository");
    assert_eq!(stop.pieces_left, 3, "{stop:?}");
    assert!(
        (stop.new_estimate_usd.unwrap() - 12.0).abs() < 1e-9,
        "{stop:?}"
    );
    assert!(!failed.is_empty());
    // The second repository starts nothing.
    let other = find_repo(
        &agent,
        "acme/web",
        &repo_files(),
        &rules(),
        &corpus(),
        &c,
        &two_pieces(),
        Some(&meter),
    )
    .await
    .unwrap();
    assert_eq!(agent.seen.lock().unwrap().len(), 1, "one piece in all");
    assert!(!other.3.is_empty());
}

// ─── a piece that fails the same way every time is cut in two on the resume ──────────────────

fn quick_joints() -> ScriptedAgent {
    ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([])),
    ])
    .with_usage(usage(0.5))
}

fn three_files() -> Vec<(String, String)> {
    let mut files = repo_files();
    files.push(("src/c.rs".to_string(), rust_file(3)));
    files
}

async fn run_three(
    agent: &dyn PieceAgent,
    dir: &std::path::Path,
    meter: &UsageMeter,
) -> FinderResult {
    in_run(
        dir,
        find_repo(
            agent,
            REPO,
            &three_files(),
            &rules(),
            &corpus(),
            &RunControls::default(),
            &two_pieces(),
            Some(meter),
        ),
    )
    .await
    .unwrap()
}

/// Part one (a.rs) finishes and part two (b.rs and c.rs) ends on `stop`; then the resume gets a
/// model that answers. Returns what the resume ran, its failed parts, its extras and the cost it
/// replayed as resumed. `expect_halves` says which scripts the resume is given: one for each half,
/// or one for the whole of part two.
async fn resume_after(
    stop: AgentStop,
    expect_halves: bool,
) -> (Vec<String>, Vec<FailedPass>, FinderExtras, f64) {
    let dir = tempfile::tempdir().unwrap();
    let stopped = ScriptedAgent::new(vec![])
        .with_stop(stop)
        .with_usage(usage(0.2));
    let first = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        ("src/b.rs", stopped),
        ("joints", quick_joints()),
    ]);
    let meter = UsageMeter::default();
    let (_, _, _, failed, _, _) = run_three(&first, dir.path(), &meter).await;
    assert_eq!(
        failed.len(),
        1,
        "part two is the one failed part: {failed:?}"
    );

    let mut scripts = vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        ("joints", quick_joints()),
    ];
    if expect_halves {
        scripts.push(("src/b.rs", part("src/b.rs", 4, 1.0)));
        scripts.push(("src/c.rs", part("src/c.rs", 3, 1.0)));
    } else {
        scripts.push((
            "src/b.rs",
            ScriptedAgent::new(full_script(
                &[("src/b.rs", 4), ("src/c.rs", 3)],
                vec![],
                vec![],
                vec![
                    answer_clean("ARCH-ONE-1", "src/b.rs", 1),
                    answer_clean("SEC-TWO-1", "src/c.rs", 1),
                ],
            ))
            .with_usage(usage(1.0)),
        ));
    }
    let second = ByPiece::new(scripts);
    let meter2 = UsageMeter::default();
    let (_, _, _, failed2, _, extras) = run_three(&second, dir.path(), &meter2).await;
    let seen = second.seen.lock().unwrap().clone();
    (seen, failed2, extras, meter2.snapshot().resumed_cost_usd)
}

/// The halves together read exactly the parent's files, each in full, and no coverage record
/// stands for the parent itself.
fn assert_halves_cover_the_parent(extras: &FinderExtras) {
    assert_eq!(extras.coverage.len(), 3, "a.rs plus the two halves");
    let mut read: Vec<String> = Vec::new();
    for c in &extras.coverage {
        for (file, state) in &c.files {
            assert_eq!(*state, FileState::ReadInFull, "{file}");
            read.push(file.clone());
        }
        assert!(c.still_unread.is_empty(), "{:?}", c.still_unread);
    }
    read.sort();
    assert_eq!(read, ["src/a.rs", "src/b.rs", "src/c.rs"], "each file once");
    let halves: Vec<&str> = extras
        .coverage
        .iter()
        .map(|c| c.piece_key.as_str())
        .filter(|k| k.ends_with("/a") || k.ends_with("/b"))
        .collect();
    assert_eq!(
        halves.len(),
        2,
        "{halves:?}: the parent has no record of its own"
    );
}

#[tokio::test]
async fn a_piece_that_hit_its_turn_limit_runs_as_two_halves_on_the_resume_and_nothing_twice() {
    let (seen, failed, extras, resumed) =
        resume_after(AgentStop::TurnLimit("60 turns".into()), true).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(
        !seen.contains(&"src/a.rs".to_string()),
        "the finished part is replayed, not run: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|k| *k == "src/b.rs").count(),
        1,
        "{seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|k| *k == "src/c.rs").count(),
        1,
        "{seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|k| *k == "joints").count(),
        1,
        "{seen:?}"
    );
    assert_halves_cover_the_parent(&extras);
    // The stored part one (1.0) and the stopped parent's paid turns (0.2) are replayed as resumed.
    assert!((resumed - 1.2).abs() < 1e-9, "{resumed}");
}

#[tokio::test]
async fn a_piece_that_reached_its_own_spend_ceiling_is_halved_but_the_runs_limit_is_not_a_reason() {
    let (seen, failed, extras, _) = resume_after(
        AgentStop::SpendLimit(
            "the piece's spend ceiling of $4.00 was reached after 9 turns".into(),
        ),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(seen.contains(&"src/c.rs".to_string()), "{seen:?}");
    assert_halves_cover_the_parent(&extras);
}

#[tokio::test]
async fn a_reply_cut_off_at_the_output_limit_twice_is_halved_on_the_resume() {
    let (seen, failed, extras, _) = resume_after(
        AgentStop::Failed("the reply was cut off at the output limit of 32000 tokens twice".into()),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(seen.contains(&"src/c.rs".to_string()), "{seen:?}");
    assert_halves_cover_the_parent(&extras);
}

#[tokio::test]
async fn a_prompt_too_long_400_is_halved_on_the_resume() {
    let (seen, failed, extras, _) = resume_after(
        AgentStop::Failed(
            "Anthropic API HTTP 400: prompt is too long: 250000 tokens > 200000 maximum".into(),
        ),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(seen.contains(&"src/c.rs".to_string()), "{seen:?}");
    assert_halves_cover_the_parent(&extras);
}

#[tokio::test]
async fn repeated_time_outs_are_halved_on_the_resume() {
    let (seen, failed, extras, _) = resume_after(
        AgentStop::Failed(
            "Anthropic API request failed: error sending request: operation timed out".into(),
        ),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(seen.contains(&"src/c.rs".to_string()), "{seen:?}");
    assert_halves_cover_the_parent(&extras);
}

#[tokio::test]
async fn a_transient_stop_reruns_the_piece_whole_and_never_splits_it() {
    let transient = [
        AgentStop::Failed("Anthropic API HTTP 429: rate limit exceeded".into()),
        AgentStop::Failed("Anthropic API HTTP 529: overloaded".into()),
        AgentStop::Failed("Anthropic API request failed: connection reset by peer".into()),
        AgentStop::Failed(
            "the run stopped because the provider failed repeatedly; no further request was sent"
                .into(),
        ),
        AgentStop::SpendLimit("the run's spend limit blocks the next turn of this piece".into()),
    ];
    for stop in transient {
        let (seen, failed, _, _) = resume_after(stop.clone(), false).await;
        assert!(failed.is_empty(), "{stop:?}: {failed:?}");
        assert!(
            !seen.contains(&"src/c.rs".to_string()),
            "{stop:?} must not split: {seen:?}"
        );
        assert_eq!(
            seen.iter().filter(|k| *k == "src/b.rs").count(),
            1,
            "{stop:?}: {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_one_file_piece_that_hit_its_turn_limit_cannot_be_halved_and_stays_a_named_failed_part() {
    let dir = tempfile::tempdir().unwrap();
    let stopped = || {
        ByPiece::new(vec![
            ("src/a.rs", part("src/a.rs", 6, 1.0)),
            (
                "src/b.rs",
                part("src/b.rs", 4, 1.0).with_stop(AgentStop::TurnLimit("60 turns".into())),
            ),
            ("joints", quick_joints()),
        ])
    };
    let files = repo_files();
    for attempt in 0..2 {
        let agent = stopped();
        let meter = UsageMeter::default();
        let (_, _, _, failed, _, _) = in_run(
            dir.path(),
            find_repo(
                &agent,
                REPO,
                &files,
                &rules(),
                &corpus(),
                &RunControls::default(),
                &two_pieces(),
                Some(&meter),
            ),
        )
        .await
        .unwrap();
        assert_eq!(failed.len(), 1, "attempt {attempt}: {failed:?}");
        assert_eq!(failed[0].files, vec!["src/b.rs".to_string()]);
        assert!(
            failed[0].reason.contains("turn limit"),
            "{}",
            failed[0].reason
        );
        let seen = agent.seen.lock().unwrap().clone();
        assert_eq!(
            seen.iter().filter(|k| *k == "src/b.rs").count(),
            1,
            "attempt {attempt}: the single file runs once, whole: {seen:?}"
        );
    }
}
