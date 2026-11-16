//! The shared types, the limits and the scripted stand-in agent.

use super::cli_agent::EngagementKind;
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
    Room::build(&piece, &files, "# checklist\n").room
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
        .with_stop(AgentStop::TurnLimit("40 turns".into()))
        .with_again(vec![ScriptStep::Read {
            path: "src/a.rs".into(),
            first_line: 1,
            count: 10,
        }]);
    let r = room();
    let first = agent.run(&r, "brief", &limits()).await.expect("runs");
    assert_eq!(first.stop, AgentStop::TurnLimit("40 turns".into()));
    let again = send_again(&agent, &r, &first, &["src/a.rs".to_string()], &limits())
        .await
        .expect("runs");
    assert_eq!(again.trace.len(), 1);
    assert_eq!(
        again.stop,
        AgentStop::Finished,
        "a follow-up ends on its own"
    );
}

#[tokio::test]
async fn send_again_is_a_helper_over_follow_up_and_sends_the_files_in_full() {
    let agent = ScriptedAgent::new(vec![]);
    let r = room();
    let first = agent.run(&r, "brief", &limits()).await.expect("runs");
    let again = send_again(&agent, &r, &first, &["src/a.rs".to_string()], &limits())
        .await
        .expect("runs");
    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].starts_with(SEND_AGAIN_MESSAGE),
        "{}",
        messages[0]
    );
    assert!(messages[0].contains("--- src/a.rs ---"));
    assert!(
        messages[0].contains("fn c() {}"),
        "the whole file is in the message"
    );
    assert_eq!(again.shown_in_full, vec!["src/a.rs".to_string()]);
    assert!(
        first.shown_in_full.is_empty(),
        "tool reads are not shown_in_full"
    );
}

#[tokio::test]
async fn a_follow_up_that_sends_no_file_shows_none_and_a_failed_turn_shows_none() {
    let agent = ScriptedAgent::new(vec![])
        .with_follow_up(vec![], AgentStop::Failed("provider error".into()));
    let r = room();
    let first = agent.run(&r, "brief", &limits()).await.expect("runs");
    let failed = agent
        .follow_up(
            &r,
            &first,
            &FollowUp::with_files("look again", &["src/a.rs".to_string()]),
            &limits(),
        )
        .await
        .expect("runs");
    assert_eq!(failed.stop, AgentStop::Failed("provider error".into()));
    assert!(failed.shown_in_full.is_empty(), "the turn did not complete");
    // The queue is empty now: the next follow-up plays the `again` script and completes.
    let repair = agent
        .follow_up(
            &r,
            &first,
            &FollowUp::text("fix problems-1.json"),
            &limits(),
        )
        .await
        .expect("runs");
    assert_eq!(repair.stop, AgentStop::Finished);
    assert!(repair.shown_in_full.is_empty());
    assert_eq!(agent.messages()[1], "fix problems-1.json");
}

#[tokio::test]
async fn after_a_paid_turn_a_failure_is_ok_with_the_usage_never_err() {
    let usage = TurnUsage {
        input_tokens: 1_000,
        output_tokens: 200,
        cost_usd: Some(0.42),
        ..TurnUsage::default()
    };
    let agent = ScriptedAgent::new(vec![ScriptStep::Ls(String::new())])
        .with_stop(AgentStop::Failed(
            "the provider dropped the connection".into(),
        ))
        .with_usage(usage.clone());
    let r = room();
    let run = agent
        .run(&r, "brief", &limits())
        .await
        .expect("a failure after a paid turn is Ok, never Err");
    assert_eq!(
        run.stop,
        AgentStop::Failed("the provider dropped the connection".into())
    );
    assert_eq!(run.usage, vec![usage.clone()], "the paid turn is counted");
    let follow = agent
        .follow_up(&r, &run, &FollowUp::text("again"), &limits())
        .await
        .expect("runs");
    assert_eq!(follow.usage, vec![usage]);
}

