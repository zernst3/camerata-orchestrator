//! Rank 2 (SPIKE_TOKEN_EFFICIENCY_RESULTS.md, item 1): Message Batches as the API default, and
//! the pieces that make it safe enough to be the default. Switched by
//! [`crate::efficiency::EfficiencyConfig::batch_default`]; with it off every function here
//! reproduces the pre-rank-2 opt-in batch exactly.
//!
//! * [`resolve_scan_mode`]: which mode a run uses when nobody forced one.
//! * [`batch_cache_ttl`]: the prompt-cache TTL of one batch submission, by the price arithmetic.
//! * [`BatchApi`]: the Message Batches calls as a seam, so the batch path is testable with a
//!   fake and no network. Production passes the real [`Llm`].
//! * [`submit_round`]: one round of submissions (split at the API's item cap, each gated by the
//!   spend limit), used for the first submission AND the resubmission of failed items. A failed
//!   scan pass is a `PassFailure` (the shape every pass runner uses), real-time or batch.
//! * [`disclosable_error`]: what of a provider error a failed-pass disclosure may carry.

use super::*;
use crate::llm::{BatchItem, BatchResultRow, BatchStatus, CacheTtl, LlmResponse};

/// Printed once per run when the CLI (`claude` subprocess) backend runs a scan that would have
/// been a batch on the API backend.
pub const CLI_BACKEND_REALTIME_NOTICE: &str = "The CLI backend cannot use Message Batches, so \
     this scan runs in real time (parallel). Use the API backend for the batch discount.";

/// The mode a run executes in, and the one notice to show when the default could not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanModeChoice {
    pub mode: ScanMode,
    /// Shown once (the CLI prints it, the app adds it to the scan notes). `None` = nothing to say.
    pub notice: Option<String>,
}

/// The mode a run uses. `requested` is what [`ScanMode::parse`] returned: `Some` when the caller
/// forced a mode (`--realtime`, `--batch`, or the app's picker set to a named mode), which always
/// wins; `None` when the run's default applies:
///
/// * `batch_default` off: `Parallel`, the pre-rank-2 default (batch is opt-in).
/// * API backend: `Batch` (half price on every scan token).
/// * CLI backend: `Parallel` with [`CLI_BACKEND_REALTIME_NOTICE`]; a `claude` subprocess has no
///   batches endpoint.
/// * Blocked backend: `Parallel`; no model call is made anyway.
pub fn resolve_scan_mode(
    requested: Option<ScanMode>,
    backend: &crate::llm::BackendResolution,
    eff: crate::efficiency::EfficiencyConfig,
) -> ScanModeChoice {
    let realtime = |notice: Option<&str>| ScanModeChoice {
        mode: ScanMode::Parallel,
        notice: notice.map(str::to_string),
    };
    if let Some(mode) = requested {
        return ScanModeChoice { mode, notice: None };
    }
    if !eff.batch_default {
        return realtime(None);
    }
    match backend {
        crate::llm::BackendResolution::Api => ScanModeChoice {
            mode: ScanMode::Batch,
            notice: None,
        },
        crate::llm::BackendResolution::Cli => realtime(Some(CLI_BACKEND_REALTIME_NOTICE)),
        crate::llm::BackendResolution::Blocked { .. } => realtime(None),
    }
}

/// The prompt-cache TTL one batch submission of `items` items asks for (and is priced at).
///
/// * `batch_long_cache` off: 5 minutes, always (rank 3's switch keeps its meaning).
/// * `batch_default` off: [`crate::llm::BATCH_CACHE_TTL`] (1 hour), the pre-rank-2 behavior.
/// * Otherwise the cheaper of the two by the price arithmetic, [`one_hour_cache_is_cheaper`].
pub(crate) fn batch_cache_ttl(eff: crate::efficiency::EfficiencyConfig, items: usize) -> CacheTtl {
    if !eff.batch_long_cache {
        CacheTtl::FiveMinutes
    } else if !eff.batch_default {
        crate::llm::BATCH_CACHE_TTL
    } else if one_hour_cache_is_cheaper(items) {
        CacheTtl::OneHour
    } else {
        CacheTtl::FiveMinutes
    }
}

