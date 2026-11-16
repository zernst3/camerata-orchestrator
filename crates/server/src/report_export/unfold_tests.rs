//! A ruling on a row does not rule out the places of other rules that row carries.
//!
//! The places a ruled-out row absorbed for another rule (or another service), and the rules it
//! also matches, become rows of their own: held for a decision, in every count, in the review
//! tree and in the Fixes. Synthetic two-service reports: `acme/api` and `acme/web`.

use std::collections::HashMap;

use camerata_api_types::audit::FindingKind;
use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use serde_json::Value;

use super::overlay::apply_overlay;
use super::unfold::{UNFOLDED_FROM_CAPTURE, UNFOLD_CLIENT_NOTE};
use super::*;
use crate::onboard::{Finding, MergedLocation, ScanReport};
use crate::scan_ledger::{CellState, RuleTier};
use crate::xlsx_export::build_findings_export;

const RULE_A: &str = "ARCH-SERVER-AUTHZ-1";
const RULE_B: &str = "SEC-QUERY-INPUT-1";
const RULE_C: &str = "SEC-LOGGING-1";
const API: &str = "acme/api";
const WEB: &str = "acme/web";

fn report_of(repos: &[&str]) -> ScanReport {
    let repos: Vec<String> = repos.iter().map(|r| (*r).to_string()).collect();
    let mut report = ScanReport::gated(&repos);
    report.gated = false;
    report.provenance.started_at = "2026-11-02T10:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-02T10:00:00Z".to_string();
    report
}

fn ran(report: &mut ScanReport, repo: &str, rule: &str, emitted: usize) {
    report
        .ledger
        .record_rule(repo, rule, RuleTier::Semantic, true, None, 5, emitted);
}

fn host(repo: &str) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: "src/admin.ts".to_string(),
        line: 40,
        rule_id: RULE_A.to_string(),
        severity: "high".to_string(),
        snippet: "router.post(\"/admin/40\", handler)".to_string(),
        detail: "The handler changes a record without checking the caller may do so.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

fn web_site() -> MergedLocation {
    MergedLocation::site(WEB, "src/query.ts", 7, RULE_B, "query(userInput)", false)
        .with_rating("medium", "Query built from user input")
}

/// The api's host row for rule A absorbed a rule B site in the web.
fn api_host_absorbing_a_web_site() -> ScanReport {
    let mut r = report_of(&[API, WEB]);
    ran(&mut r, API, RULE_A, 1);
    ran(&mut r, WEB, RULE_A, 0);
    ran(&mut r, API, RULE_B, 0);
    ran(&mut r, WEB, RULE_B, 1);
    let mut h = host(API);
    h.also_locations = vec![web_site()];
    r.findings.push(h);
    r
}

