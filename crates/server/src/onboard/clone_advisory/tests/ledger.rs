//! The ledger, the convention row, the corpus ids and the judge model.

use super::*;
use crate::integrity_gate::SkipClass;
use crate::scan_ledger::{RuleTier, ScanLedger, ADVISORY_PARTIAL_NOTE, ADVISORY_UNAVAILABLE_NOTE};

const RULE: CloneRule = CloneRule::SingleSourceLogic;

async fn corpus() -> camerata_rules::RuleSet {
    camerata_rules::load_corpus(&camerata_rules::corpus_path())
        .await
        .expect("the bundled corpus loads")
}

// ---- the ledger ----

#[tokio::test]
async fn offline_records_the_rule_as_not_run_with_the_advisory_note() {
    let all = discount_repo("0.2", "0.15");
    let outcome = judge(None, &all, set_for(&all, &[RULE])).await;
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    let entry = ledger.rule(RULE.id()).expect("recorded");
    assert!(!entry.ran);
    assert_eq!(entry.tier, RuleTier::Architectural);
    assert_eq!(
        entry.skip_reason.as_deref(),
        Some(ADVISORY_UNAVAILABLE_NOTE)
    );
    assert_eq!(
        entry.coverage_note.as_deref(),
        Some(ADVISORY_UNAVAILABLE_NOTE)
    );
    assert_eq!(entry.findings_emitted, 0);
    assert!(
        !entry.verified_clean(),
        "a rule that was not judged is never verified clean"
    );
    assert_eq!(entry.files_evaluated, 2);
    // The integrity gate reads the reason as a disclosed scope choice, not a defect.
    assert_eq!(
        SkipClass::classify(entry.skip_reason.as_deref()),
        SkipClass::ScopedOut
    );
}

#[tokio::test]
async fn zero_candidates_credits_the_rule_as_run_and_clean() {
    let all = files(&[("src/a.ts", "export const a = 1;\n")]);
    let outcome = judge(None, &all, set_for(&all, &[RULE])).await;
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    let entry = ledger.rule(RULE.id()).unwrap();
    assert!(entry.ran);
    assert!(entry.skip_reason.is_none() && entry.coverage_note.is_none());
    assert!(entry.verified_clean());
}

#[tokio::test]
async fn a_fully_judged_rule_ran_and_counts_its_confirmed_findings() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the discount");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    let entry = ledger.rule(RULE.id()).unwrap();
    assert!(entry.ran);
    assert_eq!(entry.findings_emitted, 1);
    assert!(!entry.verified_clean());
}

#[tokio::test]
async fn a_partly_judged_rule_is_not_credited_as_run_and_says_why() {
    let (all, set) = synthetic_set(20, 5);
    let stub = StubJudge::answering("different", "");
    let outcome = judge(Some(&stub), &all, set).await;
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    let entry = ledger.rule(RULE.id()).unwrap();
    assert!(!entry.ran);
    assert_eq!(entry.coverage_note.as_deref(), Some(ADVISORY_PARTIAL_NOTE));
    assert!(
        entry.skip_reason.as_deref().unwrap().contains("5 of 25"),
        "{:?}",
        entry.skip_reason
    );
    // A judgment that was attempted and fell short is not a scope choice.
    assert_eq!(
        SkipClass::classify(entry.skip_reason.as_deref()),
        SkipClass::Unrecognised
    );
}

#[tokio::test]
async fn a_later_pass_that_ran_settles_ran_but_the_note_survives() {
    // The semantic pass records the same id afterwards; ran ORs, the disclosure stays.
    let all = discount_repo("0.2", "0.15");
    let outcome = judge(None, &all, set_for(&all, &[RULE])).await;
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    ledger.record_rule("", RULE.id(), RuleTier::Semantic, true, None, 2, 0);
    let entry = ledger.rule(RULE.id()).unwrap();
    assert!(entry.ran);
    assert_eq!(
        entry.tier,
        RuleTier::Architectural,
        "the first recording fixes the tier"
    );
    assert_eq!(
        entry.coverage_note.as_deref(),
        Some(ADVISORY_UNAVAILABLE_NOTE)
    );
}

