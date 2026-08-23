//! The migration-timeline replay fold: shared by [`super::rls_checker::SupabaseRlsChecker`]
//! and [`super::search_path_checker::SupabaseFnSearchPathChecker`] — "near-free once RLS
//! ships" per the design memo, because both checkers need the SAME ordered-file, split,
//! classify, fold pipeline; only the queries over the resulting [`Timeline`] differ.
//!
//! # Ordering contract
//!
//! `supabase/migrations/<timestamp>_*.sql` files are folded in ASCENDING filename order
//! (Supabase's own fixed-width `YYYYMMDDHHMMSS_name.sql` convention makes lexicographic
//! string order equal to chronological order). `supabase/schemas/*.sql` declarative files,
//! when present, are folded AFTER every migration, also in filename order — the "declarative
//! shortcut" (memo §3 step 5) falls out for free from this: whichever statement establishes
//! a table/function LAST wins, so a declarative snapshot naturally overrides migration
//! history for the objects it re-declares, while objects it doesn't mention keep whatever
//! the migration replay computed.

use std::collections::BTreeMap;

use super::sql_parse::{classify_statement, ParsedStmt};
use super::splitter::split_statements;
use crate::arch_checker::RepoView;

/// Where a fact was last established: the file + 1-based line of the statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: String,
    pub line: usize,
}

/// One policy currently active on a table (created and not since dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRecord {
    pub name: String,
    pub established_at: Location,
}

/// The replayed end-state of one table: does it currently have RLS enabled, and what
/// policies (if any) currently exist on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableState {
    pub schema: String,
    pub table: String,
    pub rls_enabled: bool,
    /// Where `rls_enabled`'s CURRENT value was last established — `None` only when the
    /// table's very first `CREATE TABLE` never had its RLS default statement recorded, which
    /// should not happen in practice (creation always stamps a location).
    pub rls_established_at: Option<Location>,
    pub policies: Vec<PolicyRecord>,
}

/// The replayed end-state of one `SECURITY DEFINER` function's search_path posture. Only
/// `SECURITY DEFINER` functions are load-bearing for `SUPABASE-FUNC-SEARCH-PATH-1`, but every
/// `CREATE FUNCTION` is recorded so a later `CREATE OR REPLACE` can correctly supersede it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionState {
    pub schema: String,
    pub name: String,
    pub security_definer: bool,
    pub has_search_path: bool,
    pub established_at: Location,
}

/// The full replayed end-state: every currently-existing table, keyed `(schema, table)`, and
/// every currently-defined function, keyed `(schema, name)`. A `BTreeMap` so iteration order
/// (and therefore finding order) is deterministic across runs.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    pub tables: BTreeMap<(String, String), TableState>,
    pub functions: BTreeMap<(String, String), FunctionState>,
}

/// Build the replayed [`Timeline`] from a [`RepoView`]: gather `supabase/migrations/*.sql`
/// (sorted by filename) then `supabase/schemas/*.sql` (sorted by filename, folded last), and
/// fold every statement in each, in order. Never panics — a malformed file just contributes
/// fewer recognized statements (`classify_statement` returns `None` for anything it can't
/// place), never an error that aborts the whole fold.
pub fn build_timeline(repo: &RepoView<'_>) -> Timeline {
    let mut migration_files: Vec<&(String, String)> = repo
        .files
        .iter()
        .filter(|(path, _)| crate::arch_checker::glob_match("supabase/migrations/*.sql", path))
        .collect();
    migration_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut schema_files: Vec<&(String, String)> = repo
        .files
        .iter()
        .filter(|(path, _)| crate::arch_checker::glob_match("supabase/schemas/*.sql", path))
        .collect();
    schema_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut timeline = Timeline::default();
    for (path, content) in migration_files.into_iter().chain(schema_files) {
        fold_file(&mut timeline, path, content);
    }
    timeline
}

fn fold_file(timeline: &mut Timeline, path: &str, content: &str) {
    for stmt in split_statements(content) {
        if let Some(parsed) = classify_statement(&stmt.text) {
            apply_statement(timeline, path, stmt.line, parsed);
        }
    }
}

