//! W1 item 4 — the mechanical-enforcement build gate.
//!
//! A corpus rule that declares `enforcement = "mechanical"` is a promise: Camerata runs a
//! REAL, deterministic check for it, so a scan report can honestly say the rule was
//! "mechanically checked" (never flagged "verified clean" on a rule nothing ever evaluated).
//! That promise has exactly four ways to be kept today:
//!
//! 1. A registered [`camerata_checks::arch_checker::ArchChecker`] answers the rule id
//!    (`camerata_checks::arch_checker::all_checker_rule_ids`).
//! 2. The gate's own rule registry answers it (`camerata_gateway::lookup_arm`/`RULE_REGISTRY`)
//!    — the deterministic content-scan path the brownfield floor reuses.
//! 3. A bundled Semgrep rule answers it, derived from the ACTUAL shipped ruleset
//!    (`assets/semgrep-rules/security.yml`) rather than a hand-maintained list, so adding real
//!    Semgrep coverage for a rule clears it here automatically.
//! 4. The rule's own `[[sources]]` names a linter the scan-time preview actually runs
//!    (`crate::scan_tools::tool_for_linter` — Clippy, Ruff, ESLint, or Semgrep), the SAME
//!    generic, data-driven mechanism `onboard`'s preview pass already uses to run that tool
//!    against the repo and produce a real, deterministic finding. This is not a hand-waved
//!    4th category: it is the exact detector `scan_tools::run_scan_tools` wires today, for
//!    exactly this rule's own `[[sources]]`, with no additional allowlisting.
//!
//! A rule declaring `mechanical` that matches NONE of the four is a phantom: the corpus
//! claims a hard check exists, but nothing in the pipeline ever runs it, and the gap is
//! invisible until a report ships that calls it "clean". [`mechanical_rules_missing_detector`]
//! is the audit function; the test below is the CI-time gate (fails the build, lists the
//! offending ids) — see the module's companion runtime piece, `ScanLedger`
//! (`crate::scan_ledger`), which handles the SAME gap at scan time via disclosure rather than
//! a build failure (runtime must never refuse a scan; only tests may fail hard).

use std::collections::HashSet;

/// Every corpus rule id answered by a bundled Semgrep rule, derived from the shipped YAML's
/// own `id:` lines rather than hand-maintained — adding a new Semgrep rule (and its
/// `semgrep_floor_category` mapping) automatically grows this set, so it can never silently
/// drift stale the way a copy-pasted id list would.
///
/// Reads BOTH bundled rule files: `security.yml` (the pattern-based rules) and, as of W3,
/// `taint-security.yml` (the commodity-class taint rules — SQL injection, XSS, open redirect,
/// command injection). A rule added to either file is picked up automatically.
pub fn semgrep_covered_rule_ids() -> HashSet<&'static str> {
    const PATTERN_YAML: &str = include_str!("../assets/semgrep-rules/security.yml");
    const TAINT_YAML: &str = include_str!("../assets/semgrep-rules/taint-security.yml");
    [PATTERN_YAML, TAINT_YAML]
        .iter()
        .flat_map(|yaml| yaml.lines())
        .filter_map(|line| line.trim().strip_prefix("- id:"))
        .map(str::trim)
        .filter_map(crate::semgrep_floor_category)
        .collect()
}

/// Channel 4: does ANY of `rule`'s own authored `[[sources]]` name a linter the scan-time
/// preview actually runs? Reuses `scan_tools::tool_for_linter` verbatim — not a parallel
/// linter list that could drift from the real one.
pub fn has_preview_tool_source(rule: &camerata_rules::Rule) -> bool {
    rule.sources
        .iter()
        .filter_map(|s| s.linter.as_deref())
        .any(|linter| crate::scan_tools::tool_for_linter(linter).is_some())
}

