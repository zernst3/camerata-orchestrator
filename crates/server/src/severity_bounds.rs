//! The corpus's authored severity bounds, applied to a finding set in ONE place.
//!
//! A corpus rule can bound the severity of its own findings three ways:
//!
//! - `min_severity`: a FLOOR (W8 / W5-Q2). Raises a lower severity up to it, for an accusing,
//!   located finding only (see [`crate::report_export::raise_to_rule_min_severity`]).
//! - `max_severity`: a CEILING (E2). Clamps a higher severity down to it.
//! - `deviation_ceiling`: the A5.2 ceiling for a consistency deviation (default `medium`).
//!
//! # Why this module exists
//!
//! These bounds used to be applied ONLY at export time, inside `report_export::build_report_json`
//! (the PDF) and `xlsx_export::partition_rows` (the workbook, CSV and JSON). The live scan
//! copied each producer's severity into the [`Finding`] verbatim, so the cockpit's triage view
//! showed a different severity from the billed PDF/xlsx for the same finding on every one of
//! the corpus rules that declares a bound. `ui-core`'s triage ranking even documents that it
//! assumes the ceilings were "applied upstream"; upstream did not apply them.
//!
//! The fix is a POSITION fix, in the same spirit as the evidence-gate seal fix: the bounds are
//! applied to the findings themselves, as part of the scan pipeline, via
//! [`apply_corpus_severity_bounds`]. The exporters keep calling the same primitives through
//! [`bounded_severity`] / [`apply_severity_ceilings`], so there is exactly ONE definition of
//! the order and the rules, and the export-side clamps stay as idempotent backstops for a
//! `ScanReport` that did not come through the live pipeline (a persisted older scan, a test
//! fixture, a hand-built report).
//!
//! # Order
//!
//! Floor, then `max_severity`, then the deviation ceiling: exactly the order both exporters
//! use. The two ceilings only ever LOWER a severity, so their relative order cannot change the
//! result (the outcome is the lower of the two); the floor must run first so a ceiling always
//! gets the last word. The corpus-load gate rejects a `min_severity` ranking above the same
//! rule's `max_severity`, so floor-then-ceilings always lands inside `[min, max]`.
//!
//! # What this deliberately does not do
//!
//! The PDF additionally applies C4-R1 (an unlocated above-informational finding is shown as
//! `info`). That is a report-integrity downgrade of how an UNLOCATED row is presented, not a
//! corpus-authored bound, and the workbook does not apply it either, so it stays export-side.
//! The floor already declines an unlocated row, so it cannot undo C4-R1 from any position.

use crate::onboard::Finding;
use crate::report_export::{
    clamp_deviation_severity, clamp_to_rule_max_severity, normalize_severity,
    raise_to_rule_min_severity,
};

/// Apply BOTH ceilings to an already-normalized `severity`: the rule's `max_severity` (E2),
/// then the A5.2 deviation ceiling. Ceilings only lower, so the order between the two is
/// immaterial to the result; it is fixed here to match the historical exporter order.
///
/// `pub(crate)`: shared by the PDF partition in `report_export::build_report_json`, which must
/// slot C4-R1 between the floor and these, and by [`bounded_severity`].
pub(crate) fn apply_severity_ceilings(
    severity: String,
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    let severity = clamp_to_rule_max_severity(severity, &f.rule_id, corpus);
    clamp_deviation_severity(severity, f, corpus)
}

/// `f`'s severity after every corpus-authored bound: normalize, floor, then both ceilings.
/// The single definition the live scan ([`apply_corpus_severity_bounds`]) and the workbook /
/// CSV / JSON partition (`xlsx_export::partition_rows`) share, so the two cannot drift.
pub(crate) fn bounded_severity(f: &Finding, corpus: Option<&camerata_rules::RuleSet>) -> String {
    let severity = raise_to_rule_min_severity(normalize_severity(&f.severity), f, corpus);
    apply_severity_ceilings(severity, f, corpus)
}

