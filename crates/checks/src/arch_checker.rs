//! The deterministic architectural-rule executor seam.
//!
//! See `docs/design/2026-07-26_architectural-executor-feasibility.md` §2 for the design
//! rationale. This module generalizes the unit of analysis from "parsed module" (the
//! 06-19 ADR's rejected shape) to "repo view": a checker builds whatever private model it
//! needs (an AST, a migration timeline, an import graph) from a slice of `(path, content)`
//! pairs and answers a fixed set of rule ids deterministically. No LLM, no network, no
//! process spawn — a checker is a pure function over text that already lives in memory
//! (the scan's file walk already produced it).
//!
//! # What lives here vs. what lives in `crates/server`
//!
//! The trait, the shared violation type, glob-interest matching, and the checker registry
//! live in THIS crate (`camerata-checks`) because they have no dependency on the server's
//! `Finding` wire type. The adapter that turns an [`ArchViolation`] into a `Finding` lives
//! in `crates/server` (`onboard::architectural`) instead — `camerata-checks` does not (and
//! must not) depend on `camerata-server`, so the adapter has to live on the consuming side
//! of that boundary. This mirrors how `manifest_runner.rs` and `vcs_action.rs` already
//! split "the deterministic engine" from "how the caller renders its output."

/// The shared input currency every `ArchChecker` sees: the repo's spec (`owner/repo` or a
/// local path label) plus every `(path, content)` pair the caller already has in memory.
/// Callers construct this from what they already hold — the scan from its in-memory file
/// walk (`onboard::audit_repos`), a future Layer-2 runner from a worktree read.
pub struct RepoView<'a> {
    /// `owner/repo` (or a local-dir label) — carried through to violations for logging /
    /// multi-repo callers, even though a checker itself never branches on it.
    pub spec: &'a str,
    /// Every file the checker MAY read, as `(path, content)` pairs. A checker should only
    /// look at files whose path matches one of its own [`ArchChecker::interest_globs`] —
    /// the caller is not required to pre-filter, but pre-filtering keeps the checker cheap.
    pub files: &'a [(String, String)],
}

/// The severity vocabulary a violation carries — matches the existing `Finding.severity`
/// string vocabulary (`crates/server/src/onboard.rs`) so the adapter is a direct copy, not
/// a re-mapping. `low` is reserved for down-ranking elsewhere (test-scope); a checker never
/// emits it directly.
pub const SEVERITY_CRITICAL: &str = "critical";
pub const SEVERITY_HIGH: &str = "high";
pub const SEVERITY_MEDIUM: &str = "medium";

/// One violation found by a deterministic architectural checker. Carries enough to build a
/// `Finding` on the server side without the adapter having to re-derive anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchViolation {
    /// The rule id this violation answers (e.g. `SUPABASE-RLS-ENABLED-1`). Must be one of
    /// the emitting checker's own [`ArchChecker::rule_ids`].
    pub rule_id: String,
    /// The file (repo-relative path) the violation is attributed to — for a migration-replay
    /// finding, the file that LAST established the flagged end-state (not necessarily the
    /// file that first created the object).
    pub file: String,
    /// The 1-based line, within `file`, of the establishing statement. `0` when no single
    /// line is meaningful (e.g. a config-level finding).
    pub line: usize,
    /// The table, view, function, or other object name the finding is about, when the
    /// checker's model names one. `None` for object-less findings.
    pub object: Option<String>,
    /// Human-readable explanation — carries the buyer-facing framing AND (for the Supabase
    /// checkers) the honesty caveat from the design spec §4. This is what the adapter copies
    /// into `Finding.detail` verbatim.
    pub message: String,
    /// `critical` | `high` | `medium` — see the `SEVERITY_*` constants above.
    pub severity: &'static str,
}

