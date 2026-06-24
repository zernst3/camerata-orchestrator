# 2026-06-23: Semgrep Rust + C# rules verified against real binary

## Context

The 2026-06-23 `feat(semgrep)` commit added 6 new rules (3 Rust, 3 C#) to
`crates/server/assets/semgrep-rules/security.yml` and extended
`hardcoded-secret` to cover Rust and C#. A follow-up audit using the
provisioned `semgrep 1.167` binary revealed three rule families that either
silently produced zero findings on confirmed violations or caused
PatternParseError at scan time.

This fix-up commit documents the root causes, records which rules were changed
and how, and confirms every shipped Rust + C# rule now provably fires.

---

## Verification method

For each Rust and C# rule, a minimal fixture file was written containing a
clear violation, and the real semgrep binary was invoked:

```
/Users/zacharyernst/Library/Application Support/camerata/tooling/semgrep-venv/bin/semgrep \
  --quiet --config crates/server/assets/semgrep-rules/security.yml \
  --no-git-ignore <fixture>
```

Rules were confirmed MATCHED when >= 1 finding was reported.

---

## Per-rule outcome table

| Rule ID | Fixture | Status | Notes |
|---|---|---|---|
| `camerata.security.hardcoded-secret` (Python, JS, TS, Java, Ruby) | `secret.py` (`password = "hunter2"`, `api_key = 'sk-...'`) | MATCHED | Rule narrowed to languages with single-quote string literals |
| `camerata.security.hardcoded-secret-dquote` (Go, Rust) | `secret.rs` (`let password = "hunter2"`), `secret.go` (`password := "hunter2"`) | MATCHED | New rule; Go + Rust extracted from original because `'...'` pattern causes parse errors in both |
| `camerata.security.sql-string-concat-rust` | `sql_inject.rs` (`format!("SELECT * FROM users WHERE id = {}", id)`) | MATCHED (after fix) | Pattern changed — see below |
| `camerata.security.weak-hash-rust` | `weak_hash.rs` (`md5::compute(data)`, `Sha1::new()`) | MATCHED | No change needed; rule was correct |
| `camerata.security.disabled-tls-rust` | `tls_disabled.rs` (`.danger_accept_invalid_certs(true)`) | MATCHED | No change needed; rule was correct |
| `camerata.security.sql-string-concat-csharp` | `sql_inject.cs` (`new SqlCommand($"SELECT...{id}", conn)`) | MATCHED | No change needed; rule was correct |
| `camerata.security.weak-hash-csharp` | `weak_hash.cs` (`MD5.Create()`, `SHA1.Create()`, `new MD5CryptoServiceProvider()`) | MATCHED | No change needed; rule was correct |
| `camerata.security.disabled-tls-csharp` | `tls_all.cs` (lambda + delegate on both ServicePointManager and HttpClientHandler) | MATCHED (after fix) | Pattern changed — see below |
| `camerata.security.subprocess-shell-true` (Python, regression) | `test.py` (`subprocess.run("...", shell=True)`) | MATCHED | Regression confirmed; no change |

---

## Root causes and pattern changes

### 1. `sql-string-concat-rust`: `metavariable-regex` silently no-ops on macro arguments

**Original pattern (broken):**
```yaml
patterns:
  - pattern-either:
      - pattern: format!($TEMPLATE, ...)
      - pattern: $S + $VAR
  - metavariable-regex:
      metavariable: $TEMPLATE
      regex: '(?i)(SELECT|INSERT|UPDATE|DELETE|FROM|WHERE)'
```

**Root cause:** semgrep 1.167 does not evaluate `metavariable-regex` constraints
against metavariables bound inside Rust macro argument positions. The constraint
parses without error but is silently ignored, returning 0 findings even when
`$TEMPLATE` visibly contains `"SELECT * FROM users WHERE id = {}"`.

The `$S + $VAR` form for string concatenation has a related problem: `$S` binds
to the variable name on the left side of the `+` operator (e.g. `base`), not to
the string literal that was initially assigned to that variable. The SQL-keyword
regex therefore never matches `base`, and those cases also silently return 0.

**Fix:** Replace both patterns with a single `pattern-regex` operating on raw
file text, which is unaffected by parser limitations:

```yaml
pattern-regex: '(?i)format!\s*\(\s*"[^"]*\b(?:SELECT|INSERT|UPDATE|DELETE|FROM|WHERE)\b[^"]*"[^)]*,'
```

This matches `format!` calls whose first argument string contains a SQL keyword
followed by at least one more argument (the positional hole values). False
positive check on a non-SQL `format!("Hello, {}!", name)` confirmed 0 findings.

### 2. `disabled-tls-csharp`: ellipsis-wildcard lambda syntax is invalid C#

**Original pattern (broken):**
```yaml
pattern-either:
  - pattern: ServicePointManager.ServerCertificateValidationCallback = (... => true)
  - ...
```

**Root cause:** `(... => true)` is not valid C# lambda syntax in semgrep's
grammar. It causes `Stdlib.Parsing.Parse_error` at scan time, silently
skipping the entire rule for any `.cs` file.

**Fix:** Replace the wildcard ellipsis with four explicit metavariable parameters
matching the actual `RemoteCertificateValidationCallback` signature:

```yaml
pattern-either:
  - pattern: ServicePointManager.ServerCertificateValidationCallback = ($A, $B, $C, $D) => true;
  - pattern: ServicePointManager.ServerCertificateValidationCallback = delegate { return true; };
  - pattern: $HANDLER.ServerCertificateCustomValidationCallback = ($A, $B, $C, $D) => true;
  - pattern: $HANDLER.ServerCertificateCustomValidationCallback = delegate { return true; };
```

All four forms confirmed MATCHED on explicit fixtures.

### 3. `hardcoded-secret`: `'...'` causes parse errors in Go and Rust; C# variable-assignment pattern produces 0 findings

**Root cause (Rust + Go):** `$VAR = '...'` is not a string-literal assignment in
either language. In Rust, `'...'` is lifetime syntax; in Go, it is a rune literal.
Both semgrep parsers raise a parse/lexical error and skip the rule at scan time.

**Root cause (C#):** semgrep's C# parser does not match the `$VAR = "..."`
pattern for local variable declarations (returns 0 findings on confirmed
violations even in a simple test fixture). The pattern parses without error but
never fires.

**Fix for Go + Rust:** Remove both languages from the multi-language
`hardcoded-secret` rule (which includes the `'...'` branch) and add a new
`camerata.security.hardcoded-secret-dquote` rule covering `[go, rust]` with
only the `$VAR = "..."` pattern. Confirmed MATCHED on both languages.

**Fix for C#:** Remove `csharp` from `hardcoded-secret` and do NOT add a
dedicated C# rule. The C# `$VAR = "..."` assignment pattern is unreliable in
this semgrep version. C# hardcoded secrets are covered by the deterministic
floor (token-entropy scan), not by the semgrep layer.

**Bonus fix:** The original rule included a `metavariable-regex` on `$LITERAL`
that was never bound by any pattern (not a match for `$VAR = "..."`). This was
a latent bug that `semgrep --validate` now catches and rejects. The filter was
removed; the `$VAR` name-regex is sufficient for precision.

---

## Coverage note

The deterministic floor already covers Rust SQL-string-concat and secrets via
token-entropy and literal-detection passes. The semgrep layer adds structural
signal as a second layer; the floor does not depend on it. Removing a broken
semgrep rule therefore does not create a coverage gap, but fixing rules so they
fire correctly does remove the false-confidence "0 findings" result.

---

## Validation

```
semgrep --validate --config crates/server/assets/semgrep-rules/security.yml
# Configuration is valid - found 0 configuration error(s), and 17 rule(s).
```
