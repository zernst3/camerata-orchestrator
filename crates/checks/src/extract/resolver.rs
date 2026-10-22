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
//!   The alias comes from the importer's NEAREST ancestor `tsconfig.json`, with `baseUrl` and
//!   `paths` relative to that file's directory. Matching follows TypeScript: a pattern with no
//!   `*` is an exact match, a `*` pattern must match its prefix AND suffix, and the best match
//!   wins (exact, then longest prefix, then pattern text), never map iteration order.
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
    let tsconfigs = TsConfigs::load(files);
    let mut out = Vec::new();
    for (path, content) in files {
        let Some(lang) = super::lang_for_path(path) else {
            continue;
        };
        for imp in super::imports(lang, content) {
            let resolved = match lang {
                SourceLang::Rust => resolve_rust(path, &imp.specifier, &path_set),
                SourceLang::TypeScript | SourceLang::Tsx | SourceLang::JavaScript => {
                    resolve_ecma(path, &imp.specifier, &path_set, &tsconfigs)
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

/// Multi-root sibling of [`reachable_from`]: every repo file reachable via zero or more import
/// edges from ANY of `starts` (the starts themselves included). One BFS over an adjacency
/// index, `O(V + E)`, cycle-safe through the visited set. [`reachable_from`] is left as it is.
pub fn reachable_from_all<'s>(
    graph: &[ResolvedImport],
    starts: impl IntoIterator<Item = &'s str>,
) -> HashSet<String> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in graph {
        adjacency
            .entry(edge.from_file.as_str())
            .or_default()
            .push(edge.to_file.as_str());
    }
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for start in starts {
        if visited.insert(start.to_string()) {
            queue.push_back(start);
        }
    }
    while let Some(cur) = queue.pop_front() {
        for &next in adjacency.get(cur).into_iter().flatten() {
            if visited.insert(next.to_string()) {
                queue.push_back(next);
            }
        }
    }
    visited
}

// ─── shared path helpers ────────────────────────────────────────────────────────────────

pub(crate) fn directory_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[..idx],
        None => "",
    }
}

