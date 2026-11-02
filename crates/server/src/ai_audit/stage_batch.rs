//! The shared batch runner for every post-scan stage (design 2026-11-02, section 6; M2).
//!
//! Stage 1 and stage 2 calibration, detail regeneration, fix generation and alternatives all do
//! the same thing: build one model request per item, send them all, absorb each reply into the
//! stage's own result, and account for every item that did not come back. This module is that
//! loop, written once. A stage implements [`StageItems`] (its requests, how it absorbs a reply,
//! how it re-asks after a reply it turned down) and calls [`run_stage`].
//!
//! * [`StageMode::Batch`] generalises [`super::batch_mode::submit_round`]: the items are split by
//!   request-body bytes and item count ([`super::batch_mode::plan_item_ranges`]), each
//!   sub-batch is gated by the spend limit and the outage stop before it is submitted, then
//!   polled and fetched. Every poll tick touches the job and sets the stage's progress.
//! * [`StageMode::Parallel`] is the fallback where there is no batch API (the CLI backend, a
//!   run forced to real time): a bounded number of requests in flight, with additive-increase,
//!   multiplicative-decrease pacing. A 429 or an overloaded answer halves the permits (never
//!   below one) and holds every new request for the provider's `retry-after`; fifty successes
//!   give one permit back, up to the configured maximum (never above [`MAX_IN_FLIGHT_CAP`]).
//!   A rate-limited item is sent again, not failed: only when the stage has been rate limited
//!   with no success for the whole [`StageSettings::rate_limit_deadline`] (six hours by default)
//!   does it become a failed part.
//!
//! RUN TO COMPLETION: every item ends absorbed, resumed (absorbed by an earlier process) or in
//! [`StageOutcome::failed`] with its class and reason. Nothing is skipped silently. An item that
//! failed in a way sending it again can fix (a transport failure, a row the batch left out) is
//! sent once more; a reply the stage turned down is re-asked once through [`StageItems::retry`].
//!
//! EVERY REQUEST IS SIZE-CHECKED (design amendment A7): each item, and each retry request, passes
//! [`super::request_budget::ensure_request_fits`] before anything is submitted or sent. One that
//! does not fit is recorded as a failed part with the reason; it is never sent and never cut. A
//! stage that can split an oversized item does so in [`StageItems::items`].
//!
//! RESUME (design 6.2): with a [`StageCheckpoint`] the runner appends every absorbed item (its
//! usage and reply) and every submitted batch as it goes. On the next run an absorbed item is
//! not sent again (its usage is written to the meter so the ledger is never a zero-token stage,
//! and its reply is handed to [`StageItems::absorb`] unless the stage reports it already holds
//! the item); a batch that has not settled is polled and fetched by id, never submitted again;
//! only an item with no result and no live batch is sent.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use camerata_llm::failure::{self, AttemptRecord, FailureClass};
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::FutureExt;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use super::batch_mode::{
    batch_cache_ttl, batch_poll_interval, disclosable_error, max_batch_body_bytes, note_batch_call,
    plan_item_ranges, poll_and_fetch, BatchApi, JobCancelled, PollProgress, RoundContext,
};
use super::request_budget::ensure_request_fits;
use super::*;
use crate::checkpoint::{AbsorbedLine, BatchRecord, StageCheckpoint};
use crate::jobs::JobStore;
use crate::llm::{BatchItem, BatchResultRow, CacheTtl, LlmPort, LlmRequest, LlmResponse};

/// Requests in flight at once in the parallel fallback, on the API backend.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 8;

/// Requests in flight at once on the CLI backend, which spawns a process per request.
pub const CLI_MAX_IN_FLIGHT: usize = 2;

/// The most requests the parallel fallback ever has in flight, whatever it is asked for.
pub const MAX_IN_FLIGHT_CAP: usize = 16;

/// The fewest requests in flight: a halving never goes below this.
pub const MIN_IN_FLIGHT: usize = 1;

/// Successes in a row (since the last throttle) that give one permit back.
pub const SUCCESSES_PER_PERMIT: u32 = 50;

/// How long every new request is held after a 429 or an overload that came with no `retry-after`.
pub const DEFAULT_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(30);

/// How long a stage may be rate limited, with no success at all in between, before the items it
/// could not send become failed parts: six hours.
pub const DEFAULT_RATE_LIMIT_DEADLINE: Duration = Duration::from_secs(6 * 60 * 60);

/// Why a stage turned down a model reply (a fix that named something not in the evidence, a
/// verdict for the wrong site, an unparseable body). The reason is kept on the failed part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemReject {
    pub reason: String,
}

impl ItemReject {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for ItemReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// What a post-scan stage hands the runner. Items are keyed by [`Self::Id`]; its `Display` is the
/// stable key the checkpoint files and the batch mapping use (a site key, a rule-group id).
pub trait StageItems {
    type Id: Clone + Ord + std::fmt::Display + Send + Sync + 'static;

    /// Every request the stage needs, one per id. Ids must be unique: a repeated id is recorded
    /// as a failed part.
    fn items(&self) -> Vec<(Self::Id, LlmRequest)>;

    /// Take the model's reply for `id`. `Err` turns the reply down (the runner asks
    /// [`Self::retry`] once). An `Ok` is final: the item is absorbed and never paid for again.
    fn absorb(&mut self, id: &Self::Id, resp: LlmResponse) -> Result<(), ItemReject>;

    /// A replacement request after `reject`, or `None` to give the item up as a failed part. At
    /// most one retry per item (M4): a second rejection is final.
    fn retry(&self, id: &Self::Id, reject: &ItemReject) -> Option<LlmRequest>;

    /// True when the stage already holds `id`'s result from before a restart (it persisted its
    /// own state). On resume the runner then skips the item without handing its stored reply to
    /// [`Self::absorb`] a second time. The default, `false`, replays the stored reply.
    fn already_absorbed(&self, _id: &Self::Id) -> bool {
        false
    }
}

/// How the runner sends a stage's requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageMode {
    /// Message Batches: half price, answers in minutes to hours. Needs [`StageCtx::batch`]; a
    /// run without it falls back to [`Self::parallel_default`] and says so on the meter.
    Batch,
    /// Real-time requests, at most `max_in_flight` at once (clamped to
    /// [`MIN_IN_FLIGHT`]..=[`MAX_IN_FLIGHT_CAP`]).
    Parallel { max_in_flight: usize },
}

