//! Pass-1 W8 — the W5-Q2 severity FLOOR, proved against the REAL shipped corpus.
//!
//! > **W5-Q2 DECIDED — a single named severity is a FLOOR, not a ceiling.**
//! > Zach: *"Floor, it's the minimum rating."*
//! >
//! > — the audit findings tracker
//!
//! # Why this file exists separately from `report_export.rs`'s own `req_w8_*` tests
//!
//! Every unit test over `raise_to_rule_min_severity` builds its own SYNTHETIC single-rule
//! corpus (`ruleset_with_min_severity("FLOOR-1", "high", None)`) and proves the function
//! behaves. Not one of them could fail if the real corpus declared no floors at all.
//!
//! That is not a hypothetical. The E2 ceiling shipped in exactly that state: `max_severity`,
//! `clamp_to_rule_max_severity` and all four `req_e2_*` tests passed green against a corpus in
//! which **0 of 417** rules declared a ceiling. The mechanism was fully implemented, fully
//! "tested", and completely inert — the precise failure the tracker's test discipline names:
//!
//! > "I don't want code that confidently thinks it's doing what is written here in the
//! > product requirements, but actually isn't."
//!
//! So these tests take the floor from the **real, shipped corpus TOMLs** — never from a
//! fixture — and drive a real report export through `build_report_json`. They are the tests
//! that would have failed before the transcription commit.
//!
//! # No fixture repos, no benchmark repos
//!
//! Nothing here reads a fixture or benchmark repository. The only external input is the
//! bundled rule corpus (`camerata_rules::DEFAULT_CORPUS_PATH`), and the floors asserted are
//! the ones transcribed from each rule's own authored prose. A rule id appears below only
//! because its TOML states that severity for its own flagged condition.
//!
//! # Both row sections are scanned (W5-Q4)
//!
//! `exported_severities` chains `curated_findings` and `held_for_review_findings`. A severity
//! test reading only the former *panics* on any rule `is_informational` routes out of every
//! action bucket by rule identity, and those are exactly the rules whose severity bound is
//! most emphatically authored. Zach's W5-Q4 ruling: *"yes, widen these tests."*

use std::collections::HashMap;

use camerata_api_types::audit::FindingKind;
use camerata_server::onboard::{AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport};
use camerata_server::report_export::{build_report_json, ReportOptions};

/// Load the real shipped corpus, or `None` when it isn't checked out (mirrors the
/// `path.exists()` skip guard every corpus-reading test in `camerata_rules` uses).
async fn real_corpus() -> Option<camerata_rules::RuleSet> {
    let path = std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(
        camerata_rules::load_corpus(path)
            .await
            .expect("the real corpus loads"),
    )
}

