//! `LlmResponse` — the pure, serde-only completion-result wire body, relocated here
//! (Phase A of the DTO extraction) from `camerata_server::llm`, which re-exports it so
//! `crate::llm::LlmResponse` call sites resolve unchanged.
//!
//! `LlmRequest`, `Llm`, `Vendor`, `Backend`, `MODELS`, and `ModelInfo` all STAY in
//! `camerata_server::llm` (a later phase relocates them); only the response shape and
//! its inherent accounting method move here.

use serde::Serialize;

/// A completion result.
#[derive(Debug, Clone, Serialize)]
pub struct LlmResponse {
    pub text: String,
    pub model: String,
    /// `cli` | `api` — which backend served it (surfaced honestly in the UI).
    pub backend: String,
    /// Cost in USD when known: the CLI reports it directly; the API path computes it from
    /// token usage × the model's list price (see `MODELS`).
    pub cost_usd: Option<f64>,
    /// Real billed input tokens (folds in cache read/creation), when the backend reports
    /// usage. Drives the post-scan ACTUAL-vs-estimated readout.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Real billed output tokens, when reported.
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Tokens read FROM the prompt cache on this call (billed at ~0.1× the normal input
    /// rate). Populated only by the API path when prompt caching is active; zero otherwise.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens WRITTEN TO the prompt cache on this call (billed at ~1.25× the normal input
    /// rate, one-time cost to seed the 5-min TTL cache). Populated only by the API path
    /// when prompt caching is active; zero otherwise.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// OpenRouter response-level cache discount (0.0–1.0) when `X-OpenRouter-Cache: true`
    /// is set. `1.0` means the full response was served from cache (no model cost);
    /// `0.0` means no discount; `None` when the field was absent (non-OR backend or
    /// caching not enabled). Use this to track savings in the usage ledger / UI.
    #[serde(default)]
    pub or_cache_discount: Option<f64>,
}

impl LlmResponse {
    /// The EFFECTIVE prompt-cache hit ratio for this call: of all the input tokens billed on
    /// the input side (fresh input + cache reads + cache creation), the fraction that was served
    /// from the prefix cache (`cache_read`). `1.0` means the entire input prefix was a cache hit;
    /// `0.0` means nothing was cached (or the backend reports no cache usage).
    ///
    /// This is the practical verification of prefix stability (design requirement #4): if the
    /// geological layering is holding, the stable Layer-1/Layer-2 prefix should be read from
    /// cache on every call after the first, so this ratio should climb toward 1.0 across a
    /// multi-turn / bounce loop. A ratio that stays near 0 signals the prefix is churning.
    ///
    /// Returns `None` when there is no input-token accounting to compute a ratio from (e.g. a
    /// backend/stub that reports no usage), so callers can distinguish "no data" from "0% hit".
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        // `input_tokens` already folds in cache_read + cache_creation (see `usage_tokens`), so it
        // is the correct denominator: the total input-side billing base for this call.
        let total_input = self.input_tokens?;
        if total_input == 0 {
            return None;
        }
        Some(self.cache_read_input_tokens as f64 / total_input as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bare LlmResponse for accounting tests: only the token fields matter.
    fn resp_with_tokens(input: Option<u64>, cache_read: u64, cache_creation: u64) -> LlmResponse {
        LlmResponse {
            text: String::new(),
            model: "m".to_string(),
            backend: "test".to_string(),
            cost_usd: None,
            input_tokens: input,
            output_tokens: None,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
            or_cache_discount: None,
        }
    }

    #[test]
    fn cache_hit_ratio_reports_the_cached_fraction_of_input() {
        // input_tokens folds in cache_read + cache_creation, so it is the full denominator.
        // 900 of 1000 input tokens read from cache = 0.9.
        let r = resp_with_tokens(Some(1000), 900, 50);
        assert_eq!(r.cache_hit_ratio(), Some(0.9));

        // Full hit: everything came from cache.
        let r = resp_with_tokens(Some(500), 500, 0);
        assert_eq!(r.cache_hit_ratio(), Some(1.0));

        // Cold call: no cache reads at all.
        let r = resp_with_tokens(Some(1000), 0, 1000);
        assert_eq!(r.cache_hit_ratio(), Some(0.0));
    }

    #[test]
    fn cache_hit_ratio_none_without_usage_or_zero_input() {
        // No usage reported (CLI/stub) → None, so callers can tell "no data" from "0% hit".
        assert_eq!(resp_with_tokens(None, 0, 0).cache_hit_ratio(), None);
        // Zero input tokens → None (avoids a divide-by-zero and is meaningless anyway).
        assert_eq!(resp_with_tokens(Some(0), 0, 0).cache_hit_ratio(), None);
    }
}