/// Which detector channel (if any) answers `rule_id` deterministically today — reused by the
/// scan-time ledger (`onboard::audit_repos`) to classify a selected mechanical/architectural
/// rule the SAME way this build-time gate does, so a rule that would fail this test also
/// gets an honest "no wired detector" ledger entry at scan time rather than a silent
/// "verified clean". `checker_ids` is passed in (not recomputed) so a per-repo caller can
/// reuse the SAME config-aware set (`camerata_checks::arch_checker::checker_rule_ids_for_repo`)
/// it already computed for this repo, rather than the static whole-registry set.
///
/// `corpus` supplies channel 4 (a rule's own `[[sources]]` naming a scan-preview linter —
/// see [`has_preview_tool_source`]): `None` degrades to checking only channels 1-3, which
/// MUST NOT happen in a caller that also wants channel-4 parity with
/// [`mechanical_rules_missing_detector`] (the build gate always passes `Some`) — a caller
/// with no corpus loaded at all has no way to look up a rule's `[[sources]]` regardless.
///
/// W3 (commodity-class taint layer): a rule id in [`REGEX_DEMOTED_FOR_SEMGREP`] prefers
/// channel 3 (semgrep) over channel 2 (gateway rule registry) when BOTH exist, inverting
/// the normal precedence. See that const's doc comment for why this is scoped to a named
/// allowlist rather than a global reorder.
pub fn detector_channel(
    rule_id: &str,
    checker_ids: &HashSet<&str>,
    semgrep_ids: &HashSet<&str>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Option<&'static str> {
    if checker_ids.contains(rule_id) {
        return Some("arch_checker");
    }
    if REGEX_DEMOTED_FOR_SEMGREP.contains(&rule_id) && semgrep_ids.contains(rule_id) {
        return Some("semgrep");
    }
    if camerata_gateway::lookup_arm(rule_id).is_some() {
        Some("gateway_rule_registry")
    } else if semgrep_ids.contains(rule_id) {
        Some("semgrep")
    } else if corpus
        .and_then(|c| c.get_by_id(rule_id))
        .is_some_and(has_preview_tool_source)
    {
        Some("scan_preview_linter")
    } else {
        None
    }
}

/// Corpus rule ids whose gateway-arm detector is a known SHAPE-FITTED heuristic (authored
/// to catch one narrow syntax, not the general defect class) that a broader, taint-mode
/// external-tool rule now covers more completely. For these ids ONLY, [`detector_channel`]
/// prefers the semgrep channel over the gateway-registry channel — i.e. the regex is
/// DEMOTED from "the detector of record" to a fast in-loop backstop that still runs (Layer
/// 2/3 cannot shell out to an external tool synchronously on every file edit) but is no
/// longer what coverage reporting credits.
///
/// `SEC-NO-RAW-SQL-CONCAT-1`'s gateway arm (`camerata_gateway::sec_sql_concat_regex`) only
/// matches a DOUBLE-QUOTED string literal containing a DML keyword + confirming clause +
/// `{}`/`+` — it cannot match single-quoted strings, backtick/template literals, Python `%`
/// formatting, or a query built across multiple string segments. The semgrep taint rule
/// family `camerata.security.taint-sql-injection-*` (`assets/semgrep-rules/taint-security.yml`)
/// covers every one of those shapes across 8 languages by tracking dataflow instead of
/// matching one spelling. Demoting the regex's channel here does NOT remove it from the
/// gateway (the Layer-2/3 content-scan gate keeps running it as a synchronous, zero-cost
/// backstop) and does NOT reduce coverage — the opposite: it corrects the ledger/coverage
/// story to credit the tool that actually covers the class broadly, per the W3 plan's
/// "retire or demote the shape-fitted regex" directive.
///
/// Scoped to a named allowlist (not a global channel reorder) because several OTHER
/// gateway-registry rules (e.g. `SEC-NO-HARDCODED-SECRETS-1`'s entropy-aware secret scan)
/// are NOT shape-fitted and are the better detector of the two — reordering globally would
/// wrongly demote a stronger native check in favor of a weaker generic pattern rule.
pub const REGEX_DEMOTED_FOR_SEMGREP: &[&str] = &["SEC-NO-RAW-SQL-CONCAT-1"];

