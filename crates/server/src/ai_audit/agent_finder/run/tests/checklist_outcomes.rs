//! Build item 3: checklist outcomes, no false clean.

use super::*;

fn answers_without_sec() -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![answer_clean("ARCH-ONE-1", "src/a.rs", 1)],
    )
}

#[tokio::test]
async fn a_rule_with_no_answer_after_the_follow_up_is_a_failed_part_naming_the_rule() {
    let agent = ScriptedAgent::new(answers_without_sec());
    let (_, _, _, failed, _, extras) = run(&agent).await;
    let sent = agent.messages();
    assert_eq!(sent.len(), 1, "one follow-up: {sent:?}");
    assert!(
        sent[0].contains("SEC-TWO-1") && !sent[0].contains("ARCH-ONE-1"),
        "{}",
        sent[0]
    );
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].reason.contains("SEC-TWO-1"),
        "{}",
        failed[0].reason
    );
    assert!(extras
        .partial_rules
        .iter()
        .any(|p| p.rule_id == "SEC-TWO-1" && p.unit == REPO));
    assert!(!extras
        .partial_rules
        .iter()
        .any(|p| p.rule_id == "ARCH-ONE-1"));
}

#[tokio::test]
async fn the_follow_up_answer_joins_the_earlier_answers_and_clears_the_failure() {
    // The follow-up writes a checklist with only the missing rule; the earlier answer stays.
    let agent = ScriptedAgent::new(answers_without_sec()).with_follow_up(
        vec![out(
            "checklist.json",
            &json!({"answers": [answer_clean("SEC-TWO-1", "src/b.rs", 2)]}),
        )],
        AgentStop::Finished,
    );
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(extras.partial_rules.is_empty());
    assert_eq!(agent.messages().len(), 1);
}

#[tokio::test]
async fn a_clean_answer_with_no_valid_checked_place_is_never_clean() {
    // SEC-TWO-1 is answered clean with a line the file does not have.
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/b.rs", 99),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, extras) = run(&agent).await;
    let sent = agent.messages();
    assert_eq!(
        sent.len(),
        1,
        "a rule that did not hold up is asked about once: {sent:?}"
    );
    assert!(sent[0].contains("SEC-TWO-1"));
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].reason.contains("SEC-TWO-1"));
    assert!(extras
        .partial_rules
        .iter()
        .any(|p| p.rule_id == "SEC-TWO-1"));
    assert!(
        !extras
            .not_applicable_rules
            .contains(&"SEC-TWO-1".to_string()),
        "a clean answer that did not hold up is not a not applicable one"
    );
    // The coverage record for that rule and repository does not read clean.
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    extras.note_before_rules(&mut ledger, REPO);
    for rule in ["ARCH-ONE-1", "SEC-TWO-1"] {
        ledger.record_rule(
            REPO,
            rule,
            crate::scan_ledger::RuleTier::Semantic,
            true,
            None,
            2,
            0,
        );
    }
    extras.note_after_rules(&mut ledger, REPO);
    assert!(!ledger
        .rule_for_repo("SEC-TWO-1", REPO)
        .unwrap()
        .verified_clean());
    assert!(matches!(
        ledger.cell_state("SEC-TWO-1", REPO, true),
        crate::scan_ledger::CellState::NotReviewed { .. }
    ));
    assert!(matches!(
        ledger.cell_state("ARCH-ONE-1", REPO, true),
        crate::scan_ledger::CellState::Clean
    ));
}

#[tokio::test]
async fn a_clean_answer_naming_a_file_that_was_neither_read_nor_shown_is_not_clean() {
    // A place in a file outside the part was neither read through a tool nor shown in full (a
    // file shown in full by the send again step does count, see the coverage tests).
    let script = full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/elsewhere.rs", 1),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, _) = run(&agent).await;
    assert!(
        failed.iter().any(|f| f.reason.contains("SEC-TWO-1")),
        "{failed:?}"
    );
}

