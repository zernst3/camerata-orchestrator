//! Shared TypeScript / TSX / JavaScript extractor: native `tree-sitter` grammars
//! (`tree-sitter-typescript`, `tree-sitter-javascript` — cc-compiled, statically linked, NOT
//! wasm, per D2). One implementation serves all three dialects because the node-kind
//! vocabulary this extractor cares about (`import_statement`, `function_declaration`,
//! `call_expression`, `member_expression`, ...) is IDENTICAL across the TS, TSX, and JS
//! grammars — `tree-sitter-typescript` is the JS grammar plus type-annotation/JSX
//! extensions, not a divergent vocabulary. A single walker parameterized by which grammar to
//! load (via [`EcmaDialect`]) avoids tripling the traversal logic across three near-identical
//! files, while still giving each dialect its own parser instance (a TS file is never parsed
//! with the JS grammar or vice versa).
//!
//! # Never panics
//!
//! `tree_sitter::Parser::parse` on these grammars ALWAYS returns `Some(Tree)` for any input —
//! there is no parse failure mode, only error-recovery `ERROR` nodes embedded in an otherwise
//! valid tree. This extractor's tree walk never indexes into a node it hasn't already checked
//! the kind/field of, so a malformed/truncated/binary-ish source degrades to fewer (or zero)
//! results, never a panic.

use tree_sitter::{Node, Tree};

use super::{FunctionSpan, Import, ImportKind, MethodCall};

/// Which of the three v1 ECMAScript-family grammars to parse `source` with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcmaDialect {
    TypeScript,
    Tsx,
    JavaScript,
}

fn language_for(dialect: EcmaDialect) -> tree_sitter::Language {
    match dialect {
        EcmaDialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        EcmaDialect::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        EcmaDialect::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
    }
}

/// Parse `source` with the grammar for `dialect`. Returns `None` only if the grammar itself
/// could not be loaded (never happens for our statically-linked, version-pinned grammars) —
/// kept as an `Option` so callers degrade gracefully instead of unwrapping.
fn parse(dialect: EcmaDialect, source: &str) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language_for(dialect)).ok()?;
    parser.parse(source, None)
}

/// 1-based start line of `node`.
fn start_line(node: Node) -> usize {
    node.start_position().row + 1
}

/// 1-based end line of `node`.
fn end_line(node: Node) -> usize {
    node.end_position().row + 1
}

/// Slice `node`'s exact source text out of `source`. Byte ranges from a `tree-sitter` parse of
/// `source` always index into `source` itself, so this never panics on a successfully
/// constructed tree; the `unwrap_or_default` is defensive only.
fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// Extract the string-literal contents of a `(string (string_fragment) )`-shaped node.
/// Returns `None` for a non-literal specifier (a template string, a variable, string
/// concatenation) — the resolver deliberately cannot follow those, so it's correct for the
/// extractor to surface nothing rather than guess.
fn string_literal_text<'s>(node: Node, source: &'s str) -> Option<&'s str> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    // An empty string literal ('') has no `string_fragment` child at all — that's a valid
    // (if useless) specifier, so treat "no fragment child" as the empty string rather than
    // "unparseable".
    for child in node.children(&mut cursor) {
        if child.kind() == "string_fragment" {
            return Some(text(child, source));
        }
    }
    Some("")
}

/// Collect the raw source text of every `decorator` node immediately preceding `node` among
/// its named siblings (TS/JS decorators attach as PRECEDING SIBLINGS of the class member or
/// declaration they annotate, not as a child of it) — e.g. `@Get('/x')` before a
/// `method_definition`. Returned in source order (earliest first).
fn preceding_decorators<'s>(node: Node, source: &'s str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = node.prev_named_sibling();
    while let Some(n) = cur {
        if n.kind() == "decorator" {
            out.push(text(n, source).to_string());
            cur = n.prev_named_sibling();
        } else {
            break;
        }
    }
    out.reverse();
    out
}

/// The Express/Koa/generic-router HTTP-verb method names this extractor recognizes as a route
/// REGISTRATION call (`router.get(...)`, `app.post(...)`, ...) — deliberately excludes `use`
/// (Express middleware registration covers far more than routes, e.g. `app.use(cors())`, and
/// including it would flag a huge amount of non-handler code as handler-classified).
const EXPRESS_ROUTE_VERBS: &[&str] = &["get", "post", "put", "delete", "patch", "options", "head", "all"];

