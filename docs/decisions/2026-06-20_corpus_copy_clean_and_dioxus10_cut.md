# Corpus Copy Clean and RUST-DIOXUS-10 Cut

**Date:** 2026-06-20
**Branch:** fix/corpus-copy

## What changed

Two corpus maintenance passes applied to `crates/rules/principles/**/*.toml`.

### 1. RUST-DIOXUS-10 deleted

`crates/rules/principles/rust/dioxus/rust-dioxus-10-auth-can-flags.toml` was deleted.

**Reason:** RUST-DIOXUS-10 duplicated `ARCH-SERVER-AUTHZ-1` (in the permissions domain). Both rules express the identical invariant: authorization decisions computed server-side; UI gates affordances on capability flags carried by the response; raw permission codes never reach the client. ARCH-SERVER-AUTHZ-1 is the canonical home for this invariant (permissions domain, mechanical enforcement, lint-backed, grounded against CONVENTIONS.md). Keeping a second copy in the Dioxus domain would require the two to stay in sync as the rule text evolves, and the permissions domain already audits every project that uses any UI stack. Deleting the duplicate reduces maintenance surface and eliminates the risk of the two copies diverging.

References in `docs/rule-grounding/citation-validation.md` and `docs/rule-grounding/rust-frameworks.md` were also removed, and the rust-frameworks summary counts updated (43 → 42 total, 5 → 4 ungrounded).

### 2. Default-status boilerplate removed from all `why` fields

A deterministic Perl script (`/tmp/fix-why-boilerplate.pl`) was run across all 285 TOML files. It applied three replacements in `[decision].why` and each `[[option]].why`:

| Pattern removed | Replacement |
|----------------|-------------|
| Leading `"The adopted default. "` | Stripped (keep the rest of the sentence) |
| `"A defensible alternative the project considered and did not adopt as the default."` | `"A defensible alternative the project considered."` |
| `"A defensible alternative the project considered and did not adopt as the default: <rest>"` | `"A defensible alternative the project considered: <rest>"` |

**Reason:** The UI badges the default option and the non-default options visually; repeating that status in the prose is both redundant and wrong for rules that declare no default. The boilerplate was generated automatically and provided no substantive reasoning. Removing it makes the `why` fields carry only the actual reasoning content.

**Scope:** 254 files modified, 489 line changes, 0 .rs files touched.

### Verification

`cargo test -p camerata-rules` green (50 unit tests + 1 doc test) after both changes. All TOML still parses.

## Files touched

- `crates/rules/principles/rust/dioxus/rust-dioxus-10-auth-can-flags.toml` (deleted)
- `docs/rule-grounding/citation-validation.md` (RUST-DIOXUS-10 row removed)
- `docs/rule-grounding/rust-frameworks.md` (RUST-DIOXUS-10 entries removed, summary counts updated)
- `crates/rules/principles/**/*.toml` (254 files: why-text boilerplate stripped)
- `docs/decisions/2026-06-20_corpus_copy_clean_and_dioxus10_cut.md` (this file)
