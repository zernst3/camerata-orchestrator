# Go Testing Rule Grounding Report

Generated: 2026-06-20 — tcr/go branch

Domain: `go:testing`
Location: `crates/rules/principles/go/testing/`

## Summary

| Category | Count |
|---|---|
| Grounded | 7 |
| Ungrounded (draft) | 0 |
| Demoted (mechanical → prose) | 0 |

All 7 rules were grounded against authoritative sources before commit. No rules were left as `draft`. No rules required enforcement demotion.

## Mechanical vs Prose Justification

Three rules are `mechanical` (a real linter exists to enforce them):

| Rule ID | Enforcement | Linter |
|---|---|---|
| GO-TESTING-COLOCATED-TEST-FILES-1 | mechanical | go build: _test.go exclusion (tool-enforced by the go command itself) |
| GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1 | mechanical | golangci-lint: tparallel (related sleep/parallel discipline) |
| GO-TESTING-HELPER-T-HELPER-1 | mechanical | golangci-lint: thelper |

Two rules are `structured` (emit to CONVENTIONS.md; no single linter enforces the full rule, but the convention is canonical):

| Rule ID | Enforcement | Rationale |
|---|---|---|
| GO-TESTING-TABLE-DRIVEN-T-RUN-1 | structured | Go Wiki canonical guidance; no linter enforces full table-driven structure |
| GO-TESTING-INTEGRATION-BUILD-TAGS-1 | structured | cmd/go build constraint docs; convention is well-defined but linter-less |

Two rules are `prose` (architectural/design decisions; not mechanically checkable):

| Rule ID | Enforcement | Rationale |
|---|---|---|
| GO-TESTING-BLACK-BOX-PKG-TEST-1 | prose | testpackage linter exists but the decision (when to use white-box) is architectural |
| GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1 | prose | Design pattern; no linter detects "mock not injected at interface boundary" |

## Full Rule Table

| Rule ID | Verification | Primary Source URL | Linter Rule | Status |
|---|---|---|---|---|
| GO-TESTING-COLOCATED-TEST-FILES-1 | grounded | https://pkg.go.dev/testing | go build: _test.go exclusion | grounded |
| GO-TESTING-BLACK-BOX-PKG-TEST-1 | grounded | https://pkg.go.dev/cmd/go#hdr-Test_packages | golangci-lint: testpackage | grounded |
| GO-TESTING-TABLE-DRIVEN-T-RUN-1 | grounded | https://go.dev/wiki/TableDrivenTests | — | grounded |
| GO-TESTING-INTEGRATION-BUILD-TAGS-1 | grounded | https://pkg.go.dev/cmd/go#hdr-Build_constraints | — | grounded |
| GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1 | grounded | https://pkg.go.dev/github.com/stretchr/testify/mock | — | grounded |
| GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1 | grounded | https://pkg.go.dev/testing#T.TempDir | golangci-lint: tparallel | grounded |
| GO-TESTING-HELPER-T-HELPER-1 | grounded | https://pkg.go.dev/testing#T.Helper | golangci-lint: thelper | grounded |

---

## Per-Rule Grounding Detail

### GO-TESTING-COLOCATED-TEST-FILES-1
- **Claim**: Unit test files are co-located with source, named `_test.go`.
- **Authority**: `pkg.go.dev/testing` (primary); `go.dev/doc/code#Testing` (official tutorial); `pkg.go.dev/cmd/go#hdr-Test_packages` (tool behaviour).
- **Mechanical bar**: The go tool itself enforces `_test.go` exclusion from production builds. This is a language-toolchain rule, not a linter convention.
- **Verdict**: grounded.

### GO-TESTING-BLACK-BOX-PKG-TEST-1
- **Claim**: Use `package foo_test` for black-box tests; `package foo` only when internal access is genuinely needed.
- **Authority**: `pkg.go.dev/cmd/go#hdr-Test_packages` (defines the two legal package declarations); `go.dev/doc/effective_go#interfaces_and_types` (implicit satisfaction); `golangci-lint testpackage` linter.
- **Mechanical bar**: The `testpackage` golangci-lint linter enforces separate `_test` packages. The architectural choice (when white-box is justified) is prose.
- **Verdict**: grounded. Enforcement kept as `prose` because the decision of when to use white-box is design-level, even though a linter could flag violations.

