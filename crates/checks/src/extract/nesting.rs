//! The one cheap structural guard in front of every recursive parser or tree walker that reads
//! a client file.
//!
//! `syn::parse_file`, the `syn` visitors, the tree-sitter walkers in `ecma` / `python` and the
//! enclosing-block resolver all recurse once per level of bracket nesting. A stack overflow
//! aborts the whole process rather than unwinding, so one generated or minified file with a few
//! thousand nested brackets would kill an inspection (and the server hosting it). Real code
//! nests brackets a few dozen deep at most.
//!
//! [`max_bracket_depth`] measures that depth in one linear, non-recursive pass that skips
//! comments and string literals (so a `"((("` or a regex-free `// {{{` does not count), and
//! [`within_limit`] / [`parseable`] compare it with a per-language limit chosen with a wide
//! margin under the depth that overflowed a 2 MiB stack (tokio's worker and blocking-pool
//! default) in an unoptimised build, measured per parser:
//!
//! | Parser | Overflowed at (debug, 2 MiB) | Limit |
//! |---|---|---|
//! | `syn` (Rust) | about 100 braces, 140 brackets, 170 parens | [`MAX_RUST_NESTING`] = 64 |
//! | tree-sitter walkers (TS, TSX, JS, Python) | about 3,000 | [`MAX_TREE_NESTING`] = 400 |
//!
//! A file over the limit is treated like one that failed to parse: the extractors return
//! nothing for it (a worse result, never a crash). The scan therefore skips such a file's AST
//! checks; its text-based checks still run.

use super::SourceLang;

/// Deepest bracket nesting `syn` is given.
pub const MAX_RUST_NESTING: usize = 64;

/// Deepest bracket nesting the tree-sitter walkers (`ecma`, `python`, the clone tokenizer, the
/// enclosing-block resolver) are given.
pub const MAX_TREE_NESTING: usize = 400;

/// The deepest bracket nesting the highlighter accepts. Highlighting is iterative (no stack
/// risk), but its query matching goes super-linear on deep nesting, so this bounds time, not
/// stack.
pub const MAX_HIGHLIGHT_NESTING: usize = 1_000;

/// The longest single line the highlighter accepts: a minified bundle is one line of this
/// kind, and tree-sitter's cost on it is dominated by pathological backtracking.
pub const MAX_HIGHLIGHT_LINE_BYTES: usize = 20_000;

/// Which lexical conventions to skip while counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lex {
    Rust,
    CLike,
    Python,
}

fn lex_for(lang: SourceLang) -> Lex {
    match lang {
        SourceLang::Rust => Lex::Rust,
        SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => Lex::CLike,
        SourceLang::Python => Lex::Python,
    }
}

/// The per-language limit for the parser that handles `lang`.
pub fn limit_for(lang: SourceLang) -> usize {
    match lang {
        SourceLang::Rust => MAX_RUST_NESTING,
        _ => MAX_TREE_NESTING,
    }
}

/// Whether `source` is shallow enough for the parser of `lang`.
pub fn parseable(lang: SourceLang, source: &str) -> bool {
    within_limit(lang, source, limit_for(lang))
}

/// Whether the bracket nesting of `source` (read as `lang`) stays at or under `limit`. Stops
/// reading at the first excess.
pub fn within_limit(lang: SourceLang, source: &str, limit: usize) -> bool {
    scan(lex_for(lang), source.as_bytes(), Some(limit)) <= limit
}

/// The deepest bracket nesting of `source` read as `lang`, comments and string literals
/// excluded. Linear time, constant stack.
pub fn max_bracket_depth(lang: SourceLang, source: &str) -> usize {
    scan(lex_for(lang), source.as_bytes(), None)
}

/// Longest line of `source` in bytes.
pub fn longest_line(source: &str) -> usize {
    source.split('\n').map(str::len).max().unwrap_or(0)
}

fn scan(lex: Lex, b: &[u8], stop_over: Option<usize>) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'(' | b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
                if stop_over.is_some_and(|limit| max > limit) {
                    return max;
                }
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'/' if lex != Lex::Python && b.get(i + 1) == Some(&b'/') => {
                i = skip_to_newline(b, i);
            }
            b'#' if lex == Lex::Python => {
                i = skip_to_newline(b, i);
            }
            b'/' if lex != Lex::Python && b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'"' => i = skip_string(lex, b, i, b'"'),
            b'\'' => {
                i = if lex == Lex::Rust {
                    skip_rust_char_or_lifetime(b, i)
                } else {
                    skip_string(lex, b, i, b'\'')
                };
            }
            b'`' if lex == Lex::CLike => i = skip_template(b, i),
            _ => i += 1,
        }
    }
    max
}

