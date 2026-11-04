//! The shared batch runner (design 2026-11-02, section 6.4): every test drives
//! [`stage_batch::run_stage`] through a scripted stage and fake model ports, with no network and
//! with the clock paused, so waits of minutes or hours resolve at once and can be measured.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camerata_llm::failure::{AttemptRecord, FailureClass, LlmCallFailure};
use tokio::time::Instant;

use super::batch_mode::{
    submit_round, BatchApi, BatchWork, PollTicker, RoundContext, STALL_AFTER_FAILED_POLLS,
};
use super::stage_batch::{
    run_stage, Aimd, ItemReject, StageCtx, StageItems, StageMode, StageSettings, CLI_MAX_IN_FLIGHT,
    DEFAULT_MAX_IN_FLIGHT, DEFAULT_RATE_LIMIT_DEADLINE, MAX_IN_FLIGHT_CAP, SUCCESSES_PER_PERMIT,
};
use super::*;
use crate::checkpoint::{BatchRecord, StageCheckpoint};
use crate::jobs::JobStore;
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};

const MODEL: &str = "claude-sonnet-5-5";
const STAGE: Stage = Stage::Calibration;

// ── the scripted stage ──────────────────────────────────────────────────────────────────────────

fn request(prompt: String) -> LlmRequest {
    LlmRequest::new(prompt)
        .with_model(MODEL)
        .with_max_tokens(1024)
}

fn prompt_for(id: &str) -> String {
    format!("ITEM={id};")
}

/// The item id a request is for.
fn id_of(prompt: &str) -> String {
    let rest = prompt.split("ITEM=").nth(1).expect("an ITEM= marker");
    rest.split(';').next().expect("an id").to_string()
}

/// A stage whose behaviour each test scripts: which replies it turns down, which it gives up on,
/// which requests are huge.
#[derive(Default)]
struct Scripted {
    ids: Vec<String>,
    got: BTreeMap<String, String>,
    /// Ids whose reply is turned down unless it answers a retry request.
    reject_first: BTreeSet<String>,
    /// Ids whose reply is always turned down.
    reject_always: BTreeSet<String>,
    /// Ids for which `retry` gives up (`None`).
    no_retry: BTreeSet<String>,
    /// Ids the stage already holds from before a restart.
    holds: BTreeSet<String>,
    /// Ids whose request is far over any model window.
    huge: BTreeSet<String>,
    /// Ids whose RETRY request is far over any model window.
    huge_retry: BTreeSet<String>,
    /// Bytes of padding every request carries, so a window of them has a real size.
    padding: usize,
    absorb_calls: usize,
}

impl Scripted {
    fn new(ids: &[&str]) -> Self {
        Self {
            ids: ids.iter().map(ToString::to_string).collect(),
            ..Self::default()
        }
    }

    fn numbered(n: usize) -> Self {
        Self {
            ids: (0..n).map(|i| format!("item-{i:03}")).collect(),
            ..Self::default()
        }
    }
}

impl StageItems for Scripted {
    type Id = String;

    fn items(&self) -> Vec<(String, LlmRequest)> {
        self.ids
            .iter()
            .map(|id| {
                let mut prompt = prompt_for(id);
                prompt.push_str(&"p".repeat(self.padding));
                if self.huge.contains(id) {
                    prompt.push_str(&"x".repeat(20_000_000));
                }
                (id.clone(), request(prompt))
            })
            .collect()
    }

    fn absorb(&mut self, id: &String, resp: LlmResponse) -> Result<(), ItemReject> {
        self.absorb_calls += 1;
        let is_retry_reply = resp.text.contains("RETRY");
        if self.reject_always.contains(id) || (self.reject_first.contains(id) && !is_retry_reply) {
            return Err(ItemReject::new("the verdict named the wrong site"));
        }
        self.got.insert(id.clone(), resp.text);
        Ok(())
    }

    fn retry(&self, id: &String, _reject: &ItemReject) -> Option<LlmRequest> {
        if self.no_retry.contains(id) {
            return None;
        }
        let mut prompt = format!("ITEM={id};RETRY");
        if self.huge_retry.contains(id) {
            prompt.push_str(&"x".repeat(20_000_000));
        }
        Some(request(prompt))
    }

    fn already_absorbed(&self, id: &String) -> bool {
        self.holds.contains(id)
    }
}

// ── fake model ports ────────────────────────────────────────────────────────────────────────────