/// Join `dir` and a relative path `rel` (which may contain `.`/`..` segments), normalizing the
/// result. A leading `/` on `rel` is treated as repo-root-relative (ignores `dir` entirely).
pub(crate) fn normalize_join(dir: &str, rel: &str) -> String {
    let mut segs: Vec<&str> = if let Some(stripped) = rel.strip_prefix('/') {
        let _ = stripped;
        Vec::new()
    } else {
        // A bare "." (tsconfig's common `baseUrl = "."`) means "repo root" — treat exactly
        // like an empty dir, not as a literal path segment.
        dir.split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect()
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
pub(crate) fn rust_src_root(path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split('/').collect();
    let idx = segs.iter().rposition(|&s| s == "src")?;
    Some(segs[..=idx].join("/"))
}

/// The importing file's OWN module path, relative to its crate's `src_root` — e.g.
/// `"extract/mod.rs"` (under the same `src_root`) → `["extract"]`; `"extract/rust_syn.rs"` →
/// `["extract", "rust_syn"]`; `"lib.rs"` → `[]` (crate root).
fn rust_module_path(path: &str, src_root: &str) -> Vec<String> {
    let rest = path
        .strip_prefix(src_root)
        .unwrap_or(path)
        .trim_start_matches('/');
    let rest = rest.strip_suffix(".rs").unwrap_or(rest);
    let mut segs: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if matches!(
        segs.last().map(|s| s.as_str()),
        Some("mod") | Some("lib") | Some("main")
    ) {
        segs.pop();
    }
    segs
}

fn rust_module_candidates(
    src_root: &str,
    segments: &[String],
    path_set: &HashSet<&str>,
) -> Option<String> {
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

/// Every parseable `tsconfig.json` in the file set, with the directory it governs. An importer
/// is governed by the NEAREST ancestor tsconfig, and that tsconfig's `baseUrl` / `paths` are
/// relative to ITS directory, as `tsc` reads them. (`extends` is not followed: a nested config
/// that inherits its `paths` resolves nothing, a false negative.) An absent or malformed file
/// (including the common JSONC-with-comments case, which `serde_json` rejects) is simply not an
/// entry: never a panic, never a hard error that would abort the whole resolver.
#[derive(Debug, Default)]
struct TsConfigs {
    /// `(directory, config)`; the directory is `""` for a repo-root tsconfig.
    entries: Vec<(String, TsConfigFile)>,
}

impl TsConfigs {
    fn load(files: &[(String, String)]) -> Self {
        let mut entries: Vec<(String, TsConfigFile)> = files
            .iter()
            .filter(|(p, _)| p == "tsconfig.json" || p.ends_with("/tsconfig.json"))
            .filter_map(|(p, c)| {
                let cfg = serde_json::from_str::<TsConfigFile>(c).ok()?;
                Some((directory_of(p).to_string(), cfg))
            })
            .collect();
        // Deepest first, so the first ancestor found in `nearest` is the nearest one.
        entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Self { entries }
    }

    fn nearest(&self, importer: &str) -> Option<(&str, &TsConfigFile)> {
        let dir = directory_of(importer);
        self.entries
            .iter()
            .find(|(d, _)| d.is_empty() || dir == d || dir.starts_with(&format!("{d}/")))
            .map(|(d, c)| (d.as_str(), c))
    }
}

/// The `paths` entry that `specifier` selects, with the text its `*` captured (empty for an
/// exact pattern), following TypeScript: a pattern with no `*` matches only the identical
/// specifier; a pattern with one `*` must match both the text before and after it; when several
/// patterns match, an exact one wins, then the longest prefix, then the pattern text (so the
/// choice never depends on map iteration order).
fn best_paths_match<'p>(
    paths: &'p HashMap<String, Vec<String>>,
    specifier: &str,
) -> Option<(&'p [String], String)> {
    let mut best: Option<(bool, usize, &'p str, &'p [String], String)> = None;
    for (pattern, templates) in paths {
        let candidate = match pattern.split_once('*') {
            None => (pattern == specifier).then(|| (true, pattern.len(), String::new())),
            Some((prefix, suffix)) => {
                let fits = specifier.len() >= prefix.len() + suffix.len()
                    && specifier.starts_with(prefix)
                    && specifier.ends_with(suffix);
                fits.then(|| {
                    let mid = &specifier[prefix.len()..specifier.len() - suffix.len()];
                    (false, prefix.len(), mid.to_string())
                })
            }
        };
        let Some((exact, prefix_len, captured)) = candidate else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((b_exact, b_len, b_pat, _, _)) => {
                (exact, prefix_len, std::cmp::Reverse(pattern.as_str()))
                    > (*b_exact, *b_len, std::cmp::Reverse(*b_pat))
            }
        };
        if better {
            best = Some((exact, prefix_len, pattern.as_str(), templates, captured));
        }
    }
    best.map(|(_, _, _, templates, captured)| (templates, captured))
}

const ECMA_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
const ECMA_INDEX_CANDIDATES: &[&str] = &["/index.ts", "/index.tsx", "/index.js", "/index.jsx"];

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
    tsconfigs: &TsConfigs,
) -> Option<String> {
    if specifier.starts_with('.') || specifier.starts_with('/') {
        let dir = directory_of(importer);
        let base = normalize_join(dir, specifier);
        return try_ecma_extensions(&base, path_set);
    }
    // Non-relative (bare) specifier: only resolvable via a tsconfig `paths` alias, read from
    // the importer's nearest tsconfig and relative to that tsconfig's own directory.
    let (ts_dir, ts) = tsconfigs.nearest(importer)?;
    let base_url = normalize_join(
        ts_dir,
        ts.compiler_options.base_url.as_deref().unwrap_or("."),
    );
    let (templates, captured) = best_paths_match(&ts.compiler_options.paths, specifier)?;
    for template in templates {
        let templ_resolved = template.replace('*', &captured);
        let joined = normalize_join(&base_url, &templ_resolved);
        if let Some(found) = try_ecma_extensions(&joined, path_set) {
            return Some(found);
        }
    }
    None // external package (or an unaliased bare specifier) — correctly dropped
}

/// A TS/TSX/JS specifier resolver built ONCE from the repo's file set, for a checker that
/// needs to resolve specifiers the import graph does not carry (an `export { a as b } from`,
/// whose exported name [`build_import_graph`] drops). It holds the same path set and
/// `tsconfig.json` the graph builder uses and delegates to the same `resolve_ecma`, so the two
/// can never disagree about where a specifier points. Same policy as the graph: an
/// unresolvable or external specifier is `None`, never a guess.
pub struct EcmaResolver<'a> {
    path_set: HashSet<&'a str>,
    tsconfigs: TsConfigs,
}

