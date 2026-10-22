//! Tokenizer for Rust, built on `proc_macro2` token streams.
//!
//! `syn` supplies the top-level item boundaries and the item kinds; the tokens themselves come
//! from each item's token stream, not from walking the `syn` AST, because a token stream carries
//! every macro body (`json!`, `format!`, `sqlx::query!`) that an AST walk hides, and macro bodies
//! are code the client wrote (spec section 2).
//!
//! Rust has no token-level distinction between unary and binary operators, or between a generic
//! angle bracket and a comparison, so both are decided by the preceding token. A wrong guess can
//! only move a token between two classes, consistently in every copy; it can make two sequences
//! differ, never invent a match.

use proc_macro2::{Delimiter, Spacing, Span, TokenStream, TokenTree};
use quote::ToTokens;
use syn::{Attribute, ImplItem, Item};

use super::builder::ItemBuilder;
use super::{
    ItemTokens, TokenClass, TokenMode, TEXT_ARITH, TEXT_ASSIGN, TEXT_BIT, TEXT_CMP, TEXT_IDENT,
    TEXT_LIT, TEXT_LOGIC, TEXT_NUM, TEXT_PROP, TEXT_STR, TEXT_UNOP,
};
use crate::extract::canonical_number;

/// Forgets the spans this thread's `proc_macro2` created, once tokenizing is done with them.
///
/// `proc_macro2` keeps a per-thread source map of every text it has lexed, so a loop over 20,000
/// files would hold all of them (and eventually wrap its 32-bit offsets). Every position this
/// module needs is read while the stream is walked, so nothing outlives the call.
struct SpanReset;

impl Drop for SpanReset {
    fn drop(&mut self) {
        proc_macro2::extra::invalidate_current_thread_spans();
    }
}

/// Deepest delimiter nesting a file may have. `syn` builds its token buffer recursively, so an
/// adversarial `f!((((...))))` overflows the stack, and a stack overflow aborts the process
/// rather than unwinding. Real code never nests groups this deep; syn itself stops expressions
/// at 128.
const MAX_GROUP_DEPTH: usize = 200;

/// The deepest group nesting of a token stream, counted without recursion.
fn group_depth(stream: &TokenStream) -> usize {
    let mut stack = vec![stream.clone().into_iter()];
    let mut deepest = 1;
    while let Some(top) = stack.last_mut() {
        match top.next() {
            Some(TokenTree::Group(group)) => {
                stack.push(group.stream().into_iter());
                deepest = deepest.max(stack.len());
            }
            Some(_) => {}
            None => {
                stack.pop();
            }
        }
    }
    deepest
}

pub(super) fn tokenize(source: &str, mode: TokenMode) -> Option<Vec<ItemTokens>> {
    let _reset = SpanReset;
    // A `#!/usr/bin/env` line is not Rust tokens; blank it in place (same length, so every
    // byte offset and line number is unchanged), as `syn::parse_file` would.
    let shebang = source.starts_with("#!") && !source.starts_with("#![");
    let lexed: TokenStream = if shebang {
        format!("//{}", &source[2..]).parse().ok()?
    } else {
        source.parse().ok()?
    };
    if group_depth(&lexed) > MAX_GROUP_DEPTH {
        return None;
    }
    let file: syn::File = syn::parse2(lexed).ok()?;
    let lines = LineIndex::new(source);
    let mut builder = ItemBuilder::new(mode);
    collect_items(&file.items, &lines, &mut builder);
    Some(builder.finish())
}

/// Tokenize each item that contributes tokens; an inline `mod x { ... }` is flattened so its
/// functions are items of their own (a window must not run across a whole module).
fn collect_items(items: &[Item], lines: &LineIndex<'_>, builder: &mut ItemBuilder) {
    for item in items {
        if attrs_mark_test_code(item_attrs(item)) {
            continue;
        }
        let Some((kind, name)) = item_head(item) else {
            if let Item::Mod(module) = item {
                if let Some((_, inner)) = &module.content {
                    collect_items(inner, lines, builder);
                }
            }
            continue;
        };
        builder.begin_item(kind, name);
        let mut walker = StreamWalker::new(lines, builder);
        walker.walk(item.to_token_stream());
        builder.end_item();
    }
}