fn skip_to_newline(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i] != b'\n' {
        i += 1;
    }
    i
}

/// Skip a quoted string starting at `b[i] == quote`. A Python triple quote runs to its
/// matching triple; every other string stops at its closing quote, honouring `\` escapes (a
/// Rust or Python string may span lines, a JS one may not, but ending at a newline for the
/// latter only limits the damage of a stray quote).
fn skip_string(lex: Lex, b: &[u8], start: usize, quote: u8) -> usize {
    if lex == Lex::Python && b.get(start + 1) == Some(&quote) && b.get(start + 2) == Some(&quote) {
        let mut i = start + 3;
        while i < b.len() {
            if b[i] == b'\\' {
                i += 2;
            } else if b[i] == quote && b.get(i + 1) == Some(&quote) && b.get(i + 2) == Some(&quote)
            {
                return i + 3;
            } else {
                i += 1;
            }
        }
        return b.len();
    }
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == quote => return i + 1,
            b'\n' if lex == Lex::CLike => return i,
            _ => i += 1,
        }
    }
    b.len()
}

/// A JS template literal: to the next unescaped backtick (substitutions are not nested into).
fn skip_template(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// In Rust a `'` opens either a char literal (`'('`, `'\n'`, `'\u{1F600}'`) or a lifetime
/// (`'a`). Skip a char literal whole; for a lifetime skip only the quote.
fn skip_rust_char_or_lifetime(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    if b.get(i) == Some(&b'\\') {
        // Escape: up to the closing quote within a short span (`'\u{10FFFF}'` is 12 bytes).
        let limit = (i + 12).min(b.len());
        i += 2;
        while i < limit {
            if b[i] == b'\'' {
                return i + 1;
            }
            i += 1;
        }
        return start + 1;
    }
    // One (possibly multi-byte) char then a closing quote.
    let mut j = i + 1;
    while j < b.len() && j < i + 4 && (b[j] & 0xC0) == 0x80 {
        j += 1;
    }
    if i < b.len() && b[i] != b'\'' && b.get(j) == Some(&b'\'') {
        return j + 1;
    }
    start + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deep(open: &str, close: &str, n: usize) -> String {
        format!("{}{}", open.repeat(n), close.repeat(n))
    }

    #[test]
    fn counts_the_deepest_nesting() {
        assert_eq!(max_bracket_depth(SourceLang::Rust, "fn f() { (a[1]) }"), 3);
        assert_eq!(max_bracket_depth(SourceLang::Rust, "{}{}{}"), 1);
        assert_eq!(max_bracket_depth(SourceLang::JavaScript, ""), 0);
        assert_eq!(
            max_bracket_depth(SourceLang::JavaScript, &deep("[", "]", 5_000)),
            5_000
        );
        // Stray closers never underflow.
        assert_eq!(max_bracket_depth(SourceLang::Rust, ")))))(("), 2);
    }

    #[test]
    fn comments_and_strings_do_not_count() {
        let rust = "// (((((\n/* {{{{{ */ let s = \"(((((\"; let c = '('; let r = &'a str; f(1)";
        assert_eq!(max_bracket_depth(SourceLang::Rust, rust), 1);
        let js = "// ((((\n/* [[[[ */ var s = '((((' + \"[[[[\" + `{{{{`; f(g(1))";
        assert_eq!(max_bracket_depth(SourceLang::JavaScript, js), 2);
        let py = "# ((((\ns = '''(((\n(((''' + \"[[[[\"\nf(g(1))\n";
        assert_eq!(max_bracket_depth(SourceLang::Python, py), 2);
        // An escaped quote does not end the string.
        assert_eq!(
            max_bracket_depth(SourceLang::JavaScript, r#"var s = "\"(((("; f()"#),
            1
        );
    }

    #[test]
    fn limit_checks_stop_early_and_use_the_per_language_limit() {
        assert!(parseable(
            SourceLang::Rust,
            &deep("(", ")", MAX_RUST_NESTING)
        ));
        assert!(!parseable(
            SourceLang::Rust,
            &deep("(", ")", MAX_RUST_NESTING + 1)
        ));
        assert!(parseable(
            SourceLang::JavaScript,
            &deep("[", "]", MAX_TREE_NESTING)
        ));
        assert!(!parseable(
            SourceLang::Python,
            &deep("[", "]", MAX_TREE_NESTING + 1)
        ));
        assert!(!within_limit(SourceLang::Rust, &deep("{", "}", 10), 9));
    }

    #[test]
    fn longest_line_is_measured_in_bytes() {
        assert_eq!(longest_line(""), 0);
        assert_eq!(longest_line("ab\ncdef\ng"), 4);
        assert_eq!(longest_line("é"), 2);
    }
}

/// Every entry point that hands a client file to a recursive parser or walker, driven with
/// adversarially deep input on a deliberately small stack. A stack overflow aborts the whole
/// test process, so a regression here fails loudly rather than quietly.
#[cfg(test)]
mod abort_tests {
    use super::*;
    use crate::extract::clone::{tokens, TokenMode};
    use crate::extract::enclosing::{enclosing_block, BlockKind};
    use crate::extract::{
        computed_loads, exports, functions, imports, literals, method_calls, SourceLang,
    };

    /// Tokio's default worker and blocking-pool stack.
    const SMALL_STACK: usize = 2 * 1024 * 1024;

    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(SMALL_STACK)
            .spawn(f)
            .expect("spawn")
            .join()
            .expect("the work must not panic")
    }

    fn shapes() -> Vec<(&'static str, SourceLang, String)> {
        let wrap = |pre: &str, open: &str, close: &str, n: usize, post: &str| {
            format!("{pre}{}{}{post}", open.repeat(n), close.repeat(n))
        };
        vec![
            (
                "a.rs",
                SourceLang::Rust,
                wrap("fn f() { ", "(", ")", 1_000, "; }\n"),
            ),
            (
                "a.rs",
                SourceLang::Rust,
                wrap("fn f() ", "{", "}", 1_000, "\n"),
            ),
            (
                "a.rs",
                SourceLang::Rust,
                wrap("fn f() { ", "[", "]", 20_000, "; }\n"),
            ),
            (
                "a.js",
                SourceLang::JavaScript,
                wrap("var x = ", "[", "]", 20_000, ";\n"),
            ),
            (
                "a.js",
                SourceLang::JavaScript,
                wrap("var x = ", "(", ")", 20_000, ";\n"),
            ),
            (
                "a.ts",
                SourceLang::TypeScript,
                wrap("const x = ", "{", "}", 20_000, ";\n"),
            ),
            (
                "a.tsx",
                SourceLang::Tsx,
                wrap("const x = ", "[", "]", 20_000, ";\n"),
            ),
            (
                "a.py",
                SourceLang::Python,
                wrap("x = ", "[", "]", 20_000, "\n"),
            ),
            (
                "a.py",
                SourceLang::Python,
                wrap("x = ", "(", ")", 20_000, "\n"),
            ),
        ]
    }

    #[test]
    fn deeply_nested_input_never_aborts_any_extractor() {
        for (path, lang, src) in shapes() {
            let (path, src2) = (path.to_string(), src.clone());
            let kind = on_small_stack(move || {
                assert!(functions(lang, &src2).is_empty());
                assert!(imports(lang, &src2).is_empty());
                assert!(method_calls(lang, &src2).is_empty());
                assert!(literals(lang, &src2).is_empty());
                assert!(computed_loads(lang, &src2).is_empty());
                let _ = exports(lang, &src2);
                for mode in [TokenMode::Exact, TokenMode::Structural] {
                    assert!(tokens(lang, &src2, mode).is_empty());
                }
                enclosing_block(&path, &src2, 1).map(|b| b.kind)
            });
            assert_eq!(
                kind,
                Some(BlockKind::Window),
                "{lang:?}: too deep to parse falls back to the line window"
            );
        }
    }

    /// The clone tokenizer has its own group-depth guard (200, token based, because a macro body
    /// is opaque to it); this pins that the depth it still admits fits a small stack.
    #[test]
    fn the_clone_tokenizer_admits_only_depths_that_fit_a_small_stack() {
        for (open, close) in [("(", ")"), ("{", "}"), ("[", "]")] {
            let src = format!(
                "fn f() {{ let x = {}1{}; }}\n",
                open.repeat(199),
                close.repeat(199)
            );
            on_small_stack(move || {
                let _ = tokens(SourceLang::Rust, &src, TokenMode::Structural);
            });
        }
    }

    #[test]
    fn deeply_nested_sql_does_not_abort_the_statement_splitter() {
        let src = format!("select {}1{};\n", "(".repeat(50_000), ")".repeat(50_000));
        let kind = on_small_stack(move || enclosing_block("a.sql", &src, 1).map(|b| b.kind));
        assert_eq!(kind, Some(BlockKind::SqlStatement));
    }

    #[test]
    fn ordinary_nesting_is_still_extracted() {
        let src = "fn outer() { let v = vec![(1, [2, 3])]; if v.len() > 0 { inner(v); } }\nfn inner(_v: Vec<(i32, [i32; 2])>) {}\n";
        assert_eq!(functions(SourceLang::Rust, src).len(), 2);
        let js = "function f() { return [[1, [2, [3]]], { a: (1 + (2)) }]; }\n";
        assert_eq!(functions(SourceLang::JavaScript, js).len(), 1);
    }
}
