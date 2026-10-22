//! The shared per-language AST extractor layer (Pass 4b-1).
//!
//! See `docs/design/2026-07-27_ast-extractor-layer.md` §1/§2 for the design rationale.
//! **No shared enriched model is handed to checkers** — this module exposes pure
//! functions ([`imports`], [`functions`], [`method_calls`]) that a checker calls to build
//! its OWN private model. `camerata-checks` stays server-independent (these functions take
//! `&str` source text the caller already has in memory — no filesystem, no network, no
//! process spawn).
//!
//! # Parser strategy (D2)
//!
//! | Language | Parser |
//! |---|---|
//! | Rust | [`syn`] 2 (`full` + `visit` features) via [`rust_syn`] |
//! | TypeScript / TSX / JavaScript | native `tree-sitter` grammars via [`ecma`] |
//! | Python | native `tree-sitter` grammar via [`python`] |
//!
//! # The one contract every extractor must honor: never panic
//!
//! A file that fails to parse (malformed, truncated, an adversarial fragment, or — for the
//! tree-sitter languages — anything at all, since tree-sitter's error-recovery always
//! returns SOME tree) yields fewer (possibly zero) results. Never a crash, never a false
//! positive manufactured from a bad parse. This mirrors the contract `ArchChecker::check`
//! already carries and the SQL splitter's existing discipline (see
//! `docs/design/2026-07-27_ast-extractor-layer.md` §1).

mod ancestry;
pub mod clone;
pub mod ecma;
pub mod enclosing;
pub mod paths;
pub mod python;
pub mod resolver;
pub mod rust_syn;

/// Source language, resolved from the file extension. Mirrors the scan's `lang_for_ext`
/// (`crates/server/src/onboard/propose.rs`) conceptually, but kept LOCAL to
/// `camerata-checks` (this crate must not depend on `camerata-server`) — see the crate-level
/// layering note in `arch_checker.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceLang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
}

impl SourceLang {
    /// Human-readable label, used in diagnostics/tests.
    pub fn label(self) -> &'static str {
        match self {
            SourceLang::Rust => "rust",
            SourceLang::TypeScript => "typescript",
            SourceLang::Tsx => "tsx",
            SourceLang::JavaScript => "javascript",
            SourceLang::Python => "python",
        }
    }
}

/// Resolve a [`SourceLang`] from a repo-relative (or absolute) file path by extension.
/// Returns `None` for any extension this extractor layer doesn't cover (v1 language set per
/// D2: Rust, TypeScript/TSX, JavaScript, Python — Go/Java/C#/Ruby are explicitly deferred).
/// A path with no extension, or an unrecognized one, is simply not extracted — never a panic,
/// never a guess.
pub fn lang_for_path(path: &str) -> Option<SourceLang> {
    let ext = path.rsplit('.').next()?;
    // Guard against a path with no '.' at all (rsplit still yields the whole string) — a bare
    // "Makefile"-shaped path must not be misread as extension "Makefile".
    if ext.len() == path.len() && !path.contains('.') {
        return None;
    }
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some(SourceLang::Rust),
        "ts" | "mts" | "cts" => Some(SourceLang::TypeScript),
        "tsx" => Some(SourceLang::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(SourceLang::JavaScript),
        "py" | "pyi" => Some(SourceLang::Python),
        _ => None,
    }
}

/// The kind of import/use declaration — lets a checker distinguish "a normal named import"
/// from a re-export (which is ALSO a dependency edge for the import-graph resolver, but
/// semantically different) or a glob/namespace import (which carries no enumerable names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// `import { a, b } from 'x'` / `use foo::{A, B};` / `from x import a, b`.
    Named,
    /// `import * as ns from 'x'` / `use foo::*;` (Rust glob) / an implicit Python
    /// package-level `import a.b.c`.
    Namespace,
    /// `import foo from 'x'` (JS/TS default import). No Rust/Python equivalent.
    Default,
    /// A dynamic `import('x')` call, or Python's `importlib.import_module('x')` shape —
    /// carried for resolver completeness even though it's an expression, not a declaration.
    Dynamic,
    /// `export { a } from 'x'` / `export * from 'x'` — a re-export. Still a dependency edge
    /// (this file's public surface depends on `x`), so the resolver treats it like any other
    /// import; checkers that care about the distinction can filter on `kind`.
    ReExport,
    /// A side-effect-only import with no bindings: `import 'x';` / Rust `extern crate foo;`.
    SideEffect,
}

