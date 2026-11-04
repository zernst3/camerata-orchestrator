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
    let pdf_data = package.data_json.clone();
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

    // WO9: the new outputs are held to the same rule one by one. The name the reviewer gave the
    // sub-group is the group's label and its cause on the plan, the tree, the tickets and in the
    // PDF's patterns; the reason for the split, the merge reason and the reviewer's name are in
    // none of them, and the decisions file records only that a reason exists.
    let entries = read_zip_entries(&zip).unwrap();
    let xlsx = entries
        .iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .map(|(_, bytes)| bytes.clone())
        .unwrap();
    let sheet = |name: &str| -> String {
        xlsx_sheet_grid(&xlsx, name)
            .unwrap_or_else(|e| panic!("no {name} sheet: {e}"))
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    };
    let label = "Helper builds SQL by hand";
    let new_outputs: Vec<(&str, String)> = vec![
        ("Remediation plan sheet", sheet("Remediation plan")),
        ("Tree sheet", sheet("Tree")),
        ("Decisions sheet", sheet("Decisions")),
        ("Sites sheet", sheet("Sites")),
        (
            "csv/tickets.csv",
            String::from_utf8_lossy(&entries["csv/tickets.csv"]).into_owned(),
        ),
        (
            "decisions.json",
            String::from_utf8_lossy(&entries["decisions.json"]).into_owned(),
        ),
        ("PDF patterns", pdf_data.clone()),
    ];
    for (name, text) in &new_outputs {
        for secret in secrets {
            assert!(!text.contains(secret), "{name} carries {secret}");
        }
    }
    for name in [
        "Remediation plan sheet",
        "Tree sheet",
        "csv/tickets.csv",
        "PDF patterns",
    ] {
        let text = &new_outputs.iter().find(|(n, _)| *n == name).unwrap().1;
        assert!(
            text.contains(label),
            "{name} does not carry the sub-group's name"
        );
    }
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

// ── the merge check refuses when it cannot read its evidence ───────────────────────────────

/// The entries of a package that clears the gate.
fn clean_entries() -> std::collections::BTreeMap<String, Vec<u8>> {
    let reviewed = apply_overlay(&report_of_three().seal(), &[], &[], &[]);
    let zip = release(package_of(&reviewed))
        .expect("the package clears the gate")
        .bytes;
    read_zip_entries(&zip).expect("zip")
}

#[test]
fn the_merge_check_refuses_a_package_whose_findings_or_decisions_file_is_unreadable() {
    use super::held::merged_without_decision_in_archive;
    // A clean package raises nothing.
    assert!(merged_without_decision_in_archive(&clean_entries()).is_empty());

    for file in ["findings.json", "decisions.json"] {
        let key = if file == "findings.json" {
            "findings"
        } else {
            "decisions"
        };
        let mutations: [(&str, Option<Vec<u8>>); 4] = [
            ("absent", None),
            ("corrupt", Some(b"{ not json".to_vec())),
            (
                "not an array",
                Some(format!("{{\"{key}\": \"nope\"}}").into_bytes()),
            ),
            ("key missing", Some(b"{}".to_vec())),
        ];
        for (how, replacement) in mutations {
            let mut entries = clean_entries();
            match replacement {
                None => {
                    entries.remove(file);
                }
                Some(bytes) => {
                    entries.insert(file.to_string(), bytes);
                }
            }
            let problems = merged_without_decision_in_archive(&entries);
            assert!(
                problems.iter().any(|p| p.file == file),
                "{file} {how}: the merge check must refuse and name the file, got {problems:?}"
            );
        }
    }
}

// ── the review screen's export panel lists exactly what the gate refuses (WO9c) ───────────

/// The (path, line) a gate problem's location names ("src/users.rs line 21").
fn located(p: &Problem) -> (String, usize) {
    let (path, line) = p
        .location
        .rsplit_once(" line ")
        .unwrap_or_else(|| panic!("not a row location: {}", p.location));
    (path.to_string(), line.parse().expect("a line number"))
}

fn sorted(mut v: Vec<(String, usize)>) -> Vec<(String, usize)> {
    v.sort();
    v
}

/// The rows of the view for one reason, as (path, line).
fn view_rows(rows: &[camerata_api_types::review_view::HeldRow]) -> Vec<(String, usize)> {
    sorted(rows.iter().map(|h| (h.path.clone(), h.line)).collect())
}

fn gate_problems(reviewed: &ReviewedReport) -> GateFailure {
    match release(package_of(reviewed)) {
        Err(ReleaseError::Blocked(failure)) => failure,
        other => panic!("the package must be refused: {other:?}"),
    }
}

