//! False-negative audit, Batch A follow-up: an absorbed member at the row's own place is not
//! another site; the member's own headline and severity stay visible.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::onboard::MergedLocation;

const RULE: &str = "SEC-NO-HARDCODED-SECRETS-1";

fn member(path: &str, line: usize, severity: &str, headline: &str) -> MergedLocation {
    MergedLocation::site("owner/repo", path, line, "AI-INVENTED-RULE-9", "x", false)
        .with_rating(severity, headline)
}

fn row_with(members: Vec<MergedLocation>) -> crate::onboard::Finding {
    let mut f = finding(RULE, "src/main.rs", 10, "medium");
    f.also_locations = members;
    f
}

#[test]
fn a_member_at_the_rows_own_line_is_not_another_site() {
    let f = row_with(vec![member(
        "src/main.rs",
        10,
        "critical",
        "Any user can read any invoice",
    )]);
    assert_eq!(f.distinct_site_count(), 1);
    let report = report_with(vec![f], vec![RULE]);
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(site.site_count, 1);
    assert!(site.also_sites.is_empty(), "{:?}", site.also_sites);
    assert_eq!(
        site.also_flagged_here,
        ["Also flagged here as Any user can read any invoice (critical)"]
    );
}

#[test]
fn a_member_at_another_line_is_a_site_and_a_member_at_the_same_line_is_not() {
    let f = row_with(vec![
        member("src/main.rs", 10, "high", "Same place"),
        member("src/other.rs", 4, "high", "Other place"),
        member("src/other.rs", 4, "low", "Other place again"),
    ]);
    assert_eq!(f.distinct_site_count(), 2);
    let report = report_with(vec![f], vec![RULE]);
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(site.site_count, 2);
    assert_eq!(site.also_sites.len(), 1);
    assert_eq!(site.also_flagged_here.len(), 2);
    assert!(site.also_flagged_here[1].starts_with("Also flagged at src/other.rs:4 as "));
}

#[test]
fn an_absorbed_site_without_a_corpus_title_shows_its_headline_and_a_higher_severity() {
    let f = row_with(vec![member(
        "src/other.rs",
        4,
        "critical",
        "Missing owner check",
    )]);
    let report = report_with(vec![f], vec![RULE]);
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let also = &json.curated_findings[0].sites[0].also_sites[0];
    assert!(also.title.is_none());
    assert_eq!(also.headline.as_deref(), Some("Missing owner check"));
    assert_eq!(
        also.severity.as_deref(),
        Some("critical"),
        "it outranks the row's medium"
    );
}

#[test]
fn a_lower_or_equal_member_severity_is_not_shown() {
    let f = row_with(vec![
        member("src/a.rs", 1, "low", "Minor"),
        member("src/b.rs", 2, "medium", "Same"),
    ]);
    let report = report_with(vec![f], vec![RULE]);
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let sites = &json.curated_findings[0].sites[0].also_sites;
    assert!(sites.iter().all(|s| s.severity.is_none()), "{sites:?}");
}

#[test]
fn the_sites_vs_rows_check_counts_distinct_places() {
    use camerata_api_types::audit::{DeviationKind, FindingKind};
    // Two deviating sites are claimed; the row and a same-line member are ONE place.
    let mut f = finding("ARCH-API-VERSIONING-1", "src/api/routes.rs", 42, "medium");
    f.kind = Some(FindingKind::Deviation(DeviationKind {
        deviating_sites: 2,
        assessed_sites: None,
        convention_option_id: None,
        convention_label: "observed majority".to_string(),
    }));
    f.also_locations = vec![MergedLocation::site(
        f.repo.clone(),
        "src/api/routes.rs",
        42,
        "ARCH-API-VERSIONING-1",
        "x",
        false,
    )];
    let mut report = report_with(vec![f], vec!["ARCH-API-VERSIONING-1"]);
    let before = report.findings.len();
    apply_sites_vs_rows_check(&mut report);
    assert_eq!(
        report.findings.len(),
        before + 1,
        "one place is represented, two are claimed"
    );
}
