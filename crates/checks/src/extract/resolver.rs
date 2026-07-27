//! The cross-file import resolver: turns per-file [`super::Import`]s into an intra-repo import
//! GRAPH by resolving each specifier to a repo-relative file path, when it can. See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group C.
//!
//! # The one policy every resolution path here must honor: false-negative, never false-positive
//!
//! An import whose target cannot be determined UNAMBIGUOUSLY is dropped — never flagged,
//! never guessed. Concretely:
//! - A bare/external specifier (an npm package, an external Rust crate, a stdlib import) has
//!   no repo file to resolve to and is silently dropped.
//! - When more than one repo file is a plausible match for an ambiguous specifier (the
//!   absolute-dotted Python case, see [`resolve_python`]), the resolver drops the edge rather
//!   than picking one.
//! - Relative/deterministic forms (a TS `./`-relative path, a Rust `crate::`/`self::`/
//!   `super::` path, a Python leading-dot relative import) have a single well-defined target
//!   under each language's own module-resolution rules, so those follow a fixed priority
//!   order (first match wins) rather than an ambiguity check — there IS only one correct
//!   answer there, by construction.
//!
//! # Scope (v1 / foundation pass)
//!
//! - **Rust**: only `crate::`, `self::`, and `super::`-prefixed specifiers are resolved. A
//!   bare specifier (`serde::Deserialize`, or a Rust-2015-style unprefixed local path) is
//!   ambiguous without full crate name resolution and is out of scope — dropped.
//! - **TS/TSX/JS**: relative (`./`, `../`, `/`) specifiers always attempt resolution.
//!   Non-relative (bare) specifiers only resolve via a `tsconfig.json` `compilerOptions.paths`
//!   alias, when one is present in the file set; otherwise treated as an external package.
//! - **Python**: leading-dot relative imports (`.`, `..pkg`) resolve against the importing
//!   file's own directory tree. A dotted absolute import (`a.b.c`) resolves by SUFFIX match
//!   against every Python file's own derived dotted module path (the repo's true "source
//!   root" — `src/`, `app/`, or the repo root itself — isn't known to this extractor), dropped
//!   on zero or multiple matches.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Deserialize;

use super::{Import, ImportKind, SourceLang};

/// One resolved import edge: `from_file` names (via `imp.specifier`) something that exists in
/// this repo at `to_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImport {
    pub from_file: String,
    pub to_file: String,
    pub specifier: String,
    pub kind: ImportKind,
    pub line: usize,
}

/// Build the intra-repo import graph: every [`super::Import`] in every file, resolved to a
/// target file where possible. Unresolvable imports are silently absent from the result (see
/// module docs) — this function never errors and never panics (every per-language extractor
/// it calls already guarantees that).
pub fn build_import_graph(files: &[(String, String)]) -> Vec<ResolvedImport> {
    let path_set: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let tsconfig = load_tsconfig(files);
    let mut out = Vec::new();
    for (path, content) in files {
        let Some(lang) = super::lang_for_path(path) else {
            continue;
        };
        for imp in super::imports(lang, content) {
            let resolved = match lang {
                SourceLang::Rust => resolve_rust(path, &imp.specifier, &path_set),
                SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => {
                    resolve_ecma(path, &imp.specifier, &path_set, tsconfig.as_ref())
                }
                SourceLang::Python => resolve_python(path, &imp, &path_set),
            };
            if let Some(to_file) = resolved {
                if to_file == *path {
                    continue; // a self-edge is never meaningful (and would trivially "cycle")
                }
                out.push(ResolvedImport {
                    from_file: path.clone(),
                    to_file,
                    specifier: imp.specifier.clone(),
                    kind: imp.kind,
                    line: imp.line,
                });
            }
        }
    }
    out
}

/// BFS over `graph` from `start`, returning every repo file reachable via zero or more import
/// edges (including `start` itself). Guards against a genuine import cycle (A imports B, B
/// imports A) with a visited-set, so it terminates in `O(V + E)` — never an infinite loop.
pub fn reachable_from(graph: &[ResolvedImport], start: &str) -> HashSet<String> {
    let mut visited = HashSet::new();
    visited.insert(start.to_string());
    let mut queue = VecDeque::new();
    queue.push_back(start.to_string());
    while let Some(cur) = queue.pop_front() {
        for edge in graph.iter().filter(|e| e.from_file == cur) {
            if visited.insert(edge.to_file.clone()) {
                queue.push_back(edge.to_file.clone());
            }
        }
    }
    visited
}

