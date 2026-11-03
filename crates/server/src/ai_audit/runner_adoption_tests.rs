//! Every post-scan stage on the shared batch runner, with resume (design 2026-11-02, sections 6.2
//! to 6.4, A7, A13 and A15; WO7).
//!
//! Every test drives the real stage through a scripted model and a fake Message Batches API: no
//! network and no spend. The clock is paused where a poll wait would otherwise be real.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::batch_mode::BatchApi;
use super::calibration::{Stage1Inputs, BUILT_RANGES};
use super::*;
use crate::checkpoint::{self, StageCheckpoint};
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};

const GOOD: &str = r#"{"severity":"high","real":"yes","effort":"low","category":"authorization","reason":"reachable without a check"}"#;

/// What every scripted reply is billed, so a test can add up what a stage reports.
const INPUT_TOKENS: u64 = 100;
const OUTPUT_TOKENS: u64 = 10;

fn reply_of(text: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        model: "stub".to_string(),
        backend: "stub".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(INPUT_TOKENS),
        output_tokens: Some(OUTPUT_TOKENS),
        cache_read_input_tokens: 0,
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

/// The site a stage 1 request is about: the `call_<n>();` marker its snippet carries.
fn site_of(prompt: &str) -> Option<usize> {
    let rest = prompt.split("call_").nth(1)?;
    rest.split("();").next()?.parse().ok()
}

// ── Test doubles ───────────────────────────────────────────────────────────────────────────

/// A real-time model that answers every stage 1 request with a verdict and records who it was
/// asked about. `fail_sites` fail in transport.
struct Realtime {
    seen: Mutex<Vec<usize>>,
    requests: Mutex<Vec<LlmRequest>>,
    fail_sites: Mutex<BTreeSet<usize>>,
}

impl Realtime {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            fail_sites: Mutex::new(BTreeSet::new()),
        }
    }

    fn failing(sites: &[usize]) -> Self {
        let r = Self::new();
        *r.fail_sites.lock().unwrap() = sites.iter().copied().collect();
        r
    }

    fn heal(&self) {
        self.fail_sites.lock().unwrap().clear();
    }

    fn asked(&self) -> Vec<usize> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmPort for Realtime {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let site = site_of(&req.prompt);
        if let Some(n) = site {
            self.seen.lock().unwrap().push(n);
        }
        self.requests.lock().unwrap().push(req);
        if site.is_some_and(|n| self.fail_sites.lock().unwrap().contains(&n)) {
            anyhow::bail!("connection reset by peer");
        }
        Ok(reply_of(GOOD))
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

/// A fake Message Batches API. It keeps no request bodies (30,000 of them would be 300 MB): only
/// the sites each batch carries and the shape of the first item of each.
struct FakeBatch {
    realtime: Realtime,
    /// Per accepted batch (`msgbatch_<n>`): `(custom id, site)` of every item.
    batches: Mutex<Vec<Vec<(String, usize)>>>,
    /// The first item of each batch, as it was sent.
    first_items: Mutex<Vec<BatchItem>>,
    /// Batches an earlier process submitted: id to its `(custom id, site)` items.
    known: Mutex<BTreeMap<String, Vec<(String, usize)>>>,
    /// Batch ids from this index on never end (a process killed while it waits).
    hang_from: Option<usize>,
    polls_on_hung: AtomicUsize,
    fetched: Mutex<Vec<String>>,
}

impl FakeBatch {
    fn new() -> Self {
        Self {
            realtime: Realtime::new(),
            batches: Mutex::new(Vec::new()),
            first_items: Mutex::new(Vec::new()),
            known: Mutex::new(BTreeMap::new()),
            hang_from: None,
            polls_on_hung: AtomicUsize::new(0),
            fetched: Mutex::new(Vec::new()),
        }
    }

    fn hanging_from(mut self, batch: usize) -> Self {
        self.hang_from = Some(batch);
        self
    }

    fn submitted_sites(&self) -> Vec<usize> {
        self.batches
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .map(|(_, site)| *site)
            .collect()
    }

    fn batch_count(&self) -> usize {
        self.batches.lock().unwrap().len()
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
                        site_of(&item_prompt(i)).expect("a stage 1 item"),
                    )
                })
                .collect(),
        );
        if let Some(first) = items.into_iter().next() {
            self.first_items.lock().unwrap().push(first);
        }
        Ok(id)
    }
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        let n: usize = batch_id.trim_start_matches("msgbatch_").parse().unwrap();
        if self.hang_from.is_some_and(|from| n >= from) {
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
            .map(|(custom_id, _)| BatchResultRow {
                custom_id,
                response: Some(reply_of(GOOD)),
                error: None,
                billed: None,
            })
            .collect())
    }
}

