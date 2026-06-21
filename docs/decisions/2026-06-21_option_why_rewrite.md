# 2026-06-21 Option why rewrite: rust principles corpus

## What changed

Rewrote 119 placeholder `why` values across every `.toml` file under
`crates/rules/principles/rust/`. Every placeholder matched one of these
patterns:

- Exactly: `"A defensible alternative the project considered."`
- Beginning with that phrase, possibly with trailing content

All 119 options now carry a real trade-off sentence or two derived from the
`[decision].why` paragraph in the same file, or from the option's own
directive where the decision paragraph did not cover it.

## Files touched

- `rust-domain-1-single-domain-crate.toml` (2 options)
- `rust-domain-2-newtype-ids.toml` (2 options)
- `rust-domain-3-newtype-validated-strings.toml` (3 options)
- `rust-domain-4-errors-via-thiserror.toml` (2 options)
- `rust-domain-5-async.toml` (2 options)
- `rust-domain-6-shared-repository-error.toml` (4 options)
- `rust-domain-7-explicit-uow.toml` (3 options)
- `rust-mapper-1-mappers-own-crate.toml` (2 options)
- `rust-no-unwrap-1.toml` (2 options)
- `axum/rust-axum-typed-error-response-1.toml` (1 option)
- `axum/rust-axum-extractors-validate-1.toml` (1 option)
- `axum/rust-axum-middleware-tower-1.toml` (1 option)
- `axum/rust-axum-timeout-limits-1.toml` (1 option)
- `axum/rust-axum-state-shared-deps-1.toml` (1 option)
- `axum/rust-axum-handlers-thin-delegate-1.toml` (1 option)
- `tokio/rust-tokio-structured-concurrency-joinset-1.toml` (1 option)
- `tokio/rust-tokio-cancellation-safe-selects-1.toml` (1 option)
- `tokio/rust-tokio-no-blocking-in-async-1.toml` (1 option)
- `tokio/rust-tokio-no-std-mutex-across-await-1.toml` (1 option)
- `tokio/rust-tokio-bounded-channels-1.toml` (1 option)
- `dioxus/rust-dioxus-1-file-structure.toml` (3 options)
- `dioxus/rust-dioxus-2-functional-components.toml` (2 options)
- `dioxus/rust-dioxus-3-state-use-signal.toml` (3 options)
- `dioxus/rust-dioxus-4-context-providers.toml` (3 options)
- `dioxus/rust-dioxus-5-effects.toml` (3 options)
- `dioxus/rust-dioxus-6-async-resources.toml` (3 options)
- `dioxus/rust-dioxus-7-event-handlers.toml` (3 options)
- `dioxus/rust-dioxus-8-rsx-patterns.toml` (3 options)
- `dioxus/rust-dioxus-9-server-functions.toml` (3 options)
- `dioxus/rust-dioxus-11-fullstack-ssr.toml` (3 options)
- `dioxus/rust-dioxus-12-svg-icons-inline.toml` (3 options)
- `dioxus/rust-dioxus-13-forms-newtype-validation.toml` (3 options)
- `dioxus/rust-dioxus-14-primitives-first.toml` (3 options)
- `seaorm/rust-entities-1-deriveentitymodel.toml` (3 options)
- `seaorm/rust-entities-3-flat-entity-files.toml` (3 options)
- `seaorm/rust-entities-4-fks-on-both-sides.toml` (3 options)
- `seaorm/rust-entities-5-empty-activemodelbehavior.toml` (2 options)
- `seaorm/rust-entities-6-cite-schema-source.toml` (3 options)
- `seaorm/rust-entities-7-composite-unique-at-db.toml` (3 options)
- `seaorm/rust-entities-9-self-referential-fks.toml` (2 options)
- `seaorm/rust-entities-10-owner-vs-audit-fks.toml` (2 options)
- `seaorm/rust-entities-11-multi-business-fks.toml` (3 options)
- `seaorm/rust-entities-12-pgenum-as-typed-enum.toml` (2 options)
- `seaorm/rust-entities-13-entities-separate-crate.toml` (2 options)
- `seaorm/rust-seaorm-entities-2-no-serde.toml` (2 options)
- `sqlx/rust-sqlx-compile-checked-queries-1.toml` (1 option)
- `sqlx/rust-sqlx-migrations-checked-in-1.toml` (1 option)
- `sqlx/rust-sqlx-connection-pool-sized-1.toml` (1 option)
- `sqlx/rust-sqlx-transactions-multi-write-1.toml` (1 option)
- `testing/rust-testing-1-unit-test-location.toml` (2 options)
- `testing/rust-testing-2-integration-test-location.toml` (2 options)
- `testing/rust-testing-3-naming.toml` (2 options)
- `testing/rust-testing-4-aaa-structure.toml` (2 options)
- `testing/rust-testing-5-mock-at-boundaries.toml` (2 options)
- `testing/rust-testing-6-determinism.toml` (2 options)
- `testing/rust-testing-7-doc-tests.toml` (2 options)

Total: 119 options rewritten across 55 files.

## What was left alone

All `[decision].why` fields and all `[[option]].why` fields that already
contained real trade-off content were left exactly as written.

## Test result

`cargo test -p camerata-rules`: 54 tests pass, 0 fail.