impl StageMode {
    /// The parallel fallback with its default width: [`DEFAULT_MAX_IN_FLIGHT`] on the API,
    /// [`CLI_MAX_IN_FLIGHT`] on the CLI backend.
    pub fn parallel_default(cli_backend: bool) -> Self {
        Self::Parallel {
            max_in_flight: if cli_backend {
                CLI_MAX_IN_FLIGHT
            } else {
                DEFAULT_MAX_IN_FLIGHT
            },
        }
    }
}

/// The tunables of one run. The defaults are the named constants above.
#[derive(Debug, Clone)]
pub struct StageSettings {
    /// Time between batch polls (`CAMERATA_BATCH_POLL_SECS`, 10s by default).
    pub poll_interval: Duration,
    /// See [`DEFAULT_RATE_LIMIT_DEADLINE`].
    pub rate_limit_deadline: Duration,
    /// See [`DEFAULT_RATE_LIMIT_PAUSE`].
    pub rate_limit_pause: Duration,
    /// The most request-body bytes one batch submission carries.
    pub max_body_bytes: usize,
    /// The projected input dollars of one item, for the spend-limit gate on a batch.
    pub per_item_usd: Option<f64>,
}

impl Default for StageSettings {
    fn default() -> Self {
        Self {
            poll_interval: batch_poll_interval(),
            rate_limit_deadline: DEFAULT_RATE_LIMIT_DEADLINE,
            rate_limit_pause: DEFAULT_RATE_LIMIT_PAUSE,
            max_body_bytes: max_batch_body_bytes(),
            per_item_usd: None,
        }
    }
}

/// Everything a run needs besides the stage's items.
pub struct StageCtx<'a> {
    /// The repo the stage runs on, for the not-run records the spend cap and the outage stop make.
    pub repo: &'a str,
    /// The model the stage's requests name; batch results are priced as this when the API echoes
    /// an id the registry does not price.
    pub model: &'a str,
    /// The real-time port: the parallel fallback sends through it.
    pub realtime: &'a dyn LlmPort,
    /// The Message Batches API; `None` where there is none (the CLI backend).
    pub batch: Option<&'a dyn BatchApi>,
    pub meter: Option<&'a UsageMeter>,
    pub job: Option<(&'a JobStore, &'a str)>,
    /// Where to keep resume state; `None` runs without it.
    pub checkpoint: Option<&'a StageCheckpoint>,
    pub settings: StageSettings,
}

/// An item that did not come back, with everything the failed-part report needs.
#[derive(Debug, Clone)]
pub struct FailedItem<Id> {
    pub id: Id,
    /// What kind of failure it was, which decides what the run does about it.
    pub class: FailureClass,
    /// A plain sentence, scrubbed and capped; never a response body.
    pub reason: String,
    /// What the transport said on each attempt, oldest first.
    pub attempts: Vec<AttemptRecord>,
    /// The batch that carries this item when it was accepted but its results could not be
    /// retrieved. The item was NOT sent again (it may still be running and billed); a resume
    /// polls this batch.
    pub live_batch: Option<String>,
}

/// How the parallel fallback paced itself, for the log and for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacingReport {
    /// The permits the run started with.
    pub start_limit: usize,
    /// The fewest permits it ever had.
    pub min_limit: usize,
    /// The permits it ended with.
    pub final_limit: usize,
    /// Times the permits were halved.
    pub throttles: u32,
}

/// What a run came to. Every item the stage listed is in exactly one of the three lists.
#[derive(Debug, Clone)]
pub struct StageOutcome<Id> {
    /// Items absorbed by this run, in the order they were absorbed.
    pub absorbed: Vec<Id>,
    /// Items an earlier process had absorbed; skipped, not paid for again.
    pub resumed: Vec<Id>,
    /// Items with no result, each with its reason.
    pub failed: Vec<FailedItem<Id>>,
    pub pacing: PacingReport,
    /// Set when the job was cancelled before the stage finished. A cancelled stage is neither
    /// failed nor an outage: its unsent items are listed here and are sent by a later run.
    pub cancelled: Option<StageCancelled<Id>>,
}

/// What a cancel left undone.
#[derive(Debug, Clone)]
pub struct StageCancelled<Id> {
    /// Items that were neither absorbed nor failed: never sent, or sent in a batch that was
    /// cancelled. A resume sends them (a cancelled batch is polled first, so what it finished
    /// is absorbed and not paid for twice).
    pub not_sent: Vec<Id>,
    /// Provider batches the runner asked to cancel.
    pub batches_cancelled: Vec<String>,
}

impl<Id> StageOutcome<Id> {
    /// True when every item is absorbed or resumed and the stage was not cancelled: the stage
    /// has nothing left to report.
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty() && self.cancelled.is_none()
    }
}

/// One item waiting to be sent.
struct Work<Id> {
    id: Id,
    req: Arc<LlmRequest>,
}

impl<Id: Clone> Clone for Work<Id> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            req: self.req.clone(),
        }
    }
}

/// The ledger of one run: which item ended how.
struct Book<Id> {
    total: usize,
    absorbed: Vec<Id>,
    resumed: Vec<Id>,
    failed: BTreeMap<Id, FailedItem<Id>>,
    done: BTreeSet<Id>,
    /// Items whose reply was turned down once already (the retry is spent).
    retried: BTreeSet<Id>,
    /// Items sent a second time after a failure sending can fix (the resend is spent).
    resent: BTreeSet<Id>,
    /// Attempt records gathered for an item so far.
    trails: BTreeMap<Id, Vec<AttemptRecord>>,
    pacing: PacingReport,
    /// The job was cancelled: nothing more is dispatched.
    cancelled: bool,
    /// Provider batches the runner asked to cancel.
    batches_cancelled: Vec<String>,
    /// Items neither absorbed nor failed when the stage ended cancelled.
    not_sent: Vec<Id>,
}

/// The most attempt records one failed part keeps (a long rate limit would otherwise grow it for
/// hours): the first few and the last ones.
const MAX_TRAIL: usize = 12;