// ── Fixtures ───────────────────────────────────────────────────────────────────────────────

/// A site of `ARCH-X-1`: the `n`-th, in a file of its own so every site key is distinct.
fn site(n: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: format!("src/f{:03}.rs", n % 200),
        line: 1 + n % 40,
        rule_id: "ARCH-X-1".to_string(),
        severity: "medium".to_string(),
        snippet: format!("call_{n}();"),
        detail: "The handler skips the check.".to_string(),
        ..Finding::default()
    }
}

fn sites(n: usize) -> Vec<Finding> {
    (0..n).map(site).collect()
}

fn files() -> Vec<(String, String)> {
    (0..200)
        .map(|f| {
            (
                format!("src/f{f:03}.rs"),
                (1..=60).map(|i| format!("line {i}\n")).collect(),
            )
        })
        .collect()
}

async fn calibrate(
    llm: &dyn LlmPort,
    findings: Vec<Finding>,
    files: &[(String, String)],
    meter: Option<&UsageMeter>,
    batch: bool,
) -> (Vec<Finding>, Vec<FailedPass>) {
    super::calibration::verify_findings(
        llm,
        "acme/app",
        findings,
        None,
        None,
        meter,
        &Stage1Inputs::new("This repository has 200 code files.")
            .with_files(files)
            .with_concurrency(4)
            .with_batch(batch),
    )
    .await
}

fn seam(api: &Arc<FakeBatch>) -> BatchSeam {
    BatchSeam::new(api.clone() as Arc<dyn BatchApi>)
}

fn verified(findings: &[Finding]) -> usize {
    findings
        .iter()
        .filter(|f| f.confidence.as_deref() == Some("high"))
        .count()
}

// ── Stage 1 on the runner ──────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn stage1_runs_as_a_batch_when_the_backend_has_batches_and_in_real_time_otherwise() {
    let files = files();
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let (out, failed) = calibrate(&llm, sites(5), &files, None, true).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(api.batch_count(), 1, "the five sites went as one batch");
    assert_eq!(
        api.realtime.asked().len(),
        0,
        "and nothing was sent in real time"
    );
    assert_eq!(verified(&out), 5, "every site got its verdict");

    // No batch API on the seam: the same sites run in real time.
    let plain = Realtime::new();
    let (out, failed) = calibrate(&plain, sites(5), &files, None, true).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(plain.asked().len(), 5);
    assert_eq!(verified(&out), 5);

    // A run that asked for real time stays in real time even where the seam has batches.
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let (out, _) = calibrate(&llm, sites(5), &files, None, false).await;
    assert_eq!(api.batch_count(), 0);
    assert_eq!(api.realtime.asked().len(), 5);
    assert_eq!(verified(&out), 5);
}

#[tokio::test(start_paused = true)]
async fn the_batch_path_keeps_the_cache_breakpoint_after_the_shared_prompt() {
    let files = files();
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    let (_, failed) = calibrate(&llm, sites(3), &files, None, true).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(api.batch_count(), 1, "stage 1 went out as a batch");

    // The item that is actually sent: the shared prefix is its own cached block, the site is not.
    let item = api.first_items.lock().unwrap()[0].clone();
    let system = item
        .params
        .system
        .clone()
        .expect("the system prompt is sent");
    assert!(
        system.starts_with(&verify_system_prompt()),
        "the shared system prompt leads every item"
    );
    let content = serde_json::to_value(&item.params.messages[0].content).unwrap();
    let blocks = content.as_array().expect("cached content is blocks");
    assert_eq!(blocks.len(), 2, "one breakpoint makes two blocks");
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    let prefix = blocks[0]["text"].as_str().unwrap();
    assert!(prefix.contains("Repository: acme/app"), "{prefix}");
    assert!(
        !prefix.contains("call_0();"),
        "no site text in the cached prefix"
    );
    assert!(blocks[1].get("cache_control").is_none());
    assert!(blocks[1]["text"].as_str().unwrap().contains("call_0();"));
}

