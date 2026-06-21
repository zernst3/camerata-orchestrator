# Python Testing Rule Grounding Report

Family: **python:testing** (`crates/rules/principles/python/testing/`)
Total rules: 7
Grounded: 7
Demoted: 0
Ungrounded: 0
Draft: 0

## Summary

All 7 rules in the `python:testing` family were grounded against authoritative sources:
pytest documentation (docs.pytest.org), the Python stdlib unittest.mock docs
(docs.python.org), and Ruff linter rules (docs.astral.sh/ruff/rules) covering the
`PT` (flake8-pytest-style) rule family.

No rules were demoted. Every mechanical rule maps to a real, named Ruff PT rule
that can be enabled in CI via `lint.select = ["PT"]`. Rules with no mechanical
linter equivalent were set to `enforcement = "structured"` (they require a human
or structured architectural review to enforce, not a lint regex).

## Grounding Notes by Rule

### PYTHON-TESTING-UNIT-LAYOUT-1 — enforcement: structured

No linter enforces the directory-layout choice (that is a structural/build-system
property, not a per-file lint). Grounded against pytest's Good Integration
Practices page, which explicitly documents and recommends the `tests/` top-level
layout for projects using the `src/` layout. Source:
https://docs.pytest.org/en/stable/explanation/goodpractices.html

### PYTHON-TESTING-FILE-NAMING-1 — enforcement: mechanical

