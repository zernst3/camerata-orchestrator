//! `ImportBoundaryChecker`: Pass 4b-2 — the first config-gated checker on the AST-extractor
//! layer (`docs/design/2026-07-27_ast-extractor-layer.md` §4 Group C). Builds the intra-repo
//! import graph ONCE ([`crate::extract::resolver::build_import_graph`]) and answers three
//! rule ids over it plus the `.camerata/architecture.toml` boundary map (Pass 4b-1):
//!
//! - `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` — any import edge whose target layer is not in the
//!   source layer's `[imports]` allow-list.
//! - `ARCH-API-DTOS-1` — a `[dtos].controllers` file importing a `[dtos].domain_types` file
//!   (config-gated on the OPTIONAL `[dtos]` section — the rule TOML's own `default = false`
//!   means this facet stays silent for any repo that hasn't opted in, config-present or not).
//! - `ARCH-STRICT-LAYERING-1` (IMPORT FACET ONLY) — a file in a declared layer importing a
//!   DB-client package (a `[db].handles` token match) outside `[db].allowed_in`. The
//!   CALL-SITE facet (`db.query(...)` inside a handler body) is Pass 4c, not this checker.
//!
//! # D3: config-presence-only gate (this pass's simplification)
//!
//! [`ArchChecker::config_unsatisfied_for`] answers a SINGLE bool per checker (the trait's
//! existing shape, Pass 4b-1) — it cannot express "satisfied for rule A, unsatisfied for rule
//! B" within one repo. This checker resolves that by gating on ONE fact: does
//! `.camerata/architecture.toml` exist and parse for this repo? If yes, all three rule ids are
//! excluded from the LLM-advisory prompt for this repo; if no (absent OR malformed — the
//! config module's own contract says treat `Err` like `Ok(None)`), all three stay advisory.
//!
//! This is a KNOWN coarsening for `ARCH-API-DTOS-1` specifically: a repo that configures
//! `[layers]`/`[imports]` but never opts into `[dtos]` will have `ARCH-API-DTOS-1` excluded
//! from the LLM prompt even though this checker emits zero verdicts for it (no `[dtos]`
//! section to check against — see `check_api_dtos` below, which silently no-ops when
//! `cfg.dtos` is `None`). Flagged in the "Pass 4b-2 landed" design-doc note as a refinement
//! candidate (per-rule-id `config_unsatisfied_for` granularity) for a later pass — not fixed
//! here, per this pass's explicit framing as a config presence/absence gate.
//!
//! # Why `[db].tx_flow_control_in` is ALSO exempted from the import facet
//!
//! The rule TOML's own directive carves out "transaction flow control belongs in services" —
//! a service wrapping a repository call in `db.transaction(...)` legitimately needs to IMPORT
//! the DB client to do that. The design table names this exemption for the Group-D CALL
//! facet; a bare import-facet check with no matching exemption would flag that same
//! legitimate service's import even though its call-site is fine, defeating the option's
//! whole purpose. This checker therefore unions `[db].allowed_in` and
//! `[db].tx_flow_control_in` for its own (import-only) verdict — a deliberate, documented
//! widening beyond the design table's literal wording, revisited once Pass 4c's call-site
//! facet exists and the two can be cross-checked against real fixtures.
//!
//! # False-negative discipline
//!
//! Every lookup that can be ambiguous ([`ArchitectureConfig::layer_for_path`]'s
//! `Err(LayerConflict)`, an unresolved import, a file matching no declared layer) is DROPPED,
//! never flagged — the same contract `resolver` and `layer_for_path` already carry, extended
//! here rather than re-litigated.

use std::collections::HashSet;

use crate::arch_checker::{matches_any_glob, ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};
use crate::architecture_config::{
    architecture_config_from_files, ArchitectureConfig, DbConfig, ARCHITECTURE_CONFIG_PATH,
};
use crate::extract;
use crate::extract::resolver::{self, ResolvedImport};

pub const RULE_NO_CROSS_BOUNDARY_IMPORTS: &str = "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1";
pub const RULE_API_DTOS: &str = "ARCH-API-DTOS-1";
pub const RULE_STRICT_LAYERING: &str = "ARCH-STRICT-LAYERING-1";

const RULE_IDS: &[&str] = &[RULE_NO_CROSS_BOUNDARY_IMPORTS, RULE_API_DTOS, RULE_STRICT_LAYERING];

