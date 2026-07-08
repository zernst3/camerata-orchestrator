//! Pure, serde-only model-registry wire shapes, relocated here (Phase A of the DTO
//! extraction) from `camerata_server::model_registry`, which re-exports every name below
//! so `crate::model_registry::X` call sites resolve unchanged.
//!
//! `ModelRegistry`, `RegistryInner`, and the OpenRouter fetch types (`OpenRouterModelsResp`,
//! `OpenRouterModelRaw`, etc.) all STAY in `camerata_server::model_registry` (a later phase
//! relocates them) — they are behavior (caching, HTTP fetch), not pure data.

use serde::{Deserialize, Serialize};

/// One model in the registry. Provider-agnostic.
///
/// `provider` is the stable, UI-groupable key: `"claude"` for the subscription-CLI path,
/// `"openrouter"` for any model fetched from the OpenRouter catalog. Vendor-specific ids
/// (e.g. `"anthropic"`) are NOT used here — the provider key is the DRIVER choice, not the
/// upstream lab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Provider key — `"claude"` or `"openrouter"`. Drives selector grouping.
    pub provider: String,
    /// Human-readable display label (e.g. `"Opus 4.8"`, `"Qwen3 235B Coder (free)"`).
    pub display: String,
    /// The model id as passed to the API / CLI (e.g. `"claude-opus-4-8"`,
    /// `"qwen/qwen3-235b-a22b-04-28:free"`).
    pub id: String,
    /// Whether this model is free to call (prompt + completion price = 0). Always `false`
    /// for Claude (subscription cost, not billed per-token).
    pub free: bool,
    /// Whether this model supports tool use (function-calling). `true` for all Claude
    /// models; determined from `supported_parameters` for OpenRouter models.
    pub tool_use: bool,
    /// Context window in tokens. Used by the UI as an informational badge.
    pub context: u64,
    /// Heuristic coding suitability (0.0–1.0). `1.0` for Claude (all-purpose). For
    /// OpenRouter models, derived from the model name/id (presence of "coder", "code",
    /// "dev", "starcoder", "deepseek-coder", "qwen-coder", etc.) and whether the model
    /// supports tool use.
    pub coding: f32,
    /// USD per million input tokens. `0.0` for free models and Claude (subscription).
    #[serde(default)]
    pub price_in: f64,
    /// USD per million output tokens. `0.0` for free models and Claude (subscription).
    #[serde(default)]
    pub price_out: f64,
    /// Relative subscription-quota weight (Claude-only). Higher = heavier on the quota.
    /// Used by the profile cascade to prefer lighter models for offloadable steps.
    /// `0` for non-Claude models (they bill per-token, not via the subscription).
    ///
    /// Scale: Haiku = 1, Sonnet = 3, Opus = 10 (rough relative quota cost).
    #[serde(default)]
    pub weight: u8,
    /// Whether this model supports prompt caching. `true` for all Claude-provider models
    /// and for OpenRouter models whose family is deepseek, google/gemini, or anthropic.
    /// Used by the UI badge to show a `cache` tag.
    #[serde(default)]
    pub caching: bool,
    /// Whether this model supports vision / multimodal input (images). `true` for all
    /// Claude 4.x Opus, Sonnet, and Haiku models (all multimodal). For OpenRouter models,
    /// derived from `architecture.input_modalities` — `true` when the list contains
    /// `"image"`. Used to filter the Designer (vision) band model selector so only
    /// vision-capable models are offered.
    #[serde(default)]
    pub vision: bool,
}

/// A compile-time-only helper for the Claude static list. Converted to [`RegistryEntry`]
/// at runtime (so `RegistryEntry` can own `String` fields without const-string gymnastics).
pub struct RegistryEntryStatic {
    pub display: &'static str,
    pub id: &'static str,
    pub context: u64,
    pub weight: u8,
    pub price_in: f64,
    pub price_out: f64,
}

impl RegistryEntryStatic {
    pub fn to_entry(&self) -> RegistryEntry {
        // All Claude 4.x models (Opus 4.x, Sonnet 4.x, Haiku 4.5) are multimodal.
        // The static catalog only lists Claude 4 models, so all entries are vision-capable.
        let vision = self.id.contains("claude-opus-4")
            || self.id.contains("claude-sonnet-4")
            || self.id.contains("claude-haiku-4");
        RegistryEntry {
            provider: "claude".to_string(),
            display: self.display.to_string(),
            id: self.id.to_string(),
            free: false,
            tool_use: true, // All Claude models support tool use.
            context: self.context,
            coding: 1.0,
            price_in: self.price_in,
            price_out: self.price_out,
            weight: self.weight,
            caching: true, // All Claude (subscription/CLI) models support prompt caching.
            vision,
        }
    }
}

// ── HTTP request / response shapes ────────────────────────────────────────────

/// Response for `GET /api/models/registry`.
#[derive(Serialize)]
pub struct RegistryResp {
    /// All known models (Claude static + OpenRouter cached).
    pub models: Vec<RegistryEntry>,
    /// Whether the OpenRouter portion has been fetched yet. `false` = call
    /// `POST /api/models/registry/refresh` to populate it.
    pub openrouter_fetched: bool,
}

/// Response for `POST /api/models/registry/refresh`.
#[derive(Serialize)]
pub struct RefreshResp {
    /// How many new OpenRouter entries were fetched (0 = key absent or fetch error).
    pub openrouter_count: usize,
    /// Whether the key was present and the fetch was attempted.
    pub attempted: bool,
    /// The full registry after refresh.
    pub models: Vec<RegistryEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RegistryEntry serde ───────────────────────────────────────────────────

    #[test]
    fn registry_entry_serde_roundtrip() {
        let entry = RegistryEntry {
            provider: "claude".to_string(),
            display: "Sonnet 4.6".to_string(),
            id: "claude-sonnet-4-6".to_string(),
            free: false,
            tool_use: true,
            context: 200_000,
            coding: 1.0,
            price_in: 3.0,
            price_out: 15.0,
            weight: 3,
            caching: true,
            vision: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }
}
