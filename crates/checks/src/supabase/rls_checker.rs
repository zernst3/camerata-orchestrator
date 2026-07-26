//! `SupabaseRlsChecker`: the flagship checker (design memo §3) — replays the
//! `supabase/migrations/*.sql` (+ `supabase/schemas/*.sql`) timeline and answers the three
//! RLS rule ids over the FINAL state per table, scoped by `supabase/config.toml`'s exposed
//! schemas.

use std::collections::BTreeSet;

use super::config::parse_exposed_schemas;
use super::timeline::{build_timeline, TableState};
use crate::arch_checker::{ArchChecker, ArchViolation, RepoView, SEVERITY_CRITICAL, SEVERITY_MEDIUM};

pub const RULE_RLS_ENABLED: &str = "SUPABASE-RLS-ENABLED-1";
pub const RULE_RLS_NO_POLICY: &str = "SUPABASE-RLS-NO-POLICY-1";
pub const RULE_RLS_POLICY_DISABLED: &str = "SUPABASE-RLS-POLICY-DISABLED-1";

const RULE_IDS: &[&str] = &[RULE_RLS_ENABLED, RULE_RLS_NO_POLICY, RULE_RLS_POLICY_DISABLED];

const INTEREST_GLOBS: &[&str] = &[
    "supabase/migrations/*.sql",
    "supabase/schemas/*.sql",
    "supabase/config.toml",
];

/// The honesty caveat every RLS finding carries (design memo §3 step 7 / spec §4): a repo
/// scan can only prove what the migration HISTORY says, never what the live database
/// currently enforces (dashboard-applied changes never land in a migration).
const HONESTY_CAVEAT: &str = "This reflects the migration history in this repository only — it is not a live-database check. \
Changes made through the Supabase dashboard never land in a migration, so confirm against the \
production database before treating this as settled.";

pub struct SupabaseRlsChecker;

impl ArchChecker for SupabaseRlsChecker {
    fn rule_ids(&self) -> &'static [&'static str] {
        RULE_IDS
    }

    fn interest_globs(&self) -> &'static [&'static str] {
        INTEREST_GLOBS
    }

    fn check(&self, repo: &RepoView<'_>) -> Vec<ArchViolation> {
        let exposed = exposed_schemas(repo);
        let timeline = build_timeline(repo);

        let mut violations = Vec::new();
        for state in timeline.tables.values() {
            violations.extend(check_table(state, &exposed));
        }
        violations
    }
}

fn exposed_schemas(repo: &RepoView<'_>) -> BTreeSet<String> {
    repo.files
        .iter()
        .find(|(path, _)| path == "supabase/config.toml")
        .map(|(_, content)| parse_exposed_schemas(content))
        .unwrap_or_else(|| ["public".to_string()].into_iter().collect())
}

/// The buyer-friendly display name for a table: bare name in the (overwhelmingly common)
/// `public` schema, schema-qualified otherwise — matches the corpus rules' own framing
/// ("your `profiles` table") while staying unambiguous for non-default schemas.
fn display_name(schema: &str, table: &str) -> String {
    if schema == "public" {
        format!("`{table}`")
    } else {
        format!("`{schema}.{table}`")
    }
}

