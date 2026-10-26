//! Server-side syntax highlighting for the in-app code viewer
//! (`docs/design` / `WORK_ORDER_REVIEW_SPEED.md` A.3).
//!
//! The viewer receives render-ready lines, not a parse job: this module turns a source string
//! into, per line, a list of `(TokenClass, byte range within the line)` spans from a fixed set of
//! ten classes. Text outside every span is plain.
//!
//! # Languages
//!
//! | Language | Highlighter |
//! |---|---|
//! | Rust, TypeScript, TSX, JavaScript, Python | `tree-sitter-highlight` with each grammar's own highlight queries |
//! | SQL (`.sql`) | a hand-written keyword tokenizer ([`sql`]): comments, strings (including `$$` bodies), numbers, keywords, types |
//! | anything else | plain |
//!
//! # The contract: never panic, never delay
//!
//! A source that fails to highlight (a query that does not compile, a parse the highlighter
//! gives up on, anything that panics) yields plain lines with [`HighlightStatus::Plain`]. A
//! highlight that takes longer than [`DEFAULT_BUDGET`] (250 ms) is abandoned and reports
//! [`HighlightStatus::Skipped`], so opening a file never waits on a pathological input. Pure
//! and read-only: no filesystem, no process, no network.

use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use super::{lang_for_path, SourceLang};

/// How long a highlight may run before it is abandoned and the file shown plain.
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(250);

/// The ten token classes the viewer styles (`tok-keyword` ... `tok-operator`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenClass {
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
    Variable,
    Property,
    Punctuation,
    Operator,
}

impl TokenClass {
    /// All classes, in the order of [`TokenClass::CLASS_NAMES`].
    pub const ALL: [TokenClass; 10] = [
        TokenClass::Keyword,
        TokenClass::String,
        TokenClass::Comment,
        TokenClass::Number,
        TokenClass::Function,
        TokenClass::Type,
        TokenClass::Variable,
        TokenClass::Property,
        TokenClass::Punctuation,
        TokenClass::Operator,
    ];

    /// The class names, indexed by [`TokenClass::index`]. The wire form of a span is its index.
    pub const CLASS_NAMES: [&'static str; 10] = [
        "keyword",
        "string",
        "comment",
        "number",
        "function",
        "type",
        "variable",
        "property",
        "punctuation",
        "operator",
    ];

    /// Stable lower-case name (the CSS class is `tok-<name>`).
    pub fn label(self) -> &'static str {
        Self::CLASS_NAMES[self.index()]
    }

    /// Position in [`TokenClass::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }
}

/// What the highlighter could do with the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightStatus {
    /// Spans were produced.
    Highlighted,
    /// No highlighter for this language, or it failed: every line is plain.
    Plain,
    /// The highlight ran past its time budget and was abandoned: every line is plain.
    Skipped,
}

/// A highlighted source: one entry in `lines` per line of `source.lines()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlighted {
    /// Per line, non-overlapping spans in increasing order, as byte ranges WITHIN that line
    /// (the line's text excludes its terminator). Empty for a plain line.
    pub lines: Vec<Vec<(TokenClass, Range<usize>)>>,
    pub status: HighlightStatus,
}

/// A language the highlighter knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightLang {
    /// Tree-sitter highlighted.
    Source(SourceLang),
    /// The SQL keyword tokenizer.
    Sql,
}

/// The highlight language for a repo-relative path, by extension; `None` means plain.
pub fn highlight_lang_for_path(path: &str) -> Option<HighlightLang> {
    let is_sql = path
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("sql"));
    if is_sql {
        return Some(HighlightLang::Sql);
    }
    lang_for_path(path).map(HighlightLang::Source)
}

/// Highlight `source` as `lang` within [`DEFAULT_BUDGET`]. Never panics.
pub fn highlight(lang: Option<HighlightLang>, source: &str) -> Highlighted {
    highlight_with_budget(lang, source, DEFAULT_BUDGET)
}

/// [`highlight`] with an explicit time budget (a zero budget always reports `Skipped` for a
/// language that has a highlighter).
pub fn highlight_with_budget(
    lang: Option<HighlightLang>,
    source: &str,
    budget: Duration,
) -> Highlighted {
    let ranges = line_ranges(source);
    let plain = |status| Highlighted {
        lines: vec![Vec::new(); ranges.len()],
        status,
    };
    let Some(lang) = lang else {
        return plain(HighlightStatus::Plain);
    };
    let started = Instant::now();
    let spans = std::panic::catch_unwind(AssertUnwindSafe(|| match lang {
        HighlightLang::Source(l) => tree_sitter_spans(l, source, started, budget),
        HighlightLang::Sql => sql::spans(source, started, budget),
    }));
    match spans {
        Ok(Ok(spans)) => Highlighted {
            lines: split_into_lines(source, &ranges, spans),
            status: HighlightStatus::Highlighted,
        },
        Ok(Err(Abandon::OverBudget)) => plain(HighlightStatus::Skipped),
        Ok(Err(Abandon::Failed)) | Err(_) => plain(HighlightStatus::Plain),
    }
}

