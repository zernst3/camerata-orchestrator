//! W22 / A5.7 — rules whose options already describe the CLIENT'S CODE, but whose `kind` said
//! those options were co-equal choices.
//!
//! # The A5.7 hold, and why it is lifted
//!
//! `DECISION_KIND_RULES_WITH_REJECTED_PROSE` in `w14_corpus_schema_gates.rs` pins a roster of
//! `decision`-kind rules carrying an option whose `why` opens with the literal token
//! `Rejected:`. That shape is self-contradicting: `decision` means the options are equally
//! legitimate answers to an open question, and `Rejected:` says one of them is not. The
//! roster existed because resolving each one is a judgement about what the rule MEANS, which
//! was the owner's call. That hold was lifted on 2026-10-19.
//!
//! # What goes wrong while the kind is wrong
//!
//! `decision` kind forbids `violates` (the `decision_kind_rules_with_violates_flag` build
//! gate), so a rule in this shape cannot flag the state its own prose rejects. A codebase
//! actually in that state is then decided onto the rejected option by the alternatives pass,
//! and `conventions_observed_findings` reports it to the client as the convention they
//! consistently follow, with zero violations observed. The rule's own text says the state is
//! wrong and the report says the client is fine.
//!
//! # Scope of THIS file
//!
//! The two rules whose options were already correct — they describe states of the client's
//! code, and the `why` on the rejected ones names only costs, never a context in which they
//! are right. Only the `kind` and the flags needed to change.
//!
//! Deliberately NOT here, because they need their OPTIONS rewritten rather than reclassified:
//! `SUPABASE-AUTH-GETSESSION-SERVER-1` (both options are detector scope — "flag narrowly" vs
//! "flag everywhere" — so there is no client state to flag) and `SUPABASE-RLS-INITPLAN-1`
//! (both describe Camerata's own report policy). Reclassifying those without rewriting them
//! would flag an option that was never about the client's code in the first place.
//!
//! Also deliberately NOT here, by prior rulings recorded in the roster itself:
//! `ARCH-OBSERVABILITY-CORRELATION-ID-1` stays `decision`, and `SEC-NO-RAW-SQL-CONCAT-1 /
//! escape-then-concatenate` is the roster's own documented "genuine finding the 244-rule
//! audit missed".

use camerata_rules::{corpus_path, load_corpus_lenient, RuleKind};

/// `(rule id, options that are faults and must be flagged)`.
const RESOLVED: &[(&str, &[&str])] = &[
    // Both rejected options ship the full object to the browser and differ only in when they
    // pretend otherwise: one trusts the UI not to render extra fields, the other strips them
    // in client-side JS after the data has already been transmitted. Neither is a context in
    // which the choice is correct, which is what distinguishes a fault from an exception.
    (
        "ARCH-SERVER-CLIENT-PROJECTION-1",
        &[
            "pass-raw-query-result-rely-on-ui-to-not-render-extra-fields",
            "strip-sensitive-fields-client-side-after-hydration",
        ],
    ),
    // "Carry the local-dev scaffold default into the production-linked configuration
    // unreviewed" is a real state a repository can be in, and the rule's own `why` calls it
    // "precisely the gap the rule targets".
    (
        "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
        &["leave-confirmations-off-matching-local-dev-default"],
    ),
];

#[tokio::test]
async fn req_w22_a57_resolved_rules_can_flag_the_state_their_own_prose_rejects() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    for (rule_id, faults) in RESOLVED {
        let rule = corpus
            .get_by_id(rule_id)
            .unwrap_or_else(|| panic!("{rule_id} is missing from the corpus"));

        assert_ne!(
            rule.kind,
            Some(RuleKind::Decision),
            "{rule_id} must not be `decision`: that kind forbids `violates`, so the state its \
             own `why` opens by REJECTING cannot be flagged, and a codebase in that state is \
             told it consistently follows it with zero violations observed."
        );

        for want in *faults {
            let opt = rule
                .options
                .iter()
                .find(|o| o.id == *want)
                .unwrap_or_else(|| panic!("{rule_id} has no option `{want}`"));
            assert!(
                opt.violates,
                "{rule_id} / {want} must carry `violates = true` — its own `why` opens \
                 `Rejected:`, so the corpus already says it is a fault; only the flag was \
                 missing."
            );
        }

        // The converse half. Over-correction here is as damaging as the original defect: the
        // default option describes the state the rule WANTS, and flagging it would report
        // every conforming codebase as defective.
        for opt in &rule.options {
            if faults.contains(&opt.id.as_str()) {
                continue;
            }
            assert!(
                !opt.violates,
                "{rule_id} / {} must NOT be flagged — it is the conforming state or a \
                 legitimate alternative.",
                opt.id
            );
        }
    }
}

/// The roster and the corpus must not drift. Once a rule is resolved here, its entries must be
/// gone from `DECISION_KIND_RULES_WITH_REJECTED_PROSE`, because that roster asserts equality
/// in BOTH directions — a resolved rule left on it fails the W14 gate, and a rule silently
/// dropped from it without being resolved loses its pin.
#[tokio::test]
async fn req_w22_a57_resolved_rules_are_no_longer_decision_kind_so_the_roster_cannot_list_them() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let roster_src = include_str!("w14_corpus_schema_gates.rs");
    for (rule_id, _) in RESOLVED {
        let rule = corpus.get_by_id(rule_id).expect("present");
        assert_ne!(rule.kind, Some(RuleKind::Decision));

        // The roster lists entries as `RULE-ID / option-id`. A resolved rule may still be
        // MENTIONED in a comment recording why it was removed — that is the house style and
        // is wanted — so this checks for a live list entry, i.e. the id inside a quoted
        // string followed by ` / `.
        let live_entry = format!("\"{rule_id} / ");
        assert!(
            !roster_src.contains(&live_entry),
            "{rule_id} is resolved ({:?}) but still has a LIVE entry in \
             DECISION_KIND_RULES_WITH_REJECTED_PROSE. That roster asserts equality in both \
             directions, so it must be edited deliberately when a rule leaves it — see the \
             worked precedent for ARCH-OBSERVABILITY-SILENT-FAILURE-1 in that file.",
            rule.kind
        );
    }
}
