//! `UtcDatesChecker`: LEXICAL (comment-aware, not a bare regex) detection of the universal
//! `UI-UTC-DATES-1` rule — Pass 4a "Group A" (no AST, no new dependencies). See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group A and
//! `crates/rules/principles/ui/ui-utc-dates-1.toml`.
//!
//! # What this checks
//!
//! The rule's own `qualifies` names the exact forbidden call shapes: `toLocaleString(`,
//! `toLocaleDateString(`, `Intl.DateTimeFormat(` without a resolved `timeZone` (the design
//! table adds `toLocaleTimeString(` to the same family). A call to any of these in feature
//! code should instead go through the project's single centralized date-rendering helper.
//!
//! # Why every finding here is `needs-review`, not a hard verdict
//!
//! The production design (`.camerata/architecture.toml`'s `[helpers].date_helper`, §3 of the
//! design doc) exempts the helper FILE ITSELF from this scan — otherwise the helper's own
//! legitimate call to `Intl.DateTimeFormat` would self-flag. That config loader is
//! explicitly OUT of scope for this pass (gated on Zach's D1 confirm). Without it, this
//! checker cannot tell "a random feature file calling the platform API directly" from "the
//! project's own date helper doing exactly its job" — so every finding is demoted to
//! `needs-review` via the existing `[needs review: ...]` message-suffix convention
//! (`ui_core::rules::split_needs_review`), the SAME mechanism the UI already renders for
//! calibration-flagged findings. This requires no changes to `Finding`, the adapter, or the
//! report/template/serializer.

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};

pub const RULE_UI_UTC_DATES: &str = "UI-UTC-DATES-1";

const RULE_IDS: &[&str] = &[RULE_UI_UTC_DATES];

const INTEREST_GLOBS: &[&str] = &["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"];

/// The forbidden call-shape substrings, per the rule TOML + the design table. Order doesn't
/// matter; a line is flagged on the FIRST one it contains (one violation per line is enough
/// to surface it for review).
const FORBIDDEN_CALLS: &[&str] =
    &["toLocaleString(", "toLocaleDateString(", "toLocaleTimeString(", "Intl.DateTimeFormat("];

/// Path fragments that mark generated/vendor/build output this checker should never scan —
/// findings there are noise, not actionable feature code. Not expressed as a NEGATIVE glob
/// (the seam's `interest_globs` has no exclusion syntax), so filtered here instead.
const SKIP_PATH_FRAGMENTS: &[&str] =
    &["node_modules/", "/dist/", "/build/", "/.next/", ".min.js"];

pub struct UtcDatesChecker;

impl ArchChecker for UtcDatesChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        repo.files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .filter(|(path, _)| !SKIP_PATH_FRAGMENTS.iter().any(|frag| path.contains(frag)))
            .flat_map(|(path, content)| violations_in_file(path, content))
            .collect()
    }
}

/// Scan one file's lines for a forbidden call OUTSIDE a `//` line comment or a `/* ... */`
/// block comment. Deliberately does not attempt string-literal awareness (a call name
/// appearing inside a string is a vanishingly rare false-positive shape compared to the
/// comment case, and string-literal tracking without a real lexer risks worse mistakes on
/// escaped quotes) — this mirrors the discipline `architectural::strip_line_comment` already
/// established for the handler-no-db proof checker, extended with block-comment tracking.
fn violations_in_file(path: &str, content: &str) -> Vec<ArchViolation> {
    let mut violations = Vec::new();
    let mut in_block_comment = false;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let (code, next_in_block) = strip_comments(raw_line, in_block_comment);
        in_block_comment = next_in_block;

        for call in FORBIDDEN_CALLS {
            if let Some(rel) = code.find(call) {
                let col_context = &code[rel..];
                violations.push(ArchViolation {
                    rule_id: RULE_UI_UTC_DATES.to_string(),
                    file: path.to_string(),
                    line: line_no,
                    object: Some((*call).trim_end_matches('(').to_string()),
                    severity: SEVERITY_MEDIUM,
                    message: format!(
                        "Direct call to `{}` in feature code — dates should render through this project's \
                         single centralized date-rendering helper (store UTC, render in the viewer's local \
                         timezone), not via inline platform locale formatting, so a future timezone/locale \
                         policy change is one edit instead of many (UI-UTC-DATES-1). [needs review: no \
                         .camerata/architecture.toml `[helpers].date_helper` is configured yet, so this \
                         checker cannot tell this call site apart from the project's own date helper doing \
                         its job — confirm this file is feature code, not the helper itself, before treating \
                         it as a violation]",
                        call.trim_end_matches('(')
                    ),
                });
                let _ = col_context; // reserved: column-level detail not surfaced today
                break; // one violation per line is enough to flag it
            }
        }
    }

    violations
}

