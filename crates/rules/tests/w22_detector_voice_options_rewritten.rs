//! W22 / A5.7 — the two rules whose options described CAMERATA rather than the client's code.
//!
//! # Why these two could not be reclassified like the rest
//!
//! The other A5.7 rules were resolved by changing `kind` and flagging the fault, because
//! their options already named states a repository could be in. These two could not be, and
//! reclassifying them would have been worse than leaving them alone: there was nothing in
//! either option set to flag, because neither option was ever about the scanned repository.
//!
//! - `SUPABASE-AUTH-GETSESSION-SERVER-1` offered "flag `getSession()` in server paths" versus
//!   "flag every `getSession()` call, unscoped". Both are DETECTOR SCOPE. No file in a
//!   client's tree distinguishes them, so the alternatives pass's question — "which option
//!   does this repo follow?" — has no groundable answer, and whichever the model picked
//!   shipped as the client's convention.
//! - `SUPABASE-RLS-INITPLAN-1` offered "recommend the wrap, opt-in only" versus "report it as
//!   a standard security finding". Both describe CAMERATA'S OWN REPORT POLICY. The W14 roster
//!   had already diagnosed this precisely ("how CAMERATA'S OWN REPORT should classify an
//!   initplan finding, not a codebase choice") — it simply was never actionable while the
//!   A5.7 hold stood.
//!
//! # The two shapes they resolve to, and why they differ
//!
//! They do NOT get the same treatment, and the difference is the whole judgement:
//!
//! - `getSession()` on a server path is a FAULT. The rule offers no legitimate alternative —
//!   its own prose says `getSession()` "explicitly does not revalidate the token", which is
//!   CWE-345. So: `kind = "defect"`, one correct state, one flagged state.
//! - A bare `auth.*()` call in a policy predicate is a COST, not a fault. The rule's own
//!   default `why` calls the wrap "a zero-risk, purely mechanical performance win with a real
//!   (if scale-dependent) cost when skipped", and the rule carries `opt_in_only = true`. A
//!   small table genuinely does not care. So it stays `decision`, with two client-state
//!   options and NO `violates` — flagging it would ship a high-severity defect for a
//!   performance choice that is correct at most scales.
//!
//! That asymmetry is why this could not be a bulk pass over "rules with detector-voice
//! options". The voice defect is shared; the right resolution is not.

use camerata_rules::{corpus_path, load_corpus_lenient, RuleKind};

#[tokio::test]
async fn req_w22_getsession_is_a_defect_with_client_state_options() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let rule = corpus
        .get_by_id("SUPABASE-AUTH-GETSESSION-SERVER-1")
        .expect("rule present");

    assert_eq!(
        rule.kind,
        Some(RuleKind::Defect),
        "there is no legitimate alternative to revalidating a token on a server path, so this \
         is a defect, not a choice"
    );

    // The detector-scope ids are gone. Both described how aggressively Camerata should scan.
    for gone in [
        "flag-getsession-in-server-paths",
        "flag-getsession-everywhere-unscoped",
    ] {
        assert!(
            !rule.options.iter().any(|o| o.id == gone),
            "`{gone}` describes detector scope, not a state of the client's code"
        );
    }

    let correct = rule
        .options
        .iter()
        .find(|o| o.id == "server-identity-via-getclaims-or-getuser")
        .expect("the conforming client state must exist");
    assert!(!correct.violates, "the conforming state is never the fault");

    let fault = rule
        .options
        .iter()
        .find(|o| o.id == "server-trusts-getsession-for-identity")
        .expect("the faulty client state must exist");
    assert!(
        fault.violates,
        "trusting an unrevalidated session for a server-side authorization decision is the \
         fault this rule exists to report"
    );

    // Both labels must be decidable from the repository alone (LABEL-3). The tell for the old
    // shape was a label that began by telling a tool what to do.
    for o in &rule.options {
        assert!(
            !o.label.starts_with("Flag ") && !o.label.starts_with("Report "),
            "{} still reads as an instruction to the scanner: {:?}",
            o.id,
            o.label
        );
    }
}

#[tokio::test]
async fn req_w22_initplan_options_describe_policies_not_camerata_reporting() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let rule = corpus
        .get_by_id("SUPABASE-RLS-INITPLAN-1")
        .expect("rule present");

    // Stays a decision. A bare auth call is a scale-dependent cost, not a fault, and the rule
    // is opt-in by its own declaration.
    assert_eq!(rule.kind, Some(RuleKind::Decision));
    assert!(
        rule.opt_in_only,
        "the opt-in placement belongs to this flag, not to an option's prose"
    );
    assert!(
        !rule.options.iter().any(|o| o.violates),
        "a performance optimisation that is correct to skip at small scale must not ship a \
         guaranteed high-severity defect"
    );

    for gone in [
        "wrap-auth-calls-opt-in",
        "bundle-as-a-standard-security-finding",
    ] {
        assert!(
            !rule.options.iter().any(|o| o.id == gone),
            "`{gone}` describes Camerata's own report policy, not the client's policies"
        );
    }

    for want in [
        "auth-calls-wrapped-in-a-scalar-subselect",
        "bare-auth-calls-in-policy-predicates",
    ] {
        assert!(
            rule.options.iter().any(|o| o.id == want),
            "expected the client-state option `{want}`"
        );
    }

    // No option may still be addressed to the tool.
    for o in &rule.options {
        for banned in ["Recommend", "Report", "Flag", "finding"] {
            assert!(
                !o.label.contains(banned),
                "{} still describes Camerata rather than the client's policies: {:?}",
                o.id,
                o.label
            );
        }
    }
}