/// Why a highlighter gave up.
enum Abandon {
    OverBudget,
    Failed,
}

/// Absolute-offset spans over the whole source, in increasing, non-overlapping order.
type Spans = Vec<(TokenClass, Range<usize>)>;

/// Byte range of each line of `source`, matching `str::lines()` exactly (terminator and a
/// trailing `\r` excluded; no phantom empty line after a final `\n`).
pub fn line_ranges(source: &str) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let newline = bytes[start..].iter().position(|b| *b == b'\n');
        let (mut end, next) = match newline {
            Some(i) => (start + i, start + i + 1),
            None => (bytes.len(), bytes.len()),
        };
        if newline.is_some() && end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        out.push(start..end);
        start = next;
    }
    out
}

/// Cut whole-source spans at line boundaries into per-line spans, clamped to char boundaries.
fn split_into_lines(source: &str, ranges: &[Range<usize>], spans: Spans) -> Vec<Spans> {
    let mut lines: Vec<Spans> = vec![Vec::new(); ranges.len()];
    for (class, span) in spans {
        let start = floor_boundary(source, span.start);
        let end = ceil_boundary(source, span.end);
        if start >= end {
            continue;
        }
        // First line whose end is past the span start.
        let first = ranges.partition_point(|r| r.end <= start);
        for (offset, range) in ranges[first..].iter().enumerate() {
            if range.start >= end {
                break;
            }
            let from = start.max(range.start);
            let to = end.min(range.end);
            if from < to {
                lines[first + offset].push((class, from - range.start..to - range.start));
            }
        }
    }
    lines
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------------------------
// tree-sitter
// ---------------------------------------------------------------------------------------------

/// Capture names the highlight queries use that map onto a class, matched by longest dotted
/// prefix (`function.method` falls to `function`). Order is the `Highlight` index space.
const RECOGNIZED: [(&str, Option<TokenClass>); 20] = [
    ("keyword", Some(TokenClass::Keyword)),
    ("string", Some(TokenClass::String)),
    ("escape", Some(TokenClass::String)),
    ("comment", Some(TokenClass::Comment)),
    ("number", Some(TokenClass::Number)),
    ("function", Some(TokenClass::Function)),
    ("type", Some(TokenClass::Type)),
    ("constructor", Some(TokenClass::Type)),
    ("variable", Some(TokenClass::Variable)),
    ("constant", Some(TokenClass::Variable)),
    ("property", Some(TokenClass::Property)),
    ("attribute", Some(TokenClass::Property)),
    ("punctuation", Some(TokenClass::Punctuation)),
    ("operator", Some(TokenClass::Operator)),
    ("label", Some(TokenClass::Variable)),
    ("tag", Some(TokenClass::Keyword)),
    // `true` / `false` / `null` / `None` / numeric literals: split by first character below.
    ("constant.builtin", None),
    // Template substitutions and the like carry their own inner highlights.
    ("embedded", None),
    ("string.special", Some(TokenClass::String)),
    ("variable.builtin", Some(TokenClass::Keyword)),
];

const CONSTANT_BUILTIN: usize = 16;

/// The compiled highlight configuration of one language, built once. `None` when the grammar's
/// queries did not compile (the language then renders plain).
fn config_for(lang: SourceLang) -> Option<&'static HighlightConfiguration> {
    static RUST: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    static TS: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    static TSX: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    static JS: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    static PY: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    let (cell, build): (_, fn() -> Option<HighlightConfiguration>) = match lang {
        SourceLang::Rust => (&RUST, || {
            build_config(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            )
        }),
        SourceLang::TypeScript => (&TS, || {
            build_config(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                ]
                .join("\n"),
            )
        }),
        SourceLang::Tsx => (&TSX, || {
            build_config(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                ]
                .join("\n"),
            )
        }),
        SourceLang::JavaScript => (&JS, || {
            build_config(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                ]
                .join("\n"),
            )
        }),
        SourceLang::Python => (&PY, || {
            build_config(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
            )
        }),
    };
    let cell: &'static OnceLock<Option<HighlightConfiguration>> = cell;
    cell.get_or_init(build).as_ref()
}

