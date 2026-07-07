//! The pluggable per-stack EXTRACTOR seam.
//!
//! An [`Extractor`] turns a repo's SOURCE into the neutral produced/consumed
//! lists the [`crate::integration::engine`] reconciles. Extractors are the ONLY
//! stack-aware code in the integration gate; they are the cross-agent sibling of
//! the per-stack Layer-2 linters in [`crate::multilang`], and they are SELECTED
//! off the SAME [`crate::multilang::WorktreeLanguage`] detection the linters use.
//!
//! # The review-tier fallback (the ADR's hard line)
//!
//! Where NO extractor exists for a stack (or a stack has an extractor but a given
//! SEAM within it is not statically recoverable), that seam is reported
//! REVIEW-TIER — routed to human QA and honestly labeled — NEVER a faked green.
//! [`select_extractors`] returns the extractors that CAN run; the caller records
//! every detected language with NO extractor as a review-tier seam so an
//! unsupported stack never silently passes.
//!
//! # First extractors (what is built vs staged)
//!
//! Built now, to PROVE the engine:
//! - [`GenericRouteExtractor`] — ENDPOINT reconciliation. Recognizes the common
//!   route-declaration and route-call idioms across the web stacks (express /
//!   fastify / axum / flask / gin style `METHOD "/path"` plus fetch/axios/reqwest
//!   client calls). Routes normalize cleanly across stacks (method + path), so one
//!   generic extractor covers the endpoint seam for several stacks at once.
//! - [`GenericEventExtractor`] — EVENT emit-vs-consume. Recognizes the common
//!   `emit("name")` / `publish("name")` and `on("name")` / `subscribe("name")`
//!   idioms.
//!
//! Staged (listed in the ADR): full typed request/response SCHEMA recovery,
//! migration-vs-entity reconciliation, config-key declaration-vs-read, and
//! stack-native AST extractors (tree-sitter) that replace the line-idiom
//! heuristics with precise parses.

use std::path::{Path, PathBuf};

use crate::integration::vocab::{
    normalize_path, ArtifactKind, Consumed, Produced, RepoArtifacts,
};
use crate::multilang::WorktreeLanguage;

/// The pluggable per-stack extractor contract. An extractor is handed a repo dir
/// and returns its produced/consumed artifacts. Pure w.r.t. the engine: it only
/// READS source; it never writes.
pub trait Extractor: Send + Sync {
    /// A stable name for logging / the review-tier report.
    fn name(&self) -> &'static str;

    /// The seam this extractor covers (for review-tier bookkeeping: a stack may
    /// have an endpoint extractor but no event extractor).
    fn seam(&self) -> Seam;

    /// Extract this repo's produced + consumed artifacts. Best-effort: an
    /// unreadable file is skipped, never fatal (breadth over abort, like the
    /// language detector). `repo` is the `owner/repo` label stamped on records.
    fn extract(&self, repo: &str, dir: &Path) -> RepoArtifacts;
}

/// The cross-agent seams an extractor can cover. Used to know which seams a
/// detected stack has an extractor for (the rest are review-tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seam {
    Endpoint,
    Event,
}

impl Seam {
    pub fn label(&self) -> &'static str {
        match self {
            Seam::Endpoint => "endpoint",
            Seam::Event => "event",
        }
    }
}

