//! `.camerata/architecture.toml` — the operator-authored boundary-map config (D1). See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §3 for the full schema rationale.
//!
//! Same trust model as `.camerata/checks.toml` ([`crate::manifest`]): operator-authored,
//! agent-write-denied by the existing `.camerata/` hard-guard (`SEC-NO-CAMERATA-CONFIG-1`).
//! Loaded two ways with zero new plumbing, mirroring how `checks.toml` is read by BOTH the
//! scan (`RepoView`'s file slice) and the Layer-2 worktree runner:
//!
//! - [`load_architecture_config`] — reads from a repo/worktree root on disk (Layer-2 path,
//!   mirrors [`crate::manifest::load_manifest`] exactly).
//! - [`architecture_config_from_files`] — reads from an in-memory `(path, content)` slice
//!   (the scan path, `RepoView::files`).
//!
//! # Absent / malformed config
//!
//! A missing `.camerata/architecture.toml` is the COMMON case (most repos won't have one) and
//! is NEVER an error: both loaders return `Ok(None)`. A malformed file (bad TOML, wrong
//! types) returns `Err` with a clear message — never a panic, never a crash that takes a scan
//! down with it. Callers MUST treat `Err` the same as `Ok(None)` for the purpose of running
//! checkers (degrade to "unconfigured"), while still surfacing the parse error somewhere
//! visible (a scan note / log line) so a broken map doesn't look silently like "no policy".
//!
//! # Layer classification + the overlap diagnostic
//!
//! [`ArchitectureConfig::layer_for_path`] classifies a repo file into a declared `[layers]`
//! entry by glob match. Per design: two globs from DIFFERENT layers both matching the SAME
//! real file is a genuine map error (i.e. it's only detectable against real files, not from
//! the glob strings alone in general) — this returns [`LayerConflict`] rather than picking one
//! layer, so a checker calling it can treat the finding as "config error", never as a rule
//! violation. [`ArchitectureConfig::diagnostics`] additionally flags the STATICALLY detectable
//! subset of map errors (an `[imports]` entry naming an undeclared layer; the exact same glob
//! string declared under two different layers) without needing any real files at all.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

// ─── schema ──────────────────────────────────────────────────────────────────────────────

/// The parsed `.camerata/architecture.toml`. See module docs + the design doc §3 for the
/// worked example. `version` is required (a config-format tripwire: a future breaking schema
/// change bumps it, and an old loader reading a newer version fails loudly instead of
/// mis-parsing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchitectureConfig {
    pub version: u32,

    /// Layer name → path globs owning it (repo-relative). An arbitrary set of names — this
    /// crate never hardcodes "handlers"/"services"/etc.
    #[serde(default)]
    pub layers: BTreeMap<String, Vec<String>>,

    /// Allowed import direction between DECLARED layers: key may import any layer named in
    /// its value list. Any declared-layer→declared-layer edge NOT listed here is forbidden;
    /// an edge involving an UNDECLARED layer (or a file matching no layer at all) is simply
    /// not judged (see design §3's FP-avoidance semantics).
    #[serde(default)]
    pub imports: BTreeMap<String, Vec<String>>,

    #[serde(default)]
    pub db: Option<DbConfig>,

    #[serde(default)]
    pub dtos: Option<DtosConfig>,

    #[serde(default)]
    pub authz: Option<AuthzConfig>,

    #[serde(default)]
    pub helpers: Option<HelpersConfig>,

    /// `true` when this map was INFERRED from the repo's own directory layout by
    /// [`infer_architecture_config`] rather than authored in `.camerata/architecture.toml`
    /// (Pass-1 W4, finding A4). Never read from or written to TOML — `serde(skip)` — because
    /// it is a provenance fact about how the value was produced, not part of the schema.
    ///
    /// Checkers MUST branch on this: an inferred map is positive evidence about the repo's
    /// structure, but it is not a policy the project signed up to, so a verdict derived from
    /// it has to be weaker and narrower than one derived from an authored map. See
    /// [`crate::import_boundary_checker`]'s module doc for exactly what it narrows to.
    #[serde(skip)]
    pub inferred: bool,
}

