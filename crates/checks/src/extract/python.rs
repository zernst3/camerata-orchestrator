//! Python extractor: native `tree-sitter-python` grammar.
//!
//! # Relative-import specifier encoding
//!
//! Python's `from . import x` / `from ..pkg import y` relative-import forms have no
//! "specifier" string to lift verbatim (there's no literal like TS/JS's `'./x'`) — the
//! grammar represents them as a leading-dot COUNT plus an optional dotted module name. This
//! extractor renders that back into a leading-dot STRING so [`super::Import::specifier`]
//! stays a single uniform `String` field across every language: `from . import x` → specifier
//! `"."`; `from ..pkg import y` → specifier `"..pkg"`. The resolver (`extract::resolver`)
//! reads the leading-dot count back off this string to walk up the importing file's own
//! package tree — see that module's docs.
//!
//! # Never panics
//!
//! Same contract as [`super::ecma`]: `tree-sitter-python` always returns a tree (error nodes,
//! never a parse failure), and this walker only reads fields/kinds it has already matched.

use tree_sitter::{Node, Tree};

use super::ancestry::{walk_with_ancestors, Home, Pos};

use super::{
    canonical_number, ComputedLoad, FunctionSpan, Import, ImportKind, Literal, LiteralCtx,
    LiteralKind, MethodCall, ModuleConst,
};

pub(crate) fn parse(source: &str) -> Option<Tree> {
    // The walkers recurse once per nesting level; see `nesting`.
    if !super::nesting::parseable(super::SourceLang::Python, source) {
        return None;
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

fn start_line(node: Node) -> usize {
    node.start_position().row + 1
}

fn end_line(node: Node) -> usize {
    node.end_position().row + 1
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// Join a `dotted_name` node's `identifier` children with `.` (`os` → `"os"`,
/// `os.path` → `"os.path"`).
fn dotted_name_text(node: Node, source: &str) -> String {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "identifier")
        .map(|c| text(c, source))
        .collect::<Vec<_>>()
        .join(".")
}

/// Extract a python string literal's content (`(string (string_start) (string_content)
/// (string_end))`), or `""` for an empty literal with no `string_content` child.
fn string_literal_text<'s>(node: Node, source: &'s str) -> Option<&'s str> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return Some(text(child, source));
        }
    }
    Some("")
}

/// Decorators attach as CHILDREN of a `decorated_definition` wrapper (unlike TS/JS, where
/// they're preceding siblings) — collect them when `node`'s parent is that wrapper.
fn decorators_for(node: Node, source: &str) -> Vec<String> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    if parent.kind() != "decorated_definition" {
        return Vec::new();
    }
    let mut cursor = parent.walk();
    parent
        .children(&mut cursor)
        .filter(|c| c.kind() == "decorator")
        .map(|c| text(c, source).to_string())
        .collect()
}

// ─── imports ─────────────────────────────────────────────────────────────────────────────

pub fn imports(source: &str) -> Vec<Import> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_imports(tree.root_node(), source, &mut out);
    out
}

fn walk_imports(node: Node, source: &str, out: &mut Vec<Import>) {
    match node.kind() {
        "import_statement" => collect_plain_import(node, source, out),
        "import_from_statement" => collect_from_import(node, source, out),
        "call" => collect_import_module_call(node, source, out),
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_imports(child, source, out);
    }
}

/// `import a, b as c` — each `name:` field is its own whole-module (namespace) import.
fn collect_plain_import(node: Node, source: &str, out: &mut Vec<Import>) {
    let line = start_line(node);
    let mut cursor = node.walk();
    for name_node in node.children_by_field_name("name", &mut cursor) {
        let dotted = match name_node.kind() {
            "dotted_name" => Some(dotted_name_text(name_node, source)),
            "aliased_import" => name_node
                .child_by_field_name("name")
                .map(|n| dotted_name_text(n, source)),
            _ => None,
        };
        if let Some(specifier) = dotted {
            out.push(Import {
                specifier,
                names: Vec::new(),
                kind: ImportKind::Namespace,
                line,
                type_only: false,
            });
        }
    }
}

/// `from x import a, b as c` / `from . import x` / `from ..pkg import y` / `from x import *`.
fn collect_from_import(node: Node, source: &str, out: &mut Vec<Import>) {
    let Some(module_node) = node.child_by_field_name("module_name") else {
        return;
    };
    let specifier = match module_node.kind() {
        "dotted_name" => dotted_name_text(module_node, source),
        "relative_import" => {
            let mut cursor = module_node.walk();
            let mut s = String::new();
            for child in module_node.children(&mut cursor) {
                match child.kind() {
                    "import_prefix" => s.push_str(text(child, source)),
                    "dotted_name" => s.push_str(&dotted_name_text(child, source)),
                    _ => {}
                }
            }
            s
        }
        _ => return,
    };
    let line = start_line(node);
    let mut cursor = node.walk();
    let has_wildcard = node
        .children(&mut cursor)
        .any(|c| c.kind() == "wildcard_import");
    if has_wildcard {
        out.push(Import {
            specifier,
            names: Vec::new(),
            kind: ImportKind::Namespace,
            line,
            type_only: false,
        });
        return;
    }
    let mut cursor2 = node.walk();
    for name_node in node.children_by_field_name("name", &mut cursor2) {
        let origin = match name_node.kind() {
            "dotted_name" => Some(dotted_name_text(name_node, source)),
            "aliased_import" => name_node
                .child_by_field_name("name")
                .map(|n| dotted_name_text(n, source)),
            _ => None,
        };
        if let Some(name) = origin {
            out.push(Import {
                specifier: specifier.clone(),
                names: vec![name],
                kind: ImportKind::Named,
                line,
                type_only: false,
            });
        }
    }
}

/// `importlib.import_module('x.y')` (or any `<...>.import_module('x')` call, aliased
/// `importlib` included) — Python's closest analogue to a dynamic `import()`. Only a
/// string-literal argument is captured; anything computed is unresolvable and dropped.
fn collect_import_module_call(node: Node, source: &str, out: &mut Vec<Import>) {
    let Some(func) = node.child_by_field_name("function") else {
        return;
    };
    if func.kind() != "attribute" {
        return;
    }
    let Some(attr) = func.child_by_field_name("attribute") else {
        return;
    };
    if text(attr, source) != "import_module" {
        return;
    }
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    let Some(first_arg) = args.children(&mut cursor).find(|c| c.kind() == "string") else {
        return;
    };
    let Some(specifier) = string_literal_text(first_arg, source) else {
        return;
    };
    out.push(Import {
        specifier: specifier.to_string(),
        names: Vec::new(),
        kind: ImportKind::Dynamic,
        line: start_line(node),
        type_only: false,
    });
}

