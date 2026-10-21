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

use std::collections::HashMap;

use tree_sitter::{Node, Tree};

use super::{
    ExportSurface, FunctionSpan, Import, ImportKind, Literal, MethodCall, NamedReExport,
    StarReExport,
};

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
fn preceding_decorators(node: Node, source: &str) -> Vec<String> {
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
const EXPRESS_ROUTE_VERBS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "all",
];

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

/// Whether `node` (an import/export statement or one of its specifiers) carries the TS `type`
/// modifier as a direct keyword child: `import type { A }`, `import { type A }`,
/// `export type { A }`, `export { type A }`.
fn has_type_keyword(node: Node) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == "type");
    found
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
    let statement_type_only = has_type_keyword(node);
    let clause = node
        .children(&mut cursor)
        .find(|c| c.kind() == "import_clause");
    let Some(clause) = clause else {
        // `import 'side-effect';` — no clause at all.
        out.push(Import {
            specifier: specifier.to_string(),
            names: Vec::new(),
            kind: ImportKind::SideEffect,
            line,
            type_only: false,
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
                type_only: statement_type_only,
            }),
            "namespace_import" => out.push(Import {
                specifier: specifier.to_string(),
                names: Vec::new(),
                kind: ImportKind::Namespace,
                line,
                type_only: statement_type_only,
            }),
            "named_imports" => {
                let mut nc = part.walk();
                for spec in part
                    .children(&mut nc)
                    .filter(|c| c.kind() == "import_specifier")
                {
                    if let Some(name_node) = spec.child_by_field_name("name") {
                        out.push(Import {
                            specifier: specifier.to_string(),
                            // Origin name (pre-`as`), matching the Rust extractor's
                            // aliased-import convention.
                            names: vec![text(name_node, source).to_string()],
                            kind: ImportKind::Named,
                            line,
                            type_only: statement_type_only || has_type_keyword(spec),
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
    let export_clause = node
        .children(&mut cursor)
        .find(|c| c.kind() == "export_clause");
    if let Some(clause) = export_clause {
        let mut cc = clause.walk();
        for spec in clause
            .children(&mut cc)
            .filter(|c| c.kind() == "export_specifier")
        {
            if let Some(name_node) = spec.child_by_field_name("name") {
                out.push(Import {
                    specifier: specifier.to_string(),
                    names: vec![text(name_node, source).to_string()],
                    kind: ImportKind::ReExport,
                    line,
                    type_only: has_type_keyword(node) || has_type_keyword(spec),
                });
                continue;
            }
        }
        return;
    }
    // `export * from 'x'` or `export * as ns from 'x'` — no enumerable names.
    out.push(Import {
        specifier: specifier.to_string(),
        names: Vec::new(),
        kind: ImportKind::ReExport,
        line,
        type_only: has_type_keyword(node),
    });
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
        type_only: false,
    });
}

// ─── exports ─────────────────────────────────────────────────────────────────────────────

/// The file's export surface: its own exported names, its named re-exports (with BOTH the
/// origin and the exported name, which [`imports`] cannot carry for an aliased re-export), and
/// its star re-exports. Only top-level `export` statements are read (an `export` is
/// syntactically illegal anywhere else). CommonJS `module.exports` is out of scope.
pub fn exports(dialect: EcmaDialect, source: &str) -> ExportSurface {
    let mut out = ExportSurface::default();
    let Some(tree) = parse(dialect, source) else {
        return out;
    };
    let root = tree.root_node();
    let bindings = import_bindings(root, source);
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() == "export_statement" {
            collect_export_statement(node, source, &bindings, &mut out);
        }
    }
    out
}

/// A top-level import binding: the module it came from, the name that module exports it
/// under (`"default"` for a default import), and whether the import was type-only.
struct ImportBinding {
    specifier: String,
    origin: String,
    type_only: bool,
}

/// The file's named and default import bindings by LOCAL name. `import { a as b }` maps `b`.
/// A namespace import is not here: re-exporting one is an own export of a namespace object.
fn import_bindings(root: Node, source: &str) -> HashMap<String, ImportBinding> {
    let mut out = HashMap::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() != "import_statement" {
            continue;
        }
        let Some(specifier) = node
            .child_by_field_name("source")
            .and_then(|s| string_literal_text(s, source))
        else {
            continue;
        };
        let statement_type_only = has_type_keyword(node);
        let mut nc = node.walk();
        let Some(clause) = node.children(&mut nc).find(|c| c.kind() == "import_clause") else {
            continue;
        };
        let mut cc = clause.walk();
        for part in clause.children(&mut cc) {
            match part.kind() {
                "identifier" => {
                    out.insert(
                        text(part, source).to_string(),
                        ImportBinding {
                            specifier: specifier.to_string(),
                            origin: "default".to_string(),
                            type_only: statement_type_only,
                        },
                    );
                }
                "named_imports" => {
                    let mut sc = part.walk();
                    for spec in part
                        .children(&mut sc)
                        .filter(|c| c.kind() == "import_specifier")
                    {
                        let Some(name) = spec.child_by_field_name("name") else {
                            continue;
                        };
                        let local = spec.child_by_field_name("alias").unwrap_or(name);
                        out.insert(
                            text(local, source).to_string(),
                            ImportBinding {
                                specifier: specifier.to_string(),
                                origin: text(name, source).to_string(),
                                type_only: statement_type_only || has_type_keyword(spec),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn collect_export_statement(
    node: Node,
    source: &str,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut ExportSurface,
) {
    let line = start_line(node);
    let source_node = node.child_by_field_name("source");
    let specifier = source_node.and_then(|s| string_literal_text(s, source));
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();

    // `export default ...` in all its forms (expression, function, class) is the single
    // export `default`; a named function/class after it does not also export its own name.
    if children.iter().any(|c| c.kind() == "default") {
        out.own.push("default".to_string());
        return;
    }

    if let Some(clause) = children.iter().find(|c| c.kind() == "export_clause") {
        let mut cc = clause.walk();
        for spec in clause
            .children(&mut cc)
            .filter(|c| c.kind() == "export_specifier")
        {
            let Some(name_node) = spec.child_by_field_name("name") else {
                continue;
            };
            let Some(origin) = export_name_text(name_node, source) else {
                continue;
            };
            let exported = match spec.child_by_field_name("alias") {
                Some(alias) => match export_name_text(alias, source) {
                    Some(a) => a,
                    None => continue,
                },
                None => origin.clone(),
            };
            let type_only = has_type_keyword(node) || has_type_keyword(spec);
            match (specifier, source_node) {
                (Some(sp), _) => out.named_re_exports.push(NamedReExport {
                    specifier: sp.to_string(),
                    origin,
                    exported,
                    line,
                    type_only,
                }),
                // `import { a } from './x'; export { a }` forwards `a` exactly like
                // `export { a } from './x'`, so it is recorded as the re-export it is.
                (None, None) if bindings.contains_key(&origin) => {
                    let b = &bindings[&origin];
                    out.named_re_exports.push(NamedReExport {
                        specifier: b.specifier.clone(),
                        origin: b.origin.clone(),
                        exported,
                        line,
                        type_only: type_only || b.type_only,
                    });
                }
                // `export { a, b as c }` of a local declaration exposes it as an own name.
                (None, None) => out.own.push(exported),
                // A source that is not a plain string literal cannot be followed.
                (None, Some(_)) => {}
            }
        }
        return;
    }

    // `export * from 'x'` / `export * as ns from 'x'`.
    if source_node.is_some() {
        if let Some(ns) = children.iter().find(|c| c.kind() == "namespace_export") {
            let mut nc = ns.walk();
            let name = ns
                .children(&mut nc)
                .find(|c| c.kind() == "identifier")
                .map(|c| text(c, source).to_string());
            if let Some(name) = name {
                out.own.push(name);
            }
        } else if let Some(sp) = specifier {
            out.star_re_exports.push(StarReExport {
                specifier: sp.to_string(),
                line,
            });
        }
        return;
    }

    if let Some(decl) = node.child_by_field_name("declaration") {
        collect_declared_names(decl, source, &mut out.own);
    }
}

/// An export name: a plain identifier or the keyword `default` (`export { default } from`).
/// A string-literal name (`export { a as "x-y" }`) is rare and unusable as an identifier, so
/// it yields `None` and the specifier is skipped rather than guessed.
fn export_name_text(node: Node, source: &str) -> Option<String> {
    if node.kind() == "string" {
        return None;
    }
    let t = text(node, source);
    (!t.is_empty()).then(|| t.to_string())
}

/// The names an exported declaration introduces.
fn collect_declared_names(decl: Node, source: &str, out: &mut Vec<String>) {
    match decl.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = decl.walk();
            for d in decl
                .children(&mut cursor)
                .filter(|c| c.kind() == "variable_declarator")
            {
                if let Some(name) = d.child_by_field_name("name") {
                    collect_pattern_names(name, source, out);
                }
            }
        }
        // `export declare function f(): void;` wraps the real declaration.
        "ambient_declaration" => {
            let mut cursor = decl.walk();
            for child in decl.named_children(&mut cursor) {
                collect_declared_names(child, source, out);
            }
        }
        // function / generator / class / abstract class / interface / type alias / enum /
        // namespace / overload signature: the declared name is the `name` field.
        _ => {
            if let Some(name) = decl.child_by_field_name("name") {
                if matches!(name.kind(), "identifier" | "type_identifier") {
                    out.push(text(name, source).to_string());
                }
            }
        }
    }
}

/// Every binding a declarator's left-hand side introduces, so `export const { a, b: c } = x`
/// is read as `a` and `c`. Missing a binding here would let a star re-export be mistaken for
/// the only source of that name, so destructuring is walked rather than skipped.
fn collect_pattern_names(node: Node, source: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            out.push(text(node, source).to_string());
        }
        "pair_pattern" => {
            if let Some(v) = node.child_by_field_name("value") {
                collect_pattern_names(v, source, out);
            }
        }
        "assignment_pattern" => {
            if let Some(l) = node.child_by_field_name("left") {
                collect_pattern_names(l, source, out);
            }
        }
        "object_assignment_pattern" => {
            if let Some(l) = node.child_by_field_name("left") {
                collect_pattern_names(l, source, out);
            }
        }
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_pattern_names(child, source, out);
            }
        }
        _ => {}
    }
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
            let obj = node
                .child_by_field_name("object")
                .map(|n| render_member_path(n, source));
            let prop = node
                .child_by_field_name("property")
                .map(|n| text(n, source).to_string());
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
                if let (Some(obj), Some(prop)) = (
                    func.child_by_field_name("object"),
                    func.child_by_field_name("property"),
                ) {
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

// ─── literals ────────────────────────────────────────────────────────────────────────────

pub fn literals(dialect: EcmaDialect, source: &str) -> Vec<Literal> {
    let _ = (dialect, source);
    Vec::new()
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
        let vs = imports(
            EcmaDialect::TypeScript,
            "import { a as b, c } from 'bar';\n",
        );
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
        let vs = imports(
            EcmaDialect::TypeScript,
            "export { a as b } from 'bar';\nexport * from 'baz';\n",
        );
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().all(|i| i.kind == ImportKind::ReExport));
        assert!(vs
            .iter()
            .any(|i| i.specifier == "bar" && i.names == vec!["a".to_string()]));
        assert!(vs
            .iter()
            .any(|i| i.specifier == "baz" && i.names.is_empty()));
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
        let vs = imports(
            EcmaDialect::Tsx,
            "import React from 'react';\nconst x = <div>hi</div>;\n",
        );
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
        let fs = functions(
            EcmaDialect::TypeScript,
            "const handler = (req, res) => {};\n",
        );
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "handler");
    }

    #[test]
    fn method_definition_in_class_with_decorator() {
        let fs = functions(
            EcmaDialect::TypeScript,
            "class Foo {\n  @Get('/x')\n  method() {}\n}\n",
        );
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
        let fs = functions(
            EcmaDialect::TypeScript,
            "router.get('/x', (req,res) => {});\n",
        );
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "<anonymous>");
    }

    #[test]
    fn express_route_registration_callback_carries_a_synthetic_route_marker() {
        let fs = functions(
            EcmaDialect::TypeScript,
            "router.get('/x', (req,res) => {});\n",
        );
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].attrs, vec!["<express-route:router.get>".to_string()]);
    }

    #[test]
    fn express_route_registration_recognizes_app_and_every_http_verb() {
        for verb in [
            "get", "post", "put", "delete", "patch", "options", "head", "all",
        ] {
            let src = format!("app.{verb}('/x', function (req, res) {{}});\n");
            let fs = functions(EcmaDialect::JavaScript, &src);
            assert_eq!(fs.len(), 1, "{fs:#?}");
            assert_eq!(
                fs[0].attrs,
                vec![format!("<express-route:app.{verb}>")],
                "verb {verb}"
            );
        }
    }

    #[test]
    fn express_middleware_use_is_not_marked_as_a_route() {
        // Deliberately excluded (see EXPRESS_ROUTE_VERBS doc): app.use(...) covers far more
        // than routes and would over-classify ordinary middleware as a handler.
        let fs = functions(
            EcmaDialect::JavaScript,
            "app.use((req,res,next) => { next(); });\n",
        );
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
        let fs = functions(
            EcmaDialect::TypeScript,
            "const obj = { method() { return 1; } };\n",
        );
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "method");
    }

    // ── method calls ──────────────────────────────────────────────────────────

    #[test]
    fn this_field_chain_receiver() {
        let cs = method_calls(
            EcmaDialect::TypeScript,
            "class A { m() { this.db.query(); } }\n",
        );
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
        assert!(cs
            .iter()
            .any(|c| c.method == "query" && c.receiver == "self.db"));
        assert!(cs.iter().any(|c| c.method == "execute"));
    }

    // ── adversarial: malformed / truncated / binary-ish input must never panic ─

    #[test]
    fn truncated_source_does_not_panic() {
        for dialect in [
            EcmaDialect::TypeScript,
            EcmaDialect::Tsx,
            EcmaDialect::JavaScript,
        ] {
            let src = "function broken( {{{ ??? ";
            let _ = imports(dialect, src);
            let _ = functions(dialect, src);
            let _ = method_calls(dialect, src);
        }
    }

    #[test]
    fn binary_like_content_does_not_panic() {
        let weird = "\u{0}\u{1}\u{FFFD} not js at all \u{FFFD} {{{";
        for dialect in [
            EcmaDialect::TypeScript,
            EcmaDialect::Tsx,
            EcmaDialect::JavaScript,
        ] {
            let _ = imports(dialect, weird);
            let _ = functions(dialect, weird);
            let _ = method_calls(dialect, weird);
        }
    }

    #[test]
    fn empty_source_does_not_panic() {
        for dialect in [
            EcmaDialect::TypeScript,
            EcmaDialect::Tsx,
            EcmaDialect::JavaScript,
        ] {
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
        src.push('1');
        for _ in 0..200 {
            src.push('}');
        }
        src.push_str(";\n");
        // Just proving no crash / no hang; result content is not the point of this test.
        let _ = imports(EcmaDialect::TypeScript, &src);
        let _ = functions(EcmaDialect::TypeScript, &src);
        let _ = method_calls(EcmaDialect::TypeScript, &src);
    }
    // ── exports ───────────────────────────────────────────────────────────────

    fn own(dialect: EcmaDialect, src: &str) -> Vec<String> {
        let mut v = exports(dialect, src).own;
        v.sort();
        v
    }

    #[test]
    fn exports_declaration_forms_are_own_names() {
        let src = "export function f() {}\n\
                   export async function g() {}\n\
                   export function* gen() {}\n\
                   export class C {}\n\
                   export const k = 1, m = 2;\n\
                   export let l = 3;\n\
                   export var v = 4;\n\
                   export interface I {}\n\
                   export type T = string;\n\
                   export enum E { A }\n\
                   export namespace N { export const x = 1; }\n\
                   export abstract class AC {}\n\
                   export declare function ov(): void;\n";
        assert_eq!(
            own(EcmaDialect::TypeScript, src),
            ["AC", "C", "E", "I", "N", "T", "f", "g", "gen", "k", "l", "m", "ov", "v"]
        );
    }

    #[test]
    fn exports_js_declaration_forms() {
        let src = "export function f() {}\nexport class C {}\nexport const a = 1;\n";
        assert_eq!(own(EcmaDialect::JavaScript, src), ["C", "a", "f"]);
    }

    #[test]
    fn exports_destructured_const_yields_every_binding() {
        let src = "export const { a, b: c, ...rest } = obj;\nexport const [x, , y] = arr;\n";
        assert_eq!(
            own(EcmaDialect::TypeScript, src),
            ["a", "c", "rest", "x", "y"]
        );
    }

    #[test]
    fn exports_default_in_every_form_is_default_only() {
        for src in [
            "export default 42;\n",
            "export default function named() {}\n",
            "export default class Named {}\n",
            "export default () => 1;\n",
        ] {
            assert_eq!(own(EcmaDialect::TypeScript, src), ["default"], "{src}");
        }
    }

    #[test]
    fn exports_local_clause_yields_exported_names_not_local_names() {
        let src = "const a = 1, b = 2;\nexport { a, b as c };\n";
        let ex = exports(EcmaDialect::TypeScript, src);
        assert_eq!(ex.own, ["a", "c"]);
        assert!(ex.named_re_exports.is_empty());
        assert!(ex.star_re_exports.is_empty());
    }

    #[test]
    fn exports_local_clause_aliased_to_default() {
        let src = "const a = 1;\nexport { a as default };\n";
        assert_eq!(own(EcmaDialect::TypeScript, src), ["default"]);
    }

    #[test]
    fn exports_named_re_export_carries_origin_exported_and_line() {
        let src = "\nexport { a, b as c } from './x';\n";
        let ex = exports(EcmaDialect::TypeScript, src);
        assert!(ex.own.is_empty(), "{ex:#?}");
        assert_eq!(
            ex.named_re_exports,
            vec![
                NamedReExport {
                    specifier: "./x".into(),
                    origin: "a".into(),
                    exported: "a".into(),
                    line: 2,
                    type_only: false,
                },
                NamedReExport {
                    specifier: "./x".into(),
                    origin: "b".into(),
                    exported: "c".into(),
                    line: 2,
                    type_only: false,
                },
            ]
        );
    }

    #[test]
    fn exports_default_re_export_forms() {
        let src = "export { default } from './d';\nexport { default as Widget } from './w';\n\
                   export { thing as default } from './t';\n";
        let ex = exports(EcmaDialect::TypeScript, src);
        let got: Vec<(&str, &str, &str)> = ex
            .named_re_exports
            .iter()
            .map(|r| (r.specifier.as_str(), r.origin.as_str(), r.exported.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("./d", "default", "default"),
                ("./w", "default", "Widget"),
                ("./t", "thing", "default"),
            ]
        );
    }

    #[test]
    fn exports_type_only_re_export_is_a_named_re_export() {
        let ex = exports(
            EcmaDialect::TypeScript,
            "export type { T as U } from './t';\n",
        );
        assert_eq!(ex.named_re_exports.len(), 1, "{ex:#?}");
        assert_eq!(ex.named_re_exports[0].origin, "T");
        assert_eq!(ex.named_re_exports[0].exported, "U");
    }

    #[test]
    fn exports_star_re_export_is_recorded_with_its_line() {
        let ex = exports(
            EcmaDialect::TypeScript,
            "export * from './a';\n\nexport * from './b';\n",
        );
        assert!(
            ex.own.is_empty() && ex.named_re_exports.is_empty(),
            "{ex:#?}"
        );
        assert_eq!(
            ex.star_re_exports,
            vec![
                StarReExport {
                    specifier: "./a".into(),
                    line: 1
                },
                StarReExport {
                    specifier: "./b".into(),
                    line: 3
                },
            ]
        );
    }

    #[test]
    fn exports_star_as_namespace_is_an_own_export_not_a_star() {
        let ex = exports(EcmaDialect::TypeScript, "export * as ns from './a';\n");
        assert_eq!(ex.own, ["ns"]);
        assert!(ex.star_re_exports.is_empty());
        assert!(ex.named_re_exports.is_empty());
    }

    #[test]
    fn exports_ignore_imports_and_non_exported_declarations() {
        let src = "import { a } from './a';\nfunction hidden() {}\nconst also = 1;\n";
        assert_eq!(
            exports(EcmaDialect::TypeScript, src),
            ExportSurface::default()
        );
    }

    #[test]
    fn imports_carry_the_type_only_flag_for_statement_and_inline_forms() {
        let src = "import type { A } from 'x';\n\
                   import { type B, c } from 'y';\n\
                   import type D from 'z';\n\
                   import E from 'w';\n";
        let vs = imports(EcmaDialect::TypeScript, src);
        let flag = |name: &str| {
            vs.iter()
                .find(|i| i.names == [name.to_string()])
                .map(|i| i.type_only)
        };
        assert_eq!(flag("A"), Some(true), "{vs:#?}");
        assert_eq!(flag("B"), Some(true), "{vs:#?}");
        assert_eq!(flag("c"), Some(false), "{vs:#?}");
        let defaults: Vec<(&str, bool)> = vs
            .iter()
            .filter(|i| i.kind == ImportKind::Default)
            .map(|i| (i.specifier.as_str(), i.type_only))
            .collect();
        assert_eq!(defaults, [("z", true), ("w", false)]);
    }

    #[test]
    fn exports_carry_the_type_only_flag_for_statement_and_inline_forms() {
        let src = "export type { A } from './x';\nexport { type B, c } from './y';\n";
        let ex = exports(EcmaDialect::TypeScript, src);
        let flags: Vec<(&str, bool)> = ex
            .named_re_exports
            .iter()
            .map(|r| (r.exported.as_str(), r.type_only))
            .collect();
        assert_eq!(flags, [("A", true), ("B", true), ("c", false)]);
    }

    #[test]
    fn exports_local_export_of_an_imported_binding_is_a_named_re_export() {
        let src = "import { a } from './x';\n\
                   import { q as r } from './q';\n\
                   import d from './d';\n\
                   import type { T } from './t';\n\
                   import * as ns from './n';\n\
                   const local = 1;\n\
                   export { a, r, d as dd, T, ns, local };\n";
        let ex = exports(EcmaDialect::TypeScript, src);
        let got: Vec<(&str, &str, &str, bool, usize)> = ex
            .named_re_exports
            .iter()
            .map(|r| {
                (
                    r.specifier.as_str(),
                    r.origin.as_str(),
                    r.exported.as_str(),
                    r.type_only,
                    r.line,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("./x", "a", "a", false, 7),
                ("./q", "q", "r", false, 7),
                ("./d", "default", "dd", false, 7),
                ("./t", "T", "T", true, 7),
            ]
        );
        let mut own = ex.own.clone();
        own.sort();
        assert_eq!(own, ["local", "ns"]);
    }

    #[test]
    fn exports_malformed_input_does_not_panic() {
        for src in [
            "export",
            "export {",
            "export { a as } from",
            "export * from",
            "export default",
            "export const = ;",
            "export { a as 'x-y' } from './x';",
            "export * as from './x';",
            "\u{0}\u{1} export { \u{ffff} }",
            "",
        ] {
            for dialect in [
                EcmaDialect::TypeScript,
                EcmaDialect::Tsx,
                EcmaDialect::JavaScript,
            ] {
                let _ = exports(dialect, src);
            }
        }
    }
}
