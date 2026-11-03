//! WO8 at the release gate: a merge needs its decision record, a group decision needs a record per
//! site, a restored finding needs a ruling; and what the reviewer's overlay writes into a package
//! (design 5.3, 5.5 and 3B).

use std::collections::HashMap;

use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use camerata_api_types::edits::{Edit, EditField};

use super::held::restored_without_decision;
use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::overlay::{MERGED_INTO_CAPTURE, MERGED_LABEL_CAPTURE};
use crate::report_export::{apply_overlay, build_report_json, render_data_json, ReviewedReport};

const NOW: &str = "2026-11-04T09:00:00Z";

fn second_row(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("format!(\"select {{}}\", x{line})"),
        detail: "Raw SQL \u{2014} The query is built from the request.".to_string(),
        ..Finding::default()
    }
}

fn report_of_three() -> ScanReport {
    let mut report = scan_report();
    report.findings.push(second_row("src/users.rs", 21));
    report.findings.push(second_row("src/orders.rs", 33));
    report
}

fn decision(id: &str, key: &str, state: DecisionState, reason: &str) -> Decision {
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state,
        reason: reason.to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: None,
        decided_by: "SECRET-REVIEWER".to_string(),
        decided_at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

fn package_of(reviewed: &ReviewedReport) -> ProductPackage {
    let options = client_options();
    let json = build_report_json(reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn check_problems(failure: &GateFailure, check: CheckId) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == check)
        .flat_map(|o| o.problems.iter())
        .collect()
}

/// The merge-and-group-decision problems the gate finds in the package made from `reviewed`,
/// read from the package's own files.
fn merge_problems(reviewed: &ReviewedReport) -> Vec<Problem> {
    match release(package_of(reviewed)) {
        Ok(_) => Vec::new(),
        Err(ReleaseError::Blocked(failure)) => {
            check_problems(&failure, CheckId::MergedWithoutDecision)
                .into_iter()
                .cloned()
                .collect()
        }
        Err(other) => panic!("the package must assemble: {other}"),
    }
}

fn at(report: &ScanReport, path: &str) -> Finding {
    report
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .clone()
}

// ── the two new checks ────────────────────────────────────────────────────────────────────

#[test]
fn release_gate_refuses_a_merged_row_without_a_decision_record() {
    // A row that exports as merged although no decision record names it (a mark with nothing
    // behind it) is a claim nobody made.
    let mut report = report_of_three();
    report.findings[1].captures.insert(
        MERGED_INTO_CAPTURE.to_string(),
        "some-target-key".to_string(),
    );
    report.findings[1].captures.insert(
        MERGED_LABEL_CAPTURE.to_string(),
        "Merged into src/db.rs:12".to_string(),
    );
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    match release(package_of(&reviewed)) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = check_problems(&failure, CheckId::MergedWithoutDecision);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            assert!(
                problems[0].location.contains("src/users.rs"),
                "{:?}",
                problems[0]
            );
        }
        other => panic!("a merged row with no decision record must block: {other:?}"),
    }

    // The same merge, made through a decision with a reason, clears the gate.
    let sealed = report_of_three().seal();
    let target = at(&sealed, "src/db.rs").site_key.clone();
    let merged = at(&sealed, "src/users.rs").site_key.clone();
    let merge = decision(
        "dec-000001",
        &merged,
        DecisionState::MergedInto(target.clone()),
        "same query helper",
    );
    let reviewed = apply_overlay(&sealed, std::slice::from_ref(&merge), &[], &[]);
    let released = release(package_of(&reviewed));
    assert!(released.is_ok(), "{:?}", released.err());

    // And a merge record with no reason does not count.
    let mut no_reason = merge;
    no_reason.reason = "  ".to_string();
    let rows = apply_overlay(&sealed, std::slice::from_ref(&no_reason), &[], &[]);
    let problems = merge_problems(&rows);
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn release_gate_refuses_a_node_decision_missing_a_member_record() {
    let sealed = report_of_three().seal();
    let keys: Vec<String> = sealed.findings.iter().map(|f| f.site_key.clone()).collect();
    let node = |id: &str| Decision {
        scope: Scope::Node("rule:SEC-NO-RAW-SQL-CONCAT-1".to_string()),
        applies_to: keys.clone(),
        ..decision(
            id,
            "rule:SEC-NO-RAW-SQL-CONCAT-1",
            DecisionState::Confirmed,
            "pattern",
        )
    };
    let member = |parent: &str, i: usize, key: &str| Decision {
        id: format!("{parent}.{i:05}"),
        inherited_from: Some(parent.to_string()),
        ..decision("x", key, DecisionState::Confirmed, "pattern")
    };

    // Every covered site has its record: clean.
    let mut whole = vec![node("dec-000001")];
    whole.extend(
        keys.iter()
            .enumerate()
            .map(|(i, k)| member("dec-000001", i, k)),
    );
    let reviewed = apply_overlay(&sealed, &whole, &[], &[]);
    assert!(merge_problems(&reviewed).is_empty());
    assert!(release(package_of(&reviewed)).is_ok());

    // One site's record is missing: the group decision names a site nothing holds a ruling for.
    let mut partial = vec![node("dec-000001")];
    partial.extend(
        keys.iter()
            .enumerate()
            .take(keys.len() - 1)
            .map(|(i, k)| member("dec-000001", i, k)),
    );
    let reviewed = apply_overlay(&sealed, &partial, &[], &[]);
    match release(package_of(&reviewed)) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = check_problems(&failure, CheckId::MergedWithoutDecision);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            assert!(
                problems[0].message.contains("no record of its own"),
                "{:?}",
                problems[0]
            );
        }
        other => panic!("a group decision missing a member's record must block: {other:?}"),
    }

    // An undo of one site of a batch is not a missing record: the record is there, revoked.
    let mut undone = whole;
    undone[1].revoked_at = Some(NOW.to_string());
    let reviewed = apply_overlay(&sealed, &undone, &[], &[]);
    assert!(merge_problems(&reviewed).is_empty());
}