/// Every rule id in `corpus` that declares `enforcement = "mechanical"` but resolves to NONE
/// of the four wired-detector channels (see module doc). Sorted for stable, readable test
/// output. Empty on a healthy corpus.
#[cfg_attr(not(test), allow(dead_code))]
pub fn mechanical_rules_missing_detector(corpus: &camerata_rules::RuleSet) -> Vec<String> {
    let checker_ids = camerata_checks::arch_checker::all_checker_rule_ids();
    let semgrep_ids = semgrep_covered_rule_ids();
    let mut offenders: Vec<String> = corpus
        .iter()
        .filter(|r| r.enforcement == camerata_rules::EnforcementKind::Mechanical)
        .filter(|r| {
            detector_channel(r.id.0.as_str(), &checker_ids, &semgrep_ids, Some(corpus)).is_none()
        })
        .map(|r| r.id.0.clone())
        .collect();
    offenders.sort();
    offenders
}

/// Pre-existing corpus debt this gate finds but W1 does NOT fix: rules declaring
/// `enforcement = "mechanical"` with no wired detector, UNRELATED to the parameterized-SQL
/// defect this commit closes (testing-convention rules, CI/CD meta-rules, Supabase rules,
/// integration-contract rules, C#/Java/Ruby language rules with no registered checker). A
/// full-corpus run of [`mechanical_rules_missing_detector`] on the day this gate landed found
/// 35 of these, spanning concerns well beyond "pipeline-integrity ledger" scope — auditing
/// and correcting each one (wire a real detector, or correct its declared tier) is real,
/// legitimate follow-up work, tracked here explicitly rather than silently fixed or silently
/// ignored.
///
/// This list is a GRANDFATHER, not a permanent exemption: it must never grow for a rule
/// authored after this gate landed (a NEW mechanical rule with no detector fails the test
/// below immediately, which is the whole point of the gate) — it only shrinks, as each
/// pre-existing id is resolved and removed. [`every_mechanical_rule_has_a_wired_detector`]
/// below asserts BOTH that the live offender set is a SUBSET of this list (no new phantom
/// mechanical rules) AND that it does not already contain an entry this list no longer
/// needs (the list itself must not go stale once an id is actually fixed). Only read from
/// this module's own tests (see the `has_preview_tool_source` doc comment for why that's
/// marked the same way).
///
/// W3 (commodity-class taint layer) removed `RUBY-AVOID-EVAL-SEND-1`: it now resolves via
/// channel 3 (`camerata.security.taint-ruby-eval-send`, a real taint-mode detector — see
/// `assets/semgrep-rules/taint-security.yml`), closing the gap where it declared a Brakeman
/// integration Camerata never actually ran.
#[cfg_attr(not(test), allow(dead_code))]
const KNOWN_PRE_W1_MECHANICAL_GAPS: &[&str] = &[
    "CICD-CODEQL-SECURITY-SCAN-1",
    "CICD-DEPENDENCY-AUDIT-1",
    "CSHARP-ASPNETCORE-ASYNC-ACTIONS-1",
    "CSHARP-IDISPOSABLE-USING-1",
    "CSHARP-NO-HARDCODED-SECRETS-1",
    "CSHARP-NO-SWALLOWED-EXCEPTIONS-1",
    "CSHARP-NULLABLE-REFERENCE-TYPES-1",
    "GO-ERRORS-MUST-BE-CHECKED-1",
    "GO-PACKAGE-BOUNDARIES-CLEAR-1",
    "GO-TESTING-COLOCATED-TEST-FILES-1",
    "GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1",
    "GO-TESTING-HELPER-T-HELPER-1",
    "INTEGRATION-API-CONTRACT-1",
    "INTEGRATION-AUTH-SEAM-1",
    "INTEGRATION-EVENT-WIRING-1",
    "JAVA-EXCEPTION-HANDLING-1",
    "JAVA-NO-HARDCODED-SECRETS-1",
    "JAVA-RESOURCE-MANAGEMENT-1",
    "JAVA-TESTING-AAA-STRUCTURE-1",
    "JAVA-TESTING-DETERMINISTIC-1",
    "JAVA-TESTING-INTEGRATION-TEST-LOCATION-1",
    "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
    "JAVASCRIPT-REACT-RULES-OF-HOOKS-1",
    "JAVASCRIPT-TESTING-NAMING-1",
    "JAVASCRIPT-TESTING-NO-DISABLED-TESTS-1",
    "JAVASCRIPT-TESTING-UNIT-COLOCATION-1",
    "RUBY-FROZEN-STRING-LITERAL-1",
    "RUBY-RAILS-NO-SECRETS-IN-CODE-1",
    "RUBY-RAILS-STRONG-PARAMS-1",
    "RUBY-TESTING-DESCRIBE-NAMING-1",
    "RUBY-TESTING-UNIT-FILE-LOCATION-1",
    "SUPABASE-AUTH-EDGE-JWT-1",
    "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
    "SUPABASE-RLS-USER-METADATA-1",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// W3: `semgrep_covered_rule_ids` reads BOTH bundled rule files — a regression guard for
    /// the exact bug this gate caught during W3 development (the function only read
    /// `security.yml`, silently missing every id declared in the new `taint-security.yml`,
    /// which made `RUBY-AVOID-EVAL-SEND-1` look like a NEW phantom even though a real taint
    /// detector for it existed).
    #[test]
    fn semgrep_covered_rule_ids_includes_taint_file_ids() {
        let ids = semgrep_covered_rule_ids();
        assert!(
            ids.contains("SEC-NO-RAW-SQL-CONCAT-1"),
            "must include ids from taint-security.yml, not just security.yml"
        );
        assert!(ids.contains("RUBY-AVOID-EVAL-SEND-1"));
        assert!(ids.contains("SEC-NO-COMMAND-INJECTION-1"));
        assert!(ids.contains("SEC-NO-UNSAFE-HTML-SINK-1"));
        assert!(ids.contains("SEC-NO-OPEN-REDIRECT-1"));
    }

    /// W3: `SEC-NO-RAW-SQL-CONCAT-1` is in [`REGEX_DEMOTED_FOR_SEMGREP`], and
    /// `detector_channel` resolves it to `"semgrep"` (not `"gateway_rule_registry"`) once
    /// semgrep covers it — the regex is demoted from "detector of record" even though
    /// `camerata_gateway::lookup_arm` still answers this id (the gate keeps running it as a
    /// fast in-loop backstop; see [`REGEX_DEMOTED_FOR_SEMGREP`]'s doc comment).
    #[test]
    fn sql_concat_regex_is_demoted_in_favor_of_semgrep_channel() {
        assert!(REGEX_DEMOTED_FOR_SEMGREP.contains(&"SEC-NO-RAW-SQL-CONCAT-1"));
        // Sanity: the gateway arm still exists (we did not remove the backstop).
        assert!(
            camerata_gateway::lookup_arm("SEC-NO-RAW-SQL-CONCAT-1").is_some(),
            "the in-loop regex backstop must still be wired, just no longer primary"
        );
        let checker_ids: HashSet<&str> = HashSet::new();
        let semgrep_ids: HashSet<&str> = ["SEC-NO-RAW-SQL-CONCAT-1"].into_iter().collect();
        assert_eq!(
            detector_channel("SEC-NO-RAW-SQL-CONCAT-1", &checker_ids, &semgrep_ids, None),
            Some("semgrep"),
            "must prefer the semgrep channel over the gateway rule registry for this id"
        );
    }

    /// A rule NOT in [`REGEX_DEMOTED_FOR_SEMGREP`] that ALSO has both a gateway arm and a
    /// semgrep mapping keeps the ORIGINAL precedence (gateway registry first) — the demotion
    /// is scoped to the named allowlist, never a global channel reorder.
    #[test]
    fn non_demoted_rule_keeps_gateway_registry_precedence() {
        let checker_ids: HashSet<&str> = HashSet::new();
        let semgrep_ids: HashSet<&str> = ["SEC-NO-HARDCODED-SECRETS-1"].into_iter().collect();
        assert!(camerata_gateway::lookup_arm("SEC-NO-HARDCODED-SECRETS-1").is_some());
        assert_eq!(
            detector_channel(
                "SEC-NO-HARDCODED-SECRETS-1",
                &checker_ids,
                &semgrep_ids,
                None
            ),
            Some("gateway_rule_registry"),
            "a non-demoted rule must keep its original channel precedence"
        );
    }

    /// The CI-time gate: every `mechanical` corpus rule must have a real detector, OR be an
    /// already-tracked, pre-existing gap (see [`KNOWN_PRE_W1_MECHANICAL_GAPS`]). This is a
    /// TEST — it fails the build loudly, which is the point (see the module doc contrasting
    /// this with the runtime disclosure path in `scan_ledger`). If a NEW id shows up here
    /// (not in the grandfather list), the honest fix is EITHER wire a real detector for it OR
    /// correct its declared `enforcement` field to the tier that matches reality — never
    /// delete the rule and never fake a detector just to turn this green. If an id in the
    /// grandfather list is fixed, remove it from the list — leaving it in would silently hide
    /// a real fix instead of shrinking the tracked debt.
    #[tokio::test]
    async fn every_mechanical_rule_has_a_wired_detector() {
        let path = camerata_rules::corpus_path();
        let corpus = camerata_rules::load_corpus(&path)
            .await
            .expect("corpus must load cleanly");
        let offenders = mechanical_rules_missing_detector(&corpus);
        let known: HashSet<&str> = KNOWN_PRE_W1_MECHANICAL_GAPS.iter().copied().collect();
        let new_offenders: Vec<&String> = offenders
            .iter()
            .filter(|id| !known.contains(id.as_str()))
            .collect();
        assert!(
            new_offenders.is_empty(),
            "NEW mechanical-enforcement rule(s) with no wired detector (none of: arch_checker \
             registry, gateway rule registry, bundled Semgrep mapping, scan-preview linter \
             source) — these are counted as checked in every report but nothing ever evaluates \
             them: {new_offenders:?}"
        );
        let stale_grandfather: Vec<&&str> = KNOWN_PRE_W1_MECHANICAL_GAPS
            .iter()
            .filter(|id| !offenders.iter().any(|o| o == *id))
            .collect();
        assert!(
            stale_grandfather.is_empty(),
            "these ids in KNOWN_PRE_W1_MECHANICAL_GAPS now have a real detector (or were \
             corrected) — remove them from the grandfather list so it stays an accurate debt \
             count: {stale_grandfather:?}"
        );
    }

    /// The W1 item-4 regression guard: the parameterized-SQL family this commit's "honest
    /// fix" corrected (mechanical -> structured, since no detector exists for any of them)
    /// must never reappear as a mechanical-with-no-detector offender, and must never be
    /// grandfathered either — they were genuinely fixed, not swept under the known-gaps list.
    #[tokio::test]
    async fn the_w1_sql_parameterized_fix_set_is_resolved_not_grandfathered() {
        let path = camerata_rules::corpus_path();
        let corpus = camerata_rules::load_corpus(&path)
            .await
            .expect("corpus must load cleanly");
        let offenders = mechanical_rules_missing_detector(&corpus);
        let known: HashSet<&str> = KNOWN_PRE_W1_MECHANICAL_GAPS.iter().copied().collect();
        for id in [
            "CSHARP-SQL-PARAMETERIZED-1",
            "GO-SQL-PARAMETERIZED-1",
            "GO-GORM-PARAMETERIZED-QUERIES-1",
            "JAVA-SQL-PARAMETERIZED-1",
            "RUBY-RAILS-NO-STRING-SQL-1",
        ] {
            assert!(
                !offenders.iter().any(|o| o == id),
                "{id} must no longer be a mechanical-with-no-detector offender"
            );
            assert!(
                !known.contains(id),
                "{id} must not be grandfathered — it was genuinely fixed"
            );
            let rule = corpus
                .get_by_id(id)
                .unwrap_or_else(|| panic!("{id} missing from corpus"));
            assert_ne!(
                rule.enforcement,
                camerata_rules::EnforcementKind::Mechanical,
                "{id} must no longer declare mechanical enforcement"
            );
        }
    }
}
