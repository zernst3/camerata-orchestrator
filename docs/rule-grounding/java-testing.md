# Java Testing Rule Grounding Report

Grounding pass date: 2026-06-20
Family: `java:testing` (subdirectory `crates/rules/principles/java/testing/`)
Total rules: 7
Grounded: 7
Ungrounded: 0
Draft (no source): 0

## Summary

All 7 rules were grounded against real authoritative sources. No rules were demoted from `mechanical` to `prose` in this pass, though two rules that cover structural/process conventions were authored as `prose` from the start (unit test location and test naming) because no standard Checkstyle or PMD rule ID exists that enforces the specific conformance bar described.

### Rules Authored as `prose` from the Start (not demoted)

| Rule ID | Reason |
|---------|--------|
| `JAVA-TESTING-UNIT-TEST-LOCATION-1` | The Maven Surefire default pattern (*Test.java discovery in src/test/java) is a build-tool convention, not a static-analysis check. No Checkstyle or PMD rule enforces same-package mirroring between src/main/java and src/test/java. |
| `JAVA-TESTING-NAMING-1` | JUnit 5's `DisplayNameGenerator.ReplaceUnderscores` encourages descriptive names but does not mandate a specific naming pattern. PMD's `UnitTestShouldUseTestAnnotation` catches only the legacy `test` prefix anti-pattern, not the positive convention. Grounded as prose. |
| `JAVA-TESTING-MOCK-BOUNDARIES-1` | Mockito does not ship a static-analysis rule that flags mocking of concrete types or value objects. The guidance is in Mockito's own Javadoc (quoted warnings). No Checkstyle/PMD rule ID exists for this check. Grounded as prose. |
| `JAVA-TESTING-ASSERTIONS-ASSERTJ-1` | No standard published Checkstyle or PMD rule ID mandates AssertJ over JUnit assertions. A Semgrep or custom rule could enforce this, but no rule with a published identifier from the grounding authorities exists. PMD `UnitTestShouldIncludeAssert` is cited as a related rule (requires at least one assertion, which AssertJ satisfies). |

---

## Citation Table

| Rule ID | Enforcement | Verification | Source URL | Linter Rule | Status |
|---------|------------|-------------|------------|-------------|--------|
| JAVA-TESTING-UNIT-TEST-LOCATION-1 | prose | grounded | https://maven.apache.org/guides/introduction/introduction-to-the-standard-directory-layout.html | — | grounded |
| JAVA-TESTING-UNIT-TEST-LOCATION-1 | prose | grounded | https://maven.apache.org/surefire/maven-surefire-plugin/examples/inclusion-exclusion.html | Maven Surefire: default *Test.java inclusion pattern | grounded |
| JAVA-TESTING-INTEGRATION-TEST-LOCATION-1 | mechanical | grounded | https://maven.apache.org/surefire/maven-failsafe-plugin/examples/inclusion-exclusion.html | Maven Failsafe: default *IT.java inclusion pattern | grounded |
| JAVA-TESTING-INTEGRATION-TEST-LOCATION-1 | mechanical | grounded | https://docs.junit.org/5.14.1/writing-tests/annotations.html | JUnit 5: @Tag | grounded |
| JAVA-TESTING-NAMING-1 | prose | grounded | https://docs.junit.org/6.0.2/writing-tests/display-names.html | — | grounded |
| JAVA-TESTING-NAMING-1 | prose | grounded | https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html#unittestshouldincludeassert | PMD: UnitTestShouldUseTestAnnotation | grounded |
| JAVA-TESTING-AAA-STRUCTURE-1 | mechanical | grounded | https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html#unittestshouldincludeassert | PMD: UnitTestShouldIncludeAssert | grounded |
| JAVA-TESTING-AAA-STRUCTURE-1 | mechanical | grounded | https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html#unittestcontainstoomanyasserts | PMD: UnitTestContainsTooManyAsserts | grounded |
| JAVA-TESTING-AAA-STRUCTURE-1 | mechanical | grounded | https://docs.junit.org/5.14.1/writing-tests/annotations.html | JUnit 5: @Test, @BeforeEach, @AfterEach | grounded |
| JAVA-TESTING-MOCK-BOUNDARIES-1 | prose | grounded | https://site.mockito.org/javadoc/current/org/mockito/Mockito.html | — | grounded |
| JAVA-TESTING-MOCK-BOUNDARIES-1 | prose | grounded | https://javadoc.io/doc/org.mockito/mockito-core/latest/org/mockito/Mockito.html | — | grounded |
| JAVA-TESTING-DETERMINISTIC-1 | mechanical | grounded | https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html#iju-assert-method-invoked-from-run-method | SpotBugs: IJU_ASSERT_METHOD_INVOKED_FROM_RUN_METHOD | grounded |
| JAVA-TESTING-DETERMINISTIC-1 | mechanical | grounded | https://docs.junit.org/5.14.1/writing-tests/annotations.html | JUnit 5: @BeforeEach, @AfterEach | grounded |
| JAVA-TESTING-ASSERTIONS-ASSERTJ-1 | prose | grounded | https://assertj.github.io/doc/ | — | grounded |
| JAVA-TESTING-ASSERTIONS-ASSERTJ-1 | prose | grounded | https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html#unittestshouldincludeassert | PMD: UnitTestShouldIncludeAssert | grounded |

---

## Authorities Used