/// Best-effort Express-style route-REGISTRATION marker for an anonymous `arrow_function` /
/// `function_expression`: when `node` is (syntactically) a callback ARGUMENT passed directly to
/// a `receiver.VERB(...)` call whose `VERB` is a recognized HTTP method
/// (`router.get('/x', (req,res) => {...})`), returns a synthetic marker string
/// (`"<express-route:router.get>"`) that a checker can recognize as a structural route-attribute
/// equivalent — Express has no decorator syntax, so this is the closest AST-level analogue to
/// `#[get("/x")]` / `@app.route(...)`. Returns `None` for every other calling context (a plain
/// callback passed to `.map`/`.then`/anything else) — never a guess, never a panic on an
/// unmatched shape.
fn express_route_marker(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() != "arguments" {
        return None;
    }
    let call = parent.parent()?;
    if call.kind() != "call_expression" {
        return None;
    }
    let func = call.child_by_field_name("function")?;
    if func.kind() != "member_expression" {
        return None;
    }
    let object = func.child_by_field_name("object")?;
    let property = func.child_by_field_name("property")?;
    let verb = text(property, source).to_ascii_lowercase();
    if !EXPRESS_ROUTE_VERBS.contains(&verb.as_str()) {
        return None;
    }
    let receiver = render_member_path(object, source);
    Some(format!("<express-route:{receiver}.{verb}>"))
}

// ─── imports ─────────────────────────────────────────────────────────────────────────────

