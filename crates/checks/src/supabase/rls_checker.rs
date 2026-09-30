//! `SupabaseRlsChecker`: the flagship checker (design memo §3) — replays the
//! `supabase/migrations/*.sql` (+ `supabase/schemas/*.sql`) timeline and answers the three
//! RLS rule ids over the FINAL state per table, scoped by `supabase/config.toml`'s exposed
//! schemas.

use std::collections::{BTreeMap, BTreeSet};

use super::config::parse_exposed_schemas;
use super::timeline::{
    build_timeline, build_timeline_from_globs, find_wrong_table_reenable, ProtectionEvent,
    ProtectionKind, TableState,
};
use crate::arch_checker::{
    ArchChecker, ArchViolation, RepoView, SEVERITY_CRITICAL, SEVERITY_INFO, SEVERITY_MEDIUM,
};

pub const RULE_RLS_ENABLED: &str = "SUPABASE-RLS-ENABLED-1";
pub const RULE_RLS_NO_POLICY: &str = "SUPABASE-RLS-NO-POLICY-1";
pub const RULE_RLS_POLICY_DISABLED: &str = "SUPABASE-RLS-POLICY-DISABLED-1";
/// A table whose replayed end-state shows Row Level Security EXPLICITLY disabled (not merely
/// never enabled) and never correctly restored — either nobody wrote a matching re-enable at
/// all, or the migration's own re-enable statement targeted a DIFFERENT object (the D4
/// wrong-table copy-paste typo). Unlike [`RULE_RLS_ENABLED`], this fires independent of
/// `supabase/config.toml` exposure scoping: an explicit disable that is never restored is a
/// security-relevant authoring defect in its own right, not a PostgREST-reachability question
/// — see the module doc and [`check_disabled_not_restored`] for the full rationale.
pub const RULE_RLS_DISABLED_NOT_RESTORED: &str = "SUPABASE-RLS-DISABLED-NOT-RESTORED-1";

const RULE_IDS: &[&str] = &[
    RULE_RLS_ENABLED,
    RULE_RLS_NO_POLICY,
    RULE_RLS_POLICY_DISABLED,
    RULE_RLS_DISABLED_NOT_RESTORED,
];

