//! False-negative audit, Batch C, H6: absorbed sites reach the PDF, and a rule that survived
//! only as an absorbed site counts as matched in the scorecard.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::onboard::MergedLocation;

fn site_at(path: &str, line: usize, rule_id: &str, consequence: bool) -> MergedLocation {
    MergedLocation {
        repo: "owner/repo".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        snippet: "x".to_string(),
        consequence,
    }
}

fn report_with_absorbed_sites(n: usize) -> crate::onboard::ScanReport {
    let mut primary = finding("SEC-NO-HARDCODED-SECRETS-1", "src/main.rs", 10, "high");
    primary.also_locations = (0..n)
        .map(|i| {
            site_at(
                &format!("src/other{i}.rs"),
                20 + i,
                "AI-INVENTED-RULE-9",
                i == 1,
            )
        })
        .collect();
    report_with(vec![primary], vec!["SEC-NO-HARDCODED-SECRETS-1"])
}

#[test]
fn a_curated_site_carries_its_absorbed_sites_without_raw_rule_ids() {
    let json = build_report_json(
        &report_with_absorbed_sites(3).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(site.also_sites.len(), 3);
    assert_eq!(site.also_sites[0].path, "src/other0.rs");
    assert_eq!(site.also_sites[0].line, 20);
    assert!(site.also_sites[1].consequence);
    // The invented id has no corpus title: nothing is printed for it, and it is never leaked.
    assert!(site.also_sites.iter().all(|s| s.title.is_none()));
    let data = serde_json::to_string(&site.also_sites).unwrap();
    assert!(!data.contains("AI-INVENTED-RULE-9"), "{data}");
}

#[test]
fn an_unmerged_row_has_no_also_sites() {
    let report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    assert!(json.curated_findings[0].sites[0].also_sites.is_empty());
}

#[test]
fn a_rule_that_survived_only_as_an_absorbed_site_is_not_counted_clean_in_the_scorecard() {
    let a = "SEC-NO-HARDCODED-SECRETS-1";
    let b = "SEC-NO-SECRET-FILE-1";
    assert_eq!(
        category_for(a, None),
        category_for(b, None),
        "the fixture needs both rules in one category"
    );
    let mut primary = finding(a, "src/main.rs", 10, "high");
    primary.also_locations = vec![site_at(".env.production", 1, b, false)];
    let report = report_with(vec![primary], vec![a, b]);
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let row = json
        .scorecard
        .rows
        .iter()
        .find(|r| r.category == category_for(a, None))
        .expect("the category has a scorecard row");
    assert_eq!(row.audited_rules, 2);
    assert_eq!(
        row.clean_rules, 0,
        "the absorbed rule fired, so it cannot be clean: {row:?}"
    );
}

#[tokio::test]
async fn the_pdf_lists_every_absorbed_site_up_to_ten_and_counts_the_rest() {
    if super::tests::which_typst().is_none() {
        eprintln!("skipping the PDF absorbed-sites test: typst not on PATH");
        return;
    }
    let json = build_report_json(
        &report_with_absorbed_sites(12).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(joined.contains("Alsofoundat12othersites"), "{text}");
    assert!(joined.contains("src/other0.rs"), "{text}");
    assert!(joined.contains("src/other9.rs"), "{text}");
    assert!(
        !joined.contains("src/other10.rs"),
        "the PDF names ten: {text}"
    );
    assert!(
        joined.contains("and2more,listedinfullintheworkbookandfindings.json"),
        "{text}"
    );
}
