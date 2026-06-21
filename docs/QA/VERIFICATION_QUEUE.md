# Risk-Ordered Verification Queue

**Purpose.** This is the ordered list a human walks to promote rules from `grounded` to `verified`.
Work top-to-bottom: each row is one rule to eyeball, confirm its citation is accurate, and flip
`verification = "grounded"` to `verification = "verified"` in the TOML file.

**TODO:** No `demo_set = true` marker exists yet in the schema. When a "demo set" field is added,
re-sort rules that carry it to the top of every tier — they are the face of the product and must be
verified first.

---

## How to read this queue

| Column | Meaning |
|---|---|
| Rule ID | The `id` field from the TOML file |
| Domain | Language / layer scope of the rule |
| Enforcement | `mechanical`, `structured`, `architectural`, or `prose` |
| Primary source URL | First `[[sources]].url` — the URL to open and spot-check |
| Linter citation | What the registry validator extracted from `qualifies` |

**Verification steps (per row):**
1. Open the TOML file in `crates/rules/principles/`.
2. Open the primary source URL — confirm it says what the `qualifies` field claims.
3. If the rule has a linter citation (`resolves` in citation-validation.md), run or cross-check that
   linter rule against a real codebase; confirm the rule catches what it claims.
4. If everything checks out, change `verification = "grounded"` to `verification = "verified"`.

---

## Tier A: mechanical / deny-level rules (highest blast radius — enforce by build failure)

These rules are set to `enforcement = "mechanical"`. When Camerata applies them, they become
a required CI gate or a linter deny. A wrong rule here breaks builds or causes spurious failures
across every consumer. Verify these first.

### A1. Mechanical rules with a resolving linter citation (easiest to cross-check)

| Rule ID | Domain | Enforcement | Primary source URL | Linter citation (resolves) |
|---|---|---|---|---|
| `RUST-NO-UNWRAP-1` | rust | mechanical | https://rust-lang.github.io/rust-clippy/master/index.html#unwrap_used | `clippy::unwrap_used` |
| `JAVASCRIPT-NO-VAR-1` | javascript | mechanical | https://eslint.org/docs/latest/rules/no-var | `eslint::no-var` |
| `JAVASCRIPT-STRICT-EQUALITY-1` | javascript | mechanical | https://eslint.org/docs/latest/rules/eqeqeq | `eslint::eqeqeq` (extracted via citation-validation) |
| `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` | javascript:react | mechanical | https://react.dev/reference/eslint-plugin-react-hooks/lints/rules-of-hooks | `react-hooks::rules-of-hooks` |
| `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` | javascript:react | mechanical | https://react.dev/reference/eslint-plugin-react-hooks/lints/exhaustive-deps | `react-hooks::exhaustive-deps` |
| `JAVASCRIPT-TYPESCRIPT-NO-EXPLICIT-ANY-1` | javascript:typescript | mechanical | https://typescript-eslint.io/rules/no-explicit-any | `typescript-eslint::no-explicit-any` |
| `JAVASCRIPT-TYPESCRIPT-NO-FLOATING-PROMISES-1` | javascript:typescript | mechanical | https://typescript-eslint.io/rules/no-floating-promises | `typescript-eslint::no-floating-promises` |
| `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1` | javascript:typescript | mechanical | https://typescript-eslint.io/rules/no-non-null-assertion | `typescript-eslint::no-non-null-assertion` |
| `JAVASCRIPT-NEXT-DUAL-API-1` | javascript:next | mechanical | https://nextjs.org/docs/app/getting-started/server-and-client-components#preventing-environment-poisoning | `eslint::no-restricted-imports`, `eslint::no-restricted-globals` |
| `PYTHON-NO-BARE-EXCEPT-1` | python | mechanical | https://docs.astral.sh/ruff/rules/bare-except/ | `ruff::e722`, `ruff::ble001` |
| `PYTHON-PARAMETERIZED-SQL-1` | python | mechanical | https://docs.astral.sh/ruff/rules/hardcoded-sql-expression/ | `ruff::s608`, `bandit::b608` |
| `PYTHON-DJANGO-ORM-PARAMETERIZED-1` | python:django | mechanical | https://docs.djangoproject.com/en/6.0/topics/db/sql/ | `ruff::s608`, `bandit::b608` |
| `PYTHON-DJANGO-SETTINGS-FROM-ENV-1` | python:django | mechanical | https://docs.djangoproject.com/en/6.0/ref/settings/#secret-key | `bandit::b105`, `bandit::b106`, `bandit::b107` |
| `PYTHON-FLASK-CONFIG-FROM-ENV-1` | python:flask | mechanical | https://flask.palletsprojects.com/en/stable/config/ | `bandit::b105`, `bandit::b106`, `bandit::b107` |
| `PYTHON-FLASK-PARAMETERIZED-SQL-1` | python:flask | mechanical | https://docs.astral.sh/ruff/rules/hardcoded-sql-expression/ | `ruff::s608`, `bandit::b608` |
| `RUBY-FROZEN-STRING-LITERAL-1` | ruby | mechanical | https://github.com/rubocop/rubocop/blob/master/lib/rubocop/cop/style/frozen_string_literal_comment.rb | `rubocop::style/frozenstringliteralcomment` |
| `GO-ERRORS-MUST-BE-CHECKED-1` | go | mechanical | https://go.dev/wiki/CodeReviewComments#handle-errors | `golangci-lint::errcheck` |
| `GO-GORM-PARAMETERIZED-QUERIES-1` | go:gorm | mechanical | https://securego.io/docs/rules/g201-g202.html | `golangci-lint::errcheck` (gosec via staticcheck proxy) |
| `CSHARP-IDISPOSABLE-USING-1` | csharp | mechanical | https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca2000 | `roslyn::ca1001`, `roslyn::ca1816`, `roslyn-style::ide0090` |
| `CSHARP-NO-SWALLOWED-EXCEPTIONS-1` | csharp | mechanical | https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca1031 | `roslyn::ca1031`, `roslyn::ca1068` |

