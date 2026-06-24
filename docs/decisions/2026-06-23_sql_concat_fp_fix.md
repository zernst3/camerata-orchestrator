# SEC-NO-RAW-SQL-CONCAT-1: identifier-embedded DML words — false-positive fix

Date: 2026-06-23
Status: Accepted; implemented in `crates/gateway/src/lib.rs`.

## Context

`SEC-NO-RAW-SQL-CONCAT-1` is a deterministic floor rule that fires at both the
Layer-1 gate and the maintenance scan. It is rated `critical` and blocks writes.

Two false positives were blocking the author's own dogfooding session:

- `crates/ui/src/cockpit/rules.rs:719`:
  `label { class: "repo-select-label", "Filter by repo:" }`
- `crates/ui/src/cockpit.rs:1833`:
  `class: "app-update-dismiss",`

Neither line has anything to do with SQL.

## Root cause

The previous regex used a DOTALL cross-line window (`(?isx)` with `.{0,200}?`):

```
\b(?:SELECT|INSERT|UPDATE|DELETE)\b .{0,200}?
\b(?:FROM|INTO|SET|VALUES|JOIN|WHERE)\b .{0,200}?
(?:\{\w*\} | "\s*\+)
```

`\b` in the `regex` crate is based on `\w = [0-9A-Za-z_]`. The `-` in a
kebab-case identifier is NOT a word character, so it acts as a word boundary.
Concretely:

- `"repo-select-label"`: `\bSELECT\b` matched `select` between the two `-`
  boundaries inside the class string.
- `"app-update-dismiss"`: `\bUPDATE\b` matched `update` between `-app`
  and `-dismiss`.

Once the DML keyword matched, the DOTALL window scanned the next 200 characters
of source across line boundaries. In both cases that window contained:

1. A clause match: `.set(true)` or `repo_filter.set(e.value())` — the method
   call `.set(` has `\b` at `.`/`s` (non-word/word boundary) and at `t`/`(`
   (word/non-word), so `\bSET\b` fired on the method name.
2. An interpolation match: `{rf}` or `{00D7}` or similar rsx `{...}` nearby
   satisfied `\{\w*\}`.

All three parts of the old pattern could be satisfied by completely unrelated
source tokens spread across adjacent lines. This is the defining failure of a
cross-context window anchored on a raw identifier token.

## Decision: anchor the entire match inside a string literal

The fix requires that the DML keyword AND the confirming clause BOTH appear
inside the SAME double-quoted string literal. This is expressed by:

1. Starting the match at an opening `"`.
2. Using `(?:[^"\\]|\\[\s\S])*?` (escape-aware, non-greedy) for all intra-string
   spans. `[^"\\]` matches any char except a closing quote or backslash (including
   newlines, for multi-line Rust string literals). `\\[\s\S]` matches a backslash
   followed by any character, including `\<newline>` (Rust line-continuation
   sequences) and `\\n`/`\\t` escape codes.
3. The DML keyword and the confirming clause must each be found within `[^"\\]*`
   spans — no closing `"` can appear between the opening quote and either keyword.
4. The interpolation (`\{\w*\}`) or string-concat (`"\s*\+`) must follow the
   clause within the same string, or the string must close and be immediately
   concatenated.

Effect on the FP cases:

- `"repo-select-label"` opens with `"`, contains `select` (still word-bounded by
  `-`), but the string closes with `label"` — no `FROM`/`SET`/`WHERE` appears
  between the opening `"` and the closing `"`. No match.
- `"app-update-dismiss"` similarly: `update` matched but no SQL clause inside the
  same string literal. No match.

Effect on true positives (all preserved):

- `format!("SELECT * FROM users WHERE id = {}", user_id)` — DML + clause + `{}`
  all inside one string. Matches.
- `"SELECT name FROM accounts WHERE org = " + org_id` — DML + clause inside the
  string, then `" +`. Matches.
- `format!("UPDATE orders SET status = {status} WHERE id = {id}")` — DML + SET +
  `{status}`. Matches.
- `format!("INSERT INTO events (name) VALUES ('{}')", name)` — INSERT + INTO +
  `{}`. Matches.
- Multi-line Rust strings with `\\\n` continuation sequences: handled by
  `\\[\s\S]` in the escape alternation.
- Mixed-case (`sElEcT`), tab/newline padding: handled by `(?i)` and the
  intra-string wildcard spans. Matches.

## Known gaps introduced (acceptable)

- Raw string literals (`r#"..."#`) are not matched by the `"` anchor. A raw SQL
  string in a raw literal is a known false-negative. This gap is documented in
  the regex doc-comment and is judged acceptable: real SQL-by-concat almost
  always uses `format!()` with a normal string literal.
- The word-boundary caveat for `-` still means `\bselect\b` fires inside
  `"repo-select-label"`. The clause gate (same-string) is what suppresses the
  match, not the absence of a keyword match. This is correct: the fix is
  structural (same-string anchor), not lexical (smarter tokenization).

## Alternatives considered

**A. Lookbehind to exclude identifier context.** The `regex` crate has no
lookbehind. Would require the `fancy_regex` crate (backtracking engine) or a
two-pass approach. Rejected: higher complexity, new dependency, slower.

**B. Exclude hyphens explicitly from the keyword match.** Add a negative char
class before the DML alternation: `[^-\w](?:SELECT|...)`. But `regex` has no
lookbehind, so this would have to be a capture-group offset trick. The same
anchor-in-string approach is cleaner and catches more of the FP class.

**C. Require the opening quote before the DML keyword (prior approach with
separate clause gate).** This was the state before this fix. It required the
clause to appear anywhere in the DOTALL window — not inside the same string. The
remaining FP class (DML in identifier + clause as method call + unrelated `{}`)
was still exploitable, as demonstrated by the bug report.

## Tests added

`sql_concat_identifier_fp_regression` in `crates/gateway/src/lib.rs` covers:

- Both exact user-reported FP cases verbatim.
- Four additional identifier/rsx patterns that must stay silent.
- Five true-positive patterns that must still fire.

All existing `SEC-NO-RAW-SQL-CONCAT-1` tests remain green. Total gateway test
count: 94 (up from 93).