#[tokio::test]
async fn a_rule_every_piece_found_not_applicable_is_recorded_so_and_a_clean_one_is_not() {
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "not_applicable"}),
            answer_clean("SEC-TWO-1", "src/a.rs", 1),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(extras.not_applicable_rules, vec!["ARCH-ONE-1".to_string()]);
}

#[tokio::test]
async fn output_that_cannot_be_used_gets_one_repair_turn_then_the_piece_fails() {
    let mut script = good_script(vec![], vec![]);
    script[2] = ScriptStep::Write {
        path: "_camerata/out/problems-1.json".to_string(),
        text: "{ not json".to_string(),
    };
    let agent = ScriptedAgent::new(script); // the repair turn writes nothing
    let (_, _, _, failed, _, _) = run(&agent).await;
    let sent = agent.messages();
    assert_eq!(sent.len(), 1, "exactly one repair turn: {sent:?}");
    assert!(sent[0].contains("problems-1.json"), "{}", sent[0]);
    assert!(
        failed.iter().any(|f| f.reason.contains("problems-1.json")),
        "{failed:?}"
    );
}

#[tokio::test]
async fn a_repair_turn_that_fixes_the_file_clears_the_failure() {
    let mut script = good_script(vec![], vec![]);
    script[2] = ScriptStep::Write {
        path: "_camerata/out/problems-1.json".to_string(),
        text: "{ not json".to_string(),
    };
    let agent = ScriptedAgent::new(script).with_follow_up(
        vec![out("problems-1.json", &json!([]))],
        AgentStop::Finished,
    );
    let (_, _, _, failed, _, _) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
}

#[tokio::test]
async fn a_stray_file_is_reported_and_does_not_fail_the_piece() {
    let mut script = good_script(vec![], vec![]);
    script.push(ScriptStep::Write {
        path: "_camerata/out/scratch-notes.txt".to_string(),
        text: "notes".to_string(),
    });
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(
        extras.notes.iter().any(|n| n.contains("scratch-notes.txt")),
        "{:?}",
        extras.notes
    );
}

#[tokio::test]
async fn a_piece_file_under_the_reserved_folder_stays_owed_and_is_reported_plainly() {
    let mut files = repo_files();
    files.push(("_camerata/notes.rs".to_string(), rust_file(2)));
    let agent = ScriptedAgent::new(good_script(vec![], vec![]));
    let (_, _, _, failed, _, extras) =
        run_with(&agent, &files, &rules(), &corpus(), &options()).await;
    let f = failed
        .iter()
        .find(|f| f.files.contains(&"_camerata/notes.rs".to_string()))
        .unwrap_or_else(|| panic!("the owed file is a failed part: {failed:?}"));
    assert!(f.reason.contains("could not serve") && f.reason.contains("_camerata/notes.rs"));
    assert!(extras.coverage[0]
        .not_served
        .contains_key("_camerata/notes.rs"));
    assert_eq!(
        extras.coverage[0].files["_camerata/notes.rs"],
        FileState::NotReviewed
    );
}

#[tokio::test]
async fn a_send_again_that_rewrites_the_problems_file_loses_no_earlier_problem() {
    // Run 1 writes problems 1 and 2 and skips b.rs; the send again rewrites problems-1.json
    // with problem 3 only. All three problems must survive, and the rewrite is noted.
    let script = full_script(
        &[("src/a.rs", 6)],
        vec![
            problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }"),
            problem(2, "src/a.rs", 3, "low", "fn item_3() { run(3); }"),
        ],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    );
    let agent = ScriptedAgent::new(script).with_follow_up(
        vec![out(
            "problems-1.json",
            &json!([problem(3, "src/b.rs", 2, "low", "fn item_2() { run(2); }")]),
        )],
        AgentStop::Finished,
    );
    let (rows, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        extras.notes.iter().any(|n| n.contains("problems-1.json")),
        "the rewrite is an operator note: {:?}",
        extras.notes
    );
}

