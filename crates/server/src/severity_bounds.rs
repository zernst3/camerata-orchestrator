//! The corpus's authored severity bounds, applied to the server's findings.
//!
//! The bounds themselves (the `min_severity` floor, the C4-R1 null-location safety net, the
//! `max_severity` ceiling and the A5.2 `deviation_ceiling`), their order and their rationale
//! live in [`camerata_rules::severity_bounds`], the one implementation this crate and the
//! repo-installed CI gate (`camerata-check`) share. This module is the server's adapter: it
//! maps an [`onboard::Finding`](crate::onboard::Finding) onto
//! [`camerata_rules::severity_bounds::BoundedFinding`] and resolves a finding's rule through
//! [`crate::report_export::corpus_rule_for`], which also grounds an external tool's own rule id.
//!
//! # Why the bounds run in the scan pipeline
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
//! [`apply_corpus_severity_bounds`]. The exporters call [`bounded_severity`], the same chain, so
//! the export-side clamps stay as idempotent backstops for a `ScanReport` that did not come
//! through the live pipeline (a persisted older scan, a test fixture, a hand-built report).
//!
//! # C4-R1 is part of the chain (changed 2026-10-21)
//!
//! C4-R1 says no finding above `info` may ship with BOTH an empty path AND line 0. Its rationale
//! is about the FINDING, not about one artifact's presentation. It had been applied by the PDF
//! alone, which left the live scan, the workbook, the CSV/JSON exports and `findings.json`
//! showing the producer's severity for the same row (`high` where the PDF said `info`). So it
//! runs in the shared chain, between the floor and the ceilings, and every surface agrees.
//!
//! Consequence worth knowing: an unlocated finding is `info` in the live triage view too, so an
//! operator-forced `violates` pick (which carries no evidence and anchors at `high`) is shown
//! at `info` from the moment it is emitted, exactly as the PDF always showed it.

use crate::onboard::Finding;
use crate::report_export::corpus_rule_for;
use camerata_api_types::audit::FindingKind;
use camerata_rules::severity_bounds::{BoundKind, BoundedFinding};

impl BoundedFinding for Finding {
    fn severity(&self) -> &str {
        &self.severity
    }

    fn rule_id(&self) -> &str {
        &self.rule_id
    }

    fn bound_kind(&self) -> BoundKind {
        match &self.kind {
            Some(FindingKind::Defect) => BoundKind::Defect,
            Some(FindingKind::Deviation(_)) => BoundKind::Deviation,
            Some(
                FindingKind::ConventionObserved(_)
                | FindingKind::NoConvention(_)
                | FindingKind::NotAssessed(_),
            ) => BoundKind::Informational,
            None => BoundKind::Unspecified,
        }
    }

    fn is_unlocated(&self) -> bool {
        camerata_rules::severity_bounds::is_unlocated(&self.path, self.line)
    }

    fn set_severity(&mut self, severity: String) {
        self.severity = severity;
    }

    fn note_severity_adjustment(&mut self, note: String) {
        record_adjustment(self, note);
    }
}

/// The `Finding::captures` key a severity adjustment is recorded under, one sentence per line.
///
/// M2/M8: the live scan applies the bounds in place, so by export time the producer's severity
/// is gone and a downgrade (the unlocated safety net most of all) was invisible. The note rides
/// with the finding instead. A capture is the carrier because it is a serde-default map that
/// every `Finding` already has; a dedicated `Finding::severity_adjustments` field, when the AI
/// tier adds one for its own ceilings, joins in through [`recorded_adjustments`].
pub const SEVERITY_ADJUSTMENTS_CAPTURE: &str = "severity_adjustments";

/// Most adjustment sentences one finding keeps (a row passes through a handful of producers).
const MAX_RECORDED_ADJUSTMENTS: usize = 8;

/// Append `note` to the finding's recorded adjustments (never twice, and at most
/// [`MAX_RECORDED_ADJUSTMENTS`]). The ONE writer: the corpus bounds and the AI tier's ceilings
/// both record through it.
pub(crate) fn record_adjustment(f: &mut Finding, note: String) {
    let entry = f
        .captures
        .entry(SEVERITY_ADJUSTMENTS_CAPTURE.to_string())
        .or_default();
    if entry.lines().any(|l| l == note) || entry.lines().count() >= MAX_RECORDED_ADJUSTMENTS {
        return;
    }
    if !entry.is_empty() {
        entry.push('\n');
    }
    entry.push_str(&note);
}

/// The adjustments recorded on `f` so far, oldest first.
pub(crate) fn recorded_adjustments(f: &Finding) -> Vec<String> {
    f.captures
        .get(SEVERITY_ADJUSTMENTS_CAPTURE)
        .map(|v| {
            v.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every severity adjustment to tell the client about for `f`, as finished sentences: the ones
/// recorded when the scan applied the bounds, plus any the bounds would make now (a finding that
/// reached an exporter without the live bound pass). Empty for a finding no bound moved.
pub(crate) fn severity_adjustment_lines(
    f: &Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Vec<String> {
    let mut notes = recorded_adjustments(f);
    let (_, now) = camerata_rules::severity_bounds::bounded_severity_explained(
        f,
        corpus_rule_for(&f.rule_id, corpus),
    );
    for note in now {
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
    notes.iter().map(|n| finish_sentence(n)).collect()
}

/// Capitalise the first letter and end with a full stop.
fn finish_sentence(note: &str) -> String {
    let note = note.trim();
    let mut chars = note.chars();
    let mut out = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => return String::new(),
    };
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// `f`'s severity after every bound (see [`camerata_rules::severity_bounds::bounded_severity`]).
/// The definition the live scan ([`apply_corpus_severity_bounds`]), the PDF partition
/// (`report_export::build_report_json`) and the workbook / CSV / JSON partition
/// (`xlsx_export::partition_rows`) all share, so the surfaces cannot drift.
pub(crate) fn bounded_severity(f: &Finding, corpus: Option<&camerata_rules::RuleSet>) -> String {
    camerata_rules::severity_bounds::bounded_severity(f, corpus_rule_for(&f.rule_id, corpus))
}

/// Apply every severity bound to every finding, in place
/// ([`camerata_rules::severity_bounds::apply_corpus_severity_bounds`]: idempotent, and a finding
/// no bound moves keeps its producer's exact string).
///
/// No corpus is still a corpus: the deviation ceiling's `medium` DEFAULT needs no authored rule,
/// and the exporters apply it with no corpus at all. A caller holding an `Option<&RuleSet>`
/// therefore uses [`apply_scan_severity_bounds`].
pub fn apply_corpus_severity_bounds(findings: &mut [Finding], corpus: &camerata_rules::RuleSet) {
    camerata_rules::severity_bounds::apply_corpus_severity_bounds(findings, |id| {
        corpus_rule_for(id, Some(corpus))
    });
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
    use camerata_api_types::audit::DeviationKind;

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

    /// The live scan applies the same exemption the shared chain defines: a producer-emitted
    /// `info` is an observation, not a defect, and the floor never raises it.
    #[test]
    fn the_live_scan_floor_never_raises_a_producer_emitted_info() {
        let set = rule_set(|r| r.min_severity = Some("critical".to_string()));
        assert_eq!(bounded("info", Some(FindingKind::Defect), &set), "info");
        assert_eq!(bounded("Info", Some(deviation()), &set), "Info");
        assert_eq!(bounded("low", Some(FindingKind::Defect), &set), "critical");
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
