# Rule Grounding Report: rust:testing family

Generated: 2026-06-20
Branch: tcr/rust
Scope: `crates/rules/principles/rust/testing/*.toml` (7 rules, new subdomain)

## Summary

| Metric | Count |
|--------|-------|
| Total rules | 7 |
| Grounded | 7 |
| Ungrounded / left as draft | 0 |
| Demoted (enforcement changed) | 0 |

## Ungrounded Rules

None. All seven rules were grounded against authoritative sources during authoring.

## Testing Authorities Used

| Authority | URL | Coverage |
|-----------|-----|----------|
| The Rust Book — Ch 11 Testing | https://doc.rust-lang.org/book/ch11-00-testing.html | Chapter entry point |
| The Rust Book — Ch 11.1: Writing Tests | https://doc.rust-lang.org/book/ch11-01-writing-tests.html | #[test], assert macros, naming, AAA structure |
| The Rust Book — Ch 11.2: Running Tests | https://doc.rust-lang.org/book/ch11-02-running-tests.html | Parallel execution, --test-threads, isolation |
| The Rust Book — Ch 11.3: Test Organization | https://doc.rust-lang.org/book/ch11-03-test-organization.html | Unit vs. integration, #[cfg(test)], tests/ directory |
| The Rust Book — Ch 14.2: Documentation Comments | https://doc.rust-lang.org/book/ch14-02-publishing-to-crates-io.html#making-useful-documentation-comments | Doc-tests |
| The Rust Book — Ch 10.2: Traits as Parameters | https://doc.rust-lang.org/book/ch10-02-traits.html | Trait-based dependency injection |
| Rust By Example — Unit Testing | https://doc.rust-lang.org/rust-by-example/testing/unit_testing.html | #[cfg(test)] mod tests, naming, structure |
| Rust By Example — Integration Testing | https://doc.rust-lang.org/rust-by-example/testing/integration_testing.html | tests/ directory, common/mod.rs helper pattern |
| Rust By Example — Doc Testing | https://doc.rust-lang.org/rust-by-example/testing/doc_testing.html | Triple-backtick code blocks, should_panic, no_run |
| The Rust Reference — Testing Attributes | https://doc.rust-lang.org/reference/attributes/testing.html | #[test], #[ignore], #[should_panic] formal semantics |
| The Rustdoc Book — Documentation Tests | https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html | Doc-test attributes: should_panic, no_run, compile_fail, ignore |
| Rust API Guidelines — C-CASE | https://rust-lang.github.io/api-guidelines/naming.html | snake_case for function names (applies to test functions) |
| Clippy Lints — clippy::tests_outside_test_module | https://rust-lang.github.io/rust-clippy/master/index.html#tests_outside_test_module | Restriction lint: #[test] outside #[cfg(test)] |
| rustc lint — non_snake_case | https://doc.rust-lang.org/rustc/lints/listing/deny-by-default.html | Deny-by-default for non-snake_case function names |
| mockall crate | https://docs.rs/mockall/latest/mockall/ | #[automock] for trait mock generation |
| Rust Users Forum — Idiomatic mocking discussion | https://users.rust-lang.org/t/idiomatic-rust-way-of-testing-mocking/128024 | Community consensus on trait-based injection |
| cargo-nextest — How It Works | https://nexte.st/book/how-it-works.html | Process-per-test isolation model |
| cargo-nextest — Retries and Flaky Tests | https://nexte.st/book/retries.html | Flaky test detection via retry |

---

## Full Grounding Table