#[tokio::test]
async fn an_unusable_middle_version_does_not_lose_the_earlier_problems() {
    // Run 1 writes problems 1 and 2; the send again step writes broken JSON over the file; the
    // repair turn writes problem 3 only. The last usable version before the broken one is the
    // base, so all three problems survive and the note about the rewrite is true.
    let script = full_script(
        &[("src/a.rs", 6)],
        vec![
            problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }"),
            problem(2, "src/a.rs", 3, "low", "fn item_3() { run(3); }"),
        ],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    );
    let agent = ScriptedAgent::new(script)
        .with_follow_up(
            vec![ScriptStep::Write {
                path: "_camerata/out/problems-1.json".to_string(),
                text: "[{\"n\": 3,".to_string(),
            }],
            AgentStop::Finished,
        )
        .with_again(vec![out(
            "problems-1.json",
            &json!([problem(3, "src/b.rs", 2, "low", "fn item_2() { run(2); }")]),
        )]);
    let (rows, _, _, _, _, extras) = run(&agent).await;
    assert_eq!(agent.messages().len(), 2, "send again, then repair");
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        extras
            .notes
            .iter()
            .any(|n| n.contains("problems-1.json") && n.contains("were kept")),
        "{:?}",
        extras.notes
    );
}

fn one_problem_script() -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 6)],
        vec![problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }")],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    )
}

#[tokio::test]
async fn a_later_version_of_a_problem_at_the_same_place_is_a_correction_and_wins() {
    let agent = ScriptedAgent::new(one_problem_script()).with_follow_up(
        vec![out(
            "problems-1.json",
            &json!([problem(1, "src/a.rs", 2, "low", "fn item_2() { run(2); }")]),
        )],
        AgentStop::Finished,
    );
    let (rows, _, _, _, _, _) = run(&agent).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].line, 2);
    assert_eq!(
        rows[0].severity.to_string().to_lowercase(),
        "low",
        "the later version is the one kept"
    );
}

/// An intended change (round 2, defect 2): a problem number reused at a different place used to
/// erase the earlier problem silently; now the later file is unusable, like a number used twice
/// across files, so the agent is asked to write it again and, if it does not, the part fails.
#[tokio::test]
async fn a_problem_number_reused_at_a_different_place_is_a_repair_turn_then_a_failed_part() {
    let agent = ScriptedAgent::new(one_problem_script()).with_follow_up(
        vec![out(
            "problems-1.json",
            &json!([problem(1, "src/a.rs", 4, "low", "fn item_4() { run(4); }")]),
        )],
        AgentStop::Finished,
    );
    let (rows, _, _, failed, _, _) = run(&agent).await;
    assert_ne!(rows.len(), 1, "never one row for two places: {rows:?}");
    assert!(
        agent
            .messages()
            .iter()
            .any(|m| m.contains("problems-1.json") && m.contains("earlier step")),
        "a repair turn names the file: {:?}",
        agent.messages()
    );
    assert!(
        failed
            .iter()
            .any(|f| f.reason.contains("problems-1.json") && f.reason.contains("used twice")),
        "{failed:?}"
    );
}

#[tokio::test]
async fn a_stray_problems_file_that_is_not_a_problems_list_is_repaired_then_a_failed_part() {
    for name in ["problems_2.json", "problems-2a.json", "problems.json"] {
        let mut script = good_script(vec![], vec![]);
        script.push(out(name, &json!({"note": "nothing here"})));
        let agent = ScriptedAgent::new(script);
        let (_, _, _, failed, _, _) = run(&agent).await;
        assert_eq!(agent.messages().len(), 1, "{name}: one repair turn");
        assert!(
            agent.messages()[0].contains(name),
            "{name}: {:?}",
            agent.messages()
        );
        assert_eq!(failed.len(), 1, "{name}: {failed:?}");
        assert!(
            failed[0].reason.contains(name),
            "{name}: {}",
            failed[0].reason
        );
    }
}

