//! Tokenizer for the tree-sitter dialects: TypeScript, TSX, JavaScript and Python.
//!
//! The token stream is the pre-order sequence of leaf nodes of each top-level item (spec
//! section 2), with these exceptions: a string is ONE token (a template literal or f-string
//! emits its substitutions' tokens after it, between the grammar's own `${`/`{` and `}`); a
//! comment (and a Python docstring) is dropped; a Python `block` emits a synthetic `{` on entry
//! and `}` on exit, because tree-sitter-python has no indent leaves.
//!
//! The walk is an explicit cursor loop, never recursion: a tree-sitter tree for a long operator
//! chain is thousands of levels deep, and this module must not overflow the stack on any input.

use tree_sitter::{Node, TreeCursor};

use super::builder::ItemBuilder;
use super::{
    ItemTokens, TokenClass, TokenMode, TEXT_ARITH, TEXT_ASSIGN, TEXT_BIT, TEXT_CMP, TEXT_IDENT,
    TEXT_LIT, TEXT_LOGIC, TEXT_NUM, TEXT_PROP, TEXT_STR, TEXT_UNOP,
};
use crate::extract::ecma::{self, EcmaDialect};
use crate::extract::{canonical_number, python, SourceLang};

/// Which grammar family is being walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Ecma,
    Python,
}

pub(super) fn tokenize(lang: SourceLang, source: &str, mode: TokenMode) -> Option<Vec<ItemTokens>> {
    match lang {
        SourceLang::Python => {
            let tree = python::parse(source)?;
            Some(walk_file(Dialect::Python, tree.root_node(), source, mode))
        }
        SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => {
            let dialect = match lang {
                SourceLang::TypeScript => EcmaDialect::TypeScript,
                SourceLang::Tsx => EcmaDialect::Tsx,
                _ => EcmaDialect::JavaScript,
            };
            let mut tree = ecma::parse(dialect, source)?;
            // The JavaScript grammar cannot parse some JSX (an attribute named `class`); the TSX
            // grammar can. A JS file that did not parse cleanly gets the same second chance the
            // literal extractor gives it.
            if dialect == EcmaDialect::JavaScript && tree.root_node().has_error() {
                if let Some(retry) =
                    ecma::parse(EcmaDialect::Tsx, source).filter(|t| !t.root_node().has_error())
                {
                    tree = retry;
                }
            }
            Some(walk_file(Dialect::Ecma, tree.root_node(), source, mode))
        }
        SourceLang::Rust => None,
    }
}

/// Tokenize every top-level item of a parsed file.
fn walk_file(dialect: Dialect, root: Node<'_>, source: &str, mode: TokenMode) -> Vec<ItemTokens> {
    let mut walker = Walker {
        dialect,
        source,
        builder: ItemBuilder::new(mode),
        stack: Vec::new(),
    };
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        // An item holding a parse-error node is dropped, not guessed at.
        if child.has_error() {
            continue;
        }
        let Some(head) = (match dialect {
            Dialect::Ecma => ecma_item(child, source),
            Dialect::Python => python_item(child, source),
        }) else {
            continue;
        };
        walker.builder.begin_item(head.kind, head.name);
        walker.emit_item(child);
        walker.builder.end_item();
    }
    walker.builder.finish()
}

/// What a top-level item is, once it is known to contribute tokens.
struct ItemHead {
    kind: &'static str,
    name: Option<String>,
}

fn text_of<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

fn name_of(node: Node<'_>, source: &str) -> Option<String> {
    let name = node.child_by_field_name("name")?;
    Some(text_of(name, source).to_string())
}

// ---------------------------------------------------------------------------------------------
// Top-level item classification: which items contribute tokens, and what they are called.
// ---------------------------------------------------------------------------------------------