fn reply_for(prompt: &str) -> LlmResponse {
    LlmResponse {
        text: format!("answer to {prompt}"),
        model: MODEL.to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.02),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

fn rate_limited(retry_after_secs: u64) -> anyhow::Error {
    let mut rec = AttemptRecord::new("api", MODEL);
    rec.http_status = Some(429);
    rec.retry_after_secs = Some(retry_after_secs);
    let mut f = LlmCallFailure::new(
        FailureClass::RateLimited,
        "Anthropic API HTTP 429: slow down",
    );
    f.attempts = vec![rec];
    f.retryable = true;
    anyhow::Error::new(f)
}

fn content_error(text: &str) -> anyhow::Error {
    anyhow::Error::new(LlmCallFailure::new(FailureClass::Content, text))
}

/// One request the fake real-time port saw.
#[derive(Debug, Clone)]
struct Call {
    id: String,
    started: Instant,
    /// Calls in flight, this one included, when it started.
    active_at_start: usize,
}

/// What the script answers for one call: how long it takes, and what it returns.
type Script = dyn Fn(&str, usize) -> (Duration, anyhow::Result<LlmResponse>) + Send + Sync;

struct Realtime {
    script: Box<Script>,
    per_id: Mutex<HashMap<String, usize>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    calls: Mutex<Vec<Call>>,
    prompt_lens: Mutex<Vec<usize>>,
}

impl Realtime {
    fn new(
        script: impl Fn(&str, usize) -> (Duration, anyhow::Result<LlmResponse>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            script: Box::new(script),
            per_id: Mutex::new(HashMap::new()),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            prompt_lens: Mutex::new(Vec::new()),
        }
    }

    /// Every call answers after `latency`.
    fn ok_after(latency: Duration) -> Self {
        Self::new(move |id, _| (latency, Ok(reply_for(&prompt_for(id)))))
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmPort for Realtime {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let id = id_of(&req.prompt);
        let nth = {
            let mut per_id = self.per_id.lock().unwrap();
            let n = per_id.entry(id.clone()).or_insert(0);
            let nth = *n;
            *n += 1;
            nth
        };
        self.prompt_lens.lock().unwrap().push(req.prompt.len());
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.calls.lock().unwrap().push(Call {
            id: id.clone(),
            started: Instant::now(),
            active_at_start: active,
        });
        let (latency, result) = (self.script)(&req.prompt_marker(), nth);
        tokio::time::sleep(latency).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        result.map(|mut r| {
            r.text = format!("answer to {}", req.prompt);
            r
        })
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

/// `LlmRequest` helper for the fake: the id the script is keyed on.
trait PromptMarker {
    fn prompt_marker(&self) -> String;
}

impl PromptMarker for LlmRequest {
    fn prompt_marker(&self) -> String {
        id_of(&self.prompt)
    }
}

/// How the fake Message Batches API answers one item of one batch.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Ok,
    Errored,
    Missing,
    /// Failed but billed: a model refusal.
    Refused,
}

type RowScript = dyn Fn(usize, &str) -> Row + Send + Sync;
type SubmitScript = dyn Fn(usize) -> Option<anyhow::Error> + Send + Sync;
type PollProbe = dyn Fn(usize) + Send + Sync;

struct Api {
    realtime: Realtime,
    rows: Box<RowScript>,
    submit_script: Box<SubmitScript>,
    /// `(custom id, item id)` per accepted batch; the batch id is `msgbatch_<index>`.
    batches: Mutex<Vec<Vec<(String, String)>>>,
    /// Batches an earlier process submitted: id to `(custom id, item id)`.
    known: Mutex<HashMap<String, Vec<(String, String)>>>,
    submit_calls: AtomicUsize,
    polls: AtomicUsize,
    fetched: Mutex<Vec<String>>,
    /// `(processing, succeeded)` per poll before the batch ends.
    poll_counts: Mutex<VecDeque<(u32, u32)>>,
    fetch_fails: bool,
    probe: Option<Box<PollProbe>>,
    submit_times: Mutex<Vec<Instant>>,
    cancel_calls: Mutex<Vec<String>>,
    /// Batch ids whose poll and fetch fail, with the HTTP status the provider answers.
    broken: Mutex<HashMap<String, u16>>,
    /// The simulated kill: `submit` accepts the batch (the provider holds it) and then the
    /// process dies before the id can be saved, so the call never returns.
    kill_after_accepting: bool,
}

impl Api {
    fn new() -> Self {
        Self {
            realtime: Realtime::ok_after(Duration::from_millis(1)),
            rows: Box::new(|_, _| Row::Ok),
            submit_script: Box::new(|_| None),
            batches: Mutex::new(Vec::new()),
            known: Mutex::new(HashMap::new()),
            submit_calls: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            fetched: Mutex::new(Vec::new()),
            poll_counts: Mutex::new(VecDeque::new()),
            fetch_fails: false,
            probe: None,
            submit_times: Mutex::new(Vec::new()),
            cancel_calls: Mutex::new(Vec::new()),
            broken: Mutex::new(HashMap::new()),
            kill_after_accepting: false,
        }
    }

    /// A saved batch the provider answers `status` for on every poll and fetch (404: gone).
    fn breaking(self, batch_id: &str, status: u16) -> Self {
        self.broken
            .lock()
            .unwrap()
            .insert(batch_id.to_string(), status);
        self
    }

    /// The process is killed after the provider accepts a submission and before its id is saved.
    fn killed_after_accepting(mut self) -> Self {
        self.kill_after_accepting = true;
        self
    }

    fn broken_answer(&self, batch_id: &str) -> Option<anyhow::Error> {
        let status = *self.broken.lock().unwrap().get(batch_id)?;
        if status == 404 {
            let body = r#"{"type":"error","error":{"type":"not_found_error","message":"batch not found"}}"#;
            let mut f = LlmCallFailure::new(
                FailureClass::Content,
                format!("batch results HTTP 404 Not Found: {body}"),
            );
            f.http = Some((404, body.to_string()));
            Some(anyhow::Error::new(f))
        } else {
            Some(anyhow::anyhow!(
                "Message Batches API HTTP {status}: unavailable"
            ))
        }
    }

    fn rows(mut self, f: impl Fn(usize, &str) -> Row + Send + Sync + 'static) -> Self {
        self.rows = Box::new(f);
        self
    }

    fn submit_script(
        mut self,
        f: impl Fn(usize) -> Option<anyhow::Error> + Send + Sync + 'static,
    ) -> Self {
        self.submit_script = Box::new(f);
        self
    }

    fn polls_before_end(self, counts: &[(u32, u32)]) -> Self {
        *self.poll_counts.lock().unwrap() = counts.iter().copied().collect();
        self
    }

    fn failing_fetch(mut self) -> Self {
        self.fetch_fails = true;
        self
    }

    fn probing(mut self, f: impl Fn(usize) + Send + Sync + 'static) -> Self {
        self.probe = Some(Box::new(f));
        self
    }

    /// Know a batch an earlier process submitted, from the checkpoint's own record of it.
    fn knowing(self, record: &BatchRecord) -> Self {
        self.known.lock().unwrap().insert(
            record.batch_id.clone(),
            record
                .items
                .iter()
                .map(|(cid, key)| (cid.clone(), key.clone()))
                .collect(),
        );
        self
    }

    fn submitted_item_ids(&self) -> Vec<Vec<String>> {
        self.batches
            .lock()
            .unwrap()
            .iter()
            .map(|b| b.iter().map(|(_, id)| id.clone()).collect())
            .collect()
    }
}

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

#[async_trait::async_trait]
impl BatchApi for Api {
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
        let call = self.submit_calls.fetch_add(1, Ordering::SeqCst);
        self.submit_times.lock().unwrap().push(Instant::now());
        if let Some(err) = (self.submit_script)(call) {
            return Err(err);
        }
        let mut batches = self.batches.lock().unwrap();
        let id = format!("msgbatch_{}", batches.len());
        batches.push(
            items
                .iter()
                .map(|i| (i.custom_id.clone(), id_of(&item_prompt(i))))
                .collect(),
        );
        drop(batches);
        if self.kill_after_accepting {
            panic!("simulated kill: the process died after the provider accepted batch {id}");
        }
        Ok(id)
    }
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        if let Some(e) = self.broken_answer(batch_id) {
            return Err(e);
        }
        let n = self.polls.fetch_add(1, Ordering::SeqCst);
        if let Some(probe) = &self.probe {
            probe(n);
        }
        let _ = batch_id;
        match self.poll_counts.lock().unwrap().pop_front() {
            Some((processing, succeeded)) => Ok(BatchStatus {
                processing_status: "in_progress".to_string(),
                request_counts: BatchRequestCounts {
                    processing,
                    succeeded,
                    ..BatchRequestCounts::default()
                },
            }),
            None => Ok(BatchStatus {
                processing_status: "ended".to_string(),
                request_counts: BatchRequestCounts::default(),
            }),
        }
    }
    async fn cancel_batch(&self, batch_id: &str) -> anyhow::Result<()> {
        self.cancel_calls.lock().unwrap().push(batch_id.to_string());
        Ok(())
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        if let Some(e) = self.broken_answer(batch_id) {
            return Err(e);
        }
        if self.fetch_fails {
            anyhow::bail!("Message Batches API HTTP 503: results unavailable");
        }
        self.fetched.lock().unwrap().push(batch_id.to_string());
        let members = match self.known.lock().unwrap().get(batch_id) {
            Some(known) => known.clone(),
            None => {
                let n: usize = batch_id.trim_start_matches("msgbatch_").parse().unwrap();
                self.batches.lock().unwrap()[n].clone()
            }
        };
        let round: usize = batch_id
            .trim_start_matches("msgbatch_")
            .parse()
            .unwrap_or(0);
        Ok(members
            .into_iter()
            .filter_map(|(custom_id, item)| {
                let row = |response: Option<LlmResponse>, error: Option<&str>| BatchResultRow {
                    custom_id: custom_id.clone(),
                    response,
                    error: error.map(str::to_string),
                    billed: None,
                };
                match (self.rows)(round, &item) {
                    Row::Ok => Some(row(Some(reply_for(&prompt_for(&item))), None)),
                    Row::Errored => Some(row(None, Some("overloaded"))),
                    Row::Missing => None,
                    Row::Refused => Some(BatchResultRow {
                        billed: Some(reply_for("")),
                        ..row(None, Some("the model refused this request (refusal)"))
                    }),
                }
            })
            .collect())
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────────────────────────

fn settings() -> StageSettings {
    StageSettings {
        poll_interval: Duration::from_secs(1),
        rate_limit_deadline: DEFAULT_RATE_LIMIT_DEADLINE,
        rate_limit_pause: Duration::from_secs(30),
        max_body_bytes: 192_000_000,
        per_item_usd: None,
    }
}

fn ctx<'a>(realtime: &'a dyn LlmPort, batch: Option<&'a dyn BatchApi>) -> StageCtx<'a> {
    StageCtx {
        repo: "acme/widgets",
        model: MODEL,
        realtime,
        batch,
        meter: None,
        job: None,
        checkpoint: None,
        settings: settings(),
    }
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

fn failed_ids(out: &stage_batch::StageOutcome<String>) -> Vec<String> {
    out.failed.iter().map(|f| f.id.clone()).collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

// ── 6.4 invariants ──────────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn run_stage_in_batch_mode_absorbs_every_item_or_records_it_as_a_failed_part() {
    let mut stage = Scripted::new(&["a", "b", "c", "d", "e", "f", "g"]);
    // Round 0: b errors, c is left out, d is refused (billed), g errors. Round 1 (the one resend):
    // b and c answer, g errors again.
    let api = Api::new().rows(|round, item| match (round, item) {
        (0, "b" | "g") => Row::Errored,
        (0, "c") => Row::Missing,
        (_, "d") => Row::Refused,
        (1, "g") => Row::Errored,
        _ => Row::Ok,
    });
    let c = ctx(&api.realtime, Some(&api));

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;

    assert_eq!(
        sorted(out.absorbed.clone()),
        ids(&["a", "b", "c", "e", "f"])
    );
    assert_eq!(failed_ids(&out), ids(&["d", "g"]));
    // Every item is in exactly one place.
    assert_eq!(
        out.absorbed.len() + out.resumed.len() + out.failed.len(),
        7,
        "nothing is dropped and nothing is counted twice"
    );
    let d = out.failed.iter().find(|f| f.id == "d").unwrap();
    assert_eq!(
        d.class,
        FailureClass::Content,
        "a refusal is not sent again"
    );
    let g = out.failed.iter().find(|f| f.id == "g").unwrap();
    assert_eq!(g.class, FailureClass::Transport);
    assert!(!g.reason.is_empty());
    // One submission of all seven, one follow-up of the three that could be sent again.
    assert_eq!(api.submitted_item_ids().len(), 2);
    assert_eq!(api.submitted_item_ids()[0].len(), 7);
    assert_eq!(
        sorted(api.submitted_item_ids()[1].clone()),
        ids(&["b", "c", "g"])
    );
    assert_eq!(stage.got["b"], "answer to ITEM=b;");
    assert_eq!(
        api.realtime.call_count(),
        0,
        "batch mode sends nothing in real time"
    );
}

#[tokio::test(start_paused = true)]
async fn parallel_fallback_never_exceeds_max_in_flight() {
    let rt = Realtime::ok_after(Duration::from_secs(10));
    let mut stage = Scripted::numbered(40);
    let c = ctx(&rt, None);

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 3 },
        &c,
    )
    .await;

    assert_eq!(out.absorbed.len(), 40);
    assert!(out.failed.is_empty());
    assert_eq!(
        rt.max_active.load(Ordering::SeqCst),
        3,
        "it used all three and never a fourth"
    );

    // Asked for more than the cap, it stops at the cap.
    let rt = Realtime::ok_after(Duration::from_secs(10));
    let mut stage = Scripted::numbered(40);
    let c = ctx(&rt, None);
    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 100 },
        &c,
    )
    .await;
    assert_eq!(out.absorbed.len(), 40);
    assert_eq!(rt.max_active.load(Ordering::SeqCst), MAX_IN_FLIGHT_CAP);

    // The named defaults.
    assert_eq!(DEFAULT_MAX_IN_FLIGHT, 8);
    assert_eq!(CLI_MAX_IN_FLIGHT, 2);
    assert_eq!(
        StageMode::parallel_default(false),
        StageMode::Parallel { max_in_flight: 8 }
    );
    assert_eq!(
        StageMode::parallel_default(true),
        StageMode::Parallel { max_in_flight: 2 }
    );
}