// ─── shared path helpers ────────────────────────────────────────────────────────────────

fn directory_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[..idx],
        None => "",
    }
}

/// Join `dir` and a relative path `rel` (which may contain `.`/`..` segments), normalizing the
/// result. A leading `/` on `rel` is treated as repo-root-relative (ignores `dir` entirely).
fn normalize_join(dir: &str, rel: &str) -> String {
    let mut segs: Vec<&str> = if let Some(stripped) = rel.strip_prefix('/') {
        let _ = stripped;
        Vec::new()
    } else {
        // A bare "." (tsconfig's common `baseUrl = "."`) means "repo root" — treat exactly
        // like an empty dir, not as a literal path segment.
        dir.split('/').filter(|s| !s.is_empty() && *s != ".").collect()
    };
    let rel = rel.strip_prefix('/').unwrap_or(rel);
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    segs.join("/")
}

// ─── Rust: crate:: / self:: / super:: ───────────────────────────────────────────────────

/// The nearest ancestor `src/` directory in `path` (the LAST `src` path segment scanning
/// left-to-right, i.e. the closest enclosing crate root) — e.g.
/// `"crates/checks/src/extract/mod.rs"` → `"crates/checks/src"`. Returns `None` for a Rust
/// file that isn't under any `src/` directory (a build script at crate root, `xtask/main.rs`
/// outside `src/`, ...) — out of scope for this resolver, dropped.
fn rust_src_root(path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split('/').collect();
    let idx = segs.iter().rposition(|&s| s == "src")?;
    Some(segs[..=idx].join("/"))
}

/// The importing file's OWN module path, relative to its crate's `src_root` — e.g.
/// `"extract/mod.rs"` (under the same `src_root`) → `["extract"]`; `"extract/rust_syn.rs"` →
/// `["extract", "rust_syn"]`; `"lib.rs"` → `[]` (crate root).
fn rust_module_path(path: &str, src_root: &str) -> Vec<String> {
    let rest = path.strip_prefix(src_root).unwrap_or(path).trim_start_matches('/');
    let rest = rest.strip_suffix(".rs").unwrap_or(rest);
    let mut segs: Vec<String> =
        rest.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    if matches!(segs.last().map(|s| s.as_str()), Some("mod") | Some("lib") | Some("main")) {
        segs.pop();
    }
    segs
}

fn rust_module_candidates(src_root: &str, segments: &[String], path_set: &HashSet<&str>) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    let joined = segments.join("/");
    let as_file = format!("{src_root}/{joined}.rs");
    if path_set.contains(as_file.as_str()) {
        return Some(as_file);
    }
    let as_dir_mod = format!("{src_root}/{joined}/mod.rs");
    if path_set.contains(as_dir_mod.as_str()) {
        return Some(as_dir_mod);
    }
    None
}

fn resolve_rust(importer: &str, specifier: &str, path_set: &HashSet<&str>) -> Option<String> {
    let src_root = rust_src_root(importer)?;
    let importer_module = rust_module_path(importer, &src_root);

    // NOTE: `specifier` (per the `rust_syn` extractor's convention) is ALWAYS the module-path
    // PREFIX only — the leaf item name (`Baz` in `crate::a::b::Baz`) already lives in the
    // `Import::names` field, never in `specifier`. So `target_segments` below IS the full
    // module path to resolve to a file — no "drop the last segment as the item name" step
    // is needed (an earlier version of this function did that and it was a bug: it stripped
    // a genuine module segment, e.g. turning `self::helper` — module path `["x","helper"]` —
    // into `["x"]`, which could wrongly resolve back to the IMPORTING file itself).
    let target_segments: Vec<String> = if let Some(rest) = specifier.strip_prefix("crate::") {
        rest.split("::").map(|s| s.to_string()).collect()
    } else if let Some(rest) = specifier.strip_prefix("self::") {
        let mut v = importer_module.clone();
        v.extend(rest.split("::").map(|s| s.to_string()));
        v
    } else if specifier.starts_with("super::") {
        let mut rest = specifier;
        let mut base = importer_module.clone();
        while let Some(r) = rest.strip_prefix("super::") {
            base.pop();
            rest = r;
        }
        base.extend(rest.split("::").map(|s| s.to_string()));
        base
    } else {
        // Bare / extern-crate specifier — out of v1 scope (see module docs), drop.
        return None;
    };
    rust_module_candidates(&src_root, &target_segments, path_set)
}

