//! Coverage comes from the trace only (MVP checklist item 4).

use super::coverage::{coverage_of as cov_of, FileState, PieceCoverage};
use super::room::NotServed;
use super::{AgentRun, AgentStop, ToolEvent};

/// One run holding `trace` and showing nothing in full.
fn run_of(trace: &[ToolEvent]) -> AgentRun {
    AgentRun {
        trace: trace.to_vec(),
        ..AgentRun::empty(AgentStop::Finished)
    }
}

/// A follow-up run that the transport reports as having shown `shown` in full.
fn shown_run(shown: &[&str]) -> AgentRun {
    AgentRun {
        shown_in_full: shown.iter().map(|s| (*s).to_string()).collect(),
        ..AgentRun::empty(AgentStop::Finished)
    }
}

/// Coverage of one run's trace with nothing sent again and nothing unserved.
fn coverage_of(
    key: &str,
    trace: &[ToolEvent],
    files: &[(String, usize)],
    context: &[String],
    claim: Option<&[String]>,
) -> PieceCoverage {
    cov_of(key, &[run_of(trace)], files, &[], context, claim)
}

fn read(path: &str, first: usize, asked: usize, returned: usize) -> ToolEvent {
    ToolEvent::Read {
        path: path.to_string(),
        first_line: first,
        line_count: asked,
        lines_returned: returned,
    }
}

fn files(list: &[(&str, usize)]) -> Vec<(String, usize)> {
    list.iter().map(|(p, n)| (p.to_string(), *n)).collect()
}