/// THE TTL RULE: one hour from two items up, five minutes for a single item.
///
/// It is what this comparison reduces to, per token of a cached prefix shared by every item of
/// one submission (the repo map is, in every batch item of a scan), at the multipliers
/// [`crate::llm::PriceBasis`] bills at (the batch halving applies to both sides, so it cancels):
///
/// * 5 minutes: `n` items cost `n x write(5m)` = `1.25 n`, on the ASSUMPTION that a 5-minute
///   entry has expired before each next item (the API runs a batch's items over minutes to
///   hours). That is a worst case, not a measurement: an item that does land inside the window
///   reads at `0.1` and the 5-minute side would be cheaper than this arithmetic says.
/// * 1 hour: one write, then reads: `write(1h) + (n - 1) x read` = `2 + 0.1 (n - 1)`.
///
/// `2 + 0.1 (n - 1) < 1.25 n` holds exactly when `n >= 2` (`n > 1.65`), so the rule is just
/// that. A one-item submission (a small resubmission) has nothing to re-read and keeps the
/// 5-minute cache. A test recomputes the comparison from the live multipliers, so a price
/// change that moves the threshold fails there instead of leaving this rule stale.
pub(crate) fn one_hour_cache_is_cheaper(items: usize) -> bool {
    items >= 2
}

/// The Message Batches calls the batch scan path makes. A seam so the path runs against a fake
/// in tests; production implements it on the real [`Llm`].
#[async_trait::async_trait]
pub trait BatchApi: Send + Sync {
    /// Whether an Anthropic API key is configured (no key, no batches).
    fn has_api_key(&self) -> bool;
    /// The same client as a real-time completer, for the truncation retry.
    fn realtime(&self) -> &dyn LlmPort;
    /// The exact input tokens of one request, or `None` when unavailable.
    async fn count_input_tokens(&self, req: &LlmRequest, model: &str) -> Option<u64>;
    /// Submit items; returns the batch id.
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String>;
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus>;
    async fn fetch(&self, batch_id: &str, ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>>;
}

/// A model seam that carries a [`BatchApi`] as well as the real-time calls, so a test can drive
/// the batch scan path (and see that it ran) without the network. Hand it to the audit entry
/// points as the `LlmPort`: the batch dispatch recognizes it and uses its [`BatchApi`] where
/// production uses the real [`Llm`]; every real-time call goes to the api's own completer.
pub struct BatchSeam {
    api: std::sync::Arc<dyn BatchApi>,
}

impl BatchSeam {
    pub fn new(api: std::sync::Arc<dyn BatchApi>) -> Self {
        Self { api }
    }

    pub(crate) fn api(&self) -> &dyn BatchApi {
        self.api.as_ref()
    }

