//! Build item 7: run to completion. A finished piece is one absorbed line in the checkpoint
//! `finder_<repository>`; a resume replays it at no cost and runs only what is missing.

use super::*;
use crate::ai_audit::UsageMeter;
use crate::stage_telemetry::Stage;

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

fn joints_agent() -> ScriptedAgent {
    ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([])),
    ])
    .with_usage(usage(0.5))
}

fn agent_set() -> ByPiece {
    ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        ("src/b.rs", part("src/b.rs", 4, 1.0)),
        ("joints", joints_agent()),
    ])
}

async fn in_run<F: std::future::Future>(dir: &std::path::Path, fut: F) -> F::Output {
    crate::checkpoint::with_run_checkpoints(Some(dir.to_path_buf()), fut).await
}

async fn go(agent: &dyn PieceAgent, meter: &UsageMeter) -> FinderResult {
    find_repo(
        agent,
        REPO,
        &repo_files(),
        &rules(),
        &corpus(),
        &RunControls::default(),
        &two_pieces(),
        Some(meter),
    )
    .await
    .unwrap()
}

fn places(rows: &[crate::onboard::Finding]) -> Vec<(String, usize, String)> {
    let mut v: Vec<_> = rows
        .iter()
        .map(|f| (f.path.clone(), f.line, f.rule_id.clone()))
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_resume_replays_stored_pieces_with_no_agent_call_and_counts_their_cost_as_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let first_meter = UsageMeter::default();
    let first_agent = agent_set();
    let (rows1, _, _, failed1, _, _) = in_run(dir.path(), go(&first_agent, &first_meter)).await;
    assert!(failed1.is_empty(), "{failed1:?}");
    assert_eq!(
        first_agent.runs.load(Ordering::SeqCst),
        3,
        "two pieces and the joints"
    );

    let meter = UsageMeter::default();
    let second_agent = agent_set();
    let (rows2, _, _, failed2, _, extras) = in_run(dir.path(), go(&second_agent, &meter)).await;
    assert!(failed2.is_empty(), "{failed2:?}");
    assert_eq!(
        second_agent.runs.load(Ordering::SeqCst),
        0,
        "nothing is paid twice"
    );
    assert_eq!(second_agent.follow_ups.load(Ordering::SeqCst), 0);
    assert_eq!(
        places(&rows1),
        places(&rows2),
        "the replay gives the same problems"
    );
    assert_eq!(
        extras.coverage.len(),
        2,
        "coverage is read again from the stored trace"
    );
    let usage = meter.snapshot();
    assert_eq!(usage.calls, 3, "the stored turns are counted");
    assert!(
        (usage.resumed_cost_usd - 2.5).abs() < 1e-9,
        "{}",
        usage.resumed_cost_usd
    );
}

#[tokio::test]
async fn a_resume_runs_only_the_missing_piece() {
    let dir = tempfile::tempdir().unwrap();
    // First attempt: part two hits its turn limit.
    let first = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        (
            "src/b.rs",
            part("src/b.rs", 4, 1.0).with_stop(AgentStop::TurnLimit("60 turns".into())),
        ),
        ("joints", joints_agent()),
    ]);
    let meter = UsageMeter::default();
    let (_, _, _, failed, _, _) = in_run(dir.path(), go(&first, &meter)).await;
    assert_eq!(failed.len(), 1, "{failed:?}");

    let second = agent_set();
    let meter2 = UsageMeter::default();
    let (_, _, _, failed2, _, _) = in_run(dir.path(), go(&second, &meter2)).await;
    assert!(failed2.is_empty(), "{failed2:?}");
    let seen = second.seen.lock().unwrap().clone();
    assert!(
        !seen.contains(&"src/a.rs".to_string()),
        "part one is replayed: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|k| *k == "src/b.rs").count(),
        1,
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_failed_piece_is_a_failed_part_with_files_class_and_attempts_and_its_turns_are_wasted() {
    let dir = tempfile::tempdir().unwrap();
    let agent = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        (
            "src/b.rs",
            part("src/b.rs", 4, 2.0).with_stop(AgentStop::TurnLimit("60 turns".into())),
        ),
        ("joints", joints_agent()),
    ]);
    let meter = UsageMeter::default();
    let (_, _, _, failed, _, extras) = in_run(dir.path(), go(&agent, &meter)).await;
    assert_eq!(failed.len(), 1, "{failed:?}");
    let f = &failed[0];
    assert_eq!(f.repo, REPO);
    assert!(f.pass.contains("part 2 of 2"), "{}", f.pass);
    assert_eq!(f.class, crate::ai_audit::FailureClass::Content);
    assert!(!f.attempts.is_empty());
    assert_eq!(f.files, vec!["src/b.rs".to_string()]);
    assert!(extras
        .partial_rules
        .iter()
        .any(|p| p.rule_id == "SEC-TWO-1"));
    let telemetry = meter.stage_telemetry(0);
    let finder = telemetry
        .stages
        .iter()
        .find(|s| s.stage == Stage::Finder)
        .expect("finder stage recorded");
    assert!(finder.usage.wasted_calls >= 1, "{:?}", finder.usage);
    assert!(
        (finder.usage.wasted_cost_usd - 2.0).abs() < 1e-9,
        "{:?}",
        finder.usage
    );
}

