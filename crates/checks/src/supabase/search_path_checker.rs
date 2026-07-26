//! `SupabaseFnSearchPathChecker`: "near-free once RLS ships" per the design memo — reuses
//! the SAME splitter + migration-timeline fold as [`super::rls_checker::SupabaseRlsChecker`],
//! adding only the query over `Timeline::functions`.

use super::timeline::build_timeline;
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_HIGH};

pub const RULE_FUNC_SEARCH_PATH: &str = "SUPABASE-FUNC-SEARCH-PATH-1";

const RULE_IDS: &[&str] = &[RULE_FUNC_SEARCH_PATH];
const INTEREST_GLOBS: &[&str] = &["supabase/migrations/*.sql", "supabase/schemas/*.sql"];

pub struct SupabaseFnSearchPathChecker;

impl ArchChecker for SupabaseFnSearchPathChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let timeline = build_timeline(repo);
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
}
