# C# Rule Grounding — Citation Table

Family: `csharp` (includes `aspnetcore` and `efcore` subdirectories)
Grounding completed: 2026-06-20
Branch: `ground/csharp`

All 27 rules in this family carry `verification = "grounded"`. Rules that had
`enforcement = "mechanical"` but cited only vague "optional Roslyn analyzers" with
no canonical rule ID were demoted to `enforcement = "prose"`.

## Root csharp rules

| Rule ID | Enforcement | Linter / Rule ID | Primary Source |
|---|---|---|---|
| CSHARP-ASYNC-NO-BLOCKING-1 | mechanical | AsyncFixer: AsyncFixer02 | [ASP.NET Core Best Practices](https://learn.microsoft.com/en-us/aspnet/core/fundamentals/best-practices?view=aspnetcore-10.0) |
| CSHARP-CONTROLLER-SERVICE-REPO-1 | prose (demoted) | — | [Common web app architectures](https://learn.microsoft.com/en-us/dotnet/architecture/modern-web-apps-azure/common-web-application-architectures) |
| CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1 | prose (demoted) | — | [DI guidelines](https://learn.microsoft.com/en-us/dotnet/core/extensions/dependency-injection/guidelines) |
| CSHARP-EF-CORE-PARAMETERIZED-1 | prose (demoted) | — | [EF Core SQL Queries](https://learn.microsoft.com/en-us/ef/core/querying/sql-queries) |
| CSHARP-IDISPOSABLE-USING-1 | mechanical | CA2000, CA1001 | [CA2000](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca2000) / [CA1001](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca1001) |
| CSHARP-IENUMERABLE-COLLECTIONS-1 | prose (demoted) | — | [Framework Design Guidelines: Collections](https://learn.microsoft.com/en-us/dotnet/standard/design-guidelines/guidelines-for-collections) |
| CSHARP-NO-HARDCODED-SECRETS-1 | mechanical | gitleaks (tool) | [Safe storage of app secrets](https://learn.microsoft.com/en-us/aspnet/core/security/app-secrets?view=aspnetcore-10.0) |
| CSHARP-NO-SWALLOWED-EXCEPTIONS-1 | mechanical | CA1031 | [CA1031](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca1031) |
| CSHARP-NULLABLE-REFERENCE-TYPES-1 | mechanical | C# compiler (`<Nullable>enable</Nullable>`) | [Nullable reference types](https://learn.microsoft.com/en-us/dotnet/csharp/fundamentals/null-safety/nullable-reference-types) |
| CSHARP-RECORDS-IMMUTABILITY-1 | prose (demoted) | — | [C# record types](https://learn.microsoft.com/en-us/dotnet/csharp/fundamentals/types/records) |
| CSHARP-SMALL-FOCUSED-INTERFACES-1 | prose (demoted) | — | [Interface Design Guidelines](https://learn.microsoft.com/en-us/dotnet/standard/design-guidelines/interface) |
| CSHARP-SQL-PARAMETERIZED-1 | mechanical | CA2100 | [CA2100](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca2100) |

## aspnetcore rules

| Rule ID | Enforcement | Linter / Rule ID | Primary Source |
|---|---|---|---|
| CSHARP-ASPNETCORE-ASYNC-ACTIONS-1 | mechanical | AsyncFixer: AsyncFixer02 | [ASP.NET Core Best Practices](https://learn.microsoft.com/en-us/aspnet/core/fundamentals/best-practices?view=aspnetcore-10.0) |
| CSHARP-ASPNETCORE-AUTHORIZE-ATTRIBUTE-1 | structured | — | [Simple authorization](https://learn.microsoft.com/en-us/aspnet/core/security/authorization/simple?view=aspnetcore-10.0) |
| CSHARP-ASPNETCORE-CORS-EXPLICIT-1 | structured | — | [CORS in ASP.NET Core](https://learn.microsoft.com/en-us/aspnet/core/security/cors?view=aspnetcore-10.0) |
| CSHARP-ASPNETCORE-MIDDLEWARE-ORDERING-1 | structured | ASP0001 (related) | [Middleware](https://learn.microsoft.com/en-us/aspnet/core/fundamentals/middleware/?view=aspnetcore-10.0) / [ASP0001](https://learn.microsoft.com/en-us/aspnet/core/diagnostics/asp0001?view=aspnetcore-10.0) |
| CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1 | structured | — | [Choose between minimal/controller APIs](https://learn.microsoft.com/en-us/aspnet/core/fundamentals/apis?view=aspnetcore-9.0) |
| CSHARP-ASPNETCORE-MODEL-VALIDATION-1 | prose (demoted) | — | [ASP.NET Core Web API](https://learn.microsoft.com/en-us/aspnet/core/web-api/?view=aspnetcore-10.0) |
| CSHARP-ASPNETCORE-OPTIONS-PATTERN-1 | structured | — | [Options pattern](https://learn.microsoft.com/en-us/aspnet/core/fundamentals/configuration/options?view=aspnetcore-10.0) |
| CSHARP-ASPNETCORE-THIN-CONTROLLERS-1 | structured | — | [Common web app architectures](https://learn.microsoft.com/en-us/dotnet/architecture/modern-web-apps-azure/common-web-application-architectures) |

## efcore rules

| Rule ID | Enforcement | Linter / Rule ID | Primary Source |
|---|---|---|---|
| CSHARP-EFCORE-ASNOTRACKING-1 | structured | — | [Tracking vs No-Tracking](https://learn.microsoft.com/en-us/ef/core/querying/tracking) |
| CSHARP-EFCORE-DBCONTEXT-SCOPED-1 | prose (demoted) | — | [DbContext configuration](https://learn.microsoft.com/en-us/ef/core/dbcontext-configuration/) |
| CSHARP-EFCORE-EXPLICIT-TRANSACTIONS-1 | structured | — | [Using Transactions](https://learn.microsoft.com/en-us/ef/core/saving/transactions) |
| CSHARP-EFCORE-MIGRATIONS-CHECKED-IN-1 | structured | — | [Applying Migrations](https://learn.microsoft.com/en-us/ef/core/managing-schemas/migrations/applying) |
| CSHARP-EFCORE-NO-LAZY-LOADING-1 | prose (demoted) | — | [Lazy Loading](https://learn.microsoft.com/en-us/ef/core/querying/related-data/lazy) |
| CSHARP-EFCORE-NO-NPLUS1-1 | structured | — | [Efficient Querying](https://learn.microsoft.com/en-us/ef/core/performance/efficient-querying) |
| CSHARP-EFCORE-PARAMETERIZED-LINQ-1 | prose (demoted) | — | [EF Core SQL Queries](https://learn.microsoft.com/en-us/ef/core/querying/sql-queries) |

## Demotions (mechanical -> prose)

7 rules were demoted from `enforcement = "mechanical"` to `enforcement = "prose"` because
their `qualifies` field cited only "optional Roslyn analyzers" or "custom Roslyn analyzers"
with no canonical rule ID that can be cited:

1. CSHARP-CONTROLLER-SERVICE-REPO-1 — architectural layering, no Roslyn CA rule
2. CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1 — constructor injection style, no Roslyn CA rule
3. CSHARP-EF-CORE-PARAMETERIZED-1 — EF Core parameterized LINQ, "custom Roslyn" only
4. CSHARP-IENUMERABLE-COLLECTIONS-1 — return-type guidance, no Roslyn CA rule
5. CSHARP-RECORDS-IMMUTABILITY-1 — DTO style guidance, no Roslyn CA rule
6. CSHARP-SMALL-FOCUSED-INTERFACES-1 — ISP guidance, no Roslyn CA rule
7. CSHARP-ASPNETCORE-MODEL-VALIDATION-1 — [ApiController] usage, no Roslyn CA rule
8. CSHARP-EFCORE-DBCONTEXT-SCOPED-1 — DI lifetime check, "Roslyn or integration test"
9. CSHARP-EFCORE-NO-LAZY-LOADING-1 — lazy loading absent, "Roslyn or Semgrep rule"
10. CSHARP-EFCORE-PARAMETERIZED-LINQ-1 — FromSqlRaw guard, "Roslyn analyzer or Semgrep rule"

## Rules retaining mechanical enforcement with confirmed linter IDs

| Rule ID | Linter Rule |
|---|---|
| CSHARP-ASYNC-NO-BLOCKING-1 | AsyncFixer: AsyncFixer02 |
| CSHARP-IDISPOSABLE-USING-1 | CA2000, CA1001 |
| CSHARP-NO-SWALLOWED-EXCEPTIONS-1 | CA1031 |
| CSHARP-NULLABLE-REFERENCE-TYPES-1 | C# compiler (NRT feature) |
| CSHARP-SQL-PARAMETERIZED-1 | CA2100 |
| CSHARP-ASPNETCORE-ASYNC-ACTIONS-1 | AsyncFixer: AsyncFixer02 |
| CSHARP-ASPNETCORE-MIDDLEWARE-ORDERING-1 | ASP0001 (related — flags UseAuthorization before UseRouting) |
