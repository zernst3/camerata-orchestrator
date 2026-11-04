//! Scan result merging and audit jobs: floor categories, preview dedup, the external
//! tool ledger reconcile, resume, and the audit job request types.

use crate::*;

/// Map an external-tool (semgrep) rule id to the CORPUS rule id it is evidence for, or
/// `None` when the tool rule has no corpus-grounded twin (net-new coverage that must
/// pass through untouched, or a rule we haven't grounded yet).
///
/// Originally scoped to just the FLOOR (`crate::onboard::AUDIT_RULES`) overlap case
/// (hence the name): the floor runs at the Layer-1 gate AND at scan preview, semgrep
/// runs at scan preview AND CI (Layer-2 / Layer-3), and several semgrep rules duplicate
/// a floor rule on the same (repo, path, line) axis. W3 (the commodity-class taint
/// layer) generalizes this table's ACTUAL use beyond that original scope: it is also
/// the GROUNDING map `crate::report_export::corpus_rule_for` joins against so a preview
/// finding's citation, severity calibration, authored remediation, and effort estimate
/// come from OUR corpus rule rather than a generic "found by this tool" label — and it
/// is the signal `mechanical_gate::semgrep_covered_rule_ids` uses to recognize a
/// `mechanical` corpus rule as genuinely detector-backed (channel 3). Every mapped
/// corpus rule id below therefore means "a semgrep rule answers for this corpus rule,"
/// which is a superset of "...and ALSO has a floor twin" — the table comment marks
/// which rows are floor overlaps (dedup-relevant) vs. grounding-only (no floor twin, no
/// dedup collapse, just citation/remediation attachment).
///
/// | Semgrep rule id                                       | Corpus rule id                  | Floor twin? |
/// |--------------------------------------------------------|----------------------------------|-------------|
/// | `camerata.security.hardcoded-secret`                   | `SEC-NO-HARDCODED-SECRETS-1`     | yes |
/// | `camerata.security.hardcoded-secret-dquote`            | `SEC-NO-HARDCODED-SECRETS-1`     | yes |
/// | `camerata.security.sql-string-concat-python`           | `SEC-NO-RAW-SQL-CONCAT-1`        | yes |
/// | `camerata.security.sql-string-concat-js`               | `SEC-NO-RAW-SQL-CONCAT-1`        | yes |
/// | `camerata.security.sql-string-concat-rust`             | `SEC-NO-RAW-SQL-CONCAT-1`        | yes |
/// | `camerata.security.sql-string-concat-csharp`           | `SEC-NO-RAW-SQL-CONCAT-1`        | yes |
/// | `camerata.security.taint-sql-injection-{python,js,rust,csharp,go,ruby,java}` | `SEC-NO-RAW-SQL-CONCAT-1` | yes |
/// | `camerata.security.disabled-tls-rust`                  | `SEC-NO-DISABLED-TLS-1`          | yes |
/// | `camerata.security.disabled-tls-csharp`                | `SEC-NO-DISABLED-TLS-1`          | yes |
/// | `camerata.security.yaml-unsafe-load`                   | `SEC-NO-UNSAFE-DESERIALIZATION-1`| yes |
/// | `camerata.security.taint-xss-{python,js,csharp,go,ruby,java}` | `SEC-NO-UNSAFE-HTML-SINK-1` | no (grounding-only) |
/// | `camerata.security.taint-open-redirect-{python,js,csharp,go,ruby,java}` | `SEC-NO-OPEN-REDIRECT-1` | no (grounding-only) |
/// | `camerata.security.exec-injection` / `-js`             | `SEC-NO-COMMAND-INJECTION-1`     | no (grounding-only) |
/// | `camerata.security.taint-command-injection-{python,js,rust,csharp,go,ruby,java}` | `SEC-NO-COMMAND-INJECTION-1` | no (grounding-only) |
/// | `camerata.security.taint-ruby-eval-send`               | `RUBY-AVOID-EVAL-SEND-1`         | no (grounding-only; closes a pre-existing phantom detector) |
/// | `camerata.security.taint-log-secret-{python,js,rust,csharp,go,ruby,java}` | `SEC-NO-SENSITIVE-DATA-IN-LOGS-1` | no (grounding-only; `severity: ERROR` -> "high") |
/// | `camerata.security.taint-log-pii-{python,js,rust,csharp,go,ruby,java}` | `SEC-NO-SENSITIVE-DATA-IN-LOGS-1` | no (grounding-only; `severity: WARNING` -> "medium") |
///
/// The remaining semgrep rules (`weak-hash-*`, `path-traversal-python`,
/// `subprocess-shell-true`) have no corpus-grounded twin yet and map to `None`.
///
/// NOTE: rule ids here must be the NORMALIZED (clean) form — `normalize_semgrep_rule_id`
/// in `scan_tools` strips any absolute-path prefix before findings reach this function.
///
/// IMPORTANT: do NOT remove rules from either ruleset to fix overlap. The floor enforces
/// at Layer 1 (gate); semgrep enforces at Layers 2-3 (CI). Trimming semgrep would punch
/// a hole in CI coverage. The fix is presentation-time dedup at scan preview only (see
/// `dedup_scan_previews`). Decision: docs/decisions/2026-06-22_scan_floor_semgrep_dedup.md
/// and docs/decisions/2026-10-06_w3_commodity_taint_layer.md
pub(crate) fn semgrep_floor_category(semgrep_rule_id: &str) -> Option<&'static str> {
    match semgrep_rule_id {
        "camerata.security.hardcoded-secret" | "camerata.security.hardcoded-secret-dquote" => {
            Some("SEC-NO-HARDCODED-SECRETS-1")
        }
        "camerata.security.sql-string-concat-python"
        | "camerata.security.sql-string-concat-js"
        | "camerata.security.sql-string-concat-rust"
        | "camerata.security.sql-string-concat-csharp"
        | "camerata.security.taint-sql-injection-python"
        | "camerata.security.taint-sql-injection-js"
        | "camerata.security.taint-sql-injection-rust"
        | "camerata.security.taint-sql-injection-csharp"
        | "camerata.security.taint-sql-injection-go"
        | "camerata.security.taint-sql-injection-ruby"
        | "camerata.security.taint-sql-injection-java" => Some("SEC-NO-RAW-SQL-CONCAT-1"),
        "camerata.security.disabled-tls-rust" | "camerata.security.disabled-tls-csharp" => {
            Some("SEC-NO-DISABLED-TLS-1")
        }
        "camerata.security.yaml-unsafe-load" => Some("SEC-NO-UNSAFE-DESERIALIZATION-1"),
        "camerata.security.taint-xss-python"
        | "camerata.security.taint-xss-js"
        | "camerata.security.taint-xss-csharp"
        | "camerata.security.taint-xss-go"
        | "camerata.security.taint-xss-ruby"
        | "camerata.security.taint-xss-java" => Some("SEC-NO-UNSAFE-HTML-SINK-1"),
        "camerata.security.taint-open-redirect-python"
        | "camerata.security.taint-open-redirect-js"
        | "camerata.security.taint-open-redirect-csharp"
        | "camerata.security.taint-open-redirect-go"
        | "camerata.security.taint-open-redirect-ruby"
        | "camerata.security.taint-open-redirect-java" => Some("SEC-NO-OPEN-REDIRECT-1"),
        "camerata.security.exec-injection"
        | "camerata.security.exec-injection-js"
        | "camerata.security.taint-command-injection-python"
        | "camerata.security.taint-command-injection-js"
        | "camerata.security.taint-command-injection-rust"
        | "camerata.security.taint-command-injection-csharp"
        | "camerata.security.taint-command-injection-go"
        | "camerata.security.taint-command-injection-ruby"
        | "camerata.security.taint-command-injection-java" => Some("SEC-NO-COMMAND-INJECTION-1"),
        // Grounded to the Ruby-specific corpus rule (not SEC-NO-COMMAND-INJECTION-1): this
        // closes RUBY-AVOID-EVAL-SEND-1's pre-existing phantom-detector gap (it declared a
        // Brakeman integration Camerata never ran) with a real taint-mode detector for the
        // exact defect the corpus TOML already describes.
        "camerata.security.taint-ruby-eval-send" => Some("RUBY-AVOID-EVAL-SEND-1"),
        // Sensitive-data-in-logs taint class (new): both the secret/credential (ERROR ->
        // "high") and PII (WARNING -> "medium") rule families ground to the SAME corpus rule
        // — SEC-NO-SENSITIVE-DATA-IN-LOGS-1's own `[decision].why` is what documents the two
        // severities, not two separate corpus rules. See
        // `crates/server/assets/semgrep-rules/taint-security.yml`'s "Sensitive data reaching
        // a log sink" section for the full rationale. CWE-532.
        "camerata.security.taint-log-secret-python"
        | "camerata.security.taint-log-secret-js"
        | "camerata.security.taint-log-secret-rust"
        | "camerata.security.taint-log-secret-csharp"
        | "camerata.security.taint-log-secret-go"
        | "camerata.security.taint-log-secret-ruby"
        | "camerata.security.taint-log-secret-java"
        | "camerata.security.taint-log-pii-python"
        | "camerata.security.taint-log-pii-js"
        | "camerata.security.taint-log-pii-rust"
        | "camerata.security.taint-log-pii-csharp"
        | "camerata.security.taint-log-pii-go"
        | "camerata.security.taint-log-pii-ruby"
        | "camerata.security.taint-log-pii-java" => Some("SEC-NO-SENSITIVE-DATA-IN-LOGS-1"),
        _ => None,
    }
}