/// `None` for an item that contributes no tokens: imports, declarations without function
/// bodies, type aliases.
fn item_head(item: &Item) -> Option<(&'static str, Option<String>)> {
    match item {
        Item::Fn(f) => Some(("function", Some(f.sig.ident.to_string()))),
        Item::Impl(i) => {
            let has_body = i.items.iter().any(|m| matches!(m, ImplItem::Fn(_)));
            let name = i.self_ty.to_token_stream().to_string();
            Some((if has_body { "impl" } else { "impl-empty" }, Some(name)))
        }
        Item::Trait(t) => {
            // A trait with no default method body is a declaration (a type-level item).
            let has_body = t
                .items
                .iter()
                .any(|m| matches!(m, syn::TraitItem::Fn(f) if f.default.is_some()));
            has_body.then(|| ("trait", Some(t.ident.to_string())))
        }
        Item::Const(c) => Some((
            if is_data_expr(&c.expr) {
                "data"
            } else {
                "const"
            },
            Some(c.ident.to_string()),
        )),
        Item::Static(s) => Some((
            if is_data_expr(&s.expr) {
                "data"
            } else {
                "static"
            },
            Some(s.ident.to_string()),
        )),
        Item::Macro(m) => Some((
            "macro",
            m.ident
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| m.mac.path.segments.last().map(|s| s.ident.to_string())),
        )),
        Item::Verbatim(_) => Some(("other", None)),
        // use, extern crate, mod x;, struct, enum, union, type, foreign mod, trait alias.
        _ => None,
    }
}

/// An array, tuple or struct literal: a lookup table, not logic.
fn is_data_expr(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Array(_) | syn::Expr::Tuple(_) | syn::Expr::Struct(_) => true,
        syn::Expr::Reference(r) => is_data_expr(&r.expr),
        syn::Expr::Paren(p) => is_data_expr(&p.expr),
        _ => false,
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Fn(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Const(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        _ => &[],
    }
}

/// `#[cfg(test)]` (also `cfg(all(test, ..))`, never `cfg(not(test))`) and `#[test]` /
/// `#[tokio::test]`: in-file test code, excluded like a test path.
fn attrs_mark_test_code(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        if path.segments.last().is_some_and(|s| s.ident == "test") {
            return true;
        }
        if !path.is_ident("cfg") {
            return false;
        }
        let tokens = attr.meta.to_token_stream().to_string();
        let words: Vec<&str> = tokens
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .collect();
        words.contains(&"test") && !words.contains(&"not")
    })
}

// ---------------------------------------------------------------------------------------------
// Positions.
// ---------------------------------------------------------------------------------------------

/// Maps a `proc_macro2` (line, character column) position to a byte offset in the source.
struct LineIndex<'s> {
    source: &'s str,
    starts: Vec<usize>,
    ascii: Vec<bool>,
}

impl<'s> LineIndex<'s> {
    fn new(source: &'s str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        let ascii = (0..starts.len())
            .map(|i| source[starts[i]..Self::end_of(&starts, i, source.len())].is_ascii())
            .collect();
        Self {
            source,
            starts,
            ascii,
        }
    }

    fn end_of(starts: &[usize], line_index: usize, len: usize) -> usize {
        starts.get(line_index + 1).copied().unwrap_or(len)
    }

    /// Byte offset of 1-based `line`, 0-based character `column`.
    fn offset(&self, line: usize, column: usize) -> u32 {
        let Some(index) = line.checked_sub(1).filter(|i| *i < self.starts.len()) else {
            return u32::try_from(self.source.len()).unwrap_or(u32::MAX);
        };
        let start = self.starts[index];
        let byte = if self.ascii[index] {
            start + column
        } else {
            let end = Self::end_of(&self.starts, index, self.source.len());
            self.source[start..end]
                .char_indices()
                .nth(column)
                .map_or(end, |(i, _)| start + i)
        };
        u32::try_from(byte.min(self.source.len())).unwrap_or(u32::MAX)
    }

    fn start(&self, span: Span) -> u32 {
        let at = span.start();
        self.offset(at.line, at.column)
    }

    fn end(&self, span: Span) -> u32 {
        let at = span.end();
        self.offset(at.line, at.column)
    }
}

fn line_of(span: Span) -> u32 {
    u32::try_from(span.start().line).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------------------------
// The token walk.
// ---------------------------------------------------------------------------------------------

/// Rust keywords that stay verbatim in both modes (`true` and `false` are literals).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "type", "union", "unsafe",
    "use", "where", "while", "yield",
];

/// Keywords after which the next token begins a name (`fn name<T>`, `struct Name<T>`).
const DECLARATION_KEYWORDS: &[&str] = &["fn", "struct", "enum", "trait", "type", "union"];

