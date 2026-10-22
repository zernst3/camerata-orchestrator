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

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Tree};

use super::ancestry::{walk_with_ancestors, Home, Pos};

use super::{
    canonical_number, ComputedLoad, ExportSurface, FunctionSpan, Import, ImportKind, Literal,
    LiteralCtx, LiteralKind, MethodCall, ModuleConst, NamedReExport, StarReExport,
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
pub(crate) fn parse(dialect: EcmaDialect, source: &str) -> Option<Tree> {
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

// ─── computed loads ──────────────────────────────────────────────────────────────────────

/// Every load the import graph cannot follow: `import(<non-string>)`, `require(<non-string>)`,
/// `require.resolve(<non-string>)`, `require.context('<dir>', ..)`, `import.meta.glob(..)` and
/// `import.meta.globEager(..)`. See [`ComputedLoad`]. A string-literal `import('./x')` /
/// `require('./x')` is an ordinary edge ([`imports`]) and is not reported here.
pub fn computed_loads(dialect: EcmaDialect, source: &str) -> Vec<ComputedLoad> {
    let Some(tree) = parse(dialect, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_computed_loads(tree.root_node(), source, &mut out);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadCall {
    /// `import(x)`, `require(x)`, `require.resolve(x)`: a module specifier.
    Specifier,
    /// `require.context(dir, ..)`: the first argument is a directory.
    Directory,
    /// `import.meta.glob(pattern | [patterns])`.
    Glob,
}

fn load_call_kind(func: Node, source: &str) -> Option<LoadCall> {
    if func.kind() == "import" {
        return Some(LoadCall::Specifier);
    }
    let compact: String = text(func, source)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    match compact.as_str() {
        "require" | "require.resolve" => Some(LoadCall::Specifier),
        "require.context" => Some(LoadCall::Directory),
        "import.meta.glob" | "import.meta.globEager" => Some(LoadCall::Glob),
        _ => None,
    }
}

fn walk_computed_loads(node: Node, source: &str, out: &mut Vec<ComputedLoad>) {
    if node.kind() == "call_expression" {
        collect_computed_load(node, source, out);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_computed_loads(child, source, out);
    }
}

fn collect_computed_load(node: Node, source: &str, out: &mut Vec<ComputedLoad>) {
    let Some(func) = node.child_by_field_name("function") else {
        return;
    };
    let Some(kind) = load_call_kind(func, source) else {
        return;
    };
    let Some(first) = node
        .child_by_field_name("arguments")
        .and_then(|args| args.named_child(0))
    else {
        return;
    };
    let line = start_line(node);
    let mut push = |prefix: Option<String>| out.push(ComputedLoad { prefix, line });
    match kind {
        LoadCall::Specifier => {
            // A plain string literal is an ordinary edge (`imports`), not a computed load.
            if first.kind() != "string" {
                push(leading_literal(first, source));
            }
        }
        LoadCall::Directory => push(match string_literal_text(first, source) {
            Some(dir) if dir.ends_with('/') => Some(dir.to_string()),
            Some(dir) => Some(format!("{dir}/")),
            None => None,
        }),
        LoadCall::Glob => {
            if first.kind() == "array" {
                let mut cursor = first.walk();
                for element in first.named_children(&mut cursor) {
                    match string_literal_text(element, source) {
                        // A negated pattern removes files from the load, never adds.
                        Some(pattern) if pattern.starts_with('!') => {}
                        _ => push(leading_literal(element, source).map(glob_head)),
                    }
                }
            } else {
                push(leading_literal(first, source).map(glob_head));
            }
        }
    }
}

/// The literal text a path expression starts with: a string's content, a template literal's
/// first quasi (the text before its first `${`), or the leftmost operand of a `+` chain
/// resolved the same way. `None` when the expression does not start with literal text.
fn leading_literal(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "string" => string_literal_text(node, source).map(str::to_string),
        "template_string" => {
            let start = node.start_byte() + 1; // past the opening backtick
            let mut end = node.end_byte().saturating_sub(1).max(start);
            let mut cursor = node.walk();
            if let Some(sub) = node
                .children(&mut cursor)
                .find(|c| c.kind() == "template_substitution")
            {
                end = sub.start_byte().max(start);
            }
            source.get(start..end).map(str::to_string)
        }
        "parenthesized_expression" => leading_literal(node.named_child(0)?, source),
        "binary_expression" => {
            let op = node.child_by_field_name("operator")?;
            if text(op, source) != "+" {
                return None;
            }
            leading_literal(node.child_by_field_name("left")?, source)
        }
        _ => None,
    }
}

/// A glob pattern's literal head: the text before its first wildcard character.
fn glob_head(pattern: String) -> String {
    match pattern.find(['*', '{', '[', '?']) {
        Some(i) => pattern[..i].to_string(),
        None => pattern,
    }
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

/// Node kinds the nearest-call walk passes through on its way up from a literal: containers
/// (arrays, objects, pairs), grouping and spreads, templates, and the expression wrappers
/// that carry a literal along unchanged (`a || 'x-y'`, `c ? 'x-y' : 'z-w'`, `'x-y' as T`).
/// A function boundary is deliberately absent: a literal in a callback body belongs to the
/// callback, not to the call that receives it.
const CALLEE_PASS_THROUGH: &[&str] = &[
    "array",
    "object",
    "pair",
    "parenthesized_expression",
    "spread_element",
    "template_string",
    "template_substitution",
    "ternary_expression",
    "binary_expression",
    "as_expression",
    "satisfies_expression",
    "non_null_expression",
    "await_expression",
    "jsx_expression",
    "type_assertion",
    "sequence_expression",
];

/// The wrappers `value_of_key` looks through between a literal and the `pair` it is the value
/// of. A call or a function ends the search: `{ env: foo('a-b') }` is not `env`'s value.
const VALUE_WRAPPERS: &[&str] = &[
    "parenthesized_expression",
    "as_expression",
    "satisfies_expression",
    "non_null_expression",
    "ternary_expression",
    "binary_expression",
    "await_expression",
    "array",
    "type_assertion",
];

/// The wrappers a module-level initializer may pass through and still be "the constant":
/// objects, arrays and the type-level wrappers (`as const`, `satisfies T`). A call is not
/// among them, so `const Q = register('x-y')` is not a home for `'x-y'`.
const INITIALIZER_WRAPPERS: &[&str] = &[
    "object",
    "array",
    "parenthesized_expression",
    "as_expression",
    "satisfies_expression",
    "non_null_expression",
    "type_assertion",
];

/// Ancestors that put a literal in a TYPE position rather than a value position.
const TYPE_POSITION_KINDS: &[&str] = &[
    "literal_type",
    "enum_assignment",
    "interface_declaration",
    "type_alias_declaration",
    "type_annotation",
    "type_arguments",
];

/// Calls whose first argument is a module specifier (besides `require(..)` and `import(..)`).
const SPECIFIER_CALLEES: &[&str] = &["require.resolve", "jest.mock", "vi.mock"];

/// Extract every string and number literal with its [`LiteralCtx`]. See
/// [`super::literals`] for what counts as a literal.
pub fn literals(dialect: EcmaDialect, source: &str) -> Vec<Literal> {
    let Some(mut tree) = parse(dialect, source) else {
        return Vec::new();
    };
    // The JavaScript grammar cannot parse some JSX (an attribute named `class` or `for`). The
    // TSX grammar can, so a JavaScript file that did not parse cleanly gets a second chance
    // before the literals under its error nodes are given up.
    if dialect == EcmaDialect::JavaScript && tree.root_node().has_error() {
        if let Some(retry) = parse(EcmaDialect::Tsx, source).filter(|t| !t.root_node().has_error())
        {
            tree = retry;
        }
    }
    let root = tree.root_node();
    let type_sources = typeof_sources(root, source);
    let mut out = Vec::new();
    walk_with_ancestors(root, |pos| {
        if let Some(literal) = literal_at(pos, source, &type_sources) {
            out.push(literal);
        }
    });
    out
}

/// The literal `node` is, when it is one (a string, a substitution-free template, or a plain
/// decimal number); `None` for every other node.
fn literal_at(pos: Pos, source: &str, type_sources: &HashSet<String>) -> Option<Literal> {
    let node = pos.node;
    if pos.under_error() {
        return None;
    }
    let (value, kind) = match node.kind() {
        "string" => (unescape_js(inner_text(node, source)?), LiteralKind::Str),
        "template_string" => {
            let mut cursor = node.walk();
            let has_substitution = node
                .children(&mut cursor)
                .any(|c| c.kind() == "template_substitution");
            if has_substitution {
                return None;
            }
            (inner_text(node, source)?.to_string(), LiteralKind::Str)
        }
        "number" => (canonical_number(text(node, source))?, LiteralKind::Number),
        _ => return None,
    };
    let mut ctx = literal_ctx(pos, source);
    if !ctx.type_position {
        ctx.type_position = ctx
            .module_const
            .as_ref()
            .is_some_and(|m| type_sources.contains(&m.name));
    }
    Some(Literal {
        value,
        kind,
        line: start_line(node),
        ctx,
    })
}

/// The text between a string/template node's delimiters (one character each side).
fn inner_text<'s>(node: Node, source: &'s str) -> Option<&'s str> {
    let full = text(node, source);
    let mut chars = full.chars();
    let first = chars.next()?;
    let last = chars.next_back()?;
    if !matches!(first, '\'' | '"' | '`') || last != first {
        return None;
    }
    full.get(first.len_utf8()..full.len() - last.len_utf8())
}

/// Resolve JavaScript string escapes. Unknown escapes keep the escaped character; a line
/// continuation (`\` + newline) vanishes.
fn unescape_js(raw: &str) -> String {
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
        match chars.next() {
            None => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some('0') => out.push('\0'),
            Some('\n') => {}
            Some('x') => {
                let hex: String = (0..2)
                    .filter_map(|_| chars.next_if(char::is_ascii_hexdigit))
                    .collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) if hex.len() == 2 => out.push(ch),
                    _ => out.push('x'),
                }
            }
            Some('u') => {
                let code = if chars.next_if_eq(&'{').is_some() {
                    let hex: String =
                        std::iter::from_fn(|| chars.next_if(char::is_ascii_hexdigit)).collect();
                    chars.next_if_eq(&'}');
                    u32::from_str_radix(&hex, 16).ok()
                } else {
                    let hex: String = (0..4)
                        .filter_map(|_| chars.next_if(char::is_ascii_hexdigit))
                        .collect();
                    (hex.len() == 4)
                        .then(|| u32::from_str_radix(&hex, 16).ok())
                        .flatten()
                };
                match code.and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => out.push('u'),
                }
            }
            Some(other) => out.push(other),
        }
    }
    out
}