fn decision(key: &str, state: DecisionState) -> Decision {
    Decision {
        id: format!("dec-{key:.6}"),
        key: key.to_string(),
        state,
        reason: "a reviewer's call".to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "auditor".to_string(),
        decided_at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

fn host_key(sealed: &crate::onboard::SealedReport) -> String {
    finding_key(
        sealed
            .findings
            .iter()
            .find(|f| f.rule_id == RULE_A)
            .expect("the host row"),
    )
}

fn ruled_out_host(report: ScanReport) -> (crate::onboard::SealedReport, ReviewedReport, String) {
    let sealed = report.seal();
    let key = host_key(&sealed);
    let reviewed = apply_overlay(
        &sealed,
        &[decision(&key, DecisionState::FalsePositive)],
        &[],
        &[],
    );
    (sealed, reviewed, key)
}

fn exports(reviewed: &ReviewedReport) -> (AuditReportJson, Value) {
    let json = build_report_json(reviewed, None, &ReportOptions::default());
    let export = build_findings_export(reviewed, None, &json, &HashMap::new());
    (
        json,
        serde_json::to_value(&export).expect("findings.json serializes"),
    )
}

fn rows_of(findings_json: &Value) -> Vec<Value> {
    findings_json["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn row_for<'a>(rows: &'a [Value], rule: &str) -> Option<&'a Value> {
    rows.iter().find(|r| r["rule_id"] == rule)
}

fn unfolded<'a>(reviewed: &'a ReviewedReport, rule: &str) -> Vec<&'a Finding> {
    reviewed
        .report()
        .findings
        .iter()
        .filter(|f| f.rule_id == rule && f.captures.contains_key(UNFOLDED_FROM_CAPTURE))
        .collect()
}

#[test]
fn a_site_a_ruled_out_row_absorbed_for_another_rule_becomes_a_held_row() {
    let (_sealed, reviewed, _key) = ruled_out_host(api_host_absorbing_a_web_site());
    let (json, findings) = exports(&reviewed);
    let rows = rows_of(&findings);
    let b = row_for(&rows, RULE_B);
    assert!(b.is_some(), "findings.json holds a rule B row: {rows:?}");
    let b = b.unwrap();
    assert_eq!(b["repo"], WEB);
    assert_eq!(b["path"], "src/query.ts");
    assert_eq!(b["line"], 7);
    assert!(
        row_for(&rows, RULE_A).is_none(),
        "anti-vacuity: the ruled-out host is not among the findings"
    );
    assert!(
        json.matrix.held.iter().any(|r| r.rule_id == RULE_B),
        "held for a decision: {:?}",
        json.matrix.held
    );
    assert_eq!(
        json.cover.stats.high, 1,
        "the severity counts include the unfolded row (the host's severity before the ruling)"
    );
    let row = unfolded(&reviewed, RULE_B);
    assert_eq!(row.len(), 1);
    assert!(row[0].needs_review);
    assert!(
        reviewed
            .report()
            .tree
            .as_ref()
            .is_some_and(|t| t.nodes.iter().any(|n| n.members.contains(&row[0].site_key))),
        "the review tree has the row"
    );
    let fixes = findings["fixes"].to_string();
    assert!(
        fixes.contains("src/query.ts"),
        "a Fix groups the place: {fixes}"
    );
}

#[test]
fn the_unfolded_row_copies_the_evidence_and_carries_the_neutral_client_note() {
    let (_sealed, reviewed, _key) = ruled_out_host(api_host_absorbing_a_web_site());
    let row = unfolded(&reviewed, RULE_B);
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].snippet, "query(userInput)");
    assert_eq!(row[0].repo, WEB);
    assert_eq!(row[0].line, 7);
    assert!(
        row[0].detail.contains(UNFOLD_CLIENT_NOTE),
        "the client note is on the row: {}",
        row[0].detail
    );
    assert_eq!(
        UNFOLD_CLIENT_NOTE,
        "Was grouped with a problem that was ruled out; still open."
    );
}

#[test]
fn a_rule_the_ruled_out_host_also_matches_gets_a_row_at_the_hosts_place() {
    let mut r = report_of(&[API, WEB]);
    ran(&mut r, API, RULE_A, 1);
    ran(&mut r, WEB, RULE_A, 0);
    ran(&mut r, API, RULE_C, 0);
    ran(&mut r, WEB, RULE_C, 0);
    let mut h = host(API);
    h.also_matches = vec![RULE_C.to_string()];
    r.findings.push(h);
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    let (_json, findings) = exports(&reviewed);
    let rows = rows_of(&findings);
    let c = row_for(&rows, RULE_C);
    assert!(c.is_some(), "a rule C row at the host's place: {rows:?}");
    let c = c.unwrap();
    assert_eq!(c["repo"], API);
    assert_eq!(c["path"], "src/admin.ts");
    assert_eq!(c["line"], 40);
    let row = unfolded(&reviewed, RULE_C);
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].snippet, host(API).snippet);
}

