# Framework Corpus Addition: Rust (Axum, Tokio, sqlx) and JS (Vue, NestJS)

**Date:** 2026-06-20  
**Status:** IMPLEMENTED  
**Scope:** Camerata Orchestrator Rule Corpus

## Summary

Added five new framework rule corpora to the Camerata Orchestrator, extending coverage across backend (Rust async web and database) and frontend (Vue) ecosystems, plus a modern backend alternative (NestJS). Each framework received 6 rules, totaling 30 new rules across the corpus.

## Frameworks Added

### Rust Frameworks

#### 1. Rust Axum (`rust:axum`)
Located: `crates/rules/principles/rust/axum/`

Axum is a modular, type-safe web framework built on tower middleware. Rules address:

- **RUST-AXUM-EXTRACTORS-VALIDATE-1**: Use extractors for request validation; reject invalid requests at the boundary
- **RUST-AXUM-STATE-SHARED-DEPS-1**: Share application dependencies via Axum State, not global static variables
- **RUST-AXUM-MIDDLEWARE-TOWER-1**: Implement cross-cutting concerns as tower middleware, not inline in handlers
- **RUST-AXUM-TYPED-ERROR-RESPONSE-1**: Handlers return typed error types that implement IntoResponse
- **RUST-AXUM-HANDLERS-THIN-DELEGATE-1**: Handlers stay thin; business logic and data access live in services or repositories
- **RUST-AXUM-TIMEOUT-LIMITS-1**: Set request timeouts and body size limits to prevent resource exhaustion

**Key Patterns:**
- Type-driven extractors for validation
- State-based dependency injection (not globals)
- Tower middleware stack for cross-cutting concerns
- IntoResponse for typed error handling
- Thin handler delegation

#### 2. Rust Tokio (`rust:tokio`)
Located: `crates/rules/principles/rust/tokio/`

Tokio is the async runtime. Rules address concurrency and task safety:

- **RUST-TOKIO-NO-BLOCKING-IN-ASYNC-1**: Do not call blocking operations directly in async code; use tokio::task::spawn_blocking
- **RUST-TOKIO-CANCELLATION-SAFE-SELECTS-1**: Use tokio::select! safely; ensure selected branch leaves system in consistent state
- **RUST-TOKIO-BOUNDED-CHANNELS-1**: Use bounded channels to prevent memory exhaustion and apply backpressure
- **RUST-TOKIO-STRUCTURED-CONCURRENCY-JOINSET-1**: Use JoinSet for structured concurrency; track spawned tasks
- **RUST-TOKIO-NO-STD-MUTEX-ACROSS-AWAIT-1**: Do not hold std::sync::Mutex lock across await points; use tokio::sync::Mutex

**Key Patterns:**
- No blocking calls in async functions (spawn_blocking)
- Cancellation-safe select! reasoning
- Bounded channels for backpressure
- JoinSet for structured task management
- tokio::sync::Mutex (not std::sync::Mutex) across await points

#### 3. Rust sqlx (`rust:sqlx`)
Located: `crates/rules/principles/rust/sqlx/`

sqlx provides compile-time SQL checking. Rules address data access:

- **RUST-SQLX-COMPILE-CHECKED-QUERIES-1**: Use the sqlx::query! macro for compile-time SQL validation; never use format! to build SQL
- **RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1**: Wrap multiple writes in an explicit transaction to ensure atomicity
- **RUST-SQLX-CONNECTION-POOL-SIZED-1**: Use a connection pool with explicit size configuration and reuse connections
- **RUST-SQLX-MIGRATIONS-CHECKED-IN-1**: Check database migrations into version control; run migrations at startup

**Key Patterns:**
- sqlx::query! macro (compile-checked, never format! SQL)
- Explicit transactions for multi-write atomicity
- Sized connection pools (not per-query creation)
- Version-controlled migrations (run at startup)

### JavaScript Frameworks

#### 4. JavaScript Vue (`javascript:vue`)
Located: `crates/rules/principles/javascript/vue/`

Vue is a reactive frontend framework. Rules address component architecture:

