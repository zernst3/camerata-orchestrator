//! Stage 2, the group reviewer, on the Message Batches path (GitHub #337), and the two stage
//! runner gaps found beside it: a credit error on a batch ROW, and the label of a credit stop.
//!
//! Every test drives the real stage through a scripted model and a fake Message Batches API: no
//! network and no spend. The clock is paused where a poll wait would otherwise be real.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::batch_mode::BatchApi;
use super::group_review::*;
use super::*;
use crate::checkpoint;
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};
use crate::onboard::ScanReport;
use camerata_llm::failure::FailureClass;

const SQL: &str = "SEC-NO-RAW-SQL-CONCAT-1";
const SECRETS: &str = "SEC-NO-HARDCODED-SECRETS-1";
const MODEL: &str = "claude-fable-5-1";
const INPUT_TOKENS: u64 = 2_000;
const OUTPUT_TOKENS: u64 = 300;
const CACHE_READ_TOKENS: u64 = 1_500;
const CREDIT_ERROR: &str =
    "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing.";

// ── Fixtures ───────────────────────────────────────────────────────────────────────────────

fn site(rule: &str, i: usize) -> Finding {
    let mut f = Finding {
        repo: "acme/app".to_string(),
        path: format!("src/{}/f{i:04}.rs", rule.to_ascii_lowercase()),
        line: 10 + i,
        rule_id: rule.to_string(),
        severity: "medium".to_string(),
        snippet: format!("call_{rule}_{i}();"),
        detail: format!("Headline {rule} {i} - the handler skips the check."),
        effort: Some("low".to_string()),
        confidence: Some("high".to_string()),
        ..Finding::default()
    };
    f.captures.insert(
        STAGE1_CAPTURE.to_string(),
        serde_json::json!({"real": "yes", "reason": format!("stage 1 reason {i}")}).to_string(),
    );
    f
}

/// A report holding `n` sites of each `(rule, n)`; one rule group per rule.
fn report_of(rules: &[(&str, usize)]) -> ScanReport {
    let mut report = ScanReport::gated(&["acme/app".to_string()]);
    report.gated = false;
    report.code_lines = 40_000;
    for (rule, n) in rules {
        for i in 0..*n {
            report.findings.push(site(rule, i));
        }
    }
    report.assign_site_keys();
    report
}

fn cfg<'a>(rules: &'a [(String, String)], batch: bool) -> GroupReviewConfig<'a> {
    GroupReviewConfig {
        model: MODEL,
        repo_shape: "This repository has 12 code files.",
        rules,
        rank: crate::review_tree::RankSettings::default(),
        max_in_flight: 4,
        cli_backend: false,
        batch,
        job: None,
        feedback: None,
    }
}

fn reply_for(system: Option<&str>, prompt: &str) -> String {
    stand_in_reply(system, prompt).expect("a group review request")
}

fn response(text: &str) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: None,
        text: text.to_string(),
        model: "echoed-snapshot-id".to_string(),
        backend: "api/batch".to_string(),
        // No dollar figure: the runner prices the row, so the meter's price is what is under test.
        cost_usd: None,
        input_tokens: Some(INPUT_TOKENS),
        output_tokens: Some(OUTPUT_TOKENS),
        cache_read_input_tokens: CACHE_READ_TOKENS,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// The text of a batch item's user content, however it was split into cached blocks.
fn item_prompt(item: &BatchItem) -> String {
    let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
    match content.as_str() {
        Some(t) => t.to_string(),
        None => content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
    }
}

// ── Test doubles ───────────────────────────────────────────────────────────────────────────

/// A real-time model that answers every group review request with the stand-in reply and keeps
/// every request.
struct Realtime {
    requests: Mutex<Vec<LlmRequest>>,
}