#[tokio::test(start_paused = true)]
async fn a_429_halves_in_flight_and_waits_retry_after() {
    // item-002 is rate limited on its first call, sooner than the others finish, and the provider
    // says to wait 120 seconds.
    let rt = Realtime::new(|id, nth| {
        if id == "item-002" && nth == 0 {
            (Duration::from_secs(5), Err(rate_limited(120)))
        } else {
            (Duration::from_secs(10), Ok(reply_for(&prompt_for(id))))
        }
    });
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(30);
    let mut c = ctx(&rt, None);
    c.meter = Some(&meter);
    let t0 = Instant::now();

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 8 },
        &c,
    )
    .await;

    assert_eq!(
        out.absorbed.len(),
        30,
        "the rate-limited item was sent again"
    );
    assert!(out.failed.is_empty());
    assert_eq!(out.pacing.start_limit, 8);
    assert_eq!(out.pacing.throttles, 1);
    assert_eq!(out.pacing.min_limit, 4, "eight permits, halved once");
    let calls = rt.calls();
    // The first eight started together; nothing started again until the 120 seconds were over
    // (the 429 landed at second 5, so the hold ends at second 125).
    let later: Vec<&Call> = calls.iter().skip(8).collect();
    assert!(!later.is_empty());
    for call in &later {
        assert!(
            call.started.duration_since(t0) >= Duration::from_secs(125),
            "{} started at {:?}, inside the hold",
            call.id,
            call.started.duration_since(t0)
        );
        assert!(
            call.active_at_start <= 4,
            "{} ran as one of {} in flight after the halving",
            call.id,
            call.active_at_start
        );
    }
    assert!(!meter.outage_stopped());
}

#[test]
fn fifty_successes_restore_one_permit() {
    assert_eq!(SUCCESSES_PER_PERMIT, 50);
    let now = Instant::now();
    let mut a = Aimd::new(8, DEFAULT_RATE_LIMIT_DEADLINE);
    assert_eq!(a.limit(), 8);
    a.on_throttle(Duration::from_secs(1), now, true);
    assert_eq!(a.limit(), 4);
    for _ in 0..49 {
        a.on_success();
    }
    assert_eq!(a.limit(), 4, "forty-nine is not enough");
    a.on_success();
    assert_eq!(a.limit(), 5, "the fiftieth gives one permit back");
    for _ in 0..(50 * 20) {
        a.on_success();
    }
    assert_eq!(a.limit(), 8, "it never goes past what it was asked for");

    // The floor is one permit.
    let mut b = Aimd::new(2, DEFAULT_RATE_LIMIT_DEADLINE);
    b.on_throttle(Duration::from_secs(1), now, true);
    assert_eq!(b.limit(), 1);
    b.on_throttle(Duration::from_secs(1), now + Duration::from_secs(5), true);
    assert_eq!(b.limit(), 1, "never below one");

    // A burst of 429s inside one hold is one event, not several.
    let mut c = Aimd::new(8, DEFAULT_RATE_LIMIT_DEADLINE);
    c.on_throttle(Duration::from_secs(60), now, true);
    c.on_throttle(Duration::from_secs(60), now + Duration::from_secs(1), true);
    c.on_throttle(Duration::from_secs(60), now + Duration::from_secs(2), true);
    assert_eq!(c.limit(), 4);

    // Asked for more than the cap, it is the cap.
    assert_eq!(
        Aimd::new(100, DEFAULT_RATE_LIMIT_DEADLINE).limit(),
        MAX_IN_FLIGHT_CAP
    );
}

#[tokio::test(start_paused = true)]
async fn fifty_successes_restore_a_permit_in_a_run() {
    // The first call is rate limited; the rest answer. Two hundred items give far more than fifty
    // successes after the halving, so the run ends above where it was halved to and no higher
    // than the eight it was asked for.
    let rt = Realtime::new(|id, nth| {
        if id == "item-000" && nth == 0 {
            (Duration::from_secs(1), Err(rate_limited(1)))
        } else {
            (Duration::from_secs(1), Ok(reply_for(&prompt_for(id))))
        }
    });
    let mut stage = Scripted::numbered(200);
    let c = ctx(&rt, None);

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 8 },
        &c,
    )
    .await;

    assert_eq!(out.absorbed.len(), 200);
    assert_eq!(out.pacing.min_limit, 4);
    assert!(
        out.pacing.final_limit > 4 && out.pacing.final_limit <= 8,
        "{:?}",
        out.pacing
    );
}