impl<Id: Clone + Ord> Book<Id> {
    fn new() -> Self {
        Self {
            total: 0,
            absorbed: Vec::new(),
            resumed: Vec::new(),
            failed: BTreeMap::new(),
            done: BTreeSet::new(),
            retried: BTreeSet::new(),
            resent: BTreeSet::new(),
            trails: BTreeMap::new(),
            pacing: PacingReport::default(),
            cancelled: false,
            batches_cancelled: Vec::new(),
            not_sent: Vec::new(),
        }
    }

    fn finished(&self) -> usize {
        self.done.len()
    }

    fn note_attempts(&mut self, id: &Id, attempts: Vec<AttemptRecord>) {
        let trail = self.trails.entry(id.clone()).or_default();
        trail.extend(attempts);
        if trail.len() > MAX_TRAIL {
            let drop = trail.len() - MAX_TRAIL;
            trail.drain(MAX_TRAIL / 3..MAX_TRAIL / 3 + drop);
        }
    }

    fn fail(&mut self, id: &Id, class: FailureClass, reason: &str, live_batch: Option<String>) {
        let attempts = self.trails.remove(id).unwrap_or_default();
        self.done.insert(id.clone());
        self.failed.insert(
            id.clone(),
            FailedItem {
                id: id.clone(),
                class,
                reason: failure::scrub_message(
                    &disclosable_error(reason),
                    failure::MAX_RECORD_MESSAGE_CHARS,
                ),
                attempts,
                live_batch,
            },
        );
    }

    fn into_outcome(self) -> StageOutcome<Id> {
        StageOutcome {
            absorbed: self.absorbed,
            resumed: self.resumed,
            failed: self.failed.into_values().collect(),
            pacing: self.pacing,
            cancelled: self.cancelled.then_some(StageCancelled {
                not_sent: self.not_sent,
                batches_cancelled: self.batches_cancelled,
            }),
        }
    }
}

/// Run one stage to completion. See the module docs.
///
/// `stage` names the usage row the calls are recorded under and the progress line on the job.
pub async fn run_stage<S>(
    stage: Stage,
    s: &mut S,
    mode: StageMode,
    ctx: &StageCtx<'_>,
) -> StageOutcome<S::Id>
where
    S: StageItems + Send,
{
    let mut book: Book<S::Id> = Book::new();
    let items = s.items();
    book.total = items.len();
    let mut by_key: BTreeMap<String, S::Id> = BTreeMap::new();
    let mut reqs: BTreeMap<S::Id, Arc<LlmRequest>> = BTreeMap::new();
    for (id, req) in items {
        if reqs.contains_key(&id) {
            book.fail(
                &id,
                FailureClass::Content,
                "the stage listed this item id twice, so the second request was not sent",
                None,
            );
            continue;
        }
        by_key.insert(id.to_string(), id.clone());
        reqs.insert(id, Arc::new(req));
    }

    replay_checkpoint(stage, s, ctx, &mut book, &by_key);

    // A7: no request is submitted or sent without passing the size check.
    for (id, req) in &reqs {
        if book.done.contains(id) {
            continue;
        }
        if let Err(too_large) = ensure_request_fits(req) {
            book.fail(id, FailureClass::Content, &too_large.to_string(), None);
        }
    }
    report_progress(ctx, stage, &book);

    // A job cancelled before the stage starts sends nothing. What an earlier process absorbed
    // was replayed above and stays absorbed.
    if cancel_requested(ctx) {
        book.cancelled = true;
    }
    let mut follow: BTreeMap<S::Id, Work<S::Id>> = BTreeMap::new();
    if !book.cancelled && ctx.batch.is_some() {
        follow = resume_batches(stage, s, ctx, &mut book, &by_key, &reqs).await;
    }
    let pending: Vec<Work<S::Id>> = reqs
        .iter()
        .filter(|(id, _)| !book.done.contains(*id))
        .map(|(id, req)| {
            follow.remove(id).unwrap_or_else(|| Work {
                id: id.clone(),
                req: req.clone(),
            })
        })
        .collect();

    if !pending.is_empty() && !book.cancelled {
        match (mode, ctx.batch) {
            (StageMode::Batch, Some(api)) => {
                run_batch(stage, s, ctx, api, &mut book, pending).await;
            }
            (StageMode::Batch, None) => {
                let fallback = StageMode::parallel_default(true);
                if let Some(m) = ctx.meter {
                    m.record_note(format!(
                        "The {} stage was asked to run as a batch but there is no batch API on \
                         this backend, so it ran in real time instead ({fallback:?}).",
                        stage.label()
                    ));
                }
                let StageMode::Parallel { max_in_flight } = fallback else {
                    unreachable!("parallel_default is Parallel")
                };
                run_parallel(stage, s, ctx, &mut book, pending, max_in_flight).await;
            }
            (StageMode::Parallel { max_in_flight }, _) => {
                run_parallel(stage, s, ctx, &mut book, pending, max_in_flight).await;
            }
        }
    }
    if book.cancelled {
        book.not_sent = reqs
            .keys()
            .filter(|id| !book.done.contains(*id))
            .cloned()
            .collect();
    }
    if let Some((jobs, id)) = ctx.job {
        jobs.clear_stage_progress(id);
    }
    book.into_outcome()
}

// ── job liveness ────────────────────────────────────────────────────────────────────────────────

/// Touch the job and set the stage's progress: called whenever an item ends.
fn report_progress<Id: Clone + Ord>(ctx: &StageCtx<'_>, stage: Stage, book: &Book<Id>) {
    if let Some((jobs, id)) = ctx.job {
        jobs.touch_activity(id);
        jobs.set_stage_progress(id, stage, book.finished(), book.total);
    }
}

// ── cancel ──────────────────────────────────────────────────────────────────────────────────────

/// True once the job this stage runs for has been cancelled. A run with no job is never cancelled.
fn cancel_requested(ctx: &StageCtx<'_>) -> bool {
    ctx.job
        .is_some_and(|(jobs, id)| jobs.is_cancel_requested(id))
}

/// Wait `dur`, looking at the cancel flag at least once per poll interval. Returns `true` as soon
/// as the job is cancelled, so a long `retry-after` never holds a cancelled job.
async fn sleep_or_cancel(ctx: &StageCtx<'_>, dur: Duration) -> bool {
    let end = Instant::now() + dur;
    loop {
        if cancel_requested(ctx) {
            return true;
        }
        let now = Instant::now();
        if now >= end {
            return false;
        }
        tokio::time::sleep((end - now).min(ctx.settings.poll_interval)).await;
    }
}

