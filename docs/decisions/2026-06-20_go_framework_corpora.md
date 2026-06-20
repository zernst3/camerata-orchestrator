# Decision: Add Go Framework Corpora to Camerata Rules

Date: 2026-06-20  
Status: Implemented  
Scope: camerata-rules corpus, branch: fw/go-frameworks

## Summary

Added three Go framework rule corpora to the camerata-rules crate:
- **go:web** (7 rules) — HTTP/REST frameworks (net/http, Gin, Echo)
- **go:grpc** (5 rules) — gRPC service frameworks
- **go:gorm** (5 rules) — GORM database ORM

Total: 17 new framework-specific rules, maintaining consistency with existing JavaScript (Express, Next.js) and Rust (SeaORM, Dioxus) framework patterns.

## Frameworks and Rules

### go:web (HTTP/REST)
Domain: `go:web`  
Framework context: net/http, Gin, Echo, chi

Rules:
1. **GO-WEB-MIDDLEWARE-CROSS-CUTTING-1**: Cross-cutting concerns (logging, auth, timeouts, recovery) as middleware, not in handlers.
2. **GO-WEB-REQUEST-BINDING-VALIDATION-1**: Typed request structs with validation tags; no raw map access.
3. **GO-WEB-CONTEXT-PROPAGATION-1**: Request context flows through all calls; deadlines and cancellation respected.
4. **GO-WEB-THIN-HANDLERS-DELEGATION-1**: Handlers bind, delegate to services; business logic not in handlers.
5. **GO-WEB-STRUCTURED-ERROR-RESPONSES-1**: Consistent error response format (code, message, status); not ad-hoc.
6. **GO-WEB-GRACEFUL-SHUTDOWN-1**: Graceful shutdown with timeout; in-flight requests allowed to complete.
7. **GO-WEB-HANDLER-TIMEOUTS-1**: Per-route/per-group timeouts; long-running ops use background jobs.

Enforcement tiers:
- Structured: middleware, binding, context, thin-handlers, error-responses
- Prose: graceful-shutdown, handler-timeouts

### go:grpc (gRPC Services)
Domain: `go:grpc`  
Framework context: Google's grpc-go library, protocol buffers

Rules:
1. **GO-GRPC-CONTEXT-DEADLINES-1**: Context deadlines honored; outbound RPCs propagate caller context, no background-context escapes.
2. **GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1**: Auth, logging, error recovery as interceptors; not re-implemented per handler.
3. **GO-GRPC-PROTO-GENERATED-IMMUTABLE-1**: Proto-generated code is immutable; changes via .proto → protoc regeneration.
4. **GO-GRPC-STATUS-CODES-CORRECT-1**: Status codes semantically correct (NOT_FOUND, INVALID_ARGUMENT, PERMISSION_DENIED, INTERNAL, etc.).
5. **GO-GRPC-STREAMING-CLEANUP-1**: Streaming loops exit cleanly on context cancellation; no goroutine leaks.

Enforcement tiers:
- Structured: context-deadlines, interceptors, proto-immutable, streaming-cleanup
- Prose: status-codes-correct

### go:gorm (Database ORM)
Domain: `go:gorm`  
Framework context: GORM v2+, PostgreSQL/MySQL/SQLite

Rules:
1. **GO-GORM-NO-N-PLUS-ONE-1**: Related data loaded via Preload/Join in single query; no N+1 patterns.
2. **GO-GORM-PARAMETERIZED-QUERIES-1**: All SQL parameterized; query builder preferred, raw SQL with ? placeholders only; no fmt.Sprintf.
3. **GO-GORM-TRANSACTIONS-MULTI-WRITE-1**: Multi-write operations wrapped in transactions; all-or-nothing consistency at database level.
4. **GO-GORM-MIGRATIONS-MANAGED-1**: Schema changes via version-controlled migrations; no manual SQL ALTER in production.
5. **GO-GORM-CONTEXT-QUERIES-1**: All queries accept context; timeouts and cancellation propagate from requests to DB layer.

