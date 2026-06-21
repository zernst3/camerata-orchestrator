# Go Corpus Option-Why Audit (why/go)

**Date**: 2026-06-21
**Branch**: why/go
**Status**: Complete
**Scope**: `crates/rules/principles/go/` (all `.toml` files, including subdirectories `gorm/`, `grpc/`, `testing/`, `web/`)

---

## Summary

Audited every `[[option]].why` field in the Go rule corpus against the
placeholder criteria:

- Exact match or prefix: "A defensible alternative the project considered."
- "A defensible position on this decision; with no default, the project must
  choose deliberately at curation time."
- Any other content-free value that does not explain a real trade-off.

**Options rewritten: 0.**

The Go corpus was already written with real, option-specific trade-off
explanations in every `[[option]].why` field. No placeholder strings were
found. All 28 files across the four subdirectories (`gorm/`, `grpc/`,
`testing/`, `web/`) and the root `go/` directory were checked.

## Files Audited

Root (11 files):
- `go-accept-interfaces-return-structs-1.toml`
- `go-context-propagation-1.toml`
- `go-error-wrapping-with-fmt-w-1.toml`
- `go-errors-must-be-checked-1.toml`
- `go-goroutine-leaks-defer-cleanup-1.toml`
- `go-handler-service-repository-1.toml`
- `go-logging-structured-1.toml`
- `go-no-goroutine-globals-1.toml`
- `go-package-boundaries-clear-1.toml`
- `go-small-interfaces-1.toml`
- `go-sql-parameterized-1.toml`

gorm/ (5 files), grpc/ (5 files), testing/ (7 files), web/ (7 files).

## Test Result

`cargo test -p camerata-rules`: 55 tests passed, 0 failed.