/// `None` for an item that contributes no tokens: imports, re-exports, type-only declarations,
/// comments, directives.
fn ecma_item(node: Node<'_>, source: &str) -> Option<ItemHead> {
    match node.kind() {
        "comment"
        | "hash_bang_line"
        | "empty_statement"
        | "import_statement"
        | "import_alias"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration"
        | "ambient_declaration"
        | "function_signature" => None,
        "export_statement" => {
            // `export { a } from 'x'`, `export * from 'x'`, `export { a }`, `export = x`.
            if node.child_by_field_name("source").is_some() {
                return None;
            }
            let inner = node
                .child_by_field_name("declaration")
                .or_else(|| node.child_by_field_name("value"))?;
            ecma_item(inner, source)
        }
        "expression_statement" => {
            // A directive prologue (`'use client'`) is a statement of one string.
            let only = node.named_child(0)?;
            if node.named_child_count() == 1 && only.kind() == "string" {
                return None;
            }
            Some(ecma_statement_head(only))
        }
        "function_declaration" | "generator_function_declaration" => Some(ItemHead {
            kind: "function",
            name: name_of(node, source),
        }),
        "class_declaration" | "abstract_class_declaration" => Some(ItemHead {
            kind: "class",
            name: name_of(node, source),
        }),
        "lexical_declaration" | "variable_declaration" => Some(ecma_variable_head(node, source)),
        "internal_module" | "module" => Some(ItemHead {
            kind: "namespace",
            name: name_of(node, source),
        }),
        _ => ecma_expression_head(node, source),
    }
}

/// The head of an `export default <expression>` value, or any other item kind not listed above.
fn ecma_expression_head(node: Node<'_>, source: &str) -> Option<ItemHead> {
    match node.kind() {
        "function_expression" | "arrow_function" | "generator_function" | "function" => {
            Some(ItemHead {
                kind: "function",
                name: name_of(node, source),
            })
        }
        "class" => Some(ItemHead {
            kind: "class",
            name: name_of(node, source),
        }),
        "object" | "array" => Some(ItemHead {
            kind: "data",
            name: None,
        }),
        _ => Some(ItemHead {
            kind: "statement",
            name: None,
        }),
    }
}

/// An expression statement: `module.exports = { ... }` is a data module, anything else a
/// statement.
fn ecma_statement_head(expression: Node<'_>) -> ItemHead {
    if expression.kind() == "assignment_expression" {
        if let Some(right) = expression.child_by_field_name("right") {
            if ecma_is_data_value(right) {
                return ItemHead {
                    kind: "data",
                    name: None,
                };
            }
        }
    }
    ItemHead {
        kind: "statement",
        name: None,
    }
}

/// A `const`/`let`/`var` declaration: a function when the first declarator is one, a data
/// module item when EVERY declarator is an object or array literal.
fn ecma_variable_head(node: Node<'_>, source: &str) -> ItemHead {
    let mut cursor = node.walk();
    let declarators: Vec<Node<'_>> = node
        .children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .collect();
    let name = declarators
        .first()
        .and_then(|d| d.child_by_field_name("name"))
        .map(|n| text_of(n, source).to_string());
    let values: Vec<Option<Node<'_>>> = declarators
        .iter()
        .map(|d| d.child_by_field_name("value"))
        .collect();
    if !values.is_empty() && values.iter().all(|v| v.is_some_and(ecma_is_data_value)) {
        return ItemHead { kind: "data", name };
    }
    let is_function = values.first().copied().flatten().is_some_and(|v| {
        matches!(
            v.kind(),
            "arrow_function" | "function_expression" | "generator_function" | "function"
        )
    });
    ItemHead {
        kind: if is_function { "function" } else { "variable" },
        name,
    }
}

/// An object or array literal, through the wrappers a typed constant adds (`as const`,
/// `satisfies T`, parentheses).
fn ecma_is_data_value(node: Node<'_>) -> bool {
    let mut current = node;
    for _ in 0..8 {
        match current.kind() {
            "object" | "array" => return true,
            "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
            | "parenthesized_expression"
            | "type_assertion" => match current.named_child(0) {
                Some(inner) => current = inner,
                None => return false,
            },
            _ => return false,
        }
    }
    false
}

