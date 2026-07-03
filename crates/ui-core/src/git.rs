//! Pure, framework-agnostic filter predicates for the Workspace git panel (RUST-HEADLESS-CORE-1).
//!
//! The Dioxus adapter (`camerata-ui`) owns the reactive filter signals and the `for` loops that
//! render branch chips and commit rows; the *decision* of whether a given branch or commit matches
//! the current filter query lives here so it's unit-testable with no VirtualDom. The adapter calls
//! these in its `.filter(...)` before rendering.
//!
//! Matching is case-insensitive substring containment. An empty (or whitespace-only) query matches
//! everything — the filter is a narrowing affordance, not a required search.

/// True when `name` should be shown for the given branch-filter `query`.
///
/// Case-insensitive substring match on the branch name. An empty / whitespace-only query matches
/// every branch (show-all).
pub fn branch_matches(name: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&q.to_lowercase())
}

/// True when a commit row should be shown for the given commit-search `query`.
///
/// Case-insensitive substring match against any of the short-sha, subject, or author. An empty /
/// whitespace-only query matches every commit (show-all).
pub fn commit_matches(short: &str, subject: &str, author: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let needle = q.to_lowercase();
    short.to_lowercase().contains(&needle)
        || subject.to_lowercase().contains(&needle)
        || author.to_lowercase().contains(&needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_empty_query_matches_all() {
        assert!(branch_matches("main", ""));
        assert!(branch_matches("camerata/work", "   ")); // whitespace-only is still "empty"
    }

    #[test]
    fn branch_substring_case_insensitive() {
        assert!(branch_matches("camerata/work", "WORK"));
        assert!(branch_matches("Feature/Login", "login"));
        assert!(branch_matches("main", "MAI"));
    }

    #[test]
    fn branch_non_match_returns_false() {
        assert!(!branch_matches("main", "release"));
        assert!(!branch_matches("camerata/work", "zzz"));
    }

    #[test]
    fn commit_empty_query_matches_all() {
        assert!(commit_matches("abc1234", "Fix the bug", "Zach", ""));
        assert!(commit_matches("abc1234", "Fix the bug", "Zach", "  "));
    }

    #[test]
    fn commit_matches_on_short_sha() {
        assert!(commit_matches("abc1234", "unrelated", "someone", "ABC12"));
        assert!(!commit_matches("abc1234", "unrelated", "someone", "def"));
    }

    #[test]
    fn commit_matches_on_subject() {
        assert!(commit_matches("abc1234", "Fix the login bug", "someone", "LOGIN"));
    }

    #[test]
    fn commit_matches_on_author() {
        assert!(commit_matches("abc1234", "subject", "Zachary Ernst", "zachary"));
    }

    #[test]
    fn commit_non_match_across_all_fields_returns_false() {
        assert!(!commit_matches("abc1234", "subject line", "Zach", "nonexistent"));
    }
}