/// `[db]` — arms `ARCH-STRICT-LAYERING-1` and hardens `ARCH-HANDLER-NO-DB-1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbConfig {
    /// Receiver-name markers that identify a DB-handle call (`db`, `pool`, `conn`, ...).
    pub handles: Vec<String>,
    /// Layer names permitted to hold a DB-handle call directly.
    pub allowed_in: Vec<String>,
    /// Layer names exempted for TRANSACTION FLOW CONTROL ONLY (e.g. a service wrapping a
    /// repository call in `db.transaction(...)`) — the corpus rule's explicit carve-out.
    #[serde(default)]
    pub tx_flow_control_in: Vec<String>,
}

/// `[dtos]` — arms `ARCH-API-DTOS-1` (a `default = false` rule; only runs where a project has
/// opted into this section).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DtosConfig {
    pub domain_types: Vec<String>,
    pub controllers: Vec<String>,
}

/// `[authz]` — arms `ARCH-SERVER-AUTHZ-1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthzConfig {
    pub ui_paths: Vec<String>,
    pub forbidden_ui_imports: Vec<String>,
}

/// `[helpers]` — exemption files for the single-funnel UI rules. Each key names the ONE
/// helper file (or files, for a multi-entry-point helper) exempted from the corresponding
/// rule's scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HelpersConfig {
    #[serde(default)]
    pub date_helper: Vec<String>,
    #[serde(default)]
    pub image_component: Vec<String>,
}

impl ArchitectureConfig {
    /// Classify `path` into a declared `[layers]` entry by glob match (via
    /// [`crate::arch_checker::glob_match`], the same `**`-capable matcher every other
    /// checker uses). Three outcomes:
    /// - `Ok(Some(layer))` — exactly one declared layer's globs match `path`.
    /// - `Ok(None)` — no declared layer matches (correctly un-judged, per design §3).
    /// - `Err(LayerConflict)` — TWO OR MORE declared layers match the SAME file: a broken
    ///   map, reported as a config diagnostic, never as a rule violation.
    pub fn layer_for_path(&self, path: &str) -> Result<Option<&str>, LayerConflict> {
        let matches: Vec<&str> = self
            .layers
            .iter()
            .filter(|(_, globs)| crate::arch_checker::matches_any_glob(&glob_refs(globs), path))
            .map(|(name, _)| name.as_str())
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            many => Err(LayerConflict {
                path: path.to_string(),
                layers: many.iter().map(|s| s.to_string()).collect(),
            }),
        }
    }

    /// Statically-detectable config errors — no repo files needed. Non-fatal (the config is
    /// still `Ok(Some(..))`-loadable): a caller surfaces these as scan notes / config
    /// diagnostics, never as rule findings (per design §3).
    pub fn diagnostics(&self) -> Vec<String> {
        let mut out = Vec::new();

        // `[imports]` keys/values must all be declared layers.
        for (from, targets) in &self.imports {
            if !self.layers.contains_key(from) {
                out.push(format!(
                    "architecture.toml: [imports] names undeclared layer \"{from}\" as a key \
                     (not present in [layers])"
                ));
            }
            for to in targets {
                if !self.layers.contains_key(to) {
                    out.push(format!(
                        "architecture.toml: [imports].{from} names undeclared layer \"{to}\" \
                         as an allowed target (not present in [layers])"
                    ));
                }
            }
        }

        // The exact same glob string declared under two different layers is an unambiguous
        // authoring mistake, detectable from the strings alone (no files needed).
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (layer, globs) in &self.layers {
            for glob in globs {
                if let Some(other_layer) = seen.get(glob.as_str()) {
                    if *other_layer != layer.as_str() {
                        out.push(format!(
                            "architecture.toml: glob \"{glob}\" is declared under both \
                             [layers].{other_layer} and [layers].{layer}"
                        ));
                    }
                } else {
                    seen.insert(glob.as_str(), layer.as_str());
                }
            }
        }

        out
    }
}