fn python_item(node: Node<'_>, source: &str) -> Option<ItemHead> {
    match node.kind() {
        "comment" | "import_statement" | "import_from_statement" | "future_import_statement" => {
            None
        }
        "function_definition" => Some(ItemHead {
            kind: "function",
            name: name_of(node, source),
        }),
        "class_definition" => Some(ItemHead {
            kind: "class",
            name: name_of(node, source),
        }),
        "decorated_definition" => {
            let inner = node.child_by_field_name("definition")?;
            python_item(inner, source)
        }
        "expression_statement" => {
            let only = node.named_child(0)?;
            // A module docstring is documentation, like a comment.
            if node.named_child_count() == 1 && only.kind() == "string" {
                return None;
            }
            if only.kind() == "assignment" {
                let name = only
                    .child_by_field_name("left")
                    .filter(|l| l.kind() == "identifier")
                    .map(|l| text_of(l, source).to_string());
                let is_data = only
                    .child_by_field_name("right")
                    .is_some_and(|r| matches!(r.kind(), "dictionary" | "list" | "set" | "tuple"));
                return Some(ItemHead {
                    kind: if is_data { "data" } else { "variable" },
                    name,
                });
            }
            Some(ItemHead {
                kind: "statement",
                name: None,
            })
        }
        _ => Some(ItemHead {
            kind: "statement",
            name: None,
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// The token walk.
// ---------------------------------------------------------------------------------------------

/// One ancestor of the node being visited.
struct Frame {
    kind: &'static str,
    /// Inside the `name` of a JSX element: every identifier here is a tag name.
    tag_name: bool,
}

struct Walker<'s> {
    dialect: Dialect,
    source: &'s str,
    builder: ItemBuilder,
    stack: Vec<Frame>,
}

/// Parent node kinds whose anonymous operator children are binary operators.
const BINARY_PARENTS: &[&str] = &[
    "binary_expression",
    "binary_operator",
    "boolean_operator",
    "comparison_operator",
];

/// Parent node kinds whose anonymous operator children are unary operators.
const UNARY_PARENTS: &[&str] = &[
    "unary_expression",
    "unary_operator",
    "not_operator",
    "update_expression",
    "await_expression",
    "await",
];

/// Assignment operators, in every dialect covered.
const ASSIGN_OPS: &[&str] = &[
    "=", "+=", "-=", "*=", "/=", "%=", "**=", "<<=", ">>=", ">>>=", "&=", "|=", "^=", "&&=", "||=",
    "??=", "//=", "@=", ":=",
];

/// JSX container kinds whose `name` field is a tag name.
const JSX_NAME_PARENTS: &[&str] = &[
    "jsx_opening_element",
    "jsx_closing_element",
    "jsx_self_closing_element",
];

impl Walker<'_> {
    /// Tokenize the subtree of one top-level item.
    fn emit_item(&mut self, root: Node<'_>) {
        self.stack.clear();
        let mut cursor: TreeCursor<'_> = root.walk();
        loop {
            let node = cursor.node();
            let field = cursor.field_name();
            if self.enter(node, field) && cursor.goto_first_child() {
                self.push_frame(node, field);
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.stack.clear();
                    return;
                }
                self.exit(cursor.node());
            }
        }
    }

    fn push_frame(&mut self, node: Node<'_>, field: Option<&str>) {
        let tag_name = self.stack.last().is_some_and(|parent| {
            parent.tag_name || (JSX_NAME_PARENTS.contains(&parent.kind) && field == Some("name"))
        });
        self.stack.push(Frame {
            kind: node.kind(),
            tag_name,
        });
    }

    /// Leave an interior node.
    fn exit(&mut self, node: Node<'_>) {
        self.stack.pop();
        if self.dialect == Dialect::Python && node.kind() == "block" {
            let end = node.end_position();
            self.emit(
                TokenClass::Block,
                "}",
                line_of(end.row),
                node.end_byte()..node.end_byte(),
            );
        }
    }

    /// Visit a node. Returns whether the walk should descend into its children.
    fn enter(&mut self, node: Node<'_>, field: Option<&str>) -> bool {
        let kind = node.kind();
        let parent_kind = self.stack.last().map_or("", |f| f.kind);
        if kind == "comment" || kind == "line_continuation" {
            return false;
        }
        // The `export` and `default` keywords of a top-level export are visibility, not code:
        // one token of visibility must not split a family.
        if self.dialect == Dialect::Ecma
            && self.stack.len() == 1
            && parent_kind == "export_statement"
            && !node.is_named()
            && matches!(kind, "export" | "default")
        {
            return false;
        }
        match (self.dialect, kind) {
            (Dialect::Ecma, "string") => {
                self.emit_string(node, ecma_string_exact(node, self.source));
                return false;
            }
            (Dialect::Ecma, "template_string") => {
                self.emit_string(node, ecma_string_exact(node, self.source));
                return true;
            }
            (Dialect::Ecma, "regex") => {
                let raw = text_of(node, self.source).to_string();
                self.emit_string(node, raw);
                return false;
            }
            (Dialect::Python, "string") => {
                self.emit_string(node, python_string_exact(node, self.source));
                return node_has_interpolation(node);
            }
            _ => {}
        }
        // Inside a string only a substitution carries code; its quotes and fragments are the
        // one string token already emitted.
        if (parent_kind == "template_string" && kind != "template_substitution")
            || (self.dialect == Dialect::Python
                && parent_kind == "string"
                && kind != "interpolation")
        {
            return false;
        }
        if self.dialect == Dialect::Python && self.is_docstring(node, parent_kind) {
            return false;
        }
        if self.dialect == Dialect::Python && kind == "block" {
            let start = node.start_position();
            self.emit(
                TokenClass::Block,
                "{",
                line_of(start.row),
                node.start_byte()..node.start_byte(),
            );
            return true;
        }
        if self.dialect == Dialect::Python && matches!(kind, "not in" | "is not") {
            // An aliased two-word operator: one comparison token.
            self.emit_binary(node, kind);
            return false;
        }
        if node.child_count() > 0 {
            return true;
        }
        self.emit_leaf(node, field);
        false
    }

    /// A Python docstring: an expression statement of one string, first in its block.
    fn is_docstring(&self, node: Node<'_>, parent_kind: &str) -> bool {
        node.kind() == "expression_statement"
            && parent_kind == "block"
            && node.named_child_count() == 1
            && node.named_child(0).is_some_and(|c| c.kind() == "string")
            && node.prev_named_sibling().is_none()
    }

    fn emit(&mut self, class: TokenClass, text: &str, line: u32, raw: std::ops::Range<usize>) {
        self.builder.push(class, text, line, byte_range(raw));
    }

    fn emit_string(&mut self, node: Node<'_>, exact: String) {
        self.builder.push_split(
            TokenClass::Str,
            TEXT_STR,
            || exact,
            line_of(node.start_position().row),
            byte_range(node.byte_range()),
        );
    }

    fn emit_binary(&mut self, node: Node<'_>, text: &str) {
        let structural = binary_family(text).unwrap_or(text);
        self.builder.push_split(
            TokenClass::BinOp,
            structural,
            || text.to_string(),
            line_of(node.start_position().row),
            byte_range(node.byte_range()),
        );
    }

    /// Emit one leaf node.
    fn emit_leaf(&mut self, node: Node<'_>, field: Option<&str>) {
        let text = text_of(node, self.source);
        if text.is_empty() {
            return;
        }
        let line = line_of(node.start_position().row);
        let raw = byte_range(node.byte_range());
        let parent = self.stack.last();
        let parent_kind = parent.map_or("", |f| f.kind);
        let tag_name = parent.is_some_and(|f| {
            f.tag_name || (JSX_NAME_PARENTS.contains(&f.kind) && field == Some("name"))
        });
        if !node.is_named() {
            self.emit_anonymous(text, parent_kind, line, raw);
            return;
        }
        let kind = node.kind();
        match kind {
            "identifier"
            | "type_identifier"
            | "statement_identifier"
            | "property_identifier"
            | "private_property_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "shorthand_field_identifier" => {
                self.emit_identifier(kind, text, field, parent_kind, tag_name, line, raw);
            }
            "number" | "integer" | "float" => {
                self.builder.push_split(
                    TokenClass::Num,
                    TEXT_NUM,
                    || canonical_number(text).unwrap_or_else(|| text.to_string()),
                    line,
                    raw,
                );
            }
            "true" | "false" | "null" | "none" | "undefined" => {
                self.builder
                    .push_split(TokenClass::Lit, TEXT_LIT, || text.to_string(), line, raw);
            }
            "this" | "super" => self.builder.push(TokenClass::Keyword, text, line, raw),
            "predefined_type" => self.builder.push(TokenClass::Keyword, text, line, raw),
            "jsx_text" => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    self.builder.push_split(
                        TokenClass::Str,
                        TEXT_STR,
                        || trimmed.split_whitespace().collect::<Vec<_>>().join(" "),
                        line,
                        raw,
                    );
                }
            }
            _ => {
                // An unmodelled named leaf is kept verbatim: it can only ever make two
                // sequences differ, never match.
                let class = if text.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                    TokenClass::Keyword
                } else {
                    TokenClass::Punct
                };
                self.builder.push(class, text, line, raw);
            }
        }
    }

    /// An anonymous leaf: a keyword, an operator or punctuation.
    fn emit_anonymous(
        &mut self,
        text: &str,
        parent_kind: &str,
        line: u32,
        raw: std::ops::Range<u32>,
    ) {
        if ASSIGN_OPS.contains(&text) {
            self.builder.push_split(
                TokenClass::Assign,
                TEXT_ASSIGN,
                || text.to_string(),
                line,
                raw,
            );
            return;
        }
        if BINARY_PARENTS.contains(&parent_kind) {
            if let Some(family) = binary_family(text) {
                self.builder
                    .push_split(TokenClass::BinOp, family, || text.to_string(), line, raw);
                return;
            }
        }
        if UNARY_PARENTS.contains(&parent_kind) {
            // `await` and `not` change what the expression does, so they stay verbatim.
            if matches!(text, "await" | "not") {
                self.builder.push(TokenClass::UnOp, text, line, raw);
            } else {
                self.builder.push_split(
                    TokenClass::UnOp,
                    TEXT_UNOP,
                    || text.to_string(),
                    line,
                    raw,
                );
            }
            return;
        }
        let class = if text.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            TokenClass::Keyword
        } else {
            TokenClass::Punct
        };
        self.builder.push(class, text, line, raw);
    }

    /// An identifier-like leaf, classified by the role the grammar gives it.
    #[allow(clippy::too_many_arguments)]
    fn emit_identifier(
        &mut self,
        kind: &str,
        text: &str,
        field: Option<&str>,
        parent_kind: &str,
        tag_name: bool,
        line: u32,
        raw: std::ops::Range<u32>,
    ) {
        if tag_name {
            self.builder.push(TokenClass::JsxTag, text, line, raw);
            return;
        }
        if kind == "property_identifier" && parent_kind == "jsx_attribute" {
            self.builder.push(TokenClass::JsxAttr, text, line, raw);
            return;
        }
        if text == "undefined" {
            self.builder
                .push_split(TokenClass::Lit, TEXT_LIT, || text.to_string(), line, raw);
            return;
        }
        if self.dialect == Dialect::Python && matches!(text, "self" | "cls") {
            self.builder.push(TokenClass::Keyword, text, line, raw);
            return;
        }
        let is_property = match kind {
            // A method or class field NAME declares something, so it is a free identifier; the
            // same token as the key of an object literal or after a `.` is a property.
            "property_identifier" | "private_property_identifier" => !matches!(
                (parent_kind, field),
                (
                    "method_definition"
                        | "public_field_definition"
                        | "abstract_method_signature"
                        | "method_signature",
                    Some("name")
                )
            ),
            "identifier" => {
                self.dialect == Dialect::Python
                    && matches!(
                        (parent_kind, field),
                        ("attribute", Some("attribute")) | ("keyword_argument", Some("name"))
                    )
            }
            _ => false,
        };
        if is_property {
            self.builder
                .push_split(TokenClass::Prop, TEXT_PROP, || text.to_string(), line, raw);
        } else {
            // The name of the function a call invokes (or the class `new` constructs) says what
            // the code does, so Exact mode keeps it: two copies that call different helpers do
            // not agree. A rename of a local, a parameter or a declaration still collapses.
            let callee = kind == "identifier"
                && matches!(
                    (parent_kind, field),
                    ("call_expression" | "call", Some("function"))
                        | ("new_expression", Some("constructor"))
                );
            self.builder.push_split(
                TokenClass::Ident,
                TEXT_IDENT,
                || {
                    if callee {
                        text.to_string()
                    } else {
                        TEXT_IDENT.to_string()
                    }
                },
                line,
                raw,
            );
        }
    }
}

