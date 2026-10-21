//! W15 / A5.7 — the two rules the owner reclassified off the paused classification hold, pinned
//! so they cannot silently regress to `decision`.
//!
//! The hold covered 39 rules. The owner's ruling closed all but one of them, leaving 37 at
//! `decision` and changing exactly two:
//!
//! | rule | from | to |
//! |---|---|---|
//! | `ARCH-OBSERVABILITY-SILENT-FAILURE-1` | `decision` | `defect` |
//! | `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` | `decision` | `default_with_exceptions` |
//!
//! # Why these need their own pinning tests
//!
//! A `kind` is one word in a TOML file and nothing about the corpus makes a wrong one loud. The
//! W12 reclassification already learned this the expensive way: nineteen security rules sat at
//! `decision` with zero `violates` flags, reporting a missing RLS policy and a committed private
//! key as "the convention this codebase chose", and the only thing that now stops them drifting
//! back is a hardcoded roster asserted per-rule. These two get the same treatment, and for the
//! same three properties W12 found each had its own failure mode:
//!
//! 1. **the kind** — a `decision`-kind rule cannot report a violation at all;
//! 2. **exactly one `violates` flag** — zero means the rule is still silent, two means the
//!    conforming option is flagged too and correct code gets reported as a fault;
//! 3. **the DIRECTION of the flag** — it must sit on the degenerate option specifically. The
//!    244-rule audit found 76 wrong flags, including flags sitting on the SAFE option, so "one
//!    flag somewhere" is not a sufficient invariant.
//!
//! A fourth property is pinned here that W12 did not need: the **adopted default must point at
//! the correct option**. A `defect`-kind rule behaves as single-directive, so if `[decision]
//! default` named the rejected option the reclassification would lock the fault in AS the
//! project's directive — strictly worse than the silence it replaced. W12 measured this at 0 bad
//! out of 19; these two are measured the same way rather than assumed.
//!
//! Severity is deliberately NOT asserted here beyond the one ceiling the owner explicitly
//! capped — see `silent_failure_rule_keeps_its_owner_directed_low_ceiling`.
//!
//! The behavioral half of this change (that the rule now REPORTS instead of recording a
//! convention) is proved end-to-end against the real `audit_repo` in
//! `crates/server/tests/w15_reclassified_rule_silence_e2e.rs`; this file pins the corpus state
//! that makes it true.

use camerata_rules::{load_corpus, Rule, RuleKind, RuleSet, DEFAULT_CORPUS_PATH};

const SILENT_FAILURE: &str = "ARCH-OBSERVABILITY-SILENT-FAILURE-1";
const EXHAUSTIVE_DEPS: &str = "JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1";

/// The option on each rule that describes the ABSENCE/degenerate form of its requirement — the
/// one that must carry `violates`. Hardcoded rather than derived from prose: a derived
/// expectation moves with the corpus and could not catch the flag being moved onto the safe
/// option, which is the specific error the 244-rule audit found 76 times.
const SILENT_FAILURE_DEGENERATE_OPTION: &str = "allow-silent-discards";
const EXHAUSTIVE_DEPS_DEGENERATE_OPTION: &str = "dependency-arrays-are-trimmed-to-control-when-ef";

/// The option each rule's `[decision] default` must name — the CORRECT behavior, never the
/// degenerate one.
const SILENT_FAILURE_CORRECT_OPTION: &str = "log-or-justify-every-caught-error";
const EXHAUSTIVE_DEPS_CORRECT_OPTION: &str =
    "effect-and-memo-dependency-arrays-list-every-reactive-value-the-body-reads";

/// Load the real shipped corpus, or `None` when it is not checked out — the same `path.exists()`
/// skip guard every other corpus-reading test in this crate uses.
async fn corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

fn rule<'a>(set: &'a RuleSet, id: &str) -> &'a Rule {
    set.get_by_id(id)
        .unwrap_or_else(|| panic!("{id} must exist in the shipped corpus"))
}

