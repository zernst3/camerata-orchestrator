//! WO9: the tree in `findings.json` and on the Tree sheet, and the prose checks every summary
//! passes before client output.

use serde_json::Value;

use super::support::*;
use crate::release_gate::{read_zip_entries, release_internal, xlsx_sheet_grid};
use crate::report_export::apply_overlay;

fn workbook(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("a workbook")
}

fn with_summary(
    summary: &str,
    source: crate::review_tree::TextSource,
) -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    let tree = report.tree.as_mut().expect("a tree");
    let node = tree
        .nodes
        .iter_mut()
        .find(|n| n.id == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
        .expect("the SQL sub-group");
    node.summary = Some(summary.to_string());
    node.summary_source = source;
    node.cause = Some("the query is built from a request value".to_string());
    report.seal()
}

/// The SQL sub-group with `label` and `cause` as the group reviewer wrote them.
fn with_label_and_cause(label: &str, cause: &str) -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    let tree = report.tree.as_mut().expect("a tree");
    let node = tree
        .nodes
        .iter_mut()
        .find(|n| n.id == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
        .expect("the SQL sub-group");
    node.label = label.to_string();
    node.cause = Some(cause.to_string());
    report.seal()
}

/// Every client-visible text of a released package: data.json, each text entry, every workbook
/// string.
fn client_text_of(package: crate::release_gate::ProductPackage) -> (String, Value) {
    let mut text = package.data_json.clone();
    let entries = read_zip_entries(
        &crate::release_gate::release(package)
            .expect("a flagged text is a flag, not a hold")
            .bytes,
    )
    .unwrap();
    for (name, bytes) in &entries {
        if name.ends_with(".xlsx") {
            text.push_str(&crate::release_gate::xlsx_strings(bytes).unwrap().join("\n"));
        } else if !name.ends_with(".pdf") {
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    let findings = serde_json::from_slice(&entries["findings.json"]).unwrap();
    (text, findings)
}

#[test]
fn findings_json_carries_the_tree_with_counts_from_the_rows() {
    let sealed = with_summary(
        "Three handlers build SQL from request values.",
        crate::review_tree::TextSource::Model,
    );
    let (records, edits) = mixed_review(&sealed);
    let reviewed = apply_overlay(&sealed, &records, &edits, &[]);
    let entries = released_entries(&reviewed, &client_options());
    let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    let nodes = findings["tree"]["nodes"]
        .as_array()
        .expect("a tree in findings.json");
    let sub = nodes
        .iter()
        .find(|n| n["id"] == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
        .expect("the sub-group");
    assert_eq!(sub["level"], "subgroup");
    assert_eq!(sub["parent"], "rule:SEC-NO-RAW-SQL-CONCAT-1");
    assert_eq!(
        sub["summary"],
        "Three handlers build SQL from request values."
    );
    assert_eq!(sub["cause"], "the query is built from a request value");
    assert_eq!(sub["sites"], 3);
    assert_eq!(sub["review_levels"]["personally_reviewed"], 1);
    assert_eq!(
        sub["review_levels"]["reviewed_as_pattern:rule:SEC-NO-RAW-SQL-CONCAT-1"],
        2
    );
    // The rule node above it counts the same sites; parents come before children.
    let at = |id: &str| nodes.iter().position(|n| n["id"] == id).unwrap();
    assert!(at("rule:SEC-NO-RAW-SQL-CONCAT-1") < at("sub:SEC-NO-RAW-SQL-CONCAT-1/0"));
    assert_eq!(nodes[at("rule:SEC-NO-RAW-SQL-CONCAT-1")]["sites"], 3);
    // The architect's triage of a node is not in a client file.
    assert!(sub.get("internal").is_none(), "{sub}");
    assert!(sub.get("flags").is_none(), "{sub}");
    // The Tree sheet has the same nodes.
    let grid = xlsx_sheet_grid(&workbook(&entries), "How problems were grouped")
        .expect("a How problems were grouped sheet");
    assert_eq!(grid.len() - 1, nodes.len(), "one row per node");
    for header in [
        "Group id",
        "Within",
        "Group",
        "Parent group id",
        "Cause",
        "Summary",
        "Places",
    ] {
        assert!(
            grid[0].iter().any(|h| h == header),
            "{header}: {:?}",
            grid[0]
        );
    }
    assert!(!grid[0].iter().any(|h| h == "Review flags"), "client sheet");
}

#[test]
fn a_summary_with_a_methodology_leak_is_flagged_before_client_output() {
    for source in [
        crate::review_tree::TextSource::Model,
        crate::review_tree::TextSource::Reviewer,
    ] {
        let sealed = with_summary(
            "LEAK-MARKER the scanner flagged these as regex-scan hits.",
            source,
        );
        let reviewed = apply_overlay(&sealed, &[], &[], &[]);

        // A client package: the export is not held, and no file carries the text.
        let client = client_options();
        let package = package_of(&reviewed, &client);
        let data = package.data_json.clone();
        let entries = read_zip_entries(
            &crate::release_gate::release(package)
                .expect("a flagged summary is a flag, not a hold")
                .bytes,
        )
        .unwrap();
        let mut text = data;
        for (name, bytes) in &entries {
            if name.ends_with(".xlsx") {
                text.push_str(&crate::release_gate::xlsx_strings(bytes).unwrap().join("\n"));
            } else if !name.ends_with(".pdf") {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
        }
        assert!(
            !text.contains("LEAK-MARKER"),
            "{source:?}: a client file carries the text"
        );
        let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
        let sub = findings["tree"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
            .unwrap()
            .clone();
        assert_eq!(sub["summary"], "", "{source:?}: the summary is withheld");

        // The architect's package shows the flag beside the node.
        let internal = crate::report_export::ReportOptions {
            internal: true,
            ..client_options()
        };
        let zip = release_internal(package_of(&reviewed, &internal))
            .unwrap()
            .bytes;
        let entries = read_zip_entries(&zip).unwrap();
        let grid = xlsx_sheet_grid(&workbook(&entries), "How problems were grouped").unwrap();
        let flags_col = grid[0]
            .iter()
            .position(|h| h == "Review flags")
            .expect("the internal Tree sheet has a Review flags column");
        let id_col = grid[0]
            .iter()
            .position(|h| h == "Group id")
            .expect("the Group id column");
        let row = grid
            .iter()
            .find(|r| r[id_col] == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
            .unwrap();
        assert!(
            row[flags_col].contains("methodology_leak"),
            "{source:?}: {:?}",
            row[flags_col]
        );
    }
}

#[test]
fn a_label_or_cause_with_a_methodology_leak_reaches_no_row_and_no_sheet() {
    // The marker rides on every row of the sub-group (findings.json, csv/, json/, the findings
    // sheets, the All source rows sheet) unless the label and the cause pass the same checks the plan, the
    // tree, the tickets and the PDF apply.
    for (label, cause, what) in [
        (
            "Raw SQL in handlers",
            "LEAK-MARKER the scanner flagged these as regex-scan hits.",
            "cause",
        ),
        (
            "LEAK-MARKER the scanner flagged these as regex-scan hits.",
            "the query is built from a request value",
            "label",
        ),
    ] {
        let sealed = with_label_and_cause(label, cause);
        let reviewed = apply_overlay(&sealed, &[], &[], &[]);
        let (text, findings) = client_text_of(package_of(&reviewed, &client_options()));
        assert!(
            !text.contains("LEAK-MARKER"),
            "{what}: a client file carries the text"
        );
        // The rows of the sub-group still name their group, with the checked texts.
        let rows: Vec<&Value> = findings["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["group"] == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
            .collect();
        assert!(!rows.is_empty(), "{what}: the sub-group has rows");
        for row in rows {
            assert_ne!(row["sub_group"], "", "{what}: a label is always printed");
            assert!(
                !row["sub_group"].as_str().unwrap().contains("LEAK-MARKER"),
                "{what}: {row}"
            );
            if what == "cause" {
                assert_eq!(row["sub_group_cause"], "", "{what}: {row}");
            }
        }
    }

    // A text that passes is printed as written.
    let sealed = with_label_and_cause(
        "Raw SQL in handlers",
        "the query is built from a request value",
    );
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let (_, findings) = client_text_of(package_of(&reviewed, &client_options()));
    let row = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["group"] == "sub:SEC-NO-RAW-SQL-CONCAT-1/0")
        .unwrap();
    assert_eq!(row["sub_group"], "Raw SQL in handlers");
    assert_eq!(
        row["sub_group_cause"],
        "the query is built from a request value"
    );
}
