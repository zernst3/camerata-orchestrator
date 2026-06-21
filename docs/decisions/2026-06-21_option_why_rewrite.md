# Option why rewrite: ruby, sql, testing corpora

**Date:** 2026-06-21
**Branch:** why/ruby-sql
**Author:** automated (Claude Sonnet 4.6, why/ruby-sql worktree)

## What was done

Rewrote placeholder `[[option]].why` values in the ruby, sql, and testing
principle directories. Placeholders were identified by the exact strings
"A defensible alternative the project considered." and "A defensible
alternative the project considered and did not adopt" (and minor variants)
appearing in option `why` fields.

Each replacement was derived from the option's own directive and the rule's
`[decision].why` paragraph, following the rule: 1-3 sentences explaining what
choosing this option means and its concrete trade-off, with no em-dashes.

## Options rewritten: 22

### ruby/ (2 options)

- `ruby-rails-strong-params-1.toml` : `permissive-params`
- `ruby-thin-controllers-1.toml` : `fat-controllers`

### sql/ (10 options)

- `arch-expand-contract-1.toml` : `breaking-schema-changes-drop-rename-not-null-add`
- `proc-ci-migration-hygiene-1.toml` : `the-project-relies-on-human-code-review-alone-to`
- `sql-audit-columns-1.toml` : `every-table-carries-the-same-four-column-audit-q`
- `sql-audit-columns-1.toml` : `no-table-in-the-schema-carries-any-audit-columns`
- `sql-audit-columns-1.toml` : `the-audit-columns-are-written-by-orm-level-lifec`
- `sql-db-index-1-fk-indexes.toml` : `the-project-relies-on-the-database-engine-to-aut`
- `sql-db-index-1-fk-indexes.toml` : `fk-indexes-are-added-reactively-in-later-migrati`
- `sql-db-index-1-fk-indexes.toml` : `fk-indexes-are-declared-in-a-follow-up-migration`
- `sql-db-index-2-where-indexes.toml` : `indexes-on-where-clause-order-by-and-join-key-co`
- `sql-db-nplusone-1.toml` : `code-fetches-a-list-of-rows-and-then-loops-over`

### testing/ (10 options)

- `testing-arrange-act-assert-1.toml` : `freeform-no-required-structure`
- `testing-as-documentation-1.toml` : `test-names-reflect-method-under-test`
- `testing-behavior-not-implementation-1.toml` : `whitebox-internal-assertions`
- `testing-deterministic-1.toml` : `retry-on-failure`
- `testing-deterministic-1.toml` : `quarantine-flaky`
- `testing-fast-unit-tests-1.toml` : `unit-tests-may-use-real-io`
- `testing-one-assertion-per-test-1.toml` : `no-assertion-count-constraint`
- `testing-one-assertion-per-test-1.toml` : `strict-one-physical-assert`
- `testing-pyramid-1.toml` : `inverted-pyramid-heavy-e2e`
- `testing-pyramid-1.toml` : `no-explicit-distribution-policy`

## Test result

`cargo test -p camerata-rules` : 54 passed, 0 failed.