#[test]
fn a_place_that_already_has_its_own_row_is_not_counted_twice() {
    let mut r = api_host_absorbing_a_web_site();
    r.findings.push(Finding {
        repo: WEB.to_string(),
        path: "src/query.ts".to_string(),
        line: 7,
        rule_id: RULE_B.to_string(),
        severity: "medium".to_string(),
        snippet: "query(userInput)".to_string(),
        detail: "The query is built from user input.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    });
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    assert!(unfolded(&reviewed, RULE_B).is_empty(), "no second row");
    let (_json, findings) = exports(&reviewed);
    let rows = rows_of(&findings);
    assert_eq!(
        rows.iter().filter(|r| r["rule_id"] == RULE_B).count(),
        1,
        "{rows:?}"
    );
    assert!(findings.get("open_beside_ruled_out").is_none());
}

#[test]
fn the_internal_note_names_the_host_and_the_client_files_do_not() {
    let (_sealed, reviewed, key) = ruled_out_host(api_host_absorbing_a_web_site());
    let row = unfolded(&reviewed, RULE_B);
    let note = row[0]
        .captures
        .get(UNFOLDED_FROM_CAPTURE)
        .expect("the note");
    assert!(
        note.contains(&site_id(&key)),
        "the note names the host row by its readable id: {note}"
    );
    assert!(
        !note.contains('\u{0}'),
        "the note never carries the raw site key: {note:?}"
    );
    assert!(note.contains("src/admin.ts"), "{note}");
    let (json, findings) = exports(&reviewed);
    let data = render_data_json(&json).expect("data.json renders");
    for (name, text) in [
        ("findings.json", findings.to_string()),
        ("data.json", data.clone()),
    ] {
        assert!(
            !text.contains(UNFOLDED_FROM_CAPTURE),
            "{name} leaks the note"
        );
        assert!(
            !text.contains(&key.replace('\u{0}', "\\u0000")),
            "{name} names the host key"
        );
        assert!(
            !text.contains("a reviewer's call"),
            "{name} carries the host's ruling reason"
        );
    }
    let row_text = rows_of(&findings)
        .iter()
        .find(|r| r["rule_id"] == RULE_B)
        .map(Value::to_string)
        .unwrap_or_default();
    assert!(
        row_text.contains(UNFOLD_CLIENT_NOTE),
        "anti-vacuity: the neutral note is on the row: {row_text}"
    );
    assert!(
        !row_text
            .replace(UNFOLD_CLIENT_NOTE, "")
            .contains("ruled out"),
        "nothing beyond the neutral note says the host was ruled out: {row_text}"
    );
}

#[test]
fn a_row_with_no_snippet_held_for_the_place_carries_none_and_says_so() {
    let mut r = api_host_absorbing_a_web_site();
    r.findings[0].also_locations[0].snippet = String::new();
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    let row = unfolded(&reviewed, RULE_B);
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].snippet, "", "nothing is invented");
    assert!(
        row[0].captures[UNFOLDED_FROM_CAPTURE].contains("no snippet"),
        "{:?}",
        row[0].captures
    );
}

#[test]
fn the_places_leave_the_host_so_the_patterns_cell_counts_each_once() {
    let (_sealed, reviewed, _key) = ruled_out_host(api_host_absorbing_a_web_site());
    let host_row = reviewed
        .report()
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_A)
        .unwrap();
    assert!(
        host_row.also_locations.is_empty(),
        "the place left the host"
    );
    let section = build_report_json(&reviewed, None, &ReportOptions::default())
        .patterns_across_services
        .expect("two services have the section");
    let row = section.rows.iter().find(|r| r.rule_id == RULE_B);
    assert!(
        row.is_some(),
        "the services differ on rule B: {:?}",
        section.rows
    );
    let web = row.unwrap().cells.iter().find(|c| c.repo == WEB).unwrap();
    assert_eq!(web.state, CellState::Violated { sites: 1 });
    assert_eq!(web.label, "1 place");
    let (_json, findings) = exports(&reviewed);
    assert!(
        findings.get("open_beside_ruled_out").is_none(),
        "nothing is open beside a ruled-out row: {findings}"
    );
}

#[test]
fn a_one_repo_project_gets_the_unfolded_rows_too() {
    let mut r = report_of(&[API]);
    ran(&mut r, API, RULE_A, 1);
    ran(&mut r, API, RULE_B, 1);
    let mut h = host(API);
    h.also_locations = vec![MergedLocation::site(
        API,
        "src/query.ts",
        7,
        RULE_B,
        "query(userInput)",
        false,
    )];
    r.findings.push(h);
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    let (json, findings) = exports(&reviewed);
    assert!(
        json.patterns_across_services.is_none(),
        "one repo has no patterns section"
    );
    assert!(row_for(&rows_of(&findings), RULE_B).is_some(), "{findings}");
    assert!(findings.get("open_beside_ruled_out").is_none());
}

#[test]
fn the_unfolded_row_has_a_stable_key_and_the_cockpit_view_lists_it() {
    let (sealed, reviewed, key) = ruled_out_host(api_host_absorbing_a_web_site());
    let first = unfolded(&reviewed, RULE_B)[0].site_key.clone();
    assert!(
        first.contains(&key),
        "the key is built from the host key: {first}"
    );
    assert!(first.contains(RULE_B));
    let again = apply_overlay(
        &sealed,
        &[decision(&key, DecisionState::FalsePositive)],
        &[],
        &[],
    );
    assert_eq!(unfolded(&again, RULE_B)[0].site_key, first, "deterministic");
}

