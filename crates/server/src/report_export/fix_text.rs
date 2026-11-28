//! Client-facing fix text preparation: the ONE place a rule's authored `remediation` (and a
//! model-written `fix_specific`) is made presentable before any artifact renders it.
//!
//! Authored remediation lives in TOML multi-line strings, so it carries the source file's hard
//! line wraps (a newline every ~90 columns, mid-sentence) and inline-code spans in backticks.
//! Rendered as-is, the PDF card broke lines mid-sentence and printed the backticks literally.
//!
//! The contract:
//! - [`prepare`] joins every single newline into one space and keeps a blank line as a
//!   paragraph break (normalised to exactly `"\n\n"`). Code-span backticks survive.
//! - [`segments`] is the ONE parser of code spans. The PDF template never parses backticks: Rust
//!   sends it the text and code runs, so the PDF and the plain-text artifacts cannot disagree.
//! - [`plain`] is [`segments`] joined with the span delimiters dropped, for the plain-text
//!   artifacts (xlsx cells, CSV, json/inspection.json) where delimiters would read as stray
//!   punctuation.

/// Join hard line wraps into spaces, keep blank-line paragraph breaks, trim the ends.
pub(crate) fn prepare(text: &str) -> String {
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in normalised.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.join("\n\n")
}

/// One piece of fix text: prose, or code (a span set in mono by the PDF).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// Prose.
    Text(String),
    /// Code, set in mono; its own backticks, if any, are part of the text.
    Code(String),
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// True when a template body reads as code. A body with whitespace on exactly one edge is prose
/// ("don`t use `foo`"). A body spaced out on BOTH edges (a multi-line template that `prepare`
/// joined: "sql` select 1 `") is code when it holds a space or separator inside. Otherwise the
/// body must hold a space or one of the code separators `()[]{}<>=;,$+*/\"'|&%:.-`; one plain word
/// ("t`Hello`") does not.
fn looks_like_code(body: &str) -> bool {
    let (Some(first), Some(last)) = (body.chars().next(), body.chars().next_back()) else {
        return false;
    };
    let is_sep = |c: char| "()[]{}<>=;,$+*/\\\"'|&%:.-".contains(c);
    match (first.is_whitespace(), last.is_whitespace()) {
        (true, true) => {
            let inner = body.trim();
            inner.chars().any(|c| c.is_whitespace() || is_sep(c))
        }
        (false, false) => body.chars().any(|c| c.is_whitespace() || is_sep(c)),
        _ => false,
    }
}

/// True when the brackets and parentheses of `body` do not balance (an opener never closed, or a
/// closer with no opener): "fetch(" is a template cut short. Brackets inside single or double
/// quotes (`split(')')`) and inside a `[...]` character class (`[^)]*`) are not counted, and a
/// body with no letter or digit (`{`, `(`, `)`) never counts as unbalanced.
fn unbalanced_brackets(body: &str) -> bool {
    if !body.chars().any(char::is_alphanumeric) {
        return false;
    }
    let chars: Vec<char> = body.chars().collect();
    let mut stack: Vec<char> = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == '\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '[' => match chars[i + 1..].iter().position(|x| *x == ']') {
                    // A character class or index: its contents are opaque.
                    Some(off) => i += off + 1,
                    None => return true,
                },
                '(' | '{' => stack.push(c),
                ')' | '}' => {
                    let want = if c == ')' { '(' } else { '{' };
                    if stack.pop() != Some(want) {
                        return true;
                    }
                }
                ']' => return true,
                _ => {}
            },
        }
        i += 1;
    }
    !stack.is_empty()
}

