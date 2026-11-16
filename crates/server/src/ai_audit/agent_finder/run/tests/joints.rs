//! Build item 6: leads and joints.

use super::*;
use crate::onboard::Finding;

fn two_pieces() -> FinderOptions {
    let mut o = options();
    o.force_pieces = Some(2);
    o
}

/// The piece scripts: part one owns a.rs and reports `a_problems`; part two owns b.rs and writes
/// `b_problems` (a problem about a.rs there is a lead).
fn parts(a_problems: Vec<Value>, b_problems: Vec<Value>) -> (ScriptedAgent, ScriptedAgent) {
    let a = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        a_problems,
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    ));
    let b = ScriptedAgent::new(full_script(
        &[("src/b.rs", 4)],
        b_problems,
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/b.rs", 1),
            answer_clean("SEC-TWO-1", "src/b.rs", 2),
        ],
    ));
    (a, b)
}

async fn two_part_run(
    a: ScriptedAgent,
    b: ScriptedAgent,
    joints: ScriptedAgent,
) -> (
    ByPiece,
    Vec<Finding>,
    Vec<crate::ai_audit::FailedPass>,
    super::super::FinderExtras,
) {
    let agent = ByPiece::new(vec![("src/a.rs", a), ("src/b.rs", b), ("joints", joints)]);
    let (rows, _, _, failed, _, extras) =
        run_with(&agent, &repo_files(), &rules(), &corpus(), &two_pieces()).await;
    (agent, rows, failed, extras)
}

#[test]
fn the_forced_split_puts_one_file_in_each_piece() {
    let files = repo_files();
    let units = super::super::super::pieces::units_for(REPO, &files);
    let pieces = super::super::super::pieces::plan_pieces(
        &units,
        &files,
        super::super::super::pieces::TARGET_BYTES,
        Some(2),
    );
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].files, vec!["src/a.rs".to_string()]);
    assert_eq!(pieces[1].files, vec!["src/b.rs".to_string()]);
}

fn lead_in_a() -> Vec<Value> {
    vec![problem(1, "src/a.rs", 5, "high", "fn item_5() { run(5); }")]
}

fn a_own() -> Vec<Value> {
    vec![problem(1, "src/a.rs", 5, "high", "fn item_5() { run(5); }")]
}

#[tokio::test]
async fn a_lead_answered_covered_by_a_problem_adds_no_row() {
    // Part one reports a.rs:5 (global problem 1). Part two saw the same place from outside: lead 1.
    let (a, b) = parts(a_own(), lead_in_a());
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([{"lead": 1, "covered_by": 1}])),
    ]);
    let (agent, rows, failed, _) = two_part_run(a, b, joints).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        agent
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|k| *k == "joints")
            .count(),
        1
    );
    assert_eq!(rows.len(), 1, "the lead adds no row: {rows:?}");
    assert_eq!((rows[0].path.as_str(), rows[0].line), ("src/a.rs", 5));
}

#[tokio::test]
async fn a_lead_answered_with_a_new_problem_adds_that_row() {
    let (a, b) = parts(
        a_own(),
        vec![problem(
            1,
            "src/a.rs",
            3,
            "medium",
            "fn item_3() { run(3); }",
        )],
    );
    // The joints agent numbers its problems from 2 (one problem is already reported).
    let joints = ScriptedAgent::new(vec![
        out(
            "problems-1.json",
            &json!([problem(
                2,
                "src/a.rs",
                3,
                "medium",
                "fn item_3() { run(3); }"
            )]),
        ),
        out(
            "leads-answers.json",
            &json!([{"lead": 1, "new_problem": 2}]),
        ),
    ]);
    let (agent, rows, failed, _) = two_part_run(a, b, joints).await;
    assert!(failed.is_empty(), "{failed:?}");
    let brief_ok = agent.seen.lock().unwrap().contains(&"joints".to_string());
    assert!(brief_ok);
    let mut places: Vec<(String, usize)> = rows.iter().map(|f| (f.path.clone(), f.line)).collect();
    places.sort();
    assert_eq!(
        places,
        vec![("src/a.rs".to_string(), 3), ("src/a.rs".to_string(), 5)],
        "{rows:?}"
    );
    assert!(rows.iter().all(|f| !f.needs_review), "answered, not held");
}

