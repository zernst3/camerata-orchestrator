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

use std::collections::BTreeMap;
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

// ─── tests ───────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