#[tokio::test(start_paused = true)]
async fn stage1_items_are_built_in_ranges_of_the_batch_size() {
    let files = files();
    let n = CALIBRATION_BATCH_SIZE * 3 + 7;
    let api = Arc::new(FakeBatch::new());
    let llm = seam(&api);
    BUILT_RANGES.with(|r| r.borrow_mut().clear());
    let (out, failed) = calibrate(&llm, sites(n), &files, None, true).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(verified(&out), n);
    assert_eq!(api.submitted_sites().len(), n, "every site was submitted");

    let ranges = BUILT_RANGES.with(|r| r.borrow().clone());
    assert!(
        ranges.len() >= 4,
        "the requests were built in several ranges, not at once: {ranges:?}"
    );
    assert!(
        ranges.iter().all(|r| *r <= CALIBRATION_BATCH_SIZE),
        "no range is larger than the batch size: {ranges:?}"
    );
    assert_eq!(
        ranges.iter().sum::<usize>(),
        n,
        "and together they cover every site"
    );
}

#[tokio::test(start_paused = true)]
async fn stage1_in_real_time_builds_in_ranges_too() {
    let files = files();
    let n = CALIBRATION_BATCH_SIZE * 2 + 3;
    let llm = Realtime::new();
    BUILT_RANGES.with(|r| r.borrow_mut().clear());
    let (out, _) = calibrate(&llm, sites(n), &files, None, false).await;
    assert_eq!(verified(&out), n);
    let ranges = BUILT_RANGES.with(|r| r.borrow().clone());
    assert!(ranges.len() >= 3, "{ranges:?}");
    assert!(
        ranges.iter().all(|r| *r <= CALIBRATION_BATCH_SIZE),
        "{ranges:?}"
    );
    assert_eq!(ranges.iter().sum::<usize>(), n);
}

// ── Resume ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn stage1_resume_asks_only_the_sites_that_have_no_result_and_reports_the_usage_of_all() {
    let dir = tempfile::tempdir().unwrap();
    let files = files();

    // First process: sites 3 and 4 cannot be reached.
    let first = Realtime::failing(&[3, 4]);
    let meter = UsageMeter::default();
    let (out, failed) = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        calibrate(&first, sites(6), &files, Some(&meter), false),
    )
    .await;
    assert_eq!(verified(&out), 4, "four sites were calibrated");
    assert!(!failed.is_empty(), "the two that were not are disclosed");
    let cp = StageCheckpoint::new(dir.path(), "calibration_acme_app");
    assert_eq!(cp.absorbed().len(), 4, "one line per absorbed site");

    // Second process: the same checkpoint. Only the two sites with no result are asked.
    let second = Realtime::new();
    second.heal();
    let meter = UsageMeter::default();
    let (out, failed) = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        calibrate(&second, sites(6), &files, Some(&meter), false),
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let mut asked = second.asked();
    asked.sort_unstable();
    assert_eq!(asked, vec![3, 4], "nothing already absorbed is asked again");
    assert_eq!(verified(&out), 6, "every site ends with its verdict");
    let usage = meter.snapshot();
    assert_eq!(
        usage.input_tokens,
        6 * INPUT_TOKENS,
        "the usage of the four absorbed sites is reported with the two just paid for"
    );
    assert_eq!(usage.output_tokens, 6 * OUTPUT_TOKENS);
}