/// One import/use declaration. `specifier` is the raw module string as written in source
/// (`"@/lib/db"`, `"crate::repositories::user"`, `"app.repos.user"`, or (Python relative)
/// a leading-dot form like `".pkg"` / `"..pkg.sub"` — see [`python`] module docs for the
/// leading-dot encoding). `names` carries the imported/origin bindings when the syntax names
/// them (aliased imports record the ORIGIN name, not the local alias — see module docs on
/// each per-language extractor for the rationale); empty for [`ImportKind::Namespace`],
/// [`ImportKind::Default`], and [`ImportKind::SideEffect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub specifier: String,
    pub names: Vec<String>,
    pub kind: ImportKind,
    pub line: usize,
    /// TS `import type { A }` / inline `import { type A }` (and the `export type` /
    /// `export { type A }` re-export equivalents): the binding exists only at the type level
    /// and is erased at runtime. Always `false` for Rust and Python.
    pub type_only: bool,
}

/// One function/method/handler definition with its body span (1-based, inclusive lines).
/// `attrs` carries the raw source text of attributes/decorators/registration markers
/// (`#[get("/…")]`, `@app.route(...)`, or — best-effort — an Express-style
/// `router.get('/x', handler)` registration line) so a future checker can classify
/// handler-ness structurally rather than by name alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSpan {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub attrs: Vec<String>,
}

/// One `receiver.method(...)` call site — the shape both DB-boundary rules need (Group D,
/// `docs/design/2026-07-27_ast-extractor-layer.md` §4). `receiver` is a best-effort rendering
/// of the call's object expression (an identifier, a `self.field` / `this.field` chain, or a
/// synthetic `"<expr>"` placeholder for anything more complex — never a panic, never a made-up
/// name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodCall {
    pub receiver: String,
    pub method: String,
    pub line: usize,
}

/// One named re-export: `export { origin as exported } from 'specifier'`. `origin` is the
/// name as the SOURCE module exports it (`"default"` for `export { default } from`);
/// `exported` is the name THIS module exposes it under (the alias when there is one). The
/// plain [`Import`] a `ReExport` produces keeps only `origin`, which is why an aliased
/// re-export is indistinguishable from an unaliased one there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedReExport {
    pub specifier: String,
    pub origin: String,
    pub exported: String,
    pub line: usize,
    /// `export type { A } from` or `export { type A } from`: the barrel exposes only the
    /// TYPE meaning of the name, so a same-named value is not reachable through it.
    pub type_only: bool,
}

/// One star re-export: `export * from 'specifier'`. (`export * as ns from` is NOT one of
/// these: it exposes a single namespace binding, so it is recorded as an own export `ns`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarReExport {
    pub specifier: String,
    pub line: usize,
}

/// A JS/TS file's export surface: what other modules can import from it, and which of that is
/// merely forwarded from elsewhere. Only the ECMAScript dialects have one; see [`exports`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSurface {
    /// Names this module exports itself (declarations, `default`, a local `export { a, b as c }`
    /// with no source, and `export * as ns from`).
    pub own: Vec<String>,
    pub named_re_exports: Vec<NamedReExport>,
    pub star_re_exports: Vec<StarReExport>,
}

/// Whether a [`Literal`] is a string or a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LiteralKind {
    Str,
    Number,
}

/// A literal's enclosing top-level constant: `export const ORDERS_QUEUE = 'orders-high'`,
/// `ORDERS_QUEUE: Final = 'orders-high'` or `pub const BUCKET: &str = "user-uploads";`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleConst {
    pub name: String,
    /// Whether another module can import it: `export` (ECMA), no leading underscore
    /// (Python), `pub` (Rust).
    pub exported: bool,
}

/// Where a [`Literal`] sits. Every field is decided from the parent chain of the literal node
/// (tree-sitter) or from a context stack carried by the `syn` visitor (Rust). Never a guess:
/// a shape the walker does not recognise leaves the field at its default, and the checkers
/// read a default field as "no reason to exclude this occurrence".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiteralCtx {
    /// Source of `import`/`export ... from`/`require()`/`import()`/`import_module()`; also the
    /// first argument of `jest.mock`, `vi.mock`, `require.resolve`.
    pub import_specifier: bool,
    /// Last identifier of the nearest enclosing call's callee chain (`i18n.t(..)` is `"t"`,
    /// `logging.getLogger(..)` is `"getLogger"`, `cn(..)` is `"cn"`), walking up through
    /// arrays, objects, parentheses, spreads and template literals; `None` when no call
    /// encloses it.
    pub callee: Option<String>,
    /// The full callee text of that same call (`res.headers.set`, `os.environ.get`), so a
    /// guard can ask whether the receiver is a headers object or the environment.
    pub callee_chain: Option<String>,
    /// For a subscript: the receiver expression's text (`process.env`, `os.environ`, `row`).
    pub subscript_receiver: Option<String>,
    /// The literal IS a key of an object/dict literal (`{ 'a-b': 1 }`).
    pub object_key: bool,
    /// The literal is a VALUE in an object/dict literal: the key's text (identifier or string).
    pub value_of_key: Option<String>,
    /// `x['a-b']` / `x["a-b"]` subscript.
    pub subscript: bool,
    /// JSX/TSX attribute name when the literal is that attribute's value, directly or inside
    /// `{...}`; `None` otherwise.
    pub jsx_attr: Option<String>,
    /// Inside a TS union/literal type, enum member initializer, `as const` tuple used as a type
    /// source, Python `Literal[...]`, or a Python `Enum` class body assignment.
    pub type_position: bool,
    /// Inside a Rust `#[...]` attribute or a Python decorator expression.
    pub attribute: bool,
    /// Direct operand of a binary arithmetic expression (`* / + - %`) or compound assignment.
    pub arithmetic: bool,
    /// The literal is the whole initializer, or sits inside an object/array initializer reached
    /// without passing a call, of a top-level `const`/`let`/`var` (ECMA), a module-level
    /// `NAME = ...` or `NAME: Final = ...` (Python), or a `const`/`static` item (Rust).
    pub module_const: Option<ModuleConst>,
    /// The literal sits in a constant-like declaration the walker recognises but cannot attribute
    /// to a nameable home (a class body that is not at module level). Checkers neither count it
    /// as a home nor as a bare copy: it is an unmodelled shape, so it stays silent.
    pub unattributed: bool,
}