/// Strip a trailing `//` line comment and any `/* ... */` block-comment span from `line`,
/// tracking whether the line ENDS still inside an unterminated block comment (carried by the
/// caller as `in_block_comment` for the next line). Never panics on malformed input (an
/// unterminated `/*` with no matching `*/` anywhere in the file just suppresses the rest of
/// the file from scanning — a false negative, never a false positive or a crash).
fn strip_comments(line: &str, mut in_block_comment: bool) -> (String, bool) {
    // Operate over `char`s (not bytes) so a multi-byte UTF-8 character in the line (a
    // non-ASCII string literal, an emoji in a comment, ...) can never land us on a
    // non-char-boundary byte index — the panic risk a raw-byte/`str` slice mix would carry.
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if in_block_comment {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            break; // rest of the line is a line comment
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, in_block_comment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView { spec: "test/repo", files }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    #[test]
    fn flags_to_locale_string_and_marks_needs_review() {
        let f = files(vec![(
            "src/components/OrderRow.tsx",
            "const label = order.createdAt.toLocaleString();\n",
        )]);
        let vs = UtcDatesChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_UI_UTC_DATES);
        assert_eq!(vs[0].line, 1);
        assert!(vs[0].message.contains("[needs review"), "{}", vs[0].message);
    }

    #[test]
    fn flags_intl_datetimeformat() {
        let f = files(vec![("src/lib/foo.ts", "new Intl.DateTimeFormat('en-US').format(d);\n")]);
        let vs = UtcDatesChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert!(vs[0].object.as_deref() == Some("Intl.DateTimeFormat"));
    }

    #[test]
    fn flags_to_locale_date_and_time_string_variants() {
        let f = files(vec![
            ("a.ts", "x.toLocaleDateString();\n"),
            ("b.ts", "x.toLocaleTimeString();\n"),
        ]);
        assert_eq!(UtcDatesChecker.check(&view(&f)).len(), 2);
    }

    #[test]
    fn clean_file_with_no_forbidden_calls_is_untouched() {
        let f = files(vec![("src/lib/dates.ts", "export function formatDate(d: Date) { return d.toISOString(); }\n")]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn ignores_a_line_comment_occurrence() {
        let f = files(vec![("a.ts", "// x.toLocaleString() is banned here\nconst y = 1;\n")]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn ignores_a_block_comment_occurrence_single_line() {
        let f = files(vec![("a.ts", "/* x.toLocaleString() banned */\nconst y = 1;\n")]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn ignores_a_multi_line_block_comment_occurrence() {
        let f = files(vec![(
            "a.ts",
            "/*\n * do not call x.toLocaleString() here\n */\nconst y = 1;\n",
        )]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn flags_a_real_call_on_the_line_immediately_after_a_closed_block_comment() {
        let f = files(vec![("a.ts", "/* header */ x.toLocaleString();\n")]);
        let vs = UtcDatesChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn non_ui_extension_is_not_scoped_in() {
        let f = files(vec![("app.py", "x.toLocaleString()\n")]);
        assert!(!crate::arch_checker::checker_applies(&UtcDatesChecker, &f));
    }

    #[test]
    fn vendor_and_build_output_paths_are_skipped() {
        let f = files(vec![
            ("node_modules/pkg/index.js", "x.toLocaleString();\n"),
            ("apps/ui/dist/bundle.js", "x.toLocaleString();\n"),
            ("apps/ui/.next/static/chunk.js", "x.toLocaleString();\n"),
        ]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn empty_file_does_not_panic() {
        let f = files(vec![("a.ts", "")]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn unterminated_block_comment_does_not_panic_and_suppresses_rest_of_file() {
        // Malformed input: `/*` with no matching `*/` anywhere. Must not panic; the false
        // negative (missing the real call after it) is the accepted failure mode.
        let f = files(vec![("a.ts", "/* unterminated\nx.toLocaleString();\n")]);
        let vs = UtcDatesChecker.check(&view(&f));
        assert!(vs.is_empty(), "unterminated block comment suppresses the rest of the file: {vs:#?}");
    }

    #[test]
    fn non_utf8_safe_binary_like_content_does_not_panic() {
        // A file whose "content" is unusual bytes-as-lossy-string; must not panic (RepoView
        // carries String, so real invalid UTF-8 never reaches here, but stress it anyway).
        let f = files(vec![("a.ts", "\u{FFFD}\u{FFFD} weird content, no forbidden calls\n")]);
        assert!(UtcDatesChecker.check(&view(&f)).is_empty());
    }
}
