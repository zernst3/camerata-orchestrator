# Go Rule Grounding Report

Generated: 2026-06-20 — ground/go branch

## Summary

| Category | Count |
|---|---|
| Grounded | 22 |
| Ungrounded (draft) | 5 |
| Demoted (mechanical → prose) | 2 |

## Ungrounded Rules (no authoritative source found)

- **GO-HANDLER-SERVICE-REPOSITORY-1** — handler/service/repository layering is a widely-used industry pattern but is not defined in Effective Go, Code Review Comments, or any other official Go authority.
- **GO-WEB-MIDDLEWARE-CROSS-CUTTING-1** — middleware-for-cross-cutting-concerns is a framework convention (Gin, Echo) but has no official Go specification.
- **GO-WEB-REQUEST-BINDING-VALIDATION-1** — typed struct binding with validation tags is a Gin/Echo framework pattern; no official Go style guide covers it.
- **GO-WEB-STRUCTURED-ERROR-RESPONSES-1** — JSON error envelope format is an API design convention with no official Go authority.
- **GO-WEB-THIN-HANDLERS-DELEGATION-1** — thin-handler pattern is the same layering concept as GO-HANDLER-SERVICE-REPOSITORY-1; no official Go authority.

## Demoted Rules (enforcement changed from mechanical to prose)

- **GO-GORM-MIGRATIONS-MANAGED-1** — no linter enforces versioned-migration discipline; enforcement demoted from `mechanical` to `prose`.
- **GO-GRPC-PROTO-GENERATED-IMMUTABLE-1** — no standard linter enforces immutability of proto-generated files; enforcement demoted from `mechanical` to `prose`.

---

## Full Rule Table

| Rule ID | Verification | Source URL | Linter Rule | Status |
|---|---|---|---|---|
| GO-ACCEPT-INTERFACES-RETURN-STRUCTS-1 | grounded | https://go.dev/wiki/CodeReviewComments#interfaces | — | grounded |
| GO-CONTEXT-PROPAGATION-1 | grounded | https://go.dev/wiki/CodeReviewComments#contexts | golangci-lint: contextcheck | grounded |
| GO-ERROR-WRAPPING-WITH-FMT-W-1 | grounded | https://go.dev/blog/go1.13-errors | — | grounded |
| GO-ERRORS-MUST-BE-CHECKED-1 | grounded | https://go.dev/wiki/CodeReviewComments#handle-errors | golangci-lint: errcheck | grounded |
| GO-GOROUTINE-LEAKS-DEFER-CLEANUP-1 | grounded | https://go.dev/doc/effective_go#defer | — | grounded |
| GO-HANDLER-SERVICE-REPOSITORY-1 | draft | — | — | ungrounded |
| GO-LOGGING-STRUCTURED-1 | grounded | https://go.dev/blog/slog | — | grounded |
| GO-NO-GOROUTINE-GLOBALS-1 | grounded | https://go.dev/doc/articles/race_detector | — | grounded |
| GO-PACKAGE-BOUNDARIES-CLEAR-1 | grounded | https://go.dev/doc/go1.4#internalpackages | go build: circular import detection | grounded |
| GO-SMALL-INTERFACES-1 | grounded | https://go.dev/doc/effective_go#interfaces_and_types | — | grounded |
| GO-SQL-PARAMETERIZED-1 | grounded | https://securego.io/docs/rules/g201-g202.html | golangci-lint: gosec (G201, G202) | grounded |
| GO-GORM-CONTEXT-QUERIES-1 | grounded | https://gorm.io/docs/context.html | — | grounded |
| GO-GORM-MIGRATIONS-MANAGED-1 | grounded | https://gorm.io/docs/migration.html | — | demoted (mechanical → prose) |
| GO-GORM-NO-N-PLUS-ONE-1 | grounded | https://gorm.io/docs/preload.html | — | grounded |
| GO-GORM-PARAMETERIZED-QUERIES-1 | grounded | https://securego.io/docs/rules/g201-g202.html | golangci-lint: gosec (G201, G202) | grounded |
| GO-GORM-TRANSACTIONS-MULTI-WRITE-1 | grounded | https://gorm.io/docs/transactions.html | — | grounded |
| GO-GRPC-CONTEXT-DEADLINES-1 | grounded | https://grpc.io/docs/guides/deadlines/ | — | grounded |
| GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1 | grounded | https://pkg.go.dev/google.golang.org/grpc#UnaryServerInterceptor | — | grounded |
| GO-GRPC-PROTO-GENERATED-IMMUTABLE-1 | grounded | https://grpc.io/docs/languages/go/basics/ | — | demoted (mechanical → prose) |
| GO-GRPC-STATUS-CODES-CORRECT-1 | grounded | https://grpc.io/docs/guides/status-codes/ | — | grounded |
| GO-GRPC-STREAMING-CLEANUP-1 | grounded | https://grpc.io/docs/languages/go/basics/ | — | grounded |
| GO-WEB-CONTEXT-PROPAGATION-1 | grounded | https://go.dev/wiki/CodeReviewComments#contexts | golangci-lint: contextcheck | grounded |
| GO-WEB-GRACEFUL-SHUTDOWN-1 | grounded | https://pkg.go.dev/net/http#Server.Shutdown | — | grounded |
| GO-WEB-HANDLER-TIMEOUTS-1 | grounded | https://pkg.go.dev/net/http#Server | — | grounded |
| GO-WEB-MIDDLEWARE-CROSS-CUTTING-1 | draft | — | — | ungrounded |
| GO-WEB-REQUEST-BINDING-VALIDATION-1 | draft | — | — | ungrounded |
| GO-WEB-STRUCTURED-ERROR-RESPONSES-1 | draft | — | — | ungrounded |
| GO-WEB-THIN-HANDLERS-DELEGATION-1 | draft | — | — | ungrounded |

