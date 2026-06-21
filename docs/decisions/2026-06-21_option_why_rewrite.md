# Option `why` rewrite: JavaScript rule corpus

**Date:** 2026-06-21
**Branch:** why/js

## Summary

Rewrote all placeholder `[[option]] why` values in the JavaScript rule corpus
(`crates/rules/principles/javascript/`).

## Scope

- **Files touched:** 49 TOML files across 8 subdirectories (angular, express,
  nest, next, react, redux, testing, typescript, vue, and the top-level
  javascript/ directory).
- **Options rewritten:** 51 total (one file, `javascript-next-dual-api-helpers-1.toml`,
  had two placeholder options).

## What was replaced

Any `why` field that:
- Exactly matched or began with "A defensible alternative the project considered."
- Was a partial placeholder with trailing text that still started with the same
  sentinel phrase (four testing-rule options).

## How each rewrite was derived

Each replacement was derived from the rule's own `[decision] why` paragraph,
which already described the rejected option's specific cost. The option `why`
now states what adopting the rejected option means and its concrete trade-off
(when it might be right, what it costs), in 1-3 sentences matching the rule's
voice. No facts were invented beyond what the rule itself states.

## Test result

`cargo test -p camerata-rules`: 54 passed, 0 failed.
