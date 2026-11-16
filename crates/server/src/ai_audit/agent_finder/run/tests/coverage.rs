//! Build item 2: coverage from the trace, never from the model's own list.

use super::*;

fn skip_b_script() -> Vec<ScriptStep> {
    // Reads a.rs only, and writes an empty `unread.json`: it claims everything was read.
    full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    )
}

#[tokio::test]
async fn a_skipped_file_is_sent_again_whatever_the_empty_unread_list_says() {
    let agent = ScriptedAgent::new(skip_b_script());
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    let sent = agent.messages();
    assert_eq!(sent.len(), 1, "one send again turn: {sent:?}");
    assert!(
        sent[0].contains("--- src/b.rs ---") && sent[0].contains("fn item_4()"),
        "the whole file went to the model: {}",
        sent[0]
    );
    assert!(
        !sent[0].contains("--- src/a.rs ---"),
        "a.rs was read, not sent"
    );
    let c = &extras.coverage[0];
    assert_eq!(c.files["src/b.rs"], FileState::SentAgain);
    assert_eq!(c.files["src/a.rs"], FileState::ReadInFull);
    assert_eq!(
        c.claim_matched,
        Some(false),
        "the empty list was not true and is recorded as not true"
    );
}

#[tokio::test]
async fn a_file_the_second_step_also_leaves_unread_is_a_failed_part_and_reads_not_reviewed() {
    let agent = ScriptedAgent::new(skip_b_script())
        .with_follow_up(vec![], AgentStop::TurnLimit("60 turns".to_string()));
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert_eq!(failed.len(), 1, "{failed:?}");
    let f = &failed[0];
    assert_eq!(f.files, vec!["src/b.rs".to_string()]);
    assert_eq!(f.files_not_reviewed, 1);
    assert!(f.reason.contains("turn limit"), "{}", f.reason);
    assert!(!f.attempts.is_empty(), "what the attempt got back is kept");
    // The rules that cover the file are partial for the unit.
    let partial: Vec<&str> = extras
        .partial_rules
        .iter()
        .map(|p| p.rule_id.as_str())
        .collect();
    assert!(
        partial.contains(&"ARCH-ONE-1") && partial.contains(&"SEC-TWO-1"),
        "{partial:?}"
    );
    assert_eq!(extras.partial_rules[0].unit, REPO);
    // The coverage block says not reviewed.
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
    let piece = &ledger.finder_coverage()[0].pieces[0];
    assert_eq!(piece.not_reviewed, 1);
    assert_eq!(piece.not_reviewed_files, vec!["src/b.rs".to_string()]);
    assert!(!piece.complete());
    for rule in ["ARCH-ONE-1", "SEC-TWO-1"] {
        assert!(
            matches!(
                ledger.cell_state(rule, REPO, true),
                crate::scan_ledger::CellState::NotReviewed { .. }
            ),
            "{rule} must not read clean: {:?}",
            ledger.cell_state(rule, REPO, true)
        );
    }
}

#[tokio::test]
async fn the_models_own_unread_list_changes_nothing() {
    // It read both files in full and claims b.rs was not read: no send again, no failed part.
    let mut script = good_script(vec![], vec![]);
    script.pop();
    script.push(out("unread.json", &json!(["src/b.rs"])));
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(agent.messages().is_empty(), "nothing was sent again");
    let c = &extras.coverage[0];
    assert_eq!(c.files["src/b.rs"], FileState::ReadInFull);
    assert_eq!(c.claim_matched, Some(false));
}

#[tokio::test]
async fn a_file_read_in_part_is_sent_again_in_full() {
    let mut script = good_script(vec![], vec![]);
    // Replace the whole-file read of b.rs by a read of its first two lines.
    script[1] = ScriptStep::Read {
        path: "src/b.rs".to_string(),
        first_line: 1,
        count: 2,
    };
    let agent = ScriptedAgent::new(script);
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(agent.messages().len(), 1);
    assert_eq!(extras.coverage[0].files["src/b.rs"], FileState::SentAgain);
}

#[tokio::test]
async fn a_clean_place_in_a_file_that_was_shown_in_full_in_the_send_again_step_is_accepted() {
    // The agent never read b.rs, yet answered SEC-TWO-1 clean at src/b.rs:2. The send again step
    // shows b.rs in full, which is as good as a read: the answer holds up, with no rules turn.
    let agent = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            answer_clean("SEC-TWO-1", "src/b.rs", 2),
        ],
    ));
    let (_, _, _, failed, _, extras) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    let sent = agent.messages();
    assert_eq!(sent.len(), 1, "only the send again turn: {sent:?}");
    assert!(
        extras.partial_rules.is_empty(),
        "the clean answer holds: {:?}",
        extras.partial_rules
    );
}
