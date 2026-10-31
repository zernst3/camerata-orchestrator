//! Retry fixes F3, item 3: the prompt tells the model exactly what the checks require. The detail
//! block lists the names the explanation must keep, the fix block lists the names its evidence
//! lets it use, and the check reads the same list, so the sentence in the prompt and the check
//! cannot say different things. No network: every model is a stub.

use super::detail_regen::{accept_entry, names_to_keep, Entry, Reject};
use super::structured_output_tests::{files, finding, stage_of, Recorder};
use super::*;
use crate::llm::LlmRequest;

fn nonce() -> crate::prompt_frame::FrameNonce {
    crate::prompt_frame::FrameNonce::generate_avoiding(&[])
}

/// The names on the line that starts with `label` in `prompt`, between backticks.
fn listed(prompt: &str, label: &str) -> Vec<String> {
    let line = prompt
        .lines()
        .find(|l| l.starts_with(label))
        .unwrap_or_else(|| panic!("no `{label}` line in {prompt}"));
    line[label.len()..]
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn detail_finding() -> Finding {
    finding(
        "src/work.ts",
        2,
        &format!(
            "Passes raw input to doWork{HEADLINE_SEP}Skips audit_log.action and the check in lib/guard.ts before run_work."
        ),
        "doWork(raw)",
    )
}

/// Every name the detail block lists is one the check requires, a compound name as its parts, and
/// nothing the check requires is missing from the list.
#[tokio::test]
async fn the_detail_block_lists_exactly_the_names_the_check_requires() {
    let llm = Recorder::new();
    let f = detail_finding();
    let _ = detail_regen::regenerate_full_details(
        &llm,
        "o/r",
        vec![f.clone()],
        &files(),
        &[],
        &nonce(),
        None,
        None,
    )
    .await;
    let prompt = llm
        .requests()
        .into_iter()
        .find(|r| stage_of(r) == "detail")
        .expect("a detail request")
        .prompt;
    let in_prompt = listed(&prompt, "Names to keep: ");
    assert_eq!(in_prompt, names_to_keep(&f, &files()), "{prompt}");
    // The compound `audit_log.action` is listed as its parts, the path whole.
    for expected in ["audit_log", "action", "lib/guard.ts", "run_work"] {
        assert!(
            in_prompt.iter().any(|n| n == expected),
            "{expected} in {in_prompt:?}"
        );
    }
    // The check rejects an explanation that drops any one of them, naming it...
    let entry = |text: &str| Entry {
        id: "x".to_string(),
        detail: text.to_string(),
        why: None,
    };
    let full = in_prompt.join(" and ");
    assert!(accept_entry(&f, &entry(&full), "", &files()).is_ok());
    for name in &in_prompt {
        let without = in_prompt
            .iter()
            .filter(|n| *n != name)
            .cloned()
            .collect::<Vec<_>>()
            .join(" and ");
        // A part can still be a substring of another listed name (`action` of `audit_log.action`
        // does not occur in the others), so check the rejection names a missing listed name.
        match accept_entry(&f, &entry(&without), "", &files()) {
            Err(Reject::DropsNamedToken(missing)) => {
                assert!(in_prompt.contains(&missing), "{missing}");
            }
            Ok(_) => assert!(
                in_prompt
                    .iter()
                    .any(|n| n != name && n.contains(name.as_str())),
                "dropping {name} was accepted"
            ),
            other => panic!("{other:?}"),
        }
    }
    // ...and asks for nothing that is not listed: a text with the listed names and none of the
    // statement's other words is accepted.
    assert!(accept_entry(&f, &entry(&format!("{full}.")), "", &files()).is_ok());
}

/// The system prompt says what the block's line means, in the words the check enforces.
#[test]
fn the_detail_prompt_states_the_rule_the_check_enforces() {
    let system = detail_regen::detail_regen_system_prompt();
    assert!(system.contains("`Names to keep:` line"), "{system}");
    assert!(system.contains("discarded"), "{system}");
}

/// A fix block lists the names its evidence holds; every listed name passes the grounding check
/// in backticks, and a name that is not in the evidence is not listed and is rejected.
#[tokio::test]
async fn the_fix_block_lists_names_the_grounding_check_accepts() {
    let llm = Recorder::new();
    let f = finding(
        "src/work.ts",
        2,
        "`runWork` passes `raw` straight to `doWork` in lib/guard.ts",
        "doWork(raw)",
    );
    let evidence = fix_evidence_blob(&f, &files());
    let _ = generate_fix_specifics(&llm, "o/r", vec![f], &files(), None, None, None).await;
    let first: LlmRequest = llm
        .requests()
        .into_iter()
        .find(|r| stage_of(r) == "fix")
        .expect("a fix request");
    let names = listed(&first.prompt, "Names you may use: ");
    assert!(!names.is_empty(), "{}", first.prompt);
    for expected in ["runWork", "doWork", "lib/guard.ts"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    for name in &names {
        let fix = format!("Call `{name}` before the write.");
        assert_eq!(
            find_ungrounded_identifier(&fix, &evidence),
            None,
            "{name} is listed, so naming it must be accepted"
        );
    }
    assert!(!names.iter().any(|n| n == "neverInEvidence"));
    assert_eq!(
        find_ungrounded_identifier("Call `neverInEvidence` first.", &evidence).as_deref(),
        Some("neverInEvidence")
    );
    let system = first.system.as_deref().unwrap();
    assert!(system.contains("`Names you may use:` line"), "{system}");
    assert!(system.contains("describe it in plain words"), "{system}");
}
