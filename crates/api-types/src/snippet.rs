//! What "the code around a finding" means on both ends of the wire (work order B.3).
//!
//! The batch context endpoint (`POST /api/onboard/finding-context/batch`) cuts a window of
//! [`CONTEXT_RADIUS`] lines either side of a finding's line and reports whether the line still
//! matches the finding's stored snippet; the detail pane checks the same two things on a window it
//! cuts itself from a file the viewer holds. The radius and the comparison are defined here, once,
//! so the server and the pane cannot disagree about either.

/// Lines of context either side of the finding's own line.
pub const CONTEXT_RADIUS: usize = 3;

/// Whether a line's current text still contains, or is contained by, the first line of the
/// stored snippet, both trimmed. An empty `expect` has nothing to compare against and matches. A
/// blank `actual` matches no snippet: "contained by" must not make an empty line a match for
/// everything, which would let a caller probe a line the display blanks out.
///
/// `actual` must be the line as it is DISPLAYED, that is redacted in the context of its whole
/// file: the stored snippet was redacted when it was built, so a comparison against an unredacted
/// line would let a caller learn the contents of a line the display hides.
pub fn line_matches_snippet(actual: &str, expect: &str) -> bool {
    let (actual, expect) = (actual.trim(), expect.trim());
    expect.is_empty()
        || (!actual.is_empty() && (actual.contains(expect) || expect.contains(actual)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_either_way_on_trimmed_text() {
        assert!(line_matches_snippet("  let x = 1;  ", "let x = 1;"));
        assert!(
            line_matches_snippet("let x", "let x = 1;"),
            "a truncated line"
        );
        assert!(line_matches_snippet("anything", "  "), "nothing to compare");
        assert!(line_matches_snippet("", ""));
        assert!(
            !line_matches_snippet("", "let x = 1;"),
            "a blank line matches nothing"
        );
        assert!(!line_matches_snippet("   ", "let x = 1;"));
        assert!(!line_matches_snippet("let y = 2;", "let x = 1;"));
    }

    #[test]
    fn the_radius_is_three() {
        assert_eq!(CONTEXT_RADIUS, 3);
    }
}
