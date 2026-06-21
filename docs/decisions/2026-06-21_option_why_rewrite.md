# Option Why Rewrite: Java Corpus

**Date:** 2026-06-21
**Branch:** why/java
**Scope:** `crates/rules/principles/java/` (all .toml files, including `spring/` and `testing/` subdirectories)

## Summary

Scanned all 29 Java principle TOML files for placeholder `[[option]] why` values matching the patterns:
- Exactly or beginning with "A defensible alternative the project considered."
- "A defensible position on this decision; with no default, the project must choose deliberately at curation time."
- Any other content-free placeholder with no real trade-off reasoning.

## Result

**1 option why value rewritten.**

### File: `spring/java-spring-thin-controllers-1.toml`
**Option:** `fat-controllers-inline-logic`

The `why` began with "A defensible alternative the project considered." followed by a brief list of consequences. Rewrote to lead with the concrete cost of inline controller logic: HTTP-only reachability prevents unit testing without the web layer, blocks reuse from scheduled jobs or message listeners, and entangles domain rule changes with HTTP-layer code. Added a note on the @Transactional AOP ambiguity raised in the [decision] why paragraph.

## Files scanned

All 29 .toml files across:
- `java/` (12 root-level rules)
- `java/spring/` (9 Spring-specific rules)
- `java/testing/` (8 testing rules)

## Tests

`cargo test -p camerata-rules`: 54 passed, 0 failed.
