//! W11: the `kind` reclassification that resolved the `default_with_exceptions`/`default = false`
//! contradiction, held against the taxonomy's OWN stated rules.
//!
//! These live in an integration test (rather than `crates/rules/src/lib.rs`'s `mod tests`)
//! deliberately: `lib.rs` carries a concurrent workstream's in-flight W12 security-rule test, and
//! this file is wholly owned by W11 so neither sweeps up the other's uncommitted work.
//!
//! The authority under test is the `RuleKind` doc comment in `crates/rules/src/lib.rs` (~line 147):
//!
//! > `RuleKind::Decision` — the alternatives genuinely hold EQUAL weight (e.g. API versioning
//! > scheme, ID representation, package-by-feature vs. package-by-layer). The only reportable
//! > condition is INCONSISTENCY across the codebase, never "the repo picked option B" by itself.
//! > **No option on a `decision`-kind rule should carry `violates = true`.**
//!
//! and, for `DefaultWithExceptions`, that "one option is correct UNLESS justified" — which only
//! means anything if the rule actually resolves a default AND names at least one deviation.

use camerata_rules::{load_corpus, RuleKind, RuleSet, DEFAULT_CORPUS_PATH};

/// Load the real shipped corpus, or `None` when it is not checked out — mirrors the
/// `path.exists()` skip guard the in-crate `req_a5_1` corpus tests use, so this file behaves
/// identically in a corpus-less checkout rather than failing spuriously.
async fn corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

/// The seven rules W11 reclassified from `default_with_exceptions` to `decision`, because each
/// one's own `why` states the choice follows from project context rather than from a presumed
/// winner, and blesses every option in turn.
const W11_RECLASSIFIED_TO_DECISION: [&str; 7] = [
    // Red before the fix (no `violates` option at all, which is what exposed the `kind` error).
    "JAVA-PACKAGE-BY-FEATURE-1",
    "ARCH-CURSOR-PAGINATION-1",
    // Reclassified, and a contradictory `violates` flag removed as a consequence.
    "ARCH-API-VERSIONING-1",
    "ARCH-API-DTOS-1",
    "RUST-DOMAIN-3",
    "RUST-DIOXUS-14",
    "RUST-SEAORM-INTRA-AGGREGATE-TX-1",
];

