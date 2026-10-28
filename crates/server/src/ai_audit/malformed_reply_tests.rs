//! H4 item 1: a scan pass whose reply is malformed (complete, but not JSON) is retried a bounded
//! number of times and, if it never answers properly, is a FAILED pass naming how many files
//! the AI pass did not review. A malformed reply is never parsed as "zero findings". No
//! network: every model is a scripted stub.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";
const REPO_MAP: &str = "REPO MAP:\na.rs: fn a()\n";
const MALFORMED: &str = "Sure! Here are the findings you asked for, in prose rather than JSON.";
const ONE_FINDING: &str = r#"{"findings":[{"path":"a.rs","line":1,"severity":"high","rule":"RULE-0","title":"t","detail":"d"}],"proposed_rules":[]}"#;

/// Answers each call with the next scripted reply; the last reply repeats once the script runs
/// out.
struct ScriptedLlm {
    replies: Mutex<Vec<(&'static str, bool)>>,
    calls: AtomicUsize,
}

impl ScriptedLlm {
    fn new(replies: &[(&'static str, bool)]) -> Self {
        Self {
            replies: Mutex::new(replies.to_vec()),
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn response(text: &str, truncated: bool) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated,
    }
}

#[async_trait::async_trait]
impl LlmPort for ScriptedLlm {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut replies = self.replies.lock().unwrap();
        let (text, truncated) = if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies[0]
        };
        Ok(response(text, truncated))
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

/// One chunk of `n_files` files and one rule-batch through the real `run_passes`.
async fn scan(llm: &ScriptedLlm, n_files: usize) -> PassesOut {
    let files: Vec<(String, String)> = (0..n_files)
        .map(|i| (format!("f{i}.rs"), format!("fn f{i}() {{}}")))
        .collect();
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules = vec![("RULE-0".to_string(), "rule zero".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let adopted: std::collections::HashSet<String> = ["RULE-0".to_string()].into();
    run_passes(
        llm, REPO, REPO_MAP, &adopted, None, None, None, &chunks, &batches, 1, "pass", "mal", None,
        false,
    )
    .await
}

#[tokio::test]
async fn a_malformed_reply_that_never_recovers_is_a_failed_pass_not_zero_findings() {
    let llm = ScriptedLlm::new(&[(MALFORMED, false)]);
    let (findings, _p, _r, ok, failures) = scan(&llm, 3).await;
    assert_eq!(
        ok, 0,
        "a malformed reply must never count as a pass that answered"
    );
    assert!(findings.is_empty());
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(
        failures[0].files, 3,
        "the failure names how many files went unreviewed"
    );
    assert!(
        failures[0].error.contains("not valid JSON"),
        "{}",
        failures[0].error
    );
    // The first attempt plus exactly the bounded number of retries.
    assert_eq!(llm.calls(), 1 + MALFORMED_REPLY_RETRIES);
}

#[tokio::test]
async fn a_malformed_reply_is_retried_and_a_good_retry_is_used() {
    let llm = ScriptedLlm::new(&[(MALFORMED, false), (ONE_FINDING, false)]);
    let (findings, _p, _r, ok, failures) = scan(&llm, 1).await;
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(ok, 1);
    assert_eq!(findings.len(), 1);
    assert_eq!(llm.calls(), 2);
}

#[tokio::test]
async fn a_well_formed_empty_answer_is_zero_findings_and_is_not_retried() {
    let llm = ScriptedLlm::new(&[(r#"{"findings":[],"proposed_rules":[]}"#, false)]);
    let (findings, _p, _r, ok, failures) = scan(&llm, 2).await;
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(ok, 1);
    assert!(findings.is_empty());
    assert_eq!(llm.calls(), 1);
}

#[test]
fn the_retry_bound_is_small_and_nonzero() {
    assert!((1..=3).contains(&MALFORMED_REPLY_RETRIES));
}

#[test]
fn a_failed_pass_row_carries_the_unreviewed_file_count_and_says_so() {
    let failure = PassFailure::at("pass", 0, 1, 0, 1, &"reply was not valid JSON").with_files(7);
    let rows = failed_pass_rows(REPO, 0, &[failure]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].files_not_reviewed, 7);
    assert_eq!(
        rows[0].coverage_line(),
        "acme/widgets, audit pass 1/1 · rules 1/1: pass failed, 7 files not reviewed by the AI pass"
    );
}

#[test]
fn a_malformed_batch_item_is_a_failed_pass_not_zero_findings() {
    let mut results = crate::llm::BatchResults::default();
    results
        .responses
        .insert("c0-b0".to_string(), response(MALFORMED, false));
    results
        .responses
        .insert("c1-b0".to_string(), response(ONE_FINDING, false));
    let adopted: std::collections::HashSet<String> = ["RULE-0".to_string()].into();
    let (findings, _p, _r, ok, failures) = reassemble_batch_passes(
        REPO,
        "pass",
        2,
        1,
        &[(0, 0), (1, 0)],
        &[4, 2],
        &results,
        &std::collections::HashSet::new(),
        &adopted,
        None,
        None,
    );
    assert_eq!(ok, 1);
    assert_eq!(findings.len(), 1);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].pass, "pass 1/2 · rules 1/1");
    assert_eq!(failures[0].files, 4);
}
