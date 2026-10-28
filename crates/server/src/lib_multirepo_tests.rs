//! Several repos in one scan: the external-tool reconciliation and the refusal of a run that would
//! cover fewer repos than it was asked to.

use super::*;
use crate::scan_ledger::RuleTier;

const RULE: &str = "SEC-NO-RAW-SQL-CONCAT-1";

fn two_repo_report() -> crate::onboard::ScanReport {
    crate::onboard::ScanReport::gated(&["acme/one".to_string(), "acme/two".to_string()])
}

fn preview_finding(repo: &str) -> crate::onboard::Finding {
    crate::onboard::Finding {
        repo: repo.to_string(),
        path: "src/db.py".to_string(),
        line: 10,
        rule_id: "camerata.security.taint-sql-injection-python".to_string(),
        severity: "high".to_string(),
        preview: true,
        preview_tool: Some("semgrep".to_string()),
        ..crate::onboard::Finding::default()
    }
}

fn record_for_both(report: &mut crate::onboard::ScanReport) {
    for repo in ["acme/one", "acme/two"] {
        report
            .ledger
            .record_rule(repo, RULE, RuleTier::ExternalTool, true, None, 3, 0);
    }
}

fn attempted_on(
    repos: &[&str],
) -> std::collections::HashMap<String, std::collections::HashSet<&'static str>> {
    repos
        .iter()
        .map(|r| ((*r).to_string(), ["semgrep"].into_iter().collect()))
        .collect()
}

#[test]
fn a_tool_that_failed_on_one_repo_is_not_reported_as_having_run_on_it() {
    let mut report = two_repo_report();
    record_for_both(&mut report);
    report.coverage_notes = vec![crate::onboard::CoverageNote {
        repo: Some("acme/two".to_string()),
        tool: "semgrep".to_string(),
        message: "Could not preview 1 rule(s) with semgrep: exited 2".to_string(),
    }];
    reconcile_external_tool_ledger(&mut report, &attempted_on(&["acme/one", "acme/two"]), None);

    let one = report.ledger.rule_for_repo(RULE, "acme/one").unwrap();
    let two = report.ledger.rule_for_repo(RULE, "acme/two").unwrap();
    assert!(one.ran && one.skip_reason.is_none(), "{one:?}");
    assert!(!two.ran, "{two:?}");
    assert!(two.skip_reason.as_deref().unwrap().contains("did not run"));

    // The failed pass names the repo it failed on, not every repo of the scan.
    let passes: Vec<_> = report
        .failed_passes
        .iter()
        .filter(|f| f.pass == "commodity taint pass")
        .collect();
    assert_eq!(passes.len(), 1, "{passes:?}");
    assert_eq!(passes[0].repo, "acme/two");

    // And the rule is not clean for the project.
    assert!(!report.ledger.healthy_rule_ids().contains(&RULE.to_string()));
    assert_eq!(
        report.ledger.rule(RULE).unwrap().repos_not_run,
        vec!["acme/two".to_string()]
    );
}

#[test]
fn findings_from_one_repo_are_no_evidence_the_tool_ran_on_the_other() {
    let mut report = two_repo_report();
    record_for_both(&mut report);
    report.findings = vec![preview_finding("acme/one")];
    // The tool was invoked for repo one only.
    reconcile_external_tool_ledger(&mut report, &attempted_on(&["acme/one"]), None);

    let one = report.ledger.rule_for_repo(RULE, "acme/one").unwrap();
    assert!(one.ran);
    assert_eq!(one.findings_emitted, 1);
    let two = report.ledger.rule_for_repo(RULE, "acme/two").unwrap();
    assert!(!two.ran, "never invoked for acme/two: {two:?}");
    assert!(two
        .skip_reason
        .as_deref()
        .unwrap()
        .contains("never invoked for acme/two"));
    assert_eq!(
        report
            .failed_passes
            .iter()
            .filter(|f| f.repo == "acme/two")
            .count(),
        1
    );
}

#[test]
fn a_clean_run_on_every_repo_stays_verified_clean() {
    let mut report = two_repo_report();
    record_for_both(&mut report);
    reconcile_external_tool_ledger(&mut report, &attempted_on(&["acme/one", "acme/two"]), None);
    assert_eq!(report.ledger.healthy_rule_ids(), vec![RULE.to_string()]);
    assert!(report.failed_passes.is_empty());
}

#[test]
fn a_finding_of_a_rule_nobody_selected_is_tracked_for_its_own_repo() {
    let mut report = two_repo_report();
    // Tracked for repo one only; repo two's finding for the same rule must still be recorded.
    report
        .ledger
        .record_rule("acme/one", RULE, RuleTier::ExternalTool, true, None, 3, 0);
    report.findings = vec![preview_finding("acme/two")];
    reconcile_external_tool_ledger(&mut report, &attempted_on(&["acme/one", "acme/two"]), None);
    let two = report.ledger.rule_for_repo(RULE, "acme/two").unwrap();
    assert!(two.ran);
    assert_eq!(two.findings_emitted, 1);
}

#[test]
fn an_unresolvable_repo_stops_the_run_unless_a_partial_run_was_asked_for() {
    let unresolved = vec![
        ("acme/two".to_string(), "no local folder is set".to_string()),
        (
            "acme/three".to_string(),
            "/Users/auditor/three is not a local git clone".to_string(),
        ),
    ];
    let refusal = partial_run_refusal(&unresolved, 3, false).expect("refused");
    assert!(refusal.contains("2 of 3 repositories"), "{refusal}");
    assert!(refusal.contains("acme/two"), "{refusal}");
    assert!(refusal.contains("acme/three"), "{refusal}");
    assert!(refusal.contains("allow_partial"), "{refusal}");
    assert!(!refusal.contains('\u{2014}'), "no em dash: {refusal}");
    assert!(partial_run_refusal(&unresolved, 3, true).is_none());
    assert!(partial_run_refusal(&[], 3, false).is_none());
}

#[test]
fn a_repo_left_out_of_a_partial_run_is_recorded_as_not_inspected() {
    let mut report = crate::onboard::ScanReport::gated(&["acme/one".to_string()]);
    let selected = vec![crate::onboard::SelectedRule {
        id: "ARCH-X-1".to_string(),
        directive: "x".to_string(),
        repos: Vec::new(),
    }];
    record_unresolved_repos(
        &mut report,
        &[("acme/two".to_string(), "no local folder is set".to_string())],
        &selected,
        None,
    );
    assert_eq!(
        report.repos_not_inspected,
        vec![("acme/two".to_string(), "no local folder is set".to_string())]
    );
    assert!(report
        .failed_passes
        .iter()
        .any(|f| f.repo == "acme/two" && f.pass == crate::onboard::REPOSITORY_READ_PASS));
    let entry = report.ledger.rule_for_repo("ARCH-X-1", "acme/two").unwrap();
    assert!(!entry.ran);
    // Recording the same repo twice does not duplicate it.
    record_unresolved_repos(
        &mut report,
        &[("acme/two".to_string(), "again".to_string())],
        &selected,
        None,
    );
    assert_eq!(report.repos_not_inspected.len(), 1);
}
