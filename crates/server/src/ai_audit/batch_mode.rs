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
//!   spend limit), used for the first submission AND the resubmission of failed items.
//! * [`failed_scan_pass`]: the one shape a failed scan pass is disclosed in, real-time or batch.

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

/// THE TTL RULE, per token of a cached prefix shared by every item of one submission (the repo
/// map is, in every batch item of a scan). The multipliers are the ones [`crate::llm::PriceBasis`]
/// bills at (the batch halving applies to both sides, so it cancels):
///
/// * 5 minutes: the API runs a batch's items over minutes to hours, so a 5-minute entry is
///   taken to have expired before each next item. `n` items cost `n x write(5m)` = `1.25 n`.
/// * 1 hour: one write, then reads. `write(1h) + (n - 1) x read` = `2 + 0.1 (n - 1)`.
///
/// One hour is cheaper from two items up (2.1 against 2.5); a one-item submission (a small
/// resubmission) keeps the 5-minute cache, since it has nothing to re-read.
pub(crate) fn one_hour_cache_is_cheaper(items: usize) -> bool {
    let cost = |ttl: CacheTtl, n: f64| {
        let write = crate::llm::PriceBasis::batch(ttl)
            .cache_ttl
            .write_multiplier();
        match ttl {
            CacheTtl::FiveMinutes => n * write,
            CacheTtl::OneHour => write + (n - 1.0) * crate::llm::CACHE_READ_MULT,
        }
    };
    let n = items as f64;
    items > 0 && cost(CacheTtl::OneHour, n) < cost(CacheTtl::FiveMinutes, n)
}

/// The Message Batches calls the batch scan path makes. A seam so the path runs against a fake
/// in tests; production implements it on the real [`Llm`].
#[async_trait::async_trait]
pub(crate) trait BatchApi: Send + Sync {
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
const MAX_REASON_CHARS: usize = 300;

/// A scan pass that ran and failed, as the export's failed-pass list shows it. ONE shape for a
/// real-time pass whose call errored and a batch item that failed twice, so a reader cannot tell
/// the modes apart by how a gap is reported. `chunk` and `rule_batch` are 0-based.
#[allow(clippy::too_many_arguments)]
pub(crate) fn failed_scan_pass(
    repo: &str,
    group: &str,
    chunk: usize,
    n_chunks: usize,
    rule_batch: usize,
    n_rule_batches: usize,
    rules: usize,
    reason: &str,
) -> FailedPass {
    let mut reason: String = reason.trim().to_string();
    if reason.chars().count() > MAX_REASON_CHARS {
        reason = reason.chars().take(MAX_REASON_CHARS).collect::<String>() + "...";
    }
    FailedPass {
        repo: repo.to_string(),
        pass: format!(
            "scan pass {group} chunk {}/{n_chunks} rule-batch {}/{n_rule_batches}",
            chunk + 1,
            rule_batch + 1
        ),
        reason: format!(
            "FAILED: {reason}. {rules} rule check(s) were not made against this part of the \
             code. This inspection is incomplete."
        ),
    }
}

/// One (chunk, rule-batch) item of a batch scan: its id, position and the request it sends.
pub(crate) struct BatchWork {
    pub custom_id: String,
    pub ci: usize,
    pub bi: usize,
    pub req: LlmRequest,
}

/// What one [`submit_round`] produced.
#[derive(Default)]
pub(crate) struct RoundOutcome {
    /// Per custom id: the reply (already recorded on the meter), or why there is none (an
    /// errored or expired row, a missing row, a failed submission). Ids the spend limit refused
    /// are absent.
    pub results: std::collections::HashMap<String, Result<LlmResponse, String>>,
    /// Ids the spend limit refused before submission (recorded as NOT RUN on the meter).
    pub refused: std::collections::HashSet<String>,
}

/// The API's per-batch item cap.
const BATCH_CAP: usize = 100_000;

/// Everything [`submit_round`] needs besides the work itself.
pub(crate) struct RoundContext<'a> {
    pub api: &'a dyn BatchApi,
    pub repo: &'a str,
    pub label: &'a str,
    pub model: &'a str,
    pub n_c: usize,
    pub n_b: usize,
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
                for w in sub {
                    let outcome = match by_id.remove(&w.custom_id) {
                        Some(BatchResultRow {
                            response: Some(resp),
                            ..
                        }) => {
                            // Recorded as it arrives, so the spend gate of the next submission
                            // (a resubmission included) counts it.
                            if let Some(m) = ctx.meter {
                                m.record_stage(Stage::ScanPass, &resp);
                            }
                            Ok(resp)
                        }
                        Some(BatchResultRow { error, .. }) => Err(format!(
                            "batch item errored or expired ({})",
                            error.unwrap_or_else(|| "no reason given".to_string())
                        )),
                        None => Err("batch item missing from the batch results".to_string()),
                    };
                    out.results.insert(w.custom_id.clone(), outcome);
                }
            }
            Err(e) if ctx.fatal_errors => return Err(e),
            Err(e) => {
                eprintln!("[camerata-server] batch mode: sub-batch failed: {e}");
                for w in sub {
                    out.results.insert(
                        w.custom_id.clone(),
                        Err(format!("batch submission failed ({e})")),
                    );
                }
            }
        }
    }
    Ok(out)
}

/// Submit one sub-batch, poll it until it has ended, and fetch its rows.
async fn submit_poll_fetch(
    ctx: &RoundContext<'_>,
    items: Vec<BatchItem>,
    sub_idx: usize,
    n_subs: usize,
    ttl: CacheTtl,
) -> anyhow::Result<Vec<BatchResultRow>> {
    let n_items = items.len();
    let batch_id = ctx.api.submit(items).await?;
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
    poll_batch_until_ended(
        || ctx.api.poll(&batch_id),
        std::time::Duration::from_secs(poll_secs),
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
    .await?;
    ctx.api.fetch(&batch_id, ttl).await
}
