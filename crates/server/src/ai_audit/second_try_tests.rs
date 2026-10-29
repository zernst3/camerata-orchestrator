//! Run to completion: the end-of-run second try for the sub-passes (fix, estimation, alternatives).
//! The export lock refuses any failed part, so a part that failed once for a passing reason must
//! get one more try, through the same admission and spend checks, before it is called failed.
//! Fake model only; no live calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_llm::failure::{FailureClass, LlmCallFailure};

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

/// What the stub does with a call it is told to fail.
#[derive(Clone, Copy)]
enum Fail {
    /// A reply that holds nothing usable.
    Empty,
    /// The provider is unreachable.
    Transport,
}

/// Fails the first `fail_first` calls, then answers `good`. Records every system prompt.
struct FailThenGood {
    fail_first: usize,
    how: Fail,
    good: &'static str,
    empty: &'static str,
    calls: AtomicUsize,
    systems: Mutex<Vec<String>>,
}

impl FailThenGood {
    fn new(fail_first: usize, how: Fail, good: &'static str, empty: &'static str) -> Self {
        Self {
            fail_first,
            how,
            good,
            empty,
            calls: AtomicUsize::new(0),
            systems: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for FailThenGood {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.systems
            .lock()
            .unwrap()
            .push(req.system.clone().unwrap_or_default());
        if n < self.fail_first {
            return match self.how {
                Fail::Empty => Ok(response(self.empty)),
                Fail::Transport => Err(anyhow::Error::new(LlmCallFailure::new(
                    FailureClass::Transport,
                    "Anthropic API HTTP 529: overloaded",
                ))),
            };
        }
        Ok(response(self.good))
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

/// A report that is complete apart from the given failed passes.
fn report_with(failed: Vec<FailedPass>) -> crate::onboard::ScanReport {
    let mut r = crate::release_gate::test_scan_report();
    r.failed_passes = failed;
    r
}

// ── Fix ───────────────────────────────────────────────────────────────────────────────────

const GOOD_FIX: &str =
    r#"{"fixes":[{"index":0,"fix":"Validate `raw` before it reaches `query`."}]}"#;
const NO_FIX: &str = r#"{"fixes":[]}"#;

fn finding_with_code() -> (Finding, Vec<(String, String)>) {
    let f = Finding {
        repo: "o/r".to_string(),
        path: "src/db.ts".to_string(),
        line: 2,
        rule_id: "ARCH-SECOND-TRY-1".to_string(),
        severity: "medium".to_string(),
        snippet: "query(raw)".to_string(),
        detail: "`runQuery` passes `raw` straight to `query`".to_string(),
        ..Finding::default()
    };
    let files = vec![(
        "src/db.ts".to_string(),
        "export function runQuery(raw: string) {\n  return query(raw);\n}\n".to_string(),
    )];
    (f, files)
}

/// The fix stage's own rounds (three batched, one solo) all fail; the end-of-run second try gets
/// the fix. No failed pass, so the client export is allowed.
#[tokio::test]
async fn a_fix_that_fails_every_round_then_succeeds_on_the_second_try_leaves_no_failed_pass() {
    let (f, files) = finding_with_code();
    let llm = FailThenGood::new(MAX_FIX_REGENERATIONS + 2, Fail::Empty, GOOD_FIX, NO_FIX);
    let (out, failed) =
        generate_fix_specifics(&llm, "o/r", vec![f], &files, None, None, None).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        out[0].fix_specific.as_deref(),
        Some("Validate `raw` before it reaches `query`.")
    );
    assert_eq!(
        llm.calls(),
        MAX_FIX_REGENERATIONS + 3,
        "one more call: the second try"
    );
    assert!(report_with(failed).incomplete_reasons().is_empty());
}

#[tokio::test]
async fn a_fix_whose_transport_failed_then_recovers_on_the_second_try_is_not_a_failed_part() {
    let (f, files) = finding_with_code();
    let llm = FailThenGood::new(MAX_FIX_REGENERATIONS + 2, Fail::Transport, GOOD_FIX, NO_FIX);
    let (out, failed) =
        generate_fix_specifics(&llm, "o/r", vec![f], &files, None, None, None).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(out[0].fix_specific.is_some());
}

#[tokio::test]
async fn a_fix_that_fails_both_tries_records_both_with_times_and_the_class() {
    let (f, files) = finding_with_code();
    let llm = FailThenGood::new(usize::MAX, Fail::Transport, GOOD_FIX, NO_FIX);
    let (_out, failed) =
        generate_fix_specifics(&llm, "o/r", vec![f], &files, None, None, None).await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].class, FailureClass::Transport);
    assert!(
        failed[0].attempts.iter().any(|a| a.second_try),
        "the second try is on the record: {:?}",
        failed[0].attempts
    );
    assert!(failed[0].attempts.iter().any(|a| !a.second_try));
    assert!(failed[0].attempts.iter().all(|a| !a.at.is_empty()));
    assert_eq!(
        llm.calls(),
        MAX_FIX_REGENERATIONS + 3,
        "exactly one second try"
    );
}

#[tokio::test]
async fn a_stopped_run_sends_no_second_fix_try() {
    let (f, files) = finding_with_code();
    let llm = FailThenGood::new(usize::MAX, Fail::Transport, GOOD_FIX, NO_FIX);
    let meter = UsageMeter::default();
    let (_out, failed) =
        generate_fix_specifics(&llm, "o/r", vec![f], &files, None, Some(&meter), None).await;
    // Three transport failures in a row stop the run; nothing is sent after that.
    assert!(meter.outage_stopped());
    assert_eq!(llm.calls(), 3);
    assert_eq!(failed.len(), 1);
}

// ── Estimation ─────────────────────────────────────────────────────────────────────────────

const VERDICT_WITH_EFFORT: &str = r#"{"verdicts":[{"index":0,"severity":"high","confidence":"high","reason":"x","effort":"low"}]}"#;

fn calibration_finding() -> Finding {
    Finding {
        repo: "me/api".to_string(),
        path: "src/a.rs".to_string(),
        line: 3,
        rule_id: "AI-X".to_string(),
        severity: "medium".to_string(),
        snippet: "x".to_string(),
        detail: "d".to_string(),
        ..Finding::default()
    }
}

#[tokio::test]
async fn a_failed_calibration_call_is_tried_again_at_the_end_and_the_estimate_lands() {
    let llm = FailThenGood::new(1, Fail::Transport, VERDICT_WITH_EFFORT, "");
    let (out, failed) = verify_findings(
        &llm,
        "me/api",
        vec![calibration_finding()],
        None,
        None,
        None,
        false,
        "This repository has 1 code files.",
    )
    .await;
    assert!(failed.is_empty(), "no failed estimation pass: {failed:?}");
    assert_eq!(out[0].effort.as_deref(), Some("low"));
    assert_eq!(llm.calls(), 2);
    assert!(report_with(failed).incomplete_reasons().is_empty());
}

#[tokio::test]
async fn estimation_that_fails_both_tries_is_a_failed_pass_with_both_on_record() {
    let llm = FailThenGood::new(usize::MAX, Fail::Transport, VERDICT_WITH_EFFORT, "");
    let (_out, failed) = verify_findings(
        &llm,
        "me/api",
        vec![calibration_finding()],
        None,
        None,
        None,
        false,
        "This repository has 1 code files.",
    )
    .await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].pass, "hour estimation");
    assert_eq!(failed[0].class, FailureClass::Transport);
    assert_eq!(failed[0].attempts.len(), 2);
    assert!(failed[0].attempts[1].second_try);
    assert_eq!(llm.calls(), 2, "exactly one second try");
}

