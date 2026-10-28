//! The corpus's authored severity bounds: the ONE implementation every surface applies.
//!
//! A corpus rule can bound the severity of its own findings three ways:
//!
//! - [`Rule::min_severity`]: a FLOOR (W8 / W5-Q2). Raises a lower severity up to it, for an
//!   accusing, located finding only (see [`raise_to_min_severity`]).
//! - [`Rule::max_severity`]: a CEILING (E2). Clamps a higher severity down to it.
//! - [`Rule::deviation_ceiling`]: the A5.2 ceiling for a consistency deviation (default
//!   [`DEVIATION_DEFAULT_CEILING`]).
//!
//! Between the floor and the ceilings sits one bound that is NOT corpus-authored: the C4-R1
//! null-location safety net ([`downgrade_unlocated`]), which sends a finding with no path and
//! line 0 to `info`.
//!
//! # Why this lives in the rules crate
//!
//! Two binaries apply these bounds and must agree: the Camerata scan and exporters
//! (`camerata-server`), and the repo-installed CI gate (`camerata-check`), which must never
//! depend on `camerata-server`. Until 2026-10-28 the chain lived in the server alone, and
//! `camerata-check` reported ARCH-DEAD-CODE-1 at `medium` although the corpus caps it at `low`,
//! so a client's CI gate disagreed with the scan and with the billed artifacts for the same
//! finding. Both crates already depend on this one, so the chain is defined here once and each
//! caller supplies only an adapter ([`BoundedFinding`]) and its own rule lookup.
//!
//! # Order
//!
//! Floor, then C4-R1, then `max_severity`, then the deviation ceiling. The two ceilings only
//! ever LOWER a severity, so their relative order cannot change the result (the outcome is the
//! lower of the two); the floor must run first so a ceiling always gets the last word. The
//! corpus-load gate ([`crate::rules_with_invalid_severity_floor`]) rejects a `min_severity`
//! ranking above the same rule's `max_severity`, so floor-then-ceilings always lands inside
//! `[min, max]`. C4-R1 only ever lowers, and `info` ranks with `low`, so no ceiling can raise
//! it back; the floor declines an unlocated row at its own precondition, so the two cannot
//! fight from any position.

use crate::Rule;

/// The A5.2 severity ceiling for a CONSISTENCY DEVIATION when the rule authors no
/// [`Rule::deviation_ceiling`]. The owner's driver is FORWARD DRIFT COST ("consistency issues
/// would be a problem moving forward with new features moreso than the existing code"), so the
/// default cap sits at `medium`, and a rule whose decision domain genuinely compounds raises it
/// by AUTHORING `deviation_ceiling = "high"` in the corpus: an authoring-time structured field,
/// never a prose-scanning runtime heuristic.
pub const DEVIATION_DEFAULT_CEILING: &str = "medium";

/// Normalize a severity string to Camerata's canonical lowercase vocabulary
/// (`critical`/`high`/`medium`/`low`/`info`), defensively, at the read site. A casing drift from
/// an upstream producer (e.g. `"Critical"`) must never silently demote a finding to `low`.
/// Anything unrecognized maps to `low`.
pub fn normalize_severity(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "critical" => "critical".to_string(),
        "high" => "high".to_string(),
        "medium" => "medium".to_string(),
        // `info` is a real, distinct tier (Bug 4): a convention-to-consider, ranked BELOW
        // `low`. It must survive normalization rather than collapsing into `low`, or an
        // informational stance note would silently re-enter the plan/low action tier.
        "info" => "info".to_string(),
        _ => "low".to_string(),
    }
}