/// A deterministic architectural checker: builds a per-repo model and answers a fixed set
/// of corpus rule ids over it. See the module doc for the shape rationale.
pub trait ArchChecker: Send + Sync {
    /// Corpus rule ids this checker can answer (e.g. `SUPABASE-RLS-ENABLED-1`). A checker
    /// only RUNS when the caller's selected ruleset intersects this set.
    fn rule_ids(&self) -> &'static [&'static str];

    /// Path globs the checker needs to see (e.g. `supabase/migrations/*.sql`). Lets a
    /// caller decide "does this checker even apply" without running it: zero matching files
    /// means skip, never a false "✓ clean" — see [`checker_applies`].
    fn interest_globs(&self) -> &'static [&'static str];

    /// Build the model from the supplied files and answer every rule this checker owns.
    /// Must never panic — a malformed / adversarial input file should degrade to fewer (or
    /// zero) violations, never a crash that takes the whole scan down with it.
    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation>;

    /// Whether this checker's rule ids should STAY in the LLM-advisory prompt rather than be
    /// subtracted from it by [`all_checker_rule_ids`] (see design doc D3,
    /// `docs/design/2026-07-27_ast-extractor-layer.md` §0). Fully-deterministic checkers
    /// (the default, `false`) answer their rule ids exactly, so fuzzing the same rule with an
    /// LLM would be strictly worse — those ids ARE subtracted. A checker whose verdict is a
    /// **name/lexical heuristic** rather than a real structural parse (today: the promoted
    /// `handler_no_direct_db` proof checker, unconfigured) returns `true`: its findings are
    /// `needs-review` grade, so the rule stays eligible for an independent AI read even
    /// though a native checker also runs over it. Kept as a trait method (not a second
    /// registry or a config flag) to keep the seam a single flat list, per the design memo's
    /// "no shared enriched model, checker-owned models" philosophy carried over to this
    /// smaller decision.
    fn advisory_coexisting(&self) -> bool {
        false
    }
}

/// Whether `checker` has at least one file of interest in `files` — the "zero matching
/// files ⇒ skip, never a false clean" rule from the design memo. Callers MUST gate
/// `ArchChecker::check` on this (or the equivalent per-checker filtering) so a repo with no
/// `supabase/` directory never gets a spurious "no RLS findings" read as "RLS is fine."
pub fn checker_applies(checker: &dyn ArchChecker, files: &[(String, String)]) -> bool {
    any_file_matches_globs(checker.interest_globs(), files)
}

/// Whether any `(path, _)` in `files` matches at least one of `globs`.
pub fn any_file_matches_globs(globs: &[&str], files: &[(String, String)]) -> bool {
    files.iter().any(|(path, _)| matches_any_glob(globs, path))
}

/// Whether `path` matches at least one of `globs`.
pub fn matches_any_glob(globs: &[&str], path: &str) -> bool {
    globs.iter().any(|g| glob_match(g, path))
}

/// A minimal glob matcher: `*` matches any run of characters WITHIN a path segment (never
/// crossing a `/`); a bare `**` SEGMENT matches zero or more whole path segments (see the
/// Pass 4a seam-amendment note in `docs/design/2026-07-27_ast-extractor-layer.md` §1/§4);
/// every other character (including `/`) must match literally. This is intentionally not a
/// general glob engine (no `?`, no character classes) — every glob this seam ships is a
/// fixed, simple pattern, and a minimal matcher is easier to reason about and to keep
/// panic-free than pulling in a crate.
pub fn glob_match(glob: &str, path: &str) -> bool {
    // Normalize a leading "./" some callers may carry (defensive; today's callers don't).
    let path = path.strip_prefix("./").unwrap_or(path);
    let glob_segs: Vec<&str> = glob.split('/').collect();
    let path_segs: Vec<&str> = path.split('/').collect();
    glob_match_segments(&glob_segs, &path_segs)
}

