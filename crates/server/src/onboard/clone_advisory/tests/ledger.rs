//! The ledger, the convention row, the corpus ids and the judge model.

use camerata_api_types::audit::FindingKind;

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
    record_in_ledger(&mut ledger, &outcome);
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
    record_in_ledger(&mut ledger, &outcome);
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
    record_in_ledger(&mut ledger, &outcome);
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
    record_in_ledger(&mut ledger, &outcome);
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
    record_in_ledger(&mut ledger, &outcome);
    ledger.record_rule(RULE.id(), RuleTier::Semantic, true, None, 2, 0);
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

// ---- the convention row ----

fn clean_outcome() -> JudgeOutcome {
    let mut outcome = JudgeOutcome {
        model_available: true,
        files_read: 42,
        ..JudgeOutcome::default()
    };
    outcome.counts.insert(
        RULE,
        JudgeCounts {
            found: 3,
            judged: 3,
            rejected: 3,
            ..JudgeCounts::default()
        },
    );
    outcome
}

fn other_row(rule_id: &str) -> Finding {
    Finding {
        rule_id: rule_id.to_string(),
        ..Finding::default()
    }
}

#[tokio::test]
async fn a_complete_clean_run_emits_one_convention_row_stating_the_detectors_fact() {
    let corpus = corpus().await;
    let rows = convention_rows("acme/shop", &clean_outcome(), &[], Some(&corpus), false);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.rule_id, RULE.id());
    assert_eq!(row.severity, "info");
    assert!(!row.located);
    assert_eq!(
        row.detail,
        "No application decision was found written in more than one file (42 source files \
         read, 60-token threshold, 3 candidate sites reviewed and judged not to be the same \
         decision). Reported so the single-implementation convention can be adopted explicitly."
    );
    let Some(FindingKind::ConventionObserved(kind)) = &row.kind else {
        panic!("a convention row");
    };
    assert_eq!(
        kind.convention_option_id,
        "each-decision-is-implemented-once"
    );
    assert!(kind
        .convention_label
        .starts_with("Each application decision is implemented once"));
    assert!(!row.detail.contains('\u{2014}'));
}

#[tokio::test]
async fn zero_candidates_in_a_deterministic_only_scan_still_state_the_fact() {
    let corpus = corpus().await;
    let mut outcome = clean_outcome();
    outcome.model_available = false;
    outcome.counts.insert(RULE, JudgeCounts::default());
    let rows = convention_rows("acme/shop", &outcome, &[], Some(&corpus), false);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].detail.contains("0 candidate sites reviewed"),
        "{}",
        rows[0].detail
    );
}