### A2. Mechanical rules with no resolving linter citation (must spot-check source URL only)

| Rule ID | Domain | Enforcement | Primary source URL | Notes |
|---|---|---|---|---|
| `ARCH-API-DTOS-1` | api-layer | mechanical | _(no verification field — draft)_ | Draft; unsourced in citation report. No linter. |
| `ARCH-EXACT-DECIMALS-1` | api-layer | mechanical | _(no verification field — draft)_ | Draft; unsourced. Property-test gate referenced but no URL. |
| `ARCH-STRICT-LAYERING-1` | api-layer | mechanical | docs/decisions/2026-06-19_ast_architectural_rule_tier.md | Internal ADR only; no external URL. Confirm ADR is faithful. |
| `ARCH-STRUCTURED-ERRORS-1` | api-layer | mechanical | _(no verification field — draft)_ | Draft; unsourced. Review/fix before applying. |
| `ARCH-SERVER-AUTHZ-1` | permissions | mechanical | CONVENTIONS.md | Internal source. Confirm ESLint rules in qualifies match the convention. |
| `CSHARP-ASPNETCORE-ASYNC-ACTIONS-1` | csharp:aspnetcore | mechanical | https://learn.microsoft.com/en-us/aspnet/core/fundamentals/best-practices?view=aspnetcore-10.0 | No linter extracted; prose-sourced. Confirm async/sync prohibition. |
| `CSHARP-ASYNC-NO-BLOCKING-1` | csharp:aspnet | mechanical | https://learn.microsoft.com/en-us/aspnet/core/fundamentals/best-practices?view=aspnetcore-10.0 | Roslyn CA rule expected — none extracted. |
| `CSHARP-NO-HARDCODED-SECRETS-1` | csharp | mechanical | https://learn.microsoft.com/en-us/aspnet/core/security/app-secrets?view=aspnetcore-10.0&tabs=linux | No linter extracted. |
| `CSHARP-NULLABLE-REFERENCE-TYPES-1` | csharp | mechanical | https://learn.microsoft.com/en-us/dotnet/csharp/fundamentals/null-safety/nullable-reference-types | No linter extracted. |
| `CSHARP-SQL-PARAMETERIZED-1` | csharp | mechanical | https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca2100 | CA2100 expected in qualifies but not extracted. Confirm. |
| `GO-PACKAGE-BOUNDARIES-CLEAR-1` | go | mechanical | https://go.dev/doc/go1.4#internalpackages | No linter extracted. |
| `GO-SQL-PARAMETERIZED-1` | go | mechanical | https://securego.io/docs/rules/g201-g202.html | No linter extracted. |
| `JAVA-EXCEPTION-HANDLING-1` | java | mechanical | https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html#de-method-might-ignore-exception-de-might-ignore | No linter extracted. |
| `JAVA-NO-HARDCODED-SECRETS-1` | java | mechanical | https://find-sec-bugs.github.io/bugs.htm#HARD_CODE_PASSWORD | No linter extracted. |
| `JAVA-RESOURCE-MANAGEMENT-1` | java | mechanical | https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html#os-method-may-fail-to-close-stream-on-exception-os-open-stream | No linter extracted. |
| `JAVA-SQL-PARAMETERIZED-1` | java | mechanical | https://find-sec-bugs.github.io/bugs.htm#SQL_INJECTION_JDBC | No linter extracted. |
| `JAVASCRIPT-CONST-DEFAULT-1` | javascript | mechanical | https://eslint.org/docs/latest/rules/prefer-const | `eslint::prefer-const` expected but not in registry mapping. |
| `ORCH-CONFORMANCE-1` | agentic | mechanical | AGENTS.md | Internal only. Confirm gate described in AGENTS.md matches qualifies. |
| `ORCH-ENV-GATED-QUALITY-1` | agentic | mechanical | AGENTS.md | Internal only. |
| `ORCH-NEW-PATH-TESTS-1` | agentic | mechanical | AGENTS.md | Internal only. |
| `ORCH-PREREVIEW-1` | agentic | mechanical | AGENTS.md | Internal only. |
| `PYTHON-TYPE-HINTS-1` | python | mechanical | https://peps.python.org/pep-0484/ | No linter extracted. mypy/ruff expected. |
| `RUBY-AVOID-EVAL-SEND-1` | ruby | mechanical | https://brakemanscanner.org/docs/warning_types/dangerous_eval/ | Brakeman tool cited but rule name not extracted. |
| `RUBY-EAGER-LOAD-ASSOCIATIONS-1` | ruby:rails | mechanical | https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations | No linter extracted. |
| `RUBY-NO-HARDCODED-SECRETS-1` | ruby | mechanical | https://github.com/gitleaks/gitleaks | gitleaks tool cited but rule name not extracted. |
| `RUBY-RAILS-NO-SECRETS-IN-CODE-1` | ruby:rails | mechanical | https://github.com/gitleaks/gitleaks | gitleaks tool cited but rule name not extracted. |
| `RUBY-RAILS-NO-STRING-SQL-1` | ruby:rails | mechanical | https://brakemanscanner.org/docs/warning_types/sql_injection/ | Brakeman cited but rule name not extracted. |
| `RUBY-RAILS-PARAMETERIZED-QUERIES-1` | ruby:rails | mechanical | https://brakemanscanner.org/docs/warning_types/sql_injection/ | Brakeman cited but rule name not extracted. |
| `RUBY-RAILS-STRONG-PARAMS-1` | ruby:rails | mechanical | https://brakemanscanner.org/docs/warning_types/mass_assignment/ | Brakeman cited but rule name not extracted. |
| `UI-IMAGE-COMPONENT-1` | ui | mechanical | _(no verification field — draft)_ | Draft; ESLint citations resolve but verification missing. |
| `UI-UTC-DATES-1` | ui | mechanical | _(no verification field — draft)_ | Draft; ESLint citations resolve but verification missing. |