/// Names used as `typeof NAME` in a type: a module-level `as const` tuple that a type is
/// derived from is a type source, so the literals in it are type members (a union by another
/// route), not free-standing values.
fn typeof_sources(root: Node, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    if !source.contains("typeof") {
        return names;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_query" {
            if let Some(name) = leftmost_identifier(node, source) {
                names.insert(name);
            }
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children);
    }
    names
}

/// The leftmost plain identifier under `node` (`typeof Routes.orders` gives `Routes`).
fn leftmost_identifier(node: Node, source: &str) -> Option<String> {
    let mut cur = node.named_child(0)?;
    loop {
        match cur.kind() {
            "identifier" => return Some(text(cur, source).to_string()),
            "member_expression" | "subscript_expression" | "nested_type_identifier" => {
                cur = cur.named_child(0)?;
            }
            _ => return None,
        }
    }
}

fn literal_ctx(pos: Pos, source: &str) -> LiteralCtx {
    let mut ctx = LiteralCtx {
        import_specifier: is_import_specifier(pos, source),
        ..LiteralCtx::default()
    };
    apply_call_and_jsx(pos, source, &mut ctx);
    apply_key_context(pos, source, &mut ctx);
    ctx.type_position = in_type_position(pos);
    ctx.arithmetic = in_arithmetic_position(pos, source);
    module_const_of(pos, source).apply(&mut ctx);
    ctx
}

/// Whether `node` is the source of an import/re-export, a `require`/`import()` argument, or the
/// first argument of `jest.mock` / `vi.mock` / `require.resolve`.
fn is_import_specifier(pos: Pos, source: &str) -> bool {
    let node = pos.node;
    let Some(up) = pos.parent() else {
        return false;
    };
    let parent = up.node;
    match parent.kind() {
        "import_statement" | "export_statement" | "import_require_clause" => parent
            .child_by_field_name("source")
            .is_some_and(|s| s.id() == node.id()),
        "arguments" => {
            let is_first = first_named_non_comment(parent).is_some_and(|c| c.id() == node.id());
            let Some(call) = up
                .parent()
                .map(|c| c.node)
                .filter(|c| c.kind() == "call_expression")
            else {
                return false;
            };
            let Some(func) = call.child_by_field_name("function") else {
                return false;
            };
            is_first
                && (func.kind() == "import"
                    || (func.kind() == "identifier" && text(func, source) == "require")
                    || (func.kind() == "member_expression"
                        && SPECIFIER_CALLEES.contains(&render_member_path(func, source).as_str())))
        }
        _ => false,
    }
}

