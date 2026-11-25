//! #240 story 2: where every stage of the default run puts its prompt-cache marks.
//!
//! One table. Each row runs the real entry of a stage against a model that records every request
//! and answers nothing (or, for the group review, the stand-in reply, so the rule-level summary
//! call is reached), then states what the requests must look like:
//!
//! * a stage with a prefix that is the same bytes on every call and large enough to cache carries
//!   exactly ONE mark, at the end of that prefix, and the prefix is identical across its calls;
//! * a stage whose repeated text is under the provider's minimum cacheable size carries none
//!   (a mark on it would only pay the write price for nothing);
//! * in every request the LAST block carries no mark, so the part that changes on every call is
//!   never written to the cache.
//!
//! It turns the table in the design into a gate: a later prompt change cannot quietly drop a mark
//! or start caching a unique tail. The agent finder's conversation is not a row: its marks sit on
//! the last block of a growing conversation on purpose (they move forward each turn), they are
//! put on the request body inside the finder's own transport, and no request builder is
//! reachable from here.

use std::sync::Mutex;

use super::*;
use crate::llm::{BatchMessage, CacheTtl, LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";

/// Records every request. Answers with `reply(system, prompt)` when that gives a text, else with
/// an error (the stage then ends as a failed part, which is fine: the request was sent).
struct Recording {
    seen: Mutex<Vec<LlmRequest>>,
    reply: fn(Option<&str>, &str) -> Option<String>,
}

impl Recording {
    fn silent() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            reply: |_, _| None,
        }
    }

    fn standing_in() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            reply: group_review::stand_in_reply,
        }
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmPort for Recording {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let text = (self.reply)(req.system.as_deref(), &req.prompt);
        self.seen.lock().unwrap().push(req);
        match text {
            Some(text) => Ok(LlmResponse {
                reported_model: None,
                cli_cost_usd: None,
                elapsed_ms: None,
                text,
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: Some(0.001),
                input_tokens: Some(100),
                output_tokens: Some(10),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
                truncated: false,
            }),
            None => anyhow::bail!("the layout test model answers nothing"),
        }
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── the stages, driven through their real entries ───────────────────────────────────────────────

fn findings(n: usize) -> Vec<Finding> {
    (0..n)
        .map(|i| Finding {
            repo: REPO.to_string(),
            path: format!("src/f{i}.rs"),
            line: 3,
            rule_id: "ARCH-1".to_string(),
            severity: "high".to_string(),
            snippet: format!("snippet {i}"),
            detail: "detail".to_string(),
            ..Finding::default()
        })
        .collect()
}

fn files() -> Vec<(String, String)> {
    (0..4)
        .map(|i| {
            (
                format!("src/f{i}.rs"),
                "fn a() {}\nfn b() {}\nfn c() {}\n".to_string(),
            )
        })
        .collect()
}

/// More sites of one rule than a chunk holds, so the review makes two chunk calls and the
/// rule-level summary call.
async fn group_review_requests() -> Vec<LlmRequest> {
    let llm = Recording::standing_in();
    let mut report = crate::onboard::ScanReport::gated(&[REPO.to_string()]);
    report.gated = false;
    report.code_lines = 40_000;
    for i in 0..(group_review::MAX_CHUNK_MEMBERS + 20) {
        let mut f = Finding {
            repo: REPO.to_string(),
            path: format!("src/f{i:04}.rs"),
            line: 10 + i,
            rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
            severity: "medium".to_string(),
            snippet: format!("call_{i}();"),
            detail: format!("Headline {i}: the handler skips the check."),
            effort: Some("low".to_string()),
            confidence: Some("high".to_string()),
            ..Finding::default()
        };
        f.captures.insert(
            STAGE1_CAPTURE.to_string(),
            serde_json::json!({"real": "yes", "reason": "r"}).to_string(),
        );
        report.findings.push(f);
    }
    report.assign_site_keys();
    let rules: Vec<(String, String)> = Vec::new();
    let cfg = group_review::GroupReviewConfig {
        model: "claude-fable-5-1",
        repo_shape: "This repository has 12 code files.",
        rules: &rules,
        rank: crate::review_tree::RankSettings::default(),
        max_in_flight: 4,
        cli_backend: false,
        batch: false,
        job: None,
        feedback: None,
    };
    let meter = UsageMeter::default();
    let _ = group_review::review_groups(&mut report, &llm, &cfg, Some(&meter)).await;
    llm.requests()
}

async fn duplicate_check_requests() -> Vec<LlmRequest> {
    let llm = Recording::silent();
    let mut report = crate::release_gate::test_scan_report();
    report.findings = vec![
        Finding {
            repo: REPO.to_string(),
            path: "src/a.rs".to_string(),
            line: 10,
            rule_id: "SEC-A-1".to_string(),
            severity: "high".to_string(),
            detail: "one".to_string(),
            ..Finding::default()
        },
        Finding {
            repo: REPO.to_string(),
            path: "src/a.rs".to_string(),
            line: 12,
            rule_id: "SEC-B-1".to_string(),
            severity: "high".to_string(),
            detail: "two".to_string(),
            ..Finding::default()
        },
    ];
    report.assign_site_keys();
    let candidates =
        crate::possible_duplicate::DupIndex::candidates(report.tree.as_ref(), &report.findings);
    let meter = UsageMeter::default();
    let _ = crate::possible_duplicate::check::run_check(
        &report,
        &llm,
        &meter,
        "claude-sonnet-5-5",
        &candidates,
        false,
        false,
        None,
        None,
    )
    .await;
    llm.requests()
}

async fn fix_requests() -> Vec<LlmRequest> {
    let llm = Recording::silent();
    let meter = UsageMeter::default();
    let _ =
        generate_fix_specifics(&llm, REPO, findings(6), &files(), None, Some(&meter), None).await;
    llm.requests()
}

/// Two copies of one decision written in four files, so the clone rule has candidates to judge.
fn clone_files() -> Vec<(String, String)> {
    let discount = |func: &str, rate: &str| {
        format!(
            "import {{ money }} from './money';
export function {func}(order, user) {{
  const base = order.subtotal * order.quantity;
  if (user.isPremium) {{
    const discount = base * {rate};
    return base - discount;
  }}
  if (base > 100 && order.coupon !== null) {{
    return base - 10;
  }}
  return base + order.shipping;
}}
"
        )
    };
    [
        ("src/cart/total.ts", "applyDiscount", "0.2"),
        ("src/checkout/summary.ts", "quoteTotal", "0.15"),
        ("src/orders/price.ts", "priceOrder", "0.2"),
        ("src/invoices/amount.ts", "invoiceAmount", "0.1"),
    ]
    .iter()
    .map(|(p, f, r)| ((*p).to_string(), discount(f, r)))
    .collect()
}

async fn clone_judge_requests() -> Vec<LlmRequest> {
    use crate::onboard::clone_advisory::{judge_clone_candidates, JudgeRequest, VerdictCache};
    use camerata_checks::clone_rules::{candidates, CloneRule};
    let llm = Recording::silent();
    let files = clone_files();
    let armed = std::collections::HashSet::from([CloneRule::SingleSourceLogic.id()]);
    let set = candidates(&files, &armed);
    let meter = UsageMeter::default();
    let _ = judge_clone_candidates(
        Some(&llm),
        JudgeRequest {
            repo: REPO,
            files: &files,
            set,
            judge_model: "claude-haiku-5-5",
            corpus: None,
            unavailable_reason: "deterministic-only scan",
        },
        &mut VerdictCache::default(),
        Some(&meter),
        None,
    )
    .await;
    llm.requests()
}

// ── what a request's content blocks look like on the wire ───────────────────────────────────────

/// The user message's content blocks exactly as the Messages API receives them (the batch item
/// is built by the same function the real-time path shares).
fn blocks_of(req: &LlmRequest) -> Vec<serde_json::Value> {
    let item = crate::llm::build_batch_item("s0", req, &req.model, CacheTtl::OneHour);
    let message: &BatchMessage = &item.params.messages[0];
    match serde_json::to_value(&message.content).unwrap() {
        serde_json::Value::String(text) => vec![serde_json::json!({"type": "text", "text": text})],
        serde_json::Value::Array(blocks) => blocks,
        other => panic!("unexpected content shape: {other}"),
    }
}

fn is_marked(block: &serde_json::Value) -> bool {
    block.get("cache_control").is_some()
}

/// What a stage's requests must carry.
enum Layout {
    /// One mark, at the end of a prefix that is the same on every request of the stage.
    OnePrefixMark { starts_with: &'static str },
    /// No mark: the repeated text is under the provider's minimum cacheable size.
    NoMark,
}

struct Row {
    stage: &'static str,
    requests: Vec<LlmRequest>,
    layout: Layout,
    /// At least this many requests must have been built (the fixture really reached the model).
    at_least: usize,
}

#[tokio::test]
async fn the_cache_layout_of_every_stage_of_the_default_run_is_pinned() {
    let rows = vec![
        // The group review chunks and the rule-level summary: the system text plus the repository
        // line, about 2.5K tokens, the same bytes on every call. One mark at `shared_prefix`.
        Row {
            stage: "group-review",
            requests: group_review_requests().await,
            layout: Layout::OnePrefixMark {
                starts_with: "Repository: acme/widgets\n",
            },
            at_least: 3,
        },
        // The clone judge: the rule text leads every batch of a rule. One mark at its end.
        Row {
            stage: "clone-judge",
            requests: clone_judge_requests().await,
            layout: Layout::OnePrefixMark { starts_with: "" },
            at_least: 1,
        },
        // The duplicate check: its system text is about 300 tokens, under the provider's minimum
        // cacheable size, so a mark would pay the write price and never be read. None.
        Row {
            stage: "duplicate-check",
            requests: duplicate_check_requests().await,
            layout: Layout::NoMark,
            at_least: 1,
        },
        // The fix stage: its system text is about 900 tokens, also under the minimum, on a small
        // model, and 1 percent of a run. None.
        Row {
            stage: "fix",
            requests: fix_requests().await,
            layout: Layout::NoMark,
            at_least: 1,
        },
    ];

    for row in rows {
        assert!(
            row.requests.len() >= row.at_least,
            "{}: the fixture built {} request(s), expected at least {}",
            row.stage,
            row.requests.len(),
            row.at_least
        );
        let mut prefixes: Vec<String> = Vec::new();
        for req in &row.requests {
            let blocks = blocks_of(req);
            let marked = blocks.iter().filter(|b| is_marked(b)).count();
            // The last block, the part that changes on every call, is never marked.
            assert!(
                !is_marked(blocks.last().expect("a request has content")),
                "{}: the last block of a request carries a cache mark",
                row.stage
            );
            // The wire matches what the request asked for: one block more than marks, every
            // block but the last marked.
            assert_eq!(
                marked,
                req.cache_breakpoints.len(),
                "{}: marks on the wire differ from the request's breakpoints",
                row.stage
            );
            assert_eq!(
                blocks.len(),
                req.cache_breakpoints.len() + 1,
                "{}",
                row.stage
            );
            assert!(
                blocks[..blocks.len() - 1].iter().all(is_marked),
                "{}: a block before the last is unmarked",
                row.stage
            );
            match &row.layout {
                Layout::NoMark => assert!(
                    req.cache_breakpoints.is_empty(),
                    "{}: carries a cache mark on a prefix under the minimum cacheable size",
                    row.stage
                ),
                Layout::OnePrefixMark { starts_with } => {
                    assert_eq!(
                        req.cache_breakpoints.len(),
                        1,
                        "{}: expected one mark at the end of the shared prefix",
                        row.stage
                    );
                    let prefix = blocks[0]["text"].as_str().unwrap().to_string();
                    assert!(
                        prefix.starts_with(starts_with),
                        "{}: the marked prefix does not start with {starts_with:?}",
                        row.stage
                    );
                    assert!(!prefix.is_empty(), "{}: an empty marked prefix", row.stage);
                    prefixes.push(prefix);
                }
            }
        }
        if matches!(row.layout, Layout::OnePrefixMark { .. }) && row.stage == "group-review" {
            // The same bytes on every call of the stage: that is what makes it cacheable.
            prefixes.dedup();
            assert_eq!(
                prefixes.len(),
                1,
                "{}: the marked prefix differs between calls",
                row.stage
            );
        }
    }
}