/// Ask the provider to cancel a batch the job no longer wants, so it stops spending. The batch
/// stays on the checkpoint's list unsettled: whatever it finished before the cancel is fetched
/// and absorbed by the next run, never paid for twice.
async fn cancel_live_batch<Id>(
    ctx: &StageCtx<'_>,
    api: &dyn BatchApi,
    book: &mut Book<Id>,
    batch_id: &str,
) {
    book.cancelled = true;
    book.batches_cancelled.push(batch_id.to_string());
    if let Err(e) = api.cancel_batch(batch_id).await {
        let note = format!(
            "The job was cancelled but batch {batch_id} could not be cancelled at the provider \
             ({}); it may still run and be billed. Cancel it in the Anthropic console.",
            disclosable_error(&format!("{e:#}"))
        );
        eprintln!("[camerata-server] {note}");
        if let Some(m) = ctx.meter {
            m.record_note(note);
        }
    }
}

// ── checkpoint ──────────────────────────────────────────────────────────────────────────────────

fn custom_id_for(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("s{hex}")
}

fn ttl_name(ttl: CacheTtl) -> &'static str {
    match ttl {
        CacheTtl::FiveMinutes => "5m",
        CacheTtl::OneHour => "1h",
    }
}

fn ttl_from_name(name: &str) -> CacheTtl {
    if name == "5m" {
        CacheTtl::FiveMinutes
    } else {
        CacheTtl::OneHour
    }
}

fn line_for(key: &str, resp: &LlmResponse) -> AbsorbedLine {
    AbsorbedLine {
        key: key.to_string(),
        model: resp.model.clone(),
        backend: resp.backend.clone(),
        cost_usd: resp.cost_usd,
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
        cache_read_input_tokens: resp.cache_read_input_tokens,
        cache_creation_input_tokens: resp.cache_creation_input_tokens,
        text: resp.text.clone(),
    }
}

fn response_from(line: &AbsorbedLine) -> LlmResponse {
    LlmResponse {
        text: line.text.clone(),
        model: line.model.clone(),
        backend: line.backend.clone(),
        cost_usd: line.cost_usd,
        input_tokens: line.input_tokens,
        output_tokens: line.output_tokens,
        cache_read_input_tokens: line.cache_read_input_tokens,
        cache_creation_input_tokens: line.cache_creation_input_tokens,
        or_cache_discount: None,
        truncated: false,
    }
}

/// A reply's usage without its text, for marking a turned-down reply as wasted spend.
fn usage_only(resp: &LlmResponse) -> LlmResponse {
    LlmResponse {
        text: String::new(),
        model: resp.model.clone(),
        backend: resp.backend.clone(),
        cost_usd: resp.cost_usd,
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
        cache_read_input_tokens: resp.cache_read_input_tokens,
        cache_creation_input_tokens: resp.cache_creation_input_tokens,
        or_cache_discount: resp.or_cache_discount,
        truncated: resp.truncated,
    }
}

fn checkpoint_trouble(ctx: &StageCtx<'_>, what: &str, err: &std::io::Error) {
    let note = format!(
        "A stage checkpoint could not be written ({what}: {err}); a resume of this stage will send \
         the items it covers again."
    );
    eprintln!("[camerata-server] {note}");
    if let Some(m) = ctx.meter {
        m.record_note(note);
    }
}

/// Resume, first half: every item the checkpoint lists as absorbed is skipped. Its usage goes to
/// the meter (this process's meter starts empty, so without it a resumed stage reads as zero
/// tokens), and its reply goes to the stage unless the stage already holds it. A reply the stage
/// now turns down is not trusted: the item is sent again.
fn replay_checkpoint<S: StageItems>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    book: &mut Book<S::Id>,
    by_key: &BTreeMap<String, S::Id>,
) {
    let Some(cp) = ctx.checkpoint else { return };
    for line in cp.absorbed() {
        let Some(id) = by_key.get(&line.key) else {
            continue;
        };
        if book.done.contains(id) {
            continue;
        }
        let resp = response_from(&line);
        if let Some(m) = ctx.meter {
            m.record_stage(stage, &resp);
        }
        if s.already_absorbed(id) || s.absorb(id, resp).is_ok() {
            book.done.insert(id.clone());
            book.resumed.push(id.clone());
        } else {
            eprintln!(
                "[camerata-server] stage {}: the stored reply for {id} was turned down on resume; \
                 sending the item again",
                stage.label()
            );
        }
    }
}

// ── one reply ───────────────────────────────────────────────────────────────────────────────────

enum Settled<Id> {
    Absorbed,
    /// The reply was turned down and the stage supplied a replacement request.
    Retry(Work<Id>),
    Failed,
}

/// Hand a reply to the stage. Usage was already recorded on the meter by the caller.
fn settle_reply<S: StageItems>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    book: &mut Book<S::Id>,
    work: &Work<S::Id>,
    resp: LlmResponse,
) -> Settled<S::Id> {
    let id = &work.id;
    let wasted = usage_only(&resp);
    let line = ctx.checkpoint.map(|_| line_for(&id.to_string(), &resp));
    match s.absorb(id, resp) {
        Ok(()) => {
            if let (Some(cp), Some(line)) = (ctx.checkpoint, line) {
                if let Err(e) = cp.record_absorbed(&line) {
                    checkpoint_trouble(ctx, "absorbed item", &e);
                }
            }
            book.done.insert(id.clone());
            book.absorbed.push(id.clone());
            report_progress(ctx, stage, book);
            Settled::Absorbed
        }
        Err(reject) => {
            if let Some(m) = ctx.meter {
                m.record_wasted(stage, &wasted);
            }
            let reason = format!("the reply was turned down: {}", reject.reason);
            if book.retried.contains(id) {
                book.fail(
                    id,
                    FailureClass::Content,
                    &format!("{reason} (and again on the retry)"),
                    None,
                );
                report_progress(ctx, stage, book);
                return Settled::Failed;
            }
            book.retried.insert(id.clone());
            let Some(req) = s.retry(id, &reject) else {
                book.fail(id, FailureClass::Content, &reason, None);
                report_progress(ctx, stage, book);
                return Settled::Failed;
            };
            if let Err(too_large) = ensure_request_fits(&req) {
                book.fail(
                    id,
                    FailureClass::Content,
                    &format!("{reason}; the retry request was not sent: {too_large}"),
                    None,
                );
                report_progress(ctx, stage, book);
                return Settled::Failed;
            }
            Settled::Retry(Work {
                id: id.clone(),
                req: Arc::new(req),
            })
        }
    }
}