/// 30,000 synthetic sites through stage 1 as batches, killed while the third batch waits, then
/// resumed from its checkpoint. Wall time and peak memory are printed for the report.
#[tokio::test(start_paused = true)]
async fn a_resumed_30k_site_calibration_resubmits_nothing_already_absorbed() {
    const SITES: usize = 30_000;
    let started = std::time::Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let files = files();

    // First process: the third batch is submitted and never ends; the process is killed (the
    // future is dropped) while it polls.
    let first = Arc::new(FakeBatch::new().hanging_from(2));
    let first_llm = seam(&first);
    let meter = UsageMeter::default();
    {
        let run = checkpoint::with_run_checkpoints(
            Some(dir.path().to_path_buf()),
            calibrate(&first_llm, sites(SITES), &files, Some(&meter), true),
        );
        let killed = async {
            while first.polls_on_hung.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        };
        tokio::select! {
            _ = run => panic!("the first process was meant to be killed while a batch waited"),
            () = killed => {}
        }
    }
    let done_by_first = first
        .batches
        .lock()
        .unwrap()
        .iter()
        .take(2)
        .map(Vec::len)
        .sum::<usize>();
    assert!(
        done_by_first > 0,
        "the first two batches ended before the kill"
    );
    let in_flight: Vec<(String, usize)> = first.batches.lock().unwrap()[2].clone();

    // Second process: a fresh API that knows the batch the first one left running (the provider
    // still has it), and the same checkpoint.
    let second = Arc::new(FakeBatch::new());
    second
        .known
        .lock()
        .unwrap()
        .insert("msgbatch_2".to_string(), in_flight.clone());
    // The provider's ids continue where the first process stopped.
    for _ in 0..3 {
        second.batches.lock().unwrap().push(Vec::new());
    }
    let second_llm = seam(&second);
    let meter = UsageMeter::default();
    let (out, failed) = checkpoint::with_run_checkpoints(
        Some(dir.path().to_path_buf()),
        calibrate(&second_llm, sites(SITES), &files, Some(&meter), true),
    )
    .await;
    assert!(failed.is_empty(), "{:?}", failed.first());
    assert_eq!(verified(&out), SITES, "every site ends with its verdict");

    // Nothing absorbed in the first process was sent again, and the batch left running was
    // fetched, not submitted again.
    let first_sent: BTreeSet<usize> = first.submitted_sites().into_iter().collect();
    let second_sent: Vec<usize> = second.submitted_sites();
    let overlap: Vec<&usize> = second_sent
        .iter()
        .filter(|s| first_sent.contains(s))
        .collect();
    assert!(
        overlap.is_empty(),
        "{} site(s) were submitted by both processes",
        overlap.len()
    );
    assert!(
        second
            .fetched
            .lock()
            .unwrap()
            .contains(&"msgbatch_2".to_string()),
        "the running batch was fetched by id"
    );
    let all_sent: BTreeSet<usize> = first_sent
        .iter()
        .chain(second_sent.iter())
        .copied()
        .collect();
    assert_eq!(
        all_sent.len(),
        SITES,
        "every site was submitted exactly once between them"
    );
    assert_eq!(first_sent.len() + second_sent.len(), SITES);

    // The usage of the whole stage is reported by the second process: absorbed sites included.
    let usage = meter.snapshot();
    assert_eq!(usage.input_tokens, SITES as u64 * INPUT_TOKENS);
    assert_eq!(usage.output_tokens, SITES as u64 * OUTPUT_TOKENS);
    println!(
        "30k resume: wall time {:.1?}, {} batches by the first process, {} by the second",
        started.elapsed(),
        first.batch_count(),
        second.batch_count() - 3
    );
}

// ── The runner's lazy items ────────────────────────────────────────────────────────────────

use super::stage_batch::{run_stage, ItemReject, StageCtx, StageItems, StageMode, StageSettings};

/// A stage of `n` items whose requests are built only when asked for, in ranges of 7, with
/// windows of 20 on the batch path.
struct Lazy {
    n: usize,
    built: Mutex<Vec<usize>>,
    got: BTreeSet<usize>,
}

