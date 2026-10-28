//! False-negative audit, Batch C, M2/M8: a bound that moves a finding's severity says so in the
//! PDF, the workbook, `findings.json` and the CSV, and never moves it silently.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with, which_typst};
use super::*;
use crate::onboard::Finding;

const CEILING_NOTE: &str =
    "Severity lowered from high to low: the rule caps the severity of what it finds.";
const UNLOCATED_NOTE: &str =
    "Severity lowered from high to info: no file or line was identified for this finding.";

fn corpus_with_ceiling() -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule("CEIL-1", "security");
    rule.max_severity = Some("low".to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

fn capped_finding() -> Finding {
    finding("CEIL-1", "src/a.rs", 4, "high")
}

fn every_site(json: &AuditReportJson) -> Vec<&CuratedSiteJson> {
    json.curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter())
        .collect()
}

#[test]
fn the_unlocated_downgrade_records_why_on_the_finding() {
    let mut f = finding("SEC-NO-HARDCODED-SECRETS-1", "", 0, "high");
    f.path = String::new();
    f.line = 0;
    let mut fs = vec![f];
    crate::severity_bounds::apply_scan_severity_bounds(&mut fs, None);
    assert_eq!(fs[0].severity, "info");
    assert_eq!(
        crate::severity_bounds::severity_adjustment_lines(&fs[0], None),
        [UNLOCATED_NOTE]
    );
    // Applying the bounds again changes nothing and repeats nothing.
    crate::severity_bounds::apply_scan_severity_bounds(&mut fs, None);
    assert_eq!(
        crate::severity_bounds::severity_adjustment_lines(&fs[0], None),
        [UNLOCATED_NOTE]
    );
}

#[test]
fn a_row_the_bounds_never_touched_has_no_adjustment_lines() {
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 3, "high");
    assert!(crate::severity_bounds::severity_adjustment_lines(&f, None).is_empty());
}

#[test]
fn the_pdf_data_says_what_a_ceiling_lowered_whether_or_not_the_live_pass_ran() {
    let corpus = corpus_with_ceiling();
    // Export-only: the bounds were never applied to the finding itself.
    let report = report_with(vec![capped_finding()], vec!["CEIL-1"]);
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus),
        &empty_opts(),
    );
    let sites = every_site(&json);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].severity, "low");
    assert_eq!(sites[0].severity_adjustments, [CEILING_NOTE]);

    // Live pass ran first: the same single line, not two.
    let mut f = capped_finding();
    f.severity = "high".to_string();
    let mut fs = vec![f];
    crate::severity_bounds::apply_corpus_severity_bounds(&mut fs, &corpus);
    let report = report_with(fs, vec!["CEIL-1"]);
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(every_site(&json)[0].severity_adjustments, [CEILING_NOTE]);
}

#[test]
fn an_unadjusted_row_carries_an_empty_list() {
    let report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 3, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    assert!(every_site(&json)[0].severity_adjustments.is_empty());
}

#[test]
fn findings_json_and_the_csv_carry_the_adjustment_only_when_there_is_one() {
    let corpus = corpus_with_ceiling();
    let report = report_with(
        vec![
            capped_finding(),
            finding("SEC-NO-HARDCODED-SECRETS-1", "src/b.rs", 9, "high"),
        ],
        vec!["CEIL-1", "SEC-NO-HARDCODED-SECRETS-1"],
    )
    .seal();
    let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
    let export = crate::xlsx_export::build_findings_export(
        &report,
        &HashMap::new(),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    let rows = value["findings"].as_array().unwrap();
    let capped = rows.iter().find(|r| r["rule_id"] == "CEIL-1").unwrap();
    assert_eq!(capped["severity_adjustments"], CEILING_NOTE);
    let plain = rows
        .iter()
        .find(|r| r["rule_id"] == "SEC-NO-HARDCODED-SECRETS-1")
        .unwrap();
    assert!(plain.get("severity_adjustments").is_none(), "{plain}");

    let csvs = crate::xlsx_export::build_csv_exports(
        &report,
        &HashMap::new(),
        Some(&corpus),
        &empty_opts(),
    )
    .unwrap();
    let (_, all) = csvs
        .iter()
        .find(|(n, _)| n == "csv/all-findings.csv")
        .unwrap();
    let text = String::from_utf8_lossy(all);
    assert!(text.contains("severity_adjustments"), "{text}");
    assert!(text.contains(CEILING_NOTE), "{text}");
}

#[test]
fn the_workbook_has_a_severity_adjustments_column() {
    let corpus = corpus_with_ceiling();
    let report = report_with(vec![capped_finding()], vec!["CEIL-1"]).seal();
    let bytes =
        crate::xlsx_export::build_workbook(&report, &HashMap::new(), Some(&corpus), &empty_opts())
            .unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .expect("a workbook")
        .join("\n");
    assert!(all.contains("Severity adjustments"), "{all}");
    assert!(all.contains(CEILING_NOTE), "{all}");
}

#[tokio::test]
async fn the_pdf_prints_the_adjustment() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_prints_the_adjustment: typst not on PATH");
        return;
    }
    let corpus = corpus_with_ceiling();
    let report = report_with(vec![capped_finding()], vec!["CEIL-1"]);
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus),
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        joined.contains("Severityloweredfromhightolow:therulecapstheseverityofwhatitfinds."),
        "{text}"
    );
}
