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

use super::{FunctionSpan, Import, ImportKind, MethodCall};

fn parse(source: &str) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).ok()?;
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
            out.push(Import { specifier, names: Vec::new(), kind: ImportKind::Namespace, line });
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
    let has_wildcard = node.children(&mut cursor).any(|c| c.kind() == "wildcard_import");
    if has_wildcard {
        out.push(Import { specifier, names: Vec::new(), kind: ImportKind::Namespace, line });
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
    });
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
                if let (Some(obj), Some(attr)) =
                    (func.child_by_field_name("object"), func.child_by_field_name("attribute"))
                {
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
            let obj = node.child_by_field_name("object").map(|n| render_attribute_path(n, source));
            let attr = node.child_by_field_name("attribute").map(|n| text(n, source).to_string());
            match (obj, attr) {
                (Some(o), Some(a)) => format!("{o}.{a}"),
                _ => "<expr>".to_string(),
            }
        }
        _ => "<expr>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── imports ───────────────────────────────────────────────────────────────

    #[test]
    fn plain_import_and_aliased() {
        let vs = imports("import os\nimport os.path as p\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().any(|i| i.specifier == "os" && i.kind == ImportKind::Namespace));
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
        assert!(vs.iter().all(|i| i.specifier == "x" && i.kind == ImportKind::Named));
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
        assert!(vs.iter().any(|i| i.specifier == "x.y" && i.kind == ImportKind::Dynamic), "{vs:#?}");
    }

    #[test]
    fn importlib_import_module_variable_arg_is_dropped() {
        let vs = imports("importlib.import_module(name_var)\n");
        assert!(!vs.iter().any(|i| i.kind == ImportKind::Dynamic), "{vs:#?}");
    }

    // ── functions ─────────────────────────────────────────────────────────────

    #[test]
    fn decorated_method_captures_decorator_and_name() {
        let src = "class Foo:\n    @app.route('/x')\n    def method(self):\n        self.db.query()\n";
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
}