#[tokio::test]
async fn a_refused_piece_is_retried_in_two_halves_on_a_resume() {
    // Three files: part one is a.rs, part two is b.rs and c.rs.
    let mut files = repo_files();
    files.push(("src/c.rs".to_string(), rust_file(3)));
    let dir = tempfile::tempdir().unwrap();
    let a = part("src/a.rs", 6, 1.0);
    let bc_refused = ScriptedAgent::new(vec![])
        .with_stop(AgentStop::Refused("declined".into()))
        .with_usage(usage(0.2));
    let first = ByPiece::new(vec![
        ("src/a.rs", a.clone()),
        ("src/b.rs", bc_refused),
        ("joints", joints_agent()),
    ]);
    let meter = UsageMeter::default();
    let (_, _, _, failed, _, _) = in_run(
        dir.path(),
        find_repo(
            &first,
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
    assert!(
        failed.iter().any(|f| f.reason.contains("refused")),
        "{failed:?}"
    );
    // Resume: the model now answers; the piece is run as two halves, b.rs and c.rs each alone.
    let second = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        ("src/b.rs", part("src/b.rs", 4, 1.0)),
        ("src/c.rs", part("src/c.rs", 3, 1.0)),
        ("joints", joints_agent()),
    ]);
    let meter2 = UsageMeter::default();
    let (_, _, _, failed2, _, extras) = in_run(
        dir.path(),
        find_repo(
            &second,
            REPO,
            &files,
            &rules(),
            &corpus(),
            &RunControls::default(),
            &two_pieces(),
            Some(&meter2),
        ),
    )
    .await
    .unwrap();
    assert!(failed2.is_empty(), "{failed2:?}");
    let seen = second.seen.lock().unwrap().clone();
    assert!(!seen.contains(&"src/a.rs".to_string()), "{seen:?}");
    assert!(
        seen.contains(&"src/b.rs".to_string()) && seen.contains(&"src/c.rs".to_string()),
        "the refused piece ran as two halves: {seen:?}"
    );
    assert_eq!(extras.coverage.len(), 3, "three pieces after the split");
    // The refused parent's stored cost (0.2) is replayed as resumed, with the stored part one
    // (1.0): the halves' own turns are fresh.
    let usage = meter2.snapshot();
    assert!(
        (usage.resumed_cost_usd - 1.2).abs() < 1e-9,
        "the refused parent's cost is replayed as resumed: {}",
        usage.resumed_cost_usd
    );
}

#[tokio::test]
async fn a_single_file_the_model_refused_is_disclosed_and_not_tried_a_third_time() {
    let dir = tempfile::tempdir().unwrap();
    let refused = || {
        ByPiece::new(vec![
            ("src/a.rs", part("src/a.rs", 6, 1.0)),
            (
                "src/b.rs",
                ScriptedAgent::new(vec![]).with_stop(AgentStop::Refused("declined".into())),
            ),
            ("joints", joints_agent()),
        ])
    };
    let meter = UsageMeter::default();
    let _ = in_run(dir.path(), go(&refused(), &meter)).await;
    let again = refused();
    let meter2 = UsageMeter::default();
    let (_, _, _, failed, _, _) = in_run(dir.path(), go(&again, &meter2)).await;
    assert!(
        failed
            .iter()
            .any(|f| f.files == vec!["src/b.rs".to_string()]),
        "{failed:?}"
    );
    assert!(
        !again.seen.lock().unwrap().contains(&"src/b.rs".to_string()),
        "the refused file is not sent a third time"
    );
}