### GO-TESTING-TABLE-DRIVEN-T-RUN-1
- **Claim**: Multiple-case tests use a slice/map of structs with `t.Run` subtests.
- **Authority**: `go.dev/wiki/TableDrivenTests` (canonical Go Wiki); `pkg.go.dev/testing#hdr-Subtests_and_Sub_benchmarks` (t.Run API); `go.dev/wiki/TestComments` (t.Error vs t.Fatal guidance, subtest naming guidance).
- **Mechanical bar**: No single linter enforces full table-driven structure. Enforcement is `structured` (CONVENTIONS.md citation).
- **Verdict**: grounded.

### GO-TESTING-INTEGRATION-BUILD-TAGS-1
- **Claim**: Integration tests use `//go:build integration` and live in `*_integration_test.go` files.
- **Authority**: `pkg.go.dev/cmd/go#hdr-Build_constraints` (//go:build directive); `pkg.go.dev/cmd/go#hdr-Test_packages` (build tag + test file behaviour). The `*_integration_test.go` file naming convention is widely established community practice backed by cmd/go's build constraint system.
- **Mechanical bar**: No linter enforces the integration build tag convention. Enforcement is `structured`.
- **Verdict**: grounded.

### GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1
- **Claim**: Mocks implement the interface the production code accepts; injected through the same constructor.
- **Authority**: `pkg.go.dev/github.com/stretchr/testify/mock` (canonical mock pattern); `go.dev/doc/effective_go#interfaces_and_types` (implicit interface satisfaction); `go.dev/wiki/CodeReviewComments#interfaces` (consumer-defined interfaces).
- **Mechanical bar**: No linter detects "mock not injected at interface boundary." Enforcement is `prose`.
- **Verdict**: grounded.

### GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1
- **Claim**: Tests never call `time.Sleep`; use `t.TempDir`, `t.Setenv`, `t.Cleanup` for isolation.
- **Authority**: `pkg.go.dev/testing#T.TempDir`, `pkg.go.dev/testing#T.Setenv`, `pkg.go.dev/testing#T.Cleanup` (official testing package docs); `golangci-lint tparallel` (detects parallel/sleep misuse).
- **Mechanical bar**: `golangci-lint: tparallel` partially enforces this (t.Parallel + t.Setenv interaction). No single linter bans `time.Sleep` in tests specifically. Enforcement kept as `mechanical` because the t.TempDir / t.Setenv / t.Cleanup APIs are explicitly documented for this purpose and the tparallel linter covers the parallel-safety subset.
- **Verdict**: grounded.

### GO-TESTING-HELPER-T-HELPER-1
- **Claim**: Test helper functions call `t.Helper()` as first statement.
- **Authority**: `pkg.go.dev/testing#T.Helper` (official API); `go.dev/wiki/TestComments` (explicit guidance with example); `golangci-lint thelper` (mechanical enforcement).
- **Mechanical bar**: `golangci-lint: thelper` mechanically detects test helper functions missing `t.Helper()`.
- **Verdict**: grounded.

---

## Authorities Consulted

- **pkg.go.dev/testing** — https://pkg.go.dev/testing — primary Go testing package reference; _test.go naming, TestXxx signatures, t.Run, t.Helper, t.TempDir, t.Setenv, t.Cleanup
- **Go Wiki — TableDrivenTests** — https://go.dev/wiki/TableDrivenTests — canonical guidance on slice-of-structs + t.Run pattern
- **Go Wiki — TestComments** — https://go.dev/wiki/TestComments — t.Helper, t.Error vs t.Fatal, subtest naming, comparison idioms
- **Go Wiki — CodeReviewComments** — https://go.dev/wiki/CodeReviewComments — interfaces at consumer, test error message format
- **How to Write Go Code — Testing** — https://go.dev/doc/code#Testing — co-location of _test.go with source
- **cmd/go — Test packages** — https://pkg.go.dev/cmd/go#hdr-Test_packages — _test.go compilation, package foo_test semantics, testdata directory
- **cmd/go — Build constraints** — https://pkg.go.dev/cmd/go#hdr-Build_constraints — //go:build integration tag syntax
- **Effective Go — Interfaces** — https://go.dev/doc/effective_go#interfaces_and_types — implicit interface satisfaction (foundation for mock pattern)
- **testify/mock** — https://pkg.go.dev/github.com/stretchr/testify/mock — canonical Go mock library
- **golangci-lint linters** — https://golangci-lint.run/docs/linters/ — thelper, tparallel, paralleltest, testpackage linter descriptions
- **kunwardeep/paralleltest** — https://pkg.go.dev/github.com/kunwardeep/paralleltest — paralleltest linter for t.Parallel() enforcement
- **moricho/tparallel** — https://pkg.go.dev/github.com/moricho/tparallel — tparallel linter for inappropriate t.Parallel() usage
