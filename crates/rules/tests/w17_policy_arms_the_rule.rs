//! W17 item 4: `verification = "policy"` ARMS a rule. Pins the behaviour the corrected
//! [`camerata_rules::Verification::Policy`] doc comment now describes.
//!
//! ## What was wrong
//!
//! The variant's doc comment read:
//!
//! > Not grounded; kept out of the armed ruleset like `draft`, but labeled honestly rather than
//! > as `grounded`.
//!
//! The second clause has been FALSE since the A8 fix. `Rule::is_auto_recommended` returns `true`
//! for `Grounded | Verified | Policy`, and an unattended scan selects exactly the
//! auto-recommended set (`camerata_cli::inspect_cmd::curated_rule_selection`), so a `policy` rule
//! is pre-checked and runs. Believing the old comment is what disarmed 17 shipped rules on every
//! repo — `SPIRIT-FILE-SIZE-1` and every `PROC-*` governance rule among them — by conflating
//! provenance ("this cites no external authority") with worth-running.
//!
//! ## Why this is a test and not only a comment edit
//!
//! A corrected comment drifts again. The claim it makes is mechanical, so it is asserted:
//! `Policy` arms, while `is_grounded`/`is_shippable` stay false for it (the provenance answer,
//! which did NOT change). A future revert of the A8 fix fails here rather than silently
//! re-disarming 21 shipped rules.
//!
//! There is no corpus before-state to stash for this item: the defect was in the comment, not in
//! the data or the code path. The fire proof is instead the inverse — removing `Policy` from
//! `Rule::is_auto_recommended` makes both tests below go red, which is what proves they bind to
//! the claim rather than restating it.

use camerata_rules::{load_corpus, Verification, DEFAULT_CORPUS_PATH};

/// W17 item 4, the type-level claim: the exact three-way split the corrected doc comment states.
/// `Policy` arms; `Policy` is not grounded and not shippable. Those answer two different
/// questions and the old comment ran them together.
///
/// Asserted over EVERY variant, not just `Policy`, so the test states the whole ladder: a change
/// to any variant's arming behaviour lands here.
#[test]
fn req_w17_policy_verification_arms_while_staying_ungrounded() {
    // (value, arms?, grounded/shippable?)
    let ladder = [
        (Verification::Draft, false, false),
        (Verification::Policy, true, false),
        (Verification::Grounded, true, true),
        (Verification::Verified, true, true),
        (Verification::NeedsRecheck, false, true),
    ];
    for (v, should_arm, should_be_grounded) in ladder {
        // Build a minimal rule carrying this verification value and ask the real accessor.
        let body = format!(
            r#"
id = "W17-POLICY-1"
title = "W17 verification-ladder fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "{}"
"#,
            v.as_str()
        );
        let dir = tempfile::tempdir().expect("temp dir");
        let universal = dir.path().join("universal");
        std::fs::create_dir_all(&universal).expect("mkdir universal");
        std::fs::write(universal.join("w17-policy-1.toml"), body).expect("write fixture");
        let set = futures_lite_block_on(load_corpus(dir.path()));
        let set = set.expect("fixture loads");
        let rule = set.get_by_id("W17-POLICY-1").expect("fixture rule loads");

        assert_eq!(
            rule.verification(),
            v,
            "{v} must round-trip through the TOML loader"
        );
        assert_eq!(
            rule.is_auto_recommended(),
            should_arm,
            "{v}: is_auto_recommended must be {should_arm}. `policy` ARMS (A8) — a verification \
             value no longer disarms a rule everywhere; `rules_disarmed_by_verification` fails \
             the build on any value that would"
        );
        assert_eq!(
            rule.is_grounded(),
            should_be_grounded,
            "{v}: is_grounded answers PROVENANCE, not arming, and that answer did not change \
             with A8"
        );
        assert_eq!(
            rule.is_shippable(),
            should_be_grounded,
            "{v}: is_shippable delegates to is_grounded and gates nothing in production"
        );
    }
}

/// W17 item 4, against the REAL shipped corpus: every `policy` rule actually arms, and the
/// population is non-empty.
///
/// The vacuity guard is the load-bearing half. "No `policy` rule is disarmed" is trivially true
/// of a corpus with no `policy` rules, and this corpus has 21 — including the two the A8 finding
/// named by id. Pinning those two by name means a future edit that quietly re-labels them cannot
/// make this test pass by emptying its own population.
#[test]
fn req_w17_every_policy_rule_in_the_corpus_arms() {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return;
    }
    let set = futures_lite_block_on(load_corpus(path)).expect("corpus loads");

    let policy: Vec<&str> = set
        .iter()
        .filter(|r| r.verification() == Verification::Policy)
        .map(|r| r.id.0.as_str())
        .collect();
    assert!(
        policy.len() >= 15,
        "vacuity guard: only {} `policy` rules loaded (21 on 2026-10-12); this test says nothing \
         about a corpus that has none",
        policy.len()
    );

    // The two the A8 finding named. `SPIRIT-FILE-SIZE-1` was DOUBLY disarmed (policy, plus the
    // `SPIRIT-` prefix exclusion from the code-audit tier); `PROC-CITE-CONVENTION-ID-1` stands
    // for the `PROC-*` governance family the product is explicitly about.
    for id in ["SPIRIT-FILE-SIZE-1", "PROC-CITE-CONVENTION-ID-1"] {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_eq!(
            rule.verification(),
            Verification::Policy,
            "{id} is one of the A8 rules and is expected to still be `policy`; if it was \
             deliberately re-labelled, update this test's roster rather than widening it"
        );
    }

    let disarmed: Vec<&str> = set
        .iter()
        .filter(|r| r.verification() == Verification::Policy && !r.is_auto_recommended())
        .map(|r| r.id.0.as_str())
        .collect();
    assert!(
        disarmed.is_empty(),
        "a `policy` rule must arm — `verification` may no longer disarm a rule anywhere (A8). \
         Disarmed: {disarmed:#?}"
    );
}

/// Run a future to completion on a tiny current-thread runtime. `load_corpus` is `async` only
/// because it reads files with `tokio::fs`; these are synchronous `#[test]`s because nothing here
/// needs concurrency, and a `#[tokio::test]` would imply otherwise.
fn futures_lite_block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}
