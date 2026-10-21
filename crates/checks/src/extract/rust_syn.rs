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

use super::{
    canonical_number, FunctionSpan, Import, ImportKind, Literal, LiteralCtx, LiteralKind,
    MethodCall, ModuleConst,
};

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
    let mut visitor = FnVisitor {
        source,
        out: Vec::new(),
    };
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
            type_only: false,
        });
    }
}

/// Recursively flatten a `syn::UseTree` into zero or more [`Import`]s, threading the
/// accumulated path-segment `prefix` (only `UseTree::Path` segments contribute to it — the
/// final leaf segment becomes `names`, not part of `specifier`, matching the design's
/// "module path + imported symbol" split).
fn walk_use_tree(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    line: usize,
    out: &mut Vec<Import>,
) {
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
                    type_only: false,
                });
            } else {
                out.push(Import {
                    specifier: prefix.join("::"),
                    names: vec![name],
                    kind: ImportKind::Named,
                    line,
                    type_only: false,
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
                type_only: false,
            });
        }
        syn::UseTree::Glob(_) => {
            out.push(Import {
                specifier: prefix.join("::"),
                names: Vec::new(),
                kind: ImportKind::Namespace,
                line,
                type_only: false,
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

// ─── literals ────────────────────────────────────────────────────────────────────────────

/// Extract every string and number literal with its [`LiteralCtx`]. Strings are `Lit::Str`
/// (raw strings included, byte strings not); numbers are `Lit::Float` and `Lit::Int` in their
/// canonical decimal text. **Macro invocation bodies are not descended** (a token stream has
/// no expression positions, so no context guard could apply to what is inside), but the token
/// streams of attributes ARE walked for string literals, with `attribute` set.
pub fn literals(source: &str) -> Vec<Literal> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut visitor = LiteralVisitor::default();
    visitor.visit_file(&file);
    visitor.out
}

/// The nesting context a literal inherits while the visitor descends: the nearest enclosing
/// call, the struct-literal field it is the value of, and the top-level constant it
/// initializes. A construct that is not a plain value wrapper (a block, a closure, a method
/// receiver, a call's own arguments) starts a fresh frame.
#[derive(Clone, Default)]
struct Frame {
    /// `(last identifier, full callee text)` of the nearest enclosing call.
    callee: Option<(String, String)>,
    key: Option<String>,
    module_const: Option<ModuleConst>,
}

#[derive(Default)]
struct LiteralVisitor {
    out: Vec<Literal>,
    frame: Frame,
    /// Set by a binary arithmetic expression for the operand about to be visited; consumed (and
    /// cleared) by the very next [`LiteralVisitor::visit_expr`], so only a DIRECT operand sees it.
    arith_next: bool,
    /// The receiver text, set by an index expression for its index operand, consumed likewise.
    subscript_next: Option<String>,
    /// How many blocks enclose the visitor; a `const` inside a block is local, not module-level.
    block_depth: usize,
}

impl LiteralVisitor {
    fn with_frame<R>(&mut self, frame: Frame, body: impl FnOnce(&mut Self) -> R) -> R {
        let saved = std::mem::replace(&mut self.frame, frame);
        let result = body(self);
        self.frame = saved;
        result
    }

    fn push(&mut self, value: String, kind: LiteralKind, line: usize, ctx: LiteralCtx) {
        self.out.push(Literal {
            value,
            kind,
            line,
            ctx,
        });
    }

    fn emit(&mut self, lit: &syn::Lit, arithmetic: bool, subscript: Option<String>) {
        let (value, kind) = match lit {
            syn::Lit::Str(s) => (s.value(), LiteralKind::Str),
            syn::Lit::Float(f) => match canonical_number(f.base10_digits()) {
                Some(v) => (v, LiteralKind::Number),
                None => return,
            },
            syn::Lit::Int(i) => match canonical_number(i.base10_digits()) {
                Some(v) => (v, LiteralKind::Number),
                None => return,
            },
            _ => return,
        };
        let ctx = LiteralCtx {
            callee: self.frame.callee.as_ref().map(|(name, _)| name.clone()),
            callee_chain: self.frame.callee.as_ref().map(|(_, chain)| chain.clone()),
            value_of_key: self.frame.key.clone(),
            module_const: self.frame.module_const.clone(),
            arithmetic,
            subscript: subscript.is_some(),
            subscript_receiver: subscript,
            ..LiteralCtx::default()
        };
        self.push(value, kind, lit.span().start().line, ctx);
    }

    /// A `const`/`static` initializer, visited as the home of its name when the item is
    /// module-level (not inside a block), as an ordinary expression otherwise.
    fn visit_const_initializer(&mut self, name: &syn::Ident, public: bool, expr: &syn::Expr) {
        let module_const = (self.block_depth == 0).then(|| ModuleConst {
            name: name.to_string(),
            exported: public,
        });
        self.with_frame(
            Frame {
                module_const,
                ..Frame::default()
            },
            |v| v.visit_expr(expr),
        );
    }

    fn visit_attribute_strings(&mut self, attr: &syn::Attribute) {
        match &attr.meta {
            // `/// text` is `#[doc = " text"]`: prose, not a value.
            syn::Meta::NameValue(nv) if !attr.path().is_ident("doc") => {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    self.push_attribute_string(s.value(), s.span().start().line);
                }
            }
            syn::Meta::List(list) => self.walk_attribute_tokens(list.tokens.clone()),
            syn::Meta::NameValue(_) | syn::Meta::Path(_) => {}
        }
    }

    fn walk_attribute_tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for tree in tokens {
            match tree {
                proc_macro2::TokenTree::Literal(l) => {
                    if let Ok(s) = syn::parse_str::<syn::LitStr>(&l.to_string()) {
                        self.push_attribute_string(s.value(), l.span().start().line);
                    }
                }
                proc_macro2::TokenTree::Group(g) => self.walk_attribute_tokens(g.stream()),
                _ => {}
            }
        }
    }

    fn push_attribute_string(&mut self, value: String, line: usize) {
        self.push(
            value,
            LiteralKind::Str,
            line,
            LiteralCtx {
                attribute: true,
                ..LiteralCtx::default()
            },
        );
    }
}

