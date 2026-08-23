//! `HandlerNoDbChecker`: Pass 4c — the PRODUCTION AST checker for `ARCH-HANDLER-NO-DB-1`. See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group D and
//! `crates/rules/principles/api-layer/arch-handler-no-db-1.toml`.
//!
//! # Supersedes the Pass 4a interim lexical promotion
//!
//! Pass 4a shipped `crate::architectural::handler_no_direct_db`, a lexical (function-name +
//! DB-handle-name + brace-depth) heuristic, wrapped in this same struct name and registered
//! `advisory_coexisting` (always LLM-advisory, regardless of config). THIS checker replaces
//! it entirely: the lexical module (`crate::architectural`) has been deleted, and the finding
//! logic below is a real structural analysis over [`crate::extract::functions`] +
//! [`crate::extract::method_calls`] — no brace-counting, no line-based comment stripping.
//! Exactly ONE checker answers `ARCH-HANDLER-NO-DB-1` after this pass.
//!
//! # Model
//!
//! For every file the v1 extractor layer covers (Rust/TS/TSX/JS/Python):
//! 1. Classify the file's role via `.camerata/architecture.toml` (`[layers]`), when present:
//!    a file classified into the layer literally named `"handlers"` (the schema's own worked
//!    example, and the convention every fixture in this design doc's Group C/D checkers uses)
//!    makes EVERY function in that file handler-scoped, deterministically — no attribute or
//!    name guessing needed, replacing that guess entirely, per design §4's own wording. A file
//!    classified into any OTHER declared layer is confirmed NOT a handler file and is skipped
//!    entirely, config having already answered the question. A file the map doesn't classify
//!    (no match, or a [`crate::architecture_config::LayerConflict`]) falls through to the
//!    heuristic tier below — the map exists but can't answer for THIS file, so this checker
//!    degrades rather than guessing a hard verdict (D3's "fall back per D3" instruction).
//! 2. Absent that layer confirmation, a function is handler-classified by a STRUCTURAL
//!    route marker in [`crate::extract::FunctionSpan::attrs`] — a Rust attribute macro
//!    (`#[get("/x")]`, `#[actix_web::get(...)]`), a Python decorator (`@app.route(...)`,
//!    `@app.get(...)`), or the synthetic Express-registration marker
//!    (`extract::ecma::express_route_marker`, `router.get('/x', (req,res)=>{})`) — or, failing
//!    that, by the function's NAME containing a handler-ish marker (`handler`, `controller`,
//!    ...), the same fallback the interim lexical checker used. Both are heuristics (an
//!    attribute macro literally named `get` isn't PROOF the project treats it as an HTTP route,
//!    and a name is even weaker), so both land at `needs-review`, never a hard verdict.
//! 3. Direct DB access = a `receiver.method(...)` call site ([`crate::extract::method_calls`])
//!    inside a handler-classified function's body (the call's line falls within the innermost
//!    containing [`crate::extract::FunctionSpan`]) whose receiver's LAST dotted/`::` segment
//!    EXACTLY matches a `[db].handles` marker (or the default marker list `db`/`pool`/`conn`/
//!    `tx`/`executor` when `[db]` isn't configured) — an exact-segment match, not a substring
//!    match, so `database_url` can never spuriously match marker `db` the way the old lexical
//!    scanner's raw-text search theoretically could.
//!
//! # D3: config-aware degradation (deterministic vs. needs-review, per-repo)
//!
//! Unlike the interim checker (`advisory_coexisting() == true`, unconditionally advisory),
//! this checker follows the SAME per-repo pattern `ImportBoundaryChecker` established in Pass
//! 4b-2: [`ArchChecker::config_unsatisfied_for`] returns `true` (rule id stays LLM-advisory)
//! unless the repo's `.camerata/architecture.toml` declares BOTH a `"handlers"` layer AND a
//! non-empty `[db].handles` list — the two facts this checker needs to answer every finding
//! deterministically. When that gate is open, most findings ARE deterministic (`high`
//! severity); a per-FILE fallback to `needs-review` can still occur for a file the layer map
//! doesn't classify even though the repo overall is "configured" — a known, deliberate,
//! per-finding-level coarsening documented the same way `ImportBoundaryChecker`'s own
//! `ARCH-API-DTOS-1` coarsening is (config-PRESENCE, not per-finding, gates the LLM-prompt
//! exclusion).
//!
//! # False-negative safeguards
//!
//! A file matching no declared layer (or a `LayerConflict`) never gets a config-confirmed
//! non-handler skip NOR a config-confirmed handler proof — it falls to the (still real, just
//! advisory) attribute/name heuristic rather than being silently dropped. A call site with no
//! containing function, or a function with neither a route marker nor a name marker, is
//! dropped, never flagged. A file whose language isn't in the v1 extractor set, or a
//! file/parse the extractor can't handle, yields zero functions/calls — fewer findings, never
//! a crash and never a fabricated one.

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH, SEVERITY_MEDIUM};
use crate::architecture_config::{architecture_config_from_files, ArchitectureConfig};
use crate::extract::{self, FunctionSpan, MethodCall};