#[tokio::test(start_paused = true)]
async fn rate_limited_streak_does_not_trip_the_outage_stop() {
    // The first twenty calls are all 429s; far more than the outage streak of three.
    let counter = Arc::new(AtomicUsize::new(0));
    let seen = counter.clone();
    let rt = Realtime::new(move |id, _| {
        if seen.fetch_add(1, Ordering::SeqCst) < 20 {
            (Duration::from_secs(1), Err(rate_limited(1)))
        } else {
            (Duration::from_secs(1), Ok(reply_for(&prompt_for(id))))
        }
    });
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(25);
    let mut c = ctx(&rt, None);
    c.meter = Some(&meter);

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;

    assert!(
        !meter.outage_stopped(),
        "a 429 streak is a pause, not an outage"
    );
    assert_eq!(
        out.absorbed.len(),
        25,
        "every item was sent again until it answered"
    );
    assert!(out.failed.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_stage_becomes_failed_parts_only_after_its_deadline() {
    let rt = Realtime::new(|_, _| (Duration::from_secs(1), Err(rate_limited(600))));
    let mut stage = Scripted::numbered(5);
    let mut c = ctx(&rt, None);
    c.settings.rate_limit_deadline = Duration::from_secs(3600);
    let t0 = Instant::now();

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 2 },
        &c,
    )
    .await;

    let waited = Instant::now().duration_since(t0);
    assert!(
        waited >= Duration::from_secs(3600),
        "it kept waiting for the whole deadline: {waited:?}"
    );
    assert!(waited < Duration::from_secs(3600 + 1300), "{waited:?}");
    assert!(out.absorbed.is_empty());
    assert_eq!(out.failed.len(), 5, "then every item is a failed part");
    assert!(out
        .failed
        .iter()
        .all(|f| f.class == FailureClass::RateLimited));
    assert_eq!(DEFAULT_RATE_LIMIT_DEADLINE, Duration::from_secs(6 * 3600));
}

#[tokio::test(start_paused = true)]
async fn a_batch_submit_rate_limit_waits_retry_after_and_submits_the_same_items_again() {
    let api = Api::new().submit_script(|call| (call < 2).then(|| rate_limited(60)));
    let mut stage = Scripted::new(&["a", "b", "c"]);
    let c = ctx(&api.realtime, Some(&api));
    let t0 = Instant::now();

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;

    assert_eq!(out.absorbed.len(), 3);
    assert!(out.failed.is_empty());
    assert_eq!(
        api.submit_calls.load(Ordering::SeqCst),
        3,
        "two refused, one accepted"
    );
    assert_eq!(api.submitted_item_ids().len(), 1, "one batch was accepted");
    let times = api.submit_times.lock().unwrap().clone();
    assert!(times[1].duration_since(t0) >= Duration::from_secs(60));
    assert!(times[2].duration_since(times[1]) >= Duration::from_secs(60));
}

#[tokio::test(start_paused = true)]
async fn a_rejected_reply_is_asked_again_once_and_a_second_rejection_is_a_failed_part() {
    let rt = Realtime::ok_after(Duration::from_secs(1));
    let mut stage = Scripted::new(&["a", "b", "c", "d"]);
    stage.reject_first.insert("a".to_string());
    stage.reject_first.insert("b".to_string());
    stage.no_retry.insert("b".to_string());
    stage.reject_always.insert("c".to_string());
    let c = ctx(&rt, None);

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;

    assert_eq!(sorted(out.absorbed.clone()), ids(&["a", "d"]));
    assert_eq!(failed_ids(&out), ids(&["b", "c"]));
    assert_eq!(
        stage.got["a"], "answer to ITEM=a;RETRY",
        "a was re-asked and absorbed"
    );
    let per_id = |id: &str| rt.calls().iter().filter(|c| c.id == id).count();
    assert_eq!(per_id("a"), 2);
    assert_eq!(per_id("b"), 1, "the stage gave b up");
    assert_eq!(per_id("c"), 2, "one retry at most");
    assert_eq!(per_id("d"), 1);
    let c_failed = out.failed.iter().find(|f| f.id == "c").unwrap();
    assert_eq!(c_failed.class, FailureClass::Content);
    assert!(
        c_failed.reason.contains("turned down"),
        "{}",
        c_failed.reason
    );
}

// ── resume ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn resume_skips_absorbed_items_and_polls_a_live_batch_instead_of_resubmitting() {
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let all = ["a1", "a2", "a3", "a4", "a5", "a6"];

    // Process 1, real time: a1 and a2 answer and are absorbed; the rest fail.
    let rt = Realtime::new(|id, _| {
        if id == "a1" || id == "a2" {
            (Duration::from_secs(1), Ok(reply_for(&prompt_for(id))))
        } else {
            (Duration::from_secs(1), Err(content_error("boom")))
        }
    });
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&rt, None);
    c.checkpoint = Some(&cp);
    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;
    assert_eq!(sorted(out.absorbed), ids(&["a1", "a2"]));
    assert_eq!(out.failed.len(), 4);

    // Process 2, batch: a1 and a2 are skipped; the other four are submitted as one batch whose
    // results cannot be fetched. They are failed parts that name the live batch.
    let api = Api::new().failing_fetch();
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(sorted(out.resumed.clone()), ids(&["a1", "a2"]));
    assert!(out.absorbed.is_empty());
    assert_eq!(out.failed.len(), 4);
    assert!(out
        .failed
        .iter()
        .all(|f| f.live_batch.as_deref() == Some("msgbatch_0")));
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 1);
    let records = cp.batches();
    assert_eq!(records.len(), 1);
    assert!(!records[0].settled, "the batch is still live");
    assert_eq!(records[0].items.len(), 4);

    // Process 3: the batch has ended. It is polled and fetched by id and NOT submitted again;
    // the two absorbed items are not sent anywhere.
    let api = Api::new().knowing(&records[0]);
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(sorted(out.resumed), ids(&["a1", "a2"]));
    assert_eq!(sorted(out.absorbed), ids(&["a3", "a4", "a5", "a6"]));
    assert!(out.failed.is_empty());
    assert_eq!(
        api.submit_calls.load(Ordering::SeqCst),
        0,
        "nothing was resubmitted"
    );
    assert!(
        api.polls.load(Ordering::SeqCst) >= 1,
        "the live batch was polled"
    );
    assert_eq!(*api.fetched.lock().unwrap(), vec!["msgbatch_0".to_string()]);
    assert_eq!(api.realtime.call_count(), 0);
    assert!(cp.batches()[0].settled);
    assert_eq!(cp.absorbed().len(), 6);

    // Process 4: everything is absorbed already; nothing is sent, polled or fetched.
    let api = Api::new();
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(out.resumed.len(), 6);
    assert!(out.absorbed.is_empty() && out.failed.is_empty());
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.polls.load(Ordering::SeqCst), 0);
    assert_eq!(api.realtime.call_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn resume_writes_usage_for_absorbed_items_so_the_ledger_is_never_zero() {
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let rt = Realtime::ok_after(Duration::from_secs(1));
    let first_meter = UsageMeter::default();
    let mut stage = Scripted::numbered(5);
    let mut c = ctx(&rt, None);
    c.checkpoint = Some(&cp);
    c.meter = Some(&first_meter);
    run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;
    assert_eq!(first_meter.snapshot().calls, 5);

    // A new process: a fresh meter, a fresh stage. Nothing is sent, yet the meter shows what the
    // five items cost, so the stage is not a zero-token row in the ledger.
    let rt = Realtime::ok_after(Duration::from_secs(1));
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(5);
    let mut c = ctx(&rt, None);
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);
    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;
    assert_eq!(out.resumed.len(), 5);
    assert_eq!(rt.call_count(), 0, "no absorbed item is paid for again");
    let usage = meter.snapshot();
    assert_eq!(usage.calls, 5);
    assert_eq!(usage.input_tokens, 500);
    assert_eq!(usage.output_tokens, 50);
    assert!((usage.cost_usd - 0.10).abs() < 1e-9, "{}", usage.cost_usd);
    // The stage got every reply back, since it held none of them.
    assert_eq!(stage.got.len(), 5);
    assert_eq!(stage.got["item-003"], "answer to ITEM=item-003;");

    // A stage that already holds its results is not handed them a second time.
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(5);
    stage.holds = stage.ids.iter().cloned().collect();
    let mut c = ctx(&rt, None);
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);
    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;
    assert_eq!(out.resumed.len(), 5);
    assert_eq!(stage.absorb_calls, 0);
    assert_eq!(meter.snapshot().calls, 5, "the usage is still written");
}