/// Select the extractors that can run for a detected language.
///
/// This is the extractor sibling of [`crate::multilang::runner_for_worktree`]'s
/// language dispatch. Both endpoint and event extractors are GENERIC (line-idiom
/// heuristics that recognize the common cross-stack spellings), so they apply to
/// every language that ships web/event code. A language we recognize but for
/// which a seam is not recoverable returns FEWER extractors — the caller then
/// records the uncovered seam as review-tier.
///
/// `Unknown` languages get NO extractors (every seam is review-tier there).
pub fn select_extractors(lang: WorktreeLanguage) -> Vec<Box<dyn Extractor>> {
    match lang {
        // The generic idiom extractors cover the endpoint + event seams across
        // every stack that writes web/event code. This is deliberately NOT a
        // per-language special-case: the engine stays stack-agnostic, and adding
        // a precise stack-native (AST) extractor later just swaps the box here.
        WorktreeLanguage::Rust
        | WorktreeLanguage::JavaScript
        | WorktreeLanguage::Python
        | WorktreeLanguage::Go
        | WorktreeLanguage::Ruby
        | WorktreeLanguage::Java
        | WorktreeLanguage::CSharp => vec![
            Box::new(GenericRouteExtractor),
            Box::new(GenericEventExtractor),
        ],
        // No manifest recognized → no extractor → every seam is review-tier.
        WorktreeLanguage::Unknown => vec![],
    }
}

/// The seams for which NO selected extractor exists — the review-tier set. Given
/// the extractors chosen for a language, return the seams that go to human QA.
/// A language with no extractors returns ALL seams; a fully-covered language
/// returns none.
pub fn uncovered_seams(extractors: &[Box<dyn Extractor>]) -> Vec<Seam> {
    let covered: Vec<Seam> = extractors.iter().map(|e| e.seam()).collect();
    [Seam::Endpoint, Seam::Event]
        .into_iter()
        .filter(|s| !covered.contains(s))
        .collect()
}

// ── shared source walking ─────────────────────────────────────────────────────

/// Directories skipped while walking a repo for source (mirrors
/// [`crate::multilang`]'s prune list): build output, vendored deps, VCS.
const PRUNED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".camerata-venv",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    ".next",
];

/// Source-file extensions the idiom extractors scan. Language-agnostic on
/// purpose: one walk feeds every extractor.
const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "rb", "java", "cs",
];

/// Collect the source files under `dir`, pruning vendored/build dirs. Sorted for
/// deterministic extraction order.
fn collect_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let name = entry.file_name();
            if PRUNED_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            subdirs.push(path);
        } else if ft.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if SOURCE_EXTS.contains(&ext) {
                    out.push(path);
                }
            }
        }
    }
    for sub in subdirs {
        walk(&sub, out);
    }
}

/// The repo-relative display path for a file (for finding locations).
fn rel(dir: &Path, file: &Path) -> String {
    file.strip_prefix(dir)
        .unwrap_or(file)
        .to_string_lossy()
        .to_string()
}

// ── endpoint extractor ─────────────────────────────────────────────────────────

/// The HTTP methods the route heuristics recognize.
const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// A generic, stack-agnostic ENDPOINT extractor.
///
/// It recognizes two families of line idiom, which cover the route-DECLARATION
/// and route-CALL spellings shared across the mainstream web stacks:
///
/// PRODUCED (a route the repo SERVES), e.g.:
/// - `app.get("/users/:id", ...)` / `router.post('/x', ...)` (express/fastify/koa)
/// - `.route("/users/{id}", get(handler))` / `app.at("/x").get(...)` (axum/tide)
/// - `@app.get("/x")` / `@GetMapping("/x")` (fastapi/flask/spring)
/// - `r.GET("/x", h)` (gin)
///
/// CONSUMED (a route the repo CALLS), e.g.:
/// - `fetch("/users/" + id, { method: "POST" })` / `axios.post("/x")`
/// - `reqwest::Client::new().get("/x")` / `http.NewRequest("GET", "/x", ...)`
///
/// A `// camerata:integration-guard` (or a nearby auth-middleware marker) sets the
/// producer's `guarded` flag; a `// camerata:ui-gated` marker on a consumer call
/// sets `ui_gated`. These explicit markers keep guard/gating status DETERMINISTIC
/// rather than guessed; a stack-native AST extractor (staged) would infer them.
pub struct GenericRouteExtractor;