fn glob_refs(globs: &[String]) -> Vec<&str> {
    globs.iter().map(|s| s.as_str()).collect()
}

/// Two or more declared `[layers]` match the same file — see
/// [`ArchitectureConfig::layer_for_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerConflict {
    pub path: String,
    pub layers: Vec<String>,
}

impl std::fmt::Display for LayerConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "architecture.toml: \"{}\" matches globs from multiple declared layers ({}) — \
             fix the overlapping globs before this file's imports can be judged",
            self.path,
            self.layers.join(", ")
        )
    }
}

// ─── loader ──────────────────────────────────────────────────────────────────────────────

pub const ARCHITECTURE_CONFIG_PATH: &str = ".camerata/architecture.toml";

/// Error returned when `.camerata/architecture.toml` exists but cannot be parsed.
#[derive(Debug)]
pub struct ArchitectureConfigParseError {
    pub path: String,
    pub cause: String,
}

impl std::fmt::Display for ArchitectureConfigParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to parse {}: {}", self.path, self.cause)
    }
}

fn parse_str(text: &str, path_label: &str) -> Result<ArchitectureConfig, ArchitectureConfigParseError> {
    toml::from_str::<ArchitectureConfig>(text).map_err(|e| ArchitectureConfigParseError {
        path: path_label.to_string(),
        cause: e.to_string(),
    })
}

/// Load `.camerata/architecture.toml` from a repo/worktree root on disk (the Layer-2 path —
/// mirrors [`crate::manifest::load_manifest`] exactly: `Ok(Some(_))` found+parsed,
/// `Ok(None)` absent, `Err(_)` found-but-malformed). Never panics.
pub fn load_architecture_config(
    repo_root: &Path,
) -> Result<Option<ArchitectureConfig>, ArchitectureConfigParseError> {
    let path = repo_root.join(ARCHITECTURE_CONFIG_PATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ArchitectureConfigParseError {
                path: path.display().to_string(),
                cause: e.to_string(),
            })
        }
    };
    parse_str(&text, &path.display().to_string()).map(Some)
}

/// Load `.camerata/architecture.toml` from an in-memory `(path, content)` file slice (the scan
/// path — `RepoView::files` already carries every file the scan read; this just looks for the
/// one at [`ARCHITECTURE_CONFIG_PATH`]). Same `Ok(Some)`/`Ok(None)`/`Err` contract as
/// [`load_architecture_config`].
pub fn architecture_config_from_files(
    files: &[(String, String)],
) -> Result<Option<ArchitectureConfig>, ArchitectureConfigParseError> {
    match files.iter().find(|(p, _)| p == ARCHITECTURE_CONFIG_PATH) {
        Some((_, content)) => parse_str(content, ARCHITECTURE_CONFIG_PATH).map(Some),
        None => Ok(None),
    }
}