#[tokio::test]
async fn a_lead_with_no_answer_becomes_a_problem_needing_a_decision() {
    let (a, b) = parts(
        a_own(),
        vec![problem(
            1,
            "src/a.rs",
            3,
            "medium",
            "fn item_3() { run(3); }",
        )],
    );
    let joints = ScriptedAgent::new(vec![out("problems-1.json", &json!([]))]);
    let (_, rows, failed, _) = two_part_run(a, b, joints).await;
    assert!(
        failed.is_empty(),
        "no answer is not a failed part: {failed:?}"
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
    let held = rows
        .iter()
        .find(|f| f.line == 3)
        .expect("the lead became a row");
    assert!(held.needs_review);
    assert_eq!(held.rule_id, "AI-OPEN-QUESTION");
    assert!(held.detail.contains("needs a decision"));
}

#[tokio::test]
async fn a_lead_whose_answer_does_not_hold_up_also_needs_a_decision() {
    // `covered_by` names a problem that does not exist.
    let (a, b) = parts(
        a_own(),
        vec![problem(
            1,
            "src/a.rs",
            2,
            "medium",
            "fn item_2() { run(2); }",
        )],
    );
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([{"lead": 1, "covered_by": 7}])),
    ]);
    let (_, rows, _, _) = two_part_run(a, b, joints).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().any(|f| f.line == 2 && f.needs_review));
}

#[tokio::test]
async fn a_one_piece_repository_makes_no_joints_call() {
    let agent = ByPiece::new(vec![(
        "src/a.rs",
        ScriptedAgent::new(good_script(vec![], vec![])),
    )]);
    let (_, _, _, failed, _, _) = run(&agent).await;
    assert!(failed.is_empty());
    assert_eq!(agent.runs.load(Ordering::SeqCst), 1);
    assert!(!agent.seen.lock().unwrap().contains(&"joints".to_string()));
}

#[tokio::test]
async fn the_same_place_is_never_reported_by_two_pieces() {
    // Both parts report a.rs:5: part one as its problem, part two as a lead. One row.
    let (a, b) = parts(a_own(), lead_in_a());
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([{"lead": 1, "covered_by": 1}])),
    ]);
    let (_, rows, _, _) = two_part_run(a, b, joints).await;
    let at_place = rows
        .iter()
        .filter(|f| f.path == "src/a.rs" && f.line == 5)
        .count();
    assert_eq!(at_place, 1, "{rows:?}");
}

#[tokio::test]
async fn the_joints_room_holds_every_file_the_cards_the_leads_and_the_problems() {
    type Seen = (Vec<String>, Vec<String>, String);
    struct Spy(std::sync::Mutex<Option<Seen>>);
    #[async_trait::async_trait]
    impl PieceAgent for Spy {
        async fn run(
            &self,
            room: &Room,
            brief: &str,
            limits: &PieceLimits,
        ) -> anyhow::Result<AgentRun> {
            if !room.extra_files().is_empty() {
                *self.0.lock().unwrap() = Some((
                    room.piece_files().to_vec(),
                    room.extra_files().to_vec(),
                    brief.to_string(),
                ));
                return ScriptedAgent::new(vec![out("problems-1.json", &json!([]))])
                    .run(room, brief, limits)
                    .await;
            }
            let (path, lines) = if room.piece_files()[0] == "src/a.rs" {
                ("src/a.rs", 6)
            } else {
                ("src/b.rs", 4)
            };
            ScriptedAgent::new(full_script(
                &[(path, lines)],
                vec![],
                vec![],
                vec![
                    answer_clean("ARCH-ONE-1", path, 1),
                    answer_clean("SEC-TWO-1", path, 2),
                ],
            ))
            .run(room, brief, limits)
            .await
        }

        async fn follow_up(
            &self,
            room: &Room,
            prior: &AgentRun,
            turn: &FollowUp,
            limits: &PieceLimits,
        ) -> anyhow::Result<AgentRun> {
            ScriptedAgent::new(vec![])
                .follow_up(room, prior, turn, limits)
                .await
        }
    }
    let spy = Spy(Default::default());
    let _ = run_with(&spy, &repo_files(), &rules(), &corpus(), &two_pieces()).await;
    let (piece_files, extras, brief) = spy.0.lock().unwrap().clone().expect("the joints call ran");
    assert_eq!(
        piece_files,
        vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
    );
    assert_eq!(
        extras,
        vec![
            "_camerata/joints/cards.md".to_string(),
            "_camerata/joints/leads.md".to_string(),
            "_camerata/joints/problems.md".to_string()
        ]
    );
    assert!(brief.contains("from 1"), "{brief}");
}

#[tokio::test]
async fn a_lead_made_from_a_problem_keeps_the_problems_severity() {
    // Part two writes a high problem about a.rs: a lead. Nothing settles it, so it becomes a
    // problem needing a decision at the severity the source problem had.
    let (a, b) = parts(
        vec![],
        vec![problem(1, "src/a.rs", 3, "high", "fn item_3() { run(3); }")],
    );
    let joints = ScriptedAgent::new(vec![out("problems-1.json", &json!([]))]);
    let (_, rows, _, _) = two_part_run(a, b, joints).await;
    let held = rows.iter().find(|f| f.line == 3).expect("the lead row");
    assert_eq!(held.rule_id, "AI-OPEN-QUESTION");
    assert_eq!(held.severity, "high");
}