/// Follow-up (round 2): the agent has no delete tool, so a stray `problems_2.json` that already
/// holds a problems list could never be repaired and would be paid for again on a resume. Any
/// `problems*.json` that parses as a problems list is read as one.
#[tokio::test]
async fn a_stray_problems_file_that_is_a_problems_list_is_read_as_one() {
    for name in ["problems_2.json", "problems-2a.json", "problems.json"] {
        let mut script = good_script(vec![], vec![]);
        script.push(out(
            name,
            &json!([problem(7, "src/b.rs", 3, "high", "fn item_3() { run(3); }")]),
        ));
        let agent = ScriptedAgent::new(script);
        let (rows, _, _, failed, _, _) = run(&agent).await;
        assert!(
            agent.messages().is_empty(),
            "{name}: {:?}",
            agent.messages()
        );
        assert!(failed.is_empty(), "{name}: {failed:?}");
        assert_eq!(rows.len(), 1, "{name}: {rows:?}");
        assert_eq!((rows[0].path.as_str(), rows[0].line), ("src/b.rs", 3));
    }
}

#[tokio::test]
async fn a_not_a_problem_here_answer_makes_no_row_is_noted_for_the_reviewer_and_is_not_partial() {
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "not_a_problem_here",
                   "place": {"path": "src/a.rs", "line": 2}, "reason": "The input is checked first."}),
            answer_clean("SEC-TWO-1", "src/a.rs", 1),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (findings, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(findings.is_empty(), "{findings:?}");
    assert!(extras.partial_rules.is_empty());
    assert!(
        extras.not_applicable_rules.is_empty(),
        "assessed is not judged not applicable"
    );
    // The ledger never reads the rule as verified clean, and it is in no other bucket.
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    extras.note_before_rules(&mut ledger, REPO);
    for rule in ["ARCH-ONE-1", "SEC-TWO-1"] {
        ledger.record_rule(
            REPO,
            rule,
            crate::scan_ledger::RuleTier::Semantic,
            true,
            None,
            2,
            0,
        );
    }
    extras.note_after_rules(&mut ledger, REPO);
    assert_eq!(ledger.healthy_rule_ids(), vec!["SEC-TWO-1".to_string()]);
    assert!(ledger.not_applicable_rule_ids().is_empty());
    assert!(ledger.excluded_rules().is_empty());
    assert!(
        extras
            .not_a_problem_notes
            .iter()
            .any(|n| n.contains("ARCH-ONE-1")
                && n.contains("src/a.rs:2")
                && n.contains("The input is checked first.")),
        "{:?}",
        extras.not_a_problem_notes
    );
    assert_eq!(extras.not_a_problem_rules, vec!["ARCH-ONE-1".to_string()]);
}

#[tokio::test]
async fn a_rule_answered_not_a_problem_here_in_one_piece_and_clean_in_another_is_not_marked() {
    let nph = |path: &str| {
        json!({"rule": "ARCH-ONE-1", "answer": "not_a_problem_here",
               "place": {"path": path, "line": 2}, "reason": "Fine here."})
    };
    let a = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![nph("src/a.rs"), answer_clean("SEC-TWO-1", "src/a.rs", 2)],
    ));
    let b = ScriptedAgent::new(full_script(
        &[("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/b.rs", 1),
            answer_clean("SEC-TWO-1", "src/b.rs", 2),
        ],
    ));
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([])),
    ]);
    let agent = ByPiece::new(vec![("src/a.rs", a), ("src/b.rs", b), ("joints", joints)]);
    let mut opts = options();
    opts.force_pieces = Some(2);
    let (_, _, _, failed, _, extras) =
        run_with(&agent, &repo_files(), &rules(), &corpus(), &opts).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(extras.not_a_problem_rules.is_empty(), "{extras:?}");
    assert_eq!(
        extras.not_a_problem_notes.len(),
        1,
        "the note is still kept"
    );
}

#[tokio::test]
async fn a_not_a_problem_here_place_that_was_not_read_makes_the_rule_not_completed() {
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "not_a_problem_here",
                   "place": {"path": "src/elsewhere.rs", "line": 2}, "reason": "Fine."}),
            answer_clean("SEC-TWO-1", "src/a.rs", 1),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(
        failed.iter().any(|f| f.reason.contains("ARCH-ONE-1")),
        "{failed:?}"
    );
    assert!(extras.not_a_problem_notes.is_empty());
    assert!(extras.not_a_problem_rules.is_empty());
}