- **JAVASCRIPT-VUE-COMPOSITION-SCRIPT-SETUP-1**: Use Composition API with <script setup> for component logic
- **JAVASCRIPT-VUE-PROPS-DOWN-EVENTS-UP-1**: Props flow down; events flow up; avoid direct parent-to-child method calls
- **JAVASCRIPT-VUE-NO-DIRECT-DOM-1**: Do not manipulate the DOM directly; declare desired state and let Vue render
- **JAVASCRIPT-VUE-COMPUTED-OVER-METHODS-1**: Use computed properties for derived state, not methods
- **JAVASCRIPT-VUE-SCOPED-STYLES-1**: Use scoped styles to encapsulate component styling and prevent CSS leaks
- **JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1**: Use Pinia stores to manage shared state; avoid component-level prop drilling

**Key Patterns:**
- Composition API + <script setup> (not Options API)
- Unidirectional data flow (props down, events up)
- Declarative rendering (no direct DOM manipulation)
- Computed properties for caching derived state
- Scoped CSS (not global component styles)
- Pinia stores for shared state (not prop drilling)

#### 5. JavaScript NestJS (`javascript:nest`)
Located: `crates/rules/principles/javascript/nest/`

NestJS is a modular backend framework with built-in DI. Rules address server architecture:

- **JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1**: Use NestJS modules and dependency injection to organize and inject services
- **JAVASCRIPT-NEST-DTOS-VALIDATION-PIPE-1**: Use DTOs with class-validator and ValidationPipe to validate incoming requests
- **JAVASCRIPT-NEST-GUARDS-FOR-AUTH-1**: Use guards for authentication and authorization checks
- **JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1**: Use interceptors for cross-cutting concerns (logging, tracing, response transformation)
- **JAVASCRIPT-NEST-THIN-CONTROLLERS-DELEGATE-1**: Controllers stay thin; business logic and data access live in services

**Key Patterns:**
- Modules + providers + DI (not manual instantiation)
- DTOs + ValidationPipe (not manual controller validation)
- Guards for auth (not controller-embedded checks)
- Interceptors for cross-cutting concerns
- Thin controllers delegating to services

## Architectural Alignment

All new rules follow Camerata Corpus TOML format:
- `domain` uses `<lang>:<framework>` convention (e.g., `rust:axum`, `javascript:vue`)
- `layer` is `framework` for framework-specific rules
- `enforcement` is `structured` (machine-checkable via linting)
- `default = true` (all rules are adopted by default)
- Each rule includes decision question, default why, and 1-2 non-default options with rationales

## Cross-Framework Patterns

Several patterns appear across multiple frameworks, reflecting architectural consensus:

| Pattern | Rust | JS |
|---------|------|-----|
| Thin handlers/controllers | Axum | NestJS, (Vue implicit) |
| Type-safe validation at boundary | Axum | NestJS (DTOs) |
| Dependency injection | Axum (State) | NestJS (Modules/DI) |
| Cross-cutting via middleware/interceptors | Axum (Tower) | NestJS (Interceptors), Vue implicit |
| Structured concurrency | Tokio (JoinSet) | NestJS implicit |
| Async-safe locking | Tokio | NestJS (async-await native) |
| Shared state management | sqlx (transactions) | Vue (Pinia) |
| No direct data layer access | Axum (services) | NestJS (services) |

## Testing

All 39 corpus tests pass:
```
cargo test -p camerata-rules --lib
test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured
```

Corpus loads without errors; all new TOML files parse and validate successfully.

## Implementation Notes

- New subdirectories created: `rust/axum`, `rust/tokio`, `rust/sqlx`, `javascript/vue`, `javascript/nest`
- No modifications to existing framework rules (SeaORM, Express, Dioxus, etc.)
- No Rust source files modified (corpus TOML only)
- All rules use standard TOML structure (id, title, tag, domain, layer, enforcement, [decision], [[option]] blocks)

## Next Steps

- Integrate new frameworks into Camerata scanning engine (rule routing by domain)
- Wire new domains into project configuration templates
- Update Camerata documentation and framework selector UI
- Consider additional rules per framework if specific patterns emerge during scanning
