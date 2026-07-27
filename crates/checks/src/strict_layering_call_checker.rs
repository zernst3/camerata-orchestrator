//! `StrictLayeringCallChecker`: Pass 4c — the CALL-SITE facet of `ARCH-STRICT-LAYERING-1`
//! (the import facet shipped in Pass 4b-2's `import_boundary_checker`). See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group D and
//! `crates/rules/principles/api-layer/arch-strict-layering-1.toml`.
//!
//! # Model
//!
//! Built on [`crate::extract::method_calls`] + the `.camerata/architecture.toml` boundary map
//! (`[layers]` + `[db]`) — a DIFFERENT, narrower model than `ImportBoundaryChecker`'s resolved
//! import graph, per this rule's own `qualifies` text: "the boundary is 'direct database/query
//! CALLS ... are forbidden outside the repository layer' ... a call-site-and-layer predicate,
//! not an import predicate." For every file that classifies into a declared `[layers]` entry
//! NOT in `[db].allowed_in`, every `receiver.method(...)` call site whose receiver's last
//! dotted/`::` segment exactly matches a `[db].handles` marker is a violation — UNLESS the
//! file's layer is in `[db].tx_flow_control_in` AND the call itself is a `.transaction(...)`
//! invocation (the rule's own carve-out: "transaction flow control belongs in services").
//!
//! # Why this exemption is FINER-GRAINED than the import facet's
//!
//! `ImportBoundaryChecker`'s import facet (Pass 4b-2) exempts EVERY import from a
//! `tx_flow_control_in` layer, because an import can't distinguish "this service imports the
//! DB client to wrap a call in `db.transaction(...)`" from "this service imports the DB client
//! and issues ad-hoc queries directly" — that distinction only exists at the CALL site. This
//! checker is precisely that missing precision: a `tx_flow_control_in` service calling
//! `db.transaction(...)` is exempt, but the SAME service calling `db.query(...)` directly
//! (bypassing the repository, not wrapping it in a transaction) is still flagged. See
//! `import_boundary_checker`'s own module doc for the exemption it deliberately left coarse,
//! anticipating this pass.
//!
//! # Two checkers, one rule id
//!
//! `ARCH-STRICT-LAYERING-1` is answered by BOTH this checker (call facet) and
//! `ImportBoundaryChecker` (import facet) — `arch_checker::all_checkers()` runs every
//! registered checker and unions their findings, so two checkers legitimately sharing one rule
//! id is not a conflict (unlike `ARCH-HANDLER-NO-DB-1`, which this pass supersedes down to
//! exactly one owner — that rule's own history is different, see `handler_no_db_checker`'s
//! module doc). The `config_unsatisfied_for` PER-CHECKER gate means `checker_rule_ids_for_repo`
//! excludes `ARCH-STRICT-LAYERING-1` from the LLM-advisory prompt as soon as EITHER checker is
//! satisfied for a repo — the same "config presence, not per-facet" coarsening
//! `ImportBoundaryChecker` already documents for `ARCH-API-DTOS-1`.
//!
//! # False-negative safeguards
//!
//! A file matching no declared layer, or hitting a [`crate::architecture_config::LayerConflict`],
//! is dropped (never judged) — same discipline `ImportBoundaryChecker` and `HandlerNoDbChecker`
//! both already carry. No `[db]` section: silent no-op. No config at all: silent no-op AND
//! `config_unsatisfied_for` stays `true`.

use std::collections::HashSet;

use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};
use crate::architecture_config::architecture_config_from_files;
use crate::extract;
use crate::import_boundary_checker::RULE_STRICT_LAYERING;

const RULE_IDS: &[&str] = &[RULE_STRICT_LAYERING];

const INTEREST_GLOBS: &[&str] = &[
    "**/*.rs",
    "**/*.ts",
    "**/*.tsx",
    "**/*.js",
    "**/*.jsx",
    "**/*.py",
    crate::architecture_config::ARCHITECTURE_CONFIG_PATH,
];

/// The method name this checker recognizes as "transaction flow control" (the rule's own
/// `db.transaction(...)` shape) — an exact, case-insensitive method-name match, not a
/// substring search, so a hypothetical `transactional_query` method never spuriously qualifies.
const TRANSACTION_METHOD: &str = "transaction";

pub struct StrictLayeringCallChecker;

