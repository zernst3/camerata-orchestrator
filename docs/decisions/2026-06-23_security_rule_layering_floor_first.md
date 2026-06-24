# Security rule layering: floor-first

**Date:** 2026-06-23
**Status:** Accepted
**Context:** Dogfooding surfaced that the bundled semgrep security rules overlap the
deterministic security floor. Question raised: is semgrep CE moot? Decision: no — but
the floor is the #1 enforcement layer, so anything that *can* live in the floor *should*.

## The layers, in priority order

1. **Security floor (Tier 1 — enforced).** Deterministic, language-agnostic regex content
   rules in `crates/gateway`. Gate-blocking: a floor violation stops the commit/PR. This is
   the #1 enforcement mechanism. Current coverage: hardcoded-secrets, vendor-token,
   private-key, secret-files, secret-in-URL, raw-SQL-concat, disabled-TLS, path-escape.
2. **Advisory / preview (Tier 2 — surfaced, not blocked).** clippy / ruff / eslint / semgrep.
   Deterministic but NOT gate-enforced; shown as preview findings.
3. **AI review (Tier 3).** Judgment-based, architectural.

## The rule: floor-first, subject to the floor's admission bar

Anything that *can* live in the floor *should* — the floor is enforced, deterministic, and
near-zero-FP, which is strictly stronger than an advisory preview. BUT a rule is admissible
to the enforced floor ONLY if it clears the floor's bar:

> **Floor admission bar:** near-zero false-positive AND always-block-worthy — i.e. the matched
> pattern has *no* legitimate use. (Hardcoded secrets, private keys, SQL built by string
> interpolation, disabled TLS verification all clear this: there is no good reason to do them.)

A security signal that is *context-dependent* — the pattern has legitimate uses — does NOT
clear the bar and must stay in Tier 2 (advisory), because enforcing it would block correct
code. This is correct layering, not a weakness.

## Applying the bar to the four floor-missing semgrep categories

| Category | Floor-admissible? | Why |
|---|---|---|
| **unsafe-deserialization** (`yaml.load` w/o SafeLoader, `pickle.loads`, `unserialize`) | **Yes** | Near-zero legitimate use on untrusted input; almost always a vuln. **Port to floor.** |
| **shell-injection** (`shell=True` / `os.system` with interpolation) | Borderline | `shell=True` alone has uses; only *interpolated* command strings are always-bad. Port a tightly-anchored arm (interpolation required), else stay Tier 2. |
| **weak-hash** (md5 / sha1) | **No** | md5/sha1 have legitimate non-crypto uses (cache keys, ETags, checksums). Enforcing would block correct code. **Stays Tier 2 (advisory).** |
| **exec-injection** (`eval` / `exec` / `Function()`) | **No** | eval/exec are heavily context-dependent. **Stays Tier 2.** |

## Consequences

- **Port unsafe-deserialization into the floor** (new `SEC-*` arm, anchored, near-zero-FP,
  tested). Then it's enforced, not just an advisory semgrep preview.
- **Semgrep stays a selectable option** and is NOT moot: its enduring value is breadth the
  floor cannot enforce — AST-structural rules regex can't express, framework-specific rules,
  the public registry, and the context-dependent signals (weak-hash, exec, shell) that
  belong in the advisory tier by design.
- **Caveat the overlap, don't delete it.** Where a semgrep security rule duplicates a floor
  category, label it advisory/redundant in the UI; the scan-time cross-tool dedup
  (`finding_security_category` + `dedup_scan_previews`, fixed 2026-06-23 `b7db61a`) collapses
  the double-report so the user sees one row, floor canonical.
- **Per-repo artifact noted:** on a pure-Rust repo only the floor-overlapping semgrep rules
  fire (the additive four are Python/JS-targeted), which is why semgrep *looked* fully
  redundant on Camerata. It isn't, for a Python/JS codebase.

## Implemented (2026-06-24) — SEC-NO-UNSAFE-DESERIALIZATION-1

The unsafe-deserialization port is complete. Gate-blocking match-set:

| Sink | Language | Notes |
|---|---|---|
| `yaml.load(` | Python | Blocked UNLESS the same line contains `SafeLoader` or `FullLoader` (carve-out) |
| `yaml.unsafe_load(` | Python | Always blocked; no safe variant |
| `pickle.load(` / `pickle.loads(` | Python | Always blocked |
| `cPickle.load(` / `cPickle.loads(` | Python | C-extension alias of pickle; always blocked |
| `_pickle.load(` | Python | CPython internal alias; always blocked |
| `unserialize(` | PHP | Always blocked; PHP manual explicitly warns against untrusted input |
| `Marshal.load(` / `Marshal.restore(` | Ruby | Always blocked |
| `BinaryFormatter` | .NET | Always blocked; deprecated in .NET 5+ for this reason |
| `NetDataContractSerializer` | .NET | Always blocked |
| `LosFormatter` | .NET | Always blocked |
| `ObjectStateFormatter` | .NET | Always blocked |

**SafeLoader / FullLoader carve-out:** `yaml.load(data, Loader=yaml.SafeLoader)` and
`yaml.load(data, Loader=yaml.FullLoader)` are safe. The Rust regex crate has no lookahead,
so the carve-out is applied post-match in both the write-time arm and the `content_match_lines`
(brownfield audit) path: each match on `yaml.load(` is checked against the text of its line.

**Java gap:** Java's `ObjectInputStream.readObject()` is excluded. Without AST taint-tracking
the FP rate would block every framework using Java serialisation internally (Spring, Hibernate,
RMI). Java coverage is deferred to the semgrep tier.

**Test-scope policy:** Waive. Test fixtures legitimately deserialise controlled payloads.

**Dedup category:** `"deser"`. The semgrep rule `camerata.security.yaml-unsafe-load` was
previously in the `"yaml"` category (no floor twin). It is now remapped to `"deser"` so that a
floor finding and a semgrep finding on the same `(repo, path, line)` collapse to one row, floor
canonical. The `"yaml"` category string is retired.

**Files changed:**
- `crates/gateway/src/lib.rs` — rule id constructor, test-scope policy (Waive), RULE_REGISTRY
  entry, `content_match_lines` dispatch with SafeLoader carve-out, `sec_unsafe_deser_regex()`,
  `arm_sec_no_unsafe_deserialization_1`, 19 gateway tests.
- `crates/rules/principles/universal/sec-no-unsafe-deserialization-1.toml` — corpus TOML.
- `crates/server/src/onboard.rs` — AUDIT_RULES.
- `crates/server/src/lib.rs` — `finding_security_category` (new "deser" arm + yaml-unsafe-load
  remap), dedup regression test, rule-count integration test.
- `crates/server/src/eval.rs` — 5 positive eval fixtures + 3 clean controls.