/// Map an external-tool (ESLint) rule id to the CORPUS rule id it is evidence for —
/// the ESLint-backed counterpart to [`semgrep_floor_category`] above, doing the SAME two
/// jobs for the ESLint-routed subset of the corpus: (1) the grounding map
/// `crate::report_export::corpus_rule_for` joins against so a preview finding's citation,
/// severity calibration, authored remediation, and effort estimate come from OUR corpus
/// rule rather than a generic "found by this tool" label, and (2) a signal
/// [`finding_grounds_to`] uses to recognize that a raw ESLint SARIF `ruleId` (e.g.
/// `react-hooks/exhaustive-deps`) is evidence a specific corpus rule actually fired —
/// needed for [`reconcile_external_tool_ledger`]'s emitted-count correction, exactly as
/// `semgrep_floor_category` is needed there for Semgrep.
///
/// Every id on the left is ALSO present in some corpus rule's own
/// `[[sources]].linter` annotation (`crates/rules/principles/javascript/**`) — this
/// table exists only to join a raw tool-native rule id back to the corpus id that
/// authored it, mirroring `semgrep_floor_category`'s hand-maintained style rather than
/// deriving it from a scan (ESLint rule ids, unlike Semgrep's, are stable well-known
/// plugin-namespaced strings; there is no bundled ruleset to derive the reverse map
/// from the way `mechanical_gate::semgrep_covered_rule_ids` does).
///
/// `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` and `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` are the
/// motivating fix: both declare `enforcement = "mechanical"` with a `react-hooks:`
/// linter source, but before `scan_tools::tool_for_linter` recognized that token at all,
/// neither corpus id resolved to ANY detector channel (see `mechanical_gate::
/// detector_channel`) — they relied entirely on the semantic/AI pass, which is exactly
/// the class of gap a benchmark scoring near-zero on planted mechanical defects exposed.
///
/// One ambiguity: `jest/no-conditional-expect` is cited by TWO corpus rules
/// (`JAVASCRIPT-TESTING-AAA-STRUCTURE-1` and `JAVASCRIPT-TESTING-DETERMINISTIC-1`). This
/// table grounds that one raw id to `JAVASCRIPT-TESTING-DETERMINISTIC-1` only — its
/// authored rationale is specifically about a conditional assert undermining a test's
/// deterministic pass/fail behavior, the tighter semantic fit — rather than crediting
/// both (which would double-count a single tool finding across two ledger rows).
/// `JAVASCRIPT-TESTING-AAA-STRUCTURE-1` is NOT "no detector" as a result: `tool_for_linter`
/// still recognizes its `jest: no-conditional-expect` source and routes it to Eslint for
/// the scan-preview pass and the `detector_channel`/ledger "has a detector" check; this
/// table just isn't the one that credits ITS corpus id when the raw finding arrives.
pub(crate) fn eslint_floor_category(eslint_rule_id: &str) -> Option<&'static str> {
    match eslint_rule_id {
        "react-hooks/exhaustive-deps" => Some("JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1"),
        "react-hooks/rules-of-hooks" => Some("JAVASCRIPT-REACT-RULES-OF-HOOKS-1"),
        "eqeqeq" => Some("JAVASCRIPT-STRICT-EQUALITY-1"),
        "no-var" => Some("JAVASCRIPT-NO-VAR-1"),
        "prefer-const" => Some("JAVASCRIPT-CONST-DEFAULT-1"),
        "no-restricted-imports" => Some("JAVASCRIPT-NEXT-DUAL-API-1"),
        "@typescript-eslint/no-explicit-any" => Some("JAVASCRIPT-TYPESCRIPT-NO-EXPLICIT-ANY-1"),
        "@typescript-eslint/no-floating-promises" => {
            Some("JAVASCRIPT-TYPESCRIPT-NO-FLOATING-PROMISES-1")
        }
        "@typescript-eslint/no-non-null-assertion" => {
            Some("JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1")
        }
        "jest/no-conditional-expect" | "jest/expect-expect" | "jest/no-done-callback" => {
            Some("JAVASCRIPT-TESTING-DETERMINISTIC-1")
        }
        "jest/prefer-spy-on" => Some("JAVASCRIPT-TESTING-MOCK-AT-BOUNDARIES-1"),
        "jest/no-identical-title" | "jest/valid-title" => Some("JAVASCRIPT-TESTING-NAMING-1"),
        "jest/no-disabled-tests" | "jest/no-focused-tests" => {
            Some("JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1")
        }
        _ => None,
    }
}

/// Dispatch a tool-native rule id to WHICHEVER external-tool grounding table (Semgrep's
/// or ESLint's) claims it, or `None` when neither does. The single place every grounding
/// consumer ([`finding_grounds_to`], `report_export::corpus_rule_for`, and the
/// newly-discovered-row pass in [`reconcile_external_tool_ledger`]) should call instead
/// of hand-chaining `semgrep_floor_category`/`eslint_floor_category` themselves, so a
/// THIRD external tool's grounding table (were one added later) only needs wiring here
/// once rather than at every call site.
pub(crate) fn external_tool_floor_category(tool_rule_id: &str) -> Option<&'static str> {
    semgrep_floor_category(tool_rule_id).or_else(|| eslint_floor_category(tool_rule_id))
}

/// Rule-id prefix for the OBSERVABILITY/under-logging convention class (a swallowed error
/// logged nowhere, inconsistent unstructured logging alongside an adopted structured
/// logger, missing correlation/request id propagation — the three corpus rules under
/// `crates/rules/principles/universal/arch-observability-*.toml`). Every rule in this
/// class shares this prefix BY CONVENTION — deliberately a prefix check rather than a
/// hand-maintained id list (the shape `semgrep_floor_category`/`eslint_floor_category`
/// use for EXTERNAL tool ids, which we do not control and so must enumerate): these are
/// OUR OWN authored corpus ids, so a shared naming convention we commit to at authoring
/// time is sufficient, scales to a future fourth rule in the same class with zero code
/// changes here, and is exercised directly by `observability_convention_class_matches_
/// only_its_own_prefix` in this module's test suite below.
pub(crate) const OBSERVABILITY_CONVENTION_RULE_PREFIX: &str = "ARCH-OBSERVABILITY-";

/// Whether `rule_id` belongs to the owner-mandated, SEVERITY-CAPPED-AT-LOW observability
/// convention class — the single source of truth both halves of that structural cap read
/// from:
///   - `ai_audit`'s D8 severity-ceiling pass forces the EXPORTED SEVERITY STRING to exactly
///     `"low"` for a matching finding, regardless of what any other calibration pass
///     proposed.
///   - `report_export::is_informational` checks this FIRST (ahead of its own hard
///     "critical/high is never informational" invariant) so a matching finding routes to
///     the `informational` appendix — never an action bucket (do_now/do_next/plan) —
///     EVEN IF some caller fed a raw high/critical severity straight in without the D8
///     ceiling pass having run first. Both halves exist because capping the severity STRING
///     alone is not sufficient: `report_export::matrix_bucket` routes a bare "low" severity
///     to `"plan"` (an action bucket) by default, same as "medium" — only an explicit
///     bucket-routing override (the `is_informational` check) actually keeps this class out
///     of every action tier.
///
/// See `docs/RULE_AUTHORING.md`'s channel guidance: this is NOT a mechanical gate/check
/// (channel B) — there is no detector that blocks a write. It is a calibration-pipeline
/// classification that decides how a SEMANTIC finding's severity and bucket get rendered
/// once the model (or the deterministic floor, in principle) has already produced one.
pub(crate) fn is_observability_convention_class(rule_id: &str) -> bool {
    rule_id.starts_with(OBSERVABILITY_CONVENTION_RULE_PREFIX)
}

