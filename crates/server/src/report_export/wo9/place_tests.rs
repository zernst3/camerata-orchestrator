//! D2 (R-6a to R-6d): rows of different rules at one place are carried by the best-ranked row.
//! The machine files say who carries whom, All problems has one row per place headed by the
//! carrier, All source rows points each carried row at it, and the release gate refuses a carried
//! row whose carrier is missing or at another place. Synthetic fixtures only.

use std::collections::BTreeMap;

use super::support::*;
use super::workbook_tests::{cell_of, col_letter, column, links, table};
use crate::onboard::{Finding, SealedReport};
use crate::report_export::{apply_overlay, ReportOptions, ReviewedReport};
use crate::xlsx_export::vocabulary as v;

fn row(rule: &str, severity: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("use_value({rule})"),
        detail: format!("The value is used without a check, flagged by {rule}."),
        effort: Some("low".to_string()),
        category: Some("Authorization".to_string()),
        fix_specific: Some(format!("Check the value first ({rule}).")),
        located: true,
        ..Finding::default()
    }
}

/// Three rules at `src/pay.ts:12` (a critical, a high and a low row) and one row elsewhere.
fn placed_report() -> SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = vec![
        row("SEC-RULE-HIGH-1", "high", "src/pay.ts", 12),
        row("SEC-RULE-CRIT-1", "critical", "src/pay.ts", 12),
        row("SEC-RULE-LOW-1", "low", "src/pay.ts", 12),
        row("SEC-RULE-ELSE-1", "medium", "src/other.ts", 3),
    ];
    report.seal()
}

fn reviewed() -> ReviewedReport {
    apply_overlay(&placed_report(), &[], &[], &[])
}

fn options() -> ReportOptions {
    client_options()
}

fn entries() -> BTreeMap<String, Vec<u8>> {
    released_entries(&reviewed(), &options())
}

fn findings_json(entries: &BTreeMap<String, Vec<u8>>) -> serde_json::Value {
    serde_json::from_slice(&entries["findings.json"]).expect("findings.json")
}

fn text(row: &serde_json::Value, key: &str) -> String {
    row[key].as_str().unwrap_or("").to_string()
}

fn problems_text(problems: &[crate::release_gate::Problem]) -> String {
    problems
        .iter()
        .map(|p| format!("{}: {}", p.location, p.message))
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_findings(
    entries: &BTreeMap<String, Vec<u8>>,
    edit: impl FnOnce(&mut serde_json::Value),
) -> BTreeMap<String, Vec<u8>> {
    let mut findings = findings_json(entries);
    edit(&mut findings);
    let mut out = entries.clone();
    out.insert(
        "findings.json".into(),
        serde_json::to_vec(&findings).unwrap(),
    );
    out
}

#[test]
fn findings_json_marks_carried_rows_and_the_carrier_lists_them() {
    let entries = entries();
    let findings = findings_json(&entries);
    let rows = findings["findings"].as_array().unwrap();
    let at_pay: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["path"] == "src/pay.ts").collect();
    assert_eq!(at_pay.len(), 3, "every row stays");
    let carrier = at_pay
        .iter()
        .find(|r| text(r, "carried_by").is_empty())
        .expect("a carrier");
    assert_eq!(
        text(carrier, "rule_id"),
        "SEC-RULE-CRIT-1",
        "the best-ranked row"
    );
    let carrier_id = text(carrier, "site_id");
    let listed = text(carrier, "also_matches");
    for carried in at_pay
        .iter()
        .filter(|r| text(r, "carried_by") == carrier_id)
    {
        assert!(
            listed.contains(&text(carried, "rule_id")),
            "{listed} lists {}",
            text(carried, "rule_id")
        );
    }
    assert_eq!(
        at_pay
            .iter()
            .filter(|r| text(r, "carried_by") == carrier_id)
            .count(),
        2
    );
    // A row alone at its place is neither.
    let alone = rows.iter().find(|r| r["path"] == "src/other.ts").unwrap();
    assert!(text(alone, "carried_by").is_empty() && text(alone, "also_matches").is_empty());
    // The same field is a CSV column.
    let csv = String::from_utf8(entries["csv/all-findings.csv"].clone()).unwrap();
    assert!(csv.lines().next().unwrap().contains("carried_by"), "{csv}");
}