/// A call failed. When sending the same request again can fix it (a transport failure, a rate
/// limit that outlasted the retries, a row a batch left out) and the one resend is unspent, the
/// item is returned to be sent again; otherwise it becomes a failed part.
fn resend_or_fail<Id: Clone + Ord>(
    ctx: &StageCtx<'_>,
    book: &mut Book<Id>,
    work: &Work<Id>,
    class: FailureClass,
    reason: &str,
    attempts: Vec<AttemptRecord>,
) -> Option<Work<Id>> {
    book.note_attempts(&work.id, attempts);
    let outage = ctx.meter.is_some_and(UsageMeter::outage_stopped);
    if class.is_retried_at_end() && !book.resent.contains(&work.id) && !outage {
        book.resent.insert(work.id.clone());
        return Some(work.clone());
    }
    let class = if outage && class.is_retried_at_end() {
        FailureClass::OutageStop
    } else {
        class
    };
    book.fail(&work.id, class, reason, None);
    None
}

/// Why a part was not sent: the outage stop or the spend limit.
fn refusal_class(ctx: &StageCtx<'_>) -> (FailureClass, &'static str) {
    if ctx.meter.is_some_and(UsageMeter::outage_stopped) {
        (
            FailureClass::OutageStop,
            "the run stopped because the model provider could not be reached before this item was sent",
        )
    } else {
        (
            FailureClass::SpendLimit,
            "the spend limit stopped the run before this item was sent",
        )
    }
}

// ── pacing ──────────────────────────────────────────────────────────────────────────────────────

/// Tracks how long a stage has been rate limited with no success in between.
#[derive(Debug, Clone, Copy)]
struct RateClock {
    since: Option<Instant>,
    deadline: Duration,
}

impl RateClock {
    fn new(deadline: Duration) -> Self {
        Self {
            since: None,
            deadline,
        }
    }