/// One string or number literal. `value` is the unescaped content for a string and the
/// canonical text for a number (see [`canonical_number`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    pub value: String,
    pub kind: LiteralKind,
    pub line: usize,
    pub ctx: LiteralCtx,
}

/// Canonical text of a numeric literal: digit separators (`_`) removed, parsed to `f64` and
/// printed in its shortest round-trip form, so `.12`, `0.12`, `0.120` and `1.2e-1` all become
/// `0.12`. `None` for anything that is not a plain decimal number (hex, octal, binary, BigInt,
/// imaginary, non-finite): those can never be a rate, so they are not extracted.
pub fn canonical_number(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty()
        || !cleaned
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        return None;
    }
    let n: f64 = cleaned.parse().ok()?;
    n.is_finite().then(|| format!("{n}"))
}

/// A load whose target the import graph cannot follow: `import(name)`, `require(name)`,
/// `require.context('./dir')`, `import.meta.glob('./dir/*.ts')`, Python `importlib.import_module(name)`
/// / `pkgutil.iter_modules(..)`, Rust `include_dir!("dir")`. A checker that flags on
/// UN-reachability (dead code) must treat the files such a load could reach as alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedLoad {
    /// The literal head of the loaded path: the text before the first `${`, `+`, `*`, `{`,
    /// `[` or `?` of the first argument (a template literal's first quasi, the leftmost string
    /// operand of a `+` chain, a glob's literal head). A directory argument
    /// (`require.context('./icons')`, `include_dir!("assets")`) gets a trailing `/`. The
    /// directory the load can reach is this text cut at its last `/`. `None` when the argument
    /// carries no literal at all (`require(name)`): the load could name anything.
    pub prefix: Option<String>,
    pub line: usize,
}