/// `supabase/config.toml` is still needed (it drives `exposed_schemas` for the three
/// PostgREST-exposure-scoped rules above), but SQL discovery itself is `**/*.sql` — NOT
/// scoped to `supabase/`. [`RULE_RLS_DISABLED_NOT_RESTORED`]'s defect (an explicit RLS
/// disable that is never correctly restored) is general Postgres, not a Supabase/PostgREST
/// concept, exactly like `SUPABASE-FUNC-SEARCH-PATH-1`'s own breadth widening (see
/// `search_path_checker.rs` and `docs/plans/2026-09-30_cycle2-queue-hardening.md` §W1) — a
/// hand-rolled `db/migrations/*.sql` layout with no Supabase project at all can still disable
/// RLS mid-migration and never turn it back on.
const INTEREST_GLOBS: &[&str] = &["**/*.sql", "supabase/config.toml"];

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
            violations.extend(check_table(state, &exposed, &timeline.events_by_file));
        }

        // RULE_RLS_DISABLED_NOT_RESTORED replays over a SEPARATE, general-SQL timeline
        // (`**/*.sql`, not just `supabase/migrations|schemas/*.sql`) — its defect is general
        // Postgres, not Supabase/PostgREST-exposure-specific, so it must see migration
        // layouts the Supabase-scoped `timeline` above never looks at. A single migration
        // glob (no separate schema pass) is enough: a real Supabase repo's
        // `supabase/schemas/*.sql` still naturally sorts after `supabase/migrations/*.sql`
        // ('m' < 's'), preserving the "declarative snapshot overrides migration history" fold
        // order — see `search_path_checker.rs`'s identical precedent.
        let general_timeline = build_timeline_from_globs(repo, &["**/*.sql"], &[]);
        for state in general_timeline.tables.values() {
            if let Some(v) = check_disabled_not_restored(state, &general_timeline.events_by_file) {
                violations.push(v);
            }
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

/// D4: when `(schema, table)`'s current RLS-disabled state traces to an explicit `ALTER
/// TABLE ... DISABLE ROW LEVEL SECURITY` at `est_file:est_line`, and that same migration file
/// LATER re-enables RLS on a DIFFERENT table, build the extra sentence naming both tables and
/// both locations — the "meant to turn it back on, turned it on for the wrong table" causal
/// story. Returns `""` when there is nothing to add: the table was never explicitly disabled
/// (its established-at line is a `CREATE TABLE` default, not a disable statement), it was
/// correctly re-enabled in the same file, or no re-enable was attempted at all anywhere in the
/// file — in every one of those cases the plain "never re-enabled" framing already stands and
/// must not be embellished with an unearned wrong-table claim.
fn wrong_table_narrative(
    events_by_file: &BTreeMap<String, Vec<ProtectionEvent>>,
    est_file: &str,
    est_line: usize,
    schema: &str,
    table: &str,
) -> String {
    let Some(hit) = find_wrong_table_reenable(
        events_by_file,
        est_file,
        est_line,
        ProtectionKind::Rls,
        None,
        schema,
        table,
    ) else {
        return String::new();
    };
    let this_name = display_name(schema, table);
    let other_name = display_name(&hit.wrong_schema, &hit.wrong_object);
    format!(
        " This migration disabled RLS on {this_name} at {est_file}:{est_line}, and the next RLS-enabling \
         statement in that same file — {est_file}:{} — turned RLS back on for {other_name} instead of \
         {this_name}. That reads like a wrong-table typo in the re-enable: {this_name} was never actually \
         re-protected.",
        hit.line
    )
}

fn check_table(
    state: &TableState,
    exposed_schemas: &BTreeSet<String>,
    events_by_file: &BTreeMap<String, Vec<ProtectionEvent>>,
) -> Vec<ArchViolation> {
    let mut out = Vec::new();
    let name = display_name(&state.schema, &state.table);
    let (est_file, est_line) = state
        .rls_established_at
        .as_ref()
        .map(|l| (l.file.clone(), l.line))
        .unwrap_or_default();
    // Computed once — both `!rls_enabled` branches below (the exposed/critical finding and
    // the non-exposed/info one) and the policy-disabled finding further down all share the
    // exact same "is this table's current disabled state a wrong-table typo victim" fact.
    let narrative = if !state.rls_enabled {
        wrong_table_narrative(
            events_by_file,
            &est_file,
            est_line,
            &state.schema,
            &state.table,
        )
    } else {
        String::new()
    };

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
                     {est_file}:{est_line}).{narrative} {HONESTY_CAVEAT}"
                ),
            });
        } else {
            // Exposed-schema membership is a REACHABILITY precondition for
            // `SUPABASE-RLS-ENABLED-1`, not a severity modifier: PostgREST cannot serve a
            // schema absent from `[api].schemas`, so a no-RLS table there cannot be reached
            // with the shipped anon key and is not a live defect. We do NOT drop the
            // observation (over-tell) — we emit it as `info`, honestly bucketed as a
            // defense-in-depth suggestion rather than a critical/medium finding, so the
            // report shows it in the informational channel and never in do_now/do_next/plan.
            // NB: this exposure gate is scoped to MISSING-RLS only; NO-POLICY and
            // POLICY-DISABLED below signal broken INTENT independent of reachability and stay
            // ungated.
            out.push(ArchViolation {
                rule_id: RULE_RLS_ENABLED.to_string(),
                file: est_file.clone(),
                line: est_line,
                object: Some(format!("{}.{}", state.schema, state.table)),
                severity: SEVERITY_INFO,
                message: format!(
                    "Defense-in-depth note: your {name} table has no Row Level Security, but the `{}` schema is \
                     not listed as API-exposed in supabase/config.toml, so this is not directly reachable through \
                     PostgREST today. Still worth enabling RLS in case the schema is exposed later (last relevant \
                     statement: {est_file}:{est_line}).{narrative} {HONESTY_CAVEAT}",
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
            .map(|p| {
                format!(
                    "`{}` at {}:{}",
                    p.name, p.established_at.file, p.established_at.line
                )
            })
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
                 {est_file}:{est_line} (disabled).{narrative} {HONESTY_CAVEAT}",
                policy_locs.join(", ")
            ),
        });
    }

    out
}

