//! The verdict classes of the seam: what ships, what does not, and what is disclosed.

use camerata_api_types::audit::{FindingKind, NotAssessedReason};

use super::*;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

fn counts(outcome: &JudgeOutcome) -> &JudgeCounts {
    &outcome.counts[&RULE]
}

#[tokio::test]
async fn same_decision_verdict_becomes_a_high_finding_when_copies_disagree() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the premium discount calculation");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;

    assert_eq!(outcome.findings.len(), 1, "{outcome:#?}");
    let f = &outcome.findings[0];
    assert_eq!(f.severity, "high");
    assert_eq!(f.confidence.as_deref(), Some("high"));
    assert!(!f.needs_review);
    assert_eq!(f.rule_id, RULE.id());
    assert_eq!(f.kind, Some(FindingKind::Defect));
    assert!(f.preview, "a deterministic-tier row");
    assert_eq!(
        f.preview_tool.as_deref(),
        Some(crate::onboard::ARCH_PREVIEW_TOOL)
    );
    assert_eq!(f.path, "src/cart/total.ts");
    assert_eq!(f.line, 2);
    assert_eq!(f.snippet, "export function applyDiscount(order, user) {");
    // Every site: the first is the row, the rest are also_locations.
    assert_eq!(f.also_locations.len(), 1);
    assert_eq!(f.also_locations[0].path, "src/checkout/summary.ts");
    assert!(!f.also_locations[0].consequence);
    // The message states the facts: both values, every path, and the judge's clause as an
    // appearance.
    assert!(f.detail.contains("do not agree"), "{}", f.detail);
    assert!(
        f.detail.contains("`0.2`") && f.detail.contains("`0.15`"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("src/cart/total.ts:2-12"), "{}", f.detail);
    assert!(
        f.detail.contains("src/checkout/summary.ts:2-12"),
        "{}",
        f.detail
    );
    assert!(
        f.detail
            .contains("which appears to be the premium discount calculation"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("written in 2 places"), "{}", f.detail);
    assert!(
        !f.detail.contains('\u{2014}'),
        "no em-dashes in client text"
    );
    assert_eq!(
        f.captures.get("concept").map(String::as_str),
        Some("the premium discount calculation")
    );
    assert!(f.captures.contains_key("family"));
    assert_eq!(
        f.evaluated_option_id, None,
        "the Fix resolves through the default option"
    );
    let c = counts(&outcome);
    assert_eq!(
        (c.found, c.judged, c.confirmed, c.unjudged, c.over_cap),
        (1, 1, 1, 0, 0)
    );
    assert!(outcome.not_assessed.is_empty());
    assert_eq!(outcome.judge_model.as_deref(), Some(JUDGE));
}

#[tokio::test]
async fn same_decision_verdict_becomes_a_medium_finding_when_copies_agree() {
    let all = discount_repo("0.2", "0.2");
    let stub = StubJudge::answering("same_decision", "the premium discount calculation");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let f = &outcome.findings[0];
    assert_eq!(f.severity, "medium");
    assert!(f.detail.contains("currently agree"), "{}", f.detail);
    assert!(
        !f.detail.contains("do not agree"),
        "do not agree only from a real disagreement"
    );
}

#[tokio::test]
async fn an_empty_concept_omits_the_appearance_clause() {
    let all = discount_repo("0.2", "0.2");
    let stub = StubJudge::answering("same_decision", "");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let f = &outcome.findings[0];
    assert!(!f.detail.contains("appears to"), "{}", f.detail);
    assert!(!f.captures.contains_key("concept"));
}

#[tokio::test]
async fn different_and_boilerplate_verdicts_produce_no_finding_but_are_counted() {
    for word in ["different", "boilerplate"] {
        let all = discount_repo("0.2", "0.15");
        let stub = StubJudge::answering(word, "");
        let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
        assert!(outcome.findings.is_empty(), "{word}");
        assert!(
            outcome.not_assessed.is_empty(),
            "{word}: a definite no is not undecided"
        );
        let c = counts(&outcome);
        assert_eq!(
            (c.found, c.judged, c.rejected, c.confirmed, c.unjudged),
            (1, 1, 1, 0, 0),
            "{word}"
        );
    }
}

#[tokio::test]
async fn unsure_and_missing_verdicts_never_produce_a_finding() {
    let all = discount_repo("0.2", "0.15");
    // `unsure`.
    let stub = StubJudge::answering("unsure", "");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(counts(&outcome).unjudged, 1);
    assert_eq!(outcome.not_assessed.len(), 1);
    let FindingKind::NotAssessed(kind) = outcome.not_assessed[0].kind.clone().unwrap() else {
        panic!("a not_assessed row");
    };
    assert_eq!(kind.reason, NotAssessedReason::ModelDeclined);

    // An empty verdict list: the candidate has no verdict.
    let stub = StubJudge::new(|_, _| Ok(r#"{"verdicts":[]}"#.to_string()));
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(counts(&outcome).unjudged, 1);
    assert_eq!(outcome.not_assessed.len(), 1);
}

#[tokio::test]
async fn an_unparseable_response_leaves_the_batch_unjudged_and_emits_not_assessed() {
    let all = discount_repo("0.2", "0.15");
    for garbage in [
        "I think they are the same decision.",
        "{\"verdicts\": \"same_decision\"}",
        "same_decision",
        "{not json at all}",
        "",
    ] {
        let stub = StubJudge::new(move |_, _| Ok(garbage.to_string()));
        let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
        assert!(outcome.findings.is_empty(), "{garbage:?}");
        assert_eq!(counts(&outcome).unjudged, 1, "{garbage:?}");
        assert_eq!(counts(&outcome).confirmed, 0, "{garbage:?}");
        assert_eq!(outcome.not_assessed.len(), 1, "{garbage:?}");
        let FindingKind::NotAssessed(kind) = outcome.not_assessed[0].kind.clone().unwrap() else {
            panic!();
        };
        assert_eq!(kind.reason, NotAssessedReason::PassFailed, "{garbage:?}");
        assert_eq!(
            outcome.failed.len(),
            1,
            "{garbage:?}: disclosed in the methodology"
        );
        assert_eq!(outcome.failed[0].pass, FAILED_PASS_NAME);
        assert_eq!(outcome.failed[0].repo, "acme/shop");
    }
}

#[tokio::test]
async fn a_transport_error_is_a_failed_batch_not_a_finding_and_not_a_crash() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|_, _| Err(anyhow::anyhow!("429 rate limited")));
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(counts(&outcome).unjudged, 1);
    assert_eq!(outcome.not_assessed.len(), 1);
    assert!(
        outcome.failed[0].reason.contains("429 rate limited"),
        "{:?}",
        outcome.failed
    );
    assert!(outcome.failed[0].reason.contains(RULE.id()));
}

#[tokio::test]
async fn a_failed_batch_loses_only_that_batch() {
    let (all, set) = synthetic_set(40, 0);
    // Batches of 16, 16, 8: the second fails, the others confirm.
    let stub = StubJudge::new(|req, call| {
        if call == 1 {
            Err(anyhow::anyhow!("timeout"))
        } else {
            Ok(verdicts(&asked_ids(req), "same_decision", "the discount"))
        }
    });
    let outcome = judge(Some(&stub), &all, set).await;
    assert_eq!(stub.call_count(), 3);
    let c = counts(&outcome);
    assert_eq!(
        (c.found, c.judged, c.confirmed, c.unjudged),
        (40, 24, 24, 16)
    );
    assert_eq!(outcome.findings.len(), 24);
    assert_eq!(
        outcome.not_assessed.len(),
        1,
        "one row for the whole remainder"
    );
    assert!(
        outcome.not_assessed[0].detail.contains("16 of the 40"),
        "{}",
        outcome.not_assessed[0].detail
    );
}

#[tokio::test]
async fn a_verdict_for_an_unknown_id_is_ignored() {
    let all = discount_repo("0.2", "0.15");
    // Only an id that was never asked about.
    let stub = StubJudge::new(|_, _| {
        Ok(verdicts(
            &["ARCH-SINGLE-SOURCE-LOGIC-1:deadbeefdeadbeef:99".to_string()],
            "same_decision",
            "the x",
        ))
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert!(outcome.findings.is_empty(), "{outcome:#?}");
    assert_eq!(counts(&outcome).confirmed, 0);
    assert_eq!(counts(&outcome).unjudged, 1);
}

#[tokio::test]
async fn a_verdict_word_outside_the_vocabulary_never_ships() {
    let all = discount_repo("0.2", "0.15");
    for word in [
        "yes",
        "true",
        "same",
        "same_concept",
        "same_mechanism",
        "SAME-DECISION",
        " ",
    ] {
        let stub = StubJudge::answering(word, "the x");
        let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
        assert!(outcome.findings.is_empty(), "{word:?}");
        assert_eq!(counts(&outcome).unjudged, 1, "{word:?}");
    }
    // The affirmative word is accepted in any case and with surrounding spaces.
    let stub = StubJudge::answering(" Same_Decision ", "the x");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(outcome.findings.len(), 1);
}

#[tokio::test]
async fn contradictory_verdicts_for_one_id_do_not_ship() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|req, _| {
        let id = asked_ids(req).remove(0);
        Ok(serde_json::json!({ "verdicts": [
            { "id": id, "verdict": "same_decision", "concept": "the x" },
            { "id": id, "verdict": "different", "concept": "" },
        ]})
        .to_string())
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert!(outcome.findings.is_empty());
    assert_eq!(counts(&outcome).unjudged, 1);
}

#[tokio::test]
async fn text_around_the_json_and_fences_are_tolerated() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|req, _| {
        Ok(format!(
            "Here is my answer:\n```json\n{}\n```\nHope that helps.",
            verdicts(&asked_ids(req), "same_decision", "the discount")
        ))
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(outcome.findings.len(), 1);
}

#[tokio::test]
async fn no_model_emits_one_not_assessed_row_per_armed_rule_with_candidates_and_no_findings() {
    // Two rules armed with candidates (single-source and naming both see the discount pair
    // under different names), the third armed with none.
    let all = discount_repo("0.2", "0.15");
    let set = set_for(&all, &CloneRule::ALL);
    assert!(set.found(CloneRule::SingleSourceLogic) > 0);
    assert!(set.found(CloneRule::NamingOneConcept) > 0);
    assert_eq!(set.found(CloneRule::SharedPrimitive), 0);

    let outcome = judge(None, &all, set).await;
    assert!(outcome.findings.is_empty(), "candidates are never promoted");
    assert_eq!(outcome.not_assessed.len(), 2, "{:#?}", outcome.not_assessed);
    let ids: Vec<&str> = outcome
        .not_assessed
        .iter()
        .map(|f| f.rule_id.as_str())
        .collect();
    assert!(
        ids.contains(&"ARCH-SINGLE-SOURCE-LOGIC-1") && ids.contains(&"ARCH-NAMING-ONE-CONCEPT-1")
    );
    for row in &outcome.not_assessed {
        assert_eq!(row.severity, "info");
        assert!(row.path.is_empty());
        assert_eq!(row.line, 0);
        assert!(!row.located);
        assert!(row.preview);
        let FindingKind::NotAssessed(kind) = row.kind.clone().unwrap() else {
            panic!();
        };
        assert_eq!(kind.reason, NotAssessedReason::PassFailed);
        assert!(kind
            .reason_detail
            .contains("advisory judgment did not run (deterministic-only scan)"));
        assert!(
            kind.reason_detail
                .contains("candidate families found, 0 judged"),
            "{kind:?}"
        );
        assert!(
            row.detail.contains("not assessed rather than clean"),
            "{}",
            row.detail
        );
        assert!(!row.detail.contains('\u{2014}'));
    }
    assert!(!outcome.model_available);
    assert_eq!(outcome.model_calls, 0);
    assert_eq!(outcome.judge_model, None, "no model call, no model stamped");
    // The rule with no candidates ran clean.
    assert_eq!(
        outcome.counts[&CloneRule::SharedPrimitive],
        JudgeCounts::default()
    );
}

#[tokio::test]
async fn the_unavailable_reason_names_a_compliance_block() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let mut req = request(&all, set_for(&all, &[RULE]));
    req.unavailable_reason = "the project is blocked from calling a model";
    let outcome = judge_clone_candidates(None, req, &mut cache, None, None).await;
    assert!(outcome.not_assessed[0]
        .detail
        .contains("blocked from calling a model"));
}

#[tokio::test]
async fn zero_candidates_emits_no_not_assessed_row_and_credits_the_rule_as_run() {
    let all = files(&[("src/a.ts", "export const a = 1;\n")]);
    let stub = StubJudge::answering("same_decision", "the x");
    let outcome = judge(Some(&stub), &all, set_for(&all, &CloneRule::ALL)).await;
    assert_eq!(stub.call_count(), 0, "nothing to ask");
    assert!(outcome.findings.is_empty() && outcome.not_assessed.is_empty());
    for rule in CloneRule::ALL {
        assert_eq!(outcome.counts[&rule], JudgeCounts::default());
        assert!(outcome.counts[&rule].fully_assessed());
    }
    // And offline, zero candidates is still a clean run, not a not-assessed row.
    let offline = judge(None, &all, set_for(&all, &CloneRule::ALL)).await;
    assert!(offline.not_assessed.is_empty());
}

#[tokio::test]
async fn over_cap_candidates_are_counted_and_disclosed_not_judged() {
    let (all, set) = synthetic_set(32, 7);
    let stub = StubJudge::answering("same_decision", "the discount");
    let outcome = judge(Some(&stub), &all, set).await;
    let c = counts(&outcome);
    assert_eq!(
        (c.found, c.judged, c.confirmed, c.over_cap, c.unjudged),
        (39, 32, 32, 7, 0)
    );
    assert_eq!(
        stub.call_count(),
        2,
        "32 judged candidates are two batches; the 7 are never sent"
    );
    assert_eq!(outcome.findings.len(), 32);
    // Disclosed: one row, with the count and the limit.
    assert_eq!(outcome.not_assessed.len(), 1);
    let row = &outcome.not_assessed[0];
    assert!(row.detail.contains("7 of the 39"), "{}", row.detail);
    assert!(row.detail.contains("limit of 160"), "{}", row.detail);
    assert!(!c.fully_assessed());
}

#[tokio::test]
async fn candidates_are_judged_in_batches_of_sixteen_in_rank_order() {
    let (all, set) = synthetic_set(40, 0);
    let order: Vec<String> = set.by_rule[&RULE].iter().map(|c| c.id.clone()).collect();
    let stub = StubJudge::answering("different", "");
    let _ = judge(Some(&stub), &all, set).await;
    assert_eq!(stub.call_count(), 3);
    let sizes: Vec<usize> = (0..3).map(|n| asked_ids(&stub.request(n)).len()).collect();
    assert_eq!(sizes, [16, 16, 8]);
    let asked: Vec<String> = (0..3).flat_map(|n| asked_ids(&stub.request(n))).collect();
    assert_eq!(
        asked, order,
        "ranked order, no candidate dropped or repeated"
    );
}

#[tokio::test]
async fn the_request_is_bounded_cached_and_on_the_judge_model() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("different", "");
    let _ = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let req = stub.request(0);
    assert_eq!(req.model, JUDGE);
    assert!(req
        .system
        .as_deref()
        .unwrap()
        .contains("exactly one verdict word"));
    assert!(req.system.as_deref().unwrap().contains("same_decision"));
    assert_eq!(
        req.max_tokens, 4_096,
        "scaled_max_tokens(16, 80) is the floor"
    );
    // One cache breakpoint, after the stable prefix and before the candidates.
    assert_eq!(req.cache_breakpoints.len(), 1);
    let (prefix, rest) = req.prompt.split_at(req.cache_breakpoints[0]);
    assert!(prefix.starts_with("Repository: acme/shop\nRule ARCH-SINGLE-SOURCE-LOGIC-1"));
    assert!(rest.starts_with(BATCH_MARKER));
}

#[tokio::test]
async fn the_judge_never_sees_a_whole_file() {
    // A file with 400 extra lines after the function; none of them may reach the prompt.
    let padding: String = (0..400).map(|i| format!("// padding line {i}\n")).collect();
    let a = format!("{}{padding}", discount("applyDiscount", "0.2"));
    let b = format!("{}{padding}", discount("quoteTotal", "0.2"));
    let all = files(&[("src/a.ts", &a), ("src/b.ts", &b)]);
    let stub = StubJudge::answering("different", "");
    let _ = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let prompt = stub.request(0).prompt;
    assert!(
        !prompt.contains("padding line 3"),
        "no line outside the span is sent"
    );
    assert!(prompt.contains("user.isPremium"));
    assert!(prompt.len() < 6_000, "{} bytes", prompt.len());
}

#[tokio::test]
async fn usage_is_folded_into_the_meter() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("different", "");
    let meter = UsageMeter::default();
    let mut cache = VerdictCache::default();
    let _ = judge_clone_candidates(
        Some(&stub),
        request(&all, set_for(&all, &[RULE])),
        &mut cache,
        Some(&meter),
        None,
    )
    .await;
    let usage = meter.snapshot();
    assert_eq!(usage.calls, 1);
    assert_eq!(usage.input_tokens, 100);
}