#[test]
fn the_stop_reasons_carry_text_and_the_run_carries_a_serialisable_conversation_handle() {
    let stops = [
        AgentStop::Finished,
        AgentStop::TurnLimit("60 turns".into()),
        AgentStop::SpendLimit("the piece ceiling of $3.00".into()),
        AgentStop::Refused("I cannot help with that".into()),
        AgentStop::Failed("x".into()),
    ];
    for stop in stops {
        let json = serde_json::to_string(&stop).expect("serializes");
        assert_eq!(
            serde_json::from_str::<AgentStop>(&json).expect("round trips"),
            stop
        );
    }
    let mut run = AgentRun::empty(AgentStop::Finished);
    assert!(run.conversation.is_empty());
    assert!(run.shown_in_full.is_empty());
    run.conversation = ConversationHandle::new("session-123");
    let json = serde_json::to_string(&run).expect("serializes");
    let back: AgentRun = serde_json::from_str(&json).expect("round trips");
    assert_eq!(back.conversation.as_str(), "session-123");
    assert_eq!(back, run);
}

#[test]
fn the_factory_builds_each_backend_and_never_falls_back_quietly() {
    let controls = crate::cost_cap::RunControls::default();
    let dir = std::path::Path::new("/Users/auditor/.camerata/runs/r1");
    let meter = || std::sync::Arc::new(crate::ai_audit::UsageMeter::default());
    assert!(piece_agent_for(
        &controls,
        crate::llm::Backend::Cli,
        None,
        dir,
        meter(),
        EngagementKind::Client
    )
    .is_ok());
    assert!(piece_agent_for(
        &controls,
        crate::llm::Backend::Api,
        Some("sk-test"),
        dir,
        meter(),
        EngagementKind::Client
    )
    .is_ok());
    for key in [None, Some(""), Some("   ")] {
        let err = piece_agent_for(
            &controls,
            crate::llm::Backend::Api,
            key,
            dir,
            meter(),
            EngagementKind::Client,
        )
        .err()
        .expect("an API run without a key is refused");
        assert!(err.to_string().contains("key"), "{err}");
    }
    assert_eq!(finder_model_for(&controls), DEFAULT_FINDER_MODEL);
    let picked = crate::cost_cap::RunControls {
        finder_model: Some(" claude-pick ".into()),
        ..crate::cost_cap::RunControls::default()
    };
    assert_eq!(finder_model_for(&picked), "claude-pick");
    assert_eq!(pieces_in_flight_for(crate::llm::Backend::Api), 3);
    assert_eq!(pieces_in_flight_for(crate::llm::Backend::Cli), 1);
    let api = api_agent::ApiPieceAgent::new("m", "sk-secret-value");
    assert!(!format!("{api:?}").contains("sk-secret-value"));
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
    let _: fn(String, &str) -> api_agent::ApiPieceAgent =
        |m, k| api_agent::ApiPieceAgent::new(m, k);
    let _: fn(String, std::path::PathBuf) -> cli_agent::CliPieceAgent =
        |m, d| cli_agent::CliPieceAgent::new(m, d, cli_agent::EngagementKind::Client);
    let _ = run::find_repo;
    let _: fn(&[crate::onboard::Finding], &[crate::onboard::Finding]) -> run_diff::RunDiff =
        run_diff::diff_runs;
    let _: fn(&run_diff::RunDiff) -> String = run_diff::operator_note;
}

#[test]
fn only_an_internal_cli_agent_keeps_traces_and_names_them_beside_the_rooms_folder() {
    let controls = crate::cost_cap::RunControls::default();
    let rooms = std::path::Path::new("/run/folder/finder-rooms");
    let meter = || std::sync::Arc::new(crate::ai_audit::UsageMeter::default());
    let build = |backend, key, engagement| {
        piece_agent_for(&controls, backend, key, rooms, meter(), engagement)
            .expect("the agent is built")
    };
    let note = build(crate::llm::Backend::Cli, None, EngagementKind::Internal)
        .trace_note()
        .expect("an internal cli run keeps traces");
    assert!(note.contains("/run/folder/finder-traces"), "{note}");
    assert!(
        build(crate::llm::Backend::Cli, None, EngagementKind::Client)
            .trace_note()
            .is_none()
    );
    assert!(build(
        crate::llm::Backend::Api,
        Some("sk-test"),
        EngagementKind::Internal
    )
    .trace_note()
    .is_none());
}
