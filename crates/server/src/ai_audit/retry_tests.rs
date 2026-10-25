//! Rank 6, wasted retries.
//!
//! * Scan pass: retry ONLY when the output was cut off at `max_tokens` (a higher cap recovers it).
//!   A malformed-but-complete reply is not blindly retried: repeating the same request at the
//!   same cap tends to return the same thing, and the spend is paid again.
//! * Fix generation: a finding that no grounded fix can be written for (no line, no surrounding
//!   code) goes straight to the rule's authored remediation, with no model call.
//! * Tokens spent on an attempt whose output was thrown away are counted per stage.

use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::stage_telemetry::Stage;

/// Answers each call from a script of `(text, truncated)` and records the request's
/// `max_tokens`. When the script runs out it repeats the last entry.
struct ScriptedLlm {
    script: Vec<(String, bool)>,
    seen_max_tokens: Mutex<Vec<u32>>,
    fail_after: Option<usize>,
}

impl ScriptedLlm {
    fn new(script: &[(&str, bool)]) -> Self {
        Self {
            script: script.iter().map(|(t, b)| (t.to_string(), *b)).collect(),
            seen_max_tokens: Mutex::new(Vec::new()),
            fail_after: None,
        }
    }
    fn calls(&self) -> usize {
        self.seen_max_tokens.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl LlmPort for ScriptedLlm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let n = {
            let mut seen = self.seen_max_tokens.lock().unwrap();
            seen.push(req.max_tokens);
            seen.len() - 1
        };
        if self.fail_after.is_some_and(|k| n >= k) {
            anyhow::bail!("transport down");
        }
        let (text, truncated) = self.script[n.min(self.script.len() - 1)].clone();
        Ok(LlmResponse {
            text,
            model: "stub".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.25),
            input_tokens: Some(1_000),
            output_tokens: Some(if truncated { 8_192 } else { 300 }),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

const CUT_OFF: &str = r#"{"findings":[{"path":"a.rs","line":3,"severity":"high","rule":"ARCH-X-1","title":"t","code":"c","detail":"d"},{"path":"b.rs","li"#;
const COMPLETE: &str = r#"{"findings":[{"path":"a.rs","line":3,"severity":"high","rule":"ARCH-X-1","title":"t","code":"c","detail":"d"},{"path":"b.rs","line":9,"severity":"low","rule":"ARCH-X-1","title":"t2","code":"c2","detail":"d2"}],"proposed_rules":[]}"#;

async fn pass(
    llm: &ScriptedLlm,
    meter: &UsageMeter,
) -> anyhow::Result<(Vec<Finding>, Vec<ProposedRule>, Vec<String>)> {
    audit_pass(
        llm,
        None,
        "prompt".to_string(),
        Vec::new(),
        "o/r",
        &std::collections::HashSet::new(),
        None,
        "s",
        Some(meter),
    )
    .await
}

// ── fix generation: no model call where no grounded fix can be written ─────────────────────

fn finding(rule: &str, path: &str, line: usize) -> Finding {
    let mut f = Finding {
        repo: "o/r".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "high".to_string(),
        snippet: "CREATE TABLE profiles (...)".to_string(),
        detail: "no RLS evidence for `profiles`".to_string(),
        ..Finding::default()
    };
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    f
}

async fn corpus() -> camerata_rules::RuleSet {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    corpus
}

const RLS: &str = "SUPABASE-RLS-ENABLED-1";

#[tokio::test]
async fn a_line_zero_finding_gets_the_authored_remediation_with_no_model_call() {
    let corpus = corpus().await;
    let llm = ScriptedLlm::new(&[(r#"{"fixes":[]}"#, false)]);
    let meter = UsageMeter::default();
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];

    let (out, failed) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![finding(RLS, "supabase/migrations/1.sql", 0)],
        &files,
        None,
        Some(&meter),
        Some(&corpus),
    )
    .await;

    assert_eq!(
        llm.calls(),
        0,
        "no model call for a row with no surrounding code"
    );
    assert_eq!(meter.snapshot().calls, 0);
    let fix = out[0]
        .fix_specific
        .as_deref()
        .expect("the authored remediation is the fix");
    assert!(
        fix.contains("profiles"),
        "instantiated against the finding: {fix}"
    );
    assert!(!fix.contains('<'), "no raw placeholder left: {fix}");
    assert!(
        !out[0].needs_review,
        "a pre-filter never hedges the finding"
    );
    assert!(
        failed.is_empty(),
        "using the authored remediation by design is not a failed pass: {failed:?}"
    );
}

#[tokio::test]
async fn a_finding_whose_file_is_not_in_the_scanned_set_is_also_routed_to_the_fallback() {
    // Nonzero line, but no code to ground a fix in (the path is not among the scanned files).
    let corpus = corpus().await;
    let llm = ScriptedLlm::new(&[(r#"{"fixes":[]}"#, false)]);
    let (out, _) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![finding(RLS, "supabase/migrations/missing.sql", 7)],
        &[],
        None,
        None,
        Some(&corpus),
    )
    .await;
    assert_eq!(llm.calls(), 0);
    assert!(out[0].fix_specific.is_some());
}

#[tokio::test]
async fn a_finding_with_surrounding_code_still_gets_a_model_generated_fix() {
    let corpus = corpus().await;
    let llm = ScriptedLlm::new(&[(
        r#"{"fixes":[{"index":0,"fix":"Enable row level security on the `profiles` table in supabase/migrations/1.sql."}]}"#,
        false,
    )]);
    let files = vec![(
        "supabase/migrations/1.sql".to_string(),
        "CREATE TABLE profiles (id int);\n".to_string(),
    )];
    let (out, _) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![finding(RLS, "supabase/migrations/1.sql", 1)],
        &files,
        None,
        None,
        Some(&corpus),
    )
    .await;
    assert_eq!(llm.calls(), 1, "code context exists, so the model is asked");
    assert!(out[0]
        .fix_specific
        .as_deref()
        .unwrap()
        .contains("Enable row level security"));
}

#[tokio::test]
async fn without_an_authored_remediation_the_model_is_still_asked() {
    // Nothing to fall back on: skipping the model would leave the row with no fix at all (and
    // route it to held), which is worse than the model's attempt. So the pre-filter stands down.
    let llm = ScriptedLlm::new(&[(r#"{"fixes":[]}"#, false)]);
    let (_, _) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![finding("ARCH-NO-SUCH-RULE-1", "a.rs", 0)],
        &[],
        None,
        None,
        None,
    )
    .await;
    assert!(
        llm.calls() > 0,
        "the model is asked when there is no fallback"
    );
}

#[tokio::test]
async fn rejected_fix_attempts_are_counted_as_wasted_tokens() {
    // Every reply names an identifier that is not in the evidence, so every attempt is thrown
    // away: MAX_FIX_REGENERATIONS + 1 batched rounds plus the solo retry.
    let llm = ScriptedLlm::new(&[(
        r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#,
        false,
    )]);
    let meter = UsageMeter::default();
    let files = vec![("a.rs".to_string(), "let x = 1;\n".to_string())];
    let (out, failed) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![finding("ARCH-1", "a.rs", 1)],
        &files,
        None,
        Some(&meter),
        None,
    )
    .await;
    assert_eq!(out[0].fix_specific, None);
    assert_eq!(failed.len(), 1);
    let fix = meter.stage_telemetry(1_000).stages[0].usage.clone();
    let attempts = (MAX_FIX_REGENERATIONS + 2) as u64;
    assert_eq!(fix.calls, attempts);
    assert_eq!(fix.wasted_calls, attempts, "every attempt was thrown away");
    assert_eq!(fix.wasted_input_tokens, fix.input_tokens);
}

#[tokio::test]
async fn an_attempt_that_is_partly_accepted_is_partly_wasted() {
    // Two findings in one call; the model grounds one fix and invents an identifier in the other.
    let llm = ScriptedLlm::new(&[(
        r#"{"fixes":[{"index":0,"fix":"Change the value in a.rs."},{"index":1,"fix":"Use `neverInEvidence` here."}]}"#,
        false,
    )]);
    let meter = UsageMeter::default();
    let files = vec![
        ("a.rs".to_string(), "let x = 1;\n".to_string()),
        ("b.rs".to_string(), "let y = 2;\n".to_string()),
    ];
    let (_out, _) = generate_fix_specifics(
        &llm,
        "o/r",
        vec![finding("ARCH-1", "a.rs", 1), finding("ARCH-2", "b.rs", 1)],
        &files,
        None,
        Some(&meter),
        None,
    )
    .await;
    let first_round_wasted = meter.stage_telemetry(1_000).stages[0].usage.clone();
    // The first round wasted half of its call (1 of 2 rejected), and later rounds re-asked only
    // the rejected finding, so wasted tokens are strictly less than total input.
    assert!(first_round_wasted.wasted_input_tokens > 0);
    assert!(first_round_wasted.wasted_input_tokens < first_round_wasted.input_tokens);
}

// ── scan pass: truncation-only retry ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_truncated_pass_retries_once_with_a_higher_cap_and_keeps_the_complete_output() {
    let llm = ScriptedLlm::new(&[(CUT_OFF, true), (COMPLETE, false)]);
    let meter = UsageMeter::default();
    let (findings, _, _) = pass(&llm, &meter).await.expect("the pass completes");

    assert_eq!(llm.calls(), 2, "exactly one retry");
    let caps = llm.seen_max_tokens.lock().unwrap().clone();
    assert!(caps[1] > caps[0], "the retry raises the cap: {caps:?}");
    assert!(caps[1] <= AGGREGATE_MAX_TOKENS_CEILING);
    assert_eq!(
        findings.len(),
        2,
        "the retry's complete output is the one used"
    );

    // Both calls are in the stage's spend; the cut-off one is counted as wasted.
    let scan = meter.stage_telemetry(1_000).stages[0].usage.clone();
    assert_eq!(
        meter.stage_telemetry(1_000).stages[0].stage,
        Stage::ScanPass
    );
    assert_eq!(scan.calls, 2);
    assert_eq!(scan.wasted_calls, 1);
    assert_eq!(scan.wasted_output_tokens, 8_192);
}

#[tokio::test]
async fn a_pass_truncated_twice_is_retried_once_only() {
    let llm = ScriptedLlm::new(&[(CUT_OFF, true)]);
    let meter = UsageMeter::default();
    let _ = pass(&llm, &meter).await.expect("the pass completes");
    assert_eq!(llm.calls(), 2, "never a third attempt");
}

#[tokio::test]
async fn a_malformed_but_complete_pass_is_not_blindly_retried() {
    let llm = ScriptedLlm::new(&[("I could not produce JSON for this chunk.", false)]);
    let meter = UsageMeter::default();
    let (findings, _, _) = pass(&llm, &meter).await.expect("the pass completes");
    assert_eq!(llm.calls(), 1, "no truncation, no retry");
    assert!(findings.is_empty());
    // The spend bought nothing, and the table says so.
    let scan = meter.stage_telemetry(1_000).stages[0].usage.clone();
    assert_eq!(scan.calls, 1);
    assert_eq!(scan.wasted_calls, 1);
}

#[tokio::test]
async fn a_clean_pass_is_one_call_and_nothing_is_wasted() {
    let llm = ScriptedLlm::new(&[(COMPLETE, false)]);
    let meter = UsageMeter::default();
    let (findings, _, _) = pass(&llm, &meter).await.expect("the pass completes");
    assert_eq!(llm.calls(), 1);
    assert_eq!(findings.len(), 2);
    assert_eq!(meter.stage_telemetry(1_000).stages[0].usage.wasted_calls, 0);
}

#[tokio::test]
async fn a_failed_retry_never_makes_the_pass_worse_than_before() {
    // The first call is cut off, the retry hits a transport error: the pass still returns Ok with
    // what the first response parsed to (as it always did), not an Err that loses the chunk.
    let mut llm = ScriptedLlm::new(&[(CUT_OFF, true)]);
    llm.fail_after = Some(1);
    let meter = UsageMeter::default();
    let out = pass(&llm, &meter).await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(llm.calls(), 2);
}