/// Multi-character operators, longest first. A chain of joint punctuation is matched against
/// this list greedily, so `a=-1` lexes as `=` `-` and not as a fictional `=-`.
const OPERATORS: &[&str] = &[
    "<<=", ">>=", "...", "..=", "&&", "||", "<<", ">>", "==", "!=", "<=", ">=", "+=", "-=", "*=",
    "/=", "%=", "^=", "&=", "|=", "->", "=>", "::", "..",
];

/// What the previous emitted token was, for the unary/binary and generic/comparison decisions.
#[derive(Debug, Clone, Copy, Default)]
struct Previous {
    /// An expression could end here: a name, a literal, `)`, `]`, `?`.
    operand: bool,
    /// A non-keyword identifier.
    ident: bool,
    /// The identifier starts upper-case and is not an ALL_CAPS constant.
    type_like: bool,
    /// The identifier directly follows `fn`/`struct`/`enum`/`trait`/`type`/`union`.
    after_declaration: bool,
    /// The previous token was `::`.
    path_separator: bool,
    /// The previous token was a single `.`.
    dot: bool,
    /// The previous token was a keyword spelled `impl`.
    impl_keyword: bool,
    /// The previous token was `declaration keyword` (so the next ident is a declared name).
    declaration_keyword: bool,
}

struct StreamWalker<'a, 's> {
    lines: &'a LineIndex<'s>,
    builder: &'a mut ItemBuilder,
    previous: Previous,
    generic_depth: u32,
}

/// One open token stream: its trees, how far through them we are, and its closing delimiter.
struct Level {
    trees: Vec<TokenTree>,
    next: usize,
    close: Option<(&'static str, Span)>,
}

impl<'a, 's> StreamWalker<'a, 's> {
    fn new(lines: &'a LineIndex<'s>, builder: &'a mut ItemBuilder) -> Self {
        Self {
            lines,
            builder,
            previous: Previous::default(),
            generic_depth: 0,
        }
    }

    fn walk(&mut self, stream: TokenStream) {
        let mut stack = vec![Level {
            trees: stream.into_iter().collect(),
            next: 0,
            close: None,
        }];
        while let Some(level) = stack.last_mut() {
            let Some(tree) = level.trees.get(level.next).cloned() else {
                let close = level.close;
                stack.pop();
                if let Some((text, span)) = close {
                    self.emit_close(text, span);
                }
                continue;
            };
            level.next += 1;
            match tree {
                TokenTree::Group(group) => {
                    let (open, close) = match group.delimiter() {
                        Delimiter::Parenthesis => (Some("("), Some(")")),
                        Delimiter::Brace => (Some("{"), Some("}")),
                        Delimiter::Bracket => (Some("["), Some("]")),
                        Delimiter::None => (None, None),
                    };
                    if let Some(open) = open {
                        self.emit_open(open, group.span_open());
                    }
                    stack.push(Level {
                        trees: group.stream().into_iter().collect(),
                        next: 0,
                        close: close.map(|c| (c, group.span_close())),
                    });
                }
                TokenTree::Ident(ident) => {
                    let last = stack.len() - 1;
                    let level = &mut stack[last];
                    self.handle_ident(&ident.to_string(), ident.span(), level);
                }
                TokenTree::Literal(literal) => {
                    self.emit_literal(&literal.to_string(), literal.span());
                }
                TokenTree::Punct(punct) => {
                    let last = stack.len() - 1;
                    let level = &mut stack[last];
                    self.handle_punct(punct, level);
                }
            }
        }
    }

    fn emit_open(&mut self, text: &'static str, span: Span) {
        if text == "{" {
            self.generic_depth = 0;
        }
        self.builder.push(
            TokenClass::Punct,
            text,
            line_of(span),
            self.lines.start(span)..self.lines.end(span),
        );
        self.previous = Previous::default();
    }

    fn emit_close(&mut self, text: &'static str, span: Span) {
        if text == "}" {
            self.generic_depth = 0;
        }
        self.builder.push(
            TokenClass::Punct,
            text,
            line_of(span),
            self.lines.start(span)..self.lines.end(span),
        );
        self.previous = Previous {
            operand: text != "}",
            ..Previous::default()
        };
    }

    fn emit_word(&mut self, class: TokenClass, structural: &str, exact: &str, span: Span) {
        self.builder.push_split(
            class,
            structural,
            || exact.to_string(),
            line_of(span),
            self.lines.start(span)..self.lines.end(span),
        );
    }