    fn limited(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    fn cleared(&mut self) {
        self.since = None;
    }

    fn passed(&self, now: Instant) -> bool {
        self.since
            .is_some_and(|since| now.saturating_duration_since(since) >= self.deadline)
    }
}

/// Additive increase, multiplicative decrease over the number of requests in flight.
#[derive(Debug)]
pub(crate) struct Aimd {
    ceiling: usize,
    limit: usize,
    min_seen: usize,
    throttles: u32,
    successes: u32,
    pause_until: Option<Instant>,
    clock: RateClock,
}

impl Aimd {
    pub(crate) fn new(max_in_flight: usize, deadline: Duration) -> Self {
        let ceiling = max_in_flight.clamp(MIN_IN_FLIGHT, MAX_IN_FLIGHT_CAP);
        Self {
            ceiling,
            limit: ceiling,
            min_seen: ceiling,
            throttles: 0,
            successes: 0,
            pause_until: None,
            clock: RateClock::new(deadline),
        }
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    /// A reply came back. Fifty of them since the last throttle give one permit back.
    pub(crate) fn on_success(&mut self) {
        self.clock.cleared();
        self.successes += 1;
        if self.successes >= SUCCESSES_PER_PERMIT {
            self.successes = 0;
            if self.limit < self.ceiling {
                self.limit += 1;
            }
        }
    }

    /// The provider said to slow down: halve the permits (floor one) and hold every new request
    /// for `wait`. A second signal while already holding extends the hold but does not halve
    /// again: a burst of simultaneous 429s is one event. `rate_limited` starts the deadline clock
    /// (an overload does not: it fails on its own path).
    pub(crate) fn on_throttle(&mut self, wait: Duration, now: Instant, rate_limited: bool) {
        let already_holding = self.pause_until.is_some_and(|until| now < until);
        if !already_holding {
            self.limit = (self.limit / 2).max(MIN_IN_FLIGHT);
            self.min_seen = self.min_seen.min(self.limit);
            self.throttles += 1;
            self.successes = 0;
        }
        let until = now + wait;
        self.pause_until = Some(self.pause_until.map_or(until, |cur| cur.max(until)));
        if rate_limited {
            self.clock.limited(now);
        }
    }

    /// When the hold ends, if it has not.
    pub(crate) fn held_until(&self, now: Instant) -> Option<Instant> {
        self.pause_until.filter(|until| now < *until)
    }

    pub(crate) fn deadline_passed(&self, now: Instant) -> bool {
        self.clock.passed(now)
    }

    pub(crate) fn report(&self, start: usize) -> PacingReport {
        PacingReport {
            start_limit: start,
            min_limit: self.min_seen,
            final_limit: self.limit,
            throttles: self.throttles,
        }
    }
}

// ── the parallel fallback ───────────────────────────────────────────────────────────────────────

enum Called {
    /// Not admitted: the spend cap or the outage stop refused the call.
    Refused,
    Done(Result<LlmResponse, anyhow::Error>),
}

#[allow(clippy::too_many_lines)]
async fn run_parallel<S>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    book: &mut Book<S::Id>,
    pending: Vec<Work<S::Id>>,
    max_in_flight: usize,
) where
    S: StageItems + Send,
{
    let mut aimd = Aimd::new(max_in_flight, ctx.settings.rate_limit_deadline);
    let start_limit = aimd.limit();
    let mut queue: VecDeque<Work<S::Id>> = pending.into();
    let mut second_try: Vec<Work<S::Id>> = Vec::new();
    let mut in_flight: FuturesUnordered<BoxFuture<'_, (Work<S::Id>, Called)>> =
        FuturesUnordered::new();
    let label = stage.label();

    loop {
        // A cancelled job dispatches nothing more; requests already in flight finish and are
        // absorbed, and everything still queued is reported as not sent.
        if !book.cancelled && cancel_requested(ctx) {
            book.cancelled = true;
        }
        // Launch as many as the permits allow, unless the provider asked us to hold.
        loop {
            let now = Instant::now();
            if book.cancelled || in_flight.len() >= aimd.limit() || aimd.held_until(now).is_some() {
                break;
            }
            let Some(work) = queue.pop_front() else { break };
            if aimd.deadline_passed(now) {
                book.fail(
                    &work.id,
                    FailureClass::RateLimited,
                    "the provider kept rate limiting this stage and no request succeeded within \
                     the wait limit, so the item was not sent",
                    None,
                );
                report_progress(ctx, stage, book);
                continue;
            }
            if ctx.meter.is_some_and(UsageMeter::outage_stopped) {
                let (class, why) = refusal_class(ctx);
                book.fail(&work.id, class, why, None);
                report_progress(ctx, stage, book);
                continue;
            }
            let realtime = ctx.realtime;
            let meter = ctx.meter;
            let repo = ctx.repo;
            in_flight.push(
                async move {
                    let permit = match meter {
                        Some(m) => match m.admit_for_stage(repo, label).await {
                            Ok(permit) => Some(permit),
                            Err(_) => return (work, Called::Refused),
                        },
                        None => None,
                    };
                    if let Some(m) = meter {
                        m.note_request(&work.req);
                    }
                    let result = realtime.complete(work.req.as_ref().clone()).await;
                    if let Some(m) = meter {
                        match &result {
                            Ok(resp) => m.record_stage(stage, resp),
                            Err(e) => m.record_failed(stage, e),
                        }
                    }
                    drop(permit);
                    (work, Called::Done(result))
                }
                .boxed(),
            );
        }

        if in_flight.is_empty() {
            if book.cancelled {
                break;
            }
            if !queue.is_empty() {
                // Held with nothing in flight: wait the hold out, unless the job is cancelled.
                let now = Instant::now();
                if let Some(until) = aimd.held_until(now) {
                    sleep_or_cancel(ctx, until.saturating_duration_since(now)).await;
                }
                continue;
            }
            if !second_try.is_empty() {
                queue.extend(second_try.drain(..));
                continue;
            }
            break;
        }

        let now = Instant::now();
        let completed = match aimd.held_until(now).filter(|_| !queue.is_empty()) {
            // Wake when the hold ends, so waiting work can start while others are in flight.
            Some(until) => tokio::select! {
                done = in_flight.next() => done,
                () = tokio::time::sleep_until(until) => continue,
            },
            None => in_flight.next().await,
        };
        let Some((work, called)) = completed else {
            continue;
        };

        match called {
            Called::Refused => {
                let (class, why) = refusal_class(ctx);
                book.fail(&work.id, class, why, None);
                report_progress(ctx, stage, book);
            }
            Called::Done(Ok(resp)) => {
                aimd.on_success();
                if let Settled::Retry(again) = settle_reply(stage, s, ctx, book, &work, resp) {
                    queue.push_front(again);
                }
            }
            Called::Done(Err(e)) => {
                let class = classify_pass_error(&e);
                let attempts = failure::attempts_of(&e);
                let wait = failure::retry_after_of(&e).unwrap_or(ctx.settings.rate_limit_pause);
                let now = Instant::now();
                if class == FailureClass::RateLimited {
                    aimd.on_throttle(wait, now, true);
                    book.note_attempts(&work.id, attempts);
                    if aimd.deadline_passed(now) {
                        book.fail(
                            &work.id,
                            FailureClass::RateLimited,
                            &format!(
                                "the provider kept rate limiting this stage and no request \
                                 succeeded within the wait limit (last: {})",
                                disclosable_error(&format!("{e:#}"))
                            ),
                            None,
                        );
                        report_progress(ctx, stage, book);
                    } else {
                        queue.push_front(work);
                    }
                } else {
                    if failure::is_overloaded(&e) {
                        aimd.on_throttle(wait, now, false);
                    }
                    let reason = disclosable_error(&format!("{e:#}"));
                    match resend_or_fail(ctx, book, &work, class, &reason, attempts) {
                        Some(again) => second_try.push(again),
                        None => report_progress(ctx, stage, book),
                    }
                }
            }
        }
    }
    book.pacing = aimd.report(start_limit);
}

// ── the batch path ──────────────────────────────────────────────────────────────────────────────

/// The batch's price basis and the notes about ids the registry does not price, applied to every
/// row the same way `submit_round` does it.
struct Repricer<'a> {
    model: &'a str,
    basis: crate::llm::PriceBasis,
    echoed: BTreeMap<String, usize>,
}

impl<'a> Repricer<'a> {
    fn new(model: &'a str, ttl: CacheTtl) -> Self {
        Self {
            model,
            basis: crate::llm::PriceBasis::batch(ttl),
            echoed: BTreeMap::new(),
        }
    }

    fn apply(&mut self, resp: &mut LlmResponse) {
        if let Some(echoed) = crate::llm::price_as_requested(resp, self.model, self.basis) {
            *self.echoed.entry(echoed).or_insert(0) += 1;
        }
    }

    fn note(self, meter: Option<&UsageMeter>) {
        for (echoed, n) in self.echoed {
            let note = format!(
                "{n} batch result row(s) came back as model `{echoed}`, an id the model registry \
                 does not price; each was priced as the model the request named, `{}`, so the \
                 spend limit could count it.",
                self.model
            );
            eprintln!("[camerata-server] {note}");
            if let Some(m) = meter {
                m.record_note(note);
            }
        }
    }
}