// ── Alternatives ───────────────────────────────────────────────────────────────────────────

const GOOD_ALT: &str = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","applicable":true,"evidence":"a.rs:1"}]}"#;

fn two_option_alternatives() -> RuleAlternatives {
    let option = |id: &str, directive: &str| RuleOptionView {
        id: id.to_string(),
        label: id.to_string(),
        directive: directive.to_string(),
        why: "why".to_string(),
        violates: false,
    };
    RuleAlternatives {
        rule_id: "MULTI-RULE-1".to_string(),
        options: vec![
            option("opt-a", "Do it the A way."),
            option("opt-b", "Do it the B way."),
        ],
        selected_option_id: Some("opt-a".to_string()),
        kind: Some(camerata_rules::RuleKind::Decision),
    }
}

#[tokio::test]
async fn a_failed_alternatives_call_gets_a_second_try_at_the_end_of_the_stage() {
    let llm = FailThenGood::new(1, Fail::Transport, GOOD_ALT, "");
    let files = vec![("a.rs".to_string(), "fn f() {}".to_string())];
    let (recs, _unfinished) = recommend_alternatives(
        &llm,
        "me/api",
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        &[two_option_alternatives()],
        None,
        None,
        None,
    )
    .await
    .expect("the second try answers");
    assert_eq!(recs.len(), 1);
    assert_eq!(llm.calls(), 2);
}

