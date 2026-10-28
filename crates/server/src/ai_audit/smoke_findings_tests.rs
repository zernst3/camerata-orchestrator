//! Three defects found by an offline analysis of a smoke run with every efficiency switch on:
//! (1) a cut-off reply of the alternatives stage silently decided which rules apply, (2) the scan
//! stage's output cap did not scale with the rules in a batch, (3) a cross-family merge promoted an
//! example file's placeholder over the real secret. No network: every model is a scripted stub.

use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";

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

/// A model that answers by what the request holds: the alternatives stage (its prompt lists "Rules
/// to decide") gets the next scripted reply, everything else gets an empty, complete scan reply.
/// Every request is recorded.
struct StageLlm {
    alternatives: Mutex<Vec<(String, bool)>>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl StageLlm {
    fn new(alternatives: &[(&str, bool)]) -> Self {
        Self {
            alternatives: Mutex::new(
                alternatives
                    .iter()
                    .map(|(t, c)| (t.to_string(), *c))
                    .collect(),
            ),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn alternatives_prompts(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.prompt.contains("Rules to decide"))
            .map(|r| r.prompt.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl LlmPort for StageLlm {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let is_alternatives = req.prompt.contains("Rules to decide");
        self.seen.lock().unwrap().push(req);
        if !is_alternatives {
            return Ok(response(r#"{"findings":[]}"#, false));
        }
        let mut replies = self.alternatives.lock().unwrap();
        let (text, truncated) = if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies[0].clone()
        };
        Ok(response(&text, truncated))
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

// ── 1. A cut-off alternatives reply ─────────────────────────────────────────────────────────

fn alt(rule: &str) -> RuleAlternatives {
    let opt = |id: &str| crate::onboard::RuleOptionView {
        id: id.to_string(),
        label: id.to_string(),
        directive: format!("Do it the {id} way."),
        why: "why".to_string(),
        violates: false,
    };
    RuleAlternatives {
        rule_id: rule.to_string(),
        options: vec![opt("opt-a"), opt("opt-b")],
        selected_option_id: Some("opt-a".to_string()),
        kind: Some(camerata_rules::RuleKind::Decision),
    }
}

fn four_rules() -> Vec<RuleAlternatives> {
    ["RULE-A", "RULE-B", "RULE-C", "RULE-D"]
        .iter()
        .map(|r| alt(r))
        .collect()
}

/// One complete entry that picks `opt-b` with evidence.
fn entry(rule: &str) -> String {
    format!(
        r#"{{"rule_id":"{rule}","recommended_option_id":"opt-b","recommendation_reasoning":"the repo does it that way","applicable":true,"evidence":"src/a.rs:1","observed_options":[{{"option_id":"opt-b","sites":2}}]}}"#
    )
}

fn reply(rules: &[&str]) -> String {
    let body: Vec<String> = rules.iter().map(|r| entry(r)).collect();
    format!(r#"{{"recommendations":[{}]}}"#, body.join(","))
}

/// A reply that holds the entries for `done` complete and then stops in the middle of the next.
fn cut_off(done: &[&str]) -> String {
    let body: Vec<String> = done.iter().map(|r| entry(r)).collect();
    format!(
        r#"{{"recommendations":[{}{}{{"rule_id":"RULE-X","recommended_opt"#,
        body.join(","),
        if done.is_empty() { "" } else { "," }
    )
}

fn files() -> Vec<(String, String)> {
    vec![("src/a.rs".to_string(), "fn a() {}\n".to_string())]
}

async fn recommend(
    llm: &StageLlm,
    rules: &[RuleAlternatives],
) -> (Vec<RuleRecommendation>, Option<String>) {
    let files = files();
    recommend_alternatives(
        llm,
        REPO,
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        rules,
        None,
        None,
        None,
    )
    .await
    .expect("the recommendation pass runs")
}

fn ids(recs: &[RuleRecommendation]) -> Vec<&str> {
    recs.iter().map(|r| r.rule_id.as_str()).collect()
}

#[tokio::test]
async fn a_cut_off_alternatives_reply_keeps_its_complete_entries_and_retries_only_the_rest() {
    let llm = StageLlm::new(&[
        (&cut_off(&["RULE-A", "RULE-B"]), true),
        (&reply(&["RULE-C", "RULE-D"]), false),
    ]);
    let (recs, unfinished) = recommend(&llm, &four_rules()).await;
    assert_eq!(unfinished, None, "every rule was decided in the end");
    assert_eq!(ids(&recs), ["RULE-A", "RULE-B", "RULE-C", "RULE-D"]);
    assert!(
        recs.iter()
            .all(|r| !r.hallucinated && r.recommended_option_id == "opt-b" && r.applicable),
        "{recs:?}"
    );
    let prompts = llm.alternatives_prompts();
    assert_eq!(prompts.len(), 2, "one first call, one smaller retry");
    assert!(prompts[1].contains("[RULE-C]") && prompts[1].contains("[RULE-D]"));
    assert!(
        !prompts[1].contains("[RULE-A]") && !prompts[1].contains("[RULE-B]"),
        "the retry asks only for the rules the cut-off reply left out"
    );
}

#[tokio::test]
async fn a_cut_off_reply_with_nothing_complete_is_retried_as_two_halves() {
    let llm = StageLlm::new(&[
        (&cut_off(&[]), true),
        (&reply(&["RULE-A", "RULE-B"]), false),
        (&reply(&["RULE-C", "RULE-D"]), false),
    ]);
    let (recs, unfinished) = recommend(&llm, &four_rules()).await;
    assert_eq!(unfinished, None);
    assert_eq!(ids(&recs), ["RULE-A", "RULE-B", "RULE-C", "RULE-D"]);
    let prompts = llm.alternatives_prompts();
    assert_eq!(prompts.len(), 3);
    for retry in &prompts[1..] {
        let listed = ["RULE-A", "RULE-B", "RULE-C", "RULE-D"]
            .iter()
            .filter(|r| retry.contains(&format!("[{r}]")))
            .count();
        assert_eq!(listed, 2, "each retry is half the original batch: {retry}");
    }
}

#[tokio::test]
async fn rules_still_cut_off_after_the_retry_keep_their_default_and_are_named() {
    let llm = StageLlm::new(&[
        (&cut_off(&["RULE-A"]), true),
        // The retry (B, C, D) is cut off again with only B complete.
        (&cut_off(&["RULE-B"]), true),
    ]);
    let (recs, unfinished) = recommend(&llm, &four_rules()).await;
    assert_eq!(
        ids(&recs),
        ["RULE-A", "RULE-B"],
        "no invented fallback answer for a rule the model never reached"
    );
    assert_eq!(llm.alternatives_prompts().len(), 2, "retried once, no more");
    let reason = unfinished.expect("the unfinished rules are reported");
    assert!(
        reason.contains("RULE-C") && reason.contains("RULE-D"),
        "{reason}"
    );
    assert!(
        !reason.contains("RULE-A") && !reason.contains("RULE-B"),
        "{reason}"
    );
    assert!(reason.contains("cut off at the output limit"), "{reason}");
    assert!(reason.contains("default directive"), "{reason}");
    assert!(
        !reason.to_ascii_lowercase().contains("fail"),
        "no new client-facing failure wording: {reason}"
    );
}

#[tokio::test]
async fn a_cut_off_reply_is_not_retried_when_the_switch_is_off() {
    let llm = StageLlm::new(&[(&cut_off(&["RULE-A"]), true)]);
    let mut efficiency = crate::efficiency::EfficiencyConfig::default();
    efficiency
        .set("truncation_retry", false)
        .expect("a known measure");
    let meter = UsageMeter::with_controls(crate::cost_cap::RunControls {
        efficiency,
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        consensus: false,
    });
    let files = files();
    let (recs, unfinished) = recommend_alternatives(
        &llm,
        REPO,
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        &four_rules(),
        None,
        None,
        Some(&meter),
    )
    .await
    .expect("the pass runs");
    assert_eq!(llm.alternatives_prompts().len(), 1);
    assert_eq!(ids(&recs), ["RULE-A"]);
    let reason = unfinished.expect("reported");
    assert!(
        reason.contains("RULE-B") && reason.contains("RULE-D"),
        "{reason}"
    );
}

#[tokio::test]
async fn audit_repo_records_a_pass_for_rules_the_alternatives_reply_never_reached() {
    let llm = StageLlm::new(&[
        (&cut_off(&["RULE-A"]), true),
        (&cut_off(&[]), true),
        (&cut_off(&[]), true),
    ]);
    let files = vec![("src/a.rs".to_string(), "fn a() {}\n".to_string())];
    let selected = vec![("RULE-0".to_string(), "Rule zero.".to_string())];
    let (_f, _p, recs, failed_passes, _s) = audit_repo(
        &llm,
        REPO,
        &files,
        &crate::scan_routing::rules_from_pairs(&selected),
        &four_rules(),
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
    .expect("the scan runs");
    assert_eq!(ids(&recs), ["RULE-A"], "the decided rule is kept");
    let rows: Vec<_> = failed_passes
        .iter()
        .filter(|f| f.pass == "rule-alternative recommendations")
        .collect();
    assert_eq!(rows.len(), 1, "{failed_passes:?}");
    assert!(
        rows[0].reason.contains("RULE-B")
            && rows[0].reason.contains("RULE-C")
            && rows[0].reason.contains("RULE-D"),
        "{}",
        rows[0].reason
    );
    assert_eq!(rows[0].repo, REPO);
}

// ── 2. The scan's output cap scales with the rules in the batch ─────────────────────────────

/// The `max_tokens` of every scan request a run of one chunk and one batch of `n_rules` rules sent.
async fn scan_caps(n_rules: usize) -> Vec<u32> {
    let llm = StageLlm::new(&[]);
    let files: Vec<(String, String)> = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules: Vec<(String, String)> = (0..n_rules)
        .map(|i| (format!("RULE-{i}"), format!("rule {i}")))
        .collect();
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let adopted: std::collections::HashSet<String> =
        rules.iter().map(|(id, _)| id.clone()).collect();
    run_passes(
        &llm,
        REPO,
        "REPO MAP:\na.rs: fn a()\n",
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
        "smoke",
        None,
        false,
    )
    .await;
    let caps: Vec<u32> = llm
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.max_tokens)
        .collect();
    caps
}

#[test]
fn the_scan_cap_is_the_per_rule_budget_with_the_old_flat_cap_as_its_floor() {
    use camerata_ui_core::scan::{SCAN_MAX_TOKENS_FLOOR, SCAN_OUTPUT_TOKENS_PER_RULE};
    assert_eq!(scan_max_tokens(1), 8_192, "a small batch keeps today's cap");
    assert_eq!(scan_max_tokens(12), SCAN_MAX_TOKENS_FLOOR);
    assert_eq!(
        scan_max_tokens(15),
        15 * SCAN_OUTPUT_TOKENS_PER_RULE,
        "a full 15-rule batch gets its measured budget"
    );
    assert!(
        scan_max_tokens(15) >= 9_516,
        "the measured useful output of a full batch fits"
    );
    assert_eq!(
        scan_max_tokens(usize::MAX),
        AGGREGATE_MAX_TOKENS_CEILING,
        "an every-rule-in-one-prompt batch is bounded by the shared ceiling"
    );
    let mut last = 0;
    for n in 0..200 {
        let cap = scan_max_tokens(n);
        assert!(cap >= last, "never smaller for more rules");
        last = cap;
    }
}

#[tokio::test]
async fn a_full_rule_batch_is_sent_with_its_scaled_cap_and_a_small_one_with_the_floor() {
    assert_eq!(scan_caps(15).await, vec![scan_max_tokens(15)]);
    assert_eq!(scan_caps(2).await, vec![8_192]);
}
