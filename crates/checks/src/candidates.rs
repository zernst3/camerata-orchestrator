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
//!   string, quoted identifier, dollar-quoted body or block comment left open at end of file).
//!   Its statement boundaries are guesses, so the scan sends the rule whole files for the chunk.
//!
//! # Filters are wide on purpose
//!
//! A statement is a site when it MENTIONS the subject (a whole-word keyword match over the
//! statement text, string and body content included), not only when it looks wrong. A missed
//! site is a finding the model never sees; an extra site costs a few lines of prompt.

use crate::supabase::splitter::split_statements_strict;
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

/// The file is in the locator's language and could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LocateError(pub String);

/// Run `locator` over one file. Pure and panic-free (the splitter's hardening contract).
pub fn locate(
    locator: CandidateLocator,
    path: &str,
    content: &str,
) -> Result<FileCandidates, LocateError> {
    if !is_sql_path(path) {
        return Ok(FileCandidates::NotRead);
    }
    let statements = split_statements_strict(content)
        .map_err(|open| LocateError(format!("{path} does not parse as SQL: {open}")))?;
    Ok(FileCandidates::Sites(
        statements
            .iter()
            .filter(|s| statement_matches(locator, &s.text))
            .map(|s| SiteRange {
                start_line: s.line,
                end_line: s.end_line.max(s.line),
            })
            .collect(),
    ))
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
    match locator {
        CandidateLocator::SqlRoutines => routine(),
        CandidateLocator::SqlPolicies => policy(),
        CandidateLocator::SqlPoliciesAndRoutines => policy() || routine(),
        CandidateLocator::SqlTables => words.iter().any(|w| {
            w.starts_with("TABLE") || matches!(*w, "TYPE" | "TYPES" | "DOMAIN" | "DOMAINS")
        }),
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
            sites(CandidateLocator::SqlPoliciesAndRoutines),
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