/// A located `Defect`-kind finding — what every accusing producer actually emits
/// (`ai_audit::parse_findings`, `apply_alternative_decision`, `dep_audit`, the deterministic
/// floor, the external-tool previews). Both properties are load-bearing: the floor declines a
/// row with no kind (W3's "nothing silently defaults to Defect") and declines a row with no
/// location (C4-R1's null-location invariant), so a finding missing either would exercise the
/// decline path rather than the raise.
fn defect(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: "owner/repo".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("detail for {rule_id}"),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

fn report_with(findings: Vec<Finding>, audited_rule_ids: Vec<&str>) -> ScanReport {
    ScanReport {
        run_id: None,
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: Default::default(),
        repos: vec!["owner/repo".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 2,
        excluded_noise_lines: None,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "TypeScript".to_string(),
            lines: 400,
        }],
        excluded_mechanical_rules: Vec::new(),
        findings,
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance: ScanProvenance {
            audited_refs: vec![AuditedRef {
                repo: "owner/repo".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: Some("test-cal-model".to_string()),
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: Some("1.9.0".to_string()),
            vulnerability_data: None,
            started_at: "2026-10-11T00:00:00Z".to_string(),
            finished_at: "2026-10-11T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    }
}

/// Every severity a one-finding report actually EXPORTS for `rule_id`. Taken from the real
/// export path (`build_report_json`) rather than by calling the raise helper directly — "no
/// report may rank below it" is a statement about the artifact, not about a function.
///
/// Scans BOTH top-level row sections, for the reason in this module's doc comment.
fn exported_severities(
    rule_id: &str,
    proposed: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<String> {
    let report = report_with(
        vec![defect(rule_id, "src/a.ts", 1, proposed)],
        vec![rule_id],
    );
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        corpus,
        &ReportOptions::default(),
    );
    json.curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter())
        .map(|s| s.severity.clone())
        .collect()
}

/// [`exported_severities`] for the single-site case, asserting the row was exported exactly
/// once so a silently-dropped row can never read as a passing raise.
fn exported_severity(
    rule_id: &str,
    proposed: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    let severities = exported_severities(rule_id, proposed, corpus);
    assert_eq!(
        severities.len(),
        1,
        "{rule_id} must export exactly one site; a dropped row is not a raise: {severities:?}"
    );
    severities[0].clone()
}

/// Rank within the severity vocabulary, lower is MORE severe — the same direction as
/// `report_export::severity_rank`.
fn rank(sev: &str) -> u8 {
    match sev {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// req_w8: a floor **authored in a real corpus rule** raises a real report that would
/// otherwise rank below it. `SQL-MONEY-FLOAT-1`'s own prose reads "Flag as high severity:
/// 'Your `<table>.<column>` column stores a dollar amount as floating …'" — so a `low` export
/// contradicts the rule's own authored minimum, no matter what calibration proposed.
///
/// FAILS BEFORE THE FIX: on `55515b5` (floor mechanism landed, corpus untouched) this rule's
/// TOML carried no `min_severity` — 0 of 417 rules did — `raise_to_rule_min_severity` took its
/// `None` early-return, and the export kept `low`, so `assert_eq!(severity, "high")` failed
/// with `left: "low"`. Verified by `git checkout 55515b5 -- crates/rules/principles/` and
/// re-running.
#[tokio::test]
async fn req_w8_an_authored_floor_raises_a_real_rule() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SQL-MONEY-FLOAT-1";
    assert_eq!(
        corpus
            .iter()
            .find(|r| r.id.0 == RULE)
            .and_then(|r| r.min_severity.clone())
            .as_deref(),
        Some("high"),
        "{RULE} must declare the floor its own prose authors; without it this test is vacuous"
    );

    assert_eq!(
        exported_severity(RULE, "low", Some(&corpus)),
        "high",
        "{RULE} authors \"Flag as high severity\"; no report may export below high for it"
    );
}

/// A checker-emitted `info` is an OBSERVATION ("confirm in the dashboard"), not a live defect, so
/// the corpus floor never raises it (decision logged 2026-10-28). The real case is
/// `SUPABASE-RLS-POLICY-DISABLED-1`'s platform-owned row: severity `info`, rule floor `critical`.
/// Before the exemption the export (and `camerata-check`) read it as `critical`. The `low` control
/// proves the rule's floor still raises a real, lower-rated defect.
#[tokio::test]
async fn an_info_observation_is_never_raised_by_the_floor_but_a_low_defect_still_is() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SUPABASE-RLS-POLICY-DISABLED-1";
    assert_eq!(
        corpus
            .iter()
            .find(|r| r.id.0 == RULE)
            .and_then(|r| r.min_severity.clone())
            .as_deref(),
        Some("critical"),
        "{RULE} must declare its critical floor, or this test is vacuous"
    );
    assert_eq!(
        exported_severity(RULE, "info", Some(&corpus)),
        "info",
        "an info observation must not be floored to critical"
    );
    assert_eq!(
        exported_severity(RULE, "low", Some(&corpus)),
        "critical",
        "the floor still raises a below-floor defect"
    );
}

/// req_w8 (attribution control): the SAME `low` finding under a real corpus rule that authors
/// NO floor still exports `low`. Without this, the test above would also pass if some
/// unrelated part of the export pipeline happened to raise everything — it proves the raise is
/// attributable to the authored floor specifically.
///
/// `ARCH-OBSERVABILITY-SILENT-FAILURE-1` is the deliberate counterpart: its prose says the
/// pipeline "forces every finding of this rule to severity Low … regardless of the model's
/// proposed severity" and that it is "deliberately CAPPED LOW (owner directive)", so it is
/// exactly the rule a floor must never have been swept onto.
#[tokio::test]
async fn req_w8_a_rule_that_authored_no_floor_is_untouched() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "ARCH-OBSERVABILITY-SILENT-FAILURE-1";
    assert!(
        corpus
            .iter()
            .find(|r| r.id.0 == RULE)
            .expect("the control rule exists in the corpus")
            .min_severity
            .is_none(),
        "{RULE} is CAPPED LOW by owner directive — it must carry NO floor. A floor here would \
         be the over-reach failure: silently inflating a row the owner capped."
    );

    assert_eq!(
        exported_severity(RULE, "low", Some(&corpus)),
        "low",
        "a floor-free rule must be untouched; the W8 raise is per-rule, never global"
    );
}

/// req_w8: the floor is a FLOOR, not a ceiling — a real corpus rule's authored minimum must
/// never LOWER a finding that already ranks above it. `SUPABASE-RLS-NO-POLICY-1` is the
/// sharpest case available, because its prose authors BOTH bounds at medium ("emit a
/// medium-severity finding" / "it ships at medium severity"), so floor and ceiling are both
/// present and the two must compose rather than fight.
#[tokio::test]
async fn req_w8_a_floor_never_lowers_a_more_severe_finding() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SUPABASE-FUNC-DYNAMIC-SQL-INJECTION-1";
    // "Severity is High by default; Critical when the same function is GRANTed …" — the floor
    // is high and the named escalation above it must survive.
    assert_eq!(
        exported_severity(RULE, "critical", Some(&corpus)),
        "critical",
        "{RULE}'s prose names an explicit Critical escalation above its High floor; a floor \
         must never cap it back down"
    );
    assert_eq!(
        exported_severity(RULE, "low", Some(&corpus)),
        "high",
        "{RULE}'s floor is high, so a low proposal must be raised"
    );
}

/// req_w8: a rule whose prose authors BOTH bounds at the same severity is pinned there from
/// both sides, taken from the real corpus. `SUPABASE-STORAGE-PUBLIC-BUCKET-1` says "emit a
/// high-severity `needs_review` finding" (the floor) and "`needs_review`, not an automatic
/// critical" (the ceiling W5 transcribed) — two sentences, one severity.
#[tokio::test]
async fn req_w8_a_rule_authoring_both_bounds_is_pinned_at_one_severity() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SUPABASE-STORAGE-PUBLIC-BUCKET-1";
    let rule = corpus
        .iter()
        .find(|r| r.id.0 == RULE)
        .expect("the rule exists in the corpus");
    assert_eq!(rule.min_severity.as_deref(), Some("high"));
    assert_eq!(rule.max_severity.as_deref(), Some("high"));
    assert_eq!(
        exported_severity(RULE, "critical", Some(&corpus)),
        "high",
        "the ceiling must still clamp a critical down"
    );
    assert_eq!(
        exported_severity(RULE, "low", Some(&corpus)),
        "high",
        "the floor must still raise a low up"
    );
}