#[test]
fn a_decision_on_the_unfolded_row_sticks_across_exports() {
    let (sealed, _reviewed, key) = ruled_out_host(api_host_absorbing_a_web_site());
    let first = unfolded(
        &apply_overlay(
            &sealed,
            &[decision(&key, DecisionState::FalsePositive)],
            &[],
            &[],
        ),
        RULE_B,
    )[0]
    .site_key
    .clone();
    let records = [
        decision(&key, DecisionState::FalsePositive),
        decision(&first, DecisionState::Confirmed),
    ];
    for export_number in 1..=2 {
        let reviewed = apply_overlay(&sealed, &records, &[], &[]);
        let (json, findings) = exports(&reviewed);
        assert!(
            reviewed.dispositions().contains_key(&first),
            "export {export_number}: the decision is still on the row"
        );
        let rows = rows_of(&findings);
        let row = row_for(&rows, RULE_B);
        assert!(row.is_some(), "export {export_number}: {rows:?}");
        assert!(
            row.unwrap()["disposition_label"]
                .as_str()
                .is_some_and(|l| l.starts_with("Confirmed by the reviewer")),
            "export {export_number}: the confirmation is on the row: {row:?}"
        );
        assert!(
            json.matrix.held.iter().all(|r| r.rule_id != RULE_A),
            "export {export_number}: the ruled-out host is in no bucket"
        );
    }
}

#[test]
fn ruling_the_unfolded_row_out_does_not_unfold_anything_again() {
    let (sealed, _reviewed, key) = ruled_out_host(api_host_absorbing_a_web_site());
    let first = unfolded(
        &apply_overlay(
            &sealed,
            &[decision(&key, DecisionState::FalsePositive)],
            &[],
            &[],
        ),
        RULE_B,
    )[0]
    .site_key
    .clone();
    let records = [
        decision(&key, DecisionState::FalsePositive),
        decision(&first, DecisionState::FalsePositive),
    ];
    let reviewed = apply_overlay(&sealed, &records, &[], &[]);
    assert_eq!(
        reviewed.report().findings.len(),
        2,
        "the host and the one unfolded row, nothing new: {:?}",
        reviewed
            .report()
            .findings
            .iter()
            .map(|f| (&f.rule_id, &f.site_key))
            .collect::<Vec<_>>()
    );
    let (_json, findings) = exports(&reviewed);
    assert!(
        row_for(&rows_of(&findings), RULE_B).is_none(),
        "ruled out, so not a finding"
    );
    assert!(findings.get("open_beside_ruled_out").is_none());
}

#[test]
fn an_older_clients_dispositions_unfold_too() {
    let sealed = api_host_absorbing_a_web_site().seal();
    let mut dispositions = HashMap::new();
    dispositions.insert(
        host_key(&sealed),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the value is a constant".to_string(),
            ..DispositionWire::default()
        },
    );
    let reviewed = ReviewedReport::unreviewed(&sealed, &dispositions);
    assert_eq!(unfolded(&reviewed, RULE_B).len(), 1);
}

#[test]
fn a_tool_row_filed_under_a_native_rule_keeps_that_rules_own_sites() {
    // One definition of "another rule's place": a tool's row the review tree files under the
    // native rule is in that rule's family, so a site of the native rule in its own service is
    // the row's own and is ruled with it, while a site of rule B is another rule's.
    let mut r = report_of(&[API, WEB]);
    ran(&mut r, API, RULE_A, 1);
    ran(&mut r, WEB, RULE_A, 0);
    ran(&mut r, API, RULE_B, 0);
    ran(&mut r, WEB, RULE_B, 0);
    let mut tool = host(API);
    tool.rule_id = "semgrep.express-missing-authz".to_string();
    tool.preview_tool = Some("semgrep".to_string());
    tool.group_path = vec![
        "cat:authorization".to_string(),
        format!("rule:{RULE_A}"),
        format!("sub:{RULE_A}/1"),
    ];
    tool.also_locations = vec![
        MergedLocation::site(API, "src/other.ts", 3, RULE_A, "router.get(\"/x\")", false),
        MergedLocation::site(API, "src/query.ts", 9, RULE_B, "query(input)", false),
    ];
    r.findings.push(tool);
    let sealed = r.seal();
    let key = finding_key(&sealed.findings[0]);
    let reviewed = apply_overlay(
        &sealed,
        &[decision(&key, DecisionState::FalsePositive)],
        &[],
        &[],
    );
    assert_eq!(
        unfolded(&reviewed, RULE_B).len(),
        1,
        "rule B's site unfolds"
    );
    assert!(
        unfolded(&reviewed, RULE_A).is_empty(),
        "the native rule's own site is the tool row's and is ruled out with it"
    );
    let host_row = reviewed
        .report()
        .findings
        .iter()
        .find(|f| f.rule_id.starts_with("semgrep"))
        .unwrap();
    assert_eq!(
        host_row.also_locations.len(),
        1,
        "the own-family site stays"
    );
}

