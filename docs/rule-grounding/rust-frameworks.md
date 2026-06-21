# Rust Frameworks Grounding Report

**Family:** `rust/{seaorm,dioxus,axum,tokio,sqlx}`
**Date:** 2026-06-20
**Agent:** Claude Sonnet 4.6 (ground/rust-frameworks)

## Summary

| Status | Count |
|--------|-------|
| Grounded | 38 |
| Ungrounded (left as draft) | 5 |
| Demoted (mechanical → prose) | 0 |
| **Total in family** | **43** |

All 43 rules use `enforcement = "structured"` — no mechanical rules exist in this family, so no demotions were required.

---

## Ungrounded Rules

These rules assert project-level conventions with no single authoritative external source to cite. They remain `draft`.

- **RUST-DIOXUS-1** — UI crate file-role layout (views/, components/, hooks/, contexts/, services/). This is a project-level structural convention. Dioxus docs discuss component types but do not mandate this specific directory naming scheme.
- **RUST-DIOXUS-10** — Auth `_can` flags on responses; raw permission codes never reach the client. This is a cross-cutting product architecture decision documented internally; no Dioxus official doc covers this pattern.
- **RUST-DIOXUS-12** — Icons render as inline SVG from static string constants. This is a performance/bundle-size convention; no authoritative Dioxus doc mandates inline SVG specifically.
- **RUST-DIOXUS-13** — Forms display errors from domain newtype constructors; UI never re-implements validation rules. This is a domain-driven design convention; no Dioxus official doc covers this integration pattern.
- **RUST-DIOXUS-14** — Primitives-first UI layer; views compose shared primitives. This is a component-architecture convention; no Dioxus official doc mandates a primitives layer.

## Demoted Rules

None. All rules in the family use `enforcement = "structured"`, which does not require a real linter rule.

---

## Full Rule Table

| rule-id | verification | source url | grounded/ungrounded |
|---------|-------------|-----------|---------------------|
| RUST-AXUM-EXTRACTORS-VALIDATE-1 | grounded | https://docs.rs/axum/latest/axum/extract/index.html | grounded |
| RUST-AXUM-HANDLERS-THIN-DELEGATE-1 | grounded | https://docs.rs/axum/latest/axum/ | grounded |
| RUST-AXUM-MIDDLEWARE-TOWER-1 | grounded | https://docs.rs/axum/latest/axum/middleware/index.html | grounded |
| RUST-AXUM-STATE-SHARED-DEPS-1 | grounded | https://docs.rs/axum/latest/axum/extract/struct.State.html | grounded |
| RUST-AXUM-TIMEOUT-LIMITS-1 | grounded | https://docs.rs/axum/latest/axum/extract/struct.DefaultBodyLimit.html | grounded |
| RUST-AXUM-TYPED-ERROR-RESPONSE-1 | grounded | https://docs.rs/axum/latest/axum/response/index.html | grounded |
| RUST-DIOXUS-1 | draft | — | ungrounded |
| RUST-DIOXUS-10 | draft | — | ungrounded |
| RUST-DIOXUS-11 | grounded | https://dioxuslabs.com/learn/0.6/guides/fullstack/ | grounded |
| RUST-DIOXUS-12 | draft | — | ungrounded |
| RUST-DIOXUS-13 | draft | — | ungrounded |
| RUST-DIOXUS-14 | draft | — | ungrounded |
| RUST-DIOXUS-2 | grounded | https://dioxuslabs.com/learn/0.6/reference/components | grounded |
| RUST-DIOXUS-3 | grounded | https://dioxuslabs.com/learn/0.6/essentials/state/ | grounded |
| RUST-DIOXUS-4 | grounded | https://dioxuslabs.com/learn/0.6/reference/context | grounded |
| RUST-DIOXUS-5 | grounded | https://docs.rs/dioxus/latest/dioxus/prelude/fn.use_effect.html | grounded |
| RUST-DIOXUS-6 | grounded | https://dioxuslabs.com/learn/0.6/essentials/async/ | grounded |
| RUST-DIOXUS-7 | grounded | https://dioxuslabs.com/learn/0.6/reference/event_handlers | grounded |
| RUST-DIOXUS-8 | grounded | https://dioxuslabs.com/learn/0.6/reference/rsx | grounded |
| RUST-DIOXUS-9 | grounded | https://dioxuslabs.com/learn/0.6/guides/fullstack/server_functions | grounded |
| RUST-ENTITIES-1 | grounded | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveEntityModel.html | grounded |
| RUST-ENTITIES-2 | grounded | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveEntityModel.html | grounded |
| RUST-ENTITIES-3 | grounded | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ | grounded |
| RUST-ENTITIES-4 | grounded | https://www.sea-ql.org/SeaORM/docs/relation/one-to-many/ | grounded |
| RUST-ENTITIES-5 | grounded | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ | grounded |
| RUST-ENTITIES-6 | grounded | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ | grounded |
| RUST-ENTITIES-7 | grounded | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ | grounded |
| RUST-ENTITIES-9 | grounded | https://www.sea-ql.org/SeaORM/docs/relation/complex-relations/ | grounded |
| RUST-ENTITIES-10 | grounded | https://www.sea-ql.org/SeaORM/docs/relation/one-to-many/ | grounded |
| RUST-ENTITIES-11 | grounded | https://www.sea-ql.org/SeaORM/docs/relation/complex-relations/ | grounded |
| RUST-ENTITIES-12 | grounded | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveActiveEnum.html | grounded |
| RUST-ENTITIES-13 | grounded | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveEntityModel.html | grounded |
| RUST-SEAORM-INTRA-AGGREGATE-TX-1 | grounded | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ | grounded |
| RUST-SEAORM-RAW-SQL-ESCAPE-1 | grounded | https://docs.rs/sea-orm/latest/sea_orm/ | grounded |
| RUST-SQLX-COMPILE-CHECKED-QUERIES-1 | grounded | https://docs.rs/sqlx/latest/sqlx/macro.query.html | grounded |
| RUST-SQLX-CONNECTION-POOL-SIZED-1 | grounded | https://docs.rs/sqlx/latest/sqlx/pool/struct.Pool.html | grounded |
| RUST-SQLX-MIGRATIONS-CHECKED-IN-1 | grounded | https://docs.rs/sqlx/latest/sqlx/macro.migrate.html | grounded |
| RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1 | grounded | https://docs.rs/sqlx/latest/sqlx/struct.Transaction.html | grounded |
| RUST-TOKIO-BOUNDED-CHANNELS-1 | grounded | https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html | grounded |
| RUST-TOKIO-CANCELLATION-SAFE-SELECTS-1 | grounded | https://docs.rs/tokio/latest/tokio/macro.select.html | grounded |
| RUST-TOKIO-NO-BLOCKING-IN-ASYNC-1 | grounded | https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html | grounded |
| RUST-TOKIO-NO-STD-MUTEX-ACROSS-AWAIT-1 | grounded | https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html | grounded |
| RUST-TOKIO-STRUCTURED-CONCURRENCY-JOINSET-1 | grounded | https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html | grounded |

