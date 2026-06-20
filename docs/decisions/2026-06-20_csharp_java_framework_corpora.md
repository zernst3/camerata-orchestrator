# C# and Java Framework Corpora

Date: 2026-06-20
Status: Accepted
Deciders: Zach (PO), Claude (architect)

## Context

Camerata's rule corpus covered universal, SQL, API-layer, CI/CD, agentic, and Rust
framework rules, but had no first-class coverage for C# or Java frameworks. The
existing `csharp/` and `java/` root-level files used `domain = "csharp:aspnet"` and
`domain = "java:spring"` for language-level rules. Zach identified C# and Java as
user-priority languages and requested dedicated framework subdirectories for:

- C# ASP.NET Core (domain `csharp:aspnetcore`) — controller/API surface, middleware,
  validation, async, configuration, CORS, authorization
- C# Entity Framework Core (domain `csharp:efcore`) — N+1, change tracking,
  parameterized LINQ, DbContext lifetime, migrations, lazy loading, transactions
- Java Spring (domain `java:spring`) — constructor injection, @Transactional placement,
  DTO boundary, @Valid validation, N+1 with JPA fetch joins, layered architecture,
  @ConfigurationProperties, method security, thin controllers

## Directories Created

```
crates/rules/principles/csharp/aspnetcore/   (7 rules)
crates/rules/principles/csharp/efcore/       (7 rules)
crates/rules/principles/java/spring/         (8 rules)
```

## Rules Added

### C# ASP.NET Core (`csharp:aspnetcore`)

| Rule ID | Title | Enforcement |
|---|---|---|
| CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1 | Minimal APIs vs controllers consistency | structured |
| CSHARP-ASPNETCORE-MODEL-VALIDATION-1 | [ApiController] automatic 400 responses | mechanical |
| CSHARP-ASPNETCORE-MIDDLEWARE-ORDERING-1 | Canonical middleware order | structured |
| CSHARP-ASPNETCORE-ASYNC-ACTIONS-1 | All I/O-bound actions are async | mechanical |
| CSHARP-ASPNETCORE-THIN-CONTROLLERS-1 | No business logic in controllers | structured |
| CSHARP-ASPNETCORE-OPTIONS-PATTERN-1 | Options pattern for configuration | structured |
| CSHARP-ASPNETCORE-CORS-EXPLICIT-1 | Explicit named CORS origins | structured |
| CSHARP-ASPNETCORE-AUTHORIZE-ATTRIBUTE-1 | [Authorize] at controller/group level | structured |

### C# Entity Framework Core (`csharp:efcore`)

| Rule ID | Title | Enforcement |
|---|---|---|
| CSHARP-EFCORE-NO-NPLUS1-1 | Include/projection for related data; no lazy N+1 | structured |
| CSHARP-EFCORE-ASNOTRACKING-1 | AsNoTracking for read-only queries | structured |
| CSHARP-EFCORE-PARAMETERIZED-LINQ-1 | LINQ parameters; no interpolated raw SQL | mechanical |
| CSHARP-EFCORE-DBCONTEXT-SCOPED-1 | DbContext registered as Scoped | mechanical |
| CSHARP-EFCORE-MIGRATIONS-CHECKED-IN-1 | Migrations checked into source control | structured |
| CSHARP-EFCORE-NO-LAZY-LOADING-1 | Lazy loading disabled | mechanical |
| CSHARP-EFCORE-EXPLICIT-TRANSACTIONS-1 | Explicit transactions for multi-write operations | structured |

### Java Spring (`java:spring`)

| Rule ID | Title | Enforcement |
|---|---|---|
| JAVA-SPRING-CONSTRUCTOR-INJECTION-1 | Constructor injection; final fields; no @Autowired fields | mechanical |
| JAVA-SPRING-TRANSACTIONAL-SERVICE-LAYER-1 | @Transactional on service layer only | structured |
| JAVA-SPRING-DTO-BOUNDARY-1 | DTOs at HTTP boundary; no JPA entities in responses | structured |
| JAVA-SPRING-VALID-REQUEST-VALIDATION-1 | @Valid on @RequestBody; global 400 handler | mechanical |
| JAVA-SPRING-NO-NPLUS1-FETCH-JOIN-1 | Fetch joins / @EntityGraph for associations | structured |
| JAVA-SPRING-LAYERED-ARCHITECTURE-1 | Strict Controller -> Service -> Repository hierarchy | structured |
| JAVA-SPRING-CONFIGURATION-PROPERTIES-1 | @ConfigurationProperties for grouped config | structured |
| JAVA-SPRING-METHOD-SECURITY-1 | @PreAuthorize on service methods | structured |
| JAVA-SPRING-THIN-CONTROLLERS-1 | No business logic in @RestController | structured |

## Design Decisions

### Domain naming convention

Framework rules under a language subdirectory use `domain = "<lang>:<framework>"` to
match the existing convention set by `javascript:next`, `javascript:express`, and
`rust:seaorm`. The existing root-level C# and Java files (which are language-level
rules, not framework-specific) retain their domains and are not moved; they are
complementary.

### One default per framework, alternatives document trade-offs

Each rule picks a safe, opinionated default and documents 1-2 alternatives with honest
`why` rationale. The alternatives are not wrong answers — they are the positions a team
might reasonably take for a different context (e.g., a public unauthenticated API where
`AllowAnyOrigin` CORS is correct; a Singleton DbContext for a single-thread console
tool).

### Mechanical vs structured enforcement

`enforcement = "mechanical"` is used when a Roslyn analyzer, Checkstyle rule, or
Semgrep pattern can deterministically detect violations at CI time. `enforcement =
"structured"` is used when the rule is structural/architectural and requires code review
to enforce. All mechanical rules carry a `qualifies` field describing the specific
tooling that provides enforcement.

### CSHARP-EFCORE-ASNOTRACKING-1 domain choice

This rule logically belongs to the EF Core library layer but its natural placement in
TOML uses `domain = "csharp:aspnetcore"` because AsNoTracking is a query-construction
discipline for request-scoped contexts. The intent is EF Core in an ASP.NET Core
context; teams using EF Core outside ASP.NET would still apply the same rule but its
trigger context is web requests.

## Test Result

`cargo test -p camerata-rules` passes: 39 tests + 1 doc-test, 0 failures.
