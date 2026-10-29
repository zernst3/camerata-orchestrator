//! Run to completion (Zach, 2026-10-28): "We can't ship a product to anyone that didn't fully run
//! end-to-end properly." Failure classes, the end-of-run second try, the outage stop. Fake model
//! only; no live calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_llm::failure::{FailureClass, LlmCallFailure};

const REPO: &str = "acme/api";
const REPO_MAP: &str = "map";
const GOOD: &str = r#"{"findings":[]}"#;
const MALFORMED: &str = "I could not produce JSON for this, sorry.";

fn response(text: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// A typed transport failure of the given class, as the real transport returns it.
fn typed(class: FailureClass, message: &str) -> anyhow::Error {
    let mut f = LlmCallFailure::new(class, message);
    f.attempts.push(camerata_llm::failure::AttemptRecord {
        at: "2026-10-28T10:00:00Z".to_string(),
        backend: "api".to_string(),
        model: "stub".to_string(),
        http_status: Some(529),
        error_type: Some("overloaded_error".to_string()),
        message: "Overloaded".to_string(),
        request_id: Some("req_abc".to_string()),
        ..Default::default()
    });
    anyhow::Error::new(f)
}

/// What the scripted model does on one call.
#[derive(Clone)]
enum Step {
    Reply(&'static str),
    Fail(FailureClass, &'static str),
}

/// Answers each call from a script, keyed by call number; past the end it repeats the last step.
/// Counts calls.
struct Scripted {
    steps: Vec<Step>,
    calls: AtomicUsize,
    seen: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(steps: &[Step]) -> Self {
        Self {
            steps: steps.to_vec(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(req.prompt.clone());
        match self.steps[n.min(self.steps.len() - 1)].clone() {
            Step::Reply(t) => Ok(response(t)),
            Step::Fail(c, m) => Err(typed(c, m)),
        }
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

/// `n_chunks` single-file chunks and one rule-batch through the real `run_passes`.
async fn scan(llm: &dyn LlmPort, n_chunks: usize, meter: Option<&UsageMeter>) -> PassesOut {
    let files: Vec<Vec<(String, String)>> = (0..n_chunks)
        .map(|i| vec![(format!("f{i}.rs"), format!("fn f{i}() {{}}"))])
        .collect();
    let chunks: Vec<&[(String, String)]> = files.iter().map(Vec::as_slice).collect();
    let rules = vec![("RULE-0".to_string(), "rule zero".to_string())];
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let adopted: std::collections::HashSet<String> = ["RULE-0".to_string()].into();
    run_passes(
        llm,
        REPO,
        REPO_MAP,
        &crate::prompt_frame::FrameNonce::generate(),
        &adopted,
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        1,
        "pass",
        "rc",
        meter,
        false,
    )
    .await
}

// ── Item 1: failure classes ──────────────────────────────────────────────────────────────

#[test]
fn a_failed_pass_from_an_old_ledger_loads_as_content_with_no_trail() {
    let old = r#"{"repo":"a/b","pass":"audit pass[all] 1/1","reason":"timed out"}"#;
    let fp: FailedPass = serde_json::from_str(old).expect("an old ledger row still loads");
    assert_eq!(fp.class, FailureClass::Content);
    assert!(fp.attempts.is_empty());
    assert!(fp.files.is_empty());
}

#[test]
fn a_not_run_pass_from_an_old_ledger_is_a_spend_limit_refusal() {
    let old = r#"{"repo":"a/b","group":"pass[all]","chunk":1,"n_chunks":1,"rule_batch":1,
        "n_rule_batches":1,"rule_ids":["R"]}"#;
    let p: crate::cost_cap::NotRunPass = serde_json::from_str(old).expect("loads");
    assert_eq!(p.class, FailureClass::SpendLimit);
    let old_stage = r#"{"repo":"a/b","stage":"calibration"}"#;
    let s: crate::cost_cap::NotRunStage = serde_json::from_str(old_stage).expect("loads");
    assert_eq!(s.class, FailureClass::SpendLimit);
}

#[tokio::test]
async fn a_pass_that_failed_on_the_transport_carries_the_transport_class_and_its_trail() {
    let llm = Scripted::new(&[Step::Fail(
        FailureClass::Transport,
        "Anthropic API HTTP 529: x",
    )]);
    let (_f, _p, _r, ok, failures) = scan(&llm, 1, None).await;
    assert_eq!(ok, 0);
    let rows = failed_pass_rows(REPO, ok, &failures);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].class, FailureClass::Transport);
    assert_eq!(rows[0].files, vec!["f0.rs".to_string()]);
    assert_eq!(
        rows[0].attempts.len(),
        1,
        "the transport's trail rides along"
    );
    assert_eq!(rows[0].attempts[0].http_status, Some(529));
    assert_eq!(rows[0].attempts[0].request_id.as_deref(), Some("req_abc"));
}

#[tokio::test]
async fn auth_content_and_malformed_failures_each_get_their_own_class() {
    let auth = Scripted::new(&[Step::Fail(
        FailureClass::AuthOrBilling,
        "Anthropic API HTTP 401",
    )]);
    let (_, _, _, ok, failures) = scan(&auth, 1, None).await;
    assert_eq!(
        failed_pass_rows(REPO, ok, &failures)[0].class,
        FailureClass::AuthOrBilling
    );

    let malformed = Scripted::new(&[Step::Reply(MALFORMED)]);
    let (_, _, _, ok, failures) = scan(&malformed, 1, None).await;
    assert_eq!(
        failed_pass_rows(REPO, ok, &failures)[0].class,
        FailureClass::Content,
        "a malformed reply after its retries is a content failure"
    );
}

#[test]
fn the_spend_limit_stopping_a_retry_is_a_spend_limit_failure_not_a_billing_one() {
    let e = spend_limit_before_retry_error();
    assert_eq!(classify_pass_error(&e), FailureClass::SpendLimit);
    assert_eq!(e.to_string(), SPEND_LIMIT_BEFORE_RETRY);
}

#[test]
fn a_model_refusal_and_a_cut_off_reply_are_content() {
    let refusal = anyhow::Error::new(crate::llm::ModelRefusal { billed: None });
    assert_eq!(classify_pass_error(&refusal), FailureClass::Content);
    let partial: anyhow::Error = PartialPassReply::from_cut_off(
        REPO,
        "{\"findings\":[",
        &std::collections::HashSet::new(),
        2,
    )
    .into();
    assert_eq!(classify_pass_error(&partial), FailureClass::Content);
}