impl Realtime {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
        }
    }
    fn asked(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmPort for Realtime {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let text = reply_for(req.system.as_deref(), &req.prompt);
        self.requests.lock().unwrap().push(req);
        let mut r = response(&text);
        r.backend = "stub".to_string();
        r.cost_usd = Some(0.01);
        Ok(r)
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

/// A fake Message Batches API that answers each item with the stand-in group review reply (or an
/// error row).
struct FakeBatch {
    realtime: Realtime,
    /// Per accepted batch (`msgbatch_<n>`): `(custom id, the reply the item gets)`.
    batches: Mutex<Vec<Vec<(String, String)>>>,
    /// Every item sent, as it was sent.
    items: Mutex<Vec<BatchItem>>,
    /// Batches an earlier process submitted.
    known: Mutex<BTreeMap<String, Vec<(String, String)>>>,
    /// Batch ids never end (a process killed while it waits).
    hang: bool,
    polls_on_hung: AtomicUsize,
    fetched: Mutex<Vec<String>>,
    /// When set, every row of every batch is an error row with this text.
    error_rows: Option<String>,
}

impl FakeBatch {
    fn new() -> Self {
        Self {
            realtime: Realtime::new(),
            batches: Mutex::new(Vec::new()),
            items: Mutex::new(Vec::new()),
            known: Mutex::new(BTreeMap::new()),
            hang: false,
            polls_on_hung: AtomicUsize::new(0),
            fetched: Mutex::new(Vec::new()),
            error_rows: None,
        }
    }
    fn hanging(mut self) -> Self {
        self.hang = true;
        self
    }
    fn erroring(mut self, text: &str) -> Self {
        self.error_rows = Some(text.to_string());
        self
    }
    fn batch_count(&self) -> usize {
        self.batches.lock().unwrap().len()
    }
    fn items_sent(&self) -> usize {
        self.items.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl BatchApi for FakeBatch {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.realtime
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        let mut batches = self.batches.lock().unwrap();
        let id = format!("msgbatch_{}", batches.len());
        batches.push(
            items
                .iter()
                .map(|i| {
                    (
                        i.custom_id.clone(),
                        reply_for(i.params.system.as_deref(), &item_prompt(i)),
                    )
                })
                .collect(),
        );
        self.items.lock().unwrap().extend(items);
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        if self.hang {
            self.polls_on_hung.fetch_add(1, Ordering::SeqCst);
            return Ok(BatchStatus {
                processing_status: "in_progress".to_string(),
                request_counts: BatchRequestCounts {
                    processing: 1,
                    ..BatchRequestCounts::default()
                },
            });
        }
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn cancel_batch(&self, _batch_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        self.fetched.lock().unwrap().push(batch_id.to_string());
        let members = match self.known.lock().unwrap().get(batch_id) {
            Some(known) => known.clone(),
            None => {
                let n: usize = batch_id.trim_start_matches("msgbatch_").parse().unwrap();
                self.batches.lock().unwrap()[n].clone()
            }
        };
        Ok(members
            .into_iter()
            .map(|(custom_id, reply)| match &self.error_rows {
                Some(text) => BatchResultRow {
                    custom_id,
                    response: None,
                    error: Some(text.clone()),
                    billed: None,
                },
                None => BatchResultRow {
                    custom_id,
                    response: Some(response(&reply)),
                    error: None,
                    billed: None,
                },
            })
            .collect())
    }
}

fn seam(api: &Arc<FakeBatch>) -> BatchSeam {
    BatchSeam::new(api.clone() as Arc<dyn BatchApi>)
}

async fn run(
    report: &mut ScanReport,
    llm: &dyn LlmPort,
    batch: bool,
    meter: Option<&UsageMeter>,
) -> GroupReviewOutcome {
    let rules: Vec<(String, String)> = Vec::new();
    review_groups(report, llm, &cfg(&rules, batch), meter).await
}

// ── 1. Group review goes through the batch runner in Batch mode ────────────────────────────

#[tokio::test(start_paused = true)]
async fn group_review_in_batch_mode_submits_through_the_batch_runner() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let outcome = run(&mut report, &llm, true, None).await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert_eq!(outcome.rules_failed, 0);
    assert!(api.batch_count() >= 1, "the calls went out as a batch");
    assert_eq!(api.items_sent(), 2, "one item per rule group");
    assert_eq!(
        api.realtime.asked().len(),
        0,
        "and nothing was sent in real time"
    );
}

#[tokio::test(start_paused = true)]
async fn group_review_stays_real_time_when_the_run_is_not_a_batch_run() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let outcome = run(&mut report, &llm, false, None).await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert_eq!(api.batch_count(), 0, "no batch was submitted");
    assert_eq!(
        api.realtime.asked().len(),
        2,
        "both calls went in real time"
    );
}

#[tokio::test(start_paused = true)]
async fn the_cli_backend_group_review_is_unchanged_and_never_batches() {
    // The run is a batch run and the seam carries a batch API, but the backend is the CLI: the
    // stage entry the app and the command line call keeps it in real time.
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let rules: Vec<(String, String)> = Vec::new();
    let controls = crate::cost_cap::RunControls::default();
    let outcome = review_scan_report(
        &mut report,
        &llm,
        &controls,
        &rules,
        true,
        true,
        None,
        None,
        None,
    )
    .await
    .expect("it ran");
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert_eq!(api.batch_count(), 0, "the CLI backend has no batches");
    assert_eq!(api.realtime.asked().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_stage_entry_batches_on_the_api_backend_when_the_run_is_a_batch_run() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let rules: Vec<(String, String)> = Vec::new();
    let controls = crate::cost_cap::RunControls::default();
    review_scan_report(
        &mut report,
        &llm,
        &controls,
        &rules,
        false,
        true,
        None,
        None,
        None,
    )
    .await
    .expect("it ran");
    assert_eq!(api.items_sent(), 2);
    assert_eq!(api.realtime.asked().len(), 0);
}

// ── 2. The shared prefix is one cached block ───────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn two_groups_share_the_exact_cached_prefix_and_it_carries_the_breakpoint() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let llm = Realtime::new();
    run(&mut report, &llm, false, None).await;
    let requests = llm.asked();
    assert_eq!(requests.len(), 2);

    let mut prefixes = Vec::new();
    for req in &requests {
        assert_eq!(
            req.cache_breakpoints.len(),
            1,
            "one breakpoint after the shared prefix"
        );
        let end = req.cache_breakpoints[0];
        assert!(end > 0 && end < req.prompt.len());
        prefixes.push(req.prompt.as_bytes()[..end].to_vec());
    }
    assert_eq!(
        prefixes[0], prefixes[1],
        "the two groups' requests share the exact prefix bytes"
    );
    let prefix = String::from_utf8(prefixes[0].clone()).unwrap();
    assert!(prefix.starts_with("Repository: acme/app\n"), "{prefix}");
    assert!(prefix.contains("This repository has 12 code files."));
    assert!(
        !prefix.contains("Rule group"),
        "nothing group-specific is cached: {prefix}"
    );
    // The system prompt is the same for both too, and leads the cached prefix on the wire.
    assert_eq!(requests[0].system, requests[1].system);
}

#[tokio::test(start_paused = true)]
async fn the_batch_item_carries_the_cached_prefix_as_its_own_block() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    run(&mut report, &llm, true, None).await;
    let items = api.items.lock().unwrap().clone();
    assert_eq!(items.len(), 2);
    let mut firsts = Vec::new();
    for item in &items {
        let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
        let blocks = content.as_array().expect("cached content is blocks");
        assert_eq!(blocks.len(), 2, "one breakpoint makes two blocks");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        assert!(blocks[1].get("cache_control").is_none());
        firsts.push(blocks[0]["text"].as_str().unwrap().to_string());
    }
    assert_eq!(firsts[0], firsts[1], "the cached block is byte-identical");
}

// ── 3. Resume ───────────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_resumed_run_with_a_submitted_group_review_batch_fetches_it_and_resubmits_nothing() {
    let dir = tempfile::tempdir().unwrap();

    // First process: the batch is submitted and never ends; the process is killed (the future is
    // dropped) while it polls.
    let first = Arc::new(FakeBatch::new().hanging());
    let first_llm = seam(&first);
    {
        let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
        let run_future = checkpoint::with_run_checkpoints(
            Some(dir.path().to_path_buf()),
            run(&mut report, &first_llm, true, None),
        );
        let killed = async {
            while first.polls_on_hung.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        };
        tokio::select! {
            _ = run_future => panic!("the first process was meant to be killed while a batch waited"),
            () = killed => {}
        }
    }
    assert_eq!(first.batch_count(), 1, "the batch was submitted once");
    let in_flight = first.batches.lock().unwrap()[0].clone();
    assert_eq!(in_flight.len(), 2);

    // Second process: a fresh API that knows the batch the first one left running, and the same
    // checkpoint.
    let second = Arc::new(FakeBatch::new());
    second
        .known
        .lock()
        .unwrap()
        .insert("msgbatch_0".to_string(), in_flight);
    second.batches.lock().unwrap().push(Vec::new());
    let second_llm = seam(&second);
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let outcome = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        run(&mut report, &second_llm, true, None),
    )
    .await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert_eq!(
        second.items_sent(),
        0,
        "nothing the first process submitted was sent again"
    );
    assert!(
        second
            .fetched
            .lock()
            .unwrap()
            .contains(&"msgbatch_0".to_string()),
        "the running batch was fetched by id"
    );
    assert_eq!(second.realtime.asked().len(), 0);

    // A third process, with the answers all absorbed, asks nothing at all.
    let third = Arc::new(FakeBatch::new());
    let third_llm = seam(&third);
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let outcome = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        run(&mut report, &third_llm, true, None),
    )
    .await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert_eq!(third.items_sent(), 0, "{:?}", third.batch_count());
    assert_eq!(third.realtime.asked().len(), 0);
}