---

## Authorities Used

The following authoritative sources were consulted during grounding:

- **docs.rs/axum** — axum::extract, axum::response, axum::middleware, axum::Router, axum::extract::State, axum::extract::DefaultBodyLimit
- **docs.rs/tower-http** — tower_http::timeout
- **tokio.rs/tokio/tutorial** — Channels, Shared State, Select, Spawning sections
- **docs.rs/tokio** — tokio::sync::mpsc, tokio::sync::Mutex, tokio::task::spawn_blocking, tokio::task::JoinSet, tokio::macro::select
- **sea-ql.org/SeaORM** — Entity Format, Enumeration, One-to-Many, One-to-One, Complex Relations, ActiveModelBehavior
- **docs.rs/sea-orm** — DeriveEntityModel, DeriveActiveEnum
- **docs.rs/sqlx** — sqlx::query!, sqlx::Pool, sqlx::PoolOptions, sqlx::Transaction, sqlx::migrate!
- **github.com/launchbadge/sqlx** — SQLx README (compile-time checking)
- **dioxuslabs.com/learn/0.6** — Components, Hooks, Context, State/Signals, Async, RSX, Event Handlers, Server Functions, Fullstack
- **docs.rs/dioxus** — use_effect

## Key Grounding Notes

**RUST-AXUM-EXTRACTORS-VALIDATE-1**: The axum::extract docs explicitly state extractors implement `FromRequest`/`FromRequestParts` to pick apart incoming requests; the docs show a `ValidatedBody` extractor example for validation.

**RUST-AXUM-MIDDLEWARE-TOWER-1**: The middleware docs state "axum doesn't have its own bespoke middleware system and instead integrates with tower."

**RUST-AXUM-TIMEOUT-LIMITS-1**: DefaultBodyLimit doc confirms 2MB default body limit; tower-http::timeout doc confirms timeout middleware pattern.

**RUST-TOKIO-NO-STD-MUTEX-ACROSS-AWAIT-1**: tokio::sync::Mutex docs state the key feature is "the ability to keep it locked across an .await point," but recommend std::sync::Mutex in many cases. The Tokio tutorial recommends restructuring before reaching for tokio::Mutex. The rule's directive (use tokio::Mutex when you must hold across await) matches the docs.

**RUST-ENTITIES-5 (ActiveModelBehavior)**: The SeaORM Entity Format docs explicitly state: "Do not delete the ActiveModelBehavior impl block even if it is empty." This is the exact claim the rule makes.

**RUST-TOKIO-CANCELLATION-SAFE-SELECTS-1**: The tokio::select! macro docs explicitly define cancellation safety and warn about `Mutex::lock` being not cancellation safe.