// ─── computed loads ──────────────────────────────────────────────────────────────────────

/// Every load the import graph cannot follow: `importlib.import_module(<non-string>)`,
/// `import_module(<non-string>)`, `__import__(<non-string>)`, and the discovery helpers
/// `pkgutil.iter_modules(..)`, `pkgutil.walk_packages(..)`,
/// `importlib.util.spec_from_file_location(..)` and `entry_points(..)`. A Python load names a
/// dotted module, not a path, so `prefix` is always `None`: the load could reach any module of
/// its package. See [`ComputedLoad`].
pub fn computed_loads(source: &str) -> Vec<ComputedLoad> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_computed_loads(tree.root_node(), source, &mut out);
    out
}

/// Callee names whose call is a computed load only when the module argument is not a plain
/// string literal.
const SPECIFIER_LOADERS: &[&str] = &["import_module", "__import__"];
/// Callee names that discover modules or entry points at runtime, whatever their arguments.
const DISCOVERY_LOADERS: &[&str] = &[
    "iter_modules",
    "walk_packages",
    "spec_from_file_location",
    "entry_points",
];

fn walk_computed_loads(node: Node, source: &str, out: &mut Vec<ComputedLoad>) {
    if node.kind() == "call" {
        if let Some(name) = callee_name(node, source) {
            let discovery = DISCOVERY_LOADERS.contains(&name);
            let computed_specifier = SPECIFIER_LOADERS.contains(&name)
                && !node
                    .child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0))
                    .is_some_and(|first| is_plain_string(first));
            if discovery || computed_specifier {
                out.push(ComputedLoad {
                    prefix: None,
                    line: start_line(node),
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_computed_loads(child, source, out);
    }
}

/// The last name of a call's callee: `import_module` for both `import_module(..)` and
/// `importlib.import_module(..)`.
fn callee_name<'s>(call: Node, source: &'s str) -> Option<&'s str> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some(text(func, source)),
        "attribute" => func
            .child_by_field_name("attribute")
            .map(|a| text(a, source)),
        _ => None,
    }
}

/// A string literal with no `{..}` interpolation (an f-string with substitutions is computed).
fn is_plain_string(node: Node) -> bool {
    if node.kind() != "string" {
        return false;
    }
    let mut cursor = node.walk();
    let interpolated = node
        .children(&mut cursor)
        .any(|c| c.kind() == "interpolation");
    !interpolated
}

// ─── functions ───────────────────────────────────────────────────────────────────────────

pub fn functions(source: &str) -> Vec<FunctionSpan> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_functions(tree.root_node(), source, &mut out);
    out
}

fn walk_functions(node: Node, source: &str, out: &mut Vec<FunctionSpan>) {
    if node.kind() == "function_definition" {
        let name = node
            .child_by_field_name("name")
            .map(|n| text(n, source).to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());
        out.push(FunctionSpan {
            name,
            start_line: start_line(node),
            end_line: end_line(node),
            attrs: decorators_for(node, source),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_functions(child, source, out);
    }
}

// ─── method calls ────────────────────────────────────────────────────────────────────────

pub fn method_calls(source: &str) -> Vec<MethodCall> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_calls(tree.root_node(), source, &mut out);
    out
}

fn walk_calls(node: Node, source: &str, out: &mut Vec<MethodCall>) {
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "attribute" {
                if let (Some(obj), Some(attr)) = (
                    func.child_by_field_name("object"),
                    func.child_by_field_name("attribute"),
                ) {
                    out.push(MethodCall {
                        receiver: render_attribute_path(obj, source),
                        method: text(attr, source).to_string(),
                        line: start_line(attr),
                    });
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, source, out);
    }
}

/// Render an `identifier` or an `attribute` chain (`self.db`, `a.b.c`) as a dotted string.
/// Anything richer (a call, a subscript, ...) renders as `"<expr>"`.
fn render_attribute_path(node: Node, source: &str) -> String {
    match node.kind() {
        "identifier" => text(node, source).to_string(),
        "attribute" => {
            let obj = node
                .child_by_field_name("object")
                .map(|n| render_attribute_path(n, source));
            let attr = node
                .child_by_field_name("attribute")
                .map(|n| text(n, source).to_string());
            match (obj, attr) {
                (Some(o), Some(a)) => format!("{o}.{a}"),
                _ => "<expr>".to_string(),
            }
        }
        _ => "<expr>".to_string(),
    }
}

// ─── literals ────────────────────────────────────────────────────────────────────────────

/// Node kinds the nearest-call walk passes through on its way up from a literal.
/// A `lambda` is deliberately absent: its body is not an argument of the call that receives it.
const CALLEE_PASS_THROUGH: &[&str] = &[
    "list",
    "tuple",
    "set",
    "dictionary",
    "pair",
    "parenthesized_expression",
    "list_splat",
    "dictionary_splat",
    "keyword_argument",
    "conditional_expression",
    "binary_operator",
    "boolean_operator",
    "not_operator",
    "await",
    "interpolation",
    "string",
    "expression_list",
];

/// Wrappers `value_of_key` looks through between a literal and the `pair` it is the value of.
const VALUE_WRAPPERS: &[&str] = &[
    "parenthesized_expression",
    "conditional_expression",
    "boolean_operator",
    "binary_operator",
    "list",
    "tuple",
    "set",
];

/// Wrappers a module-level initializer may pass through and still be "the constant".
const INITIALIZER_WRAPPERS: &[&str] = &[
    "dictionary",
    "list",
    "tuple",
    "set",
    "parenthesized_expression",
    "expression_list",
];

/// Base classes whose body assignments are enum members.
const ENUM_BASES: &[&str] = &[
    "Enum",
    "IntEnum",
    "StrEnum",
    "Flag",
    "IntFlag",
    "ReprEnum",
    // Django's enumeration types.
    "Choices",
    "TextChoices",
    "IntegerChoices",
];

/// Extract every string and number literal with its [`LiteralCtx`]. See
/// [`super::literals`] for what counts as a literal.
pub fn literals(source: &str) -> Vec<Literal> {
    let Some(tree) = parse(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_with_ancestors(tree.root_node(), |pos| {
        // Adjacent literals (`'a' 'b'`) concatenate at compile time; neither half is the
        // constant, and the joined text is not worth guessing.
        let inside_concatenation = pos
            .parent()
            .is_some_and(|p| p.node.kind() == "concatenated_string");
        if inside_concatenation {
            return;
        }
        if let Some(literal) = literal_at(pos, source) {
            out.push(literal);
        }
    });
    out
}

fn literal_at(pos: Pos, source: &str) -> Option<Literal> {
    let node = pos.node;
    if pos.under_error() {
        return None;
    }
    let (value, kind) = match node.kind() {
        "string" => (string_value(node, source)?, LiteralKind::Str),
        "integer" | "float" => (canonical_number(text(node, source))?, LiteralKind::Number),
        _ => return None,
    };
    Some(Literal {
        value,
        kind,
        line: start_line(node),
        ctx: literal_ctx(pos, source),
    })
}

/// A plain string literal's content: not a byte string, not interpolated. Raw strings keep
/// their backslashes; every other string has its escapes resolved.
fn string_value(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    if children.iter().any(|c| c.kind() == "interpolation") {
        return None;
    }
    let start = children.iter().find(|c| c.kind() == "string_start")?;
    let end = children.iter().rev().find(|c| c.kind() == "string_end")?;
    let prefix: String = text(*start, source)
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    if prefix.contains('b') {
        return None;
    }
    let content = source.get(start.end_byte()..end.start_byte())?;
    Some(if prefix.contains('r') {
        content.to_string()
    } else {
        unescape_python(content)
    })
}

/// Resolve Python string escapes. An unknown escape keeps its backslash, as Python does;
/// a line continuation vanishes; `\N{..}` names are kept verbatim (never guessed).
fn unescape_python(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(next) = chars.next() else {
            out.push('\\');
            break;
        };
        let simple = match next {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            '0' => Some('\0'),
            '\\' | '\'' | '"' => Some(next),
            _ => None,
        };
        if let Some(ch) = simple {
            out.push(ch);
            continue;
        }
        let digits = match next {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            '\n' => continue,
            _ => {
                out.push('\\');
                out.push(next);
                continue;
            }
        };
        let hex: String = (0..digits)
            .filter_map(|_| chars.next_if(char::is_ascii_hexdigit))
            .collect();
        match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
            Some(ch) if hex.len() == digits => out.push(ch),
            _ => {
                out.push('\\');
                out.push(next);
                out.push_str(&hex);
            }
        }
    }
    out
}

