//! R5: the spend limit is checked before every call of every stage that sends one.
//!
//! One table of the eight stages the run meter names ([`Stage::ALL`]). Each test runs the real
//! stage entry on a meter whose spend ALREADY exceeds the limit (the way a resumed run starts:
//! the stored replies were paid for) and counts the calls its model receives. A stage that has
//! started may finish its current call; nothing new starts once the limit is reached, so a run
//! that begins past the limit sends nothing in any stage.
//!
//! No network: every model is a counting stub that fails the test's expectation by being asked.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::cost_cap::RunControls;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

const REPO: &str = "acme/widgets";
const LIMIT: f64 = 5.0;
const STORED: f64 = 12.0;

/// Counts every request it receives and answers each with an error, so a stage that was
/// (wrongly) allowed to send one ends as a failed part and the count says so.
struct Counting {
    calls: AtomicUsize,
}

impl Counting {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for Counting {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("the spend limit was already passed: this call must not be sent")
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

/// A meter for a run limited to [`LIMIT`] dollars that has already spent [`STORED`]: the shape of
/// a resume whose stored replies cost more than the limit allows.
fn past_the_limit() -> UsageMeter {
    let controls = RunControls {
        max_cost_usd: Some(LIMIT),
        ..RunControls::default()
    };
    let prior = ActualUsage {
        cost_usd: STORED,
        calls: 40,
        cost_complete: true,
        ..ActualUsage::default()
    };
    let meter = UsageMeter::continuing(controls, &prior);
    assert!(meter.cap_reached(), "the fixture is past its limit");
    meter
}

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

#[test]
fn the_table_names_every_stage_the_meter_counts() {
    // A new stage must add its row here: the test below walks `Stage::ALL`, and this pins the
    // list the other tests in this file cover.
    assert_eq!(
        Stage::ALL.map(Stage::label),
        [
            "scan",
            "alternatives",
            "detail",
            "calibration",
            "group-review",
            "fix",
            "deep",
            "clone-judge"
        ]
    );
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

/// Run the real entry of `stage` once on `llm` and `meter`.
#[allow(clippy::too_many_lines)]
async fn run_stage_once(stage: Stage, llm: &Counting, meter: &UsageMeter) {
    match stage {
        Stage::ScanPass => {
            let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
            let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
            let rule_batches: Vec<Vec<(String, String)>> = (0..3)
                .map(|n| vec![(format!("RULE-{n}"), format!("rule {n}"))])
                .collect();
            let batches: Vec<&[(String, String)]> =
                rule_batches.iter().map(Vec::as_slice).collect();
            let _ = run_passes(
                llm,
                REPO,
                &RepoMap::whole_text("REPO MAP:\nfoo.rs: fn foo()\n"),
                &crate::prompt_frame::FrameNonce::generate(),
                &std::collections::HashSet::new(),
                None,
                None,
                None,
                &chunks,
                None,
                &batches,
                4,
                "pass[all]",
                "r5",
                Some(meter),
                false,
            )
            .await;
        }
        Stage::Alternatives => {
            let opt = |id: &str| crate::onboard::RuleOptionView {
                id: id.to_string(),
                label: id.to_string(),
                directive: format!("Do it the {id} way."),
                why: "why".to_string(),
                violates: false,
            };
            let rules = vec![RuleAlternatives {
                rule_id: "RULE-A".to_string(),
                options: vec![opt("opt-a"), opt("opt-b")],
                selected_option_id: Some("opt-a".to_string()),
                kind: Some(camerata_rules::RuleKind::Decision),
            }];
            let files = vec![("src/a.rs".to_string(), "fn a() {}\n".to_string())];
            let _ = recommend_alternatives(
                llm,
                REPO,
                &files,
                &files,
                &crate::prompt_frame::FrameNonce::generate(),
                &rules,
                None,
                None,
                Some(meter),
            )
            .await;
        }
        Stage::DetailRegeneration => {
            let (out, _failed) = detail_regen::regenerate_full_details(
                llm,
                REPO,
                findings(3),
                &files(),
                &[],
                &crate::prompt_frame::FrameNonce::generate(),
                None,
                Some(meter),
            )
            .await;
            assert_eq!(out.len(), 3, "no finding is dropped");
        }
        Stage::Calibration => {
            let (out, _failed) = verify_findings(
                llm,
                REPO,
                findings(100),
                None,
                None,
                Some(meter),
                &Stage1Inputs::new("A repo."),
            )
            .await;
            assert_eq!(out.len(), 100, "no finding is dropped");
        }
        Stage::GroupReview => {
            let mut report = crate::onboard::ScanReport::gated(&[REPO.to_string()]);
            report.gated = false;
            report.code_lines = 40_000;
            for i in 0..6 {
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
                job: None,
            };
            let _ = group_review::review_groups(&mut report, llm, &cfg, Some(meter)).await;
        }
        Stage::FixGeneration => {
            let (out, _failed) =
                generate_fix_specifics(llm, REPO, findings(60), &files(), None, Some(meter), None)
                    .await;
            assert_eq!(out.len(), 60, "no finding is dropped");
        }
        Stage::DeepTier => {
            let _ = run_deep_tier(
                llm,
                REPO,
                &files(),
                Some("claude-opus-5-5"),
                ScanMode::Parallel,
                None,
                Some(meter),
                true,
            )
            .await;
        }
        Stage::CloneJudge => {
            use crate::onboard::clone_advisory::{
                judge_clone_candidates, JudgeRequest, VerdictCache,
            };
            use camerata_checks::clone_rules::{candidates, CloneRule};
            let files = clone_files();
            let armed = std::collections::HashSet::from([CloneRule::SingleSourceLogic.id()]);
            let set = candidates(&files, &armed);
            let _ = judge_clone_candidates(
                Some(llm),
                JudgeRequest {
                    repo: REPO,
                    files: &files,
                    set,
                    judge_model: "claude-haiku-5-5",
                    corpus: None,
                    unavailable_reason: "deterministic-only scan",
                },
                &mut VerdictCache::default(),
                Some(meter),
                None,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn a_run_that_starts_past_the_limit_sends_no_call_in_any_stage() {
    let mut sent: Vec<String> = Vec::new();
    for stage in Stage::ALL {
        let llm = Counting::new();
        let meter = past_the_limit();
        run_stage_once(stage, &llm, &meter).await;
        if llm.calls() > 0 {
            sent.push(format!("{} sent {} call(s)", stage.label(), llm.calls()));
        }
    }
    assert!(
        sent.is_empty(),
        "nothing new starts once the limit is reached: {sent:?}"
    );
}

#[tokio::test]
async fn the_same_stages_do_send_when_the_limit_is_not_reached() {
    // The control for the test above: each stage fixture really reaches its model, so a count of
    // zero past the limit is the limit's doing and not an input that sends nothing.
    let mut silent: Vec<&str> = Vec::new();
    for stage in Stage::ALL {
        let llm = Counting::new();
        let meter = UsageMeter::with_controls(RunControls {
            max_cost_usd: Some(LIMIT),
            ..RunControls::default()
        });
        run_stage_once(stage, &llm, &meter).await;
        if llm.calls() == 0 {
            silent.push(stage.label());
        }
    }
    assert!(silent.is_empty(), "these fixtures sent nothing: {silent:?}");
}

#[tokio::test]
async fn a_judge_stopped_by_the_limit_is_recorded_not_run() {
    let llm = Counting::new();
    let meter = past_the_limit();
    run_stage_once(Stage::CloneJudge, &llm, &meter).await;
    let record = meter.cost_cap_record().expect("a limit was set");
    assert!(
        record
            .not_run_stages
            .iter()
            .any(|s| s.stage == STAGE_CLONE_JUDGE),
        "the stage is recorded as NOT RUN: {:?}",
        record.not_run_stages
    );
}
