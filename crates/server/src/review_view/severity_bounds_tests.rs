//! The row carries the rule's severity bounds and which one moved the shown severity.

use camerata_api_types::audit::{DeviationKind, FindingKind};
use camerata_api_types::review_view::{AppliedBound, SeverityBoundsView, SiteRow};

use super::test_support::{compose, row, sealed, Stores};
use crate::onboard::Finding;
use crate::report_export::apply_overlay;

const RULE: &str = "BOUND-1";

fn corpus(configure: impl FnOnce(&mut camerata_rules::Rule)) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule(RULE, "test-fixture");
    configure(&mut rule);
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

fn deviation() -> FindingKind {
    FindingKind::Deviation(DeviationKind {
        deviating_sites: 2,
        assessed_sites: Some(10),
        convention_option_id: None,
        convention_label: "the convention".to_string(),
    })
}

/// The one row of `finding` as the review screen reads it, after the scan applied the bounds of
/// `set` (as the live pipeline does) and the corpus is handed to the composer.
fn shown(finding: Finding, set: &camerata_rules::RuleSet) -> SiteRow {
    let mut rows = vec![finding];
    crate::severity_bounds::apply_corpus_severity_bounds(&mut rows, set);
    let sealed = sealed(rows, |_| {});
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let stores = Stores {
        corpus: Some(set.clone()),
        ..Stores::default()
    };
    let page = compose(&reviewed, &stores, |c| {
        c.sites_page(&format!("rule:{RULE}"), None, 500).unwrap()
    });
    assert_eq!(page.rows.len(), 1);
    page.rows.into_iter().next().unwrap()
}

fn view(row: &SiteRow) -> SeverityBoundsView {
    row.severity_bounds.clone().expect("the row carries bounds")
}

#[test]
fn a_floor_that_raised_the_row_is_carried_with_the_severity_it_raised_from() {
    let set = corpus(|r| r.min_severity = Some("high".to_string()));
    let r = shown(row(RULE, "src/a.rs", 7, "medium"), &set);
    assert_eq!(r.severity, "high");
    let v = view(&r);
    assert_eq!(v.floor.as_deref(), Some("high"));
    assert_eq!(v.applied, Some(AppliedBound::Floor));
    assert_eq!(v.applied_from.as_deref(), Some("medium"));
}

#[test]
fn a_ceiling_that_capped_the_row_is_carried_with_the_severity_it_capped_from() {
    let set = corpus(|r| r.max_severity = Some("medium".to_string()));
    let r = shown(row(RULE, "src/a.rs", 7, "high"), &set);
    assert_eq!(r.severity, "medium");
    let v = view(&r);
    assert_eq!(v.ceiling.as_deref(), Some("medium"));
    assert_eq!(v.applied, Some(AppliedBound::Ceiling));
    assert_eq!(v.applied_from.as_deref(), Some("high"));
}

#[test]
fn a_deviation_ceiling_that_capped_the_row_is_carried_with_the_ceiling_that_applied() {
    // The rule authors none: the default ceiling applied and the row says what it was.
    let none = corpus(|r| r.min_severity = Some("low".to_string()));
    let mut f = row(RULE, "src/a.rs", 7, "high");
    f.kind = Some(deviation());
    let r = shown(f, &none);
    let v = view(&r);
    assert_eq!(v.applied, Some(AppliedBound::DeviationCeiling));
    assert_eq!(v.applied_from.as_deref(), Some("high"));
    assert_eq!(v.deviation_ceiling.as_deref(), Some("medium"));
    // The rule authors its own.
    let own = corpus(|r| r.deviation_ceiling = Some("low".to_string()));
    let mut f = row(RULE, "src/a.rs", 7, "high");
    f.kind = Some(deviation());
    let v = view(&shown(f, &own));
    assert_eq!(v.applied, Some(AppliedBound::DeviationCeiling));
    assert_eq!(v.deviation_ceiling.as_deref(), Some("low"));
}

#[test]
fn the_null_location_net_is_carried_even_for_a_rule_with_no_bounds() {
    let set = corpus(|_| {});
    let mut f = row(RULE, "", 0, "high");
    f.snippet = String::new();
    let r = shown(f, &set);
    assert_eq!(r.severity, "info");
    let v = view(&r);
    assert_eq!(v.applied, Some(AppliedBound::NoLocation));
    assert_eq!(v.applied_from.as_deref(), Some("high"));
    assert_eq!(
        (v.floor, v.ceiling, v.deviation_ceiling),
        (None, None, None)
    );
}

#[test]
fn bounds_that_changed_nothing_are_carried_without_an_applied_bound() {
    let set = corpus(|r| {
        r.min_severity = Some("medium".to_string());
        r.max_severity = Some("critical".to_string());
    });
    let r = shown(row(RULE, "src/a.rs", 7, "high"), &set);
    assert_eq!(r.severity, "high");
    let v = view(&r);
    assert_eq!(v.floor.as_deref(), Some("medium"));
    assert_eq!(v.ceiling.as_deref(), Some("critical"));
    assert_eq!((v.applied, v.applied_from), (None, None));
}

#[test]
fn a_rule_with_no_bounds_and_no_move_carries_nothing() {
    let set = corpus(|_| {});
    let r = shown(row(RULE, "src/a.rs", 7, "high"), &set);
    assert_eq!(r.severity_bounds, None);
    let wire = serde_json::to_string(&r).unwrap();
    assert!(!wire.contains("severity_bounds"), "{wire}");
    // No corpus at all is the same: nothing is invented.
    let sealed = sealed(vec![row(RULE, "src/a.rs", 7, "high")], |_| {});
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let page = compose(&reviewed, &Stores::default(), |c| {
        c.sites_page(&format!("rule:{RULE}"), None, 500).unwrap()
    });
    assert_eq!(page.rows[0].severity_bounds, None);
}