// ── liveness ────────────────────────────────────────────────────────────────────────────────────

fn idle_ms(store: &JobStore, id: &str) -> u128 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    store.idle_ms(id, now).unwrap()
}

/// What a poll saw on the job: its idle time, its stage progress and its batch id.
type Seen = (u128, Option<crate::jobs::StageProgress>, Option<String>);

#[tokio::test(start_paused = true)]
async fn poll_tick_touches_job_activity_and_progress() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    store.age_activity_for_test(&id);
    assert!(
        idle_ms(&store, &id) > 60_000,
        "the job starts out looking idle"
    );

    // What the job looked like at the start of each poll, i.e. after the previous tick.
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let probe_store = store.clone();
    let probe_id = id.clone();
    let probe_seen = seen.clone();
    let api = Api::new()
        .polls_before_end(&[(3, 1), (1, 3)])
        .probing(move |_| {
            let job = probe_store.get(&probe_id).unwrap();
            probe_seen.lock().unwrap().push((
                idle_ms(&probe_store, &probe_id),
                job.stage_progress,
                job.batch_id,
            ));
            // Age it again so the next probe shows the next tick's touch and not this one's.
            probe_store.age_activity_for_test(&probe_id);
        });
    let mut stage = Scripted::new(&["a", "b", "c", "d"]);
    let mut c = ctx(&api.realtime, Some(&api));
    c.job = Some((&store, &id));

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(out.absorbed.len(), 4);

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "two polls in progress, then the ended one");
    // The probe ages the job after each look, so each later look shows that tick's own touch.
    assert!(
        seen[1].0 < 5_000,
        "the first poll tick touched the job: {}",
        seen[1].0
    );
    assert!(seen[2].0 < 5_000, "and so did the second: {}", seen[2].0);
    let p1 = seen[1].1.clone().expect("progress after the first tick");
    assert_eq!(
        (p1.stage.as_str(), p1.done, p1.total),
        ("calibration", 1, 4)
    );
    let p2 = seen[2].1.clone().expect("progress after the second tick");
    assert_eq!((p2.done, p2.total), (3, 4));
    assert!(p2.since_ms > 0);
    assert_eq!(
        seen[1].2.as_deref(),
        Some("msgbatch_0"),
        "the batch id is on the job"
    );
    assert!(
        store.get(&id).unwrap().stage_progress.is_none(),
        "the progress line goes away when the stage ends"
    );
}

#[tokio::test(start_paused = true)]
async fn the_scan_batch_path_touches_the_job_on_every_poll_tick_too() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    store.age_activity_for_test(&id);
    let idle_after_first: Arc<Mutex<Vec<u128>>> = Arc::new(Mutex::new(Vec::new()));
    let (s2, i2, seen) = (store.clone(), id.clone(), idle_after_first.clone());
    let api = Api::new().polls_before_end(&[(2, 0)]).probing(move |_| {
        seen.lock().unwrap().push(idle_ms(&s2, &i2));
    });
    let work: Vec<BatchWork> = (0..2)
        .map(|i| BatchWork {
            custom_id: format!("c{i}-b0"),
            ci: i,
            bi: 0,
            req: request(prompt_for(&format!("w{i}"))),
        })
        .collect();
    let refs: Vec<&BatchWork> = work.iter().collect();
    let chunks: Vec<&[(String, String)]> = Vec::new();
    let batches: Vec<&[(String, String)]> = Vec::new();
    let ctx = RoundContext {
        api: &api,
        repo: "acme/widgets",
        label: "pass",
        model: MODEL,
        n_c: 0,
        n_b: 0,
        chunks: &chunks,
        batches: &batches,
        job: Some((&store, &id)),
        meter: None,
        per_item_usd: None,
        max_body_bytes: 192_000_000,
        fatal_errors: false,
    };

    let out = submit_round(&ctx, &refs).await.unwrap();

    assert_eq!(out.results.len(), 2);
    let seen = idle_after_first.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[1] < 5_000,
        "the scan's first poll tick touched the job: {}",
        seen[1]
    );
}

#[test]
fn three_failed_polls_in_a_row_stop_touching_the_job() {
    assert_eq!(STALL_AFTER_FAILED_POLLS, 3);
    let store = JobStore::new();
    let id = store.create("audit", None);
    let ticker = PollTicker::new(Some((&store, &id)), None);
    let ended = BatchStatus {
        processing_status: "in_progress".to_string(),
        request_counts: BatchRequestCounts::default(),
    };

    store.age_activity_for_test(&id);
    assert!(!ticker.failed());
    assert!(idle_ms(&store, &id) < 5_000, "one failed poll is a blip");
    store.age_activity_for_test(&id);
    assert!(!ticker.failed());
    assert!(idle_ms(&store, &id) < 5_000, "so are two");
    store.age_activity_for_test(&id);
    assert!(ticker.failed(), "the third in a row marks the job stalled");
    assert!(idle_ms(&store, &id) > 60_000, "and it is no longer touched");
    store.age_activity_for_test(&id);
    ticker.ok(&ended);
    assert!(idle_ms(&store, &id) < 5_000, "a good poll touches it again");
    store.age_activity_for_test(&id);
    assert!(!ticker.failed(), "and the streak starts over");
    assert!(idle_ms(&store, &id) < 5_000);
}

// ── A7: every request is size-checked ───────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn every_runner_item_is_checked_against_the_window_before_send() {
    for batch_mode in [false, true] {
        let rt = Realtime::ok_after(Duration::from_secs(1));
        let api = Api::new();
        let mut stage = Scripted::new(&["ok-1", "huge", "ok-2", "needs-retry"]);
        stage.huge.insert("huge".to_string());
        stage.reject_first.insert("needs-retry".to_string());
        stage.huge_retry.insert("needs-retry".to_string());
        let (mode, c) = if batch_mode {
            (StageMode::Batch, ctx(&api.realtime, Some(&api)))
        } else {
            (StageMode::Parallel { max_in_flight: 4 }, ctx(&rt, None))
        };

        let out = run_stage(STAGE, &mut stage, mode, &c).await;

        let label = if batch_mode { "batch" } else { "parallel" };
        let huge = out
            .failed
            .iter()
            .find(|f| f.id == "huge")
            .unwrap_or_else(|| panic!("{label}: the oversized item is a failed part"));
        assert_eq!(huge.class, FailureClass::Content, "{label}");
        assert!(
            huge.reason.contains("does not fit"),
            "{label}: {}",
            huge.reason
        );
        let retry = out
            .failed
            .iter()
            .find(|f| f.id == "needs-retry")
            .unwrap_or_else(|| panic!("{label}: the oversized retry is a failed part"));
        assert!(
            retry.reason.contains("retry request was not sent"),
            "{label}: {}",
            retry.reason
        );
        assert_eq!(
            sorted(out.absorbed.clone()),
            ids(&["ok-1", "ok-2"]),
            "{label}"
        );
        // Nothing oversized reached either port.
        let sent_lens: Vec<usize> = rt.prompt_lens.lock().unwrap().clone();
        assert!(
            sent_lens.iter().all(|n| *n < 1_000),
            "{label}: {sent_lens:?}"
        );
        let sent_ids: Vec<String> = if batch_mode {
            api.submitted_item_ids().concat()
        } else {
            rt.calls().iter().map(|c| c.id.clone()).collect()
        };
        assert!(!sent_ids.contains(&"huge".to_string()), "{label}");
        // needs-retry's FIRST (normal size) request was sent; its huge retry was not.
        assert_eq!(
            sent_ids.iter().filter(|i| *i == "needs-retry").count(),
            1,
            "{label}"
        );
    }
}

