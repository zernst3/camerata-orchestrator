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

// ── 1b. The alternatives request's output cap scales with the rules in the batch ─────────────

fn many_rules(n: usize) -> Vec<RuleAlternatives> {
    (0..n).map(|i| alt(&format!("RULE-{i:02}"))).collect()
}

fn rule_names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("RULE-{i:02}")).collect()
}

/// The `max_tokens` of every alternatives request the model saw.
fn alternatives_caps(llm: &StageLlm) -> Vec<u32> {
    let caps: Vec<u32> = llm
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.prompt.contains("Rules to decide"))
        .map(|r| r.max_tokens)
        .collect();
    caps
}

#[test]
fn the_alternatives_cap_is_the_per_rule_budget_with_the_old_flat_cap_as_its_floor() {
    use camerata_ui_core::scan::ALTERNATIVES_OUTPUT_TOKENS_PER_RULE;
    assert_eq!(
        alternatives_max_tokens(1),
        4_096,
        "a small batch keeps today's cap"
    );
    assert_eq!(
        alternatives_max_tokens(25),
        25 * ALTERNATIVES_OUTPUT_TOKENS_PER_RULE,
        "a full batch of 25 rules gets its measured budget"
    );
    assert!(alternatives_max_tokens(25) > 4_096);
    assert_eq!(
        alternatives_max_tokens(usize::MAX),
        AGGREGATE_MAX_TOKENS_CEILING
    );
    let mut last = 0;
    for n in 0..100 {
        let cap = alternatives_max_tokens(n);
        assert!(cap >= last, "never smaller for more rules");
        last = cap;
    }
}

#[tokio::test]
async fn a_full_alternatives_batch_is_sent_with_its_scaled_cap_and_a_retry_with_its_own() {
    let rules = many_rules(25);
    let names = rule_names(25);
    let all: Vec<&str> = names.iter().map(String::as_str).collect();
    let llm = StageLlm::new(&[
        (&cut_off(&[]), true),
        (&reply(&all[..13]), false),
        (&reply(&all[13..]), false),
    ]);
    let (recs, unfinished) = recommend(&llm, &rules).await;
    assert_eq!((recs.len(), unfinished), (25, None));
    assert_eq!(
        alternatives_caps(&llm),
        vec![
            alternatives_max_tokens(25),
            alternatives_max_tokens(13),
            alternatives_max_tokens(12)
        ],
        "each send is capped for the rules it asks about"
    );
    assert!(alternatives_caps(&llm)[1] < alternatives_caps(&llm)[0]);
}

#[tokio::test]
async fn a_small_alternatives_batch_keeps_the_floor() {
    let llm = StageLlm::new(&[(&reply(&["RULE-A", "RULE-B"]), false)]);
    let rules: Vec<RuleAlternatives> = ["RULE-A", "RULE-B"].iter().map(|r| alt(r)).collect();
    recommend(&llm, &rules).await;
    assert_eq!(alternatives_caps(&llm), vec![4_096]);
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

// ── 3. An example file's placeholder never outranks the real secret ─────────────────────────

const PLACEHOLDER_LINE: &str = "SUPABASE_SERVICE_ROLE_KEY=sb_secret_your_service_role_key_here";
/// The redacted form a real value takes in a finding (no secret-shaped literal in the source).
const REAL_LINE: &str = "SUPABASE_SERVICE_ROLE_KEY=sb_secret_a1b2\u{2026}c3d4 (46 chars)";

fn secret_row(rule: &str, path: &str, snippet: &str) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: path.to_string(),
        line: 1,
        rule_id: rule.to_string(),
        severity: "critical".to_string(),
        snippet: snippet.to_string(),
        detail: "SUPABASE_SERVICE_ROLE_KEY is committed in this file.".to_string(),
        ..Finding::default()
    }
}

fn placeholder_row() -> Finding {
    secret_row("SEC-NO-VENDOR-TOKEN-1", ".env.example", PLACEHOLDER_LINE)
}

fn real_row() -> Finding {
    secret_row("SEC-NO-SECRET-FILE-1", ".env.production", REAL_LINE)
}