pub fn imports(dialect: EcmaDialect, source: &str) -> Vec<Import> {
    let Some(tree) = parse(dialect, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_imports(tree.root_node(), source, &mut out);
    out
}

fn walk_imports(node: Node, source: &str, out: &mut Vec<Import>) {
    match node.kind() {
        "import_statement" => collect_import_statement(node, source, out),
        "export_statement" => collect_export_from(node, source, out),
        "call_expression" => collect_dynamic_import_or_require(node, source, out),
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_imports(child, source, out);
    }
}

fn collect_import_statement(node: Node, source: &str, out: &mut Vec<Import>) {
    let Some(source_node) = node.child_by_field_name("source") else {
        return;
    };
    let Some(specifier) = string_literal_text(source_node, source) else {
        return;
    };
    let line = start_line(node);
    let mut cursor = node.walk();
    let clause = node.children(&mut cursor).find(|c| c.kind() == "import_clause");
    let Some(clause) = clause else {
        // `import 'side-effect';` — no clause at all.
        out.push(Import {
            specifier: specifier.to_string(),
            names: Vec::new(),
            kind: ImportKind::SideEffect,
            line,
        });
        return;
    };
    let mut cc = clause.walk();
    for part in clause.children(&mut cc) {
        match part.kind() {
            "identifier" => out.push(Import {
                specifier: specifier.to_string(),
                names: Vec::new(),
                kind: ImportKind::Default,
                line,
            }),
            "namespace_import" => out.push(Import {
                specifier: specifier.to_string(),
                names: Vec::new(),
                kind: ImportKind::Namespace,
                line,
            }),
            "named_imports" => {
                let mut nc = part.walk();
                for spec in part.children(&mut nc).filter(|c| c.kind() == "import_specifier") {
                    if let Some(name_node) = spec.child_by_field_name("name") {
                        out.push(Import {
                            specifier: specifier.to_string(),
                            // Origin name (pre-`as`), matching the Rust extractor's
                            // aliased-import convention.
                            names: vec![text(name_node, source).to_string()],
                            kind: ImportKind::Named,
                            line,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// `export { a } from 'x'` / `export * from 'x'` / `export * as ns from 'x'` — a re-export IS
/// a dependency edge (this file's public surface depends on `x`), so it's captured here as
/// [`ImportKind::ReExport`]. A plain `export function foo() {}` (no `source` field) is a
/// declaration, not a re-export, and is correctly skipped.
fn collect_export_from(node: Node, source: &str, out: &mut Vec<Import>) {
    let Some(source_node) = node.child_by_field_name("source") else {
        return;
    };
    let Some(specifier) = string_literal_text(source_node, source) else {
        return;
    };
    let line = start_line(node);
    let mut cursor = node.walk();
    let export_clause = node.children(&mut cursor).find(|c| c.kind() == "export_clause");
    if let Some(clause) = export_clause {
        let mut cc = clause.walk();
        for spec in clause.children(&mut cc).filter(|c| c.kind() == "export_specifier") {
            if let Some(name_node) = spec.child_by_field_name("name") {
                out.push(Import {
                    specifier: specifier.to_string(),
                    names: vec![text(name_node, source).to_string()],
                    kind: ImportKind::ReExport,
                    line,
                });
                continue;
            }
        }
        return;
    }
    // `export * from 'x'` or `export * as ns from 'x'` — no enumerable names.
    out.push(Import { specifier: specifier.to_string(), names: Vec::new(), kind: ImportKind::ReExport, line });
}

/// A dynamic `import('x')` call, or a CommonJS `require('x')` call — both are runtime
/// dependency edges the resolver should still try to follow. Only a STRING-LITERAL argument
/// is captured; a computed specifier (`import(pathVar)`) is unresolvable by construction and
/// is correctly dropped (false negative, never a guess).
fn collect_dynamic_import_or_require(node: Node, source: &str, out: &mut Vec<Import>) {
    let Some(func) = node.child_by_field_name("function") else {
        return;
    };
    let is_dynamic_import = func.kind() == "import";
    let is_require = func.kind() == "identifier" && text(func, source) == "require";
    if !is_dynamic_import && !is_require {
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

pub fn functions(dialect: EcmaDialect, source: &str) -> Vec<FunctionSpan> {
    let Some(tree) = parse(dialect, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_functions(tree.root_node(), source, &mut out);
    out
}

fn walk_functions(node: Node, source: &str, out: &mut Vec<FunctionSpan>) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, source).to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());
            out.push(FunctionSpan {
                name,
                start_line: start_line(node),
                end_line: end_line(node),
                attrs: preceding_decorators(node, source),
            });
        }
        "method_definition" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, source).to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());
            out.push(FunctionSpan {
                name,
                start_line: start_line(node),
                end_line: end_line(node),
                attrs: preceding_decorators(node, source),
            });
        }
        "arrow_function" | "function_expression" => {
            let mut attrs = preceding_decorators(node, source);
            if let Some(marker) = express_route_marker(node, source) {
                attrs.push(marker);
            }
            out.push(FunctionSpan {
                name: name_from_binding_context(node, source),
                start_line: start_line(node),
                end_line: end_line(node),
                attrs,
            });
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_functions(child, source, out);
    }
}

/// Best-effort name for an anonymous `arrow_function`/`function_expression` from its
/// surrounding binding context: `const NAME = () => {}`, `obj.prop = () => {}`,
/// `{ key: () => {} }`, `export default () => {}`. Falls back to `"<anonymous>"` for any other
/// context (a bare callback argument, e.g. `router.get('/x', (req,res) => {})`) — still
/// extracted as a function span (a future handler-boundary checker correlates it via the
/// enclosing call site, not via name), just without a derivable name.
fn name_from_binding_context(node: Node, source: &str) -> String {
    let Some(parent) = node.parent() else {
        return "<anonymous>".to_string();
    };
    match parent.kind() {
        "variable_declarator" => parent
            .child_by_field_name("name")
            .map(|n| text(n, source).to_string())
            .unwrap_or_else(|| "<anonymous>".to_string()),
        "pair" => parent
            .child_by_field_name("key")
            .map(|n| text(n, source).to_string())
            .unwrap_or_else(|| "<anonymous>".to_string()),
        "assignment_expression" => parent
            .child_by_field_name("left")
            .map(|n| render_member_path(n, source))
            .unwrap_or_else(|| "<anonymous>".to_string()),
        "export_statement" => "default".to_string(),
        _ => "<anonymous>".to_string(),
    }
}

/// Render an identifier or a `member_expression` chain (`exports.handler`, `this.foo.bar`) as
/// a dotted string. Anything richer renders as `"<expr>"`.
fn render_member_path(node: Node, source: &str) -> String {
    match node.kind() {
        "identifier" | "this" | "property_identifier" => text(node, source).to_string(),
        "member_expression" => {
            let obj = node.child_by_field_name("object").map(|n| render_member_path(n, source));
            let prop = node.child_by_field_name("property").map(|n| text(n, source).to_string());
            match (obj, prop) {
                (Some(o), Some(p)) => format!("{o}.{p}"),
                _ => "<expr>".to_string(),
            }
        }
        _ => "<expr>".to_string(),
    }
}

// ─── method calls ────────────────────────────────────────────────────────────────────────

pub fn method_calls(dialect: EcmaDialect, source: &str) -> Vec<MethodCall> {
    let Some(tree) = parse(dialect, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_calls(tree.root_node(), source, &mut out);
    out
}

fn walk_calls(node: Node, source: &str, out: &mut Vec<MethodCall>) {
    if node.kind() == "call_expression" {
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "member_expression" {
                if let (Some(obj), Some(prop)) =
                    (func.child_by_field_name("object"), func.child_by_field_name("property"))
                {
                    out.push(MethodCall {
                        receiver: render_member_path(obj, source),
                        method: text(prop, source).to_string(),
                        line: start_line(prop),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── imports ───────────────────────────────────────────────────────────────

    #[test]
    fn default_import() {
        let vs = imports(EcmaDialect::TypeScript, "import foo from 'bar';\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "bar");
        assert_eq!(vs[0].kind, ImportKind::Default);
    }

    #[test]
    fn namespace_import() {
        let vs = imports(EcmaDialect::TypeScript, "import * as ns from 'bar';\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].kind, ImportKind::Namespace);
    }

    #[test]
    fn named_import_with_alias() {
        let vs = imports(EcmaDialect::TypeScript, "import { a as b, c } from 'bar';\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        // Origin names recorded (a, c), not the local alias (b).
        assert!(vs.iter().any(|i| i.names == vec!["a".to_string()]));
        assert!(vs.iter().any(|i| i.names == vec!["c".to_string()]));
        assert!(vs.iter().all(|i| i.kind == ImportKind::Named));
    }

    #[test]
    fn side_effect_import() {
        let vs = imports(EcmaDialect::JavaScript, "import 'bar';\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].kind, ImportKind::SideEffect);
    }

    #[test]
    fn re_export_named_and_star() {
        let vs = imports(EcmaDialect::TypeScript, "export { a as b } from 'bar';\nexport * from 'baz';\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().all(|i| i.kind == ImportKind::ReExport));
        assert!(vs.iter().any(|i| i.specifier == "bar" && i.names == vec!["a".to_string()]));
        assert!(vs.iter().any(|i| i.specifier == "baz" && i.names.is_empty()));
    }

    #[test]
    fn dynamic_import_with_literal_specifier() {
        let vs = imports(EcmaDialect::TypeScript, "const m = await import('bar');\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "bar");
        assert_eq!(vs[0].kind, ImportKind::Dynamic);
    }

    #[test]
    fn dynamic_import_with_variable_specifier_is_dropped() {
        let vs = imports(EcmaDialect::TypeScript, "const m = await import(path);\n");
        assert!(vs.is_empty(), "{vs:#?}");
    }

    #[test]
    fn commonjs_require() {
        let vs = imports(EcmaDialect::JavaScript, "const y = require('y');\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "y");
        assert_eq!(vs[0].kind, ImportKind::Dynamic);
    }

    #[test]
    fn tsx_import_parses_with_tsx_dialect() {
        let vs = imports(EcmaDialect::Tsx, "import React from 'react';\nconst x = <div>hi</div>;\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "react");
    }

    // ── functions ─────────────────────────────────────────────────────────────

    #[test]
    fn named_function_declaration() {
        let fs = functions(EcmaDialect::TypeScript, "function handler(req, res) {}\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "handler");
    }

    #[test]
    fn arrow_function_named_via_const() {
        let fs = functions(EcmaDialect::TypeScript, "const handler = (req, res) => {};\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "handler");
    }

    #[test]
    fn method_definition_in_class_with_decorator() {
        let fs = functions(EcmaDialect::TypeScript, "class Foo {\n  @Get('/x')\n  method() {}\n}\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "method");
        assert_eq!(fs[0].attrs, vec!["@Get('/x')".to_string()]);
    }

    #[test]
    fn export_default_anonymous_arrow_gets_default_name() {
        let fs = functions(EcmaDialect::TypeScript, "export default () => {};\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "default");
    }

    #[test]
    fn callback_argument_arrow_is_still_captured_anonymously() {
        let fs = functions(EcmaDialect::TypeScript, "router.get('/x', (req,res) => {});\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "<anonymous>");
    }

    #[test]
    fn express_route_registration_callback_carries_a_synthetic_route_marker() {
        let fs = functions(EcmaDialect::TypeScript, "router.get('/x', (req,res) => {});\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].attrs, vec!["<express-route:router.get>".to_string()]);
    }

    #[test]
    fn express_route_registration_recognizes_app_and_every_http_verb() {
        for verb in ["get", "post", "put", "delete", "patch", "options", "head", "all"] {
            let src = format!("app.{verb}('/x', function (req, res) {{}});\n");
            let fs = functions(EcmaDialect::JavaScript, &src);
            assert_eq!(fs.len(), 1, "{fs:#?}");
            assert_eq!(fs[0].attrs, vec![format!("<express-route:app.{verb}>")], "verb {verb}");
        }
    }

    #[test]
    fn express_middleware_use_is_not_marked_as_a_route() {
        // Deliberately excluded (see EXPRESS_ROUTE_VERBS doc): app.use(...) covers far more
        // than routes and would over-classify ordinary middleware as a handler.
        let fs = functions(EcmaDialect::JavaScript, "app.use((req,res,next) => { next(); });\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert!(fs[0].attrs.is_empty(), "{fs:#?}");
    }

    #[test]
    fn plain_callback_argument_to_an_unrelated_call_is_not_marked_as_a_route() {
        let fs = functions(EcmaDialect::TypeScript, "[1, 2, 3].map((x) => x + 1);\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert!(fs[0].attrs.is_empty(), "{fs:#?}");
    }

    #[test]
    fn object_method_shorthand() {
        let fs = functions(EcmaDialect::TypeScript, "const obj = { method() { return 1; } };\n");
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "method");
    }

    // ── method calls ──────────────────────────────────────────────────────────

    #[test]
    fn this_field_chain_receiver() {
        let cs = method_calls(EcmaDialect::TypeScript, "class A { m() { this.db.query(); } }\n");
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "this.db");
        assert_eq!(cs[0].method, "query");
    }

    #[test]
    fn bare_identifier_receiver() {
        let cs = method_calls(EcmaDialect::JavaScript, "db.execute();\n");
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "db");
        assert_eq!(cs[0].method, "execute");
    }

    #[test]
    fn chained_calls_both_captured() {
        let cs = method_calls(EcmaDialect::TypeScript, "self.db.query().execute();\n");
        assert_eq!(cs.len(), 2, "{cs:#?}");
        assert!(cs.iter().any(|c| c.method == "query" && c.receiver == "self.db"));
        assert!(cs.iter().any(|c| c.method == "execute"));
    }

    // ── adversarial: malformed / truncated / binary-ish input must never panic ─

    #[test]
    fn truncated_source_does_not_panic() {
        for dialect in [EcmaDialect::TypeScript, EcmaDialect::Tsx, EcmaDialect::JavaScript] {
            let src = "function broken( {{{ ??? ";
            let _ = imports(dialect, src);
            let _ = functions(dialect, src);
            let _ = method_calls(dialect, src);
        }
    }

    #[test]
    fn binary_like_content_does_not_panic() {
        let weird = "\u{0}\u{1}\u{FFFD} not js at all \u{FFFD} {{{";
        for dialect in [EcmaDialect::TypeScript, EcmaDialect::Tsx, EcmaDialect::JavaScript] {
            let _ = imports(dialect, weird);
            let _ = functions(dialect, weird);
            let _ = method_calls(dialect, weird);
        }
    }

    #[test]
    fn empty_source_does_not_panic() {
        for dialect in [EcmaDialect::TypeScript, EcmaDialect::Tsx, EcmaDialect::JavaScript] {
            assert!(imports(dialect, "").is_empty());
            assert!(functions(dialect, "").is_empty());
            assert!(method_calls(dialect, "").is_empty());
        }
    }

    #[test]
    fn deeply_nested_object_does_not_stack_overflow() {
        let mut src = String::from("const x = ");
        for _ in 0..200 {
            src.push_str("{ a: ");
        }
        src.push_str("1");
        for _ in 0..200 {
            src.push('}');
        }
        src.push_str(";\n");
        // Just proving no crash / no hang; result content is not the point of this test.
        let _ = imports(EcmaDialect::TypeScript, &src);
        let _ = functions(EcmaDialect::TypeScript, &src);
        let _ = method_calls(EcmaDialect::TypeScript, &src);
    }
}
