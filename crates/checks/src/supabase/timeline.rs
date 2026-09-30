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

use super::splitter::split_statements;
use super::sql_parse::{classify_statement, ParsedStmt};
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
    /// Every boolean-style protection toggle statement (today: RLS enable/disable, trigger
    /// enable/disable), in FILE ORDER, keyed by the file it appeared in. `tables`/`functions`
    /// above answer "what is the CURRENT end-state" — this answers the different question the
    /// D4 wrong-table re-enable narrative needs: "what happened right after THIS statement, in
    /// this SAME file?" See [`find_wrong_table_reenable`].
    pub events_by_file: BTreeMap<String, Vec<ProtectionEvent>>,
}

/// Which disable/enable-pair protection a [`ProtectionEvent`] records. `Rls` is a per-table
/// singleton (no name); `Trigger` is per-trigger-name (a table can have several, each toggled
/// independently, so identity requires the name too — see [`ProtectionEvent::name`]).
///
/// Extension point: a constraint checker would need its own variant, but Postgres has no
/// clean boolean toggle for constraints the way it does for RLS/triggers (`DROP CONSTRAINT` +
/// `ADD CONSTRAINT` changes the constraint's identity, not just a flag), so it isn't modeled
/// here — [`find_wrong_table_reenable`] is generic over `ProtectionKind` and needs no changes
/// once a `Constraint` variant and its event-recording arm are added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionKind {
    Rls,
    Trigger,
}

/// One `{ENABLE|DISABLE}`-style toggle statement, recorded in the file it appeared in, at the
/// line it appeared on. `name` disambiguates WHICH protection this is when more than one can
/// coexist on the same table (a trigger's name); `None` for RLS, which has exactly one
/// per table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionEvent {
    pub kind: ProtectionKind,
    pub schema: String,
    pub object: String,
    pub name: Option<String>,
    pub enabled: bool,
    pub line: usize,
}

/// A detected "wrong-table re-enable" (design doc D4): within one migration file, the
/// protection identified by `kind`/`name` on some object was explicitly DISABLED, and the
/// LAST subsequent same-identity ENABLE statement in that same file targeted a DIFFERENT
/// object (`wrong_schema`.`wrong_object`, at `line`) instead of the one that was disabled —
/// the classic "meant to turn it back on, turned it on for the wrong table" typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrongTableReenable {
    pub kind: ProtectionKind,
    pub name: Option<String>,
    pub wrong_schema: String,
    pub wrong_object: String,
    pub line: usize,
}