// ─── inference (Pass-1 W4, finding A4) ───────────────────────────────────────────────────
//
// A4: the only four `ARCH-*` rules with real static analysis were gated on the SCANNED repo
// carrying a `.camerata/architecture.toml`, so they were switched off on every repo Camerata
// has ever audited. A brownfield client repo will never have that file — it is OUR config
// format — so "the operator will write one" is not a real answer for a brownfield audit, which
// is the product.
//
// The replacement is inference from the repo's own layout, under three hard constraints:
//
// 1. POSITIVE EVIDENCE ONLY. Inference returns `None` unless at least two DISTINCT canonical
//    layers each own at least one real source file. A repo with no recognisable layering gets
//    no inferred map and the checkers abstain exactly as before — correct under requirement
//    H1 ("a rule that does not run because it is irrelevant to the scanned stack is CORRECT").
// 2. HIGH-CONFIDENCE TOKENS ONLY. The directory-segment vocabulary below deliberately excludes
//    overloaded names — `api` (as often a client for a REMOTE api as a handler layer), `views`
//    (handlers in Django, templates in Rails), `models` (domain in Django, ORM rows
//    elsewhere), `types`, `store`, `lib`, `utils`. A token earns its place only if its meaning
//    is stable across ecosystems.
// 3. THE INFERRED `[imports]` MAP IS PERMISSIVE. Every layer may import itself and everything
//    BELOW it; only UPWARD edges are denied. Guessing a project's full allow-list would flood
//    a brownfield repo with hundreds of findings against a map nobody authored, which is worse
//    than the silence it replaced. An upward edge (a repository importing a handler, a domain
//    type importing a service) is dependency inversion and is a real defect regardless of
//    project taste, so it is the one verdict an inferred map can support honestly.
//
// What inference deliberately does NOT enable: `ARCH-STRICT-LAYERING-1` (both facets need
// `[db].handles`, and inferring DB-client markers means substring-matching a keyword list
// against import specifiers and receiver names — exactly the lexical-keyword-list shape the
// 2026-10-09 review explicitly banned) and `ARCH-API-DTOS-1` (opt-in by its own TOML's
// `default = false`; absent an opt-in it is irrelevant, not orphaned). Both stay config-gated,
// and both still reach the semantic tier, which as of W4 excludes nothing.

/// The canonical layers inference can recognise, ordered from the transport boundary DOWN to
/// pure data, together with the directory-segment tokens that identify each. The ORDER is
/// load-bearing: it defines "below", and therefore which import edges are upward.
///
/// Matching is on a whole path SEGMENT, case-insensitively — `src/repositories/user.rs` hits
/// `repositories`, while `src/repository_helpers.rs` does not. See the §2 constraint above for
/// why the vocabulary is this short.
pub const INFERRED_LAYER_TOKENS: &[(&str, &[&str])] = &[
    ("handlers", &["handlers", "controllers", "routes", "endpoints", "resolvers"]),
    ("services", &["services", "usecases", "use_cases", "interactors"]),
    ("repositories", &["repositories", "repository", "repos", "dao", "persistence"]),
    ("domain", &["domain", "entities"]),
];

/// The index of `layer` in [`INFERRED_LAYER_TOKENS`], i.e. how close it sits to the transport
/// boundary (0 = closest). `None` for any layer name inference did not produce — an AUTHORED
/// config's layer names are arbitrary by design (the schema never hardcodes
/// "handlers"/"services"), so there is no canonical rank for them and callers must not invent
/// one.
pub fn inferred_layer_rank(layer: &str) -> Option<usize> {
    INFERRED_LAYER_TOKENS.iter().position(|(name, _)| *name == layer)
}

/// Whether `path` has a whole segment equal (case-insensitively) to `token`.
fn path_has_segment(path: &str, token: &str) -> bool {
    path.split('/').any(|seg| seg.eq_ignore_ascii_case(token))
}