/// W24 — rules about TEST COVERAGE, identified by rule id rather than by prose.
///
/// A missing or weak test is real work and belongs in the report. It is not a CRITICAL,
/// which in this report means an unauthenticated exposure or a full compromise: no test was
/// ever the thing an attacker exploited. Shipping one at Critical alongside "anyone can read
/// every row" spends the client's alarm on the wrong row and devalues the findings next to
/// it, which is the whole reason severity anchors exist.
///
/// Matched on the rule ID, not on the finding's text. The corpus has 56 rules with `TESTING`
/// in the id across every language pack, and the id is authored while the text is generated
/// — so this cannot drift with a reworded detail, and it cannot misfire on a security finding
/// that merely MENTIONS tests. That distinction is not theoretical here: the D5 severity
/// floor matched substrings and raised anything containing "urls" because it also contains
/// "rls".
pub(crate) fn is_test_coverage_class(rule_id: &str) -> bool {
    // Segment-anchored, so `TESTING-` matches as a word and a hypothetical
    // `SEC-NO-ATTESTING-TOKEN-1` does not.
    rule_id
        .split('-')
        .any(|seg| seg.eq_ignore_ascii_case("TESTING"))
}

/// Map ANY finding's rule id (floor OR any preview tool) to a normalized security
/// category string, or `None` when no category mapping exists. Used by
/// `dedup_scan_previews` for cross-tool dedup: two findings on the same
/// (repo, path, line) with the SAME normalized category are duplicates; different
/// categories on the same line are KEPT (conservative / surface-over-hide).
///
/// Category vocabulary (lowercase, hyphen-separated):
/// - `"secret"` — hardcoded secrets, vendor tokens, private keys
/// - `"sql"`    — raw SQL string concatenation (injection)
/// - `"exec"`   — OS command / exec injection
/// - `"hash"`   — weak cryptographic hash
/// - `"path"`   — path traversal
/// - `"shell"`  — subprocess with shell=True
/// - `"deser"`  — unsafe deserialization (yaml.load w/o SafeLoader, pickle, unserialize,
///   Marshal.load, BinaryFormatter, …). Replaces the old `"yaml"` category so
///   the floor rule SEC-NO-UNSAFE-DESERIALIZATION-1 and the semgrep rule
///   camerata.security.yaml-unsafe-load share one category and collapse
///   correctly when they fire on the same (repo, path, line).
/// - `"tls"`    — disabled TLS / cert verification
/// - `"xss"`    — cross-site scripting via a raw-HTML sink (W3)
/// - `"redirect"` — open redirect via an unvalidated redirect target (W3)
/// - `"log-sensitive"` — a secret/credential or regulated-PII value reaching a logging sink
///
/// Decision: docs/decisions/2026-06-23_stack_gating_and_crosstool_dedup.md and
/// docs/decisions/2026-10-06_w3_commodity_taint_layer.md
pub(crate) fn finding_security_category(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        // Floor rules (SEC-* ids from AUDIT_RULES)
        "SEC-NO-HARDCODED-SECRETS-1" | "SEC-NO-PRIVATE-KEY-1" | "SEC-NO-VENDOR-TOKEN-1" => {
            Some("secret")
        }
        "SEC-NO-RAW-SQL-CONCAT-1" => Some("sql"),
        "SEC-NO-DISABLED-TLS-1" => Some("tls"),
        // SEC-NO-UNSAFE-DESERIALIZATION-1: the floor rule for the deser category.
        // camerata.security.yaml-unsafe-load (semgrep) maps to the same category so that
        // a floor finding and a semgrep finding on the same (repo, path, line) collapse to
        // one row, floor canonical. The old "yaml" category string is retired; both sides
        // now use "deser" (broader: covers pickle, unserialize, Marshal.load, BinaryFormatter).
        "SEC-NO-UNSAFE-DESERIALIZATION-1" => Some("deser"),
        // W3: corpus rules with no floor twin — categorized so two EXTERNAL-TOOL findings
        // (e.g. a taint rule and a future second tool) can still collapse on the same line;
        // never collapses against a floor row since no floor row carries these ids.
        "SEC-NO-UNSAFE-HTML-SINK-1" => Some("xss"),
        "SEC-NO-OPEN-REDIRECT-1" => Some("redirect"),
        "SEC-NO-COMMAND-INJECTION-1" => Some("exec"),
        "RUBY-AVOID-EVAL-SEND-1" => Some("exec"),
        // Semgrep rules (camerata.security.*)
        // NOTE: keep these in sync with the rule ids in
        // crates/server/assets/semgrep-rules/security.yml and taint-security.yml. A semgrep
        // rule whose id is missing here returns None, so the deduper cannot categorize it and
        // it will NEVER collapse against an overlapping floor finding (the 2026-06-23 dedup
        // gap: the -rust/-csharp/-dquote ids were added to the corpus but not registered here).
        "camerata.security.hardcoded-secret" | "camerata.security.hardcoded-secret-dquote" => {
            Some("secret")
        }
        "camerata.security.sql-string-concat-python"
        | "camerata.security.sql-string-concat-js"
        | "camerata.security.sql-string-concat-rust"
        | "camerata.security.sql-string-concat-csharp"
        | "camerata.security.taint-sql-injection-python"
        | "camerata.security.taint-sql-injection-js"
        | "camerata.security.taint-sql-injection-rust"
        | "camerata.security.taint-sql-injection-csharp"
        | "camerata.security.taint-sql-injection-go"
        | "camerata.security.taint-sql-injection-ruby"
        | "camerata.security.taint-sql-injection-java" => Some("sql"),
        "camerata.security.exec-injection"
        | "camerata.security.exec-injection-js"
        | "camerata.security.taint-command-injection-python"
        | "camerata.security.taint-command-injection-js"
        | "camerata.security.taint-command-injection-rust"
        | "camerata.security.taint-command-injection-csharp"
        | "camerata.security.taint-command-injection-go"
        | "camerata.security.taint-command-injection-ruby"
        | "camerata.security.taint-command-injection-java"
        | "camerata.security.taint-ruby-eval-send" => Some("exec"),
        "camerata.security.weak-hash-python"
        | "camerata.security.weak-hash-js"
        | "camerata.security.weak-hash-rust"
        | "camerata.security.weak-hash-csharp" => Some("hash"),
        "camerata.security.disabled-tls-rust" | "camerata.security.disabled-tls-csharp" => {
            Some("tls")
        }
        "camerata.security.path-traversal-python" => Some("path"),
        "camerata.security.subprocess-shell-true" => Some("shell"),
        // yaml-unsafe-load maps to "deser" (not "yaml") so it collapses with the
        // floor rule SEC-NO-UNSAFE-DESERIALIZATION-1 when both fire on the same line.
        "camerata.security.yaml-unsafe-load" => Some("deser"),
        "camerata.security.taint-xss-python"
        | "camerata.security.taint-xss-js"
        | "camerata.security.taint-xss-csharp"
        | "camerata.security.taint-xss-go"
        | "camerata.security.taint-xss-ruby"
        | "camerata.security.taint-xss-java" => Some("xss"),
        "camerata.security.taint-open-redirect-python"
        | "camerata.security.taint-open-redirect-js"
        | "camerata.security.taint-open-redirect-csharp"
        | "camerata.security.taint-open-redirect-go"
        | "camerata.security.taint-open-redirect-ruby"
        | "camerata.security.taint-open-redirect-java" => Some("redirect"),
        // Sensitive-data-in-logs taint class: secret and PII sources share one category —
        // no floor twin either way, so this only matters for collapsing the two SEVERITY
        // tiers against each other if both ever fired on the identical (repo, path, line),
        // which keeps the stronger (secret/"high") row canonical per `finding_dedup_rank`.
        "camerata.security.taint-log-secret-python"
        | "camerata.security.taint-log-secret-js"
        | "camerata.security.taint-log-secret-rust"
        | "camerata.security.taint-log-secret-csharp"
        | "camerata.security.taint-log-secret-go"
        | "camerata.security.taint-log-secret-ruby"
        | "camerata.security.taint-log-secret-java"
        | "camerata.security.taint-log-pii-python"
        | "camerata.security.taint-log-pii-js"
        | "camerata.security.taint-log-pii-rust"
        | "camerata.security.taint-log-pii-csharp"
        | "camerata.security.taint-log-pii-go"
        | "camerata.security.taint-log-pii-ruby"
        | "camerata.security.taint-log-pii-java" => Some("log-sensitive"),
        // Ruff / ESLint lints that overlap with floor or semgrep in the same category.
        // S608 = possible SQL injection (Ruff/flake8-bandit) — same category as the floor SQL rule.
        "S608" => Some("sql"),
        _ => None,
    }
}

