//! Coverage comes from the trace only (MVP checklist item 4).

use super::coverage::{coverage_of, FileState};
use super::ToolEvent;

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
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
    assert!(cov.still_unread.is_empty());
}

#[test]
fn two_partial_reads_that_cover_the_file_are_a_full_read() {
    let trace = [read("a.rs", 1, 60, 60), read("a.rs", 61, 60, 40)];
    let cov = coverage_of("k", &trace, &files(&[("a.rs", 100)]), &[], &[], None);
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
}

#[test]
fn reads_with_a_gap_are_a_partial_read_with_the_ranges_and_the_file_is_still_unread() {
    let trace = [
        read("a.rs", 1, 30, 30),
        read("a.rs", 61, 40, 40),
        read("a.rs", 20, 20, 20),
    ];
    let cov = coverage_of("k", &trace, &files(&[("a.rs", 100)]), &[], &[], None);
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
    let cov = coverage_of("k", &trace, &files(&[("a.rs", 10)]), &[], &[], None);
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
        &[],
        None,
    );
    assert!(!cov.files.contains_key("ctx.rs"));
    assert_eq!(cov.files["a.rs"], FileState::NotReviewed);
    assert_eq!(cov.context_read, vec!["ctx.rs".to_string()]);
}

#[test]
fn a_file_sent_again_is_sent_again_and_no_longer_unread() {
    let trace = [read("a.rs", 1, 5, 5)];
    let cov = coverage_of(
        "k",
        &trace,
        &files(&[("a.rs", 100), ("b.rs", 10)]),
        &[],
        &["a.rs".to_string(), "b.rs".to_string()],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::SentAgain);
    assert_eq!(cov.files["b.rs"], FileState::SentAgain);
    assert!(cov.still_unread.is_empty());
}

#[test]
fn a_file_read_in_full_stays_read_in_full_even_if_it_is_also_sent_again() {
    let cov = coverage_of(
        "k",
        &[read("a.rs", 1, 10, 10)],
        &files(&[("a.rs", 10)]),
        &[],
        &["a.rs".to_string()],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
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
        &[],
        Some(&claim),
    );
    assert_eq!(cov.claim_matched, Some(true));
    let none = coverage_of("k", &trace, &files(&[("a.rs", 10)]), &[], &[], None);
    assert_eq!(none.claim_matched, None);
}

#[test]
fn an_empty_file_is_read_in_full_by_any_read_of_it() {
    let cov = coverage_of(
        "k",
        &[read("empty.rs", 1, 10, 0)],
        &files(&[("empty.rs", 0)]),
        &[],
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
        &[],
        None,
    );
    assert_eq!(cov.files["a.rs"], FileState::ReadInFull);
}
