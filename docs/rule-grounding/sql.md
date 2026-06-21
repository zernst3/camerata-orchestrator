# SQL Rule-Family Grounding Report

Generated: 2026-06-20
Branch: ground/sql
Family: `crates/rules/principles/sql/`

---

## Summary

| Category | Count |
|---|---|
| Total rules | 6 |
| Grounded | 5 |
| Ungrounded | 1 |
| Demoted (mechanical -> prose) | 4 |

---

## Ungrounded Rules

- **SQL-AUDIT-COLUMNS-1** — The three-strategy audit-column pattern (admin-curated / user-generated / system-managed) is the project's own design convention. No authoritative style guide or SQL standard names this three-way split. The SQL Style Guide covers `_date` suffix naming but not audit-column strategy classification. Left as draft / ungrounded.

## Demoted Rules (mechanical -> prose)

- **PROC-CI-MIGRATION-HYGIENE-1** — The combined requirement (IF NOT EXISTS + destructive-op opt-in + journal-collision check) is not implemented by any single standard linter rule. SQLFluff PG01 covers lock-safety for FK and index DDL, but not idempotency guards or destructive-op markers. Demoted to prose.
- **SQL-DB-INDEX-1** — No standard SQLFluff or other published linter rule checks that every FK-declaring migration also adds an index on the referencing column. The mechanism in the qualifies field is a custom migration linter, not a standard tool rule. Demoted to prose.
- **SQL-DB-INDEX-2** — No standard linter rule enforces that every WHERE/ORDER BY/JOIN column has a supporting index. The qualifies field itself calls this a "Prose check." Demoted to prose.
- **SQL-DB-NPLUSONE-1** — No static linter rule detects N+1 query patterns in SQL or ORM code at lint time. The qualifies field itself calls this a "Prose check." Demoted to prose.

---

## Full Citation Table

| Rule ID | Verification | Source URL | Linter Rule | Status |
|---|---|---|---|---|
| ARCH-EXPAND-CONTRACT-1 | grounded | https://martinfowler.com/bliki/ParallelChange.html | — | grounded |
| ARCH-EXPAND-CONTRACT-1 | grounded | https://martinfowler.com/articles/evodb.html | — | grounded |
| PROC-CI-MIGRATION-HYGIENE-1 | grounded | https://www.postgresql.org/docs/current/sql-createtable.html | — | demoted + grounded |
| PROC-CI-MIGRATION-HYGIENE-1 | grounded | https://docs.sqlfluff.com/en/stable/reference/rules.html#sqlfluff.rules.sphinx.Rule_PG01 | sqlfluff: PG01 | demoted + grounded |
| SQL-AUDIT-COLUMNS-1 | — | — | — | ungrounded |
| SQL-DB-INDEX-1 | grounded | https://www.postgresql.org/docs/current/ddl-constraints.html | — | demoted + grounded |
| SQL-DB-INDEX-1 | grounded | https://www.postgresql.org/docs/current/indexes-intro.html | — | demoted + grounded |
| SQL-DB-INDEX-2 | grounded | https://www.postgresql.org/docs/current/indexes-intro.html | — | demoted + grounded |
| SQL-DB-INDEX-2 | grounded | https://www.postgresql.org/docs/current/pgstatstatements.html | — | demoted + grounded |
| SQL-DB-INDEX-2 | grounded | https://www.postgresql.org/docs/current/using-explain.html | — | demoted + grounded |
| SQL-DB-NPLUSONE-1 | grounded | https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations | — | demoted + grounded |
| SQL-DB-NPLUSONE-1 | grounded | https://docs.djangoproject.com/en/5.2/topics/db/optimization/ | — | demoted + grounded |

---

## Authorities Consulted

All URLs were actually retrieved during this grounding pass.

- **Martin Fowler's bliki** — https://martinfowler.com/bliki/ParallelChange.html (Parallel Change pattern, Danilo Sato, 2014)
- **Martin Fowler / Pramod Sadalage** — https://martinfowler.com/articles/evodb.html (Evolutionary Database Design — Transition Phase for breaking schema changes)
- **PostgreSQL docs — DDL Constraints** — https://www.postgresql.org/docs/current/ddl-constraints.html (section 5.5.5 Foreign Keys: explicit statement that FK referencing columns are NOT auto-indexed)
- **PostgreSQL docs — Indexes Introduction** — https://www.postgresql.org/docs/current/indexes-intro.html (WHERE-clause and JOIN-condition columns benefit from indexes)
- **PostgreSQL docs — CREATE TABLE** — https://www.postgresql.org/docs/current/sql-createtable.html (IF NOT EXISTS: idempotency semantics for DDL)
- **PostgreSQL docs — pg_stat_statements** — https://www.postgresql.org/docs/current/pgstatstatements.html (query performance monitoring for slow-query detection)
- **PostgreSQL docs — EXPLAIN** — https://www.postgresql.org/docs/current/using-explain.html (detecting sequential scans on columns that should be index-backed)
- **SQLFluff rules reference** — https://docs.sqlfluff.com/en/stable/reference/rules.html (PG01: postgres.excessive_locks — covers ADD CONSTRAINT FOREIGN KEY NOT VALID and CREATE INDEX CONCURRENTLY)
- **Rails Guides — Active Record Querying** — https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations (explicit "N+1 Queries Problem" section with includes/preload/eager_load solutions)
- **Django docs — Database Access Optimization** — https://docs.djangoproject.com/en/5.2/topics/db/optimization/ (query-in-loop / multiple-database-hits problem)
- **SQL Style Guide** — https://www.sqlstyle.guide/ (naming conventions: _date suffix, _id, _status; does NOT cover audit-column strategies or indexes)