impl StageItems for Lazy {
    type Id = String;
    fn items(&self) -> Vec<(String, LlmRequest)> {
        Vec::new()
    }
    fn build_range(&self) -> usize {
        7
    }
    fn batch_window(&self) -> usize {
        20
    }
    fn ids(&self) -> Vec<String> {
        (0..self.n).map(|i| format!("k{i:03}")).collect()
    }
    fn items_for(&self, ids: &[String]) -> Vec<(String, LlmRequest)> {
        self.built.lock().unwrap().push(ids.len());
        ids.iter()
            .map(|id| {
                let i: usize = id[1..].parse().unwrap();
                (
                    id.clone(),
                    LlmRequest::new(format!("call_{i}();"))
                        .with_model("claude-sonnet-5-5")
                        .with_max_tokens(100),
                )
            })
            .collect()
    }
    fn absorb(&mut self, id: &String, _resp: LlmResponse) -> Result<(), ItemReject> {
        self.got.insert(id[1..].parse().unwrap());
        Ok(())
    }
    fn retry(&self, _id: &String, _reject: &ItemReject) -> Option<LlmRequest> {
        None
    }
}

#[tokio::test(start_paused = true)]
async fn a_lazy_stage_is_built_in_ranges_and_sent_in_windows_on_the_batch_path() {
    let api = FakeBatch::new();
    let mut stage = Lazy {
        n: 50,
        built: Mutex::new(Vec::new()),
        got: BTreeSet::new(),
    };
    let ctx = StageCtx {
        repo: "acme/app",
        model: "claude-sonnet-5-5",
        realtime: &api.realtime,
        batch: Some(&api),
        meter: None,
        job: None,
        checkpoint: None,
        settings: StageSettings::default(),
    };
    let out = run_stage(Stage::Calibration, &mut stage, StageMode::Batch, &ctx).await;
    assert!(out.is_complete(), "{:?}", out.failed);
    assert_eq!(stage.got.len(), 50);
    let built = stage.built.lock().unwrap().clone();
    assert!(
        built.iter().all(|n| *n <= 7),
        "ranges of at most 7: {built:?}"
    );
    assert_eq!(built.iter().sum::<usize>(), 50);
    let sizes: Vec<usize> = api.batches.lock().unwrap().iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![20, 20, 10], "one submission per window");
}

#[tokio::test(start_paused = true)]
async fn a_lazy_stage_in_parallel_mode_builds_ahead_only_by_a_range() {
    let api = FakeBatch::new();
    let mut stage = Lazy {
        n: 30,
        built: Mutex::new(Vec::new()),
        got: BTreeSet::new(),
    };
    let ctx = StageCtx {
        repo: "acme/app",
        model: "claude-sonnet-5-5",
        realtime: &api.realtime,
        batch: None,
        meter: None,
        job: None,
        checkpoint: None,
        settings: StageSettings::default(),
    };
    let out = run_stage(
        Stage::Calibration,
        &mut stage,
        StageMode::Parallel { max_in_flight: 3 },
        &ctx,
    )
    .await;
    assert!(out.is_complete(), "{:?}", out.failed);
    assert_eq!(stage.got.len(), 30);
    let built = stage.built.lock().unwrap().clone();
    assert!(built.len() >= 5, "built in several ranges: {built:?}");
    assert!(built.iter().all(|n| *n <= 7), "{built:?}");
}

// ── No stage loop calls the model itself ───────────────────────────────────────────────────

/// The text of the function that starts at `header` in `src`, braces matched.
fn fn_body<'s>(src: &'s str, header: &str) -> &'s str {
    let start = src
        .find(header)
        .unwrap_or_else(|| panic!("no `{header}` in the source"));
    let open = start + src[start..].find('{').expect("a function body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after `{header}`");
}