#[test]
fn the_real_secret_is_the_headline_row_whichever_order_the_rows_arrive_in() {
    for rows in [
        vec![placeholder_row(), real_row()],
        vec![real_row(), placeholder_row()],
    ] {
        // The group as the cross-family merge forms it; which row leads is what is under test.
        let out = [merge_semantic_group(rows)];
        assert_eq!(out[0].path, ".env.production", "the real secret leads");
        assert_eq!(out[0].severity, "critical");
        let absorbed = out[0]
            .also_locations
            .iter()
            .find(|l| l.path == ".env.example")
            .expect("the placeholder is listed as another place");
        assert_ne!(
            absorbed.severity, "critical",
            "an example file's placeholder is never rated critical, even absorbed"
        );
    }
}

#[test]
fn a_placeholder_only_row_is_never_critical() {
    let out = merge_semantic_groups(vec![placeholder_row()], &[]);
    assert_eq!(out.len(), 1);
    assert_ne!(out[0].severity, "critical", "{out:?}");
    assert!(
        crate::severity_bounds::recorded_adjustments(&out[0])
            .iter()
            .any(|a| a.contains("placeholder")),
        "the lowering is recorded in words"
    );
    // Two placeholders merged are still placeholders.
    let sample = secret_row("SEC-NO-VENDOR-TOKEN-1", ".env.sample", PLACEHOLDER_LINE);
    let out = merge_semantic_groups(vec![placeholder_row(), sample], &[]);
    assert!(out.iter().all(|f| f.severity != "critical"), "{out:?}");
}

#[test]
fn a_placeholder_value_outside_an_example_file_is_also_a_placeholder() {
    let row = secret_row("SEC-NO-VENDOR-TOKEN-1", "config/dev.env", PLACEHOLDER_LINE);
    let out = merge_semantic_groups(vec![row], &[]);
    assert_ne!(out[0].severity, "critical", "{out:?}");
}

#[test]
fn a_real_secret_alone_keeps_its_rating_and_a_non_secret_rule_in_an_example_file_is_untouched() {
    let out = merge_semantic_groups(vec![real_row()], &[]);
    assert_eq!(out[0].severity, "critical");
    let mut other = secret_row("ARCH-STRICT-LAYERING-1", "docs/layering.example", "x");
    other.detail = "A layering violation.".to_string();
    let out = merge_semantic_groups(vec![other], &[]);
    assert_eq!(out[0].severity, "critical", "only secret rules are lowered");
}

/// A real key pasted into an example file is a classic leak, not a placeholder: the file name
/// says "example" but the row cites a real value (redacted, or raw before redaction), so the
/// rating stands. Only a row with no real value in it is lowered on the strength of its path.
#[test]
fn a_real_key_committed_in_an_example_file_keeps_its_rating() {
    let redacted = secret_row("SEC-NO-VENDOR-TOKEN-1", ".env.example", REAL_LINE);
    let out = merge_semantic_groups(vec![redacted], &[]);
    assert_eq!(out[0].severity, "critical", "{out:?}");
    assert!(crate::severity_bounds::recorded_adjustments(&out[0]).is_empty());
    // A raw value as the floor sees it before redaction, built at run time (mixed-case body, no
    // placeholder word), in a `*.example` file.
    let body: String = (0..40).map(|i| char::from(b"aZ3kQ9mT"[i % 8])).collect();
    let raw = secret_row(
        "SEC-NO-VENDOR-TOKEN-1",
        "config/settings.yaml.example",
        &format!("SUPABASE_SERVICE_ROLE_KEY=sb_secret_{body}"),
    );
    let out = merge_semantic_groups(vec![raw], &[]);
    assert_eq!(out[0].severity, "critical", "{out:?}");
    // A placeholder in the same kind of file is still lowered, and ranks below the real key.
    let out = merge_semantic_groups(vec![placeholder_row()], &[]);
    assert_eq!(out[0].severity, "low");
}

/// The `SEC-` prefix covers injection, transport and sink rules too; none of those is about a
/// credential, so an example file name or a template token in the snippet says nothing about
/// them and must not lower their rating.
#[test]
fn a_security_rule_that_is_not_a_secret_rule_is_not_lowered_in_an_example_file() {
    for (rule, path, snippet) in [
        (
            "SEC-NO-RAW-SQL-CONCAT-1",
            "db/queries.sql.example",
            "SELECT * FROM users WHERE id = ' + id + '",
        ),
        (
            "SEC-NO-DISABLED-TLS-1",
            "docker-compose.yml.example",
            "NODE_TLS_REJECT_UNAUTHORIZED=0",
        ),
        (
            "SEC-NO-COMMAND-INJECTION-1",
            "scripts/deploy.sh.example",
            "eval \"$CMD\"",
        ),
    ] {
        let mut f = secret_row(rule, path, snippet);
        f.detail = "A security defect.".to_string();
        let out = merge_semantic_groups(vec![f], &[]);
        assert_eq!(out[0].severity, "critical", "{rule} in {path}: {out:?}");
    }
}