// ── 4. The meter prices the batch and the cache ────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_meter_charges_batched_group_review_at_the_batch_and_cache_prices() {
    use crate::llm::{usage_cost_usd, CallUsage, PriceBasis, BATCH_CACHE_TTL};

    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let meter = UsageMeter::default();
    let outcome = run(&mut report, &llm, true, Some(&meter)).await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");

    let usage = CallUsage {
        input: Some(INPUT_TOKENS),
        output: Some(OUTPUT_TOKENS),
        cache_read: CACHE_READ_TOKENS,
        cache_creation: 0,
        cache_creation_1h: None,
    };
    let batch = usage_cost_usd(MODEL, &usage, PriceBasis::batch(BATCH_CACHE_TTL)).expect("priced");
    let realtime = usage_cost_usd(MODEL, &usage, PriceBasis::realtime()).expect("priced");
    assert!(batch < realtime, "the batch price is the discounted one");

    let snap = meter.snapshot();
    assert_eq!(snap.calls, 2);
    assert!(
        (snap.cost_usd - 2.0 * batch).abs() < 1e-6,
        "two items at the batch price {batch}: {}",
        snap.cost_usd
    );
    assert_eq!(snap.cache_read_input_tokens, 2 * CACHE_READ_TOKENS);
    assert!(meter
        .stage_telemetry(40_000)
        .stages
        .iter()
        .any(|s| s.stage == Stage::GroupReview));
}