/// `+ - * / %` and their compound-assignment forms.
fn is_arithmetic(op: &syn::BinOp) -> bool {
    use syn::BinOp::{
        Add, AddAssign, Div, DivAssign, Mul, MulAssign, Rem, RemAssign, Sub, SubAssign,
    };
    matches!(
        op,
        Add(_)
            | Sub(_)
            | Mul(_)
            | Div(_)
            | Rem(_)
            | AddAssign(_)
            | SubAssign(_)
            | MulAssign(_)
            | DivAssign(_)
            | RemAssign(_)
    )
}

/// The `(last identifier, full text)` of a call's function expression, when it is a path.
fn callee_of_path(func: &syn::Expr) -> Option<(String, String)> {
    let syn::Expr::Path(p) = func else {
        return None;
    };
    if p.qself.is_some() {
        return None;
    }
    let last = p.path.segments.last()?.ident.to_string();
    Some((last, render_receiver(func)))
}

impl<'ast> Visit<'ast> for LiteralVisitor {
    // A macro body is a bare token stream: nothing in it has a position the guards could use.
    fn visit_macro(&mut self, _node: &'ast syn::Macro) {}

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        self.visit_attribute_strings(node);
    }

    fn visit_lit(&mut self, node: &'ast syn::Lit) {
        // Reached only for literals outside expression position (match-arm patterns).
        self.emit(node, false, None);
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.block_depth += 1;
        self.with_frame(Frame::default(), |v| syn::visit::visit_block(v, node));
        self.block_depth -= 1;
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        let public = matches!(node.vis, syn::Visibility::Public(_));
        self.visit_const_initializer(&node.ident, public, &node.expr);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        let public = matches!(node.vis, syn::Visibility::Public(_));
        self.visit_const_initializer(&node.ident, public, &node.expr);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        let public = matches!(node.vis, syn::Visibility::Public(_));
        self.visit_const_initializer(&node.ident, public, &node.expr);
    }