// ── per-item progress ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_run_of_n_items_moves_the_stage_progress_from_0_to_n_and_touches_activity_per_item() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    // What the job showed when each request started: its progress and how long it had been idle.
    type Look = (Option<(usize, usize)>, u128);
    let seen: Arc<Mutex<Vec<Look>>> = Arc::new(Mutex::new(Vec::new()));
    let (s2, i2, seen2) = (store.clone(), id.clone(), seen.clone());
    let rt = Realtime::new(move |item, _| {
        let progress = s2
            .get(&i2)
            .unwrap()
            .stage_progress
            .map(|p| (p.done, p.total));
        seen2.lock().unwrap().push((progress, idle_ms(&s2, &i2)));
        // Age the job, so the next look shows whether absorbing an item touched it again.
        s2.age_activity_for_test(&i2);
        (Duration::from_secs(1), Ok(reply_for(&prompt_for(item))))
    });
    let mut stage = Scripted::numbered(6);
    let mut c = ctx(&rt, None);
    c.job = Some((&store, &id));

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 1 },
        &c,
    )
    .await;

    assert_eq!(out.absorbed.len(), 6);
    let seen = seen.lock().unwrap().clone();
    let progress: Vec<Option<(usize, usize)>> = seen.iter().map(|s| s.0).collect();
    assert_eq!(
        progress,
        (0..6).map(|done| Some((done, 6))).collect::<Vec<_>>(),
        "progress moves up by one for each absorbed item"
    );
    for (i, (_, idle)) in seen.iter().enumerate() {
        assert!(
            *idle < 5_000,
            "absorbing item {i} touched the job (idle {idle} ms)"
        );
    }
    assert_eq!(
        store.last_progress_label(&id).as_deref(),
        Some("calibration: 6 of 6 ended"),
        "the last item brought it to N of N"
    );
    assert!(store.get(&id).unwrap().stage_progress.is_none());
}

// ── cancel ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn cancelling_mid_stage_stops_new_requests_within_one_tick_and_keeps_what_was_absorbed() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    let cancelled_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let (s2, i2, at) = (store.clone(), id.clone(), cancelled_at.clone());
    let rt = Realtime::new(move |item, nth| {
        if item == "item-010" && nth == 0 {
            *at.lock().unwrap() = Some(Instant::now());
            s2.cancel(&i2);
        }
        (Duration::from_secs(1), Ok(reply_for(&prompt_for(item))))
    });
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(40);
    let mut c = ctx(&rt, None);
    c.job = Some((&store, &id));
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 2 },
        &c,
    )
    .await;

    let t = cancelled_at
        .lock()
        .unwrap()
        .expect("the script cancelled the job");
    assert!(
        rt.calls().iter().all(|call| call.started <= t),
        "no request was dispatched after the cancel"
    );
    let cancelled = out.cancelled.as_ref().expect("the stage is cancelled");
    assert!(
        out.failed.is_empty(),
        "a cancel is not a failure: {:?}",
        out.failed
    );
    assert!(!out.is_complete());
    assert!(!meter.outage_stopped(), "and not an outage");
    assert!(!cancelled.not_sent.is_empty());
    assert!(cancelled.batches_cancelled.is_empty());
    assert!(
        out.absorbed.len() >= 10,
        "what was in flight still finished"
    );
    assert_eq!(
        out.absorbed.len() + out.resumed.len() + cancelled.not_sent.len(),
        40,
        "every item is absorbed or listed as not sent"
    );
    // Absorbed items survive in the checkpoint...
    assert_eq!(cp.absorbed().len(), out.absorbed.len());

    // ...and a later run picks up from it, paying only for what was never sent.
    let rt2 = Realtime::ok_after(Duration::from_secs(1));
    let mut stage2 = Scripted::numbered(40);
    let mut c2 = ctx(&rt2, None);
    c2.checkpoint = Some(&cp);
    let out2 = run_stage(
        STAGE,
        &mut stage2,
        StageMode::Parallel { max_in_flight: 2 },
        &c2,
    )
    .await;
    assert!(out2.cancelled.is_none() && out2.is_complete());
    assert_eq!(out2.resumed.len(), out.absorbed.len());
    assert_eq!(out2.absorbed.len(), cancelled.not_sent.len());
    assert_eq!(rt2.call_count(), cancelled.not_sent.len());
}