pytest's default discovery algorithm is the authority: test files must match
`test_*.py` or `*_test.py`; test functions must be prefixed `test_`. This is
not a Ruff rule but is built into pytest itself. No separate linter rule ID is
needed — pytest itself rejects files that deviate at collection time (unless
custom `python_files` / `python_functions` configuration overrides). Grounded
against the Good Integration Practices page (same URL as above, "Conventions
for Python test discovery" section).

*Mechanical enforcement path:* `pytest --collect-only` in CI surfaces
misconfigured files as "no tests ran" rather than a lint error. A project can
additionally add `ruff --select PT` to catch style issues in test files.

### PYTHON-TESTING-AAA-STRUCTURE-1 — enforcement: structured

No linter enforces the Arrange-Act-Assert structure at the function-body level
(that requires reading the code). The composite-assertion linter rule (Ruff
PT018) is a complementary mechanical check that surfaces one symptom of a
poorly-structured test. Grounded against:
- pytest anatomy page: https://docs.pytest.org/en/stable/explanation/anatomy.html
- Ruff PT018: https://docs.astral.sh/ruff/rules/pytest-composite-assertion/

### PYTHON-TESTING-FIXTURES-CONFTEST-1 — enforcement: structured

No linter enforces the "put shared fixtures in conftest.py" convention. Grounded
against the pytest fixture how-to and explanation pages:
- https://docs.pytest.org/en/stable/how-to/fixtures.html
- https://docs.pytest.org/en/stable/explanation/fixtures.html

### PYTHON-TESTING-PARAMETRIZE-1 — enforcement: mechanical

Two real Ruff rules enforce idiomatic `@pytest.mark.parametrize` usage:
- PT006 — wrong argnames type (single-param should be a string, not a tuple)
- PT007 — wrong values type

Copy-paste detection (multiple test functions with identical bodies) is not
mechanically lintable, but the rule documents the structural requirement that
motivates PT006/PT007 enforcement. Grounded against:
- https://docs.pytest.org/en/stable/how-to/parametrize.html
- https://docs.astral.sh/ruff/rules/pytest-parametrize-names-wrong-type/
- https://docs.astral.sh/ruff/rules/pytest-parametrize-values-wrong-type/

### PYTHON-TESTING-MOCK-BOUNDARIES-1 — enforcement: structured

The "patch at the right namespace" principle is documented in the Python stdlib,
not in a linter rule. Ruff PT008 (patch-with-lambda) is a related mechanical
rule but covers a narrower sub-issue. The boundary-isolation principle (mock
only external I/O) has no linter encoding — it requires design review. Grounded
against:
- https://docs.python.org/3/library/unittest.mock.html
- https://docs.pytest.org/en/stable/how-to/monkeypatch.html

### PYTHON-TESTING-NO-FLAKY-1 — enforcement: structured

Non-determinism cannot be detected by a lint rule (a call to `datetime.now()` is
only a problem in a test if the result is not controlled). The rule is grounded
against pytest's explicit flaky-test explanation page, which defines the problem,
identifies the root cause (insufficient environment isolation), and documents
mitigation strategies. Grounded against:
- https://docs.pytest.org/en/stable/explanation/flaky.html

## Citation Table

| Rule ID | Verification | Source URL | Linter Rule | Notes |
|---|---|---|---|---|
| PYTHON-TESTING-UNIT-LAYOUT-1 | grounded | https://docs.pytest.org/en/stable/explanation/goodpractices.html | — | test layout / directory structure |
| PYTHON-TESTING-FILE-NAMING-1 | grounded | https://docs.pytest.org/en/stable/explanation/goodpractices.html | — | pytest built-in discovery; test_*.py convention |
| PYTHON-TESTING-AAA-STRUCTURE-1 | grounded | https://docs.pytest.org/en/stable/explanation/anatomy.html | — | Arrange-Act-Assert-Cleanup four-phase model |
| PYTHON-TESTING-AAA-STRUCTURE-1 | grounded | https://docs.astral.sh/ruff/rules/pytest-composite-assertion/ | Ruff: PT018 | composite assertion lint companion |
| PYTHON-TESTING-FIXTURES-CONFTEST-1 | grounded | https://docs.pytest.org/en/stable/how-to/fixtures.html | — | conftest.py, scope, yield teardown |
| PYTHON-TESTING-FIXTURES-CONFTEST-1 | grounded | https://docs.pytest.org/en/stable/explanation/fixtures.html | — | fixture explicitness, modularity, composability |
| PYTHON-TESTING-PARAMETRIZE-1 | grounded | https://docs.pytest.org/en/stable/how-to/parametrize.html | — | @pytest.mark.parametrize how-to |
| PYTHON-TESTING-PARAMETRIZE-1 | grounded | https://docs.astral.sh/ruff/rules/pytest-parametrize-names-wrong-type/ | Ruff: PT006 | wrong argnames type |
| PYTHON-TESTING-PARAMETRIZE-1 | grounded | https://docs.astral.sh/ruff/rules/pytest-parametrize-values-wrong-type/ | Ruff: PT007 | wrong values type |
| PYTHON-TESTING-MOCK-BOUNDARIES-1 | grounded | https://docs.python.org/3/library/unittest.mock.html | — | patch-at-usage-namespace; "where to patch" |
| PYTHON-TESTING-MOCK-BOUNDARIES-1 | grounded | https://docs.pytest.org/en/stable/how-to/monkeypatch.html | — | monkeypatch fixture; patch at boundary |
| PYTHON-TESTING-NO-FLAKY-1 | grounded | https://docs.pytest.org/en/stable/explanation/flaky.html | — | flaky definition, root causes, remediation |

## Authorities Used

- **pytest Good Integration Practices** — https://docs.pytest.org/en/stable/explanation/goodpractices.html (test layout, file naming, discovery)
- **pytest Anatomy of a test** — https://docs.pytest.org/en/stable/explanation/anatomy.html (Arrange-Act-Assert-Cleanup)
- **pytest How to use fixtures** — https://docs.pytest.org/en/stable/how-to/fixtures.html (conftest.py, scope, yield)
- **pytest About fixtures** — https://docs.pytest.org/en/stable/explanation/fixtures.html (explicitness, modularity)
- **pytest How to parametrize** — https://docs.pytest.org/en/stable/how-to/parametrize.html (@pytest.mark.parametrize)
- **pytest How to monkeypatch** — https://docs.pytest.org/en/stable/how-to/monkeypatch.html (monkeypatch fixture)
- **pytest Flaky tests** — https://docs.pytest.org/en/stable/explanation/flaky.html (determinism, isolation)
- **Python stdlib unittest.mock** — https://docs.python.org/3/library/unittest.mock.html (where to patch)
- **Ruff PT018** — https://docs.astral.sh/ruff/rules/pytest-composite-assertion/ (composite assertion)
- **Ruff PT006** — https://docs.astral.sh/ruff/rules/pytest-parametrize-names-wrong-type/ (parametrize argnames type)
- **Ruff PT007** — https://docs.astral.sh/ruff/rules/pytest-parametrize-values-wrong-type/ (parametrize values type)