---

## Tier B: known-language grounded rules (maintainer can verify fast — C#, Rust, TypeScript)

These are `grounded` with external citations. The maintainer knows these languages and can confirm
citations quickly. Ordered: Rust first (maintainer's primary), then TypeScript, then C#.

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `RUST-DOMAIN-1` | rust | structured | https://doc.rust-lang.org/book/ch07-00-managing-growing-projects-with-packages-crates-and-modules.html |
| `RUST-DOMAIN-2` | rust | structured | https://rust-lang.github.io/api-guidelines/type-safety.html |
| `RUST-DOMAIN-3` | rust | structured | https://rust-lang.github.io/api-guidelines/type-safety.html |
| `RUST-DOMAIN-4` | rust | structured | https://rust-lang.github.io/api-guidelines/interoperability.html |
| `RUST-DOMAIN-5` | rust | prose | https://doc.rust-lang.org/book/ch17-00-async-await.html |
| `RUST-DOMAIN-6` | rust | structured | https://rust-lang.github.io/api-guidelines/interoperability.html |
| `RUST-ENTITIES-1` | rust:seaorm | structured | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveEntityModel.html |
| `RUST-ENTITIES-2` | rust:seaorm | structured | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveEntityModel.html |
| `RUST-ENTITIES-3` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ |
| `RUST-ENTITIES-4` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/relation/one-to-many/ |
| `RUST-ENTITIES-5` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ |
| `RUST-ENTITIES-6` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ |
| `RUST-ENTITIES-7` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ |
| `RUST-ENTITIES-9` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/relation/complex-relations/ |
| `RUST-ENTITIES-10` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/relation/one-to-many/ |
| `RUST-ENTITIES-11` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/relation/complex-relations/ |
| `RUST-ENTITIES-12` | rust:seaorm | structured | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveActiveEnum.html |
| `RUST-ENTITIES-13` | rust:seaorm | structured | https://docs.rs/sea-orm/latest/sea_orm/derive.DeriveEntityModel.html |
| `RUST-SEAORM-INTRA-AGGREGATE-TX-1` | rust:seaorm | structured | https://www.sea-ql.org/SeaORM/docs/generate-entity/entity-format/ |
| `RUST-SEAORM-RAW-SQL-ESCAPE-1` | rust:seaorm | structured | https://docs.rs/sea-orm/latest/sea_orm/ |
| `RUST-SQLX-COMPILE-CHECKED-QUERIES-1` | rust:sqlx | structured | https://docs.rs/sqlx/latest/sqlx/macro.query.html |
| `RUST-SQLX-CONNECTION-POOL-SIZED-1` | rust:sqlx | structured | https://docs.rs/sqlx/latest/sqlx/pool/struct.Pool.html |
| `RUST-SQLX-MIGRATIONS-CHECKED-IN-1` | rust:sqlx | structured | https://docs.rs/sqlx/latest/sqlx/macro.migrate.html |
| `RUST-SQLX-TRANSACTIONS-MULTI-WRITE-1` | rust:sqlx | structured | https://docs.rs/sqlx/latest/sqlx/struct.Transaction.html |
| `RUST-TOKIO-BOUNDED-CHANNELS-1` | rust:tokio | structured | https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html |
| `RUST-TOKIO-CANCELLATION-SAFE-SELECTS-1` | rust:tokio | structured | https://docs.rs/tokio/latest/tokio/macro.select.html |
| `RUST-TOKIO-NO-BLOCKING-IN-ASYNC-1` | rust:tokio | structured | https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html |
| `RUST-TOKIO-NO-STD-MUTEX-ACROSS-AWAIT-1` | rust:tokio | structured | https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html |
| `RUST-TOKIO-STRUCTURED-CONCURRENCY-JOINSET-1` | rust:tokio | structured | https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html |
| `RUST-AXUM-EXTRACTORS-VALIDATE-1` | rust:axum | structured | https://docs.rs/axum/latest/axum/extract/index.html |
| `RUST-AXUM-HANDLERS-THIN-DELEGATE-1` | rust:axum | structured | https://docs.rs/axum/latest/axum/ |
| `RUST-AXUM-MIDDLEWARE-TOWER-1` | rust:axum | structured | https://docs.rs/axum/latest/axum/middleware/index.html |
| `RUST-AXUM-STATE-SHARED-DEPS-1` | rust:axum | structured | https://docs.rs/axum/latest/axum/extract/struct.State.html |
| `RUST-AXUM-TIMEOUT-LIMITS-1` | rust:axum | structured | https://docs.rs/axum/latest/axum/extract/struct.DefaultBodyLimit.html |
| `RUST-AXUM-TYPED-ERROR-RESPONSE-1` | rust:axum | structured | https://docs.rs/axum/latest/axum/response/index.html |
| `RUST-DIOXUS-2` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/reference/components |
| `RUST-DIOXUS-3` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/essentials/state/ |
| `RUST-DIOXUS-4` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/reference/context |
| `RUST-DIOXUS-5` | rust:dioxus | structured | https://docs.rs/dioxus/latest/dioxus/prelude/fn.use_effect.html |
| `RUST-DIOXUS-6` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/essentials/async/ |
| `RUST-DIOXUS-7` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/reference/event_handlers |
| `RUST-DIOXUS-8` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/reference/rsx |
| `RUST-DIOXUS-9` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/guides/fullstack/server_functions |
| `RUST-DIOXUS-11` | rust:dioxus | structured | https://dioxuslabs.com/learn/0.6/guides/fullstack/ |
| `JAVASCRIPT-REACT-FUNCTION-COMPONENTS-1` | javascript:react | structured | https://react.dev/reference/react/Component |
| `JAVASCRIPT-REACT-KEYED-LISTS-1` | javascript:react | structured | https://react.dev/learn/rendering-lists#keeping-list-items-in-order-with-key |
| `JAVASCRIPT-NEXT-ROUTE-PLACEMENT-1` | javascript:next | structured | https://nextjs.org/docs/app/api-reference/file-conventions/route-groups |
| `JAVASCRIPT-TYPESCRIPT-STRICT-MODE-1` | javascript:typescript | prose | https://www.typescriptlang.org/tsconfig#strict |
| `CSHARP-ASPNETCORE-AUTHORIZE-ATTRIBUTE-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/aspnet/core/security/authorization/simple?view=aspnetcore-10.0 |
| `CSHARP-ASPNETCORE-CORS-EXPLICIT-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/aspnet/core/security/cors?view=aspnetcore-10.0 |
| `CSHARP-ASPNETCORE-MIDDLEWARE-ORDERING-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/aspnet/core/fundamentals/middleware/?view=aspnetcore-10.0 |
| `CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/aspnet/core/fundamentals/apis?view=aspnetcore-9.0 |
| `CSHARP-ASPNETCORE-OPTIONS-PATTERN-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/aspnet/core/fundamentals/configuration/options?view=aspnetcore-10.0 |
| `CSHARP-ASPNETCORE-THIN-CONTROLLERS-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/dotnet/architecture/modern-web-apps-azure/common-web-application-architectures |
| `CSHARP-EFCORE-ASNOTRACKING-1` | csharp:aspnetcore | structured | https://learn.microsoft.com/en-us/ef/core/querying/tracking |
| `CSHARP-EFCORE-EXPLICIT-TRANSACTIONS-1` | csharp:efcore | structured | https://learn.microsoft.com/en-us/ef/core/saving/transactions |
| `CSHARP-EFCORE-MIGRATIONS-CHECKED-IN-1` | csharp:efcore | structured | https://learn.microsoft.com/en-us/ef/core/managing-schemas/migrations/applying |
| `CSHARP-EFCORE-NO-NPLUS1-1` | csharp:efcore | structured | https://learn.microsoft.com/en-us/ef/core/performance/efficient-querying |

---

## Tier C: other grounded rules (external citation, non-priority language or domain)

These are all `grounded` with external citations but fall outside the Tier B fast-path languages.
Work through them in family order.

### Go

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `GO-ACCEPT-INTERFACES-RETURN-STRUCTS-1` | go | prose | https://go.dev/wiki/CodeReviewComments#interfaces |
| `GO-CONTEXT-PROPAGATION-1` | go | structured | https://go.dev/wiki/CodeReviewComments#contexts |
| `GO-ERROR-WRAPPING-WITH-FMT-W-1` | go | prose | https://go.dev/blog/go1.13-errors |
| `GO-GOROUTINE-LEAKS-DEFER-CLEANUP-1` | go | structured | https://go.dev/doc/effective_go#defer |
| `GO-LOGGING-STRUCTURED-1` | go | prose | https://go.dev/blog/slog |
| `GO-NO-GOROUTINE-GLOBALS-1` | go | structured | https://go.dev/doc/articles/race_detector |
| `GO-SMALL-INTERFACES-1` | go | prose | https://go.dev/doc/effective_go#interfaces_and_types |
| `GO-GORM-CONTEXT-QUERIES-1` | go:gorm | structured | https://gorm.io/docs/context.html |
| `GO-GORM-MIGRATIONS-MANAGED-1` | go:gorm | prose | https://gorm.io/docs/migration.html |
| `GO-GORM-NO-N-PLUS-ONE-1` | go:gorm | structured | https://gorm.io/docs/preload.html |
| `GO-GORM-TRANSACTIONS-MULTI-WRITE-1` | go:gorm | structured | https://gorm.io/docs/transactions.html |
| `GO-GRPC-CONTEXT-DEADLINES-1` | go:grpc | structured | https://grpc.io/docs/guides/deadlines/ |
| `GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1` | go:grpc | structured | https://pkg.go.dev/google.golang.org/grpc#UnaryServerInterceptor |
| `GO-GRPC-PROTO-GENERATED-IMMUTABLE-1` | go:grpc | prose | https://grpc.io/docs/languages/go/basics/ |
| `GO-GRPC-STATUS-CODES-CORRECT-1` | go:grpc | prose | https://grpc.io/docs/guides/status-codes/ |
| `GO-GRPC-STREAMING-CLEANUP-1` | go:grpc | prose | https://grpc.io/docs/languages/go/basics/ |
| `GO-WEB-CONTEXT-PROPAGATION-1` | go:web | structured | https://go.dev/wiki/CodeReviewComments#contexts |
| `GO-WEB-GRACEFUL-SHUTDOWN-1` | go:web | structured | https://pkg.go.dev/net/http#Server.Shutdown |
| `GO-WEB-HANDLER-TIMEOUTS-1` | go:web | prose | https://pkg.go.dev/net/http#Server |

### Java

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `JAVA-CONSTRUCTOR-INJECTION-1` | java:spring | prose | https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html |
| `JAVA-IMMUTABILITY-FINAL-1` | java | prose | https://checkstyle.org/checks/design/finalclass.html |
| `JAVA-JPA-EAGER-LOAD-1` | java | prose | https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html#jpa.entity-graph |
| `JAVA-LAYERING-CONTROLLER-SERVICE-REPO-1` | java:spring | prose | https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html |
| `JAVA-LOGGING-STRUCTURED-1` | java | prose | https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html#systemprintln |
| `JAVA-OPTIONAL-OVER-NULL-1` | java | prose | https://errorprone.info/bugpatterns |
| `JAVA-PACKAGE-BY-FEATURE-1` | java | prose | https://pmd.github.io/pmd/pmd_rules_java_design.html#loosepackagecoupling |
| `JAVA-SMALL-INTERFACES-1` | java | prose | https://pmd.github.io/pmd/pmd_rules_java_design.html#excessivepubliccount |
| `JAVA-SPRING-CONFIGURATION-PROPERTIES-1` | java:spring | structured | https://docs.spring.io/spring-boot/reference/features/external-config.html#features.external-config.typesafe-configuration-properties |
| `JAVA-SPRING-CONSTRUCTOR-INJECTION-1` | java:spring | prose | https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html |
| `JAVA-SPRING-DTO-BOUNDARY-1` | java:spring | structured | https://docs.spring.io/spring-framework/reference/web/webmvc/mvc-controller/ann-validation.html |
| `JAVA-SPRING-LAYERED-ARCHITECTURE-1` | java:spring | structured | https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html |
| `JAVA-SPRING-METHOD-SECURITY-1` | java:spring | structured | https://docs.spring.io/spring-security/reference/servlet/authorization/method-security.html |
| `JAVA-SPRING-NO-NPLUS1-FETCH-JOIN-1` | java:spring | structured | https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html#jpa.entity-graph |
| `JAVA-SPRING-THIN-CONTROLLERS-1` | java:spring | structured | https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html |
| `JAVA-SPRING-TRANSACTIONAL-SERVICE-LAYER-1` | java:spring | structured | https://docs.spring.io/spring-framework/reference/data-access/transaction/declarative/annotations.html |
| `JAVA-SPRING-VALID-REQUEST-VALIDATION-1` | java:spring | prose | https://docs.spring.io/spring-framework/reference/web/webmvc/mvc-controller/ann-validation.html |

### Python

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `PYTHON-EXPLICIT-IMPORTS-1` | python | prose | https://peps.python.org/pep-0008/#imports |
| `PYTHON-NO-BLOCKING-IO-ASYNC-1` | python | structured | https://docs.python.org/3/library/asyncio-eventloop.html#asyncio.loop.run_in_executor |
| `PYTHON-ORM-EAGER-LOAD-1` | python | prose | https://docs.sqlalchemy.org/en/20/orm/queryguide/relationships.html |
| `PYTHON-SERVICE-LAYER-1` | python | structured | https://docs.djangoproject.com/en/6.0/misc/design-philosophies/ |
| `PYTHON-SETTINGS-FROM-ENV-1` | python | structured | https://bandit.readthedocs.io/en/latest/plugins/b105_hardcoded_password_string.html |
| `PYTHON-DJANGO-CSRF-AUTH-1` | python:django | structured | https://docs.djangoproject.com/en/6.0/ref/csrf/ |
| `PYTHON-DJANGO-FAT-MODEL-SERVICE-1` | python:django | structured | https://docs.djangoproject.com/en/6.0/misc/design-philosophies/ |
| `PYTHON-DJANGO-FORMS-SERIALIZERS-VALIDATE-1` | python:django | structured | https://docs.djangoproject.com/en/6.0/topics/forms/ |
| `PYTHON-DJANGO-MIGRATIONS-CHECKED-IN-1` | python:django | structured | https://docs.djangoproject.com/en/6.0/topics/migrations/ |
| `PYTHON-DJANGO-SELECT-RELATED-1` | python:django | structured | https://docs.djangoproject.com/en/6.0/topics/db/optimization/ |
| `PYTHON-FASTAPI-AUTH-DEPENDENCY-1` | python:fastapi | structured | https://fastapi.tiangolo.com/tutorial/dependencies/ |
| `PYTHON-FASTAPI-DI-SESSION-1` | python:fastapi | structured | https://fastapi.tiangolo.com/tutorial/sql-databases/ |
| `PYTHON-FASTAPI-PYDANTIC-MODELS-1` | python:fastapi | structured | https://fastapi.tiangolo.com/tutorial/request-body/ |
| `PYTHON-FLASK-APP-FACTORY-1` | python:flask | structured | https://flask.palletsprojects.com/en/stable/patterns/appfactories/ |
| `PYTHON-FLASK-AUTH-ON-PROTECTED-ROUTES-1` | python:flask | structured | https://flask-login.readthedocs.io/en/latest/ |
| `PYTHON-FLASK-ERROR-HANDLERS-1` | python:flask | structured | https://flask.palletsprojects.com/en/stable/errorhandling/ |
| `PYTHON-FLASK-REQUEST-VALIDATION-1` | python:flask | structured | https://flask.palletsprojects.com/en/stable/api/#flask.Request.json |
| `PYTHON-FLASK-SERVICE-LAYER-1` | python:flask | structured | https://flask.palletsprojects.com/en/stable/patterns/appfactories/ |

### Ruby

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `RUBY-ACTIVERECORD-SCOPES-1` | ruby:rails | structured | https://guides.rubyonrails.org/active_record_querying.html#scopes |
| `RUBY-CONCERNS-JUDICIOUSLY-1` | ruby:rails | structured | https://guides.rubyonrails.org/engines.html#concerns |
| `RUBY-RAILS-BACKGROUND-JOBS-1` | ruby:rails | structured | https://guides.rubyonrails.org/active_job_basics.html |
| `RUBY-RAILS-CSRF-1` | ruby:rails | structured | https://guides.rubyonrails.org/security.html#cross-site-request-forgery-csrf |
| `RUBY-RAILS-EAGER-LOAD-1` | ruby:rails | structured | https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations |
| `RUBY-RAILS-MODEL-VALIDATIONS-1` | ruby:rails | structured | https://guides.rubyonrails.org/active_record_validations.html |
| `RUBY-RAILS-SCOPES-1` | ruby:rails | structured | https://guides.rubyonrails.org/active_record_querying.html#scopes |
| `RUBY-RAILS-SKINNY-CONTROLLERS-1` | ruby:rails | structured | https://guides.rubyonrails.org/action_controller_overview.html |
| `RUBY-SMALL-METHODS-1` | ruby | structured | https://rubystyle.guide/#short-methods |
| `RUBY-THIN-CONTROLLERS-1` | ruby:rails | structured | https://guides.rubyonrails.org/action_controller_overview.html |
| `RUBY-VALIDATION-LAYER-1` | ruby:rails | structured | https://guides.rubyonrails.org/active_record_validations.html |

### JavaScript (non-TypeScript non-React non-Next)

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `JAVASCRIPT-ANGULAR-AVOID-LOGIC-IN-TEMPLATES-1` | javascript:angular | prose | https://angular.dev/style-guide |
| `JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1` | javascript:angular | structured | https://angular.dev/guide/di |
| `JAVASCRIPT-ANGULAR-LAZY-LOADING-ROUTES-1` | javascript:angular | structured | https://angular.dev/guide/routing/lazy-loading |
| `JAVASCRIPT-ANGULAR-NO-DIRECT-DOM-MANIPULATION-1` | javascript:angular | structured | https://angular.dev/guide/components/dom-apis |
| `JAVASCRIPT-ANGULAR-ONPUSH-CHANGE-DETECTION-1` | javascript:angular | structured | https://angular.dev/best-practices/skipping-subtrees |
| `JAVASCRIPT-ANGULAR-REACTIVE-FORMS-1` | javascript:angular | structured | https://angular.dev/guide/forms/reactive-forms |
| `JAVASCRIPT-ANGULAR-ROUTE-GUARDS-1` | javascript:angular | structured | https://angular.dev/guide/routing/route-guards |
| `JAVASCRIPT-ANGULAR-STANDALONE-COMPONENTS-1` | javascript:angular | structured | https://angular.dev/guide/components/anatomy-of-components |
| `JAVASCRIPT-ANGULAR-SUBSCRIPTION-CLEANUP-1` | javascript:angular | structured | https://angular.dev/ecosystem/rxjs-interop/take-until-destroyed |
| `JAVASCRIPT-ANGULAR-TYPED-HTTPCLIENT-1` | javascript:angular | structured | https://angular.dev/guide/http/making-requests |
| `JAVASCRIPT-EXPRESS-CENTRAL-ERROR-HANDLER-1` | javascript:express | structured | https://expressjs.com/en/advanced/best-practice-performance.html |
| `JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1` | javascript:express | structured | https://expressjs.com/en/advanced/best-practice-security.html |
| `JAVASCRIPT-EXPRESS-THIN-CONTROLLERS-1` | javascript:express | structured | https://expressjs.com/en/advanced/best-practice-performance.html |
| `JAVASCRIPT-EXPRESS-VALIDATE-INPUT-1` | javascript:express | structured | https://expressjs.com/en/advanced/best-practice-security.html |
| `JAVASCRIPT-NEST-DTOS-VALIDATION-PIPE-1` | javascript:nest | structured | https://docs.nestjs.com/pipes#class-validator |
| `JAVASCRIPT-NEST-GUARDS-FOR-AUTH-1` | javascript:nest | structured | https://docs.nestjs.com/guards |
| `JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1` | javascript:nest | structured | https://docs.nestjs.com/interceptors |
| `JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1` | javascript:nest | structured | https://docs.nestjs.com/modules |
| `JAVASCRIPT-NEST-THIN-CONTROLLERS-DELEGATE-1` | javascript:nest | structured | https://docs.nestjs.com/controllers |
| `JAVASCRIPT-REDUX-NO-EFFECTS-IN-REDUCERS-1` | javascript:redux | structured | https://redux.js.org/style-guide/#reducers-must-not-have-side-effects |
| `JAVASCRIPT-REDUX-NO-STATE-MUTATION-1` | javascript:redux | structured | https://redux.js.org/style-guide/#do-not-mutate-state |
| `JAVASCRIPT-REDUX-SERIALIZABLE-STATE-1` | javascript:redux | prose | https://redux.js.org/style-guide/#do-not-put-non-serializable-values-in-state-or-actions |
| `JAVASCRIPT-REDUX-TOOLKIT-DEFAULT-1` | javascript:redux | structured | https://redux.js.org/style-guide/#use-redux-toolkit-for-writing-redux-logic |
| `JAVASCRIPT-VUE-COMPOSITION-SCRIPT-SETUP-1` | javascript:vue | structured | https://vuejs.org/guide/extras/composition-api-faq |
| `JAVASCRIPT-VUE-COMPUTED-OVER-METHODS-1` | javascript:vue | structured | https://vuejs.org/guide/essentials/computed.html |
| `JAVASCRIPT-VUE-NO-DIRECT-DOM-1` | javascript:vue | structured | https://vuejs.org/guide/essentials/reactivity-fundamentals.html |
| `JAVASCRIPT-VUE-PROPS-DOWN-EVENTS-UP-1` | javascript:vue | structured | https://vuejs.org/guide/components/props.html |
| `JAVASCRIPT-VUE-SCOPED-STYLES-1` | javascript:vue | structured | https://vuejs.org/style-guide/rules-essential#component-style-scoping |
| `JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1` | javascript:vue | structured | https://pinia.vuejs.org/introduction.html |

### SQL / cross-cutting / agentic / permissions

| Rule ID | Domain | Enforcement | Primary source URL |
|---|---|---|---|
| `ARCH-EXPAND-CONTRACT-1` | sql | prose | https://martinfowler.com/bliki/ParallelChange.html |
| `ARCH-IAC-1` | iac | prose | docs/decisions/2026-06-15_credential_delegated_scope_and_build_targets.md |
| `ARCH-MONOLITH-FIRST-1` | fullstack | structured | docs/decisions/2026-06-14_persistence_sqlite_event_sourced_versioning.md |
| `ARCH-NO-SECRETS-IN-URL-1` | * | structured | docs/ENFORCEMENT.md |
| `ARCH-PARALLEL-INDEPENDENT-1` | fullstack | structured | docs/decisions/2026-06-16_scan_execution_modes.md |
| `ARCH-FETCH-THEN-AUTHORIZE-1` | permissions | structured | CONVENTIONS.md |
| `ARCH-HANDLER-NO-DB-1` | api-layer | architectural | docs/decisions/2026-06-19_ast_architectural_rule_tier.md |
| `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1` | api-layer | architectural | docs/decisions/2026-06-19_ast_architectural_rule_tier.md |
| `ARCH-TRIGGER-ENV-1` | ci-cd | prose | CONVENTIONS.md |
| `ARCH-TRUNK-SYNC-1` | ci-cd | prose | CONVENTIONS.md |
| `PROC-AUTO-MERGE-1` | ci-cd | prose | AGENTS.md |
| `PROC-CI-MIGRATION-HYGIENE-1` | sql | prose | https://www.postgresql.org/docs/current/sql-createtable.html |
| `PROC-CITE-CONVENTION-ID-1` | * | structured | CONVENTIONS.md |
| `PROC-HIDE-DEAD-END-1` | permissions | structured | CONVENTIONS.md |
| `PROC-MIGRATION-ROLE-GRANTS-1` | permissions | structured | CONVENTIONS.md |
| `PROC-PERMISSION-CONFIG-1` | permissions | prose | CONVENTIONS.md |
| `PROC-PR-CONCURRENCY-1` | concurrency | structured | CONVENTIONS.md |
| `PROC-REGRESSION-TEST-1` | * | structured | CONVENTIONS.md |
| `PROC-STORY-DOCS-1` | agentic | prose | docs/decisions/2026-06-20_poststoryhook_doc_emission.md |
| `SPIRIT-DOC-DECISIONS-1` | * | structured | CONVENTIONS.md |
| `SPIRIT-FILE-SIZE-1` | * | prose | AGENTS.md |
| `SPIRIT-OPTIMIZE-1` | * | prose | AGENTS.md |
| `SPIRIT-ROBUSTNESS-1` | * | prose | AGENTS.md |
| `SQL-DB-INDEX-1` | sql | prose | https://www.postgresql.org/docs/current/ddl-constraints.html |
| `SQL-DB-INDEX-2` | sql | prose | https://www.postgresql.org/docs/current/indexes-intro.html |
| `SQL-DB-NPLUSONE-1` | sql | prose | https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations |
| `ORCH-AUTOCALLS-LEDGER-1` | agentic | structured | AGENTS.md |
| `ORCH-BUDGET-MONITOR-1` | agentic | prose | AGENTS.md |
| `ORCH-CLEAR-WINNER-1` | agentic | prose | AGENTS.md |
| `ORCH-CONFLICTING-ROBUSTNESS-1` | agentic | prose | AGENTS.md |
| `ORCH-CONTEXT-OVERRIDE-1` | agentic | prose | AGENTS.md |
| `ORCH-MODEL-TIERING-1` | agentic | prose | AGENTS.md |
| `ORCH-NO-NATURAL-BREAK-1` | agentic | prose | AGENTS.md |
| `ORCH-NOVELTY-1` | agentic | prose | AGENTS.md |
| `ORCH-ONE-WAY-DOOR-1` | agentic | prose | AGENTS.md |
| `ORCH-OUTPUT-DIGEST-1` | agentic | prose | AGENTS.md |
| `ORCH-PRECISION-RECALL-1` | agentic | prose | AGENTS.md |
| `ORCH-REVIEWER-SPLIT-1` | agentic | structured | AGENTS.md |
| `ORCH-TIERED-ESCALATION-1` | agentic | prose | AGENTS.md |
| `ORCH-TRAINING-CUTOFF-1` | agentic | prose | AGENTS.md |

---

## Tier D: unknown-language tail (draft, no verification)

These 36 rules have no `verification` field (schema default = `draft`). They are candidates to ground
or cut before promotion. Listed by family.

| Rule ID | Domain | Enforcement | Status |
|---|---|---|---|
| `ARCH-API-DTOS-1` | api-layer | mechanical | draft — needs grounding or cut |
| `ARCH-API-VERSIONING-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-BOUNDARY-VALIDATION-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-CURSOR-PAGINATION-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-EXACT-DECIMALS-1` | api-layer | mechanical | draft — needs grounding or cut |
| `ARCH-EXPLICIT-TX-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-HOT-READ-CACHE-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-IDEMPOTENCY-KEYS-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-MIDDLEWARE-FIRST-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-REPO-PER-AGGREGATE-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-REPO-RETURNS-DOMAIN-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-SERVICE-DI-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-STRUCTURED-ERRORS-1` | api-layer | mechanical | draft — needs grounding or cut |
| `ARCH-TYPED-PATH-PARAMS-1` | api-layer | structured | draft — needs grounding or cut |
| `ARCH-UTC-TIMESTAMPS-1` | api-layer | structured | draft — needs grounding or cut |
| `PROC-FEATURE-FLAGS-1` | ci-cd | structured | draft — needs grounding or cut |
| `GO-HANDLER-SERVICE-REPOSITORY-1` | go | structured | draft — needs grounding or cut |
| `GO-WEB-MIDDLEWARE-CROSS-CUTTING-1` | go:web | structured | draft — needs grounding or cut |
| `GO-WEB-REQUEST-BINDING-VALIDATION-1` | go:web | structured | draft — needs grounding or cut |
| `GO-WEB-STRUCTURED-ERROR-RESPONSES-1` | go:web | structured | draft — needs grounding or cut |
| `GO-WEB-THIN-HANDLERS-DELEGATION-1` | go:web | structured | draft — needs grounding or cut |
| `JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1` | javascript:angular | structured | draft — needs grounding or cut |
| `RUST-DIOXUS-1` | rust:dioxus | structured | draft — needs grounding or cut |
| `RUST-DIOXUS-10` | rust:dioxus | structured | draft — needs grounding or cut |
| `RUST-DIOXUS-12` | rust:dioxus | structured | draft — needs grounding or cut |
| `RUST-DIOXUS-13` | rust:dioxus | structured | draft — needs grounding or cut |
| `RUST-DIOXUS-14` | rust:dioxus | structured | draft — needs grounding or cut |
| `RUST-DOMAIN-7` | rust | structured | draft — needs grounding or cut |
| `RUST-HEADLESS-CORE-1` | rust | structured | draft — needs grounding or cut |
| `RUST-MAPPER-1` | rust | structured | draft — needs grounding or cut |
| `RUST-PURE-STATE-TRANSITIONS-1` | rust | structured | draft — needs grounding or cut |
| `SQL-AUDIT-COLUMNS-1` | sql | structured | draft — needs grounding or cut |
| `UI-CONSENT-GATED-1` | ui | structured | draft — needs grounding or cut |
| `UI-IMAGE-COMPONENT-1` | ui | mechanical | draft — needs grounding or cut |
| `UI-QUERY-LIBRARY-1` | ui | structured | draft — needs grounding or cut |
| `UI-UTC-DATES-1` | ui | mechanical | draft — needs grounding or cut |

---

*Regenerate citation data: `cargo run -p camerata-linter-registry --example generate-report`*