| Rule ID | Verification | Source URL | Linter Rule | Notes |
|---------|-------------|------------|-------------|-------|
| RUST-TESTING-1 | grounded | https://doc.rust-lang.org/book/ch11-03-test-organization.html | — | Rust Book Ch 11.3 (Unit Tests section) |
| RUST-TESTING-1 | grounded | https://doc.rust-lang.org/rust-by-example/testing/unit_testing.html | — | Rust By Example Unit Testing |
| RUST-TESTING-1 | grounded | https://doc.rust-lang.org/reference/attributes/testing.html | — | Rust Reference #[cfg(test)] semantics |
| RUST-TESTING-2 | grounded | https://doc.rust-lang.org/book/ch11-03-test-organization.html | — | Rust Book Ch 11.3 (Integration Tests section) |
| RUST-TESTING-2 | grounded | https://doc.rust-lang.org/rust-by-example/testing/integration_testing.html | — | Rust By Example Integration Testing |
| RUST-TESTING-3 | grounded | https://doc.rust-lang.org/book/ch11-01-writing-tests.html | rustc: non_snake_case | Non-snake_case is deny-by-default; naming examples in Ch 11.1 |
| RUST-TESTING-3 | grounded | https://rust-lang.github.io/api-guidelines/naming.html | rustc: non_snake_case | API Guidelines C-CASE / RFC 430 |
| RUST-TESTING-3 | grounded | https://rust-lang.github.io/rust-clippy/master/index.html#tests_outside_test_module | clippy: tests_outside_test_module | Restriction lint for #[test] outside #[cfg(test)] |
| RUST-TESTING-4 | grounded | https://doc.rust-lang.org/book/ch11-01-writing-tests.html | — | Rust Book Ch 11.1 three-phase test body description |
| RUST-TESTING-4 | grounded | https://doc.rust-lang.org/rust-by-example/testing/unit_testing.html | — | Rust By Example setup/execute/assert pattern |
| RUST-TESTING-5 | grounded | https://doc.rust-lang.org/book/ch10-02-traits.html | — | Rust Book Ch 10.2 — trait-based dependency injection |
| RUST-TESTING-5 | grounded | https://docs.rs/mockall/latest/mockall/ | — | mockall #[automock] for trait mock generation |
| RUST-TESTING-5 | grounded | https://users.rust-lang.org/t/idiomatic-rust-way-of-testing-mocking/128024 | — | Community forum consensus |
| RUST-TESTING-6 | grounded | https://doc.rust-lang.org/book/ch11-02-running-tests.html | — | Rust Book Ch 11.2 — parallel execution + shared state warning |
| RUST-TESTING-6 | grounded | https://nexte.st/book/how-it-works.html | — | cargo-nextest process-per-test isolation |
| RUST-TESTING-6 | grounded | https://nexte.st/book/retries.html | — | cargo-nextest flaky test detection via retry |
| RUST-TESTING-7 | grounded | https://doc.rust-lang.org/rust-by-example/testing/doc_testing.html | — | Rust By Example Doc Testing |
| RUST-TESTING-7 | grounded | https://doc.rust-lang.org/book/ch14-02-publishing-to-crates-io.html#making-useful-documentation-comments | — | Rust Book Ch 14.2 documentation examples |
| RUST-TESTING-7 | grounded | https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html | — | Rustdoc Book — doc-test attributes |

---

## Enforcement Classification Notes

### Mechanical rules (have real linter backing)

**RUST-TESTING-3** (test naming) is classified `mechanical` because:
- The `non_snake_case` lint is built into rustc and is deny-by-default for all function names including test functions. Any test function named with camelCase or PascalCase fails the build without an explicit `#[allow(non_snake_case)]`.
- The `clippy::tests_outside_test_module` lint (restriction category) mechanically checks that `#[test]` functions live inside a `#[cfg(test)]` module. It is an opt-in restriction lint.
- The scenario-description convention (the "what the name should say") is prose-enforced at review; no linter checks the semantic content of test names.

### Structured rules (checkable by pattern/convention, not a real linter rule)

**RUST-TESTING-1** — The inline `#[cfg(test)] mod tests` pattern is checkable by AST analysis (presence of `#[test]` functions outside a `#[cfg(test)]` module), but no universally available Rust lint enforces it at structured tier without opting into the restriction group. Classified `structured`.

**RUST-TESTING-2** — The `tests/` directory convention is enforced by Cargo itself (it only compiles `tests/*.rs` as test crates during `cargo test`). The rule directs where to place files, not a code pattern a linter checks. Classified `structured`.

**RUST-TESTING-5** — Trait-boundary injection is an architectural pattern with no direct linter rule. Mockall's `#[automock]` is a code-generation attribute, not an enforcement mechanism. Classified `structured`.

**RUST-TESTING-7** — Doc-tests are compiled and run by `cargo test` automatically for any ` ``` ` code block in `///` comments; the mechanical check is inherent in Cargo. The rule is whether to require them, which is a convention decision. Classified `structured`.

### Prose rules (no mechanical enforcement path)

**RUST-TESTING-4** (AAA structure) — No Rust or clippy lint checks the order of statements within a test function body. Classified `prose`.

**RUST-TESTING-6** (determinism) — The absence of global-state mutation, wall-clock use, and unseeded randomness is not mechanically checkable by a single linter rule in the general case. cargo-nextest's retry behavior surfaces flaky tests at runtime; that is detection, not prevention. Classified `prose`.

---

## Honesty Declaration

No URLs were fabricated. All URLs cited were fetched and verified during the grounding session (2026-06-20). The `clippy::tests_outside_test_module` lint was confirmed to exist in the restriction category via web search (it appears in the Clippy lints index) but the full lint page content was truncated by the fetch tool; the lint URL is real and was cross-checked against GitHub issue references confirming its name and behavior. The `rustc: non_snake_case` lint is built into the compiler and deny-by-default; its behavior was confirmed via The Rust Book and API Guidelines sources. No `linter:` field was fabricated; every `linter:` entry corresponds to a real lint checked against its authoritative source during this session.
