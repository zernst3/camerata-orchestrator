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
    /// Human-readable display label (e.g. `"Opus 5"`, `"Qwen3 235B Coder (free)"`).
    pub display: String,
    /// The model id as passed to the API / CLI (e.g. `"claude-opus-5"`,
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
    /// current Claude Opus, Sonnet, and Haiku models (all multimodal). For OpenRouter
    /// models, derived from `architecture.input_modalities` — `true` when the list
    /// contains `"image"`. Used to filter the Designer (vision) band model selector so
    /// only vision-capable models are offered.
    #[serde(default)]
    pub vision: bool,
    /// Whether `price_in` / `price_out` are real, known prices vs. a `0.0` placeholder.
    ///
    /// Added for the live Anthropic `/v1/models` fetch (Feature A, see
    /// `docs/design/2026-08-27_backend-safety-and-live-models.md`): that endpoint returns
    /// id + display name + created-at, NOT pricing, so a live-fetched model id absent from
    /// the hardcoded `CLAUDE_REGISTRY_MODELS` price map has no known price. `false` in that
    /// case (with `price_in`/`price_out` left at `0.0`) so the UI can show "price unknown"
    /// rather than a wrong ($0, i.e. apparently-free) number. `#[serde(default = "default_price_known")]`
    /// so every pre-existing producer/consumer (OpenRouter, the static Claude catalog, and
    /// any JSON captured before this field existed) deserializes as `true` — prices were
    /// always known before this feature shipped.
    #[serde(default = "default_price_known")]
    pub price_known: bool,
}

/// `serde(default)` helper: absent `price_known` in JSON means "written before this field
/// existed", which always carried a known price — so the correct default is `true`, not
/// the bool default of `false`.
fn default_price_known() -> bool {
    true
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
        // Every current Claude model (Opus, Sonnet, Haiku) is multimodal, and this static
        // catalog only ever lists Claude models — so every entry is vision-capable. Not
        // substring-matched against a generation number (e.g. `-4-`, `-5-`) so this stays
        // correct across model refreshes without needing an update here too.
        let vision = true;
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
            price_known: true, // The static catalog's whole reason to exist is known prices.
        }
    }
}

// ── HTTP request / response shapes ────────────────────────────────────────────

/// Response for `GET /api/models/registry`.
#[derive(Serialize)]
pub struct RegistryResp {
    /// All known models. The Claude portion is backend-aware (Feature A, see
    /// `docs/design/2026-08-27_backend-safety-and-live-models.md`): the live Anthropic
    /// `/v1/models` list when the API backend is active with a key AND that fetch has
    /// succeeded at least once, else the hardcoded `CLAUDE_REGISTRY_MODELS` catalog. Plus
    /// any cached OpenRouter entries.
    pub models: Vec<RegistryEntry>,
    /// Whether the OpenRouter portion has been fetched yet. `false` = call
    /// `POST /api/models/registry/refresh` to populate it.
    pub openrouter_fetched: bool,
    /// Whether a live Anthropic `/v1/models` fetch has ever completed in this process
    /// (success or failure — a failed fetch still marks this `true` so the UI doesn't loop
    /// showing "never tried"). Independent of whether `models` is CURRENTLY serving the
    /// live list — that also depends on the active backend at request time. `false` on a
    /// fresh install / CLI-only setup that has never attempted the fetch.
    #[serde(default)]
    pub anthropic_fetched: bool,
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

/// Response for `POST /api/models/registry/refresh/anthropic` — the Feature-A sibling of
/// [`RefreshResp`], for the live Anthropic `/v1/models` catalog rather than OpenRouter's.
#[derive(Serialize)]
pub struct AnthropicRefreshResp {
    /// How many live Anthropic models were fetched (0 = key absent or fetch error).
    pub anthropic_count: usize,
    /// Whether the Anthropic key was present and the fetch was attempted.
    pub attempted: bool,
    /// The full registry after refresh (Claude portion reflects whichever source —
    /// live or hardcoded — the active backend currently selects; see [`RegistryResp`]).
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
            display: "Sonnet 5".to_string(),
            id: "claude-sonnet-5".to_string(),
            free: false,
            tool_use: true,
            context: 200_000,
            coding: 1.0,
            price_in: 3.0,
            price_out: 15.0,
            weight: 3,
            caching: true,
            vision: true,
            price_known: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    /// JSON captured before `price_known` existed (every RegistryEntry the wire ever
    /// carried, prior to Feature A) must still deserialize — and must default to `true`,
    /// not the bool default of `false`, since every one of those prices WAS known.
    #[test]
    fn registry_entry_deserializes_pre_price_known_json_as_known() {
        let json = r#"{
            "provider": "claude",
            "display": "Sonnet 5",
            "id": "claude-sonnet-5",
            "free": false,
            "tool_use": true,
            "context": 200000,
            "coding": 1.0,
            "price_in": 3.0,
            "price_out": 15.0,
            "weight": 3,
            "caching": true,
            "vision": true
        }"#;
        let entry: RegistryEntry = serde_json::from_str(json).unwrap();
        assert!(entry.price_known, "pre-existing JSON with no price_known field must default to true");
    }
}
