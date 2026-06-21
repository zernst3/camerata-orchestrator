# Option why audit: python/ corpus

**Date:** 2026-06-21
**Branch:** why/python
**Scope:** `crates/rules/principles/python/` (32 files, 116 option why values)

## Finding

Zero placeholder `why` values were found in the Python principles corpus.

Every `[[option]].why` field already contains a concrete, option-specific trade-off sentence (1-3 sentences), not the generic placeholder text ("A defensible alternative the project considered." / "A defensible position on this decision; with no default, the project must choose deliberately at curation time.") targeted by the rewrite directive.

The Python corpus was authored after the placeholder pattern was established as the ground-level text for empty options, and it was populated with real trade-offs from the start. Files examined span the top-level Python directory and three subdirectories (django/, flask/, testing/).

## Options rewritten

0

## Tests

`cargo test -p camerata-rules`: 54 passed, 0 failed.