impl<'a> EcmaResolver<'a> {
    pub fn new(files: &'a [(String, String)]) -> Self {
        Self {
            path_set: files.iter().map(|(p, _)| p.as_str()).collect(),
            tsconfigs: TsConfigs::load(files),
        }
    }

    /// The repo-relative file `specifier` (written in `importer`) points at, if any.
    pub fn resolve(&self, importer: &str, specifier: &str) -> Option<String> {
        resolve_ecma(importer, specifier, &self.path_set, &self.tsconfigs)
    }
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
    let stripped = path
        .strip_suffix(".py")
        .or_else(|| path.strip_suffix(".pyi"))
        .unwrap_or(path);
    let mut segs: Vec<String> = stripped
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
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
            let joined = if base.is_empty() {
                rel
            } else {
                format!("{base}/{rel}")
            };
            return try_python_extensions(&joined, path_set);
        }
        // Bare `from . import x` / `from .. import x` — try `x` as a submodule of `base`.
        let name = imp.names.first()?;
        let joined = if base.is_empty() {
            name.clone()
        } else {
            format!("{base}/{name}")
        };
        return try_python_extensions(&joined, path_set);
    }
    resolve_python_absolute(spec, path_set)
}

/// The absolute-dotted-specifier branch of [`resolve_python`]: the repo's actual source root
/// (`src/`, `app/`, repo root, ...) isn't known to this extractor, so match by SUFFIX against
/// every `.py`/`.pyi` file's own derived module path. Zero or multiple matches both drop the
/// edge (the ambiguity case this module's docs call out) rather than guess.
fn resolve_python_absolute(spec: &str, path_set: &HashSet<&str>) -> Option<String> {
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

/// The repo file an ABSOLUTE dotted Python module name (`pkg.cli`) names, by the same suffix
/// match the import graph uses for `import pkg.cli`: a package's `__init__.py` is the package.
/// Zero or several matches give `None` (never a guess). For a caller that holds a module name
/// rather than an `import` statement, such as a `pyproject.toml` script entry.
pub(crate) fn resolve_python_module(module: &str, path_set: &HashSet<&str>) -> Option<String> {
    resolve_python_absolute(module, path_set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
    }

    // ── EcmaResolver: the public single-specifier entry point ──────────────────

    #[test]
    fn ecma_resolver_resolves_relative_index_and_alias_specifiers() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
            ),
            ("src/a/consumer.ts", ""),
            ("src/a/utils.ts", ""),
            ("src/b/widgets/index.ts", ""),
        ]);
        let r = EcmaResolver::new(&f);
        assert_eq!(
            r.resolve("src/a/consumer.ts", "./utils").as_deref(),
            Some("src/a/utils.ts")
        );
        assert_eq!(
            r.resolve("src/a/consumer.ts", "../b/widgets").as_deref(),
            Some("src/b/widgets/index.ts")
        );
        assert_eq!(
            r.resolve("src/a/consumer.ts", "@/a/utils").as_deref(),
            Some("src/a/utils.ts")
        );
    }

    #[test]
    fn ecma_resolver_drops_external_and_missing_specifiers() {
        let f = files(vec![("src/a.ts", "")]);
        let r = EcmaResolver::new(&f);
        assert_eq!(r.resolve("src/a.ts", "react"), None);
        assert_eq!(r.resolve("src/a.ts", "./missing"), None);
    }

    #[test]
    fn ecma_resolver_agrees_with_the_import_graph() {
        let f = files(vec![
            ("src/routes/orders.ts", "import { db } from '../lib/db';\n"),
            ("src/lib/db.ts", "export const db = {};\n"),
        ]);
        let graph = build_import_graph(&f);
        let r = EcmaResolver::new(&f);
        assert_eq!(
            r.resolve("src/routes/orders.ts", "../lib/db"),
            Some(graph[0].to_file.clone())
        );
    }

    #[test]
    fn tsconfig_paths_longest_matching_prefix_wins_deterministically() {
        // `@acme/ui` (exact) and `@acme/ui/*` (wildcard) both exist; the order a map happens to
        // iterate them in must never decide which one `@acme/ui/Button` takes.
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {
                    "@acme/*": ["packages/wrong/*"],
                    "@acme/ui": ["packages/ui/src/index.ts"],
                    "@acme/ui/*": ["packages/ui/src/*"]}}}"#,
            ),
            ("packages/ui/src/Button.tsx", ""),
            ("packages/ui/src/index.ts", ""),
            ("packages/wrong/ui/Button.tsx", ""),
            ("packages/app/src/a.tsx", ""),
        ]);
        for _ in 0..20 {
            let r = EcmaResolver::new(&f);
            assert_eq!(
                r.resolve("packages/app/src/a.tsx", "@acme/ui/Button")
                    .as_deref(),
                Some("packages/ui/src/Button.tsx")
            );
            assert_eq!(
                r.resolve("packages/app/src/a.tsx", "@acme/ui").as_deref(),
                Some("packages/ui/src/index.ts")
            );
        }
    }

    #[test]
    fn tsconfig_paths_pattern_without_a_star_is_an_exact_match() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@acme/ui": ["packages/ui/src/index.ts"]}}}"#,
            ),
            ("packages/ui/src/index.ts", ""),
            ("a.ts", ""),
        ]);
        let r = EcmaResolver::new(&f);
        assert_eq!(
            r.resolve("a.ts", "@acme/ui").as_deref(),
            Some("packages/ui/src/index.ts")
        );
        // A different package that merely shares the prefix is NOT the aliased one.
        assert_eq!(r.resolve("a.ts", "@acme/ui-icons"), None);
        assert_eq!(r.resolve("a.ts", "@acme/ui/extra"), None);
    }

    #[test]
    fn tsconfig_paths_wildcard_must_match_prefix_and_suffix() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@c/*/api": ["src/*/api.ts"]}}}"#,
            ),
            ("src/foo/api.ts", ""),
            ("src/foo/other.ts", ""),
            ("a.ts", ""),
        ]);
        let r = EcmaResolver::new(&f);
        assert_eq!(
            r.resolve("a.ts", "@c/foo/api").as_deref(),
            Some("src/foo/api.ts")
        );
        assert_eq!(r.resolve("a.ts", "@c/foo/other"), None);
    }

    #[test]
    fn nested_tsconfig_scopes_its_paths_to_its_own_directory() {
        let f = files(vec![
            (
                "packages/a/tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#,
            ),
            ("src/lib/x.ts", ""),
            ("packages/a/src/lib/x.ts", ""),
            ("packages/a/src/foo.ts", ""),
            ("src/other.ts", ""),
            ("packages/b/src/bar.ts", ""),
        ]);
        let r = EcmaResolver::new(&f);
        // Under packages/a, `@/` means packages/a/src, not the repo's src.
        assert_eq!(
            r.resolve("packages/a/src/foo.ts", "@/lib/x").as_deref(),
            Some("packages/a/src/lib/x.ts")
        );
        // Files outside that tsconfig's directory are not governed by it.
        assert_eq!(r.resolve("src/other.ts", "@/lib/x"), None);
        assert_eq!(r.resolve("packages/b/src/bar.ts", "@/lib/x"), None);
    }

    #[test]
    fn nearest_ancestor_tsconfig_wins_and_base_url_is_relative_to_it() {
        let f = files(vec![
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["root-src/*"]}}}"#,
            ),
            (
                "packages/a/tsconfig.json",
                r#"{"compilerOptions": {"baseUrl": "src", "paths": {"@/*": ["*"]}}}"#,
            ),
            ("root-src/m.ts", ""),
            ("packages/a/src/m.ts", ""),
            ("packages/a/src/foo.ts", ""),
            ("top.ts", ""),
        ]);
        let r = EcmaResolver::new(&f);
        assert_eq!(
            r.resolve("packages/a/src/foo.ts", "@/m").as_deref(),
            Some("packages/a/src/m.ts")
        );
        assert_eq!(r.resolve("top.ts", "@/m").as_deref(), Some("root-src/m.ts"));
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
            (
                "src/controllers/orders.ts",
                "import { repo } from '@/repositories/orders';\n",
            ),
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
            (
                "src/controllers/orders.ts",
                "import { repo } from '@/repositories/orders';\n",
            ),
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
            (
                "crates/checks/src/arch_checker.rs",
                "pub trait ArchChecker {}\n",
            ),
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
            (
                "crates/checks/src/extract/ecma.rs",
                "use super::rust_syn::imports;\nuse self::helper::x;\n",
            ),
            (
                "crates/checks/src/extract/rust_syn.rs",
                "pub fn imports() {}\n",
            ),
            (
                "crates/checks/src/extract/ecma/helper.rs",
                "pub fn x() {}\n",
            ),
        ]);
        let graph = build_import_graph(&f);
        assert!(
            graph
                .iter()
                .any(|e| e.to_file == "crates/checks/src/extract/rust_syn.rs"),
            "{graph:#?}"
        );
        assert!(
            graph
                .iter()
                .any(|e| e.to_file == "crates/checks/src/extract/ecma/helper.rs"),
            "{graph:#?}"
        );
    }

    #[test]
    fn rust_bare_extern_crate_specifier_is_dropped() {
        let f = files(vec![(
            "crates/checks/src/lib.rs",
            "use serde::Deserialize;\n",
        )]);
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

    // ── reachable_from_all (multi-root) ─────────────────────────────────────────

    fn chain_graph() -> Vec<ResolvedImport> {
        build_import_graph(&files(vec![
            ("src/a.ts", "import './b';\n"),
            ("src/b.ts", "export const b = 1;\n"),
            ("src/c.ts", "import './d';\n"),
            ("src/d.ts", "export const d = 1;\n"),
            ("src/e.ts", "export const e = 1;\n"),
        ]))
    }

    #[test]
    fn reachable_from_all_unions_the_reach_of_every_start() {
        let graph = chain_graph();
        let reached = reachable_from_all(&graph, ["src/a.ts", "src/c.ts"]);
        let mut got: Vec<&str> = reached.iter().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(got, ["src/a.ts", "src/b.ts", "src/c.ts", "src/d.ts"]);
    }

    #[test]
    fn reachable_from_all_with_one_start_matches_reachable_from() {
        let graph = chain_graph();
        assert_eq!(
            reachable_from_all(&graph, ["src/a.ts"]),
            reachable_from(&graph, "src/a.ts")
        );
    }

    #[test]
    fn reachable_from_all_with_no_starts_reaches_nothing() {
        let graph = chain_graph();
        assert!(reachable_from_all(&graph, std::iter::empty::<&str>()).is_empty());
    }

    #[test]
    fn reachable_from_all_counts_a_start_that_is_not_in_the_graph() {
        let reached = reachable_from_all(&[], ["src/nope.ts"]);
        assert_eq!(reached.len(), 1);
        assert!(reached.contains("src/nope.ts"));
    }

    #[test]
    fn reachable_from_all_terminates_on_a_cycle_with_two_starts() {
        let graph = build_import_graph(&files(vec![
            ("src/a.ts", "import './b';\n"),
            ("src/b.ts", "import './a';\n"),
        ]));
        let reached = reachable_from_all(&graph, ["src/a.ts", "src/b.ts"]);
        assert_eq!(reached.len(), 2);
    }

    // ── resolve_python_module ───────────────────────────────────────────────────

    #[test]
    fn resolve_python_module_matches_files_and_packages_by_suffix() {
        let f = files(vec![
            ("src/pkg/__init__.py", ""),
            ("src/pkg/cli.py", ""),
            ("src/pkg/sub/__init__.py", ""),
        ]);
        let set: HashSet<&str> = f.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            resolve_python_module("pkg.cli", &set).as_deref(),
            Some("src/pkg/cli.py")
        );
        assert_eq!(
            resolve_python_module("pkg.sub", &set).as_deref(),
            Some("src/pkg/sub/__init__.py")
        );
        assert_eq!(
            resolve_python_module("pkg", &set).as_deref(),
            Some("src/pkg/__init__.py")
        );
        assert_eq!(resolve_python_module("pkg.missing", &set), None);
    }

    #[test]
    fn resolve_python_module_drops_an_ambiguous_name() {
        let f = files(vec![("a/svc/cli.py", ""), ("b/svc/cli.py", "")]);
        let set: HashSet<&str> = f.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(resolve_python_module("svc.cli", &set), None);
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