/// A word character, for the ambiguity check: a letter, digit or underscore.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Split prepared text into prose and code segments. The rule, and the only place it lives:
/// - A code span opened by a run of N backticks (N is 1 or 2) closes at the next run of exactly N.
///   One leading and one trailing space are stripped when both are present and the body is not
///   all spaces. The delimiters are not part of the code.
/// - A run with no matching closer is literal prose, and the scan goes on after it.
/// - A run of three or more backticks (a fence) is never a span: it prints literally, and a
///   longer run inside a span is body text.
/// - A single backtick that directly follows an identifier character (a bare tagged template such
///   as sql`select 1`) is part of the code itself when its body looks like code (see
///   [`looks_like_code`]): the code is the attached identifier, the opening backtick, the body
///   and the closing backtick, all kept. Adjacent templates join into one code run.
/// - AMBIGUITY: when, after that parse, any backtick that is NOT a bare template's opener or
///   closer is glued to a word character on its OUTSIDE (a span opener with a word character
///   directly before it, a span closer with one directly after it, or an unmatched run with one
///   on either side), the whole text is ONE prose run with every backtick printed literally.
///   Nothing is dropped, the text is still copy-paste ready, and none of it is set in mono.
///   The same holds for a span whose body has unbalanced brackets, or whose closer is directly
///   followed by '$', '<' or '{'. Ordinary prose spans ("(`A`) and (`B`)", "`a`=`b`",
///   "`GET`/`POST`") open after a space or punctuation and close before punctuation, so they are
///   never ambiguous.
///
/// A backtick inside code is never dropped.
pub(crate) fn segments(text: &str) -> Vec<Seg> {
    let bytes = text.as_bytes();
    // Maximal backtick runs as (start, len), fences (3 or more) left out.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let st = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            if i - st <= 2 {
                runs.push((st, i - st));
            }
        } else {
            i += 1;
        }
    }
    let word_before = |at: usize| text[..at].chars().next_back().is_some_and(is_word_char);
    let word_after = |at: usize| text[at..].chars().next().is_some_and(is_word_char);
    let mut out: Vec<Seg> = Vec::new();
    let mut ambiguous = false;
    let mut pos = 0usize;
    let mut k = 0usize;
    while k < runs.len() {
        let (start, n) = runs[k];
        let Some(j) = (k + 1..runs.len()).find(|&j| runs[j].1 == n) else {
            // An unmatched run prints literally; glued to a word it makes the text ambiguous.
            ambiguous |= word_before(start) || word_after(start + n);
            k += 1;
            continue;
        };
        let (cstart, _) = runs[j];
        let cend = cstart + n;
        let prefix = &text[pos..start];
        let tagged = n == 1
            && prefix.as_bytes().last().is_some_and(|b| is_ident(*b))
            && looks_like_code(&text[start + n..cstart]);
        if tagged {
            // Back up over the attached identifier (letters, digits, `_`, `$`, inner dots).
            let pb = prefix.as_bytes();
            let mut s = pb.len();
            while s > 0 && (is_ident(pb[s - 1]) || pb[s - 1] == b'.') {
                s -= 1;
            }
            while s < pb.len() && !is_ident(pb[s]) {
                s += 1;
            }
            if s > 0 {
                out.push(Seg::Text(prefix[..s].to_string()));
            }
            out.push(Seg::Code(format!("{}{}", &prefix[s..], &text[start..cend])));
        } else {
            // An ordinary span: a word glued to the outside of a delimiter is ambiguous, and so is
            // a body with unbalanced brackets (a template's backtick ended it early) or a closer
            // directly followed by '$', '<' or '{' (an expression or markup follows it). These
            // triggers only ever turn a parse into the literal text, never into more code.
            let body_text = &text[start + n..cstart];
            ambiguous |= word_before(start)
                || word_after(cend)
                || unbalanced_brackets(body_text)
                || text[cend..].starts_with(['$', '<', '{']);
            if !prefix.is_empty() {
                out.push(Seg::Text(prefix.to_string()));
            }
            let body = &text[start + n..cstart];
            let stripped = if body.len() >= 2
                && body.starts_with(' ')
                && body.ends_with(' ')
                && !body.bytes().all(|b| b == b' ')
            {
                &body[1..body.len() - 1]
            } else {
                body
            };
            out.push(Seg::Code(stripped.to_string()));
        }
        pos = cend;
        k = j + 1;
    }
    if ambiguous {
        return vec![Seg::Text(text.to_string())];
    }
    if pos < text.len() {
        out.push(Seg::Text(text[pos..].to_string()));
    }
    // Adjacent code runs (templates nested in one another) are one run.
    let mut merged: Vec<Seg> = Vec::new();
    for seg in out {
        match (merged.last_mut(), &seg) {
            (Some(Seg::Code(prev)), Seg::Code(next)) => prev.push_str(next),
            _ => merged.push(seg),
        }
    }
    merged
}