#[tokio::test]
async fn candidate_list_and_verdicts_are_written_to_the_transcript() {
    let all = discount_repo("0.2", "0.15");
    let set = set_for(&all, &[RULE]);
    let id = set.by_rule[&RULE][0].id.clone();
    let stub = StubJudge::answering("same_decision", "the premium discount calculation");
    let store = TranscriptStore::new();
    let mut cache = VerdictCache::default();
    let _ = judge_clone_candidates(
        Some(&stub),
        request(&all, set),
        &mut cache,
        None,
        Some((&store, "scan-key")),
    )
    .await;
    let agents = store.get("scan-key");
    assert_eq!(agents.len(), 1, "{agents:#?}");
    let agent = &agents[0];
    assert_eq!(agent.session_id, "clone-judge:ARCH-SINGLE-SOURCE-LOGIC-1");
    assert!(agent.role.contains("clone judge"), "{}", agent.role);
    // The full ranked candidate list is in the prompt; the raw response and each verdict are in
    // the output.
    assert!(agent.prompt.contains("Ranked candidates:"));
    assert!(agent.prompt.contains(&id), "{}", agent.prompt);
    assert!(agent.prompt.contains("src/cart/total.ts:2-12"));
    assert!(agent.output.contains("same_decision"));
    assert!(
        agent.output.contains(&format!("{id} -> same_decision")),
        "{}",
        agent.output
    );
    assert!(
        agent
            .output
            .contains("found 1, judged 1 (0 from cache), confirmed 1"),
        "{}",
        agent.output
    );
    assert_eq!(agent.status, "done");
}