    fn handle_ident(&mut self, text: &str, span: Span, level: &mut Level) {
        // A `pub` / `pub(crate)` / `pub(in path)` prefix is visibility, not code.
        if text == "pub" {
            if let Some(TokenTree::Group(group)) = level.trees.get(level.next) {
                if group.delimiter() == Delimiter::Parenthesis && is_visibility_scope(group) {
                    level.next += 1;
                }
            }
            return;
        }
        let previous = self.previous;
        if text == "true" || text == "false" {
            self.builder.push_split(
                TokenClass::Lit,
                TEXT_LIT,
                || text.to_string(),
                line_of(span),
                self.lines.start(span)..self.lines.end(span),
            );
            self.previous = Previous {
                operand: true,
                ..Previous::default()
            };
        } else if KEYWORDS.contains(&text) {
            self.emit_word(TokenClass::Keyword, text, text, span);
            self.previous = Previous {
                operand: matches!(text, "self" | "Self"),
                impl_keyword: text == "impl",
                declaration_keyword: DECLARATION_KEYWORDS.contains(&text),
                ..Previous::default()
            };
        } else if previous.dot {
            self.builder.push_split(
                TokenClass::Prop,
                TEXT_PROP,
                || text.to_string(),
                line_of(span),
                self.lines.start(span)..self.lines.end(span),
            );
            self.previous = Previous {
                operand: true,
                ..Previous::default()
            };
        } else {
            // A name directly followed by `(` that does not declare anything is a call (or a
            // tuple constructor): Exact mode keeps its text, as it does for ECMAScript and
            // Python callees. `fn name(` declares, and a rename of it is not a disagreement.
            let called = !previous.declaration_keyword
                && matches!(
                    level.trees.get(level.next),
                    Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis
                );
            self.emit_word(
                TokenClass::Ident,
                TEXT_IDENT,
                if called { text } else { TEXT_IDENT },
                span,
            );
            let upper_first = text.chars().next().is_some_and(char::is_uppercase);
            let all_caps = text.len() > 1
                && text
                    .chars()
                    .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_');
            self.previous = Previous {
                operand: true,
                ident: true,
                type_like: upper_first && !all_caps,
                after_declaration: previous.declaration_keyword,
                ..Previous::default()
            };
        }
    }

    fn emit_literal(&mut self, text: &str, span: Span) {
        let starts_numeric = text.starts_with(|c: char| c.is_ascii_digit());
        if starts_numeric {
            self.builder.push_split(
                TokenClass::Num,
                TEXT_NUM,
                || {
                    let bare = strip_numeric_suffix(text);
                    canonical_number(bare).unwrap_or_else(|| text.to_string())
                },
                line_of(span),
                self.lines.start(span)..self.lines.end(span),
            );
        } else {
            self.builder.push_split(
                TokenClass::Str,
                TEXT_STR,
                || string_content(text),
                line_of(span),
                self.lines.start(span)..self.lines.end(span),
            );
        }
        self.previous = Previous {
            operand: true,
            ..Previous::default()
        };
    }

    /// One run of joint punctuation, split into operators.
    fn handle_punct(&mut self, first: proc_macro2::Punct, level: &mut Level) {
        // Skip a doc comment or doc attribute: `#[doc = "..."]` / `#![doc = "..."]`.
        if first.as_char() == '#' && is_doc_attribute(&level.trees, level.next) {
            level.next += if matches!(level.trees.get(level.next), Some(TokenTree::Punct(p)) if p.as_char() == '!')
            {
                2
            } else {
                1
            };
            return;
        }
        let mut chain = vec![(first.as_char(), first.span())];
        let mut joint = first.spacing() == Spacing::Joint;
        while joint {
            let Some(TokenTree::Punct(next)) = level.trees.get(level.next) else {
                break;
            };
            chain.push((next.as_char(), next.span()));
            joint = next.spacing() == Spacing::Joint;
            level.next += 1;
        }
        let text: String = chain.iter().map(|(c, _)| *c).collect();
        let macro_bang_follows = matches!(level.trees.get(level.next), Some(TokenTree::Group(_)));
        let mut at = 0;
        while at < text.len() {
            let rest = &text[at..];
            let length = self.next_operator(rest);
            let op = &rest[..length];
            let from = chain[at].1;
            let to = chain[at + length - 1].1;
            let span_start = self.lines.start(from);
            let span_end = self.lines.end(to);
            self.emit_operator(op, line_of(from), span_start..span_end, macro_bang_follows);
            at += length;
        }
    }

    /// The byte length of the operator starting `rest` (greedy, with generic brackets split).
    fn next_operator(&self, rest: &str) -> usize {
        if rest.starts_with('>') && self.generic_depth > 0 {
            return 1;
        }
        if rest.starts_with('<') && self.opens_generic() {
            return 1;
        }
        OPERATORS
            .iter()
            .find(|op| rest.starts_with(**op))
            .map_or(1, |op| op.len())
    }