/// Infer a boundary map from a repo's own directory layout, for a repo with no authored
/// `.camerata/architecture.toml`. See the section doc above for the three constraints this
/// obeys; `None` whenever the evidence is too thin to support any verdict (fewer than two
/// distinct layers with real files), which is the common case and is not an error.
///
/// Only files whose extension the extractor layer understands count as evidence — a layer
/// "found" solely because of a `docs/domain/README.md` is not a code layer.
pub fn infer_architecture_config(files: &[(String, String)]) -> Option<ArchitectureConfig> {
    // layer name -> the tokens that actually matched at least one real source file.
    let mut matched: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    for (path, _) in files {
        if crate::extract::lang_for_path(path).is_none() {
            continue;
        }
        for (layer, tokens) in INFERRED_LAYER_TOKENS {
            for token in *tokens {
                if path_has_segment(path, token) {
                    matched.entry(*layer).or_default().insert(*token);
                }
            }
        }
    }
    if matched.len() < 2 {
        return None;
    }
    let layers: BTreeMap<String, Vec<String>> = matched
        .iter()
        .map(|(layer, tokens)| {
            let globs = tokens.iter().map(|t| format!("**/{t}/**")).collect();
            ((*layer).to_string(), globs)
        })
        .collect();
    // Permissive, upward-deny-only: each layer may import itself and everything below it,
    // restricted to the layers actually present in THIS repo.
    let present: Vec<&'static str> = INFERRED_LAYER_TOKENS
        .iter()
        .map(|(l, _)| *l)
        .filter(|l| matched.contains_key(l))
        .collect();
    let imports: BTreeMap<String, Vec<String>> = present
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            let allowed = present[i..].iter().map(|l| (*l).to_string()).collect();
            ((*layer).to_string(), allowed)
        })
        .collect();
    Some(ArchitectureConfig {
        version: 1,
        layers,
        imports,
        // Left unset on purpose: see the section doc's "what inference deliberately does NOT
        // enable". `db` would require a DB-client keyword list; `dtos` is opt-in by rule
        // design; `authz`/`helpers` are project-specific exemption maps with no layout signal.
        db: None,
        dtos: None,
        authz: None,
        helpers: None,
        inferred: true,
    })
}