/// Precedence rank for a finding when choosing the canonical row among duplicates.
/// Lower = higher precedence (kept as canonical). Ordering:
///   0 = floor (deterministic, stable, gate-enforced SEC-* id)
///   1 = clippy / ruff / eslint (native language linter)
///   2 = semgrep (polyglot, more permissive category match)
pub(crate) fn finding_dedup_rank(f: &crate::onboard::Finding) -> u8 {
    if !f.preview {
        return 0; // floor finding (enforced, never preview)
    }
    match f.preview_tool.as_deref() {
        Some("clippy") | Some("ruff") | Some("eslint") => 1,
        Some("semgrep") => 2,
        _ => 1,
    }
}

/// Generalized scan-preview dedup: merge `previews` into `existing` (floor + any
/// already-accumulated preview findings), collapsing findings that share the same
/// `(repo, path, line)` AND a compatible `finding_security_category` into a single
/// canonical row.
///
/// ## Canonical preference order
///
/// floor > clippy/ruff/eslint (rank 1) > semgrep (rank 2)
///
/// When a new preview finding duplicates an EXISTING row, the existing row is
/// kept if its rank is lower (higher precedence), and the new finding's rule id is
/// appended to `also_matches` on the canonical. If the new finding has HIGHER
/// precedence than the existing preview row, the new finding BECOMES canonical (its
/// rule_id is kept and the old rule_id is swapped into `also_matches`).
///
/// ## Conservative dedup
///
/// Only deduplicated when same location AND same security category. Two findings on
/// the same line but different categories are BOTH kept ("surface over hide").
///
/// ## Backward compatibility
///
/// This is a strict superset of the old `dedup_preview_against_floor` contract.
/// The floor↔semgrep dedup behavior is preserved exactly as a special case.
///
/// Decision: docs/decisions/2026-06-23_stack_gating_and_crosstool_dedup.md
pub(crate) fn dedup_scan_previews(
    existing: &mut [crate::onboard::Finding],
    previews: Vec<crate::onboard::Finding>,
) -> Vec<crate::onboard::Finding> {
    dedup_scan_previews_with(existing, previews, None)
}

/// The tool rule selectors each corpus rule names in its `sources[].linter` entries, by native
/// rule id, normalised for comparison with a preview row's tool rule id (see
/// [`normalised_tool_rule_id`]). `linter = "@typescript-eslint: no-non-null-assertion"` on
/// `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1` yields
/// `@typescript-eslint/no-non-null-assertion`. Semgrep entries name a config pack, not a rule
/// id, so they yield nothing.
pub(crate) fn native_tool_selectors(
    corpus: &camerata_rules::RuleSet,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for rule in corpus.iter() {
        let selectors: Vec<String> = rule
            .sources
            .iter()
            .filter_map(|s| s.linter.as_deref())
            .filter_map(crate::scan_tools::selector_for_linter)
            .map(|sel| normalised_tool_rule_id(&sel))
            .collect();
        if !selectors.is_empty() {
            out.insert(rule.id.0.clone(), selectors);
        }
    }
    out
}

/// A tool rule id in the form used to compare a preview row's id with a corpus selector:
/// lower case, without a `clippy::` prefix.
pub(crate) fn normalised_tool_rule_id(id: &str) -> String {
    let lower = id.trim().to_ascii_lowercase();
    lower.strip_prefix("clippy::").unwrap_or(&lower).to_string()
}

