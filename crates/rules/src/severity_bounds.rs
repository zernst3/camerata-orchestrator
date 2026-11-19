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
    /// Record why a bound moved the severity (one plain sentence per step, for example
    /// "severity lowered from high to info: no file or line was identified for this finding").
    /// Called by [`apply_corpus_severity_bounds`] right after [`Self::set_severity`], once per
    /// bound that moved it. The default drops the note, so a caller with nowhere to put it
    /// (the CI gate) is unchanged.
    fn note_severity_adjustment(&mut self, _note: String) {}
    /// Whether a reviewer stage doubted this row, as a plain clause naming who and how (for
    /// example "the per-site reviewer judged it not real"): stage 1 said it is not real, or the
    /// group reviewer called it a rule misfit or a likely false alarm (owner ruling R-7). A floor
    /// never raises such a row. The default is `None`, so a caller with no reviewers (the CI
    /// gate) is unchanged.
    fn reviewer_doubt(&self) -> Option<String> {
        None
    }
    /// Whether the rule's `min_severity` is only guidance for this row: the model's own rating
    /// stands and the floor never raises it (owner ruling, 2026-11-17: the agent finder's rows).
    /// The default is `false`, so the CI gate and every other producer keep the floor.
    fn rule_minimum_is_guidance(&self) -> bool {
        false
    }
    /// Hold the row for a decision: called when a floor would have raised it but a reviewer
    /// doubted it, so the human reviewer decides it. `detail` says why in the operator's words
    /// (who doubted it, and which floor was held back); it is for the operator's own package
    /// only, never for a client. The default does nothing.
    fn hold_for_decision(&mut self, _detail: String) {}
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
/// 4. **Not a row a reviewer doubted** ([`BoundedFinding::reviewer_doubt`]). Owner ruling R-7
///    (2026-11-15): the auditor reports a borderline case at `low`, and a reviewer stage that
///    then says the row is not real, a rule misfit or a likely false alarm has said the floor's
///    premise (an under-rated defect) does not hold. The floor lifted exactly those rows to
///    `critical` and `high` in the graded runs. Such a row keeps its reviewed severity; see
///    [`floor_held_for_doubt`] for the note and the hold.
pub fn raise_to_min_severity<F: BoundedFinding + ?Sized>(
    severity: String,
    f: &F,
    rule: Option<&Rule>,
) -> String {
    if f.reviewer_doubt().is_some() {
        return severity;
    }
    match floor_that_would_raise(&severity, f, rule) {
        Some(floor) => floor,
        None => severity,
    }
}

/// The rule's floor when it ranks above `severity` and the floor's first three preconditions
/// hold (see [`raise_to_min_severity`]); reviewer doubt is NOT consulted here.
fn floor_that_would_raise<F: BoundedFinding + ?Sized>(
    severity: &str,
    f: &F,
    rule: Option<&Rule>,
) -> Option<String> {
    if !f.bound_kind().is_accusing()
        || f.is_unlocated()
        || severity == "info"
        || f.rule_minimum_is_guidance()
    {
        return None;
    }
    let floor = normalize_severity(rule.and_then(|r| r.min_severity.as_deref())?);
    (severity_rank(severity) > severity_rank(&floor)).then_some(floor)
}

/// What R-7 says about a floor that did not raise a row: the note a client may read and the
/// detail only the operator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldFloor {
    /// Neutral, for the client: "severity held at low pending a decision".
    pub note: String,
    /// For the operator's package only: who doubted the row and which floor was held back.
    pub detail: String,
}