/// [`prepare`], then drop the backtick delimiters around each code span (see [`segments`]); the
/// backticks that belong to the code, and unmatched runs, stay.
pub(crate) fn plain(text: &str) -> String {
    join_plain(&segments(&prepare(text)))
}

/// The text of `segs` with the span delimiters already dropped, as one string.
pub(crate) fn join_plain(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| match s {
            Seg::Text(t) | Seg::Code(t) => t.as_str(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hard_wrapped_remediation_becomes_one_paragraph() {
        let src = "Rotate the key immediately: treat it as\ncompromised, since it has shipped.\nRemove it from `.env`\nand `VITE_`,\nREACT_APP_.";
        assert_eq!(
            prepare(src),
            "Rotate the key immediately: treat it as compromised, since it has shipped. Remove it from `.env` and `VITE_`, REACT_APP_."
        );
    }

    #[test]
    fn a_blank_line_still_splits_paragraphs() {
        assert_eq!(
            prepare("one\nwrapped\n\ntwo\n\n\n\nthree"),
            "one wrapped\n\ntwo\n\nthree"
        );
        // Blank lines made of spaces and CRLF endings behave the same.
        assert_eq!(prepare("a\r\nb\r\n  \r\nc"), "a b\n\nc");
    }

    #[test]
    fn leading_and_trailing_whitespace_is_trimmed() {
        assert_eq!(prepare("\n\n  hello\nworld  \n\n"), "hello world");
        assert_eq!(prepare(""), "");
        assert_eq!(prepare("  \n \n"), "");
    }

    #[test]
    fn plain_text_strips_the_backticks_of_each_span_and_nothing_else() {
        assert_eq!(
            plain("Edit `apps/web/.env.production`\nand `VITE_`."),
            "Edit apps/web/.env.production and VITE_."
        );
    }

    #[test]
    fn plain_text_keeps_backticks_that_are_not_a_span() {
        // Unclosed, empty and triple runs are not spans: they print literally.
        assert_eq!(plain("a ` b"), "a ` b");
        assert_eq!(plain("a `` b"), "a `` b");
        assert_eq!(plain("add ```rust fence"), "add ```rust fence");
    }

    #[test]
    fn plain_text_has_no_backticks_for_the_service_role_remediation_shape() {
        let src = "Remove it from\n`apps/web/.env.production` and any client-bundle-prefixed environment variable (`NEXT_PUBLIC_`, `VITE_`,\n`REACT_APP_`, `EXPO_PUBLIC_`), and move it.";
        let out = plain(src);
        assert!(!out.contains('`'), "{out}");
        assert!(!out.contains('\n'), "{out}");
    }

    #[test]
    fn a_tagged_template_is_one_code_segment_with_its_backticks() {
        let segs = segments("Run const rows = await sql`select id limit 30`; then set `x` to 1.");
        assert_eq!(
            segs,
            vec![
                Seg::Text("Run const rows = await ".to_string()),
                Seg::Code("sql`select id limit 30`".to_string()),
                Seg::Text("; then set ".to_string()),
                Seg::Code("x".to_string()),
                Seg::Text(" to 1.".to_string()),
            ]
        );
    }

    #[test]
    fn a_longer_run_wraps_code_that_holds_backticks() {
        assert_eq!(
            segments("Use `` echo `date` `` now"),
            vec![
                Seg::Text("Use ".to_string()),
                Seg::Code("echo `date`".to_string()),
                Seg::Text(" now".to_string()),
            ]
        );
    }
}