/// Fill `callee`, `callee_chain` (the nearest enclosing call) and `jsx_attr` by walking up
/// through [`CALLEE_PASS_THROUGH`] nodes until a call, a JSX attribute or anything else.
fn apply_call_and_jsx(pos: Pos, source: &str, ctx: &mut LiteralCtx) {
    let mut cur = pos;
    while let Some(up) = cur.parent() {
        let parent = up.node;
        match parent.kind() {
            "arguments" => {
                if let Some(call) = up
                    .parent()
                    .map(|c| c.node)
                    .filter(|c| matches!(c.kind(), "call_expression" | "new_expression"))
                {
                    set_callee(call, source, ctx);
                }
                return;
            }
            // A tagged template: the template IS the call's `arguments`.
            "call_expression"
                if parent
                    .child_by_field_name("arguments")
                    .is_some_and(|a| a.id() == cur.node.id()) =>
            {
                set_callee(parent, source, ctx);
                return;
            }
            "jsx_attribute" => {
                ctx.jsx_attr = parent
                    .named_child(0)
                    .map(|n| text(n, source).to_string())
                    .filter(|n| !n.is_empty());
                return;
            }
            kind if CALLEE_PASS_THROUGH.contains(&kind) => cur = up,
            _ => return,
        }
    }
}

fn set_callee(call: Node, source: &str, ctx: &mut LiteralCtx) {
    let func = call
        .child_by_field_name("function")
        .or_else(|| call.child_by_field_name("constructor"));
    let Some(func) = func else {
        return;
    };
    let name = match func.kind() {
        "identifier" => Some(text(func, source).to_string()),
        "import" => Some("import".to_string()),
        "member_expression" => func
            .child_by_field_name("property")
            .map(|p| text(p, source).to_string()),
        _ => None,
    };
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        let chain: String = text(func, source)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .replace("?.", ".");
        ctx.callee = Some(name);
        ctx.callee_chain = Some(chain);
    }
}

