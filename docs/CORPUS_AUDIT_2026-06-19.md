# Rule Corpus Audit Report
**Date:** 2026-06-19  
**Worktree:** wave4/corpus-toml-audit  
**Status:** All issues resolved

## Summary

**Total Rules:** 123  
**Issues Found:** 4 (all resolved)  
**Unique Domains:** 18

| Severity | Count | Status |
|----------|-------|--------|
| Critical | 0     | ✓ |
| High     | 4     | ✓ Fixed |
| Medium   | 0     | ✓ |
| Low      | 0     | ✓ |

### Issue Breakdown

| Issue Type | Count | Resolution |
|------------|-------|-----------|
| Missing `[decision].default` when `default=true` | 4 | Fixed: added missing default option ids |
| TOML parse errors | 0 | — |
| Missing required fields | 0 | — |
| Invalid enforcement values | 0 | — |
| Duplicate rule ids | 0 | — |
| Domain-folder mismatches | 0 | — |

## Audit Details

### Issues Resolved

All four issues were of the same type: rules declared `default = true` (indicating they ship with an adopted default option) but were missing the corresponding `[decision].default = "<option-id>"` field required by the loader.

| File | Rule ID | Issue | Fix |
|------|---------|-------|-----|
| agentic/orch-context-override-1.toml | ORCH-CONTEXT-OVERRIDE-1 | Missing `[decision].default` | Added `default = "rule-overrides-driven-by-project-context-require"` |
| agentic/orch-prereview-1.toml | ORCH-PREREVIEW-1 | Missing `[decision].default` | Added `default = "ai-pre-review-then-human-review"` |
| ci-cd/arch-trigger-env-1.toml | ARCH-TRIGGER-ENV-1 | Missing `[decision].default` | Added `default = "git-ref-environment-immutable-prod-tags"` |
| ci-cd/arch-trunk-sync-1.toml | ARCH-TRUNK-SYNC-1 | Missing `[decision].default` | Added `default = "one-merge-commit-after-the-release-ships"` |

### Domains Validated

The corpus organizes rules by domain in a hierarchical folder structure with support for sub-domain notation (e.g., `javascript:express` for rules nested in `javascript/express/`).

**Domains found (18 total):**
- Universal: `*`
- Single-level: `agentic`, `api-layer`, `ci-cd`, `concurrency`, `fullstack`, `iac`, `permissions`, `rust`, `sql`, `ui`, `javascript`
- Sub-domains: `javascript:express`, `javascript:next`, `javascript:react`, `javascript:redux`, `javascript:typescript`, `rust:dioxus`, `rust:seaorm`

All domain tags in rule TOML files are properly aligned with their containing folder structure.

## Verification

**Corpus Load Test:** PASS
```
$ cargo test -p camerata-rules
   test result: ok. 27 passed; 0 failed
```

**Cargo Check:** PASS
```
$ cargo check
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 09s
```

**Loader Consistency:** All 123 rules successfully load. The RuleSet indexing (by id and by domain) is fully consistent with the corpus iteration order.

## Notes

1. **No Trivial Typos Found:** The corpus is well-maintained; rationale and directive strings are complete and correctly formed.

2. **No Missing Required Fields:** All rules have the core required fields (id, title, enforcement, domain) properly populated.

3. **No Duplicate IDs:** All 123 rule IDs are unique across the corpus.

4. **Enforcement Kind Validation:** All enforcement values are valid (`prose`, `structured`, or `mechanical`). No malformed values were found.

5. **Sub-Domain Notation:** The corpus successfully uses `:` notation for hierarchical domain organization (e.g., `rust:dioxus`). This is parsed correctly by the loader and is not a violation.

## Recommendations

1. Consider adding a corpus-validation CI check that runs `cargo test -p camerata-rules` on every PR to catch similar issues at review time.

2. The four issues fixed in this audit suggest that rules declaring `default = true` may benefit from a stricter validation pass during curation. All rules with `default = true` must have `[decision].default` populated and referencing an actual option id.

3. The 18-domain landscape is healthy. No evidence of accidental domain duplication or orphaned domains.
