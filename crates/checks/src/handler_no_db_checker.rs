//! `HandlerNoDbChecker`: the Pass 4a "Group A" promotion of the existing lexical PROOF
//! checker (`crate::architectural::handler_no_direct_db`) into the `ArchChecker` seam. See
//! `docs/design/2026-07-27_ast-extractor-layer.md` §4 Group A row for `ARCH-HANDLER-NO-DB-1`
//! and `crates/rules/principles/api-layer/arch-handler-no-db-1.toml`.
//!
//! # Why this is an INTERIM promotion, not the production checker
//!
//! `crate::architectural::handler_no_direct_db` is a NAME/lexical heuristic (function names
//! containing `handler`/`controller`/... , DB-handle names containing `db`/`pool`/...) — it
//! does real structural reasoning (function boundaries + brace depth, see that module's own
//! doc comment) but it cannot resolve types or read route attributes, so a handler whose name
//! doesn't match a marker is invisible to it, and a struct field genuinely named `db` that
//! isn't a real database handle would be a false positive. The production version (Pass 4c,
//! `extract::functions` + `extract::method_calls` + route-attribute classification, optionally
//! sharpened by a `.camerata/architecture.toml` layer map) supersedes this. Until then, this
//! wrapper gives the rule SOME deterministic coverage today.
//!
//! # D3: this checker stays LLM-advisory-COEXISTING
//!
//! Per design doc D3 (§0), a config-gated import-graph/layering checker with NO config
//! present should skip entirely and leave its rule id in the LLM prompt (the honest "nothing
//! deterministic ran" story). `ARCH-HANDLER-NO-DB-1`'s name-heuristic fallback is the ONE
//! named exception: it stays armed unconditionally, but reports every finding at
//! `needs-review` grade (the `[needs review: ...]` message-suffix convention,
//! `ui_core::rules::split_needs_review` — same mechanism `ui_dates::UtcDatesChecker` uses) —
//! and, unlike a fully-deterministic checker, this checker's rule id must NOT be subtracted
//! from the LLM-audit prompt (`all_checker_rule_ids`), so an AI reviewer still gets an
//! independent pass at this rule. See [`ArchChecker::advisory_coexisting`] on this checker's
//! `impl` below.

use crate::architectural::{handler_no_direct_db, HANDLER_NO_DIRECT_DB_RULE_ID};
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_MEDIUM};

const RULE_IDS: &[&str] = &[HANDLER_NO_DIRECT_DB_RULE_ID];

const INTEREST_GLOBS: &[&str] = &["**/*.rs"];

pub struct HandlerNoDbChecker;

impl ArchChecker for HandlerNoDbChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        repo.files
            .iter()
            .filter(|(path, _)| crate::arch_checker::matches_any_glob(INTEREST_GLOBS, path))
            .flat_map(|(path, content)| {
                handler_no_direct_db(content).into_iter().map(move |v| ArchViolation {
                    rule_id: v.rule_id,
                    file: path.clone(),
                    line: v.line,
                    object: Some(v.function.clone()),
                    severity: SEVERITY_MEDIUM,
                    message: format!(
                        "{} [needs review: name-heuristic proof checker (function/DB-handle identified by \
                         name, not by type or route attribute) — no .camerata/architecture.toml layer map is \
                         configured to sharpen this; confirm `{}` is really a request handler and the \
                         receiver is really a database handle before treating this as a violation]",
                        v.message, v.function
                    ),
                })
            })
            .collect()
    }

    /// D3: stays LLM-advisory-eligible — see the module doc above. This is the one deliberate
    /// exception in the registry; every other Group-A checker here (and every Supabase
    /// checker) is a hard deterministic verdict and correctly leaves this `false` (the
    /// trait's default).
    fn advisory_coexisting(&self) -> bool {
        true
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

    #[test]
    fn fires_on_a_real_handler_touching_db_directly() {
        let src = r#"
            async fn list_orgs_handler(db: &Db) -> Result<Vec<Org>> {
                let rows = db.query("select * from orgs").await?;
                Ok(rows)
            }
        "#;
        let f = files(vec![("src/routes/orgs.rs", src)]);
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, HANDLER_NO_DIRECT_DB_RULE_ID);
        assert_eq!(vs[0].file, "src/routes/orgs.rs");
        assert_eq!(vs[0].line, 3);
        assert_eq!(vs[0].object.as_deref(), Some("list_orgs_handler"));
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
        assert!(vs[0].message.contains("[needs review"), "{}", vs[0].message);
    }

    #[test]
    fn clean_on_a_handler_that_delegates_to_a_service() {
        let src = r#"
            async fn list_orgs_handler(svc: &OrgService) -> Result<Vec<Org>> {
                Ok(svc.list_orgs().await?)
            }
        "#;
        let f = files(vec![("src/routes/orgs.rs", src)]);
        assert!(HandlerNoDbChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn glob_scoping_ignores_non_rust_files() {
        let src = "function listOrgsHandler(db) { return db.query('select 1'); }";
        let f = files(vec![("src/routes/orgs.ts", src)]);
        assert!(!crate::arch_checker::checker_applies(&HandlerNoDbChecker, &f));
        assert!(HandlerNoDbChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn nested_rust_path_via_double_star_glob_is_scoped_in() {
        let src = r#"
            async fn create_org_handler(db: &Db) -> Result<()> {
                db.execute("insert ...").await?;
                Ok(())
            }
        "#;
        let f = files(vec![("crates/api/src/routes/nested/orgs.rs", src)]);
        assert!(crate::arch_checker::checker_applies(&HandlerNoDbChecker, &f));
        let vs = HandlerNoDbChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }

    #[test]
    fn empty_and_malformed_source_does_not_panic() {
        let f = files(vec![
            ("src/a.rs", ""),
            ("src/b.rs", "fn {{{ not valid rust at all"),
        ]);
        // Must not panic; the lexical scanner degrades to zero findings on garbage input.
        let _ = HandlerNoDbChecker.check(&view(&f));
    }

    #[test]
    fn advisory_coexisting_is_true_for_this_checker() {
        assert!(HandlerNoDbChecker.advisory_coexisting());
    }

    #[test]
    fn rule_id_is_excluded_from_the_llm_exclusion_set() {
        // D3: the whole point of `advisory_coexisting` — verify it end-to-end through the
        // registry-level function, not just the trait method in isolation.
        let ids = crate::arch_checker::all_checker_rule_ids();
        assert!(!ids.contains(HANDLER_NO_DIRECT_DB_RULE_ID), "{ids:?}");
    }
}