    fn visit_trait_item_const(&mut self, node: &'ast syn::TraitItemConst) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        if let Some((_, expr)) = &node.default {
            self.visit_const_initializer(&node.ident, false, expr);
        }
    }

    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        let arithmetic = std::mem::take(&mut self.arith_next);
        let subscript = self.subscript_next.take();
        match node {
            syn::Expr::Lit(l) => {
                for attr in &l.attrs {
                    self.visit_attribute(attr);
                }
                self.emit(&l.lit, arithmetic, subscript);
            }
            // Grouping keeps a literal "direct": `total * (0.12)`.
            syn::Expr::Paren(p) => {
                self.arith_next = arithmetic;
                self.subscript_next = subscript;
                self.visit_expr(&p.expr);
            }
            syn::Expr::Group(g) => {
                self.arith_next = arithmetic;
                self.subscript_next = subscript;
                self.visit_expr(&g.expr);
            }
            syn::Expr::Binary(b) => {
                let arith = is_arithmetic(&b.op);
                self.arith_next = arith;
                self.visit_expr(&b.left);
                self.arith_next = arith;
                self.visit_expr(&b.right);
            }
            syn::Expr::Call(c) => {
                self.with_frame(Frame::default(), |v| v.visit_expr(&c.func));
                let frame = Frame {
                    callee: callee_of_path(&c.func),
                    ..Frame::default()
                };
                self.with_frame(frame, |v| {
                    for arg in &c.args {
                        v.visit_expr(arg);
                    }
                });
            }
            syn::Expr::MethodCall(m) => {
                self.with_frame(Frame::default(), |v| v.visit_expr(&m.receiver));
                let method = m.method.to_string();
                let chain = format!("{}.{method}", render_receiver(&m.receiver));
                let frame = Frame {
                    callee: Some((method, chain)),
                    ..Frame::default()
                };
                self.with_frame(frame, |v| {
                    for arg in &m.args {
                        v.visit_expr(arg);
                    }
                });
            }
            syn::Expr::Index(i) => {
                self.with_frame(Frame::default(), |v| v.visit_expr(&i.expr));
                self.subscript_next = Some(render_receiver(&i.expr));
                self.with_frame(Frame::default(), |v| v.visit_expr(&i.index));
                self.subscript_next = None;
            }
            syn::Expr::Struct(st) => {
                for attr in &st.attrs {
                    self.visit_attribute(attr);
                }
                for field in &st.fields {
                    let key = match &field.member {
                        syn::Member::Named(id) => Some(id.to_string()),
                        syn::Member::Unnamed(_) => None,
                    };
                    let frame = Frame {
                        key,
                        ..self.frame.clone()
                    };
                    self.with_frame(frame, |v| v.visit_expr(&field.expr));
                }
                if let Some(rest) = &st.rest {
                    self.with_frame(Frame::default(), |v| v.visit_expr(rest));
                }
            }
            // Plain value wrappers keep the frame; every other construct starts a fresh one.
            syn::Expr::Array(_)
            | syn::Expr::Tuple(_)
            | syn::Expr::Reference(_)
            | syn::Expr::Unary(_)
            | syn::Expr::Cast(_)
            | syn::Expr::Path(_)
            | syn::Expr::Macro(_) => syn::visit::visit_expr(self, node),
            _ => self.with_frame(Frame::default(), |v| syn::visit::visit_expr(v, node)),
        }
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
        assert!(vs
            .iter()
            .any(|i| i.specifier == "foo::bar" && i.names == vec!["Baz".to_string()]));
        // Aliased leaf records the ORIGIN name (Qux), not the local alias (Quux).
        assert!(vs
            .iter()
            .any(|i| i.specifier == "foo::bar" && i.names == vec!["Qux".to_string()]));
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
        assert!(vs.iter().any(|i| i.specifier == "foo::bar"
            && i.kind == ImportKind::Namespace
            && i.names.is_empty()));
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
        assert!(vs
            .iter()
            .any(|i| i.specifier == "std::collections" && i.names == vec!["HashMap".to_string()]));
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
        assert!(cs
            .iter()
            .any(|c| c.method == "query" && c.receiver == "self.db"));
        assert!(cs
            .iter()
            .any(|c| c.method == "execute" && c.receiver == "<expr>"));
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
        let all = lits("fn f() {\n    let a = \"orders-high\";\n\n    g(\"x:y\");\n}\n");
        assert_eq!(all.len(), 2, "{all:#?}");
        assert_eq!(all[0].value, "orders-high");
        assert_eq!(all[0].kind, LiteralKind::Str);
        assert_eq!(all[0].line, 2);
        assert_eq!(all[1].value, "x:y");
        assert_eq!(all[1].line, 4);
    }

    #[test]
    fn literal_escapes_raw_strings_and_byte_strings() {
        let src = "fn f() { g(\"a\\nb\", r\"a\\nb\", r#\"q\"x\"#, b\"orders-high\", 'c'); }\n";
        let values: Vec<String> = lits(src).into_iter().map(|l| l.value).collect();
        assert_eq!(values, ["a\nb", "a\\nb", "q\"x"]);
    }

    #[test]
    fn literal_numbers_are_canonical() {
        let src = "fn f() { g(0.12, 0.120f64, 0.12_f32, 1e-1, 1_000, 0xFF, 3u8); }\n";
        let values: Vec<(String, LiteralKind)> =
            lits(src).into_iter().map(|l| (l.value, l.kind)).collect();
        let n = LiteralKind::Number;
        let expected: Vec<(String, LiteralKind)> =
            ["0.12", "0.12", "0.12", "0.1", "1000", "255", "3"]
                .iter()
                .map(|v| (v.to_string(), n))
                .collect();
        assert_eq!(values, expected);
    }

    #[test]
    fn literal_macro_bodies_are_not_descended() {
        let src = "fn f() {\n\
            println!(\"orders-high\");\n\
            let a = format!(\"/api/{}\", \"x-y\");\n\
            let j = json!({\"q\": \"orders-high\"});\n\
            let v = vec![\"a-b\", \"c-d\"];\n\
            sqlx::query!(\"select 1\");\n\
        }\n";
        assert!(lits(src).is_empty(), "{:#?}", lits(src));
    }

    #[test]
    fn literal_match_arm_patterns_are_literals() {
        let src = "fn f(s: &str) -> u8 { match s { \"ADD_TODO\" => 1, _ => 0 } }\n";
        assert!(lits(src).iter().any(|l| l.value == "ADD_TODO"));
    }

    // ctx.attribute

    #[test]
    fn ctx_attribute_for_route_serde_cfg_and_nested_attribute_strings() {
        for (src, value) in [
            (
                "#[get(\"/api/v1/orders\")]\nasync fn a() {}\n",
                "/api/v1/orders",
            ),
            (
                "#[serde(rename = \"created-at\")]\nstruct S { a: u8 }\n",
                "created-at",
            ),
            (
                "#[cfg(feature = \"extra-fast\")]\nfn f() {}\n",
                "extra-fast",
            ),
            (
                "#[cfg_attr(feature = \"a-b\", serde(rename = \"x-y\"))]\nstruct S;\n",
                "x-y",
            ),
            ("#[path = \"x-y.rs\"]\nmod m;\n", "x-y.rs"),
            (
                "struct S {\n    #[serde(rename = \"x-y\")]\n    a: u8,\n}\n",
                "x-y",
            ),
            (
                "enum E {\n    #[serde(rename = \"x-y\")]\n    A,\n}\n",
                "x-y",
            ),
            (
                "#![allow(clippy::x)]\n#![doc(html_root_url = \"x-y\")]\nfn f() {}\n",
                "x-y",
            ),
        ] {
            assert!(ctx(src, value).attribute, "{src}");
        }
    }

    #[test]
    fn ctx_attribute_is_false_for_code_and_doc_comments_are_not_literals() {
        assert!(!ctx("fn f() { g(\"x-y\"); }\n", "x-y").attribute);
        let src = "/// orders-high is documented here\n#[doc = \"orders-high\"]\nfn f() {}\n";
        assert!(lits(src).is_empty(), "{:#?}", lits(src));
    }

    #[test]
    fn ctx_attribute_strings_in_nested_groups_are_found_with_their_line() {
        let src = "\n#[outer(inner(deep(\"x-y\")))]\nfn f() {}\n";
        let l = lit(src, "x-y");
        assert!(l.ctx.attribute);
        assert_eq!(l.line, 2);
    }

    // ctx.callee, ctx.callee_chain

    #[test]
    fn ctx_callee_for_path_calls_and_method_calls() {
        let c = ctx("fn f() { std::env::var(\"X_Y\"); }\n", "X_Y");
        assert_eq!(c.callee.as_deref(), Some("var"));
        assert_eq!(c.callee_chain.as_deref(), Some("std::env::var"));
        let c = ctx(
            "fn f() { client.bucket(\"user-uploads\"); }\n",
            "user-uploads",
        );
        assert_eq!(c.callee.as_deref(), Some("bucket"));
        assert_eq!(c.callee_chain.as_deref(), Some("client.bucket"));
        let c = ctx(
            "fn f() { self.db.headers.set(\"x-request-id\"); }\n",
            "x-request-id",
        );
        assert_eq!(c.callee.as_deref(), Some("set"));
        assert_eq!(c.callee_chain.as_deref(), Some("self.db.headers.set"));
        let c = ctx("fn f() { cn(\"items-center\"); }\n", "items-center");
        assert_eq!(c.callee.as_deref(), Some("cn"));
        assert_eq!(c.callee_chain.as_deref(), Some("cn"));
    }

    #[test]
    fn ctx_callee_walks_up_through_arrays_tuples_references_structs_and_operators() {
        for src in [
            "fn f() { cn(&[\"items-center\"]); }\n",
            "fn f() { cn((\"items-center\", 1)); }\n",
            "fn f() { cn(&\"items-center\"); }\n",
            "fn f() { cn((\"items-center\")); }\n",
            "fn f() { cn(S { a: \"items-center\" }); }\n",
            "fn f() { cn(c || \"items-center\" == x); }\n",
            "fn f() { cn(if c { 1 } else { 2 } + \"items-center\".len()); }\n",
        ] {
            let all = lits(src);
            let hit = all.iter().find(|l| l.value == "items-center").unwrap();
            // The last form holds a receiver literal, which is not an argument.
            if src.contains(".len()") {
                assert_eq!(hit.ctx.callee, None, "{src}");
            } else {
                assert_eq!(hit.ctx.callee.as_deref(), Some("cn"), "{src}");
            }
        }
    }

    #[test]
    fn ctx_callee_is_the_nearest_call_and_stops_at_blocks_and_closures() {
        let c = ctx("fn f() { outer(inner(\"svc-orders\")); }\n", "svc-orders");
        assert_eq!(c.callee.as_deref(), Some("inner"));
        assert_eq!(
            ctx("fn f() { foo(|| \"svc-orders\"); }\n", "svc-orders").callee,
            None
        );
        assert_eq!(
            ctx(
                "fn f() { foo({ let a = \"svc-orders\"; a }); }\n",
                "svc-orders"
            )
            .callee,
            None
        );
    }

    #[test]
    fn ctx_callee_is_none_for_a_method_receiver_and_outside_calls() {
        assert_eq!(
            ctx("fn f() { \"svc-orders\".to_string(); }\n", "svc-orders").callee,
            None
        );
        assert_eq!(
            ctx("fn f() { let a = \"svc-orders\"; }\n", "svc-orders").callee,
            None
        );
    }

    #[test]
    fn ctx_callee_is_none_for_a_computed_callee() {
        let c = ctx("fn f() { (get())(\"svc-orders\"); }\n", "svc-orders");
        assert_eq!(c.callee, None);
        assert_eq!(c.callee_chain, None);
    }

    // ctx.subscript

    #[test]
    fn ctx_subscript_with_receiver() {
        let c = ctx("fn f() { let a = row[\"a-b\"]; }\n", "a-b");
        assert!(c.subscript);
        assert_eq!(c.subscript_receiver.as_deref(), Some("row"));
        let c = ctx(
            "fn f() { let a = self.headers[\"x-tenant\"]; }\n",
            "x-tenant",
        );
        assert_eq!(c.subscript_receiver.as_deref(), Some("self.headers"));
    }

    #[test]
    fn ctx_subscript_is_false_elsewhere() {
        assert!(!ctx("fn f() { let a = [\"a-b\"]; }\n", "a-b").subscript);
        assert!(!ctx("fn f() { g(\"a-b\"); }\n", "a-b").subscript);
        assert!(!ctx("fn f() { let a = \"a-b\".len(); }\n", "a-b").subscript);
    }

    // ctx.value_of_key

    #[test]
    fn ctx_value_of_key_for_struct_literal_fields() {
        let c = ctx(
            "fn f() { let a = S { name: \"orders-high\" }; }\n",
            "orders-high",
        );
        assert_eq!(c.value_of_key.as_deref(), Some("name"));
        assert!(!c.object_key);
        let c = ctx("fn f() { let a = S { env: &[\"a-b\"] }; }\n", "a-b");
        assert_eq!(c.value_of_key.as_deref(), Some("env"));
    }

    #[test]
    fn ctx_value_of_key_does_not_cross_a_call_or_a_block() {
        assert_eq!(
            ctx("fn f() { let a = S { name: g(\"a-b\") }; }\n", "a-b").value_of_key,
            None
        );
        assert_eq!(
            ctx("fn f() { let a = S { name: { \"a-b\" } }; }\n", "a-b").value_of_key,
            None
        );
    }

    #[test]
    fn ctx_rust_never_sets_the_ecma_only_fields() {
        let c = ctx("fn f() { g(\"a-b\"); }\n", "a-b");
        assert!(!c.import_specifier);
        assert!(!c.object_key);
        assert!(!c.type_position);
        assert_eq!(c.jsx_attr, None);
    }

    // ctx.arithmetic

    #[test]
    fn ctx_arithmetic_for_binary_operators_both_sides() {
        for src in [
            "fn f() { total * 0.12; }\n",
            "fn f() { 0.12 * total; }\n",
            "fn f() { total / 0.12; }\n",
            "fn f() { total + 0.12; }\n",
            "fn f() { total - 0.12; }\n",
            "fn f() { total % 0.12; }\n",
            "fn f() { total * (0.12); }\n",
            "fn f() { x += 0.12; }\n",
            "fn f() { x -= 0.12; }\n",
            "fn f() { x *= 0.12; }\n",
            "fn f() { x /= 0.12; }\n",
            "fn f() { x %= 0.12; }\n",
        ] {
            assert!(ctx(src, "0.12").arithmetic, "{src}");
        }
    }

    #[test]
    fn ctx_arithmetic_is_false_elsewhere() {
        for src in [
            "fn f() { x < 0.12; }\n",
            "fn f() { x == 0.12; }\n",
            "fn f() { g(0.12); }\n",
            "fn f() { let a = 0.12; }\n",
            "fn f() { let a = [0.12]; }\n",
            "fn f() { let a = -0.12; }\n",
            "fn f() { let a = S { opacity: 0.12 }; }\n",
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
    fn ctx_module_const_for_const_and_static_items() {
        assert_eq!(
            ctx("const Q: &str = \"orders-high\";\n", "orders-high").module_const,
            mc("Q", false)
        );
        assert_eq!(
            ctx("pub const Q: &str = \"orders-high\";\n", "orders-high").module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx("pub static Q: &str = \"orders-high\";\n", "orders-high").module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx(
                "mod m { pub const BUCKET: &str = \"user-uploads\"; }\n",
                "user-uploads"
            )
            .module_const,
            mc("BUCKET", true)
        );
    }

    #[test]
    fn ctx_module_const_through_arrays_references_and_struct_literals() {
        assert_eq!(
            ctx(
                "pub const Q: &[&str] = &[\"orders-high\", \"b-c\"];\n",
                "orders-high"
            )
            .module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx(
                "const C: Cfg = Cfg { name: \"orders-high\" };\n",
                "orders-high"
            )
            .module_const,
            mc("C", false)
        );
        assert_eq!(
            ctx(
                "const Q: (&str, u8) = (\"orders-high\", 1);\n",
                "orders-high"
            )
            .module_const,
            mc("Q", false)
        );
    }

    #[test]
    fn ctx_module_const_for_associated_consts() {
        assert_eq!(
            ctx(
                "impl A { pub const Q: &str = \"orders-high\"; }\n",
                "orders-high"
            )
            .module_const,
            mc("Q", true)
        );
        assert_eq!(
            ctx(
                "trait T { const Q: &str = \"orders-high\"; }\n",
                "orders-high"
            )
            .module_const,
            mc("Q", false)
        );
    }

    #[test]
    fn ctx_module_const_is_none_inside_functions_and_behind_calls() {
        for src in [
            "fn f() { const Q: &str = \"orders-high\"; }\n",
            "fn f() { static Q: &str = \"orders-high\"; }\n",
            "const Q: Lazy<String> = Lazy::new(|| \"orders-high\".into());\n",
            "const Q: String = String::from(\"orders-high\");\n",
            "const Q: &str = { \"orders-high\" };\n",
            "fn f() { let q = \"orders-high\"; }\n",
        ] {
            assert_eq!(ctx(src, "orders-high").module_const, None, "{src}");
        }
    }

    #[test]
    fn literal_malformed_input_does_not_panic() {
        for src in [
            "fn f( {",
            "const Q: &str = \"",
            "#[get(\"/x\"",
            "\u{0}\u{1}\u{2}",
            "fn f() { let a = 0.; }",
            "",
        ] {
            let _ = lits(src);
        }
    }
}