    /// The batch API itself, shared, for a wrapper that stores what it does.
    pub(crate) fn api_arc(&self) -> std::sync::Arc<dyn BatchApi> {
        self.api.clone()
    }
}

#[async_trait::async_trait]
impl LlmPort for BatchSeam {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.api.realtime().complete(req).await
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.api.realtime().complete_streaming(req, on_delta).await
    }
    fn note_rejected_reply(&self, request_key: &str, reason: &str) {
        self.api.realtime().note_rejected_reply(request_key, reason);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl BatchApi for Llm {
    fn has_api_key(&self) -> bool {
        self.api_key().is_some()
    }
    fn realtime(&self) -> &dyn LlmPort {
        self
    }
    async fn count_input_tokens(&self, req: &LlmRequest, model: &str) -> Option<u64> {
        crate::llm::exact_input_token_count(self, req, model).await
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        Ok(self.submit_batch(items).await?.batch_id)
    }
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        self.poll_batch_status(batch_id).await
    }
    async fn fetch(&self, batch_id: &str, ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        self.fetch_batch_results(batch_id, ttl).await
    }
}

/// The longest error text a failed-pass disclosure carries. The full text stays in the log.
const MAX_REASON_CHARS: usize = 400;

/// What of a provider error a failed-pass disclosure may carry: the error TYPE and MESSAGE, never
/// a response body. A provider HTTP error reads `<where> HTTP <status>: <json body>`; the body
/// can echo request content or carry fields the client report has no business showing, so this
/// keeps the prefix plus the body's `error.type` and `error.message` when it parses, and the
/// prefix alone when it does not. Text with no body is kept as is. Capped at
/// [`MAX_REASON_CHARS`]. Idempotent, so applying it again to an already clean reason changes
/// nothing.
pub(crate) fn disclosable_error(raw: &str) -> String {
    let raw = raw.trim();
    let clean = match raw.find('{') {
        None => raw.to_string(),
        Some(at) => {
            let prefix = raw[..at].trim_end().trim_end_matches(':').trim_end();
            let detail = raw
                .rfind('}')
                .and_then(|end| serde_json::from_str::<serde_json::Value>(&raw[at..=end]).ok())
                .and_then(|body| {
                    let err = body.get("error").unwrap_or(&body);
                    let field = |k: &str| err.get(k).and_then(|v| v.as_str()).map(str::to_string);
                    match (field("type"), field("message")) {
                        (Some(t), Some(m)) => Some(format!("{t}: {m}")),
                        (Some(x), None) | (None, Some(x)) => Some(x),
                        (None, None) => None,
                    }
                });
            match detail {
                Some(d) if prefix.is_empty() => d,
                Some(d) => format!("{prefix}: {d}"),
                None => prefix.to_string(),
            }
        }
    };
    if clean.chars().count() > MAX_REASON_CHARS {
        clean.chars().take(MAX_REASON_CHARS).collect::<String>() + "..."
    } else {
        clean
    }
}

/// One (chunk, rule-batch) item of a batch scan: its id, position and the request it sends.
pub(crate) struct BatchWork {
    pub custom_id: String,
    pub ci: usize,
    pub bi: usize,
    pub req: LlmRequest,
}

/// Why one batch item has no reply, and whether sending it again is safe.
#[derive(Debug, Clone)]
pub(crate) struct ItemFailure {
    pub reason: String,
    /// `true` only when the item can NOT be running or billed: the batch never accepted it (a
    /// failed submit), or the API answered for it with an error or left it out of results it
    /// returned. `false` when its batch was accepted and the answer could not be retrieved: that
    /// batch may still be running and paid for, and a second copy would be paid for again.
    pub resubmittable: bool,
    /// `true` when the end-of-run second try (one real-time call) may send the item again: the
    /// same condition as [`Self::resubmittable`] for a first failure, and true for an item that
    /// already failed its resubmission. `false` for an item whose batch was accepted, because a
    /// second copy of a running batch's item is paid for again.
    pub second_tryable: bool,
}

/// What one [`submit_round`] produced.
#[derive(Default)]
pub(crate) struct RoundOutcome {
    /// Per custom id: the reply (already recorded on the meter), or why there is none (an
    /// errored or expired row, a failed submission, a batch whose results could not be
    /// retrieved). An id absent from both this map and `refused` was missing from results the
    /// API returned. Ids the spend limit refused are absent.
    pub results: std::collections::HashMap<String, Result<LlmResponse, ItemFailure>>,
    /// Ids the spend limit refused before submission (recorded as NOT RUN on the meter).
    pub refused: std::collections::HashSet<String>,
}

/// The API's per-batch item cap.
const BATCH_CAP: usize = 100_000;

/// How many times a sub-batch's poll-and-fetch is attempted after a SUCCESSFUL submit when errors
/// are not fatal (each attempt already tolerates a streak of failed polls, see
/// [`MAX_CONSECUTIVE_POLL_FAILURES`]). Bounded so a broken network cannot hold a scan forever;
/// the batch itself is never submitted again.
const POST_SUBMIT_ATTEMPTS: u32 = 3;

/// Everything [`submit_round`] needs besides the work itself.
pub(crate) struct RoundContext<'a> {
    pub api: &'a dyn BatchApi,
    pub repo: &'a str,
    pub label: &'a str,
    pub model: &'a str,
    pub n_c: usize,
    pub n_b: usize,
    /// The file chunks, for naming the files a refused pass would have checked.
    pub chunks: &'a [&'a [(String, String)]],
    pub batches: &'a [&'a [(String, String)]],
    pub job: Option<(&'a crate::jobs::JobStore, &'a str)>,
    pub meter: Option<&'a UsageMeter>,
    /// The projected input dollars of one item, for the spend-limit gate.
    pub per_item_usd: Option<f64>,
    /// `true` (the pre-rank-2 behavior): a failed submit, poll or fetch aborts the scan.
    /// `false`: it fails only that sub-batch's items, so replies already in hand are kept and
    /// the failure is disclosed per item.
    pub fatal_errors: bool,
}