#[tokio::test]
async fn every_failed_condition_silences_the_convention_row() {
    let corpus = corpus().await;
    let ok = || clean_outcome();
    let run =
        |outcome: &JudgeOutcome, findings: &[Finding], corpus: Option<&_>, incomplete: bool| {
            convention_rows("acme/shop", outcome, findings, corpus, incomplete)
        };
    assert_eq!(run(&ok(), &[], Some(&corpus), false).len(), 1, "control");

    // A confirmed finding.
    let mut o = ok();
    o.counts.get_mut(&RULE).unwrap().confirmed = 1;
    assert!(run(&o, &[], Some(&corpus), false).is_empty(), "confirmed");
    // A trace from any tier: a row, a same-location demotion, an absorbed site.
    assert!(
        run(&ok(), &[other_row(RULE.id())], Some(&corpus), false).is_empty(),
        "ai row"
    );
    let mut demoted = other_row("SOMETHING-ELSE-1");
    demoted.also_matches = vec![RULE.id().to_string()];
    assert!(
        run(&ok(), &[demoted], Some(&corpus), false).is_empty(),
        "also_matches"
    );
    let mut absorbed = other_row("SOMETHING-ELSE-1");
    absorbed.also_locations = vec![crate::onboard::MergedLocation {
        repo: "acme/shop".to_string(),
        path: "a.ts".to_string(),
        line: 1,
        rule_id: RULE.id().to_string(),
        snippet: String::new(),
        consequence: false,
    }];
    assert!(
        run(&ok(), &[absorbed], Some(&corpus), false).is_empty(),
        "also_locations"
    );
    // A row under another rule is no trace.
    assert_eq!(
        run(
            &ok(),
            &[other_row("ARCH-DEAD-CODE-1")],
            Some(&corpus),
            false
        )
        .len(),
        1
    );
    // A truncated index, an incomplete scan.
    let mut o = ok();
    o.index_truncated = true;
    assert!(
        run(&o, &[], Some(&corpus), false).is_empty(),
        "truncated index"
    );
    assert!(
        run(&ok(), &[], Some(&corpus), true).is_empty(),
        "incomplete scan"
    );
    // Candidates left undecided, over the cap, or never judged for want of a model.
    let mut o = ok();
    o.counts.get_mut(&RULE).unwrap().unjudged = 1;
    assert!(run(&o, &[], Some(&corpus), false).is_empty(), "unjudged");
    let mut o = ok();
    o.counts.get_mut(&RULE).unwrap().over_cap = 1;
    assert!(run(&o, &[], Some(&corpus), false).is_empty(), "over cap");
    let mut o = ok();
    o.model_available = false;
    assert!(
        run(&o, &[], Some(&corpus), false).is_empty(),
        "candidates found but no model"
    );
    // Nothing read, and no corpus to name the convention.
    let mut o = ok();
    o.files_read = 0;
    assert!(
        run(&o, &[], Some(&corpus), false).is_empty(),
        "no files read"
    );
    assert!(run(&ok(), &[], None, false).is_empty(), "no corpus");
}

#[tokio::test]
async fn the_ui_rule_needs_react_files_in_scope_to_claim_a_clean_convention() {
    let corpus = corpus().await;
    let mut outcome = JudgeOutcome {
        model_available: true,
        files_read: 10,
        ui_files: 0,
        ..JudgeOutcome::default()
    };
    outcome
        .counts
        .insert(CloneRule::SharedPrimitive, JudgeCounts::default());
    assert!(
        convention_rows("acme/api", &outcome, &[], Some(&corpus), false).is_empty(),
        "a repo with no React-family UI has nothing to be consistent about"
    );
    outcome.ui_files = 4;
    let rows = convention_rows("acme/web", &outcome, &[], Some(&corpus), false);
    assert_eq!(rows.len(), 1);
    assert!(rows[0]
        .detail
        .contains("No UI mechanism was found hand-written in more than one component"));
    assert!(rows[0].detail.contains("40-token threshold"));
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
fn judge_model_is_the_fast_tier_unless_overridden() {
    {
        let _env = camerata_test_env::EnvGuard::remove(ENV_NAME);
        // Default: the fast tier's primary, never the audit model.
        assert_eq!(
            resolve_clone_judge_model(None),
            camerata_fleet::tier::default_fast_model()
        );
        assert_eq!(resolve_clone_judge_model(None), "claude-haiku-4-5-20251001");
        assert_ne!(resolve_clone_judge_model(None), crate::llm::DEFAULT_MODEL);
        // A blank pick is no pick.
        assert_eq!(
            resolve_clone_judge_model(Some("  ")),
            "claude-haiku-4-5-20251001"
        );
    }
    {
        // The environment overrides the default; an explicit pick overrides the environment.
        let _env = camerata_test_env::EnvGuard::set(ENV_NAME, "env-judge");
        assert_eq!(resolve_clone_judge_model(None), "env-judge");
        assert_eq!(
            resolve_clone_judge_model(Some("explicit-judge")),
            "explicit-judge"
        );
    }
    {
        let _env = camerata_test_env::EnvGuard::set(ENV_NAME, "  ");
        assert_eq!(resolve_clone_judge_model(None), "claude-haiku-4-5-20251001");
    }
}

#[tokio::test]
async fn the_judge_model_is_the_one_the_requests_used() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("different", "");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(outcome.judge_model.as_deref(), Some(JUDGE));
    assert_eq!(stub.request(0).model, JUDGE);
}
