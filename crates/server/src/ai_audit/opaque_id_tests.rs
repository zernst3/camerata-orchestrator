//! Retry fixes F3, item 2: ids Camerata already knows are never echoed. The detail stage's reply
//! is addressed by the opaque id alone (the finding's path and line are attached by Camerata),
//! and the fix stage moves from batch position to the same kind of opaque id.
//!
//! The detail half is pinned in `detail_regen_tests` and `structured_output_tests`; this file
//! pins the fix half. No network: every model is a stub.

use super::reply_schemas::check::prompt_ids;
use super::structured_output_tests::{files, finding, response, stage_of, Recorder};
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

/// Answers the fix prompt with a fix per id, in REVERSE order of the prompt, each fix saying
/// which prompt position it was written for.
struct ReversedFixes;

#[async_trait::async_trait]
impl LlmPort for ReversedFixes {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let fixes: Vec<serde_json::Value> = prompt_ids(&req.prompt)
            .iter()
            .enumerate()
            .rev()
            .map(|(pos, id)| {
                serde_json::json!({
                    "id": id,
                    "fix": format!("Add a guard for the case number {pos}.")
                })
            })
            .collect();
        Ok(response(
            &serde_json::json!({ "fixes": fixes }).to_string(),
            "api",
            "claude-sonnet-5-5",
        ))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn two_findings() -> Vec<Finding> {
    let mut f0 = finding(
        "src/work.ts",
        2,
        "`runWork` passes `raw` to `doWork`",
        "doWork(raw)",
    );
    f0.rule_id = "ARCH-A-1".to_string();
    let mut f1 = finding("src/work.ts", 1, "`runWork` takes `raw`", "runWork");
    f1.rule_id = "ARCH-B-1".to_string();
    vec![f0, f1]
}

/// A reply in reverse order still lands each fix on its own finding: the id is the address.
#[tokio::test]
async fn a_fix_reply_is_matched_to_its_finding_by_id_not_by_position() {
    let (out, failed) = generate_fix_specifics(
        &ReversedFixes,
        "o/r",
        two_findings(),
        &files(),
        None,
        None,
        None,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Add a guard for the case number 0.")
    );
    assert_eq!(
        out[1].fix_specific.as_deref(),
        Some("Add a guard for the case number 1.")
    );
}

/// A reply addressed by position (what a model numbering from 0 or 1 would write) matches no
/// finding: nothing lands on a neighbour.
#[test]
fn a_position_is_no_address_for_a_fix() {
    let by_position =
        r#"{"fixes":[{"index":0,"fix":"Add a guard."},{"index":1,"fix":"Add one."}]}"#;
    assert!(parse_fix_specifics(by_position).is_empty());
    let numbered = r#"{"fixes":[{"id":"1","fix":"Add a guard."}]}"#;
    let parsed = parse_fix_specifics(numbered);
    assert!(
        parsed.contains_key("1"),
        "the parser keeps what it is given"
    );
    // ...and the stage looks up only its own ids, which are never a bare number.
}

/// The fix prompt gives each finding an opaque id, six hex digits, distinct within the batch, and
/// names no batch position; the reply shape in the system prompt is by id.
#[tokio::test]
async fn the_fix_prompt_addresses_findings_by_distinct_opaque_ids() {
    let llm = Recorder::new();
    let _ = generate_fix_specifics(&llm, "o/r", two_findings(), &files(), None, None, None).await;
    let first = llm
        .requests()
        .into_iter()
        .find(|r| stage_of(r) == "fix")
        .expect("a fix request");
    let ids = prompt_ids(&first.prompt);
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    for id in &ids {
        assert_eq!(id.len(), 6);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
    assert!(!first.prompt.contains("[0] rule"), "{}", first.prompt);
    let system = first.system.as_deref().unwrap();
    assert!(system.contains(r#"{"fixes":[{"id":"#), "{system}");
    assert!(!system.contains("[index]"), "{system}");
}

/// A finding keeps its id from one round to the next (the id hashes the finding's place in the
/// run, not its place in the batch), so a retry's feedback and a resume find the same finding.
#[tokio::test]
async fn a_finding_keeps_its_fix_id_across_rounds() {
    // Every reply names something not in the evidence, so the pass asks again; the rounds ask
    // about the same two findings.
    let llm = Recorder::new().replying(r#"{"fixes":[]}"#);
    let _ = generate_fix_specifics(&llm, "o/r", two_findings(), &files(), None, None, None).await;
    let rounds: Vec<Vec<String>> = llm
        .requests()
        .into_iter()
        .filter(|r| stage_of(r) == "fix")
        .map(|r| prompt_ids(&r.prompt))
        .collect();
    assert!(rounds.len() >= 3, "{rounds:?}");
    // The batched rounds repeat the same two ids; the solo round's ids are among them.
    assert_eq!(rounds[0], rounds[1]);
    assert!(rounds
        .last()
        .unwrap()
        .iter()
        .all(|id| rounds[0].contains(id)));
}