#[test]
fn all_problems_has_one_row_per_place_headed_by_the_carrier_and_all_source_rows_points_at_it() {
    let reviewed = reviewed();
    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &options()).expect("workbook");
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    assert_eq!(problems.len() - 1, 2, "two places, four rows: {problems:?}");
    let findings = findings_json(&released_entries(&reviewed, &options()));
    let rows = findings["findings"].as_array().unwrap();
    let carrier = rows
        .iter()
        .find(|r| r["path"] == "src/pay.ts" && text(r, "carried_by").is_empty())
        .unwrap();
    let carrier_id = text(carrier, "site_id");
    let at = (1..problems.len())
        .find(|r| cell_of(&problems, *r, v::H_PLACE_ID) == carrier_id)
        .expect("the place's row is headed by the carrier");
    // Every rule of the place is named once in the Rules and Rule ids cells.
    let rule_ids = cell_of(&problems, at, v::H_RULE_IDS);
    for rule in ["SEC-RULE-CRIT-1", "SEC-RULE-HIGH-1", "SEC-RULE-LOW-1"] {
        assert_eq!(rule_ids.matches(rule).count(), 1, "{rule} in {rule_ids}");
    }
    // Every carried row's source row links to that place's row.
    let sources = table(&xlsx, v::ALL_SOURCE_ROWS);
    let place_col = column(&sources, v::H_PLACE_ID).unwrap();
    let represented = col_letter(column(&sources, v::H_REPRESENTED_BY).unwrap());
    let mut checked = 0;
    for (cell, sheet, target) in links(&xlsx, v::ALL_SOURCE_ROWS) {
        let letters: String = cell.chars().take_while(char::is_ascii_alphabetic).collect();
        if letters != represented {
            continue;
        }
        let r = cell[letters.len()..].parse::<usize>().unwrap() - 3;
        let source_id = &sources[r][place_col];
        let carried = rows
            .iter()
            .any(|f| text(f, "site_id") == *source_id && text(f, "carried_by") == carrier_id);
        if carried {
            let grid = table(&xlsx, &sheet);
            assert_eq!(
                cell_of(&grid, target - 1, v::H_PLACE_ID),
                carrier_id,
                "the carried row {source_id} points at its carrier's row"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 2, "both carried rows");
}

#[test]
fn the_gate_refuses_a_carried_row_whose_carrier_is_missing_or_elsewhere_and_accepts_none() {
    let entries = entries();
    let clean = crate::release_gate::place_archive_problems(&entries);
    assert!(clean.is_empty(), "{}", problems_text(&clean));

    let carried_index = |f: &serde_json::Value| {
        f["findings"]
            .as_array()
            .unwrap()
            .iter()
            .position(|r| !text(r, "carried_by").is_empty())
            .expect("a carried row")
    };
    // A carrier that is no row of the file.
    let missing = with_findings(&entries, |f| {
        let i = carried_index(f);
        f["findings"][i]["carried_by"] = serde_json::json!("0000000000000000");
    });
    let problems = crate::release_gate::place_archive_problems(&missing);
    assert!(
        problems_text(&problems).contains("is not a row of findings.json"),
        "{problems:?}"
    );
    // A carrier at another place.
    let elsewhere = with_findings(&entries, |f| {
        let other = f["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == "src/other.ts")
            .map(|r| text(r, "site_id"))
            .unwrap();
        let i = carried_index(f);
        f["findings"][i]["carried_by"] = serde_json::json!(other);
    });
    let problems = crate::release_gate::place_archive_problems(&elsewhere);
    assert!(
        problems_text(&problems).contains("another place"),
        "{problems:?}"
    );
    // A carrier that does not list the rule it carries.
    let unlisted = with_findings(&entries, |f| {
        for r in f["findings"].as_array_mut().unwrap() {
            r["also_matches"] = serde_json::json!("");
        }
    });
    let problems = crate::release_gate::place_archive_problems(&unlisted);
    assert!(
        problems_text(&problems).contains("does not list the rule"),
        "{problems:?}"
    );
    // A package written before the field existed passes.
    let older = with_findings(&entries, |f| {
        for r in f["findings"].as_array_mut().unwrap() {
            r.as_object_mut().unwrap().remove("carried_by");
        }
    });
    let problems = crate::release_gate::place_archive_problems(&older);
    assert!(problems.is_empty(), "{}", problems_text(&problems));
    // A carrier that is itself carried: the chain has no head.
    let chained = with_findings(&entries, |f| {
        let rows = f["findings"].as_array_mut().unwrap();
        let carrier = rows
            .iter()
            .position(|r| r["path"] == "src/pay.ts" && text(r, "carried_by").is_empty())
            .unwrap();
        let carried = rows
            .iter()
            .position(|r| !text(r, "carried_by").is_empty())
            .unwrap();
        let carried_id = text(&rows[carried], "site_id");
        rows[carrier]["carried_by"] = serde_json::json!(carried_id);
    });
    let problems = crate::release_gate::place_archive_problems(&chained);
    assert!(
        problems_text(&problems).contains("itself carried"),
        "{problems:?}"
    );
}

#[test]
fn a_package_with_carried_rows_clears_every_other_check() {
    // `released_entries` panics unless the whole gate passes, MergedWithoutDecision and FixParity
    // included: no carried row gains a `merged_into`, and the Fix list still covers every row.
    let entries = entries();
    let clean = crate::release_gate::fix_archive_problems(&entries);
    assert!(clean.is_empty(), "{}", problems_text(&clean));
    let findings = findings_json(&entries);
    assert!(findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| text(r, "merged_into").is_empty()));
}

#[test]
fn a_place_is_as_severe_as_its_worst_rule_even_when_a_less_severe_row_carries_it() {
    // The critical row is held for review (needs a decision) and the high row is asserted, so the
    // asserted row ranks first and carries the place; the sheet, the machine files and the
    // scorecard still count the place at its worst severity.
    let mut report = crate::release_gate::test_scan_report();
    let mut held = row("SEC-RULE-HELD-1", "critical", "src/held.ts", 9);
    held.needs_review = true;
    held.confidence = Some("needs-review".to_string());
    report.findings = vec![held, row("SEC-RULE-SURE-1", "high", "src/held.ts", 9)];
    let reviewed = apply_overlay(&report.seal(), &[], &[], &[]);
    let entries = released_entries(&reviewed, &options());
    let findings = findings_json(&entries);
    let rows = findings["findings"].as_array().unwrap();
    let carrier = rows
        .iter()
        .find(|r| text(r, "carried_by").is_empty())
        .unwrap();
    assert_eq!(
        text(carrier, "rule_id"),
        "SEC-RULE-SURE-1",
        "the asserted row carries"
    );
    assert_eq!(text(carrier, "severity"), "high");

    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &options()).expect("workbook");
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    assert_eq!(problems.len() - 1, 1, "one place");
    assert_eq!(
        cell_of(&problems, 1, v::H_SEVERITY).to_lowercase(),
        "critical",
        "{problems:?}"
    );
    assert_eq!(
        cell_of(&problems, 1, v::H_PLACE_ID),
        text(carrier, "site_id")
    );
}

