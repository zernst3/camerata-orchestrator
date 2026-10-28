//! A dollar-quote-aware, comment-aware SQL statement splitter.
//!
//! Migration replay (see `crates/checks/src/supabase/timeline.rs`) needs to fold a file's
//! DDL statements in order, but a flat split on `;` breaks the moment a migration contains
//! a `CREATE FUNCTION ... AS $$ ... ; ... $$` body — the semicolons *inside* the
//! dollar-quoted function body are not statement terminators. This module is the "genuinely
//! careful part" the design memo calls out: it tracks single-quoted strings, double-quoted
//! identifiers, `$$`/`$tag$` dollar-quoted bodies, and `--`/`/* */` comments, so the
//! resulting statements are split on real top-level semicolons only.
//!
//! # Hardening contract
//!
//! This splitter runs over untrusted, possibly hand-edited or truncated migration files —
//! a vibe-coded repo is exactly the corpus this product audits. **It must never panic.**
//! Malformed input (an unterminated dollar-quote, an unterminated string, a truncated file)
//! degrades to a best-effort split — whatever statement text was accumulated up to EOF is
//! flushed as a final (possibly incomplete) statement — never a crash, never an infinite
//! loop, never a byte-index panic. Every code path advances `i` on every iteration, and the
//! whole thing operates on a `Vec<char>` (not raw bytes) so no operation can land mid
//! multi-byte UTF-8 sequence.

/// One SQL statement extracted from a file: whitespace-normalized text (comments stripped,
/// runs of whitespace collapsed to a single space, still containing the real content of any
/// string/quoted-identifier/dollar-quoted body verbatim) plus the 1-based line on which the
/// statement's first non-comment, non-whitespace character appeared, and the 1-based line of
/// its terminating `;` (for a final statement with no `;`, the file's last line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement {
    pub text: String,
    pub line: usize,
    pub end_line: usize,
}

/// A construct [`split_statements_strict`] found still open at end of file: the file does not
/// parse as SQL, so its statement boundaries past `line` are guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unterminated {
    /// What was left open.
    pub construct: &'static str,
    /// The 1-based line it opened on.
    pub line: usize,
}

impl std::fmt::Display for Unterminated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unterminated {} opened on line {}",
            self.construct, self.line
        )
    }
}

/// Split `sql` into top-level statements. See the module doc for the hardening contract:
/// this function never panics, regardless of input.
pub fn split_statements(sql: &str) -> Vec<SqlStatement> {
    split_inner(sql).0
}

/// [`split_statements`] for a caller that must know the split is TRUSTWORTHY: `Err` when a
/// string literal, quoted identifier, dollar-quoted body or block comment is still open at end
/// of file. The lenient split runs such a construct to EOF and carries on; a caller that narrows
/// what a reader sees by statement (the candidate-site locators) must instead treat the file as
/// unparseable, because every boundary after the open construct is a guess. Never panics.
pub fn split_statements_strict(sql: &str) -> Result<Vec<SqlStatement>, Unterminated> {
    match split_inner(sql) {
        (statements, None) => Ok(statements),
        (_, Some(open)) => Err(open),
    }
}

