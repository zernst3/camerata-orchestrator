//! WO9, amendment A5 and the wave 5 review: the group reviewer's triage of a row (why it was
//! escalated, its rank, the outlier mark, the disagreement with stage 1 and the model's own words
//! for each) is on the INTERNAL workbook and data files only. A client package carries none of it.

use camerata_api_types::review::{
    DisagreementField, EscalationReason, EscalationState, OutlierReason, Stage2Disagreement,
};

use super::support::*;
use crate::release_gate::{read_zip_entries, release_internal, xlsx_strings};
use crate::report_export::apply_overlay;

const WHYS: [&str; 3] = [
    "SECRET-WHY-ESCALATION",
    "SECRET-WHY-OUTLIER",
    "SECRET-WHY-DISAGREE",
];

fn triaged_sealed() -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    let row = &mut report.findings[0];
    row.review.escalation_reason = Some(EscalationReason::RuleMisfit);
    row.review.escalation_why = WHYS[0].to_string();
    row.review.stage2_rank = Some(2);
    row.review.global_rank = Some(7);
    row.review.escalation_state = EscalationState::Escalated;
    row.review.outlier_reason = Some(OutlierReason::DifferentConstruct);
    row.review.outlier_why = WHYS[1].to_string();
    row.review.stage2_disagreement = vec![Stage2Disagreement {
        field: DisagreementField::Severity,
        stage1: "medium".to_string(),
        mine: "critical".to_string(),
        why: WHYS[2].to_string(),
    }];
    report.seal()
}

fn all_text(entries: &std::collections::BTreeMap<String, Vec<u8>>, data_json: &str) -> String {
    let mut text = String::from(data_json);
    for (name, bytes) in entries {
        if name.ends_with(".xlsx") {
            text.push_str(&xlsx_strings(bytes).expect("strings").join("\n"));
        } else if !name.ends_with(".pdf") {
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    text
}

#[test]
fn stage2_reasons_and_why_text_appear_only_in_the_internal_workbook() {
    let sealed = triaged_sealed();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);

    // A client package: none of it, in any file, the PDF's data or the workbook.
    let client = client_options();
    let package = package_of(&reviewed, &client);
    let data = package.data_json.clone();
    let entries = read_zip_entries(&crate::release_gate::release(package).expect("clears").bytes)
        .expect("zip");
    let text = all_text(&entries, &data);
    for secret in WHYS {
        assert!(!text.contains(secret), "a client file carries {secret}");
    }
    for key in [
        "escalation_reason",
        "escalation_why",
        "stage2_rank",
        "global_rank",
        "over_budget_reason",
        "outlier_reason",
        "outlier_why",
        "stage2_disagreement",
        "rule_misfit",
        "different_construct",
    ] {
        assert!(!text.contains(key), "a client file carries {key}");
    }

    // The operator's own package: every one of them, in the workbook and the data files.
    let internal = crate::report_export::ReportOptions {
        internal: true,
        ..client_options()
    };
    let package = package_of(&reviewed, &internal);
    let data = package.data_json.clone();
    let zip = release_internal(package)
        .expect("an internal package clears")
        .bytes;
    let entries = read_zip_entries(&zip).expect("zip");
    let workbook = entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| xlsx_strings(b).expect("strings").join("\n"))
        .expect("a workbook");
    for want in WHYS.iter().copied().chain([
        "rule_misfit",
        "different_construct",
        "Escalation reason",
        "Outlier why",
    ]) {
        assert!(
            workbook.contains(want),
            "the internal workbook lacks {want}"
        );
    }
    let findings: serde_json::Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    let row = &findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "src/a.rs")
        .cloned()
        .expect("the triaged row");
    assert_eq!(row["escalation_reason"], "rule_misfit");
    assert_eq!(row["escalation_why"], WHYS[0]);
    assert_eq!(row["stage2_rank"], "2");
    assert_eq!(row["outlier_why"], WHYS[1]);
    assert!(row["stage2_disagreement"]
        .as_str()
        .unwrap()
        .contains(WHYS[2]));
    // The PDF is the same document for both: it never carries the triage.
    for secret in WHYS {
        assert!(!data.contains(secret), "the PDF data carries {secret}");
    }
}

