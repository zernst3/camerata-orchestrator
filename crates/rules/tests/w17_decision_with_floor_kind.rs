//! W17 item 2: `RuleKind::DecisionWithFloor`, the fourth kind — SEVERAL equally legitimate
//! options PLUS options that are genuine faults under every one of them.
//!
//! ## The gap it closes
//!
//! The three-way taxonomy had no slot for this shape. `decision` forbids `violates` flags, so
//! classifying such a rule there means deleting real fault flags — the gotcha-3 class, where
//! "no RLS policy" shipped as a convention the codebase chose. `default_with_exceptions`
//! requires one privileged default with the rest as deviations FROM it, so classifying it there
//! means fabricating a default that does not exist, and drags in gotcha 2 (`default = false` +
//! DWE is a contradiction). Each forces a false statement about the rule into the corpus.
//!
//! ## Zero new pipeline behaviour — verified, not assumed
//!
//! The claim was that a fourth kind needs no new pipeline behaviour because the two mechanisms
//! that key on `kind` are independent. Read, confirmed at:
//!
//! - `ai_audit::apply_alternative_decision` — `if !option.violates { ... }`. It branches purely
//!   on the DECIDED option's flag and never mentions `kind`. Both the flag-present and
//!   flag-absent paths were therefore already reachable from any kind that enters the pass.
//! - the no-convention escape — `build_alternatives_block`'s `[PURE DECISION]` marker and
//!   `parse_alternative_recommendations`' honoring of a `no_convention` claim. Both were
//!   `a.kind == Some(RuleKind::Decision)` EQUALITY checks, so a new variant is excluded
//!   automatically. W17 replaced both with `RuleKind::offers_no_convention_escape()` so the
//!   exclusion is a stated decision in one exhaustive match rather than an accident of a
//!   comparison, but the behaviour is byte-identical for every pre-existing kind.
//! - eligibility for the pass is `!Rule::is_defect_kind()` inside
//!   `onboard::audit::is_semantic_multi_option_rule`, which is false for the new kind, so it
//!   enters on the ordinary terms.
//!
//! The escape being WITHHELD is a safety decision, not the cheap branch: it severs the per-site
//! violation path in three layers, so granting it would let a repo split between a legitimate
//! option and a named fault answer "no convention" and have the fault go unreported — exactly
//! the A5 defect this taxonomy exists to prevent.
//!
//! ## The candidate population: ONE rule, and the evidence that it is one
//!
//! `RUST-DOMAIN-6` was MOVED. Nothing else was, and `CANDIDATE_SWEEP_*` below records the sweep
//! that found nothing else to move, as data rather than as a claim. The owner approved ADDING
//! the kind, not reclassifying N rules into it.

use camerata_rules::{load_corpus, RuleKind, RuleSet, DEFAULT_CORPUS_PATH};

async fn corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

/// The ONLY rule W17 classified `decision_with_floor`.
const MOVED_TO_DECISION_WITH_FLOOR: [&str; 1] = ["RUST-DOMAIN-6"];

