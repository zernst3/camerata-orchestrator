//! Rust extractor: `syn` 2 (`full` + `visit` features) + `proc-macro2` (`span-locations`).
//!
//! `syn::parse_file` gives a full AST; `syn::visit::Visit` walks it. Line numbers come from
//! `proc-macro2::Span::start().line` (requires the `span-locations` feature — without it
//! every span reports line 0). Attribute TEXT is recovered via `Span::byte_range()` sliced
//! out of the ORIGINAL source string (rather than re-serializing the parsed tokens), so
//! `#[get("/x")]` round-trips byte-for-byte instead of through `quote`'s reformatting.
//!
//! # Never panics
//!
//! `syn::parse_file` returns `Err` (not a panic) on anything it cannot parse — a truncated
//! file, a syntax error, a non-Rust file handed to this extractor by mistake. Every public
//! function here maps that `Err` to an empty `Vec`, never propagating a panic. `visit::Visit`
//! itself cannot panic on a successfully-parsed AST (no `unwrap`/indexing on attacker-shaped
//! data in the visitor below).

use syn::spanned::Spanned;
use syn::visit::Visit;

use super::{FunctionSpan, Import, ImportKind, MethodCall};

/// Extract every `use` (and `extern crate`) declaration. See module docs for the
/// specifier/names convention: `specifier` is the module-path PREFIX, `names` the LEAF
/// binding(s) — `use crate::repositories::user::UserRepo;` yields
/// `specifier = "crate::repositories::user"`, `names = ["UserRepo"]`. An aliased leaf
/// (`... as Alias`) records the ORIGIN name in `names`, not the local alias — the origin name
/// is what a config-driven "forbidden import" list names, and is stable regardless of local
/// renaming (see `ImportKind` docs in `extract::mod`).
pub fn imports(source: &str) -> Vec<Import> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut visitor = ImportVisitor { out: Vec::new() };
    visitor.visit_file(&file);
    visitor.out
}

/// Extract every `fn` item: top-level functions, `impl` methods, and trait methods that carry
/// a default body (a trait fn with NO body has no span to report — skipped). Nested `fn`
/// declarations (a fn defined inside another fn's body) are also visited, since `syn::visit`
/// recurses into a function's block by default.
///
/// `start_line`/`end_line` cover the WHOLE item span, which in `syn`'s `Spanned` impl includes
/// any leading `#[attr]` lines — i.e. `start_line` is the first attribute's line when present,
/// not the `fn` keyword's line. `attrs` carries each attribute's raw source text
/// (`#[get("/x")]`), sliced from `source` by byte range — never a re-serialization.
pub fn functions(source: &str) -> Vec<FunctionSpan> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut visitor = FnVisitor { source, out: Vec::new() };
    visitor.visit_file(&file);
    visitor.out
}

/// Extract every `receiver.method(...)` call site (`syn::Expr::MethodCall`). `receiver` is a
/// best-effort rendering: a bare identifier or a `self.field`/`a.b.c` field-access chain
/// renders verbatim; anything more complex (a call, an index, a cast, ...) renders as the
/// placeholder `"<expr>"` — never a guess, never a panic.
pub fn method_calls(source: &str) -> Vec<MethodCall> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut visitor = CallVisitor { out: Vec::new() };
    visitor.visit_file(&file);
    visitor.out
}

// ─── imports ─────────────────────────────────────────────────────────────────────────────

struct ImportVisitor {
    out: Vec<Import>,
}

impl<'ast> Visit<'ast> for ImportVisitor {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        let line = node.span().start().line;
        let mut prefix = Vec::new();
        walk_use_tree(&node.tree, &mut prefix, line, &mut self.out);
        // Deliberately do not call the default `visit_item_use` walker further — `UseTree`
        // has no nested items to descend into beyond what `walk_use_tree` already covers.
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        self.out.push(Import {
            specifier: node.ident.to_string(),
            names: Vec::new(),
            kind: ImportKind::SideEffect,
            line: node.span().start().line,
        });
    }
}

/// Recursively flatten a `syn::UseTree` into zero or more [`Import`]s, threading the
/// accumulated path-segment `prefix` (only `UseTree::Path` segments contribute to it — the
/// final leaf segment becomes `names`, not part of `specifier`, matching the design's
/// "module path + imported symbol" split).
fn walk_use_tree(tree: &syn::UseTree, prefix: &mut Vec<String>, line: usize, out: &mut Vec<Import>) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            walk_use_tree(&p.tree, prefix, line, out);
            prefix.pop();
        }
        syn::UseTree::Name(n) => {
            let name = n.ident.to_string();
            if name == "self" {
                // `use foo::bar::{self, ...}` imports the module `foo::bar` ITSELF as a
                // binding — a namespace-shaped import of the accumulated prefix.
                out.push(Import {
                    specifier: prefix.join("::"),
                    names: Vec::new(),
                    kind: ImportKind::Namespace,
                    line,
                });
            } else {
                out.push(Import {
                    specifier: prefix.join("::"),
                    names: vec![name],
                    kind: ImportKind::Named,
                    line,
                });
            }
        }
        syn::UseTree::Rename(r) => {
            // Origin name (pre-`as`) recorded in `names`, per the module-doc rationale.
            out.push(Import {
                specifier: prefix.join("::"),
                names: vec![r.ident.to_string()],
                kind: ImportKind::Named,
                line,
            });
        }
        syn::UseTree::Glob(_) => {
            out.push(Import {
                specifier: prefix.join("::"),
                names: Vec::new(),
                kind: ImportKind::Namespace,
                line,
            });
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                walk_use_tree(item, prefix, line, out);
            }
        }
    }
}