#[tokio::test]
async fn a_piece_the_spend_limit_refuses_is_recorded_not_run_with_its_files() {
    let meter = UsageMeter::with_controls(RunControls {
        scope: Default::default(),
        max_cost_usd: Some(0.5),
        ..RunControls::default()
    });
    // The first piece costs more than the whole limit; the second is refused.
    let agent = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 3.0)),
        ("src/b.rs", part("src/b.rs", 4, 3.0)),
        ("joints", joints_agent()),
    ]);
    let (_, _, _, failed, _, extras) = go(&agent, &meter).await;
    let refused: Vec<_> = failed
        .iter()
        .filter(|f| f.class == crate::ai_audit::FailureClass::SpendLimit)
        .collect();
    assert!(!refused.is_empty(), "{failed:?}");
    let piece = refused
        .iter()
        .find(|f| f.files == vec!["src/b.rs".to_string()])
        .unwrap_or_else(|| panic!("the refused piece names its files: {refused:?}"));
    assert!(piece.reason.starts_with("NOT RUN"), "{}", piece.reason);
    assert!(extras
        .partial_rules
        .iter()
        .any(|p| p.rule_id == "ARCH-ONE-1"));
    let record = meter
        .cost_cap_record()
        .expect("the limit recorded what it stopped");
    assert!(
        record
            .not_run_passes
            .iter()
            .any(|p| p.files == vec!["src/b.rs".to_string()]),
        "{record:?}"
    );
}

#[tokio::test]
async fn a_piece_that_cannot_start_is_a_failed_part_and_costs_nothing() {
    struct Down;
    #[async_trait::async_trait]
    impl PieceAgent for Down {
        async fn run(&self, _: &Room, _: &str, _: &PieceLimits) -> anyhow::Result<AgentRun> {
            anyhow::bail!("the Anthropic key is missing")
        }
        async fn follow_up(
            &self,
            _: &Room,
            _: &AgentRun,
            _: &FollowUp,
            _: &PieceLimits,
        ) -> anyhow::Result<AgentRun> {
            anyhow::bail!("not reached")
        }
    }
    let meter = UsageMeter::default();
    let (rows, _, _, failed, _, _) = find_repo(
        &Down,
        REPO,
        &repo_files(),
        &rules(),
        &corpus(),
        &RunControls::default(),
        &options(),
        Some(&meter),
    )
    .await
    .unwrap();
    assert!(rows.is_empty());
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].reason.contains("key is missing"));
    assert_eq!(failed[0].files.len(), 2);
    assert_eq!(meter.snapshot().calls, 0);
}

#[tokio::test]
async fn a_piece_the_spend_limit_refuses_has_a_coverage_record_listing_its_files_as_not_reviewed() {
    let meter = UsageMeter::with_controls(RunControls {
        scope: Default::default(),
        max_cost_usd: Some(0.5),
        ..RunControls::default()
    });
    let agent = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 3.0)),
        ("src/b.rs", part("src/b.rs", 4, 3.0)),
        ("joints", joints_agent()),
    ]);
    let (_, _, _, _, _, extras) = go(&agent, &meter).await;
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    for rule in ["ARCH-ONE-1", "SEC-TWO-1"] {
        ledger.record_rule(
            REPO,
            rule,
            crate::scan_ledger::RuleTier::Semantic,
            true,
            None,
            2,
            0,
        );
    }
    extras.note_after_rules(&mut ledger, REPO);
    let blocks = ledger.finder_coverage();
    let pieces: Vec<_> = blocks.iter().flat_map(|b| b.pieces.iter()).collect();
    assert_eq!(pieces.len(), 2, "both pieces have a coverage record");
    let missing = pieces
        .iter()
        .find(|p| p.not_reviewed_files == vec!["src/b.rs".to_string()])
        .unwrap_or_else(|| panic!("the piece that did not start lists its file: {pieces:?}"));
    assert_eq!(missing.not_reviewed, 1);
    assert!(!missing.complete());
}

fn joints_calls(agent: &ByPiece) -> usize {
    agent
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|k| *k == "joints")
        .count()
}

#[tokio::test]
async fn no_joints_call_is_paid_while_a_piece_has_a_failed_part_and_the_resume_makes_one() {
    let dir = tempfile::tempdir().unwrap();
    let failing = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6, 1.0)),
        (
            "src/b.rs",
            part("src/b.rs", 4, 1.0).with_stop(AgentStop::TurnLimit("60 turns".into())),
        ),
        ("joints", joints_agent()),
    ]);
    let meter = UsageMeter::default();
    let (_, _, _, failed, _, _) = in_run(dir.path(), go(&failing, &meter)).await;
    assert!(!failed.is_empty());
    assert_eq!(joints_calls(&failing), 0, "the run is incomplete already");
    let resumed = agent_set();
    let meter2 = UsageMeter::default();
    let (_, _, _, failed2, _, _) = in_run(dir.path(), go(&resumed, &meter2)).await;
    assert!(failed2.is_empty(), "{failed2:?}");
    assert_eq!(joints_calls(&resumed), 1, "the resume runs it once");
}
