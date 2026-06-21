# Rule Grounding Report: rust-core family

Generated: 2026-06-20  
Branch: ground/rust-core  
Scope: `crates/rules/principles/rust/*.toml` (top-level only; excludes axum/, dioxus/, seaorm/, sqlx/, tokio/ subdirs)

## Summary

| Metric | Count |
|--------|-------|
| Total rules | 11 |
| Grounded | 7 |
| Ungrounded (left as draft) | 4 |
| Demoted (enforcement changed) | 0 |

## Ungrounded Rules (no real authoritative Rust source found)

- **RUST-DOMAIN-7** — Explicit UnitOfWork parameter on transactional repository methods. This is a Rust-specific architectural decision pattern; no single canonical Rust-lang authority (API Guidelines, The Rust Book, Clippy) covers the UoW trait + downcast pattern. Left as draft.
- **RUST-HEADLESS-CORE-1** — Headless core crate plus per-framework adapters. This is a general UI component architecture principle (hexagonal/ports-and-adapters). No Rust-lang authoritative source covers this; the concept is language-agnostic. Left as draft.
- **RUST-MAPPER-1** — Mappers live in their own crate. This is a hexagonal architecture layering decision. No canonical Rust-lang authority covers mapper crate placement. Left as draft.
- **RUST-PURE-STATE-TRANSITIONS-1** — Pure state transition functions; effects at the edges. This is a general functional programming/ELM-architecture pattern with no Rust-specific canonical authority in API Guidelines, The Rust Book, or Clippy. Left as draft.

## Demoted Rules

None. No mechanical rules required demotion to prose.

---

## Full Grounding Table

| Rule ID | Verification | Source URL | Linter Rule | Status |
|---------|-------------|------------|-------------|--------|
| RUST-DOMAIN-1 | grounded | https://doc.rust-lang.org/book/ch07-00-managing-growing-projects-with-packages-crates-and-modules.html | — | grounded |
| RUST-DOMAIN-1 | grounded | https://doc.rust-lang.org/reference/items/modules.html | — | grounded |
| RUST-DOMAIN-2 | grounded | https://rust-lang.github.io/api-guidelines/type-safety.html (C-NEWTYPE) | — | grounded |
| RUST-DOMAIN-2 | grounded | https://doc.rust-lang.org/rust-by-example/generics/new_types.html | — | grounded |
| RUST-DOMAIN-3 | grounded | https://rust-lang.github.io/api-guidelines/type-safety.html (C-NEWTYPE) | — | grounded |
| RUST-DOMAIN-3 | grounded | https://rust-lang.github.io/api-guidelines/dependability.html (C-VALIDATE) | — | grounded |
| RUST-DOMAIN-4 | grounded | https://rust-lang.github.io/api-guidelines/interoperability.html (C-GOOD-ERR) | — | grounded |
| RUST-DOMAIN-4 | grounded | https://docs.rs/thiserror/latest/thiserror/ | — | grounded |
| RUST-DOMAIN-4 | grounded | https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html | — | grounded |
| RUST-DOMAIN-5 | grounded | https://doc.rust-lang.org/book/ch17-00-async-await.html | — | grounded |
| RUST-DOMAIN-5 | grounded | https://tokio.rs/tokio/tutorial | — | grounded |
| RUST-DOMAIN-6 | grounded | https://rust-lang.github.io/api-guidelines/interoperability.html (C-GOOD-ERR) | — | grounded |
| RUST-DOMAIN-6 | grounded | https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html | — | grounded |
| RUST-DOMAIN-7 | draft | — | — | ungrounded |
| RUST-HEADLESS-CORE-1 | draft | — | — | ungrounded |
| RUST-MAPPER-1 | draft | — | — | ungrounded |
| RUST-NO-UNWRAP-1 | grounded | https://rust-lang.github.io/rust-clippy/master/index.html#unwrap_used | clippy: unwrap_used | grounded |
| RUST-NO-UNWRAP-1 | grounded | https://doc.rust-lang.org/clippy/lint_configuration.html | — | grounded |
| RUST-NO-UNWRAP-1 | grounded | https://doc.rust-lang.org/book/ch09-03-to-panic-or-not-to-panic.html | — | grounded |
| RUST-PURE-STATE-TRANSITIONS-1 | draft | — | — | ungrounded |

---

## Grounded Rule Narratives

### RUST-DOMAIN-1 — Single domain crate, modules by bounded context

The Rust Book chapter 7 ("Managing Growing Projects with Packages, Crates, and Modules") and the Rust Reference on Modules establish the language-level primitives (crates vs. modules, visibility, file-system conventions) that this rule is premised on. The rule's architectural stance (one crate, modules for bounded contexts) is a design choice built on those primitives; the sources confirm the meaning of the terms used and Rust's module system behavior.