/// `RULE_RLS_DISABLED_NOT_RESTORED`: mint a security-tier finding, independent of
/// `supabase/config.toml` exposure scoping and independent of any other rule that happens to
/// touch the same lines, when a table's replayed end-state is RLS-disabled AND that disabled
/// state traces to an EXPLICIT `ALTER TABLE ... DISABLE ROW LEVEL SECURITY` statement (not
/// merely a table that was never touched by RLS at all — `RULE_RLS_ENABLED` already covers
/// that plain case, gated by exposure).
///
/// Two sub-shapes both count, and both mint the SAME rule at the SAME severity:
/// - **(a) never restored**: the disable statement was never followed by a matching re-enable
///   anywhere the D4 same-file search (`find_wrong_table_reenable`) or the timeline replay's
///   own cross-file fold could find one.
/// - **(b) wrong-table re-enable**: the migration's own subsequent re-enable statement in the
///   SAME file targeted a DIFFERENT object — the classic copy-paste typo — so this table was
///   never actually re-protected even though an `ENABLE ROW LEVEL SECURITY` statement exists
///   right there in the diff.
///
/// Severity is always `SEVERITY_CRITICAL` — a security conclusion this deterministic must
/// never be downgraded by PostgREST-exposure scoping (unlike `RULE_RLS_ENABLED`) and must
/// never inherit a co-located rule's severity (a schema-hygiene or architecture rule matching
/// the same disable/enable lines does not change what THIS rule concludes). This is also what
/// lets a later cross-tier merge treat this rule as primary over an unrelated same-line hit
/// without the merge itself having to reason about severity provenance.
fn check_disabled_not_restored(
    state: &TableState,
    events_by_file: &BTreeMap<String, Vec<ProtectionEvent>>,
) -> Option<ArchViolation> {
    if state.rls_enabled {
        return None; // Currently protected (possibly re-enabled in a later migration) — clean.
    }
    let loc = state.rls_established_at.as_ref()?;

    // Precondition: the CURRENT disabled state must trace to an explicit DISABLE statement at
    // exactly this file:line, not a `CREATE TABLE` default (a table simply never touched by
    // RLS is `RULE_RLS_ENABLED`'s plain case, not this rule's concern).
    let was_explicit_disable = events_by_file.get(&loc.file).is_some_and(|events| {
        events.iter().any(|ev| {
            ev.kind == ProtectionKind::Rls
                && ev.schema == state.schema
                && ev.object == state.table
                && !ev.enabled
                && ev.line == loc.line
        })
    });
    if !was_explicit_disable {
        return None;
    }

    let name = display_name(&state.schema, &state.table);
    let hit = find_wrong_table_reenable(
        events_by_file,
        &loc.file,
        loc.line,
        ProtectionKind::Rls,
        None,
        &state.schema,
        &state.table,
    );

    let message = match &hit {
        Some(wrong) => {
            let wrong_name = display_name(&wrong.wrong_schema, &wrong.wrong_object);
            format!(
                "Row Level Security was explicitly disabled on {name} at {}:{}, and the next RLS-enabling \
                 statement in that same migration file — {}:{} — turned protection back on for {wrong_name} \
                 instead of {name}. That is a wrong-table copy-paste in the re-enable statement: {name} was \
                 never actually re-protected. {HONESTY_CAVEAT}",
                loc.file, loc.line, loc.file, wrong.line
            )
        }
        None => format!(
            "Row Level Security was explicitly disabled on {name} at {}:{} and never re-enabled anywhere in \
             the migration history. A disable that is never restored is a stronger signal than a table that \
             simply never had RLS in the first place — something intentionally turned protection off (a \
             backfill, a hotfix, a debugging session) and the matching re-enable was never written. \
             {HONESTY_CAVEAT}",
            loc.file, loc.line
        ),
    };

    Some(ArchViolation {
        rule_id: RULE_RLS_DISABLED_NOT_RESTORED.to_string(),
        file: loc.file.clone(),
        line: loc.line,
        object: Some(format!("{}.{}", state.schema, state.table)),
        severity: SEVERITY_CRITICAL,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(files: &'a [(String, String)]) -> RepoView<'a> {
        RepoView {
            spec: "test/repo",
            files,
        }
    }

    fn files(pairs: Vec<(&str, &str)>) -> Vec<(String, String)> {
        pairs
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect()
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
    fn non_exposed_schema_emits_info_not_a_defect() {
        // A no-RLS table in a schema NOT listed in `[api].schemas` is unreachable via
        // PostgREST, so it is not a live defect. The observation is still emitted (over-tell)
        // — but as `info`, the informational channel, never as a critical/medium finding.
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
        assert_eq!(
            vs[0].severity, SEVERITY_INFO,
            "non-exposed schema is an informational suggestion, not a defect"
        );
        assert!(
            vs[0].message.contains("Defense-in-depth"),
            "the informational note text is preserved: {vs:#?}"
        );
    }

    #[test]
    fn config_toml_multi_schema_widens_exposure_scope() {
        let f = files(vec![
            (
                "supabase/config.toml",
                "[api]\nschemas = [\"public\", \"app\"]\n",
            ),
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table app.orders (id uuid primary key);",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert_eq!(vs.len(), 1);
        assert_eq!(
            vs[0].severity, SEVERITY_CRITICAL,
            "app schema is exposed via config.toml"
        );
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
        let disabled = vs
            .iter()
            .find(|v| v.rule_id == RULE_RLS_POLICY_DISABLED)
            .unwrap();
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
        assert!(
            vs.is_empty(),
            "declarative snapshot must be authoritative: {vs:#?}"
        );
    }

    #[test]
    fn no_supabase_files_yields_no_findings_zero_matching_files_never_a_false_clean() {
        let f = files(vec![("README.md", "hello")]);
        assert!(!crate::arch_checker::checker_applies(
            &SupabaseRlsChecker,
            &f
        ));
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
        // Two findings now co-exist by design: the exposure-scoped RULE_RLS_ENABLED (unchanged
        // by this test's own history) AND RULE_RLS_DISABLED_NOT_RESTORED — this scenario IS an
        // explicit disable that is never restored, which is exactly that rule's positive case.
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RLS_ENABLED);
        assert_eq!(vs[0].object.as_deref(), Some("public.accounts"));
        assert_eq!(vs[0].severity, SEVERITY_CRITICAL);
        let restored = vs
            .iter()
            .find(|v| v.rule_id == RULE_RLS_DISABLED_NOT_RESTORED)
            .expect(
                "explicit disable under the renamed name must also mint the security-tier finding",
            );
        assert_eq!(restored.object.as_deref(), Some("public.accounts"));
        assert_eq!(restored.severity, SEVERITY_CRITICAL);
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
        // RLS-ENABLED-1 finding is present but INFORMATIONAL because `internal` isn't exposed.
        let enabled = vs.iter().find(|v| v.rule_id == RULE_RLS_ENABLED).unwrap();
        assert_eq!(
            enabled.severity, SEVERITY_INFO,
            "non-exposed schema stays informational even with a policy present"
        );
        let disabled = vs
            .iter()
            .find(|v| v.rule_id == RULE_RLS_POLICY_DISABLED)
            .unwrap();
        assert_eq!(disabled.severity, SEVERITY_CRITICAL);
    }

    // ── D4: wrong-table re-enable narrative ─────────────────────────────────────────

    #[test]
    fn wrong_table_reenable_is_narrated_on_the_exposed_critical_finding() {
        // The canonical D4 bug: this migration disables RLS on `profiles` for a backfill,
        // and its own re-enable statement typos the table — turning RLS on for `accounts`
        // instead. `profiles` stays exposed, and the finding must say WHY.
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create table public.accounts (id uuid primary key);\n\
             alter table public.profiles disable row level security;\n\
             alter table public.accounts enable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let profiles = vs
            .iter()
            .find(|v| {
                v.rule_id == RULE_RLS_ENABLED && v.object.as_deref() == Some("public.profiles")
            })
            .expect("profiles must still be flagged — it was never actually re-enabled");
        assert_eq!(profiles.severity, SEVERITY_CRITICAL);
        assert!(
            profiles.message.contains("profiles") && profiles.message.contains("accounts"),
            "narrative must name both the exposed table and the wrong table it re-enabled instead: {}",
            profiles.message
        );
        assert!(
            profiles.message.to_lowercase().contains("wrong-table"),
            "narrative must explicitly call out the wrong-table typo: {}",
            profiles.message
        );
        // `accounts` itself is now correctly protected (just no policy yet — a separate,
        // already-covered finding) and must NOT carry any wrong-table claim about itself.
        let accounts_findings: Vec<_> = vs
            .iter()
            .filter(|v| v.object.as_deref() == Some("public.accounts"))
            .collect();
        for v in accounts_findings {
            assert!(!v.message.to_lowercase().contains("wrong-table"), "{v:#?}");
        }
    }

    #[test]
    fn correct_reenable_in_same_file_yields_no_finding_and_no_narrative() {
        // Disabled for a backfill, then correctly re-enabled on the SAME table in the same
        // file, policy still present — fully clean, nothing to flag at all.
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create policy p1 on public.profiles for select using (true);\n\
             alter table public.profiles disable row level security;\n\
             alter table public.profiles enable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert!(
            vs.is_empty(),
            "correctly re-enabled with a policy in place must be fully clean: {vs:#?}"
        );
    }

    #[test]
    fn never_reenabled_anywhere_keeps_the_plain_narrative_with_no_wrong_table_claim() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles disable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        // RULE_RLS_ENABLED (the exposure-scoped bare finding) AND
        // RULE_RLS_DISABLED_NOT_RESTORED (this explicit disable was never restored) both fire —
        // deliberate over-telling, not a regression of this test's original assertions below.
        assert_eq!(vs.len(), 2, "{vs:#?}");
        assert_eq!(vs[0].rule_id, RULE_RLS_ENABLED);
        assert!(
            vs[0].message.contains("No evidence of RLS being enabled"),
            "{}",
            vs[0].message
        );
        assert!(
            !vs[0].message.to_lowercase().contains("wrong-table"),
            "no re-enable was attempted anywhere — must not fabricate a wrong-table claim: {}",
            vs[0].message
        );
        let restored = vs
            .iter()
            .find(|v| v.rule_id == RULE_RLS_DISABLED_NOT_RESTORED)
            .expect("an explicit disable with no re-enable anywhere must mint the security-tier finding");
        assert_eq!(restored.severity, SEVERITY_CRITICAL);
        assert_eq!(restored.object.as_deref(), Some("public.profiles"));
        assert!(
            !restored.message.to_lowercase().contains("wrong-table"),
            "no re-enable was attempted anywhere — must not fabricate a wrong-table claim: {}",
            restored.message
        );
    }

    #[test]
    fn a_table_never_touched_by_rls_is_not_mistaken_for_a_wrong_table_victim() {
        // The critical false-positive guard at the checker level: a migration creates two
        // tables and enables RLS on only one of them (ordinary, extremely common migration
        // shape) — the untouched table's finding must read as the plain "no RLS" case, never
        // as if some OTHER statement's enable was "meant for it."
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.audit_log (id uuid primary key);\n\
             create table public.accounts (id uuid primary key);\n\
             alter table public.accounts enable row level security;\n\
             create policy p1 on public.accounts for select using (true);",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let audit = vs
            .iter()
            .find(|v| v.object.as_deref() == Some("public.audit_log"))
            .expect("audit_log must still be flagged for missing RLS");
        assert!(
            !audit.message.to_lowercase().contains("wrong-table"),
            "a table that was never explicitly disabled must not borrow an unrelated enable: {}",
            audit.message
        );
    }

    #[test]
    fn wrong_table_reenable_is_narrated_on_the_policy_disabled_finding_too() {
        // Same wrong-table shape, but this time `profiles` also has a policy written for it —
        // exercising RULE_RLS_POLICY_DISABLED's own message, not just RULE_RLS_ENABLED's.
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create policy p1 on public.profiles for select using (true);\n\
             create table public.accounts (id uuid primary key);\n\
             alter table public.profiles disable row level security;\n\
             alter table public.accounts enable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let disabled = vs
            .iter()
            .find(|v| v.rule_id == RULE_RLS_POLICY_DISABLED)
            .expect("policy-disabled finding must still fire");
        assert!(
            disabled.message.to_lowercase().contains("wrong-table")
                && disabled.message.contains("accounts"),
            "{}",
            disabled.message
        );
    }

    #[test]
    fn non_exposed_schema_info_finding_also_carries_the_narrative() {
        let f = files(vec![
            ("supabase/config.toml", "[api]\nschemas = [\"public\"]\n"),
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table internal.secrets (id uuid primary key);\n\
                 alter table internal.secrets enable row level security;\n\
                 create table internal.other (id uuid primary key);\n\
                 alter table internal.secrets disable row level security;\n\
                 alter table internal.other enable row level security;",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let info = vs
            .iter()
            .find(|v| v.object.as_deref() == Some("internal.secrets"))
            .expect("secrets must still be flagged (informationally — internal isn't exposed)");
        assert_eq!(info.severity, SEVERITY_INFO);
        assert!(
            info.message.to_lowercase().contains("wrong-table") && info.message.contains("other"),
            "{}",
            info.message
        );
    }

    // ── C3-2: RULE_RLS_DISABLED_NOT_RESTORED — deterministic, security-tier, independent of
    // exposure scoping and of any co-located rule's own severity ─────────────────────────────
    //
    // A migration that disables row-level protection and never restores it must mint a
    // top-severity SECURITY finding deterministically — it must never depend on the
    // non-deterministic AI-advisory tier, and must never inherit an unrelated (or merely
    // co-located) rule's severity.

    fn restored_findings<'a>(vs: &'a [ArchViolation]) -> Vec<&'a ArchViolation> {
        vs.iter()
            .filter(|v| v.rule_id == RULE_RLS_DISABLED_NOT_RESTORED)
            .collect()
    }

    #[test]
    fn positive_a_disabled_with_no_reenable_anywhere_fires_one_critical_finding() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles disable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let restored = restored_findings(&vs);
        assert_eq!(restored.len(), 1, "{vs:#?}");
        assert_eq!(restored[0].severity, SEVERITY_CRITICAL);
        assert_eq!(restored[0].object.as_deref(), Some("public.profiles"));
        assert!(
            restored[0].message.contains("profiles"),
            "{}",
            restored[0].message
        );
        assert!(
            !restored[0].message.to_lowercase().contains("wrong-table"),
            "case (a) has no wrong-table claim to make: {}",
            restored[0].message
        );
    }

    #[test]
    fn positive_b_disabled_then_wrong_table_reenable_fires_one_critical_finding_naming_both() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             create table public.accounts (id uuid primary key);\n\
             alter table public.profiles disable row level security;\n\
             alter table public.accounts enable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let restored = restored_findings(&vs);
        assert_eq!(restored.len(), 1, "{vs:#?}");
        assert_eq!(restored[0].severity, SEVERITY_CRITICAL);
        assert_eq!(
            restored[0].object.as_deref(),
            Some("public.profiles"),
            "the affected (still-unprotected) object is profiles, not the mismatched accounts"
        );
        assert!(
            restored[0].message.contains("profiles") && restored[0].message.contains("accounts"),
            "message must name both the affected object and the mismatched object: {}",
            restored[0].message
        );
        assert!(
            restored[0].message.to_lowercase().contains("wrong-table")
                || restored[0].message.to_lowercase().contains("copy-paste"),
            "message must explicitly call out the wrong-table/copy-paste mismatch: {}",
            restored[0].message
        );
    }

    #[test]
    fn safe_twin_disable_then_correct_same_object_reenable_fires_zero_findings() {
        let f = files(vec![(
            "supabase/migrations/20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n\
             alter table public.profiles enable row level security;\n\
             alter table public.profiles disable row level security;\n\
             alter table public.profiles enable row level security;",
        )]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        assert!(
            restored_findings(&vs).is_empty(),
            "a disable correctly restored on the SAME object must mint nothing: {vs:#?}"
        );
    }

    #[test]
    fn severity_is_independent_of_a_co_located_lower_severity_finding_on_the_same_lines() {
        // `internal` is NOT an exposed schema, so RULE_RLS_ENABLED — the OTHER rule that also
        // fires over these exact same lines/object — is downgraded to SEVERITY_INFO (see
        // `non_exposed_schema_emits_info_not_a_defect`). RULE_RLS_DISABLED_NOT_RESTORED must
        // still conclude SEVERITY_CRITICAL for the identical object: a security conclusion must
        // never inherit, or be shadowed by, a co-located rule's own (lower) severity.
        let f = files(vec![
            ("supabase/config.toml", "[api]\nschemas = [\"public\"]\n"),
            (
                "supabase/migrations/20240101000000_init.sql",
                "create table internal.secrets (id uuid primary key);\n\
                 alter table internal.secrets enable row level security;\n\
                 alter table internal.secrets disable row level security;",
            ),
        ]);
        let vs = SupabaseRlsChecker.check(&view(&f));
        let enabled = vs.iter().find(|v| v.rule_id == RULE_RLS_ENABLED).expect(
            "the co-located, lower-severity rule must still fire for this test to prove anything",
        );
        assert_eq!(
            enabled.severity, SEVERITY_INFO,
            "sanity check: the co-located rule really is lower severity here"
        );
        let restored = restored_findings(&vs);
        assert_eq!(restored.len(), 1, "{vs:#?}");
        assert_eq!(
            restored[0].severity, SEVERITY_CRITICAL,
            "the security rule's own severity must not be shadowed by the co-located rule's info-tier verdict"
        );
        assert_eq!(restored[0].object.as_deref(), Some("internal.secrets"));
    }

    #[test]
    fn fires_outside_the_supabase_folder_convention_general_postgres_layout() {
        // The defect (explicit disable, never restored) is general Postgres, not a
        // Supabase/PostgREST-exposure concept — a hand-rolled `db/migrations/` layout with no
        // `supabase/` directory at all must still be inspected, mirroring
        // `SUPABASE-FUNC-SEARCH-PATH-1`'s own breadth widening.
        let f = files(vec![(
            "db/migrations/0001_init.sql",
            "create table public.widgets (id uuid primary key);\n\
             alter table public.widgets enable row level security;\n\
             alter table public.widgets disable row level security;",
        )]);
        assert!(
            crate::arch_checker::checker_applies(&SupabaseRlsChecker, &f),
            "a plain db/migrations/*.sql layout must arm the checker via the **/*.sql glob"
        );
        let vs = SupabaseRlsChecker.check(&view(&f));
        let restored = restored_findings(&vs);
        assert_eq!(restored.len(), 1, "{vs:#?}");
        assert_eq!(restored[0].severity, SEVERITY_CRITICAL);
        assert_eq!(restored[0].file, "db/migrations/0001_init.sql");
    }
}