#[tokio::test(start_paused = true)]
async fn a_run_past_its_spend_limit_submits_no_group_review_batch() {
    let controls = crate::cost_cap::RunControls {
        scope: Default::default(),
        max_cost_usd: Some(5.0),
        ..crate::cost_cap::RunControls::default()
    };
    let prior = ActualUsage {
        cost_usd: 12.0,
        calls: 40,
        cost_complete: true,
        ..ActualUsage::default()
    };
    let meter = UsageMeter::continuing(controls, &prior);
    assert!(meter.cap_reached());
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    run(&mut report, &llm, true, Some(&meter)).await;
    assert_eq!(api.batch_count(), 0, "nothing is submitted past the limit");
    assert_eq!(api.realtime.asked().len(), 0);
}

// ── 5. A credit error on a batch ROW stops the run at once ─────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_credit_error_on_a_batch_row_stops_the_run_and_is_not_resent() {
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let api = Arc::new(FakeBatch::new().erroring(CREDIT_ERROR));
    let llm = seam(&api);
    let meter = UsageMeter::default();
    let outcome = run(&mut report, &llm, true, Some(&meter)).await;

    assert_eq!(api.batch_count(), 1, "the errored rows were not sent again");
    assert!(
        meter.outage_stopped(),
        "the row's credit error reached run health"
    );
    let trigger = meter.health().trigger().expect("a trigger");
    assert_eq!(trigger.class, FailureClass::AuthOrBilling);
    assert_eq!(outcome.rules_reviewed, 0);
    assert_eq!(outcome.rules_failed, 2);
    let classes: Vec<FailureClass> = report.failed_passes.iter().map(|f| f.class).collect();
    assert!(
        classes.contains(&FailureClass::AuthOrBilling),
        "the failed part is labeled as billing, not transport: {classes:?}"
    );
    assert!(
        !classes.contains(&FailureClass::Transport),
        "no part is labeled transport: {classes:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_ordinary_error_row_is_still_resent_once() {
    let mut report = report_of(&[(SQL, 3)]);
    let api = Arc::new(FakeBatch::new().erroring("internal server error, please retry"));
    let llm = seam(&api);
    let meter = UsageMeter::default();
    run(&mut report, &llm, true, Some(&meter)).await;
    assert_eq!(api.batch_count(), 2, "one resend of the errored row");
    assert!(!meter.outage_stopped());
}

