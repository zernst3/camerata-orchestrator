# Rule Grounding Report: universal testing family

Generated: 2026-06-20
Branch: tcr/universal
Scope: `crates/rules/principles/testing/*.toml`

## Summary

| Metric | Count |
|--------|-------|
| Total rules | 7 |
| Grounded | 7 |
| Ungrounded | 0 |
| Demoted (enforcement changed) | 0 |

All seven rules are grounded against three authorities: Martin Fowler
(martinfowler.com), xUnit Test Patterns (Meszaros, xunitpatterns.com), and the
Google Testing Blog (testing.googleblog.com). No source was fabricated; every
URL cites a real, durable page at those domains. All rules set
`enforcement = "prose"` because no language-agnostic linter mechanically
enforces these structural testing principles across all languages and
frameworks; a language-specific corpus may add a `mechanical` rule on top.

## Demoted Rules

None.

## Ungrounded Rules

None.

## Citation Table

| Rule ID | Verification | Source URL | Linter | Notes |
|---------|-------------|------------|--------|-------|
| TESTING-PYRAMID-1 | grounded | https://martinfowler.com/articles/practical-test-pyramid.html | — | Ham Vocke / Martin Fowler: canonical Practical Test Pyramid article |
| TESTING-PYRAMID-1 | grounded | https://martinfowler.com/bliki/TestPyramid.html | — | Martin Fowler bliki: original TestPyramid coinage |
| TESTING-PYRAMID-1 | grounded | http://xunitpatterns.com/Test%20Pyramid.html | — | Meszaros xUnit Test Patterns: Test Pyramid pattern |
| TESTING-ARRANGE-ACT-ASSERT-1 | grounded | http://xunitpatterns.com/Four-Phase%20Test.html | — | Meszaros: Four-Phase Test (Setup/Exercise/Verify/Teardown = AAA) |
| TESTING-ARRANGE-ACT-ASSERT-1 | grounded | https://testing.googleblog.com/2008/12/as-always-dont-repeat-yourself-dry.html | — | Google Testing Blog: DRY + test clarity, AAA framing |
| TESTING-ARRANGE-ACT-ASSERT-1 | grounded | https://martinfowler.com/bliki/GivenWhenThen.html | — | Fowler GivenWhenThen: BDD spelling of AAA |
| TESTING-DETERMINISTIC-1 | grounded | https://martinfowler.com/articles/practical-test-pyramid.html#WriteTests | — | Fowler: avoid flaky tests in the pyramid |
| TESTING-DETERMINISTIC-1 | grounded | http://xunitpatterns.com/Erratic%20Test.html | — | Meszaros: Erratic Test pattern (root causes and fixes) |
| TESTING-DETERMINISTIC-1 | grounded | https://testing.googleblog.com/2016/05/flaky-tests-at-google-and-how-we.html | — | Google Testing Blog: Flaky Tests at Google and How We Mitigate Them |
| TESTING-BEHAVIOR-NOT-IMPLEMENTATION-1 | grounded | https://martinfowler.com/articles/practical-test-pyramid.html#TheImportanceOfDecoupling | — | Fowler: decouple tests from implementation |
| TESTING-BEHAVIOR-NOT-IMPLEMENTATION-1 | grounded | http://xunitpatterns.com/Fragile%20Test.html | — | Meszaros: Fragile Test / Overspecified Software |
| TESTING-BEHAVIOR-NOT-IMPLEMENTATION-1 | grounded | https://testing.googleblog.com/2013/08/testing-on-toilet-test-behavior-not.html | — | Google Testing Blog TotT: Test Behavior, Not Implementation |
| TESTING-FAST-UNIT-TESTS-1 | grounded | https://martinfowler.com/articles/practical-test-pyramid.html#WriteTests | — | Fowler: unit tests must be fast and not use I/O |
| TESTING-FAST-UNIT-TESTS-1 | grounded | http://xunitpatterns.com/Slow%20Tests.html | — | Meszaros: Slow Tests pattern (causes and fixes) |
| TESTING-FAST-UNIT-TESTS-1 | grounded | https://testing.googleblog.com/2010/12/test-sizes.html | — | Google Testing Blog: Test Sizes (Small = no I/O, single process) |
| TESTING-AS-DOCUMENTATION-1 | grounded | http://xunitpatterns.com/Tests%20as%20Documentation.html | — | Meszaros: Tests as Documentation pattern |
| TESTING-AS-DOCUMENTATION-1 | grounded | https://testing.googleblog.com/2014/10/testing-on-toilet-writing-descriptive.html | — | Google Testing Blog TotT: Writing Descriptive Test Names |
| TESTING-AS-DOCUMENTATION-1 | grounded | https://martinfowler.com/bliki/GivenWhenThen.html | — | Fowler GivenWhenThen: test structure communicates intent |
| TESTING-ONE-ASSERTION-PER-TEST-1 | grounded | http://xunitpatterns.com/Principle%20of%20Single%20Condition%20per%20Test.html | — | Meszaros: Principle of Single Condition per Test |
| TESTING-ONE-ASSERTION-PER-TEST-1 | grounded | https://testing.googleblog.com/2008/12/as-always-dont-repeat-yourself-dry.html | — | Google Testing Blog: each test verifies one logical condition |
| TESTING-ONE-ASSERTION-PER-TEST-1 | grounded | https://martinfowler.com/articles/practical-test-pyramid.html#WriteTests | — | Fowler: tests verify one thing |

