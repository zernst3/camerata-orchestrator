//! Enclosing-block resolver for the finding-code-context feature.
//!
//! See `docs/design/2026-07-27_finding-code-context.md` for the full design. This module
//! answers ONE question, purely: "given a violation at `(path, line)` in `source`, what's the
//! smallest sane block of code a reviewer should see around it?" It never touches a
//! filesystem (the caller already has `source` in memory — same contract as the rest of
//! `extract`) and it NEVER PANICS: every code path below degrades toward the `Window`
//! fallback rather than erroring, mirroring the "never panic" contract the rest of this
//! crate's extractors and the SQL splitter already carry.
//!
//! # Dispatch (design §2)
//!
//! 1. **Code** (Rust/TS/TSX/JS/Python, via [`super::lang_for_path`]): call [`super::functions`]
//!    and pick the SMALLEST span containing the violation line (smallest = innermost — this
//!    handles nested fns/closures/methods for free, with zero extra logic). No containing span
//!    (a parse failure yielding zero spans, or a top-level statement outside any function) →
//!    falls through to the window.
//! 2. **SQL** (`.sql`): [`crate::supabase::splitter::split_statements`] already segments on
//!    real top-level semicolons and records each statement's 1-based start line. The enclosing
//!    statement is `[stmt[i].line, stmt[i+1].line - 1]` (or EOF for the last statement).
//!    **Important:** `SqlStatement::text` is whitespace-normalized (comments stripped) — this
//!    module never displays it. [`slice_lines`] re-slices `source`'s RAW lines instead, so the
//!    reviewer sees the file byte-for-byte, comments and all.
//! 3. **Everything else** (unknown/no extension): a fixed ±12-line window around the
//!    violation, clamped to the file's bounds.
//!
//! A violation `line` that is out of the file's bounds (the file shrank since the finding was
//! recorded) returns `None` — the caller treats that as "changed since scan" (design §5).

use super::{functions, lang_for_path, SourceLang};

/// How many lines of padding the [`BlockKind::Window`] fallback shows on each side of the
/// violation line, when no semantic block (function/SQL statement) could be resolved.
const WINDOW_PADDING: usize = 12;

/// What kind of enclosing unit [`enclosing_block`] resolved. Lets the UI label the section
/// accurately ("enclosing function" vs "SQL statement" vs plain "surrounding lines") instead
/// of always claiming a semantic block was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// The smallest function/method span (from [`super::functions`]) containing the line.
    Function,
    /// The enclosing top-level SQL statement (from `split_statements`).
    SqlStatement,
    /// No semantic block was resolved; a fixed-size line window around the violation.
    Window,
}

impl BlockKind {
    /// Stable, lower_snake_case label — used as the endpoint's `"kind"` JSON field and in
    /// tests. Kept separate from `Debug` so a `Debug` impl change never silently changes the
    /// wire format.
    pub fn label(self) -> &'static str {
        match self {
            BlockKind::Function => "function",
            BlockKind::SqlStatement => "sql_statement",
            BlockKind::Window => "window",
        }
    }
}

/// The resolved enclosing block: 1-based, inclusive line bounds plus what kind of unit was
/// found. Does NOT carry the block's text — callers slice `source`'s raw lines themselves via
/// [`slice_lines`], so the SQL case in particular never accidentally displays the splitter's
/// whitespace-normalized `SqlStatement::text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnclosingBlock {
    pub start_line: usize,
    pub end_line: usize,
    pub kind: BlockKind,
}

/// Resolve the enclosing block around `line` (1-based) in `source`, dispatching on `path`'s
/// extension. Returns `None` ONLY when `line` is out of the file's current bounds (line 0, or
/// past the last line) — every other input, however malformed, degrades to `Some` (worst case
/// a [`BlockKind::Window`]). Never panics.
pub fn enclosing_block(path: &str, source: &str, line: usize) -> Option<EnclosingBlock> {
    // `str::lines()` doesn't count a final unterminated line as a phantom empty extra, but it
    // also reports 0 for a genuinely empty file — clamp to at least 1 so a `line == 1` request
    // against an empty file still resolves to a (trivially empty) window rather than bailing,
    // while any `line > 1` on an empty file correctly reports "gone".
    let total_lines = source.lines().count().max(1);
    if line == 0 || line > total_lines {
        return None;
    }

    if is_sql_path(path) {
        return Some(enclosing_sql_statement(source, line, total_lines));
    }

    if let Some(lang) = lang_for_path(path) {
        if let Some((start, end)) = smallest_containing_function(lang, source, line) {
            return Some(EnclosingBlock {
                start_line: start,
                end_line: end,
                kind: BlockKind::Function,
            });
        }
    }

    Some(window(line, total_lines))
}

