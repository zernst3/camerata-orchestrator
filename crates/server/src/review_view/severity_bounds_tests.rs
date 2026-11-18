//! The row carries the rule's severity bounds and which one moved the shown severity.

use camerata_api_types::audit::{DeviationKind, FindingKind};
use camerata_api_types::review_view::{AppliedBound, AppliedStep, SeverityBoundsView, SiteRow};

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

fn step(bound: AppliedBound, from: &str) -> AppliedStep {
    AppliedStep {
        bound,
        from: from.to_string(),
    }
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
    assert_eq!(v.applied, vec![step(AppliedBound::Floor, "medium")]);
}

#[test]
fn a_ceiling_that_capped_the_row_is_carried_with_the_severity_it_capped_from() {
    let set = corpus(|r| r.max_severity = Some("medium".to_string()));
    let r = shown(row(RULE, "src/a.rs", 7, "high"), &set);
    assert_eq!(r.severity, "medium");
    let v = view(&r);
    assert_eq!(v.ceiling.as_deref(), Some("medium"));
    assert_eq!(v.applied, vec![step(AppliedBound::Ceiling, "high")]);
}

#[test]
fn a_deviation_ceiling_that_capped_the_row_is_carried_with_the_ceiling_that_applied() {
    // The rule authors none: the default ceiling applied and the row says what it was.
    let none = corpus(|r| r.min_severity = Some("low".to_string()));
    let mut f = row(RULE, "src/a.rs", 7, "high");
    f.kind = Some(deviation());
    let r = shown(f, &none);
    let v = view(&r);
    assert_eq!(
        v.applied,
        vec![step(AppliedBound::DeviationCeiling, "high")]
    );
    assert_eq!(v.deviation_ceiling.as_deref(), Some("medium"));
    // The rule authors its own.
    let own = corpus(|r| r.deviation_ceiling = Some("low".to_string()));
    let mut f = row(RULE, "src/a.rs", 7, "high");
    f.kind = Some(deviation());
    let v = view(&shown(f, &own));
    assert_eq!(v.applied.len(), 1);
    assert_eq!(v.applied[0].bound, AppliedBound::DeviationCeiling);
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
    assert_eq!(v.applied, vec![step(AppliedBound::NoLocation, "high")]);
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
    assert_eq!(v.applied, Vec::new());
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

/// A deviation row of the rule `set`, after the live bounds pass.
fn deviation_row(severity: &str) -> Finding {
    let mut f = row(RULE, "src/a.rs", 7, severity);
    f.kind = Some(deviation());
    f
}

#[test]
fn a_floor_then_the_deviation_ceiling_carries_both_steps_in_order() {
    let set = corpus(|r| r.min_severity = Some("high".to_string()));
    let r = shown(deviation_row("low"), &set);
    assert_eq!(r.severity, "medium");
    let v = view(&r);
    assert_eq!(
        v.applied,
        vec![
            step(AppliedBound::Floor, "low"),
            step(AppliedBound::DeviationCeiling, "high")
        ]
    );
    // A ceiling then the deviation ceiling.
    let set = corpus(|r| r.max_severity = Some("high".to_string()));
    let r = shown(deviation_row("critical"), &set);
    assert_eq!(r.severity, "medium");
    assert_eq!(
        view(&r).applied,
        vec![
            step(AppliedBound::Ceiling, "critical"),
            step(AppliedBound::DeviationCeiling, "high")
        ]
    );
}

#[test]
fn a_cap_note_no_longer_explains_a_severity_the_follow_reviewers_pass_restored() {
    use camerata_api_types::review::EscalationReason;
    let set = corpus(|_| {});
    let mut f = deviation_row("high");
    f.review.escalation_reason = Some(EscalationReason::AuthorizationBypass);
    let mut rows = vec![f];
    crate::severity_bounds::apply_corpus_severity_bounds(&mut rows, &set);
    assert_eq!(rows[0].severity, "medium", "the deviation cap ran first");
    crate::follow_reviewers::follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "a security defect is restored");
    let r = shown_after_scan(rows, &set);
    assert_eq!(r.severity, "high");
    assert_eq!(r.severity_bounds, None, "the cap no longer explains high");
}