/// req_w8: every floor the real corpus declares is actually HONOURED at the export boundary —
/// stated as a property over whatever the corpus contains, so a floor added later is covered
/// without editing this file. For each rule declaring a floor, propose the weakest severity in
/// the vocabulary and assert the export never ranks below the declared value.
///
/// FAILS BEFORE THE FIX: the loop body never executed on `55515b5` (no rule declared a floor),
/// so the test was vacuous; the explicit non-empty assertion below is what turns that vacuity
/// into a failure.
#[tokio::test]
async fn req_w8_no_report_ranks_below_any_floor_the_corpus_declares() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let declaring: Vec<(String, String)> = corpus
        .iter()
        .filter_map(|r| r.min_severity.clone().map(|f| (r.id.0.clone(), f)))
        .collect();
    assert!(
        !declaring.is_empty(),
        "W8 is INERT: no corpus rule declares a floor, so this property is vacuous"
    );
    for (rule_id, floor) in &declaring {
        let severities = exported_severities(rule_id, "low", Some(&corpus));
        assert!(
            !severities.is_empty(),
            "{rule_id} exported no row at all, so this iteration proves nothing"
        );
        for severity in &severities {
            assert!(
                rank(severity) <= rank(floor),
                "{rule_id} declares min_severity = {floor:?} but the report exported \
                 {severity:?}, which ranks below it"
            );
        }
    }
}

/// req_w8: the floor and the E2 ceiling compose for EVERY rule that authors both — the export
/// always lands inside `[min_severity, max_severity]`, from a proposal at either extreme.
/// Stated as a property so the corpus-load gate's "unsatisfiable pair" rejection has a
/// behavioural counterpart: the gate proves the pair is coherent, this proves the pipeline
/// honours it.
#[tokio::test]
async fn req_w8_the_floor_and_the_ceiling_compose_for_every_rule_authoring_both() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let both: Vec<(String, String, String)> = corpus
        .iter()
        .filter_map(|r| {
            Some((
                r.id.0.clone(),
                r.min_severity.clone()?,
                r.max_severity.clone()?,
            ))
        })
        .collect();
    assert!(
        !both.is_empty(),
        "no corpus rule authors BOTH bounds, so the composition property is vacuous"
    );
    for (rule_id, floor, ceiling) in &both {
        for proposed in ["critical", "low"] {
            for severity in exported_severities(rule_id, proposed, Some(&corpus)) {
                assert!(
                    rank(&severity) <= rank(floor) && rank(&severity) >= rank(ceiling),
                    "{rule_id} declares min_severity = {floor:?} / max_severity = {ceiling:?} \
                     but a {proposed:?} proposal exported {severity:?}, outside that range"
                );
            }
        }
    }
}
