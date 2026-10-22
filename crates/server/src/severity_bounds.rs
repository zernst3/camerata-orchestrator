//! The corpus's authored severity bounds, applied to a finding set in ONE place.
//!
//! A corpus rule can bound the severity of its own findings three ways:
//!
//! - `min_severity`: a FLOOR (W8 / W5-Q2). Raises a lower severity up to it, for an accusing,
//!   located finding only (see [`crate::report_export::raise_to_rule_min_severity`]).
//! - `max_severity`: a CEILING (E2). Clamps a higher severity down to it.
//! - `deviation_ceiling`: the A5.2 ceiling for a consistency deviation (default `medium`).
//!
//! Between the floor and the ceilings sits one bound that is NOT corpus-authored: the C4-R1
//! null-location safety net ([`crate::report_export::downgrade_unlocated_above_informational`]),
//! which sends a finding with no path and line 0 to `info`. See "C4-R1" below.
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
//! Floor, then C4-R1, then `max_severity`, then the deviation ceiling: the one order every
//! surface uses. The two ceilings only ever LOWER a severity, so their relative order cannot
//! change the result (the outcome is the lower of the two); the floor must run first so a
//! ceiling always gets the last word. The corpus-load gate rejects a `min_severity` ranking
//! above the same rule's `max_severity`, so floor-then-ceilings always lands inside
//! `[min, max]`.
//!
//! # C4-R1 is part of the chain (changed 2026-10-21)
//!
//! C4-R1 says no finding above `info` may ship with BOTH an empty path AND line 0: nothing
//! concrete was established about where it is, so it cannot be actionable at its stated
//! severity. Its rationale (see `downgrade_unlocated_above_informational`) is about the FINDING,
//! not about one artifact's presentation, and it was written as "the single place every
//! checker's findings funnel through". It had been applied by the PDF alone, which left the
//! live scan, the workbook, the CSV/JSON exports and `findings.json` showing the producer's
//! severity for the same row (`high` where the PDF said `info`). Nothing in the design makes
//! that difference intentional: the docs only recorded it as an acknowledged asymmetry (and, in
//! `RULES_REFERENCE.md`, as an open issue). So it now runs here, between the floor and the
//! ceilings, and every surface agrees. It only ever lowers, and `info` ranks with `low`, so no
//! ceiling can raise it back. The floor still declines an unlocated row at its own
//! precondition, so the two cannot fight from any position.
//!
//! Consequence worth knowing: an unlocated finding is `info` in the live triage view too, so an
//! operator-forced `violates` pick (which carries no evidence and anchors at `high`) is shown
//! at `info` from the moment it is emitted, exactly as the PDF always showed it.

use crate::onboard::Finding;
use crate::report_export::{
    clamp_deviation_severity, clamp_to_rule_max_severity, downgrade_unlocated_above_informational,
    normalize_severity, raise_to_rule_min_severity,
};

/// Apply BOTH ceilings to an already-normalized `severity`: the rule's `max_severity` (E2),
/// then the A5.2 deviation ceiling. Ceilings only lower, so the order between the two is
/// immaterial to the result; it is fixed here to match the historical exporter order.
fn apply_severity_ceilings(
    severity: String,
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> String {
    let severity = clamp_to_rule_max_severity(severity, &f.rule_id, corpus);
    clamp_deviation_severity(severity, f, corpus)
}

/// `f`'s severity after every bound: normalize, the floor, C4-R1, then both ceilings. The
/// single definition the live scan ([`apply_corpus_severity_bounds`]), the PDF partition
/// (`report_export::build_report_json`) and the workbook / CSV / JSON partition
/// (`xlsx_export::partition_rows`) all share, so the surfaces cannot drift.
pub(crate) fn bounded_severity(f: &Finding, corpus: Option<&camerata_rules::RuleSet>) -> String {
    let severity = raise_to_rule_min_severity(normalize_severity(&f.severity), f, corpus);
    let severity = downgrade_unlocated_above_informational(severity, f);
    apply_severity_ceilings(severity, f, corpus)
}

/// Apply every severity bound (`min_severity` floor, the C4-R1 unlocated downgrade,
/// `max_severity` ceiling, `deviation_ceiling`) to every finding, in place, in the order every
/// surface uses. See the module docs for why this belongs in the scan pipeline.
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
        // The floor declined (it would have said `high`); C4-R1 then sends the unlocated row to
        // `info`, so the result is `info` and never the floor's `high`.
        assert_eq!(fs[0].severity, "info");
    }

    /// C4-R1 sits between the floor and the ceilings: an unlocated finding is `info` whatever
    /// the producer said and whatever the rule's bounds are, and a located one is untouched by it.
    #[test]
    fn an_unlocated_finding_is_info_before_the_ceilings_and_a_located_one_is_not() {
        let set = rule_set(|r| {
            r.min_severity = Some("high".to_string());
            r.max_severity = Some("critical".to_string());
        });
        for sev in ["critical", "high", "medium", "low", "info"] {
            for kind in [None, Some(FindingKind::Defect), Some(deviation())] {
                let mut fs = vec![Finding {
                    path: String::new(),
                    line: 0,
                    ..located(sev, kind)
                }];
                apply_corpus_severity_bounds(&mut fs, &set);
                assert_eq!(fs[0].severity, "info", "unlocated {sev}");
            }
        }
        // Either half of a location is a location.
        let mut half = vec![Finding {
            path: String::new(),
            line: 4,
            ..located("high", Some(FindingKind::Defect))
        }];
        apply_corpus_severity_bounds(&mut half, &set);
        assert_eq!(half[0].severity, "high");
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