/// Look for a wrong-table re-enable affecting the `kind`/`name` protection on `(schema,
/// object)`, whose disabling statement is at `disable_line` in `file`.
///
/// Two guards keep this from over-firing:
/// - **Precondition**: `disable_line` must itself be a recorded, explicit DISABLE event for
///   this exact `(kind, name, schema, object)`. A table whose current disabled state traces to
///   a `CREATE TABLE` default (RLS simply never touched) has nothing to narrate — the file's
///   unrelated `ENABLE ...` statements for OTHER objects are ordinary migration work, not a
///   typo, and must never be mistaken for one. Without this check, any migration that creates
///   several tables and enables RLS on only some of them would falsely read as "wrong table."
/// - **Identity**: candidate re-enables must match both `kind` AND `name` (`None` only matches
///   `None`) — a disabled `audit_trg` trigger is not "the same protection" as an unrelated
///   `log_trg` trigger enabled elsewhere, even on the same kind of object.
///
/// Returns `None` when: the object itself was correctly re-enabled later in the file (nothing
/// to narrate — the plain end-state already reflects "protected"); no re-enable of this
/// identity was attempted anywhere in the file (the existing "never re-enabled" framing
/// already covers that honestly); or the precondition above isn't met.
pub fn find_wrong_table_reenable(
    events_by_file: &BTreeMap<String, Vec<ProtectionEvent>>,
    file: &str,
    disable_line: usize,
    kind: ProtectionKind,
    name: Option<&str>,
    schema: &str,
    object: &str,
) -> Option<WrongTableReenable> {
    let events = events_by_file.get(file)?;

    let was_explicitly_disabled = events.iter().any(|ev| {
        ev.kind == kind
            && ev.name.as_deref() == name
            && ev.schema == schema
            && ev.object == object
            && !ev.enabled
            && ev.line == disable_line
    });
    if !was_explicitly_disabled {
        return None;
    }

    let mut last_wrong: Option<&ProtectionEvent> = None;
    for ev in events {
        if ev.kind != kind || ev.name.as_deref() != name || !ev.enabled || ev.line <= disable_line {
            continue;
        }
        if ev.schema == schema && ev.object == object {
            // Correctly re-enabled later in this same file — nothing to narrate.
            return None;
        }
        let is_later = match last_wrong {
            None => true,
            Some(w) => ev.line > w.line,
        };
        if is_later {
            last_wrong = Some(ev);
        }
    }
    last_wrong.map(|ev| WrongTableReenable {
        kind: ev.kind,
        name: ev.name.clone(),
        wrong_schema: ev.schema.clone(),
        wrong_object: ev.object.clone(),
        line: ev.line,
    })
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
        ParsedStmt::AlterRls {
            schema,
            table,
            enabled,
        } => {
            let key = (schema.clone(), table.clone());
            timeline
                .events_by_file
                .entry(file.to_string())
                .or_default()
                .push(ProtectionEvent {
                    kind: ProtectionKind::Rls,
                    schema: schema.clone(),
                    object: table.clone(),
                    name: None,
                    enabled,
                    line,
                });
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
        ParsedStmt::AlterTrigger {
            schema,
            table,
            trigger,
            enabled,
        } => {
            // No checker tracks trigger end-state today (see `ProtectionKind::Trigger`'s doc
            // comment) — this only feeds the D4 wrong-table re-enable event log.
            timeline
                .events_by_file
                .entry(file.to_string())
                .or_default()
                .push(ProtectionEvent {
                    kind: ProtectionKind::Trigger,
                    schema,
                    object: table,
                    name: Some(trigger),
                    enabled,
                    line,
                });
        }
        ParsedStmt::CreatePolicy {
            schema,
            table,
            name,
        } => {
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
        ParsedStmt::DropPolicy {
            schema,
            table,
            name,
        } => {
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
        let files: Vec<(String, String)> = files
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        let repo = RepoView {
            spec: "test/repo",
            files: &files,
        };
        build_timeline(&repo)
    }

    #[test]
    fn table_never_touched_by_rls_has_no_rls() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);",
        )]);
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
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
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
        assert!(
            !st.rls_enabled,
            "final state must be DISABLED, not a per-statement OR"
        );
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
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
        assert!(
            st.rls_enabled,
            "a per-file scan would miss the later migration; replay must not"
        );
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
        assert!(!t
            .tables
            .contains_key(&("public".to_string(), "profiles".to_string())));
        let st = t
            .tables
            .get(&("public".to_string(), "accounts".to_string()))
            .unwrap();
        assert!(st.rls_enabled, "rename must carry forward the RLS state");
        assert_eq!(
            st.policies.len(),
            1,
            "rename must carry forward existing policies"
        );
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
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
        assert!(
            !st.rls_enabled,
            "a dropped-and-recreated table must NOT inherit the old RLS state"
        );
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
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
        assert!(
            st.rls_enabled,
            "an idempotent IF NOT EXISTS re-declaration must not wipe RLS state"
        );
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
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
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
        let orders = t
            .tables
            .get(&("public".to_string(), "orders".to_string()))
            .unwrap();
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
        let st = t
            .tables
            .get(&("public".to_string(), "profiles".to_string()))
            .unwrap();
        assert!(st.policies.is_empty());
    }

    #[test]
    fn non_exposed_schema_table_still_tracked_for_the_checker_to_scope() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table internal.audit_log (id uuid primary key);",
        )]);
        assert!(t
            .tables
            .contains_key(&("internal".to_string(), "audit_log".to_string())));
    }

    #[test]
    fn create_or_replace_function_supersedes_prior_definition() {
        let t = timeline_from(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create function public.f() returns void security definer as $$ begin end; $$ language plpgsql;\n\
             create or replace function public.f() returns void security definer set search_path = public as $$ begin end; $$ language plpgsql;",
        )]);
        let f = t
            .functions
            .get(&("public".to_string(), "f".to_string()))
            .unwrap();
        assert!(
            f.has_search_path,
            "the LATER definition (with search_path) must win"
        );
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

    // ── D4: wrong-table re-enable narrative — `find_wrong_table_reenable` ──────────────

    const MIGRATION: &str = "supabase/migrations/20240101000000_init.sql";

    fn disable_line_for(t: &Timeline, schema: &str, table: &str) -> usize {
        t.tables
            .get(&(schema.to_string(), table.to_string()))
            .and_then(|st| st.rls_established_at.as_ref())
            .expect("table must exist with an established RLS location")
            .line
    }

    #[test]
    fn disable_x_then_wrong_table_enable_y_is_detected() {
        // The canonical D4 bug: RLS disabled on `profiles` for a backfill, and the
        // migration's later re-enable statement targets `accounts` instead — a wrong-table
        // typo that leaves `profiles` exposed.
        let t = timeline_from(vec![(
            MIGRATION,
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create table public.accounts (id uuid primary key);\n\
             alter table public.profiles disable row level security;\n\
             alter table public.accounts enable row level security;",
        )]);
        assert!(
            !t.tables
                .get(&("public".to_string(), "profiles".to_string()))
                .unwrap()
                .rls_enabled
        );
        let disable_line = disable_line_for(&t, "public", "profiles");
        let hit = find_wrong_table_reenable(
            &t.events_by_file,
            MIGRATION,
            disable_line,
            ProtectionKind::Rls,
            None,
            "public",
            "profiles",
        )
        .expect("must detect the wrong-table re-enable");
        assert_eq!(hit.wrong_schema, "public");
        assert_eq!(hit.wrong_object, "accounts");
        assert!(hit.line > disable_line);
    }

    #[test]
    fn disable_x_then_correct_enable_x_is_not_mis_narrated() {
        let t = timeline_from(vec![(
            MIGRATION,
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles disable row level security;\n\
             alter table public.profiles enable row level security;",
        )]);
        // The final state is correctly enabled again — a real disable/enable-pair caller
        // would never reach the narrative for this table (there is no finding), but the
        // detector itself must also refuse to narrate: probing with the disable statement's
        // OWN line must find the SAME-table re-enable and return None.
        let disable_line = t
            .events_by_file
            .get(MIGRATION)
            .unwrap()
            .iter()
            .find(|e| e.kind == ProtectionKind::Rls && !e.enabled)
            .unwrap()
            .line;
        let hit = find_wrong_table_reenable(
            &t.events_by_file,
            MIGRATION,
            disable_line,
            ProtectionKind::Rls,
            None,
            "public",
            "profiles",
        );
        assert!(
            hit.is_none(),
            "correct same-table re-enable must not be mis-narrated: {hit:#?}"
        );
    }

    #[test]
    fn disable_x_with_no_enable_anywhere_is_not_wrong_table() {
        let t = timeline_from(vec![(
            MIGRATION,
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles disable row level security;",
        )]);
        let disable_line = disable_line_for(&t, "public", "profiles");
        let hit = find_wrong_table_reenable(
            &t.events_by_file,
            MIGRATION,
            disable_line,
            ProtectionKind::Rls,
            None,
            "public",
            "profiles",
        );
        assert!(
            hit.is_none(),
            "no re-enable attempted at all — plain 'never re-enabled' stands: {hit:#?}"
        );
    }

    #[test]
    fn a_table_never_touched_by_rls_is_never_mistaken_for_a_wrong_table_victim() {
        // The critical false-positive guard: migrations routinely create several tables and
        // enable RLS on only SOME of them. A table that was simply never touched (its
        // established-at line is its CREATE TABLE, not an explicit DISABLE) must never read
        // as "disabled, and the enable over there was meant for it."
        let t = timeline_from(vec![(
            MIGRATION,
            "create table public.audit_log (id uuid primary key);\n\
             create table public.accounts (id uuid primary key);\n\
             alter table public.accounts enable row level security;",
        )]);
        let never_touched_line = disable_line_for(&t, "public", "audit_log");
        let hit = find_wrong_table_reenable(
            &t.events_by_file,
            MIGRATION,
            never_touched_line,
            ProtectionKind::Rls,
            None,
            "public",
            "audit_log",
        );
        assert!(hit.is_none(), "a table that was never explicitly disabled must not borrow an unrelated enable: {hit:#?}");
    }

    #[test]
    fn generalizes_to_trigger_disable_enable_pairs_by_name() {
        // Same shape as RLS, but the "protection" is a named trigger: disabling
        // `sync_totals` on `orders` and re-enabling a trigger of the SAME NAME on
        // `invoices` is the identical wrong-table typo pattern.
        let t = timeline_from(vec![(
            MIGRATION,
            "create table public.orders (id uuid primary key);\n\
             create table public.invoices (id uuid primary key);\n\
             alter table public.orders disable trigger sync_totals;\n\
             alter table public.invoices enable trigger sync_totals;",
        )]);
        let disable_line = t
            .events_by_file
            .get(MIGRATION)
            .unwrap()
            .iter()
            .find(|e| e.kind == ProtectionKind::Trigger && !e.enabled)
            .unwrap()
            .line;
        let hit = find_wrong_table_reenable(
            &t.events_by_file,
            MIGRATION,
            disable_line,
            ProtectionKind::Trigger,
            Some("sync_totals"),
            "public",
            "orders",
        )
        .expect("trigger disable/enable-pair mismatch must be detected identically to RLS");
        assert_eq!(hit.wrong_object, "invoices");
        assert_eq!(hit.name.as_deref(), Some("sync_totals"));
    }

    #[test]
    fn differently_named_triggers_are_unrelated_protections_not_a_wrong_table_pair() {
        // Disabling trigger `a` on X and separately enabling an UNRELATED trigger `b`
        // elsewhere must never be narrated as a wrong-table re-enable of `a`.
        let t = timeline_from(vec![(
            MIGRATION,
            "create table public.orders (id uuid primary key);\n\
             create table public.invoices (id uuid primary key);\n\
             alter table public.orders disable trigger trg_a;\n\
             alter table public.invoices enable trigger trg_b;",
        )]);
        let disable_line = t
            .events_by_file
            .get(MIGRATION)
            .unwrap()
            .iter()
            .find(|e| e.kind == ProtectionKind::Trigger && !e.enabled)
            .unwrap()
            .line;
        let hit = find_wrong_table_reenable(
            &t.events_by_file,
            MIGRATION,
            disable_line,
            ProtectionKind::Trigger,
            Some("trg_a"),
            "public",
            "orders",
        );
        assert!(
            hit.is_none(),
            "a differently-named trigger enable is not the same protection: {hit:#?}"
        );
    }
}