// ── the review's six fixes ────────────────────────────────────────────────────────────────

fn rezip(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(name.clone(), options).expect("start");
        writer.write_all(bytes).expect("write");
    }
    writer.finish().expect("finish").into_inner()
}

fn site_id_of(entries: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    let findings = findings_json(entries);
    let row = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .unwrap();
    text(row, "site_id")
}

#[test]
fn a_place_reads_as_a_defect_when_a_departure_carries_one() {
    // The held critical defect ranks below the asserted high departure, so the departure carries
    // the place; the place is still a defect, at critical, everywhere it is counted.
    let mut report = crate::release_gate::test_scan_report();
    let mut defect = row("SEC-RULE-DEFECT-1", "critical", "src/dev.ts", 4);
    defect.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    defect.needs_review = true;
    defect.confidence = Some("needs-review".to_string());
    let mut departure = row("SEC-RULE-DEPART-1", "high", "src/dev.ts", 4);
    departure.kind = Some(camerata_api_types::audit::FindingKind::Deviation(
        camerata_api_types::audit::DeviationKind {
            deviating_sites: 1,
            assessed_sites: Some(4),
            convention_option_id: Some("o".to_string()),
            convention_label: "The usual way".to_string(),
        },
    ));
    report.findings = vec![defect, departure];
    let reviewed = apply_overlay(&report.seal(), &[], &[], &[]);
    let entries = released_entries(&reviewed, &options());
    let findings = findings_json(&entries);
    let rows = findings["findings"].as_array().unwrap();
    let carrier = rows
        .iter()
        .find(|r| text(r, "carried_by").is_empty())
        .unwrap();
    assert_eq!(
        text(carrier, "rule_id"),
        "SEC-RULE-DEPART-1",
        "the departure carries"
    );

    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &options()).expect("workbook");
    let problems = table(&xlsx, v::ALL_PROBLEMS);
    assert_eq!(problems.len() - 1, 1);
    assert_eq!(cell_of(&problems, 1, v::H_KIND).to_lowercase(), "defect");
    assert_eq!(
        cell_of(&problems, 1, v::H_SEVERITY).to_lowercase(),
        "critical"
    );
}