#[test]
fn one_read_of_every_line_is_a_full_read() {
    let cov = coverage_of(
        "k",
        &[read("a.rs", 1, 100, 100)],
        &files(&[("a.rs", 100)]),
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
    assert!(cov.still_unread.is_empty());
}

#[test]
fn two_partial_reads_that_cover_the_file_are_a_full_read() {
    let trace = [read("a.rs", 1, 60, 60), read("a.rs", 61, 60, 40)];
    let cov = coverage_of("k", &trace, &files(&[("a.rs", 100)]), &[], None);
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
}

#[test]
fn reads_with_a_gap_are_a_partial_read_with_the_ranges_and_the_file_is_still_unread() {
    let trace = [
        read("a.rs", 1, 30, 30),
        read("a.rs", 61, 40, 40),
        read("a.rs", 20, 20, 20),
    ];
    let cov = coverage_of("k", &trace, &files(&[("a.rs", 100)]), &[], None);
    assert_eq!(
        cov.files["a.rs"],
        FileState::ReadInPart(vec![(1, 39), (61, 100)])
    );
    assert_eq!(cov.still_unread, vec!["a.rs".to_string()]);
}

#[test]
fn a_grep_hit_is_not_a_read() {
    let trace = [
        ToolEvent::Grep {
            pattern: "fn main".into(),
        },
        ToolEvent::Glob {
            pattern: "**/*.rs".into(),
        },
        ToolEvent::Ls { path: "src".into() },
    ];
    let cov = coverage_of("k", &trace, &files(&[("a.rs", 10)]), &[], None);
    assert_eq!(cov.files["a.rs"], FileState::NotReviewed);
    assert_eq!(cov.still_unread, vec!["a.rs".to_string()]);
}

#[test]
fn a_truncated_read_counts_only_its_returned_lines() {
    // Asked for the whole 5,000 line file, the tool returned 2,000.
    let cov = coverage_of(
        "k",
        &[read("big.rs", 1, 5_000, 2_000)],
        &files(&[("big.rs", 5_000)]),
        &[],
        None,
    );
    assert_eq!(cov.files["big.rs"], FileState::ReadInPart(vec![(1, 2_000)]));
}

#[test]
fn a_read_that_returned_nothing_covers_nothing() {
    let cov = coverage_of(
        "k",
        &[read("a.rs", 500, 10, 0)],
        &files(&[("a.rs", 100)]),
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::NotReviewed);
}

#[test]
fn a_context_file_read_does_not_cover_anything() {
    let trace = [read("ctx.rs", 1, 50, 50)];
    let cov = coverage_of(
        "k",
        &trace,
        &files(&[("a.rs", 10)]),
        &["ctx.rs".to_string()],
        None,
    );
    assert!(!cov.files.contains_key("ctx.rs"));
    assert_eq!(cov.files["a.rs"], FileState::NotReviewed);
    assert_eq!(cov.context_read, vec!["ctx.rs".to_string()]);
}

#[test]
fn a_file_the_transport_showed_in_full_is_sent_again_and_no_longer_unread() {
    let runs = [
        run_of(&[read("a.rs", 1, 5, 5)]),
        shown_run(&["a.rs", "b.rs"]),
    ];
    let cov = cov_of(
        "k",
        &runs,
        &files(&[("a.rs", 100), ("b.rs", 10)]),
        &[],
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::SentAgain);
    assert_eq!(cov.files["b.rs"], FileState::SentAgain);
    assert!(cov.still_unread.is_empty());
}

#[test]
fn a_file_read_in_full_stays_read_in_full_even_if_it_is_also_sent_again() {
    let runs = [run_of(&[read("a.rs", 1, 10, 10)]), shown_run(&["a.rs"])];
    let cov = cov_of("k", &runs, &files(&[("a.rs", 10)]), &[], &[], None);
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
}

#[test]
fn only_the_transport_can_mark_a_file_as_shown_a_run_with_nothing_shown_covers_nothing() {
    // A follow-up that read nothing and showed nothing: the files stay owed.
    let runs = [run_of(&[read("a.rs", 1, 5, 5)]), shown_run(&[])];
    let cov = cov_of(
        "k",
        &runs,
        &files(&[("a.rs", 100), ("b.rs", 10)]),
        &[],
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInPart(vec![(1, 5)]));
    assert_eq!(cov.files["b.rs"], FileState::NotReviewed);
    assert_eq!(
        cov.still_unread,
        vec!["a.rs".to_string(), "b.rs".to_string()]
    );
}

#[tokio::test]
async fn a_send_again_that_stops_at_the_turn_limit_leaves_the_file_not_reviewed() {
    use super::{send_again, ScriptStep, ScriptedAgent};
    let repo = vec![(
        "a.rs".to_string(),
        "fn a() {}
fn b() {}
"
        .to_string(),
    )];
    let piece = super::Piece {
        key: "k".into(),
        unit: "u".into(),
        files: vec!["a.rs".into()],
    };
    let room = super::Room::build(
        &piece, &repo, "# c
",
    )
    .room;
    let limits = super::PieceLimits::for_piece(1, 1.0);
    // The first run reads nothing of a.rs. The send again stops at the turn limit: the model
    // never completed a turn with the file in it.
    let agent = ScriptedAgent::new(vec![ScriptStep::Ls(String::new())])
        .with_again_stop(AgentStop::TurnLimit("40 turns".into()));
    let first = super::PieceAgent::run(&agent, &room, "brief", &limits)
        .await
        .expect("runs");
    let again = send_again(&agent, &room, &first, &["a.rs".to_string()], &limits)
        .await
        .expect("runs");
    assert!(again.shown_in_full.is_empty(), "{:?}", again.shown_in_full);
    let cov = cov_of(
        "k",
        &[first, again],
        &room.piece_line_counts(),
        &[],
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::NotReviewed);
    assert_eq!(cov.still_unread, vec!["a.rs".to_string()]);
}

#[tokio::test]
async fn a_send_again_that_completes_shows_the_file_in_full() {
    use super::{send_again, ScriptedAgent};
    let repo = vec![(
        "a.rs".to_string(),
        "fn a() {}
fn b() {}
"
        .to_string(),
    )];
    let piece = super::Piece {
        key: "k".into(),
        unit: "u".into(),
        files: vec!["a.rs".into()],
    };
    let room = super::Room::build(
        &piece, &repo, "# c
",
    )
    .room;
    let limits = super::PieceLimits::for_piece(1, 1.0);
    let agent = ScriptedAgent::new(vec![]);
    let first = super::PieceAgent::run(&agent, &room, "brief", &limits)
        .await
        .expect("runs");
    // Asking for a path that is not a piece file shows nothing of it.
    let again = send_again(
        &agent,
        &room,
        &first,
        &["a.rs".to_string(), "nowhere.rs".to_string()],
        &limits,
    )
    .await
    .expect("runs");
    assert_eq!(again.shown_in_full, vec!["a.rs".to_string()]);
    let sent = agent.messages();
    assert!(sent[0].contains("fn b() {}"), "{}", sent[0]);
    let cov = cov_of(
        "k",
        &[first, again],
        &room.piece_line_counts(),
        &[],
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::SentAgain);
}

#[test]
fn a_piece_file_the_room_could_not_serve_stays_owed_with_its_reason() {
    let not_served = [NotServed {
        path: "gone.rs".into(),
        reason: "the file is not in the repository set the room was built from".into(),
    }];
    // Even a transport that claims it showed the file, and a read that says it returned lines,
    // cannot cover a file the room never held.
    let runs = [run_of(&[read("gone.rs", 1, 5, 5)]), shown_run(&["gone.rs"])];
    let cov = cov_of("k", &runs, &files(&[("a.rs", 3)]), &not_served, &[], None);
    assert_eq!(cov.files.get("gone.rs"), Some(&FileState::NotReviewed));
    assert!(cov.still_unread.contains(&"gone.rs".to_string()));
    assert!(
        cov.not_served
            .get("gone.rs")
            .is_some_and(|r| r.contains("not in the repository")),
        "{:?}",
        cov.not_served
    );
    assert_eq!(cov.files["a.rs"], FileState::NotReviewed);
}

#[test]
fn the_models_own_list_never_changes_a_state() {
    let trace = [read("a.rs", 1, 10, 10)];
    // The model claims it read everything: a.rs really was, b.rs was not.
    let claim = Vec::<String>::new();
    let cov = coverage_of(
        "k",
        &trace,
        &files(&[("a.rs", 10), ("b.rs", 10)]),
        &[],
        Some(&claim),
    );
    assert_eq!(cov.files["b.rs"], FileState::NotReviewed);
    assert_eq!(cov.still_unread, vec!["b.rs".to_string()]);
    assert_eq!(cov.claim_matched, Some(false));
    // The model claims a.rs was unread: the trace says it was read in full.
    let claim = vec!["a.rs".to_string(), "b.rs".to_string()];
    let cov = coverage_of(
        "k",
        &trace,
        &files(&[("a.rs", 10), ("b.rs", 10)]),
        &[],
        Some(&claim),
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
    assert_eq!(cov.claim_matched, Some(false));
}

#[test]
fn a_claim_that_names_exactly_the_files_not_read_in_full_matches() {
    let trace = [read("a.rs", 1, 10, 10)];
    let claim = vec!["b.rs".to_string()];
    let cov = coverage_of(
        "k",
        &trace,
        &files(&[("a.rs", 10), ("b.rs", 10)]),
        &[],
        Some(&claim),
    );
    assert_eq!(cov.claim_matched, Some(true));
    let none = coverage_of("k", &trace, &files(&[("a.rs", 10)]), &[], None);
    assert_eq!(none.claim_matched, None);
}

#[test]
fn an_empty_file_is_read_in_full_by_any_read_of_it() {
    let cov = coverage_of(
        "k",
        &[read("empty.rs", 1, 10, 0)],
        &files(&[("empty.rs", 0)]),
        &[],
        None,
    );
    assert_eq!(cov.files["empty.rs"], FileState::ReadInFull);
}

#[test]
fn a_path_written_with_a_leading_dot_slash_counts_for_the_same_file() {
    let cov = coverage_of(
        "k",
        &[read("./a.rs", 1, 10, 10)],
        &files(&[("a.rs", 10)]),
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
}