fn literal_ctx(pos: Pos, source: &str) -> LiteralCtx {
    let mut ctx = LiteralCtx::default();
    apply_call_context(pos, source, &mut ctx);
    apply_key_context(pos, source, &mut ctx);
    ctx.type_position = in_type_position(pos, source);
    ctx.attribute = has_ancestor(pos, "decorator");
    ctx.arithmetic = in_arithmetic_position(pos, source);
    module_const_of(pos, source).apply(&mut ctx);
    ctx
}

fn has_ancestor(pos: Pos, kind: &str) -> bool {
    let mut cur = pos.parent();
    while let Some(p) = cur {
        if p.node.kind() == kind {
            return true;
        }
        cur = p.parent();
    }
    false
}

/// Whether `child` is `parent`'s `field` child.
fn is_field_of(parent: Node, field: &str, child: Node) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|n| n.id() == child.id())
}

/// The first named child that is not a comment (a comment in an argument list is an extra,
/// not an argument).
fn first_named_non_comment(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| c.kind() != "comment");
    found
}

/// Fill `import_specifier`, `callee` and `callee_chain` from the nearest enclosing call, found
/// by walking up through [`CALLEE_PASS_THROUGH`] nodes.
fn apply_call_context(pos: Pos, source: &str, ctx: &mut LiteralCtx) {
    let mut cur = pos;
    while let Some(up) = cur.parent() {
        let parent = up.node;
        match parent.kind() {
            "argument_list" => {
                let Some(call) = up.parent().map(|c| c.node).filter(|c| c.kind() == "call") else {
                    return;
                };
                let Some(func) = call.child_by_field_name("function") else {
                    return;
                };
                let name = match func.kind() {
                    "identifier" => Some(text(func, source).to_string()),
                    "attribute" => func
                        .child_by_field_name("attribute")
                        .map(|a| text(a, source).to_string()),
                    _ => None,
                };
                if let Some(name) = name.filter(|n| !n.is_empty()) {
                    let first =
                        first_named_non_comment(parent).is_some_and(|c| c.id() == cur.node.id());
                    ctx.import_specifier = first
                        && cur.node.id() == pos.node.id()
                        && matches!(name.as_str(), "import_module" | "__import__");
                    ctx.callee_chain = Some(
                        text(func, source)
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect(),
                    );
                    ctx.callee = Some(name);
                }
                return;
            }
            kind if CALLEE_PASS_THROUGH.contains(&kind) => cur = up,
            _ => return,
        }
    }
}

/// Fill `object_key`, `value_of_key`, `subscript` and `subscript_receiver`.
fn apply_key_context(pos: Pos, source: &str, ctx: &mut LiteralCtx) {
    let node = pos.node;
    let Some(parent) = pos.parent().map(|p| p.node) else {
        return;
    };
    match parent.kind() {
        "pair" => ctx.object_key = is_field_of(parent, "key", node),
        "subscript" => {
            ctx.subscript = is_field_of(parent, "subscript", node);
            if ctx.subscript {
                ctx.subscript_receiver = parent
                    .child_by_field_name("value")
                    .map(|v| text(v, source).to_string());
            }
        }
        _ => {}
    }
    if ctx.object_key {
        return;
    }
    let mut cur = pos;
    while let Some(up) = cur.parent() {
        let p = up.node;
        if p.kind() == "pair" {
            if is_field_of(p, "value", cur.node) {
                ctx.value_of_key = p
                    .child_by_field_name("key")
                    .and_then(|k| key_text(k, source));
                // `attrs={'autocomplete': '..'}` is a Django widget's HTML attributes.
                let widget_attrs = up
                    .parent()
                    .filter(|d| d.node.kind() == "dictionary")
                    .and_then(|d| d.parent())
                    .is_some_and(|kw| {
                        kw.node.kind() == "keyword_argument"
                            && kw
                                .node
                                .child_by_field_name("name")
                                .is_some_and(|n| text(n, source) == "attrs")
                    });
                if widget_attrs {
                    ctx.jsx_attr = ctx.value_of_key.clone();
                }
            }
            return;
        }
        if VALUE_WRAPPERS.contains(&p.kind()) {
            cur = up;
        } else {
            return;
        }
    }
}

