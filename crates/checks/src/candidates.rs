//! Candidate-site locators (token-efficiency rank 8): the deterministic half of a rule's
//! `candidates` block (`camerata_rules::Candidates`).
//!
//! A locator answers one question about one file: "where in this file can this rule's subject
//! sit?". The scan (`camerata-server`'s `candidates` module) then sends the rule those sites with
//! context instead of the whole file. Every locator here is a statement filter over the same SQL
//! statement splitter the Supabase checkers use ([`crate::supabase::splitter`]), so a site is a
//! whole top-level statement: a function definition arrives with its entire dollar-quoted body.
//!
//! # A locator never claims "nothing here" about a file it did not read
//!
//! The answer is three-valued, and the scan honours each value differently:
//!
//! * [`FileCandidates::Sites`]: the locator parsed the file; these are ALL its candidate sites
//!   (an empty list means it parsed the file and nothing in it can be the rule's subject).
//! * [`FileCandidates::NotRead`]: the file is not in the locator's language (a `.ts` file, a
//!   `supabase/config.toml`). The locator says NOTHING about it, so the scan carries it whole.
//! * `Err(`[`LocateError`]`)`: the file is in the locator's language but does not parse (a
//!   string, quoted identifier, dollar-quoted body or block comment left open at end of file, or
//!   a backslash before a quote outside an `E'...'` string, whose meaning depends on server
//!   settings), or the caller cancelled the work. Its statement boundaries are guesses, so the
//!   scan sends the rule whole files for the chunk.
//!
//! # Filters are wide on purpose
//!
//! A statement is a site when it MENTIONS the subject (a whole-word keyword match over the
//! statement text, string and body content included), not only when it looks wrong. A missed
//! site is a finding the model never sees; an extra site costs a few lines of prompt.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::supabase::splitter::{split_statements_strict_until, SplitError};
pub use camerata_rules::CandidateLocator;

/// One candidate site: 1-based, inclusive line bounds in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SiteRange {
    pub start_line: usize,
    pub end_line: usize,
}

/// What a locator found in one file. See the module doc for how each value is honoured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileCandidates {
    /// The file was parsed; these are every candidate site in it, in file order (possibly none).
    Sites(Vec<SiteRange>),
    /// The file is not in the locator's language; the locator says nothing about it.
    NotRead,
}

/// The file is in the locator's language and could not be parsed (or the work was cancelled).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LocateError(pub String);

/// Run `locator` over one file. Pure and panic-free (the splitter's hardening contract).
pub fn locate(
    locator: CandidateLocator,
    path: &str,
    content: &str,
) -> Result<FileCandidates, LocateError> {
    locate_until(locator, path, content, &AtomicBool::new(false))
}

/// [`locate`] that stops with an `Err` soon after `cancel` is raised (the splitter polls it every
/// few thousand characters and this loop polls it between statements), so a caller that stopped
/// waiting can stop the work too instead of leaving a thread spinning on a pathological file.
pub fn locate_until(
    locator: CandidateLocator,
    path: &str,
    content: &str,
    cancel: &AtomicBool,
) -> Result<FileCandidates, LocateError> {
    if !is_sql_path(path) {
        return Ok(FileCandidates::NotRead);
    }
    let statements = split_statements_strict_until(content, cancel).map_err(|problem| {
        LocateError(match problem {
            SplitError::Cancelled => format!("{path}: {problem}"),
            _ => format!("{path} does not parse as SQL: {problem}"),
        })
    })?;
    let mut sites = Vec::new();
    for (n, s) in statements.iter().enumerate() {
        if n % 64 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(LocateError(format!("{path}: {}", SplitError::Cancelled)));
        }
        if statement_matches(locator, &s.text) {
            sites.push(SiteRange {
                start_line: s.line,
                end_line: s.end_line.max(s.line),
            });
        }
    }
    Ok(FileCandidates::Sites(sites))
}

fn is_sql_path(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("sql"))
}

