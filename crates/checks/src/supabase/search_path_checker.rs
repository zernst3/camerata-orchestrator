//! `SupabaseFnSearchPathChecker`: "near-free once RLS ships" per the design memo — reuses
//! the SAME splitter + migration-timeline fold as [`super::rls_checker::SupabaseRlsChecker`],
//! adding only the query over `Timeline::functions`.
//!
//! Despite the module's name (kept for its shared splitter/timeline heritage and its
//! corpus-folder home, `supabase/database-functions/`), the vulnerability this answers —
//! `SECURITY DEFINER` without a pinned `search_path` — is GENERAL Postgres, not
//! Supabase-specific. A hand-rolled `db/migrations/*.sql` layout with no Supabase project at
//! all is just as exploitable, so [`INTEREST_GLOBS`] intentionally covers `.sql` anywhere in
//! the repo (see [`build_timeline_from_globs`](super::timeline::build_timeline_from_globs))
//! rather than only Supabase's fixed folder convention — see
//! `docs/plans/2026-09-30_cycle2-queue-hardening.md` §W1.

use super::timeline::build_timeline_from_globs;
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};

pub const RULE_FUNC_SEARCH_PATH: &str = "SUPABASE-FUNC-SEARCH-PATH-1";

const RULE_IDS: &[&str] = &[RULE_FUNC_SEARCH_PATH];
/// `.sql` anywhere in the repo — NOT scoped to `supabase/` (see the module doc). A single
/// glob is enough: [`build_timeline_from_globs`] folds every match in filename order, and a
/// real Supabase repo's `supabase/schemas/*.sql` still naturally sorts after its
/// `supabase/migrations/*.sql` (`'m' < 's'`), so the "declarative snapshot overrides
/// migration history" shortcut keeps working without a second glob pass.
const INTEREST_GLOBS: &[&str] = &["**/*.sql"];

pub struct SupabaseFnSearchPathChecker;

impl ArchChecker for SupabaseFnSearchPathChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let timeline = build_timeline_from_globs(repo, INTEREST_GLOBS, &[]);
        timeline
            .functions
            .values()
            .filter(|f| f.security_definer && !f.has_search_path)
            .map(|f| {
                let name = if f.schema == "public" {
                    format!("`{}`", f.name)
                } else {
                    format!("`{}.{}`", f.schema, f.name)
                };
                ArchViolation {
                    rule_id: RULE_FUNC_SEARCH_PATH.to_string(),
                    file: f.established_at.file.clone(),
                    line: f.established_at.line,
                    object: Some(format!("{}.{}", f.schema, f.name)),
                    severity: SEVERITY_HIGH,
                    message: format!(
                        "One of your database functions ({name}) runs with elevated power (SECURITY DEFINER) but \
                         resolves object names loosely — a known Postgres attack lets someone swap in their own \
                         object underneath it by manipulating the search_path. Defined at {}:{} with no `SET \
                         search_path` clause. Prefer SECURITY INVOKER unless elevated privileges are genuinely \
                         required; when DEFINER is required, add `SET search_path = <trusted_schema>, pg_temp`. \
                         This reflects the migration history in this repository only — confirm the deployed \
                         function definition matches before treating this as settled.",
                        f.established_at.file, f.established_at.line
                    ),
                }
            })
            .collect()
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
    fn definer_without_search_path_fires() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_fn.sql",
            "create function public.promote_admin() returns void security definer as $$ begin end; $$ language plpgsql;",
        )]);
        let vs = SupabaseFnSearchPathChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_FUNC_SEARCH_PATH);
        assert_eq!(vs[0].severity, SEVERITY_HIGH);
        assert!(vs[0].message.contains("promote_admin"));
    }

    #[test]
    fn definer_with_search_path_is_clean() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_fn.sql",
            "create function public.promote_admin() returns void security definer set search_path = public, \
             pg_temp as $$ begin end; $$ language plpgsql;",
        )]);
        let vs = SupabaseFnSearchPathChecker.check(&view(&f));
        assert!(vs.is_empty(), "{vs:#?}");
    }

    #[test]
    fn invoker_function_is_never_flagged() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_fn.sql",
            "create function public.helper() returns void as $$ begin end; $$ language plpgsql;",
        )]);
        let vs = SupabaseFnSearchPathChecker.check(&view(&f));
        assert!(vs.is_empty(), "{vs:#?}");
    }

    #[test]
    fn no_supabase_files_never_applies() {
        let f = files(vec![("README.md", "hello")]);
        assert!(!crate::arch_checker::checker_applies(&SupabaseFnSearchPathChecker, &f));
    }

    // ── W1 breadth hardening: general Postgres, not just Supabase ──────────────────────
    // See docs/plans/2026-09-30_cycle2-queue-hardening.md §W1 — the vulnerability (a
    // SECURITY DEFINER function with no pinned search_path) is general Postgres, so the
    // checker must inspect `.sql` wherever it lives, not only `supabase/migrations/*.sql`.

    #[test]
    fn plain_postgres_definer_without_search_path_fires_outside_supabase_layout() {
        // A repo with NO `supabase/` layout at all (a plain `db/migrations/` convention) must
        // still be inspected and fire exactly one finding — the general-Postgres regression
        // guard for the arming/glob gap.
        let f = files(vec![(
            "db/migrations/0001_fn.sql",
            "create function public.promote_admin() returns void security definer as $$ begin end; $$ language plpgsql;",
        )]);
        let vs = SupabaseFnSearchPathChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_FUNC_SEARCH_PATH);
        assert_eq!(vs[0].file, "db/migrations/0001_fn.sql");
    }

    #[test]
    fn safe_twin_sibling_functions_never_leak_search_path_state() {
        // Per-function attribution proof: an invoker-rights function, a definer function WITH
        // search_path pinned, and a definer function WITHOUT it, all in the SAME file — exactly
        // the guilty one (and only it) must fire. A sibling's `SET search_path` clause must
        // never be attributed to a different function.
        let f = files(vec![(
            "db/migrations/0001_fns.sql",
            "create function public.helper() returns void as $$ begin end; $$ language plpgsql;\n\
             create function public.rotate_share_link() returns void security definer set search_path = public, pg_temp \
             as $$ begin end; $$ language plpgsql;\n\
             create function public.grant_temporary_access() returns void security definer as $$ begin end; $$ language plpgsql;",
        )]);
        let vs = SupabaseFnSearchPathChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "expected exactly the one guilty function: {vs:#?}");
        assert_eq!(
            vs[0].object.as_deref(),
            Some("public.grant_temporary_access"),
            "the wrong function fired — sibling search_path state leaked: {vs:#?}"
        );
    }

    #[test]
    fn nested_non_supabase_sql_path_is_inspected() {
        // Breadth guard: the checker must apply to SQL wherever it lives, not just a
        // Supabase-shaped path — a deeply nested, generic layout must still count.
        let f = files(vec![(
            "services/billing/sql/migrations/v3/0007_add_fn.sql",
            "create function public.f() returns void security definer as $$ begin end; $$ language plpgsql;",
        )]);
        assert!(crate::arch_checker::checker_applies(&SupabaseFnSearchPathChecker, &f));
        let vs = SupabaseFnSearchPathChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
    }
}