#[test]
fn export_gate_view_equals_release_gate_refusals() {
    use camerata_api_types::review_view::{ExportGateView, HoldReason};

    // Six rows, each held for a different reason the gate refuses: evidence not verified, junk
    // text, a merge with no decision behind it, a restored row nobody has ruled on, a site the
    // Engine Check said No to that nothing has corrected, and a site a group decision covers
    // without writing its record.
    let mut report = report_of_three();
    report.findings.push(second_row("src/cart.rs", 7));
    report.findings.push(second_row("src/wishlist.rs", 9));
    report.findings.push(second_row("src/profile.rs", 3));
    let row = |report: &mut ScanReport, path: &str| -> usize {
        report
            .findings
            .iter()
            .position(|f| f.path == path)
            .unwrap_or_else(|| panic!("no row in {path}"))
    };
    let (db, users, orders, cart) = (
        row(&mut report, "src/db.rs"),
        row(&mut report, "src/users.rs"),
        row(&mut report, "src/orders.rs"),
        row(&mut report, "src/cart.rs"),
    );
    report.findings[db].captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found near the cited line".to_string(),
    );
    report.findings[users].detail = "A title \u{2014} I cannot assist.".to_string();
    report.findings[orders].captures.insert(
        MERGED_INTO_CAPTURE.to_string(),
        "some-target-key".to_string(),
    );
    report.findings[orders].captures.insert(
        MERGED_LABEL_CAPTURE.to_string(),
        "Merged into src/db.rs:12".to_string(),
    );
    report.findings[cart].captures.insert(
        crate::engine_discard::ENGINE_RESTORED_CAPTURE.to_string(),
        "its headline is a placeholder".to_string(),
    );
    let sealed = report.seal();
    let key_of = |path: &str| at(&sealed, path).site_key;
    let wishlist = key_of("src/wishlist.rs");
    let node = Decision {
        scope: Scope::Node("rule:SEC-NO-RAW-SQL-CONCAT-1".to_string()),
        applies_to: vec![wishlist.clone()],
        ..decision(
            "dec-000001",
            "rule:SEC-NO-RAW-SQL-CONCAT-1",
            DecisionState::Confirmed,
            "pattern",
        )
    };
    let profile = key_of("src/profile.rs");
    let reviewed = apply_overlay(&sealed, std::slice::from_ref(&node), &[], &[])
        .with_engine_check(recorded(vec![no_verdict(&profile)], Vec::new()));

    let view = crate::release_gate::holds::gate_holds(&reviewed);
    let failure = gate_problems(&reviewed);

    // Row by row, kind by kind: the view lists the rows each check refuses and no others.
    let located_in = |check: CheckId, file: &str| {
        sorted(
            check_problems(&failure, check)
                .into_iter()
                .filter(|p| p.file == file)
                .map(located)
                .collect(),
        )
    };
    let kind = |reason: HoldReason| {
        view_rows(
            &view
                .held
                .iter()
                .filter(|h| h.reason == reason)
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        kind(HoldReason::EvidenceUnverified),
        located_in(CheckId::EvidenceUnverified, "findings.json")
    );
    assert_eq!(
        kind(HoldReason::JunkContent),
        located_in(CheckId::Content, "findings.json")
    );
    assert_eq!(
        kind(HoldReason::RestoredWithoutDecision),
        located_in(CheckId::RestoredWithoutDecision, "findings.json")
    );
    assert_eq!(
        kind(HoldReason::EngineCheckNoWithoutCorrection),
        located_in(CheckId::EngineCheckNoWithoutCorrection, "findings.json")
    );
    assert_eq!(
        view_rows(&view.merged_without_decision),
        located_in(CheckId::MergedWithoutDecision, "findings.json")
    );
    assert!(view
        .merged_without_decision
        .iter()
        .all(|h| h.reason == HoldReason::MergedWithoutDecision));
    // Each kind found its one row (the fixture holds one of each), so the equalities above are
    // not empty ones.
    for (what, rows) in [
        ("evidence", kind(HoldReason::EvidenceUnverified)),
        ("junk", kind(HoldReason::JunkContent)),
        ("restored", kind(HoldReason::RestoredWithoutDecision)),
        (
            "engine check",
            kind(HoldReason::EngineCheckNoWithoutCorrection),
        ),
        ("merged", view_rows(&view.merged_without_decision)),
    ] {
        assert_eq!(rows.len(), 1, "{what}: {rows:?}");
    }

    // The group decision's gap: one problem per site, one gap entry naming the same site.
    let gap_problems = check_problems(&failure, CheckId::MergedWithoutDecision)
        .into_iter()
        .filter(|p| p.file == crate::report_export::decisions_file::DECISIONS_FILE)
        .count();
    let gap_sites: usize = view
        .node_decisions_missing_members
        .iter()
        .map(|g| g.missing.len())
        .sum();
    assert_eq!((gap_problems, gap_sites), (1, 1));
    assert_eq!(
        view.node_decisions_missing_members[0].missing,
        vec![wishlist.clone()]
    );

    // And nothing else: every problem the five checks raise is a row or a gap in the view.
    let refused: usize = [
        CheckId::EvidenceUnverified,
        CheckId::Content,
        CheckId::RestoredWithoutDecision,
        CheckId::EngineCheckNoWithoutCorrection,
        CheckId::MergedWithoutDecision,
    ]
    .iter()
    .map(|c| check_problems(&failure, *c).len())
    .sum();
    assert_eq!(
        refused,
        view.held.len() + view.merged_without_decision.len() + gap_sites
    );

    // Rule on every one of them: the view empties and the gate lets the package through.
    let mut records = vec![node];
    records.push(Decision {
        inherited_from: Some("dec-000001".to_string()),
        ..decision(
            "dec-000001.00000",
            &wishlist,
            DecisionState::Confirmed,
            "pattern",
        )
    });
    let sealed = {
        let mut report = report_of_three();
        report.findings.push(second_row("src/cart.rs", 7));
        report.findings.push(second_row("src/wishlist.rs", 9));
        report.findings.push(second_row("src/profile.rs", 3));
        let db = row(&mut report, "src/db.rs");
        let users = row(&mut report, "src/users.rs");
        let cart = row(&mut report, "src/cart.rs");
        report.findings[db].captures.insert(
            crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
            "not found near the cited line".to_string(),
        );
        report.findings[users].detail = "A title \u{2014} I cannot assist.".to_string();
        report.findings[cart].captures.insert(
            crate::engine_discard::ENGINE_RESTORED_CAPTURE.to_string(),
            "its headline is a placeholder".to_string(),
        );
        report.seal()
    };
    let key = |path: &str| at(&sealed, path).site_key;
    let (db, users, orders, cart, profile) = (
        key("src/db.rs"),
        key("src/users.rs"),
        key("src/orders.rs"),
        key("src/cart.rs"),
        key("src/profile.rs"),
    );
    for (i, k) in [&db, &users, &cart, &profile].into_iter().enumerate() {
        records.push(decision(
            &format!("dec-1{i:05}"),
            k,
            DecisionState::Confirmed,
            "",
        ));
    }
    records.push(decision(
        "dec-200000",
        &orders,
        DecisionState::MergedInto(db.clone()),
        "same query helper",
    ));
    // The decision on the profile row is later than the Engine Check's No: it is the correction.
    let reviewed = apply_overlay(&sealed, &records, &[], &[])
        .with_engine_check(recorded(vec![no_verdict(&profile)], Vec::new()));
    assert_eq!(
        crate::release_gate::holds::gate_holds(&reviewed),
        ExportGateView::default()
    );
    assert!(release(package_of(&reviewed)).is_ok());
}