Authorities used: The Rust Book (ch7), Rust Reference (Modules).

### RUST-DOMAIN-2 — Newtype IDs (every ID is a wrapper, never a bare Uuid)

Grounded by Rust API Guideline C-NEWTYPE ("Newtypes provide static distinctions"), which describes exactly the pattern: wrapping a primitive to get compile-time type safety and prevent catastrophic mis-assignment. Rust By Example's Newtype Idiom page provides the canonical code demonstration.

No clippy lint enforces newtype usage for IDs specifically; the rule's enforcement remains "structured" (code review / PR checks).

Authorities used: Rust API Guidelines (C-NEWTYPE), Rust By Example (New Type Idiom).

### RUST-DOMAIN-3 — Newtype validated strings

Grounded by the same C-NEWTYPE guideline plus C-VALIDATE ("Functions validate their arguments"), which explicitly recommends static enforcement via constrained type constructors as the preferred tier over runtime checks. The API Guidelines describe exactly the `Email::try_new` pattern the rule mandates.

Authorities used: Rust API Guidelines (C-NEWTYPE, C-VALIDATE).

### RUST-DOMAIN-4 — Errors via thiserror enums per crate

Grounded by Rust API Guideline C-GOOD-ERR ("Error types are meaningful and well-behaved"), which requires `std::error::Error` implementation, `Send + Sync`, and meaningful typed error messages; the `thiserror` crate documentation confirms it produces exactly these implementations as a derive macro. The Rust Book chapter 9.2 covers `Result<T, E>` and typed error handling.

The anyhow-vs-thiserror split (libraries use typed enums, binaries use anyhow) is a well-established Rust ecosystem convention backed by the design goals described in both the `thiserror` and `anyhow` crate documentation; the rule applies it per-crate.

Authorities used: Rust API Guidelines (C-GOOD-ERR), thiserror docs, The Rust Book (ch9.2).

### RUST-DOMAIN-5 — Async all the way down

Grounded by The Rust Book chapter 17 ("Async and Await"), which establishes that `await` must be called in an async context and that mixing blocking I/O in async contexts defeats concurrency. The Tokio tutorial explicitly states "when writing asynchronous code, you cannot use the ordinary blocking APIs provided by the Rust standard library" and must use async versions.

No clippy lint enforces "no block_on in layered code"; enforcement stays "prose".

Authorities used: The Rust Book (ch17), Tokio Tutorial.

### RUST-DOMAIN-6 — Category-scoped errors, one shared enum per failure category

Grounded by C-GOOD-ERR, which requires error types to implement `std::error::Error` and be well-behaved at crate boundaries, and by The Rust Book ch9.2, which explains the design rationale for typed errors. The "one enum per category" partitioning is a specific interpretation of how to organize crate error vocabularies that follows directly from C-GOOD-ERR's guidance to avoid `()` errors and to have meaningful crate-specific types.

Authorities used: Rust API Guidelines (C-GOOD-ERR), The Rust Book (ch9.2).

### RUST-NO-UNWRAP-1 — unwrap() forbidden in non-test code

Grounded by the real Clippy lint `clippy::unwrap_used` (restriction category), which is exactly what the rule mandates. The `allow-unwrap-in-tests` configuration option (documented in Clippy's lint_configuration page) provides the test exemption described in the rule's `qualifies` field. The Rust Book chapter 9.3 ("To panic! or Not to panic!") provides the canonical guidance distinguishing test panics (correct) from library/server panics (avoid).

Linter: `clippy: unwrap_used` (restriction, disabled by default; enabled via `[workspace.lints.clippy] unwrap_used = "deny"`).

Authorities used: Clippy lint index (#unwrap_used), Clippy Configuration (allow-unwrap-in-tests), The Rust Book (ch9.3).

---

## Authorities Consulted

- Rust API Guidelines: https://rust-lang.github.io/api-guidelines/ (sections: type-safety, dependability, interoperability, flexibility, predictability)
- The Rust Book: https://doc.rust-lang.org/book/ (chapters 7, 9, 10, 17)
- Rust By Example: https://doc.rust-lang.org/rust-by-example/
- Rust Reference (Modules): https://doc.rust-lang.org/reference/items/modules.html
- Clippy Lint Index: https://rust-lang.github.io/rust-clippy/master/index.html
- Clippy Configuration: https://doc.rust-lang.org/clippy/lint_configuration.html
- Tokio Tutorial: https://tokio.rs/tokio/tutorial
- thiserror crate: https://docs.rs/thiserror/latest/thiserror/