/// R-7: the note for the floor that WOULD have raised `severity` but did not, because a reviewer
/// doubted the row. `None` when the row is not doubted or no floor would have moved it. The
/// caller holds the row for a decision and records the note ([`apply_corpus_severity_bounds`]).
/// The note is neutral because it reaches the client; the reviewer detail is separate.
pub fn floor_held_for_doubt<F: BoundedFinding + ?Sized>(
    severity: &str,
    f: &F,
    rule: Option<&Rule>,
) -> Option<HeldFloor> {
    let doubt = f.reviewer_doubt()?;
    let floor = floor_that_would_raise(severity, f, rule)?;
    Some(HeldFloor {
        note: format!("severity held at {severity} pending a decision"),
        detail: format!(
            "{doubt}, so the rule's minimum severity ({floor}) does not raise it; the row keeps \
             {severity} and is held for a decision"
        ),
    })
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

/// Why [`raise_to_min_severity`] moved a severity, as the note [`bounded_severity_explained`]
/// writes it (`severity raised from low to high: <this>`). One constant, so a reader of the
/// recorded notes can tell a rule minimum from every other bound without matching prose.
pub const MIN_SEVERITY_REASON: &str = "the rule sets a minimum severity for what it finds";

/// Whether `note`, one recorded severity adjustment, is a raise to the rule's minimum.
pub fn is_min_severity_note(note: &str) -> bool {
    note.starts_with("severity raised from ") && note.trim_end().ends_with(MIN_SEVERITY_REASON)
}

/// Which bound a recorded severity adjustment came from (see [`classify_adjustment`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundStep {
    /// The rule's `min_severity` raised the row.
    Floor,
    /// C4-R1: the row has no file and line, so it went to `info`.
    NoLocation,
    /// The rule's `max_severity` lowered the row.
    Ceiling,
    /// The deviation ceiling (authored or the default) lowered the row.
    DeviationCeiling,
}

/// Read back which bound wrote `note` (one recorded severity adjustment) and the severity that
/// bound moved the row FROM. `None` for any note this chain did not write as a move (a held-back
/// floor, a returned severity, the AI tier's own notes). The notes are built in
/// [`bounded_severity_explained`] from the constants read here, so the two cannot drift.
pub fn classify_adjustment(note: &str) -> Option<(BoundStep, &str)> {
    let rest = note
        .strip_prefix("severity raised from ")
        .or_else(|| note.strip_prefix("severity lowered from "))?;
    let (from, rest) = rest.split_once(" to ")?;
    let (_, why) = rest.split_once(": ")?;
    let step = match why {
        MIN_SEVERITY_REASON => BoundStep::Floor,
        UNLOCATED_WHY => BoundStep::NoLocation,
        MAX_CAP_WHY => BoundStep::Ceiling,
        DEVIATION_CAP_WHY => BoundStep::DeviationCeiling,
        _ => return None,
    };
    Some((step, from))
}

/// The `(from, to)` severities of any recorded note of the form `severity raised|lowered from X
/// to Y[: why]`, whoever wrote it (a bound, a merge, a phrase floor, a reviewer pass). `None` for
/// a note that is not a move (a held-back floor, a returned severity).
pub fn adjustment_move(note: &str) -> Option<(&str, &str)> {
    let rest = note
        .strip_prefix("severity raised from ")
        .or_else(|| note.strip_prefix("severity lowered from "))?;
    let (from, rest) = rest.split_once(" to ")?;
    let to = rest.split_once(": ").map_or(rest, |(to, _)| to);
    Some((from, to.trim()))
}

/// Why C4-R1 lowered an unlocated row, as its note says it.
pub const UNLOCATED_WHY: &str = "no file or line was identified for this finding";

/// Why the rule's `max_severity` lowered a row, as its note says it.
pub const MAX_CAP_WHY: &str = "the rule caps the severity of what it finds";

/// `f`'s severity after every bound: normalize, the floor, C4-R1, then both ceilings (see the
/// module docs for the order). `rule` is the corpus rule the caller resolved for `f`; `None`
/// still applies C4-R1 and the deviation ceiling's default, which need no authored rule.
pub fn bounded_severity<F: BoundedFinding + ?Sized>(f: &F, rule: Option<&Rule>) -> String {
    bounded_severity_explained(f, rule).0
}

