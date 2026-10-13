//! W14: the three corpus-load gates that stop the 244-rule `violates` audit's error classes
//! from returning. 76 of 244 flags (31 percent) were wrong, the dominant shape being a MISSING
//! flag — the engine silent on a fault its own rule describes in plain language. Hand-auditing
//! 244 rules is not repeatable; a build failure is.
//!
//! - **Gate 1** — an option whose `why` opens with `Rejected:` must carry `violates = true`, on
//!   any rule whose `kind` permits the flag
//!   (`camerata_rules::options_with_rejected_prose_but_no_violates_flag`).
//! - **Gate 1b** — ADVISORY: a `decision`-kind rule with a `Rejected:` option is evidence its
//!   `kind` is wrong; pinned to a known roster rather than failed, because resolving it means a
//!   reclassification the owner has paused (A5.7).
//! - **Gate 2** — no option on a `decision`-kind rule carries `violates = true`
//!   (`camerata_rules::decision_kind_rules_with_violates_flag`).
//! - **Gate 3** — no TOML file in the corpus carries an unknown FIELD NAME at any nesting level
//!   (`camerata_rules::corpus_files_with_unknown_fields`).
//!
//! These live in an integration test rather than `crates/rules/src/lib.rs`'s `mod tests` for the
//! same reason `w11_kind_taxonomy.rs` does: `lib.rs` carries concurrent workstreams' in-flight
//! tests, and a file wholly owned by W14 means neither sweeps up the other's work.
//!
//! Every gate here is proven to FIRE, not merely to pass, by building a synthetic corpus in a
//! temp dir that contains exactly the violation the gate targets and asserting the gate reports
//! it. A gate that cannot fail is the same failure class as the mechanisms this corpus has
//! already shipped inert three times.

use camerata_rules::{
    corpus_files_with_unknown_fields, corpus_rules_with_qualifies_misplaced_into_sources,
    decision_kind_rule_count, decision_kind_rules_with_rejected_prose,
    decision_kind_rules_with_violates_flag, load_corpus,
    options_with_rejected_prose_but_no_violates_flag, RuleKind, RuleSet, DEFAULT_CORPUS_PATH,
};

/// Load the real shipped corpus, or `None` when it is not checked out — mirrors the
/// `path.exists()` skip guard the in-crate `req_a5_1` corpus tests and `w11_kind_taxonomy.rs`
/// use, so this file behaves identically in a corpus-less checkout rather than failing
/// spuriously.
async fn corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

fn corpus_dir() -> Option<&'static std::path::Path> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    path.exists().then_some(path)
}

// ────────────────────────────────────────────────────────────────────────────
// Synthetic-corpus helpers: the FIRE proofs
// ────────────────────────────────────────────────────────────────────────────

/// A minimal, VALID two-option rule TOML. `kind`, the second option's `violates` line and its
/// `why` are parameterized so each fire-proof can introduce exactly one defect and nothing else.
/// Everything else is held at a value the other corpus gates accept, so a fire proof can never
/// pass for an unrelated reason.
fn rule_toml(kind: &str, second_why: &str, second_violates: bool) -> String {
    let violates_line = if second_violates {
        "violates = true"
    } else {
        ""
    };
    format!(
        r#"
id = "W14-FIXTURE-1"
title = "W14 synthetic fixture rule"
tag = "universal"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "{kind}"
default = true
verification = "grounded"

[decision]
question = "Synthetic?"
default = "the-correct-option"

[[option]]
id = "the-correct-option"
label = "The correct option"
directive = "Do the correct thing."
why = "The natural fit for the synthetic fixture."
remediation = "Do the correct thing. Verify by observing the correct thing."
effort = "low"

[[option]]
id = "the-other-option"
label = "The other option"
{violates_line}
directive = "Do the other thing."
why = "{second_why}"
remediation = "Move to the correct option. Verify by observing the correct thing."
effort = "low"
"#
    )
}