// ── 4. Two different credentials near each other stay two rows ──────────────────────────────

/// A redacted password (no prefix, no tail) and a redacted Stripe-shaped key: two different
/// credentials. No secret-shaped literal in the source; these are the redacted forms a finding
/// carries.
const PASSWORD_LINE: &str = "const dbPassword = \"\u{2026} (12 chars)\";";
const STRIPE_LINE: &str = "const stripeKey = \"sk_live_\u{2026}ab12 (32 chars)\";";

fn credential_row(rule: &str, line: usize, snippet: &str, detail: &str) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: "src/config.ts".to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "critical".to_string(),
        snippet: snippet.to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

#[test]
fn a_floor_password_and_an_ai_stripe_key_two_lines_apart_stay_two_rows() {
    let floor = credential_row(
        "SEC-NO-HARDCODED-SECRETS-1",
        5,
        PASSWORD_LINE,
        "A database password is written into the source.",
    );
    let ai = credential_row(
        "AI-HARDCODED-STRIPE-KEY",
        7,
        STRIPE_LINE,
        "A live payment key is written into the source.",
    );
    for rows in [vec![floor.clone(), ai.clone()], vec![ai, floor]] {
        let out = merge_semantic_groups(rows, &[]);
        assert_eq!(out.len(), 2, "two credentials, two decisions: {out:?}");
        assert!(out.iter().all(|f| f.also_locations.is_empty()), "{out:?}");
    }
}

#[test]
fn two_ai_rows_for_different_credentials_with_one_rule_id_stay_two_rows() {
    let one = credential_row(
        "AI-HARDCODED-SECRET-1",
        5,
        PASSWORD_LINE,
        "A database password is written into the source.",
    );
    let two = credential_row(
        "AI-HARDCODED-SECRET-1",
        7,
        STRIPE_LINE,
        "A live payment key is written into the source.",
    );
    let out = merge_semantic_groups(vec![one, two], &[]);
    assert_eq!(out.len(), 2, "{out:?}");
}

#[test]
fn the_same_credential_flagged_by_the_floor_and_the_ai_on_adjacent_lines_is_one_row() {
    let floor = credential_row(
        "SEC-NO-HARDCODED-SECRETS-1",
        5,
        STRIPE_LINE,
        "A payment key is written into the source.",
    );
    let ai = credential_row(
        "AI-HARDCODED-STRIPE-KEY",
        6,
        "stripeKey = \"sk_live_\u{2026}ab12 (32 chars)\"",
        "A live payment key is written into the source.",
    );
    let out = merge_semantic_groups(vec![floor, ai], &[]);
    assert_eq!(out.len(), 1, "one credential, one row: {out:?}");
    assert_eq!(out[0].also_locations.len(), 1);
}

#[test]
fn a_row_that_cites_no_credential_value_is_not_proven_to_be_the_same_credential() {
    let floor = credential_row(
        "SEC-NO-HARDCODED-SECRETS-1",
        5,
        STRIPE_LINE,
        "A payment key is written into the source.",
    );
    let ai = credential_row(
        "AI-HARDCODED-STRIPE-KEY",
        6,
        "",
        "A live payment key is written into the source.",
    );
    let out = merge_semantic_groups(vec![floor, ai], &[]);
    assert_eq!(out.len(), 2, "{out:?}");
}

#[test]
fn a_credential_seen_in_two_repositories_is_never_one_row() {
    let here = credential_row(
        "SEC-NO-HARDCODED-SECRETS-1",
        5,
        STRIPE_LINE,
        "A payment key is written into the source.",
    );
    let mut there = here.clone();
    there.repo = "acme/other".to_string();
    there.rule_id = "AI-HARDCODED-STRIPE-KEY".to_string();
    let out = merge_semantic_groups(vec![here, there], &[]);
    assert_eq!(out.len(), 2, "{out:?}");
}
