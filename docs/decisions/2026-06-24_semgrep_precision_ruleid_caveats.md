# 2026-06-24 Semgrep Precision, Rule-ID Normalization, and Advisory Caveats

## Context

Three related problems surfaced during the rivet scan on the real
semgrep binary (v1.167.0):

1. **False positives** — `sql-string-concat-rust` fired on non-SQL
   format strings: an AI system prompt (`chat.rs`), a log line
   ("reading build output from"), and a URL route pattern
   ("/api/uow/{}/update-branch").

2. **Path-prefixed rule IDs** — when semgrep is invoked with an absolute
   `--config` path, it dotted-prefixes every rule id with the config
   path, e.g.:
   ```
   Users.zacharyernst.Documents.Repos.camerata-orchestrator.crates.server.assets.semgrep-rules.camerata.security.sql-string-concat-rust
   ```
   This broke the UI display, portable baselines, AND
   `finding_security_category` dedup (which matches the clean
   `camerata.security.<name>` form).

3. **Missing overlapping-rule entries** — `semgrep_floor_category` only
   listed 3 of the 9 rules that overlap the floor, so the deduper
   silently skipped finding-collapse for the other 6.

## Decisions

### Step 1: Tighten `sql-string-concat-rust` pattern-regex

**Decision:** Re-anchor the `pattern-regex` to require all three
conditions inside ONE double-quoted string literal:

1. A DML verb (`SELECT`/`INSERT`/`UPDATE`/`DELETE`) at or near the
   start of the string (optional leading whitespace/newline only).
2. A confirming SQL clause (`FROM`/`INTO`/`SET`/`VALUES`/`JOIN`/`WHERE`)
   in the SAME string.
3. A format placeholder (`{}` or `{name}`) OR closing quote followed by
   `+` (string concatenation).

The "DML at string start" gate eliminates mid-sentence prose patterns
like "select the best option from the list: {}" while keeping all
real SQL injection cases (DML queries always start at the beginning of
their string literal in practice).

Parameterized queries using `?`/`$1` placeholders (no `{}`/`+`) remain
outside scope of this rule by design — they're safe.

The C# `sql-string-concat-csharp` rule uses structural patterns
(requiring `SqlCommand`/`SqliteCommand`/`$CMD.CommandText` context)
and was already precise; no change needed.

**Verification (before/after):**

| Fixture                                                    | Before | After |
|------------------------------------------------------------|--------|-------|
| `format!("SELECT * FROM users WHERE id = {}", id)`         | fires  | fires |
| `format!("INSERT INTO logs VALUES ({})", id)`              | fires  | fires |
| `format!("UPDATE accounts SET balance = {} WHERE id = {}", ...)` | fires | fires |
| `format!("DELETE FROM sessions WHERE token = {}", id)`     | fires  | fires |
| `"SELECT * FROM users WHERE id = " + &id.to_string()`      | fires  | fires |
| `format!("reading build output from '{}'", dir)`           | fires  | **no** |
| `format!("You answer questions about…select…from…: {}", …)` | fires  | **no** |
| `"/api/uow/{}/update-branch"`                              | fires  | **no** |
| `"UPDATE agent_os_fs SET x = ? WHERE y = ?"`               | fires  | **no** |

### Step 2: Normalize semgrep rule IDs on parse

**Decision:** Add `normalize_semgrep_rule_id(raw: &str) -> String` in
`crates/server/src/scan_tools.rs`. If the raw id contains
`camerata.security.`, return the substring from that sentinel onward;
otherwise return unchanged.

Wire into `parse_sarif` for `ScanTool::Semgrep` only. All downstream
consumers (UI, baselines, `finding_security_category`) then always see
the clean `camerata.security.<name>` form.

Also expand `semgrep_floor_category` to include the 6 previously-missing
entries: `hardcoded-secret-dquote`, `sql-string-concat-rust`,
`sql-string-concat-csharp`, `disabled-tls-rust`, `disabled-tls-csharp`,
`yaml-unsafe-load`. These were present in `finding_security_category`
but not in `semgrep_floor_category`, so the deduper never collapsed them
against their floor finding.

**normalize_semgrep_rule_id examples:**

| Input                                              | Output                                      |
|----------------------------------------------------|---------------------------------------------|
| `Users.alice.camerata…camerata.security.sql-string-concat-rust` | `camerata.security.sql-string-concat-rust` |
| `camerata.security.hardcoded-secret`               | `camerata.security.hardcoded-secret` (unchanged) |
| `python.lang.security.audit.exec-detected`         | `python.lang.security.audit.exec-detected` (unchanged) |

### Step 3: Advisory caveats on floor-overlapping rules

**Decision:** Append an advisory note to the `message:` of each
semgrep rule whose category the floor ALSO enforces. This surfaces the
overlap to UI consumers without removing the rule (which would punch a
hole in CI coverage).

Rules annotated (9 total):

| Rule                             | Floor rule                     |
|----------------------------------|--------------------------------|
| `hardcoded-secret`               | `SEC-NO-HARDCODED-SECRETS-1`   |
| `hardcoded-secret-dquote`        | `SEC-NO-HARDCODED-SECRETS-1`   |
| `sql-string-concat-python`       | `SEC-NO-RAW-SQL-CONCAT-1`      |
| `sql-string-concat-js`           | `SEC-NO-RAW-SQL-CONCAT-1`      |
| `sql-string-concat-rust`         | `SEC-NO-RAW-SQL-CONCAT-1`      |
| `sql-string-concat-csharp`       | `SEC-NO-RAW-SQL-CONCAT-1`      |
| `disabled-tls-rust`              | `SEC-NO-DISABLED-TLS-1`        |
| `disabled-tls-csharp`            | `SEC-NO-DISABLED-TLS-1`        |
| `yaml-unsafe-load`               | `SEC-NO-UNSAFE-DESERIALIZATION-1` |

Rules NOT annotated (no floor equivalent, kept clean):
- `weak-hash-python`, `weak-hash-js`, `weak-hash-rust`, `weak-hash-csharp`
- `exec-injection`, `exec-injection-js`
- `subprocess-shell-true`
- `path-traversal-python`

## Alternatives considered

- **Remove overlapping semgrep rules entirely** — rejected. The floor
  enforces at Layer 1 (gate); semgrep enforces at Layers 2-3 (CI).
  Trimming semgrep would punch a CI coverage hole.
- **Config-path-agnostic `--config`** — using `--config .` instead of
  an absolute path avoids the prefix. Rejected as a primary fix because
  Camerata must scan external repos that don't live in the CWD.

## Commits

- `016a1a9` — fix(semgrep): tighten sql-string-concat-rust to eliminate standalone FPs
- `cce3dff` — fix(scan): normalize semgrep rule ids to strip absolute-path prefix (#79)
- `9b92071` — docs(semgrep): add advisory-duplicate caveats to floor-overlapping rules
- (step 4 commit) — fix(tests): update stale semgrep_floor_category test assertions + decision doc
