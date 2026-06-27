//! Process/session-global, provider-agnostic LLM usage ledger.
//!
//! [`crate::ai_audit::UsageMeter`] tracks usage for ONE audit (its passes + calibration);
//! this ledger is the CUMULATIVE, session-wide counterpart that EVERY model call in the
//! process folds into — the audit, the research chat, story authoring, decomposition,
//! clarification suggestion, routine-prompt authoring, severity calibration, and the
//! escalation translator. It powers the cockpit's persistent "tokens / $ / calls" meter.
//!
//! **Provider-agnostic by construction.** It keys off the vendor-neutral
//! [`crate::llm::LlmResponse`] usage fields, NOT off any Anthropic-specific shape:
//!   - When `cost_usd` is present (the Anthropic CLI reports a dollar figure directly), it
//!     is added verbatim.
//!   - When `cost_usd` is `None` but tokens are present (the shape a future Gemini arm will
//!     produce — Gemini reports tokens but no dollar field), the cost is DERIVED from
//!     [`crate::llm::MODELS`] list pricing for that model id. So a Gemini call still yields a
//!     `$` figure with zero changes here, the moment its `MODELS` entries + `complete` arm land.
//!   - When the model id is absent from `MODELS`, tokens still accumulate at `$0` (never a panic).
//!
//! **Rate-limited state.** The ledger also carries a transient "we are being rate-limited"
//! flag. [`is_rate_limit_signal`] is the provider-agnostic detector: it recognizes the
//! Anthropic signals today (HTTP 429, "overloaded", "rate_limit", and the CLI idle-timeout
//! "likely rate-limited/queued" hang), and Gemini's `RESOURCE_EXHAUSTED` / 429 can be added
//! to the same set later. [`UsageLedger::note_failure`] sets the flag on a detected signal;
//! every successful [`UsageLedger::record`] CLEARS it.
//!
//! This is OBSERVABILITY ONLY. Nothing here changes the gate, the model selection, or any
//! LLM behavior — it only watches what already flows through the [`crate::llm`] seam.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::llm::{LlmResponse, MODELS};

/// Per-model accumulated usage, surfaced as the `by_model` breakdown.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ModelUsage {
    pub model: String,
    /// Input + output tokens combined for this model (the headline "tokens" figure).
    pub tokens: u64,
    pub cost: f64,
    pub calls: u64,
}

/// The cumulative, session-wide usage snapshot the `/api/usage` endpoint returns.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct UsageSnapshot {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub total_cost_usd: f64,
    pub calls: u64,
    pub by_model: Vec<ModelUsage>,
    /// True while the most recent signal was a rate-limit and no successful call has cleared it.
    pub rate_limited: bool,
    /// Details of the last rate-limit signal observed (set on detection, retained across
    /// the clear so the UI can show "last rate-limited at …" history if it wants).
    pub last_rate_limit: Option<RateLimitEvent>,
}

/// A single rate-limit observation: when it happened + a short human detail.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RateLimitEvent {
    /// Unix epoch seconds when the signal was observed.
    pub when_unix: u64,
    /// A short description of the signal (truncated error text / "idle-timeout hang" / etc.).
    pub detail: String,
}

/// Provider-agnostic rate-limit signal detector.
///
/// Returns `true` when the given text (a CLI stderr line, an API error body, the idle-timeout
/// hang message, or a JSON error blob) looks like a rate-limit / overload / queue signal.
/// Matching is case-insensitive and substring-based so it survives wrapping/prefixing.
///
/// ANTHROPIC signals covered now:
///   - HTTP `429` (the standard "Too Many Requests" status).
///   - `"overloaded"` (Anthropic's `overloaded_error` / 529 surface).
///   - `"rate_limit"` / `"rate limit"` (the `rate_limit_error` type + prose form).
///   - the CLI idle-timeout hang message, which itself says "likely rate-limited/queued".
///
/// TODO(gemini): when the Google/Gemini arm is wired in `llm.rs::complete`, add its
/// rate-limit surface here — Gemini returns `RESOURCE_EXHAUSTED` (gRPC) and HTTP `429`. The
/// `429` token already matches; add a `"resource_exhausted"` substring check for the gRPC
/// status. (OpenAI similarly uses `429` + `"rate_limit_exceeded"`, already partly covered.)
pub fn is_rate_limit_signal(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("429")
        || t.contains("overloaded")
        || t.contains("rate_limit")
        || t.contains("rate limit")
        || t.contains("rate-limit")
        // The CLI idle-timeout hang explicitly attributes itself to rate-limiting/queueing.
        || t.contains("likely rate-limited/queued")
        || t.contains("rate-limited/queued")
    // TODO(gemini): || t.contains("resource_exhausted")
}