/// Deal with every row of one fetched batch. `members` lists the batch's items as
/// `(custom id, work)`. Returns the work to send in a follow-up submission: items the batch left
/// out or errored (sent once more) and items whose reply the stage turned down and re-asked.
fn settle_rows<S: StageItems>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    book: &mut Book<S::Id>,
    rows: Vec<BatchResultRow>,
    members: Vec<(String, Work<S::Id>)>,
    ttl: CacheTtl,
) -> Vec<Work<S::Id>> {
    let mut by_id: HashMap<String, BatchResultRow> =
        rows.into_iter().map(|r| (r.custom_id.clone(), r)).collect();
    let mut repricer = Repricer::new(ctx.model, ttl);
    let mut follow = Vec::new();
    for (cid, work) in members {
        if book.done.contains(&work.id) {
            continue;
        }
        match by_id.remove(&cid) {
            Some(BatchResultRow {
                response: Some(mut resp),
                ..
            }) => {
                repricer.apply(&mut resp);
                if let Some(m) = ctx.meter {
                    m.record_stage(stage, &resp);
                }
                if let Settled::Retry(again) = settle_reply(stage, s, ctx, book, &work, resp) {
                    follow.push(again);
                }
            }
            Some(BatchResultRow {
                error: Some(error),
                billed,
                ..
            }) => {
                // A failed item that was still billed (a model refusal) counts toward the spend
                // limit, and the model answered, so it is a content failure: not sent again.
                let refused = billed.is_some();
                if let (Some(m), Some(mut billed)) = (ctx.meter, billed) {
                    repricer.apply(&mut billed);
                    m.record_stage(stage, &billed);
                }
                if refused {
                    book.fail(&work.id, FailureClass::Content, &error, None);
                    report_progress(ctx, stage, book);
                } else if let Some(again) = resend_or_fail(
                    ctx,
                    book,
                    &work,
                    FailureClass::Transport,
                    &error,
                    Vec::new(),
                ) {
                    follow.push(again);
                } else {
                    report_progress(ctx, stage, book);
                }
            }
            Some(_) | None => {
                if let Some(again) = resend_or_fail(
                    ctx,
                    book,
                    &work,
                    FailureClass::Transport,
                    "the batch returned no result for this item",
                    Vec::new(),
                ) {
                    follow.push(again);
                } else {
                    report_progress(ctx, stage, book);
                }
            }
        }
    }
    repricer.note(ctx.meter);
    follow
}

fn round_ctx<'a>(stage: Stage, ctx: &'a StageCtx<'a>, api: &'a dyn BatchApi) -> RoundContext<'a> {
    RoundContext {
        api,
        repo: ctx.repo,
        label: stage.label(),
        model: ctx.model,
        n_c: 0,
        n_b: 0,
        chunks: &[],
        batches: &[],
        job: ctx.job,
        meter: ctx.meter,
        per_item_usd: ctx.settings.per_item_usd,
        max_body_bytes: ctx.settings.max_body_bytes,
        fatal_errors: false,
    }
}

/// Resume, second half: batches an earlier process submitted and did not settle are polled and
/// fetched by id (never submitted again). Returns the follow-up work their rows produced, to
/// replace those items' requests in the first round.
async fn resume_batches<S>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    book: &mut Book<S::Id>,
    by_key: &BTreeMap<String, S::Id>,
    reqs: &BTreeMap<S::Id, Arc<LlmRequest>>,
) -> BTreeMap<S::Id, Work<S::Id>>
where
    S: StageItems + Send,
{
    let mut out = BTreeMap::new();
    let (Some(cp), Some(api)) = (ctx.checkpoint, ctx.batch) else {
        return out;
    };
    let mut records = cp.batches();
    for i in 0..records.len() {
        if records[i].settled {
            continue;
        }
        if cancel_requested(ctx) {
            book.cancelled = true;
            break;
        }
        let record = records[i].clone();
        let members: Vec<(String, Work<S::Id>)> = record
            .items
            .iter()
            .filter_map(|(cid, key)| {
                let id = by_key.get(key)?;
                if book.done.contains(id) {
                    return None;
                }
                Some((
                    cid.clone(),
                    Work {
                        id: id.clone(),
                        req: reqs.get(id)?.clone(),
                    },
                ))
            })
            .collect();
        let ttl = ttl_from_name(&record.ttl);
        let rctx = round_ctx(stage, ctx, api);
        let progress = PollProgress {
            stage,
            done_before: book.finished(),
            total: Some(book.total),
        };
        eprintln!(
            "[camerata-server] stage {}: resuming batch {} ({} items) by polling it, not \
             submitting it again",
            stage.label(),
            record.batch_id,
            members.len()
        );
        match poll_and_fetch(
            &rctx,
            &record.batch_id,
            ttl,
            ctx.settings.poll_interval,
            Some(progress),
            false,
            true,
        )
        .await
        {
            Ok(rows) => {
                for work in settle_rows(stage, s, ctx, book, rows, members, ttl) {
                    out.insert(work.id.clone(), work);
                }
                records[i].settled = true;
            }
            Err(failed) if failed.error.is::<JobCancelled>() => {
                cancel_live_batch(ctx, api, book, &record.batch_id).await;
                break;
            }
            Err(failed) => {
                let error = disclosable_error(&format!("{:#}", failed.error));
                for (_, work) in &members {
                    book.fail(
                        &work.id,
                        classify_pass_error(&failed.error),
                        &format!(
                            "batch {} is still not retrievable ({error}); it may be running and \
                             billed, so it was not submitted again",
                            record.batch_id
                        ),
                        Some(record.batch_id.clone()),
                    );
                }
            }
        }
        if let Err(e) = cp.save_batches(&records) {
            checkpoint_trouble(ctx, "batch list", &e);
        }
    }
    out
}

/// Submit, poll and settle the stage's items as Message Batches, sending once more what a
/// batch could not answer, until every item has ended.
async fn run_batch<S>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    api: &dyn BatchApi,
    book: &mut Book<S::Id>,
    pending: Vec<Work<S::Id>>,
) where
    S: StageItems + Send,
{
    let mut clock = RateClock::new(ctx.settings.rate_limit_deadline);
    let mut work = pending;
    while !work.is_empty() && !book.cancelled {
        work = batch_round(stage, s, ctx, api, book, work, &mut clock).await;
    }
}