fn build_config(
    language: tree_sitter::Language,
    name: &str,
    highlights: &str,
) -> Option<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(language, name, highlights, "", "").ok()?;
    let names: Vec<&str> = RECOGNIZED.iter().map(|(n, _)| *n).collect();
    config.configure(&names);
    Some(config)
}

fn tree_sitter_spans(
    lang: SourceLang,
    source: &str,
    started: Instant,
    budget: Duration,
) -> Result<Spans, Abandon> {
    let config = config_for(lang).ok_or(Abandon::Failed)?;
    if started.elapsed() > budget {
        return Err(Abandon::OverBudget);
    }
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, source.as_bytes(), None, |_| None)
        .map_err(|_| Abandon::Failed)?;
    let mut spans: Spans = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for event in events {
        if started.elapsed() > budget {
            return Err(Abandon::OverBudget);
        }
        match event.map_err(|_| Abandon::Failed)? {
            HighlightEvent::HighlightStart(h) => stack.push(h.0),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let Some(&top) = stack.last() else { continue };
                let Some(class) = class_of(top, source, start) else {
                    continue;
                };
                spans.push((class, start..end));
            }
        }
    }
    Ok(spans)
}

/// The class of capture `index` for the span starting at `start`.
fn class_of(index: usize, source: &str, start: usize) -> Option<TokenClass> {
    if index == CONSTANT_BUILTIN {
        // `42` is a number; `true`, `null`, `None` read as keywords.
        let first = source.as_bytes().get(start).copied()?;
        return Some(if first.is_ascii_digit() {
            TokenClass::Number
        } else {
            TokenClass::Keyword
        });
    }
    RECOGNIZED.get(index).and_then(|(_, class)| *class)
}

// ---------------------------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------------------------

/// The SQL keyword tokenizer. Hand-written (no regex dependency): comments, single-quoted and
/// dollar-quoted strings, double-quoted identifiers, numbers, keywords, types, operators.
mod sql {
    use super::{Abandon, Spans, TokenClass};
    use std::time::{Duration, Instant};

    /// Statements, DDL, clauses and the policy words Supabase RLS findings are about.
    const KEYWORDS: &[&str] = &[
        "select",
        "insert",
        "update",
        "delete",
        "from",
        "where",
        "into",
        "values",
        "set",
        "create",
        "alter",
        "drop",
        "table",
        "view",
        "index",
        "function",
        "trigger",
        "policy",
        "schema",
        "extension",
        "type",
        "grant",
        "revoke",
        "on",
        "to",
        "for",
        "as",
        "with",
        "check",
        "using",
        "enable",
        "disable",
        "row",
        "level",
        "security",
        "force",
        "returns",
        "return",
        "language",
        "security_definer",
        "definer",
        "invoker",
        "begin",
        "end",
        "declare",
        "if",
        "then",
        "else",
        "elsif",
        "loop",
        "execute",
        "perform",
        "and",
        "or",
        "not",
        "in",
        "is",
        "null",
        "true",
        "false",
        "like",
        "ilike",
        "between",
        "exists",
        "join",
        "inner",
        "left",
        "right",
        "full",
        "outer",
        "cross",
        "group",
        "order",
        "by",
        "having",
        "limit",
        "offset",
        "union",
        "all",
        "distinct",
        "case",
        "when",
        "default",
        "primary",
        "key",
        "foreign",
        "references",
        "unique",
        "constraint",
        "cascade",
        "restrict",
        "replace",
        "public",
        "authenticated",
        "anon",
        "service_role",
        "permissive",
        "restrictive",
        "usage",
        "privileges",
        "commit",
        "rollback",
        "transaction",
    ];

    const TYPES: &[&str] = &[
        "int",
        "integer",
        "bigint",
        "smallint",
        "serial",
        "bigserial",
        "numeric",
        "decimal",
        "real",
        "text",
        "varchar",
        "char",
        "boolean",
        "bool",
        "uuid",
        "date",
        "time",
        "timestamp",
        "timestamptz",
        "interval",
        "json",
        "jsonb",
        "bytea",
        "void",
        "trigger",
        "record",
        "setof",
    ];

    fn is_ident_start(b: u8) -> bool {
        b.is_ascii_alphabetic() || b == b'_'
    }

    fn is_ident(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
    }

    /// The end of a `$tag$` opener starting at `i`, if one starts there (`$1` is a parameter, not
    /// a dollar quote).
    fn dollar_open(b: &[u8], i: usize) -> Option<usize> {
        let mut j = i + 1;
        if j < b.len() && is_ident_start(b[j]) {
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
        }
        (j < b.len() && b[j] == b'$').then_some(j + 1)
    }