#[tokio::test]
async fn the_transcript_lists_every_candidate_even_when_no_model_ran() {
    let all = discount_repo("0.2", "0.15");
    let set = set_for(&all, &[RULE]);
    let store = TranscriptStore::new();
    let mut cache = VerdictCache::default();
    let _ = judge_clone_candidates(
        None,
        request(&all, set),
        &mut cache,
        None,
        Some((&store, "k")),
    )
    .await;
    let agent = &store.get("k")[0];
    assert!(agent.prompt.contains("Ranked candidates:"));
    assert!(agent.output.contains("-> unjudged"), "{}", agent.output);
    assert_eq!(
        agent.status, "blocked",
        "an undecided candidate is not 'done'"
    );
}

#[tokio::test]
async fn an_unarmed_rule_has_no_entry_and_costs_nothing() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the x");
    let set = set_for(&all, &[CloneRule::SharedPrimitive]);
    let outcome = judge(Some(&stub), &all, set).await;
    assert_eq!(stub.call_count(), 0);
    assert_eq!(
        outcome.counts.keys().copied().collect::<Vec<_>>(),
        [CloneRule::SharedPrimitive]
    );
}

#[tokio::test]
async fn confirmed_findings_are_ranked_and_the_judge_input_lists_the_facts() {
    let all = files(&[
        ("src/a.ts", &discount("one", "0.2")),
        ("src/b.ts", &discount("two", "0.2")),
        ("src/c.ts", &shipping("ship", "20")),
        ("src/d.ts", &shipping("freight", "25")),
    ]);
    let stub = StubJudge::answering("same_decision", "the thing");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(outcome.findings.len(), 2);
    // Disagreeing family first (the shipping weights differ), then the agreeing one.
    assert_eq!(outcome.findings[0].severity, "high");
    assert_eq!(outcome.findings[1].severity, "medium");
    let sent = stub.request(0).prompt;
    assert!(sent.contains("\"agree\":false") && sent.contains("\"agree\":true"));
    assert!(
        sent.contains("\"disagreements\":[{\"a\":\"20\",\"b\":\"25\""),
        "{sent}"
    );
}