pub const RULE_HANDLER_NO_DB: &str = "ARCH-HANDLER-NO-DB-1";

const RULE_IDS: &[&str] = &[RULE_HANDLER_NO_DB];

const INTEREST_GLOBS: &[&str] = &[
    "**/*.rs",
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.py",
    crate::architecture_config::ARCHITECTURE_CONFIG_PATH,
];

/// The declared `[layers]` name this checker treats as authoritative "handler" proof — matches
/// every worked example in the design doc and every sibling checker's own fixtures. Not
/// hardcoded anywhere else in `architecture_config` (that module stays layer-name-agnostic by
/// design), but a checker whose RULE is specifically about "handlers" is entitled to look for
/// the conventional name, exactly as `[dtos]`/`[authz]`/`[helpers]` are named, purpose-specific
/// sections.
const HANDLERS_LAYER_NAME: &str = "handlers";

/// Fallback function-NAME markers (weakest tier) — ported verbatim from the deleted interim
/// lexical checker's `DEFAULT_HANDLER_MARKERS`.
const DEFAULT_HANDLER_NAME_MARKERS: &[&str] = &["handler", "handle_", "controller", "_route", "endpoint"];

/// Fallback DB-handle-name markers, used only when `.camerata/architecture.toml` has no `[db]`
/// section (or an empty `handles` list) — ported verbatim from the deleted interim lexical
/// checker's `DEFAULT_DB_HANDLE_MARKERS`.
const DEFAULT_DB_HANDLE_MARKERS: &[&str] = &["db", "pool", "conn", "tx", "executor"];

/// Route-verb tokens recognized inside an attribute/decorator's raw text (Rust `#[get(...)]` /
/// `#[actix_web::get(...)]`, Python `@app.route(...)` / `@app.get(...)`). The synthetic
/// Express marker (`<express-route:...>`, from `extract::ecma::express_route_marker`) is
/// recognized by its own prefix, not this list.
const ROUTE_VERB_TOKENS: &[&str] = &["get", "post", "put", "delete", "patch", "route", "options", "head"];

pub struct HandlerNoDbChecker;