/// The source files the v1 extractor layer supports, plus the config files this checker
/// needs to READ from disk on the Layer-2 path (`NativeArchCheckRunner::collect_interest_files`
/// only reads files matching the union of every ARMED checker's `interest_globs` — so the
/// architecture.toml and any tsconfig.json path aliasing the resolver needs must be declared
/// here too, or the Layer-2 gate would silently see this checker as "unconfigured" even when
/// a real config sits on disk).
const INTEREST_GLOBS: &[&str] = &[
    "**/*.rs",
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.py",
    ARCHITECTURE_CONFIG_PATH,
    "tsconfig.json",
    "**/tsconfig.json",
];

pub struct ImportBoundaryChecker;

impl ArchChecker for ImportBoundaryChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let cfg = match architecture_config_from_files(repo.files) {
            Ok(Some(cfg)) => cfg,
            // Absent OR malformed: abstain entirely (D3) — zero deterministic findings, and
            // `config_unsatisfied_for` (below) keeps every rule id LLM-advisory-eligible.
            _ => return Vec::new(),
        };

        let mut out = Vec::new();
        let graph = resolver::build_import_graph(repo.files);

        for edge in &graph {
            check_cross_boundary(&cfg, edge, &mut out);
            check_api_dtos(&cfg, edge, &mut out);
        }

        check_strict_layering_import_facet(&cfg, repo.files, &mut out);

        out
    }

    /// D3: gate on config PRESENCE (parses + exists) only — see the module doc's "config
    /// presence-only gate" section for why this checker doesn't attempt per-rule-id
    /// granularity here.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        architecture_config_from_files(repo.files).ok().flatten().is_none()
    }
}

/// `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1`: `edge`'s source and target must both classify into a
/// declared layer (an unclassified endpoint, or a [`crate::architecture_config::LayerConflict`]
/// on either side, is dropped — never judged), and the target layer must be in the source
/// layer's `[imports]` allow-list. A source layer with NO `[imports]` entry at all behaves
/// exactly like an explicit empty list (`domain = []` in the design's worked example) — the
/// schema's own semantics are "any edge not explicitly listed is forbidden," so a missing key
/// denies everything, including same-layer imports, unless the project explicitly lists
/// itself as its own allowed target.
fn check_cross_boundary(cfg: &ArchitectureConfig, edge: &ResolvedImport, out: &mut Vec<ArchViolation>) {
    let (Ok(Some(from_layer)), Ok(Some(to_layer))) =
        (cfg.layer_for_path(&edge.from_file), cfg.layer_for_path(&edge.to_file))
    else {
        return;
    };
    let allowed: &[String] = cfg.imports.get(from_layer).map(|v| v.as_slice()).unwrap_or(&[]);
    if allowed.iter().any(|l| l.as_str() == to_layer) {
        return;
    }
    let allowed_desc = if allowed.is_empty() { "(nothing)".to_string() } else { allowed.join(", ") };
    out.push(ArchViolation {
        rule_id: RULE_NO_CROSS_BOUNDARY_IMPORTS.to_string(),
        file: edge.from_file.clone(),
        line: edge.line,
        object: Some(edge.specifier.clone()),
        severity: SEVERITY_HIGH,
        message: format!(
            "\"{}\" (layer \"{from_layer}\") imports \"{}\" (layer \"{to_layer}\") via `{}` — \
             \"{from_layer}\" may import: {allowed_desc} (ARCH-NO-CROSS-BOUNDARY-IMPORTS-1)",
            edge.from_file, edge.to_file, edge.specifier
        ),
    });
}

/// `ARCH-API-DTOS-1`: `edge`'s source file matches `[dtos].controllers` and its target matches
/// `[dtos].domain_types`. Silently a no-op when `[dtos]` isn't configured — this is the
/// `default = false` rule; a repo that hasn't opted into a DTO boundary at all gets zero
/// findings for it, exactly like the design table specifies.
fn check_api_dtos(cfg: &ArchitectureConfig, edge: &ResolvedImport, out: &mut Vec<ArchViolation>) {
    let Some(dtos) = &cfg.dtos else {
        return;
    };
    let from_is_controller = matches_any_glob(&glob_refs(&dtos.controllers), &edge.from_file);
    let to_is_domain = matches_any_glob(&glob_refs(&dtos.domain_types), &edge.to_file);
    if !(from_is_controller && to_is_domain) {
        return;
    }
    out.push(ArchViolation {
        rule_id: RULE_API_DTOS.to_string(),
        file: edge.from_file.clone(),
        line: edge.line,
        object: Some(edge.specifier.clone()),
        severity: SEVERITY_HIGH,
        message: format!(
            "\"{}\" (a controller, per [dtos].controllers) imports \"{}\" (a domain type, per \
             [dtos].domain_types) via `{}` — controllers must return dedicated DTOs mapped to and \
             from domain types, not the domain types directly (ARCH-API-DTOS-1)",
            edge.from_file, edge.to_file, edge.specifier
        ),
    });
}