#[test]
fn a_restored_row_without_a_ruling_blocks_a_client_package() {
    let sealed = report_of_three().seal();
    let mut marked = sealed.as_scan_report().clone();
    marked.findings[1].captures.insert(
        crate::engine_discard::ENGINE_RESTORED_CAPTURE.to_string(),
        "its headline is a placeholder".to_string(),
    );
    let key = marked.findings[1].site_key.clone();
    let undecided = marked.clone().seal();
    let reviewed = apply_overlay(&undecided, &[], &[], &[]);
    assert_eq!(
        restored_without_decision(&reviewed.report().findings, &reviewed).len(),
        1
    );
    match release(package_of(&reviewed)) {
        Err(ReleaseError::Blocked(failure)) => {
            assert_eq!(
                check_problems(&failure, CheckId::RestoredWithoutDecision).len(),
                1,
                "{:?}",
                failure.report
            );
        }
        other => panic!("an undecided restored row must block the release: {other:?}"),
    }
    let decided = apply_overlay(
        &undecided,
        &[
            decision(
                "dec-000001",
                &key,
                DecisionState::EngineRestored,
                "the engine was wrong",
            ),
            decision("dec-000002", &key, DecisionState::Confirmed, ""),
        ],
        &[],
        &[],
    );
    assert!(release(package_of(&decided)).is_ok());
}

// ── what an edit writes into a package ────────────────────────────────────────────────────