    /// Whether a `<` at this point opens a generic argument list rather than comparing.
    fn opens_generic(&self) -> bool {
        let p = self.previous;
        p.path_separator
            || p.impl_keyword
            || !p.operand
            || (p.ident && (p.type_like || p.after_declaration))
    }

    fn emit_operator(
        &mut self,
        op: &str,
        line: u32,
        raw: std::ops::Range<u32>,
        macro_bang_follows: bool,
    ) {
        let p = self.previous;
        let (class, structural): (TokenClass, &str) = match op {
            "<" if self.opens_generic() => {
                self.generic_depth += 1;
                (TokenClass::Punct, op)
            }
            ">" if self.generic_depth > 0 => {
                self.generic_depth -= 1;
                (TokenClass::Punct, op)
            }
            "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "&=" | "|=" | "<<=" | ">>=" => {
                (TokenClass::Assign, TEXT_ASSIGN)
            }
            "==" | "!=" | "<=" | ">=" | "<" | ">" => (TokenClass::BinOp, TEXT_CMP),
            "&&" | "||" if p.operand => (TokenClass::BinOp, TEXT_LOGIC),
            "&&" => (TokenClass::UnOp, TEXT_UNOP),
            "+" | "-" | "*" | "/" | "%" if p.operand => (TokenClass::BinOp, TEXT_ARITH),
            "-" | "*" => (TokenClass::UnOp, TEXT_UNOP),
            "&" if p.operand => (TokenClass::BinOp, TEXT_BIT),
            "&" => (TokenClass::UnOp, TEXT_UNOP),
            "|" if p.operand => (TokenClass::BinOp, TEXT_BIT),
            "^" | "<<" | ">>" => (TokenClass::BinOp, TEXT_BIT),
            "!" if p.ident && macro_bang_follows => (TokenClass::Punct, op),
            "!" => (TokenClass::UnOp, TEXT_UNOP),
            _ => (TokenClass::Punct, op),
        };
        self.builder
            .push_split(class, structural, || op.to_string(), line, raw);
        self.previous = Previous {
            operand: op == "?",
            path_separator: op == "::",
            dot: op == ".",
            ..Previous::default()
        };
    }
}

/// `(crate)`, `(super)`, `(self)`, `(in path)`: the scope of a `pub(...)`.
fn is_visibility_scope(group: &proc_macro2::Group) -> bool {
    matches!(
        group.stream().into_iter().next(),
        Some(TokenTree::Ident(i)) if matches!(i.to_string().as_str(), "crate" | "super" | "self" | "in")
    )
}

/// Whether the trees at `at` are `[doc ...]` or `![doc ...]` (the rest of a `#` attribute).
fn is_doc_attribute(trees: &[TokenTree], at: usize) -> bool {
    let mut index = at;
    if matches!(trees.get(index), Some(TokenTree::Punct(p)) if p.as_char() == '!') {
        index += 1;
    }
    let Some(TokenTree::Group(group)) = trees.get(index) else {
        return false;
    };
    group.delimiter() == Delimiter::Bracket
        && matches!(
            group.stream().into_iter().next(),
            Some(TokenTree::Ident(i)) if i == "doc"
        )
}

/// A numeric literal without its type suffix (`10u32`, `0.5_f64`, `3usize`).
fn strip_numeric_suffix(text: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
        "f32", "f64",
    ];
    for suffix in SUFFIXES {
        if let Some(bare) = text.strip_suffix(suffix) {
            // Not a hex digit run such as `0xf32`.
            if !text.starts_with("0x") {
                return bare.trim_end_matches('_');
            }
        }
    }
    text
}

/// The content of a string, byte string, raw string or char literal, without delimiters or
/// prefix. A literal that does not match a known shape is returned whole.
fn string_content(text: &str) -> String {
    let body = text
        .strip_prefix("br")
        .or_else(|| text.strip_prefix('r'))
        .or_else(|| text.strip_prefix('b'))
        .or_else(|| text.strip_prefix('c'))
        .unwrap_or(text);
    // A raw string `r#"..."#` carries the same number of `#` on both sides.
    let hashes = body.chars().take_while(|c| *c == '#').count();
    let inner = &body[hashes..];
    let inner = inner.strip_suffix(&"#".repeat(hashes)).unwrap_or(inner);
    let unwrapped = inner
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    match unwrapped {
        Some(content) => content.replace("\\\"", "\"").replace("\\'", "'"),
        None => text.to_string(),
    }
}