impl ArchChecker for StrictLayeringCallChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let Ok(Some(cfg)) = architecture_config_from_files(repo.files) else {
            return Vec::new();
        };
        let Some(db) = &cfg.db else {
            return Vec::new();
        };
        if db.handles.is_empty() {
            return Vec::new();
        }
        let markers: Vec<String> = db.handles.iter().map(|h| h.to_ascii_lowercase()).collect();
        let allowed: HashSet<&str> = db.allowed_in.iter().map(|s| s.as_str()).collect();
        let tx_layers: HashSet<&str> = db.tx_flow_control_in.iter().map(|s| s.as_str()).collect();

        let mut out = Vec::new();
        for (path, content) in repo.files {
            let Ok(Some(layer)) = cfg.layer_for_path(path) else {
                continue; // unclassified or LayerConflict -> never judged
            };
            if allowed.contains(layer) {
                continue; // this layer legitimately holds a DB handle
            }
            let Some(lang) = extract::lang_for_path(path) else {
                continue;
            };
            let exempt_tx_layer = tx_layers.contains(layer);

            for call in extract::method_calls(lang, content) {
                let Some(marker) = matching_marker(&call.receiver, &markers) else {
                    continue;
                };
                if exempt_tx_layer && call.method.eq_ignore_ascii_case(TRANSACTION_METHOD) {
                    // The rule's own carve-out: THIS specific call orchestrates a transaction,
                    // which is what "transaction flow control belongs in services" permits —
                    // any OTHER db-handle call in the same file is still judged normally.
                    continue;
                }
                out.push(ArchViolation {
                    rule_id: RULE_STRICT_LAYERING.to_string(),
                    file: path.clone(),
                    line: call.line,
                    object: Some(format!("{}.{}", call.receiver, call.method)),
                    severity: SEVERITY_HIGH,
                    message: format!(
                        "\"{path}\" (layer \"{layer}\") calls `{}.{}(...)` directly — a `[db].handles` \
                         marker (\"{marker}\") — direct database-handle calls are only allowed in \
                         layer(s): {} (ARCH-STRICT-LAYERING-1, call-site facet; the import facet is a \
                         separate check){}",
                        call.receiver,
                        call.method,
                        allowed_join(&allowed),
                        if exempt_tx_layer {
                            format!(
                                " — note: \"{layer}\" is exempted for `.{TRANSACTION_METHOD}(...)` calls \
                                 only, per `[db].tx_flow_control_in`; this call is a DIFFERENT method"
                            )
                        } else {
                            String::new()
                        }
                    ),
                });
            }
        }
        out
    }

    /// D3: this facet needs a `[db]` section (handles + at least the notion of `allowed_in`)
    /// to answer anything — absent that, stay LLM-advisory for this repo. Matches
    /// `ImportBoundaryChecker`'s own "config presence for the section this facet needs" gate.
    fn config_unsatisfied_for(&self, repo: &RepoView<'_>) -> bool {
        match architecture_config_from_files(repo.files) {
            Ok(Some(cfg)) => cfg.db.as_ref().is_none_or(|db| db.handles.is_empty()),
            _ => true,
        }
    }
}

fn allowed_join(allowed: &HashSet<&str>) -> String {
    if allowed.is_empty() {
        "(nothing)".to_string()
    } else {
        let mut v: Vec<&str> = allowed.iter().copied().collect();
        v.sort_unstable();
        v.join(", ")
    }
}