/// Whether the preview row's tool rule id IS the tool rule the native rule `native_id` is
/// enforced by: the corpus's own `sources[].linter` mapping (`selectors`, from
/// [`native_tool_selectors`]) or the built-in tool-id map ([`external_tool_floor_category`]).
pub(crate) fn preview_is_native_rule_tool(
    preview_rule_id: &str,
    native_id: &str,
    selectors: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> bool {
    if external_tool_floor_category(preview_rule_id) == Some(native_id) {
        return true;
    }
    let wanted = normalised_tool_rule_id(preview_rule_id);
    selectors
        .and_then(|m| m.get(native_id))
        .is_some_and(|sels| sels.contains(&wanted))
}

/// The `Finding::captures` key listing the preview tools that also reported a row of the same
/// rule id at its place (comma separated, no duplicates). Provenance only: not a rule id and
/// never read as one.
pub(crate) const ALSO_SEEN_BY_CAPTURE: &str = "also_seen_by";

pub(crate) fn note_also_seen_by(f: &mut crate::onboard::Finding, tool: &str) {
    let entry = f
        .captures
        .entry(ALSO_SEEN_BY_CAPTURE.to_string())
        .or_default();
    if !entry.split(',').any(|t| t == tool) {
        if !entry.is_empty() {
            entry.push(',');
        }
        entry.push_str(tool);
    }
}

/// [`dedup_scan_previews`] with the corpus's tool mapping. A preview row collapses into an
/// existing row at the same `(repo, path, line)` when:
///
/// 1. both carry the SAME rule id (a deterministic row and the preview of the same rule);
/// 2. the preview's tool rule id is the tool rule the existing native rule is enforced by
///    (`selectors`, from the rule file's `sources[].linter`; for example the eslint rule
///    `@typescript-eslint/no-non-null-assertion` and
///    `JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1`); or
/// 3. both share a security category (as before).
///
/// Both ids stay on the merged row (the folded one in `also_matches`) and the merged row keeps
/// the higher of the two severities: a collapse never lowers a severity and never drops a
/// violation (the folded row's place is the canonical row's place).
pub(crate) fn dedup_scan_previews_with(
    existing: &mut [crate::onboard::Finding],
    previews: Vec<crate::onboard::Finding>,
    selectors: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Vec<crate::onboard::Finding> {
    let mut out: Vec<crate::onboard::Finding> = Vec::with_capacity(previews.len());
    let tools = crate::review_tree::ToolMap::from_selectors(selectors);

    for preview in previews {
        // An existing finding at the same location that is the same rule or the same rule
        // FAMILY: the native rule this tool rule enforces, or another tool's rule enforcing that
        // same native rule (`review_tree::family_of`). A row of another family at the same line
        // is NOT folded in: it stays its own row and the grouping pass links it.
        let preview_family = crate::review_tree::family_of(&preview, &tools);
        let same_place = |f: &crate::onboard::Finding| {
            f.repo == preview.repo && f.path == preview.path && f.line == preview.line
        };
        // The row with the IDENTICAL rule id wins over the first row that merely shares a
        // category or a tool mapping.
        let twin_idx = existing
            .iter()
            .position(|f| same_place(f) && f.rule_id == preview.rule_id)
            .or_else(|| {
                existing.iter().position(|f| {
                    same_place(f)
                        && (preview_is_native_rule_tool(&preview.rule_id, &f.rule_id, selectors)
                            || crate::review_tree::family_of(f, &tools) == preview_family)
                })
            });

        if let Some(idx) = twin_idx {
            let existing_rank = finding_dedup_rank(&existing[idx]);
            let preview_rank = finding_dedup_rank(&preview);
            // The merged row keeps the higher severity of the two.
            let severity = if crate::report_export::severity_rank(&preview.severity)
                < crate::report_export::severity_rank(&existing[idx].severity)
            {
                preview.severity.clone()
            } else {
                existing[idx].severity.clone()
            };

            if preview_rank >= existing_rank {
                // Existing row has equal or higher precedence: keep it, fold preview in.
                // Both ids stay on the row. For the same id there is no second rule id; which
                // tool also saw the row is recorded in `captures` (`also_seen_by`), never in
                // `also_matches`, which every rule-id consumer (scorecard, remediation plan)
                // reads as rule ids.
                if preview.rule_id != existing[idx].rule_id {
                    if !existing[idx].also_matches.contains(&preview.rule_id) {
                        existing[idx].also_matches.push(preview.rule_id.clone());
                    }
                } else if let Some(tool) = preview.preview_tool.as_deref() {
                    note_also_seen_by(&mut existing[idx], tool);
                }
                // The collapsed tool row keeps its own rule, tool, key and line (A1).
                existing[idx].flagged_by.push(flagged_by_of(&preview));
                existing[idx].severity = severity;
                // drop the preview finding (continue to next)
            } else {
                // Preview has higher precedence: promote it to canonical, demote existing.
                let old_rule_id = existing[idx].rule_id.clone();
                let mut new_canonical = preview;
                new_canonical
                    .also_matches
                    .append(&mut existing[idx].also_matches);
                if old_rule_id != new_canonical.rule_id
                    && !new_canonical.also_matches.contains(&old_rule_id)
                {
                    new_canonical.also_matches.push(old_rule_id);
                }
                // The row it replaces becomes a collapsed source, with what it had collected.
                new_canonical
                    .flagged_by
                    .append(&mut existing[idx].flagged_by);
                new_canonical.flagged_by.push(flagged_by_of(&existing[idx]));
                new_canonical.severity = severity;
                existing[idx] = new_canonical;
                // promoted to existing; don't push to out
            }
        } else {
            // No twin at this (repo, path, line): net-new coverage or a different defect —
            // keep it.
            out.push(preview);
        }
    }

    out
}

/// The record a row leaves on the row it is collapsed into: its own rule, tool, line and key.
pub(crate) fn flagged_by_of(f: &crate::onboard::Finding) -> crate::review_tree::FlaggedBy {
    crate::review_tree::FlaggedBy {
        rule_id: f.rule_id.clone(),
        tool: f.preview_tool.clone(),
        site_key: camerata_api_types::site_key::site_key(
            &f.repo, &f.path, &f.rule_id, &f.snippet, 0,
        ),
        path: f.path.clone(),
        line: f.line,
    }
}

/// Backward-compatible alias kept so the large existing test suite (`dedup_preview_against_floor`)
/// continues to pass without modification. Delegates directly to `dedup_scan_previews`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn dedup_preview_against_floor(
    existing: &mut [crate::onboard::Finding],
    previews: Vec<crate::onboard::Finding>,
) -> Vec<crate::onboard::Finding> {
    dedup_scan_previews(existing, previews)
}

/// Run the scan-time deterministic preview pass over each local repo source and append
/// its preview findings to the report. A no-op when there are no preview-runnable rules
/// (or the corpus is unavailable). Preview findings are ADVISORY-but-deterministic — they
/// carry stable tool rule-ids, stay OUT of the LLM review, and are labeled "not enforced
/// until wired" in the UI. layer3_only rules were already excluded by `split_scannable_rules`.
///
/// Stack-gating: for each repo the set of languages present (from its file extensions)
/// is derived and passed to `run_scan_tools` so tools whose required language is absent
/// are omitted entirely — they never appear as a passing "✓ 0".
/// Scan-time deterministic PREVIEW pass: run every CI-tier mechanical rule's own tool
/// (`crate::scan_tools::run_scan_tools`) that is locally previewable ([`split_scannable_rules`]'s
/// `preview_rules`) and fold its (deduped) findings into `report` as advisory preview
/// findings — never enforced, just surfaced early. A no-op when `preview_rules` is empty or
/// no corpus loaded.
///
/// `pub`: also called directly by `camerata inspect` (the headless CLI), so the SAME
/// preview-linter pass the interactive `onboard_audit` handler runs also runs headlessly —
/// see `crates/cli/src/inspect_cmd.rs`.
pub async fn merge_scan_preview(
    report: &mut crate::onboard::ScanReport,
    sources: &[(String, std::path::PathBuf)],
    preview_rules: &[crate::onboard::SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
    // The async job to report per-tool deterministic progress into (`(store, id)`), or `None`
    // on the synchronous path. When set, each preview tool registers + streams running → done
    // with its findings count, mirroring the floor's progress.
    job: Option<(&crate::jobs::JobStore, &str)>,
    // Whether a preview tool that executes the repository's code (`cargo clippy`) may run: the
    // engagement's client-tooling decision (see `crate::client_lock`).
    tooling: crate::client_lock::ClientTooling,
) {
    // Every tool NAME this call actually attempted to invoke, across every repo in `sources`
    // (regardless of whether each attempt then succeeded or failed) — fed to
    // `reconcile_external_tool_ledger` so it can tell "the tool ran and found nothing" apart
    // from "the tool was never invoked this scan at all" (empty `preview_rules`/no corpus, or
    // no selected rule this scan routed to it). See `scan_tools::run_scan_tools`'s doc comment.
    let mut tools_attempted: std::collections::HashMap<
        String,
        std::collections::HashSet<&'static str>,
    > = std::collections::HashMap::new();
    // Pass-1 W1 seal fix: this pass runs AFTER `onboard::audit_repos`'s own merge stages and
    // strictly APPENDS findings — the exact "row-adding pass that ran outside the ledger's
    // view" shape this fixes (see `crate::scan_ledger::StageAccounting::added`'s doc comment).
    // `rows_in` is snapshotted now, before the per-repo loop below mutates `report.findings`;
    // `rows_out`/the ledger stage are recorded at the very end of this function, alongside
    // `reconcile_external_tool_ledger` (the other end-of-pass reconciliation this function
    // already does).
    let rows_in = report.findings.len();
    if !preview_rules.is_empty() {
        if let Some(set) = corpus {
            let lookup = |id: &str| set.get_by_id(id);
            // The tool rule ids the corpus's native rules are enforced by, so a preview row of
            // such a tool rule collapses into the native rule's row on the same line.
            let selectors = native_tool_selectors(set);
            for (spec, dir) in sources {
                // Only the rules bound to this repo (or project-level) preview against it.
                let for_repo: Vec<crate::onboard::SelectedRule> = preview_rules
                    .iter()
                    .filter(|r| r.applies_to(spec))
                    .cloned()
                    .collect();
                if for_repo.is_empty() {
                    continue;
                }
                // Derive the language set present in this repo so stack-gating can omit
                // tools whose language is absent (e.g. no eslint on a Rust-only repo).
                let dir_clone = dir.clone();
                let (present_languages, repo_files) = tokio::task::spawn_blocking(move || {
                    crate::onboard::files::read_local_repo_files(&dir_clone)
                        .map(|ex| (crate::scan_tools::languages_from_files(&ex.files), ex.files))
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default();
                let (previews, mut notes, attempted) = crate::scan_tools::run_scan_tools(
                    spec,
                    dir,
                    &for_repo,
                    &lookup,
                    Some(&present_languages),
                    job,
                    tooling,
                    sources.len() > 1,
                )
                .await;
                tools_attempted
                    .entry(spec.clone())
                    .or_default()
                    .extend(attempted);
                // Dedup preview findings that overlap the deterministic floor or other preview
                // tools BEFORE appending. When two tools flag the same (repo, path, line) for a
                // compatible security category, the higher-precedence finding is kept canonical
                // and the other tool's id is folded into `also_matches`. See `dedup_scan_previews`.
                join_repo_previews(report, previews, Some(&selectors), &repo_files, Some(dir));
                report.coverage_notes.append(&mut notes);
            }
        }
    }
    // Pass-1 W1 seal fix: one ledger stage entry for every row this pass ADDED (net of its own
    // internal `dedup_scan_previews` collapsing), a no-op when nothing was added this run. See
    // the `rows_in` snapshot at the top of this function and
    // `crate::scan_ledger::record_addition_stage`'s doc comment.
    let rows_out = report.findings.len();
    crate::scan_ledger::record_addition_stage(
        &mut report.ledger,
        "external-tool-preview-merge",
        rows_in,
        rows_out,
        "external_tool_preview_findings",
    );
    // W3: reconcile the ledger's ExternalTool-tier entries against what ACTUALLY happened in
    // the pass above — unconditionally, even when the pass above never ran at all (empty
    // `preview_rules` / no corpus), because `onboard::audit_repos`'s CI-tier rule loop may have
    // already speculatively recorded such an entry as `ran = true, findings_emitted = 0` purely
    // on the strength of "a semgrep rule maps to this corpus rule" — BEFORE this pass (the one
    // that actually runs the tool) executed at all. See `reconcile_external_tool_ledger`'s doc
    // comment for why this correction can't happen earlier.
    reconcile_external_tool_ledger(report, &tools_attempted, corpus);
}

/// Join one repo's external-tool preview rows to the report: collapse those that overlap rows
/// already there ([`dedup_scan_previews_with`]), then append the rest. The rows are tool output
/// and carry snippets that reach the client, so they get the same evidence check the audit's own
/// rows get (`onboard::evidence_check`), with the counts added to the same ledger tally.
pub(crate) fn join_repo_previews(
    report: &mut crate::onboard::ScanReport,
    previews: Vec<crate::onboard::Finding>,
    selectors: Option<&std::collections::HashMap<String, Vec<String>>>,
    files: &[(String, String)],
    root: Option<&std::path::Path>,
) {
    let mut deduped = dedup_scan_previews_with(&mut report.findings, previews, selectors);
    crate::onboard::evidence_check::verify_and_record(
        &mut report.ledger,
        &mut deduped,
        files,
        root,
    );
    report.findings.extend(deduped);
}

/// Corrects the ledger's `RuleTier::ExternalTool` entries to reflect what the scan-time
/// external-tool pass (the loop in [`merge_scan_preview`] above) ACTUALLY produced, instead of
/// the speculative `ran = true, findings_emitted = 0` placeholder `onboard::audit_repos`'s
/// CI-tier rule loop records before this pass runs (that loop only knows "a semgrep rule maps
/// to this corpus rule id" — it cannot know whether semgrep is installed, provisions
/// successfully, or actually executes for THIS scan, because the two loops run in different,
/// decoupled pipeline stages).
///
/// Three outcomes, decided per rule id:
///
/// 1. **The tool ran (or wasn't needed) for every repo this scan touched** — no hard-failure
///    `CoverageNote` names it. The entry is corrected to `ran = true` with the REAL emitted
///    count (from `report.findings`, post-dedup, via [`finding_grounds_to`]) — never the
///    hardcoded `0` the pre-pass loop guessed. A genuinely clean pass (tool ran, zero findings)
///    is still honestly `verified_clean()` here.
/// 2. **The tool is absent or failed for every repo this scan touched** (a `CoverageNote` whose
///    `tool` matches names a hard failure, not merely "unrouted"). The entry is corrected to
///    `ran = false` with a disclosed reason ("commodity taint pass did not run: <reason>"), and
///    a matching [`crate::ai_audit::FailedPass`] is pushed onto `report.failed_passes` — the
///    SAME disclosure mechanism every other degraded pass in this codebase uses. Never a
///    silent "verified clean", never a refused export (`report.findings`/`report.ledger`'s
///    OTHER entries are completely untouched).
/// 3. **The tool was never even INVOKED this scan at all** — `tools_attempted` (from
///    [`merge_scan_preview`], ultimately [`crate::scan_tools::run_scan_tools`]'s own
///    `attempted` return value) doesn't contain `"semgrep"`. This happens when the whole
///    preview loop never ran (`preview_rules` empty or no corpus), or every repo's selected
///    rules happened to route away from Semgrep, or the language gate omitted it everywhere —
///    none of which produce a `CoverageNote` (a tool nothing ever tried to invoke fails
///    silently, by construction). Without this check, such a scan would fall through to
///    outcome 1 purely because `emitted == 0 && failed_tools` is empty — exactly the silent
///    "verified clean, except nothing ever ran" class this whole ledger exists to prevent. The
///    entry is corrected to `ran = false` with a disclosed reason and a `FailedPass`, the same
///    as outcome 2 — the pass-level outcome is what's authoritative here, not the absence of a
///    per-finding failure note.
///
/// A tool that failed for SOME repos but produced real findings for others is treated as
/// outcome 1 (a nonzero `emitted` count wins) — the honest statement in that case is "ran, N
/// findings", not "did not run", and the per-repo failure is still visible in
/// `report.coverage_notes` either way (this function never removes a `CoverageNote`).
/// Which external tool backs `rule_id`'s detector, for [`reconcile_external_tool_ledger`]'s
/// per-rule disclosure text. Reuses `scan_tools::tool_for_rule` verbatim (the SAME function
/// `group_by_tool`/`run_scan_tools` use to decide which tool actually drives a rule at scan
/// time) against `corpus`, so this can never answer a different question than what really
/// ran. Falls back to `"semgrep"` when `corpus` is absent or the rule id has no corpus
/// record — this is the ONLY tool `reconcile_external_tool_ledger` ever had to reason about
/// before ESLint-backed corpus rules existed, so every pre-existing caller that doesn't pass
/// a corpus (every test written before this function learned about ESLint) keeps its exact
/// original "semgrep" behavior.
pub(crate) fn external_tool_name_for_ledger_rule(
    rule_id: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> &'static str {
    corpus
        .and_then(|c| c.get_by_id(rule_id))
        .and_then(crate::scan_tools::tool_for_rule)
        .map(|t| t.name())
        .unwrap_or("semgrep")
}

/// Human-readable pass label for a tool name, used in disclosure text
/// (`report.failed_passes` / the ledger's `skip_reason`). Semgrep keeps its original,
/// already-tested "commodity taint pass" label verbatim; every other tool gets an honest
/// "the `<tool>` preview pass" label naming itself.
pub(crate) fn pass_label_for_tool(tool_name: &str) -> String {
    if tool_name == "semgrep" {
        "commodity taint pass".to_string()
    } else {
        format!("the {tool_name} preview pass")
    }
}

/// Which external tools were invoked for which repo. [`merge_scan_preview`] builds the per-repo
/// map; the flat set is the shape callers had before the ledger was per repo, and answers for
/// every repo at once.
pub(crate) trait ToolsAttempted {
    /// Whether `tool` was invoked for `repo`. An empty `repo` (an entry that names no repo) asks
    /// whether it was invoked for any repo.
    fn invoked(&self, repo: &str, tool: &str) -> bool;
}

impl ToolsAttempted for std::collections::HashSet<&str> {
    fn invoked(&self, _repo: &str, tool: &str) -> bool {
        self.contains(tool)
    }
}

impl ToolsAttempted for std::collections::HashMap<String, std::collections::HashSet<&'static str>> {
    fn invoked(&self, repo: &str, tool: &str) -> bool {
        if repo.is_empty() {
            self.values().any(|tools| tools.contains(tool))
        } else {
            self.get(repo).is_some_and(|tools| tools.contains(tool))
        }
    }
}

/// The repos named in a pass disclosure: the one repo an entry is about, or every repo of the
/// scan for an entry that names none.
pub(crate) fn repo_for_disclosure(report: &crate::onboard::ScanReport, repo: &str) -> String {
    if repo.is_empty() {
        report.repos.join(", ")
    } else {
        repo.to_string()
    }
}

pub(crate) fn reconcile_external_tool_ledger(
    report: &mut crate::onboard::ScanReport,
    tools_attempted: &impl ToolsAttempted,
    corpus: Option<&camerata_rules::RuleSet>,
) {
    // Decided per (rule, repo): a tool that failed or was never invoked on one repo says nothing
    // about the rule on another repo, and a finding on one repo is no evidence for another.
    let external: Vec<(String, String)> = report
        .ledger
        .rules()
        .filter(|r| r.tier == crate::scan_ledger::RuleTier::ExternalTool)
        .map(|r| (r.rule_id.clone(), r.repo.clone()))
        .collect();

    for (rule_id, repo) in external {
        let emitted = report
            .findings
            .iter()
            .filter(|f| (repo.is_empty() || f.repo == repo) && finding_grounds_to(f, &rule_id))
            .count();

        if emitted > 0 {
            report
                .ledger
                .correct_rule_after_external_pass(&repo, rule_id, true, None, emitted);
            continue;
        }

        // Which tool backs THIS rule id — "semgrep" unless the corpus says otherwise (see
        // `external_tool_name_for_ledger_rule`'s doc comment for why that default is safe).
        let tool_name = external_tool_name_for_ledger_rule(&rule_id, corpus);
        let pass_label = pass_label_for_tool(tool_name);

        // A note for this tool that names this repo, or names no repo at all. "unrouted" is a
        // ROUTING gap (a selected rule with no tool to drive it), never a tool-execution failure.
        let failure = report
            .coverage_notes
            .iter()
            .filter(|n| n.tool != "unrouted" && n.tool == tool_name)
            .find(|n| match n.repo.as_deref() {
                Some(note_repo) => repo.is_empty() || note_repo == repo,
                None => true,
            })
            .map(|n| (n.message.clone(), n.tool_failed));

        if let Some((reason, invoked_and_failed)) = failure {
            report.ledger.correct_rule_after_external_pass(
                &repo,
                rule_id.clone(),
                false,
                Some(format!("{pass_label} did not run: {reason}")),
                0,
            );
            if invoked_and_failed {
                // The tool was started and then crashed or wrote nothing usable: a failed part.
                // It locks a client export and is re-run on resume. One entry per repo and
                // pass, however many rules the tool was asked to check.
                let repo_name = repo_for_disclosure(report, &repo);
                let already = report
                    .failed_passes
                    .iter()
                    .any(|f| !f.scope_note && f.repo == repo_name && f.pass == pass_label);
                if !already {
                    report.failed_passes.push(crate::ai_audit::FailedPass {
                        repo: repo_name,
                        pass: pass_label.clone(),
                        reason: format!("{pass_label} failed: {reason}"),
                        files_not_reviewed: 0,
                        class: crate::ai_audit::FailureClass::Io,
                        files: Vec::new(),
                        attempts: Vec::new(),
                        scope_note: false,
                    });
                }
            } else {
                report.failed_passes.push(crate::ai_audit::FailedPass {
                    repo: repo_for_disclosure(report, &repo),
                    pass: pass_label.clone(),
                    reason: format!("{rule_id}: {pass_label} did not run: {reason}"),
                    files_not_reviewed: 0,
                    class: crate::ai_audit::FailureClass::Content,
                    files: Vec::new(),
                    attempts: Vec::new(),
                    // A mechanical tool that was not run (by policy, not installed for this
                    // stack, or no rule routed to it) is a disclosed limit of the scope, not a
                    // failed part.
                    scope_note: true,
                });
            }
            continue;
        }

        if !tools_attempted.invoked(&repo, tool_name) {
            // Outcome 3: a pass-level skip with no per-finding failure note to key off of.
            let scope = if repo.is_empty() {
                "any scanned repo".to_string()
            } else {
                repo.clone()
            };
            let reason = format!(
                "{tool_name} was never invoked for {scope} this scan (no selected rule routed to \
                 it, no supported language was present, or the preview pass did not run at all)"
            );
            report.ledger.correct_rule_after_external_pass(
                &repo,
                rule_id.clone(),
                false,
                Some(format!("{pass_label} did not run: {reason}")),
                0,
            );
            report.failed_passes.push(crate::ai_audit::FailedPass {
                repo: repo_for_disclosure(report, &repo),
                pass: pass_label.clone(),
                reason: format!("{rule_id}: {pass_label} did not run: {reason}"),
                files_not_reviewed: 0,
                class: crate::ai_audit::FailureClass::Content,
                files: Vec::new(),
                attempts: Vec::new(),
                // A mechanical tool that was not run (by policy, not installed for this stack, or
                // no rule routed to it) is a disclosed limit of the scope, not a failed part.
                scope_note: true,
            });
            continue;
        }

        // Attempted, no hard failure, genuinely zero findings — a real clean result.
        report
            .ledger
            .correct_rule_after_external_pass(&repo, rule_id, true, None, 0);
    }

    // Grading-cycle fix (2026-10-07): the loop above only CORRECTS rule ids `onboard::
    // audit_repos`'s speculative pre-pass already registered — which only ever covers
    // corpus rules the user actually SELECTED this run. But once Semgrep runs at all, it
    // runs its WHOLE bundled commodity-taint ruleset (`taint-security.yml`/`security.yml`
    // — see `scan_tools`'s module doc comment), which can produce real, exported findings
    // for a grounded corpus rule id (e.g. `SEC-NO-COMMAND-INJECTION-1`) the user never
    // selected at all, and so was NEVER pre-registered as `RuleTier::ExternalTool` in the
    // first place. Before this fix, such a finding shipped in the export while contributing
    // ZERO to every ledger total (`render_ledger_summary`'s per-family `findings_emitted`
    // sum only ever iterates the ledger's rules, which never contained that id) — the exact
    // "family reports findings=0 while preview rows ARE present in the export" defect.
    // This second pass discovers any such row and records it fresh, so the ledger's
    // per-family counts always equal the rows actually exported, never a strict subset of
    // them keyed off selection alone. It is keyed by (rule, repo), so a repo the rule was
    // tracked for does not hide a finding of the same rule in another repo.
    // Discovery is deliberately scoped to the GROUNDING MAPS only (`external_tool_floor_category`
    // — Semgrep's `semgrep_floor_category` plus ESLint's `eslint_floor_category`, each a
    // closed, well-known set) rather than trusting an untracked `f.rule_id`/`also_matches`
    // member verbatim as if it were itself a corpus id — a tool-native id with no grounding
    // mapping has no corpus rule to credit, and inventing a ledger row keyed on a raw
    // `camerata.security.*`/eslint-plugin string would just add noise with no real rule
    // behind it. ESLint's bundled base config (`assets/eslint/camerata.config.mjs`) runs a
    // handful of core rules at "warn" UNCONDITIONALLY (not gated on selection) — `eqeqeq`,
    // `no-var`, `prefer-const` among them — so exactly the same "the tool ran its whole
    // baseline, not just the selected subset" shape Semgrep's whole-ruleset behavior already
    // required this discovery pass for applies to ESLint too.
    let mut newly_discovered: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for f in &report.findings {
        if !f.preview {
            continue;
        }
        // The primary's own id, plus every absorbed sibling (`also_matches` — a merge can
        // fold in MORE THAN ONE distinct tool rule id onto one primary row), each grounded
        // independently; `also_matches` already-tracked members are the correction loop
        // above's job, not this discovery pass's.
        let candidates =
            std::iter::once(f.rule_id.as_str()).chain(f.also_matches.iter().map(String::as_str));
        for candidate in candidates {
            if let Some(grounded) = external_tool_floor_category(candidate) {
                let tracked = report.ledger.rule_for_repo(grounded, &f.repo).is_some()
                    || report.ledger.rule_for_repo(grounded, "").is_some();
                if !tracked {
                    *newly_discovered
                        .entry((grounded.to_string(), f.repo.clone()))
                        .or_insert(0) += 1;
                }
            }
        }
    }
    for ((rule_id, repo), emitted) in newly_discovered {
        report.ledger.record_rule(
            &repo,
            rule_id,
            crate::scan_ledger::RuleTier::ExternalTool,
            true,
            None,
            0,
            emitted,
        );
    }
}

/// Whether finding `f` is evidence that the corpus rule `rule_id` was evaluated — either `f`
/// IS that rule (a floor row, or a preview row whose own id already equals the corpus id), OR
/// `f`'s own rule id GROUNDS to `rule_id` via [`external_tool_floor_category`] (a standalone
/// preview row for a tool rule that hasn't been deduped into anything else — Semgrep OR
/// ESLint), OR one of `f.also_matches` does (the tool's rule id was folded into a
/// floor/higher-precedence row by [`dedup_scan_previews`] — the defect is still real and
/// still counted, just not as its own top-level row). Mirrors `ScanLedger::fired_rule_ids`'s
/// own "a merged-away rule still counts" principle at the cross-tool-grounding layer.
pub(crate) fn finding_grounds_to(f: &crate::onboard::Finding, rule_id: &str) -> bool {
    if f.rule_id == rule_id {
        return true;
    }
    if external_tool_floor_category(&f.rule_id) == Some(rule_id) {
        return true;
    }
    f.also_matches
        .iter()
        .any(|m| m == rule_id || external_tool_floor_category(m) == Some(rule_id))
}

/// The scan mode a desktop scan runs in ([`crate::ai_audit::resolve_scan_mode`]), with the one
/// notice the resolution may carry (the CLI backend cannot batch) added to the scan's notes.
pub(crate) fn scan_mode_for_run(
    requested: Option<crate::ai_audit::ScanMode>,
    backend: &crate::llm::BackendResolution,
    efficiency: crate::efficiency::EfficiencyConfig,
    notes: &mut Vec<String>,
) -> crate::ai_audit::ScanMode {
    let choice = crate::ai_audit::resolve_scan_mode(requested, backend, efficiency);
    if let Some(notice) = choice.notice {
        eprintln!("[camerata-server] {notice}");
        notes.push(notice);
    }
    choice.mode
}

/// The note a synchronous scan carries when it ran in real time although the resolved mode was
/// Message Batch.
pub const SYNCHRONOUS_SCAN_REALTIME_NOTICE: &str = "This scan ran in real time (parallel), not \
     as a Message Batch: a batch can run for hours, and this request has no job to record its \
     batch id on, so a dropped connection would leave a running, billed batch nobody can find. \
     Start the scan as a background job to use batches.";

/// The mode the SYNCHRONOUS desktop endpoint (`onboard_audit`, no job) actually runs. A Message
/// Batch there would have no persisted batch id, so a resolved `Batch` runs in real time instead
/// and says so once in the report notes; every other mode is unchanged. Batch runs only on the
/// job path ([`start_audit_job`]), where the batch id is recorded on the job.
pub(crate) fn synchronous_scan_mode(
    mode: crate::ai_audit::ScanMode,
    notes: &mut Vec<String>,
) -> crate::ai_audit::ScanMode {
    if mode != crate::ai_audit::ScanMode::Batch {
        return mode;
    }
    eprintln!("[camerata-server] {SYNCHRONOUS_SCAN_REALTIME_NOTICE}");
    notes.push(SYNCHRONOUS_SCAN_REALTIME_NOTICE.to_string());
    crate::ai_audit::ScanMode::Parallel
}

/// Mode 3 — START an async audit JOB. Spawns the same audit in the background and returns a
/// job id IMMEDIATELY, so the request never blocks for the (possibly many-minute) run. The
/// UI polls `GET /api/onboard/audit/job/:id` for progress + incremental findings, then the
/// final report. The work is decoupled from this request — it survives a dropped poll.
/// The repos one project holds, by name and by folder, for deciding which project a saved run
/// belongs to.
pub(crate) struct ProjectRepoSet {
    pub(crate) id: String,
    pub(crate) names: std::collections::HashSet<String>,
    pub(crate) dirs: Vec<std::path::PathBuf>,
}

impl ProjectRepoSet {
    /// Whether every repo the run inspected is one of this project's repos (by name or folder).
    pub(crate) fn holds_all_repos_of(&self, record: &crate::run_store::RunRecord) -> bool {
        let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        !record.repos.is_empty()
            && record.repos.iter().all(|repo| {
                self.names.contains(&repo.name)
                    || self.dirs.contains(&canon(std::path::Path::new(&repo.dir)))
            })
    }
}

/// The repo set of every project.
pub(crate) fn project_repo_sets(state: &AppState) -> Vec<ProjectRepoSet> {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    state
        .projects
        .list()
        .into_iter()
        .map(|project| ProjectRepoSet {
            id: project.id.clone(),
            names: project.repos.iter().cloned().collect(),
            dirs: resolve_local_sources(state, &project.repos)
                .sources
                .iter()
                .map(|(_, dir)| canon(dir))
                .collect(),
        })
        .collect()
}

/// Whether the saved run `record` belongs to the project `project_id`.
///
/// A cockpit run belongs to the project it was started in and to no other, whatever repos two
/// projects share: a consultant's second client must never be offered (or resume) the first
/// client's inspection. A cockpit run saved before runs recorded their project is placed by its
/// repos, and only when exactly one project holds all of them (with several holders it is listed
/// nowhere rather than guessed). The command line's runs match by repos.
pub(crate) fn run_belongs_to_project(
    sets: &[ProjectRepoSet],
    project_id: &str,
    record: &crate::run_store::RunRecord,
) -> bool {
    use crate::run_store::RunOrigin;
    match (record.origin, record.project_id.as_deref()) {
        (RunOrigin::Cockpit, Some(owner)) => owner == project_id,
        (RunOrigin::Cockpit, None) => {
            let holders: Vec<&ProjectRepoSet> = sets
                .iter()
                .filter(|set| set.holds_all_repos_of(record))
                .collect();
            holders.len() == 1 && holders[0].id == project_id
        }
        (RunOrigin::Cli, _) => sets
            .iter()
            .any(|set| set.id == project_id && set.holds_all_repos_of(record)),
    }
}

/// The refusal for resuming `record` while the open (active) project is not the one it belongs
/// to, or `None` when it belongs to the open project. A resume runs the run's saved request under
/// the open project and can land the report in it, so it must be the project that started the run.
pub(crate) fn resume_project_refusal(
    state: &AppState,
    record: &crate::run_store::RunRecord,
) -> Option<String> {
    let Some(open) = state.projects.active() else {
        return Some("open the project this inspection belongs to before resuming it".to_string());
    };
    let sets = project_repo_sets(state);
    if run_belongs_to_project(&sets, &open.id, record) {
        return None;
    }
    Some(match record.project_id.as_deref() {
        Some(owner) => {
            let name = state.projects.get(owner).map_or_else(
                || "another project".to_string(),
                |p| format!("\"{}\"", p.name),
            );
            format!(
                "this inspection was started in {name}, not in the open project \"{}\"; open \
                 {name} to run what remains, so its report lands in the right project",
                open.name
            )
        }
        None => format!(
            "this inspection was saved before runs recorded their project and cannot be placed \
             in the open project \"{}\"; run it again from the scan screen",
            open.name
        ),
    })
}

/// The query of `GET /api/inspections`.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct InspectionsQuery {
    /// The project to list for; the open (active) project when absent.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
}

/// `GET /api/inspections`: the saved inspections of the open project, newest first, each with its
/// id, when it started, its repos, where it stands (complete, incomplete, interrupted or live),
/// how many parts remain and what finishing it should cost. The cockpit offers the newest
/// unfinished one on the scan screen ("This inspection was interrupted"). A run belongs to the
/// project when every repo it inspected is one of the project's repos (by name or by folder).
pub(crate) async fn list_saved_inspections(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<InspectionsQuery>,
) -> impl IntoResponse {
    let project = query
        .project_id
        .as_deref()
        .and_then(|id| state.projects.get(id))
        .or_else(|| state.projects.active());
    let Some(project) = project else {
        return Json(serde_json::json!({ "project_id": null, "inspections": [] }));
    };
    let sets = project_repo_sets(&state);
    let project_id = project.id.clone();
    let listed_for = project_id.clone();
    let listed = tokio::task::spawn_blocking(move || {
        crate::run_store::list_inspections_in(&crate::run_store::runs_dir(), |record| {
            run_belongs_to_project(&sets, &listed_for, record)
        })
    })
    .await
    .unwrap_or_default();
    Json(serde_json::json!({ "project_id": project_id, "inspections": listed }))
}

/// What "Run what remains" would do for a saved run: the parts that remain, what they should
/// cost, and the plain reason the resume cannot start now when it cannot.
/// A run is previewed only in the project that started it, as it is resumed (409 otherwise, in
/// the same words).
pub(crate) async fn run_resume_plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Ok(record) = crate::run_store::read_record(&id) {
        if let Some(why) = resume_project_refusal(&state, &record) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": why })),
            )
                .into_response();
        }
    }
    match crate::run_store::preview_resume(&id) {
        Ok(preview) => (StatusCode::OK, Json(serde_json::json!(preview))).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Start the saved run `id` again as a background job that runs only the parts that are not
/// done. The same job the scan button starts, with the saved request and `resume_run` set, so a
/// refusal (the repo moved on, the rules changed, the run is in use) fails the job with the reason.
pub(crate) async fn run_resume_start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let record = match crate::run_store::read_record(&id) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };
    if let Some(why) = resume_project_refusal(&state, &record) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": why })),
        )
            .into_response();
    }
    let req: AuditReq =
        match serde_json::from_value(record.args) {
            Ok(r) => r,
            Err(_) => return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "this run was not started from the app, so the app cannot resume it; \
                              use `camerata inspect --resume` instead"
                })),
            )
                .into_response(),
        };
    let job_id = start_audit_job(
        &state,
        req,
        AuditJobOptions {
            resume_run: Some(id),
            ..AuditJobOptions::default()
        },
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({ "job_id": job_id })),
    )
        .into_response()
}