/// [`bounded_severity`] plus one plain sentence for every bound that moved the severity, in the
/// order they ran. The severity is the same value `bounded_severity` returns (that function
/// calls this one), so the reason can never disagree with the result. A bound that leaves the
/// severity alone adds nothing, with one exception: a floor a reviewer's doubt held back (R-7)
/// says so ([`floor_held_for_doubt`]), because a row the floor did not raise is exactly the row the
/// reader would otherwise expect at the floor. An unmoved finding with no held floor returns an
/// empty list.
pub fn bounded_severity_explained<F: BoundedFinding + ?Sized>(
    f: &F,
    rule: Option<&Rule>,
) -> (String, Vec<String>) {
    let mut notes: Vec<String> = Vec::new();
    let mut step = |before: &str, after: String, why: String| -> String {
        if after != before {
            let verb = if severity_rank(&after) < severity_rank(before) {
                "raised"
            } else {
                "lowered"
            };
            notes.push(format!("severity {verb} from {before} to {after}: {why}"));
        }
        after
    };
    let start = normalize_severity(f.severity());
    let held = floor_held_for_doubt(&start, f, rule);
    let raised = step(
        &start,
        raise_to_min_severity(start.clone(), f, rule),
        MIN_SEVERITY_REASON.to_string(),
    );
    let located = step(
        &raised,
        downgrade_unlocated(raised.clone(), f),
        UNLOCATED_WHY.to_string(),
    );
    let capped = step(
        &located,
        clamp_to_max_severity(located.clone(), rule),
        MAX_CAP_WHY.to_string(),
    );
    let last = step(
        &capped,
        clamp_deviation_severity(capped.clone(), f, rule),
        DEVIATION_CAP_WHY.to_string(),
    );
    if let Some(note) = held {
        notes.insert(0, note.note);
    }
    (last, notes)
}

/// Why the A5.2 deviation ceiling lowered a row, as its note says it.
pub const DEVIATION_CAP_WHY: &str =
    "a consistency deviation is capped below the severity of a defect";