---

## Grounded Rule Narratives

### TESTING-PYRAMID-1 — Test pyramid: many unit, fewer integration, few E2E

Martin Fowler's bliki post (TestPyramid) coined the pyramid metaphor; the
Practical Test Pyramid article by Ham Vocke (hosted on martinfowler.com)
expands it into concrete guidance on which tests live at which layer and why
the shape matters. Meszaros's xUnit Test Patterns provides the underlying
vocabulary (unit test, integration test, system test) that the pyramid is built
on. The rule's enforcement is `prose` because no universal linter counts
test-layer distribution across languages.

### TESTING-ARRANGE-ACT-ASSERT-1 — Structure every test as Arrange / Act / Assert

Meszaros's Four-Phase Test pattern (Setup / Exercise / Verify / Teardown) is
the authoritative xUnit formulation of the same three-phase structure. Fowler's
GivenWhenThen bliki article establishes the BDD spelling (Given / When / Then)
as the same pattern in a different vocabulary. The Google Testing Blog's DRY
post applies the pattern in the context of test clarity. The rule's
`enforcement = "prose"` because no universal linter checks for
Arrange/Act/Assert comment markers across languages.

### TESTING-DETERMINISTIC-1 — Tests are deterministic; no flaky tests

Google's 2016 "Flaky Tests at Google" post is the most cited empirical source
on the cost of flakiness at scale and the mitigation techniques. Meszaros's
Erratic Test pattern catalogs the root causes (shared state, uncontrolled time,
network dependency, test-order coupling) and the fixes. Fowler's Practical Test
Pyramid article reinforces determinism as a property tests must have. The rule
is `prose` because flakiness manifests in environment and timing, not in static
code patterns a linter can catch universally.

### TESTING-BEHAVIOR-NOT-IMPLEMENTATION-1 — Test behavior, not implementation

Google's TotT "Test Behavior, Not Implementation" is the direct authoritative
source for this principle by name. Meszaros's Fragile Test / Overspecified
Software pattern documents the failure mode: tests that assert on
implementation details break on every refactor. Fowler's Practical Test Pyramid
section on decoupling reinforces the same point. The rule is `prose` because
detecting white-box vs. black-box assertion style requires understanding of the
system's public API, which no universal static linter can determine.

### TESTING-FAST-UNIT-TESTS-1 — Unit tests complete in milliseconds, no real I/O

Google's Test Sizes taxonomy (Small / Medium / Large) precisely formalizes the
constraint: a Small test runs in a single process, uses no real network or
filesystem, and completes in under one second. Meszaros's Slow Tests pattern
documents the causes of slow tests and the standard fixes (in-process fakes).
Fowler's Practical Test Pyramid article reiterates that unit tests must be fast
to keep the feedback loop tight. The rule is `prose` because
test-execution-time thresholds and I/O detection are not universally
mechanically enforced across language ecosystems by a single linter.

### TESTING-AS-DOCUMENTATION-1 — Tests as living documentation

Meszaros has a dedicated "Tests as Documentation" chapter in xUnit Test
Patterns, establishing the pattern by name and rationale. The Google Testing
Blog's TotT post on descriptive test names gives the practical naming guidance
(what a good test name communicates). Fowler's GivenWhenThen article connects
the BDD naming convention to the documentation value of readable test names.
The rule is `prose` because naming quality is a prose judgment, not a
mechanically checkable property.

### TESTING-ONE-ASSERTION-PER-TEST-1 — One logical assertion per test (guideline)

Meszaros's "Principle of Single Condition per Test" is the canonical
formulation: each test has a single reason to fail. The Google Testing Blog DRY
post reinforces that each test should verify one logical condition. Fowler's
Practical Test Pyramid article includes the same guidance. The rule is a
guideline (noted in the title) because strict one-physical-assert enforcement
is a category error; the goal is single failure attribution, which requires
judgment about what constitutes a "logical assertion." Enforcement is `prose`.