fn node_has_interpolation(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| c.kind() == "interpolation");
    found
}

/// The collapsed class text of a binary operator, or `None` when `text` is not one.
fn binary_family(text: &str) -> Option<&'static str> {
    match text {
        "+" | "-" | "*" | "/" | "%" | "**" | "//" | "@" => Some(TEXT_ARITH),
        "==" | "===" | "!=" | "!==" | "<" | ">" | "<=" | ">=" | "<>" | "in" | "instanceof"
        | "is" | "is not" | "not in" => Some(TEXT_CMP),
        "&&" | "||" | "??" | "and" | "or" => Some(TEXT_LOGIC),
        "|" | "&" | "^" | "<<" | ">>" | ">>>" => Some(TEXT_BIT),
        _ => None,
    }
}

fn line_of(row: usize) -> u32 {
    u32::try_from(row.saturating_add(1)).unwrap_or(u32::MAX)
}

fn byte_range(range: std::ops::Range<usize>) -> std::ops::Range<u32> {
    let start = u32::try_from(range.start).unwrap_or(u32::MAX);
    let end = u32::try_from(range.end).unwrap_or(u32::MAX);
    start..end
}

/// Resolve the escapes whose meaning is only the character after the backslash, so `'it\'s'`
/// and `"it's"` are the same string. Every other escape stays verbatim.
fn simple_unescape(raw: &str, out: &mut String) {
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.clone().next() {
                Some(next @ ('\'' | '"' | '\\')) => {
                    out.push(next);
                    chars.next();
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
}

/// The Exact text of an ECMAScript string or template literal: its fragments with the quotes
/// removed, each substitution standing as `${}` (its tokens are emitted separately).
fn ecma_string_exact(node: Node<'_>, source: &str) -> String {
    let mut out = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_fragment" | "escape_sequence" => {
                simple_unescape(text_of(child, source), &mut out);
            }
            "template_substitution" => out.push_str("${}"),
            _ => {}
        }
    }
    out
}

/// The Exact text of a Python string: its content with prefix and quotes removed, each
/// interpolation standing as `{}`.
fn python_string_exact(node: Node<'_>, source: &str) -> String {
    let mut out = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_content" => simple_unescape(text_of(child, source), &mut out),
            "interpolation" => out.push_str("{}"),
            _ => {}
        }
    }
    out
}