/// Rank of a NORMALIZED severity: lower is MORE severe. `low`, `info` and anything else share
/// the bottom rank, so a bound can never treat `info` as more severe than `low`.
pub fn severity_rank(sev: &str) -> u8 {
    match sev {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// What the bounds need to know about a finding's KIND. The server maps its structured
/// `FindingKind` onto this; `camerata-check` maps a checker violation the same way the scan's
/// adapter does (a deterministic violation is a [`BoundKind::Defect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundKind {
    /// An objective fault.
    Defect,
    /// A consistency deviation: subject to the A5.2 deviation ceiling.
    Deviation,
    /// A row that says something is fine or was not evaluated (convention observed, no
    /// convention, not assessed). Never raised by a floor.
    Informational,
    /// No kind recorded. Never assumed to be a defect or a deviation (W3's honesty rule that
    /// nothing may silently default to `Defect`).
    Unspecified,
}

impl BoundKind {
    /// A Defect or a Deviation: a row that ACCUSES the code of something.
    pub fn is_accusing(self) -> bool {
        matches!(self, BoundKind::Defect | BoundKind::Deviation)
    }
}

/// Whether a finding at `path` / `line` is UNLOCATED for C4-R1: BOTH an empty path AND line 0.
/// Either half of a location is a location.
pub fn is_unlocated(path: &str, line: usize) -> bool {
    path.is_empty() && line == 0
}

/// The facts of one finding the bounds read and the one field they write. Implemented by the
/// server's `onboard::Finding` and by `camerata-check`'s violation row, so both run
/// [`bounded_severity`] / [`apply_corpus_severity_bounds`] rather than a copy of them.
pub trait BoundedFinding {
    /// The producer's severity, as written (any casing).
    fn severity(&self) -> &str;
    /// The rule id the caller resolves to a corpus [`Rule`].
    fn rule_id(&self) -> &str;
    /// The finding's kind, as the bounds understand it.
    fn bound_kind(&self) -> BoundKind;
    /// Whether the finding is unlocated (see [`is_unlocated`]).
    fn is_unlocated(&self) -> bool;
    /// Overwrite the severity. Called only when a bound moved it.
    fn set_severity(&mut self, severity: String);
}

/// E2: clamp `severity` DOWN to `rule`'s [`Rule::max_severity`], when one is declared and
/// `severity` currently ranks ABOVE it. Never raises: this is a ceiling, and applying it is
/// always safe whatever produced `severity`. No rule, or no `max_severity`, is a no-op.
pub fn clamp_to_max_severity(severity: String, rule: Option<&Rule>) -> String {
    let Some(raw_ceiling) = rule.and_then(|r| r.max_severity.as_deref()) else {
        return severity;
    };
    let ceiling = normalize_severity(raw_ceiling);
    if severity_rank(&severity) < severity_rank(&ceiling) {
        ceiling
    } else {
        severity
    }
}

/// W8 / W5-Q2: RAISE `severity` UP to `rule`'s [`Rule::min_severity`], when one is declared and
/// `severity` currently ranks BELOW it. The structural mirror of [`clamp_to_max_severity`],
/// built because the owner ruled that a rule's prose statement of a single severity is a FLOOR
/// ("Floor, it's the minimum rating."). Never lowers.
///
/// # Two preconditions, both deliberate: a floor is not as safe as a ceiling
///
/// A ceiling can only lower a severity, so it is safe to apply unconditionally. A floor RAISES
/// one, so an over-broad floor is a false alarm shipped to a client. Two guards bound it:
///
/// 1. **An ACCUSING kind, explicitly present** ([`BoundKind::is_accusing`]). An informational
///    row says something is FINE (or was not evaluated) and always ships at `info` by W3's
///    design; raising one would lie to the client and re-route it into an action bucket. An
///    absent kind is excluded too: nothing may silently default to `Defect`.
/// 2. **A real location.** An unlocated finding established nothing concrete to rate, and
///    [`downgrade_unlocated`] (C4-R1) exists to force exactly that shape to `info`. Declining
///    here, rather than relying on running before C4-R1, makes the floor order-independent with
///    respect to that safety net.
/// 3. **Not a producer-emitted `info`.** A producer that emits `info` is making an
///    OBSERVATION ("confirm in the dashboard"), not claiming a live defect, and a floor exists
///    to stop an under-rated DEFECT. Raising an observation to the rule's floor turned
///    SUPABASE-RLS-POLICY-DISABLED-1's platform-owned row (`info`, "confirm in the dashboard")
///    into a `critical` in both the scan and `camerata-check`. The decision (logged 2026-10-28)
///    is that `info` is exempt, whatever the kind and whoever the producer.
pub fn raise_to_min_severity<F: BoundedFinding + ?Sized>(
    severity: String,
    f: &F,
    rule: Option<&Rule>,
) -> String {
    if !f.bound_kind().is_accusing() || f.is_unlocated() || severity == "info" {
        return severity;
    }
    let Some(raw_floor) = rule.and_then(|r| r.min_severity.as_deref()) else {
        return severity;
    };
    let floor = normalize_severity(raw_floor);
    if severity_rank(&severity) > severity_rank(&floor) {
        floor
    } else {
        severity
    }
}

/// A5.2: clamp a [`BoundKind::Deviation`] row's severity DOWN to its rule's authored
/// [`Rule::deviation_ceiling`], defaulting to [`DEVIATION_DEFAULT_CEILING`] when the rule (or the
/// corpus) authors none. A no-op for every other kind: a Defect under the same rule is untouched,
/// and an unkinded row is NOT assumed to be a deviation. A ceiling, never a floor.
pub fn clamp_deviation_severity<F: BoundedFinding + ?Sized>(
    severity: String,
    f: &F,
    rule: Option<&Rule>,
) -> String {
    if f.bound_kind() != BoundKind::Deviation {
        return severity;
    }
    let ceiling = rule
        .and_then(|r| r.deviation_ceiling.as_deref())
        .map(normalize_severity)
        .unwrap_or_else(|| DEVIATION_DEFAULT_CEILING.to_string());
    if severity_rank(&severity) < severity_rank(&ceiling) {
        ceiling
    } else {
        severity
    }
}

/// C4-R1: the pipeline-wide null-location safety net. No finding above `info` may ship
/// unlocated (see [`is_unlocated`]): nothing concrete was established about where it is, so it
/// cannot be actionable at its stated severity. Downgrades the SEVERITY only, to `info`; it
/// never drops the finding, so a reader still sees "something was flagged, but the scan could
/// not pin down where".
pub fn downgrade_unlocated<F: BoundedFinding + ?Sized>(severity: String, f: &F) -> String {
    if severity != "info" && f.is_unlocated() {
        "info".to_string()
    } else {
        severity
    }
}

/// `f`'s severity after every bound: normalize, the floor, C4-R1, then both ceilings (see the
/// module docs for the order). `rule` is the corpus rule the caller resolved for `f`; `None`
/// still applies C4-R1 and the deviation ceiling's default, which need no authored rule.
pub fn bounded_severity<F: BoundedFinding + ?Sized>(f: &F, rule: Option<&Rule>) -> String {
    let severity = raise_to_min_severity(normalize_severity(f.severity()), f, rule);
    let severity = downgrade_unlocated(severity, f);
    let severity = clamp_to_max_severity(severity, rule);
    clamp_deviation_severity(severity, f, rule)
}

/// Apply every severity bound to every finding, in place. `resolve` maps a finding's rule id
/// to its corpus rule (a plain `RuleSet::get_by_id`, or a lookup that also grounds an external
/// tool's id); an unresolved id still gets the bounds that need no rule.
///
/// Properties the callers rely on:
///
/// - **Idempotent.** A bounded severity is a fixed point of the bounds, so running this twice
///   (the live scan, then an export-side backstop) changes nothing.
/// - **Minimal write.** The severity is only rewritten when a bound actually moves it. A finding
///   no bound touches keeps its producer's exact string; this does not re-case or re-vocabulary
///   a severity as a side effect.
pub fn apply_corpus_severity_bounds<'c, F: BoundedFinding>(
    findings: &mut [F],
    resolve: impl Fn(&str) -> Option<&'c Rule>,
) {
    for f in findings.iter_mut() {
        let bounded = bounded_severity(f, resolve(f.rule_id()));
        // Compare against the NORMALIZED original, so an unrelated casing difference
        // ("High" vs "high") is not mistaken for a bound moving the severity.
        if bounded != normalize_severity(f.severity()) {
            f.set_severity(bounded);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bare_rule, RuleSet};

    /// A minimal [`BoundedFinding`]: the trait is the whole contract, so the chain is tested
    /// here against it directly, independent of either caller's finding type.
    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        severity: String,
        rule_id: String,
        kind: BoundKind,
        path: String,
        line: usize,
    }

    impl BoundedFinding for Row {
        fn severity(&self) -> &str {
            &self.severity
        }
        fn rule_id(&self) -> &str {
            &self.rule_id
        }
        fn bound_kind(&self) -> BoundKind {
            self.kind
        }
        fn is_unlocated(&self) -> bool {
            is_unlocated(&self.path, self.line)
        }
        fn set_severity(&mut self, severity: String) {
            self.severity = severity;
        }
    }

    fn row(severity: &str, kind: BoundKind) -> Row {
        Row {
            severity: severity.to_string(),
            rule_id: "BOUND-1".to_string(),
            kind,
            path: "src/a.rs".to_string(),
            line: 7,
        }
    }

    fn rule_set(configure: impl FnOnce(&mut Rule)) -> RuleSet {
        let mut rule = bare_rule("BOUND-1", "test-fixture");
        configure(&mut rule);
        let mut set = RuleSet::default();
        set.push(rule);
        set
    }

    fn bounded(severity: &str, kind: BoundKind, set: &RuleSet) -> String {
        let mut rows = vec![row(severity, kind)];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        rows.remove(0).severity
    }

    #[test]
    fn a_max_severity_ceiling_lowers_and_never_raises() {
        let set = rule_set(|r| r.max_severity = Some("low".to_string()));
        assert_eq!(bounded("critical", BoundKind::Defect, &set), "low");
        assert_eq!(bounded("medium", BoundKind::Defect, &set), "low");
        let set = rule_set(|r| r.max_severity = Some("high".to_string()));
        assert_eq!(bounded("low", BoundKind::Defect, &set), "low");
    }

    #[test]
    fn a_floor_raises_only_an_accusing_located_finding() {
        let set = rule_set(|r| r.min_severity = Some("high".to_string()));
        assert_eq!(bounded("low", BoundKind::Defect, &set), "high");
        assert_eq!(bounded("low", BoundKind::Deviation, &set), "medium");
        assert_eq!(bounded("low", BoundKind::Informational, &set), "low");
        assert_eq!(bounded("low", BoundKind::Unspecified, &set), "low");
        assert_eq!(bounded("critical", BoundKind::Defect, &set), "critical");
    }

    #[test]
    fn a_floor_never_raises_a_producer_emitted_info_observation() {
        let set = rule_set(|r| r.min_severity = Some("critical".to_string()));
        for kind in [BoundKind::Defect, BoundKind::Deviation] {
            assert_eq!(bounded("info", kind, &set), "info", "{kind:?}");
            assert_eq!(bounded("INFO", kind, &set), "INFO", "{kind:?}");
        }
        // The exemption is only for `info`: the next tier up is still floored.
        assert_eq!(bounded("low", BoundKind::Defect, &set), "critical");
    }

    #[test]
    fn an_unlocated_finding_is_info_and_either_half_of_a_location_counts() {
        let set = rule_set(|r| r.min_severity = Some("high".to_string()));
        for kind in [
            BoundKind::Defect,
            BoundKind::Deviation,
            BoundKind::Informational,
            BoundKind::Unspecified,
        ] {
            let mut rows = vec![Row {
                path: String::new(),
                line: 0,
                ..row("critical", kind)
            }];
            apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
            assert_eq!(rows[0].severity, "info", "{kind:?}");
        }
        assert!(!is_unlocated("", 4));
        assert!(!is_unlocated("a.rs", 0));
    }

    #[test]
    fn a_deviation_caps_at_the_default_and_an_authored_ceiling_overrides_it() {
        let set = RuleSet::default();
        assert_eq!(bounded("high", BoundKind::Deviation, &set), "medium");
        assert_eq!(bounded("high", BoundKind::Defect, &set), "high");
        let set = rule_set(|r| r.deviation_ceiling = Some("high".to_string()));
        assert_eq!(bounded("critical", BoundKind::Deviation, &set), "high");
    }

    #[test]
    fn the_lower_of_the_two_ceilings_wins_and_a_ceiling_has_the_last_word() {
        let set = rule_set(|r| {
            r.max_severity = Some("low".to_string());
            r.deviation_ceiling = Some("high".to_string());
        });
        assert_eq!(bounded("critical", BoundKind::Deviation, &set), "low");
        let set = rule_set(|r| {
            r.min_severity = Some("high".to_string());
            r.max_severity = Some("high".to_string());
        });
        assert_eq!(bounded("low", BoundKind::Defect, &set), "high");
        assert_eq!(bounded("critical", BoundKind::Defect, &set), "high");
    }

    #[test]
    fn an_unmoved_severity_keeps_its_exact_string_and_a_rerun_changes_nothing() {
        let set = rule_set(|r| {
            r.min_severity = Some("medium".to_string());
            r.max_severity = Some("high".to_string());
        });
        assert_eq!(bounded("High", BoundKind::Defect, &set), "High");
        assert_eq!(bounded("Critical", BoundKind::Defect, &set), "high");
        let mut rows: Vec<Row> = ["critical", "high", "medium", "low", "info", "Critical"]
            .into_iter()
            .map(|s| row(s, BoundKind::Defect))
            .collect();
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        let once = rows.clone();
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows, once);
    }

    #[test]
    fn normalize_and_rank_share_one_vocabulary() {
        assert_eq!(normalize_severity("Critical"), "critical");
        assert_eq!(normalize_severity("INFO"), "info");
        assert_eq!(normalize_severity("hgih"), "low");
        assert!(severity_rank("critical") < severity_rank("high"));
        assert!(severity_rank("high") < severity_rank("medium"));
        assert!(severity_rank("medium") < severity_rank("low"));
        assert_eq!(severity_rank("low"), severity_rank("info"));
    }
}