// ---- no convention row (orchestrator decision D1) ----

#[tokio::test]
async fn a_clean_rule_is_credited_in_the_ledger_but_claims_nothing_to_the_client() {
    // A repo with nothing repeated: every armed rule has zero candidates. The ledger credits
    // each as having run (it did), and the outcome carries no row at all: no "clean" row, no
    // convention row, nothing that claims an absence the detector cannot prove.
    let all = files(&[
        ("src/a.ts", "export const a = 1;\n"),
        ("src/b.ts", "export const b = 2;\n"),
    ]);
    let outcome = judge(None, &all, set_for(&all, &CloneRule::ALL)).await;
    assert!(outcome.findings.is_empty() && outcome.not_assessed.is_empty());
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    for rule in CloneRule::ALL {
        let entry = ledger.rule(rule.id()).expect("recorded");
        assert!(entry.ran, "{}", rule.id());
        assert!(entry.verified_clean(), "{}", rule.id());
    }
}

// ---- the corpus ids ----

#[tokio::test]
async fn the_option_ids_the_seam_names_are_the_corpus_defaults_and_violations() {
    let corpus = corpus().await;
    for rule in CloneRule::ALL {
        let r = corpus
            .get_by_id(rule.id())
            .unwrap_or_else(|| panic!("{} in corpus", rule.id()));
        let default = r
            .options
            .iter()
            .find(|o| o.id == findings::default_option_id(rule))
            .expect("the default option exists");
        assert!(!default.violates);
        assert_eq!(r.default_option.as_deref(), Some(default.id.as_str()));
        let violating = r
            .options
            .iter()
            .find(|o| o.id == findings::violating_option_id(rule))
            .expect("the violating option exists");
        assert!(violating.violates);
        assert!(!rule_text(rule, Some(&corpus)).is_empty());
    }
}

/// A confirmed finding must carry a Fix line in the export: the rule's remediation, resolved
/// through the default option. A finding that named the violating option would resolve to none
/// (that option has no remediation) and be held for want of a fix.
#[tokio::test]
async fn a_confirmed_finding_resolves_the_rules_authored_fix() {
    let corpus = corpus().await;
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the discount");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let fix =
        crate::report_export::resolve_fix(RULE.id(), Some(&corpus), &outcome.findings[0], None)
            .expect("the finding resolves a Fix");
    assert!(fix.contains("one exported function"), "{fix}");
    assert!(!fix.contains('\u{2014}'));
    for rule in CloneRule::ALL {
        let r = corpus.get_by_id(rule.id()).unwrap();
        let default = r.resolved_option(None).unwrap();
        assert!(default.remediation.is_some(), "{}", rule.id());
        assert!(default.effort.is_some(), "{}: an effort band", rule.id());
    }
}

// ---- the judge model ----

const ENV_NAME: &str = "CAMERATA_CLONE_JUDGE_MODEL";

#[test]
#[serial_test::serial(process_env)]
fn judge_model_is_the_fast_tier_unless_picked_and_never_the_environment() {
    // Default: the fast tier's primary, never the audit model.
    assert_eq!(
        resolve_clone_judge_model(None),
        camerata_fleet::tier::default_fast_model()
    );
    assert_eq!(resolve_clone_judge_model(None), "claude-haiku-5-5");
    assert_ne!(resolve_clone_judge_model(None), crate::llm::DEFAULT_MODEL);
    // A blank pick is no pick.
    assert_eq!(resolve_clone_judge_model(Some("  ")), "claude-haiku-5-5");
    // The variable that used to override the default is ignored; an explicit pick still wins.
    let _env = camerata_test_env::EnvGuard::set(ENV_NAME, "env-judge");
    assert_eq!(resolve_clone_judge_model(None), "claude-haiku-5-5");
    assert_eq!(
        resolve_clone_judge_model(Some("explicit-judge")),
        "explicit-judge"
    );
}

#[tokio::test]
async fn the_judge_model_is_the_one_the_requests_used() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("different", "");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(outcome.judge_model.as_deref(), Some(JUDGE));
    assert_eq!(stub.request(0).model, JUDGE);
}