/// The severity the deviation ceiling lowered a row FROM, read back from the note
/// [`bounded_severity_explained`] wrote for it (`None` for any other note). The note is built and
/// read here, in one place, so the two cannot drift. Used when a reviewer's reason shows a row
/// filed as a deviation is a defect: the cap no longer applies and the row takes back the
/// severity the cap took off.
pub fn deviation_cap_lowered_from(note: &str) -> Option<&str> {
    let rest = note.strip_prefix("severity lowered from ")?;
    let (from, rest) = rest.split_once(" to ")?;
    let (_, why) = rest.split_once(": ")?;
    (why == DEVIATION_CAP_WHY).then_some(from)
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
/// - **A held floor holds the row (R-7).** A row whose floor a reviewer's doubt held back is
///   passed to [`BoundedFinding::hold_for_decision`] and given the held note, whether or not a
///   ceiling moved it. A rerun hands the same note again; the server's note writer keeps one.
pub fn apply_corpus_severity_bounds<'c, F: BoundedFinding>(
    findings: &mut [F],
    resolve: impl Fn(&str) -> Option<&'c Rule>,
) {
    for f in findings.iter_mut() {
        let rule = resolve(f.rule_id());
        let start = normalize_severity(f.severity());
        let held = floor_held_for_doubt(&start, f, rule);
        let (bounded, notes) = bounded_severity_explained(f, rule);
        // Compare against the NORMALIZED original, so an unrelated casing difference
        // ("High" vs "high") is not mistaken for a bound moving the severity.
        let moved = bounded != start;
        if moved {
            f.set_severity(bounded);
        }
        if moved || held.is_some() {
            for note in notes {
                f.note_severity_adjustment(note);
            }
        }
        if let Some(held) = held {
            f.hold_for_decision(held.detail);
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

    /// A row that keeps the notes the bounds hand it, to prove nothing moves silently.
    #[derive(Debug, Clone)]
    struct NotedRow {
        inner: Row,
        notes: Vec<String>,
    }

    impl BoundedFinding for NotedRow {
        fn severity(&self) -> &str {
            self.inner.severity()
        }
        fn rule_id(&self) -> &str {
            self.inner.rule_id()
        }
        fn bound_kind(&self) -> BoundKind {
            self.inner.bound_kind()
        }
        fn is_unlocated(&self) -> bool {
            self.inner.is_unlocated()
        }
        fn set_severity(&mut self, severity: String) {
            self.inner.severity = severity;
        }
        fn note_severity_adjustment(&mut self, note: String) {
            self.notes.push(note);
        }
    }

    fn noted(severity: &str, kind: BoundKind) -> NotedRow {
        NotedRow {
            inner: row(severity, kind),
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_ceiling_says_it_lowered_the_severity_and_why() {
        let set = rule_set(|r| r.max_severity = Some("low".to_string()));
        let mut rows = vec![noted("high", BoundKind::Defect)];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows[0].inner.severity, "low");
        assert_eq!(
            rows[0].notes,
            ["severity lowered from high to low: the rule caps the severity of what it finds"]
        );
    }

    #[test]
    fn the_unlocated_downgrade_is_no_longer_silent() {
        let set = RuleSet::default();
        let mut unlocated = noted("high", BoundKind::Defect);
        unlocated.inner.path = String::new();
        unlocated.inner.line = 0;
        let mut rows = vec![unlocated];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows[0].inner.severity, "info");
        assert_eq!(
            rows[0].notes,
            ["severity lowered from high to info: no file or line was identified for this finding"]
        );
    }

    #[test]
    fn a_floor_says_it_raised_the_severity() {
        let set = rule_set(|r| r.min_severity = Some("high".to_string()));
        let mut rows = vec![noted("low", BoundKind::Defect)];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows[0].inner.severity, "high");
        assert_eq!(rows[0].notes.len(), 1);
        assert!(rows[0].notes[0].starts_with("severity raised from low to high"));
    }

    #[test]
    fn an_unmoved_row_and_a_rerun_add_no_notes_and_the_result_matches_bounded_severity() {
        let set = rule_set(|r| r.max_severity = Some("low".to_string()));
        let mut rows = vec![
            noted("high", BoundKind::Defect),
            noted("low", BoundKind::Defect),
        ];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows[0].notes.len(), 1, "the rerun must not repeat the note");
        assert!(rows[1].notes.is_empty());
        for severity in ["critical", "high", "medium", "low", "info", "Critical"] {
            let f = noted(severity, BoundKind::Defect);
            let rule = set.get_by_id("BOUND-1");
            assert_eq!(
                bounded_severity_explained(&f, rule).0,
                bounded_severity(&f, rule)
            );
        }
    }

    /// A row a reviewer doubted (owner ruling R-7), keeping its notes and its hold.
    #[derive(Debug, Clone)]
    struct DoubtedRow {
        inner: NotedRow,
        doubted: bool,
        held: bool,
    }

    impl BoundedFinding for DoubtedRow {
        fn severity(&self) -> &str {
            self.inner.severity()
        }
        fn rule_id(&self) -> &str {
            self.inner.rule_id()
        }
        fn bound_kind(&self) -> BoundKind {
            self.inner.bound_kind()
        }
        fn is_unlocated(&self) -> bool {
            self.inner.is_unlocated()
        }
        fn set_severity(&mut self, severity: String) {
            self.inner.set_severity(severity);
        }
        fn note_severity_adjustment(&mut self, note: String) {
            self.inner.note_severity_adjustment(note);
        }
        fn reviewer_doubt(&self) -> Option<String> {
            self.doubted
                .then(|| "the reviewer judged it not real".to_string())
        }
        fn hold_for_decision(&mut self, _detail: String) {
            self.held = true;
        }
    }

    fn doubted(severity: &str, doubted: bool) -> DoubtedRow {
        DoubtedRow {
            inner: noted(severity, BoundKind::Defect),
            doubted,
            held: false,
        }
    }

    #[test]
    fn a_floor_never_raises_a_row_a_reviewer_doubted_and_holds_it_with_a_note() {
        let set = rule_set(|r| r.min_severity = Some("critical".to_string()));
        let mut rows = vec![doubted("low", true), doubted("low", false)];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].inner.inner.severity, "low");
        assert!(rows[0].held, "the doubted row is held for a decision");
        assert_eq!(rows[0].inner.notes.len(), 1, "{:?}", rows[0].inner.notes);
        assert!(
            rows[0].inner.notes[0].starts_with("severity held at low"),
            "{:?}",
            rows[0].inner.notes
        );
        // Nobody doubted the second row: the floor raises it and does not hold it.
        assert_eq!(rows[1].inner.inner.severity, "critical");
        assert!(!rows[1].held);
        // A ceiling still lowers a doubted row: doubt only ever blocks a raise.
        let set = rule_set(|r| r.max_severity = Some("low".to_string()));
        let mut rows = vec![doubted("high", true)];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows[0].inner.inner.severity, "low");
        assert!(!rows[0].held, "no floor was held, so nothing to decide");
    }

    #[test]
    fn the_deviation_cap_note_reads_back_the_severity_it_lowered_from() {
        let set = RuleSet::default();
        let mut rows = vec![NotedRow {
            inner: row("critical", BoundKind::Deviation),
            notes: Vec::new(),
        }];
        apply_corpus_severity_bounds(&mut rows, |id| set.get_by_id(id));
        assert_eq!(rows[0].inner.severity, "medium");
        assert_eq!(rows[0].notes.len(), 1);
        assert_eq!(
            deviation_cap_lowered_from(&rows[0].notes[0]),
            Some("critical")
        );
        assert_eq!(
            deviation_cap_lowered_from(
                "severity lowered from high to low: the rule caps the severity of what it finds"
            ),
            None
        );
    }

    #[test]
    fn a_rule_minimum_note_is_told_apart_from_every_other_bound() {
        let floor = rule_set(|r| r.min_severity = Some("high".to_string()));
        let (_, notes) =
            bounded_severity_explained(&row("low", BoundKind::Defect), floor.get_by_id("BOUND-1"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(is_min_severity_note(&notes[0]), "{notes:?}");
        let ceiling = rule_set(|r| r.max_severity = Some("low".to_string()));
        let (_, notes) = bounded_severity_explained(
            &row("high", BoundKind::Defect),
            ceiling.get_by_id("BOUND-1"),
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(!is_min_severity_note(&notes[0]), "{notes:?}");
        let unlocated = Row {
            path: String::new(),
            line: 0,
            ..row("high", BoundKind::Defect)
        };
        let (_, notes) = bounded_severity_explained(&unlocated, None);
        assert!(notes.iter().all(|n| !is_min_severity_note(n)), "{notes:?}");
    }

    #[test]
    fn each_recorded_adjustment_reads_back_its_bound_and_its_from_severity() {
        let both = rule_set(|r| {
            r.min_severity = Some("high".to_string());
            r.max_severity = Some("medium".to_string());
        });
        let rule = both.get_by_id("BOUND-1");
        // The floor raises low to high, then the ceiling lowers it to medium.
        let (_, notes) = bounded_severity_explained(&row("low", BoundKind::Defect), rule);
        let steps: Vec<_> = notes
            .iter()
            .filter_map(|n| classify_adjustment(n))
            .collect();
        assert_eq!(
            steps,
            vec![(BoundStep::Floor, "low"), (BoundStep::Ceiling, "high")],
            "{notes:?}"
        );
        let (_, notes) = bounded_severity_explained(&row("critical", BoundKind::Deviation), None);
        assert_eq!(
            classify_adjustment(&notes[0]),
            Some((BoundStep::DeviationCeiling, "critical"))
        );
        let unlocated = Row {
            path: String::new(),
            line: 0,
            ..row("high", BoundKind::Defect)
        };
        let (_, notes) = bounded_severity_explained(&unlocated, None);
        assert_eq!(
            classify_adjustment(&notes[0]),
            Some((BoundStep::NoLocation, "high"))
        );
        assert_eq!(
            classify_adjustment("severity held at low pending a decision"),
            None
        );
        assert_eq!(classify_adjustment("something the AI tier wrote"), None);
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
