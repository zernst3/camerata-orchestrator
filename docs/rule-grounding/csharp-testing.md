# C# Testing Rule Grounding — Citation Report

Family: `csharp:testing`
Subdir: `crates/rules/principles/csharp/testing/`
Grounding completed: 2026-06-20
Branch: `tcr/csharp`

All 7 rules in this family carry `verification = "grounded"`. Each rule was
mapped to at least one real, publicly accessible authoritative source before
being set to `grounded`. No rule was set to `verification = "verified"` — that
status is human-only and is never set by an automated grounding pass.

---

## Rule table

| Rule ID | File | Enforcement | Primary Authority | Linter / Tool |
|---|---|---|---|---|
| CSHARP-TESTING-SEPARATE-UNIT-PROJECT-1 | `csharp-testing-separate-unit-project-1.toml` | prose | MS ASP.NET Core Integration Tests | none (structural convention) |
| CSHARP-TESTING-NAMING-METHOD-SCENARIO-RESULT-1 | `csharp-testing-naming-method-scenario-result-1.toml` | prose | MS Unit Testing Best Practices | none (naming convention) |
| CSHARP-TESTING-AAA-STRUCTURE-1 | `csharp-testing-aaa-structure-1.toml` | prose | MS Unit Testing Best Practices | none (structural convention) |
| CSHARP-TESTING-MOCK-AT-BOUNDARY-1 | `csharp-testing-mock-at-boundary-1.toml` | prose | MS Unit Testing Best Practices + Moq GitHub README | none (design pattern) |
| CSHARP-TESTING-DETERMINISTIC-NO-FLAKY-1 | `csharp-testing-deterministic-no-flaky-1.toml` | prose | MS Unit Testing Best Practices + xUnit shared-context docs | none (design rule) |
| CSHARP-TESTING-XUNIT-CONSTRUCTOR-ISOLATION-1 | `csharp-testing-xunit-constructor-isolation-1.toml` | prose | xUnit shared-context docs + MS Unit Testing Best Practices | none (xUnit pattern) |
| CSHARP-TESTING-INTEGRATION-WEBAPPLICATIONFACTORY-1 | `csharp-testing-integration-webapplicationfactory-1.toml` | prose | MS ASP.NET Core Integration Tests | none (framework API) |

---

## Why all 7 rules are `enforcement = "prose"` (not `mechanical`)

The mechanical bar requires a real, citeable linter rule that fires in CI
(e.g., a Roslyn analyzer with a stable CA/ASP rule ID, a clippy lint, or an
established tool like gitleaks). Testing-convention rules do not map to any
such linter for the following reasons:

1. **Project layout** (separate .UnitTests / .IntegrationTests) — no Roslyn
   CA rule checks that infrastructure packages are absent from a unit-test
   csproj at build time. StyleCop and SonarQube have no rule for this.
2. **Test method naming** — StyleCop rule SA1600 (member must have XML doc)
   is irrelevant. There is no canonical Roslyn CA rule that enforces the
   `MethodName_Scenario_Result` three-part pattern. xunit-analyzers has no
   naming-convention rule.
3. **AAA structure** — no linter enforces the presence of exactly one Act
   statement or three-region comments in a test body.
4. **Mock at boundary** — Moq ships no analyzer that detects mocking of
   concrete non-boundary classes.
5. **Determinism / no flaky tests** — xunit-analyzers (xunit2000-xunit2999
   range) flag specific anti-patterns (e.g., xUnit1013: public method should
   be marked as test), but no rule enforces injection of IClock or bans
   Thread.Sleep.
6. **xUnit constructor isolation** — xunit-analyzers has no rule that enforces
   use of constructor/Dispose vs. a [SetUp]-equivalent; the framework simply
   does not have [SetUp].
7. **WebApplicationFactory** — no analyzer enforces IClassFixture usage with
   WebApplicationFactory; it is a design pattern.

If a future xunit-analyzers version or a community Roslyn analyzer ships a
stable rule ID for any of the above, the affected rule should be upgraded to
`enforcement = "mechanical"` and the linter ID added to `[[sources]]`.

---

## Per-rule authority detail

### CSHARP-TESTING-SEPARATE-UNIT-PROJECT-1