// ─── TS / TSX / JS: relative paths + tsconfig `paths` aliases ───────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
struct TsConfigFile {
    #[serde(rename = "compilerOptions", default)]
    compiler_options: TsCompilerOptions,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TsCompilerOptions {
    #[serde(rename = "baseUrl", default)]
    base_url: Option<String>,
    #[serde(default)]
    paths: HashMap<String, Vec<String>>,
}

/// Find and parse a `tsconfig.json` in `files` (preferring one at the repo root). Absent or
/// malformed (including the common JSONC-with-comments case, which `serde_json` rejects) both
/// degrade to `None` — never a panic, never a hard error that would abort the whole resolver.
fn load_tsconfig(files: &[(String, String)]) -> Option<TsConfigFile> {
    let content = files
        .iter()
        .find(|(p, _)| p == "tsconfig.json")
        .or_else(|| files.iter().find(|(p, _)| p.ends_with("/tsconfig.json")))
        .map(|(_, c)| c.as_str())?;
    serde_json::from_str::<TsConfigFile>(content).ok()
}

const ECMA_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
const ECMA_INDEX_CANDIDATES: &[&str] =
    &["/index.ts", "/index.tsx", "/index.js", "/index.jsx"];

fn try_ecma_extensions(base: &str, path_set: &HashSet<&str>) -> Option<String> {
    if path_set.contains(base) {
        return Some(base.to_string());
    }
    for ext in ECMA_EXTENSIONS {
        let c = format!("{base}{ext}");
        if path_set.contains(c.as_str()) {
            return Some(c);
        }
    }
    for idx in ECMA_INDEX_CANDIDATES {
        let c = format!("{base}{idx}");
        if path_set.contains(c.as_str()) {
            return Some(c);
        }
    }
    None
}

fn resolve_ecma(
    importer: &str,
    specifier: &str,
    path_set: &HashSet<&str>,
    tsconfig: Option<&TsConfigFile>,
) -> Option<String> {
    if specifier.starts_with('.') || specifier.starts_with('/') {
        let dir = directory_of(importer);
        let base = normalize_join(dir, specifier);
        return try_ecma_extensions(&base, path_set);
    }
    // Non-relative (bare) specifier: only resolvable via a tsconfig `paths` alias.
    let ts = tsconfig?;
    let base_url = ts.compiler_options.base_url.clone().unwrap_or_else(|| ".".to_string());
    for (pattern, templates) in &ts.compiler_options.paths {
        let prefix = pattern.trim_end_matches('*');
        if !specifier.starts_with(prefix) {
            continue;
        }
        let suffix = &specifier[prefix.len()..];
        for template in templates {
            let templ_resolved = template.replace('*', suffix);
            let joined = normalize_join(&base_url, &templ_resolved);
            if let Some(found) = try_ecma_extensions(&joined, path_set) {
                return Some(found);
            }
        }
    }
    None // external package (or an unaliased bare specifier) — correctly dropped
}

// ─── Python: leading-dot relative + absolute-dotted suffix match ───────────────────────

fn try_python_extensions(base: &str, path_set: &HashSet<&str>) -> Option<String> {
    let as_file = format!("{base}.py");
    if path_set.contains(as_file.as_str()) {
        return Some(as_file);
    }
    let as_pkg = format!("{base}/__init__.py");
    if path_set.contains(as_pkg.as_str()) {
        return Some(as_pkg);
    }
    None
}

/// This file's own dotted module path (`"app/services/user.py"` → `["app","services","user"]`;
/// `"app/services/__init__.py"` → `["app","services"]` — a package's `__init__.py` IS the
/// package, not a `.__init__` submodule of it).
fn python_module_segments(path: &str) -> Vec<String> {
    let stripped = path.strip_suffix(".py").or_else(|| path.strip_suffix(".pyi")).unwrap_or(path);
    let mut segs: Vec<String> =
        stripped.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    if segs.last().map(|s| s.as_str()) == Some("__init__") {
        segs.pop();
    }
    segs
}

fn resolve_python(importer: &str, imp: &Import, path_set: &HashSet<&str>) -> Option<String> {
    let spec = &imp.specifier;
    if spec.starts_with('.') {
        let level = spec.chars().take_while(|&c| c == '.').count();
        let module_rest = &spec[level..];
        let mut base = directory_of(importer).to_string();
        for _ in 1..level {
            base = directory_of(&base).to_string();
        }
        if !module_rest.is_empty() {
            let rel = module_rest.replace('.', "/");
            let joined = if base.is_empty() { rel } else { format!("{base}/{rel}") };
            return try_python_extensions(&joined, path_set);
        }
        // Bare `from . import x` / `from .. import x` — try `x` as a submodule of `base`.
        let name = imp.names.first()?;
        let joined = if base.is_empty() { name.clone() } else { format!("{base}/{name}") };
        return try_python_extensions(&joined, path_set);
    }
    // Absolute dotted specifier — the repo's actual source root (`src/`, `app/`, repo root,
    // ...) isn't known to this extractor, so match by SUFFIX against every `.py`/`.pyi`
    // file's own derived module path. Zero or multiple matches both drop the edge (the
    // ambiguity case this module's docs call out) rather than guess.
    let want: Vec<&str> = spec.split('.').collect();
    let mut matches: Vec<&str> = Vec::new();
    for candidate_path in path_set.iter() {
        if !(candidate_path.ends_with(".py") || candidate_path.ends_with(".pyi")) {
            continue;
        }
        let segs = python_module_segments(candidate_path);
        if segs.len() >= want.len() && segs[segs.len() - want.len()..] == want[..] {
            matches.push(candidate_path);
        }
    }
    match matches.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    // ── TS / JS: relative resolution ───────────────────────────────────────────

    #[test]
    fn ts_relative_import_resolves_with_extension_inference() {
        let f = files(vec![
            ("src/routes/orders.ts", "import { db } from '../lib/db';\n"),
            ("src/lib/db.ts", "export const db = {};\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].from_file, "src/routes/orders.ts");
        assert_eq!(graph[0].to_file, "src/lib/db.ts");
    }

    #[test]
    fn ts_relative_import_resolves_to_index_file() {
        let f = files(vec![
            ("src/a/consumer.ts", "import { x } from './widgets';\n"),
            ("src/a/widgets/index.ts", "export const x = 1;\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "src/a/widgets/index.ts");
    }

    #[test]
    fn ts_external_package_is_dropped() {
        let f = files(vec![("src/a.ts", "import React from 'react';\n")]);
        assert!(build_import_graph(&f).is_empty());
    }

    #[test]
    fn ts_tsconfig_paths_alias_resolves() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
            ),
            ("src/controllers/orders.ts", "import { repo } from '@/repositories/orders';\n"),
            ("src/repositories/orders.ts", "export const repo = {};\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "src/repositories/orders.ts");
    }

    #[test]
    fn ts_malformed_tsconfig_does_not_panic_and_drops_alias_import() {
        let f = files(vec![
            ("tsconfig.json", "{ not valid json !!"),
            ("src/controllers/orders.ts", "import { repo } from '@/repositories/orders';\n"),
            ("src/repositories/orders.ts", "export const repo = {};\n"),
        ]);
        // Must not panic; the alias is simply unresolvable without a valid tsconfig.
        let graph = build_import_graph(&f);
        assert!(graph.is_empty(), "{graph:#?}");
    }

    // ── Rust ────────────────────────────────────────────────────────────────────

    #[test]
    fn rust_crate_path_resolves_to_module_file() {
        let f = files(vec![
            (
                "crates/checks/src/handler_no_db_checker.rs",
                "use crate::arch_checker::ArchChecker;\n",
            ),
            ("crates/checks/src/arch_checker.rs", "pub trait ArchChecker {}\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "crates/checks/src/arch_checker.rs");
    }

    #[test]
    fn rust_crate_path_resolves_to_mod_rs() {
        let f = files(vec![
            ("crates/checks/src/lib.rs", "use crate::extract::imports;\n"),
            ("crates/checks/src/extract/mod.rs", "pub fn imports() {}\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "crates/checks/src/extract/mod.rs");
    }

    #[test]
    fn rust_self_and_super_resolve_relative_to_importer_module() {
        let f = files(vec![
            ("crates/checks/src/extract/ecma.rs", "use super::rust_syn::imports;\nuse self::helper::x;\n"),
            ("crates/checks/src/extract/rust_syn.rs", "pub fn imports() {}\n"),
            ("crates/checks/src/extract/ecma/helper.rs", "pub fn x() {}\n"),
        ]);
        let graph = build_import_graph(&f);
        assert!(
            graph.iter().any(|e| e.to_file == "crates/checks/src/extract/rust_syn.rs"),
            "{graph:#?}"
        );
        assert!(
            graph.iter().any(|e| e.to_file == "crates/checks/src/extract/ecma/helper.rs"),
            "{graph:#?}"
        );
    }

    #[test]
    fn rust_bare_extern_crate_specifier_is_dropped() {
        let f = files(vec![("crates/checks/src/lib.rs", "use serde::Deserialize;\n")]);
        assert!(build_import_graph(&f).is_empty());
    }

    #[test]
    fn rust_file_outside_src_dir_is_dropped() {
        let f = files(vec![
            ("xtask/main.rs", "use crate::foo::Bar;\n"),
            ("xtask/foo.rs", "pub struct Bar;\n"),
        ]);
        assert!(build_import_graph(&f).is_empty());
    }

    // ── Python ────────────────────────────────────────────────────────────────

    #[test]
    fn python_relative_single_dot_resolves() {
        let f = files(vec![
            ("app/services/order.py", "from . import sibling\n"),
            ("app/services/sibling.py", "x = 1\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "app/services/sibling.py");
    }

    #[test]
    fn python_relative_double_dot_with_module_resolves() {
        // `order.py`'s CURRENT package is "app/sub" (its containing dir); level=2 ("..")
        // means the PARENT of that package, i.e. "app" — so "..pkg" resolves to "app/pkg.py".
        let f = files(vec![
            ("app/sub/order.py", "from ..pkg import thing\n"),
            ("app/pkg.py", "thing = 1\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "app/pkg.py");
    }

    #[test]
    fn python_absolute_dotted_resolves_when_unambiguous() {
        let f = files(vec![
            ("app/main.py", "import app.services.user\n"),
            ("app/services/user.py", "class User: pass\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 1, "{graph:#?}");
        assert_eq!(graph[0].to_file, "app/services/user.py");
    }

    #[test]
    fn python_absolute_dotted_ambiguous_match_is_dropped() {
        // Two files both plausibly answer "services.user" as a suffix — genuinely ambiguous
        // without knowing the real source root, so the resolver must drop, not guess.
        let f = files(vec![
            ("app/main.py", "import services.user\n"),
            ("app/services/user.py", "class User: pass\n"),
            ("vendor/lib/services/user.py", "class OtherUser: pass\n"),
        ]);
        let graph = build_import_graph(&f);
        assert!(graph.is_empty(), "{graph:#?}");
    }

    #[test]
    fn python_absolute_dotted_no_match_is_dropped() {
        let f = files(vec![("app/main.py", "import numpy\n")]);
        assert!(build_import_graph(&f).is_empty());
    }

    // ── cross-language aggregation + cycles ─────────────────────────────────────

    #[test]
    fn build_import_graph_aggregates_across_languages_and_files() {
        let f = files(vec![
            ("src/a.ts", "import { b } from './b';\n"),
            ("src/b.ts", "export const b = 1;\n"),
            ("app/x.py", "from . import y\n"),
            ("app/y.py", "z = 1\n"),
            ("crates/c/src/lib.rs", "use crate::inner::Thing;\n"),
            ("crates/c/src/inner.rs", "pub struct Thing;\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 3, "{graph:#?}");
    }

    #[test]
    fn reachable_from_handles_a_genuine_cycle_without_looping() {
        let f = files(vec![
            ("src/a.ts", "import { b } from './b';\n"),
            ("src/b.ts", "import { a } from './a';\n"),
        ]);
        let graph = build_import_graph(&f);
        assert_eq!(graph.len(), 2, "{graph:#?}");
        let reached = reachable_from(&graph, "src/a.ts");
        assert!(reached.contains("src/a.ts"));
        assert!(reached.contains("src/b.ts"));
        assert_eq!(reached.len(), 2);
    }

    #[test]
    fn reachable_from_unknown_start_is_just_itself() {
        let graph: Vec<ResolvedImport> = Vec::new();
        let reached = reachable_from(&graph, "src/nope.ts");
        assert_eq!(reached.len(), 1);
    }

    // ── adversarial: empty file set, self-import, does not panic ───────────────

    #[test]
    fn empty_file_set_does_not_panic() {
        assert!(build_import_graph(&[]).is_empty());
    }

    #[test]
    fn self_referential_import_produces_no_self_edge() {
        // Contrived (a file "importing itself"), but must not panic or produce a
        // meaningless self-loop edge.
        let f = files(vec![("src/a.ts", "import { a } from './a';\n")]);
        assert!(build_import_graph(&f).is_empty());
    }
}