/// CANDIDATE SWEEP, part 1 — the 17 rules whose own `[decision].why` uses the "several
/// legitimate shapes, no presumed winner" phrasing AND which are already `kind = "decision"`
/// with ZERO `violates` flags.
///
/// These are the near-miss population: correctly classified `decision`, no floor named, nothing
/// to move. They matter to the sweep because they are what `RUST-DOMAIN-6` was measured AGAINST
/// — 18 rules in the corpus use that phrasing, 17 of them carry no flag, and exactly one
/// combines the phrasing with flags. That one-of-eighteen split is the whole argument for the
/// kind existing and for it having one member.
///
/// Two were read closely because a keyword scan flagged fault-sounding language in their
/// `[decision].why`, and both are genuine `decision` rules — the language describes a
/// TRADE-OFF COST, not a degenerate option:
///   - `ARCH-CURSOR-PAGINATION-1` ("a reader paging through must not silently skip or repeat
///     rows") — a property of one option, not a condemnation of another. All four options are
///     blessed.
///   - `JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1` ("reaching for one when the other fits is
///     the cost to avoid") — names mis-selection as the cost, not an option as a fault. Both
///     options are blessed.
const CANDIDATE_SWEEP_DECISION_RULES_WITH_NO_FLOOR: [&str; 17] = [
    "ARCH-API-DTOS-1",
    "ARCH-API-VERSIONING-1",
    "ARCH-CURSOR-PAGINATION-1",
    "ARCH-MONOLITH-FIRST-1",
    "CSHARP-ASPNETCORE-MINIMAL-API-VS-CONTROLLERS-1",
    "JAVA-PACKAGE-BY-FEATURE-1",
    "JAVASCRIPT-ANGULAR-REACTIVE-FORMS-1",
    "JAVASCRIPT-ANGULAR-STANDALONE-COMPONENTS-1",
    "JAVASCRIPT-REACT-FUNCTION-COMPONENTS-1",
    "JAVASCRIPT-VUE-COMPOSITION-SCRIPT-SETUP-1",
    "JAVASCRIPT-VUE-STORE-PINIA-SHARED-STATE-1",
    "RUST-DIOXUS-1",
    "RUST-DIOXUS-9",
    "RUST-DOMAIN-1",
    "RUST-DOMAIN-2",
    "RUST-ENTITIES-13",
    "RUST-ENTITIES-3",
];

/// CANDIDATE SWEEP, part 2 — the rules examined and REJECTED as candidates, each with the
/// self-evidence that rejected it. Recorded here rather than in a report, so a later pass
/// cannot quietly sweep them in.
///
/// - The **50 other rules** with >= 2 non-`violates` options and >= 1 `violates` option (the
///   structural shape that superficially resembles the new kind) all carry `default = true` AND
///   a resolvable `[decision].default`. A privileged default exists, so
///   `default_with_exceptions` is the honest classification and nothing is being forced.
///   `RUST-DOMAIN-6` was the ONLY one of the 51 with `default = false` and no
///   `[decision].default`; that single discriminator separates it from all 50.
/// - The **7 GATE-1b roster rules** (`decision`-kind with `Rejected:` prose:
///   `ARCH-OBSERVABILITY-CORRELATION-ID-1`, `ARCH-SERVER-CLIENT-PROJECTION-1`,
///   `SEC-NO-RAW-SQL-CONCAT-1`, `SUPABASE-AUTH-EMAIL-CONFIRMATION-1`,
///   `SUPABASE-AUTH-GETSESSION-SERVER-1`, `SUPABASE-RLS-INITPLAN-1`) each have exactly ONE
///   blessed option plus one or two `Rejected:` ones, and a resolvable `[decision].default`.
///   One correct answer and the rest faults is `defect`/`default_with_exceptions` shape, not
///   this one — there are no equal-weight alternatives for a floor to sit beneath. They remain
///   on the owner's paused A5.7 classification hold, where they were.
/// - The **2 remaining W11 category-(c) rules** (`INTEGRATION-API-CONTRACT-1`,
///   `INTEGRATION-AUTH-SEAM-1`) have exactly two options: the check and its absence. Also one
///   blessed option, also not this shape. Their gap is real and is a DIFFERENT gap; they stay
///   pinned in `w11_kind_taxonomy.rs`.
const CANDIDATE_SWEEP_REJECTED_NOT_THIS_SHAPE: [&str; 8] = [
    "ARCH-OBSERVABILITY-CORRELATION-ID-1",
    "ARCH-SERVER-CLIENT-PROJECTION-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "SUPABASE-AUTH-EMAIL-CONFIRMATION-1",
    "SUPABASE-AUTH-GETSESSION-SERVER-1",
    "SUPABASE-RLS-INITPLAN-1",
    "INTEGRATION-API-CONTRACT-1",
    "INTEGRATION-AUTH-SEAM-1",
];