impl ArchChecker for HandlerNoDbChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let cfg = architecture_config_from_files(repo.files).ok().flatten();
        let db_markers = db_handle_markers(cfg.as_ref());

        let mut out = Vec::new();
        for (path, content) in repo.files {
            let Some(lang) = extract::lang_for_path(path) else {
                continue;
            };
            let fact = file_layer_fact(cfg.as_ref(), path);
            if matches!(fact, FileLayerFact::ConfirmedOtherLayer) {
                // Config already answered "not a handler file" for us — never fall back to a
                // guess for a file the map explicitly assigned elsewhere.
                continue;
            }

            let functions = extract::functions(lang, content);
            if functions.is_empty() {
                continue;
            }
            let calls = extract::method_calls(lang, content);

            for call in &calls {
                let Some(func) = innermost_containing(&functions, call.line) else {
                    continue;
                };
                let Some(verdict) = classify_function(func, &fact) else {
                    continue;
                };
                let Some(marker) = matching_db_marker(&call.receiver, &db_markers) else {
                    continue;
                };
                out.push(build_violation(path, call, func, verdict, &marker));
            }
        }
        out
    }

    /// D3: deterministic-mode requires BOTH a declared `"handlers"` layer AND a non-empty
    /// `[db].handles` list — the two facts every DETERMINISTIC (`high`-severity) finding this
    /// checker emits depends on. Absent either, this checker still runs (attrs/name fallback,
    /// `needs-review`), but the rule id stays LLM-advisory-eligible for this repo, same
    /// pattern as `ImportBoundaryChecker`.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        let Some(cfg) = architecture_config_from_files(repo.files).ok().flatten() else {
            return true;
        };
        let has_handlers_layer = cfg.layers.contains_key(HANDLERS_LAYER_NAME);
        let has_db_handles = cfg.db.as_ref().is_some_and(|db| !db.handles.is_empty());
        !(has_handlers_layer && has_db_handles)
    }
}

// ─── per-file layer classification ──────────────────────────────────────────────────────────

enum FileLayerFact {
    /// No `.camerata/architecture.toml` at all.
    NoConfig,
    /// Config present; `path` classifies into the `"handlers"` layer.
    ConfirmedHandlers,
    /// Config present; `path` classifies into some OTHER declared layer — definitively not a
    /// handler file.
    ConfirmedOtherLayer,
    /// Config present, but `path` matches no declared layer, or hits a `LayerConflict` — the
    /// map exists but can't answer for this specific file.
    Unclassified,
}

fn file_layer_fact(cfg: Option<&ArchitectureConfig>, path: &str) -> FileLayerFact {
    let Some(cfg) = cfg else {
        return FileLayerFact::NoConfig;
    };
    match cfg.layer_for_path(path) {
        Ok(Some(name)) if name == HANDLERS_LAYER_NAME => FileLayerFact::ConfirmedHandlers,
        Ok(Some(_other)) => FileLayerFact::ConfirmedOtherLayer,
        Ok(None) => FileLayerFact::Unclassified,
        Err(_layer_conflict) => FileLayerFact::Unclassified,
    }
}

fn db_handle_markers(cfg: Option<&ArchitectureConfig>) -> Vec<String> {
    if let Some(db) = cfg.and_then(|c| c.db.as_ref()) {
        if !db.handles.is_empty() {
            return db.handles.iter().map(|h| h.to_ascii_lowercase()).collect();
        }
    }
    DEFAULT_DB_HANDLE_MARKERS.iter().map(|s| s.to_string()).collect()
}

// ─── per-function handler classification ────────────────────────────────────────────────────

enum Verdict {
    /// Config-confirmed: the file classifies into the `"handlers"` layer.
    Deterministic,
    /// A route attribute/decorator/registration marker matched, but no layer map confirmed it.
    NeedsReviewAttr(String),
    /// Only the function's NAME matched a handler-ish marker — the weakest tier.
    NeedsReviewName,
}

fn classify_function(func: &FunctionSpan, fact: &FileLayerFact) -> Option<Verdict> {
    match fact {
        FileLayerFact::ConfirmedOtherLayer => None,
        FileLayerFact::ConfirmedHandlers => Some(Verdict::Deterministic),
        FileLayerFact::NoConfig | FileLayerFact::Unclassified => {
            if let Some(marker) = route_marker(&func.attrs) {
                Some(Verdict::NeedsReviewAttr(marker))
            } else if name_matches(&func.name, DEFAULT_HANDLER_NAME_MARKERS) {
                Some(Verdict::NeedsReviewName)
            } else {
                None
            }
        }
    }
}