/// Apply the corpus's authored severity bounds (`min_severity` floor, `max_severity` ceiling,
/// `deviation_ceiling`) to every finding, in place, in the order the exporters use. See the
/// module docs for why this belongs in the scan pipeline.
///
/// Properties the callers rely on:
///
/// - **Idempotent.** A bounded severity is a fixed point of the bounds, so running this twice
///   (the live scan, then an export-side backstop) changes nothing.
/// - **Minimal write.** `Finding::severity` is only rewritten when a bound actually moves it.
///   A finding no bound touches keeps its producer's exact string; this function does not
///   re-case or re-vocabulary a severity as a side effect.
/// - **No corpus is still a corpus.** The deviation ceiling's `medium` DEFAULT needs no
///   authored rule, and the exporters apply it with no corpus at all. A caller holding an
///   `Option<&RuleSet>` therefore uses [`apply_scan_severity_bounds`], which applies an empty
///   `RuleSet` rather than skipping, so the live view and the export agree for a scan that ran
///   without a corpus too.
pub fn apply_corpus_severity_bounds(findings: &mut [Finding], corpus: &camerata_rules::RuleSet) {
    for f in findings.iter_mut() {
        let bounded = bounded_severity(f, Some(corpus));
        // Compare against the NORMALIZED original, so an unrelated casing difference
        // ("High" vs "high") is not mistaken for a bound moving the severity.
        if bounded != normalize_severity(&f.severity) {
            f.severity = bounded;
        }
    }
}