fn apply_statement(timeline: &mut Timeline, file: &str, line: usize, stmt: ParsedStmt) {
    let loc = || Location {
        file: file.to_string(),
        line,
    };
    match stmt {
        ParsedStmt::CreateTable {
            schema,
            table,
            if_not_exists,
        } => {
            let key = (schema.clone(), table.clone());
            if if_not_exists && timeline.tables.contains_key(&key) {
                // Idempotent re-declaration of an already-known table: a no-op, must NOT
                // wipe accumulated RLS/policy state.
                return;
            }
            timeline.tables.insert(
                key,
                TableState {
                    schema,
                    table,
                    rls_enabled: false, // Postgres default: RLS is off until explicitly enabled.
                    rls_established_at: Some(loc()),
                    policies: Vec::new(),
                },
            );
        }
        ParsedStmt::DropTable { schema, table } => {
            timeline.tables.remove(&(schema, table));
        }
        ParsedStmt::RenameTable { schema, from, to } => {
            let from_key = (schema.clone(), from);
            let to_key = (schema.clone(), to.clone());
            if let Some(mut st) = timeline.tables.remove(&from_key) {
                st.table = to;
                timeline.tables.insert(to_key, st);
            } else {
                // Unknown prior state (e.g. the table came from a baseline this repo never
                // committed) — start a fresh entry under the new name rather than losing the
                // rename entirely; a table that then never gets RLS enabled is still
                // correctly flagged under its NEW name.
                timeline.tables.insert(
                    to_key,
                    TableState {
                        schema,
                        table: to,
                        rls_enabled: false,
                        rls_established_at: Some(loc()),
                        policies: Vec::new(),
                    },
                );
            }
        }
        ParsedStmt::AlterRls { schema, table, enabled } => {
            let key = (schema.clone(), table.clone());
            let entry = timeline.tables.entry(key).or_insert_with(|| TableState {
                schema,
                table,
                rls_enabled: false,
                rls_established_at: None,
                policies: Vec::new(),
            });
            entry.rls_enabled = enabled;
            entry.rls_established_at = Some(loc());
        }
        ParsedStmt::CreatePolicy { schema, table, name } => {
            let key = (schema.clone(), table.clone());
            let entry = timeline.tables.entry(key).or_insert_with(|| TableState {
                schema,
                table,
                rls_enabled: false,
                rls_established_at: None,
                policies: Vec::new(),
            });
            entry.policies.retain(|p| p.name != name); // CREATE POLICY on an existing name replaces it
            entry.policies.push(PolicyRecord {
                name,
                established_at: loc(),
            });
        }
        ParsedStmt::DropPolicy { schema, table, name } => {
            if let Some(entry) = timeline.tables.get_mut(&(schema, table)) {
                entry.policies.retain(|p| p.name != name);
            }
        }
        ParsedStmt::CreateFunction {
            schema,
            name,
            security_definer,
            has_search_path,
        } => {
            timeline.functions.insert(
                (schema.clone(), name.clone()),
                FunctionState {
                    schema,
                    name,
                    security_definer,
                    has_search_path,
                    established_at: loc(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline_from(files: Vec<(&str, &str)>) -> Timeline {
        let files: Vec<(String, String)> = files.into_iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
        let repo = RepoView { spec: "test/repo", files: &files };
        build_timeline(&repo)
    }

    #[test]
    fn table_never_touched_by_rls_has_no_rls() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);",
        )]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(!st.rls_enabled);
        assert!(st.policies.is_empty());
    }

    #[test]
    fn enable_then_disable_final_state_is_disabled() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles disable row level security;",
        )]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(!st.rls_enabled, "final state must be DISABLED, not a per-statement OR");
    }

    #[test]
    fn rls_enabled_in_a_later_migration_is_reflected_no_false_positive() {
        let t = timeline_from(vec![
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table public.profiles (id uuid primary key);",
            ),
            (
                "supabase/migrations/20240201000000_secure.sql",
                "alter table public.profiles enable row level security;",
            ),
        ]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(st.rls_enabled, "a per-file scan would miss the later migration; replay must not");
        assert_eq!(
            st.rls_established_at.as_ref().unwrap().file,
            "supabase/migrations/20240201000000_secure.sql"
        );
    }

    #[test]
    fn rename_preserves_rls_and_policy_state_under_new_name() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create policy p1 on public.profiles for select using (true);\n\
             alter table public.profiles rename to accounts;",
        )]);
        assert!(!t.tables.contains_key(&("public".to_string(), "profiles".to_string())));
        let st = t.tables.get(&("public".to_string(), "accounts".to_string())).unwrap();
        assert!(st.rls_enabled, "rename must carry forward the RLS state");
        assert_eq!(st.policies.len(), 1, "rename must carry forward existing policies");
    }

    #[test]
    fn drop_then_recreate_resets_state() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             drop table public.profiles;\n\
             create table public.profiles (id uuid primary key);",
        )]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(!st.rls_enabled, "a dropped-and-recreated table must NOT inherit the old RLS state");
        assert!(st.policies.is_empty());
    }

    #[test]
    fn if_not_exists_on_an_existing_table_does_not_reset_state() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create table if not exists public.profiles (id uuid primary key);",
        )]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(st.rls_enabled, "an idempotent IF NOT EXISTS re-declaration must not wipe RLS state");
    }

    #[test]
    fn declarative_schema_file_is_authoritative_for_covered_tables() {
        // Migration says RLS is OFF; the declarative schema snapshot (folded LAST) says ON —
        // the schema file must win, proving the "authoritative end-state" shortcut.
        let t = timeline_from(vec![
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table public.profiles (id uuid primary key);",
            ),
            (
                "supabase/schemas/public.sql",
                "create table public.profiles (id uuid primary key);\n\
                 alter table public.profiles enable row level security;",
            ),
        ]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(st.rls_enabled);
    }

    #[test]
    fn declarative_schema_file_does_not_affect_tables_it_does_not_mention() {
        let t = timeline_from(vec![
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table public.profiles (id uuid primary key);\n\
                 alter table public.profiles enable row level security;\n\
                 create table public.orders (id uuid primary key);",
            ),
            (
                "supabase/schemas/public.sql",
                "create table public.profiles (id uuid primary key);\n\
                 alter table public.profiles enable row level security;",
            ),
        ]);
        // orders is untouched by the schema file — migration-derived state (no RLS) stands.
        let orders = t.tables.get(&("public".to_string(), "orders".to_string())).unwrap();
        assert!(!orders.rls_enabled);
    }

    #[test]
    fn drop_policy_removes_it_from_the_active_set() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create policy p1 on public.profiles for select using (true);\n\
             drop policy p1 on public.profiles;",
        )]);
        let st = t.tables.get(&("public".to_string(), "profiles".to_string())).unwrap();
        assert!(st.policies.is_empty());
    }

    #[test]
    fn non_exposed_schema_table_still_tracked_for_the_checker_to_scope() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table internal.audit_log (id uuid primary key);",
        )]);
        assert!(t.tables.contains_key(&("internal".to_string(), "audit_log".to_string())));
    }

    #[test]
    fn create_or_replace_function_supersedes_prior_definition() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create function public.f() returns void security definer as $$ begin end; $$ language plpgsql;\n\
             create or replace function public.f() returns void security definer set search_path = public as $$ begin end; $$ language plpgsql;",
        )]);
        let f = t.functions.get(&("public".to_string(), "f".to_string())).unwrap();
        assert!(f.has_search_path, "the LATER definition (with search_path) must win");
    }

    #[test]
    fn empty_migrations_and_junk_files_yield_an_empty_timeline_without_panicking() {
        let t = timeline_from(vec![
            ("supabase/migrations/README.md", "not sql at all"),
            ("supabase/migrations/20240101000000_init.sql", ""),
        ]);
        assert!(t.tables.is_empty());
        assert!(t.functions.is_empty());
    }

    #[test]
    fn no_supabase_files_at_all_yields_an_empty_timeline() {
        let t = timeline_from(vec![("README.md", "hello")]);
        assert!(t.tables.is_empty());
    }
}
