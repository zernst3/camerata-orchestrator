//! Build item 1: one piece, end to end.

use super::*;

#[tokio::test]
async fn a_scripted_piece_with_two_problems_returns_two_and_no_failed_part() {
    let agent = ScriptedAgent::new(good_script(
        vec![
            problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }"),
            problem(2, "src/b.rs", 3, "low", "fn item_3() { run(3); }"),
        ],
        vec![json!({"n": 1, "rule": "ARCH-ONE-1"})],
    ));
    let (rows, _proposed, recs, failed, samples, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(recs.is_empty());
    assert_eq!(rows.len(), 2, "{rows:?}");
    let cited = rows
        .iter()
        .find(|f| f.rule_id == "ARCH-ONE-1")
        .expect("the cited problem keeps its rule");
    assert_eq!((cited.path.as_str(), cited.line), ("src/a.rs", 2));
    let uncited = rows
        .iter()
        .find(|f| f.rule_id.starts_with("AI-"))
        .expect("the problem with no rule is model-reviewed");
    assert_eq!((uncited.path.as_str(), uncited.line), ("src/b.rs", 3));
    assert!(
        samples.iter().any(|s| s.stage == "ai-location-merge"),
        "{samples:?}"
    );
    assert!(samples.iter().any(|s| s.stage == "agent-finder"));
    assert_eq!(extras.coverage.len(), 1);
    assert!(extras.coverage[0].still_unread.is_empty());
    assert!(extras.partial_rules.is_empty());
    assert_eq!(extras.problems_total, 2);
    assert_eq!(
        extras.problems_without_rule, 1,
        "the share with no rule is recorded"
    );
}

#[tokio::test]
async fn the_agent_is_asked_once_and_given_no_follow_up_when_everything_is_in_order() {
    let agent = ByPiece::new(vec![(
        "src/a.rs",
        ScriptedAgent::new(good_script(vec![], vec![])),
    )]);
    let (rows, _, _, failed, _, _) = run(&agent).await;
    assert!(rows.is_empty() && failed.is_empty());
    assert_eq!(agent.runs.load(Ordering::SeqCst), 1);
    assert_eq!(agent.follow_ups.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_problem_in_a_file_outside_the_piece_is_a_lead_and_never_a_row_of_its_own_piece() {
    // The piece has one file; the agent writes a problem about the other one.
    let files = repo_files();
    let mut opts = options();
    opts.force_pieces = Some(2);
    let agent = ByPiece::new(vec![
        (
            "src/a.rs",
            ScriptedAgent::new(full_script(
                &[("src/a.rs", 6)],
                vec![problem(1, "src/b.rs", 3, "high", "fn item_3() { run(3); }")],
                vec![],
                vec![
                    answer_clean("ARCH-ONE-1", "src/a.rs", 1),
                    answer_clean("SEC-TWO-1", "src/a.rs", 2),
                ],
            )),
        ),
        (
            "src/b.rs",
            ScriptedAgent::new(full_script(
                &[("src/b.rs", 4)],
                vec![],
                vec![],
                vec![
                    answer_clean("ARCH-ONE-1", "src/b.rs", 1),
                    answer_clean("SEC-TWO-1", "src/b.rs", 2),
                ],
            )),
        ),
        (
            "joints",
            ScriptedAgent::new(vec![
                out("problems-1.json", &json!([])),
                out("leads-answers.json", &json!([{"lead": 1, "covered_by": 1}])),
            ]),
        ),
    ]);
    let (rows, _, _, _failed, _, _) = run_with(&agent, &files, &rules(), &corpus(), &opts).await;
    // Piece one's problem is about a file piece two owns: it is a lead. The joints agent could
    // not cover it with a problem at that file (there is none), so it needs a decision.
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].path, "src/b.rs");
    assert!(rows[0].needs_review, "an unsettled lead needs a decision");
}
