# Split `onboard.rs` into `onboard/` submodule directory

**Date:** 2026-06-23
**Status:** Implemented (refactor/split-onboard branch)

## Context

`crates/server/src/onboard.rs` grew to 4,064 lines, making it hard to navigate and
creating a large edit surface for any single file. The module had six well-separated
concerns that were accidentally colocated rather than architecturally coupled.

## Decision

Split `onboard.rs` into a module root + six submodule files under `onboard/`:

| Submodule | Content |
|-----------|---------|
| `onboard/audit.rs` | `audit_content`, `audit_files`, `severity_for`, `title_for`, `classify_repo_findings`, `is_code_auditable_rule` |
| `onboard/propose.rs` | `propose_rules`, `propose_corpus_rules`, `detect_stack`, `detect_frameworks`, `lang_for_ext`, `testing_domain_for_language`, `domains_for_stack` |
| `onboard/files.rs` | `read_local_repo_files`, `ExtractedRepo`, `has_code_ext`, `is_noise_path`, `extra_exclude_dirs`, `HARD_CAP_FILES`, `MAX_FILE_BYTES`, all noise constants |
| `onboard/self_ref.rs` | `is_governance_or_corpus_artifact`, `is_self_referential_snippet`, `corpus_texts_from_ruleset`, `suppress_self_referential` |
| `onboard/report.rs` | `build_report`, `csv_escape`, `tech_debt_csv`, `tech_debt_issue_body`, `create_tech_debt_ticket`, `create_issue`, `merge_deep_reports` |
| `onboard/greenfield.rs` | `scaffold_greenfield_blocking`, `GreenfieldResult` |

## Module root keeps

The root `onboard.rs` (now the module root) retains:
- **Submodule declarations** (`pub mod audit; pub mod files;` …)
- **All public re-exports** (`pub use audit::audit_content;` etc.) so `crate::onboard::X`
  paths are stable — zero external callers changed
- **All shared types** that are pervasive across submodules: `Finding`, `RuleOptionView`,
  `RuleSourceView`, `ProposedRule`, `RepoStack`, `CoverageNote`, `ScanReport`, `AUDIT_RULES`,
  `default_status`, and their `impl` blocks
- **`SelectedRule` + `impl`** (tightly coupled to `audit_repos`)
- **Orchestration functions**: `scan_repos`, `audit_repos`, `suppression_registry` — these
  call every submodule and are the entry points; they belong at the root
- **The full `#[cfg(test)] mod tests`** — the test module uses `use super::*` to pull
  every item from root (including all re-exports), so it was the most cohesive choice to
  leave it in place rather than scatter 54 tests across six files with complex cross-module
  `use` paths

## Re-export strategy

Root uses `pub use submod::item` for everything that was public in the original file.
`pub(crate) use` is used for private helpers the test module needs via `use super::*`:
`classify_repo_findings`, `is_code_auditable_rule`, `csv_escape`, `has_code_ext`,
`is_noise_path`, `extra_exclude_dirs`, `HARD_CAP_FILES`, `domains_for_stack`,
`merge_deep_reports`, and the `camerata_gateway` test-scope primitives
(`is_in_test_scope`, `test_scope_line_ranges`, `TEST_PATH_NOTE`, `TEST_PATH_SEVERITY`).

Items re-exported only for test consumption carry `#[allow(unused_imports)]` because
rustc does not count `#[cfg(test)]` callers as usage for the unused-imports lint.

## Rationale

- **Readability and navigation:** Each submodule is ~150–400 lines with a single clear
  concern. Finding the greenfield scaffold is now a `onboard/greenfield.rs` lookup, not
  a search through 4,000 lines.
- **Edit surface:** A change to the CSV rendering touches only `report.rs`; a change to
  stack detection touches only `propose.rs`. No risk of accidentally editing the wrong area.
- **Compile-time:** No meaningful change (single crate, incremental rebuilds; the motivation
  was readability, not compilation).
- **Zero behavior change:** Pure move. No logic, signatures, or tests were modified.
  All 713 `camerata-server` tests pass at the same count.

## Verification

- `cargo build --workspace` clean
- `cargo test -p camerata-server`: 713 passed, 0 failed (identical to pre-split)
- `cargo check -p camerata-ui` clean
- No other server files modified (the `pub use` re-export strategy absorbed all callers)

## New file line counts

| File | Lines |
|------|-------|
| `onboard.rs` (module root) | ~2,460 |
| `onboard/audit.rs` | ~195 |
| `onboard/propose.rs` | ~400 |
| `onboard/files.rs` | ~250 |
| `onboard/self_ref.rs` | ~100 |
| `onboard/report.rs` | ~190 |
| `onboard/greenfield.rs` | ~145 |
