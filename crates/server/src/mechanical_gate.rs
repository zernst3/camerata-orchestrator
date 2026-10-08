//! W1 item 4 (inverted by W4) — the mechanical-enforcement build gate.
//!
//! A corpus rule that declares `enforcement = "mechanical"` promises a REAL check exists for
//! it — but, as W4 established (see `onboard::audit_repos`'s top-level doc comment), that
//! check does NOT have to be a deterministic one. "Mechanical" means "deterministically
//! checkable IN PRINCIPLE, once someone builds a repo-specific CI gate" — on a repo Camerata
//! has never scanned, that gate usually does not exist yet. The W1-era version of this gate
//! got the invariant backwards: it REQUIRED a wired deterministic detector for every
//! `mechanical` rule and grandfathered ~35 pre-existing ids that had none, which is exactly
//! backwards — those 35 ids were never actually unchecked (once the semantic/AI pass stopped
//! being withheld from CI-tier rules), they just had no DETERMINISTIC check, which was always
//! fine as long as the model still saw them.
//!
//! A rule's promise is kept by ONE of two routes:
//!
//! 1. A REAL deterministic detector answers it — one of four channels (see
//!    [`detector_channel`]): a registered [`camerata_checks::arch_checker::ArchChecker`], the
//!    gate's own rule registry (`camerata_gateway::lookup_arm`), a bundled Semgrep rule
//!    (derived from the ACTUAL shipped ruleset, not a hand-maintained list), or the rule's own
//!    `[[sources]]` naming a linter the scan-time preview actually runs
//!    (`crate::scan_tools::tool_for_linter`).
//! 2. NO deterministic detector exists, but the rule is GUARANTEED to reach the semantic/AI
//!    pass — every selected, applicable rule does, as of W4, UNLESS it is a governance/process
//!    rule (`ORCH-*`/`SPIRIT-*`/`PROC-*` — see `onboard::audit::is_code_auditable_rule`), which
//!    is a category error to code-audit regardless of enforcement tier.
//!
//! The ONLY genuine phantom left is a `mechanical` rule that is BOTH routes' failure at once:
//! no deterministic detector AND not code-auditable (so nothing would ever hand it to the
//! model either). [`mechanical_rules_missing_detector`] finds route-1 failures;
//! [`every_mechanical_rule_with_no_detector_is_code_auditable`] below is the CI-time gate that
//! asserts route 2 always catches what route 1 misses — see the module's companion runtime
//! piece, `ScanLedger` (`crate::scan_ledger`), which records the SAME per-rule facts at scan
//! time via disclosure rather than a build failure (runtime must never refuse a scan; only
//! tests may fail hard).

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
/// output. Having no detector is NOT itself a defect since W4 — see
/// [`every_mechanical_rule_with_no_detector_is_code_auditable`], the test that asserts every
/// id this returns is still guaranteed to reach the semantic pass.
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

    /// ESLint wiring regression guard: `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` and
    /// `JAVASCRIPT-REACT-RULES-OF-HOOKS-1` both declare `enforcement = "mechanical"` with a
    /// `react-hooks:` linter source, but before `scan_tools::tool_for_linter` recognized that
    /// token, NEITHER resolved to any detector channel at all — zero Rust source anywhere
    /// referenced either id (confirmed by grep before this fix), so both relied entirely on
    /// the semantic/AI pass despite being literal, off-the-shelf ESLint rules
    /// (`react-hooks/exhaustive-deps`, `react-hooks/rules-of-hooks`). This is exactly the
    /// class of gap `mechanical_rules_missing_detector` exists to surface (even though W4
    /// made "no detector" non-fatal as long as the rule is code-auditable — see this
    /// module's top-level doc comment): a genuinely wireable ESLint rule should resolve to
    /// channel 4 (`has_preview_tool_source` / `"scan_preview_linter"`), not silently fall
    /// through to "no channel at all".
    #[tokio::test]
    async fn react_hooks_rules_resolve_to_the_scan_preview_linter_channel() {
        let path = camerata_rules::corpus_path();
        let corpus = camerata_rules::load_corpus(&path)
            .await
            .expect("corpus must load cleanly");
        let checker_ids: HashSet<&str> = HashSet::new();
        let semgrep_ids = semgrep_covered_rule_ids();

        for id in [
            "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1",
            "JAVASCRIPT-REACT-RULES-OF-HOOKS-1",
        ] {
            let rule = corpus
                .get_by_id(id)
                .unwrap_or_else(|| panic!("{id} missing from corpus"));
            assert_eq!(
                rule.enforcement,
                camerata_rules::EnforcementKind::Mechanical,
                "{id} must still declare mechanical enforcement"
            );
            assert_eq!(
                detector_channel(id, &checker_ids, &semgrep_ids, Some(&corpus)),
                Some("scan_preview_linter"),
                "{id} must resolve to the scan-preview-linter channel now that \
                 `tool_for_linter` recognizes its `react-hooks:` source"
            );
        }
    }

    /// W4's INVERTED CI-time gate. The W1-era version of this test required a wired
    /// deterministic detector for every `mechanical` rule, with a ~35-id grandfather list for
    /// the ones that had none — backwards, per this module's doc comment: having no
    /// deterministic detector is fine as long as the rule is GUARANTEED to reach the
    /// semantic/AI pass instead. That guarantee holds for every rule that is code-auditable
    /// (`onboard::audit::is_code_auditable_rule`) — as of W4, `onboard::audit_repos` hands the
    /// model every selected, applicable, code-auditable rule, CI-tier or not (see that
    /// function's top-level doc comment). So the real invariant is narrower than "must have a
    /// detector": a `mechanical` rule with no detector must NOT ALSO be a governance/process
    /// rule (`ORCH-*`/`SPIRIT-*`/`PROC-*`) — that combination is the one shape with NO route to
    /// ever being evaluated by anything. This is a TEST — it fails the build loudly, which is
    /// the point (see the module doc contrasting this with the runtime disclosure path in
    /// `scan_ledger`). A corpus author hitting this should either wire a real detector, or
    /// correct the rule's `enforcement` tier, or correct its id prefix/category — never add a
    /// grandfather list back.
    #[tokio::test]
    async fn every_mechanical_rule_with_no_detector_is_code_auditable() {
        let path = camerata_rules::corpus_path();
        let corpus = camerata_rules::load_corpus(&path)
            .await
            .expect("corpus must load cleanly");
        let offenders = mechanical_rules_missing_detector(&corpus);
        let orphaned: Vec<&String> = offenders
            .iter()
            .filter(|id| !crate::onboard::audit::is_code_auditable_rule(id))
            .collect();
        assert!(
            orphaned.is_empty(),
            "mechanical-enforcement rule(s) with NO wired deterministic detector AND NOT \
             code-auditable (governance/process ids are filtered out of the semantic prompt) — \
             these have NO route to ever being evaluated by anything: {orphaned:?}"
        );
    }

    /// W1 item-4 regression guard, generalized by W4: the parameterized-SQL family an earlier
    /// fix corrected (mechanical -> structured, since no detector exists for any of them) must
    /// never reappear as a mechanical-with-no-detector offender UNLESS it is code-auditable
    /// (in which case that is fine now, per the inverted gate above) — this just pins their
    /// declared enforcement tier so a future edit can't silently re-declare them `mechanical`
    /// without someone noticing the diff.
    #[tokio::test]
    async fn the_w1_sql_parameterized_fix_set_stays_non_mechanical() {
        let path = camerata_rules::corpus_path();
        let corpus = camerata_rules::load_corpus(&path)
            .await
            .expect("corpus must load cleanly");
        for id in [
            "CSHARP-SQL-PARAMETERIZED-1",
            "GO-SQL-PARAMETERIZED-1",
            "GO-GORM-PARAMETERIZED-QUERIES-1",
            "JAVA-SQL-PARAMETERIZED-1",
            "RUBY-RAILS-NO-STRING-SQL-1",
        ] {
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