// ─── functions ───────────────────────────────────────────────────────────────────────────

struct FnVisitor<'s> {
    source: &'s str,
    out: Vec<FunctionSpan>,
}

impl<'s> FnVisitor<'s> {
    /// Render each attribute's raw source text via its byte range, skipping (rather than
    /// panicking on) any span whose byte range falls outside `source` — defensive only; a
    /// successfully-parsed file's spans always index into the exact source it was parsed
    /// from, so this should never trigger in practice.
    fn attr_texts(&self, attrs: &[syn::Attribute]) -> Vec<String> {
        attrs
            .iter()
            .filter_map(|a| {
                let range = a.span().byte_range();
                self.source.get(range).map(|s| s.to_string())
            })
            .collect()
    }
}

impl<'s, 'ast> Visit<'ast> for FnVisitor<'s> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let span = node.span();
        self.out.push(FunctionSpan {
            name: node.sig.ident.to_string(),
            start_line: span.start().line,
            end_line: span.end().line,
            attrs: self.attr_texts(&node.attrs),
        });
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let span = node.span();
        self.out.push(FunctionSpan {
            name: node.sig.ident.to_string(),
            start_line: span.start().line,
            end_line: span.end().line,
            attrs: self.attr_texts(&node.attrs),
        });
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        // Only a DEFAULT-bodied trait fn has a real body span worth reporting; a signature
        // with no body is a declaration, not a definition.
        if node.default.is_some() {
            let span = node.span();
            self.out.push(FunctionSpan {
                name: node.sig.ident.to_string(),
                start_line: span.start().line,
                end_line: span.end().line,
                attrs: self.attr_texts(&node.attrs),
            });
        }
        syn::visit::visit_trait_item_fn(self, node);
    }
}

// ─── method calls ────────────────────────────────────────────────────────────────────────