impl Extractor for GenericRouteExtractor {
    fn name(&self) -> &'static str {
        "generic-route"
    }
    fn seam(&self) -> Seam {
        Seam::Endpoint
    }
    fn extract(&self, repo: &str, dir: &Path) -> RepoArtifacts {
        let mut produced = Vec::new();
        let mut consumed = Vec::new();
        for file in collect_source_files(dir) {
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            let rel_path = rel(dir, &file);
            for (i, line) in content.lines().enumerate() {
                let loc = format!("{rel_path}:{}", i + 1);
                if let Some((method, path)) = parse_route_declaration(line) {
                    produced.push(Produced {
                        repo: repo.to_string(),
                        kind: ArtifactKind::Endpoint {
                            method,
                            path: normalize_path(&path),
                        },
                        shape: None,
                        guarded: Some(line_has_guard(line)),
                        location: loc,
                    });
                } else if let Some((method, path)) = parse_route_call(line) {
                    consumed.push(Consumed {
                        repo: repo.to_string(),
                        kind: ArtifactKind::Endpoint {
                            method,
                            path: normalize_path(&path),
                        },
                        shape: None,
                        ui_gated: Some(line_is_ui_gated(line)),
                        location: loc,
                    });
                }
            }
        }
        RepoArtifacts {
            repo: repo.to_string(),
            produced,
            consumed,
        }
    }
}

/// True when a route-declaration line carries an explicit guard marker.
fn line_has_guard(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("camerata:integration-guard")
        || l.contains("requireauth")
        || l.contains("require_auth")
        || l.contains("@preauthorize")
        || l.contains("ensure_authorized")
        || l.contains(".guard(")
}

/// True when a consumer call is an explicitly UI-gated affordance.
fn line_is_ui_gated(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("camerata:ui-gated") || l.contains("can(") || l.contains("_can.")
}

/// True when a line looks like an HTTP-CLIENT call (fetch/axios/reqwest/etc.),
/// so a `.post(` on it is a route CALL, not a route declaration. Keeps the two
/// parsers from double-counting the same `.method(` token.
fn looks_like_client_call(lower_line: &str) -> bool {
    ["fetch", "axios", "reqwest", "newrequest", "http.", "httpclient", ".request("]
        .iter()
        .any(|k| lower_line.contains(k))
}

/// Parse a route DECLARATION idiom out of one line → (METHOD, path).
///
/// Recognizes `X.METHOD("path"` where `X` is any receiver (app/router/r/...),
/// the axum/actix `.route("path", METHOD(...))` form, and the decorator /
/// annotation forms `@app.METHOD("path")` / `@METHODMapping("path")`.
fn parse_route_declaration(line: &str) -> Option<(String, String)> {
    let lower = line.to_lowercase();

    // A client call is a CONSUMER, not a producer — do not treat its `.post(` as
    // a route declaration.
    if looks_like_client_call(&lower) {
        return None;
    }

    // axum/actix `.route("/users/{id}", get(handler))`: path is the first string,
    // method is a verb token elsewhere on the line (`get(` / `post(` ...).
    if let Some(idx) = lower.find(".route(") {
        if let Some(path) = extract_first_string(&line[idx + ".route(".len()..]) {
            if is_route_path(&path) {
                for m in METHODS {
                    let ml = m.to_lowercase();
                    // `get(` / `post(` as a routing-method combinator on the line.
                    if lower.contains(&format!("{ml}(")) {
                        return Some((m.to_string(), path));
                    }
                }
                // A `.route(...)` with no recognizable verb defaults to GET.
                return Some(("GET".to_string(), path));
            }
        }
    }

    // `.get("/x"` / `.post('/x'` / `@app.get("/x"` / `r.GET("/x"`
    for m in METHODS {
        let ml = m.to_lowercase();
        for token in [format!(".{ml}("), format!(".{ml} (")] {
            if let Some(idx) = lower.find(&token) {
                if let Some(path) = extract_first_string(&line[idx + token.len()..]) {
                    if is_route_path(&path) {
                        return Some((m.to_string(), path));
                    }
                }
            }
        }
        // Spring-style `@GetMapping("/x")`.
        let mapping = format!("@{}mapping", ml);
        if let Some(idx) = lower.find(&mapping) {
            if let Some(path) = extract_first_string(&line[idx..]) {
                if is_route_path(&path) {
                    return Some((m.to_string(), path));
                }
            }
        }
    }
    None
}