/// Fill `object_key`, `value_of_key`, `subscript` and `subscript_receiver`.
fn apply_key_context(pos: Pos, source: &str, ctx: &mut LiteralCtx) {
    let node = pos.node;
    let Some(parent) = pos.parent().map(|p| p.node) else {
        return;
    };
    let is = |field: &str| is_field_of(parent, field, node);
    match parent.kind() {
        "pair" | "pair_pattern" => ctx.object_key = is("key"),
        "method_definition"
        | "public_field_definition"
        | "property_signature"
        | "enum_assignment" => ctx.object_key = is("name"),
        "subscript_expression" => {
            ctx.subscript = is("index");
            if ctx.subscript {
                ctx.subscript_receiver = parent
                    .child_by_field_name("object")
                    .map(|o| text(o, source).to_string());
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

/// The text of an object key: an identifier, a number, or a string's unescaped content. A
/// computed key (`[k]`) has no static text.
fn key_text(key: Node, source: &str) -> Option<String> {
    match key.kind() {
        "property_identifier" | "identifier" | "private_property_identifier" | "number" => {
            Some(text(key, source).to_string())
        }
        "string" => inner_text(key, source).map(unescape_js),
        _ => None,
    }
}

fn in_type_position(pos: Pos) -> bool {
    let mut cur = pos.parent();
    while let Some(p) = cur {
        if TYPE_POSITION_KINDS.contains(&p.node.kind()) {
            return true;
        }
        cur = p.parent();
    }
    false
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
    let operator = |p: Node| {
        p.child_by_field_name("operator")
            .map(|o| text(o, source))
            .unwrap_or_default()
    };
    match parent.kind() {
        "binary_expression" => {
            let op = operator(parent);
            // `'rate ' + 0.12` is string concatenation, not arithmetic.
            let concatenation = op == "+"
                && ["left", "right"]
                    .iter()
                    .filter_map(|f| parent.child_by_field_name(f))
                    .any(|side| {
                        side.id() != cur.id() && matches!(side.kind(), "string" | "template_string")
                    });
            matches!(op, "+" | "-" | "*" | "/" | "%") && !concatenation
        }
        "augmented_assignment_expression" => {
            matches!(operator(parent), "+=" | "-=" | "*=" | "/=" | "%=")
                && parent
                    .child_by_field_name("right")
                    .is_some_and(|r| r.id() == cur.id())
        }
        _ => false,
    }
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

fn module_const_of(pos: Pos, source: &str) -> Home {
    let mut cur = pos;
    loop {
        let Some(up) = cur.parent() else {
            return Home::None;
        };
        let parent = up.node;
        match parent.kind() {
            "variable_declarator" if is_field_of(parent, "value", cur.node) => {
                return declarator_home(up, source);
            }
            "pair" if is_field_of(parent, "value", cur.node) => cur = up,
            // `Object.freeze({ .. })` returns its argument: the constant is still the object.
            "arguments" => match up.parent() {
                Some(call)
                    if is_object_freeze(call.node, source)
                        && first_named_non_comment(parent)
                            .is_some_and(|a| a.id() == cur.node.id()) =>
                {
                    cur = call;
                }
                _ => return Home::None,
            },
            "enum_assignment" if is_field_of(parent, "value", cur.node) => {
                return enum_member_home(up, source);
            }
            "export_statement" if is_field_of(parent, "value", cur.node) => {
                // `export default <expression>`; the declaration forms use another field.
                return match up.parent() {
                    Some(p) if p.node.kind() == "program" => Home::Const(ModuleConst {
                        name: "default".to_string(),
                        exported: true,
                    }),
                    _ => Home::None,
                };
            }
            "assignment_expression" if is_field_of(parent, "right", cur.node) => {
                return commonjs_home(up, source);
            }
            "public_field_definition" if is_field_of(parent, "value", cur.node) => {
                return static_field_home(up, source);
            }
            kind if INITIALIZER_WRAPPERS.contains(&kind) => cur = up,
            _ => return Home::None,
        }
    }
}

fn is_object_freeze(call: Node, source: &str) -> bool {
    call.kind() == "call_expression"
        && call
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "member_expression" && text(f, source) == "Object.freeze")
}

/// `const NAME = ..` / `let` / `var` at module level, inside `export`, or inside a namespace.
fn declarator_home(declarator_pos: Pos, source: &str) -> Home {
    let declarator = declarator_pos.node;
    let Some(name) = declarator.child_by_field_name("name") else {
        return Home::None;
    };
    // A declarator the grammar had to recover around is not a reliable home.
    if name.kind() != "identifier" || declarator.has_error() {
        return Home::None;
    }
    let Some(declaration_pos) = declarator_pos.parent() else {
        return Home::None;
    };
    if !matches!(
        declaration_pos.node.kind(),
        "lexical_declaration" | "variable_declaration"
    ) {
        return Home::None;
    }
    let Some(container_pos) = declaration_pos.parent() else {
        return Home::None;
    };
    let name = text(name, source).to_string();
    let (block, exported) = match container_pos.node.kind() {
        "program" => {
            return Home::Const(ModuleConst {
                name,
                exported: false,
            })
        }
        "export_statement" => match container_pos.parent() {
            Some(p) if p.node.kind() == "program" => {
                return Home::Const(ModuleConst {
                    name,
                    exported: true,
                });
            }
            Some(p) if p.node.kind() == "statement_block" => (p, true),
            _ => return Home::None,
        },
        "statement_block" => (container_pos, false),
        _ => return Home::None,
    };
    match namespace_path(block, source) {
        Some(path) => Home::Const(ModuleConst {
            name: format!("{path}.{name}"),
            exported,
        }),
        None => Home::None,
    }
}

/// The dotted name of the namespace whose body is `block` (`namespace A.B { .. }` is `A.B`,
/// nested namespaces join), when every enclosing construct is a namespace reaching module level.
fn namespace_path(block: Pos, source: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut block = block;
    loop {
        let module = block
            .parent()
            .filter(|m| m.node.kind() == "internal_module")?;
        let name = module.node.child_by_field_name("name")?;
        parts.push(text(name, source).to_string());
        // `namespace X {}` is an expression statement, `export namespace X {}` an export.
        let holder = module
            .parent()
            .filter(|h| matches!(h.node.kind(), "expression_statement" | "export_statement"))?;
        let outer = holder.parent()?;
        match outer.node.kind() {
            "program" => {
                parts.reverse();
                return Some(parts.join("."));
            }
            "statement_block" => block = outer,
            _ => return None,
        }
    }
}

/// `enum E { Member = <literal> }` at module level.
fn enum_member_home(member: Pos, source: &str) -> Home {
    let Some(body) = member.parent() else {
        return Home::None;
    };
    let Some(declaration) = body
        .parent()
        .filter(|d| d.node.kind() == "enum_declaration")
    else {
        return Home::None;
    };
    let Some(enum_name) = declaration.node.child_by_field_name("name") else {
        return Home::None;
    };
    let exported = match declaration.parent() {
        Some(c) if c.node.kind() == "program" => false,
        Some(c) if c.node.kind() == "export_statement" => match c.parent() {
            Some(p) if p.node.kind() == "program" => true,
            _ => return Home::None,
        },
        _ => return Home::None,
    };
    let Some(member_name) = member
        .node
        .child_by_field_name("name")
        .and_then(|n| key_text(n, source))
    else {
        return Home::None;
    };
    Home::Const(ModuleConst {
        name: format!("{}.{member_name}", text(enum_name, source)),
        exported,
    })
}

/// `module.exports = ..` / `exports.NAME = ..` / `module.exports.NAME = ..` as a top-level
/// statement.
fn commonjs_home(assignment: Pos, source: &str) -> Home {
    let Some(left) = assignment.node.child_by_field_name("left") else {
        return Home::None;
    };
    let left_text: String = text(left, source)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let is_commonjs = left.kind() == "member_expression"
        && (left_text == "module.exports"
            || left_text.starts_with("exports.")
            || left_text.starts_with("module.exports."));
    let top_level_statement = assignment
        .parent()
        .filter(|s| s.node.kind() == "expression_statement")
        .and_then(|s| s.parent())
        .is_some_and(|p| p.node.kind() == "program");
    if is_commonjs && top_level_statement {
        Home::Const(ModuleConst {
            name: left_text,
            exported: true,
        })
    } else {
        Home::None
    }
}

/// A `static` class field: `Class.FIELD` is a home when the class is at module level, and
/// unattributed (neither home nor bare) when the class sits anywhere else. An instance field is
/// per-object state and is an ordinary value.
fn static_field_home(field: Pos, source: &str) -> Home {
    let mut cursor = field.node.walk();
    let is_static = field
        .node
        .children(&mut cursor)
        .any(|c| c.kind() == "static");
    if !is_static {
        return Home::None;
    }
    let Some(field_name) = field
        .node
        .child_by_field_name("name")
        .and_then(|n| key_text(n, source))
    else {
        return Home::None;
    };
    let class = field
        .parent()
        .filter(|b| b.node.kind() == "class_body")
        .and_then(|b| b.parent())
        .filter(|c| {
            matches!(
                c.node.kind(),
                "class_declaration" | "abstract_class_declaration"
            )
        });
    let Some(class) = class else {
        return Home::Unattributed;
    };
    let Some(class_name) = class.node.child_by_field_name("name") else {
        return Home::Unattributed;
    };
    let exported = match class.parent() {
        Some(c) if c.node.kind() == "program" => false,
        Some(c)
            if c.node.kind() == "export_statement"
                && c.parent().is_some_and(|p| p.node.kind() == "program") =>
        {
            true
        }
        _ => return Home::Unattributed,
    };
    Home::Const(ModuleConst {
        name: format!("{}.{field_name}", text(class_name, source)),
        exported,
    })
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

    // ── literals ──────────────────────────────────────────────────────────────

    use crate::extract::{canonical_number, Literal, LiteralCtx, LiteralKind, ModuleConst};

    fn lits(dialect: EcmaDialect, src: &str) -> Vec<Literal> {
        literals(dialect, src)
    }

    /// The single literal whose value is `value`; panics when there is not exactly one.
    fn lit(dialect: EcmaDialect, src: &str, value: &str) -> Literal {
        let all = lits(dialect, src);
        let mut hits: Vec<Literal> = all.iter().filter(|l| l.value == value).cloned().collect();
        assert_eq!(hits.len(), 1, "{value:?} in {all:#?}");
        hits.remove(0)
    }

    fn ts(src: &str, value: &str) -> LiteralCtx {
        lit(EcmaDialect::TypeScript, src, value).ctx
    }

    fn tsx(src: &str, value: &str) -> LiteralCtx {
        lit(EcmaDialect::Tsx, src, value).ctx
    }

    #[test]
    fn literal_strings_carry_value_kind_and_line() {
        let src = "const a = 'orders-high';\n\nfoo(\"x:y\");\n";
        let all = lits(EcmaDialect::TypeScript, src);
        assert_eq!(all.len(), 2, "{all:#?}");
        assert_eq!(all[0].value, "orders-high");
        assert_eq!(all[0].kind, LiteralKind::Str);
        assert_eq!(all[0].line, 1);
        assert_eq!(all[1].value, "x:y");
        assert_eq!(all[1].line, 3);
    }

    #[test]
    fn literal_escape_sequences_are_unescaped() {
        let src =
            "f('it\\'s', \"a\\nb\", 'tab\\there', '\\u0041\\x42', '\\u{43}', 'back\\\\slash');\n";
        let values: Vec<String> = lits(EcmaDialect::TypeScript, src)
            .into_iter()
            .map(|l| l.value)
            .collect();
        assert_eq!(
            values,
            ["it's", "a\nb", "tab\there", "AB", "C", "back\\slash"]
        );
    }

    #[test]
    fn literal_empty_string_is_the_empty_value() {
        let all = lits(EcmaDialect::TypeScript, "f('');\n");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].value, "");
    }

    #[test]
    fn literal_template_without_substitution_is_a_literal_with_raw_text() {
        let all = lits(EcmaDialect::TypeScript, "f(`orders-high`);\n");
        assert_eq!(all.len(), 1, "{all:#?}");
        assert_eq!(all[0].value, "orders-high");
        assert_eq!(all[0].kind, LiteralKind::Str);
    }

    #[test]
    fn literal_template_with_substitution_is_skipped_but_nested_strings_are_not() {
        let all = lits(
            EcmaDialect::TypeScript,
            "f(`/api/${v}`, `a-${c ? 'x-y' : 'z-w'}`);\n",
        );
        let values: Vec<&str> = all.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(values, ["x-y", "z-w"], "{all:#?}");
    }

    #[test]
    fn literal_numbers_are_canonical_and_non_decimal_spellings_are_skipped() {
        let src = "a(.12); a(0.120); a(1.2e-1); a(1_0); a(0x1F); a(10n); a(0b11);\n";
        let values: Vec<(String, LiteralKind)> = lits(EcmaDialect::TypeScript, src)
            .into_iter()
            .map(|l| (l.value, l.kind))
            .collect();
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
        assert_eq!(canonical_number(".12").as_deref(), Some("0.12"));
    }

    #[test]
    fn literal_works_for_all_three_dialects() {
        let src = "const a = 'orders-high';\n";
        for d in [
            EcmaDialect::TypeScript,
            EcmaDialect::Tsx,
            EcmaDialect::JavaScript,
        ] {
            assert_eq!(lits(d, src).len(), 1, "{d:?}");
        }
    }

    // ctx.import_specifier

    #[test]
    fn ctx_import_specifier_import_export_from_and_side_effect() {
        for src in [
            "import x from 'react-dom/client';",
            "import { a } from 'react-dom/client';",
            "import 'react-dom/client';",
            "import * as ns from 'react-dom/client';",
            "export * from 'react-dom/client';",
            "export { a } from 'react-dom/client';",
            "import type { T } from 'react-dom/client';",
            "import x = require('react-dom/client');",
        ] {
            assert!(ts(src, "react-dom/client").import_specifier, "{src}");
        }
    }

    #[test]
    fn ctx_import_specifier_require_dynamic_import_and_test_mocks() {
        for src in [
            "const x = require('react-dom/client');",
            "const x = await import('react-dom/client');",
            "jest.mock('react-dom/client');",
            "vi.mock('react-dom/client', () => ({}));",
            "require.resolve('react-dom/client');",
        ] {
            assert!(ts(src, "react-dom/client").import_specifier, "{src}");
        }
    }

    #[test]
    fn ctx_import_specifier_is_false_for_ordinary_calls_and_later_arguments() {
        assert!(!ts("foo('react-dom/client');", "react-dom/client").import_specifier);
        assert!(!ts("jest.mock('x-y', 'react-dom/client');", "react-dom/client").import_specifier);
        assert!(!ts("const a = 'react-dom/client';", "react-dom/client").import_specifier);
        assert!(!ts("requireThing('react-dom/client');", "react-dom/client").import_specifier);
    }

    // ctx.callee, ctx.callee_chain

    #[test]
    fn ctx_callee_is_the_last_identifier_and_chain_is_the_full_text() {
        let c = ts("i18n.t('errors-required');", "errors-required");
        assert_eq!(c.callee.as_deref(), Some("t"));
        assert_eq!(c.callee_chain.as_deref(), Some("i18n.t"));
        let c = ts("logging.getLogger('svc-orders');", "svc-orders");
        assert_eq!(c.callee.as_deref(), Some("getLogger"));
        let c = ts("cn('items-center');", "items-center");
        assert_eq!(c.callee.as_deref(), Some("cn"));
        assert_eq!(c.callee_chain.as_deref(), Some("cn"));
        let c = ts("res.headers.set('x-request-id', v);", "x-request-id");
        assert_eq!(c.callee.as_deref(), Some("set"));
        assert_eq!(c.callee_chain.as_deref(), Some("res.headers.set"));
    }

    #[test]
    fn ctx_callee_walks_up_through_arrays_objects_parentheses_spreads_and_templates() {
        for src in [
            "cn(['items-center']);",
            "cn({ a: 'items-center' });",
            "cn(('items-center'));",
            "cn(...['items-center']);",
            "cn(`${cond ? 'items-center' : ''}`);",
            "cn(cond && 'items-center');",
            "cn(cond ? 'items-center' : 'x-y');",
            "cn(a || 'items-center');",
            "cn('items-center' as string);",
            "cn(['a', { b: ['items-center'] }]);",
        ] {
            assert_eq!(
                ts(src, "items-center").callee.as_deref(),
                Some("cn"),
                "{src}"
            );
        }
    }

    #[test]
    fn ctx_callee_is_the_nearest_call() {
        let c = ts("outer(inner('svc-orders'));", "svc-orders");
        assert_eq!(c.callee.as_deref(), Some("inner"));
    }

    #[test]
    fn ctx_callee_does_not_cross_a_function_boundary() {
        for src in [
            "foo(() => 'svc-orders');",
            "foo(function () { return 'svc-orders'; });",
            "foo(() => { bar = 'svc-orders'; });",
        ] {
            assert_eq!(ts(src, "svc-orders").callee, None, "{src}");
        }
    }

    #[test]
    fn ctx_callee_is_none_outside_any_call() {
        let c = ts("const a = 'svc-orders'; x = 'svc-orders2';", "svc-orders");
        assert_eq!(c.callee, None);
        assert_eq!(c.callee_chain, None);
    }

    #[test]
    fn ctx_callee_covers_new_expressions_and_tagged_templates() {
        let c = ts("new Logger('svc-orders');", "svc-orders");
        assert_eq!(c.callee.as_deref(), Some("Logger"));
        let c = ts("const a = css`flex-start`;", "flex-start");
        assert_eq!(c.callee.as_deref(), Some("css"));
        let c = ts("const a = styled.div`flex-start`;", "flex-start");
        assert_eq!(c.callee.as_deref(), Some("div"));
        assert_eq!(c.callee_chain.as_deref(), Some("styled.div"));
    }

    #[test]
    fn ctx_callee_is_unrecognised_for_computed_callee_chains() {
        let c = ts("getThing()('svc-orders');", "svc-orders");
        assert_eq!(c.callee, None, "a call-result callee has no identifier");
        assert_eq!(c.callee_chain, None);
    }

    // ctx.subscript, ctx.subscript_receiver

    #[test]
    fn ctx_subscript_with_receiver() {
        let c = ts("const a = process.env['X_Y'];", "X_Y");
        assert!(c.subscript);
        assert_eq!(c.subscript_receiver.as_deref(), Some("process.env"));
        let c = ts("const a = row[\"a-b\"];", "a-b");
        assert!(c.subscript);
        assert_eq!(c.subscript_receiver.as_deref(), Some("row"));
    }

    #[test]
    fn ctx_subscript_is_false_for_an_array_literal_and_for_the_receiver_position() {
        assert!(!ts("const a = ['a-b'];", "a-b").subscript);
        assert!(!ts("const a = 'a-b'.length;", "a-b").subscript);
        assert!(!ts("foo['a-b'.length];", "a-b").subscript);
    }

    // ctx.object_key, ctx.value_of_key

    #[test]
    fn ctx_object_key_for_string_keys() {
        let c = ts("const a = { 'x-tenant': 1 };", "x-tenant");
        assert!(c.object_key);
        assert_eq!(c.value_of_key, None);
        assert!(ts("const { 'x-tenant': t } = req;", "x-tenant").object_key);
    }

    #[test]
    fn ctx_value_of_key_names_the_key() {
        let c = ts(
            "const a = { justifyContent: 'space-between' };",
            "space-between",
        );
        assert_eq!(c.value_of_key.as_deref(), Some("justifyContent"));
        assert!(!c.object_key);
        let c = ts("const a = { 'x-env': 'A_B' };", "A_B");
        assert_eq!(c.value_of_key.as_deref(), Some("x-env"));
        let c = ts("const a = { env: cond ? 'a-b' : 'c-d' };", "a-b");
        assert_eq!(c.value_of_key.as_deref(), Some("env"));
        let c = ts("const a = { env: ['a-b'] };", "a-b");
        assert_eq!(c.value_of_key.as_deref(), Some("env"));
    }

    #[test]
    fn ctx_value_of_key_does_not_cross_a_call() {
        let c = ts("const a = { env: foo('a-b') };", "a-b");
        assert_eq!(c.value_of_key, None);
        let c = ts("const a = { env: () => 'a-b' };", "a-b");
        assert_eq!(c.value_of_key, None);
    }

    #[test]
    fn ctx_value_of_key_is_none_for_computed_keys() {
        let c = ts("const a = { [k]: 'a-b' };", "a-b");
        assert_eq!(c.value_of_key, None);
    }

    // ctx.jsx_attr

    #[test]
    fn ctx_jsx_attr_direct_value_and_braced_value() {
        let c = tsx(
            "const a = <div className=\"items-center\" />;",
            "items-center",
        );
        assert_eq!(c.jsx_attr.as_deref(), Some("className"));
        let c = tsx(
            "const a = <div className={'items-center'} />;",
            "items-center",
        );
        assert_eq!(c.jsx_attr.as_deref(), Some("className"));
        let c = tsx(
            "const a = <label htmlFor={ok ? 'items-center' : 'x-y'} />;",
            "items-center",
        );
        assert_eq!(c.jsx_attr.as_deref(), Some("htmlFor"));
        let c = tsx("const a = <div class=\"items-center\" />;", "items-center");
        assert_eq!(c.jsx_attr.as_deref(), Some("class"));
        let c = lit(
            EcmaDialect::JavaScript,
            "const a = <div className=\"items-center\" />;",
            "items-center",
        )
        .ctx;
        assert_eq!(c.jsx_attr.as_deref(), Some("className"));
    }

    /// The JavaScript grammar cannot parse a keyword attribute name (`class=`, `for=`) and
    /// recovers with an error node that re-parents the string as a declarator's value. A
    /// recovered parse must never invent a home out of that.
    #[test]
    fn ctx_module_const_is_none_when_the_declaration_did_not_parse_cleanly() {
        let c = lit(
            EcmaDialect::JavaScript,
            "const a = <div class=\"items-center\" />;",
            "items-center",
        )
        .ctx;
        assert_eq!(c.module_const, None);
    }

    #[test]
    fn ctx_jsx_attr_is_none_outside_attributes_and_behind_a_call() {
        let c = tsx("const a = <div>{'items-center'}</div>;", "items-center");
        assert_eq!(c.jsx_attr, None);
        let c = tsx(
            "const a = <div className={cn('items-center')} />;",
            "items-center",
        );
        assert_eq!(c.jsx_attr, None);
        assert_eq!(c.callee.as_deref(), Some("cn"));
    }

    // ctx.type_position

    #[test]
    fn ctx_type_position_union_and_literal_types() {
        for src in [
            "type S = 'IN_PROGRESS' | 'DONE';",
            "let s: 'IN_PROGRESS' | 'DONE';",
            "function f(s: 'IN_PROGRESS'): void {}",
            "const m: Record<'IN_PROGRESS' | 'DONE', number> = {};",
            "interface I { s: 'IN_PROGRESS' }",
        ] {
            assert!(ts(src, "IN_PROGRESS").type_position, "{src}");
        }
    }

    #[test]
    fn ctx_type_position_enum_member_initializer() {
        assert!(ts("enum S { InProgress = 'IN_PROGRESS' }", "IN_PROGRESS").type_position);
        assert!(ts("const enum S { InProgress = 'IN_PROGRESS' }", "IN_PROGRESS").type_position);
    }

    #[test]
    fn ctx_type_position_const_tuple_used_as_a_type_source() {
        let src = "export const STATUSES = ['IN_PROGRESS', 'DONE'] as const;\n\
                   export type Status = (typeof STATUSES)[number];\n";
        assert!(ts(src, "IN_PROGRESS").type_position);
        let src = "export const STATUSES = ['IN_PROGRESS', 'DONE'] as const;\n\
                   export type Status = typeof STATUSES[number];\n";
        assert!(ts(src, "DONE").type_position);
    }

    #[test]
    fn ctx_type_position_is_false_for_value_uses() {
        for src in [
            "if (s === 'IN_PROGRESS') {}",
            "const a = 'IN_PROGRESS';",
            "switch (s) { case 'IN_PROGRESS': break; }",
            "export const STATUSES = ['IN_PROGRESS', 'DONE'] as const;",
        ] {
            assert!(!ts(src, "IN_PROGRESS").type_position, "{src}");
        }
    }

    // ctx.attribute

    #[test]
    fn ctx_attribute_is_never_set_for_ecma_decorators() {
        let src = "class A { @Get('/api/v1/orders') list() {} }\n";
        assert!(!ts(src, "/api/v1/orders").attribute);
    }

    // ctx.arithmetic

    #[test]
    fn ctx_arithmetic_for_binary_operators_both_sides() {
        for src in [
            "total * 0.12;",
            "0.12 * total;",
            "total / 0.12;",
            "total + 0.12;",
            "total - 0.12;",
            "total % 0.12;",
            "total * (0.12);",
            "const a = (total + fee) * 0.12;",
        ] {
            assert!(ts(src, "0.12").arithmetic, "{src}");
        }
    }

    #[test]
    fn ctx_arithmetic_for_compound_assignment() {
        for src in [
            "x += 0.12;",
            "x -= 0.12;",
            "x *= 0.12;",
            "x /= 0.12;",
            "x %= 0.12;",
        ] {
            assert!(ts(src, "0.12").arithmetic, "{src}");
        }
    }

    #[test]
    fn ctx_arithmetic_is_false_elsewhere() {
        for src in [
            "x < 0.12;",
            "x === 0.12;",
            "foo(0.12);",
            "x = 0.12;",
            "const a = { opacity: 0.12 };",
            "x = [0.12];",
            "x = cond ? 0.12 : 0;",
            "x && 0.12;",
            "x **= 0.12;",
            "x = -0.12;",
        ] {
            assert!(!ts(src, "0.12").arithmetic, "{src}");
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
    fn ctx_module_const_whole_initializer_const_let_var() {
        assert_eq!(
            ts("const Q = 'orders-high';", "orders-high").module_const,
            mc("Q", false)
        );
        assert_eq!(
            ts("let Q = 'orders-high';", "orders-high").module_const,
            mc("Q", false)
        );
        assert_eq!(
            ts("var q = 'orders-high';", "orders-high").module_const,
            mc("q", false)
        );
        assert_eq!(
            ts("export const Q = 'orders-high';", "orders-high").module_const,
            mc("Q", true)
        );
    }

    #[test]
    fn ctx_module_const_through_as_const_objects_and_arrays() {
        let c = ts(
            "export const Routes = { orders: '/api/orders' } as const;",
            "/api/orders",
        );
        assert_eq!(c.module_const, mc("Routes", true));
        let c = ts("export const Q = ['orders-high'] as const;", "orders-high");
        assert_eq!(c.module_const, mc("Q", true));
        let c = ts(
            "const Routes = { a: { b: '/api/orders' } } satisfies Record<string, unknown>;",
            "/api/orders",
        );
        assert_eq!(c.module_const, mc("Routes", false));
        let c = ts("const Q = ('orders-high');", "orders-high");
        assert_eq!(c.module_const, mc("Q", false));
    }

    #[test]
    fn ctx_module_const_is_none_behind_a_call() {
        assert_eq!(
            ts("const Q = f('orders-high');", "orders-high").module_const,
            None
        );
        assert_eq!(
            ts("export const Q = { a: g('orders-high') };", "orders-high").module_const,
            None
        );
        assert_eq!(
            ts("const Q = () => 'orders-high';", "orders-high").module_const,
            None
        );
    }

    #[test]
    fn ctx_module_const_is_none_when_not_top_level() {
        for src in [
            "function f() { const Q = 'orders-high'; }",
            "if (x) { const Q = 'orders-high'; }",
            "class A { Q = 'orders-high'; }",
            "const f = () => { const Q = 'orders-high'; };",
        ] {
            assert_eq!(ts(src, "orders-high").module_const, None, "{src}");
        }
    }

    #[test]
    fn ctx_module_const_is_none_for_destructuring_and_object_keys() {
        assert_eq!(
            ts("const { a } = { a: 'orders-high' };", "orders-high").module_const,
            None
        );
        assert_eq!(
            ts("const Q = { 'orders-high': 1 };", "orders-high").module_const,
            None
        );
    }

    #[test]
    fn literal_malformed_input_does_not_panic() {
        for src in [
            "const a = '",
            "const a = `${",
            "<div className=\"",
            "type T = 'a' |",
            "\u{0}\u{1}\u{2}",
            "enum {",
            "",
        ] {
            for dialect in [
                EcmaDialect::TypeScript,
                EcmaDialect::Tsx,
                EcmaDialect::JavaScript,
            ] {
                let _ = lits(dialect, src);
            }
        }
    }

    // ── more home shapes ──────────────────────────────────────────────────────

    #[test]
    fn ctx_module_const_through_object_freeze() {
        assert_eq!(
            ts(
                "export const Q = Object.freeze({ A: 'orders-high' });",
                "orders-high"
            )
            .module_const,
            mc("Q", true)
        );
        assert_eq!(
            ts(
                "const Q = Object.freeze(['orders-high'] as const);",
                "orders-high"
            )
            .module_const,
            mc("Q", false)
        );
        // Only `Object.freeze` is transparent; any other call is not the constant.
        for src in [
            "const Q = Object.assign({ A: 'orders-high' });",
            "const Q = freeze({ A: 'orders-high' });",
            "const Q = Object.freeze(make('orders-high'));",
            "const Q = Object.freeze(0, 'orders-high');",
        ] {
            assert_eq!(ts(src, "orders-high").module_const, None, "{src}");
        }
    }

    #[test]
    fn ctx_module_const_export_default_is_a_home_named_default() {
        for src in [
            "export default { ORDERS: 'orders-high' };",
            "export default 'orders-high';",
            "export default ['orders-high'];",
            "export default { a: { b: 'orders-high' } } as const;",
        ] {
            assert_eq!(
                ts(src, "orders-high").module_const,
                mc("default", true),
                "{src}"
            );
        }
        for src in [
            "export default defineConfig({ q: 'orders-high' });",
            "export default function f() { return 'orders-high'; }",
            "export default () => 'orders-high';",
        ] {
            assert_eq!(ts(src, "orders-high").module_const, None, "{src}");
        }
    }

    #[test]
    fn ctx_module_const_commonjs_exports() {
        assert_eq!(
            ts("module.exports = { ORDERS: 'orders-high' };", "orders-high").module_const,
            mc("module.exports", true)
        );
        assert_eq!(
            ts("exports.ORDERS = 'orders-high';", "orders-high").module_const,
            mc("exports.ORDERS", true)
        );
        assert_eq!(
            ts("module.exports.ORDERS = 'orders-high';", "orders-high").module_const,
            mc("module.exports.ORDERS", true)
        );
        for src in [
            "window.ORDERS = 'orders-high';",
            "this.q = 'orders-high';",
            "function f() { exports.ORDERS = 'orders-high'; }",
            "module.exports = make('orders-high');",
        ] {
            assert_eq!(ts(src, "orders-high").module_const, None, "{src}");
        }
    }

    #[test]
    fn ctx_module_const_enum_members_are_homes_and_stay_type_positions() {
        let c = ts(
            "export enum Routes { Orders = '/api/orders' }",
            "/api/orders",
        );
        assert_eq!(c.module_const, mc("Routes.Orders", true));
        assert!(c.type_position);
        assert_eq!(
            ts("enum E { A = 'x-y' }", "x-y").module_const,
            mc("E.A", false)
        );
        assert_eq!(
            ts("export const enum K { A = 'ORDERS_HIGH' }", "ORDERS_HIGH").module_const,
            mc("K.A", true)
        );
        assert_eq!(
            ts("function f() { enum E { A = 'x-y' } }", "x-y").module_const,
            None
        );
    }

    #[test]
    fn ctx_module_const_namespace_members() {
        assert_eq!(
            ts(
                "export namespace Q { export const ORDERS = 'orders-high'; }",
                "orders-high"
            )
            .module_const,
            mc("Q.ORDERS", true)
        );
        assert_eq!(
            ts(
                "namespace A.B { export const X = 'orders-high'; }",
                "orders-high"
            )
            .module_const,
            mc("A.B.X", true)
        );
        assert_eq!(
            ts("namespace N { const X = 'orders-high'; }", "orders-high").module_const,
            mc("N.X", false)
        );
        assert_eq!(
            ts(
                "function f() { namespace N { export const X = 'orders-high'; } }",
                "orders-high"
            )
            .module_const,
            None
        );
    }

    #[test]
    fn ctx_module_const_static_class_fields_of_a_module_level_class() {
        assert_eq!(
            ts(
                "export class Q { static ORDERS = 'orders-high'; }",
                "orders-high"
            )
            .module_const,
            mc("Q.ORDERS", true)
        );
        assert_eq!(
            ts(
                "class Q { static readonly ORDERS = 'orders-high'; }",
                "orders-high"
            )
            .module_const,
            mc("Q.ORDERS", false)
        );
        // An instance field is per-object state, not a shared constant.
        assert_eq!(
            ts("class Q { orders = 'orders-high'; }", "orders-high").module_const,
            None
        );
        assert!(!ts("class Q { orders = 'orders-high'; }", "orders-high").unattributed);
        // A static field of a class the walker cannot name is neither a home nor a bare copy.
        for src in [
            "function f() { class Q { static ORDERS = 'orders-high'; } }",
            "const Q = class { static ORDERS = 'orders-high'; };",
        ] {
            let c = ts(src, "orders-high");
            assert_eq!(c.module_const, None, "{src}");
            assert!(c.unattributed, "{src}");
        }
    }

    #[test]
    fn ctx_first_argument_skips_comments() {
        assert!(
            ts(
                "const m = import(/* webpackChunkName: \"x\" */ 'lodash-es');",
                "lodash-es"
            )
            .import_specifier
        );
        assert!(ts("jest.mock(/* c */ 'lodash-es');", "lodash-es").import_specifier);
        assert!(ts("require(// c\n 'lodash-es');", "lodash-es").import_specifier);
        assert!(!ts("foo(/* c */ 'x-y', 'lodash-es');", "lodash-es").import_specifier);
    }

    #[test]
    fn ctx_arithmetic_is_false_for_string_concatenation() {
        for src in [
            "const m = 'rate ' + 0.12;",
            "const m = 0.12 + ' rate';",
            "const m = `rate ` + 0.12;",
            "const m = 0.12 + `${x}`;",
        ] {
            let all = lits(EcmaDialect::TypeScript, src);
            let hit = all.iter().find(|l| l.value == "0.12").unwrap();
            assert!(!hit.ctx.arithmetic, "{src}");
        }
        // `-`, `*` and a numeric `+` are still arithmetic.
        for src in [
            "const m = 'rate' - 0.12;",
            "const m = 'rate' * 0.12;",
            "const m = total + 0.12;",
            "const m = 1 + 0.12;",
            "const m = ('a' + b) * 0.12;",
        ] {
            assert!(ts(src, "0.12").arithmetic, "{src}");
        }
    }

    #[test]
    fn literals_under_error_nodes_are_not_read() {
        // `class` as a JSX attribute name makes the JavaScript grammar recover with an error
        // node; with the TSX re-parse available the literal is read cleanly instead.
        let all = lits(
            EcmaDialect::JavaScript,
            "export const A = () => <div class=\"is-active\">a</div>;\n",
        );
        let hit = all.iter().find(|l| l.value == "is-active").unwrap();
        assert_eq!(hit.ctx.jsx_attr.as_deref(), Some("class"));
        // Unrecoverable input never yields a literal from inside a recovered region.
        let broken = lits(
            EcmaDialect::TypeScript,
            "foo(@@@ 'orders-high' @@@);\nbar('x-y');\n",
        );
        assert!(
            broken.iter().all(|l| l.value != "orders-high"),
            "{broken:#?}"
        );
    }

    // ── computed loads ────────────────────────────────────────────────────────

    fn loads(src: &str) -> Vec<(Option<String>, usize)> {
        computed_loads(EcmaDialect::JavaScript, src)
            .into_iter()
            .map(|l| (l.prefix, l.line))
            .collect()
    }

    fn some(prefix: &str, line: usize) -> (Option<String>, usize) {
        (Some(prefix.to_string()), line)
    }

    #[test]
    fn dynamic_import_of_a_variable_is_a_load_with_no_prefix() {
        assert_eq!(loads("import(name);\n"), vec![(None, 1)]);
    }

    #[test]
    fn dynamic_import_of_a_template_keeps_the_first_quasi() {
        assert_eq!(
            loads("const m = await import(`./locales/${lang}`);\n"),
            vec![some("./locales/", 1)]
        );
    }

    #[test]
    fn require_of_a_variable_is_a_load_with_no_prefix() {
        assert_eq!(loads("\nrequire(name);\n"), vec![(None, 2)]);
    }

    #[test]
    fn require_of_a_concatenation_keeps_the_leftmost_literal() {
        assert_eq!(loads("require('./a' + name);\n"), vec![some("./a", 1)]);
        assert_eq!(
            loads("require('./plugins/' + a + '.js');\n"),
            vec![some("./plugins/", 1)]
        );
    }

    #[test]
    fn concatenation_that_starts_with_a_variable_has_no_prefix() {
        assert_eq!(loads("require(base + '/x');\n"), vec![(None, 1)]);
    }

    #[test]
    fn require_resolve_of_a_variable_is_a_load() {
        assert_eq!(loads("require.resolve(name);\n"), vec![(None, 1)]);
    }

    #[test]
    fn require_context_names_a_directory() {
        assert_eq!(
            loads("const r = require.context('./icons', true, /\\.svg$/);\n"),
            vec![some("./icons/", 1)]
        );
        assert_eq!(
            loads("require.context('./icons/', false);\n"),
            vec![some("./icons/", 1)]
        );
    }

    #[test]
    fn import_meta_glob_keeps_the_literal_head_of_the_pattern() {
        assert_eq!(
            loads("const m = import.meta.glob('./plugins/*.ts');\n"),
            vec![some("./plugins/", 1)]
        );
        assert_eq!(
            loads("import.meta.globEager('./p/**/*.ts');\n"),
            vec![some("./p/", 1)]
        );
        assert_eq!(
            loads("import.meta.glob('./x/{a,b}.ts');\n"),
            vec![some("./x/", 1)]
        );
    }

    #[test]
    fn import_meta_glob_array_reports_each_positive_pattern() {
        assert_eq!(
            loads("import.meta.glob(['./a/*.ts', '!./a/skip.ts', './b/*.ts']);\n"),
            vec![some("./a/", 1), some("./b/", 1)]
        );
    }

    #[test]
    fn literal_import_and_require_are_edges_not_computed_loads() {
        assert!(loads("import('./x');\nrequire('./y');\n").is_empty());
    }

    #[test]
    fn unrelated_calls_are_not_loads() {
        assert!(
            loads("foo(name);\nobj.require(name);\nglob(name);\nimport.meta.url;\n").is_empty()
        );
    }

    #[test]
    fn computed_loads_work_in_the_typescript_dialects() {
        let ts = computed_loads(
            EcmaDialect::TypeScript,
            "const m = await import(`./l/${x}`) as Mod;\n",
        );
        assert_eq!(ts.len(), 1, "{ts:#?}");
        assert_eq!(ts[0].prefix.as_deref(), Some("./l/"));
        let tsx = computed_loads(EcmaDialect::Tsx, "const C = lazy(() => import(name));\n");
        assert_eq!(tsx.len(), 1, "{tsx:#?}");
    }

    #[test]
    fn computed_loads_never_panic_on_bad_input() {
        assert!(loads("").is_empty());
        assert!(loads("require(").len() <= 1);
        let _ = loads("\u{0}\u{1}\u{FFFD} import( ``${");
    }
}