/// W11, the taxonomy's own words: "No option on a `decision`-kind rule should carry
/// `violates = true`." Asserted corpus-WIDE, not just over W11's seven — a `decision` rule with a
/// flagged option is a contradiction wherever it appears.
///
/// FAILS BEFORE THE FIX: inverted — before W11 the seven rules below were
/// `default_with_exceptions`, so they were not `decision` rules and this sweep did not see them.
/// It is the pairing of this test with `w11_reclassified_rules_are_decision_kind_and_carry_no_violates_flag`
/// that pins the fix: the latter proves they ARE now `decision`, this one proves that being
/// `decision` costs them every flag.
///
/// TEST DISCIPLINE exception (a), W17: a fourth kind
/// (`RuleKind::DecisionWithFloor`) was added, so the invariant is restated as an EXHAUSTIVE
/// `match` over `Option<RuleKind>` instead of an `== Some(Decision)` comparison. Nothing the
/// test asserted was weakened — `decision` still permits no flag, and the three other kinds
/// still do. What changed is that the fourth kind had to state its answer to be compiled at all,
/// rather than inheriting one by falling past a comparison it does not match. A new variant
/// silently matching a catch-all is how this corpus accumulated inert mechanisms, so the match
/// has no `_` arm and a FIFTH kind will not compile until someone decides.
#[tokio::test]
async fn no_decision_kind_rule_in_the_corpus_carries_a_violates_flag() {
    let Some(set) = corpus().await else {
        return;
    };

    /// Whether a rule of this kind may carry a `violates` option. Mirrors
    /// `RuleKind::permits_violates_flag` DELIBERATELY rather than calling it: this test is the
    /// independent statement of the taxonomy's rule, and asserting it by calling the production
    /// predicate would only prove the predicate equals itself. The equality of the two is
    /// asserted separately, in `w17_decision_with_floor_kind.rs`.
    fn flags_permitted(kind: Option<RuleKind>) -> bool {
        match kind {
            // No acceptable alternative anywhere; the flag records which option is the fault.
            Some(RuleKind::Defect) => true,
            // The premise is that the alternatives hold EQUAL weight. A flag contradicts it.
            Some(RuleKind::Decision) => false,
            // The "exceptions" half of the name IS the degenerate options.
            Some(RuleKind::DefaultWithExceptions) => true,
            // W17: equal-weight options PLUS named faults. The faults are the whole point.
            Some(RuleKind::DecisionWithFloor) => true,
            // `rules_missing_kind` fails the build on this separately; it is not this sweep's
            // business, and treating it as flag-permitting keeps the two gates from overlapping.
            None => true,
        }
    }

    let offenders: Vec<String> = set
        .iter()
        .filter(|r| !flags_permitted(r.kind))
        .filter_map(|r| {
            let flagged: Vec<&str> = r
                .options
                .iter()
                .filter(|o| o.violates)
                .map(|o| o.id.as_str())
                .collect();
            (!flagged.is_empty()).then(|| format!("{} (kind {:?}): {:?}", r.id.0, r.kind, flagged))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "a `decision`-kind rule must carry no `violates` option (RuleKind docs, lib.rs): {offenders:#?}"
    );

    // Guard the sweep against silently passing because the corpus has no decision rules at all.
    let swept = set.iter().filter(|r| !flags_permitted(r.kind)).count();
    assert!(
        swept >= W11_RECLASSIFIED_TO_DECISION.len(),
        "sweep is vacuous: only {swept} rules of a flag-forbidding kind loaded"
    );

    // And the complement, so the sweep is proven to be a PARTITION rather than a filter that
    // happens to select nothing interesting: the flag-permitting kinds must actually be using
    // the flag somewhere, or `flags_permitted` could return true for everything undetected.
    let flagged_elsewhere = set
        .iter()
        .filter(|r| flags_permitted(r.kind))
        .filter(|r| r.options.iter().any(|o| o.violates))
        .count();
    assert!(
        flagged_elsewhere >= 50,
        "only {flagged_elsewhere} flag-PERMITTING rules actually carry a flag; if this collapses, \
         `flags_permitted` is no longer dividing the corpus and the sweep above proves nothing"
    );
}

/// W11: each of the seven rules is now `decision`-kind AND carries no `violates` flag. This is the
/// per-rule counterpart to the corpus-wide sweep — it pins the specific ids, so a later revert of
/// any single rule's `kind` back to `default_with_exceptions` fails here instead of quietly
/// shrinking the sweep's population.
///
/// FAILS BEFORE THE FIX: all seven were `kind = "default_with_exceptions"`, and five of them
/// (every one except the two red ones) additionally carried a `violates` flag on an option their
/// own prose blesses.
#[tokio::test]
async fn w11_reclassified_rules_are_decision_kind_and_carry_no_violates_flag() {
    let Some(set) = corpus().await else {
        return;
    };
    for id in W11_RECLASSIFIED_TO_DECISION {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_eq!(
            rule.kind,
            Some(RuleKind::Decision),
            "{id} must be `decision`-kind: its `why` says the choice follows from context, \
             not from a presumed winner"
        );
        let flagged: Vec<&str> = rule
            .options
            .iter()
            .filter(|o| o.violates)
            .map(|o| o.id.as_str())
            .collect();
        assert!(
            flagged.is_empty(),
            "{id} is `decision`-kind, so no option may carry `violates = true`, found: {flagged:?}"
        );
        // A decision rule still has to offer something to be inconsistent BETWEEN.
        assert!(
            rule.options.len() >= 2,
            "{id} is `decision`-kind but offers {} option(s); inconsistency is only \
             reportable across at least two",
            rule.options.len()
        );
    }
}

/// W11: `ARCH-HOT-READ-CACHE-1` was the one rule in the candidate set where the `default` FIELD
/// was wrong rather than the `kind` — its prose names an adopted default ("Hot reads sit behind a
/// TTL cache with explicit write-side invalidation") and condemns all three alternatives, with no
/// "not from a presumed winner" anywhere. So it stays `default_with_exceptions` and gains the
/// default it always described.
///
/// The `has_default()` assertion is the load-bearing one: `rule_from_toml` only derives
/// `default_option` when `default = true` AND `[decision].default` names an option id, so setting
/// the bool alone would have left the rule claiming a default that resolves to `None`.
///
/// FAILS BEFORE THE FIX: the rule was `default = false` with no `[decision].default` key, so
/// `has_default()` was false while `kind` asserted a default existed.
#[tokio::test]
async fn arch_hot_read_cache_is_default_with_exceptions_with_a_resolvable_default_and_a_flag() {
    let Some(set) = corpus().await else {
        return;
    };
    let id = "ARCH-HOT-READ-CACHE-1";
    let rule = set
        .get_by_id(id)
        .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
    assert_eq!(
        rule.kind,
        Some(RuleKind::DefaultWithExceptions),
        "{id} names an adopted default in its prose and condemns every alternative"
    );
    assert!(
        rule.has_default(),
        "{id} is `default_with_exceptions`, so it must resolve an adopted default option \
         (needs both `default = true` and a `[decision].default` naming a real option id)"
    );
    let default_id = rule
        .default_option
        .as_deref()
        .expect("has_default() implies default_option is Some");
    assert!(
        rule.options.iter().any(|o| o.id == default_id),
        "{id}'s `[decision].default` ({default_id}) must name one of its own options"
    );
    assert!(
        rule.options.iter().any(|o| o.violates),
        "{id} is `default_with_exceptions`, so at least one option must be the degenerate \
         form marked `violates` (this is what `req_a5_1_corpus_has_no_suspicious_\
         default_with_exceptions_rules` gates)"
    );
    // The adopted default is never itself the deviation.
    let default_opt = rule
        .options
        .iter()
        .find(|o| o.id == default_id)
        .expect("checked present above");
    assert!(
        !default_opt.violates,
        "{id}'s adopted default must not be marked `violates`"
    );
}

/// W11, category (c) — the TAXONOMY GAP, pinned so it cannot be silently "resolved" by a later
/// pass without a deliberate decision.
///
/// `INTEGRATION-API-CONTRACT-1` / `INTEGRATION-AUTH-SEAM-1`: exactly two options — the check,
/// and its ABSENCE. That is `defect`-shaped ("every option other than the correct one describes
/// the absence"), but both are deliberately `opt_in_only = true` with `default = false`, a
/// combination the corpus explicitly blesses, so neither the `kind` nor the `default` field is
/// self-evidently the thing that is wrong.
///
/// W11 changed NOTHING on these and reported them for a ruling. This test asserts the as-left
/// state, so the open question stays visible.
///
/// TEST DISCIPLINE exception (a), W17: `RUST-DOMAIN-6` was the third rule on this list and has
/// been REMOVED from it, because the owner resolved its half of the gap. It is now
/// `kind = "decision_with_floor"`, the fourth `RuleKind`, and
/// `w17_decision_with_floor_kind.rs` pins it there. The two `INTEGRATION-*` rules stay: their
/// gap is a DIFFERENT shape and is NOT what the new kind resolves — each has exactly ONE blessed
/// option plus its absence, so there are no equal-weight alternatives for a floor to sit under.
/// Classifying them `decision_with_floor` would assert a choice they do not offer. This test is
/// deliberately not emptied, because the remaining question is still open.
#[tokio::test]
async fn w11_category_c_taxonomy_gap_rules_are_left_untouched_pending_a_ruling() {
    let Some(set) = corpus().await else {
        return;
    };
    for id in ["INTEGRATION-API-CONTRACT-1", "INTEGRATION-AUTH-SEAM-1"] {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_eq!(
            rule.kind,
            Some(RuleKind::DefaultWithExceptions),
            "{id} is an UNRESOLVED taxonomy gap (W11 category c) and must keep its \
             as-authored `kind` until the owner rules on it"
        );
        assert!(
            rule.options.iter().any(|o| o.violates),
            "{id} keeps its `violates` flag(s): its prose names genuine faults among its \
             options, so the flags are not the error here"
        );
    }
}