/// `ARCH-STRICT-LAYERING-1` (import facet only): a raw (unresolved) per-file import scan for a
/// `[db].handles` marker — a real DB-client package is typically EXTERNAL (an npm package, a
/// crate), so this deliberately does NOT go through the resolved intra-repo graph the other
/// two facets use. A file whose declared layer is in `[db].allowed_in` OR
/// `[db].tx_flow_control_in` (see the module doc's exemption note) is skipped; every other
/// declared-layer file is scanned for a matching import.
fn check_strict_layering_import_facet(
    cfg: &ArchitectureConfig,
    files: &[(String, String)],
    out: &mut Vec<ArchViolation>,
) {
    let Some(db) = &cfg.db else {
        return;
    };
    let mut exempt: HashSet<&str> = db.allowed_in.iter().map(|s| s.as_str()).collect();
    exempt.extend(db.tx_flow_control_in.iter().map(|s| s.as_str()));

    let markers: Vec<String> = db.handles.iter().map(|h| h.to_ascii_lowercase()).collect();
    if markers.is_empty() {
        return;
    }

    for (path, content) in files {
        let Ok(Some(layer)) = cfg.layer_for_path(path) else {
            continue;
        };
        if exempt.contains(layer) {
            continue;
        }
        let Some(lang) = extract::lang_for_path(path) else {
            continue;
        };
        for imp in extract::imports(lang, content) {
            let Some(marker) = matching_db_marker(&imp.specifier, &markers) else {
                continue;
            };
            out.push(ArchViolation {
                rule_id: RULE_STRICT_LAYERING.to_string(),
                file: path.clone(),
                line: imp.line,
                object: Some(imp.specifier.clone()),
                severity: SEVERITY_HIGH,
                message: format!(
                    "\"{path}\" (layer \"{layer}\") imports `{}` — a DB-client package (matches \
                     [db].handles marker \"{marker}\") — direct DB-client imports are only allowed \
                     in layer(s): {} (ARCH-STRICT-LAYERING-1, import facet; the call-site facet is \
                     a separate check)",
                    imp.specifier,
                    allowed_join(db)
                ),
            });
        }
    }
}

fn allowed_join(db: &DbConfig) -> String {
    let mut all: Vec<&str> = db.allowed_in.iter().map(|s| s.as_str()).collect();
    all.extend(db.tx_flow_control_in.iter().map(|s| s.as_str()));
    if all.is_empty() {
        "(nothing)".to_string()
    } else {
        all.join(", ")
    }
}

/// Tokenize `specifier` on any non-alphanumeric/underscore boundary and return the first
/// declared `[db].handles` marker (already lowercased) found as an EXACT token match —
/// deliberately not a raw substring match, so a short marker like `"db"` can never spuriously
/// match an unrelated specifier such as `"database-types"` or `"adobe-something"`. A real
/// DB-client package name is expected to appear as one of its own path/scope segments
/// (`@prisma/client` -> `"prisma"`; `supabase-js` -> `"supabase"`; a bespoke internal
/// `sea_orm` wrapper module -> `"sea_orm"` as a single underscore-preserving token).
fn matching_db_marker(specifier: &str, markers: &[String]) -> Option<String> {
    let lower = specifier.to_ascii_lowercase();
    let tokens: Vec<&str> =
        lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|s| !s.is_empty()).collect();
    markers.iter().find(|m| tokens.contains(&m.as_str())).cloned()
}

