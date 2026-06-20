# Java Rule Corpus: Security, Architecture, and Idiom Rules

Date: 2026-06-20
Status: Accepted
Deciders: Claude (author), implied Zach (PO)

## Context

The camerata rules system provides a corpus of principle rules for multiple programming languages, supporting code governance and design automation. Python and Go corpora are well-established (12 and 13 rules respectively); this decision records the addition of a Java rule corpus spanning security, architectural patterns, and idiomatic practices.

Java is a common backend language in microservices, Spring Boot applications, and enterprise systems. A focused corpus of principles helps teams avoid common pitfalls (SQL injection, resource leaks, hardcoded secrets) and encourages consistent architectural patterns (layering, dependency injection, package organization).

## Decision

**Add a Java rule corpus with 12 foundational rules**, organized by concern:

### Security Rules (4)

1. **JAVA-SQL-PARAMETERIZED-1** (mechanical): SQL queries use PreparedStatement or JPA parameters; never concatenated strings. Prevents SQL injection.

2. **JAVA-NO-HARDCODED-SECRETS-1** (mechanical): Credentials and keys are loaded from environment variables or secure vaults; never embedded in source. Prevents credential leaks.

3. **JAVA-EXCEPTION-HANDLING-1** (mechanical): Exceptions are never silently swallowed; every catch block logs, re-throws, or transforms with context. Prevents silent failures and enables diagnostics.

4. **JAVA-RESOURCE-MANAGEMENT-1** (mechanical): Resources (connections, streams, readers) are closed via try-with-resources or finally blocks; never left to garbage collection. Prevents resource leaks.

### Architectural Rules (3)

5. **JAVA-LAYERING-CONTROLLER-SERVICE-REPO-1** (prose): Three-tier layering (controllers → services → repositories) is enforced; no database queries in controllers, no business logic in repositories. Enables testability and reusability.

6. **JAVA-SMALL-INTERFACES-1** (prose): Interfaces are small and focused with 1-3 methods; avoids god-interfaces. Improves implementability and testability.

7. **JAVA-PACKAGE-BY-FEATURE-1** (prose): Packages are organized by feature/domain (e.g., `com.example.users`, `com.example.organizations`), not by layer. Groups related code and makes dependencies explicit.

### Idiom Rules (5)

8. **JAVA-CONSTRUCTOR-INJECTION-1** (mechanical): Dependencies are injected via constructor parameters marked final; no @Autowired field injection. Enforces immutability and improves testability.

9. **JAVA-OPTIONAL-OVER-NULL-1** (prose): Methods return Optional<T> instead of null; null is used only for legacy compatibility. Makes absence explicit and prevents NullPointerException.

10. **JAVA-IMMUTABILITY-FINAL-1** (prose): Classes and fields are marked final by default; mutability is explicit and intentional. Prevents bugs and enables thread-safety.

11. **JAVA-LOGGING-STRUCTURED-1** (prose): Logging uses a structured framework (slf4j, Logback, Log4j2) with machine-readable output; no System.out.println(). Enables centralized logging and debugging.

12. **JAVA-JPA-EAGER-LOAD-1** (mechanical): JPA relationships default to lazy loading; eager loading is explicit to prevent N+1 queries. Improves query performance.

## Format

Each rule follows the camerata corpus TOML schema:
- **id**: `JAVA-<CONCEPT>-<TIER>` (e.g., `JAVA-SQL-PARAMETERIZED-1`)
- **title**: concise, active-voice description
- **tag**: `"stack"` (applies to business code)
- **domain**: `"java"` or `"java:spring"` (Spring-specific)
- **layer**: `"language"`, `"architecture"`, or `"security"`
- **enforcement**: `"mechanical"` (detectable by static analysis / linter) or `"prose"` (code-review enforced)
- **default**: `true` (all rules adopted as default)
- **decision**: `question` (what is being decided), `default` (adopted answer), `why` (rationale)
- **options**: alternative choices with labels, directives, and reasoning

All rules are consistent with existing Python and Go corpora in structure and philosophy.

## Rationale

1. **Mechanical enforcement where possible**: SQL injection, hardcoded secrets, exception handling, resource leaks, and constructor injection can be detected and gated by static analysis, reducing reliance on code review.

2. **Prose rules for architecture and idioms**: Layering, interfaces, package organization, Optional vs. null, immutability, and structured logging are enforced by convention and review; static gates are less reliable.

3. **Spring-specific where appropriate**: Constructor injection and JPA loading patterns are Spring idioms; domain is marked `"java:spring"` to allow filtering if non-Spring Java projects need a different ruleset.

4. **Coverage breadth**: The corpus covers the most common Java pitfalls and best practices from OWASP (injection, resource leaks), SOLID principles, and Spring documentation.

5. **Compatibility with camerata tooling**: All rules load into the camerata rules engine without modification; the corpus integrates seamlessly with existing domain selection and enforcement-tier partitioning.

## Usage

Projects using the Java corpus can select rules by domain (`java`), enforcement tier, or individual rule ID. Example queries:

- `cargo run -- select --domain java` — all Java rules
- `cargo run -- select --domain java --enforcement mechanical` — Java rules detectable by static analysis
- `cargo run -- select --id JAVA-SQL-PARAMETERIZED-1` — a single rule
- `cargo run -- select --domain java:spring` — Spring-specific rules only

The corpus is the source of truth for governance decisions; teams configure linters (SpotBugs, Checkstyle, Semgrep) and code-review templates to reflect these rules.

## Alternatives Considered

1. **Fewer rules, more selective scope**: A minimal corpus (5-6 rules, security-only) would be easier to adopt but leaves architectural and idiom gaps. This decision opts for breadth to serve multiple concerns.

2. **Language-specific sub-versions**: `java-spring.toml` vs. `java-plain.toml` for Spring vs. plain Java projects. Rejected; `domain = "java:spring"` filtering is sufficient and keeps the corpus unified.

3. **Integration with existing language rules**: Some rules (structured logging, immutability) mirror principles in Python and Go corpora. This is intentional — principle consistency across languages is a feature, not duplication.

## Files

- `crates/rules/principles/java/java-*.toml` (12 corpus files, new directory)

## Testing

The corpus loads and passes all existing camerata-rules unit tests:
- `cargo test -p camerata-rules` — 39 tests pass; no regressions
- TOML parsing is validated; all rules have valid domain, enforcement tier, decision, and option fields
- No Rust source files modified; corpus-only addition

## Next Steps

1. (Optional) Add linter configuration examples in each rule (e.g., Semgrep patterns, SpotBugs plugins) for teams implementing governance.
2. (Optional) Extend to Android-specific rules if needed (JUnit test patterns, lifecycle management).
3. Catalog the Java corpus in the Camerata User Guide (`docs/USER_GUIDE.md`) for discoverability.