/// Source check: every post-scan stage sends its requests through `run_stage`, and no stage loop
/// calls the model itself. A stage that grows a direct `complete` call, or loses its `run_stage`
/// call, fails here.
#[test]
fn every_post_scan_stage_runs_through_run_stage() {
    const AUDIT: &str = include_str!("../ai_audit.rs");
    const CALIBRATION: &str = include_str!("calibration.rs");
    const GROUP_REVIEW: &str = include_str!("group_review.rs");
    const DETAIL: &str = include_str!("detail_regen.rs");

    // (stage, the functions that drive it, source they live in)
    let stages: [(&str, &str, &[&str]); 5] = [
        (
            "stage 1 calibration",
            CALIBRATION,
            &["pub async fn verify_findings"],
        ),
        (
            "stage 2 group review",
            GROUP_REVIEW,
            &["pub async fn review_groups"],
        ),
        (
            "detail regeneration",
            DETAIL,
            &["async fn run_detail_round"],
        ),
        (
            "fix generation",
            AUDIT,
            &["async fn run_fix_specifics_round"],
        ),
        ("alternatives", AUDIT, &["async fn send"]),
    ];
    for (stage, src, headers) in stages {
        for header in headers {
            let body = fn_body(src, header);
            assert!(
                body.contains("run_stage("),
                "{stage}: `{header}` does not send through run_stage"
            );
            for direct in [".complete(", ".complete_streaming("] {
                assert!(
                    !body.contains(direct),
                    "{stage}: `{header}` calls the model directly (`{direct}`)"
                );
            }
        }
    }
    // The drivers of the stages that loop over rounds hand each round to the functions above.
    for (stage, header) in [
        ("fix generation", "pub async fn generate_fix_specifics"),
        ("alternatives", "async fn recommend_alternatives"),
    ] {
        let body = fn_body(AUDIT, header);
        for direct in [".complete(", ".complete_streaming(", "run_stage("] {
            assert!(
                !body.contains(direct),
                "{stage}: `{header}` makes model calls itself (`{direct}`); they belong in its round function"
            );
        }
    }
    // Whole-file check for the stage modules: no non-test code calls the model directly.
    for (name, src) in [
        ("calibration.rs", CALIBRATION),
        ("detail_regen.rs", DETAIL),
        ("group_review.rs", GROUP_REVIEW),
    ] {
        let code = src.split("#[cfg(test)]").next().unwrap();
        for direct in [".complete(", ".complete_streaming("] {
            assert!(
                !code.contains(direct),
                "{name} calls the model directly (`{direct}`)"
            );
        }
    }
}

// ── A refused stage 1 window is disclosed (review wave 6, D1) ──────────────────────────────

fn capped_meter(cap: f64, hint: Option<f64>) -> UsageMeter {
    UsageMeter::with_controls(crate::cost_cap::RunControls {
        review: Default::default(),
        efficiency: crate::efficiency::EfficiencyConfig::default(),
        max_cost_usd: Some(cap),
        estimate_hint_usd: hint,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    })
}

/// A body limit that puts three of the test sites in each submission, measured from the item
/// that is actually sent so the test does not depend on the prompt's size.
async fn body_limit_for_three_sites(files: &[(String, String)]) -> usize {
    let probe = Arc::new(FakeBatch::new());
    let (_, failed) = calibrate(&seam(&probe), sites(3), files, None, true).await;
    assert!(failed.is_empty(), "{failed:?}");
    let item = probe.first_items.lock().unwrap()[0].clone();
    let size = serde_json::to_vec(&item).unwrap().len() + 1;
    size * 3 + size / 2
}

/// The run report as the scan would assemble it from what the meter recorded.
fn report_of(meter: &UsageMeter) -> crate::onboard::ScanReport {
    let mut report = crate::onboard::ScanReport::gated(&["acme/app".to_string()]);
    if let Some(record) = meter.cost_cap_record() {
        report.failed_passes.extend(record.failed_passes());
    }
    if let Some(outage) = meter.outage_record() {
        report
            .failed_passes
            .extend(crate::cost_cap::not_run_failed_passes(
                &outage.not_run_passes,
                &outage.not_run_stages,
                "the provider could not be reached",
            ));
    }
    report
}

