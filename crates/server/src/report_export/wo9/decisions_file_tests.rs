//! WO9: the package's `decisions.json` and Decisions sheet, and the release gate reading them.

use camerata_api_types::decisions::DecisionState;
use serde_json::Value;

use super::support::*;
use crate::release_gate::{audit_zip, xlsx_sheet_grid, CheckId};
use crate::report_export::apply_overlay;

fn workbook(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("a workbook")
}

#[test]
fn the_package_carries_decisions_json_and_a_decisions_sheet() {
    let sealed = mixed_sealed();
    let (records, edits) = mixed_review(&sealed);
    let reviewed = apply_overlay(&sealed, &records, &edits, &[]);
    let entries = released_entries(&reviewed, &client_options());

    let file: Value = serde_json::from_slice(&entries["decisions.json"]).expect("decisions.json");
    let rows = file["decisions"].as_array().expect("a decisions array");
    // The group decision: its node record and one record for each site it covers; the false
    // positive.
    assert_eq!(rows.len(), 4, "{rows:?}");
    let node = rows
        .iter()
        .find(|r| r["scope"] == "node")
        .expect("the node record");
    assert_eq!(node["node_id"], "rule:SEC-NO-RAW-SQL-CONCAT-1");
    assert_eq!(node["state"], "confirmed");
    assert_eq!(node["applies_to"].as_array().unwrap().len(), 2);
    assert_eq!(node["has_reason"], true);
    let inherited: Vec<&Value> = rows
        .iter()
        .filter(|r| r["inherited_from"] == "dec-000001")
        .collect();
    assert_eq!(inherited.len(), 2);
    let fp = rows
        .iter()
        .find(|r| r["state"] == "false_positive")
        .expect("the false positive");
    assert_eq!(
        fp["site_id"],
        crate::report_export::site_id(&key_at(&sealed, "src/e.rs"))
    );
    // The reasons a client already sees (a false positive's, on its own sheet) are there; the
    // others are recorded as present and not printed; a reviewer's name never is.
    assert_eq!(fp["reason"], "the layer is allowed here");
    assert!(node["reason"].as_str().unwrap_or_default().is_empty());
    let text = String::from_utf8_lossy(&entries["decisions.json"]).into_owned();
    assert!(!text.contains("SECRET-REVIEWER"), "{text}");
    assert!(!text.contains("the same query helper"), "{text}");

    let grid = xlsx_sheet_grid(&workbook(&entries), "Reviewer decisions")
        .expect("a Reviewer decisions sheet");
    assert_eq!(grid.len() - 1, rows.len(), "one sheet row per record");
    assert!(grid[0].iter().any(|h| h == "Decision id"), "{:?}", grid[0]);
}

#[test]
fn merged_without_decision_reads_the_package_decisions_json() {
    let sealed = mixed_sealed();
    let target = key_at(&sealed, "src/a.rs");
    let merged = key_at(&sealed, "src/b.rs");
    let merge = decision(
        "dec-000001",
        &merged,
        DecisionState::MergedInto(target),
        "same query helper",
    );
    let reviewed = apply_overlay(&sealed, std::slice::from_ref(&merge), &[], &[]);
    let package = package_of(&reviewed, &client_options());
    // In memory the merge has its record. The package's own decisions.json is what the gate
    // reads, so a record struck from that file is a merge with no decision.

    let rebuilt = |datasets: Vec<(String, Vec<u8>)>| {
        crate::assemble_unreleased_zip(
            &package.stem,
            &package.pdf,
            &package.xlsx,
            &package.findings_json,
            &package.readme,
            &datasets,
            &package.provenance,
        )
        .expect("a zip")
    };
    let problems = |zip: &[u8]| -> usize {
        audit_zip(zip, None)
            .checks
            .iter()
            .filter(|o| o.check == CheckId::MergedWithoutDecision)
            .map(|o| o.problems.len())
            .sum()
    };
    assert_eq!(problems(&rebuilt(package.datasets.clone())), 0, "as built");

    // The record is struck from the file the client receives: the gate refuses the package.
    let mut stripped = package.datasets.clone();
    for (name, bytes) in &mut stripped {
        if name == "decisions.json" {
            let mut file: Value = serde_json::from_slice(bytes).unwrap();
            file["decisions"] = Value::Array(Vec::new());
            *bytes = serde_json::to_vec_pretty(&file).unwrap();
        }
    }
    assert_eq!(
        problems(&rebuilt(stripped)),
        1,
        "the merge has no record in the package"
    );

    // A record with no reason recorded does not count either.
    let mut unreasoned = package.datasets.clone();
    for (name, bytes) in &mut unreasoned {
        if name == "decisions.json" {
            let mut file: Value = serde_json::from_slice(bytes).unwrap();
            for row in file["decisions"].as_array_mut().unwrap() {
                row["has_reason"] = Value::Bool(false);
            }
            *bytes = serde_json::to_vec_pretty(&file).unwrap();
        }
    }
    assert_eq!(problems(&rebuilt(unreasoned)), 1, "the merge has no reason");
}

#[test]
fn a_revoked_false_positive_prints_no_reason() {
    // The reviewer undid the ruling: its reason is reviewer text for a ruling that no longer
    // stands, so the record stays in the file (the history) without it.
    let sealed = mixed_sealed();
    let mut fp = decision(
        "dec-000001",
        &key_at(&sealed, "src/e.rs"),
        DecisionState::FalsePositive,
        "REVOKED-REASON the layer is allowed here",
    );
    let live = decision(
        "dec-000002",
        &key_at(&sealed, "src/d.rs"),
        DecisionState::FalsePositive,
        "LIVE-REASON generated code",
    );
    fp.revoked_at = Some(NOW.to_string());
    let reviewed = apply_overlay(&sealed, &[fp, live], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());

    let file: Value = serde_json::from_slice(&entries["decisions.json"]).unwrap();
    let rows = file["decisions"].as_array().unwrap();
    let revoked = rows.iter().find(|r| r["id"] == "dec-000001").unwrap();
    assert_eq!(revoked["revoked"], true);
    assert_eq!(revoked["has_reason"], true);
    assert_eq!(revoked["reason"], "", "{revoked}");
    let live = rows.iter().find(|r| r["id"] == "dec-000002").unwrap();
    assert_eq!(live["reason"], "LIVE-REASON generated code");

    // And on no sheet of the workbook.
    let text = crate::release_gate::xlsx_strings(&workbook(&entries))
        .unwrap()
        .join("\n");
    assert!(!text.contains("REVOKED-REASON"), "the workbook prints it");
    assert!(
        text.contains("LIVE-REASON"),
        "a live ruling's reason prints"
    );
}
