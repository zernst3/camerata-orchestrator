# JavaScript Testing Rule Grounding Report

**Date:** 2026-06-20
**Family:** javascript:testing
**Scope:** `crates/rules/principles/javascript/testing/*.toml`
**Branch:** `tcr/js`

## Summary

- Rules authored: 7
- Grounded: 7 / 7
- Draft (no source): 0
- Demoted (mechanical → prose): 1
- Demoted (mechanical → structured): 1

## Demotion Table

| Rule ID | Original enforcement | Final enforcement | Reason |
|---------|---------------------|-------------------|--------|
| `JAVASCRIPT-TESTING-AAA-STRUCTURE-1` | mechanical | prose | No linter rule enforces Arrange-Act-Assert phase ordering or single-act constraint at the AST level. `jest/no-conditional-expect` catches one subclass of AAA violation (assertions in conditionals) but does not enforce the full pattern. Enforcement is PR review. |
| `JAVASCRIPT-TESTING-INTEGRATION-LOCATION-1` | mechanical | structured | No ESLint rule enforces integration test directory placement. The placement is a project structural convention controlled via `testPathIgnorePatterns` / CI script split, not a lint gate. |

## Rule Grounding Table

| Rule ID | Verification | Enforcement | Source URL(s) | Linter Rule(s) | Notes |
|---------|-------------|-------------|---------------|----------------|-------|
| `JAVASCRIPT-TESTING-UNIT-COLOCATION-1` | grounded | mechanical | https://jestjs.io/docs/configuration#testmatch-arraystring, https://vitest.dev/config/include | `jest: testMatch default glob`, `vitest: include default glob` | Both frameworks discover `*.test.ts` by default; no config needed |
| `JAVASCRIPT-TESTING-INTEGRATION-LOCATION-1` | grounded | structured | https://jestjs.io/docs/configuration#testpathignorepatterns-arraystring | (none — structural convention) | `testPathIgnorePatterns` enables CI split; placement is convention |
| `JAVASCRIPT-TESTING-AAA-STRUCTURE-1` | grounded | prose | https://jestjs.io/docs/api, https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-conditional-expect.md | `jest: no-conditional-expect` (partial enforcement) | AAA itself has no linter rule; `no-conditional-expect` catches one failure mode |
| `JAVASCRIPT-TESTING-MOCK-AT-BOUNDARIES-1` | grounded | structured | https://jestjs.io/docs/mock-functions, https://testing-library.com/docs/guiding-principles/ | `jest: prefer-spy-on` (adjacent rule, not direct enforcement) | The boundary principle is documented in Jest + Testing Library; no rule directly enforces it |
| `JAVASCRIPT-TESTING-DETERMINISTIC-1` | grounded | mechanical | https://jestjs.io/docs/timer-mocks, eslint-plugin-jest README | `jest: no-done-callback`, `jest: expect-expect`, `jest: no-conditional-expect` | Multiple recommended rules form the mechanical gate; fake-timer API is the runtime contract |
| `JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1` | grounded | mechanical | https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-disabled-tests.md | `jest: no-disabled-tests`, `jest: no-focused-tests` | Both in recommended config; must be set to error severity |
| `JAVASCRIPT-TESTING-NAMING-1` | grounded | mechanical | https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-identical-title.md, eslint-plugin-jest README | `jest: no-identical-title`, `jest: valid-title` | Both in recommended config; content naming convention is prose |

## Authorities Consulted

All source URLs were fetched and verified during the grounding pass. No URL was fabricated; only URLs that returned substantive content were cited.

### Primary Testing Authorities

- **Jest docs — configuration (testMatch, testPathIgnorePatterns):**
  https://jestjs.io/docs/configuration
- **Jest docs — timer mocks (jest.useFakeTimers, advanceTimersByTime):**
  https://jestjs.io/docs/timer-mocks
- **Jest docs — mock functions (jest.fn, jest.mock, boundary mocking):**
  https://jestjs.io/docs/mock-functions
- **Jest docs — globals API (describe, it/test, expect):**
  https://jestjs.io/docs/api
- **Vitest configuration — include default pattern (`['**/*.{test,spec}.?(c|m)[jt]s?(x)']`):**
  https://vitest.dev/config/include
- **Testing Library — Guiding Principles (user-behaviour focus, mock boundaries):**
  https://testing-library.com/docs/guiding-principles/

### ESLint Plugin Jest Authorities

All rules below are part of the `jest-community/eslint-plugin-jest` package:
https://github.com/jest-community/eslint-plugin-jest

| Rule | Status in package | Doc URL |
|------|------------------|---------|
| `jest/no-disabled-tests` | Recommended (⚠️ warn default, must be raised to error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-disabled-tests.md |
| `jest/no-focused-tests` | Recommended (error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/README.md |
| `jest/no-identical-title` | Recommended (error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-identical-title.md |
| `jest/valid-title` | Recommended (error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/README.md |
| `jest/no-conditional-expect` | Recommended (error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-conditional-expect.md |
| `jest/expect-expect` | Recommended (error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/README.md |
| `jest/no-done-callback` | Recommended (error) | https://github.com/jest-community/eslint-plugin-jest/blob/main/README.md |
| `jest/prefer-spy-on` | Not recommended (style/opt-in) | https://github.com/jest-community/eslint-plugin-jest/blob/main/README.md |

### Secondary References

- **Empirical AAA adoption study (2025, IEEE Transactions on Software Engineering):**
  https://www.computer.org/csdl/journal/ts/2025/04/10859187/23X97981ZLi
  (Cited for the 77% adoption figure in JAVASCRIPT-TESTING-AAA-STRUCTURE-1)
- **Testing Library — fake timers usage:**
  https://testing-library.com/docs/using-fake-timers/
- **Jest config — projects (unit vs integration CI split):**
  https://jestjs.io/docs/configuration#projects-arraystring--projectconfig

## Demotion Rationale

### JAVASCRIPT-TESTING-AAA-STRUCTURE-1: mechanical → prose

The Arrange-Act-Assert pattern is a structuring convention, not an AST-checkable property. No linter in the `eslint-plugin-jest` ecosystem has a rule that verifies phase ordering or the single-act constraint. The `jest/no-conditional-expect` rule catches one failure mode (an assertion hidden in a conditional), but it does not verify that Arrange, Act, and Assert phases exist, are in order, or that only one Act phase is present. Demoted to `prose`; `no-conditional-expect` is listed as a partial adjacent source.

### JAVASCRIPT-TESTING-INTEGRATION-LOCATION-1: mechanical → structured

Directory placement of integration tests is enforced via `testPathIgnorePatterns` configuration and CI script design (a separate `test:integration` npm script / CI step), not by a lint rule. No ESLint rule validates that files ending in `.test.ts` inside a particular directory path match an integration-test naming convention. Demoted to `structured`; the Jest `testPathIgnorePatterns` documentation is the grounding source for the split-run mechanism.