/// Recursive segment-by-segment match, the engine behind [`glob_match`]. A `**` glob
/// segment matches zero-or-more path segments (tried both ways via backtracking); any other
/// glob segment must match exactly one path segment via [`segment_match`]. Recursion depth
/// is bounded by the number of path segments in a repo-relative path (at most a few dozen in
/// practice), so this never risks a stack overflow on real input.
fn glob_match_segments(glob_segs: &[&str], path_segs: &[&str]) -> bool {
    match glob_segs.first() {
        None => path_segs.is_empty(),
        Some(&"**") => {
            // Try consuming zero path segments (the rest of the glob must match the rest of
            // the path from here), then try consuming one-and-recurse (the classic
            // "**" backtrack) until the path is exhausted.
            if glob_match_segments(&glob_segs[1..], path_segs) {
                return true;
            }
            match path_segs.split_first() {
                Some((_, rest)) => glob_match_segments(glob_segs, rest),
                None => false,
            }
        }
        Some(seg) => match path_segs.split_first() {
            Some((p, rest)) => segment_match(seg, p) && glob_match_segments(&glob_segs[1..], rest),
            None => false,
        },
    }
}

/// Match one path segment against one glob segment containing zero or more `*` wildcards.
fn segment_match(glob_seg: &str, path_seg: &str) -> bool {
    // Split the glob segment on '*'; the path segment must contain each literal piece, in
    // order, with the first/last piece anchored (unless the glob starts/ends with '*').
    let parts: Vec<&str> = glob_seg.split('*').collect();
    if parts.len() == 1 {
        return glob_seg == path_seg;
    }
    let mut cursor = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !path_seg[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
        } else if i == parts.len() - 1 {
            if !path_seg[cursor..].ends_with(part) {
                return false;
            }
            // No cursor advance needed — this is the last piece.
        } else {
            match path_seg[cursor..].find(part) {
                Some(rel) => cursor += rel + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// The registry of every native architectural checker this build ships. Callers select by
/// intersecting each checker's [`ArchChecker::rule_ids`] against their own armed/selected
/// rule set — the registry itself does no filtering.
pub fn all_checkers() -> Vec<Box<dyn ArchChecker>> {
    vec![
        Box::new(crate::supabase::rls_checker::SupabaseRlsChecker),
        Box::new(crate::supabase::search_path_checker::SupabaseFnSearchPathChecker),
        Box::new(crate::python_testing::PythonTestFileNamingChecker),
        Box::new(crate::ui_dates::UtcDatesChecker),
        Box::new(crate::handler_no_db_checker::HandlerNoDbChecker),
    ]
}

/// Every rule id a FULLY-DETERMINISTIC registered checker answers — the set the
/// LLM-exclusion filter (scan wiring, `crates/server/src/onboard.rs`) subtracts from the
/// AI-audit prompt, mirroring how `camerata_gateway::lookup_arm` already excludes
/// gate-arm-covered rules there. Checkers that opt into
/// [`ArchChecker::advisory_coexisting`] are deliberately EXCLUDED from this set — their rule
/// ids stay in the LLM prompt alongside the native checker's `needs-review` finding (D3).
pub fn all_checker_rule_ids() -> std::collections::HashSet<&'static str> {
    all_checkers()
        .iter()
        .filter(|c| !c.advisory_coexisting())
        .flat_map(|c| c.rule_ids().iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_exact_file() {
        assert!(glob_match("supabase/config.toml", "supabase/config.toml"));
        assert!(!glob_match("supabase/config.toml", "supabase/config.toml.bak"));
    }

    #[test]
    fn glob_match_star_within_segment() {
        assert!(glob_match(
            "supabase/migrations/*.sql",
            "supabase/migrations/20240101000000_init.sql"
        ));
        assert!(!glob_match(
            "supabase/migrations/*.sql",
            "supabase/migrations/nested/20240101000000_init.sql"
        ));
        assert!(!glob_match(
            "supabase/migrations/*.sql",
            "supabase/migrations/20240101000000_init.sql.bak"
        ));
    }

    #[test]
    fn glob_match_rejects_different_segment_count() {
        assert!(!glob_match("supabase/*.sql", "a/b/c.sql"));
    }

    #[test]
    fn any_file_matches_globs_true_and_false() {
        let files = vec![
            ("README.md".to_string(), String::new()),
            (
                "supabase/migrations/20240101000000_init.sql".to_string(),
                String::new(),
            ),
        ];
        let globs = ["supabase/migrations/*.sql", "supabase/config.toml"];
        assert!(any_file_matches_globs(&globs, &files));

        let files_no_supabase = vec![("README.md".to_string(), String::new())];
        assert!(!any_file_matches_globs(&globs, &files_no_supabase));
    }

    #[test]
    fn checker_applies_gates_on_interest_globs() {
        struct Dummy;
        impl ArchChecker for Dummy {
            fn rule_ids(&self) -> &'static [&'static str] {
                &["DUMMY-1"]
            }
            fn interest_globs(&self) -> &'static [&'static str] {
                &["supabase/config.toml"]
            }
            fn check(&self, _repo: &RepoView<'_>) -> Vec<ArchViolation> {
                Vec::new()
            }
        }
        let checker = Dummy;
        let empty: Vec<(String, String)> = Vec::new();
        assert!(!checker_applies(&checker, &empty));
        let with_file = vec![("supabase/config.toml".to_string(), String::new())];
        assert!(checker_applies(&checker, &with_file));
    }

    #[test]
    fn all_checkers_registry_covers_expected_rule_ids() {
        let ids = all_checker_rule_ids();
        for expected in [
            "SUPABASE-RLS-ENABLED-1",
            "SUPABASE-RLS-NO-POLICY-1",
            "SUPABASE-RLS-POLICY-DISABLED-1",
            "SUPABASE-FUNC-SEARCH-PATH-1",
            "PYTHON-TESTING-FILE-NAMING-1",
            "UI-UTC-DATES-1",
        ] {
            assert!(ids.contains(expected), "missing {expected} from registry: {ids:?}");
        }
    }

    #[test]
    fn all_checker_rule_ids_excludes_advisory_coexisting_checkers() {
        // ARCH-HANDLER-NO-DB-1 (D3): the promoted lexical proof checker must NOT be
        // subtracted from the LLM-advisory prompt, even though a native checker registers
        // and runs over it — see `handler_no_db_checker::HandlerNoDbChecker`.
        let ids = all_checker_rule_ids();
        assert!(
            !ids.contains("ARCH-HANDLER-NO-DB-1"),
            "ARCH-HANDLER-NO-DB-1 must stay LLM-advisory-eligible per D3: {ids:?}"
        );
        // But the checker IS registered and DOES answer the rule id (just excluded from
        // this particular subtraction set).
        let all_ids: std::collections::HashSet<&str> = all_checkers()
            .iter()
            .flat_map(|c| c.rule_ids().iter().copied())
            .collect();
        assert!(all_ids.contains("ARCH-HANDLER-NO-DB-1"), "checker must still be registered: {all_ids:?}");
    }

    // ── `**` glob support (Pass 4a seam amendment) ──────────────────────────────

    #[test]
    fn glob_match_double_star_matches_zero_leading_segments() {
        assert!(glob_match("**/tests/**/*.py", "tests/test_foo.py"));
    }

    #[test]
    fn glob_match_double_star_matches_multiple_leading_and_trailing_segments() {
        assert!(glob_match("**/tests/**/*.py", "a/b/tests/sub/dir/test_foo.py"));
    }

    #[test]
    fn glob_match_double_star_does_not_match_a_similarly_named_segment() {
        // "testsuite" must NOT satisfy a literal "tests" segment — ** spans whole segments,
        // it never does a substring match within one.
        assert!(!glob_match("**/tests/**/*.py", "testsuite/test_foo.py"));
    }

    #[test]
    fn glob_match_double_star_trailing_matches_everything_remaining() {
        assert!(glob_match("src/**", "src/a/b/c.rs"));
        assert!(glob_match("src/**", "src/top.rs"));
    }

    #[test]
    fn glob_match_double_star_alone_matches_empty_and_nonempty() {
        assert!(glob_match("**", "anything/at/all.rs"));
        assert!(glob_match("**", "single.rs"));
    }
}