/// Whether `receiver`'s LAST `.`/`::`-delimited segment EXACTLY matches one of `markers`
/// (already lowercased) — see `handler_no_db_checker::matching_db_marker`'s identical
/// discipline (an exact-segment match, never a substring search over a longer identifier).
fn matching_marker(receiver: &str, markers: &[String]) -> Option<String> {
    let normalized = receiver.replace("::", ".");
    let last = normalized.rsplit('.').next().unwrap_or(&normalized).to_ascii_lowercase();
    markers.iter().find(|m| m.as_str() == last).cloned()
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
handles = ["db", "prisma"]
allowed_in = ["repositories"]
tx_flow_control_in = ["services"]
"#;

    fn with_config(mut extra: Vec<(&str, &str)>) -> Vec<(String, String)> {
        extra.push((crate::architecture_config::ARCHITECTURE_CONFIG_PATH, DB_CONFIG));
        files(extra)
    }

    // ── fires on a real violation (Rust + TS) ─────────────────────────────────

    #[test]
    fn handler_calling_db_directly_is_flagged_rust() {
        let f = with_config(vec![(
            "src/routes/orders.rs",
            "async fn list(db: &Db) -> Result<()> {\n    db.query(\"select 1\").await\n}\n",
        )]);
        let vs = StrictLayeringCallChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "src/routes/orders.rs");
        assert_eq!(vs[0].line, 2);
        assert_eq!(vs[0].rule_id, RULE_STRICT_LAYERING);
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert!(vs[0].message.contains("handlers"), "{}", vs[0].message);
    }

    #[test]
    fn handler_calling_db_directly_is_flagged_ts() {
        let f = with_config(vec![(
            "src/routes/orders.ts",
            "export function list(db: Db) {\n  return db.query('select 1');\n}\n",
        )]);
        let vs = StrictLayeringCallChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].line, 2);
    }

    // ── clean on compliant repository layer ───────────────────────────────────

    #[test]
    fn repository_calling_db_directly_is_clean() {
        let f = with_config(vec![(
            "src/repositories/orders_repo.rs",
            "async fn fetch(db: &Db) -> Result<()> {\n    db.query(\"select 1\").await\n}\n",
        )]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
    }

    // ── the finer-grained call-site transaction exemption ─────────────────────

    #[test]
    fn service_wrapping_a_call_in_transaction_is_exempt() {
        let f = with_config(vec![(
            "src/services/order_service.rs",
            "async fn place(db: &Db) -> Result<()> {\n    db.transaction(|tx| async { Ok(()) }).await\n}\n",
        )]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn service_calling_db_directly_without_transaction_is_still_flagged() {
        // This is the key improvement over the import facet's coarser exemption
        // (`import_boundary_checker::service_importing_db_client_for_tx_flow_control_is_exempted`
        // exempts the WHOLE import): a service in `tx_flow_control_in` that calls a raw query
        // directly (NOT `.transaction(...)`) must still be flagged at the call site.
        let f = with_config(vec![(
            "src/services/order_service.rs",
            "async fn place(db: &Db) -> Result<()> {\n    db.query(\"select 1\").await\n}\n",
        )]);
        let vs = StrictLayeringCallChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].file, "src/services/order_service.rs");
    }

    // ── D3 / silence conditions ────────────────────────────────────────────────

    #[test]
    fn silent_when_db_section_absent() {
        let cfg = "version = 1\n[layers]\nhandlers = [\"src/routes/**\"]\n[imports]\nhandlers = []\n";
        let f = files(vec![
            (crate::architecture_config::ARCHITECTURE_CONFIG_PATH, cfg),
            ("src/routes/orders.rs", "async fn list(db: &Db) -> Result<()> {\n    db.query(\"x\").await\n}\n"),
        ]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
        assert!(StrictLayeringCallChecker.config_unsatisfied_for(&view(&f)));
    }

    #[test]
    fn silent_and_advisory_when_no_config_at_all() {
        let f = files(vec![(
            "src/routes/orders.rs",
            "async fn list(db: &Db) -> Result<()> {\n    db.query(\"x\").await\n}\n",
        )]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
        assert!(StrictLayeringCallChecker.config_unsatisfied_for(&view(&f)));
    }

    #[test]
    fn config_satisfied_excludes_rule_id_from_llm_advisory_set() {
        let f = with_config(vec![]);
        assert!(!StrictLayeringCallChecker.config_unsatisfied_for(&view(&f)));
    }

    // ── false-negative safeguards ──────────────────────────────────────────────

    #[test]
    fn unclassified_file_is_never_judged() {
        let f = with_config(vec![(
            "src/utils/misc.rs",
            "async fn helper(db: &Db) -> Result<()> {\n    db.query(\"x\").await\n}\n",
        )]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn layer_conflict_is_dropped_not_flagged() {
        let conflicting = r#"
version = 1
[layers]
a = ["src/shared/**"]
b = ["src/shared/**"]
[imports]
a = []
b = []
[db]
handles = ["db"]
allowed_in = []
"#;
        let f = files(vec![
            (crate::architecture_config::ARCHITECTURE_CONFIG_PATH, conflicting),
            ("src/shared/x.rs", "async fn f(db: &Db) -> Result<()> {\n    db.query(\"x\").await\n}\n"),
        ]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn marker_token_does_not_substring_match_a_longer_identifier() {
        let f = with_config(vec![(
            "src/routes/orders.rs",
            "async fn list(database: &Database) -> Result<()> {\n    database.execute(\"x\").await\n}\n",
        )]);
        assert!(StrictLayeringCallChecker.check(&view(&f)).is_empty());
    }

    // ── adversarial: malformed source never panics ────────────────────────────

    #[test]
    fn malformed_source_does_not_panic() {
        let f = with_config(vec![
            ("src/routes/broken.rs", "fn {{{ not valid rust at all"),
            ("src/routes/broken.ts", "import { from 'oops"),
            ("src/routes/broken.py", "def f(:\n    pass"),
        ]);
        let _ = StrictLayeringCallChecker.check(&view(&f));
    }

    #[test]
    fn empty_file_set_does_not_panic() {
        let empty: Vec<(String, String)> = Vec::new();
        assert!(StrictLayeringCallChecker.check(&view(&empty)).is_empty());
    }

    // ── registry wiring ────────────────────────────────────────────────────────

    #[test]
    fn checker_is_registered_and_shares_the_rule_id_with_import_boundary_checker() {
        let owners: usize = crate::arch_checker::all_checkers()
            .iter()
            .filter(|c| c.rule_ids().contains(&RULE_STRICT_LAYERING))
            .count();
        assert_eq!(
            owners, 2,
            "ARCH-STRICT-LAYERING-1 must be answered by exactly two checkers (import facet + call facet)"
        );
    }
}