// ── two places of one rule in one file are two problems ──────────────────────────────────

fn b_site_at(line: usize, snippet: &str) -> MergedLocation {
    MergedLocation::site(WEB, "src/query.ts", line, RULE_B, snippet, false)
        .with_rating("medium", "Query built from user input")
}

fn host_carrying(sites: Vec<MergedLocation>) -> ScanReport {
    let mut r = report_of(&[API, WEB]);
    ran(&mut r, API, RULE_A, 1);
    ran(&mut r, WEB, RULE_A, 0);
    ran(&mut r, API, RULE_B, 0);
    ran(&mut r, WEB, RULE_B, 1);
    let mut h = host(API);
    h.also_locations = sites;
    r.findings.push(h);
    r
}

fn lines_of(reviewed: &ReviewedReport, rule: &str) -> Vec<usize> {
    let mut lines: Vec<usize> = unfolded(reviewed, rule).iter().map(|f| f.line).collect();
    lines.sort_unstable();
    lines
}

#[test]
fn two_places_of_one_rule_with_one_snippet_in_one_file_are_two_problems() {
    let r = host_carrying(vec![
        b_site_at(7, "query(userInput)"),
        b_site_at(30, "query(userInput)"),
    ]);
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    assert_eq!(lines_of(&reviewed, RULE_B), vec![7, 30]);
    let keys: std::collections::HashSet<&str> = unfolded(&reviewed, RULE_B)
        .iter()
        .map(|f| f.site_key.as_str())
        .collect();
    assert_eq!(keys.len(), 2, "each problem has a key of its own");
    let host_row = reviewed
        .report()
        .findings
        .iter()
        .find(|f| f.rule_id == RULE_A)
        .unwrap();
    assert!(host_row.also_locations.is_empty());
}

#[test]
fn a_carried_place_beside_a_problem_of_its_own_in_the_same_file_still_gets_a_problem() {
    let mut r = host_carrying(vec![b_site_at(30, "query(userInput)")]);
    r.findings.push(Finding {
        repo: WEB.to_string(),
        path: "src/query.ts".to_string(),
        line: 7,
        rule_id: RULE_B.to_string(),
        severity: "medium".to_string(),
        snippet: "query(userInput)".to_string(),
        detail: "The query is built from user input.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    });
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    assert_eq!(
        lines_of(&reviewed, RULE_B),
        vec![30],
        "line 30 has no problem of its own, so it gets one"
    );
}

#[test]
fn two_places_with_no_snippet_in_one_file_are_two_problems() {
    let r = host_carrying(vec![b_site_at(7, ""), b_site_at(30, "")]);
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    assert_eq!(lines_of(&reviewed, RULE_B), vec![7, 30]);
}

// ── what the new problem says about itself ───────────────────────────────────────────────

#[test]
fn a_split_out_problem_takes_the_higher_of_its_own_severity_and_the_hosts() {
    // The place was rated critical on its own; the host was high. The place keeps its rating.
    let mut r = host_carrying(vec![
        b_site_at(7, "query(a)").with_rating("critical", "Query built from user input")
    ]);
    r.findings[0].severity = "high".to_string();
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    let row = unfolded(&reviewed, RULE_B);
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].severity, "critical");

    // The place was rated low; the host was high. The host's rating stands.
    let mut r = host_carrying(vec![
        b_site_at(7, "query(a)").with_rating("low", "Query built from user input")
    ]);
    r.findings[0].severity = "high".to_string();
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    assert_eq!(unfolded(&reviewed, RULE_B)[0].severity, "high");

    // A place with no recorded rating takes the host's.
    let mut r = host_carrying(vec![MergedLocation::site(
        WEB,
        "src/query.ts",
        7,
        RULE_B,
        "query(a)",
        false,
    )]);
    r.findings[0].severity = "high".to_string();
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    assert_eq!(unfolded(&reviewed, RULE_B)[0].severity, "high");
}