pub(crate) async fn onboard_audit_start(
    State(state): State<AppState>,
    Json(req): Json<AuditReq>,
) -> Json<serde_json::Value> {
    let job_id = start_audit_job(&state, req, AuditJobOptions::default()).await;
    Json(serde_json::json!({ "job_id": job_id }))
}

impl AuditReq {
    /// How this scan executes: the environment's efficiency switches, the request's spend
    /// limit and the project's fix model (`fix_model`, from [`fix_step_model`]). A limit that is
    /// not a positive number cannot be honored as written, so it becomes the smallest possible
    /// limit (one pass runs, the rest are reported as not run), never "no limit": a typo must
    /// not remove the guard.
    pub(crate) fn run_controls(
        &self,
        fix_model: Option<String>,
        clone_judge_model: Option<String>,
    ) -> crate::cost_cap::RunControls {
        crate::cost_cap::RunControls {
            clone_judge_model,
            max_cost_usd: self.max_cost_usd.map(|cap| {
                crate::cost_cap::RunControls::validate_cap(cap).unwrap_or(f64::MIN_POSITIVE)
            }),
            fix_model,
            ..crate::cost_cap::RunControls::from_env()
        }
    }

    /// [`Self::run_controls`] for the app's active project: its fix step model (unset unless the
    /// operator picked one) is what the run's fix stage is told. The one place the project's
    /// fix setting becomes run controls; both audit entry points come through it.
    pub(crate) fn run_controls_for(&self, state: &AppState) -> crate::cost_cap::RunControls {
        let mut controls = self.run_controls(fix_step_model(state), clone_judge_step_model(state));
        // The review tree's settings (group review model, escalation rate and floor) are the
        // active project's.
        if let Some(project) = state.projects.active() {
            controls.review = project.review.clone();
        }
        controls
    }
}

/// How a started audit job differs from the plain "scan" the UI's scan button starts. Every
/// field defaults to the plain scan, so [`onboard_audit_start`] passes `default()`.
#[derive(Default)]
pub(crate) struct AuditJobOptions {
    /// The project the job belongs to. `None` = the active project (the plain scan).
    pub project_id: Option<String>,
    /// A prior scan manifest to use instead of the project's persisted incremental cache. The
    /// delta rescan builds one from `git diff` so only the files changed since the inspected
    /// commit are re-audited.
    pub prior_override: Option<crate::scan_cache::ScanManifest>,
    /// Notes to show with the result (for example "the inspection ran on a dirty tree").
    pub extra_notes: Vec<String>,
    /// Set for a delta retest: the finished report is recorded as a `DeltaRetest` inspection
    /// entry instead of a full one.
    pub delta: Option<DeltaRetest>,
    /// The saved run to resume instead of starting a new one ("Run what remains"). The job then
    /// runs only the parts that are not done, through [`crate::run_store::open_for_resume`].
    pub resume_run: Option<String>,
}

/// The commits a delta retest ran between.
#[derive(Clone)]
pub(crate) struct DeltaRetest {
    pub base_inspection_id: String,
    pub from_sha: String,
    pub to_sha: String,
}