/// W17 item 2: `RUST-DOMAIN-6` is `decision_with_floor`, and carries the shape that kind
/// describes — at least two unflagged options, at least one flagged, and NO adopted default.
///
/// The `has_default()` assertion is the one that proves the move was needed rather than
/// cosmetic: as `default_with_exceptions` this rule asserted a default existed while
/// `default = false` and no `[decision].default` meant none resolved. That is gotcha 2, and the
/// new kind removes the contradiction by not claiming a default at all.
///
/// FAILS BEFORE THE FIX: the rule was `kind = "default_with_exceptions"`. Proven by
/// `git stash push -- crates/rules/principles/` and re-running.
#[tokio::test]
async fn req_w17_rust_domain_6_is_decision_with_floor() {
    let Some(set) = corpus().await else {
        return;
    };
    let id = MOVED_TO_DECISION_WITH_FLOOR[0];
    let rule = set
        .get_by_id(id)
        .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));

    assert_eq!(
        rule.kind,
        Some(RuleKind::DecisionWithFloor),
        "{id}'s own `why` holds both halves: organizing errors by category or by \
         module/aggregate is \"a genuine organizational choice\" that follows \"not from a \
         presumed winner\", while three other shapes are \"not organizational choices but \
         boundary failures ... the options to avoid regardless of which organization is chosen\""
    );

    let blessed: Vec<&str> = rule
        .options
        .iter()
        .filter(|o| !o.violates)
        .map(|o| o.id.as_str())
        .collect();
    let flagged: Vec<&str> = rule
        .options
        .iter()
        .filter(|o| o.violates)
        .map(|o| o.id.as_str())
        .collect();
    assert!(
        blessed.len() >= 2,
        "{id} needs at least TWO equal-weight options or there is no decision to make, found: \
         {blessed:?}"
    );
    assert!(
        !flagged.is_empty(),
        "{id} needs at least one named fault or it is a plain `decision`, found none"
    );
    assert!(
        !rule.has_default(),
        "{id} must resolve NO adopted default — that is the whole distinction from \
         `default_with_exceptions`, and claiming one while `default = false` is gotcha 2"
    );

    // Every flagged option follows the `Rejected:`-adjacent fault-vocabulary discipline in
    // substance: its `why` must condemn the shape, not bless it. Asserted as the absence of
    // blessing vocabulary, because this rule's authors used "defeats"/"breaks"/"eliminates"
    // rather than the literal token, and GATE 1 only binds options that DO use the token.
    for oid in &flagged {
        let opt = rule
            .options
            .iter()
            .find(|o| &o.id == oid)
            .expect("just collected");
        let why = opt.why.to_lowercase();
        for blessing in ["the natural fit", "a legitimate choice", "acceptable when"] {
            assert!(
                !why.contains(blessing),
                "{id} / {oid} is flagged `violates` but its `why` blesses it with {blessing:?} — \
                 one of the two is wrong, and in the 244-rule audit it was the flag three times \
                 out of four"
            );
        }
    }
}

/// W17 item 2: exactly ONE corpus rule is `decision_with_floor`, and it is `RUST-DOMAIN-6`.
///
/// The upper bound is the load-bearing half. The owner approved adding the KIND, not a sweep,
/// and a kind that quietly accumulates members between reviews is how a taxonomy stops
/// describing the corpus. Growth fails here and should land as a deliberate roster edit with the
/// per-rule self-evidence, exactly as GATE 1b's roster does.
#[tokio::test]
async fn req_w17_decision_with_floor_has_exactly_one_member() {
    let Some(set) = corpus().await else {
        return;
    };
    let members: Vec<&str> = set
        .iter()
        .filter(|r| r.kind == Some(RuleKind::DecisionWithFloor))
        .map(|r| r.id.0.as_str())
        .collect();
    assert_eq!(
        members, MOVED_TO_DECISION_WITH_FLOOR,
        "`decision_with_floor` must have exactly the approved membership. Adding a rule is a \
         classification decision, not a drive-by: record the self-evidence in \
         MOVED_TO_DECISION_WITH_FLOOR.\nfound:    {members:#?}\nexpected: {:#?}",
        MOVED_TO_DECISION_WITH_FLOOR
    );
}