/// Does a (whitespace-normalized, comment-stripped) statement mention `locator`'s subject?
fn statement_matches(locator: CandidateLocator, text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    let words: Vec<&str> = upper
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    let routine = || {
        words.first() == Some(&"DO")
            || words.iter().any(|w| {
                w.starts_with("FUNCTION") || w.starts_with("PROCEDURE") || w.starts_with("ROUTINE")
            })
    };
    let policy = || words.iter().any(|w| w.starts_with("POLIC"));
    // A view statement (`CREATE [OR REPLACE] [MATERIALIZED] VIEW`, `ALTER VIEW`, `DROP VIEW`).
    let view = || words.iter().any(|w| w.starts_with("VIEW"));
    match locator {
        CandidateLocator::SqlRoutines => routine(),
        CandidateLocator::SqlPolicies => policy(),
        CandidateLocator::SqlPoliciesRoutinesAndViews => policy() || routine() || view(),
        // Everything that gives a value a column type: tables, types, domains, views (a view's
        // columns take the types of its select list casts), and `SELECT ... INTO new_table`
        // (which creates a table with no TABLE keyword). An INSERT or MERGE `INTO` only adds rows.
        CandidateLocator::SqlTables => {
            let into_creates_a_table = !matches!(words.first(), Some(&"INSERT") | Some(&"MERGE"))
                && words.contains(&"INTO");
            into_creates_a_table
                || view()
                || words.iter().any(|w| {
                    w.starts_with("TABLE") || matches!(*w, "TYPE" | "TYPES" | "DOMAIN" | "DOMAINS")
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATION: &str = "\
-- 1: table
create table public.notes (
  id uuid primary key,
  created_at timestamp
);
alter table public.notes enable row level security;
create policy \"own notes\" on public.notes
  using (auth.uid() = id);
create or replace function public.touch(p uuid)
returns void
language plpgsql
security definer
as $$
begin
  update public.notes set created_at = now() where id = p;
end;
$$;
grant execute on function public.touch(uuid) to anon;
insert into public.notes values (gen_random_uuid(), now());
do $$ begin perform 1; end $$;
create type public.status as enum ('a', 'b');
";

    fn sites(locator: CandidateLocator) -> Vec<(usize, usize)> {
        match locate(locator, "supabase/migrations/1.sql", MIGRATION).unwrap() {
            FileCandidates::Sites(s) => s.iter().map(|r| (r.start_line, r.end_line)).collect(),
            FileCandidates::NotRead => panic!("a .sql file is read"),
        }
    }

    #[test]
    fn routines_are_function_definitions_grants_and_do_blocks_with_their_whole_body() {
        assert_eq!(
            sites(CandidateLocator::SqlRoutines),
            vec![(9, 17), (18, 18), (20, 20)]
        );
    }

    #[test]
    fn policies_are_policy_statements() {
        assert_eq!(sites(CandidateLocator::SqlPolicies), vec![(7, 8)]);
    }

    #[test]
    fn policies_and_routines_is_the_union() {
        assert_eq!(
            sites(CandidateLocator::SqlPoliciesRoutinesAndViews),
            vec![(7, 8), (9, 17), (18, 18), (20, 20)]
        );
    }

    #[test]
    fn tables_are_table_type_and_domain_statements() {
        // The function updates a table without the word TABLE, so it is not a table site.
        assert_eq!(
            sites(CandidateLocator::SqlTables),
            vec![(2, 5), (6, 6), (21, 21)]
        );
    }

    #[test]
    fn a_file_outside_the_language_is_not_read_never_empty() {
        for path in [
            "supabase/config.toml",
            "src/db.ts",
            "Makefile",
            "schema.sql.md",
        ] {
            for locator in CandidateLocator::ALL {
                assert_eq!(
                    locate(*locator, path, "create function x() ..."),
                    Ok(FileCandidates::NotRead),
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn an_unparseable_sql_file_is_an_error_never_empty() {
        for broken in [
            "create function f() returns int as $$ select 1;",
            "insert into t values ('open;",
            "create table \"x (id int);",
            "/* never closed\ncreate policy p on t using (true);",
        ] {
            let err = locate(CandidateLocator::SqlPolicies, "m.sql", broken).unwrap_err();
            assert!(
                err.0.contains("m.sql") && err.0.contains("unterminated"),
                "{err}"
            );
        }
    }

    // ── negative space: every shape a rule's subject can take is a site ───────────────────────
    //
    // A locator that misses a shape hides every finding of that shape from the model, silently.
    // So for each locator these tables list the statement shapes that MUST be sites (the ways
    // the rule's subject is actually written) and the shapes that must NOT be (so a locator
    // cannot pass by matching everything). Adding a shape a rule can see means adding a line.

    fn is_site(locator: CandidateLocator, statement: &str) -> bool {
        let sql = format!("{statement};");
        match locate(locator, "m.sql", &sql).expect("the fixture parses") {
            FileCandidates::Sites(s) => !s.is_empty(),
            FileCandidates::NotRead => panic!("a .sql file is read"),
        }
    }

    fn check_shapes(locator: CandidateLocator, sites: &[&str], not_sites: &[&str]) {
        for s in sites {
            assert!(
                is_site(locator, s),
                "{} must find this statement:\n{s}",
                locator.name()
            );
        }
        for s in not_sites {
            assert!(
                !is_site(locator, s),
                "{} must NOT find this statement:\n{s}",
                locator.name()
            );
        }
    }

    const ROUTINE_SHAPES: &[&str] = &[
        "create function f() returns int language sql as $$ select 1 $$",
        "CREATE OR REPLACE FUNCTION public.f(p uuid) RETURNS void SECURITY DEFINER AS $$ begin end $$ LANGUAGE plpgsql",
        "create function \"my fn\"() returns int language sql as $$ select 1 $$",
        "create procedure p() language sql as $$ select 1 $$",
        "alter function f() set search_path = public",
        "alter function f() owner to postgres",
        "alter procedure p() security invoker",
        "alter routine r() rename to r2",
        "drop function if exists f()",
        "comment on function f() is 'x'",
        "grant execute on function f() to anon",
        "grant execute on all functions in schema public to anon",
        "grant execute on procedure p() to anon",
        "grant execute on routine r() to anon",
        "revoke execute on function f() from public",
        "alter default privileges in schema public grant execute on functions to anon",
        "alter default privileges grant execute on routines to anon",
        "create trigger tr after insert on t for each row execute function f()",
        "create trigger tr after insert on t for each row execute procedure f()",
        "create event trigger e on ddl_command_end execute function f()",
        "do $$ begin perform 1; end $$",
        "DO LANGUAGE plpgsql $$ begin execute 'create function g() returns int as $f$ select 1 $f$ language sql'; end $$",
        "create schema s create function f() returns int language sql as $$ select 1 $$",
    ];

    const POLICY_SHAPES: &[&str] = &[
        "create policy p on t using (true)",
        "create policy \"own rows\" on t for select using (auth.uid() = user_id)",
        "CREATE POLICY p ON t AS RESTRICTIVE FOR ALL TO anon USING (true) WITH CHECK (true)",
        "create policy p on storage.objects for insert with check (bucket_id = 'a')",
        "create policy p on t using (public.is_member(org_id))",
        "alter policy p on t using (true)",
        "drop policy p on t",
        "drop policy if exists p on t",
        "do $$ begin execute 'create policy p on t using (true)'; end $$",
        "create function f() returns void language plpgsql as $$ begin create policy p on t using (true); end $$",
    ];

    const VIEW_SHAPES: &[&str] = &[
        "create view me as select (auth.jwt()->'user_metadata'->>'role') as role",
        "create or replace view me as select raw_user_meta_data as meta from auth.users",
        "create view me with (security_invoker = true) as select 1 as one",
        "create materialized view mv as select raw_user_meta_data from auth.users",
        "alter view me set (security_invoker = true)",
        "alter materialized view mv rename to mv2",
        "drop view if exists me",
        "create schema s create view v as select 1 as one",
    ];

    const TABLE_SHAPES: &[&str] = &[
        "create table t (id int, created_at timestamp)",
        "create temp table t (id int, amount real)",
        "create unlogged table t (a float8)",
        "create foreign table t (a timestamp) server s",
        "create table t as select now()::timestamp as created_at",
        "create table t (like other including all)",
        "create table t (a int references b (id) on delete cascade)",
        "alter table t add column amount double precision",
        "alter table t alter column created_at type timestamp",
        "alter table t add constraint fk foreign key (a) references b (id) on delete cascade",
        "alter table only t add constraint c check (status in ('a', 'b'))",
        "create type status as enum ('a', 'b')",
        "create type money_pair as (amount double precision, at timestamp)",
        "alter type status add value 'c'",
        "create domain money_amount as double precision",
        "alter domain money_amount set not null",
        // A view's columns take the type of the select list: a cast there declares a column type.
        "create view v as select sum(amount)::double precision as total from t",
        "create or replace view v as select created_at::timestamp as at from t",
        "create materialized view mv as select sum(amount)::double precision as total from t",
        "alter view v alter column total set default 0",
        "alter materialized view mv rename to mv2",
        // `SELECT ... INTO` creates a table; no TABLE, TYPE, DOMAIN or VIEW word appears.
        "select id, created_at::timestamp as at into events_archive from events",
        "select * into temp scratch from events",
        "with moved as (delete from events returning *) select * into archive from moved",
        "create function f() returns table (at timestamp) language sql as $$ select now()::timestamp $$",
        "create function f() returns void language plpgsql as $$ begin create temp table scratch (amount real); end $$",
        "do $$ begin execute 'create table t (at timestamp)'; end $$",
        "create schema s create table t (at timestamp)",
        "CREATE   TABLE\n  t(a int)",
    ];

    /// Statements no locator should send: they carry rows, grants or settings, never the subject
    /// of a rule that locates policies, routines, views or column types.
    const INERT_SHAPES: &[&str] = &[
        "insert into t values (1, now())",
        "insert into t select created_at::timestamp from s",
        "merge into t using s on t.id = s.id when matched then update set a = 1",
        "update t set a = 1",
        "delete from t",
        "copy t from stdin",
        "create index i on t (a)",
        "comment on column t.a is 'x'",
        "set search_path = public",
        "select 1",
        "create extension pgcrypto",
        "create sequence s",
        "create role r",
        "revoke all on t from anon",
    ];

    #[test]
    fn routines_find_every_way_a_routine_is_defined_changed_or_granted() {
        let mut not_sites: Vec<&str> = INERT_SHAPES.to_vec();
        not_sites.extend([
            "create table t (a int)",
            "create type status as enum ('a')",
            "create policy p on t using (public.is_member(org_id))",
            "grant select on t to anon",
            "create view v as select 1 as one",
        ]);
        check_shapes(CandidateLocator::SqlRoutines, ROUTINE_SHAPES, &not_sites);
    }

    #[test]
    fn policies_find_every_way_a_policy_is_written() {
        let mut not_sites: Vec<&str> = INERT_SHAPES.to_vec();
        not_sites.extend([
            "create table t (a int)",
            "alter table t enable row level security",
            "create function f() returns int language sql as $$ select 1 $$",
            "grant select on t to anon",
            "create view v as select 1 as one",
        ]);
        check_shapes(CandidateLocator::SqlPolicies, POLICY_SHAPES, &not_sites);
    }

    #[test]
    fn policies_routines_and_views_find_the_union_and_every_view_statement() {
        let mut sites: Vec<&str> = POLICY_SHAPES.to_vec();
        sites.extend(ROUTINE_SHAPES);
        sites.extend(VIEW_SHAPES);
        let mut not_sites: Vec<&str> = INERT_SHAPES.to_vec();
        not_sites.extend([
            "create table t (a int)",
            "create type status as enum ('a')",
            "grant select on t to anon",
        ]);
        check_shapes(
            CandidateLocator::SqlPoliciesRoutinesAndViews,
            &sites,
            &not_sites,
        );
    }

    #[test]
    fn tables_find_every_way_a_column_type_is_declared() {
        let mut not_sites: Vec<&str> = INERT_SHAPES.to_vec();
        not_sites.extend([
            "create policy p on t using (true)",
            "create trigger tr before insert on t for each row execute function f()",
            "create function f() returns int language sql as $$ select 1 $$",
            "alter function f() set search_path = public",
        ]);
        check_shapes(CandidateLocator::SqlTables, TABLE_SHAPES, &not_sites);
    }

    #[test]
    fn a_view_that_a_policy_reads_is_a_site_of_the_user_metadata_locator() {
        // The role claim reaches the policy through the view, not through the policy's own text.
        let sql = "\
create view me as select (auth.jwt()->'user_metadata'->>'role') as role;
create policy admin_only on t using ((select role from me) = 'admin');
insert into t values (1);
";
        let found = locate(CandidateLocator::SqlPoliciesRoutinesAndViews, "m.sql", sql);
        let narrower = locate(CandidateLocator::SqlPolicies, "m.sql", sql);
        let lines = |r: Result<FileCandidates, LocateError>| match r.unwrap() {
            FileCandidates::Sites(s) => s.iter().map(|r| r.start_line).collect::<Vec<_>>(),
            FileCandidates::NotRead => panic!("read"),
        };
        assert_eq!(lines(found), vec![1, 2]);
        assert_eq!(
            lines(narrower),
            vec![2],
            "a policy-only locator would miss the view"
        );
    }

    #[test]
    fn an_escape_string_with_an_escaped_quote_keeps_the_statements_after_it_apart() {
        let sql = "insert into t values (E'it\\'s; fine');\ncreate function f() returns int language sql as $$ select 1 $$;\n";
        assert_eq!(
            locate(CandidateLocator::SqlRoutines, "m.sql", sql),
            Ok(FileCandidates::Sites(vec![SiteRange {
                start_line: 2,
                end_line: 2
            }]))
        );
    }

    #[test]
    fn a_backslash_before_a_quote_in_a_standard_string_falls_back_never_guesses() {
        // Under `standard_conforming_strings = off` this is one string; under the default the
        // quote ends it and the rest is another string. Either way the boundaries are a guess.
        let sql = "insert into t values ('it\\'s');\ncreate function f() returns int language sql as $$ select 1 $$;\n";
        let err = locate(CandidateLocator::SqlRoutines, "m.sql", sql).unwrap_err();
        assert!(
            err.0.contains("m.sql") && err.0.contains("ambiguous backslash"),
            "{err}"
        );
    }

    #[test]
    fn a_raised_cancel_flag_stops_a_locate() {
        let big = "insert into t values (1);\n".repeat(20_000);
        let cancel = AtomicBool::new(true);
        let err = locate_until(CandidateLocator::SqlTables, "big.sql", &big, &cancel).unwrap_err();
        assert!(err.0.contains("cancelled"), "{err}");
        // A non-SQL file never reads its content, cancelled or not.
        assert_eq!(
            locate_until(CandidateLocator::SqlTables, "a.ts", &big, &cancel),
            Ok(FileCandidates::NotRead)
        );
    }

    #[test]
    fn an_sql_file_with_no_subject_parses_to_zero_sites() {
        assert_eq!(
            locate(
                CandidateLocator::SqlRoutines,
                "SEED.SQL",
                "insert into t values (1);"
            ),
            Ok(FileCandidates::Sites(Vec::new()))
        );
    }
}