#[tokio::test(start_paused = true)]
async fn a_live_batch_receives_a_cancel_call_and_the_stage_is_cancelled_not_failed() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    let (s2, i2) = (store.clone(), id.clone());
    let api = Api::new()
        .polls_before_end(&[(3, 1), (2, 2), (1, 3)])
        .probing(move |n| {
            if n == 1 {
                s2.cancel(&i2);
            }
        });
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let meter = UsageMeter::default();
    let mut stage = Scripted::new(&["a", "b", "c", "d"]);
    let mut c = ctx(&api.realtime, Some(&api));
    c.job = Some((&store, &id));
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;

    assert_eq!(
        api.polls.load(Ordering::SeqCst),
        2,
        "no poll after the one that saw the cancel"
    );
    assert_eq!(
        *api.cancel_calls.lock().unwrap(),
        vec!["msgbatch_0".to_string()]
    );
    assert!(api.fetched.lock().unwrap().is_empty());
    let cancelled = out.cancelled.as_ref().expect("the stage is cancelled");
    assert_eq!(cancelled.batches_cancelled, vec!["msgbatch_0".to_string()]);
    assert_eq!(
        sorted(cancelled.not_sent.clone()),
        ids(&["a", "b", "c", "d"])
    );
    assert!(out.failed.is_empty(), "not failed: {:?}", out.failed);
    assert!(!meter.outage_stopped());
    let records = cp.batches();
    assert_eq!(records.len(), 1);
    assert!(
        !records[0].settled,
        "the batch stays on record so a resume polls it"
    );

    // A later run polls the cancelled batch and takes what it finished; it submits nothing.
    let api2 = Api::new().knowing(&records[0]);
    let mut stage2 = Scripted::new(&["a", "b", "c", "d"]);
    let mut c2 = ctx(&api2.realtime, Some(&api2));
    c2.checkpoint = Some(&cp);
    let out2 = run_stage(STAGE, &mut stage2, StageMode::Batch, &c2).await;
    assert_eq!(out2.absorbed.len(), 4);
    assert!(out2.cancelled.is_none());
    assert_eq!(api2.submit_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn a_job_cancelled_before_the_stage_starts_submits_and_sends_nothing() {
    for batch_mode in [false, true] {
        let store = Arc::new(JobStore::new());
        let id = store.create("audit", None);
        store.cancel(&id);
        let api = Api::new();
        let rt = Realtime::ok_after(Duration::from_secs(1));
        let mut stage = Scripted::numbered(5);
        let (mode, mut c) = if batch_mode {
            (StageMode::Batch, ctx(&api.realtime, Some(&api)))
        } else {
            (StageMode::Parallel { max_in_flight: 4 }, ctx(&rt, None))
        };
        c.job = Some((&store, &id));

        let out = run_stage(STAGE, &mut stage, mode, &c).await;

        let cancelled = out.cancelled.expect("cancelled");
        assert_eq!(cancelled.not_sent.len(), 5, "batch_mode={batch_mode}");
        assert!(out.absorbed.is_empty() && out.failed.is_empty());
        assert_eq!(api.submit_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rt.call_count(), 0);
        assert_eq!(api.realtime.call_count(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn a_cancel_during_a_rate_limit_wait_ends_the_wait_within_a_tick() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    let (s2, i2) = (store.clone(), id.clone());
    // The submit is rate limited for an hour; the job is cancelled while it waits.
    let api = Api::new().submit_script(move |_| {
        s2.cancel(&i2);
        Some(rate_limited(3600))
    });
    let mut stage = Scripted::new(&["a", "b"]);
    let mut c = ctx(&api.realtime, Some(&api));
    c.job = Some((&store, &id));
    let t0 = Instant::now();

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;

    assert!(Instant::now().duration_since(t0) < Duration::from_secs(10));
    assert_eq!(
        api.submit_calls.load(Ordering::SeqCst),
        1,
        "it did not submit again"
    );
    assert!(out.cancelled.is_some());
    assert!(out.failed.is_empty());
}

// ── overload is a pause (Fable wave 3) ──────────────────────────────────────────────────────────

/// What the transport returns for a 529 `overloaded_error` that outlasted its retries, classified
/// by the transport's own rules (no retry-after: the provider sent none).
fn overloaded() -> anyhow::Error {
    let (class, _) =
        camerata_llm::failure::classify_http(529, Some("overloaded_error"), "Overloaded");
    let mut rec = AttemptRecord::new("api", MODEL);
    rec.http_status = Some(529);
    rec.error_type = Some("overloaded_error".to_string());
    let mut f = LlmCallFailure::new(class, "Anthropic API HTTP 529: overloaded");
    f.attempts = vec![rec];
    f.retryable = true;
    anyhow::Error::new(f)
}

#[tokio::test(start_paused = true)]
async fn three_overloads_in_a_row_pause_the_stage_and_it_recovers_without_an_outage_stop() {
    // The first three calls are overloaded; with no retry-after the pause is the 30 second default.
    let counter = Arc::new(AtomicUsize::new(0));
    let seen = counter.clone();
    let rt = Realtime::new(move |id, _| {
        if seen.fetch_add(1, Ordering::SeqCst) < 3 {
            (Duration::from_secs(1), Err(overloaded()))
        } else {
            (Duration::from_secs(1), Ok(reply_for(&prompt_for(id))))
        }
    });
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(10);
    let mut c = ctx(&rt, None);
    c.meter = Some(&meter);
    let t0 = Instant::now();

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;

    assert!(!meter.outage_stopped(), "three overloads are not an outage");
    assert_eq!(
        out.absorbed.len(),
        10,
        "every item was sent again and answered"
    );
    assert!(out.failed.is_empty());
    assert!(out.pacing.throttles >= 1, "it halved: {:?}", out.pacing);
    assert!(
        Instant::now().duration_since(t0) >= Duration::from_secs(30),
        "and held for the default pause"
    );
}

#[tokio::test(start_paused = true)]
async fn three_connection_failures_in_a_row_still_trip_the_outage_stop() {
    let rt = Realtime::new(|_, _| {
        (
            Duration::from_secs(1),
            Err(anyhow::Error::new(LlmCallFailure::new(
                FailureClass::Transport,
                "Anthropic API request failed after 3 retries: connection reset by peer",
            ))),
        )
    });
    let meter = UsageMeter::default();
    let mut stage = Scripted::numbered(10);
    let mut c = ctx(&rt, None);
    c.meter = Some(&meter);

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 1 },
        &c,
    )
    .await;

    assert!(meter.outage_stopped(), "connection failures are an outage");
    assert!(out.absorbed.is_empty());
    assert_eq!(out.failed.len(), 10);
    assert!(out
        .failed
        .iter()
        .any(|f| f.class == FailureClass::OutageStop));
}

// ── a paused stage is alive ─────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_long_retry_after_keeps_touching_the_job_and_labels_it_paused() {
    let store = Arc::new(JobStore::new());
    let id = store.create("audit", None);
    // item-000 is rate limited for 60 seconds on its first call. The job is aged when that call
    // starts; what the retry call finds is what the 60 second hold did to it.
    type Look = (u128, Option<String>);
    let looks: Arc<Mutex<Vec<Look>>> = Arc::new(Mutex::new(Vec::new()));
    let (s2, i2, l2) = (store.clone(), id.clone(), looks.clone());
    let rt = Realtime::new(move |item, nth| {
        if nth == 1 {
            l2.lock()
                .unwrap()
                .push((idle_ms(&s2, &i2), s2.last_progress_label(&i2)));
        }
        if item == "item-000" && nth == 0 {
            s2.age_activity_for_test(&i2);
            (Duration::from_secs(1), Err(rate_limited(60)))
        } else {
            (Duration::from_secs(1), Ok(reply_for(&prompt_for(item))))
        }
    });
    let mut stage = Scripted::numbered(1);
    let mut c = ctx(&rt, None);
    c.job = Some((&store, &id));

    let out = run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 1 },
        &c,
    )
    .await;

    assert_eq!(out.absorbed.len(), 1);
    let looks = looks.lock().unwrap().clone();
    assert_eq!(looks.len(), 1, "the item was sent again once");
    let (idle, label) = &looks[0];
    assert!(
        *idle < 5_000,
        "the hold kept touching the job (idle {idle} ms)"
    );
    assert_eq!(label.as_deref(), Some("paused: rate limited"));
}

// ── WO7b F1: a batch that is gone for good is settled and its members sent once more ────────────

/// A batch the first process submitted whose results could not be fetched, saved under
/// `gone_id` so a stub can answer for it by name. Returns the checkpoint's single record.
async fn save_unsettled_batch(cp: &StageCheckpoint, all: &[&str], gone_id: &str) -> BatchRecord {
    let first = Api::new().failing_fetch();
    let mut stage = Scripted::new(all);
    let mut c = ctx(&first.realtime, Some(&first));
    c.checkpoint = Some(cp);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert!(
        !out.failed.is_empty(),
        "the unabsorbed members wait on the batch"
    );
    let mut records = cp.batches();
    assert_eq!(records.len(), 1);
    assert!(!records[0].settled);
    records[0].batch_id = gone_id.to_string();
    cp.save_batches(&records).unwrap();
    records.remove(0)
}

fn cap_meter() -> UsageMeter {
    UsageMeter::with_controls(crate::cost_cap::RunControls {
        review: Default::default(),
        efficiency: Default::default(),
        max_cost_usd: Some(1000.0),
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    })
}