/// The one splitter: the statements, plus the first construct left open at EOF (if any).
fn split_inner(sql: &str) -> (Vec<SqlStatement>, Option<Unterminated>) {
    // Normalize CRLF to LF up front so line counting and comment/string scanning never has
    // to special-case '\r' — a stray '\r' left in `cur` would otherwise leak into the
    // whitespace-normalized statement text.
    let normalized = sql.replace("\r\n", "\n");
    let chars: Vec<char> = normalized.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_start_line: Option<usize> = None;
    let mut unterminated: Option<Unterminated> = None;
    let mut open = |construct: &'static str, line: usize| {
        if unterminated.is_none() {
            unterminated = Some(Unterminated { construct, line });
        }
    };

    // Push a single normalizing space into `cur`, but only once a statement has actually
    // started and only if the accumulator doesn't already end in one — keeps multi-line
    // statements token-separated ("SECURITY DEFINER\nSET search_path" -> "SECURITY DEFINER
    // SET search_path", not a run-together "DEFINERSET") without accumulating runs of spaces.
    let push_sep = |cur: &mut String, started: bool| {
        if started && !cur.is_empty() && !cur.ends_with(' ') {
            cur.push(' ');
        }
    };

    while i < n {
        let c = chars[i];

        // ── line comment ────────────────────────────────────────────────────────────
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            push_sep(&mut cur, cur_start_line.is_some());
            continue; // the '\n' (or EOF) is handled by the next loop iteration
        }

        // ── block comment (non-nesting; matches Postgres's own non-nesting default) ──
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let comment_line = line;
            i += 2;
            while i < n && !(i + 1 < n && chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            // Consume the closing "*/" if present; an UNTERMINATED block comment just runs
            // to EOF, which the `i < n` guard above already handles safely.
            if i + 1 < n {
                i += 2;
            } else {
                open("block comment", comment_line);
                i = n;
            }
            push_sep(&mut cur, cur_start_line.is_some());
            continue;
        }

        // ── single-quoted string literal ('' escapes a literal quote) ────────────────
        if c == '\'' {
            if cur_start_line.is_none() {
                cur_start_line = Some(line);
            }
            let open_line = line;
            cur.push(c);
            i += 1;
            loop {
                if i >= n {
                    // unterminated string: best-effort, run to EOF, never panic
                    open("string literal", open_line);
                    break;
                }
                let ch = chars[i];
                if ch == '\n' {
                    line += 1;
                }
                cur.push(ch);
                i += 1;
                if ch == '\'' {
                    if i < n && chars[i] == '\'' {
                        // doubled quote inside the literal: escaped, keep scanning
                        cur.push(chars[i]);
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            continue;
        }

        // ── double-quoted identifier ("" escapes a literal quote) ────────────────────
        if c == '"' {
            if cur_start_line.is_none() {
                cur_start_line = Some(line);
            }
            let open_line = line;
            cur.push(c);
            i += 1;
            loop {
                if i >= n {
                    // unterminated identifier: best-effort, never panic
                    open("quoted identifier", open_line);
                    break;
                }
                let ch = chars[i];
                if ch == '\n' {
                    line += 1;
                }
                cur.push(ch);
                i += 1;
                if ch == '"' {
                    if i < n && chars[i] == '"' {
                        cur.push(chars[i]);
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            continue;
        }

        // ── dollar-quoted body ($$ ... $$ or $tag$ ... $tag$) ─────────────────────────
        if c == '$' {
            if let Some((tag, after_open)) = parse_dollar_tag(&chars, i) {
                if cur_start_line.is_none() {
                    cur_start_line = Some(line);
                }
                let open_line = line;
                for &ch in &chars[i..after_open] {
                    cur.push(ch);
                }
                i = after_open;
                loop {
                    if i >= n {
                        // unterminated dollar-quote: best-effort, run to EOF, never panic
                        open("dollar-quoted body", open_line);
                        break;
                    }
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    if chars[i] == '$' {
                        if let Some((tag2, after2)) = parse_dollar_tag(&chars, i) {
                            if tag2 == tag {
                                for &ch in &chars[i..after2] {
                                    cur.push(ch);
                                }
                                i = after2;
                                break;
                            }
                        }
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            // Not a valid dollar-quote open (e.g. a `$1` positional parameter): fall through
            // and treat '$' as an ordinary character below.
        }

        // ── statement terminator ──────────────────────────────────────────────────────
        if c == ';' {
            if let Some(start) = cur_start_line.take() {
                let text = cur.trim().to_string();
                if !text.is_empty() {
                    out.push(SqlStatement {
                        text,
                        line: start,
                        end_line: line,
                    });
                }
            }
            cur.clear();
            i += 1;
            continue;
        }

        // ── newline: count it, normalize to a token separator ────────────────────────
        if c == '\n' {
            line += 1;
            push_sep(&mut cur, cur_start_line.is_some());
            i += 1;
            continue;
        }

        // ── other whitespace: normalize to a single separator ────────────────────────
        if c.is_whitespace() {
            push_sep(&mut cur, cur_start_line.is_some());
            i += 1;
            continue;
        }

        // ── ordinary character ────────────────────────────────────────────────────────
        if cur_start_line.is_none() {
            cur_start_line = Some(line);
        }
        cur.push(c);
        i += 1;
    }

    // EOF: flush a trailing statement with no terminating ';' (a truncated file, or a
    // migration that simply omits the final semicolon).
    if let Some(start) = cur_start_line {
        let text = cur.trim().to_string();
        if !text.is_empty() {
            out.push(SqlStatement {
                text,
                line: start,
                end_line: line,
            });
        }
    }

    (out, unterminated)
}

/// If `chars[i]` begins a valid dollar-quote tag (`$$` or `$tag$`, where `tag` is
/// `[A-Za-z_][A-Za-z0-9_]*`), return `(tag, index_just_past_the_opening_marker)`. Returns
/// `None` for anything else, including a positional parameter like `$1` (digits alone are
/// not a valid tag start) — so `price = $1` is never mistaken for a dollar-quote open.
///
/// `pub(crate)` so `sql_parse.rs` can reuse the exact same dollar-quote grammar when it
/// needs to find "where does the function body start" (the signature/body boundary for the
/// `SECURITY DEFINER` / `SET search_path` scan) — the two modules must agree on what counts
/// as a dollar-quote open, or the boundary they each compute could silently disagree.
pub(crate) fn parse_dollar_tag(chars: &[char], i: usize) -> Option<(String, usize)> {
    if i >= chars.len() || chars[i] != '$' {
        return None;
    }
    let start = i + 1;
    let mut j = start;
    while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
        j += 1;
    }
    if j > start {
        // A non-empty tag must start with a letter or underscore (Postgres identifier
        // rule) — rejects `$1`, `$123`, etc. as dollar-quote opens.
        let first = chars[start];
        if !(first.is_ascii_alphabetic() || first == '_') {
            return None;
        }
    }
    if j < chars.len() && chars[j] == '$' {
        let tag: String = chars[start..j].iter().collect();
        Some((tag, j + 1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_line_is_the_terminators_line_and_eof_for_a_final_unterminated_statement() {
        let sql =
            "create table a (\n  id int\n);\ncreate function f() as $$\nselect 1;\n$$;\nselect 2";
        let stmts = split_statements(sql);
        let spans: Vec<(usize, usize)> = stmts.iter().map(|s| (s.line, s.end_line)).collect();
        assert_eq!(spans, vec![(1, 3), (4, 6), (7, 7)]);
    }

    #[test]
    fn strict_split_refuses_a_construct_left_open_at_eof_and_agrees_otherwise() {
        let ok = "create table a (id int);\ncomment on table a is 'x;y';";
        assert_eq!(split_statements_strict(ok), Ok(split_statements(ok)));
        for (sql, construct, line) in [
            (
                "select 1;\ncreate function f() as $$ begin",
                "dollar-quoted body",
                2,
            ),
            ("select 'abc", "string literal", 1),
            ("select \"abc", "quoted identifier", 1),
            ("select 1;\n\n/* note", "block comment", 3),
        ] {
            assert_eq!(
                split_statements_strict(sql),
                Err(Unterminated { construct, line }),
                "{sql}"
            );
        }
    }

    #[test]
    fn splits_simple_statements() {
        let sql = "create table a (id int);\ncreate table b (id int);";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].text, "create table a (id int)");
        assert_eq!(stmts[0].line, 1);
        assert_eq!(stmts[1].text, "create table b (id int)");
        assert_eq!(stmts[1].line, 2);
    }

    #[test]
    fn multiple_statements_on_one_line() {
        let sql = "create table a (id int); create table b (id int);";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].line, 1);
        assert_eq!(stmts[1].line, 1);
    }

    #[test]
    fn strips_line_comments() {
        let sql =
            "create table a (id int); -- trailing note\nalter table a enable row level security;";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(!stmts[1].text.to_lowercase().contains("trailing"));
    }

    #[test]
    fn strips_block_comments_including_multiline() {
        let sql = "/* header\n   comment */ create table a (id int);";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(!stmts[0].text.to_lowercase().contains("header"));
        // The comment spans 2 lines; the statement starts on line 2 where "create" appears.
        assert_eq!(stmts[0].line, 2);
    }

    #[test]
    fn dollar_quoted_function_body_semicolons_do_not_split() {
        let sql = r#"
            create function f() returns void
            security definer
            set search_path = public
            as $$
            begin
              insert into a values (1);
              return;
            end;
            $$ language plpgsql;
        "#;
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            1,
            "the whole CREATE FUNCTION is one statement: {stmts:#?}"
        );
        assert!(stmts[0].text.contains("security definer"));
        assert!(stmts[0].text.contains("set search_path"));
    }

    #[test]
    fn tagged_dollar_quote_body_containing_plain_dollar_quote() {
        // A $func$ ... $func$ body that itself contains a literal "$$" (e.g. embedded in a
        // string the function builds) must not close early on the inner $$.
        let sql = r#"
            create function f() returns text
            security definer
            set search_path = public
            as $func$
            begin
              return '$$ not a real close $$';
            end;
            $func$ language plpgsql;
            create table after_it (id int);
        "#;
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            2,
            "tagged dollar-quote must not close on an inner $$: {stmts:#?}"
        );
        assert!(stmts[1]
            .text
            .to_lowercase()
            .contains("create table after_it"));
    }

    #[test]
    fn semicolon_inside_single_quoted_string_does_not_split() {
        let sql = "insert into notes (body) values ('a; b; c');";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn escaped_quote_inside_string_literal() {
        let sql = "insert into notes (body) values ('it''s fine; really');";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn double_quoted_identifier_with_spaces_and_semicolon_like_content() {
        let sql = r#"create table "Table; With Spaces" (id int);"#;
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].text.contains("\"Table; With Spaces\""));
    }

    #[test]
    fn unicode_identifier_survives() {
        let sql = r#"create table "café_users" (id int);"#;
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].text.contains("café_users"));
    }

    #[test]
    fn crlf_line_endings_are_normalized() {
        let sql = "create table a (id int);\r\ncreate table b (id int);\r\n";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1].line, 2);
    }

    // ── hardening: adversarial / malformed input must never panic ────────────────────

    #[test]
    fn unterminated_dollar_quote_does_not_panic_and_produces_best_effort_statement() {
        let sql = "create function f() as $$ begin return 1;";
        let stmts = split_statements(sql); // must not panic
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].text.contains("begin return 1"));
    }

    #[test]
    fn unterminated_single_quote_does_not_panic() {
        let sql = "insert into a values ('unterminated";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn unterminated_double_quote_does_not_panic() {
        let sql = "create table \"unterminated";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn unterminated_block_comment_does_not_panic() {
        let sql = "create table a (id int); /* never closes";
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            1,
            "only the pre-comment statement is real: {stmts:#?}"
        );
    }

    #[test]
    fn empty_input_yields_no_statements() {
        assert!(split_statements("").is_empty());
    }

    #[test]
    fn only_whitespace_and_comments_yields_no_statements() {
        assert!(split_statements("   \n-- just a comment\n/* another */\n  ").is_empty());
    }

    #[test]
    fn truncated_utf8_boundary_safe_multibyte_content() {
        // Multi-byte content right up against a comment/quote boundary must not panic —
        // operating on `Vec<char>` (not raw bytes) guarantees this, but assert it directly.
        let sql = "create table \"日本語\" (id int); -- 注釈\ncreate table b (id int);";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn positional_parameter_is_not_mistaken_for_dollar_quote() {
        let sql = "select * from a where id = $1;";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].text.contains("$1"));
    }

    #[test]
    fn garbage_binary_ish_input_does_not_panic() {
        let sql = "\u{0}\u{1}$$$\"'''--/*;;;;$tag$$tag$";
        let _ = split_statements(sql); // only asserting: no panic
    }

    #[test]
    fn trailing_statement_without_semicolon_is_flushed() {
        let sql = "create table a (id int);\ncreate table b (id int)";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1].text, "create table b (id int)");
    }
}