// ── the Engine Check's No holds the row until it is corrected (design A2b) ────────────────

/// A time before every decision and edit the tests make (`NOW`).
const VERDICT_AT: &str = "2026-11-04T08:00:00Z";
const CHECK_RUN: &str = "insp-1";

fn no_verdict(key: &str) -> camerata_api_types::review_view::EngineCheckVerdict {
    camerata_api_types::review_view::EngineCheckVerdict {
        key: key.to_string(),
        run_id: CHECK_RUN.to_string(),
        right: false,
        wrong: Some(camerata_api_types::review_view::EngineCheckError::WrongSeverity),
        correction: None,
        by: "reviewer".to_string(),
        at: VERDICT_AT.to_string(),
    }
}

fn recorded(
    verdicts: Vec<camerata_api_types::review_view::EngineCheckVerdict>,
    edits: Vec<Edit>,
) -> crate::engine_check::EngineCheckEvidence {
    crate::engine_check::EngineCheckEvidence::Recorded {
        run_id: CHECK_RUN.to_string(),
        verdicts,
        edits,
    }
}

fn engine_check_problems(reviewed: &ReviewedReport) -> Vec<Problem> {
    match release(package_of(reviewed)) {
        Ok(_) => Vec::new(),
        Err(ReleaseError::Blocked(failure)) => {
            check_problems(&failure, CheckId::EngineCheckNoWithoutCorrection)
                .into_iter()
                .cloned()
                .collect()
        }
        Err(other) => panic!("the package must assemble: {other}"),
    }
}