fn edit_of(n: usize, f: &Finding, field: EditField, old: &str, new: &str) -> Edit {
    Edit {
        id: format!("edit-{n:06}"),
        key: f.site_key.clone(),
        field,
        old: old.to_string(),
        new: new.to_string(),
        reason: "SECRET-REASON the wording was wrong".to_string(),
        by: "SECRET-REVIEWER".to_string(),
        at: NOW.to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

fn parse(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("json")
}

fn text_entries(zip: &[u8]) -> Vec<(String, String)> {
    read_zip_entries(zip)
        .expect("zip")
        .into_iter()
        .filter(|(name, _)| !name.ends_with(".pdf") && !name.ends_with(".xlsx"))
        .map(|(name, bytes)| (name, String::from_utf8_lossy(&bytes).into_owned()))
        .collect()
}

fn xlsx_text(zip: &[u8]) -> Vec<String> {
    let entries = read_zip_entries(zip).expect("zip");
    let (_, xlsx) = entries
        .iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .expect("a workbook");
    xlsx_strings(xlsx).expect("workbook strings")
}

#[test]
fn every_export_carries_reviewer_edited_and_the_edit_ids() {
    let sealed = report_of_three().seal();
    let edited = at(sealed.as_scan_report(), "src/users.rs");
    let edits = vec![
        edit_of(
            1,
            &edited,
            EditField::Headline,
            "Raw SQL",
            "Query built from input",
        ),
        edit_of(2, &edited, EditField::Severity, "high", "medium"),
    ];
    let reviewed = apply_overlay(&sealed, &[], &edits, &[]);
    let package = package_of(&reviewed);
    let data_json = package.data_json.clone();
    let findings_json = package.findings_json.clone();
    let zip = release(package).expect("the package clears the gate").bytes;
    let entries = read_zip_entries(&zip).unwrap();

    let row_of = |rows: &serde_json::Value, path: &str| -> serde_json::Value {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == path)
            .unwrap_or_else(|| panic!("no row for {path}"))
            .clone()
    };
    // findings.json and the JSON dataset.
    for (name, rows) in [
        ("findings.json", parse(&findings_json)["findings"].clone()),
        (
            "json/all-findings.json",
            parse(&entries["json/all-findings.json"]),
        ),
    ] {
        let edited_row = row_of(&rows, "src/users.rs");
        assert_eq!(edited_row["reviewer_edited"], true, "{name}");
        assert_eq!(
            edited_row["edit_ids"],
            serde_json::json!(["edit-000001", "edit-000002"]),
            "{name}"
        );
        let plain = row_of(&rows, "src/db.rs");
        assert_eq!(plain["reviewer_edited"], false, "{name}");
        assert_eq!(plain["edit_ids"], serde_json::json!([]), "{name}");
    }
    // The CSV dataset has both columns.
    let csv = String::from_utf8_lossy(&entries["csv/all-findings.csv"]).into_owned();
    let header = csv.lines().next().unwrap();
    assert!(
        header.contains("reviewer_edited") && header.contains("edit_ids"),
        "{header}"
    );
    assert!(csv.contains("edit-000001"), "{csv}");
    // The PDF's data.
    assert!(
        data_json.contains("\"reviewer_edited\":true"),
        "the PDF data"
    );
    assert!(data_json.contains("edit-000002"));
    // The workbook.
    let cells = xlsx_text(&zip);
    assert!(cells.iter().any(|c| c == "Reviewer edited"), "{cells:?}");
    assert!(cells.iter().any(|c| c.contains("edit-000001")), "{cells:?}");
}

#[test]
fn an_edit_never_reaches_client_output_as_internal_text() {
    let sealed = report_of_three().seal();
    let edited = at(sealed.as_scan_report(), "src/users.rs");
    let merged = at(sealed.as_scan_report(), "src/orders.rs");
    let target = at(sealed.as_scan_report(), "src/db.rs");
    // The name the reviewer gives a sub-group is the group's label and cause in the client
    // package (design D16: the sub-group is the unit of the plan); only the reason for the split
    // is internal.
    let mut split = edit_of(
        3,
        &edited,
        EditField::Subgroup,
        "",
        "Helper builds SQL by hand",
    );
    split.reason = "SECRET-SPLIT-REASON".to_string();
    let edits = vec![
        edit_of(
            1,
            &edited,
            EditField::Headline,
            "Raw SQL",
            "Query built from input",
        ),
        edit_of(2, &edited, EditField::Severity, "high", "medium"),
        split,
    ];
    let merge = decision(
        "dec-000001",
        &merged.site_key,
        DecisionState::MergedInto(target.site_key.clone()),
        "SECRET-MERGE-REASON",
    );
    let reviewed = apply_overlay(&sealed, std::slice::from_ref(&merge), &edits, &[]);
    let package = package_of(&reviewed);
    let data_json = package.data_json.clone();
    let zip = release(package).expect("the package clears the gate").bytes;

    // Reasons and reviewer names are internal.
    let secrets = [
        "SECRET-REASON",
        "SECRET-REVIEWER",
        "SECRET-SPLIT-REASON",
        "SECRET-MERGE-REASON",
    ];
    let mut outputs: Vec<(String, String)> = text_entries(&zip);
    outputs.push(("PDF data".to_string(), data_json));
    outputs.push(("workbook".to_string(), xlsx_text(&zip).join("\n")));
    for (name, text) in &outputs {
        for secret in secrets {
            assert!(!text.contains(secret), "{name} carries {secret}");
        }
    }
    // The edit ids are structured data, in the data files only: never in the README, the
    // manifest or any prose.
    for (name, text) in &outputs {
        let data_file = matches!(
            name.as_str(),
            "findings.json"
                | "json/all-findings.json"
                | "csv/all-findings.csv"
                | "PDF data"
                | "workbook"
        );
        if !data_file {
            assert!(!text.contains("edit-0000"), "{name} carries an edit id");
        }
    }
    // The merged row says only where it points.
    let findings =
        String::from_utf8_lossy(&read_zip_entries(&zip).unwrap()["findings.json"]).into_owned();
    assert!(findings.contains("Merged into src/db.rs:12"), "{findings}");
}

#[test]
fn each_editable_field_round_trips_into_the_exports() {
    let mut report = scan_report();
    report.findings[0].kind = None;
    report.findings[0].fix_specific = Some("Tailored fix text for this file.".to_string());
    report.findings[0].detail = "Raw SQL \u{2014} The query is built from the request.".to_string();
    let sealed = report.seal();
    let row = sealed.findings[0].clone();
    let cases: [(EditField, &str, &str); 8] = [
        (
            EditField::Headline,
            "Query assembled from user input",
            "headline",
        ),
        (
            EditField::Detail,
            "The reviewer's own explanation of the query.",
            "detail",
        ),
        (
            EditField::Fix,
            "Bind the parameters with the driver.",
            "fix_specific",
        ),
        (EditField::FixType, "Standard fix", "fix_type"),
        (EditField::Severity, "low", "severity"),
        (EditField::RuleId, "SEC-REVIEWER-RULE-7", "rule_id"),
        (EditField::Kind, "defect", "kind"),
        (EditField::Category, "secret-exposure", "category"),
    ];
    assert_eq!(
        cases.len() + 1,
        EditField::ALL.len(),
        "every editable field but the sub-group (a tree edit) has a case here"
    );
    for (n, (field, new, column)) in cases.iter().enumerate() {
        let edits = vec![edit_of(n + 1, &row, *field, "", new)];
        let reviewed = apply_overlay(&sealed, &[], &edits, &[]);
        let package = package_of(&reviewed);
        let findings = parse(&package.findings_json);
        let got = &findings["findings"][0];
        let want: serde_json::Value = match field {
            EditField::Category => {
                serde_json::json!(crate::report_export::category_display_label(new))
            }
            _ => serde_json::json!(new),
        };
        assert_eq!(
            got[column], want,
            "{field:?} did not reach findings.json: {got}"
        );
        assert_eq!(got["reviewer_edited"], true, "{field:?}");
        // Every other dataset agrees with findings.json (the gate's parity check).
        let released = release(package);
        assert!(released.is_ok(), "{field:?}: {:?}", released.err());
    }
    // A text edit also reaches the PDF's data.
    for (field, new) in [
        (EditField::Headline, "Query assembled from user input"),
        (
            EditField::Detail,
            "The reviewer's own explanation of the query.",
        ),
        (EditField::Fix, "Bind the parameters with the driver."),
    ] {
        let reviewed = apply_overlay(&sealed, &[], &[edit_of(1, &row, field, "", new)], &[]);
        let package = package_of(&reviewed);
        assert!(
            package.data_json.contains(new),
            "{field:?} is not in the PDF data"
        );
    }
    let _ = HashMap::<String, String>::new();
}