/// W17 item 2, the CANDIDATE SWEEP as an assertion: the near-miss population is still
/// near-miss. Every rule the sweep examined and rejected is still classified as it was.
///
/// This is what makes the reported candidate population auditable rather than a paragraph.
/// The 17 `decision` rules must still be `decision` with zero flags (if one acquires a flag, it
/// becomes a genuine candidate and someone must look); the 8 rejected rules must still NOT be
/// `decision_with_floor` (if one drifts in, the reasoning recorded above was overridden
/// silently).
#[tokio::test]
async fn req_w17_the_decision_with_floor_candidate_sweep_still_holds() {
    let Some(set) = corpus().await else {
        return;
    };

    for id in CANDIDATE_SWEEP_DECISION_RULES_WITH_NO_FLOOR {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_eq!(
            rule.kind,
            Some(RuleKind::Decision),
            "{id} was swept as a `decision` rule with no floor named; if its kind changed, the \
             W17 candidate analysis no longer describes the corpus"
        );
        let flagged: Vec<&str> = rule
            .options
            .iter()
            .filter(|o| o.violates)
            .map(|o| o.id.as_str())
            .collect();
        assert!(
            flagged.is_empty(),
            "{id} has acquired a `violates` flag. It uses the \"no presumed winner\" phrasing, \
             so phrasing + flag is now the `decision_with_floor` shape and it needs a \
             classification decision — not a flag on a `decision` rule, which GATE 2 forbids. \
             Found: {flagged:?}"
        );
    }

    for id in CANDIDATE_SWEEP_REJECTED_NOT_THIS_SHAPE {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_ne!(
            rule.kind,
            Some(RuleKind::DecisionWithFloor),
            "{id} was examined and REJECTED as a `decision_with_floor` candidate: it offers \
             exactly ONE blessed option, so there are no equal-weight alternatives for a floor \
             to sit beneath. Moving it here overrides that reading and must be deliberate"
        );
    }
}

/// W17 item 2: the new kind's pipeline contract, as the doc comment states it.
///
/// Asserted per variant over the WHOLE taxonomy rather than only for the new one, so the test
/// states the matrix and any kind's behaviour changing lands here. The two predicates are the
/// only kind-keyed branches in the pipeline:
/// `permits_violates_flag` scopes GATE 1 and `offers_no_convention_escape` gates the
/// `[PURE DECISION]` marker and the parser's `no_convention` honoring in
/// `camerata_server::ai_audit`.
///
/// This also discharges the duplication in `w11_kind_taxonomy.rs`, whose
/// `no_decision_kind_rule_in_the_corpus_carries_a_violates_flag` restates the flag rule in its
/// own local `match` deliberately (asserting it by calling the production predicate would only
/// prove the predicate equals itself). The two are checked to AGREE here, which is the honest
/// way to hold an independent restatement and its implementation together.
#[test]
fn req_w17_the_four_kinds_pipeline_contract() {
    // (kind, permits a `violates` flag, offered the no-convention escape)
    let matrix = [
        (RuleKind::Defect, true, false),
        (RuleKind::Decision, false, true),
        (RuleKind::DefaultWithExceptions, true, false),
        (RuleKind::DecisionWithFloor, true, false),
    ];
    assert_eq!(
        matrix.len(),
        RuleKind::ALL.len(),
        "the matrix must cover every variant; a new kind belongs in both"
    );
    for (kind, permits_flag, offers_escape) in matrix {
        assert!(
            RuleKind::ALL.contains(&kind),
            "{kind} must be listed in RuleKind::ALL"
        );
        assert_eq!(
            kind.permits_violates_flag(),
            permits_flag,
            "{kind}: permits_violates_flag must be {permits_flag}"
        );
        assert_eq!(
            kind.offers_no_convention_escape(),
            offers_escape,
            "{kind}: offers_no_convention_escape must be {offers_escape}. The escape severs the \
             per-site violation path in three layers, so it is honest ONLY where no option is a \
             fault — `decision` alone"
        );
    }

    // `decision_with_floor` is the only kind that both permits a flag AND is not `defect`'s
    // never-enters-the-pass case while also having no privileged default. Stated as the
    // distinction it is, so the new value cannot collapse into a synonym for an existing one.
    assert!(
        RuleKind::DecisionWithFloor.permits_violates_flag()
            && !RuleKind::DecisionWithFloor.offers_no_convention_escape(),
        "the new kind's defining combination: flags permitted, escape withheld"
    );
    assert_ne!(
        RuleKind::DecisionWithFloor.as_str(),
        RuleKind::Decision.as_str(),
        "the two must be distinguishable on the wire"
    );
}
