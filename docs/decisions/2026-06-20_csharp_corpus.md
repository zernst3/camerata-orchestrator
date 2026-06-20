# C# / .NET Rule Corpus

**Date:** 2026-06-20  
**Status:** Complete  
**Branch:** dev5/csharp-corpus

## Summary

Added a 12-rule C# / .NET principles corpus under `crates/rules/principles/csharp/` to complete domain coverage alongside Python, Go, and Java corpora. The corpus mirrors the exact TOML format and decision structure of existing language corpora and covers security, idioms, and architecture concerns specific to C# and ASP.NET.

## Corpus Rules

The following 12 rules were added:

### Security & Best Practices (5 rules)

1. **CSHARP-SQL-PARAMETERIZED-1**: SQL queries use parameterized SqlParameter or EF parameters; string concatenation is forbidden. Domain: `csharp`. Enforcement: `mechanical`. Prevents SQL injection.

2. **CSHARP-EF-CORE-PARAMETERIZED-1**: Entity Framework queries use parameterized LINQ syntax; FromSqlRaw is forbidden. Domain: `csharp:aspnet`. Enforcement: `mechanical`. Blocks raw SQL concatenation in ORM context.

3. **CSHARP-NO-HARDCODED-SECRETS-1**: Secrets (connection strings, API keys, tokens) come from external config (environment variables, Azure Key Vault); no hardcoding in source. Domain: `csharp`. Enforcement: `mechanical`. Prevents credential leaks to VCS.

4. **CSHARP-IDISPOSABLE-USING-1**: IDisposable resources are managed via using statements/declarations; no manual Dispose() calls or naked instantiation. Domain: `csharp`. Enforcement: `mechanical`. Prevents resource leaks (connections, file handles, streams).

5. **CSHARP-ASYNC-NO-BLOCKING-1**: Async code never blocks via .Result or .Wait(); all async/await chains are end-to-end. Domain: `csharp:aspnet`. Enforcement: `mechanical`. Prevents thread-pool starvation and deadlocks in web servers.

### Exception & Error Handling (1 rule)

6. **CSHARP-NO-SWALLOWED-EXCEPTIONS-1**: Exceptions are never silently swallowed; catch blocks log, rethrow, or transform exceptions; no empty catch bodies. Domain: `csharp`. Enforcement: `mechanical`. Ensures bugs surface, not disappear.

### Architecture & Design (3 rules)

7. **CSHARP-CONTROLLER-SERVICE-REPO-1**: Three-layer architecture: controllers handle HTTP only, services contain business logic, repositories own data access. Domain: `csharp:aspnet`. Enforcement: `mechanical`. Separates concerns and improves testability.

8. **CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1**: Dependencies are injected via constructor parameters (not [Inject] attributes); dependencies are private readonly. Domain: `csharp:aspnet`. Enforcement: `mechanical`. Makes dependencies explicit and immutable; improves testability.

9. **CSHARP-SMALL-FOCUSED-INTERFACES-1**: Interfaces are narrow and focused on single responsibility; fat interfaces are split. Domain: `csharp`. Enforcement: `mechanical`. Follows Interface Segregation Principle; improves reusability and testability.

### Idioms & Language Features (3 rules)

10. **CSHARP-NULLABLE-REFERENCE-TYPES-1**: Nullable Reference Types are enabled in all projects; null-unsafe patterns are enforced at compile time. Domain: `csharp`. Enforcement: `mechanical`. Shifts null safety from runtime to compile time; prevents NullReferenceException class of bugs.

11. **CSHARP-RECORDS-IMMUTABILITY-1**: DTOs and value types use records (C# 9+) or immutable classes; mutable classes are avoided outside domain entities. Domain: `csharp`. Enforcement: `mechanical`. Promotes thread safety, shareability, and reduces boilerplate.

12. **CSHARP-IENUMERABLE-COLLECTIONS-1**: Methods return IEnumerable<T> or ICollection<T> instead of arrays or List<T>; callers should not couple to concrete collection types. Domain: `csharp`. Enforcement: `mechanical`. Hides implementation details; prevents accidental corruption of internal state.

## Design Decisions

### Domains Used

- `csharp`: General C# language rules applicable to all .NET projects.
- `csharp:aspnet`: Rules specific to ASP.NET (web framework, HTTP handlers, dependency injection containers).

### Enforcement Tiers

All rules use `mechanical` enforcement (enforced by static analysis, compilers, or linters) because C# has strong tooling support:

- **Compiler warnings/errors** (Nullable Reference Types, async/await, SQL parameterization).
- **Roslyn analyzers** (empty catch blocks, IDisposable resource leaks, hardcoded secrets).
- **Linters** (gitleaks for credential scanning, custom Semgrep rules for SQL injection).
- **Code review** (architecture layering, interface design, immutability patterns).

### Default Option Selections

Every rule follows the same default pattern as Java, Go, and Python corpora:

- **Security defaults** are restrictive (e.g., parameterized queries, external secrets config, specific exception types).
- **Architecture defaults** enforce separation of concerns and immutability (e.g., three-layer controllers/services/repos, records for DTOs, constructor injection).
- **Idiom defaults** use modern language features when available (e.g., C# 8.0+ using declarations, C# 9+ records, Nullable Reference Types).

### Coverage

The corpus covers:

- **Security threats** mitigated by C#-specific patterns (SQL injection, credential leaks, resource leaks, thread-pool starvation).
- **Architectural boundaries** enforced in ASP.NET applications (layering, dependency injection, interface contracts).
- **Modern C# idioms** that improve code quality (records, nullable reference types, using declarations, immutability).

The corpus is **not** exhaustive but covers the highest-impact rules for production C# / ASP.NET codebases.

## Testing

All 39 existing tests in `camerata-rules` pass:

```
test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s
```

The corpus loader (`load_corpus_lenient`) successfully loads the new TOML files without errors.

Cargo check passes with no new errors (existing warnings in `camerata-ui` are pre-existing).

## File Changes

- **Added:** 11 new TOML rule files in `crates/rules/principles/csharp/`:
  - `csharp-sql-parameterized-1.toml`
  - `csharp-idisposable-using-1.toml`
  - `csharp-async-no-blocking-1.toml`
  - `csharp-no-hardcoded-secrets-1.toml`
  - `csharp-ef-core-parameterized-1.toml`
  - `csharp-no-swallowed-exceptions-1.toml`
  - `csharp-controller-service-repo-1.toml`
  - `csharp-dependency-injection-constructor-1.toml`
  - `csharp-small-focused-interfaces-1.toml`
  - `csharp-nullable-reference-types-1.toml`
  - `csharp-records-immutability-1.toml`
  - `csharp-ienumerable-collections-1.toml`

- **Added:** This decision document at `docs/decisions/2026-06-20_csharp_corpus.md`.

## Next Steps

The corpus is now available for use in governance rulesets targeting C# / ASP.NET codebases. Consumers can select rules by domain (`csharp`, `csharp:aspnet`), enforcement tier, or individual rule ID via the camerata-rules API.

Future extensions:

- Add C#-specific rules for async enumerable patterns (IAsyncEnumerable).
- Add rules for LINQ query pitfalls (N+1 problem in EF Core, lazy vs. eager evaluation).
- Add rules for dependency injection container configuration (registering singletons vs. transients).