Enforcement tiers:
- Structured: no-n-plus-one, parameterized-queries, transactions, migrations, context-queries
- Mechanical: parameterized-queries (SQL injection risk), migrations (version control)

## Design Rationale

### TOML Format Consistency
Each rule follows the established camerata corpus format:
- **id**: Framework-code + description anchor (e.g., GO-WEB-MIDDLEWARE-CROSS-CUTTING-1)
- **domain**: Language:Framework pair (go:web, go:grpc, go:gorm)
- **layer**: framework (web/grpc) or library (gorm, paralleling rust:seaorm)
- **enforcement**: structured/mechanical/prose per rule's validation difficulty
- **default**: true (all new rules are defaults)
- **[decision]** section: question, rationale (why), adoption default
- **[[option]]** blocks: adopted default + 1-2 alternatives with directives and trade-offs

### Rule Selection Criteria
Rules were drawn from:
- **Common failure modes**: N+1 queries, SQL injection, context lifecycle mismanagement, unhandled cancellation
- **Framework idioms**: Go's context.Context, gRPC interceptors, GORM's Preload/Join, middleware chains
- **Layering principles**: Thin handlers → services → repositories (matching Agora's hexagonal architecture pattern from rust port)
- **Consistency with existing corpora**: Mirrored express/next patterns (middleware, error handling, request binding) and seaorm patterns (parameterized queries, transactions)

### Coverage Balance
Each framework corpus covers 5–7 core rules:
- **Depth**: Each rule includes realistic alternatives and trade-offs (not just "do this")
- **Breadth**: Span presentation (middleware, handlers, error handling), domain (context, services), and data (queries, transactions)
- **Maturity**: Rules are actionable and verifiable (no abstract principles; mechanical/structured enforcement is always defined)

## Implementation Notes

### File Organization
```
crates/rules/principles/go/
├── go-*.toml                    (base language rules, pre-existing)
├── web/
│   ├── go-web-middleware-cross-cutting-1.toml
│   ├── go-web-request-binding-validation-1.toml
│   ├── go-web-context-propagation-1.toml
│   ├── go-web-thin-handlers-delegation-1.toml
│   ├── go-web-structured-error-responses-1.toml
│   ├── go-web-graceful-shutdown-1.toml
│   └── go-web-handler-timeouts-1.toml
├── grpc/
│   ├── go-grpc-context-deadlines-1.toml
│   ├── go-grpc-interceptors-auth-logging-1.toml
│   ├── go-grpc-proto-generated-immutable-1.toml
│   ├── go-grpc-status-codes-correct-1.toml
│   └── go-grpc-streaming-cleanup-1.toml
└── gorm/
    ├── go-gorm-no-n-plus-one-1.toml
    ├── go-gorm-parameterized-queries-1.toml
    ├── go-gorm-transactions-multi-write-1.toml
    ├── go-gorm-migrations-managed-1.toml
    └── go-gorm-context-queries-1.toml
```

### Testing
All 17 rules load successfully via `cargo test -p camerata-rules`. Test output confirms:
- Corpus loads without parse errors
- Domains are correctly indexed (go:web, go:grpc, go:gorm)
- Option defaults match the default field (per rule_has_default_reflects_default_option_field test)
- No duplicate IDs or malformed TOML

## Future Extensions

Potential additions (out of scope for this commit):
- **go:testing** — table-driven tests, mocking patterns, benchmark conventions
- **go:concurrency** — goroutine pools, channel usage, deadlock prevention
- **go:logging** — structured logging libraries (slog, zap), log levels, redaction
- **go:deployment** — containerization, signal handling, metrics/observability export

## Related Commits

- Branch: fw/go-frameworks
- Committed: New framework rule files + this decision doc
- CI: cargo test -p camerata-rules passes (39 tests, 0 failures)