fn glob_refs(globs: &[String]) -> Vec<&str> {
    globs.iter().map(|s| s.as_str()).collect()
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
domain       = ["src/domain/**"]

[imports]
handlers     = ["services", "domain"]
services     = ["repositories", "domain"]
repositories = ["domain"]
domain       = []
"#;

    fn with_config(mut extra: Vec<(&str, &str)>) -> Vec<(String, String)> {
        extra.push((ARCHITECTURE_CONFIG_PATH, LAYERED_CONFIG));
        files(extra)
    }

    // ── ARCH-NO-CROSS-BOUNDARY-IMPORTS-1: Rust fixture ─────────────────────────

    #[test]
    fn rust_handler_importing_repository_directly_is_flagged_with_file_line_and_layers() {
        let f = with_config(vec![
            (
                "src/routes/orders.rs",
                "use crate::repositories::orders_repo::OrdersRepo;\n\npub fn f(_r: &OrdersRepo) {}\n",
            ),
            ("src/repositories/orders_repo.rs", "pub struct OrdersRepo;\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        let hits: Vec<_> = vs.iter().filter(|v| v.rule_id == RULE_NO_CROSS_BOUNDARY_IMPORTS).collect();
        assert_eq!(hits.len(), 1, "{vs:#?}");
        assert_eq!(hits[0].file, "src/routes/orders.rs");
        assert_eq!(hits[0].line, 1);
        assert!(hits[0].message.contains("handlers"), "{}", hits[0].message);
        assert!(hits[0].message.contains("repositories"), "{}", hits[0].message);
        assert_eq!(hits[0].severity, SEVERITY_HIGH);
    }

    #[test]
    fn rust_compliant_layered_repo_is_clean() {
        let f = with_config(vec![
            (
                "src/services/order_service.rs",
                "use crate::repositories::orders_repo::OrdersRepo;\nuse crate::domain::order::Order;\n",
            ),
            (
                "src/repositories/orders_repo.rs",
                "use crate::domain::order::Order;\npub struct OrdersRepo;\n",
            ),
            ("src/domain/order.rs", "pub struct Order;\n"),
        ]);
        assert!(ImportBoundaryChecker.check(&view(&f)).is_empty());
    }

    // ── ARCH-NO-CROSS-BOUNDARY-IMPORTS-1: TS fixture (multi-language proof) ────

    #[test]
    fn ts_handler_importing_repository_directly_is_flagged() {
        let f = with_config(vec![
            (
                "src/routes/orders.ts",
                "import { OrdersRepo } from '../repositories/orders_repo';\nexport function f(r: OrdersRepo) {}\n",
            ),
            ("src/repositories/orders_repo.ts", "export class OrdersRepo {}\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        let hits: Vec<_> = vs.iter().filter(|v| v.rule_id == RULE_NO_CROSS_BOUNDARY_IMPORTS).collect();
        assert_eq!(hits.len(), 1, "{vs:#?}");
        assert_eq!(hits[0].file, "src/routes/orders.ts");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn ts_compliant_layered_repo_is_clean() {
        let f = with_config(vec![
            (
                "src/services/order_service.ts",
                "import { OrdersRepo } from '../repositories/orders_repo';\nimport { Order } from '../domain/order';\n",
            ),
            (
                "src/repositories/orders_repo.ts",
                "import { Order } from '../domain/order';\nexport class OrdersRepo {}\n",
            ),
            ("src/domain/order.ts", "export class Order {}\n"),
        ]);
        assert!(ImportBoundaryChecker.check(&view(&f)).is_empty());
    }

    // ── D3: no-config repo abstains entirely ───────────────────────────────────

    #[test]
    fn no_config_repo_emits_zero_deterministic_findings() {
        let f = files(vec![(
            "src/routes/orders.ts",
            "import { OrdersRepo } from '../repositories/orders_repo';\n",
        )]);
        assert!(ImportBoundaryChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn no_config_repo_stays_config_unsatisfied() {
        let f = files(vec![("src/routes/orders.ts", "")]);
        assert!(ImportBoundaryChecker.config_unsatisfied_for(&view(&f)));
    }

    #[test]
    fn no_config_repo_rule_ids_stay_in_llm_advisory_set_per_d3() {
        let f = files(vec![("README.md", "")]);
        let repo = view(&f);
        let per_repo = crate::arch_checker::checker_rule_ids_for_repo(&repo);
        for id in RULE_IDS {
            assert!(!per_repo.contains(id), "{id} must stay LLM-advisory without config: {per_repo:?}");
        }
    }

    #[test]
    fn config_present_repo_rule_ids_are_excluded_from_llm_advisory_set() {
        let f = with_config(vec![]);
        assert!(!ImportBoundaryChecker.config_unsatisfied_for(&view(&f)));
        let repo = view(&f);
        let per_repo = crate::arch_checker::checker_rule_ids_for_repo(&repo);
        for id in RULE_IDS {
            assert!(per_repo.contains(id), "{id} must be excluded from the LLM prompt once configured: {per_repo:?}");
        }
    }

    #[test]
    fn malformed_config_is_treated_like_absent_config_not_a_panic() {
        let f = files(vec![
            (ARCHITECTURE_CONFIG_PATH, "}{ not valid toml at all"),
            ("src/routes/orders.ts", "import { OrdersRepo } from '../repositories/orders_repo';\n"),
        ]);
        let repo = view(&f);
        assert!(ImportBoundaryChecker.check(&repo).is_empty());
        assert!(ImportBoundaryChecker.config_unsatisfied_for(&repo));
    }

    // ── false-negative safeguards: unresolved / unclassified never false-flag ──

    #[test]
    fn external_package_import_produces_no_false_positive() {
        let f = with_config(vec![("src/routes/orders.ts", "import React from 'react';\n")]);
        assert!(ImportBoundaryChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn file_outside_every_declared_layer_produces_no_false_positive() {
        // "src/utils/misc.ts" matches no [layers] glob at all — its import is real and
        // resolves, but neither endpoint (nor the source) is a declared layer, so it must
        // never be judged.
        let f = with_config(vec![
            ("src/utils/misc.ts", "import { OrdersRepo } from '../repositories/orders_repo';\n"),
            ("src/repositories/orders_repo.ts", "export class OrdersRepo {}\n"),
        ]);
        assert!(ImportBoundaryChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn layer_conflict_from_overlapping_globs_is_dropped_not_flagged() {
        let conflicting_cfg = r#"
version = 1
[layers]
handlers = ["src/shared/**"]
services = ["src/shared/**"]
[imports]
handlers = []
services = []
"#;
        let f = files(vec![
            (ARCHITECTURE_CONFIG_PATH, conflicting_cfg),
            ("src/shared/orders.ts", "import { x } from './other';\n"),
            ("src/shared/other.ts", "export const x = 1;\n"),
        ]);
        // Both endpoints hit a LayerConflict (two layers match the same glob) — must be
        // dropped, never flagged, per the false-negative discipline.
        assert!(ImportBoundaryChecker.check(&view(&f)).is_empty());
    }

    // ── ARCH-API-DTOS-1 ─────────────────────────────────────────────────────────

    const DTOS_CONFIG: &str = r#"
version = 1
[layers]
controllers = ["src/controllers/**"]
domain      = ["src/domain/**"]
[imports]
controllers = ["domain"]
domain      = []
[dtos]
domain_types = ["src/domain/**"]
controllers  = ["src/controllers/**"]
"#;

    #[test]
    fn controller_importing_domain_type_directly_is_flagged_as_api_dtos() {
        let f = files(vec![
            (ARCHITECTURE_CONFIG_PATH, DTOS_CONFIG),
            ("src/controllers/orders.ts", "import { Order } from '../domain/order';\n"),
            ("src/domain/order.ts", "export class Order {}\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        let hits: Vec<_> = vs.iter().filter(|v| v.rule_id == RULE_API_DTOS).collect();
        assert_eq!(hits.len(), 1, "{vs:#?}");
        assert_eq!(hits[0].file, "src/controllers/orders.ts");
    }

    #[test]
    fn api_dtos_facet_is_silent_when_dtos_section_absent() {
        // [layers]/[imports] present (so cross-boundary CAN fire), but no [dtos] section —
        // ARCH-API-DTOS-1 must emit nothing (the default=false, opt-in rule).
        let f = with_config(vec![
            (
                "src/services/order_service.ts",
                "import { OrdersRepo } from '../repositories/orders_repo';\n",
            ),
            ("src/repositories/orders_repo.ts", "export class OrdersRepo {}\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        assert!(vs.iter().all(|v| v.rule_id != RULE_API_DTOS), "{vs:#?}");
    }

    // ── ARCH-STRICT-LAYERING-1 (import facet) ───────────────────────────────────

    const DB_CONFIG: &str = r#"
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
handles = ["prisma", "supabase", "sea_orm"]
allowed_in = ["repositories"]
tx_flow_control_in = ["services"]
"#;

    #[test]
    fn handler_importing_db_client_package_outside_allowed_layers_is_flagged() {
        let f = files(vec![
            (ARCHITECTURE_CONFIG_PATH, DB_CONFIG),
            ("src/routes/orders.ts", "import { PrismaClient } from '@prisma/client';\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        let hits: Vec<_> = vs.iter().filter(|v| v.rule_id == RULE_STRICT_LAYERING).collect();
        assert_eq!(hits.len(), 1, "{vs:#?}");
        assert_eq!(hits[0].file, "src/routes/orders.ts");
        assert!(hits[0].message.contains("prisma"), "{}", hits[0].message);
    }

    #[test]
    fn repository_importing_db_client_package_in_allowed_in_is_clean() {
        let f = files(vec![
            (ARCHITECTURE_CONFIG_PATH, DB_CONFIG),
            ("src/repositories/orders_repo.ts", "import { PrismaClient } from '@prisma/client';\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        assert!(vs.iter().all(|v| v.rule_id != RULE_STRICT_LAYERING), "{vs:#?}");
    }

    #[test]
    fn service_importing_db_client_for_tx_flow_control_is_exempted() {
        // [db].tx_flow_control_in = ["services"] — a service importing the DB client to wrap
        // a repository call in a transaction is the rule's own documented exception; this
        // checker's import facet must not flag it (see the module doc's exemption note).
        let f = files(vec![
            (ARCHITECTURE_CONFIG_PATH, DB_CONFIG),
            ("src/services/order_service.ts", "import { PrismaClient } from '@prisma/client';\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        assert!(vs.iter().all(|v| v.rule_id != RULE_STRICT_LAYERING), "{vs:#?}");
    }

    #[test]
    fn strict_layering_facet_is_silent_when_db_section_absent() {
        let f = with_config(vec![(
            "src/routes/orders.ts",
            "import { PrismaClient } from '@prisma/client';\n",
        )]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        assert!(vs.iter().all(|v| v.rule_id != RULE_STRICT_LAYERING), "{vs:#?}");
    }

    #[test]
    fn db_marker_token_match_does_not_substring_match_an_unrelated_specifier() {
        // "db" as a marker must NOT match a specifier like "database-types" via substring —
        // only an exact token match counts.
        let f = files(vec![
            (
                ARCHITECTURE_CONFIG_PATH,
                "version = 1\n[layers]\nhandlers = [\"src/routes/**\"]\n[imports]\nhandlers = []\n\
                 [db]\nhandles = [\"db\"]\nallowed_in = []\n",
            ),
            ("src/routes/orders.ts", "import { Types } from 'database-types';\n"),
        ]);
        let vs = ImportBoundaryChecker.check(&view(&f));
        assert!(vs.iter().all(|v| v.rule_id != RULE_STRICT_LAYERING), "{vs:#?}");
    }

    // ── adversarial: malformed source does not panic ───────────────────────────

    #[test]
    fn malformed_source_files_do_not_panic() {
        let f = with_config(vec![
            ("src/routes/broken.rs", "fn {{{ not valid rust at all"),
            ("src/routes/broken.ts", "import { from 'oops"),
            ("src/routes/broken.py", "def f(:\n    pass"),
        ]);
        // Must not panic; the extractors degrade to fewer/zero results on garbage input.
        let _ = ImportBoundaryChecker.check(&view(&f));
    }

    #[test]
    fn empty_file_set_does_not_panic() {
        let empty: Vec<(String, String)> = Vec::new();
        assert!(ImportBoundaryChecker.check(&view(&empty)).is_empty());
    }

    // ── registry wiring ──────────────────────────────────────────────────────────

    #[test]
    fn checker_is_registered_and_answers_its_three_rule_ids() {
        let ids: HashSet<&str> =
            crate::arch_checker::all_checkers().iter().flat_map(|c| c.rule_ids().iter().copied()).collect();
        for id in RULE_IDS {
            assert!(ids.contains(id), "{id} missing from the registry: {ids:?}");
        }
    }

    #[test]
    fn checker_interest_globs_scope_out_unrelated_files() {
        let f = files(vec![("README.md", "")]);
        assert!(!crate::arch_checker::checker_applies(&ImportBoundaryChecker, &f));
    }
}