/// The innermost (tightest-spanning) [`FunctionSpan`] whose `[start_line, end_line]` contains
/// `line`, or `None` when `line` sits outside every function (a top-level statement, a
/// module-level `const` initializer, ...) — never flagged, matching the old lexical checker's
/// own "DB call outside any handler body is not flagged" behavior.
fn innermost_containing(functions: &[FunctionSpan], line: usize) -> Option<&FunctionSpan> {
    functions
        .iter()
        .filter(|f| f.start_line <= line && line <= f.end_line)
        .min_by_key(|f| f.end_line.saturating_sub(f.start_line))
}

/// The first attr/decorator/registration-marker text that looks like an HTTP route, or `None`.
fn route_marker(attrs: &[String]) -> Option<String> {
    attrs.iter().find(|a| is_route_marker(a)).cloned()
}

fn is_route_marker(attr: &str) -> bool {
    let lower = attr.to_ascii_lowercase();
    if lower.starts_with("<express-route:") {
        return true;
    }
    ROUTE_VERB_TOKENS.iter().any(|verb| contains_ident_call(&lower, verb))
}

/// Whether `haystack` contains `ident` as a WHOLE identifier immediately followed by `(` — a
/// word-boundary-anchored search, so `#[get("/x")]` matches `"get"` but a hypothetical
/// `#[target(...)]` does NOT spuriously match `"get"` as a substring.
fn contains_ident_call(haystack: &str, ident: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(ident) {
        let idx = start + rel;
        let before_ok = idx == 0 || !(bytes[idx - 1].is_ascii_alphanumeric() || bytes[idx - 1] == b'_');
        let after = idx + ident.len();
        let after_ok = after < bytes.len() && bytes[after] == b'(';
        if before_ok && after_ok {
            return true;
        }
        start = idx + ident.len();
    }
    false
}

/// Whether a (lowercased) function name contains any of `markers` as a substring — matches the
/// deleted interim lexical checker's own `name_matches` exactly (kept a substring match, not
/// word-boundary, for behavioral parity with the fixture this rule has always fired on).
fn name_matches(name: &str, markers: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    markers.iter().any(|m| lower.contains(&m.to_ascii_lowercase()))
}

/// Whether `receiver`'s LAST `.`/`::`-delimited segment EXACTLY matches one of `markers`
/// (already lowercased) — an exact-segment match, not a substring search over raw text, so
/// `self.db` matches marker `db` (a real hit) while an identifier like `database_url` (a single
/// segment, not equal to `db`) never spuriously matches. Mirrors the token-match discipline
/// `import_boundary_checker::matching_db_marker` already established for the import facet.
fn matching_db_marker(receiver: &str, markers: &[String]) -> Option<String> {
    let normalized = receiver.replace("::", ".");
    let last = normalized.rsplit('.').next().unwrap_or(&normalized).to_ascii_lowercase();
    markers.iter().find(|m| m.as_str() == last).cloned()
}

// ─── violation construction ──────────────────────────────────────────────────────────────────