#[tokio::test]
async fn an_alternatives_call_that_fails_both_tries_returns_an_error_carrying_both_attempts() {
    let llm = FailThenGood::new(usize::MAX, Fail::Transport, GOOD_ALT, "");
    let files = vec![("a.rs".to_string(), "fn f() {}".to_string())];
    let err = recommend_alternatives(
        &llm,
        "me/api",
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        &[two_option_alternatives()],
        None,
        None,
        None,
    )
    .await
    .expect_err("both tries failed");
    let attempts = camerata_llm::failure::attempts_of(&err);
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert!(!attempts[0].second_try && attempts[1].second_try);
    assert_eq!(classify_pass_error(&err), FailureClass::Transport);
    assert_eq!(llm.calls(), 2);
}

/// The second try happens inside the alternatives stage, so it is over before the scan reads the
/// stage's decision: every alternatives call precedes the first scan call.
#[tokio::test]
async fn the_alternatives_second_try_happens_before_the_scan_reads_the_result() {
    struct Ordered {
        log: Mutex<Vec<&'static str>>,
        alt_calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl LlmPort for Ordered {
        async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
            let system = req.system.clone().unwrap_or_default();
            let is_alt = system.starts_with(&alternatives_system_prompt()[..40]);
            self.log
                .lock()
                .unwrap()
                .push(if is_alt { "alt" } else { "other" });
            if is_alt {
                if self.alt_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(anyhow::Error::new(LlmCallFailure::new(
                        FailureClass::Transport,
                        "Anthropic API HTTP 529: overloaded",
                    )));
                }
                return Ok(response(GOOD_ALT));
            }
            Ok(response(r#"{"findings":[]}"#))
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
    let llm = Ordered {
        log: Mutex::new(Vec::new()),
        alt_calls: AtomicUsize::new(0),
    };
    let files = vec![("a.rs".to_string(), "fn f() {}\n".to_string())];
    let selected = vec![("MULTI-RULE-1".to_string(), "Do it.".to_string())];
    let (_f, _p, recs, failed, _s) = audit_repo(
        &llm,
        "me/api",
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &[two_option_alternatives()],
        &std::collections::HashMap::new(),
        None,
        None,
        ScanMode::Parallel,
        false,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("audit runs");
    let log = llm.log.lock().unwrap().clone();
    let last_alt = log
        .iter()
        .rposition(|r| *r == "alt")
        .expect("alternatives ran");
    let first_other = log
        .iter()
        .position(|r| *r == "other")
        .expect("the scan ran");
    assert!(
        last_alt < first_other,
        "alternatives (second try included) precede the scan: {log:?}"
    );
    assert_eq!(log.iter().filter(|r| **r == "alt").count(), 2);
    assert_eq!(recs.len(), 1, "the second try's decision is the one used");
    assert!(
        !failed
            .iter()
            .any(|f| f.pass == "rule-alternative recommendations"),
        "{failed:?}"
    );
}