#[allow(clippy::too_many_lines)]
async fn batch_round<S>(
    stage: Stage,
    s: &mut S,
    ctx: &StageCtx<'_>,
    api: &dyn BatchApi,
    book: &mut Book<S::Id>,
    work: Vec<Work<S::Id>>,
    clock: &mut RateClock,
) -> Vec<Work<S::Id>>
where
    S: StageItems + Send,
{
    let eff = efficiency_of(ctx.meter);
    let cids: Vec<String> = work
        .iter()
        .map(|w| custom_id_for(&w.id.to_string()))
        .collect();
    let ranges = plan_item_ranges(
        cids.iter()
            .map(String::as_str)
            .zip(work.iter().map(|w| w.req.as_ref())),
        ctx.model,
        ctx.settings.max_body_bytes,
    );
    if ranges.len() > 1 {
        eprintln!(
            "[camerata-server] stage {}: {} items do not fit one batch submission, splitting into \
             {} sub-batches",
            stage.label(),
            work.len(),
            ranges.len()
        );
    }
    let mut follow: Vec<Work<S::Id>> = Vec::new();
    for range in ranges {
        let sub = &work[range.clone()];
        let sub_cids = &cids[range];

        // Nothing more is submitted once the job is cancelled.
        if book.cancelled || cancel_requested(ctx) {
            book.cancelled = true;
            break;
        }
        // Nothing more is submitted after an outage stop or past the spend limit.
        if ctx.meter.is_some_and(UsageMeter::outage_stopped) {
            let (class, why) = refusal_class(ctx);
            for w in sub {
                book.fail(&w.id, class, why, None);
            }
            continue;
        }
        if let Some(m) = ctx.meter {
            if let Some(why) =
                m.batch_refusal_reason(ctx.settings.per_item_usd.map(|p| p * sub.len() as f64))
            {
                eprintln!("[camerata-server] {why}");
                m.record_note(why.clone());
                for w in sub {
                    book.fail(&w.id, FailureClass::SpendLimit, &why, None);
                }
                continue;
            }
        }

        let ttl = batch_cache_ttl(eff, sub.len());
        let submitted: Result<String, anyhow::Error> = loop {
            let items: Vec<BatchItem> = sub
                .iter()
                .zip(sub_cids)
                .map(|(w, cid)| crate::llm::build_batch_item(cid.clone(), &w.req, ctx.model, ttl))
                .collect();
            let attempt = api.submit(items).await;
            note_batch_call(ctx.meter, &attempt);
            match attempt {
                Ok(id) => {
                    clock.cleared();
                    break Ok(id);
                }
                Err(e) if classify_pass_error(&e) == FailureClass::RateLimited => {
                    // Nothing was accepted: wait the provider's retry-after and submit again,
                    // until the stage has been rate limited for the whole deadline.
                    let now = Instant::now();
                    clock.limited(now);
                    if clock.passed(now) {
                        break Err(e);
                    }
                    let wait = failure::retry_after_of(&e).unwrap_or(ctx.settings.rate_limit_pause);
                    eprintln!(
                        "[camerata-server] stage {}: batch submission rate limited, waiting {wait:?}",
                        stage.label()
                    );
                    if let Some((jobs, id)) = ctx.job {
                        jobs.touch_activity(id);
                    }
                    if sleep_or_cancel(ctx, wait).await {
                        break Err(anyhow::Error::new(JobCancelled));
                    }
                }
                Err(e) => break Err(e),
            }
        };

        let batch_id = match submitted {
            Ok(id) => id,
            Err(e) if e.is::<JobCancelled>() => {
                book.cancelled = true;
                break;
            }
            Err(e) => {
                // Nothing was accepted, so nothing can be running: safe to send again.
                let class = classify_pass_error(&e);
                let attempts = failure::attempts_of(&e);
                let reason = format!(
                    "batch submission failed ({})",
                    disclosable_error(&format!("{e:#}"))
                );
                for w in sub {
                    if let Some(again) =
                        resend_or_fail(ctx, book, w, class, &reason, attempts.clone())
                    {
                        follow.push(again);
                    } else {
                        report_progress(ctx, stage, book);
                    }
                }
                continue;
            }
        };
        eprintln!(
            "[camerata-server] stage {}: submitted batch {batch_id} ({} items)",
            stage.label(),
            sub.len()
        );
        if let Some((jobs, id)) = ctx.job {
            jobs.set_batch_id(id, &batch_id);
        }
        let mut record = BatchRecord {
            batch_id: batch_id.clone(),
            ttl: ttl_name(ttl).to_string(),
            items: sub_cids
                .iter()
                .zip(sub)
                .map(|(cid, w)| (cid.clone(), w.id.to_string()))
                .collect(),
            settled: false,
        };
        let mut records = ctx
            .checkpoint
            .map(StageCheckpoint::batches)
            .unwrap_or_default();
        records.push(record.clone());
        if let Some(cp) = ctx.checkpoint {
            if let Err(e) = cp.save_batches(&records) {
                checkpoint_trouble(ctx, "batch list", &e);
            }
        }

        let rctx = round_ctx(stage, ctx, api);
        let progress = PollProgress {
            stage,
            done_before: book.finished(),
            total: Some(book.total),
        };
        match poll_and_fetch(
            &rctx,
            &batch_id,
            ttl,
            ctx.settings.poll_interval,
            Some(progress),
            false,
            true,
        )
        .await
        {
            Ok(rows) => {
                let members: Vec<(String, Work<S::Id>)> =
                    sub_cids.iter().cloned().zip(sub.iter().cloned()).collect();
                follow.extend(settle_rows(stage, s, ctx, book, rows, members, ttl));
                record.settled = true;
                if let Some(last) = records.last_mut() {
                    last.settled = true;
                }
                if let Some(cp) = ctx.checkpoint {
                    if let Err(e) = cp.save_batches(&records) {
                        checkpoint_trouble(ctx, "batch list", &e);
                    }
                }
            }
            Err(failed) if failed.error.is::<JobCancelled>() => {
                cancel_live_batch(ctx, api, book, &batch_id).await;
                break;
            }
            Err(failed) => {
                // The batch was accepted and is running or finished server-side. It is NOT
                // submitted again: that would pay for the same items twice and orphan this id.
                // The record stays unsettled so a resume polls it.
                let error = disclosable_error(&format!("{:#}", failed.error));
                let class = classify_pass_error(&failed.error);
                if let Some(m) = ctx.meter {
                    m.record_note(format!(
                        "batch {batch_id} ({} item(s)) was submitted but its results could not be \
                         retrieved: {error}. It was NOT submitted again, because it may still be \
                         running and billed; a resume polls it.",
                        sub.len()
                    ));
                }
                for w in sub {
                    book.fail(
                        &w.id,
                        class,
                        &format!(
                            "batch {batch_id} was submitted but its results could not be \
                             retrieved ({error}); it may still be running and billed, so it was \
                             not submitted again"
                        ),
                        Some(batch_id.clone()),
                    );
                    report_progress(ctx, stage, book);
                }
            }
        }
    }
    follow
}