/// Submit `work` as one or more Message Batches (split at [`BATCH_CAP`]), each checked against
/// the spend limit BEFORE it is submitted (a submitted batch cannot be cancelled cheaply), poll
/// each to the end and collect every item's outcome. Used for the first round and for the
/// resubmission round, so the resubmission passes through the same spend gate.
pub(crate) async fn submit_round(
    ctx: &RoundContext<'_>,
    work: &[&BatchWork],
) -> anyhow::Result<RoundOutcome> {
    let eff = efficiency_of(ctx.meter);
    let mut out = RoundOutcome::default();
    let n_subs = work.len().div_ceil(BATCH_CAP);
    if n_subs > 1 {
        eprintln!(
            "[camerata-server] batch mode: {} items exceed the 100k cap, splitting into {n_subs} \
             sub-batches",
            work.len()
        );
    }
    for (sub_idx, sub) in work.chunks(BATCH_CAP).enumerate() {
        // After an outage stop nothing more is submitted: every item is recorded NOT RUN (the
        // meter classes it as an outage stop) and refused, as for the spend limit.
        if ctx.meter.is_some_and(UsageMeter::outage_stopped) {
            for w in sub {
                out.refused.insert(w.custom_id.clone());
                if let Some(m) = ctx.meter {
                    m.record_not_run(crate::cost_cap::NotRunPass {
                        repo: ctx.repo.to_string(),
                        group: ctx.label.to_string(),
                        chunk: w.ci + 1,
                        n_chunks: ctx.n_c,
                        rule_batch: w.bi + 1,
                        n_rule_batches: ctx.n_b,
                        rule_ids: ctx.batches[w.bi].iter().map(|(id, _)| id.clone()).collect(),
                        files: ctx.chunks[w.ci]
                            .iter()
                            .map(|(path, _)| path.clone())
                            .collect(),
                        class: camerata_llm::failure::FailureClass::SpendLimit,
                    });
                }
            }
            continue;
        }
        // Checked again for every sub-batch: the spend of the ones before it is known by now.
        if let Some(m) = ctx.meter {
            if let Some(why) =
                m.batch_refusal_reason(ctx.per_item_usd.map(|p| p * sub.len() as f64))
            {
                eprintln!("[camerata-server] {why}");
                m.record_note(why);
                for w in sub {
                    out.refused.insert(w.custom_id.clone());
                    m.record_not_run(crate::cost_cap::NotRunPass {
                        repo: ctx.repo.to_string(),
                        group: ctx.label.to_string(),
                        chunk: w.ci + 1,
                        n_chunks: ctx.n_c,
                        rule_batch: w.bi + 1,
                        n_rule_batches: ctx.n_b,
                        rule_ids: ctx.batches[w.bi].iter().map(|(id, _)| id.clone()).collect(),
                        files: ctx.chunks[w.ci]
                            .iter()
                            .map(|(path, _)| path.clone())
                            .collect(),
                        class: camerata_llm::failure::FailureClass::SpendLimit,
                    });
                }
                continue;
            }
        }
        let ttl = batch_cache_ttl(eff, sub.len());
        let items: Vec<BatchItem> = sub
            .iter()
            .map(|w| crate::llm::build_batch_item(&w.custom_id, &w.req, ctx.model, ttl))
            .collect();
        match submit_poll_fetch(ctx, items, sub_idx, n_subs, ttl).await {
            Ok(rows) => {
                let mut by_id: std::collections::HashMap<String, BatchResultRow> =
                    rows.into_iter().map(|r| (r.custom_id.clone(), r)).collect();
                // A row is priced by the model id the API echoes in it. One the registry does
                // not price is priced as the model the request named (the one the pre-flight
                // checked), so the spend limit stays enforceable; the echoed ids are noted once
                // each per sub-batch, with their count.
                let basis = crate::llm::PriceBasis::batch(ttl);
                let mut repriced: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                let mut reprice = |resp: &mut crate::llm::LlmResponse| {
                    if let Some(echoed) = crate::llm::price_as_requested(resp, ctx.model, basis) {
                        *repriced.entry(echoed).or_insert(0) += 1;
                    }
                };
                for w in sub {
                    match by_id.remove(&w.custom_id) {
                        Some(BatchResultRow {
                            response: Some(mut resp),
                            ..
                        }) => {
                            reprice(&mut resp);
                            // Recorded as it arrives, so the spend gate of the next submission
                            // (a resubmission included) counts it.
                            if let Some(m) = ctx.meter {
                                m.record_stage(Stage::ScanPass, &resp);
                            }
                            out.results.insert(w.custom_id.clone(), Ok(resp));
                        }
                        Some(BatchResultRow {
                            error: Some(error),
                            billed,
                            ..
                        }) => {
                            // A failed item that was still billed (a model refusal) counts toward
                            // the spend limit before its failure is reported. The model answered,
                            // so it is a content failure: the same item is not sent again.
                            let refused = billed.is_some();
                            if let (Some(m), Some(mut billed)) = (ctx.meter, billed) {
                                reprice(&mut billed);
                                m.record_stage(Stage::ScanPass, &billed);
                            }
                            out.results.insert(
                                w.custom_id.clone(),
                                Err(ItemFailure {
                                    reason: error,
                                    resubmittable: !refused,
                                    second_tryable: !refused,
                                }),
                            );
                        }
                        // No reply and no reason, or no row at all: left out of `results`, which
                        // the caller reports as missing from the batch results.
                        Some(_) | None => {}
                    }
                }
                for (echoed, n) in repriced {
                    let note = format!(
                        "{n} batch result row(s) came back as model `{echoed}`, an id the model \
                         registry does not price; each was priced as the model the request \
                         named, `{}`, so the spend limit could count it.",
                        ctx.model
                    );
                    eprintln!("[camerata-server] {note}");
                    if let Some(m) = ctx.meter {
                        m.record_note(note);
                    }
                }
            }
            Err(failure) if ctx.fatal_errors => return Err(failure.error),
            Err(failure) => {
                eprintln!(
                    "[camerata-server] batch mode: sub-batch failed: {}",
                    failure.error
                );
                let error = disclosable_error(&format!("{:#}", failure.error));
                let item = match &failure.batch_id {
                    // Nothing was accepted, so nothing can be running: safe to send again.
                    None => ItemFailure {
                        reason: format!("batch submission failed ({error})"),
                        resubmittable: true,
                        second_tryable: true,
                    },
                    // The batch was accepted and is running or finished server-side. It is NOT
                    // resubmitted: that would pay for the same items again, outside the cap's
                    // sight, and orphan this id.
                    Some(id) => {
                        if let Some(m) = ctx.meter {
                            m.record_note(format!(
                                "batch {id} ({} item(s)) was submitted but its results could not \
                                 be retrieved: {error}. It was NOT resubmitted, because it may \
                                 still be running and billed; check it in the Anthropic console.",
                                sub.len()
                            ));
                        }
                        ItemFailure {
                            reason: format!(
                                "batch {id} was submitted but its results could not be retrieved \
                                 ({error}); it may still be running and billed, so it was not \
                                 resubmitted"
                            ),
                            resubmittable: false,
                            second_tryable: false,
                        }
                    }
                };
                for w in sub {
                    out.results.insert(w.custom_id.clone(), Err(item.clone()));
                }
            }
        }
    }
    Ok(out)
}

