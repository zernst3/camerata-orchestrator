# Floor Rules Expansion and Test-Scope Gate

**Date:** 2026-06-22
**Status:** Implemented
**Author:** Zachary Ernst

## Context

The layer-1 governance gate (`camerata-gateway`) shipped with three content rules
(SEC-NO-HARDCODED-SECRETS-1, SEC-NO-RAW-SQL-CONCAT-1, ARCH-NO-SECRETS-IN-URL-1) and
two path rules (GOV-1, SEC-NO-PATH-ESCAPE-1, SEC-NO-SECRET-FILES-1). The brownfield
audit (`camerata-server/onboard.rs`) ran the same arms over existing repo content.

Two gaps were identified:

1. **Missing floor rules.** Four additional deterministic security checks were
   clearly implementable and high-value: PEM private-key block detection, vendor token
   shape matching, secret-bearing file path denial, and TLS-verification-disabled
   detection. All four have low false-positive rates and clear remediation paths.

2. **No test-scope gate.** The brownfield audit implemented test-path and
   test-scope down-ranking in `onboard.rs`, but the write-time gate arms in
   `gateway/lib.rs` ignored the test/fixture context entirely. A `#[cfg(test)]` block
   containing `verify=False` would deny a legitimate test write. The test-scope
   primitives were also duplicated between the two crates.

## Decisions

### D1: Add four new floor rules to the gate

**SEC-NO-PRIVATE-KEY-1** — Deny file content containing a PEM private-key block header
(RSA, EC, DSA, OPENSSH, PGP, PKCS#8). Downgrade policy in test scope (still deny:
a real private key in a test file is still a real key).

**SEC-NO-VENDOR-TOKEN-1** — Deny high-precision vendor credential token shapes (AWS
AKIA/ASIA, GitHub gh*_, Slack xox*-, Stripe sk_live_, Google AIza*, Anthropic sk-ant-).
Near-zero FP rate. Downgrade policy in test scope.

**SEC-NO-SECRET-FILE-1** — Deny writing a file whose path marks it as secret-bearing
(.pem, .p12, .pfx, .key, .jks, .keystore, id_rsa/dsa/ecdsa/ed25519, real .env files).
Path-based, no content scan needed. Companion to SEC-NO-SECRET-FILES-1 for the
brownfield audit.

**SEC-NO-DISABLED-TLS-1** — Deny content that disables TLS/certificate verification
(verify=False, rejectUnauthorized:false, InsecureSkipVerify:true,
NODE_TLS_REJECT_UNAUTHORIZED=0, CURLOPT_SSL_VERIFYPEER false/0). Waive policy in test
scope: test infrastructure legitimately connects to local TLS proxies.

All four are added to `AUDIT_RULES` in `onboard.rs` and to `RULE_REGISTRY` in
`gateway/lib.rs`.

### D2: Introduce TestScopePolicy with two variants

Rather than a boolean `is_test`, each rule declares a `TestScopePolicy`:

- **Waive** — the rule does not apply in test scope. Used for SEC-NO-RAW-SQL-CONCAT-1
  (test migrations write raw SQL legitimately) and SEC-NO-DISABLED-TLS-1 (test
  infrastructure may connect to local proxies without proper certs).
- **Downgrade** — the rule fires in test scope but the brownfield audit down-ranks the
  finding to `low` severity and adds the test-path note. Used for all secret/credential
  rules: a real private key or vendor token in a test file is still real.

The `test_scope_policy(rule_id)` function is the single source of truth. Default is
Downgrade (conservative: unknown rules deny in test scope too).

### D3: Hoist test-scope primitives into gateway/lib.rs

`is_test_or_fixture_path`, `test_scope_line_ranges`, `is_in_test_scope`,
`TEST_PATH_NOTE`, and `TEST_PATH_SEVERITY` are moved from `onboard.rs` into
`gateway/lib.rs` as `pub` items. `onboard.rs` references them as
`camerata_gateway::*`. Rationale: the gate is the single source of truth for what is
and is not a security violation; the test-scope classification is part of that
determination at write time, not only at audit time.

### D4: Refit existing three content arm functions for test-scope awareness

`arm_sec_no_hardcoded_secrets_1`, `arm_sec_no_raw_sql_concat_1`, and
`arm_arch_no_secrets_in_url_1` are updated to:
1. Compute the match line number from `m.start()` (byte offset to line number).
2. Check `is_test_or_fixture_path(path) || is_in_test_scope(match_line, &ranges)`.
3. Dispatch on `test_scope_policy(rule_id)` — Waive returns `Ok(())`; Downgrade
   continues to `Err(...)`.

The `_path` parameter is renamed to `path` in all three (it was unused before).

## Alternatives Rejected

**Per-rule boolean `waive_in_test`** — less expressive than the enum; adding a third
state (e.g. `Annotate`) would require a migration. Enum is forward-compatible.

**Keep test-scope primitives in onboard.rs, gate calls into server** — creates an
awkward crate-dependency inversion (gateway would depend on server). Moving them to
gateway (the lower-level crate) is the correct direction.

**Apply Waive to all secret rules in test scope** — a real private key committed
in a test file is a real private key. Downgrade surfaces it at low severity so the
architect can verify, rather than silently allowing it.

## Files Changed

- `crates/gateway/src/lib.rs` — new enum, policy fn, 4 new rules + arms, hoisted
  primitives, refitted existing arms, new tests
- `crates/server/src/onboard.rs` — removed local primitives, updated AUDIT_RULES,
  updated callers to use `camerata_gateway::` prefix
- `crates/rules/principles/universal/sec-no-private-key-1.toml` — new
- `crates/rules/principles/universal/sec-no-vendor-token-1.toml` — new
- `crates/rules/principles/universal/sec-no-secret-file-1.toml` — new
- `crates/rules/principles/universal/sec-no-disabled-tls-1.toml` — new
