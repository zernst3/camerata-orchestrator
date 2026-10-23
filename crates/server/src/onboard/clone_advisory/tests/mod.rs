//! Tests for the clone advisory seam. No live model, no key, no network: a scripted [`StubJudge`]
//! answers every request, and counts them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use camerata_checks::clone_rules::{candidates, Candidate, CandidateSet, CloneRule, IndexSummary};

use super::prompt::BATCH_MARKER;
use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

mod cache;
mod ledger;
mod other_rules;
mod redaction;
mod repros;
mod seam;

type Script = Box<dyn FnMut(&LlmRequest, usize) -> anyhow::Result<String> + Send>;

/// A model that never touches the network: it calls `script(request, call_index)` for each
/// request and returns the text it produces (or its error).
pub(super) struct StubJudge {
    script: Mutex<Script>,
    pub calls: AtomicUsize,
    pub requests: Mutex<Vec<LlmRequest>>,
}

impl StubJudge {
    pub(super) fn new(
        script: impl FnMut(&LlmRequest, usize) -> anyhow::Result<String> + Send + 'static,
    ) -> Self {
        Self {
            script: Mutex::new(Box::new(script)),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Answers every candidate it is asked about with `word` and `concept`.
    pub(super) fn answering(word: impl Into<String>, concept: impl Into<String>) -> Self {
        let (word, concept) = (word.into(), concept.into());
        Self::new(move |req, _| Ok(verdicts(&asked_ids(req), &word, &concept)))
    }

    pub(super) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(super) fn request(&self, n: usize) -> LlmRequest {
        self.requests.lock().unwrap()[n].clone()
    }
}

#[async_trait]
impl LlmPort for StubJudge {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req.clone());
        let text = (self.script.lock().unwrap())(&req, index)?;
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.001),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        })
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let response = self.complete(req).await?;
        on_delta(&response.text);
        Ok(response)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The candidate ids a request asks about, read from the JSON array after the batch marker.
pub(super) fn asked_ids(req: &LlmRequest) -> Vec<String> {
    let json = req
        .prompt
        .split_once(BATCH_MARKER)
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let items: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i["id"].as_str().map(str::to_string))
        .collect()
}

/// A well-formed response giving every id the same verdict.
pub(super) fn verdicts(ids: &[String], word: &str, concept: &str) -> String {
    let items: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!({ "id": id, "verdict": word, "concept": concept }))
        .collect();
    serde_json::json!({ "verdicts": items }).to_string()
}

pub(super) fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
        .collect()
}

/// A TypeScript function applying a premium discount at `rate`, about 100 tokens.
pub(super) fn discount(func: &str, rate: &str) -> String {
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
}

/// A structurally unrelated second decision (shipping cost), so a repo can hold two families.
pub(super) fn shipping(func: &str, weight: &str) -> String {
    format!(
        "export function {func}(parcel, zone) {{
  let cost = parcel.weight * 1.5;
  if (zone.remote === true) {{
    cost = cost + zone.surcharge;
  }}
  if (parcel.weight > {weight} && parcel.fragile !== false) {{
    cost = cost * 2;
  }}
  return cost > 500 ? 500 : cost;
}}
"
    )
}

/// A repo whose discount is written twice with `rate_a` and `rate_b`.
pub(super) fn discount_repo(rate_a: &str, rate_b: &str) -> Vec<(String, String)> {
    files(&[
        ("src/cart/total.ts", &discount("applyDiscount", rate_a)),
        ("src/checkout/summary.ts", &discount("quoteTotal", rate_b)),
    ])
}

pub(super) fn armed(rules: &[CloneRule]) -> std::collections::HashSet<&'static str> {
    rules.iter().map(|r| r.id()).collect()
}

pub(super) fn set_for(files: &[(String, String)], rules: &[CloneRule]) -> CandidateSet {
    candidates(files, &armed(rules))
}

pub(super) const JUDGE: &str = "stub-judge-model";

/// A request over `files` with the standard test settings.
pub(super) fn request<'a>(files: &'a [(String, String)], set: CandidateSet) -> JudgeRequest<'a> {
    JudgeRequest {
        repo: "acme/shop",
        files,
        set,
        judge_model: JUDGE,
        corpus: None,
        unavailable_reason: "deterministic-only scan",
    }
}

/// Run the judge with a fresh cache and no meter or transcript.
pub(super) async fn judge(
    llm: Option<&dyn LlmPort>,
    files: &[(String, String)],
    set: CandidateSet,
) -> JudgeOutcome {
    let mut cache = VerdictCache::default();
    judge_clone_candidates(llm, request(files, set), &mut cache, None, None).await
}

/// `n` distinct single-source candidates built from one real candidate, ids made unique, for the
/// batching and cap tests (the producer itself is tested in the checks crate).
pub(super) fn synthetic_set(n: usize, over_cap: u32) -> (Vec<(String, String)>, CandidateSet) {
    let all = discount_repo("0.2", "0.2");
    let base = set_for(&all, &[CloneRule::SingleSourceLogic]).by_rule
        [&CloneRule::SingleSourceLogic][0]
        .clone();
    let list: Vec<Candidate> = (0..n)
        .map(|i| {
            let mut c = base.clone();
            c.id = format!(
                "{}:{i:016x}:{}",
                CloneRule::SingleSourceLogic.id(),
                c.family.token_len
            );
            c.rank = u32::try_from(i).unwrap();
            c
        })
        .collect();
    let mut by_rule = BTreeMap::new();
    by_rule.insert(CloneRule::SingleSourceLogic, list);
    let mut over = BTreeMap::new();
    over.insert(CloneRule::SingleSourceLogic, over_cap);
    (
        all,
        CandidateSet {
            by_rule,
            over_cap: over,
            index: IndexSummary {
                files_read: 2,
                ..IndexSummary::default()
            },
        },
    )
}