struct CallVisitor {
    out: Vec<MethodCall>,
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.out.push(MethodCall {
            receiver: render_receiver(&node.receiver),
            method: node.method.to_string(),
            line: node.method.span().start().line,
        });
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Best-effort render of a method call's receiver expression. A bare identifier (`db`,
/// `self`) or a field-access chain (`self.db`, `this.repo.conn`) renders verbatim; anything
/// syntactically richer (a call, an index, a cast, a macro, ...) renders as the placeholder
/// `"<expr>"` — never a guessed name, never a panic on an unmatched variant.
fn render_receiver(expr: &syn::Expr) -> String {
    match expr {
        syn::Expr::Path(p) if p.qself.is_none() && !p.path.segments.is_empty() => p
            .path
            .segments
            .iter()
            .map(|seg| seg.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        syn::Expr::Field(f) => {
            let base = render_receiver(&f.base);
            let member = match &f.member {
                syn::Member::Named(id) => id.to_string(),
                syn::Member::Unnamed(idx) => idx.index.to_string(),
            };
            format!("{base}.{member}")
        }
        syn::Expr::Paren(p) => render_receiver(&p.expr),
        _ => "<expr>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ImportKind;

    // ── imports ───────────────────────────────────────────────────────────────

    #[test]
    fn simple_named_import() {
        let vs = imports("use crate::repositories::user::UserRepo;\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "crate::repositories::user");
        assert_eq!(vs[0].names, vec!["UserRepo"]);
        assert_eq!(vs[0].kind, ImportKind::Named);
        assert_eq!(vs[0].line, 1);
    }

    #[test]
    fn grouped_import_with_alias_and_plain() {
        let vs = imports("use foo::bar::{Baz, Qux as Quux};\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().any(|i| i.specifier == "foo::bar" && i.names == vec!["Baz".to_string()]));
        // Aliased leaf records the ORIGIN name (Qux), not the local alias (Quux).
        assert!(vs.iter().any(|i| i.specifier == "foo::bar" && i.names == vec!["Qux".to_string()]));
    }

    #[test]
    fn glob_import() {
        let vs = imports("use foo::bar::*;\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "foo::bar");
        assert!(vs[0].names.is_empty());
        assert_eq!(vs[0].kind, ImportKind::Namespace);
    }

    #[test]
    fn self_leaf_imports_the_module_itself() {
        let vs = imports("use foo::bar::{self, Baz};\n");
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert!(vs.iter().any(|i| i.specifier == "foo::bar" && i.kind == ImportKind::Namespace && i.names.is_empty()));
        assert!(vs.iter().any(|i| i.names == vec!["Baz".to_string()]));
    }

    #[test]
    fn extern_crate_is_side_effect() {
        let vs = imports("extern crate serde;\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "serde");
        assert_eq!(vs[0].kind, ImportKind::SideEffect);
    }

    #[test]
    fn nested_mod_use_is_found() {
        let vs = imports("mod inner {\n    use crate::domain::Order;\n}\n");
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].specifier, "crate::domain");
        assert_eq!(vs[0].names, vec!["Order"]);
    }

    #[test]
    fn fn_local_use_is_found() {
        let vs = imports("fn f() {\n    use std::collections::HashMap;\n    let _m: HashMap<u8,u8> = HashMap::new();\n}\n");
        assert!(vs.iter().any(|i| i.specifier == "std::collections" && i.names == vec!["HashMap".to_string()]));
    }

    // ── functions ─────────────────────────────────────────────────────────────

    #[test]
    fn top_level_fn_with_attr() {
        let src = "#[get(\"/x\")]\nfn handler() {\n    ()\n}\n";
        let fs = functions(src);
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "handler");
        assert_eq!(fs[0].attrs, vec!["#[get(\"/x\")]".to_string()]);
        assert_eq!(fs[0].start_line, 1);
        assert_eq!(fs[0].end_line, 4);
    }

    #[test]
    fn impl_method_is_found() {
        let src = "struct S;\nimpl S {\n    fn method(&self) {}\n}\n";
        let fs = functions(src);
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "method");
    }

    #[test]
    fn trait_default_method_is_found_but_signature_only_is_not() {
        let src = "trait T {\n    fn required(&self);\n    fn provided(&self) { }\n}\n";
        let fs = functions(src);
        assert_eq!(fs.len(), 1, "{fs:#?}");
        assert_eq!(fs[0].name, "provided");
    }

    #[test]
    fn nested_fn_inside_fn_body_is_found() {
        let src = "fn outer() {\n    fn inner() {}\n    inner();\n}\n";
        let fs = functions(src);
        assert_eq!(fs.len(), 2, "{fs:#?}");
        assert!(fs.iter().any(|f| f.name == "outer"));
        assert!(fs.iter().any(|f| f.name == "inner"));
    }

    // ── method calls ──────────────────────────────────────────────────────────

    #[test]
    fn field_chain_receiver_renders_verbatim() {
        let src = "fn h() {\n    self.db.query();\n}\n";
        let cs = method_calls(src);
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "self.db");
        assert_eq!(cs[0].method, "query");
    }

    #[test]
    fn bare_identifier_receiver() {
        let cs = method_calls("fn h() {\n    db.execute();\n}\n");
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "db");
        assert_eq!(cs[0].method, "execute");
    }

    #[test]
    fn complex_receiver_renders_as_expr_placeholder() {
        let cs = method_calls("fn h() {\n    make_conn().query();\n}\n");
        assert_eq!(cs.len(), 1, "{cs:#?}");
        assert_eq!(cs[0].receiver, "<expr>");
        assert_eq!(cs[0].method, "query");
    }

    #[test]
    fn chained_calls_both_captured() {
        let cs = method_calls("fn h() {\n    self.db.query().execute();\n}\n");
        assert_eq!(cs.len(), 2, "{cs:#?}");
        assert!(cs.iter().any(|c| c.method == "query" && c.receiver == "self.db"));
        assert!(cs.iter().any(|c| c.method == "execute" && c.receiver == "<expr>"));
    }

    // ── adversarial: malformed / truncated input must never panic ────────────

    #[test]
    fn truncated_source_does_not_panic() {
        assert!(imports("use crate::").is_empty());
        assert!(functions("fn broken( {{{ ???").is_empty());
        assert!(method_calls("fn h() { self.db.").is_empty());
    }

    #[test]
    fn binary_like_content_does_not_panic() {
        let weird = "\u{0}\u{1}\u{FFFD} not rust at all \u{FFFD}";
        assert!(imports(weird).is_empty());
        assert!(functions(weird).is_empty());
        assert!(method_calls(weird).is_empty());
    }

    #[test]
    fn empty_source_does_not_panic() {
        assert!(imports("").is_empty());
        assert!(functions("").is_empty());
        assert!(method_calls("").is_empty());
    }

    #[test]
    fn deeply_nested_but_valid_source_does_not_stack_overflow() {
        // A moderately deep nesting of blocks/impls is realistic (not the adversarial
        // "megabyte of parens" case, which syn's own recursive-descent parser bounds
        // separately) — this just proves our visitor recursion is well-behaved.
        let mut src = String::new();
        for i in 0..50 {
            src.push_str(&format!("mod m{i} {{\n"));
        }
        src.push_str("pub fn leaf() { self.a.b(); }\n");
        for _ in 0..50 {
            src.push_str("}\n");
        }
        let fs = functions(&src);
        assert!(fs.iter().any(|f| f.name == "leaf"));
    }
}