/// Slice `source`'s RAW (un-normalized) lines `[start_line, end_line]` (1-based, inclusive),
/// clamping to the file's actual bounds. Deliberately separate from any statement/AST text —
/// this is byte-for-byte what's on disk, which matters most for the SQL case (see module docs).
/// Returns an empty vec (never panics) for any out-of-range or empty input.
pub fn slice_lines(source: &str, start_line: usize, end_line: usize) -> Vec<String> {
    if start_line == 0 {
        return Vec::new();
    }
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() || start_line > lines.len() {
        return Vec::new();
    }
    let end = end_line.min(lines.len());
    if end < start_line {
        return Vec::new();
    }
    lines[start_line - 1..end].iter().map(|s| s.to_string()).collect()
}

fn is_sql_path(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .map(|ext| ext.eq_ignore_ascii_case("sql"))
        .unwrap_or(false)
}

/// The smallest (innermost) function span containing `line`, or `None` when no span contains
/// it (top-level code, or a parse failure that yielded zero spans).
fn smallest_containing_function(lang: SourceLang, source: &str, line: usize) -> Option<(usize, usize)> {
    functions(lang, source)
        .into_iter()
        .filter(|f| f.start_line <= line && line <= f.end_line)
        .map(|f| (f.start_line, f.end_line))
        .min_by_key(|(start, end)| end.saturating_sub(*start))
}

/// Bound the top-level SQL statement enclosing `line`: `[stmt[i].line, stmt[i+1].line - 1]`,
/// or through EOF for the last statement. Trailing blank raw lines are trimmed off the end
/// (never past `line` itself, so the violation line is always still inside the returned
/// bounds). No statement starts at or before `line` (e.g. the violation sits in leading
/// comments/whitespace before the first real statement) → window fallback.
fn enclosing_sql_statement(source: &str, line: usize, total_lines: usize) -> EnclosingBlock {
    let stmts = crate::supabase::splitter::split_statements(source);
    let Some(idx) = stmts.iter().rposition(|s| s.line <= line) else {
        return window(line, total_lines);
    };
    let start = stmts[idx].line;
    let mut end = stmts
        .get(idx + 1)
        .map(|next| next.line.saturating_sub(1))
        .unwrap_or(total_lines);

    let raw: Vec<&str> = source.lines().collect();
    while end > line && raw.get(end.saturating_sub(1)).map(|l| l.trim().is_empty()).unwrap_or(false) {
        end -= 1;
    }
    EnclosingBlock {
        start_line: start,
        end_line: end.max(start),
        kind: BlockKind::SqlStatement,
    }
}