fn key_text(key: Node, source: &str) -> Option<String> {
    match key.kind() {
        "identifier" | "integer" => Some(text(key, source).to_string()),
        "string" => string_value(key, source),
        _ => None,
    }
}

/// Inside `Literal[...]` or on the right of an assignment in an `Enum` class body.
fn in_type_position(pos: Pos, source: &str) -> bool {
    let mut cur = pos.parent();
    while let Some(up) = cur {
        let p = up.node;
        // `Literal[..]` is a `subscript` in an expression and a `generic_type` in an annotation.
        let subject = match p.kind() {
            "subscript" => p.child_by_field_name("value"),
            "generic_type" => p.named_child(0),
            _ => None,
        };
        if subject.is_some_and(|v| last_segment(text(v, source)) == "Literal") {
            return true;
        }
        cur = up.parent();
    }
    in_enum_body(pos, source)
}

fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path).trim()
}

fn in_enum_body(pos: Pos, source: &str) -> bool {
    let mut cur = pos;
    // The initializer: the literal itself or inside a tuple/parenthesized value.
    while let Some(up) = cur.parent() {
        if INITIALIZER_WRAPPERS.contains(&up.node.kind()) {
            cur = up;
        } else {
            break;
        }
    }
    let Some(assignment) = cur
        .parent()
        .filter(|a| a.node.kind() == "assignment" && is_field_of(a.node, "right", cur.node))
    else {
        return false;
    };
    let class = assignment
        .parent()
        .filter(|s| s.node.kind() == "expression_statement")
        .and_then(|s| s.parent())
        .filter(|b| b.node.kind() == "block")
        .and_then(|b| b.parent())
        .map(|c| c.node)
        .filter(|c| c.kind() == "class_definition");
    let Some(bases) = class.and_then(|c| c.child_by_field_name("superclasses")) else {
        return false;
    };
    let mut cursor = bases.walk();
    let found = bases
        .named_children(&mut cursor)
        .any(|b| ENUM_BASES.contains(&last_segment(text(b, source))));
    found
}

fn in_arithmetic_position(pos: Pos, source: &str) -> bool {
    let mut cur = pos;
    while let Some(p) = cur
        .parent()
        .filter(|p| p.node.kind() == "parenthesized_expression")
    {
        cur = p;
    }
    let Some(parent) = cur.parent().map(|p| p.node) else {
        return false;
    };
    let cur = cur.node;
    let operator = parent
        .child_by_field_name("operator")
        .map(|o| text(o, source))
        .unwrap_or_default();
    match parent.kind() {
        "binary_operator" => {
            // `'rate ' + 0.12` is string concatenation, not arithmetic.
            let concatenation = operator == "+"
                && ["left", "right"]
                    .iter()
                    .filter_map(|f| parent.child_by_field_name(f))
                    .any(|side| {
                        side.id() != cur.id()
                            && matches!(side.kind(), "string" | "concatenated_string")
                    });
            matches!(operator, "+" | "-" | "*" | "/" | "%" | "//") && !concatenation
        }
        "augmented_assignment" => {
            matches!(operator, "+=" | "-=" | "*=" | "/=" | "%=" | "//=")
                && is_field_of(parent, "right", cur)
        }
        _ => false,
    }
}

/// The `NAME = ...` / `NAME: T = ...` whose value is, or contains (through
/// [`INITIALIZER_WRAPPERS`] only), this literal: a module-level constant, or an attribute in
/// the body of a module-level class (`Class.NAME`). A class body anywhere else is unattributed.
fn module_const_of(pos: Pos, source: &str) -> Home {
    let mut cur = pos;
    let assignment_pos = loop {
        let Some(up) = cur.parent() else {
            return Home::None;
        };
        let parent = up.node;
        match parent.kind() {
            "assignment" if is_field_of(parent, "right", cur.node) => break up,
            "pair" if is_field_of(parent, "value", cur.node) => cur = up,
            kind if INITIALIZER_WRAPPERS.contains(&kind) => cur = up,
            _ => return Home::None,
        }
    };
    let assignment = assignment_pos.node;
    let Some(name) = assignment.child_by_field_name("left") else {
        return Home::None;
    };
    if name.kind() != "identifier" || assignment.has_error() {
        return Home::None;
    }
    let name = text(name, source).to_string();
    let Some(container) = assignment_pos
        .parent()
        .filter(|s| s.node.kind() == "expression_statement")
        .and_then(|s| s.parent())
    else {
        return Home::None;
    };
    match container.node.kind() {
        "module" => Home::Const(ModuleConst {
            exported: !name.starts_with('_'),
            name,
        }),
        "block" => class_attribute_home(container, &name, source),
        _ => Home::None,
    }
}