/// Current epoch seconds (0 if the clock is before the epoch, which never happens in practice).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Process/session-global cumulative usage ledger. Interior-mutable (atomics for the scalar
/// counters, a `Mutex` for the small per-model map + last-event), so it is shared as
/// `Arc<UsageLedger>` and folded into from every concurrent LLM call without `&mut`.
#[derive(Default)]
pub struct UsageLedger {
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cache_read: AtomicU64,
    cache_creation: AtomicU64,
    /// Cost held in micro-dollars to stay integer-atomic (same trick as `UsageMeter`).
    cost_micro_usd: AtomicU64,
    calls: AtomicU64,
    rate_limited: AtomicBool,
    /// Per-model breakdown + the last rate-limit event. Behind one mutex; contention is
    /// negligible (one short critical section per completed call).
    inner: Mutex<LedgerInner>,
}

#[derive(Default)]
struct LedgerInner {
    by_model: HashMap<String, ModelUsage>,
    last_rate_limit: Option<RateLimitEvent>,
}

impl UsageLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive a dollar cost for one response when the backend didn't report one.
    ///
    /// PROVIDER-AGNOSTIC COST RULE:
    ///   1. If `cost_usd` is present (Anthropic CLI), use it verbatim.
    ///   2. Else derive from [`MODELS`] list pricing for `model_id`:
    ///      `input_tokens * price_in + output_tokens * price_out`, both `$/Mtok`. This is the
    ///      Gemini-shape path (tokens reported, no cost field) — and the API path's own
    ///      computation, recomputed here so the ledger is self-contained.
    ///   3. Else (model id not in `MODELS`) -> `0.0`, so unknown models still accumulate
    ///      tokens without crashing or poisoning the dollar total.
    fn cost_for(model_id: &str, r: &LlmResponse) -> f64 {
        if let Some(c) = r.cost_usd {
            return c;
        }
        let Some(info) = MODELS.iter().find(|m| m.id == model_id) else {
            return 0.0;
        };
        let input = r.input_tokens.unwrap_or(0) as f64;
        let output = r.output_tokens.unwrap_or(0) as f64;
        (input * info.price_in + output * info.price_out) / 1_000_000.0
    }

    /// Fold one completed call's usage into the cumulative ledger and CLEAR the rate-limit
    /// flag (a successful call proves we are no longer being throttled). `model_id` is the
    /// id the call ran on (`LlmResponse::model`), used for the `by_model` key and the cost
    /// fallback. Missing token fields simply count as zero.
    pub fn record(&self, model_id: &str, r: &LlmResponse) {
        let input = r.input_tokens.unwrap_or(0);
        let output = r.output_tokens.unwrap_or(0);
        let cost = Self::cost_for(model_id, r);

        self.input_tokens.fetch_add(input, Ordering::Relaxed);
        self.output_tokens.fetch_add(output, Ordering::Relaxed);
        self.cache_read
            .fetch_add(r.cache_read_input_tokens, Ordering::Relaxed);
        self.cache_creation
            .fetch_add(r.cache_creation_input_tokens, Ordering::Relaxed);
        self.cost_micro_usd
            .fetch_add((cost * 1_000_000.0) as u64, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        // A successful call means we're being served again -> clear the rate-limit flag.
        self.rate_limited.store(false, Ordering::Relaxed);

        if let Ok(mut inner) = self.inner.lock() {
            let key = if model_id.is_empty() {
                "(unknown)".to_string()
            } else {
                model_id.to_string()
            };
            let entry = inner.by_model.entry(key.clone()).or_default();
            entry.model = key;
            entry.tokens += input + output;
            entry.cost += cost;
            entry.calls += 1;
        }
    }

    /// Note a failed call. When `detail` matches a rate-limit signal (see
    /// [`is_rate_limit_signal`]), set the rate-limited flag + record the event. Non-rate-limit
    /// failures are ignored here (they're not the meter's concern). Returns whether the flag
    /// was set, for the caller's convenience.
    pub fn note_failure(&self, detail: &str) -> bool {
        if !is_rate_limit_signal(detail) {
            return false;
        }
        self.rate_limited.store(true, Ordering::Relaxed);
        if let Ok(mut inner) = self.inner.lock() {
            // Keep the detail short so it renders cleanly in the UI badge tooltip.
            let trimmed: String = detail.chars().take(240).collect();
            inner.last_rate_limit = Some(RateLimitEvent {
                when_unix: now_unix(),
                detail: trimmed,
            });
        }
        true
    }

    /// A consistent snapshot of the whole ledger for the `/api/usage` endpoint. The
    /// `by_model` rows are sorted by descending cost (then tokens) so the heaviest model
    /// leads the breakdown.
    pub fn snapshot(&self) -> UsageSnapshot {
        let (by_model, last_rate_limit) = match self.inner.lock() {
            Ok(inner) => {
                let mut rows: Vec<ModelUsage> = inner.by_model.values().cloned().collect();
                rows.sort_by(|a, b| {
                    b.cost
                        .partial_cmp(&a.cost)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(b.tokens.cmp(&a.tokens))
                });
                (rows, inner.last_rate_limit.clone())
            }
            Err(_) => (Vec::new(), None),
        };
        UsageSnapshot {
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            cache_read: self.cache_read.load(Ordering::Relaxed),
            cache_creation: self.cache_creation.load(Ordering::Relaxed),
            total_cost_usd: self.cost_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            calls: self.calls.load(Ordering::Relaxed),
            by_model,
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
            last_rate_limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmResponse;

    /// An Anthropic-shaped response: a dollar cost is present (the CLI reports it).
    fn anthropic_resp(model: &str, input: u64, output: u64, cost: f64) -> LlmResponse {
        LlmResponse {
            text: String::new(),
            model: model.to_string(),
            backend: "cli".to_string(),
            cost_usd: Some(cost),
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        }
    }

    /// A Gemini-shaped response: tokens reported, but NO dollar cost field (cost_usd=None).
    fn gemini_shape_resp(model: &str, input: u64, output: u64) -> LlmResponse {
        LlmResponse {
            text: String::new(),
            model: model.to_string(),
            backend: "api".to_string(),
            cost_usd: None,
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        }
    }

    #[test]
    fn accumulates_across_calls_with_cache_and_by_model() {
        let l = UsageLedger::new();
        let mut a = anthropic_resp("claude-opus-4-8", 100, 50, 0.01);
        a.cache_read_input_tokens = 30;
        a.cache_creation_input_tokens = 20;
        l.record("claude-opus-4-8", &a);
        l.record("claude-opus-4-8", &anthropic_resp("claude-opus-4-8", 200, 100, 0.02));
        l.record(
            "claude-sonnet-4-6",
            &anthropic_resp("claude-sonnet-4-6", 10, 5, 0.001),
        );

        let s = l.snapshot();
        assert_eq!(s.input_tokens, 310);
        assert_eq!(s.output_tokens, 155);
        assert_eq!(s.cache_read, 30);
        assert_eq!(s.cache_creation, 20);
        assert_eq!(s.calls, 3);
        assert!((s.total_cost_usd - 0.031).abs() < 1e-9);

        // by_model: opus has 2 calls / 450 tokens, sonnet has 1 / 15 tokens.
        assert_eq!(s.by_model.len(), 2);
        // Sorted by descending cost -> opus first.
        assert_eq!(s.by_model[0].model, "claude-opus-4-8");
        assert_eq!(s.by_model[0].calls, 2);
        assert_eq!(s.by_model[0].tokens, 450);
        let sonnet = s
            .by_model
            .iter()
            .find(|m| m.model == "claude-sonnet-4-6")
            .expect("sonnet row present");
        assert_eq!(sonnet.calls, 1);
        assert_eq!(sonnet.tokens, 15);
    }

    #[test]
    fn cost_fallback_derives_from_pricing_for_known_model() {
        // Gemini-shape: no cost_usd, but a model id present in MODELS -> derive cost.
        // claude-opus-4-8 is $15/Mtok in, $75/Mtok out.
        let l = UsageLedger::new();
        l.record(
            "claude-opus-4-8",
            &gemini_shape_resp("claude-opus-4-8", 1_000_000, 1_000_000),
        );
        let s = l.snapshot();
        // 1M * 15 + 1M * 75 over 1M = 90.0.
        assert!((s.total_cost_usd - 90.0).abs() < 1e-6, "got {}", s.total_cost_usd);
        assert_eq!(s.by_model.len(), 1);
        assert!((s.by_model[0].cost - 90.0).abs() < 1e-6);
        assert_eq!(s.by_model[0].tokens, 2_000_000);
    }

    #[test]
    fn cost_fallback_unknown_model_is_zero_no_panic() {
        let l = UsageLedger::new();
        // No cost field, model id NOT in MODELS -> tokens accumulate, cost stays 0, no panic.
        l.record(
            "gemini-3-pro",
            &gemini_shape_resp("gemini-3-pro", 500, 200),
        );
        let s = l.snapshot();
        assert_eq!(s.input_tokens, 500);
        assert_eq!(s.output_tokens, 200);
        assert_eq!(s.total_cost_usd, 0.0);
        assert_eq!(s.by_model.len(), 1);
        assert_eq!(s.by_model[0].model, "gemini-3-pro");
        assert_eq!(s.by_model[0].tokens, 700);
        assert_eq!(s.by_model[0].cost, 0.0);
    }

    #[test]
    fn provider_agnostic_mixed_accumulation() {
        // An Anthropic response (cost present) + a Gemini-shape one (tokens only, known model)
        // both accumulate correctly into the same ledger.
        let l = UsageLedger::new();
        l.record(
            "claude-sonnet-4-6",
            &anthropic_resp("claude-sonnet-4-6", 100, 100, 0.0018),
        );
        // sonnet pricing 3/15: 1M in + 1M out -> 3 + 15 = 18.0.
        l.record(
            "claude-sonnet-4-6",
            &gemini_shape_resp("claude-sonnet-4-6", 1_000_000, 1_000_000),
        );
        let s = l.snapshot();
        assert_eq!(s.calls, 2);
        assert_eq!(s.input_tokens, 1_000_100);
        // 0.0018 (reported) + 18.0 (derived) = 18.0018.
        assert!((s.total_cost_usd - 18.0018).abs() < 1e-6, "got {}", s.total_cost_usd);
    }

    #[test]
    fn rate_limit_signal_detection() {
        assert!(is_rate_limit_signal("Anthropic API HTTP 429: too many requests"));
        assert!(is_rate_limit_signal("the service is Overloaded right now"));
        assert!(is_rate_limit_signal(
            r#"{"error":{"type":"rate_limit_error"}}"#
        ));
        assert!(is_rate_limit_signal("hit a rate limit, backing off"));
        assert!(is_rate_limit_signal(
            "claude produced no model output for 120s — treating as a hang (likely rate-limited/queued; set CAMERATA_LLM_IDLE_SECS to tune)"
        ));
        // Normal text / unrelated errors must NOT trip it.
        assert!(!is_rate_limit_signal("parse claude CLI JSON: unexpected token"));
        assert!(!is_rate_limit_signal("here is a normal completion of some prose"));
        assert!(!is_rate_limit_signal("findings: 4 high, 2 medium"));
    }

    #[test]
    fn rate_limited_sets_on_detection_and_clears_on_success() {
        let l = UsageLedger::new();
        assert!(!l.snapshot().rate_limited);

        // A rate-limit failure sets the flag + records the event.
        let set = l.note_failure("Anthropic API HTTP 429: overloaded");
        assert!(set);
        let s = l.snapshot();
        assert!(s.rate_limited);
        let ev = s.last_rate_limit.expect("event recorded");
        assert!(ev.detail.contains("429"));

        // A non-rate-limit failure leaves the flag as-is (does not set it for unrelated errors).
        let l2 = UsageLedger::new();
        assert!(!l2.note_failure("some unrelated parse error"));
        assert!(!l2.snapshot().rate_limited);

        // A successful record CLEARS the flag (we're being served again). The last_rate_limit
        // history is retained for the UI.
        l.record("claude-opus-4-8", &anthropic_resp("claude-opus-4-8", 1, 1, 0.0));
        let s2 = l.snapshot();
        assert!(!s2.rate_limited);
        assert!(s2.last_rate_limit.is_some(), "history retained after clear");
    }
}