/// Why a sub-batch yielded no rows, and whether the API had accepted it by then.
struct SubBatchFailure {
    /// The id of the accepted batch, or `None` when the submit itself failed (nothing accepted).
    batch_id: Option<String>,
    error: anyhow::Error,
}

/// Tell the run's outage counter how a Message Batches call went (submit, poll or fetch, after
/// the transport's own retries): a success starts the streak over, a failure adds to it. A
/// provider outage during batch mode therefore stops the run like one in real-time mode.
fn note_batch_call<T>(meter: Option<&UsageMeter>, result: &anyhow::Result<T>) {
    let Some(m) = meter else { return };
    match result {
        Ok(_) => m.health().note_success(),
        Err(e) => m
            .health()
            .note_failure(classify_pass_error(e), &e.to_string()),
    }
}

/// Submit one sub-batch, poll it until it has ended, and fetch its rows. A failed submit is
/// reported with no batch id. After a successful submit, poll and fetch are attempted up to
/// [`POST_SUBMIT_ATTEMPTS`] times (once when errors are fatal, the pre-rank-2 behavior), and a
/// final failure carries the batch id.
async fn submit_poll_fetch(
    ctx: &RoundContext<'_>,
    items: Vec<BatchItem>,
    sub_idx: usize,
    n_subs: usize,
    ttl: CacheTtl,
) -> Result<Vec<BatchResultRow>, SubBatchFailure> {
    let n_items = items.len();
    let submitted = ctx.api.submit(items).await;
    note_batch_call(ctx.meter, &submitted);
    let batch_id = submitted.map_err(|error| SubBatchFailure {
        batch_id: None,
        error,
    })?;
    eprintln!(
        "[camerata-server] batch mode: sub-batch {}/{n_subs} submitted as {batch_id} ({n_items} \
         items)",
        sub_idx + 1,
    );
    // Record the batch id on the job so the UI can surface it.
    if let Some((jstore, jid)) = ctx.job {
        jstore.set_batch_id(jid, &batch_id);
    }
    // The Anthropic spec recommends >= 1s between polls; 10s is gentle.
    // `CAMERATA_BATCH_POLL_SECS` overrides.
    let poll_secs = std::env::var("CAMERATA_BATCH_POLL_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(10);
    let interval = std::time::Duration::from_secs(poll_secs);
    let attempts = if ctx.fatal_errors {
        1
    } else {
        POST_SUBMIT_ATTEMPTS
    };
    let mut last_error = None;
    for attempt in 0..attempts {
        if attempt > 0 {
            eprintln!(
                "[camerata-server] batch {batch_id}: retrieving results again (attempt {}/{attempts})",
                attempt + 1
            );
            tokio::time::sleep(interval).await;
        }
        let polled = poll_batch_until_ended(
            || async {
                let polled = ctx.api.poll(&batch_id).await;
                note_batch_call(ctx.meter, &polled);
                polled
            },
            interval,
            MAX_CONSECUTIVE_POLL_FAILURES,
            |status| {
                eprintln!(
                    "[camerata-server] batch {batch_id}: status={} (processing={}, succeeded={}, \
                     errored={}, expired={})",
                    status.processing_status,
                    status.request_counts.processing,
                    status.request_counts.succeeded,
                    status.request_counts.errored,
                    status.request_counts.expired,
                );
            },
        )
        .await;
        let fetched = match polled {
            Ok(()) => {
                let fetched = ctx.api.fetch(&batch_id, ttl).await;
                note_batch_call(ctx.meter, &fetched);
                fetched
            }
            Err(e) => Err(e),
        };
        match fetched {
            Ok(rows) => return Ok(rows),
            Err(e) => last_error = Some(e),
        }
    }
    Err(SubBatchFailure {
        batch_id: Some(batch_id),
        error: last_error.unwrap_or_else(|| anyhow::anyhow!("no attempt was made")),
    })
}