/// Assert properties 2 and 3 together: exactly one `violates` option, and it is
/// `expected_degenerate`. Shared by both rules so the two can never drift on rigor.
fn assert_exactly_one_violates_on(rule: &Rule, expected_degenerate: &str) {
    let flagged: Vec<&str> = rule
        .options
        .iter()
        .filter(|o| o.violates)
        .map(|o| o.id.as_str())
        .collect();
    assert_eq!(
        flagged.len(),
        1,
        "{}: {} options marked `violates`, must be exactly 1 — zero leaves the rule silent on \
         its own fault, two would report conforming code as a fault. Flagged: {flagged:?}",
        rule.id.0,
        flagged.len()
    );
    assert_eq!(
        flagged[0], expected_degenerate,
        "{}: `violates` sits on `{}` but must sit on `{expected_degenerate}` — a flag on the \
         SAFE option reports conforming code as a fault, which is the inverted-flag defect the \
         244-rule audit found 76 times",
        rule.id.0, flagged[0]
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Change 1 — ARCH-OBSERVABILITY-SILENT-FAILURE-1 -> defect
// ────────────────────────────────────────────────────────────────────────────

/// req_w15: `ARCH-OBSERVABILITY-SILENT-FAILURE-1` is `defect`-kind and carries exactly one
/// `violates` flag, on `allow-silent-discards`.
///
/// Two options, one of which the rule's own author opens with `Rejected:`, is not a decision
/// under any reading: "allow a caught or inspected error to be discarded with no log call, no
/// propagation, and no comment" is CWE-390 itself, not an equal-weight alternative.
///
/// FAILS BEFORE THE FIX: the rule was `kind = "decision"` with ZERO `violates` options, so both
/// assertions failed. Measured end-to-end at the same time (see
/// `crates/server/tests/w15_reclassified_rule_silence_e2e.rs`): fed a repo of empty catch
/// blocks, the scan emitted no defect at all and instead produced a single `ConventionObserved`
/// row reading `convention_option_id: "allow-silent-discards"` — the fault recorded as the
/// codebase's own convention.
#[tokio::test]
async fn req_w15_silent_failure_is_defect_kind_with_the_discard_option_flagged() {
    let Some(set) = corpus().await else {
        return;
    };
    let r = rule(&set, SILENT_FAILURE);
    assert_eq!(
        r.kind(),
        Some(RuleKind::Defect),
        "{SILENT_FAILURE} must be `defect`: a `decision`-kind rule's only reportable condition \
         is cross-codebase inconsistency, so it can never report a discarded error as a fault"
    );
    assert_exactly_one_violates_on(r, SILENT_FAILURE_DEGENERATE_OPTION);
}

/// req_w15: the owner's explicit `max_severity = "low"` ceiling on
/// `ARCH-OBSERVABILITY-SILENT-FAILURE-1` SURVIVES the reclassification to `defect`.
///
/// This test exists because the reclassification is exactly the moment someone would "fix" the
/// ceiling for consistency. They must not. The cap is an owner directive transcribed from the
/// rule's own prose ("deliberately CAPPED LOW (owner directive)"), and the owner's stated
/// reasoning for the reclassification was that `low` is the correct VOLUME CONTROL for a fault
/// this common. Severity and kind are orthogonal (`RuleKind` classifies how a rule's
/// alternatives relate to its requirement; `max_severity` bounds how loud a finding may be), so
/// a low-severity defect is coherent and intended, not a contradiction to be resolved.
///
/// `min_severity` and `deviation_ceiling` are asserted alongside it as ABSENT, so that
/// "preserve the ceiling" cannot be satisfied by adding a compensating floor elsewhere.
#[tokio::test]
async fn req_w15_silent_failure_rule_keeps_its_owner_directed_low_ceiling() {
    let Some(set) = corpus().await else {
        return;
    };
    let r = rule(&set, SILENT_FAILURE);
    assert_eq!(
        r.max_severity.as_deref(),
        Some("low"),
        "{SILENT_FAILURE}'s `max_severity` is an explicit owner directive and must stay `low` \
         through the `defect` reclassification — a low-severity defect is coherent"
    );
    assert_eq!(
        r.min_severity, None,
        "{SILENT_FAILURE} must not gain a severity FLOOR — that would reintroduce through the \
         back door the volume the `low` ceiling exists to suppress"
    );
    assert_eq!(
        r.deviation_ceiling, None,
        "{SILENT_FAILURE} declares no deviation ceiling and must not gain one here"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Change 2 — JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1 -> default_with_exceptions
// ────────────────────────────────────────────────────────────────────────────

/// req_w15: `JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1` is `default_with_exceptions`-kind and carries
/// exactly one `violates` flag, on the trim-the-array option.
///
/// The kind, in the owner's reading: trimming a dependency array is wrong BY DEFAULT, but a
/// deliberately stale closure is a legitimate technique — so the rule is neither an equal-weight
/// `decision` nor an outright `defect`.
///
/// The FLAG, by contrast, is decided by the rule's own authored text, which condemns its single
/// non-default option rather than carving it out as a justified exception: *"treats the symptom
/// by lying about what the effect reads"*, *"This is the hardest class of React bug"*, *"The
/// correct fixes keep the array complete and control re-runs structurally instead of hiding a
/// real dependency"* — and, in the rule's `[decision] why`, *"A blanket eslint-disable on the
/// deps line is the same lie written down."* Nothing in this rule describes a legitimate reason
/// to trim, so there is nothing here to leave unflagged. The `default_with_exceptions` kind is
/// still the right one and is not merely a synonym for `defect` here: it is what permits a
/// future, genuinely-justified exception to be authored as a THIRD option with
/// `violates = false` alongside this one, which `defect` would forbid.
///
/// FAILS BEFORE THE FIX: `kind = "decision"` with zero `violates` options.
#[tokio::test]
async fn req_w15_exhaustive_deps_is_default_with_exceptions_with_the_trim_option_flagged() {
    let Some(set) = corpus().await else {
        return;
    };
    let r = rule(&set, EXHAUSTIVE_DEPS);
    assert_eq!(
        r.kind(),
        Some(RuleKind::DefaultWithExceptions),
        "{EXHAUSTIVE_DEPS} must be `default_with_exceptions`: its default is externally correct \
         (trimming a deps array is wrong unless justified), which is neither an equal-weight \
         `decision` nor a `defect` with no acceptable form anywhere"
    );
    assert_exactly_one_violates_on(r, EXHAUSTIVE_DEPS_DEGENERATE_OPTION);
}

// ────────────────────────────────────────────────────────────────────────────
// Shared — the adopted default points at the CORRECT option on both
// ────────────────────────────────────────────────────────────────────────────

/// req_w15: on BOTH reclassified rules, the adopted default names the CORRECT option, never the
/// flagged degenerate one.
///
/// This is the check that makes the reclassification safe rather than merely louder. A
/// `defect`-kind rule is excluded from the alternatives pass and behaves as SINGLE-DIRECTIVE —
/// whatever `default_option` resolves to IS the directive the scan judges the codebase against.
/// If that pointed at the degenerate option, the change would promote the fault from "silently
/// recorded as a convention" to "formally adopted as the project's directive", which is worse
/// than the bug being fixed. W12 measured this across its nineteen rules and found 0 wrong; it
/// is measured here rather than assumed.
///
/// Asserted as a triple — the default is SET, it names a REAL option, and that option is NOT
/// flagged — so the invariant cannot be satisfied vacuously by an absent or misspelled default.
#[tokio::test]
async fn req_w15_both_reclassified_rules_adopt_their_correct_option_as_the_default() {
    let Some(set) = corpus().await else {
        return;
    };
    for (id, expected_default) in [
        (SILENT_FAILURE, SILENT_FAILURE_CORRECT_OPTION),
        (EXHAUSTIVE_DEPS, EXHAUSTIVE_DEPS_CORRECT_OPTION),
    ] {
        let r = rule(&set, id);
        let default = r.default_option.as_deref().unwrap_or_else(|| {
            panic!(
                "{id}: no `[decision] default` at all — a defect/default_with_exceptions rule \
                 with no adopted default has no directive to judge a codebase against"
            )
        });
        assert_eq!(
            default, expected_default,
            "{id}: the adopted default is `{default}`, expected `{expected_default}`"
        );
        let opt = r
            .options
            .iter()
            .find(|o| o.id == default)
            .unwrap_or_else(|| {
                panic!(
                    "{id}: the adopted default `{default}` names no real `[[option]]` — the \
                     directive would resolve to nothing"
                )
            });
        assert!(
            !opt.violates,
            "{id}: the adopted default `{default}` is marked `violates` — this rule would adopt \
             its own named fault as the project's effective directive, which is strictly worse \
             than the silence the reclassification set out to fix"
        );
    }
}