#[tokio::test]
async fn a_lead_answered_covered_by_a_problem_leaves_an_operator_note() {
    let (a, b) = parts(a_own(), lead_in_a());
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([{"lead": 1, "covered_by": 1}])),
    ]);
    let (_, _, _, extras) = two_part_run(a, b, joints).await;
    assert!(
        extras
            .notes
            .iter()
            .any(|n| n.contains("covered by problem 1") && n.contains("src/a.rs")),
        "{:?}",
        extras.notes
    );
}

#[tokio::test]
async fn a_covered_by_answer_for_a_line_outside_the_problems_range_needs_a_decision() {
    // The covering problem is at a.rs:5 only; the lead is at a.rs:2 in the same file.
    let (a, b) = parts(
        a_own(),
        vec![problem(
            1,
            "src/a.rs",
            2,
            "medium",
            "fn item_2() { run(2); }",
        )],
    );
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([{"lead": 1, "covered_by": 1}])),
    ]);
    let (_, rows, _, extras) = two_part_run(a, b, joints).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().any(|f| f.line == 2 && f.needs_review),
        "{rows:?}"
    );
    assert!(
        extras.notes.iter().any(|n| n.contains("outside")),
        "{:?}",
        extras.notes
    );
}

#[tokio::test]
async fn joints_skipped_because_a_part_failed_leave_an_operator_note() {
    let (a, b) = parts(a_own(), vec![]);
    let b = b.with_stop(AgentStop::TurnLimit("60 turns".into()));
    let joints = ScriptedAgent::new(vec![out("problems-1.json", &json!([]))]);
    let (agent, _, failed, extras) = two_part_run(a, b, joints).await;
    assert!(!failed.is_empty(), "a part failed");
    assert!(
        !agent.seen.lock().unwrap().contains(&"joints".to_string()),
        "no joints call while a part failed"
    );
    assert!(
        extras
            .notes
            .iter()
            .any(|n| n.contains("joints") && n.contains("not reviewed") && n.contains("failed")),
        "{:?}",
        extras.notes
    );
}

fn joints_problem() -> Value {
    problem(2, "src/a.rs", 3, "medium", "fn item_3() { run(3); }")
}

#[tokio::test]
async fn a_joints_problem_with_sure_high_is_kept_as_sure_and_the_run_completes() {
    let (a, b) = parts(a_own(), vec![]);
    let mut bad = joints_problem();
    bad["sure"] = json!("high");
    let joints = ScriptedAgent::new(vec![out("problems-1.json", &json!([bad]))]);
    let (_, rows, failed, _) = two_part_run(a, b, joints.clone()).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(joints.messages().is_empty(), "no repair turn was needed");
    assert!(
        rows.iter().any(|f| f.path == "src/a.rs" && f.line == 3),
        "{rows:?}"
    );
}

#[tokio::test]
async fn an_unusable_joints_file_gets_one_repair_turn_and_the_run_completes() {
    let (a, b) = parts(a_own(), vec![]);
    let mut bad = joints_problem();
    bad["severity"] = json!("catastrophic");
    let joints = ScriptedAgent::new(vec![out("problems-1.json", &json!([bad]))]).with_follow_up(
        vec![out("problems-1.json", &json!([joints_problem()]))],
        AgentStop::Finished,
    );
    let (_, rows, failed, _) = two_part_run(a, b, joints.clone()).await;
    assert!(failed.is_empty(), "{failed:?}");
    let sent = joints.messages();
    assert_eq!(sent.len(), 1, "one repair turn: {sent:?}");
    assert!(
        sent[0].contains("problems-1.json") && sent[0].contains("catastrophic"),
        "the turn names the file and the value: {}",
        sent[0]
    );
    assert!(
        rows.iter().any(|f| f.path == "src/a.rs" && f.line == 3),
        "{rows:?}"
    );
}

#[tokio::test]
async fn a_joints_file_broken_twice_gets_one_repair_turn_then_a_failed_part() {
    let (a, b) = parts(a_own(), vec![]);
    let broken = ScriptStep::Write {
        path: "_camerata/out/problems-1.json".to_string(),
        text: "{not json".to_string(),
    };
    let joints =
        ScriptedAgent::new(vec![broken.clone()]).with_follow_up(vec![broken], AgentStop::Finished);
    let (_, _, failed, _) = two_part_run(a, b, joints.clone()).await;
    assert_eq!(joints.messages().len(), 1, "exactly one repair turn");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].pass.contains("joints"), "{failed:?}");
}