#[test]
fn audit_zip_refuses_a_carried_row_whose_carrier_is_at_another_place() {
    let entries = entries();
    let other = site_id_of(&entries, "src/other.ts");
    let tampered = with_findings(&entries, |f| {
        let i = f["findings"]
            .as_array()
            .unwrap()
            .iter()
            .position(|r| !text(r, "carried_by").is_empty())
            .unwrap();
        f["findings"][i]["carried_by"] = serde_json::json!(other);
    });
    let report = crate::release_gate::audit_zip(&rezip(&tampered), None);
    let fix_parity: Vec<String> = report
        .checks
        .iter()
        .filter(|c| c.check == crate::release_gate::CheckId::FixParity)
        .flat_map(|c| c.problems.iter().map(|p| p.message.clone()))
        .collect();
    assert!(
        fix_parity
            .iter()
            .any(|m| m.contains("a row at another place")),
        "{fix_parity:?}"
    );
}

#[test]
fn the_gate_refuses_an_all_problems_sheet_with_fewer_rows_than_places() {
    let entries = entries();
    let moved = with_findings(&entries, |f| {
        let i = f["findings"]
            .as_array()
            .unwrap()
            .iter()
            .position(|r| !text(r, "carried_by").is_empty())
            .unwrap();
        f["findings"][i]["line"] = serde_json::json!(13);
    });
    let problems = crate::release_gate::place_archive_problems(&moved);
    assert!(
        problems_text(&problems).contains("has 2 rows and findings.json has 3 places"),
        "{}",
        problems_text(&problems)
    );
}

#[test]
fn the_gate_refuses_an_all_problems_row_headed_by_a_carried_row() {
    let entries = entries();
    let swapped = with_findings(&entries, |f| {
        let rows = f["findings"].as_array_mut().unwrap();
        let carrier = rows
            .iter()
            .position(|r| r["path"] == "src/pay.ts" && text(r, "carried_by").is_empty())
            .unwrap();
        let carried = rows
            .iter()
            .position(|r| !text(r, "carried_by").is_empty())
            .unwrap();
        let (carrier_id, carrier_rule) = (
            text(&rows[carrier], "site_id"),
            text(&rows[carrier], "rule_id"),
        );
        let carried_id = text(&rows[carried], "site_id");
        rows[carrier]["carried_by"] = serde_json::json!(carried_id);
        rows[carried]["carried_by"] = serde_json::json!("");
        rows[carried]["also_matches"] = serde_json::json!(carrier_rule);
        let _ = carrier_id;
    });
    let problems = crate::release_gate::place_archive_problems(&swapped);
    assert!(
        problems_text(&problems).contains("heads a place with"),
        "{}",
        problems_text(&problems)
    );
}

#[test]
fn a_false_positive_neither_carries_nor_is_carried() {
    let sealed = placed_report();
    let crit = sealed
        .findings
        .iter()
        .find(|f| f.rule_id == "SEC-RULE-CRIT-1")
        .unwrap()
        .site_key
        .clone();
    let fp = decision(
        "d-fp",
        &crit,
        camerata_api_types::decisions::DecisionState::FalsePositive,
        "not real",
    );
    let reviewed = apply_overlay(&sealed, &[fp], &[], &[]);
    let entries = released_entries(&reviewed, &options());
    let findings = findings_json(&entries);
    let rows = findings["findings"].as_array().unwrap();
    let ruled_out: serde_json::Value =
        serde_json::from_slice(&entries["json/false-positives.json"]).expect("false-positives");
    let fp_row = ruled_out
        .as_array()
        .unwrap()
        .iter()
        .find(|r| text(r, "rule_id") == "SEC-RULE-CRIT-1")
        .expect("the ruled-out row is in the false-positives file");
    assert!(text(fp_row, "carried_by").is_empty() && text(fp_row, "also_matches").is_empty());
    let head = rows
        .iter()
        .find(|r| r["path"] == "src/pay.ts" && !text(r, "also_matches").is_empty())
        .expect("the high row heads the place");
    assert_eq!(text(head, "rule_id"), "SEC-RULE-HIGH-1");
    assert!(text(head, "also_matches").contains("SEC-RULE-LOW-1"));
    assert!(crate::release_gate::place_archive_problems(&entries).is_empty());

    let fp_id = text(fp_row, "site_id");
    assert!(!fp_id.is_empty());
    let bad = with_findings(&entries, |f| {
        for r in f["findings"].as_array_mut().unwrap() {
            if text(r, "rule_id") == "SEC-RULE-LOW-1" {
                r["carried_by"] = serde_json::json!(fp_id);
            }
        }
    });
    let problems = crate::release_gate::place_archive_problems(&bad);
    assert!(
        problems_text(&problems).contains("a false positive carries"),
        "{}",
        problems_text(&problems)
    );
}