/// [`apply_corpus_severity_bounds`] for a caller holding the scan pipeline's
/// `Option<&RuleSet>`. `None` (no corpus loaded) applies an EMPTY rule set rather than skipping:
/// the deviation ceiling's `medium` default needs no authored rule, and the exporters apply it
/// with no corpus at all, so skipping would reintroduce the live-vs-export disagreement for a
/// scan that ran without a corpus.
pub fn apply_scan_severity_bounds(
    findings: &mut [Finding],
    corpus: Option<&camerata_rules::RuleSet>,
) {
    match corpus {
        Some(corpus) => apply_corpus_severity_bounds(findings, corpus),
        None => apply_corpus_severity_bounds(findings, &camerata_rules::RuleSet::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::audit::{DeviationKind, FindingKind};

    fn rule_set(configure: impl FnOnce(&mut camerata_rules::Rule)) -> camerata_rules::RuleSet {
        let mut rule = camerata_rules::bare_rule("BOUND-1", "test-fixture");
        configure(&mut rule);
        let mut set = camerata_rules::RuleSet::default();
        set.push(rule);
        set
    }

    fn located(severity: &str, kind: Option<FindingKind>) -> Finding {
        Finding {
            repo: "o/r".to_string(),
            path: "src/a.rs".to_string(),
            line: 7,
            rule_id: "BOUND-1".to_string(),
            severity: severity.to_string(),
            kind,
            ..Finding::default()
        }
    }

    fn deviation() -> FindingKind {
        FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(10),
            convention_option_id: None,
            convention_label: "the convention".to_string(),
        })
    }

    fn bounded(severity: &str, kind: Option<FindingKind>, set: &camerata_rules::RuleSet) -> String {
        let mut fs = vec![located(severity, kind)];
        apply_corpus_severity_bounds(&mut fs, set);
        fs.remove(0).severity
    }

    #[test]
    fn a_max_severity_ceiling_lowers_a_higher_severity() {
        let set = rule_set(|r| r.max_severity = Some("low".to_string()));
        assert_eq!(bounded("critical", Some(FindingKind::Defect), &set), "low");
        assert_eq!(bounded("medium", Some(FindingKind::Defect), &set), "low");
    }

    #[test]
    fn a_max_severity_ceiling_never_raises() {
        let set = rule_set(|r| r.max_severity = Some("high".to_string()));
        assert_eq!(bounded("low", Some(FindingKind::Defect), &set), "low");
        assert_eq!(bounded("high", Some(FindingKind::Defect), &set), "high");
    }

    #[test]
    fn a_min_severity_floor_raises_a_lower_severity_on_an_accusing_located_finding() {
        let set = rule_set(|r| r.min_severity = Some("high".to_string()));
        assert_eq!(bounded("low", Some(FindingKind::Defect), &set), "high");
        // A floor is not a ceiling: a more severe finding is untouched.
        assert_eq!(
            bounded("critical", Some(FindingKind::Defect), &set),
            "critical"
        );
    }

    #[test]
    fn a_floor_declines_a_kindless_or_unlocated_finding() {
        let set = rule_set(|r| r.min_severity = Some("high".to_string()));
        // No kind: nothing may silently default to Defect, so the floor does not apply.
        assert_eq!(bounded("low", None, &set), "low");
        // No location: nothing concrete to rate.
        let mut fs = vec![Finding {
            path: String::new(),
            line: 0,
            ..located("low", Some(FindingKind::Defect))
        }];
        apply_corpus_severity_bounds(&mut fs, &set);
        assert_eq!(fs[0].severity, "low");
    }

    #[test]
    fn the_floor_runs_before_the_ceilings_so_a_ceiling_has_the_last_word() {
        let set = rule_set(|r| {
            r.min_severity = Some("high".to_string());
            r.max_severity = Some("high".to_string());
        });
        assert_eq!(bounded("low", Some(FindingKind::Defect), &set), "high");
        assert_eq!(bounded("critical", Some(FindingKind::Defect), &set), "high");
    }

    #[test]
    fn a_deviation_caps_at_medium_by_default_and_a_defect_does_not() {
        let set = rule_set(|r| r.max_severity = Some("critical".to_string()));
        assert_eq!(bounded("high", Some(deviation()), &set), "medium");
        assert_eq!(bounded("high", Some(FindingKind::Defect), &set), "high");
        assert_eq!(bounded("high", None, &set), "high");
    }

    #[test]
    fn an_authored_deviation_ceiling_overrides_the_medium_default() {
        let set = rule_set(|r| {
            r.max_severity = Some("critical".to_string());
            r.deviation_ceiling = Some("high".to_string());
        });
        assert_eq!(bounded("critical", Some(deviation()), &set), "high");
        assert_eq!(bounded("high", Some(deviation()), &set), "high");
    }

    #[test]
    fn the_lower_of_the_two_ceilings_wins() {
        let set = rule_set(|r| {
            r.max_severity = Some("low".to_string());
            r.deviation_ceiling = Some("high".to_string());
        });
        assert_eq!(bounded("critical", Some(deviation()), &set), "low");
    }

    #[test]
    fn a_finding_no_bound_touches_keeps_its_producers_exact_severity_string() {
        let set = rule_set(|r| r.max_severity = Some("high".to_string()));
        // Casing is the exporters' concern (they normalize on read); the live write is
        // minimal, so an unmoved severity is not re-cased.
        assert_eq!(bounded("Low", Some(FindingKind::Defect), &set), "Low");
        // But a casing difference never hides a bound that DOES move it.
        assert_eq!(bounded("Critical", Some(FindingKind::Defect), &set), "high");
    }

    #[test]
    fn a_rule_the_corpus_does_not_know_is_untouched() {
        let set = rule_set(|r| r.max_severity = Some("low".to_string()));
        let mut fs = vec![Finding {
            rule_id: "UNKNOWN-1".to_string(),
            ..located("critical", Some(FindingKind::Defect))
        }];
        apply_corpus_severity_bounds(&mut fs, &set);
        assert_eq!(fs[0].severity, "critical");
    }

    #[test]
    fn applying_the_bounds_twice_changes_nothing() {
        let set = rule_set(|r| {
            r.min_severity = Some("medium".to_string());
            r.max_severity = Some("high".to_string());
        });
        let mut fs: Vec<Finding> = ["critical", "high", "medium", "low", "info", "Critical"]
            .into_iter()
            .map(|s| located(s, Some(FindingKind::Defect)))
            .collect();
        apply_corpus_severity_bounds(&mut fs, &set);
        let once = fs.clone();
        apply_corpus_severity_bounds(&mut fs, &set);
        assert_eq!(fs, once);
    }
}