#[test]
fn a_place_a_tool_found_keeps_the_tool_as_its_source() {
    let mut r = host_carrying(vec![web_site()]);
    r.findings[0].preview_tool = Some("semgrep".to_string());
    let (_sealed, reviewed, _key) = ruled_out_host(r);
    let row = unfolded(&reviewed, RULE_B);
    assert_eq!(row[0].preview_tool.as_deref(), Some("semgrep"));
    assert_ne!(
        row[0].confidence.as_deref(),
        Some("needs-review"),
        "the held mark is needs_review, not a confidence that relabels the source"
    );
    let (_json, findings) = exports(&reviewed);
    let rows = rows_of(&findings);
    let b = row_for(&rows, RULE_B).expect("the rule B row");
    assert_eq!(
        b["provenance"], "Preview: semgrep",
        "the Source column reads Tool: semgrep from this: {b}"
    );
}

// ── the backstop's producer ──────────────────────────────────────────────────────────────

#[test]
fn the_backstop_key_lists_a_place_a_ruled_out_host_still_carries() {
    // A reviewed report the overlay did not unfold: the ruled-out host still carries rule B's
    // place, and the export's key lists it.
    let sealed = api_host_absorbing_a_web_site().seal();
    let mut dispositions = HashMap::new();
    dispositions.insert(
        host_key(&sealed),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the value is a constant".to_string(),
            ..DispositionWire::default()
        },
    );
    let reviewed = ReviewedReport::unreviewed_without_unfold(&sealed, &dispositions);
    assert!(
        unfolded(&reviewed, RULE_B).is_empty(),
        "anti-vacuity: nothing was unfolded"
    );
    let export_key = {
        let json = build_report_json(&reviewed, None, &ReportOptions::default());
        build_findings_export(&reviewed, None, &json, &HashMap::new()).open_beside_ruled_out
    };
    assert_eq!(export_key.len(), 1, "{export_key:?}");
    assert_eq!(export_key[0].rule_id, RULE_B);
    assert_eq!(export_key[0].repo, WEB);
    assert_eq!(export_key[0].path, "src/query.ts");
    assert_eq!(export_key[0].line, 7);
}

#[test]
fn a_split_out_problem_with_a_cited_rule_leaves_needs_a_decision_once_confirmed() {
    // A cited corpus rule, so the only reason the problem waits is the split-out hold.
    let mut rule = camerata_rules::ruleset_with_unauthored_rule(RULE_B)
        .get_by_id(RULE_B)
        .expect("the fixture rule")
        .clone();
    rule.options[0].remediation = Some("Pass the value as a bound parameter.".to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    let (sealed, _reviewed, key) = ruled_out_host(api_host_absorbing_a_web_site());
    let ruled_out = decision(&key, DecisionState::FalsePositive);
    let held_of = |reviewed: &ReviewedReport| {
        let json = build_report_json(reviewed, Some(&set), &ReportOptions::default());
        json.matrix
            .held
            .iter()
            .filter(|r| r.rule_id == RULE_B)
            .count()
    };
    let undecided = apply_overlay(&sealed, std::slice::from_ref(&ruled_out), &[], &[]);
    assert_eq!(held_of(&undecided), 1, "waits for a decision");
    let split = unfolded(&undecided, RULE_B)[0].site_key.clone();
    let confirmed = apply_overlay(
        &sealed,
        &[ruled_out, decision(&split, DecisionState::Confirmed)],
        &[],
        &[],
    );
    assert_eq!(held_of(&confirmed), 0, "Confirmed, so it no longer waits");
    let json = build_report_json(&confirmed, Some(&set), &ReportOptions::default());
    let export = build_findings_export(&confirmed, Some(&set), &json, &HashMap::new());
    let rows = rows_of(&serde_json::to_value(&export).unwrap());
    assert!(
        row_for(&rows, RULE_B).is_some(),
        "anti-vacuity: the confirmed problem is still a problem: {rows:?}"
    );
}
