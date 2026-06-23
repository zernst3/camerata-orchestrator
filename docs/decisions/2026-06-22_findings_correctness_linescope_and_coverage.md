# 2026-06-22 Findings correctness: line-scope test detection and coverage notes

## Status
Accepted

## Context

The Camerata scan findings system had four correctness gaps:

1. **Per-file test classification** — the entire file was classified as test or production
   based on path alone. A file like `src/auth.rs` containing both production code and an
   inline `#[cfg(test)] mod tests { ... }` block classified ALL findings as production,
   including fake tokens inside the test block.

2. **Architectural rules attempted in preview** — `group_by_tool` would route Architectural
   rules (which require custom AST checkers) to linters based on their `linter` source field,
   producing spurious preview notes or incorrect groupings. Only Mechanical rules have
   off-the-shelf linter support for the preview pass.

3. **Preview coverage notes appeared as violations** — when a tool was missing or a rule
   could not be previewed, a `note_finding` was added to the findings table (the violations
   list). This confused scan-coverage information with actual violations.

4. **No explicit flags for test/review status** — `FindingView` had no `in_test` or
   `needs_review` bool fields; the UI derived "needs review" only from a text pattern in
   `detail`. A test-scope finding had no explicit machine-readable flag.

## Decisions

### 1. Line-scope test detection (FIX A)

Added `test_scope_line_ranges(path, content)` that scans Rust files for `#[cfg(test)]`,
`#[test]`, and `#[tokio::test]` attributes and returns the brace-delimited line ranges they
cover. Classification in `audit_content` is **per-finding-by-line**: a production secret in a
file that also contains a test block stays Critical; only findings whose line number falls
inside a test scope are downgraded to low/`in_test=true`.

The brace scanner tracks strings (`"..."`, `r#"..."#`), line comments (`//`), block comments
(`/* */`), and char literals (`'x'`) to avoid being fooled by `{` or `}` inside those
constructs.

**Limitation**: simple brace counter. Does not handle all edge cases (e.g. nested raw strings
with mismatched hashes), but correct for the overwhelming majority of Rust test code.

### 2. Architectural rule exclusion from preview (FIX B)

`group_by_tool` now checks `rule.enforcement == EnforcementKind::Mechanical` exclusively.
Architectural rules are skipped entirely — no ungrouped entry, no coverage note. Architectural
rules remain covered by the AI review (advisory); the preview is only for Mechanical rules
that have a deterministic off-the-shelf linter invocation.

### 3. Coverage notes separate from violations (FIX C)

`run_scan_tools` now returns `(Vec<Finding>, Vec<CoverageNote>)`. Tool-missing and
unrouted notes become `CoverageNote` entries on `ScanReport.coverage_notes`, not Finding rows
in the violations table. The UI renders a separate "Scan coverage" section for these.

`CoverageNote` is a plain `{ tool: String, message: String }` struct with `serde(default)` for
backward compatibility.

### 4. Explicit in_test / needs_review flags (FIX D)

`Finding` gains `in_test: bool` and `needs_review: bool` (both `serde(default)` for back-compat).
`audit_content` sets them based on the line-scope classification. The UI `FindingView` mirrors
both fields. The "Needs review" column gains a distinct "Test" badge (yellow) for `in_test`
findings, alongside the existing "Needs review" (orange) badge. Old serialized findings
deserialize with both flags as `false`.

## Consequences

- Production findings in mixed files correctly stay Critical.
- Test-scope findings in the same file are correctly downgraded to low/in_test=true.
- The scan coverage information is separate from the violations table.
- Architectural rules no longer generate spurious preview notes.
- The UI clearly distinguishes test-scoped findings from calibration-flagged findings.