**Primary source:** Microsoft ASP.NET Core Integration Tests documentation
(https://learn.microsoft.com/en-us/aspnet/core/test/integration-tests)

The doc states verbatim: "Separating the tests: Helps ensure that
infrastructure testing components aren't accidentally included in the unit
tests. Allows control over which set of tests are run." This is the canonical
authority for the separate-project convention.

**Secondary source:** Microsoft Unit Testing Best Practices
(https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices)
— "You can also keep your unit tests in a separate project from your
integration tests."

**Linter:** None. This is a solution/project structure convention; no tool
enforces it at build time.

---

### CSHARP-TESTING-NAMING-METHOD-SCENARIO-RESULT-1

**Primary source:** Microsoft Unit Testing Best Practices
(https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices)

The doc defines the three-part naming standard: "The name of your test should
consist of three parts: Name of the method being tested / Scenario under which
the method is being tested / Expected behavior when the scenario is invoked."
It provides before/after code examples (Test_Single → Add_SingleNumber_ReturnsSameNumber).

**Secondary source:** SSW Rules — naming conventions for tests and test
projects (https://www.ssw.com.au/rules/follow-naming-conventions-for-tests-and-test-projects)
confirms the `[Method]_[Condition]_[ExpectedResult]` convention with examples.

**Linter:** None with a stable canonical rule ID. xunit-analyzers does not
enforce a naming scheme.

---

### CSHARP-TESTING-AAA-STRUCTURE-1

**Primary source:** Microsoft Unit Testing Best Practices
(https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices)

Three separate sections of the doc are cited:
- "Arrange your tests" — explains AAA and shows before/after examples
- "Avoid multiple Act tasks" — explicitly forbids multiple Act calls in one test; recommends [Theory] + [InlineData]
- "Avoid coding logic in unit tests" — bans loops/conditionals; replaces with parameterized tests

**Linter:** None.

---

### CSHARP-TESTING-MOCK-AT-BOUNDARY-1

**Primary source:** Microsoft Unit Testing Best Practices
(https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices)

Two sections are directly cited:
- "Avoid infrastructure dependencies" — unit tests must not depend on
  infrastructure; that's reserved for integration tests.
- "Handle stub static references with seams" — provides the concrete
  DateTime.Now example, introduces IDateTimeProvider, and shows Moq stub
  usage (`dateTimeProviderStub.Setup(dtp => dtp.DayOfWeek()).Returns(DayOfWeek.Monday)`).

**Secondary source:** Moq GitHub README
(https://github.com/devlooped/moq)
Confirms the Setup/Returns/Verify pattern and the interface-first design.

**Linter:** None. Moq itself ships no Roslyn analyzer that rejects mocking
of concrete classes.

---

### CSHARP-TESTING-DETERMINISTIC-NO-FLAKY-1

**Primary source:** Microsoft Unit Testing Best Practices
(https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices)

The "Characteristics of good unit tests" section lists "Repeatable" as a
required property: "Running a unit test should be consistent with its results."
The "Handle stub static references with seams" section codifies the fix for
DateTime.Now non-determinism.

**Secondary source:** xUnit.net — Sharing Context between Tests
(https://xunit.net/docs/shared-context)

Documents that xUnit creates a new test-class instance per test (preventing
shared mutable instance state) and that IClassFixture / ICollectionFixture are
the correct mechanisms for sharing expensive state without mutation risk.

**Linter:** None. No xunit-analyzer rule enforces a ban on Thread.Sleep or
mandates IClock injection.

---

### CSHARP-TESTING-XUNIT-CONSTRUCTOR-ISOLATION-1

**Primary source:** xUnit.net — Sharing Context between Tests
(https://xunit.net/docs/shared-context)

Documents the three isolation tiers: constructor/Dispose (per-test),
IClassFixture (per-class), ICollectionFixture (cross-class). Explains that
xUnit v2 removed [SetUp]/[TearDown] by design. Also covers IAsyncLifetime
for async setup/teardown.

**Secondary source:** Microsoft Unit Testing Best Practices
(https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices)

"Use helper methods instead of Setup and Teardown" section — warns against
per-suite setup attributes and recommends per-test helper methods or
constructors. Also notes: "The SetUp and TearDown attributes are removed in
xUnit version 2.x and later."

**Linter:** None.

---

### CSHARP-TESTING-INTEGRATION-WEBAPPLICATIONFACTORY-1

**Primary source:** Microsoft ASP.NET Core Integration Tests documentation
(https://learn.microsoft.com/en-us/aspnet/core/test/integration-tests)

Documents the WebApplicationFactory<TProgram> pattern in detail: project
prerequisites (Microsoft.AspNetCore.Mvc.Testing, Web SDK), the
CustomWebApplicationFactory subclass pattern with ConfigureWebHost, and how
to use IClassFixture<CustomWebApplicationFactory<Program>> to share the
factory across tests.

**Linter:** None. This is a framework API pattern, not a lint rule.

---

## Rules without a linter ID (all 7 — documented list)

Per the task honesty constraint: if a rule has no real linter rule ID, it is
documented as `enforcement = "prose"` rather than `enforcement = "mechanical"`.

All 7 rules in this family fall into this category. The testing-convention
space for .NET is currently under-served by static analysis: StyleCop focuses
on code style, not test structure; the xunit-analyzers package focuses on
correctness of xUnit API usage (e.g., using Assert.Equal correctly), not on
AAA structure, naming conventions, or project layout. If that changes, the
affected rules should be upgraded.

---

## Authorities used

| Authority | URL | Coverage in this corpus |
|---|---|---|
| MS Unit Testing Best Practices | https://learn.microsoft.com/en-us/dotnet/core/testing/unit-testing-best-practices | Naming, AAA, mocking seams, determinism, Setup/Teardown removal |
| MS ASP.NET Core Integration Tests | https://learn.microsoft.com/en-us/aspnet/core/test/integration-tests | Separate projects, WebApplicationFactory |
| xUnit.net Shared Context | https://xunit.net/docs/shared-context | Constructor isolation, IClassFixture, ICollectionFixture, IAsyncLifetime |
| Moq GitHub README | https://github.com/devlooped/moq | Setup/Returns/Verify pattern, interface-first mocking |
| SSW Rules — test naming | https://www.ssw.com.au/rules/follow-naming-conventions-for-tests-and-test-projects | MethodName_Scenario_Result convention corroboration |