/// Parse a route CALL idiom out of one line → (METHOD, path).
///
/// Recognizes `axios.post("/x")` / `client.get("/x")` style typed clients, and
/// `fetch("/x", { method: "POST" })` / `NewRequest("GET", "/x")` where the method
/// is a separate argument. When a fetch-style call omits the method it defaults
/// to GET (the web default).
fn parse_route_call(line: &str) -> Option<(String, String)> {
    let lower = line.to_lowercase();

    // Typed-client `.post("/x")` etc. (distinguished from a declaration by the
    // absence of a server receiver; we accept either — a declaration on a client
    // object is impossible, and the engine matches by identity regardless).
    // We only treat it as a CALL when the line also looks like a client call
    // (contains fetch/axios/http/request/client), to avoid double-counting a
    // server declaration as a call.
    let looks_like_client = ["fetch", "axios", "http", "request", "reqwest", "client", "url"]
        .iter()
        .any(|k| lower.contains(k));

    for m in METHODS {
        let ml = m.to_lowercase();
        let token = format!(".{ml}(");
        if looks_like_client {
            if let Some(idx) = lower.find(&token) {
                if let Some(path) = extract_first_string(&line[idx + token.len()..]) {
                    if is_route_path(&path) {
                        return Some((m.to_string(), path));
                    }
                }
            }
        }
    }

    // `fetch("/x", { method: "POST" })` / `fetch('/x')` (defaults to GET).
    if lower.contains("fetch(") {
        if let Some(idx) = lower.find("fetch(") {
            if let Some(path) = extract_first_string(&line[idx + "fetch(".len()..]) {
                if is_route_path(&path) {
                    let method = extract_method_kv(&lower).unwrap_or_else(|| "GET".to_string());
                    return Some((method, path));
                }
            }
        }
    }

    // `NewRequest("GET", "/x", ...)` (go net/http) → method is the first string,
    // path the second.
    if lower.contains("newrequest(") {
        if let Some(idx) = lower.find("newrequest(") {
            let rest = &line[idx + "newrequest(".len()..];
            if let Some(first) = extract_first_string(rest) {
                let up = first.to_uppercase();
                if METHODS.contains(&up.as_str()) {
                    if let Some(after) = rest.find(&first) {
                        let tail = &rest[after + first.len()..];
                        if let Some(path) = extract_first_string(tail) {
                            if is_route_path(&path) {
                                return Some((up, path));
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Pull a `method: "POST"` / `method:'post'` value out of a fetch options object.
fn extract_method_kv(lower_line: &str) -> Option<String> {
    let idx = lower_line.find("method")?;
    let after = &lower_line[idx + "method".len()..];
    // Skip `:` / whitespace / quote to the verb.
    let val = extract_first_string(after)?;
    let up = val.to_uppercase();
    if METHODS.contains(&up.as_str()) {
        Some(up)
    } else {
        None
    }
}

/// Extract the first single- or double-quoted string literal from `s`.
fn extract_first_string(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && (bytes[j] as char) != quote {
                j += 1;
            }
            if j <= bytes.len() {
                return Some(s[start..j.min(s.len())].to_string());
            }
        }
        i += 1;
    }
    None
}

/// Heuristic: does a string look like a route path (starts with `/`, or a bare
/// segment we can prefix)? We require a leading slash to avoid matching arbitrary
/// strings; `normalize_path` handles the rest.
fn is_route_path(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('/') && !t.contains(' ')
}

/// Recover the ENDPOINT artifact identity for a single source line, if it declares
/// or calls a route. Used to scope an inline `camerata:allow INTEGRATION-* -- ...`
/// waiver to exactly the endpoint on the annotated line (so a waiver never blankets
/// the whole rule). Returns a string matching [`ArtifactKind::identity`], e.g.
/// `"endpoint POST /members/export"`, or `None` when the line has no route.
pub fn extractor_endpoint_identity(line: &str) -> Option<String> {
    let recovered = parse_route_declaration(line).or_else(|| parse_route_call(line))?;
    let (method, path) = recovered;
    Some(
        ArtifactKind::Endpoint {
            method,
            path: normalize_path(&path),
        }
        .identity(),
    )
}

// ── event extractor ────────────────────────────────────────────────────────────

/// A generic, stack-agnostic EVENT extractor covering emit-vs-consume.
///
/// PRODUCED (an event the repo EMITS): `emit("name")`, `publish("name")`,
/// `dispatch("name")`, `raise("name")`, `send("name")`.
/// CONSUMED (an event the repo SUBSCRIBES to): `on("name")`, `subscribe("name")`,
/// `addEventListener("name")`, `@EventListener` + `handle("name")`, `listen("name")`.
pub struct GenericEventExtractor;

const EMIT_VERBS: &[&str] = &["emit", "publish", "dispatch", "raise", "produce"];
const SUB_VERBS: &[&str] = &["subscribe", "addeventlistener", "listen", "on"];

impl Extractor for GenericEventExtractor {
    fn name(&self) -> &'static str {
        "generic-event"
    }
    fn seam(&self) -> Seam {
        Seam::Event
    }
    fn extract(&self, repo: &str, dir: &Path) -> RepoArtifacts {
        let mut produced = Vec::new();
        let mut consumed = Vec::new();
        for file in collect_source_files(dir) {
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            let rel_path = rel(dir, &file);
            for (i, line) in content.lines().enumerate() {
                let loc = format!("{rel_path}:{}", i + 1);
                let lower = line.to_lowercase();
                if let Some(name) = parse_event(&lower, line, EMIT_VERBS) {
                    produced.push(Produced {
                        repo: repo.to_string(),
                        kind: ArtifactKind::Event { name },
                        shape: None,
                        guarded: None,
                        location: loc.clone(),
                    });
                }
                if let Some(name) = parse_event(&lower, line, SUB_VERBS) {
                    consumed.push(Consumed {
                        repo: repo.to_string(),
                        kind: ArtifactKind::Event { name },
                        shape: None,
                        ui_gated: None,
                        location: loc,
                    });
                }
            }
        }
        RepoArtifacts {
            repo: repo.to_string(),
            produced,
            consumed,
        }
    }
}

/// Find a `<verb>("name")` idiom for any verb in `verbs` and return the event name.
fn parse_event(lower_line: &str, raw_line: &str, verbs: &[&str]) -> Option<String> {
    for v in verbs {
        for token in [format!("{v}("), format!("{v} (")] {
            if let Some(idx) = lower_line.find(&token) {
                // Map the lower-case index back onto the raw line (same byte
                // length since we only lower-cased ASCII verbs; use raw for the
                // literal so the event name keeps its original casing).
                let start = idx + token.len();
                if start <= raw_line.len() {
                    if let Some(name) = extract_first_string(&raw_line[start..]) {
                        let n = name.trim();
                        // An event name must look like an identifier/topic, not a
                        // path (avoid matching `on("/route")`).
                        if !n.is_empty() && !n.starts_with('/') && !n.contains(' ') {
                            return Some(n.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn route_declaration_forms_are_extracted() {
        let td = TempDir::new().unwrap();
        write(td.path(), "server.js", "app.post('/members/export', handler)\n");
        write(td.path(), "routes.rs", ".route(\"/users/{id}\", get(show))\n");
        write(td.path(), "api.py", "@app.get(\"/health\")\n");
        let ex = GenericRouteExtractor;
        let a = ex.extract("api", td.path());
        let ids: Vec<String> = a.produced.iter().map(|p| p.kind.identity()).collect();
        assert!(ids.contains(&"endpoint POST /members/export".to_string()), "{ids:?}");
        assert!(ids.contains(&"endpoint GET /users/{}".to_string()), "{ids:?}");
        assert!(ids.contains(&"endpoint GET /health".to_string()), "{ids:?}");
    }

    #[test]
    fn route_call_forms_are_extracted() {
        let td = TempDir::new().unwrap();
        write(td.path(), "client.ts", "await axios.post('/members/csv', body)\n");
        write(td.path(), "hook.ts", "await fetch('/users/1', { method: 'GET' })\n");
        let ex = GenericRouteExtractor;
        let a = ex.extract("ui", td.path());
        let ids: Vec<String> = a.consumed.iter().map(|c| c.kind.identity()).collect();
        assert!(ids.contains(&"endpoint POST /members/csv".to_string()), "{ids:?}");
        // The concrete id `1` normalizes to the `{}` placeholder (matches a param route).
        assert!(ids.contains(&"endpoint GET /users/{}".to_string()), "{ids:?}");
    }

    #[test]
    fn guard_and_ui_gated_markers_set_flags() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "server.js",
            "app.post('/ban', requireAuth, handler) // camerata:integration-guard\n",
        );
        write(
            td.path(),
            "ui.ts",
            "if (org._can.ban) axios.post('/ban') // camerata:ui-gated\n",
        );
        let ex = GenericRouteExtractor;
        let prod = ex.extract("api", td.path());
        assert_eq!(prod.produced[0].guarded, Some(true));
        let cons = ex.extract("ui", td.path());
        assert_eq!(cons.consumed[0].ui_gated, Some(true));
    }

    #[test]
    fn event_emit_and_subscribe_extracted() {
        let td = TempDir::new().unwrap();
        write(td.path(), "emit.js", "bus.emit('member.created', payload)\n");
        write(td.path(), "sub.js", "bus.subscribe('member.created', h)\n");
        let ex = GenericEventExtractor;
        let a = ex.extract("api", td.path());
        assert_eq!(a.produced.len(), 1);
        assert_eq!(a.consumed.len(), 1);
        assert_eq!(a.produced[0].kind.identity(), "event member.created");
    }

    #[test]
    fn event_extractor_ignores_route_like_on_calls() {
        let td = TempDir::new().unwrap();
        // `on('/route')` is a path, not an event — must not be captured.
        write(td.path(), "x.js", "router.on('/route', h)\n");
        let ex = GenericEventExtractor;
        let a = ex.extract("api", td.path());
        assert!(a.consumed.is_empty(), "path-like on() must be ignored");
    }

    #[test]
    fn unknown_language_has_no_extractors_all_seams_review() {
        let ex = select_extractors(WorktreeLanguage::Unknown);
        assert!(ex.is_empty());
        let uncovered = uncovered_seams(&ex);
        assert!(uncovered.contains(&Seam::Endpoint));
        assert!(uncovered.contains(&Seam::Event));
    }

    #[test]
    fn known_language_covers_both_seams() {
        let ex = select_extractors(WorktreeLanguage::JavaScript);
        assert_eq!(ex.len(), 2);
        assert!(uncovered_seams(&ex).is_empty());
    }

    #[test]
    fn extraction_is_deterministic() {
        let td = TempDir::new().unwrap();
        write(td.path(), "b.js", "app.get('/b', h)\n");
        write(td.path(), "a.js", "app.get('/a', h)\n");
        let ex = GenericRouteExtractor;
        let first = ex.extract("api", td.path());
        let second = ex.extract("api", td.path());
        assert_eq!(first.produced, second.produced, "stable order");
    }

    #[test]
    fn vendored_dirs_are_pruned() {
        let td = TempDir::new().unwrap();
        write(td.path(), "node_modules/dep/index.js", "app.get('/leak', h)\n");
        write(td.path(), "src/app.js", "app.get('/real', h)\n");
        let ex = GenericRouteExtractor;
        let a = ex.extract("api", td.path());
        let ids: Vec<String> = a.produced.iter().map(|p| p.kind.identity()).collect();
        assert!(ids.contains(&"endpoint GET /real".to_string()));
        assert!(!ids.iter().any(|i| i.contains("/leak")), "vendored pruned");
    }
}
