//! The shared types, the limits and the scripted stand-in agent.

use super::*;

fn room() -> Room {
    let files = vec![
        (
            "src/a.rs".to_string(),
            "fn a() {}\nfn b() {}\nfn c() {}\n".to_string(),
        ),
        ("src/b.rs".to_string(), "fn z() {}\n".to_string()),
    ];
    let piece = Piece {
        key: "acme/app#1:00000000".into(),
        unit: "acme/app".into(),
        files: vec!["src/a.rs".into()],
    };
    Room::build(&piece, &files, "# checklist\n")
}

fn limits() -> PieceLimits {
    PieceLimits::for_piece(1, 1.0)
}

#[test]
fn piece_limits_follow_the_design() {
    assert_eq!(PieceLimits::turns_for(0), 60);
    assert_eq!(PieceLimits::turns_for(15), 60);
    assert_eq!(PieceLimits::turns_for(100), 230);
    assert_eq!(PieceLimits::turns_for(10_000), 400);
    let l = PieceLimits::for_piece(100, 0.5);
    assert_eq!(l.max_turn_tokens, 32_000);
    assert_eq!(l.ceiling_usd, 3.0, "at least $3");
    assert_eq!(
        PieceLimits::for_piece(100, 2.0).ceiling_usd,
        8.0,
        "four times the estimate"
    );
}

#[tokio::test]
async fn a_scripted_agent_plays_its_calls_against_the_room_and_records_the_trace() {
    let agent = ScriptedAgent::new(vec![
        ScriptStep::Ls(String::new()),
        ScriptStep::Read {
            path: "src/a.rs".into(),
            first_line: 1,
            count: 2,
        },
        ScriptStep::Grep("fn".into()),
        ScriptStep::Glob("src/*.rs".into()),
        ScriptStep::Write {
            path: "_camerata/out/problems-1.json".into(),
            text: "[]".into(),
        },
        ScriptStep::Write {
            path: "src/a.rs".into(),
            text: "hacked".into(),
        },
    ]);
    let r = room();
    let run = agent.run(&r, "brief", &limits()).await.expect("runs");
    assert_eq!(run.stop, AgentStop::Finished);
    assert_eq!(
        run.trace.len(),
        5,
        "the refused write is not recorded: {:?}",
        run.trace
    );
    assert_eq!(
        run.trace[1],
        ToolEvent::Read {
            path: "src/a.rs".into(),
            first_line: 1,
            line_count: 2,
            lines_returned: 2
        }
    );
    assert_eq!(
        run.outputs
            .get("_camerata/out/problems-1.json")
            .map(String::as_str),
        Some("[]")
    );
    assert_eq!(run.usage.len(), 1, "one entry for each turn");
}

#[tokio::test]
async fn a_scripted_read_of_a_missing_file_returns_no_lines_and_is_recorded_as_such() {
    let agent = ScriptedAgent::new(vec![ScriptStep::Read {
        path: "/etc/passwd".into(),
        first_line: 1,
        count: 5,
    }]);
    let run = agent.run(&room(), "brief", &limits()).await.expect("runs");
    assert_eq!(
        run.trace,
        vec![ToolEvent::Read {
            path: "/etc/passwd".into(),
            first_line: 1,
            line_count: 5,
            lines_returned: 0
        }]
    );
}

#[tokio::test]
async fn a_scripted_agent_can_end_on_a_limit_and_send_again_plays_the_second_script() {
    let agent = ScriptedAgent::new(vec![])
        .with_stop(AgentStop::TurnLimit)
        .with_again(vec![ScriptStep::Read {
            path: "src/a.rs".into(),
            first_line: 1,
            count: 10,
        }]);
    let r = room();
    let first = agent.run(&r, "brief", &limits()).await.expect("runs");
    assert_eq!(first.stop, AgentStop::TurnLimit);
    let again = agent
        .send_again(&r, &first, &["src/a.rs".to_string()], &limits())
        .await
        .expect("runs");
    assert_eq!(again.trace.len(), 1);
}

#[test]
fn a_finder_choice_serializes_by_name() {
    assert_eq!(
        serde_json::to_string(&FinderChoice::Agent).expect("serializes"),
        "\"Agent\""
    );
}

#[test]
fn the_stub_signatures_exist_for_later_builds() {
    // Compile-time contract: these must exist with these shapes. Nothing calls them yet.
    let _: fn() -> api_agent::ApiPieceAgent = api_agent::ApiPieceAgent::default;
    let _: fn() -> cli_agent::CliPieceAgent = cli_agent::CliPieceAgent::default;
    let _ = run::find_repo;
    let _: fn(&[crate::onboard::Finding], &[crate::onboard::Finding]) -> run_diff::RunDiff =
        run_diff::diff_runs;
    let _: fn(&run_diff::RunDiff) -> String = run_diff::operator_note;
}
