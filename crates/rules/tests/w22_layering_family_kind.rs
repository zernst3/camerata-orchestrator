//! W22 — the LAYERING FAMILY declares `default_with_exceptions`, so its degenerate option can
//! actually be flagged.
//!
//! # The defect
//!
//! Nine rules across six stacks say the same thing: transport code delegates to a service
//! layer. All nine carry `deviation_ceiling = "high"`, which is the authors' own marker that
//! they mirror `ARCH-STRICT-LAYERING-1`. Two declared `default_with_exceptions` and flagged
//! their fat-controller option. **Seven declared `decision`** — and `decision` kind FORBIDS
//! `violates` (the `decision_kind_rules_with_violates_flag` build gate), so on those seven the
//! fault could not be flagged at all.
//!
//! The consequence was not a missing finding, which would have been survivable. It was a FALSE
//! one. A repo that genuinely puts business logic in its controllers gets
//! `apply_alternative_decision` rewriting its effective directive to that option, and then
//! `conventions_observed_findings` emits:
//!
//! > The codebase consistently follows "put business logic and data access directly in
//! > handlers" — zero violations were observed this scan.
//!
//! The fault reported back to the client as their own chosen convention, with a clean bill of
//! health attached.
//!
//! # What this test pins, and what it deliberately does not
//!
//! It pins that each listed rule is `default_with_exceptions` with its degenerate option
//! flagged. It does NOT pin the full option set, because the non-degenerate alternatives are
//! the interesting part and must stay unflagged:
//!
//! - `package-by-domain-not-by-layer` (go) is Ben Johnson's "Standard Package Layout" and Mat
//!   Ryer's HTTP-services position — a respected architectural stance on an orthogonal axis
//!   (package organisation, not layering).
//! - `command-handler-per-request` (java) is CQRS.
//! - `transactional-on-repository`, `logic-in-models`, `fat-model-only` each name a context
//!   where they are correct ("Appropriate only for single-statement operations", "Acceptable
//!   for single-model operations", "Works for single-resource operations").
//!
//! Those are the "exceptions" in `default_with_exceptions`. Flagging them would ship a
//! high-severity defect to a client who made a defensible choice, which is the same class of
//! error as the one being fixed, pointing the other way. **The distinguishing test, applied
//! per option rather than per rule: does the option's own `why` name a context in which it is
//! CORRECT? If yes it is an exception; if it only names costs, it is the fault.**

use camerata_rules::{load_corpus_lenient, corpus_path, RuleKind};

/// `(rule id, the one option that is the degenerate form)`.
///
/// `python-django-fat-model-service-1` has TWO: `fat-serializers` is not a milder variant of
/// `fat-views` but the same fault through a different Django seam, and its own `why` says so
/// ("creates the same testability and reuse problem as fat views").
const LAYERING_FAMILY: &[(&str, &[&str])] = &[
    ("GO-HANDLER-SERVICE-REPOSITORY-1", &["logic-in-handlers"]),
    ("JAVA-SPRING-THIN-CONTROLLERS-1", &["fat-controllers-inline-logic"]),
    ("JAVA-SPRING-TRANSACTIONAL-SERVICE-LAYER-1", &["transactional-on-controller"]),
    ("PYTHON-DJANGO-FAT-MODEL-SERVICE-1", &["fat-views", "fat-serializers"]),
    ("PYTHON-FLASK-SERVICE-LAYER-1", &["logic-in-route-handler"]),
    ("PYTHON-SERVICE-LAYER-1", &["logic-in-views"]),
    ("RUBY-RAILS-SKINNY-CONTROLLERS-1", &["fat-controller"]),
    // The two that were already correct, and were the template for the other seven. Pinned
    // here so the family is checked as a family — if one of these is ever loosened back to
    // `decision`, this test says so rather than leaving the next auditor to find nine rules
    // in two shapes and guess which is intended.
    ("CSHARP-ASPNETCORE-THIN-CONTROLLERS-1", &["fat-controllers-inline-logic"]),
    // TWO faults, not one, and the test caught the author of this list assuming otherwise.
    // Skipping the service layer and having no layers at all are distinct states, and
    // neither `why` names a context where it is correct — `controller-accesses-repository-
    // directly` opens "Reduces boilerplate for simple CRUD" and then lists only costs, which
    // is an upside-first `why` (a separate wording defect, tracked in the Phase 3 batch) and
    // not evidence of a legitimate exception.
    ("JAVA-SPRING-LAYERED-ARCHITECTURE-1", &["no-formal-layering", "controller-accesses-repository-directly"]),
];

#[tokio::test]
async fn req_w22_the_layering_family_can_flag_its_degenerate_option() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    for (rule_id, degenerate) in LAYERING_FAMILY {
        let rule = corpus
            .get_by_id(rule_id)
            .unwrap_or_else(|| panic!("{rule_id} is missing from the corpus"));

        assert_eq!(
            rule.kind,
            Some(RuleKind::DefaultWithExceptions),
            "{rule_id} must be `default_with_exceptions`. `decision` kind forbids `violates`, \
             so its degenerate option cannot be flagged, and a repo that genuinely does the \
             wrong thing is told it \"consistently follows\" that as its chosen convention \
             with zero violations observed."
        );

        for want in *degenerate {
            let opt = rule
                .options
                .iter()
                .find(|o| o.id == *want)
                .unwrap_or_else(|| panic!("{rule_id} has no option `{want}`"));
            assert!(
                opt.violates,
                "{rule_id} / {want} is the degenerate form of this rule and must carry \
                 `violates = true`, or a codebase in that state reports as healthy."
            );
        }

        // And the converse, which is the half most at risk of over-correction: every OTHER
        // option stays unflagged. These are legitimate alternatives (CQRS, Ben Johnson's
        // package layout) or narrow-context exceptions, and flagging one ships a
        // high-severity defect to a client who made a defensible choice.
        for opt in &rule.options {
            if degenerate.contains(&opt.id.as_str()) {
                continue;
            }
            assert!(
                !opt.violates,
                "{rule_id} / {} must NOT be flagged — it is an alternative or a \
                 named-context exception, not the fault. See this file's header for the test \
                 that distinguishes them.",
                opt.id
            );
        }
    }
}