fn build_violation(path: &str, call: &MethodCall, func: &FunctionSpan, verdict: Verdict, marker: &str) -> ArchViolation {
    let object = Some(func.name.clone());
    match verdict {
        Verdict::Deterministic => ArchViolation {
            rule_id: RULE_HANDLER_NO_DB.to_string(),
            file: path.to_string(),
            line: call.line,
            object,
            severity: SEVERITY_HIGH,
            message: format!(
                "handler `{}` (file classified in the \"handlers\" layer by .camerata/architecture.toml) \
                 calls `{}.{}(...)` directly — a `[db].handles` marker (\"{marker}\") — handlers must \
                 delegate to a service or repository, never hold a database handle themselves \
                 (ARCH-HANDLER-NO-DB-1)",
                func.name, call.receiver, call.method
            ),
        },
        Verdict::NeedsReviewAttr(attr) => ArchViolation {
            rule_id: RULE_HANDLER_NO_DB.to_string(),
            file: path.to_string(),
            line: call.line,
            object,
            severity: SEVERITY_MEDIUM,
            message: format!(
                "function `{}` carries a route marker (`{attr}`) and calls `{}.{}(...)` directly — a possible \
                 database-handle call by name (\"{marker}\") — handlers must delegate to a service/repository \
                 (ARCH-HANDLER-NO-DB-1) [needs review: classified by a route attribute/decorator/registration \
                 marker, not by a configured .camerata/architecture.toml `[layers]` \"handlers\" entry — \
                 confirm `{}` is really a request handler and the receiver is really a database handle before \
                 treating this as a violation]",
                func.name, call.receiver, call.method, func.name
            ),
        },
        Verdict::NeedsReviewName => ArchViolation {
            rule_id: RULE_HANDLER_NO_DB.to_string(),
            file: path.to_string(),
            line: call.line,
            object,
            severity: SEVERITY_MEDIUM,
            message: format!(
                "function `{}` is named like a handler and calls `{}.{}(...)` directly — a possible \
                 database-handle call by name (\"{marker}\") — handlers must delegate to a service/repository \
                 (ARCH-HANDLER-NO-DB-1) [needs review: name-heuristic classification (function/DB-handle \
                 identified by name, not by type, route attribute, or a configured \
                 .camerata/architecture.toml `[layers]` \"handlers\" entry) — confirm `{}` is really a \
                 request handler and the receiver is really a database handle before treating this as a \
                 violation]",
                func.name, call.receiver, call.method, func.name
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView { spec: "test/repo", files }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    const LAYERED_CONFIG: &str = r#"
version = 1

[layers]
handlers     = ["src/routes/**"]
services     = ["src/services/**"]
repositories = ["src/repositories/**"]

[imports]
handlers     = ["services"]
services     = ["repositories"]
repositories = []

[db]
handles    = ["db", "pool"]
allowed_in = ["repositories"]
"#;

    fn with_config(mut extra: Vec<(&str, &str)>) -> Vec<(String, String)> {
        extra.push((crate::architecture_config::ARCHITECTURE_CONFIG_PATH, LAYERED_CONFIG));
        files(extra)
    }

    // ── deterministic tier: config-confirmed "handlers" layer ─────────────────

    #[test]
    fn deterministic_rust_handler_direct_db_call_is_flagged_high_severity() {
        let f = with_config(vec![(
            "src/routes/orgs.rs",
            "async fn list_orgs(db: &Db) -> Result<Vec<Org>> {\n    let rows = db.query(\"select 1\").await?;\n    Ok(rows)\n}\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_HANDLER_NO_DB);
        assert_eq!(vs[0].file, "src/routes/orgs.rs");
        assert_eq!(vs[0].line, 2);
        assert_eq!(vs[0].object.as_deref(), Some("list_orgs"));
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert!(!vs[0].message.contains("[needs review"), "{}", vs[0].message);
    }

    #[test]
    fn deterministic_ts_handler_direct_db_call_is_flagged() {
        let f = with_config(vec![(
            "src/routes/orders.ts",
            "export function listOrders(db: Db) {\n  return db.query('select 1');\n}\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert_eq!(vs[0].line, 2);
    }

    #[test]
    fn deterministic_repository_layer_file_is_never_flagged_even_though_it_touches_db() {
        // Config CONFIRMS this file is "repositories", not "handlers" — must be skipped
        // entirely, never falling back to name/attr guessing.
        let f = with_config(vec![(
            "src/repositories/orgs_repo.rs",
            "async fn fetch_all_handler(db: &Db) -> Result<Vec<Org>> {\n    db.query(\"select 1\").await\n}\n",
        )]);
        assert!(HandlerNoDbChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn deterministic_handler_that_delegates_is_clean() {
        let f = with_config(vec![(
            "src/routes/orgs.rs",
            "async fn list_orgs(svc: &OrgService) -> Result<Vec<Org>> {\n    Ok(svc.list_orgs().await?)\n}\n",
        )]);
        assert!(HandlerNoDbChecker.check(&view(&f)).is_empty());
    }

    // ── needs-review tier: attribute/decorator/registration marker, no layer map ──

    #[test]
    fn rust_attribute_marked_handler_without_config_is_needs_review() {
        let f = files(vec![(
            "src/web.rs",
            "#[get(\"/orgs\")]\nasync fn list_orgs(db: &Db) -> Result<Vec<Org>> {\n    db.query(\"select 1\").await\n}\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("[needs review"), "{}", vs[0].message);
        assert!(vs[0].message.contains("route marker"), "{}", vs[0].message);
    }

    #[test]
    fn python_decorator_marked_handler_without_config_is_needs_review() {
        let f = files(vec![(
            "app/web.py",
            "class Orgs:\n    @app.route('/orgs')\n    def list_orgs(self):\n        self.db.query('select 1')\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("route marker"), "{}", vs[0].message);
    }

    #[test]
    fn express_registration_marked_handler_without_config_is_needs_review() {
        let f = files(vec![(
            "src/web.ts",
            "router.get('/orgs', (req, res) => {\n  db.query('select 1');\n});\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("route marker"), "{}", vs[0].message);
    }

    // ── needs-review tier: name-only fallback (parity with the deleted lexical checker) ──

    #[test]
    fn name_only_fallback_matches_the_deleted_lexical_checkers_behavior() {
        let f = files(vec![(
            "src/handler.rs",
            "async fn list_orgs_handler(db: &Db) -> Result<Vec<Org>> {\n    let rows = db.query(\"select * from orgs\").await?;\n    Ok(rows)\n}\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "src/handler.rs");
        assert_eq!(vs[0].object.as_deref(), Some("list_orgs_handler"));
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("[needs review"), "{}", vs[0].message);
        assert!(vs[0].message.contains("name-heuristic"), "{}", vs[0].message);
    }

    #[test]
    fn name_only_fallback_repository_function_is_not_flagged() {
        let f = files(vec![(
            "src/repo.rs",
            "async fn fetch_all_orgs(db: &Db) -> Result<Vec<Org>> {\n    db.query(\"select 1\").await\n}\n",
        )]);
        assert!(HandlerNoDbChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn name_only_fallback_call_outside_any_function_is_not_flagged() {
        // The `db.query(...)` call sits in a module-level `static` initializer, not inside
        // any function body at all — must never be flagged, matching the deleted lexical
        // checker's own "DB call outside any handler body is not flagged" behavior.
        let f = files(vec![(
            "src/top_level.rs",
            "fn helper_handler() {}\nstatic X: i32 = db.query(\"select 1\");\n",
        )]);
        assert!(HandlerNoDbChecker.check(&view(&f)).is_empty());
    }

    // ── proven strictly better than the deleted lexical checker ──────────────────
    //
    // The lexical checker's own doc admitted this exact limitation: "does not resolve types,
    // so it cannot distinguish a `db` field of an unrelated struct from a real DB handle" — in
    // practice, it matched ANY identifier containing the marker as a raw-text substring within
    // a `<ident>.<ident>(` shape. A field/variable named `database` (not exactly `db`) calling
    // `.execute(...)` would have been indistinguishable from a real `db.execute(...)` call by
    // the OLD checker's word-boundary-before/`.`-after text scan IF the boundary before `db`
    // were satisfied by a preceding non-alnum char — but the AST version's EXACT-segment match
    // never confuses `database` with `db`, because `database` is a single, whole identifier
    // that simply does not equal the marker string.
    #[test]
    fn ast_version_does_not_confuse_a_longer_identifier_with_an_exact_db_marker() {
        let f = files(vec![(
            "src/handler.rs",
            "async fn list_orgs_handler(database: &Database) -> Result<Vec<Org>> {\n    let rows = database.execute(\"select 1\").await?;\n    Ok(rows)\n}\n",
        )]);
        assert!(
            HandlerNoDbChecker.check(&view(&f)).is_empty(),
            "an identifier named `database` must never match the `db` marker via a partial/substring read"
        );
    }

    // ── multi-language coverage ────────────────────────────────────────────────

    #[test]
    fn python_name_only_fallback_fires() {
        let f = files(vec![(
            "app/handlers.py",
            "def list_orgs_handler(db):\n    return db.query('select 1')\n",
        )]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    // ── D3: config-aware LLM-advisory gating ──────────────────────────────────

    #[test]
    fn config_unsatisfied_without_any_config() {
        let f = files(vec![("src/handler.rs", "")]);
        assert!(HandlerNoDbChecker.config_unsatisfied_for(&view(&f)));
    }

    #[test]
    fn config_unsatisfied_when_handlers_layer_present_but_no_db_handles() {
        let cfg = "version = 1\n[layers]\nhandlers = [\"src/routes/**\"]\n";
        let f = files(vec![(crate::architecture_config::ARCHITECTURE_CONFIG_PATH, cfg)]);
        assert!(HandlerNoDbChecker.config_unsatisfied_for(&view(&f)));
    }

    #[test]
    fn config_satisfied_when_handlers_layer_and_db_handles_both_present() {
        let f = with_config(vec![]);
        assert!(!HandlerNoDbChecker.config_unsatisfied_for(&view(&f)));
        let repo = view(&f);
        let per_repo = crate::arch_checker::checker_rule_ids_for_repo(&repo);
        assert!(per_repo.contains(RULE_HANDLER_NO_DB), "{per_repo:?}");
    }

    #[test]
    fn rule_id_stays_llm_advisory_per_repo_when_unconfigured() {
        let f = files(vec![("README.md", "")]);
        let repo = view(&f);
        let per_repo = crate::arch_checker::checker_rule_ids_for_repo(&repo);
        assert!(!per_repo.contains(RULE_HANDLER_NO_DB), "{per_repo:?}");
    }

    // ── glob scoping ───────────────────────────────────────────────────────────

    #[test]
    fn checker_is_registered() {
        let ids: std::collections::HashSet<&str> =
            crate::arch_checker::all_checkers().iter().flat_map(|c| c.rule_ids().iter().copied()).collect();
        assert!(ids.contains(RULE_HANDLER_NO_DB), "{ids:?}");
    }

    #[test]
    fn exactly_one_checker_owns_arch_handler_no_db_1() {
        let owners = crate::arch_checker::all_checkers()
            .iter()
            .filter(|c| c.rule_ids().contains(&RULE_HANDLER_NO_DB))
            .count();
        assert_eq!(owners, 1, "exactly one checker must own ARCH-HANDLER-NO-DB-1 after the supersession");
    }

    #[test]
    fn non_source_extension_is_not_scoped_in() {
        let f = files(vec![("README.md", "db.query()")]);
        assert!(!crate::arch_checker::checker_applies(&HandlerNoDbChecker, &f));
    }

    // ── adversarial: malformed / truncated / non-UTF8-ish input never panics ──

    #[test]
    fn malformed_source_across_every_v1_language_does_not_panic() {
        let f = files(vec![
            ("src/a.rs", "fn {{{ not valid rust at all"),
            ("src/b.ts", "function broken( {{{ ??? "),
            ("src/c.py", "def broken(:\n  ???"),
        ]);
        let _ = HandlerNoDbChecker.check(&view(&f));
    }

    #[test]
    fn empty_and_binary_like_source_does_not_panic() {
        let weird = "\u{0}\u{1}\u{FFFD} not real code \u{FFFD}";
        let f = files(vec![("src/a.rs", ""), ("src/b.rs", weird)]);
        let _ = HandlerNoDbChecker.check(&view(&f));
    }

    #[test]
    fn layer_conflict_falls_back_to_heuristic_rather_than_a_hard_verdict_or_a_panic() {
        let conflicting_cfg = r#"
version = 1
[layers]
handlers = ["src/shared/**"]
services = ["src/shared/**"]
[imports]
handlers = []
services = []
[db]
handles = ["db"]
allowed_in = []
"#;
        let f = files(vec![
            (crate::architecture_config::ARCHITECTURE_CONFIG_PATH, conflicting_cfg),
            (
                "src/shared/x_handler.rs",
                "fn x_handler(db: &Db) -> Result<()> {\n    db.query(\"select 1\").await\n}\n",
            ),
        ]);
        // Falls through to the name-only fallback (needs-review), not a panic and not a
        // silently-dropped deterministic proof.
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
    }
}