/// The authored map for `files` if there is one, else an inferred one, else `None`. The single
/// entry point every config-gated checker should use so there is ONE code path rather than a
/// parallel "inferred checker". A malformed authored config is treated as absent (the module
/// doc's standing contract) and does NOT fall through to inference — a broken map the operator
/// wrote is a config error to surface, not an invitation to guess.
pub fn architecture_config_or_inferred(files: &[(String, String)]) -> Option<ArchitectureConfig> {
    match architecture_config_from_files(files) {
        Ok(Some(cfg)) => Some(cfg),
        Ok(None) => infer_architecture_config(files),
        Err(_) => None,
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── req_a4: layer-map inference (Pass-1 W4) ─────────────────────────────────────────────

    fn infer(pairs: &[&str]) -> Option<ArchitectureConfig> {
        let files: Vec<(String, String)> =
            pairs.iter().map(|p| ((*p).to_string(), String::new())).collect();
        infer_architecture_config(&files)
    }

    /// req_a4: inference needs POSITIVE EVIDENCE — at least two DISTINCT canonical layers, each
    /// owning at least one real source file. One layer proves nothing about direction, so it
    /// yields no map and the checkers abstain (requirement H1: irrelevant is CORRECT).
    ///
    /// FAILS BEFORE THE FIX: `infer_architecture_config` did not exist; there was no path from
    /// a repo with no `.camerata/architecture.toml` to any layer map at all.
    #[test]
    fn req_a4_inference_requires_two_distinct_layers_with_real_source_files() {
        assert!(infer(&["src/controllers/a.rs", "src/controllers/b.rs"]).is_none());
        assert!(infer(&["src/main.rs", "src/util.rs"]).is_none());
        let cfg = infer(&["src/controllers/a.rs", "src/repositories/b.rs"])
            .expect("two distinct layers is sufficient evidence");
        assert!(cfg.inferred);
        assert_eq!(cfg.layers.len(), 2, "{:?}", cfg.layers);
        assert_eq!(cfg.layers.get("handlers").map(Vec::as_slice), Some(&["**/controllers/**".to_string()][..]));
    }

    /// req_a4: only files the extractor layer understands count as evidence. A `domain` layer
    /// "found" solely because of a markdown file under `docs/domain/` is not a code layer.
    #[test]
    fn req_a4_inference_only_counts_real_source_files() {
        assert!(infer(&["src/controllers/a.rs", "docs/domain/overview.md"]).is_none());
        assert!(infer(&["src/controllers/a.rs", "src/domain/order.rs"]).is_some());
    }

    /// req_a4: the inferred `[imports]` map is PERMISSIVE — every layer may import itself and
    /// everything BELOW it; only upward edges are denied. Guessing a project's real allow-list
    /// would flood a brownfield repo against a policy nobody authored.
    #[test]
    fn req_a4_inferred_imports_map_denies_only_upward_edges() {
        let cfg = infer(&[
            "src/controllers/a.rs",
            "src/services/b.rs",
            "src/repositories/c.rs",
            "src/domain/d.rs",
        ])
        .expect("four layers");
        let handlers = cfg.imports.get("handlers").expect("handlers key");
        for below in ["handlers", "services", "repositories", "domain"] {
            assert!(handlers.iter().any(|l| l == below), "handlers -> {below}: {handlers:?}");
        }
        // The bottom layer may import only itself.
        assert_eq!(cfg.imports.get("domain").map(Vec::as_slice), Some(&["domain".to_string()][..]));
        // A repository may NOT import a handler (the one verdict inference supports).
        let repos = cfg.imports.get("repositories").expect("repositories key");
        assert!(!repos.iter().any(|l| l == "handlers"), "{repos:?}");
        assert!(!repos.iter().any(|l| l == "services"), "{repos:?}");
    }

    /// req_a4: the token vocabulary deliberately excludes overloaded directory names —
    /// `api` (as often a client for a REMOTE api as a handler layer), `views` (handlers in
    /// Django, templates in Rails), `models`, `types`, `store`, `lib`, `utils`. A repo laid out
    /// only with those must NOT produce an inferred map.
    #[test]
    fn req_a4_inference_ignores_overloaded_directory_names() {
        assert!(infer(&[
            "src/api/client.ts",
            "src/views/home.ts",
            "src/models/user.ts",
            "src/types/index.ts",
            "src/store/slice.ts",
            "src/lib/helper.ts",
            "src/utils/fmt.ts",
        ])
        .is_none());
    }

    /// req_a4: the layer ORDER is load-bearing (it defines "below"), and ranks exist ONLY for
    /// inferred layer names — an authored config's layer names are arbitrary by design, so
    /// callers must never invent a rank for them.
    #[test]
    fn req_a4_inferred_layer_rank_orders_boundary_to_data_and_is_none_for_authored_names() {
        let ranks: Vec<usize> = ["handlers", "services", "repositories", "domain"]
            .iter()
            .map(|l| inferred_layer_rank(l).expect("canonical layer"))
            .collect();
        assert_eq!(ranks, vec![0, 1, 2, 3]);
        assert_eq!(inferred_layer_rank("my-bespoke-layer"), None);
    }

    /// req_a4: `architecture_config_or_inferred` prefers an AUTHORED map, falls back to
    /// inference when there is none, and treats a MALFORMED authored map as an error to
    /// surface — never as an invitation to guess at one.
    #[test]
    fn req_a4_or_inferred_prefers_authored_and_refuses_to_guess_past_a_malformed_one() {
        let layered = vec![
            ("src/controllers/a.rs".to_string(), String::new()),
            ("src/repositories/b.rs".to_string(), String::new()),
        ];
        let inferred = architecture_config_or_inferred(&layered).expect("inferred");
        assert!(inferred.inferred);

        let mut authored = layered.clone();
        authored.push((ARCHITECTURE_CONFIG_PATH.to_string(), EXAMPLE_TOML.to_string()));
        let real = architecture_config_or_inferred(&authored).expect("authored");
        assert!(!real.inferred, "an authored map must never be marked inferred");

        let mut broken = layered.clone();
        broken.push((ARCHITECTURE_CONFIG_PATH.to_string(), "not ==== toml".to_string()));
        assert!(architecture_config_or_inferred(&broken).is_none());
    }

    /// req_a4: `inferred` is pure provenance and never crosses the TOML boundary — a config
    /// file cannot declare itself inferred, and serialising an inferred map does not leak the
    /// flag into the schema.
    #[test]
    fn req_a4_the_inferred_flag_never_round_trips_through_toml() {
        let parsed = parse_str("version = 1\ninferred = true\n", "test").expect("parses");
        assert!(!parsed.inferred, "an authored TOML must not be able to claim `inferred`");
    }

    const EXAMPLE_TOML: &str = r#"
version = 1

[layers]
handlers     = ["src/routes/**", "src/controllers/**"]
services     = ["src/services/**"]
repositories = ["src/repositories/**"]
domain       = ["src/domain/**"]

[imports]
handlers     = ["services", "domain"]
services     = ["repositories", "domain"]
repositories = ["domain"]
domain       = []

[db]
handles    = ["db", "pool", "conn", "prisma", "supabase"]
allowed_in = ["repositories"]
tx_flow_control_in = ["services"]

[dtos]
domain_types = ["src/domain/**"]
controllers  = ["src/controllers/**"]

[authz]
ui_paths             = ["apps/ui/**"]
forbidden_ui_imports = ["@api/lib/permissions", "hasOrgPermission"]

[helpers]
date_helper     = ["src/lib/dates.ts"]
image_component = ["src/components/AppImage.tsx"]
"#;

    // ── round-trip: parse the design doc's worked example exactly ─────────────

    #[test]
    fn parses_the_full_design_doc_example() {
        let cfg = parse_str(EXAMPLE_TOML, "test").expect("valid config");
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.layers.len(), 4);
        assert_eq!(cfg.layers["handlers"], vec!["src/routes/**", "src/controllers/**"]);
        assert_eq!(cfg.imports["handlers"], vec!["services", "domain"]);
        let db = cfg.db.as_ref().expect("db section present");
        assert_eq!(db.handles, vec!["db", "pool", "conn", "prisma", "supabase"]);
        assert_eq!(db.allowed_in, vec!["repositories"]);
        assert_eq!(db.tx_flow_control_in, vec!["services"]);
        let dtos = cfg.dtos.as_ref().expect("dtos section present");
        assert_eq!(dtos.controllers, vec!["src/controllers/**"]);
        let authz = cfg.authz.as_ref().expect("authz section present");
        assert_eq!(authz.forbidden_ui_imports, vec!["@api/lib/permissions", "hasOrgPermission"]);
        let helpers = cfg.helpers.as_ref().expect("helpers section present");
        assert_eq!(helpers.date_helper, vec!["src/lib/dates.ts"]);
    }

    #[test]
    fn round_trips_through_serialize_and_parse() {
        let cfg = parse_str(EXAMPLE_TOML, "test").expect("valid config");
        let serialized = toml::to_string(&cfg).expect("serializes");
        let reparsed = parse_str(&serialized, "test").expect("re-parses its own output");
        assert_eq!(cfg, reparsed);
    }

    #[test]
    fn minimal_config_with_only_layers_parses() {
        let cfg = parse_str("version = 1\n[layers]\na = [\"src/a/**\"]\n", "test").expect("valid");
        assert_eq!(cfg.layers.len(), 1);
        assert!(cfg.imports.is_empty());
        assert!(cfg.db.is_none());
        assert!(cfg.dtos.is_none());
        assert!(cfg.authz.is_none());
        assert!(cfg.helpers.is_none());
    }

    #[test]
    fn version_only_config_with_no_layers_parses() {
        let cfg = parse_str("version = 1\n", "test").expect("valid");
        assert!(cfg.layers.is_empty());
    }

    // ── loader: absent / malformed on disk ─────────────────────────────────────

    fn tmpdir() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cam-archcfg-test-{}-{}-{}",
            std::process::id(),
            seq,
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_config_on_disk_returns_ok_none() {
        let root = tmpdir();
        assert!(matches!(load_architecture_config(&root), Ok(None)));
    }

    #[test]
    fn valid_config_on_disk_loads() {
        let root = tmpdir();
        let dir = root.join(".camerata");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("architecture.toml"), EXAMPLE_TOML).unwrap();
        let cfg = load_architecture_config(&root).expect("no parse error").expect("present");
        assert_eq!(cfg.version, 1);
    }

    #[test]
    fn malformed_config_on_disk_returns_err_not_panic() {
        let root = tmpdir();
        let dir = root.join(".camerata");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("architecture.toml"), "this is not valid toml }{").unwrap();
        let result = load_architecture_config(&root);
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn config_missing_required_version_field_returns_err() {
        let root = tmpdir();
        let dir = root.join(".camerata");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("architecture.toml"), "[layers]\na = [\"src/**\"]\n").unwrap();
        assert!(load_architecture_config(&root).is_err());
    }

    // ── RepoView-slice loader (scan path) ──────────────────────────────────────

    #[test]
    fn absent_from_file_slice_returns_ok_none() {
        let files = vec![("README.md".to_string(), String::new())];
        assert!(matches!(architecture_config_from_files(&files), Ok(None)));
    }

    #[test]
    fn present_in_file_slice_loads() {
        let files = vec![(".camerata/architecture.toml".to_string(), EXAMPLE_TOML.to_string())];
        let cfg = architecture_config_from_files(&files).expect("no parse error").expect("present");
        assert_eq!(cfg.version, 1);
    }

    #[test]
    fn malformed_in_file_slice_returns_err_not_panic() {
        let files = vec![(".camerata/architecture.toml".to_string(), "}{not toml".to_string())];
        assert!(architecture_config_from_files(&files).is_err());
    }

    // ── layer_for_path ──────────────────────────────────────────────────────────

    #[test]
    fn layer_for_path_matches_exactly_one_layer() {
        let cfg = parse_str(EXAMPLE_TOML, "test").unwrap();
        assert_eq!(cfg.layer_for_path("src/routes/orders.ts"), Ok(Some("handlers")));
        assert_eq!(cfg.layer_for_path("src/services/order_service.ts"), Ok(Some("services")));
    }

    #[test]
    fn layer_for_path_returns_none_for_unmatched_file() {
        let cfg = parse_str(EXAMPLE_TOML, "test").unwrap();
        assert_eq!(cfg.layer_for_path("src/utils/misc.ts"), Ok(None));
    }

    #[test]
    fn layer_for_path_reports_conflict_on_overlapping_globs() {
        let cfg = parse_str(
            "version = 1\n[layers]\na = [\"src/shared/**\"]\nb = [\"src/shared/**\"]\n",
            "test",
        )
        .unwrap();
        let result = cfg.layer_for_path("src/shared/x.ts");
        assert!(result.is_err(), "{result:?}");
        let err = result.unwrap_err();
        assert_eq!(err.path, "src/shared/x.ts");
        assert_eq!(err.layers.len(), 2);
    }

    // ── diagnostics ───────────────────────────────────────────────────────────

    #[test]
    fn diagnostics_empty_for_a_well_formed_config() {
        let cfg = parse_str(EXAMPLE_TOML, "test").unwrap();
        assert!(cfg.diagnostics().is_empty(), "{:#?}", cfg.diagnostics());
    }

    #[test]
    fn diagnostics_flags_undeclared_layer_in_imports() {
        let cfg = parse_str(
            "version = 1\n[layers]\na = [\"src/a/**\"]\n[imports]\na = [\"ghost\"]\n",
            "test",
        )
        .unwrap();
        let ds = cfg.diagnostics();
        assert!(ds.iter().any(|d| d.contains("ghost")), "{ds:#?}");
    }

    #[test]
    fn diagnostics_flags_exact_duplicate_glob_across_layers() {
        let cfg = parse_str(
            "version = 1\n[layers]\na = [\"src/shared/**\"]\nb = [\"src/shared/**\"]\n",
            "test",
        )
        .unwrap();
        let ds = cfg.diagnostics();
        assert!(ds.iter().any(|d| d.contains("src/shared/**")), "{ds:#?}");
    }
}