/// `block` is a class body when its parent is a `class_definition`; anything else (a function,
/// an `if`) is ordinary code.
fn class_attribute_home(block: Pos, attribute: &str, source: &str) -> Home {
    let Some(class) = block
        .parent()
        .filter(|c| c.node.kind() == "class_definition")
    else {
        return Home::None;
    };
    let Some(class_name) = class.node.child_by_field_name("name") else {
        return Home::Unattributed;
    };
    // A decorated class sits under a `decorated_definition` wrapper.
    let holder = class
        .parent()
        .filter(|p| p.node.kind() == "decorated_definition")
        .or_else(|| class.parent());
    let at_module_level = match holder {
        Some(h) if h.node.kind() == "module" => true,
        Some(h) if h.node.kind() == "decorated_definition" => {
            h.parent().is_some_and(|m| m.node.kind() == "module")
        }
        _ => false,
    };
    if !at_module_level {
        return Home::Unattributed;
    }
    Home::Const(ModuleConst {
        name: format!("{}.{attribute}", text(class_name, source)),
        exported: !attribute.starts_with('_'),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── imports ───────────────────────────────────────────────────────────────

    #[test]
    fn plain_import_and_aliased() {
        let vs = imports("import os\nimport os.path as p\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs
            .iter()
            .any(|i| i.specifier == "os" && i.kind == ImportKind::Namespace));
        assert!(vs.iter().any(|i| i.specifier == "os.path"));
    }

    #[test]
    fn import_multiple_on_one_line() {
        let vs = imports("import a, b as c\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().any(|i| i.specifier == "a"));
        assert!(vs.iter().any(|i| i.specifier == "b")); // origin name, not alias `c`
    }

    #[test]
    fn from_import_named_with_alias() {
        let vs = imports("from x import (a, b as c)\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs
            .iter()
            .all(|i| i.specifier == "x" && i.kind == ImportKind::Named));
        assert!(vs.iter().any(|i| i.names == vec!["a".to_string()]));
        assert!(vs.iter().any(|i| i.names == vec!["b".to_string()])); // origin, not `c`
    }

    #[test]
    fn from_import_wildcard() {
        let vs = imports("from x import *\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "x");
        assert_eq!(vs[0].kind, ImportKind::Namespace);
        assert!(vs[0].names.is_empty());
    }

    #[test]
    fn relative_import_single_dot_no_module() {
        let vs = imports("from . import sibling\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, ".");
        assert_eq!(vs[0].names, vec!["sibling"]);
    }

    #[test]
    fn relative_import_double_dot_with_module() {
        let vs = imports("from ..pkg import thing\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "..pkg");
        assert_eq!(vs[0].names, vec!["thing"]);
    }

    #[test]
    fn importlib_import_module_literal() {
        let vs = imports("import importlib\nimportlib.import_module('x.y')\n");
        assert!(
            vs.iter()
                .any(|i| i.specifier == "x.y" && i.kind == ImportKind::Dynamic),
            "{vs:#?}"
        );
    }

    #[test]
    fn importlib_import_module_variable_arg_is_dropped() {
        let vs = imports("importlib.import_module(name_var)\n");
        assert!(!vs.iter().any(|i| i.kind == ImportKind::Dynamic), "{vs:#?}");
    }

    // ── functions ─────────────────────────────────────────────────────────────

    #[test]
    fn decorated_method_captures_decorator_and_name() {
        let src =
            "class Foo:\n    @app.route('/x')\n    def method(self):\n        self.db.query()\n";
        let fs = functions(src);
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "method");
        assert_eq!(fs[0].attrs, vec!["@app.route('/x')".to_string()]);
    }

    #[test]
    fn nested_function_is_found() {
        let src = "def outer():\n    def inner():\n        pass\n    return inner\n";
        let fs = functions(src);
        assert_eq!(fs.len(), 2, "{fs:#?}");
        assert!(fs.iter().any(|f| f.name == "outer"));
        assert!(fs.iter().any(|f| f.name == "inner"));
    }

    #[test]
    fn async_def_is_found() {
        let fs = functions("async def handler(req):\n    pass\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "handler");
    }

    // ── method calls ──────────────────────────────────────────────────────────

    #[test]
    fn self_attribute_chain_receiver() {
        let cs = method_calls("class Foo:\n    def m(self):\n        self.db.query()\n");
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "self.db");
        assert_eq!(cs[0].method, "query");
    }

    #[test]
    fn bare_identifier_receiver() {
        let cs = method_calls("db.execute()\n");
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "db");
        assert_eq!(cs[0].method, "execute");
    }

    // ── adversarial: malformed / truncated / binary-ish input must never panic ─

    #[test]
    fn truncated_source_does_not_panic() {
        let src = "def broken(:\n  ???";
        let _ = imports(src);
        let _ = functions(src);
        let _ = method_calls(src);
    }

    #[test]
    fn binary_like_content_does_not_panic() {
        let weird = "\u{0}\u{1}\u{FFFD} not python at all \u{FFFD} :::";
        let _ = imports(weird);
        let _ = functions(weird);
        let _ = method_calls(weird);
    }

    #[test]
    fn empty_source_does_not_panic() {
        assert!(imports("").is_empty());
        assert!(functions("").is_empty());
        assert!(method_calls("").is_empty());
    }

    #[test]
    fn deeply_nested_indentation_does_not_stack_overflow() {
        let mut src = String::new();
        for i in 0..80 {
            src.push_str(&" ".repeat(i * 4));
            src.push_str("if True:\n");
        }
        src.push_str(&" ".repeat(80 * 4));
        src.push_str("def leaf():\n");
        src.push_str(&" ".repeat(80 * 4 + 4));
        src.push_str("self.a.b()\n");
        let cs = method_calls(&src);
        assert!(cs.iter().any(|c| c.method == "b"));
    }

    // ── literals ──────────────────────────────────────────────────────────────

    use crate::extract::{Literal, LiteralCtx, LiteralKind, ModuleConst};

    fn lits(src: &str) -> Vec<Literal> {
        literals(src)
    }

    /// The single literal whose value is `value`; panics when there is not exactly one.
    fn lit(src: &str, value: &str) -> Literal {
        let all = lits(src);
        let mut hits: Vec<Literal> = all.iter().filter(|l| l.value == value).cloned().collect();
        assert_eq!(hits.len(), 1, "{value:?} in {all:#?}");
        hits.remove(0)
    }

    fn ctx(src: &str, value: &str) -> LiteralCtx {
        lit(src, value).ctx
    }

    #[test]
    fn literal_strings_carry_value_kind_and_line() {
        let all = lits("a = 'orders-high'\n\nfoo(\"x:y\")\n");
        assert_eq!(all.len(), 2, "{all:#?}");
        assert_eq!(all[0].value, "orders-high");
        assert_eq!(all[0].kind, LiteralKind::Str);
        assert_eq!(all[0].line, 1);
        assert_eq!(all[1].value, "x:y");
        assert_eq!(all[1].line, 3);
    }

    #[test]
    fn literal_escapes_are_resolved_and_raw_strings_are_not() {
        let src = "f('a\\nb', 'it\\'s', r'a\\nb', 'tab\\there', '\\x41\\u0042', 'back\\\\slash', '\\d')\n";
        let values: Vec<String> = lits(src).into_iter().map(|l| l.value).collect();
        assert_eq!(
            values,
            [
                "a\nb",
                "it's",
                "a\\nb",
                "tab\there",
                "AB",
                "back\\slash",
                "\\d"
            ]
        );
    }

    #[test]
    fn literal_triple_quoted_and_empty_strings() {
        let all = lits("f('', \"\"\"orders-high\"\"\")\n");
        let values: Vec<&str> = all.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(values, ["", "orders-high"]);
    }

    #[test]
    fn literal_interpolated_f_strings_are_skipped_but_plain_ones_are_not() {
        let all = lits("a = f'/api/{v}'\nb = f'orders-high'\nc = f\"{d['x-y']}\"\n");
        let values: Vec<&str> = all.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(values, ["orders-high", "x-y"], "{all:#?}");
    }

    #[test]
    fn literal_byte_strings_and_adjacent_concatenations_are_skipped() {
        let all = lits("a = b'orders-high'\nb = 'orders-' 'high'\nc = ('orders-'\n     'high')\n");
        assert!(all.is_empty(), "{all:#?}");
    }

    #[test]
    fn literal_numbers_are_canonical_and_non_decimal_spellings_are_skipped() {
        let src = "a(.12, 0.120, 1.2e-1, 1_0, 0x1F, 0o17, 0b11, 3j)\n";
        let values: Vec<(String, LiteralKind)> =
            lits(src).into_iter().map(|l| (l.value, l.kind)).collect();
        let n = LiteralKind::Number;
        assert_eq!(
            values,
            [
                ("0.12".to_string(), n),
                ("0.12".to_string(), n),
                ("0.12".to_string(), n),
                ("10".to_string(), n)
            ]
        );
    }

    // ctx.import_specifier

    #[test]
    fn ctx_import_specifier_import_module_first_argument() {
        for src in [
            "x = importlib.import_module('app.orders')\n",
            "x = import_module('app.orders')\n",
            "x = __import__('app.orders')\n",
        ] {
            assert!(ctx(src, "app.orders").import_specifier, "{src}");
        }
    }

    #[test]
    fn ctx_import_specifier_is_false_for_other_calls_and_later_arguments() {
        assert!(!ctx("x = load('app.orders')\n", "app.orders").import_specifier);
        assert!(!ctx("x = import_module('a', 'app.orders')\n", "app.orders").import_specifier);
        assert!(!ctx("x = 'app.orders'\n", "app.orders").import_specifier);
    }

    // ctx.callee, ctx.callee_chain

    #[test]
    fn ctx_callee_and_chain() {
        let c = ctx("x = os.environ.get('X_Y')\n", "X_Y");
        assert_eq!(c.callee.as_deref(), Some("get"));
        assert_eq!(c.callee_chain.as_deref(), Some("os.environ.get"));
        let c = ctx("log = logging.getLogger('svc-orders')\n", "svc-orders");
        assert_eq!(c.callee.as_deref(), Some("getLogger"));
        let c = ctx("x = _('errors-required')\n", "errors-required");
        assert_eq!(c.callee.as_deref(), Some("_"));
        assert_eq!(c.callee_chain.as_deref(), Some("_"));
    }

    #[test]
    fn ctx_callee_walks_up_through_containers_keywords_and_operators() {
        for src in [
            "cn(['items-center'])\n",
            "cn(('items-center',))\n",
            "cn({'a': 'items-center'})\n",
            "cn(k='items-center')\n",
            "cn(*['items-center'])\n",
            "cn(**{'k': 'items-center'})\n",
            "cn('x-y' if c else 'items-center')\n",
            "cn(c and 'items-center')\n",
            "cn(c or 'items-center')\n",
            "cn(('items-center'))\n",
            "cn(f\"{'items-center'}\")\n",
            "cn({'items-center'})\n",
        ] {
            assert_eq!(
                ctx(src, "items-center").callee.as_deref(),
                Some("cn"),
                "{src}"
            );
        }
    }

    #[test]
    fn ctx_callee_is_the_nearest_call_and_stops_at_a_lambda() {
        let c = ctx("outer(inner('svc-orders'))\n", "svc-orders");
        assert_eq!(c.callee.as_deref(), Some("inner"));
        assert_eq!(
            ctx("foo(lambda: 'svc-orders')\n", "svc-orders").callee,
            None
        );
    }

    #[test]
    fn ctx_callee_is_none_outside_any_call_and_for_computed_callees() {
        assert_eq!(ctx("a = 'svc-orders'\n", "svc-orders").callee, None);
        let c = ctx("get_thing()('svc-orders')\n", "svc-orders");
        assert_eq!(c.callee, None);
        assert_eq!(c.callee_chain, None);
    }

    #[test]
    fn ctx_callee_ignores_a_class_base_list() {
        let c = ctx("class A('svc-orders'):\n    pass\n", "svc-orders");
        assert_eq!(c.callee, None);
    }

    // ctx.subscript

    #[test]
    fn ctx_subscript_with_receiver() {
        let c = ctx("x = os.environ['X_Y']\n", "X_Y");
        assert!(c.subscript);
        assert_eq!(c.subscript_receiver.as_deref(), Some("os.environ"));
        let c = ctx("x = row[\"a-b\"]\n", "a-b");
        assert_eq!(c.subscript_receiver.as_deref(), Some("row"));
    }

    #[test]
    fn ctx_subscript_is_false_for_lists_and_the_receiver_position() {
        assert!(!ctx("x = ['a-b']\n", "a-b").subscript);
        assert!(!ctx("x = foo['a-b'.upper()]\n", "a-b").subscript);
    }

    // ctx.object_key, ctx.value_of_key

    #[test]
    fn ctx_object_key_for_dict_keys() {
        let c = ctx("d = {'x-tenant': 1}\n", "x-tenant");
        assert!(c.object_key);
        assert_eq!(c.value_of_key, None);
    }

    #[test]
    fn ctx_value_of_key_names_the_key() {
        let c = ctx("d = {'justify': 'space-between'}\n", "space-between");
        assert_eq!(c.value_of_key.as_deref(), Some("justify"));
        assert!(!c.object_key);
        let c = ctx("d = {env: 'A_B'}\n", "A_B");
        assert_eq!(c.value_of_key.as_deref(), Some("env"));
        let c = ctx("d = {'env': 'a-b' if c else 'c-d'}\n", "a-b");
        assert_eq!(c.value_of_key.as_deref(), Some("env"));
        let c = ctx("d = {'env': ['a-b']}\n", "a-b");
        assert_eq!(c.value_of_key.as_deref(), Some("env"));
    }

    #[test]
    fn ctx_value_of_key_does_not_cross_a_call_or_a_lambda() {
        assert_eq!(ctx("d = {'env': f('a-b')}\n", "a-b").value_of_key, None);
        assert_eq!(
            ctx("d = {'env': lambda: 'a-b'}\n", "a-b").value_of_key,
            None
        );
    }

    // ctx.type_position

    #[test]
    fn ctx_type_position_literal_subscripts() {
        for src in [
            "Status = Literal['IN_PROGRESS']\n",
            "Status = typing.Literal['IN_PROGRESS', 'DONE']\n",
            "def f(s: Literal['IN_PROGRESS']) -> None: ...\n",
            "def f() -> Literal['IN_PROGRESS', 'DONE']: ...\n",
            "x: Optional[Literal['IN_PROGRESS']] = None\n",
            "x: t.Literal['IN_PROGRESS'] = 'DONE'\n",
        ] {
            assert!(ctx(src, "IN_PROGRESS").type_position, "{src}");
        }
    }

    #[test]
    fn ctx_type_position_enum_class_bodies() {
        for base in [
            "Enum",
            "enum.Enum",
            "IntEnum",
            "StrEnum",
            "Flag",
            "str, Enum",
        ] {
            let src = format!("class Status({base}):\n    IN_PROGRESS = 'IN_PROGRESS'\n");
            assert!(
                lits(&src)
                    .iter()
                    .any(|l| l.value == "IN_PROGRESS" && l.ctx.type_position),
                "{src}"
            );
        }
        let src =
            "class Status(Enum):\n    IN_PROGRESS = ('IN_PROGRESS', 1)\n    DONE: str = 'DONE'\n";
        assert!(ctx(src, "IN_PROGRESS").type_position);
        assert!(ctx(src, "DONE").type_position);
    }

    #[test]
    fn ctx_type_position_is_false_for_value_uses_and_ordinary_classes() {
        for src in [
            "if s == 'IN_PROGRESS':\n    pass\n",
            "x = 'IN_PROGRESS'\n",
            "class Status:\n    IN_PROGRESS = 'IN_PROGRESS'\n",
            "class Status(Base):\n    IN_PROGRESS = 'IN_PROGRESS'\n",
            "class Status(Enum):\n    def f(self):\n        return 'IN_PROGRESS'\n",
        ] {
            assert!(!ctx(src, "IN_PROGRESS").type_position, "{src}");
        }
    }

    // ctx.attribute

    #[test]
    fn ctx_attribute_for_decorator_expressions_only() {
        let src = "@app.route('/api/v1/orders')\ndef f():\n    return '/api/v1/orders2'\n";
        assert!(ctx(src, "/api/v1/orders").attribute);
        assert!(!ctx(src, "/api/v1/orders2").attribute);
        let src = "@pytest.mark.skipif(sys.platform == 'win-32', reason='x')\ndef f(): pass\n";
        assert!(ctx(src, "win-32").attribute);
        assert!(!ctx("x = f('win-32')\n", "win-32").attribute);
    }

    // ctx.arithmetic

    #[test]
    fn ctx_arithmetic_for_binary_operators_and_compound_assignment() {
        for src in [
            "x = total * 0.12\n",
            "x = 0.12 * total\n",
            "x = total / 0.12\n",
            "x = total + 0.12\n",
            "x = total - 0.12\n",
            "x = total % 0.12\n",
            "x = total // 0.12\n",
            "x = total * (0.12)\n",
            "x += 0.12\n",
            "x -= 0.12\n",
            "x *= 0.12\n",
            "x /= 0.12\n",
            "x //= 0.12\n",
            "x %= 0.12\n",
        ] {
            assert!(ctx(src, "0.12").arithmetic, "{src}");
        }
    }

    #[test]
    fn ctx_arithmetic_is_false_elsewhere() {
        for src in [
            "x < 0.12\n",
            "x == 0.12\n",
            "foo(0.12)\n",
            "x = 0.12\n",
            "d = {'opacity': 0.12}\n",
            "x = [0.12]\n",
            "x = total ** 0.12\n",
            "x = -0.12\n",
        ] {
            assert!(!ctx(src, "0.12").arithmetic, "{src}");
        }
    }

    // ctx.module_const

    fn mc(name: &str, exported: bool) -> Option<ModuleConst> {
        Some(ModuleConst {
            name: name.to_string(),
            exported,
        })
    }

    #[test]
    fn ctx_module_const_plain_final_and_annotated_assignments() {
        assert_eq!(
            ctx("Q = 'orders-high'\n", "orders-high").module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx("Q: Final = 'orders-high'\n", "orders-high").module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx("Q: Final[str] = 'orders-high'\n", "orders-high").module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx("_Q = 'orders-high'\n", "orders-high").module_const,
            mc("_Q", false)
        );
    }

    #[test]
    fn ctx_module_const_through_containers() {
        assert_eq!(
            ctx("ROUTES = {'orders': '/api/orders'}\n", "/api/orders").module_const,
            mc("ROUTES", true)
        );
        assert_eq!(
            ctx("QUEUES = ['orders-high', 'orders-low']\n", "orders-high").module_const,
            mc("QUEUES", true)
        );
        assert_eq!(
            ctx("QUEUES = ('orders-high',)\n", "orders-high").module_const,
            mc("QUEUES", true)
        );
        assert_eq!(
            ctx("Q = ('orders-high')\n", "orders-high").module_const,
            mc("Q", true)
        );
    }

    #[test]
    fn ctx_module_const_is_none_behind_a_call_or_when_not_module_level() {
        for src in [
            "Q = f('orders-high')\n",
            "Q = {'a': g('orders-high')}\n",
            "Q = lambda: 'orders-high'\n",
            "def f():\n    Q = 'orders-high'\n",
            "if x:\n    Q = 'orders-high'\n",
            "a = b = 'orders-high'\n",
            "a, b = 'orders-high', 'x'\n",
            "self.q = 'orders-high'\n",
            "q['k'] = 'orders-high'\n",
        ] {
            assert_eq!(ctx(src, "orders-high").module_const, None, "{src}");
        }
    }

    #[test]
    fn literal_malformed_input_does_not_panic() {
        for src in [
            "a = '",
            "a = f'{",
            "def (:\n  x = 'a-b'",
            "\u{0}\u{1}\u{2}",
            "class A(:\n",
            "x = 1_",
            "",
        ] {
            let _ = lits(src);
        }
    }

    // ── more home shapes ──────────────────────────────────────────────────────

    #[test]
    fn ctx_module_const_class_body_assignments_of_a_module_level_class() {
        let c = ctx("class Queues:\n    ORDERS = 'orders-high'\n", "orders-high");
        assert_eq!(c.module_const, mc("Queues.ORDERS", true));
        assert!(!c.unattributed);
        assert_eq!(
            ctx(
                "class Queues:\n    ORDERS: str = 'orders-high'\n",
                "orders-high"
            )
            .module_const,
            mc("Queues.ORDERS", true)
        );
        assert_eq!(
            ctx(
                "class Queues:\n    _ORDERS = ('orders-high',)\n",
                "orders-high"
            )
            .module_const,
            mc("Queues._ORDERS", false)
        );
        assert_eq!(
            ctx(
                "@dataclass\nclass Queues:\n    ORDERS = 'orders-high'\n",
                "orders-high"
            )
            .module_const,
            mc("Queues.ORDERS", true)
        );
        assert_eq!(
            ctx(
                "class Queues(Base):\n    ORDERS = {'a': 'orders-high'}\n",
                "orders-high"
            )
            .module_const,
            mc("Queues.ORDERS", true)
        );
    }

    #[test]
    fn ctx_class_body_assignments_elsewhere_are_unattributed_not_bare() {
        for src in [
            "class A:\n    class B:\n        Q = 'orders-high'\n",
            "def f():\n    class C:\n        Q = 'orders-high'\n",
            "if x:\n    class C:\n        Q = 'orders-high'\n",
        ] {
            let c = ctx(src, "orders-high");
            assert_eq!(c.module_const, None, "{src}");
            assert!(c.unattributed, "{src}");
        }
        // Method bodies and `self` attributes are ordinary code.
        for src in [
            "class A:\n    def f(self):\n        q = 'orders-high'\n",
            "class A:\n    def __init__(self):\n        self.q = 'orders-high'\n",
        ] {
            let c = ctx(src, "orders-high");
            assert_eq!(c.module_const, None, "{src}");
            assert!(!c.unattributed, "{src}");
        }
    }

    #[test]
    fn ctx_type_position_django_choices_classes() {
        for base in [
            "models.TextChoices",
            "TextChoices",
            "models.IntegerChoices",
            "Choices",
        ] {
            let src =
                format!("class Status({base}):\n    IN_PROGRESS = 'in-progress', 'In progress'\n");
            let c = ctx(&src, "in-progress");
            assert!(c.type_position, "{src}");
            assert_eq!(c.module_const, mc("Status.IN_PROGRESS", true), "{src}");
        }
    }

    #[test]
    fn ctx_first_argument_skips_comments() {
        let src =
            "import importlib\nm = importlib.import_module(\n    # plugin\n    'plugins-core')\n";
        assert!(ctx(src, "plugins-core").import_specifier);
        let src = "m = import_module('x-y',\n    # c\n    'plugins-core')\n";
        assert!(!ctx(src, "plugins-core").import_specifier);
    }

    #[test]
    fn ctx_arithmetic_is_false_for_string_concatenation() {
        for src in [
            "m = 'rate ' + 0.12\n",
            "m = 0.12 + ' rate'\n",
            "m = f'{x}' + 0.12\n",
            "m = 'a' 'b' + 0.12\n",
        ] {
            assert!(!ctx(src, "0.12").arithmetic, "{src}");
        }
        for src in ["m = total + 0.12\n", "m = 'a' * 0.12\n", "m = 1 + 0.12\n"] {
            assert!(ctx(src, "0.12").arithmetic, "{src}");
        }
    }

    #[test]
    fn ctx_jsx_attr_names_the_widget_attribute_in_a_django_attrs_dict() {
        let src = "pw = forms.CharField(widget=forms.PasswordInput(attrs={'autocomplete': 'current-password'}))\n";
        assert_eq!(
            ctx(src, "current-password").jsx_attr.as_deref(),
            Some("autocomplete")
        );
        let src = "w = Select(attrs={'class': 'form-control', 'data-x': 'a-b'})\n";
        assert_eq!(ctx(src, "form-control").jsx_attr.as_deref(), Some("class"));
        // Other keyword arguments and dicts are not HTML attributes.
        assert_eq!(ctx("w = make(opts={'k': 'a-b'})\n", "a-b").jsx_attr, None);
        assert_eq!(ctx("w = {'k': 'a-b'}\n", "a-b").jsx_attr, None);
    }

    // ── computed loads ────────────────────────────────────────────────────────

    fn load_lines(src: &str) -> Vec<usize> {
        computed_loads(src).iter().map(|l| l.line).collect()
    }

    #[test]
    fn import_module_of_a_non_literal_is_a_load() {
        assert_eq!(load_lines("importlib.import_module(name)\n"), [1]);
        assert_eq!(load_lines("\nimport_module(name)\n"), [2]);
        assert_eq!(load_lines("__import__(name)\n"), [1]);
        assert_eq!(load_lines("importlib.import_module(f\"app.{x}\")\n"), [1]);
        assert!(computed_loads("importlib.import_module(name)\n")
            .iter()
            .all(|l| l.prefix.is_none()));
    }

    #[test]
    fn import_module_of_a_literal_is_an_edge_not_a_computed_load() {
        assert!(load_lines("importlib.import_module('x.y')\n__import__('os')\n").is_empty());
    }

    #[test]
    fn discovery_helpers_are_loads_whatever_their_arguments() {
        let src = "pkgutil.iter_modules(__path__)\n\
                   pkgutil.walk_packages(path)\n\
                   importlib.util.spec_from_file_location(n, p)\n\
                   entry_points(group='x')\n\
                   metadata.entry_points()\n";
        assert_eq!(load_lines(src), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn unrelated_python_calls_are_not_loads() {
        assert!(load_lines("print(name)\nobj.iter_modules_extra(x)\nimport os\n").is_empty());
    }

    #[test]
    fn python_computed_loads_never_panic() {
        assert!(computed_loads("").is_empty());
        let _ = computed_loads("importlib.import_module(\n\u{0}\u{FFFD}");
    }
}