#[test]
fn the_gate_refuses_an_engine_check_no_without_a_correction_and_names_the_site() {
    let sealed = report_of_three().seal();
    let site = at(&sealed, "src/users.rs");
    let reviewed = apply_overlay(&sealed, &[], &[], &[])
        .with_engine_check(recorded(vec![no_verdict(&site.site_key)], Vec::new()));
    let problems = engine_check_problems(&reviewed);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].location, "src/users.rs line 21");
    assert_eq!(problems[0].evidence.as_deref(), Some(site.rule_id.as_str()));
    // A Yes on the same site holds nothing.
    let yes = camerata_api_types::review_view::EngineCheckVerdict {
        right: true,
        wrong: None,
        ..no_verdict(&site.site_key)
    };
    let reviewed =
        apply_overlay(&sealed, &[], &[], &[]).with_engine_check(recorded(vec![yes], Vec::new()));
    assert!(engine_check_problems(&reviewed).is_empty());
    // And a store nobody consulted holds nothing (the headless export has no store).
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    assert!(engine_check_problems(&reviewed).is_empty());
}

#[test]
fn a_correction_clears_an_engine_check_no_at_the_gate() {
    let sealed = report_of_three().seal();
    let site = at(&sealed, "src/users.rs");
    let key = site.site_key.clone();

    // A decision made after the verdict.
    let reviewed = apply_overlay(
        &sealed,
        &[decision(
            "dec-000001",
            &key,
            DecisionState::FalsePositive,
            "",
        )],
        &[],
        &[],
    )
    .with_engine_check(recorded(vec![no_verdict(&key)], Vec::new()));
    assert!(engine_check_problems(&reviewed).is_empty());

    // An edit made after the verdict.
    let edit = edit_of(1, &site, EditField::Fix, "", "Bind the parameters.");
    let reviewed = apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[])
        .with_engine_check(recorded(vec![no_verdict(&key)], vec![edit]));
    assert!(engine_check_problems(&reviewed).is_empty());

    // A correction the verdict names.
    let named = camerata_api_types::review_view::EngineCheckVerdict {
        correction: Some(camerata_api_types::review_view::CorrectionRef::Decision(
            "dec-000001".to_string(),
        )),
        ..no_verdict(&key)
    };
    let reviewed = apply_overlay(
        &sealed,
        &[decision(
            "dec-000001",
            &key,
            DecisionState::FalsePositive,
            "",
        )],
        &[],
        &[],
    )
    .with_engine_check(recorded(vec![named], Vec::new()));
    assert!(engine_check_problems(&reviewed).is_empty());

    // A decision made BEFORE the verdict is not a correction of it.
    let earlier = Decision {
        decided_at: "2026-11-04T07:00:00Z".to_string(),
        ..decision("dec-000001", &key, DecisionState::FalsePositive, "")
    };
    let reviewed = apply_overlay(&sealed, &[earlier], &[], &[])
        .with_engine_check(recorded(vec![no_verdict(&key)], Vec::new()));
    assert_eq!(engine_check_problems(&reviewed).len(), 1);
}

#[test]
fn the_gate_refuses_when_the_engine_check_file_cannot_be_read() {
    use crate::engine_check::EngineCheckStore;
    use camerata_api_types::review_view::{EngineCheckError, EngineCheckVerdictReq};

    let sealed = report_of_three().seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let dir = tempfile::tempdir().unwrap();
    let store = EngineCheckStore::at(dir.path().to_path_buf());
    // A Yes only: the file exists and holds no No, so a readable store refuses nothing.
    let key = at(&sealed, "src/users.rs").site_key;
    let yes = EngineCheckVerdictReq {
        right: true,
        wrong: None,
        correction: None,
        by: String::new(),
    };
    store
        .record("p", &key, &yes, CHECK_RUN, VERDICT_AT)
        .unwrap();
    let readable =
        reviewed
            .clone()
            .with_engine_check(store.evidence_for("p", CHECK_RUN, Vec::new()));
    assert!(engine_check_problems(&readable).is_empty());

    // The file is damaged after the store opened: the gate reads it as it is now.
    let path = store.file_for("p").expect("a file-backed store");
    std::fs::write(&path, b"{ not json").unwrap();
    let damaged =
        reviewed
            .clone()
            .with_engine_check(store.evidence_for("p", CHECK_RUN, Vec::new()));
    let problems = engine_check_problems(&damaged);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].file.ends_with("p.json"),
        "the problem names the file: {:?}",
        problems[0]
    );

    // And the file the store moved aside at open (a restart over a damaged file) refuses too.
    let reopened = EngineCheckStore::at(dir.path().to_path_buf());
    assert!(reopened.is_corrupt("p"));
    let moved_aside =
        reviewed
            .clone()
            .with_engine_check(reopened.evidence_for("p", CHECK_RUN, Vec::new()));
    let problems = engine_check_problems(&moved_aside);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].file.ends_with("p.json"), "{:?}", problems[0]);

    // The row view lists no row for it (the refusal is the file's, not a site's).
    let view = crate::release_gate::holds::gate_holds(&moved_aside);
    assert!(view.held.is_empty());
    let _ = EngineCheckError::NotARealIssue;
}