---

## Authorities Consulted

- **Effective Go** (https://go.dev/doc/effective_go) — interfaces, defer, goroutines, packages
- **Go Code Review Comments** (https://go.dev/wiki/CodeReviewComments) — interfaces, handle-errors, contexts, goroutine-lifetimes
- **Go Proverbs** (https://go-proverbs.github.io/) — small interfaces proverb
- **Go 1.13 error wrapping** (https://go.dev/blog/go1.13-errors) — fmt.Errorf %w, errors.Is, errors.Unwrap
- **Go 1.4 internal packages** (https://go.dev/doc/go1.4#internalpackages) — internal/ directory enforcement
- **Go Race Detector** (https://go.dev/doc/articles/race_detector) — concurrent state / goroutine safety
- **Go slog blog post** (https://go.dev/blog/slog) — structured logging rationale
- **pkg.go.dev log/slog** (https://pkg.go.dev/log/slog) — structured logging API
- **pkg.go.dev net/http#Server.Shutdown** (https://pkg.go.dev/net/http#Server.Shutdown) — graceful shutdown
- **pkg.go.dev net/http#Server** (https://pkg.go.dev/net/http#Server) — handler timeouts
- **golangci-lint linters** (https://golangci-lint.run/docs/linters/) — errcheck, contextcheck linters
- **gosec G201/G202** (https://securego.io/docs/rules/g201-g202.html) — SQL injection lint rules
- **GORM Context docs** (https://gorm.io/docs/context.html) — context propagation in queries
- **GORM Migration docs** (https://gorm.io/docs/migration.html) — AutoMigrate vs versioned migrations
- **GORM Preload docs** (https://gorm.io/docs/preload.html) — N+1 prevention via Preload/Join
- **GORM Transactions docs** (https://gorm.io/docs/transactions.html) — multi-write transaction pattern
- **gRPC Deadlines guide** (https://grpc.io/docs/guides/deadlines/) — deadline propagation
- **gRPC Status Codes guide** (https://grpc.io/docs/guides/status-codes/) — correct status code semantics
- **gRPC Go Basics tutorial** (https://grpc.io/docs/languages/go/basics/) — proto-generated code + streaming
- **grpc-go pkg.go.dev** (https://pkg.go.dev/google.golang.org/grpc#UnaryServerInterceptor) — interceptor types