// ── 6. A credit stop is labeled as billing ─────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_part_not_sent_after_a_credit_stop_says_billing_not_unreachable() {
    for batch in [true, false] {
        let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
        let api = Arc::new(FakeBatch::new());
        let llm = seam(&api);
        let meter = UsageMeter::default();
        meter
            .health()
            .note_failure(FailureClass::AuthOrBilling, CREDIT_ERROR);
        assert!(meter.outage_stopped());
        run(&mut report, &llm, batch, Some(&meter)).await;
        assert_eq!(api.batch_count(), 0, "batch={batch}: nothing was sent");
        assert_eq!(api.realtime.asked().len(), 0, "batch={batch}");
        let text: String = report
            .failed_passes
            .iter()
            .map(|f| f.reason.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("refused the request for billing (credit balance)"),
            "batch={batch}: {text}"
        );
        assert!(
            !text.contains("could not be reached"),
            "batch={batch}: {text}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_part_not_sent_after_a_bad_key_stop_keeps_its_plain_sentence() {
    let mut report = report_of(&[(SQL, 3)]);
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let meter = UsageMeter::default();
    meter
        .health()
        .note_failure(FailureClass::AuthOrBilling, "invalid x-api-key");
    // A bad key is not a credit balance: it keeps the plain sentence it always had.
    run(&mut report, &llm, true, Some(&meter)).await;
    assert_eq!(api.batch_count(), 0);
    let text: String = report
        .failed_passes
        .iter()
        .map(|f| f.reason.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("credit balance"), "{text}");
    assert!(text.contains("could not be reached"), "{text}");
}

// ── 7. A real-time resume of a submitted batch; a changed group; the estimate ──────────────

#[tokio::test(start_paused = true)]
async fn a_batch_submitted_before_a_stop_is_fetched_by_a_real_time_resume() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(FakeBatch::new().hanging());
    let first_llm = seam(&first);
    {
        let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
        let run_future = checkpoint::with_run_checkpoints(
            Some(dir.path().to_path_buf()),
            run(&mut report, &first_llm, true, None),
        );
        let killed = async {
            while first.polls_on_hung.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        };
        tokio::select! {
            _ = run_future => panic!("the first process was meant to be killed while a batch waited"),
            () = killed => {}
        }
    }
    let in_flight = first.batches.lock().unwrap()[0].clone();

    // The resume asks for real time (`--realtime`): the batch is still fetched, never paid twice.
    let second = Arc::new(FakeBatch::new());
    second
        .known
        .lock()
        .unwrap()
        .insert("msgbatch_0".to_string(), in_flight);
    second.batches.lock().unwrap().push(Vec::new());
    let second_llm = seam(&second);
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let outcome = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        run(&mut report, &second_llm, false, None),
    )
    .await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert!(second
        .fetched
        .lock()
        .unwrap()
        .contains(&"msgbatch_0".to_string()));
    assert_eq!(second.items_sent(), 0, "nothing was submitted again");
    assert_eq!(
        second.realtime.asked().len(),
        0,
        "nothing went out in real time"
    );
}

#[tokio::test(start_paused = true)]
async fn a_group_that_gained_a_row_since_the_stop_is_asked_again_and_the_others_are_reused() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(FakeBatch::new());
    let first_llm = seam(&first);
    let mut report = report_of(&[(SQL, 3), (SECRETS, 2)]);
    let outcome = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        run(&mut report, &first_llm, true, None),
    )
    .await;
    assert_eq!(outcome.rules_reviewed, 2);
    assert_eq!(first.items_sent(), 2);

    // The resume reads a report where the SQL group has one more row.
    let second = Arc::new(FakeBatch::new());
    let second_llm = seam(&second);
    let mut report = report_of(&[(SQL, 4), (SECRETS, 2)]);
    let outcome = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        run(&mut report, &second_llm, true, None),
    )
    .await;
    assert_eq!(outcome.rules_reviewed, 2, "{outcome:?}");
    assert_eq!(
        second.items_sent(),
        1,
        "only the group that changed was asked again"
    );
    let item = second.items.lock().unwrap()[0].clone();
    assert!(
        item_prompt(&item).contains("SEC-NO-RAW-SQL-CONCAT-1"),
        "and it is the group that gained the row"
    );
    // Its stored reply was not applied to the shifted members: every row is in a sub-group.
    for f in &report.findings {
        assert!(f.group_path.len() >= 3, "{:?}", f.group_path);
    }
}

#[test]
fn the_estimate_is_the_batch_price_when_the_stage_runs_batched() {
    let full = group_review_estimate_usd(MODEL, 100, 5, false, false).expect("priced");
    let batched = group_review_estimate_usd(MODEL, 100, 5, false, true).expect("priced");
    assert!(
        (batched - full * crate::llm::BATCH_MULT).abs() < 1e-9,
        "{batched} vs {full}"
    );
}