/// Where each field of the stage 2 reply is rendered in the internal workbook: the text to find
/// in its cells. The map is checked against `STAGE2_FIELD_HOMES`, so a field added to the reply
/// without a rendered home fails here.
#[test]
fn every_stage2_reply_field_is_rendered_in_the_internal_workbook_and_json() {
    use crate::ai_audit::group_review::STAGE2_FIELD_HOMES;
    use crate::review_tree::{Outlier, OutlierBy, TextSource};

    let mut report = triaged_sealed().into_inner();
    let key = report.findings[0].site_key.clone();
    let group = report.findings[0].group_path[2].clone();
    report.findings[0].review.subgroup_label = "Params".to_string();
    {
        let tree = report.tree.as_mut().unwrap();
        let node = tree.nodes.iter_mut().find(|n| n.id == group).unwrap();
        node.label = "Params".to_string();
        node.cause = Some("a query parameter".to_string());
        node.summary = Some("Raw SQL built from input.".to_string());
        node.summary_source = TextSource::Model;
        node.outliers = vec![Outlier {
            key,
            reason: OutlierReason::DifferentConstruct,
            by: OutlierBy::Model,
            why: WHYS[1].to_string(),
        }];
        let rule = tree
            .nodes
            .iter_mut()
            .find(|n| n.id == "rule:SEC-NO-RAW-SQL-CONCAT-1")
            .unwrap();
        rule.summary = Some("The rule: SQL built from input.".to_string());
    }
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let internal = crate::report_export::ReportOptions {
        internal: true,
        ..client_options()
    };
    let zip = release_internal(package_of(&reviewed, &internal))
        .expect("clears")
        .bytes;
    let entries = read_zip_entries(&zip).expect("zip");
    let workbook = entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| xlsx_strings(b).expect("strings").join("\n"))
        .unwrap();
    let findings = String::from_utf8_lossy(&entries["findings.json"]).into_owned();

    let rendered: &[(&str, &[&str])] = &[
        ("summary", &["The rule: SQL built from input."]),
        ("subgroups[].label", &["Params"]),
        ("subgroups[].cause", &["a query parameter"]),
        ("subgroups[].summary", &["Raw SQL built from input."]),
        ("subgroups[].members[]", &["sub:SEC-NO-RAW-SQL-CONCAT-1/0"]),
        ("outliers[].index", &["Outliers"]),
        ("outliers[].reason", &["different_construct"]),
        ("outliers[].why", &[WHYS[1]]),
        ("escalations[].index", &["Escalation"]),
        ("escalations[].reason", &["rule_misfit"]),
        ("escalations[].rank", &["Stage 2 rank", "Global rank"]),
        ("escalations[].why", &[WHYS[0]]),
        ("disagreements[].index", &["Stage 2 disagreement"]),
        ("disagreements[].field", &["severity: stage 1"]),
        ("disagreements[].stage1", &["stage 1 medium"]),
        ("disagreements[].mine", &["group reviewer critical"]),
        ("disagreements[].why", &[WHYS[2]]),
    ];
    let homes: std::collections::BTreeSet<&str> =
        STAGE2_FIELD_HOMES.iter().map(|(f, _)| *f).collect();
    let covered: std::collections::BTreeSet<&str> = rendered.iter().map(|(f, _)| *f).collect();
    assert_eq!(
        covered, homes,
        "every reply field has a rendered home to check"
    );
    for (field, wants) in rendered {
        for want in *wants {
            assert!(
                workbook.contains(want) || findings.contains(want),
                "{field}: the internal package does not render {want:?}"
            );
        }
    }
    // And the JSON half: the triage columns are keys of the row in findings.json.
    for key in [
        "escalation_reason",
        "escalation_why",
        "stage2_rank",
        "global_rank",
        "outlier_reason",
        "outlier_why",
        "stage2_disagreement",
    ] {
        assert!(
            findings.contains(&format!("\"{key}\"")),
            "findings.json lacks {key}"
        );
    }
}
