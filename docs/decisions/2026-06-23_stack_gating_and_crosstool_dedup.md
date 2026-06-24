# Stack-Aware Tool Gating and Cross-Preview-Tool Dedup

**Date:** 2026-06-23
**Status:** Decided + implemented
**Files:** `crates/server/src/scan_tools.rs`, `crates/server/src/lib.rs`

## Context

Dogfooding Camerata on its own Rust repo surfaced two misleading behaviours in the
scan-preview pipeline:

1. **eslint and semgrep both reported "✓ 0 findings"** on a Rust-only repo because
   the tool ran with no JS/TS files to scan. A passing "✓ 0" reads as a clean check
   but is N/A — the tool had nothing to scan.

2. **When semgrep gains Rust rules** (in parallel), a clippy finding and a semgrep
   finding on the same (repo, path, line, issue) would both appear in triage, doubling
   the noise for the same underlying problem.

## Decision 1: Stack-Aware Tool Gating

**A tool is omitted from the preview run AND from the pre-declared tool count if its
required language is absent from the repo.**

Language presence is derived from file extensions (`lang_for_ext` in
`onboard/propose.rs`). The gate is applied in two places:

- `group_by_tool(selected, lookup, present_languages: Option<&HashSet<String>>)` —
  filters tools from the routing map before any invocation.
- `preview_tool_ids_for_rules(selected, lookup, present_languages)` — filters the
  pre-declared tool count so the progress denominator N matches the tools that
  actually run.

The STACK gates, regardless of rule selection: even if a JS rule is in the selection,
a Rust-only repo will never run eslint.

**Language-to-tool mapping:**

| Tool     | Required language(s)                                                   |
|----------|------------------------------------------------------------------------|
| Clippy   | Rust                                                                   |
| Ruff     | Python                                                                 |
| Eslint   | JavaScript OR TypeScript                                               |
| Semgrep  | ANY of: Python, JS, TS, Go, Java, Ruby, Rust, C#, PHP, C, C++         |

When `present_languages` is `None` (e.g., callers that haven't threaded the file list
through yet), all tools pass — backward-compat.

When the language set is empty (unreadable repo, all binary files), all tools pass
conservatively — the gating must not silently swallow a tool that should run.

**Where the language set is derived:**

- `merge_scan_preview` (lib.rs): reads each repo's files via `read_local_repo_files`
  per-repo and passes `Some(&present_languages)` to `run_scan_tools`.
- `onboard_audit_start` pre-declaration (lib.rs): reads files for each source repo and
  builds the UNION of all present languages across repos. Uses `lang_gate = Some(&union)`
  when non-empty, `None` when empty.

## Decision 2: Cross-Preview-Tool Dedup

**Findings that share the same (repo, path, line) AND a compatible security category
are collapsed to ONE canonical row.** The lower-rank (higher-precedence) finding is kept
canonical; the other's rule id is appended to `also_matches`.

**Precedence (rank, lower = canonical):**

```
0 = floor  (deterministic SEC-* ids, gate-enforced)
1 = clippy / ruff / eslint  (native language linter)
2 = semgrep  (polyglot, broader category matching)
```

**Category mapping** (`finding_security_category` in lib.rs):

| Rule id(s)                                              | Category  |
|---------------------------------------------------------|-----------|
| SEC-NO-HARDCODED-SECRETS-1 / SEC-NO-PRIVATE-KEY-1 / SEC-NO-VENDOR-TOKEN-1 | `secret` |
| SEC-NO-RAW-SQL-CONCAT-1 / S608 (Ruff) / camerata.security.sql-string-concat-* | `sql` |
| SEC-NO-DISABLED-TLS-1                                   | `tls`     |
| camerata.security.hardcoded-secret                      | `secret`  |
| camerata.security.exec-injection*                       | `exec`    |
| camerata.security.weak-hash-*                           | `hash`    |
| camerata.security.path-traversal-python                 | `path`    |
| camerata.security.subprocess-shell-true                 | `shell`   |
| camerata.security.yaml-unsafe-load                      | `yaml`    |

Unknown rule ids map to `None` and are never collapsed — surface over hide.

**Conservative dedup:**

Two findings on the same line but with DIFFERENT categories are both kept. Over-merging
a real finding is the worst failure mode; a redundant row is cheap.

**Exact line matching** (unchanged from the prior floor↔semgrep dedup): no fuzzy proximity.

**Backward compatibility:** `dedup_preview_against_floor` is kept as an alias over the
new `dedup_scan_previews` function. All existing floor↔semgrep dedup tests pass unchanged.
The new function is a strict superset of the old contract.

## New public API surface

**`crates/server/src/scan_tools.rs`**

- `languages_from_files(files: &[(String, String)]) -> HashSet<String>` — pure, testable.
- `tool_languages_present(tool: ScanTool, present: Option<&HashSet<String>>) -> bool` — gate predicate.
- `group_by_tool` signature gains `present_languages: Option<&HashSet<String>>` parameter.
- `preview_tool_ids_for_rules` signature gains the same parameter.
- `run_scan_tools` signature gains `present_languages: Option<&HashSet<String>>`.

**`crates/server/src/lib.rs`**

- `finding_security_category(rule_id: &str) -> Option<&'static str>` — `pub(crate)`.
- `dedup_scan_previews(existing, previews) -> Vec<Finding>` — generalized dedup, `pub(crate)`.
- `dedup_preview_against_floor` — kept as backward-compat alias over `dedup_scan_previews`.

## Tests added

**scan_tools.rs:**
- `languages_from_files_maps_extensions` — extension→language coverage.
- `tool_languages_present_none_always_passes` — None is permissive.
- `tool_languages_present_rust_only` — Rust→clippy+semgrep pass; eslint+ruff fail.
- `tool_languages_present_empty_set_is_permissive` — empty set = let all through.
- `stack_gating_rust_only_repo_excludes_eslint_and_ruff` — core FIX 1 assertion.
- `stack_gating_js_rule_on_rust_repo_yields_no_eslint` — JS rule + Rust repo → no eslint.

**lib.rs:**
- `finding_security_category_maps_correctly` — full category table verification.
- `crosstool_dedup_ruff_and_semgrep_same_location_collapses` — ruff+semgrep same sql → one row.
- `crosstool_dedup_floor_beats_ruff_at_same_location` — floor wins over ruff (rank 0 vs 1).
- `crosstool_dedup_different_categories_same_line_are_kept` — different category → both kept.
- `crosstool_dedup_floor_semgrep_regression_unchanged` — old floor↔semgrep contract intact.
- `crosstool_dedup_unknown_category_never_deduped` — None-category → no collapse.