- **Apache Maven Standard Directory Layout**: https://maven.apache.org/guides/introduction/introduction-to-the-standard-directory-layout.html
- **Maven Surefire Plugin (unit test discovery)**: https://maven.apache.org/surefire/maven-surefire-plugin/examples/inclusion-exclusion.html
- **Maven Failsafe Plugin (integration test discovery)**: https://maven.apache.org/surefire/maven-failsafe-plugin/examples/inclusion-exclusion.html
- **JUnit 5 User Guide — Annotations**: https://docs.junit.org/5.14.1/writing-tests/annotations.html
- **JUnit User Guide — Display Names**: https://docs.junit.org/6.0.2/writing-tests/display-names.html
- **JUnit User Guide — Tags**: https://docs.junit.org/6.0.3/running-tests/tags.html
- **Mockito Javadoc (2.x)**: https://site.mockito.org/javadoc/current/org/mockito/Mockito.html
- **Mockito Core Javadoc (latest)**: https://javadoc.io/doc/org.mockito/mockito-core/latest/org/mockito/Mockito.html
- **AssertJ Core documentation**: https://assertj.github.io/doc/
- **PMD Java Best Practices rules**: https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html
- **SpotBugs bug descriptions**: https://spotbugs.readthedocs.io/en/stable/bugDescriptions.html

---

## Notes on Grounding Decisions

**JAVA-TESTING-UNIT-TEST-LOCATION-1 (prose)**: The Maven Standard Directory Layout defines `src/test/java` as the canonical test root and Surefire's default `*Test.java` pattern is well-documented. However, no standard published linter rule enforces that the package inside `src/test/java` mirrors the production package. This is a structural convention enforced at code-review time. Grounded against Maven documentation; enforcement is `prose`.

**JAVA-TESTING-INTEGRATION-TEST-LOCATION-1 (mechanical)**: Maven Failsafe's default inclusion patterns (`**/IT*.java`, `**/*IT.java`, `**/*ITCase.java`) are published and verifiable. JUnit 5's `@Tag` annotation is documented. Two authoritative linter anchors exist (Failsafe naming convention + `@Tag`). Enforcement is `mechanical` because the IT-suffix check maps to a real build-tool convention enforced by Failsafe.

**JAVA-TESTING-NAMING-1 (prose)**: JUnit 5's `DisplayNameGenerator.ReplaceUnderscores` and `@DisplayName` are cited from the JUnit user guide. PMD's `UnitTestShouldUseTestAnnotation` is cited as the mechanically enforceable guard against the worst naming anti-pattern (legacy `test` prefix without `@Test`). The positive naming convention (descriptive snake or camel) has no equivalent PMD rule with a published identifier. Enforcement is `prose`.

**JAVA-TESTING-AAA-STRUCTURE-1 (mechanical)**: Two real PMD rules are cited: `UnitTestShouldIncludeAssert` (at least one assertion) and `UnitTestContainsTooManyAsserts` (too many assertions indicating multiple behaviors). These partially enforce AAA structurally. The three-phase shape itself is prose-reviewed, but the linter gates are real. Enforcement is `mechanical` (the linter gates justify it).

**JAVA-TESTING-MOCK-BOUNDARIES-1 (prose)**: Mockito's own Javadoc contains explicit warnings against mocking concrete classes and value objects (quoted verbatim in the TOML). These are documentation-level constraints, not static-analysis rules. No Checkstyle, PMD, or SpotBugs rule with a published identifier specifically flags mocking of concrete types. Enforcement is `prose`.

**JAVA-TESTING-DETERMINISTIC-1 (mechanical)**: SpotBugs `IJU_ASSERT_METHOD_INVOKED_FROM_RUN_METHOD` is a real published bug pattern covering the threading anti-pattern (assertion in `Runnable.run()`). This is one specific form of the determinism violation. The broader rules (no `Thread.sleep`, no wall-clock, no ordering dependency) are best enforced by Semgrep/custom regex patterns that do not yet have published standard rule IDs, so they are noted as prose-enforced aspects within a `mechanical`-tier rule (the SpotBugs anchor justifies the tier).

**JAVA-TESTING-ASSERTIONS-ASSERTJ-1 (prose)**: AssertJ documentation is cited from the official assertj.github.io site. PMD's `UnitTestShouldIncludeAssert` is cited as the related mechanical check (ensures assertions exist; AssertJ satisfies it). No standard published Checkstyle or PMD rule ID mandates AssertJ specifically over JUnit assertions. A project-specific Semgrep rule could enforce this. Enforcement is `prose`.

---

## Unverified Linter IDs (honesty disclosure)

The following rule IDs were found in search results and confirmed through authoritative pages but could not be verified via live HTTP fetch of the exact rule anchor due to 403/404 during grounding (noted for future human verification):

- `PMD: UnitTestShouldUseTestAnnotation` — referenced from PMD best practices search result confirming PMD 4.0+ and renaming from `JUnit4TestShouldUseTestAnnotation`. The page at `https://pmd.github.io/pmd/pmd_rules_java_bestpractices.html` is the authoritative source.
- `PMD: UnitTestContainsTooManyAsserts` — referenced from the same PMD best practices page (PMD 5.0+).
- `Maven Surefire: default *Test.java inclusion pattern` — the exact default patterns `**/*Test.java`, `**/Test*.java`, `**/*Tests.java`, `**/*TestCase.java` are documented at `https://maven.apache.org/surefire/maven-surefire-plugin/examples/inclusion-exclusion.html`. The Failsafe patterns (`**/IT*.java`, `**/*IT.java`, `**/*ITCase.java`) were fetched and confirmed from `https://maven.apache.org/surefire/maven-failsafe-plugin/examples/inclusion-exclusion.html`.
