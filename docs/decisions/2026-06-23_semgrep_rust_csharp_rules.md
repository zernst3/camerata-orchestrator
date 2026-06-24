# 2026-06-23 Semgrep Rust and C# Security Rules

## Context

Camerata's bundled semgrep ruleset (`crates/server/assets/semgrep-rules/security.yml`)
covered six languages — Python, JavaScript, TypeScript, Go, Java, Ruby — across four
security families: hardcoded secrets, eval/exec injection, SQL injection, weak hashing,
path traversal, and subprocess shell injection.

Semgrep itself supports Rust and C#. Any Camerata project that is written in either
language would scan as zero findings, which is a misleading "clean" result rather than
a genuine pass. The coverage gap was identified as the immediate concern: not
correctness of the existing rules, but absence of rules entirely for two supported
target languages.

## Decision

Add dedicated rules for Rust and C# covering the same security families already
present for other languages, and extend the existing `camerata.security.hardcoded-secret`
rule's `languages` list to include both.

### Rules added

**Rust (3 new rules):**

| Rule ID | Family | FP-avoidance approach |
|---|---|---|
| `camerata.security.sql-string-concat-rust` | SQL injection | Requires SQL keyword (SELECT/INSERT/UPDATE/DELETE/FROM/WHERE) in the `format!` template string via `metavariable-regex`; avoids flagging unrelated format strings |
| `camerata.security.weak-hash-rust` | Weak cryptography | Matches specific call sites of the `md5` and `sha1` crates (`md5::compute`, `Md5::new`, `Sha1::new`, turbofish trait forms); concrete API names rather than broad string matching |
| `camerata.security.disabled-tls-rust` | Disabled TLS | Matches `danger_accept_invalid_certs(true)` / `danger_accept_invalid_hostnames(true)` on any builder; the `true` argument literal is part of the pattern so toggling to `false` does not trigger |

**C# (3 new rules):**

| Rule ID | Family | FP-avoidance approach |
|---|---|---|
| `camerata.security.sql-string-concat-csharp` | SQL injection | Targets `SqlCommand`/`SqliteCommand` constructors and `CommandText` property assignments with interpolated strings or concatenation; Entity Framework and Dapper parameterised calls don't match these patterns |
| `camerata.security.weak-hash-csharp` | Weak cryptography | Matches `MD5.Create()`, `SHA1.Create()`, and their legacy `CryptoServiceProvider`/`SHA1Managed` constructor forms from `System.Security.Cryptography`; enumerates concrete factory method names |
| `camerata.security.disabled-tls-csharp` | Disabled TLS | Matches `ServerCertificateValidationCallback = (... => true)` and the `delegate { return true; }` forms on both `ServicePointManager` and `HttpClientHandler`; the `true` return value or literal is part of every pattern, so a real pinning implementation does not trigger |

**Extended rule:**

`camerata.security.hardcoded-secret` now lists `rust` and `csharp` in its `languages`
array. The existing `$VAR = "..."` pattern is language-agnostic at the semgrep
abstract-syntax level and fires correctly on Rust `let` bindings and C# field/property
assignments.

## Why these families

- **SQL injection** and **weak hashing** are the two most frequently occurring
  confirmed findings in the existing corpus across Python and JS scans, making Rust/C#
  parity a direct impact multiplier.
- **Disabled TLS** is a single-line configuration mistake common in both ecosystems
  (reqwest's `danger_*` API in Rust; `ServicePointManager` global override in C#).
  The API names are distinctive enough that pattern precision is high.
- **Hardcoded secrets** already had a cross-language pattern; adding two language tags
  is lower risk than writing a new rule.

## What was not added

- **Path traversal for Rust/C#**: The Rust `std::fs::File::open($PATH)` pattern would
  match nearly all file I/O. Rust's type system and the common `PathBuf` usage make
  AST-level taint hard to express without generating excessive false positives. Deferred
  until a taint-mode rule can be validated against a sample corpus.
- **Exec/shell injection for C#**: `Process.Start` with user data is the equivalent
  of `subprocess(shell=True)` but the pattern space is wide (direct call, `ProcessStartInfo`
  builder, etc.). Deferred for a focused follow-up rule.

## Dedup note

Rule IDs follow the `camerata.security.<family>-<lang>` suffix convention already
established for Python and JS variants. The cross-tool dedup layer (separate change)
can match these by the `camerata.security.sql-string-concat-*` / `weak-hash-*` /
`disabled-tls-*` prefix to group findings from the same family across tools.
