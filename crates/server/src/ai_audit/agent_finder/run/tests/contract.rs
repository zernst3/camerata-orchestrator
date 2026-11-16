//! The contract with the transport: every conversation is let go of when its piece ends, and a
//! step that did not end `Finished` reviewed nothing.

use super::*;

fn skip_b_script() -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    )
}

#[tokio::test]
async fn no_conversation_is_kept_after_a_piece_with_follow_ups() {
    let agent = ScriptedAgent::new(skip_b_script());
    let _ = run(&agent).await;
    assert_eq!(agent.messages().len(), 1, "a send again turn was made");
    assert_eq!(agent.kept(), 0);
}

#[tokio::test]
async fn no_conversation_is_kept_after_a_clean_single_run() {
    let agent = ScriptedAgent::new(good_script(vec![], vec![]));
    let _ = run(&agent).await;
    assert_eq!(agent.kept(), 0);
}

#[tokio::test]
async fn no_conversation_is_kept_after_a_refused_piece() {
    let agent = ScriptedAgent::new(good_script(vec![], vec![]))
        .with_stop(AgentStop::Refused("no".to_string()));
    let _ = run(&agent).await;
    assert_eq!(agent.kept(), 0);
}

#[tokio::test]
async fn no_conversation_is_kept_after_a_follow_up_that_stops_early() {
    let agent = ScriptedAgent::new(skip_b_script())
        .with_follow_up(vec![], AgentStop::TurnLimit("60 turns".to_string()));
    let _ = run(&agent).await;
    assert_eq!(agent.kept(), 0);
}

#[tokio::test]
async fn no_conversation_is_kept_after_a_two_piece_run_with_the_joints_call() {
    let mut opts = options();
    opts.force_pieces = Some(2);
    let part = |path: &str, n: usize| {
        ScriptedAgent::new(full_script(
            &[(path, n)],
            vec![],
            vec![],
            vec![
                answer_clean("ARCH-ONE-1", path, 1),
                answer_clean("SEC-TWO-1", path, 2),
            ],
        ))
    };
    let agent = ByPiece::new(vec![
        ("src/a.rs", part("src/a.rs", 6)),
        ("src/b.rs", part("src/b.rs", 4)),
        (
            "joints",
            ScriptedAgent::new(vec![
                out("problems-1.json", &json!([])),
                out("leads-answers.json", &json!([])),
            ]),
        ),
    ]);
    let _ = run_with(&agent, &repo_files(), &rules(), &corpus(), &opts).await;
    assert_eq!(agent.kept(), 0);
}

#[tokio::test]
async fn the_reads_of_a_step_that_did_not_finish_cover_nothing() {
    // The send again step reads b.rs in full but ends at its turn limit: b.rs is not reviewed.
    let agent = ScriptedAgent::new(skip_b_script()).with_follow_up(
        vec![read_all("src/b.rs", 4)],
        AgentStop::TurnLimit("60 turns".to_string()),
    );
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert_eq!(
        extras.coverage[0].files["src/b.rs"],
        FileState::NotReviewed,
        "{:?}",
        extras.coverage[0]
    );
    assert!(
        failed
            .iter()
            .any(|f| f.files.contains(&"src/b.rs".to_string())),
        "{failed:?}"
    );
}

#[tokio::test]
async fn a_clean_answer_resting_on_a_read_of_an_unfinished_step_is_not_accepted() {
    // The first run ends at its turn limit having read both files and answered clean: nothing in
    // it counts, the whole part is a failed part and the answers read as not completed.
    let agent = ScriptedAgent::new(good_script(vec![], vec![]))
        .with_stop(AgentStop::TurnLimit("60 turns".to_string()));
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(!failed.is_empty());
    assert!(extras.coverage[0]
        .files
        .values()
        .all(|s| *s == FileState::NotReviewed));
    assert!(extras
        .partial_rules
        .iter()
        .any(|p| p.rule_id == "SEC-TWO-1"));
}