#[tokio::test(start_paused = true)]
async fn stage1_batch_refused_by_the_spend_limit_is_a_failed_pass_and_an_incomplete_run() {
    let files = files();
    let limit = body_limit_for_three_sites(&files).await;
    crate::ai_audit::batch_mode::set_test_body_limit(Some(limit));

    // The estimate says the run fits ($0.02 of a $0.025 limit), so the first window goes out;
    // its three sites bill $0.03, so the limit refuses the second window.
    let api = Arc::new(FakeBatch::new());
    let meter = capped_meter(0.025, Some(0.02));
    let (out, _) = calibrate(&seam(&api), sites(6), &files, Some(&meter), true).await;
    crate::ai_audit::batch_mode::set_test_body_limit(None);

    assert_eq!(api.batch_count(), 1, "only the first window was submitted");
    assert_eq!(out.len(), 6, "no site is dropped");
    assert_eq!(
        verified(&out),
        3,
        "the three sites of the second window have no verdict"
    );

    let record = meter.cost_cap_record().expect("a limit was set");
    assert!(record.stopped_the_run(), "the limit stopped something");
    let failed = record.failed_passes();
    assert_eq!(failed.len(), 1, "one failed pass: {failed:?}");
    assert_eq!(failed[0].repo, "acme/app");
    assert_eq!(failed[0].class, FailureClass::SpendLimit);
    assert!(
        !report_of(&meter).incomplete_reasons().is_empty(),
        "a run with a window the limit refused is not complete"
    );
}

#[tokio::test(start_paused = true)]
async fn stage1_batch_refused_by_an_outage_stop_is_recorded_as_not_run() {
    let files = files();
    let api = Arc::new(FakeBatch::new());
    let meter = capped_meter(100.0, None);
    // The provider refused the credentials: the run is stopped before stage 1 submits.
    meter
        .health()
        .note_failure(FailureClass::AuthOrBilling, "401 invalid x-api-key");
    let (out, _) = calibrate(&seam(&api), sites(3), &files, Some(&meter), true).await;

    assert_eq!(
        api.batch_count(),
        0,
        "nothing is submitted after an outage stop"
    );
    assert_eq!(out.len(), 3, "no site is dropped");
    let outage = meter.outage_record().expect("the outage tripped");
    assert_eq!(
        outage.not_run_stages.len(),
        1,
        "stage 1 is recorded as not run: {:?}",
        outage.not_run_stages
    );
    assert_eq!(outage.not_run_stages[0].repo, "acme/app");
    assert_eq!(outage.not_run_stages[0].class, FailureClass::OutageStop);
    assert!(!report_of(&meter).incomplete_reasons().is_empty());
}

#[tokio::test(start_paused = true)]
async fn stage1_in_real_time_refused_by_an_outage_stop_is_recorded_as_not_run() {
    let files = files();
    let llm = Realtime::new();
    let meter = capped_meter(100.0, None);
    meter
        .health()
        .note_failure(FailureClass::AuthOrBilling, "401 invalid x-api-key");
    let (out, _) = calibrate(&llm, sites(3), &files, Some(&meter), false).await;

    assert!(
        llm.asked().is_empty(),
        "nothing is sent after an outage stop"
    );
    assert_eq!(out.len(), 3, "no site is dropped");
    let outage = meter.outage_record().expect("the outage tripped");
    assert_eq!(
        outage.not_run_stages.len(),
        1,
        "stage 1 is recorded as not run: {:?}",
        outage.not_run_stages
    );
    assert_eq!(outage.not_run_stages[0].repo, "acme/app");
    assert!(!report_of(&meter).incomplete_reasons().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_capped_stage1_with_a_generous_limit_and_no_hint_submits_its_first_window() {
    let files = files();
    let api = Arc::new(FakeBatch::new());
    let meter = capped_meter(1_000.0, None);
    let (out, failed) = calibrate(&seam(&api), sites(5), &files, Some(&meter), true).await;

    assert_eq!(api.batch_count(), 1, "the first window was submitted");
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(verified(&out), 5, "every site got its verdict");
    let record = meter.cost_cap_record().expect("a limit was set");
    assert!(!record.stopped_the_run(), "{:?}", record.notes);
}