/// Write `files` into a fresh temp corpus dir and return the dir (keep it alive while reading).
fn synthetic_corpus(files: &[(&str, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    for (name, body) in files {
        std::fs::write(universal.join(name), body).expect("write fixture");
    }
    dir
}

// ────────────────────────────────────────────────────────────────────────────
// GATE 1 — `Rejected:` prose implies the `violates` flag
// ────────────────────────────────────────────────────────────────────────────

/// req_w14 / GATE 1, against the REAL shipped corpus: no option whose `why` opens with
/// `Rejected:` may lack `violates = true`, on any rule whose `kind` permits the flag.
///
/// This is the gate against the audit's dominant error class. The `Rejected:` convention is the
/// strongest signal available: all 17 flagged `supabase/` rules used it and that folder scored
/// 17 of 17 correct while other folders ran as low as 59 percent.
///
/// Scope note (not an exception — see the function's doc comment): `decision`-kind rules are
/// excluded because the taxonomy FORBIDS the flag there, so demanding it would make Gate 1 and
/// Gate 2 contradict each other on the same option. Those rules are reported by
/// `req_w14_decision_kind_rules_with_rejected_prose_match_the_known_roster` instead.
///
/// Measured on 2026-10-11: 70 in-scope options open with the token, all 70 flagged — zero
/// offenders, zero counter-examples.
#[tokio::test]
async fn req_w14_every_rejected_option_is_flagged_violates() {
    let Some(set) = corpus().await else {
        return;
    };
    let offenders = options_with_rejected_prose_but_no_violates_flag(&set);
    assert!(
        offenders.is_empty(),
        "an option whose `why` opens with `Rejected:` must carry `violates = true` \
         (docs/RULES_REFERENCE.md section 10.4):\n  {}",
        offenders.join("\n  ")
    );

    // Guard against passing VACUOUSLY: an empty offender list means nothing if no in-scope
    // option uses the convention at all. 70 did on 2026-10-11; require a clear majority of
    // that so a corpus edit that gutted the convention fails here rather than going quiet.
    let in_scope_rejected = set
        .iter()
        .filter(|r| r.kind != Some(RuleKind::Decision))
        .flat_map(|r| r.options.iter())
        .filter(|o| o.why.trim_start().starts_with("Rejected:"))
        .count();
    assert!(
        in_scope_rejected >= 50,
        "gate is vacuous: only {in_scope_rejected} in-scope options use the `Rejected:` \
         convention (70 did on 2026-10-11)"
    );
}

/// req_w14 / GATE 1 FIRE PROOF: the gate reports a `defect`-kind rule whose `Rejected:` option
/// is missing its flag. Independent of corpus state, so Gate 1 can never go inert the way the
/// `max_severity` mechanism did (shipped complete, fully tested, and matched by zero rules).
///
/// Three cases in one test because each is a distinct branch and a weaker test would let two
/// regress silently: the defect FIRES, the correctly-authored pair does NOT, and the
/// `decision`-kind rule is OUT OF SCOPE (Gate 2's business, not Gate 1's).
#[tokio::test]
async fn req_w14_gate_1_fires_on_a_rejected_option_with_no_flag() {
    // (a) defect-kind, `Rejected:` prose, NO flag → must fire.
    let dir = synthetic_corpus(&[(
        "w14-fixture-1.toml",
        rule_toml(
            "defect",
            "Rejected: this is the fault the rule exists to prevent.",
            false,
        ),
    )]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    let offenders = options_with_rejected_prose_but_no_violates_flag(&set);
    assert_eq!(
        offenders.len(),
        1,
        "gate 1 must fire on a `Rejected:` option with no flag, got: {offenders:?}"
    );
    assert!(
        offenders[0].contains("W14-FIXTURE-1") && offenders[0].contains("the-other-option"),
        "the offender line must name the rule AND the option: {:?}",
        offenders[0]
    );

    // (b) the same rule, correctly authored → must NOT fire.
    let dir = synthetic_corpus(&[(
        "w14-fixture-1.toml",
        rule_toml(
            "defect",
            "Rejected: this is the fault the rule exists to prevent.",
            true,
        ),
    )]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    assert!(
        options_with_rejected_prose_but_no_violates_flag(&set).is_empty(),
        "gate 1 must not fire when the `Rejected:` option carries the flag"
    );

    // (c) decision-kind, `Rejected:` prose, no flag → OUT OF SCOPE for gate 1, because the
    //     taxonomy forbids the flag there. Gate 1b reports it; gate 1 must stay silent, or the
    //     two gates would demand opposite things of the same option.
    let dir = synthetic_corpus(&[(
        "w14-fixture-1.toml",
        rule_toml("decision", "Rejected: prose and kind disagree here.", false),
    )]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    assert!(
        options_with_rejected_prose_but_no_violates_flag(&set).is_empty(),
        "gate 1 must not fire on a `decision`-kind rule — gate 2 forbids the flag there"
    );
    assert_eq!(
        decision_kind_rules_with_rejected_prose(&set).len(),
        1,
        "gate 1b must be the one that reports a `Rejected:` option on a `decision` rule"
    );
}

/// req_w14 / GATE 1 FIRE PROOF, the block-string form. The corpus authors `why` both inline and
/// as a `"""` block (which begins with a newline), so a convention that only held for one
/// quoting style would be a convention in name only. Pins that the gate trims leading
/// whitespace before matching.
#[tokio::test]
async fn req_w14_gate_1_matches_rejected_in_a_block_string_why() {
    let body = r#"
id = "W14-BLOCK-1"
title = "W14 block-string fixture"
domain = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[[option]]
id = "the-other-option"
label = "The other option"
why = """
Rejected: authored as a block string, which starts with a newline.
"""
"#;
    let dir = synthetic_corpus(&[("w14-block-1.toml", body.to_string())]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    assert_eq!(
        options_with_rejected_prose_but_no_violates_flag(&set).len(),
        1,
        "a block-string `why` beginning with a newline then `Rejected:` must still match"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// GATE 1b — ADVISORY: `Rejected:` prose on a `decision`-kind rule
// ────────────────────────────────────────────────────────────────────────────

/// The 8 options, across 7 rules, whose `why` opens with `Rejected:` on a rule classified
/// `kind = "decision"` as of 2026-10-11. A rule cannot hold both that its alternatives are of
/// EQUAL WEIGHT and that one of them is rejected; one of the two is wrong.
///
/// This roster is PINNED rather than fixed because resolving each entry means changing a rule's
/// `kind`, and 39 rules sit on the owner's paused A5.7 classification hold. Six of these seven
/// rules are on that hold list (`docs/plans/2026-10-09_rule-kind-classification.md`, "NEEDS
/// OWNER DECISION (58 rules)"). The seventh is not, and is the genuine finding this gate
/// surfaced that the 244-rule audit missed:
///
/// - `SEC-NO-RAW-SQL-CONCAT-1` was classified a CLEAR `decision` (it sits in the classification
///   doc's `decision (110)` section, NOT on the hold list) while being a CWE-89 SQL-injection
///   rule with exactly 2 options, one opening `Rejected:`, and ZERO `violates` flags. That is
///   precisely the W12 shape that saw 19 security rules reclassified to `defect` because a
///   `decision` rule with no flag cannot report a violation at all. It was not among the 19.
///   Its `enforcement = "mechanical"` does soften the live impact — a CI-tier rule never enters
///   the alternatives pass, so the flag would be inert today and the rule is still checked
///   against its correct default option's directive — but the classification contradicts the
///   rule's own prose, and the silence becomes real the moment its enforcement tier changes.
///
/// Hardcoded deliberately rather than derived: a derived list moves with the corpus and could
/// not catch a NEW rule drifting into this shape, which is exactly what this roster prevents.
const DECISION_KIND_RULES_WITH_REJECTED_PROSE: [&str; 7] = [
    // On the A5.7 hold list: "the rule's own question is phrased as a yes/no compliance check,
    // not an open style choice — could be an under-authored default_with_exceptions".
    "ARCH-OBSERVABILITY-CORRELATION-ID-1 / correlate-after-the-fact-by-timestamp-and-content",
    //
    // RESOLVED 2026-10-12 by the owner's A5.7 ruling, and REMOVED from this roster as the
    // deliberate roster edit this gate's doc comment calls for ("Shrinkage means someone
    // resolved one, which is the owner's call under the A5.7 hold and should land as a
    // deliberate roster edit, not silently"):
    //   ARCH-OBSERVABILITY-SILENT-FAILURE-1 / allow-silent-discards
    // It is now `kind = "defect"` with `violates = true` on that option, so it is no longer a
    // `decision`-kind rule and GATE 1b no longer has anything to say about it. GATE 1 ("every
    // `Rejected:` option on a flag-permitting kind carries the flag") now covers it instead,
    // and `req_w15_silent_failure_is_defect_kind_with_the_discard_option_flagged` pins it.
    // `ARCH-OBSERVABILITY-CORRELATION-ID-1` above stays `decision` by the same ruling.
    //
    "ARCH-SERVER-CLIENT-PROJECTION-1 / pass-raw-query-result-rely-on-ui-to-not-render-extra-fields",
    "ARCH-SERVER-CLIENT-PROJECTION-1 / strip-sensitive-fields-client-side-after-hydration",
    // NOT on the hold list — classified a clear `decision`. See the doc comment above.
    "SEC-NO-RAW-SQL-CONCAT-1 / escape-then-concatenate",
    // On the A5.7 hold list.
    "SUPABASE-AUTH-EMAIL-CONFIRMATION-1 / leave-confirmations-off-matching-local-dev-default",
    // On the A5.7 hold list, and a distinct sub-shape: this rule's alternatives describe
    // DETECTOR SCOPE (flag everywhere vs. only server paths), not the scanned repo's own
    // behaviour, so `violates` has nothing to mean on it in either direction.
    "SUPABASE-AUTH-GETSESSION-SERVER-1 / flag-getsession-everywhere-unscoped",
    // On the A5.7 hold list, same detector/reporting-policy sub-shape: the options describe how
    // CAMERATA'S OWN REPORT should classify an initplan finding, not a codebase choice.
    "SUPABASE-RLS-INITPLAN-1 / bundle-as-a-standard-security-finding",
];

/// req_w14 / GATE 1b, against the REAL shipped corpus: the set of `decision`-kind rules with
/// `Rejected:` prose is EXACTLY the known roster — no larger, and no smaller.
///
/// Asserting equality in BOTH directions is the point. Growth means a new rule drifted into a
/// shape its own prose contradicts, and must be caught at the moment it is authored rather than
/// by the next hand audit. Shrinkage means someone resolved one, which is the owner's call under
/// the A5.7 hold and should land as a deliberate roster edit, not silently.
#[tokio::test]
async fn req_w14_decision_kind_rules_with_rejected_prose_match_the_known_roster() {
    let Some(set) = corpus().await else {
        return;
    };
    let found = decision_kind_rules_with_rejected_prose(&set);
    let expected: Vec<String> = DECISION_KIND_RULES_WITH_REJECTED_PROSE
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        found, expected,
        "a `decision`-kind rule with a `Rejected:` option contradicts its own kind. The known \
         roster is held pending the owner's A5.7 ruling; this set must not change silently.\n\
         found:    {found:#?}\nexpected: {expected:#?}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// GATE 2 — no `violates` on a `decision`-kind rule
// ────────────────────────────────────────────────────────────────────────────

/// req_w14 / GATE 2, against the REAL shipped corpus: no option on a `decision`-kind rule
/// carries `violates = true`. The taxonomy's own words (`crates/rules/src/lib.rs` ~line 150):
/// *"No option on a `decision`-kind rule should carry `violates = true`."*
///
/// The invariant was documented and COMPLETELY UNENFORCED before W14, and the flag is live at
/// runtime: `ai_audit::apply_alternative_decision` branches only on `option.violates` and never
/// consults `kind`, so a stray flag silently emits a guaranteed high-severity Defect against a
/// rule whose premise is that its options are equivalent.
///
/// PRIOR COVERAGE, stated plainly: W11 already asserts this corpus-wide in
/// `w11_kind_taxonomy.rs::no_decision_kind_rule_in_the_corpus_carries_a_violates_flag`, and that
/// test is left exactly as written. W14's contribution is moving the check into the corpus-load
/// gate FAMILY in `lib.rs` — `decision_kind_rules_with_violates_flag`, alongside
/// `rules_missing_kind`, `rules_disarmed_by_verification` and the severity gates — so it is a
/// reusable gate any build or integrity consumer can call, rather than logic that exists only
/// inside one test body. This test is that gate function's corpus assertion.
///
/// Measured on 2026-10-11: 156 `decision`-kind rules, zero offenders.
#[tokio::test]
async fn req_w14_no_decision_kind_rule_carries_a_violates_flag() {
    let Some(set) = corpus().await else {
        return;
    };
    let offenders = decision_kind_rules_with_violates_flag(&set);
    assert!(
        offenders.is_empty(),
        "no option on a `decision`-kind rule may carry `violates = true` (RuleKind docs, \
         lib.rs ~150): {offenders:#?}"
    );

    // Vacuity guard: the sweep is meaningless if the corpus has no decision rules to sweep.
    let count = decision_kind_rule_count(&set);
    assert!(
        count >= 100,
        "gate is vacuous: only {count} decision-kind rules loaded (156 on 2026-10-11)"
    );
}

/// req_w14 / GATE 2 FIRE PROOF: the gate reports a `decision`-kind rule carrying a flag, and
/// stays silent for the same flag on a `default_with_exceptions` rule, where it is legitimate.
#[tokio::test]
async fn req_w14_gate_2_fires_on_a_violates_flag_on_a_decision_rule() {
    // (a) decision-kind + a flag → must fire.
    let dir = synthetic_corpus(&[(
        "w14-fixture-1.toml",
        rule_toml("decision", "An equally legitimate alternative.", true),
    )]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    let offenders = decision_kind_rules_with_violates_flag(&set);
    assert_eq!(
        offenders.len(),
        1,
        "gate 2 must fire on a `violates` flag on a `decision`-kind rule, got: {offenders:?}"
    );
    assert!(
        offenders[0].contains("W14-FIXTURE-1") && offenders[0].contains("the-other-option"),
        "the offender line must name the rule AND the flagged option: {:?}",
        offenders[0]
    );

    // (b) the SAME flag on a `default_with_exceptions` rule is legitimate → must NOT fire.
    //     This is what keeps gate 2 from being a blanket ban on the flag.
    let dir = synthetic_corpus(&[(
        "w14-fixture-1.toml",
        rule_toml(
            "default_with_exceptions",
            "Rejected: the degenerate form of the requirement.",
            true,
        ),
    )]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    assert!(
        decision_kind_rules_with_violates_flag(&set).is_empty(),
        "gate 2 must not fire for a flag on a `default_with_exceptions` rule, where the \
         taxonomy expects one"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// GATE 3 — the schema-vocabulary gate (gotcha 9: a misspelled field name vanishes)
// ────────────────────────────────────────────────────────────────────────────

/// req_w14 / GATE 3, against the REAL shipped corpus: no TOML file carries an unknown FIELD NAME
/// at any nesting level.
///
/// `RuleToml` deliberately does not set `serde(deny_unknown_fields)`, so `max_severety = "high"`
/// or `voilates = true` parses cleanly and simply is not there (gotcha 9). This corpus has now
/// shipped three separate silent-no-op mechanisms; that default costs more than it saves.
///
/// Measured on all 417 files on 2026-10-11: zero offenders, once `tag` (417), `layer` (417),
/// `emits` (2) and `sources.qualifies` (29) are declared accepted-and-unconsumed in `lib.rs`.
/// Those four were the only unknown keys in the entire corpus; before declaring them the gate
/// reported 865 offenders — which is also why the serde attribute could not simply be switched
/// on (see `corpus_files_with_unknown_fields`'s doc comment for the three reasons, including the
/// `crates/server/tests` fixtures W14 does not own).
#[tokio::test]
async fn req_w14_no_corpus_rule_carries_an_unknown_field() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    let offenders = corpus_files_with_unknown_fields(dir)
        .await
        .expect("corpus is readable");
    assert!(
        offenders.is_empty(),
        "no corpus rule may carry an unknown field name — an unknown field is parsed by \
         nothing and vanishes silently (gotcha 9):\n  {}",
        offenders.join("\n  ")
    );
}

/// req_w14 / GATE 3 FIRE PROOF: the gate reports a misspelled field at EVERY nesting level it
/// claims to check. One case per level, because `serde(deny_unknown_fields)` on `RuleToml` would
/// have caught only the first of these five — the nested levels are precisely what makes this
/// gate stronger than the attribute it stands in for.
#[tokio::test]
async fn req_w14_gate_3_fires_on_a_misspelled_field_at_every_nesting_level() {
    // Each case: (label, the line(s) to splice in, the location the gate must report).
    let cases: [(&str, String, &str); 5] = [
        (
            "rule level",
            // The real gotcha-9 example from docs/RULES_REFERENCE.md.
            "max_severety = \"high\"\n".to_string(),
            "rule level",
        ),
        (
            "[[option]]",
            "[[option]]\nid = \"o1\"\nlabel = \"O1\"\nvoilates = true\n".to_string(),
            "[[option]] o1",
        ),
        (
            "[decision]",
            "[decision]\nquestoin = \"typo\"\n".to_string(),
            "[decision]",
        ),
        (
            "[[sources]]",
            "[[sources]]\nurl = \"https://example.org\"\ntitle = \"T\"\nlniter = \"clippy: x\"\n"
                .to_string(),
            "[[sources]][0]",
        ),
        (
            "[verified]",
            "[verified]\nby = \"a human\"\nat = \"2026-10-11\"\nagaints = [\"x\"]\n".to_string(),
            "[verified]",
        ),
    ];

    for (label, extra, expected_location) in cases {
        let body = format!(
            r#"
id = "W14-UNKNOWN-1"
title = "W14 unknown-field fixture"
domain = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"
{extra}"#
        );
        let dir = synthetic_corpus(&[("w14-unknown-1.toml", body)]);
        let offenders = corpus_files_with_unknown_fields(dir.path())
            .await
            .expect("fixture dir is readable");
        assert_eq!(
            offenders.len(),
            1,
            "gate 3 must report exactly one unknown field for the {label} case, got: \
             {offenders:#?}"
        );
        assert!(
            offenders[0].contains(expected_location),
            "the {label} offender must name its location {expected_location:?}: {:?}",
            offenders[0]
        );
    }
}

/// req_w14 / GATE 3 NEGATIVE PROOF: the four accepted-and-unconsumed keys do NOT fire. Without
/// this, a later edit could silently re-reject the entire 417-rule corpus — or, worse, someone
/// could "fix" the gate by widening it until it reported nothing at all.
#[tokio::test]
async fn req_w14_gate_3_accepts_the_declared_unconsumed_fields() {
    let body = r#"
id = "W14-ACCEPTED-1"
title = "W14 accepted-unconsumed fixture"
tag = "stack"
domain = "universal"
layer = "library"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[[sources]]
url = "https://example.org"
title = "T"
qualifies = "Authored below a [[sources]] header, so TOML scopes it into the source table."

[[emits]]
target = "CONVENTIONS.md"
scope = "repo"
"#;
    let dir = synthetic_corpus(&[("w14-accepted-1.toml", body.to_string())]);
    let offenders = corpus_files_with_unknown_fields(dir.path())
        .await
        .expect("fixture dir is readable");
    assert!(
        offenders.is_empty(),
        "tag/layer/emits/sources.qualifies are declared accepted-and-unconsumed and must not \
         fire: {offenders:#?}"
    );
    // And the file must still LOAD — declaring a field accepted is pointless if the loader
    // chokes on it.
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    assert_eq!(set.iter().count(), 1, "the fixture rule must load");
}

/// req_w14 ADVISORY: the `qualifies`-misplaced-into-`[[sources]]` roster has not grown.
///
/// `qualifies` is a RULE-level field, but 17 rules author it on a line below a `[[sources]]`
/// header, so TOML scopes it into the source table and `RuleSource` discards it — 29 occurrences
/// on 2026-10-11, every one in the `go/` family. For the 10 affected `go/*.toml` rules there is
/// NO rule-level `qualifies` at all, so `Rule::summary` silently falls through to
/// `[decision].why` rather than the authored text. This is gotcha 9 in the wild, found by
/// writing gate 3.
///
/// It is PINNED, not fixed: the fix is to move a line in 17 hand-authored corpus files, which
/// changes each rule's derived `summary` — the corpus owner's call, not a gate's. What a gate can
/// honestly carry is that the mistake cannot SPREAD while it waits for that ruling.
#[tokio::test]
async fn req_w14_the_misplaced_qualifies_roster_has_not_grown() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    let found = corpus_rules_with_qualifies_misplaced_into_sources(dir)
        .await
        .expect("corpus is readable");
    assert_eq!(
        found.len(),
        29,
        "the misplaced-`qualifies` roster must not change silently (29 occurrences across 17 \
         rules on 2026-10-11, all in `go/`); a NEW one means the mistake is spreading:\n{found:#?}"
    );
    let distinct: std::collections::BTreeSet<&str> = found
        .iter()
        .map(|s| s.split(" (").next().unwrap_or(s))
        .collect();
    assert_eq!(
        distinct.len(),
        17,
        "expected 17 distinct rules in the misplaced-`qualifies` roster: {distinct:#?}"
    );
    // The 10 rules with NO rule-level `qualifies` are the ones that actually lost authored
    // text; pin that subset separately so a partial fix is visible as a partial fix.
    let lost = found.iter().filter(|s| s.contains("ABSENT")).count();
    assert_eq!(
        lost, 10,
        "10 of the misplaced-`qualifies` rules have no rule-level `qualifies` at all, so their \
         `summary` silently falls back to `[decision].why`: {found:#?}"
    );
}

/// req_w14 ADVISORY FIRE PROOF: the misplaced-`qualifies` reporter actually detects the shape,
/// and distinguishes the rule that LOST its authored text from one that merely duplicated it.
#[tokio::test]
async fn req_w14_misplaced_qualifies_reporter_fires_and_reports_rule_level_presence() {
    let absent = r#"
id = "W14-Q-ABSENT-1"
title = "No rule-level qualifies"
domain = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[[sources]]
url = "https://example.org"
title = "T"
qualifies = "This text is discarded; the rule has no rule-level qualifies."
"#;
    let present = r#"
id = "W14-Q-PRESENT-1"
title = "Rule-level qualifies present"
domain = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"
qualifies = "The rule-level field the engine actually reads."

[[sources]]
url = "https://example.org"
title = "T"
qualifies = "A duplicate that is discarded."
"#;
    let dir = synthetic_corpus(&[
        ("w14-q-absent-1.toml", absent.to_string()),
        ("w14-q-present-1.toml", present.to_string()),
    ]);
    let found = corpus_rules_with_qualifies_misplaced_into_sources(dir.path())
        .await
        .expect("fixture dir is readable");
    assert_eq!(found.len(), 2, "both fixtures must be reported: {found:#?}");
    assert!(
        found
            .iter()
            .any(|s| s.contains("W14-Q-ABSENT-1") && s.contains("ABSENT")),
        "the rule with no rule-level `qualifies` must be marked ABSENT: {found:#?}"
    );
    assert!(
        found
            .iter()
            .any(|s| s.contains("W14-Q-PRESENT-1") && s.contains("present")),
        "the rule that merely duplicated it must be marked present: {found:#?}"
    );
}
