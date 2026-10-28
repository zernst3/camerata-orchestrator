//! Rank 8: the real corpus's `candidates` declarations. A locator that misses a site is a
//! finding the model never sees, so the set is pinned: giving another rule `candidates` (or
//! changing a locator) fails this test until the rule is reviewed and added to `REVIEWED`, with a
//! synthetic violating fixture whose violating line the locator must still find.
//!
//! No model, no network: this drives the production locators (`camerata_checks::candidates`)
//! over the production corpus loader.

use camerata_checks::candidates::{locate, CandidateLocator, FileCandidates};
use camerata_server::scan_routing::audit_rules;

/// Every rule that declares `candidates`: its locator, a synthetic migration that violates it,
/// and the 1-based line of the violation, which must fall inside a site the locator returns.
const REVIEWED: &[(&str, CandidateLocator, &str, usize)] = &[
    // Routine rules: the function definition, its grants, and DO blocks.
    (
        "SUPABASE-FUNC-SEARCH-PATH-1",
        CandidateLocator::SqlRoutines,
        "create table t (id int);\n\
         create function public.f() returns void\n\
         language plpgsql security definer\n\
         as $$ begin delete from t; end $$;\n",
        3,
    ),
    (
        "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1",
        CandidateLocator::SqlRoutines,
        "create table t (id int);\n\
         create or replace function public.q(p text) returns void\n\
         language plpgsql as $$\n\
         begin\n\
           execute 'select * from t where id = ' || p;\n\
         end $$;\n",
        5,
    ),
    (
        "SUPABASE-FUNC-PRIVILEGED-NO-AUTHZ-1",
        CandidateLocator::SqlRoutines,
        "create table t (id int);\n\
         create function public.w(p int) returns void\n\
         language plpgsql security definer as $$\n\
         begin insert into t values (p); end $$;\n\
         grant execute on function public.w(int) to anon;\n",
        5,
    ),
    (
        "SUPABASE-FUNC-DEFINER-CROSS-TENANT-READ-1",
        CandidateLocator::SqlRoutines,
        "create table docs (org_id uuid, body text);\n\
         create function public.docs_for(p_org uuid) returns setof docs\n\
         language sql security definer\n\
         as $$ select * from docs where org_id = p_org $$;\n\
         grant execute on function public.docs_for(uuid) to authenticated;\n",
        4,
    ),
    // Policy rules: the policy statement itself.
    (
        "SUPABASE-RLS-PERMISSIVE-TRUE-1",
        CandidateLocator::SqlPolicies,
        "create table t (id int);\n\
         alter table t enable row level security;\n\
         create policy \"anyone\" on t for insert\n\
           to anon with check (true);\n",
        4,
    ),
    (
        "SUPABASE-RLS-INITPLAN-1",
        CandidateLocator::SqlPolicies,
        "create table t (id uuid, owner uuid);\n\
         create policy own on t for select\n\
           using (auth.uid() = owner);\n",
        3,
    ),
    // A policy reaches user_metadata through a helper: the helper body is a site too.
    (
        "SUPABASE-RLS-USER-METADATA-1",
        CandidateLocator::SqlPoliciesRoutinesAndViews,
        "create table t (id uuid);\n\
         create function public.is_admin() returns boolean language sql stable\n\
         as $$ select (auth.jwt() -> 'user_metadata' ->> 'role') = 'admin' $$;\n\
         create policy admins on t using (public.is_admin());\n",
        3,
    ),
    // Column rules: table, type and domain definitions.
    (
        "SQL-TIMESTAMPTZ-1",
        CandidateLocator::SqlTables,
        "create table events (\n  id bigint,\n  created_at timestamp\n);\n",
        3,
    ),
    (
        "SQL-MONEY-FLOAT-1",
        CandidateLocator::SqlTables,
        "create table invoices (id bigint);\n\
         alter table invoices add column amount double precision;\n",
        2,
    ),
    (
        "SQL-ENUM-TEXT-CONSTRAINT-1",
        CandidateLocator::SqlTables,
        "create table orders (\n  id bigint,\n  status text\n);\n",
        3,
    ),
    (
        "SQL-FK-CASCADE-FINANCIAL-1",
        CandidateLocator::SqlTables,
        "create table payments (\n  id bigint,\n  user_id uuid references users(id) on delete cascade\n);\n",
        3,
    ),
];

