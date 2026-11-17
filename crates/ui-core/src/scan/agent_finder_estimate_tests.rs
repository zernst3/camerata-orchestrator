//! The agent finder's pre-flight figures: the formula of design section 7 with its named
//! constants, the range, and the pieces table.

use super::*;

fn piece(unit: &str, files: usize, lines: usize) -> AgentFinderPieceSize {
    AgentFinderPieceSize {
        unit: unit.to_string(),
        files,
        lines,
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn two_pieces_price_two_fixed_parts_and_one_joints_call() {
    let e = estimate_agent_finder(&[piece("acme", 10, 4_000), piece("acme", 12, 6_000)]);
    assert_eq!(e.pieces, 2);
    assert_eq!(e.joints_calls, 1);
    let want = 10.0 * AGENT_FINDER_USD_PER_1000_LINES
        + 2.0 * AGENT_FINDER_USD_PER_PIECE
        + AGENT_FINDER_USD_PER_JOINTS_CALL;
    assert!(close(e.expected_usd, want), "{} vs {want}", e.expected_usd);
    // With the starting constants of design section 7 that is $13.10.
    assert!(close(e.expected_usd, 13.10), "{}", e.expected_usd);
}

#[test]
fn a_one_piece_repository_prices_no_joints_call() {
    let e = estimate_agent_finder(&[piece("acme", 10, 2_000)]);
    assert_eq!(e.pieces, 1);
    assert_eq!(e.joints_calls, 0);
    assert!(close(
        e.expected_usd,
        2.0 * AGENT_FINDER_USD_PER_1000_LINES + AGENT_FINDER_USD_PER_PIECE
    ));
}

#[test]
fn the_range_is_point_six_to_one_point_eight_times_the_expected_figure() {
    let e = estimate_agent_finder(&[piece("acme", 10, 4_000), piece("acme", 12, 6_000)]);
    assert!(close(e.low_usd, e.expected_usd * 0.6));
    assert!(close(e.high_usd, e.expected_usd * 1.8));
}

#[test]
fn the_rows_group_pieces_by_unit_and_add_files_and_lines() {
    let e = estimate_agent_finder(&[
        piece("api", 3, 100),
        piece("web", 5, 200),
        piece("api", 4, 300),
    ]);
    assert_eq!(
        e.rows,
        vec![
            AgentFinderUnitRow {
                unit: "api".into(),
                pieces: 2,
                files: 7,
                lines: 400
            },
            AgentFinderUnitRow {
                unit: "web".into(),
                pieces: 1,
                files: 5,
                lines: 200
            },
        ]
    );
}

#[test]
fn no_pieces_cost_nothing() {
    let e = estimate_agent_finder(&[]);
    assert_eq!(e.pieces, 0);
    assert!(close(e.expected_usd, 0.0));
    assert!(e.rows.is_empty());
}

#[test]
fn repositories_add_and_each_keeps_its_own_joints_call() {
    let a = estimate_agent_finder(&[piece("a", 1, 1_000), piece("a", 1, 1_000)]);
    let b = estimate_agent_finder(&[piece("b", 1, 1_000)]);
    let both = sum_agent_finder(&[a.clone(), b.clone()]);
    assert_eq!(both.pieces, 3);
    assert_eq!(both.joints_calls, 1);
    assert!(close(both.expected_usd, a.expected_usd + b.expected_usd));
    assert!(close(both.high_usd, a.high_usd + b.high_usd));
    assert_eq!(both.rows.len(), 2);
}

#[test]
fn the_table_has_a_heading_and_a_line_for_each_unit() {
    let e = estimate_agent_finder(&[piece("api", 3, 100), piece("web", 5, 200)]);
    let lines = agent_finder_table_lines(&e);
    assert_eq!(lines.len(), 3, "{lines:?}");
    for word in ["unit", "pieces", "files", "lines"] {
        assert!(lines[0].contains(word), "{}", lines[0]);
    }
    assert!(lines[1].contains("api") && lines[1].contains('3') && lines[1].contains("100"));
    assert!(lines[2].contains("web") && lines[2].contains('5') && lines[2].contains("200"));
}

#[test]
fn the_repo_line_shows_the_estimate_figures_and_the_range() {
    let e = estimate_agent_finder(&[piece("acme", 10, 4_000), piece("acme", 12, 6_000)]);
    let line = agent_finder_repo_line("acme/api", e.pieces, e.expected_usd, e.low_usd, e.high_usd);
    assert_eq!(
        line,
        "acme/api: finder stage about $13.10 (range $7.86 to $23.58), 2 piece(s)"
    );
}