fn window(line: usize, total_lines: usize) -> EnclosingBlock {
    let start = line.saturating_sub(WINDOW_PADDING).max(1);
    let end = line.saturating_add(WINDOW_PADDING).min(total_lines);
    EnclosingBlock {
        start_line: start,
        end_line: end.max(start),
        kind: BlockKind::Window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── code languages: innermost function wins ────────────────────────────────────

    #[test]
    fn rust_nested_function_picks_innermost() {
        let src = "fn outer() -> i32 {\n    fn inner() -> i32 {\n        1\n    }\n    inner()\n}\n";
        // line 3 ("1") sits inside `inner` (lines 2-4), which is nested inside `outer` (1-6).
        let block = enclosing_block("src/lib.rs", src, 3).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (2, 4), "must pick the INNER fn, not outer");
    }

    #[test]
    fn typescript_nested_function_picks_innermost() {
        let src = "function outer() {\n  function inner() {\n    return 1;\n  }\n  return inner();\n}\n";
        let block = enclosing_block("src/lib.ts", src, 3).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (2, 4));
    }

    #[test]
    fn javascript_nested_function_picks_innermost() {
        let src = "function outer() {\n  function inner() {\n    return 1;\n  }\n  return inner();\n}\n";
        let block = enclosing_block("src/index.js", src, 3).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (2, 4));
    }

    #[test]
    fn python_nested_function_picks_innermost() {
        let src = "def outer():\n    def inner():\n        pass\n    return inner\n";
        // line 3 ("pass") sits inside `inner` (lines 2-3), nested inside `outer` (1-4).
        let block = enclosing_block("app/main.py", src, 3).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (2, 3));
    }

    #[test]
    fn line_at_exact_function_start_boundary_is_contained() {
        let src = "fn f() -> i32 {\n    1\n}\n";
        let block = enclosing_block("src/lib.rs", src, 1).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (1, 3));
    }

    #[test]
    fn line_at_exact_function_end_boundary_is_contained() {
        let src = "fn f() -> i32 {\n    1\n}\n";
        let block = enclosing_block("src/lib.rs", src, 3).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (1, 3));
    }

    #[test]
    fn top_level_rust_statement_outside_any_fn_falls_back_to_window() {
        let src = "const X: i32 = 1;\nconst Y: i32 = 2;\n";
        let block = enclosing_block("src/lib.rs", src, 1).unwrap();
        assert_eq!(block.kind, BlockKind::Window, "no fn contains a top-level const");
    }

    #[test]
    fn malformed_rust_source_falls_back_to_window_no_panic() {
        // Unbalanced braces: `syn::parse_file` errors, `functions()` yields zero spans.
        let src = "fn broken( {{{ not valid rust at all\n";
        let block = enclosing_block("src/lib.rs", src, 1).unwrap();
        assert_eq!(block.kind, BlockKind::Window);
    }

    #[test]
    fn function_larger_than_the_display_cap_is_returned_in_full() {
        // The 80-line UI cap is a display concern (§4); the resolver itself never truncates.
        let mut src = String::from("fn big() {\n");
        for i in 0..120 {
            src.push_str(&format!("    let x{i} = {i};\n"));
        }
        src.push_str("}\n");
        let total = src.lines().count();
        let block = enclosing_block("src/lib.rs", &src, 60).unwrap();
        assert_eq!(block.kind, BlockKind::Function);
        assert_eq!((block.start_line, block.end_line), (1, total));
    }

    // ── SQL: enclosing statement, RAW (not normalized) lines ───────────────────────

    #[test]
    fn sql_bounds_middle_statement_between_two_others() {
        let sql = "create table a (id int);\ncreate table b (\n  id int\n);\ncreate table c (id int);\n";
        // line 3 ("  id int") is inside the second statement, which starts at line 2 and the
        // next statement starts at line 5, so it should be bounded [2, 4].
        let block = enclosing_block("migrations/0001.sql", sql, 3).unwrap();
        assert_eq!(block.kind, BlockKind::SqlStatement);
        assert_eq!((block.start_line, block.end_line), (2, 4));
    }

    #[test]
    fn sql_last_statement_bounds_through_eof() {
        let sql = "create table a (id int);\ncreate table b (\n  id int\n);\n";
        let total = sql.lines().count();
        let block = enclosing_block("migrations/0001.sql", sql, 3).unwrap();
        assert_eq!(block.kind, BlockKind::SqlStatement);
        assert_eq!(block.start_line, 2);
        assert_eq!(block.end_line, total);
    }

    #[test]
    fn sql_trims_trailing_blank_lines_but_never_past_the_violation() {
        let sql = "create table a (id int);\n\n\ncreate table b (id int);\n\n\n\n";
        // Only one statement starts at/after nothing follows the last; trailing blank lines
        // after "create table b" should be trimmed off the end bound.
        let block = enclosing_block("migrations/0001.sql", sql, 4).unwrap();
        assert_eq!(block.kind, BlockKind::SqlStatement);
        assert_eq!(block.start_line, 4);
        assert_eq!(block.end_line, 4, "trailing blank lines must be trimmed");
    }

    #[test]
    fn sql_returns_raw_lines_not_the_splitters_normalized_text() {
        // The splitter's SqlStatement::text strips comments + collapses whitespace; the
        // enclosing block must let the caller slice RAW lines instead, preserving the comment.
        let sql = "create table a (\n  -- a real column comment\n  id int\n);\n";
        let block = enclosing_block("migrations/0001.sql", sql, 2).unwrap();
        assert_eq!(block.kind, BlockKind::SqlStatement);
        let raw = slice_lines(sql, block.start_line, block.end_line);
        assert!(
            raw.iter().any(|l| l.contains("a real column comment")),
            "raw slice must keep the comment the splitter's normalized text strips: {raw:#?}"
        );
    }

    #[test]
    fn sql_with_no_enclosing_statement_falls_back_to_window() {
        // A violation reported inside a leading-comment-only region (before any real
        // statement) has no enclosing statement — window fallback, not a panic.
        let sql = "-- just a header comment, no statements at all\n-- more comment\n";
        let block = enclosing_block("migrations/0001.sql", sql, 1).unwrap();
        assert_eq!(block.kind, BlockKind::Window);
    }

    // ── unknown extension: fixed window ─────────────────────────────────────────────

    #[test]
    fn unknown_extension_falls_back_to_window() {
        let src: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        let block = enclosing_block("infra/main.tf", &src, 15).unwrap();
        assert_eq!(block.kind, BlockKind::Window);
        assert_eq!((block.start_line, block.end_line), (3, 27));
    }

    #[test]
    fn window_clamps_to_file_bounds_near_start_and_end() {
        let src: String = (1..=5).map(|n| format!("line {n}\n")).collect();
        let block = enclosing_block("config/.env", &src, 1).unwrap();
        assert_eq!((block.start_line, block.end_line), (1, 5));
        let block = enclosing_block("config/.env", &src, 5).unwrap();
        assert_eq!((block.start_line, block.end_line), (1, 5));
    }

    // ── edge cases / adversarial: no panic, always sane ────────────────────────────

    #[test]
    fn violation_line_past_eof_returns_none() {
        let src = "line 1\nline 2\n";
        assert!(enclosing_block("src/lib.rs", src, 99).is_none());
    }

    #[test]
    fn violation_line_zero_returns_none() {
        let src = "line 1\nline 2\n";
        assert!(enclosing_block("src/lib.rs", src, 0).is_none());
    }

    #[test]
    fn empty_file_with_line_one_returns_sane_window_not_none() {
        let block = enclosing_block("src/lib.rs", "", 1).unwrap();
        assert_eq!(block.kind, BlockKind::Window);
        assert_eq!((block.start_line, block.end_line), (1, 1));
    }

    #[test]
    fn empty_file_with_line_two_is_gone() {
        assert!(enclosing_block("src/lib.rs", "", 2).is_none());
    }

    #[test]
    fn minified_one_liner_does_not_panic() {
        let huge_line = "x".repeat(50_000);
        let src = format!("function f() {{ {huge_line} }}\n");
        let block = enclosing_block("dist/bundle.min.js", &src, 1).unwrap();
        // No panic is the assertion; whatever kind resolves is fine.
        assert!(block.end_line >= block.start_line);
    }

    #[test]
    fn garbage_control_characters_do_not_panic_in_any_dispatch() {
        let src = "\u{0}\u{1}$$$\"'''--/*;;;;$tag$$tag$\nfn ??? {{{\n";
        for path in ["a.rs", "a.ts", "a.js", "a.py", "a.sql", "a.toml"] {
            let _ = enclosing_block(path, src, 1); // only asserting: no panic
            let _ = enclosing_block(path, src, 2);
        }
    }

    #[test]
    fn no_extension_path_falls_back_to_window() {
        let src = "line 1\nline 2\nline 3\n";
        let block = enclosing_block("Makefile", src, 2).unwrap();
        assert_eq!(block.kind, BlockKind::Window);
    }

    // ── slice_lines ──────────────────────────────────────────────────────────────

    #[test]
    fn slice_lines_returns_the_requested_raw_range() {
        let src = "a\nb\nc\nd\n";
        assert_eq!(slice_lines(src, 2, 3), vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn slice_lines_clamps_end_past_eof() {
        let src = "a\nb\n";
        assert_eq!(slice_lines(src, 1, 99), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn slice_lines_out_of_range_returns_empty_not_panic() {
        let src = "a\nb\n";
        assert!(slice_lines(src, 0, 5).is_empty());
        assert!(slice_lines(src, 10, 20).is_empty());
        assert!(slice_lines("", 1, 5).is_empty());
    }
}