/// More violating shapes of the same rules, each of which the rule's locator must still find. The
/// first fixture of a rule is the plain `CREATE`/`ALTER` shape; these are the other ways the same
/// subject is written (a column type declared by a cast in a view, or by `SELECT ... INTO`; a
/// claim read through a view a policy selects from).
const EXTRA_SHAPES: &[(&str, CandidateLocator, &str, usize)] = &[
    (
        "SQL-TIMESTAMPTZ-1",
        CandidateLocator::SqlTables,
        "insert into events values (1);\n\
         select id, created_at::timestamp as created_at into events_archive from events;\n",
        2,
    ),
    (
        "SQL-TIMESTAMPTZ-1",
        CandidateLocator::SqlTables,
        "create view recent as\n  select created_at::timestamp as created_at from events;\n",
        2,
    ),
    (
        "SQL-MONEY-FLOAT-1",
        CandidateLocator::SqlTables,
        "create materialized view revenue as\n  select sum(amount)::double precision as total from invoices;\n",
        2,
    ),
    (
        "SQL-MONEY-FLOAT-1",
        CandidateLocator::SqlTables,
        "insert into invoices values (1, 2.5);\n\
         select id, amount::float8 as amount into invoice_copy from invoices;\n",
        2,
    ),
    (
        "SQL-ENUM-TEXT-CONSTRAINT-1",
        CandidateLocator::SqlTables,
        "create table orders (id bigint);\n\
         alter table orders add column status text;\n",
        2,
    ),
    (
        "SQL-FK-CASCADE-FINANCIAL-1",
        CandidateLocator::SqlTables,
        "create table sessions (id bigint);\n\
         alter table sessions add constraint s_pay foreign key (id)\n\
           references payments (id) on delete cascade;\n",
        3,
    ),
    // The role claim reaches the policy through a view, not through the policy's own text.
    (
        "SUPABASE-RLS-USER-METADATA-1",
        CandidateLocator::SqlPoliciesRoutinesAndViews,
        "create view me as\n  select (auth.jwt() -> 'user_metadata' ->> 'role') as role;\n\
         create policy admins on t using ((select role from me) = 'admin');\n",
        2,
    ),
    (
        "SUPABASE-FUNC-SEARCH-PATH-1",
        CandidateLocator::SqlRoutines,
        "alter table t add column a int;\n\
         create or replace function public.g() returns void\n\
         language plpgsql security definer as $$ begin end $$;\n",
        3,
    ),
];

async fn corpus() -> camerata_rules::RuleSet {
    let (set, errors) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(
        errors.is_empty(),
        "the corpus must load cleanly, so no candidates value is invalid: {errors:?}"
    );
    set
}

#[tokio::test]
async fn the_rules_with_candidates_are_exactly_the_reviewed_set_with_their_locators() {
    let set = corpus().await;
    let mut declared: Vec<(String, CandidateLocator)> = set
        .iter()
        .filter_map(|r| r.candidates.map(|c| (r.id.0.clone(), c.locator)))
        .collect();
    declared.sort();
    let mut reviewed: Vec<(String, CandidateLocator)> = REVIEWED
        .iter()
        .map(|(id, l, _, _)| ((*id).to_string(), *l))
        .collect();
    reviewed.sort();
    assert_eq!(
        declared, reviewed,
        "a rule gained, lost or changed `candidates`: review its recall risk, add a violating \
         fixture to REVIEWED, and run the per-rule fixture recall check before merging"
    );
}

#[tokio::test]
async fn every_reviewed_locator_still_finds_the_violating_line_of_its_fixture() {
    let set = corpus().await;
    for (id, locator, fixture, line) in REVIEWED.iter().chain(EXTRA_SHAPES) {
        let rule = audit_rules(&[((*id).to_string(), "d".to_string())], Some(&set))
            .pop()
            .unwrap();
        assert_eq!(
            rule.candidates.map(|c| c.locator),
            Some(*locator),
            "{id} must reach the audit carrying its locator"
        );
        let Ok(FileCandidates::Sites(sites)) = locate(
            *locator,
            "supabase/migrations/20240101_fixture.sql",
            fixture,
        ) else {
            panic!("{id}: the fixture must parse");
        };
        assert!(
            sites
                .iter()
                .any(|s| s.start_line <= *line && *line <= s.end_line),
            "{id}: the locator must return a site covering line {line}: {sites:?}"
        );
    }
}

#[test]
fn every_extra_shape_belongs_to_a_reviewed_rule_with_its_reviewed_locator() {
    for (id, locator, _, _) in EXTRA_SHAPES {
        assert!(
            REVIEWED.iter().any(|(r, l, _, _)| r == id && l == locator),
            "{id}: an extra shape must name a reviewed rule and its locator"
        );
    }
}

#[tokio::test]
async fn no_absence_or_cross_statement_rule_is_given_candidates() {
    // A chunk in which the locator finds no site sends the rule nothing. That is sound only for
    // a rule whose violation IS a statement the locator selects (the fixture test above pins it
    // per rule). A rule whose finding is that something is MISSING, or that only appears when
    // several statements are read together across the migration timeline, must keep whole files.
    const ABSENCE_RULES: &[&str] = &[
        "SUPABASE-RLS-ENABLED-1",
        "SUPABASE-RLS-NO-POLICY-1",
        "SUPABASE-RLS-POLICY-DISABLED-1",
        "SUPABASE-RLS-DISABLED-NOT-RESTORED-1",
        "SQL-AUDIT-COLUMNS-1",
        "SQL-DB-INDEX-1",
        "SQL-INVARIANTS-CONSTRAINTS-1",
        "SQL-SOFT-DELETE-PARTIAL-INDEX-1",
        "SQL-PRIVILEGED-WRITE-AUDIT-1",
    ];
    let set = corpus().await;
    for id in ABSENCE_RULES {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} missing from corpus"));
        assert!(
            rule.candidates.is_none(),
            "{id} reasons from what is absent or spread across statements; narrowing it to \
             candidate sites would hide the finding"
        );
    }
}