#[test]
fn a_bound_note_before_a_later_non_bound_raise_is_not_the_reason() {
    let set = corpus(|r| r.min_severity = Some("high".to_string()));
    let mut f = row(RULE, "src/a.rs", 7, "low");
    f.severity = "low".to_string();
    let mut rows = vec![f];
    crate::severity_bounds::apply_corpus_severity_bounds(&mut rows, &set);
    assert_eq!(rows[0].severity, "high");
    // Another writer (a merge, a phrase floor) then raised it again.
    crate::ai_audit::record_severity_adjustment(&mut rows[0], "high", "critical", "merged");
    rows[0].severity = "critical".to_string();
    let r = shown_after_scan(rows, &set);
    assert_eq!(r.severity, "critical");
    let v = view(&r);
    assert_eq!(v.floor.as_deref(), Some("high"), "the floor stays, quietly");
    assert_eq!(v.applied, Vec::new());
}

/// Rows already through the scan, as the review screen reads them.
fn shown_after_scan(rows: Vec<Finding>, set: &camerata_rules::RuleSet) -> SiteRow {
    let sealed = sealed(rows, |_| {});
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let stores = Stores {
        corpus: Some(set.clone()),
        ..Stores::default()
    };
    compose(&reviewed, &stores, |c| {
        c.sites_page(&format!("rule:{RULE}"), None, 500).unwrap()
    })
    .rows
    .into_iter()
    .next()
    .unwrap()
}

fn severity_edit(f: &Finding, old: &str, new: &str) -> camerata_api_types::edits::Edit {
    camerata_api_types::edits::Edit {
        id: "edit-000001".to_string(),
        key: f.site_key.clone(),
        field: camerata_api_types::edits::EditField::Severity,
        old: old.to_string(),
        new: new.to_string(),
        reason: "the reviewer's own rating".to_string(),
        by: "reviewer".to_string(),
        at: "2026-11-18T10:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

#[test]
fn a_floor_raised_row_the_reviewer_then_rated_carries_the_floor_quietly() {
    let set = corpus(|r| r.min_severity = Some("high".to_string()));
    let mut rows = vec![row(RULE, "src/a.rs", 7, "low")];
    crate::severity_bounds::apply_corpus_severity_bounds(&mut rows, &set);
    assert_eq!(rows[0].severity, "high");
    let sealed = sealed(rows, |_| {});
    let key = sealed.findings[0].site_key.clone();
    let probe = sealed.findings[0].clone();
    // The reviewer rates it high themselves: the floor did not decide that, they did.
    let edits = vec![severity_edit(&probe, "high", "high")];
    let reviewed = apply_overlay(&sealed, &[], &edits, &[]);
    let stores = Stores {
        corpus: Some(set.clone()),
        ..Stores::default()
    };
    let rows = compose(&reviewed, &stores, |c| {
        c.sites_page(&format!("rule:{RULE}"), None, 500).unwrap()
    })
    .rows;
    let r = rows.iter().find(|r| r.site_key == key).unwrap();
    assert_eq!(r.severity, "high");
    assert!(
        crate::report_export::overlay::reviewer_edited_field(
            reviewed
                .report()
                .findings
                .iter()
                .find(|f| f.site_key == key)
                .unwrap(),
            camerata_api_types::edits::EditField::Severity
        ),
        "the overlay marks the severity as the reviewer's"
    );
    let v = view(r);
    assert_eq!(v.floor.as_deref(), Some("high"));
    assert_eq!(v.applied, Vec::new());
}

#[test]
fn an_agent_finder_row_carries_no_floor_because_the_rule_minimum_is_guidance_for_it() {
    let set = corpus(|r| r.min_severity = Some("high".to_string()));
    let mut f = row(RULE, "src/a.rs", 7, "low");
    f.captures.insert(
        crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE.to_string(),
        "1".to_string(),
    );
    let r = shown(f, &set);
    assert_eq!(r.severity, "low", "the floor never raised it");
    assert_eq!(r.severity_bounds, None);
    // The same rule's ordinary row does carry its floor.
    let r = shown(row(RULE, "src/a.rs", 7, "high"), &set);
    assert_eq!(view(&r).floor.as_deref(), Some("high"));
}