/// Extract every import/use declaration from `source`, dispatching to the per-language
/// parser. Never panics: a parse failure yields an empty vec (fewer findings, never a crash
/// — see module docs).
pub fn imports(lang: SourceLang, source: &str) -> Vec<Import> {
    match lang {
        SourceLang::Rust => rust_syn::imports(source),
        SourceLang::TypeScript => ecma::imports(ecma::EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma::imports(ecma::EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma::imports(ecma::EcmaDialect::JavaScript, source),
        SourceLang::Python => python::imports(source),
    }
}

/// Extract every load the import graph cannot follow (see [`ComputedLoad`]). Never panics
/// (same contract as [`imports`]).
pub fn computed_loads(lang: SourceLang, source: &str) -> Vec<ComputedLoad> {
    match lang {
        SourceLang::Rust => rust_syn::computed_loads(source),
        SourceLang::TypeScript => ecma::computed_loads(ecma::EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma::computed_loads(ecma::EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma::computed_loads(ecma::EcmaDialect::JavaScript, source),
        SourceLang::Python => python::computed_loads(source),
    }
}

/// Extract a file's export surface. Only TypeScript / TSX / JavaScript have one; Rust and
/// Python return an empty surface (their visibility models are not re-export barrels in the
/// ES-module sense). Never panics (same contract as [`imports`]).
pub fn exports(lang: SourceLang, source: &str) -> ExportSurface {
    match lang {
        SourceLang::TypeScript => ecma::exports(ecma::EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma::exports(ecma::EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma::exports(ecma::EcmaDialect::JavaScript, source),
        SourceLang::Rust | SourceLang::Python => ExportSurface::default(),
    }
}

/// Extract every function/method/handler definition from `source`. Never panics (same
/// contract as [`imports`]).
pub fn functions(lang: SourceLang, source: &str) -> Vec<FunctionSpan> {
    match lang {
        SourceLang::Rust => rust_syn::functions(source),
        SourceLang::TypeScript => ecma::functions(ecma::EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma::functions(ecma::EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma::functions(ecma::EcmaDialect::JavaScript, source),
        SourceLang::Python => python::functions(source),
    }
}

/// Extract every `receiver.method(...)` call site from `source`. Never panics (same contract
/// as [`imports`]).
pub fn method_calls(lang: SourceLang, source: &str) -> Vec<MethodCall> {
    match lang {
        SourceLang::Rust => rust_syn::method_calls(source),
        SourceLang::TypeScript => ecma::method_calls(ecma::EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma::method_calls(ecma::EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma::method_calls(ecma::EcmaDialect::JavaScript, source),
        SourceLang::Python => python::method_calls(source),
    }
}

/// Extract every string and number literal from `source` with the context each one sits in
/// (see [`LiteralCtx`]). Interpolated strings (`` `a${b}` ``, f-strings), Python byte strings
/// and adjacent-string concatenations are not literals and are skipped; Rust macro bodies are
/// not descended. Never panics (same contract as [`imports`]).
pub fn literals(lang: SourceLang, source: &str) -> Vec<Literal> {
    match lang {
        SourceLang::Rust => rust_syn::literals(source),
        SourceLang::TypeScript => ecma::literals(ecma::EcmaDialect::TypeScript, source),
        SourceLang::Tsx => ecma::literals(ecma::EcmaDialect::Tsx, source),
        SourceLang::JavaScript => ecma::literals(ecma::EcmaDialect::JavaScript, source),
        SourceLang::Python => python::literals(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_for_path_dispatches_v1_extensions() {
        assert_eq!(lang_for_path("src/main.rs"), Some(SourceLang::Rust));
        assert_eq!(lang_for_path("src/lib.ts"), Some(SourceLang::TypeScript));
        assert_eq!(lang_for_path("src/App.tsx"), Some(SourceLang::Tsx));
        assert_eq!(lang_for_path("src/index.js"), Some(SourceLang::JavaScript));
        assert_eq!(lang_for_path("src/index.jsx"), Some(SourceLang::JavaScript));
        assert_eq!(lang_for_path("app/main.py"), Some(SourceLang::Python));
    }

    #[test]
    fn lang_for_path_returns_none_for_unrecognized_or_extensionless() {
        assert_eq!(lang_for_path("README.md"), None);
        assert_eq!(lang_for_path("Makefile"), None);
        assert_eq!(lang_for_path("go.mod"), None); // go deferred per D2
        assert_eq!(lang_for_path(""), None);
    }

    #[test]
    fn dispatch_never_panics_on_empty_source_for_every_language() {
        for lang in [
            SourceLang::Rust,
            SourceLang::TypeScript,
            SourceLang::Tsx,
            SourceLang::JavaScript,
            SourceLang::Python,
        ] {
            assert!(imports(lang, "").is_empty());
            assert!(functions(lang, "").is_empty());
            assert!(method_calls(lang, "").is_empty());
            assert!(literals(lang, "").is_empty());
            assert!(computed_loads(lang, "").is_empty());
            assert_eq!(exports(lang, ""), ExportSurface::default());
        }
    }

    #[test]
    fn exports_dispatch_reads_ecma_and_leaves_other_languages_empty() {
        let src = "export const a = 1;\n";
        assert_eq!(exports(SourceLang::TypeScript, src).own, ["a"]);
        assert_eq!(exports(SourceLang::Tsx, src).own, ["a"]);
        assert_eq!(exports(SourceLang::JavaScript, src).own, ["a"]);
        assert_eq!(
            exports(SourceLang::Rust, "pub fn a() {}"),
            ExportSurface::default()
        );
        assert_eq!(
            exports(SourceLang::Python, "def a(): pass"),
            ExportSurface::default()
        );
    }

    #[test]
    fn canonical_number_spellings_collapse_to_one_value() {
        for raw in [
            ".12", "0.12", "0.120", "0.1200", "1.2e-1", "0_0.12", "12e-2",
        ] {
            assert_eq!(canonical_number(raw).as_deref(), Some("0.12"), "{raw}");
        }
        assert_eq!(canonical_number("1_000.5").as_deref(), Some("1000.5"));
        assert_eq!(canonical_number("86400").as_deref(), Some("86400"));
        assert_eq!(canonical_number("1.0").as_deref(), Some("1"));
    }

    #[test]
    fn canonical_number_rejects_non_decimal_spellings() {
        for raw in ["0x1F", "0b101", "0o17", "10n", "3j", "", "abc", "1e999"] {
            assert_eq!(canonical_number(raw), None, "{raw}");
        }
    }
}