    pub(super) fn spans(
        source: &str,
        started: Instant,
        budget: Duration,
    ) -> Result<Spans, Abandon> {
        let b = source.as_bytes();
        let mut out: Spans = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if started.elapsed() > budget {
                return Err(Abandon::OverBudget);
            }
            let c = b[i];
            let start = i;
            if c == b'-' && b.get(i + 1) == Some(&b'-') {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                out.push((TokenClass::Comment, start..i));
            } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push((TokenClass::Comment, start..i));
            } else if c == b'\'' {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push((TokenClass::String, start..i));
            } else if let Some(open_end) = (c == b'$').then(|| dollar_open(b, i)).flatten() {
                let tag = &b[i..open_end];
                i = open_end;
                while i < b.len() && !b[i..].starts_with(tag) {
                    i += 1;
                }
                i = (i + tag.len()).min(b.len());
                out.push((TokenClass::String, start..i));
            } else if c == b'"' {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.push((TokenClass::Variable, start..i));
            } else if c.is_ascii_digit() {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                out.push((TokenClass::Number, start..i));
            } else if is_ident_start(c) {
                while i < b.len() && is_ident(b[i]) {
                    i += 1;
                }
                let word = source[start..i].to_ascii_lowercase();
                if KEYWORDS.contains(&word.as_str()) {
                    out.push((TokenClass::Keyword, start..i));
                } else if TYPES.contains(&word.as_str()) {
                    out.push((TokenClass::Type, start..i));
                } else if b.get(i) == Some(&b'(') {
                    out.push((TokenClass::Function, start..i));
                }
            } else if matches!(
                c,
                b'=' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'|' | b'&' | b'!' | b'~'
            ) || (c == b':' && b.get(i + 1) == Some(&b':'))
            {
                i += if c == b':' { 2 } else { 1 };
                out.push((TokenClass::Operator, start..i));
            } else if matches!(c, b'(' | b')' | b',' | b';' | b'.' | b'[' | b']') {
                i += 1;
                out.push((TokenClass::Punctuation, start..i));
            } else {
                i += 1;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(h: &Highlighted, class: TokenClass) -> bool {
        h.lines.iter().flatten().any(|(c, _)| *c == class)
    }

    fn text_of<'a>(source: &'a str, h: &Highlighted, class: TokenClass) -> Vec<&'a str> {
        let ranges = line_ranges(source);
        let mut out = Vec::new();
        for (n, spans) in h.lines.iter().enumerate() {
            for (c, r) in spans {
                if *c == class {
                    out.push(&source[ranges[n].start + r.start..ranges[n].start + r.end]);
                }
            }
        }
        out
    }

    /// One fixture per language: at least one keyword, string and comment span.
    fn fixture(path: &str) -> &'static str {
        match path {
            "a.rs" => "// note\nfn main() {\n    let s = \"hi\";\n    let n = 42;\n}\n",
            "a.ts" => "// note\nfunction f(x: number): string {\n  const s = \"hi\";\n  return s;\n}\n",
            "a.tsx" => "// note\nexport function C() {\n  const s = \"hi\";\n  return <div className=\"a\">{s}</div>;\n}\n",
            "a.js" => "// note\nfunction f() {\n  const s = 'hi';\n  return s;\n}\n",
            "a.py" => "# note\ndef f():\n    s = \"hi\"\n    return s\n",
            "a.sql" => "-- note\ncreate policy p on t for select using (owner = 'me');\n",
            other => panic!("no fixture for {other}"),
        }
    }

    #[test]
    fn every_language_yields_keyword_string_and_comment_spans() {
        for path in ["a.rs", "a.ts", "a.tsx", "a.js", "a.py", "a.sql"] {
            let src = fixture(path);
            let h = highlight(highlight_lang_for_path(path), src);
            assert_eq!(h.status, HighlightStatus::Highlighted, "{path}");
            assert_eq!(h.lines.len(), src.lines().count(), "{path}");
            for class in [TokenClass::Keyword, TokenClass::String, TokenClass::Comment] {
                assert!(has(&h, class), "{path} has no {class:?} span: {h:?}");
            }
        }
    }

    #[test]
    fn spans_cover_the_right_text() {
        let src = fixture("a.rs");
        let h = highlight(highlight_lang_for_path("a.rs"), src);
        assert!(text_of(src, &h, TokenClass::Keyword).contains(&"fn"));
        assert!(text_of(src, &h, TokenClass::String).contains(&"\"hi\""));
        assert!(text_of(src, &h, TokenClass::Comment)
            .iter()
            .any(|t| t.starts_with("// note")));
        assert!(text_of(src, &h, TokenClass::Number).contains(&"42"));
    }

    #[test]
    fn sql_spans_cover_the_right_text() {
        let src = fixture("a.sql");
        let h = highlight(highlight_lang_for_path("a.sql"), src);
        let keywords = text_of(src, &h, TokenClass::Keyword);
        for k in ["create", "policy", "select", "using"] {
            assert!(keywords.contains(&k), "{k} in {keywords:?}");
        }
        assert_eq!(text_of(src, &h, TokenClass::String), vec!["'me'"]);
        assert_eq!(text_of(src, &h, TokenClass::Comment), vec!["-- note"]);
    }

    #[test]
    fn a_multiline_construct_is_cut_at_line_boundaries() {
        let src = "/* one\n two */ select 1;\n$$ body\nmore $$;\n";
        let h = highlight(Some(HighlightLang::Sql), src);
        assert_eq!(h.lines.len(), 4);
        assert!(h.lines[0].iter().any(|(c, _)| *c == TokenClass::Comment));
        assert!(h.lines[1].iter().any(|(c, _)| *c == TokenClass::Comment));
        assert!(h.lines[2].iter().any(|(c, _)| *c == TokenClass::String));
        assert!(h.lines[3].iter().any(|(c, _)| *c == TokenClass::String));
        // Every range stays inside its own line.
        let ranges = line_ranges(src);
        for (n, spans) in h.lines.iter().enumerate() {
            for (_, r) in spans {
                assert!(r.end <= ranges[n].len(), "{n}: {r:?}");
            }
        }
    }

    #[test]
    fn dollar_params_and_escaped_quotes_do_not_open_strings() {
        let src = "select $1, 'it''s' from t;\n";
        let h = highlight(Some(HighlightLang::Sql), src);
        assert_eq!(text_of(src, &h, TokenClass::String), vec!["'it''s'"]);
    }

    #[test]
    fn malformed_input_degrades_to_plain_or_partial_without_panicking() {
        let junk = [
            "fn (((( \"unterminated\n  /* never closed\n }}}}",
            "\u{0}\u{1}\u{7f} garbage \u{1F600} \"",
            "<<<<<<< HEAD\nfn a(){}\n=======\nfn b(){}\n>>>>>>> x\n",
            "",
            "\n\n\n",
            "no trailing newline",
            "windows\r\nline\r\nendings\r\n",
        ];
        for path in ["a.rs", "a.ts", "a.tsx", "a.js", "a.py", "a.sql", "a.txt"] {
            for src in junk {
                let h = highlight(highlight_lang_for_path(path), src);
                assert_eq!(h.lines.len(), src.lines().count(), "{path} {src:?}");
                let ranges = line_ranges(src);
                for (n, spans) in h.lines.iter().enumerate() {
                    for (_, r) in spans {
                        assert!(r.start < r.end && r.end <= ranges[n].len());
                        assert!(src[ranges[n].clone()].get(r.clone()).is_some());
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_languages_are_plain() {
        let h = highlight(highlight_lang_for_path("Makefile"), "all:\n\techo hi\n");
        assert_eq!(h.status, HighlightStatus::Plain);
        assert_eq!(h.lines, vec![Vec::new(), Vec::new()]);
    }

    #[test]
    fn a_zero_budget_is_skipped_not_slow() {
        for path in ["a.rs", "a.sql"] {
            let h =
                highlight_with_budget(highlight_lang_for_path(path), fixture(path), Duration::ZERO);
            assert_eq!(h.status, HighlightStatus::Skipped, "{path}");
            assert!(h.lines.iter().all(Vec::is_empty));
        }
    }

    #[test]
    fn line_ranges_match_str_lines() {
        for src in ["", "a", "a\n", "a\nb", "a\r\nb\r\n", "\n\n", "a\n\nb\n"] {
            let from_ranges: Vec<&str> = line_ranges(src).iter().map(|r| &src[r.clone()]).collect();
            let expected: Vec<&str> = src.lines().collect();
            assert_eq!(from_ranges, expected, "{src:?}");
        }
    }

    #[test]
    fn class_names_line_up_with_indices() {
        for (i, class) in TokenClass::ALL.iter().enumerate() {
            assert_eq!(class.index(), i);
            assert_eq!(TokenClass::CLASS_NAMES[i], class.label());
        }
    }

    #[test]
    fn constant_builtin_index_matches_its_name() {
        assert_eq!(RECOGNIZED[CONSTANT_BUILTIN].0, "constant.builtin");
    }
}