fn check_table(state: &TableState, exposed_schemas: &BTreeSet<String>) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    let name = display_name(&state.schema, &state.table);
    let (est_file, est_line) = state
        .rls_established_at
        .as_ref()
        .map(|l| (l.file.clone(), l.line))
        .unwrap_or_default();

    if !state.rls_enabled {
        let exposed = exposed_schemas.contains(&state.schema);
        if exposed {
            out.push(ArchViolation {
                rule_id: RULE_RLS_ENABLED.to_string(),
                file: est_file.clone(),
                line: est_line,
                object: Some(format!("{}.{}", state.schema, state.table)),
                severity: SEVERITY_CRITICAL,
                message: format!(
                    "Your {name} table has no Row Level Security. Anyone holding your public API key — which \
                     ships in your frontend — can read and write every row. No evidence of RLS being enabled for \
                     {name} was found anywhere in the migration history (last relevant statement: \
                     {est_file}:{est_line}). {HONESTY_CAVEAT}"
                ),
            });
        } else {
            out.push(ArchViolation {
                rule_id: RULE_RLS_ENABLED.to_string(),
                file: est_file.clone(),
                line: est_line,
                object: Some(format!("{}.{}", state.schema, state.table)),
                severity: SEVERITY_MEDIUM,
                message: format!(
                    "Defense-in-depth note: your {name} table has no Row Level Security, but the `{}` schema is \
                     not listed as API-exposed in supabase/config.toml, so this is not directly reachable through \
                     PostgREST today. Still worth enabling RLS in case the schema is exposed later (last relevant \
                     statement: {est_file}:{est_line}). {HONESTY_CAVEAT}",
                    state.schema
                ),
            });
        }
    }

    if state.rls_enabled && state.policies.is_empty() {
        out.push(ArchViolation {
            rule_id: RULE_RLS_NO_POLICY.to_string(),
            file: est_file.clone(),
            line: est_line,
            object: Some(format!("{}.{}", state.schema, state.table)),
            severity: SEVERITY_MEDIUM,
            message: format!(
                "{name} is locked to everyone. Either a feature is broken, or your app is reading it with the \
                 master (service_role) key — which skips RLS entirely. RLS was enabled at {est_file}:{est_line} \
                 with zero CREATE POLICY statements found anywhere in the migration history. {HONESTY_CAVEAT}"
            ),
        });
    }

    if !state.policies.is_empty() && !state.rls_enabled {
        let policy_locs: Vec<String> = state
            .policies
            .iter()
            .map(|p| format!("`{}` at {}:{}", p.name, p.established_at.file, p.established_at.line))
            .collect();
        out.push(ArchViolation {
            rule_id: RULE_RLS_POLICY_DISABLED.to_string(),
            file: est_file.clone(),
            line: est_line,
            object: Some(format!("{}.{}", state.schema, state.table)),
            severity: SEVERITY_CRITICAL,
            message: format!(
                "You wrote access rules for {name}, but they are switched off. The table looks protected in your \
                 code and is fully open in production. Policies found: {}. RLS state last established at \
                 {est_file}:{est_line} (disabled). {HONESTY_CAVEAT}",
                policy_locs.join(", ")
            ),
        });
    }

    out
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
    fn exposed_table_with_no_rls_fires_critical_enabled_finding() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RLS_ENABLED);
        assert_eq!(vs[0].severity, SEVERITY_CRITICAL);
        assert_eq!(vs[0].object.as_deref(), Some("public.profiles"));
        assert_eq!(vs[0].file, "supabase/migrations/20240101000000_init.sql");
        assert!(vs[0].message.contains("profiles"));
        assert!(vs[0].message.to_lowercase().contains("confirm against"));
    }

    #[test]
    fn table_with_rls_and_a_policy_is_clean() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create policy p1 on public.profiles for select using (true);",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert!(vs.is_empty(), "{vs:#?}");
    }

    #[test]
    fn non_exposed_schema_is_demoted_not_critical() {
        let f = files(vec![
            ("supabase/config.toml", "[api]\nschemas = [\"public\"]\n"),
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table internal.audit_log (id uuid primary key);",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RLS_ENABLED);
        assert_ne!(vs[0].severity, SEVERITY_CRITICAL, "non-exposed schema must be demoted");
    }

    #[test]
    fn config_toml_multi_schema_widens_exposure_scope() {
        let f = files(vec![
            ("supabase/config.toml", "[api]\nschemas = [\"public\", \"app\"]\n"),
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table app.orders (id uuid primary key);",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].severity, SEVERITY_CRITICAL, "app schema is exposed via config.toml");
    }

    #[test]
    fn rls_enabled_zero_policies_fires_no_policy_medium() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.invoices (id uuid primary key);\n\
             alter table public.invoices enable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RLS_NO_POLICY);
        assert_eq!(vs[0].severity, SEVERITY_MEDIUM);
    }

    #[test]
    fn policy_without_rls_fires_policy_disabled_critical() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.orders (id uuid primary key);\n\
             create policy p1 on public.orders for select using (true);",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        // Both the bare missing-RLS finding AND the sharper policy-disabled finding fire —
        // deliberately over-telling rather than hiding the (also true) simpler fact.
        assert_eq!(vs.len(), 2, "{vs:#?}");
        let rule_ids: Vec<&str> = vs.iter().map(|v| v.rule_id.as_str()).collect();
        assert!(rule_ids.contains(&RULE_RLS_ENABLED));
        assert!(rule_ids.contains(&RULE_RLS_POLICY_DISABLED));
        let disabled = vs.iter().find(|v| v.rule_id == RULE_RLS_POLICY_DISABLED).unwrap();
        assert_eq!(disabled.severity, SEVERITY_CRITICAL);
        assert!(disabled.message.contains("p1"));
    }

    #[test]
    fn declarative_schema_shortcut_overrides_migration_history() {
        let f = files(vec![
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table public.profiles (id uuid primary key);",
            ),
            (
                "supabase/schemas/public.sql",
                "create table public.profiles (id uuid primary key);\n\
                 alter table public.profiles enable row level security;\n\
                 create policy p1 on public.profiles for select using (true);",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert!(vs.is_empty(), "declarative snapshot must be authoritative: {vs:#?}");
    }

    #[test]
    fn no_supabase_files_yields_no_findings_zero_matching_files_never_a_false_clean() {
        let f = files(vec![("README.md", "hello")]);
        assert!(!crate::arch_checker::checker_applies(&SupabaseRlsChecker, &f));
        // check() itself is also safe to call and returns nothing to fold over.
        assert!(SupabaseRlsChecker.check(&view(&f)).is_empty());
    }

    #[test]
    fn empty_migrations_dir_and_non_sql_junk_do_not_panic_or_find_anything() {
        let f = files(vec![
            ("supabase/migrations/README.md", "not sql"),
            ("supabase/migrations/.gitkeep", ""),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert!(vs.is_empty(), "{vs:#?}");
    }

    #[test]
    fn empty_migrations_files_slice_yields_no_findings() {
        let f: Vec<(String, String)> = Vec::new();
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert!(vs.is_empty());
    }

    #[test]
    fn rename_crossing_an_enable_then_disable_ends_disabled_under_new_name() {
        // enable RLS -> rename -> disable RLS (under the NEW name): the final state must be
        // DISABLED, attributed to the new table name, proving rename doesn't let a later
        // disable "miss" the table because the key changed.
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles rename to accounts;\n\
             alter table public.accounts disable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert_eq!(vs.len(), 1, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RLS_ENABLED);
        assert_eq!(vs[0].object.as_deref(), Some("public.accounts"));
        assert_eq!(vs[0].severity, SEVERITY_CRITICAL);
    }

    #[test]
    fn policy_on_a_non_exposed_schema_table_does_not_crash_and_is_scoped_correctly() {
        let f = files(vec![
            ("supabase/config.toml", "[api]\nschemas = [\"public\"]\n"),
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table internal.audit_log (id uuid primary key);\n\
                 create policy p1 on internal.audit_log for select using (true);",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        // RLS is off (no ALTER ... ENABLE anywhere) and a policy exists -> POLICY-DISABLED-1
        // fires regardless of exposure (that rule isn't exposure-scoped); the bare
        // RLS-ENABLED-1 finding is present but DEMOTED because `internal` isn't exposed.
        let enabled = vs.iter().find(|v| v.rule_id == RULE_RLS_ENABLED).unwrap();
        assert_ne!(enabled.severity, SEVERITY_CRITICAL, "non-exposed schema stays demoted even with a policy present");
        let disabled = vs.iter().find(|v| v.rule_id == RULE_RLS_POLICY_DISABLED).unwrap();
        assert_eq!(disabled.severity, SEVERITY_CRITICAL);
    }
}