#[tokio::test(start_paused = true)]
async fn a_saved_batch_the_provider_no_longer_has_is_settled_as_gone_and_its_members_sent_once_more(
) {
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let all = ["a1", "a2", "a3", "a4", "a5", "a6"];

    // Process 1: a1 and a2 are absorbed in real time.
    let rt = Realtime::new(|id, _| {
        if id == "a1" || id == "a2" {
            (Duration::from_secs(1), Ok(reply_for(&prompt_for(id))))
        } else {
            (Duration::from_secs(1), Err(content_error("boom")))
        }
    });
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&rt, None);
    c.checkpoint = Some(&cp);
    run_stage(
        STAGE,
        &mut stage,
        StageMode::Parallel { max_in_flight: 4 },
        &c,
    )
    .await;
    // Process 2: the other four go out as a batch nobody can fetch; the id is then saved as one
    // the provider has since dropped.
    let record = save_unsettled_batch(&cp, &all, "msgbatch_gone").await;
    assert_eq!(record.items.len(), 4);

    // Process 3: the provider answers 404 for the saved id. It is settled as gone, its four
    // unsettled members are submitted ONCE as a new batch, and the run completes.
    let api = Api::new().breaking("msgbatch_gone", 404);
    let meter = cap_meter();
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);
    c.settings.per_item_usd = Some(0.001);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert!(out.is_complete(), "failed: {:?}", failed_ids(&out));
    assert_eq!(sorted(out.resumed.clone()), ids(&["a1", "a2"]));
    assert_eq!(sorted(out.absorbed.clone()), ids(&["a3", "a4", "a5", "a6"]));
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 1, "submitted once");
    assert_eq!(
        sorted(api.submitted_item_ids()[0].clone()),
        ids(&["a3", "a4", "a5", "a6"]),
        "exactly the unsettled members"
    );
    assert_eq!(api.realtime.call_count(), 0);
    let records = cp.batches();
    assert!(
        records.iter().all(|r| r.settled),
        "the gone batch is settled, not left live: {records:?}"
    );
    assert!(out.failed.iter().all(|f| f.live_batch.is_none()));
    let notes = meter.cost_cap_record().unwrap().notes.join("\n");
    assert!(
        notes.contains("msgbatch_gone") && notes.contains("no longer exists"),
        "the gone batch is disclosed: {notes}"
    );

    // Process 4: nothing is left to do, and the gone id is never asked about again.
    let api = Api::new().breaking("msgbatch_gone", 404);
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(out.resumed.len(), 6);
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(api.polls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn a_transient_fetch_failure_on_resume_keeps_the_batch_live_and_resubmits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let all = ["a1", "a2", "a3"];
    save_unsettled_batch(&cp, &all, "msgbatch_busy").await;

    let api = Api::new().breaking("msgbatch_busy", 503);
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(out.failed.len(), 3);
    assert!(out
        .failed
        .iter()
        .all(|f| f.live_batch.as_deref() == Some("msgbatch_busy")));
    assert_eq!(
        api.submit_calls.load(Ordering::SeqCst),
        0,
        "not resubmitted"
    );
    assert!(
        !cp.batches()[0].settled,
        "still live: a later resume polls it"
    );
}

// ── WO7b F2: a kill between the provider accepting a batch and its id being saved ───────────────

#[tokio::test(start_paused = true)]
async fn a_kill_after_the_provider_accepts_a_batch_and_before_its_id_is_saved_is_disclosed_and_resubmitted(
) {
    use futures::FutureExt as _;
    let dir = tempfile::tempdir().unwrap();
    let cp = StageCheckpoint::new(dir.path(), "calibration");
    let all = ["a1", "a2", "a3"];

    // Process 1 is killed with the provider holding a batch whose id was never saved.
    let api = Api::new().killed_after_accepting();
    let meter = cap_meter();
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);
    c.settings.per_item_usd = Some(0.001);
    let died = std::panic::AssertUnwindSafe(run_stage(STAGE, &mut stage, StageMode::Batch, &c))
        .catch_unwind()
        .await;
    assert!(died.is_err(), "the stub killed the process");
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 1);
    assert!(
        cp.batches().is_empty(),
        "the batch id never reached the checkpoint"
    );

    // Process 2 finds the intent with no saved batch: it says a batch may have been submitted and
    // orphaned, then sends the items once.
    let api = Api::new();
    let meter = cap_meter();
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);
    c.settings.per_item_usd = Some(0.001);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert!(out.is_complete(), "failed: {:?}", failed_ids(&out));
    assert_eq!(sorted(out.absorbed.clone()), ids(&all));
    assert_eq!(
        api.submit_calls.load(Ordering::SeqCst),
        1,
        "one resubmission"
    );
    assert_eq!(sorted(api.submitted_item_ids()[0].clone()), ids(&all));
    let notes = meter.cost_cap_record().unwrap().notes;
    let orphan_lines = notes
        .iter()
        .filter(|n| n.contains("may have been submitted") && n.contains("orphaned"))
        .count();
    assert_eq!(
        orphan_lines, 1,
        "the possible orphan is on the ledger once: {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("3 item")),
        "the line says how many items it covered: {notes:?}"
    );

    // Process 3: the batch was saved and settled this time; the old intent is not reported again.
    let api = Api::new();
    let meter = cap_meter();
    let mut stage = Scripted::new(&all);
    let mut c = ctx(&api.realtime, Some(&api));
    c.checkpoint = Some(&cp);
    c.meter = Some(&meter);
    c.settings.per_item_usd = Some(0.001);
    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;
    assert_eq!(out.resumed.len(), 3);
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 0);
    assert!(meter.cost_cap_record().unwrap().notes.is_empty());
}

// ── WO7b F11: a window is admitted on the bytes of its real requests ────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_window_of_large_requests_is_refused_at_admission_though_the_old_constant_would_admit_it()
{
    // Ten requests of about 300 KB each: roughly 100,000 estimated tokens a request, about $0.15
    // a request at the batch price of the stage model, $1.50 for the window. The limit is $1.00.
    let mut stage = Scripted::numbered(10);
    stage.padding = 300_000;
    let api = Api::new();
    let meter = UsageMeter::with_controls(crate::cost_cap::RunControls {
        review: Default::default(),
        efficiency: Default::default(),
        max_cost_usd: Some(1.0),
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    });
    let mut c = ctx(&api.realtime, Some(&api));
    c.meter = Some(&meter);
    // The projection stage 1 used before: 10,000 bytes a site, priced as the stage model.
    let constant = crate::llm::input_cost_usd(
        MODEL,
        10_000u64.div_ceil(3),
        crate::llm::PriceBasis::batch(crate::llm::BATCH_CACHE_TTL),
    )
    .expect("the stage model is priced");
    c.settings.per_item_usd = Some(constant);
    assert!(
        meter.batch_refusal_reason(Some(constant * 10.0)).is_none(),
        "the constant projection admits this window"
    );

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;

    assert_eq!(
        api.submit_calls.load(Ordering::SeqCst),
        0,
        "nothing was submitted"
    );
    assert!(out.absorbed.is_empty());
    assert_eq!(out.failed.len(), 10, "every item is a failed part");
    assert!(out
        .failed
        .iter()
        .all(|f| f.class == FailureClass::SpendLimit));
    let record = meter.cost_cap_record().unwrap();
    assert!(
        record.notes.iter().any(|n| n.contains("NOT submitted")),
        "{:?}",
        record.notes
    );
}

#[tokio::test(start_paused = true)]
async fn a_window_of_small_requests_under_the_limit_is_still_admitted() {
    let mut stage = Scripted::numbered(10);
    let api = Api::new();
    let meter = UsageMeter::with_controls(crate::cost_cap::RunControls {
        review: Default::default(),
        efficiency: Default::default(),
        max_cost_usd: Some(1.0),
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    });
    let mut c = ctx(&api.realtime, Some(&api));
    c.meter = Some(&meter);
    // No pre-flight hint at all: the real request sizes are the projection.
    c.settings.per_item_usd = None;

    let out = run_stage(STAGE, &mut stage, StageMode::Batch, &c).await;

    assert!(out.is_complete(), "failed: {:?}", failed_ids(&out));
    assert_eq!(api.submit_calls.load(Ordering::SeqCst), 1);
}
